//! Packs: folders with a `pack.toml` manifest, a worker and its slices (SPEC 7).
//!
//! The loader reads every pack under the packs folder, validates it, and reports problems per
//! pack instead of failing, so one broken pack never takes the others down. The wheel's slices
//! come from the loaded packs, filtered by the selection and ordered by the user's config.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::config::SliceConfig;

pub const MANIFEST: &str = "pack.toml";
/// The core version packs are checked against (`requires_core`).
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Worker limits a pack may ask for. Anything outside these bounds is rejected, so a pack cannot
/// switch off the "never hang the PC" guarantees.
const MEMORY_MB: std::ops::RangeInclusive<usize> = 64..=4096;
const TIME_SECONDS: std::ops::RangeInclusive<u64> = 5..=3600;
const CPU_PERCENT: std::ops::RangeInclusive<u32> = 5..=100;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    id: String,
    name: String,
    version: String,
    requires_core: String,
    #[serde(default)]
    requires_packs: Vec<String>,
    license: String,
    worker: WorkerSpec,
    #[serde(default)]
    limits: LimitSpec,
    #[serde(default)]
    slices: Vec<SliceSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerSpec {
    kind: String,
    entry: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct LimitSpec {
    memory_mb: usize,
    time_seconds: u64,
    cpu_percent: u32,
}

impl Default for LimitSpec {
    fn default() -> Self {
        Self {
            memory_mb: 512,
            time_seconds: 120,
            cpu_percent: 50,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SliceSpec {
    id: String,
    label: String,
    #[serde(default)]
    icon: Option<String>,
    applies_to: Vec<String>,
    #[serde(default = "one")]
    min_files: usize,
    #[serde(default)]
    max_files: Option<usize>,
    /// Output file name template, e.g. "{stem} - merged.pdf". See [`output_name`].
    output: String,
    action: ActionSpec,
}

fn one() -> usize {
    1
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionSpec {
    worker: String,
    op: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Limits {
    pub memory_mb: usize,
    pub time_seconds: u64,
    pub cpu_percent: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pack {
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: String,
    pub dir: PathBuf,
    pub worker_exe: PathBuf,
    pub limits: Limits,
    pub slices: Vec<PackSlice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackSlice {
    /// Globally unique: "<pack id>.<slice id>".
    pub id: String,
    pub label: String,
    pub icon: Option<String>,
    /// Lower-case extensions without the dot; "*" matches any file.
    pub applies_to: Vec<String>,
    pub min_files: usize,
    pub max_files: Option<usize>,
    pub output: String,
    /// Id of the pack whose worker runs this slice.
    pub worker: String,
    pub op: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackError {
    pub dir: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Catalog {
    pub packs: Vec<Pack>,
    pub errors: Vec<PackError>,
}

impl PackSlice {
    /// Whether this slice applies to exactly this selection.
    pub fn matches(&self, paths: &[impl AsRef<Path>]) -> bool {
        let count = paths.len();
        if count < self.min_files || self.max_files.is_some_and(|max| count > max) {
            return false;
        }
        paths.iter().all(|path| {
            let ext = path
                .as_ref()
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase());
            self.applies_to
                .iter()
                .any(|want| want == "*" || Some(want) == ext.as_ref())
        })
    }
}

impl Catalog {
    /// Every loaded slice in the user's order: configured order first, then pack order.
    /// Disabled slices are included; see [`Catalog::slices_for`] for what the wheel shows.
    pub fn ordered_slices(&self, config: &SliceConfig) -> Vec<&PackSlice> {
        let mut slices: Vec<&PackSlice> = self.packs.iter().flat_map(|p| &p.slices).collect();
        let rank: HashMap<&str, usize> = config
            .order
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();
        // Stable sort keeps pack order for slices the user has not placed.
        slices.sort_by_key(|s| rank.get(s.id.as_str()).copied().unwrap_or(usize::MAX));
        slices
    }

    /// The enabled slices that apply to `paths`, in the user's order.
    pub fn slices_for(&self, paths: &[impl AsRef<Path>], config: &SliceConfig) -> Vec<&PackSlice> {
        self.ordered_slices(config)
            .into_iter()
            .filter(|s| !config.disabled.contains(&s.id) && s.matches(paths))
            .collect()
    }

    /// The slice with this id and the pack whose worker runs it.
    pub fn find(&self, slice_id: &str) -> Option<(&Pack, &PackSlice)> {
        let slice = self
            .packs
            .iter()
            .flat_map(|p| &p.slices)
            .find(|s| s.id == slice_id)?;
        let worker = self.packs.iter().find(|p| p.id == slice.worker)?;
        Some((worker, slice))
    }
}

/// Loads every pack in `root`. `exe_dir` is where Flick's own binaries are; during development a
/// pack's worker is found there when it is not inside the pack folder yet (see [`resolve_entry`]).
pub fn load_dir(root: &Path, exe_dir: Option<&Path>) -> Catalog {
    let mut catalog = Catalog::default();
    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.join(MANIFEST).is_file())
            .collect(),
        Err(e) => {
            catalog.errors.push(PackError {
                dir: root.to_path_buf(),
                message: format!("Could not read the packs folder ({e})."),
            });
            return catalog;
        }
    };
    dirs.sort();

    for dir in dirs {
        match load_pack(&dir, exe_dir) {
            Ok(pack) if catalog.packs.iter().any(|p| p.id == pack.id) => {
                catalog.errors.push(PackError {
                    dir,
                    message: format!("Another pack already uses the id \"{}\".", pack.id),
                });
            }
            Ok(pack) => catalog.packs.push(pack),
            Err(message) => catalog.errors.push(PackError { dir, message }),
        }
    }
    drop_broken_dependents(&mut catalog);
    catalog
}

/// Removes packs whose required packs, or whose slices' worker packs, did not load.
fn drop_broken_dependents(catalog: &mut Catalog) {
    loop {
        let loaded: HashSet<String> = catalog.packs.iter().map(|p| p.id.clone()).collect();
        let Some(index) = catalog
            .packs
            .iter()
            .position(|p| missing_dependency(p, &loaded).is_some())
        else {
            return;
        };
        let pack = catalog.packs.remove(index);
        let missing = missing_dependency(&pack, &loaded).unwrap_or_default();
        catalog.errors.push(PackError {
            dir: pack.dir,
            message: format!(
                "Needs the \"{missing}\" pack, which is not installed or failed to load."
            ),
        });
    }
}

fn missing_dependency(pack: &Pack, loaded: &HashSet<String>) -> Option<String> {
    pack.slices
        .iter()
        .map(|s| &s.worker)
        .find(|w| !loaded.contains(*w))
        .cloned()
}

fn load_pack(dir: &Path, exe_dir: Option<&Path>) -> Result<Pack, String> {
    let text = std::fs::read_to_string(dir.join(MANIFEST))
        .map_err(|e| format!("Could not read {MANIFEST} ({e})."))?;
    let manifest: Manifest =
        toml::from_str(&text).map_err(|e| format!("{MANIFEST} is not valid: {e}"))?;

    if !is_identifier(&manifest.id) {
        return Err(format!(
            "Pack id \"{}\" must use only lower-case letters, digits and dashes.",
            manifest.id
        ));
    }
    Version::parse(&manifest.version)
        .map_err(|e| format!("Version \"{}\" is not valid ({e}).", manifest.version))?;
    let requires = VersionReq::parse(&manifest.requires_core).map_err(|e| {
        format!(
            "requires_core \"{}\" is not valid ({e}).",
            manifest.requires_core
        )
    })?;
    let core = Version::parse(CORE_VERSION).expect("core version is valid semver");
    if !requires.matches(&core) {
        return Err(format!(
            "Needs Flick {}, but this is Flick {CORE_VERSION}.",
            manifest.requires_core
        ));
    }

    if manifest.worker.kind != "native" {
        return Err(format!(
            "Worker kind \"{}\" is not supported yet (only \"native\").",
            manifest.worker.kind
        ));
    }
    let worker_exe = resolve_entry(dir, &manifest.worker.entry, exe_dir)?;

    let limits = Limits {
        memory_mb: manifest.limits.memory_mb,
        time_seconds: manifest.limits.time_seconds,
        cpu_percent: manifest.limits.cpu_percent,
    };
    if !MEMORY_MB.contains(&limits.memory_mb)
        || !TIME_SECONDS.contains(&limits.time_seconds)
        || !CPU_PERCENT.contains(&limits.cpu_percent)
    {
        return Err(format!(
            "Limits must be memory_mb {}-{}, time_seconds {}-{}, cpu_percent {}-{}.",
            MEMORY_MB.start(),
            MEMORY_MB.end(),
            TIME_SECONDS.start(),
            TIME_SECONDS.end(),
            CPU_PERCENT.start(),
            CPU_PERCENT.end()
        ));
    }

    let allowed_workers: HashSet<&str> = std::iter::once(manifest.id.as_str())
        .chain(manifest.requires_packs.iter().map(String::as_str))
        .collect();
    let mut seen = HashSet::new();
    let mut slices = Vec::new();
    for spec in manifest.slices {
        let where_ = format!("Slice \"{}\"", spec.id);
        if !is_identifier(&spec.id) {
            return Err(format!(
                "{where_}: ids must use only lower-case letters, digits and dashes."
            ));
        }
        if !seen.insert(spec.id.clone()) {
            return Err(format!("{where_} is defined twice."));
        }
        if spec.label.trim().is_empty() {
            return Err(format!("{where_} needs a label."));
        }
        if spec.applies_to.is_empty() {
            return Err(format!(
                "{where_}: applies_to must list at least one extension."
            ));
        }
        if spec.min_files == 0 || spec.max_files.is_some_and(|max| max < spec.min_files) {
            return Err(format!(
                "{where_}: min_files must be at least 1 and not above max_files."
            ));
        }
        if !allowed_workers.contains(spec.action.worker.as_str()) {
            return Err(format!(
                "{where_} uses the \"{}\" worker; add it to requires_packs.",
                spec.action.worker
            ));
        }
        output_name(&spec.output, Path::new("example.pdf"))
            .map_err(|e| format!("{where_}: output {e}"))?;
        slices.push(PackSlice {
            id: format!("{}.{}", manifest.id, spec.id),
            label: spec.label,
            icon: spec.icon,
            applies_to: spec
                .applies_to
                .iter()
                .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
                .collect(),
            min_files: spec.min_files,
            max_files: spec.max_files,
            output: spec.output,
            worker: spec.action.worker,
            op: spec.action.op,
        });
    }

    Ok(Pack {
        id: manifest.id,
        name: manifest.name,
        version: manifest.version,
        license: manifest.license,
        dir: dir.to_path_buf(),
        worker_exe,
        limits,
        slices,
    })
}

/// The worker binary. It must stay inside the pack folder. During development the pack folder
/// holds only the manifest, so a worker built next to Flick's own binary is used instead.
fn resolve_entry(dir: &Path, entry: &Path, exe_dir: Option<&Path>) -> Result<PathBuf, String> {
    let inside = entry
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if !inside {
        return Err(format!(
            "Worker entry \"{}\" must be a path inside the pack folder.",
            entry.display()
        ));
    }
    let in_pack = dir.join(entry);
    if in_pack.is_file() {
        return Ok(in_pack);
    }
    if let (Some(exe_dir), Some(file)) = (exe_dir, entry.file_name()) {
        let built = exe_dir.join(file);
        if built.is_file() {
            return Ok(built);
        }
    }
    Err(format!("Worker \"{}\" was not found.", entry.display()))
}

fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Builds an output file name from a slice's template and the (first, after sorting) input.
///
/// Placeholders: `{stem}` (file name without extension) and `{ext}` (extension without the dot).
/// The result must be a plain file name: no folders, so output always lands next to the input.
pub fn output_name(template: &str, input: &Path) -> Result<String, String> {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = input
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let end = rest[start..].find('}').ok_or("has an unclosed \"{\".")? + start;
        match &rest[start + 1..end] {
            "stem" => out.push_str(&stem),
            "ext" => out.push_str(&ext),
            other => return Err(format!("uses unknown placeholder {{{other}}}.")),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    let invalid = |c: char| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|');
    if out.trim().is_empty() || out.contains(invalid) || out == "." || out == ".." {
        return Err(format!("\"{template}\" must give a plain file name."));
    }
    Ok(out)
}

/// Where to look for packs: `FLICK_PACKS_DIR`, then `packs` next to the executable, then (for
/// development) a `packs` folder in an ancestor of the executable, such as the repository root.
pub fn default_dir(exe_dir: &Path) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FLICK_PACKS_DIR") {
        return Some(PathBuf::from(dir));
    }
    exe_dir
        .ancestors()
        .map(|d| d.join("packs"))
        .find(|d| d.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MERGE: &str = r#"
id = "pdf-light"
name = "PDF Light"
version = "1.0.0"
requires_core = ">=0.1"
license = "AGPL-3.0"

[worker]
kind = "native"
entry = "bin/pdf-light.exe"

[[slices]]
id = "merge"
label = "Merge"
applies_to = ["pdf"]
min_files = 2
output = "{stem} - merged.pdf"
action = { worker = "pdf-light", op = "merge" }
"#;

    /// Writes a pack folder with a manifest and a fake worker binary.
    fn write_pack(root: &Path, folder: &str, manifest: &str) -> PathBuf {
        let dir = root.join(folder);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join(MANIFEST), manifest).unwrap();
        std::fs::write(dir.join("bin").join("pdf-light.exe"), b"").unwrap();
        std::fs::write(dir.join("bin").join("tools.exe"), b"").unwrap();
        dir
    }

    fn load(manifests: &[(&str, &str)]) -> Catalog {
        let root = tempfile::tempdir().unwrap();
        for (folder, manifest) in manifests {
            write_pack(root.path(), folder, manifest);
        }
        load_dir(root.path(), None)
    }

    fn only_error(catalog: &Catalog) -> &str {
        assert!(catalog.packs.is_empty(), "{:?}", catalog.packs);
        assert_eq!(catalog.errors.len(), 1, "{:?}", catalog.errors);
        &catalog.errors[0].message
    }

    #[test]
    fn loads_a_valid_pack() {
        let catalog = load(&[("pdf-light", MERGE)]);
        assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
        let pack = &catalog.packs[0];
        assert_eq!(pack.id, "pdf-light");
        assert!(pack.worker_exe.ends_with("bin/pdf-light.exe"));
        assert_eq!(
            pack.limits,
            Limits {
                memory_mb: 512,
                time_seconds: 120,
                cpu_percent: 50
            }
        );
        assert_eq!(pack.slices[0].id, "pdf-light.merge");
        assert_eq!(pack.slices[0].op, "merge");
    }

    #[test]
    fn reports_problems_per_pack() {
        let cases = [
            (MERGE.replace("1.0.0", "one"), "Version"),
            (MERGE.replace(">=0.1", ">=99"), "Needs Flick >=99"),
            (MERGE.replace("\"native\"", "\"python\""), "not supported yet"),
            (MERGE.replace("bin/pdf-light.exe", "../evil.exe"), "inside the pack folder"),
            (MERGE.replace("bin/pdf-light.exe", "bin/missing.exe"), "was not found"),
            (MERGE.replace("{stem} - merged", "../{stem}"), "plain file name"),
            (MERGE.replace("{stem}", "{name}"), "unknown placeholder"),
            (MERGE.replace("min_files = 2", "min_files = 0"), "min_files"),
            (MERGE.replace("[\"pdf\"]", "[]"), "applies_to"),
            (MERGE.replace("worker = \"pdf-light\"", "worker = \"other\""), "requires_packs"),
            (MERGE.replace("id = \"pdf-light\"", "id = \"PDF Light\""), "lower-case"),
            (MERGE.replace("label = \"Merge\"", "label = \"Merge\"\ncolour = \"red\""), "colour"),
            (format!("{MERGE}\n[limits]\nmemory_mb = 99999\n"), "Limits"),
            (MERGE.replace("\n[[slices]]", "\n[[slices]]\nid = \"merge\"\nlabel = \"Again\"\napplies_to = [\"pdf\"]\noutput = \"x.pdf\"\naction = { worker = \"pdf-light\", op = \"x\" }\n\n[[slices]]"), "defined twice"),
        ];
        for (manifest, expected) in cases {
            let catalog = load(&[("pack", &manifest)]);
            let message = only_error(&catalog);
            assert!(
                message.contains(expected),
                "expected {expected:?} in {message:?}"
            );
        }
    }

    #[test]
    fn one_broken_pack_does_not_stop_the_others() {
        let catalog = load(&[("a-good", MERGE), ("b-broken", "id = [")]);
        assert_eq!(catalog.packs.len(), 1);
        assert_eq!(catalog.errors.len(), 1);
        assert!(catalog.errors[0].dir.ends_with("b-broken"));
    }

    #[test]
    fn duplicate_pack_ids_keep_the_first() {
        let catalog = load(&[("a", MERGE), ("b", MERGE)]);
        assert_eq!(catalog.packs.len(), 1);
        assert!(catalog.packs[0].dir.ends_with("a"));
        assert!(catalog.errors[0].message.contains("already uses"));
    }

    #[test]
    fn slices_can_use_a_required_packs_worker() {
        let tools = r#"
id = "tools"
name = "Tools"
version = "1.0.0"
requires_core = ">=0.1"
requires_packs = ["pdf-light"]
license = "AGPL-3.0"
[worker]
kind = "native"
entry = "bin/tools.exe"
[[slices]]
id = "join"
label = "Join"
applies_to = ["pdf"]
output = "{stem} - joined.pdf"
action = { worker = "pdf-light", op = "merge" }
"#;
        let catalog = load(&[("pdf-light", MERGE), ("tools", tools)]);
        assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
        let (worker, slice) = catalog.find("tools.join").unwrap();
        assert_eq!(worker.id, "pdf-light");
        assert_eq!(slice.op, "merge");

        // Without pdf-light, the tools pack cannot run its slice and is dropped with a reason.
        let catalog = load(&[("tools", tools)]);
        assert!(only_error(&catalog).contains("Needs the \"pdf-light\" pack"));
    }

    #[test]
    fn dev_builds_find_the_worker_next_to_flick() {
        let root = tempfile::tempdir().unwrap();
        let exe_dir = tempfile::tempdir().unwrap();
        let dir = root.path().join("pdf-light");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST), MERGE).unwrap();
        std::fs::write(exe_dir.path().join("pdf-light.exe"), b"").unwrap();

        let catalog = load_dir(root.path(), Some(exe_dir.path()));
        assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
        assert_eq!(
            catalog.packs[0].worker_exe,
            exe_dir.path().join("pdf-light.exe")
        );
    }

    fn slice(applies_to: &[&str], min: usize, max: Option<usize>) -> PackSlice {
        PackSlice {
            id: "p.s".into(),
            label: "S".into(),
            icon: None,
            applies_to: applies_to.iter().map(|s| s.to_string()).collect(),
            min_files: min,
            max_files: max,
            output: "{stem}.out".into(),
            worker: "p".into(),
            op: "s".into(),
        }
    }

    #[test]
    fn slices_match_type_and_count() {
        let merge = slice(&["pdf"], 2, None);
        assert!(merge.matches(&["a.pdf", "B.PDF"]));
        assert!(!merge.matches(&["a.pdf"]));
        assert!(!merge.matches(&["a.pdf", "b.png"]));
        assert!(!merge.matches(&["a.pdf", "noext"]));

        let single = slice(&["png", "jpg"], 1, Some(1));
        assert!(single.matches(&["a.jpg"]));
        assert!(!single.matches(&["a.jpg", "b.png"]));

        let any = slice(&["*"], 1, None);
        assert!(any.matches(&["a.pdf", "b", "c.zip"]));
        assert!(!any.matches(&[] as &[&str]));
    }

    #[test]
    fn user_order_and_disabled_slices_apply() {
        let manifest = MERGE.to_string()
            + r#"
[[slices]]
id = "split"
label = "Split"
applies_to = ["pdf"]
output = "{stem} - part.pdf"
action = { worker = "pdf-light", op = "split" }

[[slices]]
id = "compress"
label = "Compress"
applies_to = ["pdf"]
output = "{stem} - small.pdf"
action = { worker = "pdf-light", op = "compress" }
"#;
        let catalog = load(&[("pdf-light", &manifest)]);
        let ids = |config: &SliceConfig, paths: &[&str]| -> Vec<String> {
            catalog
                .slices_for(paths, config)
                .iter()
                .map(|s| s.id.clone())
                .collect()
        };
        let default = SliceConfig::default();
        assert_eq!(
            ids(&default, &["a.pdf"]),
            ["pdf-light.split", "pdf-light.compress"]
        );
        assert_eq!(
            ids(&default, &["a.pdf", "b.pdf"]),
            ["pdf-light.merge", "pdf-light.split", "pdf-light.compress"]
        );

        let custom = SliceConfig {
            order: vec!["pdf-light.compress".into(), "unknown.slice".into()],
            disabled: vec!["pdf-light.split".into()],
        };
        assert_eq!(
            ids(&custom, &["a.pdf", "b.pdf"]),
            ["pdf-light.compress", "pdf-light.merge"]
        );
        // Disabled slices are still listed for the Settings window.
        assert_eq!(catalog.ordered_slices(&custom).len(), 3);
    }

    #[test]
    fn output_names_come_from_the_template() {
        let input = Path::new("notes/Chapter 1.pdf");
        assert_eq!(
            output_name("{stem} - merged.pdf", input).unwrap(),
            "Chapter 1 - merged.pdf"
        );
        assert_eq!(
            output_name("{stem}.{ext}.bak", input).unwrap(),
            "Chapter 1.pdf.bak"
        );
        assert!(output_name("{stem", input).is_err());
        assert!(output_name("sub/{stem}.pdf", input).is_err());
        assert!(output_name("{stem}:x", input).is_err());
        assert!(output_name("{ext}", Path::new("README")).is_err());
    }
}

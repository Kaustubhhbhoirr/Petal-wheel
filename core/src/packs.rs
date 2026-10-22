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

/// Icon names a slice may use. The wheel draws these natively, so packs cannot ship their own.
pub const ICONS: &[&str] = &[
    "merge",
    "split",
    "rotate",
    "image-pdf",
    "convert",
    "shrink",
    "compress",
    "copy",
    "zip",
    "pages",
    "more",
    "rotate-left",
    "rotate-180",
    "remove-pages",
    "organize",
    "target",
];

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

/// How deep slices may nest: the main ring plus one ring of children. The data model and the
/// loader are recursive, so raising this to 3 is the only change needed for a third level.
pub const MAX_DEPTH: usize = 2;
/// A `choice` slice holds at most this many children, so a sub-ring stays readable.
pub const MAX_CHILDREN: usize = 8;

/// Screens of the Flick app window that an `app` slice may open. Screens are built into Flick, so
/// packs pick one by name rather than shipping code.
pub const APP_SCREENS: &[&str] = &[
    "remove-pages",
    "rotate-pages",
    "organize-pages",
    "split-ranges",
    "compress-target",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SliceSpec {
    id: String,
    label: String,
    #[serde(default)]
    icon: Option<String>,
    /// Required on top-level slices; children inherit their parent's when they leave it out.
    #[serde(default)]
    applies_to: Option<Vec<String>>,
    #[serde(default)]
    min_files: Option<usize>,
    #[serde(default)]
    max_files: Option<usize>,
    #[serde(default)]
    mode: Tier,
    #[serde(default)]
    runs: Runs,
    /// Output file name template, e.g. "{stem} - merged.pdf". See [`output_name`].
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    action: Option<ActionSpec>,
    /// For `app` slices: which screen of the Flick app window opens.
    #[serde(default)]
    app: Option<String>,
    /// For `choice` slices: the child that runs when the user releases over the parent itself.
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    children: Vec<SliceSpec>,
}

/// What happens when a slice is chosen (SPEC 4.3, "action tiers").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Needs no input: runs as soon as the shortcut is released.
    #[default]
    Quick,
    /// A few fixed options: hovering opens its children as a sub-ring.
    Choice,
    /// Needs real input (pages, a number, text): opens the Flick app window with the files.
    App,
}

/// How a slice's worker is run on the selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Runs {
    /// One run with every selected file, saved next to the first one (merge, images to PDF).
    #[default]
    Once,
    /// One run per selected file, each saved next to its own input (rotate, convert).
    Each,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionSpec {
    worker: String,
    op: String,
    /// Extra settings for the op, passed to the worker as `args` (e.g. `{ degrees = 90 }`).
    #[serde(default)]
    args: Option<toml::Table>,
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

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PackSlice {
    /// Globally unique: "<pack id>.<slice id>", plus ".<child id>" for each level of nesting.
    pub id: String,
    pub label: String,
    pub icon: Option<String>,
    /// Lower-case extensions without the dot; "*" matches any file.
    pub applies_to: Vec<String>,
    pub min_files: usize,
    pub max_files: Option<usize>,
    pub mode: Tier,
    pub runs: Runs,
    /// Output name template. With `{n}` in it, one run makes several files (see
    /// [`PackSlice::multi_output`]). Empty for `choice` slices.
    pub output: String,
    /// Id of the pack whose worker runs this slice. Empty for `choice` slices.
    pub worker: String,
    pub op: String,
    /// Settings passed to the worker with every request (`null` when there are none).
    pub args: serde_json::Value,
    /// For `app` slices: the app window screen.
    pub app: Option<String>,
    /// For `choice` slices: full id of the child released-over-the-parent runs.
    pub default_child: Option<String>,
    /// For `choice` slices: the options, in manifest order.
    pub children: Vec<PackSlice>,
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
    /// Whether one run makes several numbered files (`{n}` in the output template), such as one
    /// file per page. Such workers get an empty folder to write into instead of one file path.
    pub fn multi_output(&self) -> bool {
        self.output.contains("{n}")
    }

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

/// Sorts `slices` by the user's order (ids listed in `config.order` first, in that order); slices
/// the user has not placed keep their manifest order after those. Never sorts by usage, so
/// positions stay where the user's muscle memory expects them.
fn sort_by_config(slices: &mut [&PackSlice], config: &SliceConfig) {
    let rank: HashMap<&str, usize> = config
        .order
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    // Stable sort keeps manifest order for slices the user has not placed.
    slices.sort_by_key(|s| rank.get(s.id.as_str()).copied().unwrap_or(usize::MAX));
}

/// A slice and, for `choice` slices, the children that apply, ready for the wheel.
#[derive(Debug, Clone, PartialEq)]
pub struct Applicable<'a> {
    pub slice: &'a PackSlice,
    pub children: Vec<Applicable<'a>>,
}

impl Catalog {
    /// Every loaded top-level slice in the user's order: configured order first, then pack
    /// order. Disabled slices are included; see [`Catalog::slices_for`] for what the wheel shows.
    pub fn ordered_slices(&self, config: &SliceConfig) -> Vec<&PackSlice> {
        let mut slices: Vec<&PackSlice> = self.packs.iter().flat_map(|p| &p.slices).collect();
        sort_by_config(&mut slices, config);
        slices
    }

    /// The enabled top-level slices that apply to `paths`, in the user's order.
    pub fn slices_for(&self, paths: &[impl AsRef<Path>], config: &SliceConfig) -> Vec<&PackSlice> {
        self.tree_for(paths, config)
            .into_iter()
            .map(|a| a.slice)
            .collect()
    }

    /// The enabled slices that apply to `paths`, with their applicable children, all in the
    /// user's order. A `choice` slice left with no applicable children is dropped.
    pub fn tree_for(
        &self,
        paths: &[impl AsRef<Path>],
        config: &SliceConfig,
    ) -> Vec<Applicable<'_>> {
        fn build<'a>(
            slices: Vec<&'a PackSlice>,
            paths: &[impl AsRef<Path>],
            config: &SliceConfig,
        ) -> Vec<Applicable<'a>> {
            slices
                .into_iter()
                .filter(|s| !config.disabled.contains(&s.id) && s.matches(paths))
                .filter_map(|slice| {
                    let mut kids: Vec<&PackSlice> = slice.children.iter().collect();
                    sort_by_config(&mut kids, config);
                    let children = build(kids, paths, config);
                    (slice.mode != Tier::Choice || !children.is_empty())
                        .then_some(Applicable { slice, children })
                })
                .collect()
        }
        build(self.ordered_slices(config), paths, config)
    }

    /// The slice with this id (at any depth) and the pack whose worker runs it.
    pub fn find(&self, slice_id: &str) -> Option<(&Pack, &PackSlice)> {
        fn search<'a>(slices: &'a [PackSlice], id: &str) -> Option<&'a PackSlice> {
            slices.iter().find_map(|s| {
                if s.id == id {
                    Some(s)
                } else {
                    search(&s.children, id)
                }
            })
        }
        let slice = self
            .packs
            .iter()
            .find_map(|p| search(&p.slices, slice_id))?;
        let worker = self.packs.iter().find(|p| p.id == slice.worker);
        // A `choice` slice runs no worker itself; report its own pack.
        let worker = worker.or_else(|| {
            let pack_id = slice_id.split('.').next()?;
            self.packs.iter().find(|p| p.id == pack_id)
        })?;
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
    fn walk(slices: &[PackSlice], loaded: &HashSet<String>) -> Option<String> {
        slices.iter().find_map(|s| {
            if !s.worker.is_empty() && !loaded.contains(&s.worker) {
                Some(s.worker.clone())
            } else {
                walk(&s.children, loaded)
            }
        })
    }
    walk(&pack.slices, loaded)
}

/// What a child slice inherits from its parent when it leaves a field out.
struct Inherited {
    applies_to: Vec<String>,
    min_files: usize,
    max_files: Option<usize>,
}

/// Validates one slice (and, recursively, its children) and builds it. `prefix` is the id of
/// the parent ("pdf-light" for top-level slices), `depth` is 1 for the main ring.
fn build_slice(
    spec: SliceSpec,
    prefix: &str,
    depth: usize,
    parent: Option<&Inherited>,
    allowed_workers: &HashSet<&str>,
) -> Result<PackSlice, String> {
    let where_ = format!("Slice \"{}\"", spec.id);
    if !is_identifier(&spec.id) {
        return Err(format!(
            "{where_}: ids must use only lower-case letters, digits and dashes."
        ));
    }
    if spec.label.trim().is_empty() {
        return Err(format!("{where_} needs a label."));
    }
    let applies_to = match (spec.applies_to, parent) {
        (Some(list), _) if list.is_empty() => {
            return Err(format!(
                "{where_}: applies_to must list at least one extension."
            ))
        }
        (Some(list), _) => list
            .iter()
            .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
            .collect(),
        (None, Some(parent)) => parent.applies_to.clone(),
        (None, None) => {
            return Err(format!(
                "{where_}: applies_to must list at least one extension."
            ))
        }
    };
    let min_files = spec.min_files.or(parent.map(|p| p.min_files)).unwrap_or(1);
    let max_files = spec.max_files.or(parent.and_then(|p| p.max_files));
    if min_files == 0 || max_files.is_some_and(|max| max < min_files) {
        return Err(format!(
            "{where_}: min_files must be at least 1 and not above max_files."
        ));
    }
    if let Some(icon) = spec.icon.as_deref().filter(|i| !ICONS.contains(i)) {
        return Err(format!(
            "{where_}: icon \"{icon}\" is not one of {}.",
            ICONS.join(", ")
        ));
    }

    let id = format!("{prefix}.{}", spec.id);
    let mut slice = PackSlice {
        id: id.clone(),
        label: spec.label,
        icon: spec.icon,
        applies_to,
        min_files,
        max_files,
        mode: spec.mode,
        runs: spec.runs,
        ..PackSlice::default()
    };

    match spec.mode {
        Tier::Choice => {
            if spec.action.is_some() || spec.output.is_some() || spec.app.is_some() {
                return Err(format!(
                    "{where_}: a choice slice runs one of its children, so it has no action, output or app."
                ));
            }
            if depth >= MAX_DEPTH {
                return Err(format!(
                    "{where_}: slices can nest {MAX_DEPTH} levels deep at most."
                ));
            }
            if spec.children.is_empty() || spec.children.len() > MAX_CHILDREN {
                return Err(format!(
                    "{where_}: a choice slice needs 1 to {MAX_CHILDREN} children."
                ));
            }
            let inherited = Inherited {
                applies_to: slice.applies_to.clone(),
                min_files,
                max_files,
            };
            let mut seen = HashSet::new();
            for child in spec.children {
                if !seen.insert(child.id.clone()) {
                    return Err(format!(
                        "{where_}: child \"{}\" is defined twice.",
                        child.id
                    ));
                }
                slice.children.push(
                    build_slice(child, &id, depth + 1, Some(&inherited), allowed_workers)
                        .map_err(|e| format!("{where_}: {e}"))?,
                );
            }
            if let Some(default) = spec.default {
                if !seen.contains(&default) {
                    return Err(format!(
                        "{where_}: default \"{default}\" is not one of its children."
                    ));
                }
                slice.default_child = Some(format!("{id}.{default}"));
            }
        }
        Tier::Quick | Tier::App => {
            if !spec.children.is_empty() || spec.default.is_some() {
                return Err(format!(
                    "{where_}: only choice slices have children and a default."
                ));
            }
            match (spec.mode, &spec.app) {
                (Tier::App, None) => {
                    return Err(format!(
                        "{where_}: an app slice names its screen with app = \"...\"."
                    ))
                }
                (Tier::App, Some(screen)) if !APP_SCREENS.contains(&screen.as_str()) => {
                    return Err(format!(
                        "{where_}: app \"{screen}\" is not one of {}.",
                        APP_SCREENS.join(", ")
                    ))
                }
                (Tier::Quick, Some(_)) => {
                    return Err(format!("{where_}: app = \"...\" needs mode = \"app\"."))
                }
                _ => {}
            }
            let action = spec
                .action
                .ok_or_else(|| format!("{where_} needs an action."))?;
            let output = spec
                .output
                .ok_or_else(|| format!("{where_} needs an output name."))?;
            if !allowed_workers.contains(action.worker.as_str()) {
                return Err(format!(
                    "{where_} uses the \"{}\" worker; add it to requires_packs.",
                    action.worker
                ));
            }
            let n = output.contains("{n}").then_some("1");
            output_name(&output, Path::new("example.pdf"), n)
                .map_err(|e| format!("{where_}: output {e}"))?;
            slice.output = output;
            slice.worker = action.worker;
            slice.op = action.op;
            slice.args = match action.args {
                Some(table) => serde_json::to_value(table)
                    .map_err(|e| format!("{where_}: args are not valid ({e})."))?,
                None => serde_json::Value::Null,
            };
            slice.app = spec.app;
        }
    }
    Ok(slice)
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
        if !seen.insert(spec.id.clone()) {
            return Err(format!("Slice \"{}\" is defined twice.", spec.id));
        }
        slices.push(build_slice(spec, &manifest.id, 1, None, &allowed_workers)?);
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
/// Placeholders: `{stem}` (file name without extension), `{ext}` (extension without the dot) and,
/// for slices that make several files, `{n}` (the file's number, given as `n`).
/// The result must be a plain file name: no folders, so output always lands next to the input.
pub fn output_name(template: &str, input: &Path, n: Option<&str>) -> Result<String, String> {
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
            "n" => out.push_str(n.ok_or("uses {n}, which needs a file number.")?),
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
            (MERGE.replace("label = \"Merge\"", "label = \"Merge\"\nmode = \"pages\""), "unknown variant"),
            (MERGE.replace("label = \"Merge\"", "label = \"Merge\"\nicon = \"merge.svg\""), "is not one of"),
            (MERGE.replace("{stem} - merged", "{stem} - {page}"), "unknown placeholder"),
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
            applies_to: applies_to.iter().map(|s| s.to_string()).collect(),
            min_files: min,
            max_files: max,
            output: "{stem}.out".into(),
            worker: "p".into(),
            op: "s".into(),
            ..PackSlice::default()
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
            output_name("{stem} - merged.pdf", input, None).unwrap(),
            "Chapter 1 - merged.pdf"
        );
        assert_eq!(
            output_name("{stem}.{ext}.bak", input, None).unwrap(),
            "Chapter 1.pdf.bak"
        );
        assert!(output_name("{stem", input, None).is_err());
        assert!(output_name("sub/{stem}.pdf", input, None).is_err());
        assert!(output_name("{stem}:x", input, None).is_err());
        assert!(output_name("{ext}", Path::new("README"), None).is_err());

        let split = "{stem} - page {n}.pdf";
        assert_eq!(
            output_name(split, input, Some("07")).unwrap(),
            "Chapter 1 - page 07.pdf"
        );
        assert!(output_name(split, input, None).is_err());
    }

    #[test]
    fn runs_and_multi_output_slices_load() {
        let manifest = MERGE.to_string()
            + r#"
[[slices]]
id = "split"
label = "Split"
icon = "split"
applies_to = ["pdf"]
runs = "each"
output = "{stem} - page {n}.pdf"
action = { worker = "pdf-light", op = "split" }
"#;
        let catalog = load(&[("pdf-light", &manifest)]);
        assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
        let (_, merge) = catalog.find("pdf-light.merge").unwrap();
        assert_eq!(merge.runs, Runs::Once);
        assert_eq!(merge.mode, Tier::Quick);
        assert!(!merge.multi_output());
        let (_, split) = catalog.find("pdf-light.split").unwrap();
        assert_eq!(split.runs, Runs::Each);
        assert_eq!(split.icon.as_deref(), Some("split"));
        assert!(split.multi_output());
    }

    /// A pack with a choice slice (Rotate, with a default), an app slice and plain slices.
    const NESTED: &str = r#"
id = "pdf-light"
name = "PDF Light"
version = "1.0.0"
requires_core = ">=0.1"
license = "AGPL-3.0"

[worker]
kind = "native"
entry = "bin/pdf-light.exe"

[[slices]]
id = "rotate"
label = "Rotate"
icon = "rotate"
applies_to = ["pdf"]
mode = "choice"
default = "right"

  [[slices.children]]
  id = "right"
  label = "90° right"
  runs = "each"
  output = "{stem} - rotated.pdf"
  action = { worker = "pdf-light", op = "rotate", args = { degrees = 90 } }

  [[slices.children]]
  id = "left"
  label = "90° left"
  icon = "rotate-left"
  runs = "each"
  output = "{stem} - rotated.pdf"
  action = { worker = "pdf-light", op = "rotate", args = { degrees = 270 } }

  [[slices.children]]
  id = "pick"
  label = "Pick pages..."
  mode = "app"
  app = "rotate-pages"
  max_files = 1
  output = "{stem} - rotated.pdf"
  action = { worker = "pdf-light", op = "organize" }

[[slices]]
id = "remove"
label = "Remove pages"
icon = "remove-pages"
applies_to = ["pdf"]
max_files = 1
mode = "app"
app = "remove-pages"
output = "{stem} - edited.pdf"
action = { worker = "pdf-light", op = "organize" }
"#;

    #[test]
    fn choice_slices_load_with_children_defaults_and_args() {
        let catalog = load(&[("pdf-light", NESTED)]);
        assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
        let (pack, rotate) = catalog.find("pdf-light.rotate").unwrap();
        assert_eq!(pack.id, "pdf-light");
        assert_eq!(rotate.mode, Tier::Choice);
        assert_eq!(rotate.children.len(), 3);
        assert_eq!(
            rotate.default_child.as_deref(),
            Some("pdf-light.rotate.right")
        );
        assert!(rotate.op.is_empty() && rotate.output.is_empty());

        let (_, right) = catalog.find("pdf-light.rotate.right").unwrap();
        assert_eq!(right.args, serde_json::json!({ "degrees": 90 }));
        // Children inherit file types from their parent.
        assert_eq!(right.applies_to, ["pdf"]);

        let (_, pick) = catalog.find("pdf-light.rotate.pick").unwrap();
        assert_eq!(pick.mode, Tier::App);
        assert_eq!(pick.app.as_deref(), Some("rotate-pages"));
        assert_eq!(pick.max_files, Some(1));
    }

    #[test]
    fn tree_filters_children_by_selection_and_config() {
        let catalog = load(&[("pdf-light", NESTED)]);
        let tree_ids = |config: &SliceConfig, paths: &[&str]| -> Vec<(String, Vec<String>)> {
            catalog
                .tree_for(paths, config)
                .iter()
                .map(|a| {
                    let kids = a.children.iter().map(|c| c.slice.id.clone()).collect();
                    (a.slice.id.clone(), kids)
                })
                .collect()
        };
        let default = SliceConfig::default();
        // One PDF: everything applies.
        assert_eq!(
            tree_ids(&default, &["a.pdf"]),
            [
                (
                    "pdf-light.rotate".into(),
                    vec![
                        "pdf-light.rotate.right".to_string(),
                        "pdf-light.rotate.left".into(),
                        "pdf-light.rotate.pick".into()
                    ]
                ),
                ("pdf-light.remove".into(), vec![])
            ]
        );
        // Two PDFs: the one-file app children drop out.
        let two = tree_ids(&default, &["a.pdf", "b.pdf"]);
        assert_eq!(two.len(), 1);
        assert_eq!(two[0].1.len(), 2);

        // Children follow the user's order and can be turned off.
        let custom = SliceConfig {
            order: vec!["pdf-light.rotate.left".into()],
            disabled: vec!["pdf-light.rotate.pick".into()],
        };
        assert_eq!(
            tree_ids(&custom, &["a.pdf"])[0].1,
            ["pdf-light.rotate.left", "pdf-light.rotate.right"]
        );

        // A choice with every child turned off disappears.
        let none = SliceConfig {
            order: vec![],
            disabled: vec![
                "pdf-light.rotate.right".into(),
                "pdf-light.rotate.left".into(),
                "pdf-light.rotate.pick".into(),
            ],
        };
        assert_eq!(tree_ids(&none, &["a.pdf"]).len(), 1);
        assert_eq!(tree_ids(&none, &["a.pdf"])[0].0, "pdf-light.remove");
    }

    #[test]
    fn tier_rules_are_enforced() {
        let grandchild = NESTED.replace(
            "  [[slices.children]]\n  id = \"right\"\n  label = \"90° right\"\n  runs = \"each\"\n  output = \"{stem} - rotated.pdf\"\n  action = { worker = \"pdf-light\", op = \"rotate\", args = { degrees = 90 } }\n",
            "  [[slices.children]]\n  id = \"right\"\n  label = \"More\"\n  mode = \"choice\"\n    [[slices.children.children]]\n    id = \"deep\"\n    label = \"Deep\"\n    output = \"x.pdf\"\n    action = { worker = \"pdf-light\", op = \"x\" }\n",
        );
        let cases = [
            (grandchild, "nest 2 levels deep at most"),
            (
                NESTED.replace("default = \"right\"", "default = \"up\""),
                "is not one of its children",
            ),
            (
                NESTED.replace("app = \"remove-pages\"", "app = \"paint\""),
                "is not one of remove-pages",
            ),
            (
                NESTED.replace(
                    "mode = \"app\"\napp = \"remove-pages\"",
                    "app = \"remove-pages\"",
                ),
                "needs mode = \"app\"",
            ),
            (
                NESTED.replace(
                    "mode = \"app\"\napp = \"remove-pages\"\n",
                    "mode = \"app\"\n",
                ),
                "names its screen",
            ),
            (
                NESTED.replace(
                    "mode = \"choice\"\ndefault = \"right\"",
                    "mode = \"choice\"\ndefault = \"right\"\noutput = \"x.pdf\"",
                ),
                "has no action, output or app",
            ),
            (
                NESTED.replace("  id = \"left\"", "  id = \"right\""),
                "defined twice",
            ),
            (
                MERGE.replace("label = \"Merge\"", "label = \"Merge\"\nmode = \"choice\""),
                "has no action, output or app",
            ),
            (
                MERGE.replace("output = \"{stem} - merged.pdf\"\n", ""),
                "needs an output name",
            ),
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
    fn a_missing_worker_inside_children_drops_the_pack() {
        let manifest = NESTED
            .replace(
                "requires_core = \">=0.1\"",
                "requires_core = \">=0.1\"\nrequires_packs = [\"other\"]",
            )
            .replace(
                "action = { worker = \"pdf-light\", op = \"rotate\", args = { degrees = 270 } }",
                "action = { worker = \"other\", op = \"rotate\" }",
            );
        let catalog = load(&[("pdf-light", &manifest)]);
        assert!(only_error(&catalog).contains("Needs the \"other\" pack"));
    }
}

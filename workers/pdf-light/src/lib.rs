//! Flick's native PDF worker.
//!
//! The worker reads one JSON [`Request`] from stdin and writes one JSON [`Response`] to stdout.
//! It only ever writes to the `output` path it was given and refuses to touch an existing file,
//! so an original can never be overwritten, even if the core sends a bad request.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use lopdf::{dictionary, Document, Object, ObjectId};
use serde::{Deserialize, Serialize};

/// Page attributes a page may inherit from its ancestors in the page tree (PDF 32000-1, 7.7.3.4).
const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Merge {
        inputs: Vec<PathBuf>,
        output: PathBuf,
    },
    /// One file per page, written into `output_dir` as `1.pdf`, `2.pdf`, ...
    Split {
        inputs: Vec<PathBuf>,
        output_dir: PathBuf,
    },
    /// Pages turned clockwise by `args.degrees` (90 when left out): every page, or `args.pages`.
    Rotate {
        inputs: Vec<PathBuf>,
        output: PathBuf,
        #[serde(default)]
        args: RotateArgs,
    },
    /// The document without the pages in `args.ranges` (e.g. "1-3, 7").
    RemovePages {
        inputs: Vec<PathBuf>,
        output: PathBuf,
        args: RangesArgs,
    },
    /// A new document from `args.pages`, in that order, each with an extra turn: reorder,
    /// delete and rotate in one go.
    Organize {
        inputs: Vec<PathBuf>,
        output: PathBuf,
        args: OrganizeArgs,
    },
    /// One file per range in `args.ranges` (e.g. "1-3, 4-9, 10"), written into `output_dir` as
    /// `1.pdf`, `2.pdf`, ...
    SplitRanges {
        inputs: Vec<PathBuf>,
        output_dir: PathBuf,
        args: RangesArgs,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotateArgs {
    #[serde(default = "quarter_turn")]
    pub degrees: i64,
    /// 1-based page numbers; all pages when left out.
    #[serde(default)]
    pub pages: Option<Vec<u32>>,
}

impl Default for RotateArgs {
    fn default() -> Self {
        Self {
            degrees: 90,
            pages: None,
        }
    }
}

fn quarter_turn() -> i64 {
    90
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RangesArgs {
    /// Page ranges as people type them, e.g. "1-3, 7" or "5-" (5 to the end).
    pub ranges: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrganizeArgs {
    pub pages: Vec<PagePlan>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct PagePlan {
    /// 1-based page number in the input.
    pub page: u32,
    /// Extra clockwise turn: 0, 90, 180 or 270 (-90 is the same as 270).
    #[serde(default)]
    pub rotate: i64,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Response {
    Ok {
        ok: bool,
        output: PathBuf,
        pages: u32,
    },
    /// Several files, in order, all inside the request's `output_dir`.
    Many {
        ok: bool,
        outputs: Vec<PathBuf>,
        pages: u32,
    },
    Err {
        ok: bool,
        error: WorkerError,
    },
}

#[derive(Debug, Serialize, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct WorkerError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Serialize, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    NotFound,
    Encrypted,
    NoPages,
    Unreadable,
    OutputExists,
    WriteFailed,
}

impl WorkerError {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Runs a request and converts the result into the wire response.
pub fn handle(request: Request) -> Response {
    let result = match request {
        Request::Merge { inputs, output } => merge(&inputs, &output).map(|pages| Response::Ok {
            ok: true,
            output,
            pages,
        }),
        Request::Split { inputs, output_dir } => one_input(&inputs, "Split")
            .and_then(|input| split(input, &output_dir))
            .map(|outputs| Response::Many {
                ok: true,
                pages: outputs.len() as u32,
                outputs,
            }),
        Request::Rotate {
            inputs,
            output,
            args,
        } => one_input(&inputs, "Rotate")
            .and_then(|input| rotate_pages(input, &output, args.degrees, args.pages.as_deref()))
            .map(|pages| Response::Ok {
                ok: true,
                output,
                pages,
            }),
        Request::RemovePages {
            inputs,
            output,
            args,
        } => one_input(&inputs, "Remove pages")
            .and_then(|input| remove_pages(input, &output, &args.ranges))
            .map(|pages| Response::Ok {
                ok: true,
                output,
                pages,
            }),
        Request::Organize {
            inputs,
            output,
            args,
        } => one_input(&inputs, "Organize")
            .and_then(|input| organize(input, &output, &args.pages))
            .map(|pages| Response::Ok {
                ok: true,
                output,
                pages,
            }),
        Request::SplitRanges {
            inputs,
            output_dir,
            args,
        } => one_input(&inputs, "Split")
            .and_then(|input| split_ranges(input, &output_dir, &args.ranges))
            .map(|outputs| Response::Many {
                ok: true,
                pages: outputs.len() as u32,
                outputs,
            }),
    };
    result.unwrap_or_else(|error| Response::Err { ok: false, error })
}

/// Split and rotate work on one file per run; the core runs them once per selected file.
fn one_input<'a>(inputs: &'a [PathBuf], op: &str) -> Result<&'a Path, WorkerError> {
    match inputs {
        [input] => Ok(input),
        _ => Err(WorkerError::new(
            ErrorCode::BadRequest,
            format!("{op} takes one PDF at a time."),
        )),
    }
}

/// Merges `inputs` in order into a new file at `output` and returns the total page count.
///
/// Content streams are copied as they are, so text stays selectable.
pub fn merge(inputs: &[PathBuf], output: &Path) -> Result<u32, WorkerError> {
    if inputs.len() < 2 {
        return Err(WorkerError::new(
            ErrorCode::BadRequest,
            "Merge needs at least two PDFs.",
        ));
    }
    if inputs.iter().any(|input| same_file(input, output)) {
        return Err(WorkerError::new(
            ErrorCode::BadRequest,
            "The output path is one of the inputs.",
        ));
    }

    let mut merged = Document::with_version("1.7");
    let mut next_id = 1;
    let mut pages: Vec<ObjectId> = Vec::new();

    for input in inputs {
        let mut doc = load(input)?;
        flatten_inherited_attributes(&mut doc);
        doc.renumber_objects_with(next_id);
        next_id = doc.max_id + 1;

        pages.extend(doc.get_pages().into_values());
        for (id, object) in doc.objects {
            // Old page tree nodes and catalogs are replaced by a single new tree below.
            match object.type_name().unwrap_or(b"") {
                b"Catalog" | b"Pages" => {}
                _ => {
                    merged.objects.insert(id, object);
                }
            }
        }
    }

    merged.max_id = next_id - 1;
    let pages_id = merged.new_object_id();
    for page_id in &pages {
        if let Ok(page) = merged
            .get_object_mut(*page_id)
            .and_then(Object::as_dict_mut)
        {
            page.set("Parent", pages_id);
        }
    }
    let kids: Vec<Object> = pages.iter().map(|id| Object::Reference(*id)).collect();
    merged.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => pages.len() as i64,
        }),
    );
    let catalog_id = merged.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    merged.trailer.set("Root", catalog_id);

    // Drop objects only the old catalogs referenced (outlines, forms, structure trees).
    merged.prune_objects();
    merged.renumber_objects();
    merged.compress();

    write_new(&mut merged, output)?;
    Ok(pages.len() as u32)
}

/// Saves every page of `input` as its own PDF in `output_dir` (`1.pdf`, `2.pdf`, ...) and returns
/// the new files in page order. Each file keeps only what its page uses.
pub fn split(input: &Path, output_dir: &Path) -> Result<Vec<PathBuf>, WorkerError> {
    if !output_dir.is_dir() {
        return Err(WorkerError::new(
            ErrorCode::BadRequest,
            "The output folder does not exist.",
        ));
    }
    let mut doc = load(input)?;
    flatten_inherited_attributes(&mut doc);

    let mut outputs = Vec::new();
    for (number, page_id) in doc.get_pages() {
        let mut single = extract_pages(&doc, &[page_id]);
        let output = output_dir.join(format!("{number}.pdf"));
        write_new(&mut single, &output)?;
        outputs.push(output);
    }
    Ok(outputs)
}

/// A new document holding `page_ids` of `doc`, in that order, and the objects those pages use.
fn extract_pages(doc: &Document, page_ids: &[ObjectId]) -> Document {
    let mut single = Document::with_version(doc.version.clone());
    let wanted: BTreeSet<ObjectId> = page_ids.iter().copied().collect();
    let mut stack = page_ids.to_vec();
    let mut seen = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Ok(object) = doc.get_object(id) else {
            continue;
        };
        // Links can point at other pages; those (and the old page tree) stay behind.
        if !wanted.contains(&id) && matches!(object.type_name(), Ok(b"Page" | b"Pages")) {
            continue;
        }
        references(object, &mut stack);
        single.objects.insert(id, object.clone());
    }
    let kept: BTreeSet<ObjectId> = single.objects.keys().copied().collect();
    for object in single.objects.values_mut() {
        drop_missing_references(object, &kept);
    }

    single.max_id = doc.max_id;
    let pages_id = single.new_object_id();
    for &page_id in page_ids {
        if let Ok(page) = single.get_object_mut(page_id).and_then(Object::as_dict_mut) {
            page.set("Parent", pages_id);
        }
    }
    single.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.iter().map(|id| Object::Reference(*id)).collect::<Vec<_>>(),
            "Count" => page_ids.len() as i64,
        }),
    );
    let catalog_id = single.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    single.trailer.set("Root", catalog_id);
    single.renumber_objects();
    single.compress();
    single
}

/// Objects `object` refers to, except through `Parent` keys, which lead up the page tree (or a
/// form's field tree) to the rest of the file.
fn references(object: &Object, out: &mut Vec<ObjectId>) {
    let dict = match object {
        Object::Reference(id) => return out.push(*id),
        Object::Array(items) => return items.iter().for_each(|item| references(item, out)),
        Object::Dictionary(dict) => dict,
        Object::Stream(stream) => &stream.dict,
        _ => return,
    };
    for (key, value) in dict.iter() {
        if key != b"Parent" {
            references(value, out);
        }
    }
}

/// Replaces references to objects that were not copied with null. Readers treat a missing object
/// as null anyway; this makes it explicit.
fn drop_missing_references(object: &mut Object, kept: &BTreeSet<ObjectId>) {
    let dict = match object {
        Object::Reference(id) => {
            if !kept.contains(id) {
                *object = Object::Null;
            }
            return;
        }
        Object::Array(items) => {
            return items
                .iter_mut()
                .for_each(|item| drop_missing_references(item, kept))
        }
        Object::Dictionary(dict) => dict,
        Object::Stream(stream) => &mut stream.dict,
        _ => return,
    };
    for (_, value) in dict.iter_mut() {
        drop_missing_references(value, kept);
    }
}

/// Turns every page of `input` 90 degrees clockwise. See [`rotate_pages`].
pub fn rotate(input: &Path, output: &Path) -> Result<u32, WorkerError> {
    rotate_pages(input, output, 90, None)
}

/// Turns pages of `input` clockwise by `degrees` (90, 180, 270 or -90) on top of any rotation they
/// already have: every page, or only `pages` (1-based). Saves the result at `output` and returns
/// the page count. Nothing else in the file changes, so text stays selectable.
pub fn rotate_pages(
    input: &Path,
    output: &Path,
    degrees: i64,
    pages: Option<&[u32]>,
) -> Result<u32, WorkerError> {
    let turn = quarter_turns(degrees)?;
    refuse_overwrite(input, output)?;
    let mut doc = load(input)?;
    // Pages that inherit /Rotate (or their size and fonts) from the page tree get their own copy,
    // so the new value does not hide what they inherited.
    flatten_inherited_attributes(&mut doc);

    let all = doc.get_pages();
    let count = all.len() as u32;
    let chosen: BTreeSet<u32> = match pages {
        None => all.keys().copied().collect(),
        Some([]) => {
            return Err(WorkerError::new(
                ErrorCode::BadRequest,
                "Pick at least one page to rotate.",
            ))
        }
        Some(list) => {
            if let Some(bad) = list.iter().find(|&&p| p == 0 || p > count) {
                return Err(WorkerError::new(
                    ErrorCode::BadRequest,
                    format!("There is no page {bad}; the document has {count}."),
                ));
            }
            list.iter().copied().collect()
        }
    };
    for (number, id) in all {
        if chosen.contains(&number) {
            turn_page(&mut doc, id, turn);
        }
    }

    write_new(&mut doc, output)?;
    Ok(count)
}

/// The clockwise turn for a rotation in degrees, as 0, 90, 180 or 270.
fn quarter_turns(degrees: i64) -> Result<i64, WorkerError> {
    if degrees % 90 != 0 {
        return Err(WorkerError::new(
            ErrorCode::BadRequest,
            format!("Pages turn in steps of 90 degrees, not {degrees}."),
        ));
    }
    Ok(degrees.rem_euclid(360))
}

/// Adds `turn` degrees to a page's /Rotate.
fn turn_page(doc: &mut Document, id: ObjectId, turn: i64) {
    let current = doc
        .get_dictionary(id)
        .and_then(|page| page.get(b"Rotate"))
        .and_then(|value| doc.dereference(value))
        .and_then(|(_, value)| value.as_float())
        .unwrap_or(0.0);
    // /Rotate must be a multiple of 90; anything else is rounded to the nearest one.
    let current = (current / 90.0).round() as i64 * 90;
    if let Ok(page) = doc.get_object_mut(id).and_then(Object::as_dict_mut) {
        page.set("Rotate", (current + turn).rem_euclid(360));
    }
}

/// Parses page ranges as people type them: "1-3, 7", "2 4 6", "5-" (5 to the last page).
/// Numbers are 1-based and must exist in a document of `page_count` pages. Returns the ranges in
/// the order typed.
pub fn parse_ranges(
    text: &str,
    page_count: u32,
) -> Result<Vec<std::ops::RangeInclusive<u32>>, WorkerError> {
    let bad = |message: String| WorkerError::new(ErrorCode::BadRequest, message);
    let number = |part: &str, whole: &str| -> Result<u32, WorkerError> {
        let n: u32 = part
            .trim()
            .parse()
            .map_err(|_| bad(format!("\"{whole}\" is not a page or a range like 3-7.")))?;
        if n == 0 || n > page_count {
            return Err(bad(format!(
                "There is no page {n}; the document has {page_count}."
            )));
        }
        Ok(n)
    };
    let mut ranges = Vec::new();
    for part in text
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let range = match part.split_once('-') {
            None => {
                let n = number(part, part)?;
                n..=n
            }
            Some((from, "")) => number(from, part)?..=page_count,
            Some((from, to)) => {
                let (from, to) = (number(from, part)?, number(to, part)?);
                if to < from {
                    return Err(bad(format!(
                        "\"{part}\" runs backwards; write it as {to}-{from}."
                    )));
                }
                from..=to
            }
        };
        ranges.push(range);
    }
    if ranges.is_empty() {
        return Err(bad("Type the pages, for example 1-3, 7.".into()));
    }
    Ok(ranges)
}

/// Saves `input` without the pages in `ranges` and returns how many pages are left.
pub fn remove_pages(input: &Path, output: &Path, ranges: &str) -> Result<u32, WorkerError> {
    refuse_overwrite(input, output)?;
    let doc = load(input)?;
    let count = doc.get_pages().len() as u32;
    let removed: BTreeSet<u32> = parse_ranges(ranges, count)?.into_iter().flatten().collect();
    let keep: Vec<PagePlan> = (1..=count)
        .filter(|p| !removed.contains(p))
        .map(|page| PagePlan { page, rotate: 0 })
        .collect();
    if keep.is_empty() {
        return Err(WorkerError::new(
            ErrorCode::BadRequest,
            "That would remove every page. Keep at least one.",
        ));
    }
    rebuild(doc, output, &keep)
}

/// Saves a new document made of `plan`'s pages of `input`, in that order and each turned by its
/// `rotate`, and returns its page count. Pages left out are dropped.
pub fn organize(input: &Path, output: &Path, plan: &[PagePlan]) -> Result<u32, WorkerError> {
    refuse_overwrite(input, output)?;
    let doc = load(input)?;
    rebuild(doc, output, plan)
}

/// Replaces `doc`'s page tree with `plan`'s pages and saves it. Objects only removed pages used
/// are dropped; everything kept is copied as is, so text stays selectable.
fn rebuild(mut doc: Document, output: &Path, plan: &[PagePlan]) -> Result<u32, WorkerError> {
    let pages = doc.get_pages();
    let count = pages.len() as u32;
    if plan.is_empty() {
        return Err(WorkerError::new(
            ErrorCode::BadRequest,
            "Keep at least one page.",
        ));
    }
    let mut seen = BTreeSet::new();
    for step in plan {
        if step.page == 0 || step.page > count {
            return Err(WorkerError::new(
                ErrorCode::BadRequest,
                format!("There is no page {}; the document has {count}.", step.page),
            ));
        }
        if !seen.insert(step.page) {
            return Err(WorkerError::new(
                ErrorCode::BadRequest,
                format!("Page {} is listed twice.", step.page),
            ));
        }
        quarter_turns(step.rotate)?;
    }
    flatten_inherited_attributes(&mut doc);

    let pages_id = doc.new_object_id();
    let mut kids = Vec::with_capacity(plan.len());
    for step in plan {
        let id = pages[&step.page];
        turn_page(&mut doc, id, step.rotate.rem_euclid(360));
        if let Ok(page) = doc.get_object_mut(id).and_then(Object::as_dict_mut) {
            page.set("Parent", pages_id);
        }
        kids.push(Object::Reference(id));
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => plan.len() as i64,
        }),
    );
    let catalog = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .map_err(|_| WorkerError::new(ErrorCode::Unreadable, "The PDF has no catalog."))?;
    let catalog = doc
        .get_object_mut(catalog)
        .and_then(Object::as_dict_mut)
        .map_err(|_| WorkerError::new(ErrorCode::Unreadable, "The PDF has no catalog."))?;
    catalog.set("Pages", pages_id);
    // Outlines and named destinations can point at removed pages; drop them rather than keep
    // those pages alive in the file.
    catalog.remove(b"Outlines");
    catalog.remove(b"Dests");
    doc.prune_objects();
    doc.renumber_objects();
    doc.compress();
    write_new(&mut doc, output)?;
    Ok(plan.len() as u32)
}

/// Saves each range of `ranges` as its own PDF in `output_dir` (`1.pdf`, `2.pdf`, ...) and returns
/// the files in order.
pub fn split_ranges(
    input: &Path,
    output_dir: &Path,
    ranges: &str,
) -> Result<Vec<PathBuf>, WorkerError> {
    if !output_dir.is_dir() {
        return Err(WorkerError::new(
            ErrorCode::BadRequest,
            "The output folder does not exist.",
        ));
    }
    let mut doc = load(input)?;
    flatten_inherited_attributes(&mut doc);
    let pages = doc.get_pages();
    let ranges = parse_ranges(ranges, pages.len() as u32)?;
    let mut outputs = Vec::new();
    for (i, range) in ranges.into_iter().enumerate() {
        let ids: Vec<ObjectId> = range.map(|p| pages[&p]).collect();
        let mut part = extract_pages(&doc, &ids);
        let output = output_dir.join(format!("{}.pdf", i + 1));
        write_new(&mut part, &output)?;
        outputs.push(output);
    }
    Ok(outputs)
}

fn refuse_overwrite(input: &Path, output: &Path) -> Result<(), WorkerError> {
    if same_file(input, output) {
        return Err(WorkerError::new(
            ErrorCode::BadRequest,
            "The output path is the input.",
        ));
    }
    Ok(())
}

fn load(path: &Path) -> Result<Document, WorkerError> {
    let name = display_name(path);
    if !path.is_file() {
        return Err(WorkerError::new(
            ErrorCode::NotFound,
            format!("{name} was not found."),
        ));
    }
    let doc = Document::load(path).map_err(|e| {
        WorkerError::new(
            ErrorCode::Unreadable,
            format!("{name} could not be read as a PDF ({e})."),
        )
    })?;
    // lopdf removes /Encrypt after decrypting with an empty user password (owner-only
    // protection). It is still present when a real user password is required.
    if doc.is_encrypted() {
        return Err(WorkerError::new(
            ErrorCode::Encrypted,
            format!("{name} is password protected. Remove the password first."),
        ));
    }
    if doc.get_pages().is_empty() {
        return Err(WorkerError::new(
            ErrorCode::NoPages,
            format!("{name} has no pages."),
        ));
    }
    Ok(doc)
}

/// Copies inheritable attributes from ancestor `Pages` nodes onto each page, so pages keep
/// their size, rotation and fonts after they are moved into a new, flat page tree.
fn flatten_inherited_attributes(doc: &mut Document) {
    let mut updates: BTreeMap<ObjectId, Vec<(&[u8], Object)>> = BTreeMap::new();
    for page_id in doc.get_pages().into_values() {
        let Ok(page) = doc.get_dictionary(page_id) else {
            continue;
        };
        let mut missing: Vec<&[u8]> = INHERITABLE
            .iter()
            .copied()
            .filter(|key| !page.has(key))
            .collect();
        let mut parent = page.get(b"Parent").and_then(Object::as_reference).ok();
        let mut found = Vec::new();
        // Bounded walk: a malformed file could contain a Parent cycle.
        for _ in 0..64 {
            let Some(node) = parent.and_then(|id| doc.get_dictionary(id).ok()) else {
                break;
            };
            missing.retain(|key| match node.get(key) {
                Ok(value) => {
                    found.push((*key, value.clone()));
                    false
                }
                Err(_) => true,
            });
            if missing.is_empty() {
                break;
            }
            parent = node.get(b"Parent").and_then(Object::as_reference).ok();
        }
        if !found.is_empty() {
            updates.insert(page_id, found);
        }
    }
    for (page_id, values) in updates {
        if let Ok(page) = doc.get_object_mut(page_id).and_then(Object::as_dict_mut) {
            for (key, value) in values {
                page.set(key, value);
            }
        }
    }
}

/// Saves to a path that must not exist yet. `create_new` makes the check and the create atomic.
fn write_new(doc: &mut Document, output: &Path) -> Result<(), WorkerError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                WorkerError::new(
                    ErrorCode::OutputExists,
                    format!("{} already exists.", display_name(output)),
                )
            } else {
                WorkerError::new(
                    ErrorCode::WriteFailed,
                    format!("Could not create {} ({e}).", display_name(output)),
                )
            }
        })?;
    let mut writer = BufWriter::new(file);
    let result = doc
        .save_to(&mut writer)
        .map_err(|e| e.to_string())
        .and_then(|_| writer.into_inner().map_err(|e| e.to_string()))
        .and_then(|file| file.sync_all().map_err(|e| e.to_string()));
    if let Err(e) = result {
        let _ = std::fs::remove_file(output);
        return Err(WorkerError::new(
            ErrorCode::WriteFailed,
            format!("Could not write {} ({e}).", display_name(output)),
        ));
    }
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

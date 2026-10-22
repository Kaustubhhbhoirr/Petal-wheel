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
    /// Every page turned 90 degrees clockwise.
    Rotate {
        inputs: Vec<PathBuf>,
        output: PathBuf,
    },
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
        Request::Rotate { inputs, output } => one_input(&inputs, "Rotate")
            .and_then(|input| rotate(input, &output))
            .map(|pages| Response::Ok {
                ok: true,
                output,
                pages,
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
        let mut single = single_page(&doc, page_id);
        let output = output_dir.join(format!("{number}.pdf"));
        write_new(&mut single, &output)?;
        outputs.push(output);
    }
    Ok(outputs)
}

/// A new document holding one page of `doc` and the objects that page uses.
fn single_page(doc: &Document, page_id: ObjectId) -> Document {
    let mut single = Document::with_version(doc.version.clone());
    let mut stack = vec![page_id];
    let mut seen = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Ok(object) = doc.get_object(id) else {
            continue;
        };
        // Links can point at other pages; those (and the old page tree) stay behind.
        if id != page_id && matches!(object.type_name(), Ok(b"Page" | b"Pages")) {
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
    if let Ok(page) = single.get_object_mut(page_id).and_then(Object::as_dict_mut) {
        page.set("Parent", pages_id);
    }
    single.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
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

/// Turns every page of `input` 90 degrees clockwise on top of any rotation it already has, saves
/// the result at `output` and returns the page count. Nothing else in the file changes.
pub fn rotate(input: &Path, output: &Path) -> Result<u32, WorkerError> {
    if same_file(input, output) {
        return Err(WorkerError::new(
            ErrorCode::BadRequest,
            "The output path is the input.",
        ));
    }
    let mut doc = load(input)?;
    // Pages that inherit /Rotate (or their size and fonts) from the page tree get their own copy,
    // so the new value does not hide what they inherited.
    flatten_inherited_attributes(&mut doc);

    let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
    let turned: Vec<(ObjectId, i64)> = pages
        .iter()
        .map(|&id| {
            let current = doc
                .get_dictionary(id)
                .and_then(|page| page.get(b"Rotate"))
                .and_then(|value| doc.dereference(value))
                .and_then(|(_, value)| value.as_float())
                .unwrap_or(0.0);
            // /Rotate must be a multiple of 90; anything else is rounded to the nearest one.
            let current = (current / 90.0).round() as i64 * 90;
            (id, (current + 90).rem_euclid(360))
        })
        .collect();
    for (id, rotate) in turned {
        if let Ok(page) = doc.get_object_mut(id).and_then(Object::as_dict_mut) {
            page.set("Rotate", rotate);
        }
    }

    write_new(&mut doc, output)?;
    Ok(pages.len() as u32)
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

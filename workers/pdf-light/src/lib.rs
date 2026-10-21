//! Flick's native PDF worker.
//!
//! The worker reads one JSON [`Request`] from stdin and writes one JSON [`Response`] to stdout.
//! It only ever writes to the `output` path it was given and refuses to touch an existing file,
//! so an original can never be overwritten, even if the core sends a bad request.

use std::collections::BTreeMap;
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
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Response {
    Ok {
        ok: bool,
        output: PathBuf,
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
        Request::Merge { inputs, output } => merge(&inputs, &output).map(|pages| (output, pages)),
    };
    match result {
        Ok((output, pages)) => Response::Ok {
            ok: true,
            output,
            pages,
        },
        Err(error) => Response::Err { ok: false, error },
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

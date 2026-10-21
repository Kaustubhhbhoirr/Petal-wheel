use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use lopdf::content::{Content, Operation};
use lopdf::{
    dictionary, Document, EncryptionState, EncryptionVersion, Object, Permissions, Stream,
};
use pdf_light::{merge, ErrorCode};
use tempfile::TempDir;

/// Builds a PDF with one page per label. Each page shows its label as real text.
///
/// With `inherit` set, fonts and page size live on a nested `Pages` node instead of the pages,
/// which is common in real files and easy to lose when merging.
fn sample(labels: &[&str], inherit: bool) -> Document {
    let mut doc = Document::with_version("1.5");
    let root_pages = doc.new_object_id();
    let inner_pages = doc.new_object_id();
    let font = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
    let media_box: Vec<Object> = vec![0.into(), 0.into(), 612.into(), 792.into()];

    let mut kids = Vec::new();
    for label in labels {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![72.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal(*label)]),
                Operation::new("ET", vec![]),
            ],
        };
        let contents = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let mut page = dictionary! {
            "Type" => "Page",
            "Parent" => if inherit { inner_pages } else { root_pages },
            "Contents" => contents,
        };
        if !inherit {
            page.set("Resources", resources);
            page.set("MediaBox", media_box.clone());
        }
        kids.push(Object::Reference(doc.add_object(page)));
    }

    let count = labels.len() as i64;
    if inherit {
        doc.objects.insert(
            inner_pages,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Parent" => root_pages, "Kids" => kids, "Count" => count,
            }),
        );
        doc.objects.insert(
            root_pages,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(inner_pages)],
                "Count" => count,
                "Resources" => resources,
                "MediaBox" => media_box,
                "Rotate" => 90,
            }),
        );
    } else {
        doc.objects.insert(
            root_pages,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => kids, "Count" => count,
            }),
        );
    }
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => root_pages });
    doc.trailer.set("Root", catalog);
    doc
}

fn save(dir: &TempDir, name: &str, doc: &mut Document) -> PathBuf {
    let path = dir.path().join(name);
    doc.save(&path).unwrap();
    path
}

fn encrypted(dir: &TempDir, name: &str, user_password: &str) -> PathBuf {
    let mut doc = sample(&["Secret"], false);
    // The standard security handler derives its key from the file ID.
    let id = Object::string_literal(b"flick-test-id-01".to_vec());
    doc.trailer.set("ID", vec![id.clone(), id]);
    let state = EncryptionState::try_from(EncryptionVersion::V2 {
        document: &doc,
        owner_password: "owner",
        user_password,
        key_length: 128,
        permissions: Permissions::PRINTABLE,
    })
    .unwrap();
    doc.encrypt(&state).unwrap();
    save(dir, name, &mut doc)
}

fn page_texts(path: &Path) -> Vec<String> {
    let doc = Document::load(path).unwrap();
    let count = doc.get_pages().len() as u32;
    (1..=count)
        .map(|n| doc.extract_text(&[n]).unwrap().trim().to_string())
        .collect()
}

#[test]
fn merges_in_order_and_keeps_text() {
    let dir = TempDir::new().unwrap();
    let a = save(&dir, "a.pdf", &mut sample(&["Alpha 1", "Alpha 2"], false));
    let b = save(&dir, "b.pdf", &mut sample(&["Beta 1"], false));
    let out = dir.path().join("out.pdf");

    assert_eq!(merge(&[a, b], &out).unwrap(), 3);
    assert_eq!(page_texts(&out), ["Alpha 1", "Alpha 2", "Beta 1"]);
}

#[test]
fn keeps_inherited_page_attributes() {
    let dir = TempDir::new().unwrap();
    let a = save(&dir, "a.pdf", &mut sample(&["Nested 1", "Nested 2"], true));
    let b = save(&dir, "b.pdf", &mut sample(&["Flat"], false));
    let out = dir.path().join("out.pdf");

    merge(&[a, b], &out).unwrap();

    let doc = Document::load(&out).unwrap();
    let first = doc.get_dictionary(doc.get_pages()[&1]).unwrap();
    assert!(first.has(b"MediaBox"));
    assert!(first.has(b"Resources"));
    assert_eq!(first.get(b"Rotate").unwrap().as_i64().unwrap(), 90);
    assert_eq!(page_texts(&out), ["Nested 1", "Nested 2", "Flat"]);
}

#[test]
fn rejects_password_protected_input() {
    let dir = TempDir::new().unwrap();
    let a = save(&dir, "a.pdf", &mut sample(&["Open"], false));
    let locked = encrypted(&dir, "locked.pdf", "secret");
    let out = dir.path().join("out.pdf");

    let err = merge(&[a, locked], &out).unwrap_err();
    assert_eq!(err.code, ErrorCode::Encrypted);
    assert!(err.message.contains("locked.pdf"));
    assert!(!out.exists());
}

#[test]
fn merges_owner_only_protected_input() {
    // No user password: anyone can open it, so Flick can too.
    let dir = TempDir::new().unwrap();
    let a = save(&dir, "a.pdf", &mut sample(&["Open"], false));
    let restricted = encrypted(&dir, "restricted.pdf", "");
    let out = dir.path().join("out.pdf");

    assert_eq!(merge(&[a, restricted], &out).unwrap(), 2);
    assert_eq!(page_texts(&out), ["Open", "Secret"]);
}

#[test]
fn rejects_zero_page_input() {
    let dir = TempDir::new().unwrap();
    let a = save(&dir, "a.pdf", &mut sample(&["Only"], false));
    let empty = save(&dir, "empty.pdf", &mut sample(&[], false));
    let out = dir.path().join("out.pdf");

    let err = merge(&[a, empty], &out).unwrap_err();
    assert_eq!(err.code, ErrorCode::NoPages);
    assert!(!out.exists());
}

#[test]
fn rejects_non_pdf_and_missing_input() {
    let dir = TempDir::new().unwrap();
    let a = save(&dir, "a.pdf", &mut sample(&["Only"], false));
    let junk = dir.path().join("junk.pdf");
    std::fs::write(&junk, b"this is not a pdf").unwrap();
    let out = dir.path().join("out.pdf");

    let err = merge(&[a.clone(), junk], &out).unwrap_err();
    assert_eq!(err.code, ErrorCode::Unreadable);

    let err = merge(&[a, dir.path().join("missing.pdf")], &out).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(!out.exists());
}

#[test]
fn never_overwrites_an_existing_file() {
    let dir = TempDir::new().unwrap();
    let a = save(&dir, "a.pdf", &mut sample(&["A"], false));
    let b = save(&dir, "b.pdf", &mut sample(&["B"], false));
    let original = std::fs::read(&a).unwrap();

    let err = merge(&[a.clone(), b.clone()], &a).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);

    let existing = dir.path().join("existing.pdf");
    std::fs::write(&existing, b"keep me").unwrap();
    let err = merge(&[a.clone(), b], &existing).unwrap_err();
    assert_eq!(err.code, ErrorCode::OutputExists);

    assert_eq!(std::fs::read(&a).unwrap(), original);
    assert_eq!(std::fs::read(&existing).unwrap(), b"keep me");
}

#[test]
fn needs_two_inputs() {
    let dir = TempDir::new().unwrap();
    let a = save(&dir, "a.pdf", &mut sample(&["A"], false));
    let err = merge(&[a], &dir.path().join("out.pdf")).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
}

fn run_worker(stdin: &str) -> serde_json::Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pdf-light"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn speaks_json_over_stdio() {
    let dir = TempDir::new().unwrap();
    let a = save(&dir, "a.pdf", &mut sample(&["A"], false));
    let b = save(&dir, "b.pdf", &mut sample(&["B"], false));
    let out = dir.path().join("out.pdf");
    let request = serde_json::json!({ "op": "merge", "inputs": [a, b], "output": out });

    let response = run_worker(&request.to_string());
    assert_eq!(response["ok"], true);
    assert_eq!(response["pages"], 2);
    assert!(out.exists());

    let response = run_worker("{ not json");
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "bad_request");
}

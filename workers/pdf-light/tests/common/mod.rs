//! Sample PDFs and helpers shared by the worker tests.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use lopdf::content::{Content, Operation};
use lopdf::{
    dictionary, Document, EncryptionState, EncryptionVersion, Object, Permissions, Stream,
};
use tempfile::TempDir;

/// Builds a PDF with one page per label. Each page shows its label as real text.
///
/// With `inherit` set, fonts and page size live on a nested `Pages` node instead of the pages,
/// which is common in real files and easy to lose when merging.
pub fn sample(labels: &[&str], inherit: bool) -> Document {
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

pub fn save(dir: &TempDir, name: &str, doc: &mut Document) -> PathBuf {
    let path = dir.path().join(name);
    doc.save(&path).unwrap();
    path
}

pub fn encrypted(dir: &TempDir, name: &str, user_password: &str) -> PathBuf {
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

pub fn page_texts(path: &Path) -> Vec<String> {
    let doc = Document::load(path).unwrap();
    let count = doc.get_pages().len() as u32;
    (1..=count)
        .map(|n| doc.extract_text(&[n]).unwrap().trim().to_string())
        .collect()
}

pub fn run_worker(stdin: &str) -> serde_json::Value {
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

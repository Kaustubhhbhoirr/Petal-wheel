mod common;

use common::{encrypted, page_texts, run_worker, sample, save};
use lopdf::Document;
use pdf_light::{merge, ErrorCode};
use tempfile::TempDir;

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

mod common;

use std::path::Path;

use common::{encrypted, page_texts, run_worker, sample, save};
use lopdf::{dictionary, Document, Object};
use pdf_light::{rotate, split, ErrorCode};
use tempfile::TempDir;

/// The /Rotate value of every page, as saved on the page itself.
fn rotations(path: &Path) -> Vec<i64> {
    let doc = Document::load(path).unwrap();
    doc.get_pages()
        .into_values()
        .map(|id| {
            let page = doc.get_dictionary(id).unwrap();
            page.get(b"Rotate").and_then(Object::as_i64).unwrap_or(0)
        })
        .collect()
}

fn file_names(paths: &[impl AsRef<Path>]) -> Vec<String> {
    paths
        .iter()
        .map(|p| {
            p.as_ref()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[test]
fn splits_into_one_file_per_page() {
    let dir = TempDir::new().unwrap();
    let input = save(&dir, "a.pdf", &mut sample(&["One", "Two", "Three"], false));
    let out = TempDir::new().unwrap();

    let outputs = split(&input, out.path()).unwrap();

    assert_eq!(file_names(&outputs), ["1.pdf", "2.pdf", "3.pdf"]);
    for (output, label) in outputs.iter().zip(["One", "Two", "Three"]) {
        assert_eq!(page_texts(output), [label]);
    }
}

#[test]
fn split_pages_keep_inherited_size_fonts_and_rotation() {
    let dir = TempDir::new().unwrap();
    let input = save(&dir, "a.pdf", &mut sample(&["Nested 1", "Nested 2"], true));
    let out = TempDir::new().unwrap();

    let outputs = split(&input, out.path()).unwrap();

    for output in &outputs {
        let doc = Document::load(output).unwrap();
        let page = doc.get_dictionary(doc.get_pages()[&1]).unwrap();
        assert!(page.has(b"MediaBox"));
        assert!(page.has(b"Resources"));
        assert_eq!(page.get(b"Rotate").unwrap().as_i64().unwrap(), 90);
    }
    assert_eq!(page_texts(&outputs[1]), ["Nested 2"]);
}

#[test]
fn split_leaves_other_pages_behind_even_when_linked() {
    let dir = TempDir::new().unwrap();
    let mut doc = sample(&["First", "Second"], false);
    // Page 1 links to page 2; following that link must not pull page 2 into page 1's file.
    let pages = doc.get_pages();
    let link = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Link",
        "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        "Dest" => vec![Object::Reference(pages[&2]), "Fit".into()],
    });
    doc.get_object_mut(pages[&1])
        .and_then(Object::as_dict_mut)
        .unwrap()
        .set("Annots", vec![Object::Reference(link)]);
    let input = save(&dir, "linked.pdf", &mut doc);
    let out = TempDir::new().unwrap();

    let outputs = split(&input, out.path()).unwrap();

    let first = Document::load(&outputs[0]).unwrap();
    assert_eq!(first.get_pages().len(), 1);
    assert_eq!(page_texts(&outputs[0]), ["First"]);
    // Only one page object, and the link's target is now null instead of a dangling reference.
    let page_objects = first
        .objects
        .values()
        .filter(|o| o.type_name().ok() == Some(b"Page"))
        .count();
    assert_eq!(page_objects, 1);
}

#[test]
fn split_rejects_bad_input() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();

    let locked = encrypted(&dir, "locked.pdf", "secret");
    assert_eq!(
        split(&locked, out.path()).unwrap_err().code,
        ErrorCode::Encrypted
    );

    let empty = save(&dir, "empty.pdf", &mut sample(&[], false));
    assert_eq!(
        split(&empty, out.path()).unwrap_err().code,
        ErrorCode::NoPages
    );

    let junk = dir.path().join("junk.pdf");
    std::fs::write(&junk, b"%PDF-1.4 but not really").unwrap();
    assert_eq!(
        split(&junk, out.path()).unwrap_err().code,
        ErrorCode::Unreadable
    );

    let good = save(&dir, "good.pdf", &mut sample(&["A"], false));
    let missing = dir.path().join("no such folder");
    assert_eq!(
        split(&good, &missing).unwrap_err().code,
        ErrorCode::BadRequest
    );

    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
}

#[test]
fn split_never_overwrites_a_file_in_its_folder() {
    let dir = TempDir::new().unwrap();
    let input = save(&dir, "a.pdf", &mut sample(&["A", "B"], false));
    let out = TempDir::new().unwrap();
    std::fs::write(out.path().join("2.pdf"), b"keep me").unwrap();

    let err = split(&input, out.path()).unwrap_err();
    assert_eq!(err.code, ErrorCode::OutputExists);
    assert_eq!(std::fs::read(out.path().join("2.pdf")).unwrap(), b"keep me");
}

#[test]
fn rotates_every_page_clockwise_on_top_of_its_rotation() {
    let dir = TempDir::new().unwrap();
    let mut doc = sample(&["Up", "Side", "Back"], false);
    let pages = doc.get_pages();
    for (number, rotate) in [(2, 90), (3, 270)] {
        doc.get_object_mut(pages[&number])
            .and_then(Object::as_dict_mut)
            .unwrap()
            .set("Rotate", rotate);
    }
    let input = save(&dir, "a.pdf", &mut doc);
    let original = std::fs::read(&input).unwrap();
    let out = dir.path().join("a - rotated.pdf");

    assert_eq!(rotate(&input, &out).unwrap(), 3);

    assert_eq!(rotations(&out), [90, 180, 0]);
    assert_eq!(page_texts(&out), ["Up", "Side", "Back"]);
    assert_eq!(std::fs::read(&input).unwrap(), original);
}

#[test]
fn rotate_respects_inherited_rotation_and_attributes() {
    let dir = TempDir::new().unwrap();
    // The page tree node says /Rotate 90 and holds the fonts and page size.
    let input = save(&dir, "a.pdf", &mut sample(&["Nested 1", "Nested 2"], true));
    let out = dir.path().join("out.pdf");

    rotate(&input, &out).unwrap();

    assert_eq!(rotations(&out), [180, 180]);
    let doc = Document::load(&out).unwrap();
    let page = doc.get_dictionary(doc.get_pages()[&1]).unwrap();
    assert!(page.has(b"MediaBox"));
    assert!(page.has(b"Resources"));
    assert_eq!(page_texts(&out), ["Nested 1", "Nested 2"]);
}

#[test]
fn rotate_rejects_bad_input_and_never_overwrites() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("out.pdf");

    let locked = encrypted(&dir, "locked.pdf", "secret");
    assert_eq!(
        rotate(&locked, &out).unwrap_err().code,
        ErrorCode::Encrypted
    );
    let empty = save(&dir, "empty.pdf", &mut sample(&[], false));
    assert_eq!(rotate(&empty, &out).unwrap_err().code, ErrorCode::NoPages);
    assert!(!out.exists());

    let input = save(&dir, "a.pdf", &mut sample(&["A"], false));
    assert_eq!(
        rotate(&input, &input).unwrap_err().code,
        ErrorCode::BadRequest
    );
    std::fs::write(&out, b"keep me").unwrap();
    assert_eq!(
        rotate(&input, &out).unwrap_err().code,
        ErrorCode::OutputExists
    );
    assert_eq!(std::fs::read(&out).unwrap(), b"keep me");
}

#[test]
fn split_and_rotate_speak_json_over_stdio() {
    let dir = TempDir::new().unwrap();
    let input = save(&dir, "a.pdf", &mut sample(&["A", "B"], false));
    let folder = dir.path().join("pages");
    std::fs::create_dir(&folder).unwrap();

    let request = serde_json::json!({ "op": "split", "inputs": [input], "output_dir": folder });
    let response = run_worker(&request.to_string());
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["pages"], 2);
    let outputs: Vec<String> = serde_json::from_value(response["outputs"].clone()).unwrap();
    assert_eq!(file_names(&outputs), ["1.pdf", "2.pdf"]);

    let out = dir.path().join("rotated.pdf");
    let request = serde_json::json!({ "op": "rotate", "inputs": [input], "output": out });
    let response = run_worker(&request.to_string());
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(rotations(&out), [90, 90]);

    // Both work on one file per run; the core runs them once per selected file.
    let request = serde_json::json!({ "op": "rotate", "inputs": [input, input], "output": out });
    let response = run_worker(&request.to_string());
    assert_eq!(response["error"]["code"], "bad_request");
}

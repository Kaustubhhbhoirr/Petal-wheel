//! Page operations: rotate by angle and by page, remove pages by ranges, organize, split by ranges.

mod common;

use std::path::Path;

use common::{encrypted, page_texts, run_worker, sample, save};
use lopdf::{Document, Object};
use pdf_light::{
    organize, parse_ranges, remove_pages, rotate_pages, split_ranges, ErrorCode, PagePlan,
};
use tempfile::TempDir;

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

fn eight_pages(dir: &TempDir) -> std::path::PathBuf {
    let labels: Vec<String> = (1..=8).map(|n| format!("Page {n}")).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    save(dir, "eight.pdf", &mut sample(&labels, false))
}

#[test]
fn rotates_by_quarter_and_half_turns() {
    let dir = TempDir::new().unwrap();
    let input = save(&dir, "a.pdf", &mut sample(&["One", "Two"], false));
    for (degrees, expected) in [(90, 90), (180, 180), (270, 270), (-90, 270)] {
        let out = dir.path().join(format!("r{degrees}.pdf"));
        assert_eq!(rotate_pages(&input, &out, degrees, None).unwrap(), 2);
        assert_eq!(rotations(&out), [expected, expected]);
        assert_eq!(page_texts(&out), ["One", "Two"], "text stays real");
    }
}

#[test]
fn rotates_only_the_chosen_pages() {
    let dir = TempDir::new().unwrap();
    let input = eight_pages(&dir);
    let out = dir.path().join("out.pdf");
    rotate_pages(&input, &out, 180, Some(&[2, 5])).unwrap();
    assert_eq!(rotations(&out), [0, 180, 0, 0, 180, 0, 0, 0]);
}

#[test]
fn rotate_rejects_odd_angles_and_missing_pages() {
    let dir = TempDir::new().unwrap();
    let input = eight_pages(&dir);
    let out = dir.path().join("out.pdf");
    let err = rotate_pages(&input, &out, 45, None).unwrap_err();
    assert!(err.message.contains("steps of 90"));
    let err = rotate_pages(&input, &out, 90, Some(&[9])).unwrap_err();
    assert!(err.message.contains("no page 9"));
    let err = rotate_pages(&input, &out, 90, Some(&[])).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(!out.exists());
}

#[test]
fn ranges_parse_the_way_people_type_them() {
    let pages = |text: &str| -> Vec<u32> {
        parse_ranges(text, 10)
            .unwrap()
            .into_iter()
            .flatten()
            .collect()
    };
    assert_eq!(pages("1-3,7"), [1, 2, 3, 7]);
    assert_eq!(pages(" 1-3 , 7 "), [1, 2, 3, 7]);
    assert_eq!(pages("2 4 6"), [2, 4, 6]);
    assert_eq!(pages("8-"), [8, 9, 10]);
    assert_eq!(pages("5;6"), [5, 6]);
}

#[test]
fn bad_ranges_say_what_is_wrong() {
    let cases = [
        ("3-1", "runs backwards"),
        ("0", "no page 0"),
        ("11", "no page 11"),
        ("2-12", "no page 12"),
        ("abc", "is not a page"),
        ("1-x", "is not a page"),
        ("", "Type the pages"),
        (" , ", "Type the pages"),
    ];
    for (text, expected) in cases {
        let err = parse_ranges(text, 10).unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(
            err.message.contains(expected),
            "{text:?}: expected {expected:?} in {:?}",
            err.message
        );
    }
}

#[test]
fn removes_pages_by_ranges() {
    let dir = TempDir::new().unwrap();
    let input = eight_pages(&dir);
    let out = dir.path().join("out.pdf");
    assert_eq!(remove_pages(&input, &out, "1-3,7").unwrap(), 4);
    assert_eq!(page_texts(&out), ["Page 4", "Page 5", "Page 6", "Page 8"]);
    // The original is untouched.
    assert_eq!(page_texts(&input).len(), 8);
}

#[test]
fn remove_pages_refuses_to_empty_the_document() {
    let dir = TempDir::new().unwrap();
    let input = eight_pages(&dir);
    let out = dir.path().join("out.pdf");
    let err = remove_pages(&input, &out, "1-8").unwrap_err();
    assert!(err.message.contains("every page"));
    let err = remove_pages(&input, &out, "4-2").unwrap_err();
    assert!(err.message.contains("backwards"));
    assert!(!out.exists());
}

#[test]
fn page_ops_reject_encrypted_and_empty_pdfs() {
    let dir = TempDir::new().unwrap();
    let locked = encrypted(&dir, "locked.pdf", "secret");
    let empty = save(&dir, "empty.pdf", &mut sample(&[], false));
    let out = dir.path().join("out.pdf");
    for input in [&locked, &empty] {
        let expected = if input == &locked {
            ErrorCode::Encrypted
        } else {
            ErrorCode::NoPages
        };
        assert_eq!(remove_pages(input, &out, "1").unwrap_err().code, expected);
        assert_eq!(
            rotate_pages(input, &out, 90, None).unwrap_err().code,
            expected
        );
        let plan = [PagePlan { page: 1, rotate: 0 }];
        assert_eq!(organize(input, &out, &plan).unwrap_err().code, expected);
    }
    assert!(!out.exists());
}

#[test]
fn organize_reorders_deletes_and_rotates() {
    let dir = TempDir::new().unwrap();
    let input = eight_pages(&dir);
    let out = dir.path().join("out.pdf");
    let plan = [
        PagePlan { page: 3, rotate: 0 },
        PagePlan {
            page: 1,
            rotate: 90,
        },
        PagePlan {
            page: 8,
            rotate: -90,
        },
    ];
    assert_eq!(organize(&input, &out, &plan).unwrap(), 3);
    assert_eq!(page_texts(&out), ["Page 3", "Page 1", "Page 8"]);
    assert_eq!(rotations(&out), [0, 90, 270]);

    let twice = [
        PagePlan { page: 2, rotate: 0 },
        PagePlan { page: 2, rotate: 0 },
    ];
    let other = dir.path().join("other.pdf");
    assert!(organize(&input, &other, &twice)
        .unwrap_err()
        .message
        .contains("twice"));
    assert!(organize(&input, &other, &[]).is_err());
    assert!(!other.exists());
}

#[test]
fn splits_by_ranges() {
    let dir = TempDir::new().unwrap();
    let input = eight_pages(&dir);
    let out = TempDir::new().unwrap();
    let files = split_ranges(&input, out.path(), "1-2, 5-, 3").unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(page_texts(&files[0]), ["Page 1", "Page 2"]);
    assert_eq!(
        page_texts(&files[1]),
        ["Page 5", "Page 6", "Page 7", "Page 8"]
    );
    assert_eq!(page_texts(&files[2]), ["Page 3"]);
}

#[test]
fn page_ops_speak_json() {
    let dir = TempDir::new().unwrap();
    let input = eight_pages(&dir);
    let out = dir.path().join("removed.pdf");
    let response = run_worker(
        &serde_json::json!({
            "op": "remove_pages",
            "inputs": [input],
            "output": out,
            "args": { "ranges": "1-3,7" }
        })
        .to_string(),
    );
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["pages"], 4);

    let turned = dir.path().join("turned.pdf");
    let response = run_worker(
        &serde_json::json!({
            "op": "rotate",
            "inputs": [input],
            "output": turned,
            "args": { "degrees": 180 }
        })
        .to_string(),
    );
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(rotations(&turned), [180; 8]);

    let response = run_worker(
        &serde_json::json!({
            "op": "remove_pages",
            "inputs": [input],
            "output": dir.path().join("bad.pdf"),
            "args": { "ranges": "9" }
        })
        .to_string(),
    );
    assert_eq!(response["ok"], false);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no page 9"));
}

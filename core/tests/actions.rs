//! End to end: the bundled packs' slices run through the real worker binaries, the supervisor
//! and the output rules. Windows only.
//!
//! The workers are found next to this test's binary (`target\debug`), where
//! `cargo test --workspace` builds them. Without them these tests are skipped with a note.
#![cfg(windows)]

use std::path::{Path, PathBuf};

use flick_core::jobs::{run_slice, JobError, Outcome};
use flick_core::packs::{load_dir, Catalog};
use flick_core::supervisor::CancelToken;
use image::{ImageFormat, Rgb, RgbImage};
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use tempfile::TempDir;

/// The bundled packs with the workers built next to this test, or `None` if they are not built.
fn catalog() -> Option<Catalog> {
    let deps = std::env::current_exe().unwrap();
    let target = deps.parent()?.parent()?.to_path_buf();
    if ["pdf-light.exe", "image-tools.exe"]
        .iter()
        .any(|w| !target.join(w).is_file())
    {
        eprintln!(
            "skipped: build the workers first (cargo test --workspace builds them into {})",
            target.display()
        );
        return None;
    }
    let packs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../packs");
    let catalog = load_dir(&packs, Some(&target));
    assert!(catalog.errors.is_empty(), "{:#?}", catalog.errors);
    Some(catalog)
}

fn run(catalog: &Catalog, slice_id: &str, inputs: &[PathBuf]) -> Result<Outcome, JobError> {
    let (pack, slice) = catalog.find(slice_id).expect(slice_id);
    run_slice(pack, slice, inputs, &CancelToken::default())
}

/// A PDF with one page per label, each showing its label as text.
fn pdf(dir: &TempDir, name: &str, pages: usize) -> PathBuf {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let kids: Vec<Object> = (1..=pages)
        .map(|n| {
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 24.into()]),
                    Operation::new("Td", vec![72.into(), 700.into()]),
                    Operation::new("Tj", vec![Object::string_literal(format!("Page {n}"))]),
                    Operation::new("ET", vec![]),
                ],
            };
            let contents = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => contents,
                "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            })
            .into()
        })
        .collect();
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => kids, "Count" => pages as i64,
        }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let path = dir.path().join(name);
    doc.save(&path).unwrap();
    path
}

fn picture(dir: &TempDir, name: &str, width: u32, height: u32, format: ImageFormat) -> PathBuf {
    let image = RgbImage::from_fn(width, height, |x, y| {
        Rgb([
            (x * 7 % 256) as u8,
            (y * 13 % 256) as u8,
            ((x + y) % 256) as u8,
        ])
    });
    let path = dir.path().join(name);
    image.save_with_format(&path, format).unwrap();
    path
}

fn listing(dir: &TempDir) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn page_text(path: &Path) -> String {
    let doc = Document::load(path).unwrap();
    assert_eq!(doc.get_pages().len(), 1);
    doc.extract_text(&[1]).unwrap().trim().to_string()
}

#[test]
fn split_saves_one_numbered_file_per_page() {
    let Some(catalog) = catalog() else { return };
    let dir = TempDir::new().unwrap();
    let input = pdf(&dir, "Report.pdf", 12);
    std::fs::write(dir.path().join("Report - page 05.pdf"), b"keep me").unwrap();

    let outcome = run(&catalog, "pdf-light.split.pages", &[input]).unwrap();

    assert_eq!(outcome.outputs.len(), 12);
    assert_eq!(outcome.outputs[0], dir.path().join("Report - page 01.pdf"));
    assert_eq!(
        outcome.outputs[4],
        dir.path().join("Report - page 05 (1).pdf")
    );
    assert_eq!(page_text(&outcome.outputs[4]), "Page 5");
    assert_eq!(page_text(&outcome.outputs[11]), "Page 12");
    assert_eq!(
        std::fs::read(dir.path().join("Report - page 05.pdf")).unwrap(),
        b"keep me"
    );
    // The input, the file that was already there and 12 pages: no temp folder left behind.
    assert_eq!(listing(&dir).len(), 14);
}

#[test]
fn split_of_a_bad_pdf_leaves_nothing_behind() {
    let Some(catalog) = catalog() else { return };
    let dir = TempDir::new().unwrap();
    let junk = dir.path().join("junk.pdf");
    std::fs::write(&junk, b"%PDF-1.7 not really").unwrap();

    let err = run(&catalog, "pdf-light.split.pages", &[junk]).unwrap_err();
    assert!(err.to_string().contains("junk.pdf"), "{err}");
    assert_eq!(listing(&dir), ["junk.pdf"]);
}

#[test]
fn rotate_runs_once_per_pdf_and_skips_bad_ones() {
    let Some(catalog) = catalog() else { return };
    let dir = TempDir::new().unwrap();
    let a = pdf(&dir, "a.pdf", 2);
    let b = pdf(&dir, "b.pdf", 1);
    let empty = dir.path().join("c.pdf");
    std::fs::write(&empty, b"").unwrap();

    let outcome = run(&catalog, "pdf-light.rotate.right", &[b, empty, a]).unwrap();

    assert_eq!(
        outcome.outputs,
        [
            dir.path().join("a - rotated.pdf"),
            dir.path().join("b - rotated.pdf")
        ]
    );
    assert_eq!(outcome.failed.len(), 1);
    assert!(outcome.failed[0].0.ends_with("c.pdf"));
    let doc = Document::load(&outcome.outputs[0]).unwrap();
    for id in doc.get_pages().into_values() {
        let page = doc.get_dictionary(id).unwrap();
        assert_eq!(page.get(b"Rotate").unwrap().as_i64().unwrap(), 90);
    }
}

#[test]
fn images_become_a_pdf_and_convert_and_shrink() {
    let Some(catalog) = catalog() else { return };
    let dir = TempDir::new().unwrap();
    let photo2 = picture(&dir, "photo 2.jpg", 64, 48, ImageFormat::Jpeg);
    let photo10 = picture(&dir, "photo 10.png", 48, 64, ImageFormat::Png);

    let outcome = run(&catalog, "image.to-pdf", &[photo10.clone(), photo2.clone()]).unwrap();
    assert_eq!(outcome.outputs, [dir.path().join("photo 2 - images.pdf")]);
    assert_eq!(
        Document::load(&outcome.outputs[0])
            .unwrap()
            .get_pages()
            .len(),
        2
    );

    let outcome = run(&catalog, "image.to-jpg", &[photo10]).unwrap();
    assert_eq!(outcome.outputs, [dir.path().join("photo 10.jpg")]);
    let outcome = run(&catalog, "image.to-png", std::slice::from_ref(&photo2)).unwrap();
    assert_eq!(outcome.outputs, [dir.path().join("photo 2.png")]);

    // A small JPG cannot get smaller: a clear message and no file.
    let err = run(&catalog, "image.shrink", &[photo2]).unwrap_err();
    assert!(err.to_string().contains("already small"), "{err}");
    assert!(!dir.path().join("photo 2 - small.jpg").exists());

    let big = picture(&dir, "big.png", 2400, 1200, ImageFormat::Png);
    let outcome = run(&catalog, "image.shrink", &[big]).unwrap();
    assert_eq!(outcome.outputs, [dir.path().join("big - small.jpg")]);
    let small = image::open(&outcome.outputs[0]).unwrap();
    assert_eq!((small.width(), small.height()), (1600, 800));
}

//! The packs shipped in this repository must load with the current core.

use std::path::Path;

use flick_core::config::SliceConfig;
use flick_core::packs::{load_dir, Catalog, Mode, ICONS};

fn bundled() -> Catalog {
    let packs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../packs");
    // Stand in for the built worker binaries, which live next to flick.exe during development.
    let bin = tempfile::tempdir().unwrap();
    for worker in ["pdf-light.exe", "image-tools.exe"] {
        std::fs::write(bin.path().join(worker), b"").unwrap();
    }
    load_dir(&packs, Some(bin.path()))
}

fn ids(catalog: &Catalog, paths: &[&str]) -> Vec<String> {
    catalog
        .slices_for(paths, &SliceConfig::default())
        .iter()
        .map(|s| s.id.clone())
        .collect()
}

#[test]
fn bundled_packs_load_cleanly() {
    let catalog = bundled();
    assert!(catalog.errors.is_empty(), "{:#?}", catalog.errors);

    let merge = catalog.find("pdf-light.merge").expect("merge slice");
    assert_eq!(merge.1.op, "merge");
    assert_eq!(merge.1.mode, Mode::Combine);
    assert_eq!(
        ids(&catalog, &["a.pdf", "b.pdf"]),
        ["pdf-light.merge", "pdf-light.split", "pdf-light.rotate"]
    );
    assert_eq!(
        ids(&catalog, &["a.pdf"]),
        ["pdf-light.split", "pdf-light.rotate"]
    );

    let split = catalog.find("pdf-light.split").unwrap().1;
    assert_eq!(split.mode, Mode::Each);
    assert!(split.multi_output());

    let (worker, to_pdf) = catalog.find("image.to-pdf").expect("images to PDF slice");
    assert!(worker.worker_exe.ends_with("image-tools.exe"));
    assert_eq!(to_pdf.mode, Mode::Combine);
    assert_eq!(
        ids(&catalog, &["a.jpg", "b.PNG"]),
        ["image.to-pdf", "image.shrink"]
    );
    assert_eq!(
        ids(&catalog, &["photo.jpeg"]),
        ["image.to-pdf", "image.to-png", "image.shrink"]
    );
    assert_eq!(
        ids(&catalog, &["shot.webp"]),
        ["image.to-png", "image.to-jpg", "image.shrink"]
    );
    // Mixed PDFs and images get nothing.
    assert!(ids(&catalog, &["a.pdf", "b.png"]).is_empty());
}

#[test]
fn every_bundled_slice_has_a_known_icon() {
    let catalog = bundled();
    for pack in &catalog.packs {
        for slice in &pack.slices {
            let icon = slice.icon.as_deref().unwrap_or_default();
            assert!(ICONS.contains(&icon), "{} has icon {icon:?}", slice.id);
        }
    }
}

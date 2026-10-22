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

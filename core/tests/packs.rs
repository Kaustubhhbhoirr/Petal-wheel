//! The packs shipped in this repository must load with the current core.

use std::path::Path;

use flick_core::config::SliceConfig;
use flick_core::packs::load_dir;

#[test]
fn bundled_packs_load_cleanly() {
    let packs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../packs");
    // Stand in for the built worker binaries, which live next to flick.exe during development.
    let bin = tempfile::tempdir().unwrap();
    std::fs::write(bin.path().join("pdf-light.exe"), b"").unwrap();

    let catalog = load_dir(&packs, Some(bin.path()));
    assert!(catalog.errors.is_empty(), "{:#?}", catalog.errors);

    let merge = catalog.find("pdf-light.merge").expect("merge slice");
    assert_eq!(merge.1.op, "merge");
    let ids: Vec<_> = catalog
        .slices_for(&["a.pdf", "b.pdf"], &SliceConfig::default())
        .iter()
        .map(|s| s.id.clone())
        .collect();
    assert_eq!(ids, ["pdf-light.merge"]);
}

//! The packs shipped in this repository must load with the current core.

use std::path::Path;

use flick_core::config::SliceConfig;
use flick_core::packs::{load_dir, Catalog, PackSlice, Runs, Tier, ICONS};

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
    assert_eq!(merge.1.runs, Runs::Once);
    assert_eq!(
        ids(&catalog, &["a.pdf", "b.pdf"]),
        ["pdf-light.merge", "pdf-light.split", "pdf-light.rotate"]
    );
    assert_eq!(
        ids(&catalog, &["a.pdf"]),
        [
            "pdf-light.split",
            "pdf-light.rotate",
            "pdf-light.remove-pages",
            "pdf-light.organize"
        ]
    );

    let split = catalog.find("pdf-light.split").unwrap().1;
    assert_eq!(split.mode, Tier::Choice);
    assert_eq!(
        split.default_child.as_deref(),
        Some("pdf-light.split.pages")
    );
    let every_page = catalog.find("pdf-light.split.pages").unwrap().1;
    assert_eq!(every_page.runs, Runs::Each);
    assert!(every_page.multi_output());

    // Rotate offers three angles for several PDFs, plus page picking for one.
    let children = |paths: &[&str]| -> Vec<String> {
        catalog
            .tree_for(paths, &SliceConfig::default())
            .iter()
            .find(|a| a.slice.id == "pdf-light.rotate")
            .unwrap()
            .children
            .iter()
            .map(|c| c.slice.id.clone())
            .collect()
    };
    assert_eq!(
        children(&["a.pdf", "b.pdf"]),
        [
            "pdf-light.rotate.right",
            "pdf-light.rotate.left",
            "pdf-light.rotate.half"
        ]
    );
    assert_eq!(children(&["a.pdf"]).len(), 4);
    let left = catalog.find("pdf-light.rotate.left").unwrap().1;
    assert_eq!(left.args, serde_json::json!({ "degrees": -90 }));
    let remove = catalog.find("pdf-light.remove-pages").unwrap().1;
    assert_eq!(
        (remove.mode, remove.app.as_deref()),
        (Tier::App, Some("remove-pages"))
    );

    let (worker, to_pdf) = catalog.find("image.to-pdf").expect("images to PDF slice");
    assert!(worker.worker_exe.ends_with("image-tools.exe"));
    assert_eq!(to_pdf.runs, Runs::Once);
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
    fn check(slices: &[PackSlice]) {
        for slice in slices {
            let icon = slice.icon.as_deref().unwrap_or_default();
            assert!(ICONS.contains(&icon), "{} has icon {icon:?}", slice.id);
            check(&slice.children);
        }
    }
    let catalog = bundled();
    for pack in &catalog.packs {
        check(&pack.slices);
    }
}

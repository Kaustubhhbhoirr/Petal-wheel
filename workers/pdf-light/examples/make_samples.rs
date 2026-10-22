//! Writes small text PDFs for trying Flick by hand.
//!
//!     cargo run -p pdf-light --example make_samples -- <folder>
//!
//! Creates "Chapter 1.pdf" (2 pages), "Chapter 2.pdf" (1 page) and "Chapter 10.pdf" (1 page),
//! so the natural sort order (1, 2, 10) is easy to check in the merged file.

use std::path::PathBuf;

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: make_samples <folder>"),
    );
    std::fs::create_dir_all(&dir).expect("create folder");
    for (name, pages) in [
        ("Chapter 1", &["Chapter 1, page 1", "Chapter 1, page 2"][..]),
        ("Chapter 2", &["Chapter 2, page 1"][..]),
        ("Chapter 10", &["Chapter 10, page 1"][..]),
    ] {
        let path = dir.join(format!("{name}.pdf"));
        sample(pages).save(&path).expect("save sample");
        println!("{}", path.display());
    }
}

fn sample(labels: &[&str]) -> Document {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
    let kids: Vec<Object> = labels
        .iter()
        .map(|label| {
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 28.into()]),
                    Operation::new("Td", vec![72.into(), 700.into()]),
                    Operation::new("Tj", vec![Object::string_literal(*label)]),
                    Operation::new("ET", vec![]),
                ],
            };
            let contents = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => contents,
                "Resources" => resources,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            })
            .into()
        })
        .collect();
    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    doc
}

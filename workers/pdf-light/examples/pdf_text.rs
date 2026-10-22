//! Prints the page count and each page's text, to check merge results by hand.
//!
//!     cargo run -p pdf-light --example pdf_text -- <file.pdf>

use lopdf::Document;

fn main() {
    let path = std::env::args().nth(1).expect("usage: pdf_text <file.pdf>");
    let doc = Document::load(&path).expect("load PDF");
    let pages = doc.get_pages();
    println!("{path}: {} page(s)", pages.len());
    for number in pages.keys() {
        let text = doc.extract_text(&[*number]).unwrap_or_default();
        println!("  page {number}: {}", text.trim());
    }
}

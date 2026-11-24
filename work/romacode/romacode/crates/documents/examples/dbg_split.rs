use rc_docs::color::Rgba;
use rc_docs::pdfgen::{build_document, Align, Block, Meta, PageSize};
fn main() {
    let meta = Meta { title: "Sample".into(), author: "Ada".into(), subject: "test".into() };
    let pdf = build_document(&meta, &[
        Block::Para { text: "First page content.".into(), align: Align::Left, size_pt: None, color: None, line_spacing: 1.2 },
        Block::PageBreak,
        Block::Para { text: "Second page content here.".into(), align: Align::Left, size_pt: None, color: None, line_spacing: 1.2 },
    ], PageSize::A4, 56.0, false, Rgba::new(31, 119, 180)).unwrap();
    let part = rc_docs::pdfread::split(&pdf, 2, 2).unwrap();
    let info = rc_docs::pdfread::read(&part).unwrap();
    println!("pages={} note={}", info.page_count, info.extraction_note);
    println!("TEXT: {:?}", info.text);
}

use rc_docs::pdfgen::{build_document, Align, Block, Meta, PageSize};
use rc_docs::color::Rgba;
fn main() {
    let meta = Meta { title: "Sample".into(), author: "Ada".into(), subject: "test".into() };
    let pdf = build_document(&meta, &[
        Block::Heading { level: 1, text: "Introduction".into() },
        Block::Para { text: "The quick brown fox.".into(), align: Align::Left, size_pt: None, color: None, line_spacing: 1.2 },
    ], PageSize::A4, 56.0, false, Rgba::new(31, 119, 180)).unwrap();
    // find trailer info manually
    let pos = pdf.windows(7).position(|w| w == b"trailer").unwrap();
    println!("trailer seg: {}", String::from_utf8_lossy(&pdf[pos..(pos+120).min(pdf.len())]).replace('\n', "\\n"));
}

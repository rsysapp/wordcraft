//! embed: a package carrying about 150 MB of embedded images a side (scanned exhibits) is inspected
//! within the work limit. Each image is inflated once a side and hashed as it streams; the
//! comparison uses the digests. The images are random bytes, so a deflater stores them, as a real
//! scan's compressed bytes would be. Prints the work, the time and the memory held.

use std::io::{Cursor, Write};
use std::time::Instant;

use wordcraft_docx::inspect::{MAX_WORK, inspect_measured};
use wordcraft_testkit::counting::{Counting, peak, reset_peak};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
const IMAGES: usize = 30;
const IMAGE_BYTES: usize = 5_000_000;

/// A package with the images and this body, every part deflated.
fn package(body: &str, images: &[Vec<u8>]) -> Vec<u8> {
    let types = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let links: String = (0..images.len())
        .map(|n| format!(r#"<Relationship Id="rImg{n}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image{n}.png"/>"#))
        .collect();
    let document_rels = format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{links}</Relationships>"#);
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut add = |name: &str, bytes: &[u8]| {
        writer.start_file(name, options).unwrap();
        writer.write_all(bytes).unwrap();
    };
    add("[Content_Types].xml", types.as_bytes());
    add("_rels/.rels", root.as_bytes());
    add("word/_rels/document.xml.rels", document_rels.as_bytes());
    add("word/document.xml", format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#).as_bytes());
    for (n, image) in images.iter().enumerate() {
        add(&format!("word/media/image{n}.png"), image);
    }
    writer.finish().unwrap().into_inner()
}

/// Incompressible bytes (xorshift), different for each seed.
fn random(length: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(length);
    while out.len() < length {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(length);
    out
}

#[test]
fn a_package_with_150_mb_of_images_a_side_is_inspected_within_the_work_limit() {
    let images: Vec<Vec<u8>> = (0..IMAGES).map(|n| random(IMAGE_BYTES, n as u64 + 1)).collect();
    let paragraph = |runs: &str| format!("<w:p>{runs}</w:p>");
    let text = r#"<w:r><w:t xml:space="preserve">The exhibits are attached.</w:t></w:r>"#;
    let inserted = r#"<w:ins w:id="1" w:author="Ana Setia" w:date="2026-10-11T09:00:00Z"><w:r><w:t xml:space="preserve"> Signed copies follow.</w:t></w:r></w:ins>"#;
    let base = package(&paragraph(text), &images);
    let edited = package(&paragraph(&format!("{text}{inserted}")), &images);
    drop(images);
    let media = IMAGES * IMAGE_BYTES;
    let baseline = reset_peak();
    let start = Instant::now();
    let (result, charged, work) = inspect_measured(&base, &edited);
    let elapsed = start.elapsed();
    let held = peak().saturating_sub(baseline);
    eprintln!(
        "media {:.1} MB a side, packages {:.1} / {:.1} MB: {:.2} s, work {:.1} M of {:.1} M ({:.1} ns a unit), held {:.2} MB, charged {:.2} MB",
        media as f64 / 1e6,
        base.len() as f64 / 1e6,
        edited.len() as f64 / 1e6,
        elapsed.as_secs_f64(),
        work as f64 / 1e6,
        MAX_WORK as f64 / 1e6,
        elapsed.as_secs_f64() * 1e9 / work.max(1) as f64,
        held as f64 / 1e6,
        charged as f64 / 1e6,
    );
    let inspection = result.unwrap();
    assert!(!inspection.too_many_changes, "{inspection:#?}");
    assert!(inspection.losses.is_empty() && inspection.added_parts.is_empty(), "{inspection:#?}");
    assert_eq!(inspection.new_revisions.len(), 1);
    assert!(work < MAX_WORK, "work {work}");
    assert!(held as u64 <= charged, "held {held}, charged {charged}");
}

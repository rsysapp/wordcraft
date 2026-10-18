//! embed: reading the decoded streams (Clauflow's version comparison) never holds more memory than
//! it charged to its budget, as the save check never does (`inspect_memory.rs`). Each case is read
//! under a counting allocator, its result kept alive, and the most held is checked against the
//! most charged.

use std::io::{Cursor, Write};

use wordcraft_docx::inspect::streams::{default_limits, read_pair};
use wordcraft_testkit::counting::{Counting, peak, reset_peak};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape""#;
const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;

fn package(body: &str) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in [
        ("[Content_Types].xml", TYPES.to_string()),
        ("_rels/.rels", ROOT_RELS.to_string()),
        ("word/document.xml", format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#)),
    ] {
        writer.start_file(name, options).unwrap();
        writer.write_all(bytes.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

/// The cases: shapes whose decoded form is larger than what the save check keeps.
fn cases() -> Vec<(&'static str, Vec<u8>)> {
    let author = "A".repeat(1_000);
    // Every character of a long insertion carries its revision, author and date.
    let tracked = format!(
        r#"<w:p><w:ins w:id="1" w:author="{author}" w:date="2026-10-01T09:00:00Z">{}</w:ins></w:p>"#,
        run(&"x".repeat(200_000))
    );
    // Many distinct opaque marks, each decoded with its element name and digest.
    let opaque: String = (0..20_000).map(|index| format!(r#"<w:p><w:bookmarkStart w:id="{index}" w:name="b{index}"/>{}</w:p>"#, run("a"))).collect();
    // Many text boxes, each its own story.
    let boxes: String = (0..3_000)
        .map(|_| format!(r#"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wps:txbx><w:txbxContent><w:p>{}</w:p></w:txbxContent></wps:txbx></w:drawing></mc:Choice></mc:AlternateContent></w:r></w:p>"#, run("box")))
        .collect();
    // Many paragraphs: one decoded end each.
    let paragraphs: String = (0..100_000).map(|_| "<w:p/>".to_string()).collect();
    let small_tracked = format!(
        r#"<w:p><w:ins w:id="1" w:author="Ana Contoh" w:date="2026-10-01T09:00:00Z">{}</w:ins></w:p>"#,
        run(&"x".repeat(100_000))
    );
    vec![
        ("tracked text", package(&tracked)),
        ("tracked, small", package(&small_tracked)),
        ("opaque marks", package(&opaque)),
        ("text boxes", package(&boxes)),
        ("paragraphs", package(&paragraphs)),
    ]
}

#[test]
fn reading_the_streams_never_holds_more_than_it_charged() {
    let mut over = Vec::new();
    for (name, bytes) in cases() {
        let limits = default_limits(bytes.len(), bytes.len());
        let baseline = reset_peak();
        let result = read_pair(&bytes, &bytes, &limits);
        let held = peak().saturating_sub(baseline) as u64;
        // A refused read must have stopped before holding more than its limit.
        let charged = result.as_ref().map_or(limits.memory, |pair| pair.peak_memory);
        eprintln!("{name:<16} held {:>7.1} MB  charged {:>7.1} MB  {}", held as f64 / 1e6, charged as f64 / 1e6, if result.is_ok() { "Ok" } else { "refused" });
        if held > charged {
            over.push(name);
        }
        drop(result);
    }
    assert!(over.is_empty(), "held more than charged: {over:?}");
}

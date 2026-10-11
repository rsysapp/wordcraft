//! embed: the save inspector never holds more memory than it charged to its budget. Every crafted
//! case (the shapes that once made a small upload take gigabytes or minutes) and real saves are
//! inspected under a counting allocator, and the most it held is checked against the most it
//! charged. `INSPECT_MEMORY_FULL=1` runs them at full size (slow outside a release build).

use std::io::{Cursor, Write};
use std::time::Instant;

use serde_json::json;
use wordcraft_docx::inspect::{INSPECT_LIMITS, inspect_measured};
use wordcraft_engine::Session;
use wordcraft_engine::io::{open_docx_with_limits, save_bytes};
use wordcraft_testkit::counting::{Counting, peak, reset_peak};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

fn types(overrides: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>{overrides}</Types>"#
    )
}

fn zip(entries: Vec<(String, Vec<u8>)>) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in entries {
        writer.start_file(name.as_str(), options).unwrap();
        writer.write_all(&bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// A package from its body, extra overrides and extra parts.
fn package(body: &str, overrides: &str, extra: Vec<(String, Vec<u8>)>) -> Vec<u8> {
    let mut entries = vec![
        ("[Content_Types].xml".to_string(), types(overrides).into_bytes()),
        ("_rels/.rels".to_string(), ROOT_RELS.as_bytes().to_vec()),
        ("word/document.xml".to_string(), format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#).into_bytes()),
    ];
    entries.extend(extra);
    zip(entries)
}

fn run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

fn para(text: &str) -> String {
    format!("<w:p>{}</w:p>", run(text))
}

/// Full size, or a tenth (and less where a case is quadratic in nothing but time).
fn size(full: usize) -> usize {
    if std::env::var_os("INSPECT_MEMORY_FULL").is_some() { full } else { (full / 10).max(1) }
}

fn braces(length: usize) -> String {
    "{".repeat(length)
}

/// The cases: a name, the base and the save.
fn cases() -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let mut cases = Vec::new();
    let namespace = braces(4_096);

    // Real saves: WordCraft writes the base and edits it with tracking on.
    let mut doc = wordcraft_doc::Document::from_text("Rent is due monthly on the first day.\nThe tenant keeps the premises clean.");
    doc.ensure_nonempty();
    let mut session = Session::new(doc);
    session.author = "Ana Setia".into();
    session.run("select.text", &json!({"text": "premises"})).unwrap();
    session.run("review.newComment", &json!({"text": "Which premises?"})).unwrap();
    let base = save_bytes("base.docx", &session.doc).unwrap();
    let mut edited = Session::new(open_docx_with_limits(&base, &INSPECT_LIMITS).unwrap());
    edited.author = "Sari Wulan".into();
    edited.run("review.trackChanges", &json!({"value": true})).unwrap();
    edited.run("select.text", &json!({"text": "monthly"})).unwrap();
    edited.run("text.insert", &json!({"text": "weekly"})).unwrap();
    edited.run("select.text", &json!({"text": "tenant"})).unwrap();
    edited.run("review.newComment", &json!({"text": "Name the tenant."})).unwrap();
    edited.run("review.reply", &json!({"id": 0, "text": "The flat."})).unwrap();
    let saved = save_bytes("edited.docx", &edited.doc).unwrap();
    cases.push(("real save", base, saved));

    // A long agreement with a few tracked insertions.
    let clauses: String = (0..size(30_000))
        .map(|n| para(&format!("Clause {n}. The tenant shall pay the rent on the first day of each month, without deduction. ")))
        .collect();
    let insertion =
        r#"<w:ins w:id="1" w:author="Sari" w:date="2026-10-10T08:00:00Z"><w:r><w:t xml:space="preserve">and on time </w:t></w:r></w:ins>"#;
    let edited = clauses.replacen("without deduction", &format!(r#"</w:t></w:r>{insertion}<w:r><w:t xml:space="preserve">without deduction"#), 3);
    cases.push(("long agreement", package(&clauses, "", vec![]), package(&edited, "", vec![])));

    // One element with as many distinct attributes as it may have, in a long namespace.
    let attributes: String = (0..20_000).map(|n| format!(r#" p:a{n}="1""#)).collect();
    let custom = format!(r#"<data xmlns:p="{namespace}"><e{attributes}/></data>"#);
    cases.push((
        "many attributes",
        package(&para("Rent."), "", vec![]),
        package(&para("Rent."), "", vec![("customXml/item1.xml".into(), custom.into_bytes())]),
    ));
    let attributes: String = (0..500).map(|n| format!(r#" p:a{n}="1""#)).collect();
    let custom = format!(r#"<data xmlns:p="{namespace}">{}</data>"#, format!("<e{attributes}/>").repeat(size(400)));
    cases.push((
        "attributes at the limit",
        package(&para("Rent."), "", vec![]),
        package(&para("Rent."), "", vec![("customXml/item1.xml".into(), custom.into_bytes())]),
    ));

    // Distinct long element names in a long namespace.
    let names = |count: usize| -> String {
        let elements: String = (0..count).map(|n| format!("<p:{}{n}/>", "n".repeat(4_000))).collect();
        format!(r#"<data xmlns:p="{namespace}">{elements}</data>"#)
    };
    let part = names(size(16_000)).into_bytes();
    cases.push((
        "long names",
        package(&para("Rent."), "", vec![("customXml/item1.xml".into(), part.clone())]),
        package(&para("Rent."), "", vec![("customXml/item1.xml".into(), part)]),
    ));

    // One short element repeated under a long namespace.
    let flood = format!(r#"<data xmlns:p="{namespace}">{}</data>"#, "<p:a/>".repeat(size(200_000)));
    cases.push((
        "namespace flood",
        package(&para("Rent."), "", vec![]),
        package(&para("Rent."), "", vec![("customXml/item1.xml".into(), flood.into_bytes())]),
    ));

    // The same short element as often as the element budget allows.
    let flood = format!(r#"<data xmlns:p="{namespace}">{}</data>"#, "<p:a/>".repeat(size(1_999_000)));
    cases.push((
        "namespace flood at the cap",
        package(&para("Rent."), "", vec![]),
        package(&para("Rent."), "", vec![("customXml/item1.xml".into(), flood.into_bytes())]),
    ));

    // Relationship parts of many relationships.
    let rels: String = (0..size(200_000))
        .map(|n| format!(r#"<Relationship Id="rId{n}" Type="http://example.com/t" Target="https://example.com/{n}" TargetMode="External"/>"#))
        .collect();
    let rels = format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#);
    let part = vec![
        ("word/_rels/document.xml.rels".to_string(), rels.clone().into_bytes()),
        ("customXml/_rels/item1.xml.rels".to_string(), rels.into_bytes()),
    ];
    cases.push(("many relationships", package(&para("Rent."), "", part.clone()), package(&para("Rent."), "", part)));

    // Many content type overrides.
    let overrides: String = (0..size(200_000)).map(|n| format!(r#"<Override PartName="/x/{n}.xml" ContentType="application/x-{n}+xml"/>"#)).collect();
    cases.push(("many content types", package(&para("Rent."), &overrides, vec![]), package(&para("Rent."), &overrides, vec![])));

    // Large opaque content and custom XML of braces.
    let element = format!("<w:p><w:r><w:customThing>{}</w:customThing></w:r></w:p>", braces(size((64 << 20) - 4_096)));
    let custom = format!("<data>{}</data>", braces(size(63 << 20)));
    let big =
        package(&element, "", vec![("customXml/item1.xml".into(), custom.clone().into_bytes()), ("customXml/item2.xml".into(), custom.into_bytes())]);
    cases.push(("braces", big.clone(), big));

    // The element budget filled with empty paragraphs.
    let base = "<w:p/>".repeat(size(1_999_000));
    let edited = format!("{}{}", para("x"), "<w:p/>".repeat(size(1_999_000) - 1));
    cases.push(("empty paragraphs", package(&base, "", vec![]), package(&edited, "", vec![])));

    // Nesting near the depth limit.
    let one = format!("{}{}{}", "<w:sdt><w:sdtContent>".repeat(29), para("Deep text here."), "</w:sdtContent></w:sdt>".repeat(29));
    let deep = one.repeat(size(30_000));
    cases.push(("deep", package(&deep, "", vec![]), package(&deep.replacen("Deep text here.", "Deep text HERE.", 1), "", vec![])));

    // Nested tables with distinct revision pairs (one tracked row insertion per level).
    let tables = |copy: usize| {
        let mut open = String::new();
        let mut close = String::new();
        for level in 0..19 {
            let n = copy * 19 + level;
            open.push_str(&format!(r#"<w:tbl><w:tr><w:trPr><w:ins w:id="1" w:author="Ana" w:date="2026-01-01T00:00:{n:07}Z"/></w:trPr><w:tc>"#));
            close.insert_str(0, "</w:tc></w:tr></w:tbl>");
        }
        format!("{open}{}{close}<w:p/>", para("x"))
    };
    let nested: String = (0..size(14_000)).map(tables).collect();
    cases.push(("nested tables", package(&nested, "", vec![]), package(&nested, "", vec![])));

    // One long paragraph of alternating characters.
    let alternating = |first: &str, second: &str| format!("<w:p>{}</w:p>", run(&format!("{first}{second}").repeat(size(3_900_000))));
    cases.push(("alternating", package(&alternating("a", "b"), "", vec![]), package(&alternating("b", "a"), "", vec![])));

    // Every word different.
    let words = |prefix: &str| (0..size(600_000)).map(|n| format!("{prefix}{n} ")).collect::<String>();
    cases.push(("every word different", package(&para(&words("a")), "", vec![]), package(&para(&words("b")), "", vec![])));

    // Many header stories.
    let mut overrides = String::new();
    let mut headers = Vec::new();
    for n in 0..size(19_990) {
        overrides.push_str(&format!(
            r#"<Override PartName="/word/header{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>"#
        ));
        headers.push((format!("word/header{n}.xml"), format!(r#"<w:hdr {W}>{}</w:hdr>"#, para(&format!("Header {n}"))).into_bytes()));
    }
    let many = package(&para("Body."), &overrides, headers);
    cases.push(("many headers", many.clone(), many));

    // Many comments.
    let comments: String = (0..size(20_000))
        .map(|n| format!(r#"<w:comment w:id="{n}" w:author="Ana" w:date="2026-01-01T00:00:00Z"><w:p w14:paraId="{n:08X}" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">{}</w:p></w:comment>"#, run("Check this.")))
        .collect();
    let comments = format!(r#"<w:comments {W}>{comments}</w:comments>"#);
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#;
    let commented = package(
        &para("Rent."),
        r#"<Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/>"#,
        vec![("word/comments.xml".into(), comments.into_bytes()), ("word/_rels/document.xml.rels".into(), rels.as_bytes().to_vec())],
    );
    cases.push(("many comments", commented.clone(), commented));
    cases
}

#[test]
fn the_inspection_never_holds_more_than_it_charged() {
    let mut worst = (0.0f64, "");
    let mut over = Vec::new();
    for (name, base, edited) in cases() {
        let baseline = reset_peak();
        let started = Instant::now();
        let (result, charged) = inspect_measured(&base, &edited);
        let elapsed = started.elapsed();
        let held = peak().saturating_sub(baseline) as u64;
        let ratio = held as f64 / charged.max(1) as f64;
        eprintln!(
            "{name:<24} held {:>7.1} MB  charged {:>7.1} MB  ratio {ratio:.2}  {elapsed:>9.2?}  {}",
            held as f64 / 1e6,
            charged as f64 / 1e6,
            match &result {
                Ok(_) => "Ok".to_string(),
                Err(error) => format!("{error:?}").chars().take(60).collect(),
            }
        );
        if ratio > worst.0 {
            worst = (ratio, name);
        }
        if held > charged {
            over.push(name);
        }
        drop(result);
    }
    eprintln!("largest held/charged ratio: {:.2} ({})", worst.0, worst.1);
    assert!(over.is_empty(), "held more than charged: {over:?}");
}

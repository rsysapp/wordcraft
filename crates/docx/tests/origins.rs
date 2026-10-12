//! embed: a document read from a package keeps the package, and each paragraph the part and
//! position (among the part's `w:p` elements, in document order) it was read from.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::io::Write;

use wordcraft_doc::{Document, Origin, PartKind, StoryRef};
use wordcraft_docx::{DocxError, ReadLimits, SOURCE_PACKAGE, read, read_with_limits};

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#;
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const CONTENT_TYPES: &str = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#;

fn zip_with(entries: &[(&str, String)], options: zip::write::SimpleFileOptions) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, text) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(text.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn zip(entries: &[(&str, String)]) -> Vec<u8> {
    zip_with(entries, zip::write::SimpleFileOptions::default())
}

fn para(text: &str) -> String {
    format!(r#"<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#)
}

fn root_rels() -> String {
    format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="word/document.xml"/></Relationships>"#
    )
}

fn sample() -> (Vec<u8>, Vec<&'static str>) {
    let text_box = format!(
        r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="914400"/><wp:docPr id="1" name="Box"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent>{}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#,
        para("In the box")
    );
    let body = format!(
        r#"{}<w:tbl><w:tr><w:tc>{}</w:tc></w:tr></w:tbl>{}{}<w:p><w:r><w:t>Note</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rId2"/></w:sectPr>"#,
        para("First clause"),
        para("In a cell"),
        text_box,
        para("Last clause")
    );
    let names =
        vec!["[Content_Types].xml", "_rels/.rels", "word/_rels/document.xml.rels", "word/document.xml", "word/footnotes.xml", "word/header1.xml"];
    let entries = vec![
        (names[0], CONTENT_TYPES.to_string()),
        (names[1], root_rels()),
        (
            names[2],
            format!(
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/footnotes" Target="footnotes.xml"/><Relationship Id="rId2" Type="{REL}/header" Target="header1.xml"/></Relationships>"#
            ),
        ),
        (names[3], format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#)),
        (
            names[4],
            format!(
                r#"<w:footnotes {W}><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:id="1">{}</w:footnote></w:footnotes>"#,
                para("A note")
            ),
        ),
        (names[5], format!(r#"<w:hdr {W}>{}</w:hdr>"#, para("A header"))),
    ];
    (zip(&entries), names)
}

/// The part index an origin carries: the position among the package's part names, sorted.
fn part(names: &[&str], name: &str) -> u32 {
    let mut sorted: Vec<&str> = names.to_vec();
    sorted.sort_unstable();
    sorted.iter().position(|candidate| *candidate == name).unwrap() as u32
}

fn origin_of(doc: &Document, story: StoryRef, text: &str) -> Option<Origin> {
    doc.para_paths(story).iter().filter_map(|path| doc.para(story, path)).find(|p| p.plain_text() == text).and_then(|p| p.origin)
}

fn story_of(doc: &Document, text: &str) -> StoryRef {
    std::iter::once(StoryRef::Body)
        .chain(doc.parts.keys().map(|id| StoryRef::Part(*id)))
        .find(|story| doc.para_paths(*story).iter().filter_map(|path| doc.para(*story, path)).any(|p| p.plain_text() == text))
        .unwrap()
}

#[test]
fn numbers_paragraphs_in_document_order_in_each_part() {
    let (bytes, names) = sample();
    let doc = read(&bytes).unwrap();
    let document = part(&names, "word/document.xml");
    // Document order counts the text box's paragraph after the paragraph that holds it.
    for (text, index) in [("First clause", 0), ("In a cell", 1), ("In the box", 3), ("Last clause", 4)] {
        assert_eq!(origin_of(&doc, story_of(&doc, text), text), Some(Origin { part: document, index }), "{text}");
    }
    // The separator's paragraph is counted, though the reader makes no paragraph of it.
    assert_eq!(origin_of(&doc, story_of(&doc, "A note"), "A note"), Some(Origin { part: part(&names, "word/footnotes.xml"), index: 1 }));
    assert_eq!(origin_of(&doc, story_of(&doc, "A header"), "A header"), Some(Origin { part: part(&names, "word/header1.xml"), index: 0 }));
}

#[test]
fn keeps_the_source_package_byte_for_byte() {
    let (bytes, _) = sample();
    let doc = read(&bytes).unwrap();
    assert_eq!(doc.passthrough.get(SOURCE_PACKAGE).map(|kept| kept.as_slice()), Some(bytes.as_slice()));
}

#[test]
fn every_paragraph_read_from_the_body_has_an_origin() {
    let (bytes, _) = sample();
    let doc = read(&bytes).unwrap();
    assert!(doc.body.iter().filter_map(|block| block.as_para()).all(|p| p.origin.is_some()));
}

#[test]
fn gives_no_origin_to_a_paragraph_the_reader_adds() {
    // An empty body: the reader adds the one paragraph every story must hold.
    let entries = [
        ("[Content_Types].xml", CONTENT_TYPES.to_string()),
        ("_rels/.rels", root_rels()),
        ("word/document.xml", format!(r#"<w:document {W}><w:body><w:sectPr/></w:body></w:document>"#)),
    ];
    let doc = read(&zip(&entries)).unwrap();
    let paragraphs: Vec<_> = doc.body.iter().filter_map(|block| block.as_para()).collect();
    assert_eq!(paragraphs.len(), 1);
    assert_eq!(paragraphs[0].origin, None);
}

/// Equal text in the body and in two headers, each header's tree read and dropped in turn: every
/// paragraph takes the origin of its own part, never one another part's tree left at its address.
#[test]
fn equal_text_in_different_parts_keeps_each_parts_origin() {
    let same = "Same words";
    let body = format!(
        r#"{}{}<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rId1"/></w:sectPr></w:pPr></w:p>{}<w:sectPr><w:headerReference w:type="default" r:id="rId2"/></w:sectPr>"#,
        para("Before"),
        para(same),
        para(same)
    );
    let names = ["[Content_Types].xml", "_rels/.rels", "word/_rels/document.xml.rels", "word/document.xml", "word/header1.xml", "word/header2.xml"];
    let entries = [
        (names[0], CONTENT_TYPES.to_string()),
        (names[1], root_rels()),
        (
            names[2],
            format!(
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/header" Target="header1.xml"/><Relationship Id="rId2" Type="{REL}/header" Target="header2.xml"/></Relationships>"#
            ),
        ),
        (names[3], format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#)),
        (names[4], format!(r#"<w:hdr {W}>{}{}</w:hdr>"#, para("Header one"), para(same))),
        (names[5], format!(r#"<w:hdr {W}>{}{}{}</w:hdr>"#, para("Header two"), para("More"), para(same))),
    ];
    let doc = read(&zip(&entries)).unwrap();
    let origins = |story: StoryRef| -> Vec<Option<Origin>> {
        doc.para_paths(story).iter().filter_map(|path| doc.para(story, path)).filter(|p| p.plain_text() == same).map(|p| p.origin).collect()
    };
    let document = part(&names, "word/document.xml");
    assert_eq!(origins(StoryRef::Body), [Some(Origin { part: document, index: 1 }), Some(Origin { part: document, index: 3 })]);
    let headers: Vec<u32> = doc.parts.iter().filter(|(_, p)| p.kind == PartKind::Header).map(|(id, _)| *id).collect();
    assert_eq!(headers.len(), 2);
    let header_origins: Vec<Vec<Option<Origin>>> = headers.iter().map(|id| origins(StoryRef::Part(*id))).collect();
    assert!(header_origins.contains(&vec![Some(Origin { part: part(&names, "word/header1.xml"), index: 1 })]), "{header_origins:?}");
    assert!(header_origins.contains(&vec![Some(Origin { part: part(&names, "word/header2.xml"), index: 2 })]), "{header_origins:?}");
}

/// The kept package is charged to the read limits: an archive larger than the total the reader may
/// hold is refused, even when what it decompresses to fits.
#[test]
fn refuses_to_keep_an_archive_past_the_total_limit() {
    let entries = [
        ("[Content_Types].xml", CONTENT_TYPES.to_string()),
        ("_rels/.rels", root_rels()),
        ("word/document.xml", format!(r#"<w:document {W}><w:body>{}</w:body></w:document>"#, para("Stored"))),
    ];
    // Stored entries: the archive is its parts plus the zip's own headers, so larger than its parts.
    let bytes = zip_with(&entries, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored));
    let parts: u64 = entries.iter().map(|(_, text)| text.len() as u64).sum();
    assert!((bytes.len() as u64) > parts);
    let limits = |max_total_bytes: u64| ReadLimits { max_total_bytes, ..ReadLimits::default() };
    assert!(matches!(read_with_limits(&bytes, &limits(parts)), Err(DocxError::Limit(_))));
    let doc = read_with_limits(&bytes, &limits(bytes.len() as u64)).unwrap();
    assert_eq!(doc.passthrough.get(SOURCE_PACKAGE).map(|kept| kept.len()), Some(bytes.len()));
}

/// A text box shape holding `paragraphs`, as a `w:drawing` a run carries.
fn drawing(paragraphs: &str) -> String {
    format!(
        r#"<w:drawing><wp:inline><wp:extent cx="914400" cy="914400"/><wp:docPr id="1" name="Box"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent>{paragraphs}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing>"#
    )
}

/// A package of `word/document.xml` with `body`, and `extra` parts the document's relationships
/// (`rels`, inner XML) point at.
fn package(body: &str, rels: &str, extra: &[(&'static str, String)]) -> (Vec<u8>, Vec<&'static str>) {
    package_with(body, rels, extra, zip::write::SimpleFileOptions::default())
}

fn package_with(body: &str, rels: &str, extra: &[(&'static str, String)], options: zip::write::SimpleFileOptions) -> (Vec<u8>, Vec<&'static str>) {
    let mut entries = vec![
        ("[Content_Types].xml", CONTENT_TYPES.to_string()),
        ("_rels/.rels", root_rels()),
        (
            "word/_rels/document.xml.rels",
            format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#),
        ),
        ("word/document.xml", format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#)),
    ];
    entries.extend(extra.iter().cloned());
    let names = entries.iter().map(|(name, _)| *name).collect();
    (zip_with(&entries, options), names)
}

/// A drop cap merges into the paragraph it starts. The merged paragraph is neither `w:p` alone, so
/// it has no origin: with an empty paragraph after the letter, its text would equal the frame's,
/// and an edit could otherwise be written into the drop-cap frame.
#[test]
fn a_merged_drop_cap_has_no_origin() {
    let frame = r#"<w:p><w:pPr><w:framePr w:dropCap="drop" w:lines="3"/></w:pPr><w:r><w:t>A</w:t></w:r></w:p>"#;
    let (bytes, _) = package(&format!("{frame}<w:p/>"), "", &[]);
    let doc = read(&bytes).unwrap();
    let paragraphs: Vec<_> = doc.body.iter().filter_map(|block| block.as_para()).collect();
    assert_eq!(paragraphs.len(), 1, "the drop cap merges into the empty paragraph");
    assert_eq!((paragraphs[0].plain_text(), paragraphs[0].origin), ("A".to_string(), None));
}

/// Run-level alternative content: the reader takes the Fallback when it does not understand the
/// Choice, but the numbering counts the Choice's paragraphs first.
#[test]
fn a_fallback_paragraph_is_numbered_after_the_choice() {
    let content = format!(
        r#"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="x99">{}</mc:Choice><mc:Fallback>{}</mc:Fallback></mc:AlternateContent></w:r></w:p>"#,
        drawing(&format!("{}{}", para("Choice one"), para("Choice two"))),
        drawing(&para("From the fallback"))
    );
    let (bytes, names) = package(&content, "", &[]);
    let doc = read(&bytes).unwrap();
    let story = story_of(&doc, "From the fallback");
    assert_eq!(origin_of(&doc, story, "From the fallback"), Some(Origin { part: part(&names, "word/document.xml"), index: 3 }));
}

#[test]
fn numbers_nested_cells_controls_comments_and_endnotes() {
    let body = format!(
        r#"{}<w:tbl><w:tr><w:tc>{}<w:tbl><w:tr><w:tc>{}</w:tc></w:tr></w:tbl><w:p/></w:tc></w:tr></w:tbl><w:sdt><w:sdtPr/><w:sdtContent>{}</w:sdtContent></w:sdt>{}"#,
        para("Start"),
        para("Outer cell"),
        para("Inner cell"),
        para("In a control"),
        para("End")
    );
    let rels = format!(
        r#"<Relationship Id="rId1" Type="{REL}/comments" Target="comments.xml"/><Relationship Id="rId2" Type="{REL}/endnotes" Target="endnotes.xml"/>"#
    );
    let comments = format!(
        r#"<w:comments {W}><w:comment w:id="0" w:author="A">{}{}</w:comment><w:comment w:id="1" w:author="B">{}</w:comment></w:comments>"#,
        para("First comment"),
        para("First comment, more"),
        para("Second comment")
    );
    let endnotes = format!(
        r#"<w:endnotes {W}><w:endnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:endnote><w:endnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:endnote><w:endnote w:id="1">{}</w:endnote></w:endnotes>"#,
        para("An endnote")
    );
    let (bytes, names) = package(&body, &rels, &[("word/comments.xml", comments), ("word/endnotes.xml", endnotes)]);
    let doc = read(&bytes).unwrap();
    let document = part(&names, "word/document.xml");
    for (text, index) in [("Start", 0), ("Outer cell", 1), ("Inner cell", 2), ("In a control", 4), ("End", 5)] {
        assert_eq!(origin_of(&doc, story_of(&doc, text), text), Some(Origin { part: document, index }), "{text}");
    }
    let comments = part(&names, "word/comments.xml");
    assert_eq!(origin_of(&doc, story_of(&doc, "Second comment"), "Second comment"), Some(Origin { part: comments, index: 2 }));
    assert_eq!(origin_of(&doc, story_of(&doc, "An endnote"), "An endnote"), Some(Origin { part: part(&names, "word/endnotes.xml"), index: 2 }));
}

/// Two relationships whose targets differ only in case reach one part: it is read once, so no two
/// stories share its paragraphs' origins.
#[test]
fn a_part_reached_under_two_spellings_is_read_once() {
    let body = format!(
        r#"<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rId1"/></w:sectPr></w:pPr></w:p>{}<w:sectPr><w:headerReference w:type="default" r:id="rId2"/></w:sectPr>"#,
        para("Body")
    );
    let rels = format!(
        r#"<Relationship Id="rId1" Type="{REL}/header" Target="header1.xml"/><Relationship Id="rId2" Type="{REL}/header" Target="HEADER1.xml"/>"#
    );
    let (bytes, _) = package(&body, &rels, &[("word/header1.xml", format!(r#"<w:hdr {W}>{}</w:hdr>"#, para("The header")))]);
    let doc = read(&bytes).unwrap();
    assert_eq!(doc.parts.values().filter(|p| p.kind == PartKind::Header).count(), 1);
}

/// Printing a read document names the kept archive's length, never its bytes.
#[test]
fn debug_output_of_a_read_document_stays_short() {
    let padding = "x".repeat(1 << 20);
    // Stored, so the archive itself is over a megabyte.
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let (bytes, _) = package_with(&para("Short"), "", &[("customXml/item1.xml", padding)], stored);
    let doc = read(&bytes).unwrap();
    let printed = format!("{doc:?}");
    assert!(printed.len() < 100_000, "{} characters", printed.len());
    assert!(printed.contains(&bytes.len().to_string()), "the length is shown");
}

/// The kept archive compares equal between clones and between reads of equal bytes, and unequal
/// once its bytes differ.
#[test]
fn documents_compare_by_their_kept_archive() {
    let (bytes, _) = package(&para("Same"), "", &[]);
    let (other, _) = package(&para("Same"), "", &[("customXml/item1.xml", "<x/>".into())]);
    let doc = read(&bytes).unwrap();
    assert_eq!(doc.clone(), doc);
    assert_eq!(read(&bytes).unwrap(), doc);
    assert_ne!(read(&other).unwrap(), doc);
}

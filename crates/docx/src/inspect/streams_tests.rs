//! embed: tests of the decoded streams Clauflow's version comparison reads. Packages are built here
//! from zip entries; the helpers are this file's own so the inspector's tests stay as upstream has
//! them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use super::opc;
use super::streams::*;
use crate::package::{rt, zip_entries};

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape""#;
const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const MAIN_TYPE: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";
const HEADER_TYPE: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml";
const FOOTNOTES_TYPE: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml";
const SETTINGS_TYPE: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml";
const ANA: (&str, &str) = ("Ana Contoh", "2026-10-01T09:00:00Z");

fn text(content: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{content}</w:t></w:r>"#)
}
fn para(runs: &str) -> String {
    format!("<w:p>{runs}</w:p>")
}
fn ins((author, date): (&str, &str), content: &str) -> String {
    format!(r#"<w:ins w:id="9" w:author="{author}" w:date="{date}">{}</w:ins>"#, text(content))
}
fn del((author, date): (&str, &str), content: &str) -> String {
    format!(r#"<w:del w:id="8" w:author="{author}" w:date="{date}"><w:r><w:delText xml:space="preserve">{content}</w:delText></w:r></w:del>"#)
}

/// A package from its document body, the document's relationships, extra parts and overrides.
struct Docx {
    body: String,
    rels: Vec<(String, String, String, bool)>,
    parts: Vec<(String, Vec<u8>)>,
    overrides: Vec<(String, String)>,
    defaults: Vec<(String, String)>,
}

impl Docx {
    fn new(body: &str) -> Docx {
        Docx { body: body.to_string(), rels: Vec::new(), parts: Vec::new(), overrides: Vec::new(), defaults: Vec::new() }
    }
    fn rel(mut self, (id, kind, target, external): (&str, &str, &str, bool)) -> Self {
        self.rels.push((id.into(), kind.into(), target.into(), external));
        self
    }
    fn part(mut self, name: &str, text: &str) -> Self {
        self.parts.push((name.into(), text.as_bytes().to_vec()));
        self
    }
    fn bytes(mut self, name: &str, bytes: &[u8]) -> Self {
        self.parts.push((name.into(), bytes.to_vec()));
        self
    }
    fn typed(mut self, part: &str, kind: &str) -> Self {
        self.overrides.push((part.into(), kind.into()));
        self
    }
    fn default_type(mut self, extension: &str, kind: &str) -> Self {
        self.defaults.push((extension.into(), kind.into()));
        self
    }
    fn zip(&self) -> Vec<u8> {
        let mut types = String::from(
            r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>"#,
        );
        for (extension, kind) in &self.defaults {
            types.push_str(&format!(r#"<Default Extension="{extension}" ContentType="{kind}"/>"#));
        }
        types.push_str(&format!(r#"<Override PartName="/word/document.xml" ContentType="{MAIN_TYPE}"/>"#));
        for (part, kind) in &self.overrides {
            types.push_str(&format!(r#"<Override PartName="{part}" ContentType="{kind}"/>"#));
        }
        types.push_str("</Types>");
        let document = format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {W}><w:body>{}</w:body></w:document>"#, self.body);
        let mut entries = vec![
            ("[Content_Types].xml".to_string(), types.into_bytes()),
            ("_rels/.rels".to_string(), ROOT_RELS.as_bytes().to_vec()),
            ("word/document.xml".to_string(), document.into_bytes()),
        ];
        if !self.rels.is_empty() {
            let mut rels = String::from(
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            );
            for (id, kind, target, external) in &self.rels {
                let mode = if *external { r#" TargetMode="External""# } else { "" };
                rels.push_str(&format!(r#"<Relationship Id="{id}" Type="{kind}" Target="{target}"{mode}/>"#));
            }
            rels.push_str("</Relationships>");
            entries.push(("word/_rels/document.xml.rels".to_string(), rels.into_bytes()));
        }
        entries.extend(self.parts.iter().cloned());
        zip_entries(&entries).unwrap()
    }
}

fn package(body: &str) -> Vec<u8> {
    Docx::new(body).zip()
}

/// One footnote (id 2), one comment (id 0) on "clause", and a default header.
fn with_notes_comments_and_header() -> Vec<u8> {
    let body = format!(
        r#"<w:p><w:commentRangeStart w:id="0"/>{}<w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r><w:r><w:footnoteReference w:id="2"/></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rId5"/></w:sectPr>"#,
        text("clause")
    );
    let footnotes = format!(
        r#"<w:footnotes {W}><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:id="2"><w:p>{}</w:p></w:footnote></w:footnotes>"#,
        text("A note.")
    );
    let header = format!(r#"<w:hdr {W}><w:p>{}</w:p></w:hdr>"#, text("Header text"));
    let comments = format!(
        r#"<w:comments {W}><w:comment w:id="0" w:author="Sari Contoh" w:date="2026-10-02T10:00:00Z" w:initials="SC"><w:p>{}</w:p></w:comment></w:comments>"#,
        text("Please check clause 4.")
    );
    Docx::new(&body)
        .rel(("rId5", rt::HEADER, "header1.xml", false))
        .rel(("rId6", rt::FOOTNOTES, "footnotes.xml", false))
        .rel(("rId7", rt::COMMENTS, "comments.xml", false))
        .part("word/header1.xml", &header)
        .part("word/footnotes.xml", &footnotes)
        .part("word/comments.xml", &comments)
        .typed("/word/header1.xml", HEADER_TYPE)
        .typed("/word/footnotes.xml", FOOTNOTES_TYPE)
        .typed("/word/comments.xml", opc::COMMENTS_TYPE)
        .zip()
}

/// The fictional sample agreements the engine's tests already use.
fn sample_packages() -> Vec<(String, Vec<u8>)> {
    let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../engine/tests/fixtures/word-samples");
    let mut samples: Vec<(String, Vec<u8>)> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "docx"))
        .map(|path| (path.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&path).unwrap()))
        .collect();
    samples.sort();
    assert!(samples.len() >= 4, "the sample agreements are missing from {}", folder.display());
    samples
}

fn read_one(bytes: &[u8]) -> Streams {
    read_pair(bytes, bytes, &default_limits(bytes.len(), bytes.len())).unwrap().left
}

fn chars(story: &Story) -> String {
    story.items.iter().filter_map(|item| match item { StreamItem::Char { ch, .. } => Some(*ch), _ => None }).collect()
}

fn origins(story: &Story) -> Vec<u32> {
    story.items.iter().filter_map(|item| match item { StreamItem::ParagraphEnd { origin, .. } => Some(*origin), _ => None }).collect()
}

#[test]
fn reads_body_text_and_paragraph_ends_with_origins() {
    let streams = read_one(&package(&format!("{}{}", para(&text("One")), para(&text("Two")))));
    let body = &streams.stories[0];
    assert_eq!(body.id, StoryId::Body);
    assert_eq!(body.part, "word/document.xml");
    assert_eq!(chars(body), "OneTwo");
    assert_eq!(origins(body), vec![0, 1]);
    assert!(!body.items.iter().any(|item| matches!(item, StreamItem::Opaque { element, .. } if element == "w:document" || element == "w:body")));
}

#[test]
fn origin_counts_text_box_paragraphs_in_pre_order_like_the_model_reader() {
    // A paragraph holding a text box with one paragraph, in both branches of mc:AlternateContent,
    // then a plain paragraph: the model numbers 0 (outer), 1 (choice box), 2 (fallback box), 3.
    let boxed = r#"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wps:txbx><w:txbxContent><w:p><w:r><w:t>Box</w:t></w:r></w:p></w:txbxContent></wps:txbx></w:drawing></mc:Choice><mc:Fallback><w:pict><w:txbxContent><w:p><w:r><w:t>Box</w:t></w:r></w:p></w:txbxContent></w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p>"#;
    let streams = read_one(&package(&format!("{boxed}{}", para(&text("After")))));
    assert_eq!(origins(&streams.stories[0]), vec![0, 3]);
    let boxes: Vec<&Story> = streams.stories.iter().filter(|story| matches!(story.id, StoryId::TextBox { .. })).collect();
    assert_eq!(boxes.len(), 1, "only the first alternative is read as a story");
    assert_eq!(boxes[0].id, StoryId::TextBox { part: "word/document.xml".into(), anchor: 0, ordinal: 0 });
    assert_eq!(chars(boxes[0]), "Box");
    assert_eq!(origins(boxes[0]), vec![1]);
}

#[test]
fn origin_matches_the_model_reader() {
    // Spike question 1: for every sample agreement, the stream's origin of each body paragraph
    // equals `Origin.index` of the model paragraph read from the same `w:p`.
    for (name, bytes) in sample_packages() {
        let streams = read_one(&bytes);
        let document = crate::read(&bytes).unwrap();
        let mut model: Vec<(u32, String)> = Vec::new();
        for path in document.para_paths(wordcraft_doc::StoryRef::Body) {
            let paragraph = document.para(wordcraft_doc::StoryRef::Body, &path).unwrap();
            if let Some(origin) = paragraph.origin {
                model.push((origin.index, paragraph.plain_text().to_string()));
            }
        }
        assert!(!model.is_empty(), "{name}: the model has no paragraphs with origins");
        let mut streamed: Vec<(u32, String)> = Vec::new();
        let mut current = String::new();
        for item in &streams.stories[0].items {
            match item {
                // The model's plain text keeps tracked deletions (it displays them), so they count.
                StreamItem::Char { ch, field, .. } if *field != FieldRole::Instruction => current.push(*ch),
                StreamItem::ParagraphEnd { origin, .. } => streamed.push((*origin, std::mem::take(&mut current))),
                _ => {}
            }
        }
        let normal = |text: &str| text.chars().filter(|character| !character.is_whitespace() && !character.is_control()).collect::<String>();
        for (origin, text) in &model {
            let found = streamed.iter().find(|(index, _)| index == origin).unwrap_or_else(|| panic!("{name}: origin {origin} missing"));
            assert_eq!(normal(&found.1), normal(text), "{name}: origin {origin}");
        }
    }
}

#[test]
fn tracked_insertions_and_deletions_carry_author_and_date() {
    let body = para(&format!("{}{}{}", text("Due in "), del(ANA, "thirty"), ins(ANA, "sixty")));
    let streams = read_one(&package(&body));
    let revisions: Vec<Option<RevisionKind>> =
        streams.stories[0].items.iter().filter_map(|item| match item { StreamItem::Char { revision, .. } => Some(revision.as_ref().map(|r| r.kind)), _ => None }).collect();
    assert_eq!(revisions.iter().filter(|kind| **kind == Some(RevisionKind::Deleted)).count(), 6);
    assert_eq!(revisions.iter().filter(|kind| **kind == Some(RevisionKind::Inserted)).count(), 5);
    let revision = streams.stories[0]
        .items
        .iter()
        .find_map(|item| match item { StreamItem::Char { revision: Some(revision), .. } => Some(revision), _ => None })
        .unwrap();
    assert_eq!(revision.author, "Ana Contoh");
    assert_eq!(revision.date.as_deref(), Some("2026-10-01T09:00:00Z"));
}

#[test]
fn equal_properties_share_a_format_id_across_the_pair() {
    let bold = r#"<w:r><w:rPr><w:b/></w:rPr><w:t>Bold</w:t></w:r>"#;
    let italic = r#"<w:r><w:rPr><w:i/></w:rPr><w:t>Bold</w:t></w:r>"#;
    let left = package(&para(bold));
    let right = package(&format!("{}{}", para(&format!("{bold}{}", text(" plain"))), para(italic)));
    let pair = read_pair(&left, &right, &default_limits(left.len(), right.len())).unwrap();
    let formats = |streams: &Streams| -> Vec<FormatId> {
        streams.stories[0].items.iter().filter_map(|item| match item { StreamItem::Char { format, .. } => Some(*format), _ => None }).collect()
    };
    let (left, right) = (formats(&pair.left), formats(&pair.right));
    assert_eq!(left[0], right[0]);
    assert_ne!(left[0], *right.last().unwrap(), "bold and italic differ");
}

#[test]
fn hyperlink_targets_are_readable() {
    let link = r#"<w:hyperlink r:id="rId7"><w:r><w:t>terms</w:t></w:r></w:hyperlink><w:hyperlink w:anchor="clause4"><w:r><w:t>clause 4</w:t></w:r></w:hyperlink>"#;
    let bytes = Docx::new(&para(link)).rel(("rId7", rt::HYPERLINK, "https://contoh.test/terms", true)).zip();
    let items = read_one(&bytes).stories.remove(0).items;
    assert!(items.iter().any(|item| matches!(item, StreamItem::HyperlinkStart { target: Some(target), anchor: None } if target == "https://contoh.test/terms")));
    assert!(items.iter().any(|item| matches!(item, StreamItem::HyperlinkStart { target: None, anchor: Some(anchor) } if anchor == "clause4")));
    assert_eq!(items.iter().filter(|item| matches!(item, StreamItem::HyperlinkEnd)).count(), 2);
}

#[test]
fn simple_field_instructions_and_field_roles_are_readable() {
    let fields = r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> DATE \@ "d MMMM yyyy" </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>1 October 2026</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:fldSimple w:instr=" PAGE "><w:r><w:t>3</w:t></w:r></w:fldSimple><w:r><w:t>after</w:t></w:r>"#;
    let streams = read_one(&package(&para(fields)));
    let items = &streams.stories[0].items;
    assert!(items.iter().any(|item| matches!(item, StreamItem::SimpleFieldStart { instruction } if instruction.trim() == "PAGE")));
    assert!(items.iter().any(|item| matches!(item, StreamItem::SimpleFieldEnd)));
    assert_eq!(items.iter().filter(|item| matches!(item, StreamItem::FieldBegin | StreamItem::FieldSeparate | StreamItem::FieldEnd)).count(), 3);
    let of = |role: FieldRole| -> String { items.iter().filter_map(|item| match item { StreamItem::Char { ch, field, .. } if *field == role => Some(*ch), _ => None }).collect() };
    assert!(of(FieldRole::Instruction).contains("DATE"));
    assert_eq!(of(FieldRole::Result), "1 October 20263");
    assert_eq!(of(FieldRole::None), "after");
}

#[test]
fn notes_comments_and_sections_carry_their_ids() {
    let streams = read_one(&with_notes_comments_and_header());
    let body = &streams.stories[0];
    assert!(body.items.iter().any(|item| matches!(item, StreamItem::NoteReference { kind: NoteKind::Footnote, id } if id == "2")));
    assert!(body.items.iter().any(|item| matches!(item, StreamItem::CommentStart { id } if id == "0")));
    assert!(body.items.iter().any(|item| matches!(item, StreamItem::CommentEnd { id } if id == "0")));
    assert!(body.items.iter().any(|item| matches!(item, StreamItem::CommentReference { id } if id == "0")));
    assert!(body.items.iter().any(|item| matches!(item, StreamItem::SectionBreak { headers, .. } if headers == &vec![(HeaderSlot::Default, "word/header1.xml".to_string())])));
    let note = streams.stories.iter().find(|story| story.id == StoryId::Footnote { id: "2".into() }).unwrap();
    assert_eq!(chars(note), "A note.");
    let header = streams.stories.iter().find(|story| story.id == StoryId::Header { part: "word/header1.xml".into() }).unwrap();
    assert_eq!(chars(header), "Header text");
    let comment = &streams.comments[0];
    assert_eq!((comment.id.as_str(), comment.author.as_str(), comment.text.as_str(), comment.done), ("0", "Sari Contoh", "Please check clause 4.", false));
    assert_eq!(comment.date.as_deref(), Some("2026-10-02T10:00:00Z"));
    // Body first, then the header, then the note.
    let order: Vec<&StoryId> = streams.stories.iter().map(|story| &story.id).collect();
    let position = |id: &StoryId| order.iter().position(|each| *each == id).unwrap();
    assert!(position(&StoryId::Body) < position(&StoryId::Header { part: "word/header1.xml".into() }));
    assert!(position(&StoryId::Header { part: "word/header1.xml".into() }) < position(&StoryId::Footnote { id: "2".into() }));
}

#[test]
fn tables_rows_and_cells_are_marked_with_their_changes() {
    let table = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:p>{}</w:p></w:tc></w:tr><w:tr><w:trPr><w:ins w:id="3" w:author="Ana Contoh" w:date="2026-10-01T09:00:00Z"/></w:trPr><w:tc><w:p>{}</w:p></w:tc></w:tr></w:tbl>"#,
        text("Cell one"),
        ins(ANA, "Cell two")
    );
    let items = read_one(&package(&table)).stories.remove(0).items;
    let count = |test: fn(&StreamItem) -> bool| items.iter().filter(|item| test(item)).count();
    assert_eq!(count(|item| matches!(item, StreamItem::TableStart { .. })), 1);
    assert_eq!(count(|item| matches!(item, StreamItem::TableEnd)), 1);
    assert_eq!(count(|item| matches!(item, StreamItem::RowStart { .. })), 2);
    assert_eq!(count(|item| matches!(item, StreamItem::RowEnd)), 2);
    assert_eq!(count(|item| matches!(item, StreamItem::CellStart { .. })), 2);
    assert_eq!(count(|item| matches!(item, StreamItem::CellEnd)), 2);
    assert!(items.iter().any(|item| matches!(item, StreamItem::RowStart { revision: Some(Revision { kind: RevisionKind::Inserted, .. }), .. })));
}

#[test]
fn pictures_name_their_media_and_alternative_text() {
    let picture = r#"<w:p><w:r><w:drawing><wp:inline><wp:docPr id="1" name="Logo" descr="Company logo"/><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
    let bytes = Docx::new(picture)
        .rel(("rId9", rt::IMAGE, "media/image1.png", false))
        .bytes("word/media/image1.png", &[0x89, b'P', b'N', b'G', 1, 2, 3])
        .default_type("png", "image/png")
        .zip();
    let streams = read_one(&bytes);
    let object = streams.stories[0].items.iter().find(|item| matches!(item, StreamItem::Object { .. })).unwrap();
    let StreamItem::Object { kind, digest, media, alt } = object else { panic!("not an object: {object:?}") };
    assert_eq!(*kind, ObjectKind::Drawing);
    assert_eq!(digest.len(), 64);
    assert_eq!(alt.as_deref(), Some("Company logo"));
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].part, "word/media/image1.png");
    assert_eq!(Some(&media[0].sha256), streams.parts.get("word/media/image1.png"));
}

#[test]
fn settings_report_track_changes_and_ignore_rsids() {
    let with = |track: bool, rsid: &str| {
        Docx::new(&para(&text("A")))
            .rel(("rId9", rt::SETTINGS, "settings.xml", false))
            .part(
                "word/settings.xml",
                &format!(
                    r#"<w:settings {W}>{}<w:proofState w:spelling="clean"/><w:rsids><w:rsidRoot w:val="{rsid}"/></w:rsids></w:settings>"#,
                    if track { "<w:trackRevisions/>" } else { "" }
                ),
            )
            .typed("/word/settings.xml", SETTINGS_TYPE)
            .zip()
    };
    let (a, b, c) = (read_one(&with(true, "00A1")), read_one(&with(true, "00B2")), read_one(&with(false, "00A1")));
    assert!(a.settings.track_revisions && !c.settings.track_revisions);
    assert_eq!(a.settings.other_digest, b.settings.other_digest);
    assert_eq!(a.settings.other_digest, c.settings.other_digest, "the tracking switch is reported on its own");
    assert!(!a.parts.contains_key("word/settings.xml"), "settings are reported through `settings`, not `parts`");
    assert!(a.part_order.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn reports_package_sides_by_their_errors() {
    let good = package(&para(&text("A")));
    let bad = b"not a zip".to_vec();
    assert!(matches!(read_pair(&bad, &good, &default_limits(bad.len(), good.len())), Err(super::InspectError::Base(_))));
    assert!(matches!(read_pair(&good, &bad, &default_limits(good.len(), bad.len())), Err(super::InspectError::Edited(_))));
}

#[test]
fn inspect_answers_are_unchanged() {
    // Streams mode must not change the save check.
    let left = package(&para(&text("Fee 5%")));
    let right = package(&para(&text("Fee 50%")));
    let inspection = super::inspect(&left, &right).unwrap();
    assert_eq!(inspection.untracked_changes, 1);
}

#[test]
fn refuses_what_the_inspector_refuses() {
    let tracked_table = r#"<w:p><w:ins w:id="1" w:author="Ana Contoh" w:date="2026-10-01T09:00:00Z"><w:tbl/></w:ins></w:p>"#;
    let bytes = package(tracked_table);
    assert!(super::inspect(&bytes, &bytes).is_err());
    assert!(read_pair(&bytes, &bytes, &default_limits(bytes.len(), bytes.len())).is_err());
}

#[test]
fn reports_work_and_memory_spent() {
    let bytes = package(&para(&text("A")));
    let pair = read_pair(&bytes, &bytes, &default_limits(bytes.len(), bytes.len())).unwrap();
    assert!(pair.work_spent > 0 && pair.peak_memory > 0);
}

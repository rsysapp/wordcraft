//! embed: tests of the save inspector. Every package is built here from zip entries; a case an
//! attacker could craft carries the rule that refuses it in its name.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use super::diff::{Op, diff};
use super::*;
use crate::package::zip_entries;

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
const SARI: (&str, &str) = ("Sari Wulan", "2026-10-10T08:00:00Z");
const ANA: (&str, &str) = ("Ana Setia", "2026-10-01T09:00:00Z");

fn document(body: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {W}><w:body>{body}</w:body></w:document>"#)
}

fn package(body: &str, extra: &[(&str, &str)]) -> Vec<u8> {
    let mut entries = vec![
        ("[Content_Types].xml".to_string(), CONTENT_TYPES.as_bytes().to_vec()),
        ("_rels/.rels".to_string(), ROOT_RELS.as_bytes().to_vec()),
        ("word/document.xml".to_string(), document(body).into_bytes()),
    ];
    entries.extend(extra.iter().map(|(name, text)| (name.to_string(), text.as_bytes().to_vec())));
    zip_entries(&entries).unwrap()
}

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

#[test]
fn finds_nothing_in_an_unchanged_package() {
    let base = package(&para(&text("Rent is due monthly.")), &[]);
    assert_eq!(inspect(&base, &base).unwrap(), Inspection::default());
}

#[test]
fn ignores_revision_ids_and_rsids() {
    let base = package(r#"<w:p w:rsidR="00A1"><w:r><w:t>Rent is due.</w:t></w:r></w:p>"#, &[]);
    let edited = package(r#"<w:p w:rsidR="00B2" w:rsidRDefault="00B2"><w:r w:rsidRPr="00C3"><w:t>Rent is due.</w:t></w:r></w:p>"#, &[]);
    assert_eq!(inspect(&base, &edited).unwrap(), Inspection::default());
}

#[test]
fn reports_a_tracked_insertion_by_its_author() {
    let base = package(&para(&text("Rent is due.")), &[]);
    let edited = package(&para(&format!("{}{}{}", text("Rent is due"), ins(SARI, " monthly"), text("."))), &[]);
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(counts(&inspection), vec![("Sari Wulan".into(), 1, 0, 0)]);
    assert_eq!((inspection.untracked_changes, inspection.losses.len()), (0, 0));
}

#[test]
fn reports_a_tracked_deletion_by_its_author() {
    let base = package(&para(&text("Rent is due monthly.")), &[]);
    let edited = package(&para(&format!("{}{}{}", text("Rent is due"), del(SARI, " monthly"), text("."))), &[]);
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(counts(&inspection), vec![("Sari Wulan".into(), 0, 1, 0)]);
    assert_eq!(inspection.untracked_changes, 0);
}

#[test]
fn reports_an_untracked_edit() {
    let base = package(&para(&text("Rent is due monthly.")), &[]);
    let edited = package(&para(&text("Rent is due weekly.")), &[]);
    let inspection = inspect(&base, &edited).unwrap();
    assert!(inspection.untracked_changes > 0);
    assert!(inspection.new_revisions.is_empty());
}

#[test]
fn reports_a_tracked_change_in_another_author_s_name_as_that_author_s() {
    let base = package(&para(&text("Rent.")), &[]);
    let edited = package(&para(&format!("{}{}", text("Rent."), ins(("Someone Else", "2026-10-10T08:00:00Z"), " Due monthly."))), &[]);
    assert_eq!(inspect(&base, &edited).unwrap().new_revisions[0].author, "Someone Else");
}

#[test]
fn reports_an_accepted_and_a_rejected_base_revision() {
    let base = package(&para(&format!("{}{}{}", text("Rent"), ins(ANA, " is due"), del(ANA, " monthly"))), &[]);
    // Accepting both: the insertion becomes plain text, the deleted text goes.
    let accepted = package(&para(&text("Rent is due")), &[]);
    assert_eq!(inspect(&base, &accepted).unwrap().resolved, Resolved { accepted: 2, rejected: 0 });
    // Rejecting both: the insertion goes, the deleted text comes back.
    let rejected = package(&para(&text("Rent monthly")), &[]);
    assert_eq!(inspect(&base, &rejected).unwrap().resolved, Resolved { accepted: 0, rejected: 2 });
}

#[test]
fn keeps_base_revisions_left_as_they_were() {
    let base = package(&para(&format!("{}{}", text("Rent"), ins(ANA, " is due"))), &[]);
    let edited = package(&para(&format!("{}{}{}", text("Rent"), ins(ANA, " is due"), ins(SARI, " monthly"))), &[]);
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!((inspection.resolved, inspection.altered_revisions, inspection.untracked_changes), (Resolved::default(), 0, 0));
    assert_eq!(inspection.new_revisions.len(), 1);
}

#[test]
fn reports_a_dropped_element() {
    let base = package(&para(&format!(r#"<w:bookmarkStart w:id="0" w:name="Price"/>{}<w:bookmarkEnd w:id="0"/>"#, text("Price"))), &[]);
    let edited = package(&para(&text("Price")), &[]);
    let losses = inspect(&base, &edited).unwrap().losses;
    assert!(losses.contains(&Loss {
        part: "word/document.xml".into(),
        kind: LossKind::ElementMissing,
        element: Some("w:bookmarkStart".into()),
        count: 1
    }));
}

#[test]
fn reports_a_missing_part_and_an_altered_one() {
    let styles = |size: &str| {
        format!(r#"<w:styles {W}><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="{size}"/></w:rPr></w:rPrDefault></w:docDefaults></w:styles>"#)
    };
    let base = package(&para(&text("Rent.")), &[("customXml/item1.xml", "<data>kept</data>"), ("word/styles.xml", &styles("22"))]);
    let edited = package(&para(&text("Rent.")), &[("word/styles.xml", &styles("24"))]);
    let losses = inspect(&base, &edited).unwrap().losses;
    assert!(losses.contains(&Loss { part: "customXml/item1.xml".into(), kind: LossKind::PartMissing, element: None, count: 1 }));
    assert!(losses.contains(&Loss { part: "word/styles.xml".into(), kind: LossKind::PartAltered, element: None, count: 1 }));
}

#[test]
fn ignores_regenerated_bookkeeping_and_the_tracking_switch() {
    let settings = |extra: &str| format!(r#"<w:settings {W}><w:zoom w:percent="100"/>{extra}</w:settings>"#);
    let base = package(
        &para(&text("Rent.")),
        &[
            ("docProps/app.xml", "<Properties><Application>Word</Application></Properties>"),
            ("word/settings.xml", &settings(r#"<w:rsids><w:rsidRoot w:val="00A1"/></w:rsids>"#)),
        ],
    );
    let edited = package(
        &para(&text("Rent.")),
        &[("docProps/app.xml", "<Properties><Application>Other</Application></Properties>"), ("word/settings.xml", &settings("<w:trackRevisions/>"))],
    );
    assert_eq!(inspect(&base, &edited).unwrap(), Inspection::default());
}

#[test]
fn stops_at_the_bound_and_says_so() {
    let base = package(&para(&text("one two three four five six")), &[]);
    let edited = package(&para(&text("uno dos tres cuatro cinco seis")), &[]);
    assert!(inspect_with_limit(&base, &edited, 2).unwrap().too_many_changes);
}

// A host's server inspects inside a small memory budget: a package over the element budget
// is unreadable, never read with the desktop's limits.
#[test]
fn refuses_a_package_over_the_element_budget_naming_which() {
    let base = package(&para(&text("Rent.")), &[]);
    let many: String = (0..2_000).map(|n| para(&text(&format!("Clause {n}.")))).collect();
    let edited = package(&many, &[]);
    let limits = ReadLimits { max_elements: 1_000, ..INSPECT_LIMITS };
    assert!(matches!(inspect_within(&base, &edited, &limits, MAX_CHANGES), Err(InspectError::Edited(_))));
    assert!(matches!(inspect_within(&edited, &base, &limits, MAX_CHANGES), Err(InspectError::Base(_))));
    assert!(inspect(&base, &edited).is_ok());
}

#[test]
fn reads_with_the_embed_build_s_limits() {
    assert_eq!((INSPECT_LIMITS.max_part_bytes, INSPECT_LIMITS.max_total_bytes, INSPECT_LIMITS.max_elements), (64 << 20, 192 << 20, 2_000_000));
}

#[test]
fn refuses_a_file_that_is_not_a_word_package_naming_which() {
    let base = package(&para(&text("Rent.")), &[]);
    assert!(matches!(inspect(b"not a zip at all", &base), Err(InspectError::Base(_))));
    assert!(matches!(inspect(&base, b"%PDF-1.7"), Err(InspectError::Edited(_))));
    let no_document = zip_entries(&[("[Content_Types].xml".to_string(), CONTENT_TYPES.as_bytes().to_vec())]).unwrap();
    assert!(matches!(inspect(&base, &no_document), Err(InspectError::Edited(_))));
}

#[test]
fn diff_finds_the_shortest_script() {
    let (a, b) = (['a', 'b', 'c', 'e'], ['a', 'c', 'd', 'e']);
    let ops = diff(&a, &b, 10).unwrap();
    let same: Vec<(usize, usize)> = ops.iter().filter_map(|op| if let Op::Same(left, right) = op { Some((*left, *right)) } else { None }).collect();
    assert_eq!(same, vec![(0, 0), (2, 1), (3, 3)]);
    assert!(same.iter().all(|(left, right)| a[*left] == b[*right]));
    assert_eq!(ops.iter().filter(|op| matches!(op, Op::Delete(1))).count(), 1);
    assert_eq!(ops.iter().filter(|op| matches!(op, Op::Insert(2))).count(), 1);
    assert_eq!(diff::<char>(&[], &[], 0), Some(vec![]));
    assert_eq!(diff(&['a'], &['b'], 1), None);
}

// ---- Packages with relationships, content types and comments ----------------------------------

const MAIN_TYPE: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";
const HEADER_TYPE: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml";
const FOOTNOTES_TYPE: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml";
const W14: &str = r#"xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml""#;
const W15: &str = r#"xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml""#;

/// A relationship: id, type, target, external.
type R<'a> = (&'a str, &'a str, &'a str, bool);

fn rels_xml(list: &[R<'_>]) -> String {
    let mut out =
        String::from(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#);
    for (id, kind, target, external) in list {
        let mode = if *external { r#" TargetMode="External""# } else { "" };
        out.push_str(&format!(r#"<Relationship Id="{id}" Type="{kind}" Target="{target}"{mode}/>"#));
    }
    out.push_str("</Relationships>");
    out
}

fn types_xml(main: &str, overrides: &[(&str, &str)], defaults: &[(&str, &str)]) -> String {
    let mut out = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>"#,
    );
    for (extension, kind) in defaults {
        out.push_str(&format!(r#"<Default Extension="{extension}" ContentType="{kind}"/>"#));
    }
    out.push_str(&format!(r#"<Override PartName="/word/document.xml" ContentType="{main}"/>"#));
    for (part, kind) in overrides {
        out.push_str(&format!(r#"<Override PartName="{part}" ContentType="{kind}"/>"#));
    }
    out.push_str("</Types>");
    out
}

/// A package from its document body, the document's relationships, extra parts and overrides.
struct Docx<'a> {
    main: &'a str,
    body: String,
    rels: Vec<R<'a>>,
    parts: Vec<(&'a str, Vec<u8>)>,
    overrides: Vec<(&'a str, &'a str)>,
    defaults: Vec<(&'a str, &'a str)>,
}

impl<'a> Docx<'a> {
    fn new(body: &str) -> Docx<'a> {
        Docx { main: MAIN_TYPE, body: body.to_string(), rels: Vec::new(), parts: Vec::new(), overrides: Vec::new(), defaults: Vec::new() }
    }
    fn rel(mut self, rel: R<'a>) -> Self {
        self.rels.push(rel);
        self
    }
    fn part(mut self, name: &'a str, text: &str) -> Self {
        self.parts.push((name, text.as_bytes().to_vec()));
        self
    }
    fn bytes(mut self, name: &'a str, bytes: &[u8]) -> Self {
        self.parts.push((name, bytes.to_vec()));
        self
    }
    fn typed(mut self, part: &'a str, kind: &'a str) -> Self {
        self.overrides.push((part, kind));
        self
    }
    fn default_type(mut self, extension: &'a str, kind: &'a str) -> Self {
        self.defaults.push((extension, kind));
        self
    }
    fn main(mut self, kind: &'a str) -> Self {
        self.main = kind;
        self
    }
    fn entries(&self) -> Vec<(String, Vec<u8>)> {
        let mut entries = vec![
            ("[Content_Types].xml".to_string(), types_xml(self.main, &self.overrides, &self.defaults).into_bytes()),
            ("_rels/.rels".to_string(), ROOT_RELS.as_bytes().to_vec()),
            ("word/document.xml".to_string(), document(&self.body).into_bytes()),
        ];
        if !self.rels.is_empty() {
            entries.push(("word/_rels/document.xml.rels".to_string(), rels_xml(&self.rels).into_bytes()));
        }
        entries.extend(self.parts.iter().map(|(name, bytes)| (name.to_string(), bytes.clone())));
        entries
    }
    fn zip(&self) -> Vec<u8> {
        zip_entries(&self.entries()).unwrap()
    }
}

/// A comment: id, author, text, the w14:paraId of its paragraph.
type C<'a> = (&'a str, &'a str, &'a str, &'a str);

fn comments_xml(list: &[C<'_>]) -> String {
    let mut out = format!(r#"<w:comments {W} {W14}>"#);
    for (id, author, content, para) in list {
        out.push_str(&format!(r#"<w:comment w:id="{id}" w:author="{author}" w:date="2026-10-01T09:00:00Z" w:initials="X"><w:p w14:paraId="{para}">{}</w:p></w:comment>"#, text(content)));
    }
    out.push_str("</w:comments>");
    out
}

/// A comment's state: its paraId, the paraId of the comment it answers, done.
fn comments_ex_xml(list: &[(&str, Option<&str>, bool)]) -> String {
    let mut out = format!(r#"<w15:commentsEx {W15}>"#);
    for (para, parent, done) in list {
        let parent = parent.map(|parent| format!(r#" w15:paraIdParent="{parent}""#)).unwrap_or_default();
        out.push_str(&format!(r#"<w15:commentEx w15:paraId="{para}"{parent} w15:done="{}"/>"#, if *done { 1 } else { 0 }));
    }
    out.push_str("</w15:commentsEx>");
    out
}

fn anchored(id: &str, content: &str) -> String {
    format!(r#"<w:commentRangeStart w:id="{id}"/>{}<w:commentRangeEnd w:id="{id}"/><w:r><w:commentReference w:id="{id}"/></w:r>"#, text(content))
}

/// A document with comments: its body, the comments, their states.
fn with_comments(body: &str, list: &[C<'_>], states: &[(&str, Option<&str>, bool)]) -> Vec<u8> {
    Docx::new(body)
        .rel(("rId7", rt::COMMENTS, "comments.xml", false))
        .rel(("rId8", rt::COMMENTS_EX, "commentsExtended.xml", false))
        .part("word/comments.xml", &comments_xml(list))
        .part("word/commentsExtended.xml", &comments_ex_xml(states))
        .typed("/word/comments.xml", opc::COMMENTS_TYPE)
        .typed("/word/commentsExtended.xml", opc::COMMENTS_EX_TYPE)
        .zip()
}

fn clean(inspection: &Inspection) -> bool {
    inspection.resolved == Resolved::default()
        && inspection.altered_revisions == 0
        && inspection.untracked_changes == 0
        && inspection.losses.is_empty()
        && !inspection.too_many_changes
        && inspection.added_parts.is_empty()
        && inspection.changed_relationships.is_empty()
        && inspection.content_types.is_empty()
        && inspection.comments.removed == 0
        && inspection.comments.altered == 0
}

/// Each new author with their insertions, deletions and formatting changes.
fn counts(inspection: &Inspection) -> Vec<(String, u32, u32, u32)> {
    inspection.new_revisions.iter().map(|author| (author.author.clone(), author.insertions, author.deletions, author.format_changes)).collect()
}

// ---- Adversarial packages: each rule's case -------------------------------------------------------

/// a macro project added, its relationship and the main part switched to macro-enabled.
#[test]
fn reports_a_macro_project_added() {
    let base = Docx::new(&para(&text("Rent."))).zip();
    let edited = Docx::new(&para(&text("Rent.")))
        .main("application/vnd.ms-word.document.macroEnabled.main+xml")
        .rel(("rId9", "http://schemas.microsoft.com/office/2006/relationships/vbaProject", "vbaProject.bin", false))
        .bytes("word/vbaProject.bin", b"\xD0\xCF\x11\xE0 macro")
        .default_type("bin", "application/vnd.ms-office.vbaProject")
        .zip();
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(
        inspection.added_parts,
        vec![AddedPart { part: "word/vbaProject.bin".into(), content_type: Some("application/vnd.ms-office.vbaProject".into()) }]
    );
    assert_eq!(inspection.changed_relationships.len(), 1);
    assert_eq!(inspection.changed_relationships[0].change, Change::Added);
    assert!(inspection.changed_relationships[0].kind.ends_with("vbaProject"));
    assert_eq!(
        inspection.content_types,
        vec![ContentTypeChange {
            part: "word/document.xml".into(),
            base: Some(MAIN_TYPE.into()),
            edited: Some("application/vnd.ms-word.document.macroEnabled.main+xml".into())
        }]
    );
}

/// an embedded object, an alternative chunk and custom XML, each added, are each a part.
#[test]
fn reports_every_added_part_with_its_content_type() {
    let base = Docx::new(&para(&text("Rent."))).zip();
    let edited = Docx::new(&para(&text("Rent.")))
        .bytes("word/embeddings/oleObject1.bin", b"object")
        .part("word/afchunk.html", "<html>Pay nothing.</html>")
        .part("customXml/item9.xml", "<data/>")
        .default_type("html", "text/html")
        .zip();
    let added: Vec<(String, Option<String>)> =
        inspect(&base, &edited).unwrap().added_parts.into_iter().map(|part| (part.part, part.content_type)).collect();
    assert_eq!(
        added,
        vec![
            ("customXml/item9.xml".into(), Some("application/xml".into())),
            ("word/afchunk.html".into(), Some("text/html".into())),
            ("word/embeddings/oleObject1.bin".into(), None)
        ]
    );
}

/// an external hyperlink retargeted is a changed relationship, and the link in the text
/// reads differently too.
#[test]
fn reports_an_external_hyperlink_target_changed() {
    let body = para(&format!(r#"<w:hyperlink r:id="rId5">{}</w:hyperlink>"#, text("the terms")));
    let base = Docx::new(&body).rel(("rId5", rt::HYPERLINK, "https://a.example/terms", true)).zip();
    let edited = Docx::new(&body).rel(("rId5", rt::HYPERLINK, "https://b.example/terms", true)).zip();
    let inspection = inspect(&base, &edited).unwrap();
    let changes: Vec<(Change, &str)> = inspection.changed_relationships.iter().map(|change| (change.change, change.target.as_str())).collect();
    assert_eq!(changes, vec![(Change::Removed, "https://a.example/terms"), (Change::Added, "https://b.example/terms")]);
    assert!(inspection.changed_relationships.iter().all(|change| change.source == "word/document.xml" && change.external));
    assert!(inspection.losses.iter().any(|loss| loss.element.as_deref() == Some("w:hyperlink")));
}

/// A relationship id renumbered, pointing at the same target, is no change.
#[test]
fn ignores_a_renumbered_relationship() {
    let body = |id: &str| para(&format!(r#"<w:hyperlink r:id="{id}">{}</w:hyperlink>"#, text("the terms")));
    let base = Docx::new(&body("rId5")).rel(("rId5", rt::HYPERLINK, "https://a.example/terms", true)).zip();
    let edited = Docx::new(&body("rId12")).rel(("rId12", rt::HYPERLINK, "https://a.example/terms", true)).zip();
    assert_eq!(inspect(&base, &edited).unwrap(), Inspection::default());
}

/// a part's content type switched, without a byte of it changing.
#[test]
fn reports_a_content_type_switched() {
    let base = Docx::new(&para(&text("Rent."))).part("word/styles.xml", &format!("<w:styles {W}/>")).zip();
    let edited = Docx::new(&para(&text("Rent."))).part("word/styles.xml", &format!("<w:styles {W}/>")).typed("/word/styles.xml", "text/plain").zip();
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(
        inspection.content_types,
        vec![ContentTypeChange { part: "word/styles.xml".into(), base: Some("application/xml".into()), edited: Some("text/plain".into()) }]
    );
}

/// bold added without a tracked formatting change.
#[test]
fn reports_untracked_bold() {
    let base = package(&para(&text("Rent is due.")), &[]);
    let edited = package(&para(r#"<w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">Rent is due.</w:t></w:r>"#), &[]);
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(inspection.untracked_changes, 12);
    assert!(inspection.new_revisions.is_empty());
}

/// paragraph and section properties changed without tracking.
#[test]
fn reports_untracked_paragraph_and_section_formatting() {
    let base = package(&format!(r#"{}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>"#, para(&text("Rent."))), &[]);
    let centred = package(
        &format!(r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr>{}</w:p><w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>"#, text("Rent.")),
        &[],
    );
    // The mark's own properties, and each character they govern (and the marks before it).
    assert!(inspect(&base, &centred).unwrap().untracked_changes >= 6);
    let landscape = package(&format!(r#"{}<w:sectPr><w:pgSz w:w="16838" w:h="11906"/></w:sectPr>"#, para(&text("Rent."))), &[]);
    // The section's own properties, and everything in the section.
    assert!(inspect(&base, &landscape).unwrap().untracked_changes >= 7);
}

/// numbering and styles parts are compared, not ignored.
#[test]
fn reports_numbering_and_styles_changed() {
    let numbering = |format: &str| {
        format!(
            r#"<w:numbering {W}><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="{format}"/></w:lvl></w:abstractNum></w:numbering>"#
        )
    };
    let base = package(&para(&text("Rent.")), &[("word/numbering.xml", &numbering("decimal"))]);
    let edited = package(&para(&text("Rent.")), &[("word/numbering.xml", &numbering("lowerLetter"))]);
    assert_eq!(
        inspect(&base, &edited).unwrap().losses,
        vec![Loss { part: "word/numbering.xml".into(), kind: LossKind::PartAltered, element: None, count: 1 }]
    );
}

/// a base comment's text altered.
#[test]
fn reports_a_comment_altered() {
    let body = para(&anchored("1", "Rent"));
    let base = with_comments(&body, &[("1", "Ana Setia", "Check the amount.", "00000001")], &[("00000001", None, false)]);
    let edited = with_comments(&body, &[("1", "Ana Setia", "The amount is fine.", "00000001")], &[("00000001", None, false)]);
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(inspection.comments, CommentChanges { added: vec![], replies: vec![], removed: 0, altered: 1 });
    assert!(inspection.changed_relationships.is_empty() && inspection.added_parts.is_empty() && inspection.losses.is_empty());
}

/// a base comment re-attributed, marked done, removed, or its anchor moved.
#[test]
fn reports_each_way_a_base_comment_changes() {
    let comment = |author| [("1", author, "Check the amount.", "00000001")];
    let base = with_comments(&para(&format!("{}{}", anchored("1", "Rent"), text(" is due."))), &comment("Ana Setia"), &[("00000001", None, false)]);
    let altered = |edited: &[u8]| inspect(&base, edited).unwrap().comments;
    let re_attributed =
        with_comments(&para(&format!("{}{}", anchored("1", "Rent"), text(" is due."))), &comment("Sari Wulan"), &[("00000001", None, false)]);
    assert_eq!(altered(&re_attributed).altered, 1);
    let done = with_comments(&para(&format!("{}{}", anchored("1", "Rent"), text(" is due."))), &comment("Ana Setia"), &[("00000001", None, true)]);
    assert_eq!(altered(&done).altered, 1);
    let moved = with_comments(&para(&format!("{}{}", text("Rent "), anchored("1", "is due."))), &comment("Ana Setia"), &[("00000001", None, false)]);
    let moved_report = inspect(&base, &moved).unwrap();
    assert_eq!(moved_report.comments.altered, 1);
    let removed = Docx::new(&para(&text("Rent is due."))).zip();
    let report = inspect(&base, &removed).unwrap();
    assert_eq!((report.comments.removed, report.comments.altered), (1, 0));
    // The comments parts themselves are reported as comments, not as parts or relationships.
    assert!(report.losses.is_empty() && report.changed_relationships.is_empty(), "{report:#?}");
}

/// comments and replies added, per author; the first comment's parts are its own business.
#[test]
fn reports_comments_and_replies_added_per_author() {
    let base = Docx::new(&para(&text("Rent is due."))).zip();
    let edited = with_comments(
        &para(&format!("{}{}", anchored("1", "Rent"), text(" is due."))),
        &[("1", "Sari Wulan", "Which rent?", "00000001"), ("2", "Sari Wulan", "The base rent.", "00000002"), ("3", "Budi", "Agreed.", "00000003")],
        &[("00000001", None, false), ("00000002", Some("00000001"), false), ("00000003", Some("00000001"), false)],
    );
    let inspection = inspect(&base, &edited).unwrap();
    assert!(clean(&inspection), "{inspection:#?}");
    assert_eq!(inspection.comments.added, vec![AuthorCount { author: "Sari Wulan".into(), count: 1 }]);
    assert_eq!(
        inspection.comments.replies,
        vec![AuthorCount { author: "Budi".into(), count: 1 }, AuthorCount { author: "Sari Wulan".into(), count: 1 }]
    );
}

/// an unknown element in a run (here equation text) is compared whole, never skipped.
#[test]
fn reports_a_change_inside_an_unknown_element() {
    let math = |value: &str| {
        para(&format!(
            r#"{}<m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:r><m:t>{value}</m:t></m:r></m:oMath>"#,
            text("Interest: ")
        ))
    };
    let base = package(&math("5%"), &[]);
    let edited = package(&math("50%"), &[]);
    let inspection = inspect(&base, &edited).unwrap();
    assert!(!clean(&inspection), "{inspection:#?}");
    let in_run = |value: &str| para(&format!(r#"<w:r><w:customThing w:val="{value}">kept</w:customThing></w:r>"#));
    let inspection = inspect(&package(&in_run("1"), &[]), &package(&in_run("2"), &[])).unwrap();
    assert!(!clean(&inspection), "{inspection:#?}");
}

/// the bound on changes, and the work budget, stop the comparison and say so.
#[test]
fn stops_at_the_bound_on_changes_and_on_work() {
    let words = |prefix: &str| (0..3_000).map(|n| format!("{prefix}{n} ")).collect::<String>();
    let base = package(&para(&text(&words("a"))), &[]);
    let edited = package(&para(&text(&words("b"))), &[]);
    assert!(inspect(&base, &edited).unwrap().too_many_changes);
    let mut work = budget::Budget::new(100, u64::MAX);
    let long: Vec<u32> = (0..1_000).collect();
    assert!(diff::script(long.len(), long.len(), &|i, j| long.get(i) == long.get(j), 10, &mut work).is_none());
}

/// a part that does not parse is the edited file's fault; so is one the base has broken.
#[test]
fn refuses_a_part_that_fails_to_parse() {
    let base = package(&para(&text("Rent.")), &[("word/styles.xml", &format!("<w:styles {W}/>"))]);
    for broken in [format!("<w:styles {W}><w:style>"), format!("<w:styles {W}></w:style>"), format!("<w:styles {W}/><extra/>")] {
        let edited = package(&para(&text("Rent.")), &[("word/styles.xml", &broken)]);
        assert!(matches!(inspect(&base, &edited), Err(InspectError::Edited(_))), "{broken}");
        assert!(matches!(inspect(&edited, &base), Err(InspectError::Base(_))), "{broken}");
    }
    let broken_story = zip_entries(&[
        ("[Content_Types].xml".to_string(), CONTENT_TYPES.as_bytes().to_vec()),
        ("_rels/.rels".to_string(), ROOT_RELS.as_bytes().to_vec()),
        ("word/document.xml".to_string(), format!("<w:document {W}><w:body><w:p></w:body></w:document>").into_bytes()),
    ])
    .unwrap();
    assert!(matches!(inspect(&base, &broken_story), Err(InspectError::Edited(_))));
}

// ---- Story rules -----------------------------------------------------------------------------

/// A tracked formatting change by the editor is theirs; WordCraft writes none, so this one is
/// built by hand.
#[test]
fn reports_a_tracked_format_change_by_its_author() {
    let base = package(&para(&text("Rent is due.")), &[]);
    let edited = package(
        &para(&format!(
            r#"<w:r><w:rPr><w:b/><w:rPrChange w:id="3" w:author="{}" w:date="{}"><w:rPr/></w:rPrChange></w:rPr><w:t xml:space="preserve">Rent</w:t></w:r>{}"#,
            SARI.0,
            SARI.1,
            text(" is due.")
        )),
        &[],
    );
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(counts(&inspection), vec![(SARI.0.into(), 0, 0, 1)]);
    assert!(clean(&inspection), "{inspection:#?}");
}

/// A tracked formatting change that does not say what was there before is no cover.
#[test]
fn reports_a_tracked_format_change_whose_old_formatting_is_wrong() {
    let base = package(&para(r#"<w:r><w:rPr><w:i/></w:rPr><w:t>Rent</w:t></w:r>"#), &[]);
    let edited = package(
        &para(&format!(
            r#"<w:r><w:rPr><w:b/><w:rPrChange w:author="{}" w:date="{}"><w:rPr/></w:rPrChange></w:rPr><w:t>Rent</w:t></w:r>"#,
            SARI.0, SARI.1
        )),
        &[],
    );
    assert_eq!(inspect(&base, &edited).unwrap().untracked_changes, 4);
}

/// A base formatting change accepted keeps the new formatting; rejected restores the old.
#[test]
fn reports_a_base_format_change_accepted_or_rejected() {
    let run = |props: &str| para(&format!(r#"<w:r><w:rPr>{props}</w:rPr><w:t>Rent</w:t></w:r>"#));
    let base = package(&run(&format!(r#"<w:b/><w:rPrChange w:author="{}" w:date="{}"><w:rPr/></w:rPrChange>"#, ANA.0, ANA.1)), &[]);
    assert_eq!(inspect(&base, &package(&run("<w:b/>"), &[])).unwrap().resolved, Resolved { accepted: 1, rejected: 0 });
    assert_eq!(inspect(&base, &package(&run(""), &[])).unwrap().resolved, Resolved { accepted: 0, rejected: 1 });
    assert_eq!(inspect(&base, &package(&run("<w:i/>"), &[])).unwrap().untracked_changes, 4);
}

/// A tracked paragraph and a tracked table row by the editor are theirs.
#[test]
fn reports_a_tracked_paragraph_and_row_inserted() {
    let mark = |(author, date): (&str, &str)| format!(r#"<w:pPr><w:rPr><w:ins w:id="5" w:author="{author}" w:date="{date}"/></w:rPr></w:pPr>"#);
    let base = package(&para(&text("Rent.")), &[]);
    let edited = package(&format!("<w:p>{}{}</w:p>{}", mark(SARI), ins(SARI, "New clause."), para(&text("Rent."))), &[]);
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(counts(&inspection), vec![(SARI.0.into(), 2, 0, 0)]);
    assert!(clean(&inspection), "{inspection:#?}");

    let cell = |content: &str| format!("<w:tc><w:p>{content}</w:p></w:tc>");
    let table = |rows: &str| {
        format!(
            r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>{rows}</w:tbl>{}"#,
            para("")
        )
    };
    let base = package(&table(&format!("<w:tr>{}</w:tr>", cell(&text("Rent")))), &[]);
    let new_row = format!(
        r#"<w:tr><w:trPr><w:ins w:id="6" w:author="{}" w:date="{}"/></w:trPr><w:tc><w:p>{}{}</w:p></w:tc></w:tr>"#,
        SARI.0,
        SARI.1,
        mark(SARI),
        ins(SARI, "Deposit")
    );
    let edited = package(&table(&format!("<w:tr>{}</w:tr>{new_row}", cell(&text("Rent")))), &[]);
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(counts(&inspection), vec![(SARI.0.into(), 3, 0, 0)]);
    assert!(clean(&inspection), "{inspection:#?}");
}

/// A paragraph split without tracking is an untracked change.
#[test]
fn reports_an_untracked_paragraph_split() {
    let base = package(&para(&text("Rent is due.")), &[]);
    let edited = package(&format!("{}{}", para(&text("Rent ")), para(&text("is due."))), &[]);
    assert_eq!(inspect(&base, &edited).unwrap().untracked_changes, 1);
}

/// A tracked change reusing a base change's author and date is not new: it alters that change.
#[test]
fn reports_a_change_dressed_as_a_base_change() {
    let base = package(&para(&format!("{}{}", text("Rent"), ins(ANA, " is due"))), &[]);
    let edited = package(&para(&format!("{}{}{}", text("Rent"), ins(ANA, " is due"), ins(ANA, " never"))), &[]);
    let inspection = inspect(&base, &edited).unwrap();
    assert!(inspection.new_revisions.is_empty());
    assert_eq!(inspection.altered_revisions, 6);
    // Moving a base change's wrapper onto other text alters it too.
    let moved = package(&para(&format!("{}{}", ins(ANA, "Rent"), text(" is due"))), &[]);
    assert!(inspect(&base, &moved).unwrap().altered_revisions > 0);
}

/// Deleted text outside a deletion is hidden by Word; it is never read as the same text.
#[test]
fn reports_deleted_text_outside_a_deletion() {
    let base = package(&para(&text("Rent is not due.")), &[]);
    let edited = package(&para(&format!(r#"{}<w:r><w:delText xml:space="preserve">not </w:delText></w:r>{}"#, text("Rent is "), text("due."))), &[]);
    assert!(!clean(&inspect(&base, &edited).unwrap()));
    // And plain text inside a deletion, the other way round.
    let edited = package(
        &para(&format!(
            r#"{}<w:del w:author="{}" w:date="{}"><w:r><w:t xml:space="preserve">not </w:t></w:r></w:del>{}"#,
            text("Rent is "),
            SARI.0,
            SARI.1,
            text("due.")
        )),
        &[],
    );
    assert!(!clean(&inspect(&base, &edited).unwrap()));
}

/// A tracked change around something other than runs is refused rather than guessed at; one
/// around whole paragraphs is no tracked change to Word, so it is compared whole.
#[test]
fn refuses_or_compares_whole_a_tracked_change_around_other_content() {
    let base = package(&para(&text("Rent.")), &[]);
    let around_link = package(
        &para(&format!(
            r#"{}<w:ins w:author="{}" w:date="{}"><w:hyperlink w:anchor="x">{}</w:hyperlink></w:ins>"#,
            text("Rent."),
            SARI.0,
            SARI.1,
            text(" More.")
        )),
        &[],
    );
    assert!(matches!(inspect(&base, &around_link), Err(InspectError::Edited(_))));
    let around_paragraph =
        package(&format!(r#"{}<w:ins w:author="{}" w:date="{}">{}</w:ins>"#, para(&text("Rent.")), SARI.0, SARI.1, para(&text("Pay nothing."))), &[]);
    assert!(inspect(&base, &around_paragraph).unwrap().untracked_changes > 0);
}

/// A line break turned into a page break, and `xml:space` dropped, are changes.
#[test]
fn reports_changes_a_character_stream_would_not_show() {
    let base = package(&para(r#"<w:r><w:t>Rent</w:t><w:br/><w:t xml:space="preserve"> due</w:t></w:r>"#), &[]);
    let page = package(&para(r#"<w:r><w:t>Rent</w:t><w:br w:type="page"/><w:t xml:space="preserve"> due</w:t></w:r>"#), &[]);
    assert!(!clean(&inspect(&base, &page).unwrap()));
    let collapsed = package(&para(r#"<w:r><w:t>Rent</w:t><w:br/><w:t> due</w:t></w:r>"#), &[]);
    assert!(!clean(&inspect(&base, &collapsed).unwrap()));
    let instruction = package(&para(r#"<w:r><w:t>Rent</w:t><w:br/><w:instrText xml:space="preserve"> due</w:instrText></w:r>"#), &[]);
    assert!(!clean(&inspect(&base, &instruction).unwrap()));
}

/// Footnote references keep their ids: swapping two notes' ids swaps their text.
#[test]
fn reports_footnote_references_swapped() {
    let notes = format!(
        r#"<w:footnotes {W}><w:footnote w:id="1"><w:p>{}</w:p></w:footnote><w:footnote w:id="2"><w:p>{}</w:p></w:footnote></w:footnotes>"#,
        text("Payable in cash."),
        text("Payable never.")
    );
    let body = |first: &str, second: &str| {
        para(&format!(
            r#"{}<w:r><w:footnoteReference w:id="{first}"/></w:r>{}<w:r><w:footnoteReference w:id="{second}"/></w:r>"#,
            text("Rent"),
            text(" and deposit")
        ))
    };
    let make = |first, second| {
        Docx::new(&body(first, second))
            .rel(("rId4", rt::FOOTNOTES, "footnotes.xml", false))
            .part("word/footnotes.xml", &notes)
            .typed("/word/footnotes.xml", FOOTNOTES_TYPE)
            .zip()
    };
    assert!(!clean(&inspect(&make("1", "2"), &make("2", "1")).unwrap()));
}

/// A bookmark end reads as its bookmark's name: rebinding it moves the bookmark.
#[test]
fn reports_a_bookmark_end_rebound() {
    let body = |end_of_price: &str| {
        para(&format!(
            r#"<w:bookmarkStart w:id="0" w:name="Price"/>{}<w:bookmarkStart w:id="1" w:name="Term"/>{}<w:bookmarkEnd w:id="{end_of_price}"/>{}<w:bookmarkEnd w:id="{}"/>"#,
            text("Rent "),
            text("monthly"),
            text(" for a year"),
            if end_of_price == "0" { "1" } else { "0" }
        ))
    };
    assert!(!clean(&inspect(&package(&body("0"), &[]), &package(&body("1"), &[])).unwrap()));
    // Renumbering both ids alike is no change.
    let renumbered = body("0").replace(r#"w:id="0""#, r#"w:id="7""#).replace(r#"w:id="1""#, r#"w:id="8""#);
    assert_eq!(inspect(&package(&body("0"), &[]), &package(&renumbered, &[])).unwrap(), Inspection::default());
}

/// Headers are stories, known by their content type.
#[test]
fn reports_an_untracked_change_in_a_header() {
    let header = |value: &str| format!(r#"<w:hdr {W}>{}</w:hdr>"#, para(&text(value)));
    let make = |value: &str| {
        Docx::new(&format!(r#"{}<w:sectPr><w:headerReference w:type="default" r:id="rId3"/></w:sectPr>"#, para(&text("Rent."))))
            .rel(("rId3", rt::HEADER, "header1.xml", false))
            .part("word/header1.xml", &header(value))
            .typed("/word/header1.xml", HEADER_TYPE)
            .zip()
    };
    // "Draft" to "Final" keeps the "a": four characters gone, four new.
    assert_eq!(inspect(&make("Draft"), &make("Final")).unwrap().untracked_changes, 8);
}

/// Text in a drawing's text box is inside an opaque element: compared whole.
#[test]
fn reports_a_change_in_a_text_box() {
    let shape = |value: &str| {
        para(&format!(
            r#"<w:r><w:drawing><wp:anchor xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><wp:docPr id="1" name="Box"/><w:txbxContent>{}</w:txbxContent></wp:anchor></w:drawing></w:r>"#,
            para(&text(value))
        ))
    };
    assert!(!clean(&inspect(&package(&shape("Fee 5%"), &[]), &package(&shape("Fee 50%"), &[])).unwrap()));
}

/// A picture pointed at another image part is a change, though no image part changed.
#[test]
fn reports_a_picture_retargeted() {
    let picture = r#"<w:r><w:drawing><a:blip xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" r:embed="rId6"/></w:drawing></w:r>"#;
    let make = |target| {
        Docx::new(&para(picture))
            .rel(("rId6", rt::IMAGE, target, false))
            .bytes("word/media/image1.png", b"one")
            .bytes("word/media/image2.png", b"two")
            .default_type("png", "image/png")
            .zip()
    };
    let inspection = inspect(&make("media/image1.png"), &make("media/image2.png")).unwrap();
    assert_eq!(inspection.changed_relationships.len(), 2);
    assert!(!inspection.losses.is_empty());
    // And an image part's bytes changed in place.
    let swapped = Docx::new(&para(picture))
        .rel(("rId6", rt::IMAGE, "media/image1.png", false))
        .bytes("word/media/image1.png", b"ONE")
        .bytes("word/media/image2.png", b"two")
        .default_type("png", "image/png")
        .zip();
    assert_eq!(
        inspect(&make("media/image1.png"), &swapped).unwrap().losses,
        vec![Loss { part: "word/media/image1.png".into(), kind: LossKind::PartAltered, element: None, count: 1 }]
    );
}

/// `mc:Choice Requires` names namespaces by prefix: rebinding the prefix changes which branch
/// Word shows, so it is a change.
#[test]
fn reports_a_markup_compatibility_prefix_rebound() {
    let choice = |uri: &str| {
        para(&format!(
            r#"<w:r><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wps="{uri}"><mc:Choice Requires="wps"><w:t>Shape</w:t></mc:Choice><mc:Fallback><w:t>Picture</w:t></mc:Fallback></mc:AlternateContent></w:r>"#
        ))
    };
    let base = package(&choice("http://schemas.microsoft.com/office/word/2010/wordprocessingShape"), &[]);
    let edited = package(&choice("urn:not-understood"), &[]);
    assert!(!clean(&inspect(&base, &edited).unwrap()));
}

/// Settings and document properties: protection, the hyperlink base and the title are content;
/// the last author and modified date are not.
#[test]
fn compares_settings_and_properties_by_what_matters() {
    let settings = |extra: &str| format!(r#"<w:settings {W}><w:zoom w:percent="100"/>{extra}</w:settings>"#);
    let base = package(&para(&text("Rent.")), &[("word/settings.xml", &settings(""))]);
    let protected =
        package(&para(&text("Rent.")), &[("word/settings.xml", &settings(r#"<w:documentProtection w:edit="readOnly" w:enforcement="1"/>"#))]);
    assert_eq!(inspect(&base, &protected).unwrap().losses.len(), 1);

    let app = |base_uri: &str| format!("<Properties><Application>WordCraft</Application><HyperlinkBase>{base_uri}</HyperlinkBase></Properties>");
    let report = inspect(
        &package(&para(&text("Rent.")), &[("docProps/app.xml", &app("https://a.example/"))]),
        &package(&para(&text("Rent.")), &[("docProps/app.xml", &app("https://b.example/"))]),
    )
    .unwrap();
    assert_eq!(report.losses.len(), 1);

    let core = |title: &str, author: &str| {
        format!(
            r#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{title}</dc:title><cp:lastModifiedBy>{author}</cp:lastModifiedBy></cp:coreProperties>"#
        )
    };
    let base = package(&para(&text("Rent.")), &[("docProps/core.xml", &core("Lease", "Ana"))]);
    assert_eq!(inspect(&base, &package(&para(&text("Rent.")), &[("docProps/core.xml", &core("Lease", "Sari"))])).unwrap(), Inspection::default());
    assert_eq!(inspect(&base, &package(&para(&text("Rent.")), &[("docProps/core.xml", &core("Gift", "Ana"))])).unwrap().losses.len(), 1);
}

/// A relationship removed from the base is reported, as is one added.
#[test]
fn reports_a_relationship_removed() {
    let base =
        Docx::new(&para(&text("Rent."))).rel(("rId9", rt::CUSTOM, "../customXml/item1.xml", false)).part("customXml/item1.xml", "<data/>").zip();
    let edited = Docx::new(&para(&text("Rent."))).part("customXml/item1.xml", "<data/>").zip();
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(inspection.changed_relationships.len(), 1);
    assert_eq!(inspection.changed_relationships[0].change, Change::Removed);
}

// ---- The package and its XML are read strictly ------------------------------------------

fn patched(bytes: &[u8], from: &[u8], to: &[u8], occurrences: usize) -> Vec<u8> {
    assert_eq!(from.len(), to.len());
    let mut out = bytes.to_vec();
    let mut done = 0;
    let mut at = 0;
    while done < occurrences {
        let Some(found) = out[at..].windows(from.len()).position(|window| window == from) else { break };
        let start = at + found;
        out[start..start + to.len()].copy_from_slice(to);
        at = start + to.len();
        done += 1;
    }
    assert_eq!(done, occurrences, "pattern not found");
    out
}

#[test]
fn refuses_a_zip_another_reader_could_read_otherwise() {
    let base = package(&para(&text("Rent.")), &[]);
    let two = package(&para(&text("Rent.")), &[("word/aaaa.xml", "<a/>"), ("word/bbbb.xml", "<b/>")]);
    // Two entries of one name: the zip crate keeps the last, another reader the first.
    let duplicate = patched(&two, b"word/bbbb.xml", b"word/aaaa.xml", 2);
    assert!(matches!(inspect(&base, &duplicate), Err(InspectError::Edited(_))));
    // A local header naming another entry than the directory does.
    let local = patched(&two, b"word/bbbb.xml", b"word/cccc.xml", 1);
    assert!(matches!(inspect(&base, &local), Err(InspectError::Edited(_))));
    // Two names equal but for case.
    let cased = package(&para(&text("Rent.")), &[("word/Styles.xml", "<a/>"), ("word/styles.xml", "<a/>")]);
    assert!(matches!(inspect(&base, &cased), Err(InspectError::Edited(_))));
    // Bytes before the archive, and an archive comment.
    let mut prefixed = b"junk".to_vec();
    prefixed.extend_from_slice(&base);
    assert!(matches!(inspect(&prefixed, &base), Err(InspectError::Base(_))));
    let mut commented = base.clone();
    let length = commented.len();
    commented[length - 2..].copy_from_slice(&3u16.to_le_bytes());
    commented.extend_from_slice(b"hey");
    assert!(matches!(inspect(&base, &commented), Err(InspectError::Edited(_))));
}

#[test]
fn refuses_a_damaged_entry() {
    let stored = {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, text) in [
            ("[Content_Types].xml", CONTENT_TYPES.to_string()),
            ("_rels/.rels", ROOT_RELS.to_string()),
            ("word/document.xml", document(&para(&text("Rent.")))),
        ] {
            zip.start_file(name, options).unwrap();
            std::io::Write::write_all(&mut zip, text.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    };
    assert!(inspect(&stored, &stored).is_ok());
    // One byte of the document changed: its CRC no longer matches.
    let damaged = patched(&stored, b"Rent.", b"Rant.", 1);
    assert!(matches!(inspect(&stored, &damaged), Err(InspectError::Edited(_))));
}

#[test]
fn refuses_xml_another_reader_could_read_otherwise() {
    let base = package(&para(&text("Rent.")), &[]);
    let bodies = [
        r#"<w:p><w:r w:val="a" w:val="b"><w:t>Rent.</w:t></w:r></w:p>"#.to_string(),
        r#"<w:p><x:r><w:t>Rent.</w:t></x:r></w:p>"#.to_string(),
        r#"<w:p><w:r><w:t>Rent&nbsp;.</w:t></w:r></w:p>"#.to_string(),
        r#"<w:p><w:r><w:t>Rent&#0;.</w:t></w:r></w:p>"#.to_string(),
        r#"<w:p><w:r><w:t>Rent.</w:r></w:t></w:p>"#.to_string(),
        "<w:p><w:r><w:t>Rent\u{1}.</w:t></w:r></w:p>".to_string(),
    ];
    for body in bodies {
        assert!(matches!(inspect(&base, &package(&body, &[])), Err(InspectError::Edited(_))), "{body}");
    }
    let with_doctype = zip_entries(&[
        ("[Content_Types].xml".to_string(), CONTENT_TYPES.as_bytes().to_vec()),
        ("_rels/.rels".to_string(), ROOT_RELS.as_bytes().to_vec()),
        (
            "word/document.xml".to_string(),
            format!(r#"<!DOCTYPE x [<!ENTITY e "never">]><w:document {W}><w:body>{}</w:body></w:document>"#, para(&text("Rent."))).into_bytes(),
        ),
    ])
    .unwrap();
    assert!(matches!(inspect(&base, &with_doctype), Err(InspectError::Edited(_))));
    let latin = zip_entries(&[
        ("[Content_Types].xml".to_string(), CONTENT_TYPES.as_bytes().to_vec()),
        ("_rels/.rels".to_string(), ROOT_RELS.as_bytes().to_vec()),
        (
            "word/document.xml".to_string(),
            format!(r#"<?xml version="1.0" encoding="ISO-8859-1"?><w:document {W}><w:body>{}</w:body></w:document>"#, para(&text("Rent.")))
                .into_bytes(),
        ),
    ])
    .unwrap();
    assert!(matches!(inspect(&base, &latin), Err(InspectError::Edited(_))));
}

/// XML's own normalisation applies before comparing: line ends in text are one, a literal line end
/// in an attribute is a space while `&#10;` is a line end.
#[test]
fn compares_xml_as_xml_reads_it() {
    let field = |instruction: &str| package(&para(&format!(r#"<w:fldSimple w:instr="{instruction}">{}</w:fldSimple>"#, text("1"))), &[]);
    assert!(!clean(&inspect(&field("PAGE&#10;X"), &field("PAGE\nX")).unwrap()));
    assert_eq!(inspect(&field("PAGE\nX"), &field("PAGE X")).unwrap(), Inspection::default());
    let crlf = package(&para("<w:r><w:t xml:space=\"preserve\">a\r\nb</w:t></w:r>"), &[]);
    let lf = package(&para("<w:r><w:t xml:space=\"preserve\">a\nb</w:t></w:r>"), &[]);
    assert_eq!(inspect(&crlf, &lf).unwrap(), Inspection::default());
    let reference = package(&para("<w:r><w:t xml:space=\"preserve\">a&#13;b</w:t></w:r>"), &[]);
    assert!(!clean(&inspect(&reference, &lf).unwrap()));
}

#[test]
fn refuses_nesting_deeper_than_the_limit() {
    let base = package(&para(&text("Rent.")), &[]);
    let deep = format!("{}{}{}", "<w:sdt><w:sdtContent>".repeat(40), para(&text("Rent.")), "</w:sdtContent></w:sdt>".repeat(40));
    assert!(matches!(inspect(&base, &package(&deep, &[])), Err(InspectError::Edited(_))));
}

#[test]
fn refuses_a_comment_state_for_no_comment() {
    let body = para(&anchored("1", "Rent"));
    let base = with_comments(&body, &[("1", "Ana Setia", "Check.", "00000001")], &[("00000001", None, false)]);
    let orphan = with_comments(&body, &[("1", "Ana Setia", "Check.", "00000001")], &[("00000001", None, false), ("0000000F", None, true)]);
    assert!(matches!(inspect(&base, &orphan), Err(InspectError::Edited(_))));
}

/// `xml:space` matters only where white space could be read otherwise.
#[test]
fn ignores_xml_space_where_it_changes_nothing() {
    let base = package(&para("<w:r><w:t>Rent is due.</w:t></w:r>"), &[]);
    let edited = package(&para(r#"<w:r><w:t xml:space="preserve">Rent is due.</w:t></w:r>"#), &[]);
    assert_eq!(inspect(&base, &edited).unwrap(), Inspection::default());
}

/// Roles come from exact relationship types: a look-alike type is no comments relationship, so its
/// part is an added part and its relationship a change.
#[test]
fn reads_roles_from_exact_relationship_types() {
    let base = Docx::new(&para(&text("Rent."))).zip();
    let edited = Docx::new(&para(&text("Rent.")))
        .rel(("rId7", "http://example.com/relationships/comments", "comments.xml", false))
        .part("word/comments.xml", &comments_xml(&[("1", "Sari Wulan", "Hidden.", "00000001")]))
        .typed("/word/comments.xml", opc::COMMENTS_TYPE)
        .zip();
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(inspection.comments, CommentChanges::default());
    assert_eq!(inspection.added_parts.len(), 1);
    assert_eq!(inspection.changed_relationships.len(), 1);
}

// ---- Never crash, stay bounded -----------------------------------------------------------------

/// A small deterministic generator, so a failing case can be replayed.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

fn rich_body() -> String {
    format!(
        r#"{}<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:p>{}</w:p></w:tc></w:tr></w:tbl>{}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>"#,
        para(&format!(
            r#"<w:bookmarkStart w:id="0" w:name="Price"/>{}{}<w:bookmarkEnd w:id="0"/>{}"#,
            text("Rent "),
            ins(ANA, "is due "),
            del(ANA, "monthly")
        )),
        text("Cell"),
        para(&anchored("1", "Deposit")),
    )
}

/// Bytes flipped anywhere in a package, and characters changed anywhere in its XML: every case
/// answers, as facts or as an error, and never panics.
#[test]
fn never_panics_on_mangled_packages_or_xml() {
    let base = with_comments(&rich_body(), &[("1", "Ana Setia", "Check.", "00000001")], &[("00000001", None, false)]);
    let document_xml = document(&rich_body());
    assert_eq!(inspect(&base, &base).unwrap(), Inspection::default());
    let mut random = Lcg(0x5EED);
    for _ in 0..600 {
        let mut bytes = base.clone();
        for _ in 0..1 + random.below(4) {
            let at = random.below(bytes.len());
            bytes[at] = random.next() as u8;
        }
        let _ = inspect(&base, &bytes);
        let _ = inspect(&bytes, &base);
    }
    let alphabet: Vec<char> = "<>/=\"' &;:#xwpr0129abtXYZ\u{0}\u{FFFD}é".chars().collect();
    for _ in 0..600 {
        let mut chars: Vec<char> = document_xml.chars().collect();
        for _ in 0..1 + random.below(3) {
            let at = random.below(chars.len());
            match random.below(3) {
                0 => chars[at] = alphabet[random.below(alphabet.len())],
                1 => {
                    chars.remove(at);
                }
                _ => chars.insert(at, alphabet[random.below(alphabet.len())]),
            }
        }
        let mangled: String = chars.into_iter().collect();
        let edited = zip_entries(&[
            ("[Content_Types].xml".to_string(), CONTENT_TYPES.as_bytes().to_vec()),
            ("_rels/.rels".to_string(), ROOT_RELS.as_bytes().to_vec()),
            ("word/document.xml".to_string(), mangled.into_bytes()),
        ])
        .unwrap();
        let _ = inspect(&base, &edited);
        let _ = inspect(&edited, &base);
    }
}

/// A story longer than an inspection reads is unreadable, naming which file.
#[test]
fn refuses_more_text_than_it_reads() {
    let base = package(&para(&text("Rent.")), &[]);
    let long = package(&para(&text(&"a".repeat(MAX_STORY_ITEMS + 1))), &[]);
    assert!(matches!(inspect(&base, &long), Err(InspectError::Edited(message)) if message.contains("more text")));
}

// ---- Content the save inserts changes how base content reads --------------------------------

fn field_char(kind: &str) -> String {
    format!(r#"<w:r><w:fldChar w:fldCharType="{kind}"/></w:r>"#)
}

fn instruction(code: &str) -> String {
    format!(r#"<w:r><w:instrText xml:space="preserve">{code}</w:instrText></w:r>"#)
}

fn inserted((author, date): (&str, &str), runs: &str) -> String {
    format!(r#"<w:ins w:id="4" w:author="{author}" w:date="{date}">{runs}</w:ins>"#)
}

/// tracked field characters turn base text into a field's result, whose code reads
/// otherwise. Without its context, every fact is clean.
#[test]
fn reports_base_text_made_a_field_result_by_tracked_field_characters() {
    let base = package(&para(&text("Rent is due monthly.")), &[]);
    let opening = format!("{}{}{}", field_char("begin"), instruction(" QUOTE weekly "), field_char("separate"));
    let edited = package(
        &para(&format!("{}{}{}{}{}", text("Rent is due "), inserted(SARI, &opening), text("monthly"), inserted(SARI, &field_char("end")), text("."))),
        &[],
    );
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(inspection.untracked_changes, 7, "{inspection:#?}");
    assert_eq!(inspection.new_revisions[0].field_codes, 17);
}

/// tracked field characters turn base text into field code, which no reader shows.
#[test]
fn reports_base_text_made_field_code_by_tracked_field_characters() {
    let base = package(&para(&text("Rent is due monthly.")), &[]);
    let closing = format!("{}{}", field_char("separate"), field_char("end"));
    let edited = package(
        &para(&format!(
            "{}{}{}{}{}",
            text("Rent is due "),
            inserted(SARI, &field_char("begin")),
            text("monthly"),
            inserted(SARI, &closing),
            text(".")
        )),
        &[],
    );
    assert_eq!(inspect(&base, &edited).unwrap().untracked_changes, 7);
}

/// A whole field the save inserts, beside base text, changes nothing of the base; the field is
/// reported as what the insertion holds.
#[test]
fn accepts_a_whole_tracked_field_and_reports_it() {
    let base = package(&para(&text("Rent is due monthly.")), &[]);
    let field = format!("{}{}{}{}{}", field_char("begin"), instruction(" PAGE "), field_char("separate"), text("1"), field_char("end"));
    let edited = package(&para(&format!("{}{}", text("Rent is due monthly."), inserted(SARI, &field))), &[]);
    let inspection = inspect(&base, &edited).unwrap();
    assert!(clean(&inspection), "{inspection:#?}");
    assert_eq!(inspection.new_revisions[0].field_codes, 9);
}

/// a tracked paragraph mark gives base text new paragraph properties (here an
/// indent that puts it off the page) without a tracked formatting change.
#[test]
fn reports_base_text_governed_by_a_new_mark_s_properties() {
    let mark = |properties: &str| format!(r#"<w:pPr>{properties}<w:rPr><w:ins w:id="5" w:author="{}" w:date="{}"/></w:rPr></w:pPr>"#, SARI.0, SARI.1);
    let base = package(&para(&text("Rent is due.")), &[]);
    let indented = package(&format!(r#"<w:p>{}{}</w:p><w:p/>"#, mark(r#"<w:ind w:left="-20000"/>"#), text("Rent is due.")), &[]);
    // The twelve characters, and the document and body starts before the first paragraph.
    assert_eq!(inspect(&base, &indented).unwrap().untracked_changes, 14);
    // the new mark ends a section, so the base text gets its own section.
    let section =
        package(&format!(r#"<w:p>{}{}</w:p><w:p/>"#, mark(r#"<w:sectPr><w:pgSz w:w="16838" w:h="11906"/></w:sectPr>"#), text("Rent is due.")), &[]);
    assert!(inspect(&base, &section).unwrap().untracked_changes >= 12);
    // Splitting with the base's own properties, or tracking the change of them, is no change.
    let split = package(&format!(r#"<w:p>{}{}</w:p><w:p/>"#, mark(""), text("Rent is due.")), &[]);
    assert!(clean(&inspect(&base, &split).unwrap()));
    let tracked = format!(r#"<w:ind w:left="720"/><w:pPrChange w:id="6" w:author="{}" w:date="{}"><w:pPr/></w:pPrChange>"#, SARI.0, SARI.1);
    let formatted = package(&format!(r#"<w:p>{}{}</w:p><w:p/>"#, mark(&tracked), text("Rent is due.")), &[]);
    let inspection = inspect(&base, &formatted).unwrap();
    assert!(clean(&inspection), "{inspection:#?}");
    assert_eq!(counts(&inspection), vec![(SARI.0.into(), 1, 0, 1)]);
}

/// a new comment's range start holding a run. Only an empty anchor is dropped with
/// its comment; this one's content is story content, here untracked.
#[test]
fn reports_content_inside_a_new_comment_s_anchor() {
    let base = Docx::new(&para(&text("Rent is monthly."))).zip();
    let body = para(&format!(
        r#"<w:commentRangeStart w:id="1">{}</w:commentRangeStart>{}<w:commentRangeEnd w:id="1"/><w:r><w:commentReference w:id="1"/></w:r>"#,
        text("Rent is waived."),
        text("Rent is monthly.")
    ));
    let edited = with_comments(&body, &[("1", "Sari Wulan", "Note.", "00000001")], &[("00000001", None, false)]);
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(inspection.comments.added.len(), 1);
    assert!(inspection.untracked_changes > 0, "{inspection:#?}");
    // A range marker holding content inside a tracked change is refused.
    let wrapped = para(&format!(
        r#"{}<w:ins w:author="{}" w:date="{}"><w:bookmarkStart w:id="2" w:name="x">{}</w:bookmarkStart></w:ins>"#,
        text("Rent is monthly."),
        SARI.0,
        SARI.1,
        text("Waived.")
    ));
    assert!(matches!(inspect(&base, &package(&wrapped, &[])), Err(InspectError::Edited(_))));
}

/// a removed comment's anchor that holds content is content, not part of the removal.
#[test]
fn reports_content_left_in_a_removed_comment_s_anchor() {
    let base = with_comments(&para(&anchored("1", "Rent is monthly.")), &[("1", "Ana Setia", "Check.", "00000001")], &[("00000001", None, false)]);
    let body = para(&format!(
        r#"<w:commentRangeStart w:id="1">{}</w:commentRangeStart>{}<w:commentRangeEnd w:id="1"/>"#,
        text("Rent is waived."),
        text("Rent is monthly.")
    ));
    let inspection = inspect(&base, &package(&body, &[])).unwrap();
    assert_eq!(inspection.comments.removed, 1);
    assert!(inspection.untracked_changes > 0, "{inspection:#?}");
}

// ---- Bounds ------------------------------------------------------------------------------------

/// a part of braces kept no copy of itself, escaped or not: what is held stays near the
/// part's own size, and well under a budget a printed and escaped copy would pass.
#[test]
fn holds_little_more_than_a_part_however_it_would_print() {
    let braces = format!("<data>{}</data>", "{".repeat(8 << 20));
    let base = package(&para(&text("Rent.")), &[("customXml/item1.xml", &braces)]);
    let mut budget = budget::Budget::new(u64::MAX, 48 << 20);
    assert!(inspect_budgeted(&base, &base, &INSPECT_LIMITS, MAX_CHANGES, &mut budget).is_ok());
    assert!(budget.peak() < 40 << 20, "held {}", budget.peak());
    // An opaque story element of braces likewise.
    let element = para(&format!(r#"<w:r><w:customThing>{}</w:customThing></w:r>"#, "{".repeat(8 << 20)));
    let story = package(&element, &[]);
    let mut budget = budget::Budget::new(u64::MAX, 48 << 20);
    assert!(inspect_budgeted(&story, &story, &INSPECT_LIMITS, MAX_CHANGES, &mut budget).is_ok());
    assert!(budget.peak() < 40 << 20, "held {}", budget.peak());
}

/// past the memory budget, the inspection is refused before it allocates past it.
#[test]
fn refuses_past_the_memory_budget() {
    let braces = format!("<data>{}</data>", "{".repeat(8 << 20));
    let base = package(&para(&text("Rent.")), &[("customXml/item1.xml", &braces)]);
    let mut budget = budget::Budget::new(u64::MAX, 4 << 20);
    assert!(
        matches!(inspect_budgeted(&base, &base, &INSPECT_LIMITS, MAX_CHANGES, &mut budget), Err(InspectError::Base(message)) if message.contains("memory"))
    );
    assert_eq!(memory_limit(100_000, 100_000), MEMORY_FLOOR);
    assert_eq!(memory_limit(400 << 20, 400 << 20), MAX_MEMORY);
}

/// reading and walking spend the work budget, so a package of many elements stops
/// on it rather than on the clock.
#[test]
fn refuses_reading_past_the_work_budget() {
    let many: String =
        (0..2_000).map(|n| para(&inserted(("Ana Setia", &format!("2026-01-01T00:{:02}:{:02}Z", n / 60 % 60, n % 60)), &text("x")))).collect();
    let base = package(&many, &[]);
    let mut budget = budget::Budget::new(5_000, u64::MAX);
    assert!(
        matches!(inspect_budgeted(&base, &base, &INSPECT_LIMITS, MAX_CHANGES, &mut budget), Err(InspectError::Base(message)) if message.contains("work"))
    );
}

// ---- The zip layout --------------------------------------------------------------------------

/// The central directory record of the first entry: where it starts and its length.
fn first_record(zip: &[u8]) -> (usize, usize) {
    let end = zip.len() - 22;
    let start = u32::from_le_bytes(zip[end + 16..end + 20].try_into().unwrap()) as usize;
    let field = |offset: usize| u16::from_le_bytes(zip[start + offset..start + offset + 2].try_into().unwrap()) as usize;
    (start, 46 + field(28) + field(30) + field(32))
}

/// a local header that tells another method than the directory.
#[test]
fn refuses_a_local_header_that_disagrees_with_the_directory() {
    let base = package(&para(&text("Rent.")), &[]);
    let mut edited = base.clone();
    assert_eq!(&edited[0..4], b"PK\x03\x04");
    edited[8] = 0; // the first entry claims to be stored
    assert!(matches!(inspect(&base, &edited), Err(InspectError::Edited(_))));
}

/// an entry the directory does not list, before the listed ones.
#[test]
fn refuses_an_entry_the_directory_does_not_list() {
    let base = package(&para(&text("Rent.")), &[]);
    let hidden = zip_entries(&[
        ("hidden.xml".to_string(), document(&para(&text("Rent is waived."))).into_bytes()),
        ("[Content_Types].xml".to_string(), CONTENT_TYPES.as_bytes().to_vec()),
        ("_rels/.rels".to_string(), ROOT_RELS.as_bytes().to_vec()),
        ("word/document.xml".to_string(), document(&para(&text("Rent."))).into_bytes()),
    ])
    .unwrap();
    // Drop the first entry's directory record; its bytes stay at the start of the file.
    let (start, length) = first_record(&hidden);
    let mut edited = [&hidden[..start], &hidden[start + length..]].concat();
    let end = edited.len() - 22;
    for offset in [8, 10] {
        let entries = u16::from_le_bytes(edited[end + offset..end + offset + 2].try_into().unwrap()) - 1;
        edited[end + offset..end + offset + 2].copy_from_slice(&entries.to_le_bytes());
    }
    let size = u32::from_le_bytes(edited[end + 12..end + 16].try_into().unwrap()) - length as u32;
    edited[end + 12..end + 16].copy_from_slice(&size.to_le_bytes());
    assert!(matches!(inspect(&base, &edited), Err(InspectError::Edited(message)) if message.contains("bytes")));
}

/// bytes between the directory and its end record.
#[test]
fn refuses_bytes_between_the_directory_and_its_end() {
    let base = package(&para(&text("Rent.")), &[]);
    let end = base.len() - 22;
    let edited = [&base[..end], b"junk", &base[end..]].concat();
    assert!(matches!(inspect(&base, &edited), Err(InspectError::Edited(_))));
}

// ---- Facts about the save's own changes -----------------------------------------------------

/// dates, undated changes, and what tracked insertions hold.
#[test]
fn reports_what_the_save_s_changes_hold() {
    let base = package(&para(&text("Rent.")), &[]);
    let hidden = r#"<w:r><w:rPr><w:vanish/></w:rPr><w:t>secret</w:t></w:r>"#;
    let body = para(&format!(
        r#"{}{}{}{}<w:ins w:author="{}">{}</w:ins>"#,
        text("Rent."),
        inserted((SARI.0, "2001-01-01T00:00:00Z"), hidden),
        inserted(SARI, &text("\u{202E}due")),
        inserted(
            SARI,
            r#"<w:r><w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"/></w:drawing></w:r>"#
        ),
        SARI.0,
        text(" more")
    ));
    let inspection = inspect(&base, &package(&body, &[])).unwrap();
    let sari = &inspection.new_revisions[0];
    assert_eq!((sari.earliest_date.as_deref(), sari.latest_date.as_deref(), sari.undated), (Some("2001-01-01T00:00:00Z"), Some(SARI.1), 1));
    assert_eq!((sari.hidden_text, sari.bidi_controls, sari.objects, sari.field_codes), (6, 1, 1, 0));
    // A tracked formatting change that hides base text.
    let hide = package(
        &para(&format!(
            r#"<w:r><w:rPr><w:vanish/><w:rPrChange w:author="{}" w:date="{}"><w:rPr/></w:rPrChange></w:rPr><w:t>Rent.</w:t></w:r>"#,
            SARI.0, SARI.1
        )),
        &[],
    );
    assert_eq!(inspect(&base, &hide).unwrap().new_revisions[0].hidden_text, 5);
}

/// a tracked move that arrives as other text than it left is an untracked change.
#[test]
fn reports_a_move_whose_text_differs() {
    let base = package(&format!("{}{}", para(&text("A pays 100.")), para(&text("Terms."))), &[]);
    let moved = |arrives: &str| {
        package(
            &format!(
                r#"<w:p><w:moveFrom w:author="{0}" w:date="{1}"><w:r><w:delText>A pays 100.</w:delText></w:r></w:moveFrom></w:p><w:p>{2}<w:moveTo w:author="{0}" w:date="{1}">{3}</w:moveTo></w:p>"#,
                SARI.0,
                SARI.1,
                text("Terms."),
                text(arrives)
            ),
            &[],
        )
    };
    assert_eq!(inspect(&base, &moved("A pays 100.")).unwrap().untracked_changes, 0);
    assert_eq!(inspect(&base, &moved("A pays 900.")).unwrap().untracked_changes, 1);
}

/// parsing itself is charged, element by element, not only the bytes read and hashed.
#[test]
fn parsing_spends_the_work_budget() {
    let many = format!("<data>{}</data>", "<a/>".repeat(20_000));
    let base = package(&para(&text("Rent.")), &[("customXml/item1.xml", &many)]);
    // Reading and hashing both packages costs about 21,000 units; parsing 40,000 elements more.
    let mut budget = budget::Budget::new(25_000, u64::MAX);
    assert!(
        matches!(inspect_budgeted(&base, &base, &INSPECT_LIMITS, MAX_CHANGES, &mut budget), Err(InspectError::Base(message)) if message.contains("work"))
    );
}

// ---- Tracked property changes, base fields, hidden marks, opaque content in insertions --------

/// A section break added to a base paragraph mark under a section change whose old properties are
/// empty, or hold only a revision id: Word shows no revision and a new section. Whether a mark ends a
/// section is part of what it is.
#[test]
fn reports_a_section_break_added_under_an_empty_section_change() {
    let base = package(&format!("{}{}", para(&text("Clause one.")), para(&text("Clause two."))), &[]);
    let added = |old: &str| {
        package(
            &format!(
                r#"<w:p><w:pPr><w:sectPr><w:pgSz w:w="16838" w:h="11906"/><w:sectPrChange w:id="3" w:author="{}" w:date="{}">{old}</w:sectPrChange></w:sectPr></w:pPr>{}</w:p>{}"#,
                SARI.0,
                SARI.1,
                text("Clause one."),
                para(&text("Clause two."))
            ),
            &[],
        )
    };
    for old in [r#"<w:sectPr/>"#, r#"<w:sectPr w:rsidR="00A1B2C3"/>"#, ""] {
        let inspection = inspect(&base, &added(old)).unwrap();
        assert!(inspection.untracked_changes > 0, "{old}: {inspection:#?}");
    }
}

/// A tracked formatting change accepted only when its recorded old properties are the base's: with
/// no old properties recorded at all, it explains nothing.
#[test]
fn reports_a_formatting_change_that_records_no_old_properties() {
    let base = package(&para(&text("Rent.")), &[]);
    let edited =
        package(&para(&format!(r#"<w:r><w:rPr><w:b/><w:rPrChange w:author="{}" w:date="{}"/></w:rPr><w:t>Rent.</w:t></w:r>"#, SARI.0, SARI.1)), &[]);
    assert_eq!(inspect(&base, &edited).unwrap().untracked_changes, 5);
}

fn quoted_field(code: &str, result: &str) -> String {
    format!("{}{}{}{}{}", field_char("begin"), instruction(code), field_char("separate"), text(result), field_char("end"))
}

fn deleted((author, date): (&str, &str), runs: &str) -> String {
    format!(r#"<w:del w:id="7" w:author="{author}" w:date="{date}">{runs}</w:del>"#)
}

fn deleted_field_char(kind: &str) -> String {
    format!(r#"<w:r><w:fldChar w:fldCharType="{kind}"/></w:r>"#)
}

/// A base field's separator deleted with tracking: once accepted, the result is gone. A base
/// field's code narrowed with tracking: once updated, the field shows other text. Each changes
/// the field otherwise than deleting it whole.
#[test]
fn reports_a_base_field_changed_by_tracked_deletions() {
    let base = package(&para(&format!("{}{}{}", text("Rent is due "), quoted_field(r#" QUOTE "not monthly" "#, "not monthly"), text("."))), &[]);
    let separator_gone = format!(
        "{}{}{}{}{}{}{}",
        text("Rent is due "),
        field_char("begin"),
        instruction(r#" QUOTE "not monthly" "#),
        deleted(SARI, &deleted_field_char("separate")),
        text("not monthly"),
        field_char("end"),
        text(".")
    );
    let inspection = inspect(&base, &package(&para(&separator_gone), &[])).unwrap();
    assert_eq!(inspection.untracked_changes, 1, "{inspection:#?}");
    assert_eq!(inspection.new_revisions[0].field_codes, 1);
    let narrowed = format!(
        r#"{}{}<w:r><w:instrText xml:space="preserve"> QUOTE "</w:instrText></w:r>{}<w:r><w:instrText xml:space="preserve">monthly" </w:instrText></w:r>{}{}{}{}"#,
        text("Rent is due "),
        field_char("begin"),
        deleted(SARI, r#"<w:r><w:delInstrText xml:space="preserve">not </w:delInstrText></w:r>"#),
        field_char("separate"),
        text("not monthly"),
        field_char("end"),
        text(".")
    );
    let inspection = inspect(&base, &package(&para(&narrowed), &[])).unwrap();
    assert_eq!(inspection.untracked_changes, 1, "{inspection:#?}");
    assert_eq!(inspection.new_revisions[0].field_codes, 4);
    // Its start deleted with tracking changes it too.
    let start_gone = format!(
        "{}{}{}",
        text("Rent is due "),
        deleted(SARI, &deleted_field_char("begin")),
        &quoted_field(r#" QUOTE "not monthly" "#, "not monthly").replacen(&field_char("begin"), "", 1)
    );
    assert_eq!(inspect(&base, &package(&para(&format!("{start_gone}{}", text("."))), &[])).unwrap().untracked_changes, 1);
}

/// The whole base field deleted with tracking is a tracked deletion, and nothing else.
#[test]
fn accepts_a_base_field_deleted_whole() {
    let field = |result: &str| quoted_field(r#" QUOTE "not monthly" "#, result);
    let base = package(&para(&format!("{}{}{}", text("Rent is due "), field("not monthly"), text("."))), &[]);
    let whole = format!(
        "{}{}{}",
        text("Rent is due "),
        deleted(
            SARI,
            &format!(
                r#"{}<w:r><w:delInstrText xml:space="preserve"> QUOTE "not monthly" </w:delInstrText></w:r>{}<w:r><w:delText>not monthly</w:delText></w:r>{}"#,
                field_char("begin"),
                field_char("separate"),
                field_char("end")
            )
        ),
        text(".")
    );
    let inspection = inspect(&base, &package(&para(&whole), &[])).unwrap();
    assert!(clean(&inspection), "{inspection:#?}");
    assert_eq!(counts(&inspection), vec![(SARI.0.into(), 0, 1, 0)]);
    assert!(inspection.new_revisions[0].field_codes > 0);
}

/// A hidden paragraph mark, inserted or hidden by a tracked change, is hidden text.
#[test]
fn counts_hidden_paragraph_marks() {
    let base = package(&format!("{}{}", para(&text("One.")), para(&text("Two."))), &[]);
    let hidden_by_change = package(
        &format!(
            r#"<w:p><w:pPr><w:rPr><w:vanish/><w:rPrChange w:author="{}" w:date="{}"><w:rPr/></w:rPrChange></w:rPr></w:pPr>{}</w:p>{}"#,
            SARI.0,
            SARI.1,
            text("One."),
            para(&text("Two."))
        ),
        &[],
    );
    assert_eq!(inspect(&base, &hidden_by_change).unwrap().new_revisions[0].hidden_text, 1);
    let inserted_hidden = package(
        &format!(
            r#"<w:p><w:pPr><w:rPr><w:ins w:author="{}" w:date="{}"/><w:vanish/></w:rPr></w:pPr>{}</w:p>{}"#,
            SARI.0,
            SARI.1,
            text("One."),
            para(&text("Two."))
        ),
        &[],
    );
    let inspection = inspect(&base, &inserted_hidden).unwrap();
    assert_eq!(inspection.new_revisions[0].hidden_text, 1, "{inspection:#?}");
}

/// Opaque content inside an insertion is an object, and what it holds is counted.
#[test]
fn counts_what_opaque_content_in_an_insertion_holds() {
    let base = package(&para(&text("Rent.")), &[]);
    let boxed = format!(
        r#"<w:r><w:pict><w:txbxContent><w:p><w:r><w:rPr><w:vanish/></w:rPr><w:t>hidden</w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:t>{}</w:t></w:r></w:p></w:txbxContent></w:pict></w:r>"#,
        "\u{202E}x"
    );
    let inspection = inspect(&base, &package(&para(&format!("{}{}", text("Rent."), inserted(SARI, &boxed))), &[])).unwrap();
    let sari = &inspection.new_revisions[0];
    assert_eq!((sari.objects, sari.hidden_text, sari.field_codes, sari.bidi_controls), (1, 1, 1, 1));
    let reference = r#"<w:r><w:commentReference w:id="9"><w:t>Rent is waived.</w:t></w:commentReference></w:r>"#;
    let inspection = inspect(&base, &package(&para(&format!("{}{}", text("Rent."), inserted(SARI, reference))), &[])).unwrap();
    assert_eq!(inspection.new_revisions[0].objects, 1);
}

// ---- Names, namespaces and attributes are charged ----------------------------------------------

fn custom(part: String) -> Vec<u8> {
    package(&para(&text("Rent.")), &[("customXml/item1.xml", &part)])
}

/// Distinct long names are kept once each, and charged: a small upload of many of them is refused
/// at the memory budget, never held uncharged.
#[test]
fn charges_the_names_it_keeps() {
    let elements: String = (0..2_000).map(|n| format!("<p:{}{n}/>", "n".repeat(4_000))).collect();
    let names = custom(format!(r#"<data xmlns:p="urn:x">{elements}</data>"#));
    // The part's 8 MB of bytes are held while it is parsed; its 8 MB of names on top pass 12 MB.
    let mut budget = budget::Budget::new(u64::MAX, 12 << 20);
    assert!(
        matches!(inspect_budgeted(&names, &names, &INSPECT_LIMITS, MAX_CHANGES, &mut budget), Err(InspectError::Base(message)) if message.contains("memory"))
    );
}

/// A long namespace is looked up for every element in it, and that is work: a short element
/// repeated under one cannot run past the work budget's time.
#[test]
fn charges_namespace_lookups_as_work() {
    let flood = custom(format!(r#"<data xmlns:p="{}">{}</data>"#, "{".repeat(4_096), "<p:a/>".repeat(20_000)));
    let mut budget = budget::Budget::new(5_000_000, u64::MAX);
    assert!(
        matches!(inspect_budgeted(&flood, &flood, &INSPECT_LIMITS, MAX_CHANGES, &mut budget), Err(InspectError::Base(message)) if message.contains("work"))
    );
}

/// An element with more attributes than any real one is refused.
#[test]
fn refuses_an_element_with_too_many_attributes() {
    let attributes: String = (0..600).map(|n| format!(r#" p:a{n}="1""#)).collect();
    let many = custom(format!(r#"<data xmlns:p="urn:x"><e{attributes}/></data>"#));
    let base = package(&para(&text("Rent.")), &[]);
    assert!(matches!(inspect(&base, &many), Err(InspectError::Edited(message)) if message.contains("attributes")));
}

// ---- Hidden through styles; qualified lists ------------------------------------------------------

fn styled(styles: &str, body: &str) -> Vec<u8> {
    package(body, &[("word/styles.xml", &format!("<w:styles {W}>{styles}</w:styles>"))])
}

const HIDDEN_STYLES: &str = r#"<w:style w:type="character" w:styleId="Secret"><w:rPr><w:vanish/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Hide"><w:rPr><w:vanish/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Gone"><w:basedOn w:val="Hide"/></w:style>"#;

/// Text hidden through a style counts as hidden text: a character style applied by a tracked
/// formatting change, a paragraph style (through the style it is based on) applied by one, and an
/// insertion in a hidden character style.
#[test]
fn counts_text_hidden_through_a_style() {
    let base = styled(HIDDEN_STYLES, &para(&text("Rent.")));
    let character = styled(
        HIDDEN_STYLES,
        &para(&format!(
            r#"<w:r><w:rPr><w:rStyle w:val="Secret"/><w:rPrChange w:author="{}" w:date="{}"><w:rPr/></w:rPrChange></w:rPr><w:t>Rent.</w:t></w:r>"#,
            SARI.0, SARI.1
        )),
    );
    assert_eq!(inspect(&base, &character).unwrap().new_revisions[0].hidden_text, 5);
    let paragraph = styled(
        HIDDEN_STYLES,
        &format!(
            r#"<w:p><w:pPr><w:pStyle w:val="Gone"/><w:pPrChange w:author="{}" w:date="{}"><w:pPr/></w:pPrChange></w:pPr>{}</w:p>"#,
            SARI.0,
            SARI.1,
            text("Rent.")
        ),
    );
    let inspection = inspect(&base, &paragraph).unwrap();
    // The five characters and the paragraph mark.
    assert_eq!(inspection.new_revisions[0].hidden_text, 6, "{inspection:#?}");
    let inserted_secret = styled(
        HIDDEN_STYLES,
        &para(&format!("{}{}", text("Rent."), inserted(SARI, r#"<w:r><w:rPr><w:rStyle w:val="Secret"/></w:rPr><w:t>more</w:t></w:r>"#))),
    );
    assert_eq!(inspect(&base, &inserted_secret).unwrap().new_revisions[0].hidden_text, 4);
    // Hidden by the document's defaults too.
    let defaults = r#"<w:docDefaults><w:rPrDefault><w:rPr><w:vanish/></w:rPr></w:rPrDefault></w:docDefaults>"#;
    let shown = styled(defaults, &para(&text("Rent.")));
    let inserted_default = styled(defaults, &para(&format!("{}{}", text("Rent."), inserted(SARI, &text("more")))));
    assert_eq!(inspect(&shown, &inserted_default).unwrap().new_revisions[0].hidden_text, 4);
}

/// What the inspector cannot settle about styles counts as hidden: a `basedOn` loop, and a style
/// that shows (`vanish` off) over a paragraph style that hides, which Word's toggle rules can still
/// hide.
#[test]
fn counts_text_hidden_when_its_styles_are_uncertain() {
    let styles = r#"<w:style w:type="character" w:styleId="A"><w:basedOn w:val="B"/></w:style><w:style w:type="character" w:styleId="B"><w:basedOn w:val="A"/></w:style><w:style w:type="character" w:styleId="Shown"><w:rPr><w:vanish w:val="0"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Hide"><w:rPr><w:vanish/></w:rPr></w:style>"#;
    let base = styled(styles, &para(&text("Rent.")));
    let looped =
        styled(styles, &para(&format!("{}{}", text("Rent."), inserted(SARI, r#"<w:r><w:rPr><w:rStyle w:val="A"/></w:rPr><w:t>more</w:t></w:r>"#))));
    assert_eq!(inspect(&base, &looped).unwrap().new_revisions[0].hidden_text, 4);
    let hidden_paragraph = |run: &str| styled(styles, &format!(r#"<w:p><w:pPr><w:pStyle w:val="Hide"/></w:pPr>{}{run}</w:p>"#, text("Rent.")));
    let shown = hidden_paragraph(&inserted(SARI, r#"<w:r><w:rPr><w:rStyle w:val="Shown"/></w:rPr><w:t>more</w:t></w:r>"#));
    assert_eq!(inspect(&hidden_paragraph(""), &shown).unwrap().new_revisions[0].hidden_text, 4);
    // Direct formatting decides: shown outright is shown.
    let direct = hidden_paragraph(&inserted(SARI, r#"<w:r><w:rPr><w:vanish w:val="0"/></w:rPr><w:t>more</w:t></w:r>"#));
    assert_eq!(inspect(&hidden_paragraph(""), &direct).unwrap().new_revisions[0].hidden_text, 0);
}

fn table(properties: &str, cell: &str) -> String {
    format!(
        r#"<w:tbl><w:tblPr>{properties}</w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:p>{cell}</w:p></w:tc></w:tr></w:tbl>"#
    )
}

const TABLE_STYLES: &str = r#"<w:style w:type="table" w:styleId="Plain"/><w:style w:type="table" w:styleId="HideTable"><w:rPr><w:vanish/></w:rPr></w:style><w:style w:type="table" w:styleId="Banded"><w:tblStylePr w:type="wholeTable"><w:rPr><w:vanish/></w:rPr></w:tblStylePr></w:style>"#;

/// The default paragraph style hides a paragraph that names no style: an insertion there is hidden.
#[test]
fn counts_text_hidden_through_the_default_paragraph_style() {
    let default_paragraph = r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:rPr><w:vanish/></w:rPr></w:style>"#;
    let base = styled(default_paragraph, &para(&text("Rent.")));
    let edited = styled(default_paragraph, &para(&format!("{}{}", text("Rent."), ins(SARI, "more"))));
    assert_eq!(inspect(&base, &edited).unwrap().new_revisions[0].hidden_text, 4);
}

/// A table style's run properties hide every cell's text.
#[test]
fn counts_text_hidden_through_a_table_style() {
    let base = styled(TABLE_STYLES, &table(r#"<w:tblStyle w:val="HideTable"/>"#, &text("Rent.")));
    let edited = styled(TABLE_STYLES, &table(r#"<w:tblStyle w:val="HideTable"/>"#, &format!("{}{}", text("Rent."), ins(SARI, "more"))));
    assert_eq!(inspect(&base, &edited).unwrap().new_revisions[0].hidden_text, 4);
}

/// A table that names no style takes the default table style, which can hide.
#[test]
fn counts_text_hidden_through_the_default_table_style() {
    let default_table = r#"<w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:rPr><w:vanish/></w:rPr></w:style>"#;
    let base = styled(default_table, &table("", &text("Rent.")));
    let edited = styled(default_table, &table("", &format!("{}{}", text("Rent."), ins(SARI, "more"))));
    assert_eq!(inspect(&base, &edited).unwrap().new_revisions[0].hidden_text, 4);
}

/// A tracked table property change switching a table to a style that hides its base text: Word
/// lists no revision for it, so it is an untracked change.
#[test]
fn reports_a_table_switched_to_a_hiding_style_by_a_tracked_change() {
    let base = styled(TABLE_STYLES, &table(r#"<w:tblStyle w:val="Plain"/>"#, &text("Rent.")));
    let switched = format!(
        r#"<w:tblStyle w:val="HideTable"/><w:tblPrChange w:id="1" w:author="{}" w:date="{}"><w:tblPr><w:tblStyle w:val="Plain"/></w:tblPr></w:tblPrChange>"#,
        SARI.0, SARI.1
    );
    let edited = styled(TABLE_STYLES, &table(&switched, &text("Rent.")));
    let inspection = inspect(&base, &edited).unwrap();
    assert!(inspection.untracked_changes > 0, "{inspection:#?}");
}

/// Any `vanish` a run can reach counts, whatever Word's toggles would make of it: a conditional
/// table format, the default character style.
#[test]
fn counts_text_any_reachable_vanish_hides() {
    let base = styled(TABLE_STYLES, &table(r#"<w:tblStyle w:val="Banded"/>"#, &text("Rent.")));
    let edited = styled(TABLE_STYLES, &table(r#"<w:tblStyle w:val="Banded"/>"#, &format!("{}{}", text("Rent."), ins(SARI, "more"))));
    assert_eq!(inspect(&base, &edited).unwrap().new_revisions[0].hidden_text, 4);
    let default_character = r#"<w:style w:type="character" w:default="1" w:styleId="DefaultFont"><w:rPr><w:vanish/></w:rPr></w:style>"#;
    let base = styled(default_character, &para(&text("Rent.")));
    let edited = styled(default_character, &para(&format!("{}{}", text("Rent."), ins(SARI, "more"))));
    assert_eq!(inspect(&base, &edited).unwrap().new_revisions[0].hidden_text, 4);
    // A style showing (`vanish` off) over one that hides still counts: the hiding one is reachable.
    let chain = r#"<w:style w:type="character" w:styleId="C1"><w:rPr><w:vanish/></w:rPr></w:style><w:style w:type="character" w:styleId="C2"><w:basedOn w:val="C1"/><w:rPr><w:vanish w:val="0"/></w:rPr></w:style>"#;
    let base = styled(chain, &para(&text("Rent.")));
    let edited =
        styled(chain, &para(&format!("{}{}", text("Rent."), inserted(SARI, r#"<w:r><w:rPr><w:rStyle w:val="C2"/></w:rPr><w:t>more</w:t></w:r>"#))));
    assert_eq!(inspect(&base, &edited).unwrap().new_revisions[0].hidden_text, 4);
}

/// Word lists no revision for a tracked change of a table's, row's or cell's properties, so the
/// save's own such changes are untracked changes.
#[test]
fn reports_tracked_table_property_changes_as_untracked() {
    let change = |name: &str, old: &str| format!(r#"<w:{name} w:id="1" w:author="{}" w:date="{}">{old}</w:{name}>"#, SARI.0, SARI.1);
    let row = |properties: &str, cell: &str| {
        format!(
            r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr>{properties}<w:tc>{cell}<w:p>{}</w:p></w:tc></w:tr></w:tbl>"#,
            text("Rent.")
        )
    };
    let base = package(&row("", ""), &[]);
    let row_change = package(&row(&format!(r#"<w:trPr><w:cantSplit/>{}</w:trPr>"#, change("trPrChange", "<w:trPr/>")), ""), &[]);
    let inspection = inspect(&base, &row_change).unwrap();
    assert!(inspection.untracked_changes > 0, "{inspection:#?}");
    let cell_change =
        package(&row("", &format!(r#"<w:tcPr><w:shd w:val="clear" w:fill="000000"/>{}</w:tcPr>"#, change("tcPrChange", "<w:tcPr/>"))), &[]);
    let inspection = inspect(&base, &cell_change).unwrap();
    assert!(inspection.untracked_changes > 0, "{inspection:#?}");
    // A paragraph's tracked property change still explains itself.
    let paragraph = package(&format!(r#"<w:p><w:pPr><w:jc w:val="center"/>{}</w:pPr>{}</w:p>"#, change("pPrChange", "<w:pPr/>"), text("Rent.")), &[]);
    let inspection = inspect(&package(&para(&text("Rent.")), &[]), &paragraph).unwrap();
    assert_eq!(inspection.untracked_changes, 0, "{inspection:#?}");
}

/// Inflating is charged at its measured cost (five units of about 10 ns for every 8 bytes), before
/// it runs: a binary part is inflated once a side, and hashed as it is. So inflating alone cannot
/// run past the work budget's time: three parts of 64 MiB a side are refused for work, not read.
#[test]
fn charges_inflation_at_its_cost() {
    let part = vec![0u8; 8 << 20];
    let package_with = |last: u8| {
        let mut bytes = part.clone();
        if let Some(byte) = bytes.last_mut() {
            *byte = last;
        }
        let mut entries = vec![
            ("[Content_Types].xml".to_string(), CONTENT_TYPES.as_bytes().to_vec()),
            ("_rels/.rels".to_string(), ROOT_RELS.as_bytes().to_vec()),
            ("word/document.xml".to_string(), document(&para(&text("Rent."))).into_bytes()),
        ];
        entries.push(("parts/zeros.bin".to_string(), bytes));
        zip_entries(&entries).unwrap()
    };
    let (base, edited) = (package_with(0), package_with(1));
    let (result, _, work) = inspect_measured(&base, &edited);
    assert_eq!(result.unwrap().losses.len(), 1);
    let inflated = 2 * (budget::inflated(8 << 20) + budget::hashed(8 << 20));
    assert!(work >= inflated, "work {work}, inflating alone {inflated}");
    // At the size cap, inflating alone passes the work budget: refused before it runs.
    let mut entries = vec![
        ("[Content_Types].xml".to_string(), CONTENT_TYPES.as_bytes().to_vec()),
        ("_rels/.rels".to_string(), ROOT_RELS.as_bytes().to_vec()),
        ("word/document.xml".to_string(), document(&para(&text("Rent."))).into_bytes()),
    ];
    let big = vec![0u8; (64 << 20) - 4_096];
    entries.extend((0..3).map(|n| (format!("parts/zeros{n}.bin"), big.clone())));
    let capped = zip_entries(&entries).unwrap();
    let result = inspect(&capped, &capped);
    assert!(matches!(&result, Err(InspectError::Base(message) | InspectError::Edited(message)) if message.contains("work")), "{result:?}");
}

/// Prefix lists (`mc:Ignorable` and the like) are kept as namespace numbers, charged as they are
/// expanded: thousands of them under a long namespace stay small. Lists past 1,024 names, or past
/// 64 KiB in one element, are refused.
#[test]
fn keeps_prefix_lists_small_and_bounded() {
    let namespace = "{".repeat(4_096);
    let element = format!(
        r#"<e mc:Ignorable="{0}" mc:ProcessContent="{1}" mc:MustUnderstand="{0}" xsi:type="p:a"/>"#,
        vec!["p"; 15].join(" "),
        vec!["p:a"; 15].join(" ")
    );
    let part = |count: usize| {
        custom(format!(
            r#"<data xmlns:p="{namespace}" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">{}</data>"#,
            element.repeat(count)
        ))
    };
    // The reviewer's 15.6 KB upload held 2.1 GB against a 12 MB charge: each name's namespace is
    // now looked up and charged as work, so it stops at the work budget, holding little.
    let lists = part(8_000);
    let mut budget = budget::Budget::new(MAX_WORK, 64 << 20);
    let result = inspect_budgeted(&lists, &lists, &INSPECT_LIMITS, MAX_CHANGES, &mut budget);
    assert!(matches!(&result, Err(InspectError::Base(message) | InspectError::Edited(message)) if message.contains("work")), "{result:?}");
    assert!(budget.peak() < 32 << 20, "held {}", budget.peak());
    // A tenth of it reads, holding a few megabytes.
    let lists = part(800);
    let mut budget = budget::Budget::new(MAX_WORK, 64 << 20);
    let result = inspect_budgeted(&lists, &lists, &INSPECT_LIMITS, MAX_CHANGES, &mut budget);
    assert!(result.is_ok(), "{result:?}");
    assert!(budget.peak() < 32 << 20, "held {}", budget.peak());
    let long = custom(format!(
        r#"<data xmlns:p="urn:x" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><e mc:Ignorable="{}"/></data>"#,
        vec!["p"; 1_025].join(" ")
    ));
    assert!(matches!(inspect(&lists, &long), Err(InspectError::Edited(message)) if message.contains("list")));
    let wide = custom(format!(
        r#"<data xmlns:p="urn:x" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><e mc:Ignorable="{0}" mc:ProcessContent="{0}"/></data>"#,
        vec![format!("p:{}", "a".repeat(80)); 1_000].join(" ")
    ));
    assert!(matches!(inspect(&lists, &wide), Err(InspectError::Edited(message)) if message.contains("list")));
}

// ---- Fix round 5 (Rulings W1-23, W1-24, W1-25) ---------------------------------------------------

/// The CRC-32 a zip entry carries (ISO 3309), for packages built here byte by byte.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// One entry of a package built byte by byte: its name, general-purpose flags, method (0 stored,
/// 8 deflated), its data as stored and the bytes it inflates to.
struct RawEntry {
    name: Vec<u8>,
    flags: u16,
    method: u16,
    data: Vec<u8>,
    inflated: Vec<u8>,
}

impl RawEntry {
    fn stored(name: &str, bytes: &[u8]) -> RawEntry {
        RawEntry { name: name.as_bytes().to_vec(), flags: 0, method: 0, data: bytes.to_vec(), inflated: bytes.to_vec() }
    }
}

/// A zip written field by field, so a test can carry what no zip writer makes.
fn raw_zip(entries: &[RawEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for entry in entries {
        let offset = u32::try_from(out.len()).unwrap();
        let fields = |out: &mut Vec<u8>| {
            out.extend_from_slice(&entry.flags.to_le_bytes());
            out.extend_from_slice(&entry.method.to_le_bytes());
            out.extend_from_slice(&[0, 0, 0x21, 0]);
            out.extend_from_slice(&crc32(&entry.inflated).to_le_bytes());
            out.extend_from_slice(&u32::try_from(entry.data.len()).unwrap().to_le_bytes());
            out.extend_from_slice(&u32::try_from(entry.inflated.len()).unwrap().to_le_bytes());
            out.extend_from_slice(&u16::try_from(entry.name.len()).unwrap().to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
        };
        out.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04, 20, 0]);
        fields(&mut out);
        out.extend_from_slice(&entry.name);
        out.extend_from_slice(&entry.data);
        directory.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02, 20, 0, 20, 0]);
        fields(&mut directory);
        directory.extend_from_slice(&[0; 6]);
        directory.extend_from_slice(&0u32.to_le_bytes());
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(&entry.name);
    }
    let start = u32::try_from(out.len()).unwrap();
    let count = u16::try_from(entries.len()).unwrap();
    out.extend_from_slice(&directory);
    out.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06, 0, 0, 0, 0]);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&u32::try_from(directory.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// A plain package of stored entries, with `extra` entries after its own.
fn raw_package(extra: Vec<RawEntry>) -> Vec<u8> {
    let mut entries = vec![
        RawEntry::stored("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
        RawEntry::stored("_rels/.rels", ROOT_RELS.as_bytes()),
        RawEntry::stored("word/document.xml", document(&para(&text("Rent."))).as_bytes()),
    ];
    entries.extend(extra);
    raw_zip(&entries)
}

/// The package built byte by byte reads as the same package written by the zip writer.
#[test]
fn reads_a_package_built_byte_by_byte() {
    let raw = raw_package(Vec::new());
    assert_eq!(inspect(&raw, &package(&para(&text("Rent.")), &[])).unwrap(), Inspection::default());
}

/// W1-23: what a deflate stream consumes is charged as well as what it gives. A stream of empty
/// Huffman-coded blocks gives nothing and costs its every byte; a stream of empty stored blocks
/// costs each block. Both are charged before they are inflated.
#[test]
fn charges_the_compressed_data_a_stream_consumes() {
    // Empty fixed-Huffman blocks, four in five bytes, then a final one.
    let mut fixed = [0x02, 0x08, 0x20, 0x80, 0x00].repeat(200_000);
    fixed.extend_from_slice(&[0x03, 0x00]);
    let empty = RawEntry { name: b"parts/fixed.bin".to_vec(), flags: 0, method: 8, data: fixed.clone(), inflated: Vec::new() };
    let package = raw_package(vec![empty]);
    let (result, _, work) = inspect_measured(&package, &package);
    assert_eq!(result.unwrap(), Inspection::default());
    let consumed = 2 * budget::consumed(budget::units(fixed.len()));
    assert!(work >= consumed, "work {work}, the compressed data alone {consumed}");
    // Empty stored blocks, then a final one: each block is a step.
    let mut stored = [0x00, 0x00, 0x00, 0xFF, 0xFF].repeat(200_000);
    stored.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
    let empty = RawEntry { name: b"parts/stored.bin".to_vec(), flags: 0, method: 8, data: stored, inflated: Vec::new() };
    let package = raw_package(vec![empty]);
    let (result, _, work) = inspect_measured(&package, &package);
    assert_eq!(result.unwrap(), Inspection::default());
    assert!(work >= 2 * 200_001, "work {work}");
}

/// W1-25: a binary part is inflated once a side and hashed as it streams; the comparison uses the
/// digests. Stored data (a stored entry, or deflate's stored blocks) is charged as a copy.
#[test]
fn inflates_a_binary_part_once_a_side() {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let random: Vec<u8> = (0..(4 << 20))
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect();
    let with = |bytes: &[u8]| {
        let entries = vec![
            ("[Content_Types].xml".to_string(), CONTENT_TYPES.as_bytes().to_vec()),
            ("_rels/.rels".to_string(), ROOT_RELS.as_bytes().to_vec()),
            ("word/document.xml".to_string(), document(&para(&text("Rent."))).into_bytes()),
            ("parts/scan.bin".to_string(), bytes.to_vec()),
        ];
        zip_entries(&entries).unwrap()
    };
    let base = with(&random);
    let (result, _, work) = inspect_measured(&base, &base);
    assert_eq!(result.unwrap(), Inspection::default());
    // Random bytes deflate to stored blocks: copied and hashed once a side.
    let size = budget::units(random.len());
    let once = 2 * (budget::copied(size) + budget::hashed(size));
    assert!(work >= once && work < once + once / 4, "work {work}, inflated and hashed once a side {once}");
    // One byte changed is a part altered, found by its digest.
    let mut changed = random.clone();
    changed[1_000] ^= 1;
    let inspection = inspect(&base, &with(&changed)).unwrap();
    assert_eq!(inspection.losses, vec![Loss { part: "parts/scan.bin".into(), kind: LossKind::PartAltered, element: None, count: 1 }]);
    // A stored entry whose data does not match its CRC is refused, though no step reads it as XML.
    let mut entries = vec![RawEntry::stored("parts/scan.bin", &random[..4_096])];
    entries[0].inflated[0] ^= 1;
    let damaged = raw_package(entries);
    assert!(matches!(inspect(&base, &damaged), Err(InspectError::Edited(message)) if message.contains("parts/scan.bin")));
}

/// W1-23 (H3): the zip reader decodes a name without the UTF-8 flag as CP437, where each byte past
/// ASCII takes up to three, and another reader would take such a name otherwise than its bytes say.
/// So it is refused before the zip reader decodes it; flagged UTF-8 and plain ASCII names read.
#[test]
fn refuses_a_name_the_zip_reader_would_decode_otherwise() {
    let entry = |flags: u16| RawEntry { flags, ..RawEntry::stored("parts/m\u{e9}dia.bin", b"x") };
    assert!(inspect(&raw_package(Vec::new()), &raw_package(vec![entry(0x0800)])).is_ok());
    let unflagged = raw_package(vec![entry(0)]);
    assert!(matches!(inspect(&raw_package(Vec::new()), &unflagged), Err(InspectError::Edited(message)) if message.contains("encoding")));
    // Flagged, but not UTF-8: the reader would spell it with replacement characters.
    let mut bad = entry(0x0800);
    bad.name = b"parts/m\xE9dia.bin".to_vec();
    assert!(matches!(inspect(&raw_package(Vec::new()), &raw_package(vec![bad])), Err(InspectError::Edited(message)) if message.contains("encoding")));
    // An ASCII name needs no flag.
    assert!(inspect(&raw_package(Vec::new()), &raw_package(vec![RawEntry::stored("parts/media.bin", b"x")])).is_ok());
}

/// The work of parsing `xml` on its own.
fn parse_work(xml: &str) -> Result<u64, String> {
    let mut budget = budget::Budget::new(u64::MAX, u64::MAX);
    let mut limits = xml::Limits { elements_left: INSPECT_LIMITS.max_elements, max_depth: INSPECT_LIMITS.max_depth };
    xml::parse(xml.as_bytes(), &mut xml::Names::default(), &mut limits, &mut budget)?;
    Ok(u64::MAX - budget.work_left())
}

/// W1-23 (H4): resolving a name walks the namespace declarations in scope, newest first: each
/// resolution is charged for every declaration it may walk, before it runs, and no more than 1,024
/// declarations may be in scope at once.
#[test]
fn charges_each_namespace_declaration_a_name_walks() {
    // Declarations `from..to`; an element carries at most 512 attributes, so a thousand stand on two.
    let declarations = |from: usize, to: usize| -> String { (from..to).map(|n| format!(r#" xmlns:p{n}="urn:{n}""#)).collect() };
    let thousand = |inner: &str| format!("<r{}><s{}>{inner}</s></r>", declarations(0, 500), declarations(500, 1_000));
    // Unprefixed names with no default namespace walk every declaration.
    let elements = "<e/>".repeat(50_000);
    let plain = parse_work(&format!("<r><s>{elements}</s></r>")).unwrap();
    let declared = parse_work(&thousand(&elements)).unwrap();
    assert!(declared - plain >= 50_000 * 1_000 / 8, "declared {declared}, plain {plain}");
    // So do prefixed attribute names, but not unprefixed ones.
    let attributes = r#"<e p0:a="1" b="2"/>"#.repeat(50_000);
    let walked = parse_work(&thousand(&attributes)).unwrap();
    assert!(walked - declared >= 50_000 * 1_000 / 8, "walked {walked}, declared {declared}");
    // In scope at once: nested declarations add up.
    assert!(parse_work(&thousand(&format!("<e{}/>", declarations(1_000, 1_024)))).is_ok());
    let over = parse_work(&thousand(&format!("<e{}/>", declarations(1_000, 1_025))));
    assert!(matches!(&over, Err(message) if message.contains("namespace declarations")), "{over:?}");
    // Declarations leave scope with their element.
    let siblings = |close: bool| {
        let element = if close { format!("<e{}></e>", declarations(0, 500)) } else { format!("<e{}/>", declarations(0, 500)) };
        format!("<r>{}</r>", element.repeat(3))
    };
    assert!(parse_work(&siblings(false)).is_ok());
    assert!(parse_work(&siblings(true)).is_ok());
}

/// W1-23 (H5): an attribute name is at most 256 bytes, and a name written twice is found through a
/// set, not by comparing every pair of names.
#[test]
fn bounds_attribute_names_and_checks_them_once() {
    let named = |length: usize| format!(r#"<r {}="1"/>"#, "a".repeat(length));
    assert!(parse_work(&named(256)).is_ok());
    let long = parse_work(&named(257));
    assert!(matches!(&long, Err(message) if message.contains("attribute name longer")), "{long:?}");
    let long_declaration = parse_work(&format!(r#"<r xmlns:{}="urn:x"/>"#, "p".repeat(251)));
    assert!(matches!(&long_declaration, Err(message) if message.contains("attribute name longer")), "{long_declaration:?}");
    for twice in [r#"<r a="1" a="2"/>"#, r#"<r xmlns:p="urn:x" xmlns:p="urn:y"/>"#, r#"<r xmlns:p="urn:x" xmlns:q="urn:x" p:a="1" q:a="2"/>"#] {
        assert!(matches!(parse_work(twice), Err(message) if message.contains("twice")), "{twice}");
    }
    // 512 attributes of 256 bytes each, all alike but the last byte: read in linear work.
    let attributes: String = (0..500).map(|n| format!(r#" {}{n:03}="1""#, "k".repeat(253))).collect();
    let work = parse_work(&format!("<r{attributes}/>")).unwrap();
    assert!(work < 200_000, "work {work}");
}

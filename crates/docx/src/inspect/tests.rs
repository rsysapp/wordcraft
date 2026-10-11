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
    assert_eq!(inspection.new_revisions, vec![AuthorRevisions { author: "Sari Wulan".into(), insertions: 1, deletions: 0, format_changes: 0 }]);
    assert_eq!((inspection.untracked_changes, inspection.losses.len()), (0, 0));
}

#[test]
fn reports_a_tracked_deletion_by_its_author() {
    let base = package(&para(&text("Rent is due monthly.")), &[]);
    let edited = package(&para(&format!("{}{}{}", text("Rent is due"), del(SARI, " monthly"), text("."))), &[]);
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(inspection.new_revisions, vec![AuthorRevisions { author: "Sari Wulan".into(), insertions: 0, deletions: 1, format_changes: 0 }]);
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

// A host's server inspects inside a small memory budget (D10): a package over the element budget
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

fn sari() -> Vec<AuthorRevisions> {
    vec![AuthorRevisions { author: SARI.0.into(), insertions: 0, deletions: 0, format_changes: 0 }]
}

// ---- Ruling W1-6: the adversarial packages ----------------------------------------------------

/// W1-2: a macro project added, its relationship and the main part switched to macro-enabled.
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

/// W1-2: an embedded object, an alternative chunk and custom XML, each added, are each a part.
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

/// W1-2: an external hyperlink retargeted is a changed relationship, and the link in the text
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

/// W1-2: a part's content type switched, without a byte of it changing.
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

/// W1-3: bold added without a tracked formatting change.
#[test]
fn reports_untracked_bold() {
    let base = package(&para(&text("Rent is due.")), &[]);
    let edited = package(&para(r#"<w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">Rent is due.</w:t></w:r>"#), &[]);
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(inspection.untracked_changes, 12);
    assert!(inspection.new_revisions.is_empty());
}

/// W1-3: paragraph and section properties changed without tracking.
#[test]
fn reports_untracked_paragraph_and_section_formatting() {
    let base = package(&format!(r#"{}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>"#, para(&text("Rent."))), &[]);
    let centred = package(
        &format!(r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr>{}</w:p><w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>"#, text("Rent.")),
        &[],
    );
    assert_eq!(inspect(&base, &centred).unwrap().untracked_changes, 1);
    let landscape = package(&format!(r#"{}<w:sectPr><w:pgSz w:w="16838" w:h="11906"/></w:sectPr>"#, para(&text("Rent."))), &[]);
    assert_eq!(inspect(&base, &landscape).unwrap().untracked_changes, 1);
}

/// W1-3: numbering and styles parts are compared, not ignored.
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

/// W1-4: a base comment's text altered.
#[test]
fn reports_a_comment_altered() {
    let body = para(&anchored("1", "Rent"));
    let base = with_comments(&body, &[("1", "Ana Setia", "Check the amount.", "00000001")], &[("00000001", None, false)]);
    let edited = with_comments(&body, &[("1", "Ana Setia", "The amount is fine.", "00000001")], &[("00000001", None, false)]);
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(inspection.comments, CommentChanges { added: vec![], replies: vec![], removed: 0, altered: 1 });
    assert!(inspection.changed_relationships.is_empty() && inspection.added_parts.is_empty() && inspection.losses.is_empty());
}

/// W1-4: a base comment re-attributed, marked done, removed, or its anchor moved.
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

/// W1-4: comments and replies added, per author; the first comment's parts are its own business.
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

/// W1-1: an unknown element in a run (here equation text) is compared whole, never skipped.
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

/// W1-5: the bound on changes, and the work budget, stop the comparison and say so.
#[test]
fn stops_at_the_bound_on_changes_and_on_work() {
    let words = |prefix: &str| (0..3_000).map(|n| format!("{prefix}{n} ")).collect::<String>();
    let base = package(&para(&text(&words("a"))), &[]);
    let edited = package(&para(&text(&words("b"))), &[]);
    assert!(inspect(&base, &edited).unwrap().too_many_changes);
    let mut work = diff::Work::new(100);
    let long: Vec<u32> = (0..1_000).collect();
    assert!(diff::script(long.len(), long.len(), &|i, j| long.get(i) == long.get(j), 10, &mut work).is_none());
}

/// W1-1: a part that does not parse is the edited file's fault; so is one the base has broken.
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
    assert_eq!(inspection.new_revisions, vec![AuthorRevisions { format_changes: 1, ..sari()[0].clone() }]);
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
    assert_eq!(inspection.new_revisions, vec![AuthorRevisions { insertions: 2, ..sari()[0].clone() }]);
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
    assert_eq!(inspection.new_revisions, vec![AuthorRevisions { insertions: 3, ..sari()[0].clone() }]);
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

// ---- Ruling W1-1: the package and its XML are read strictly ------------------------------------

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

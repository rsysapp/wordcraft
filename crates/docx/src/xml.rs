//! A small, bounded XML tree on top of `quick-xml`, plus a string-building XML writer.
//!
//! Element and attribute names are rewritten to *canonical prefixes* by namespace URI (`w:p`,
//! `r:id`, `a:blip`…), so the reader doesn't care which prefixes a producer chose. Names in
//! unknown namespaces keep their local name behind a `?:` prefix.

use std::ops::Range;

// embed: the events a shared reading hands its callers.
use quick_xml::events::{BytesCData, BytesRef, BytesStart, BytesText, Event};
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

use crate::DocxError;

/// Maximum element nesting accepted by the parser.
pub const MAX_DEPTH: usize = 256;
/// Maximum number of elements in one part.
pub const MAX_ELEMENTS: usize = 8_000_000;

/// Known namespaces and the canonical prefix we use for them (read and write).
pub const NAMESPACES: &[(&str, &str)] = &[
    ("w", "http://schemas.openxmlformats.org/wordprocessingml/2006/main"),
    ("r", "http://schemas.openxmlformats.org/officeDocument/2006/relationships"),
    ("wp", "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"),
    ("a", "http://schemas.openxmlformats.org/drawingml/2006/main"),
    ("pic", "http://schemas.openxmlformats.org/drawingml/2006/picture"),
    ("mc", "http://schemas.openxmlformats.org/markup-compatibility/2006"),
    ("w14", "http://schemas.microsoft.com/office/word/2010/wordml"),
    ("w15", "http://schemas.microsoft.com/office/word/2012/wordml"),
    ("wp14", "http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing"),
    ("wps", "http://schemas.microsoft.com/office/word/2010/wordprocessingShape"),
    ("wpg", "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup"),
    ("m", "http://schemas.openxmlformats.org/officeDocument/2006/math"),
    ("v", "urn:schemas-microsoft-com:vml"),
    ("o", "urn:schemas-microsoft-com:office:office"),
    ("w10", "urn:schemas-microsoft-com:office:word"),
];

/// Namespaces that only appear in non-body parts.
const OTHER_NAMESPACES: &[(&str, &str)] = &[
    ("rel", "http://schemas.openxmlformats.org/package/2006/relationships"),
    ("ct", "http://schemas.openxmlformats.org/package/2006/content-types"),
    ("cp", "http://schemas.openxmlformats.org/package/2006/metadata/core-properties"),
    ("dc", "http://purl.org/dc/elements/1.1/"),
    ("dcterms", "http://purl.org/dc/terms/"),
    ("xsi", "http://www.w3.org/2001/XMLSchema-instance"),
    ("xml", "http://www.w3.org/XML/1998/namespace"),
    ("ep", "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"),
];

/// ISO/IEC 29500 Strict namespaces map onto the transitional prefixes.
const STRICT: &[(&str, &str)] = &[
    ("w", "http://purl.oclc.org/ooxml/wordprocessingml/main"),
    ("r", "http://purl.oclc.org/ooxml/officeDocument/relationships"),
    ("wp", "http://purl.oclc.org/ooxml/drawingml/wordprocessingDrawing"),
    ("a", "http://purl.oclc.org/ooxml/drawingml/main"),
    ("pic", "http://purl.oclc.org/ooxml/drawingml/picture"),
    ("m", "http://purl.oclc.org/ooxml/officeDocument/math"),
];

// embed: also used by the preserving writer's positional tree.
pub(crate) fn prefix_for(uri: &[u8]) -> &'static str {
    for (p, u) in NAMESPACES.iter().chain(OTHER_NAMESPACES).chain(STRICT) {
        if u.as_bytes() == uri {
            return p;
        }
    }
    "?"
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    El(El),
    Text(String),
}

/// An element with canonical-prefixed name and attributes.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct El {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub kids: Vec<Node>,
}

impl El {
    /// Attribute by canonical name (`w:val`); falls back to an unqualified attribute with the
    /// same local name (some producers omit the prefix).
    pub fn attr(&self, name: &str) -> Option<&str> {
        if let Some((_, v)) = self.attrs.iter().find(|(k, _)| k == name) {
            return Some(v);
        }
        let local = name.rsplit(':').next().unwrap_or(name);
        if local != name {
            return self.attrs.iter().find(|(k, _)| k == local).map(|(_, v)| v.as_str());
        }
        None
    }
    /// Child elements.
    pub fn els(&self) -> impl Iterator<Item = &El> {
        self.kids.iter().filter_map(|n| match n {
            Node::El(e) => Some(e),
            Node::Text(_) => None,
        })
    }
    pub fn child(&self, name: &str) -> Option<&El> {
        self.els().find(|e| e.name == name)
    }
    pub fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a El> + 'a {
        self.els().filter(move |e| e.name == name)
    }
    /// `w:val` of a child.
    pub fn child_val(&self, name: &str) -> Option<&str> {
        self.child(name).and_then(|c| c.attr("w:val"))
    }
    /// Direct text content.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for n in &self.kids {
            if let Node::Text(t) = n {
                s.push_str(t);
            }
        }
        s
    }
    /// All descendant text (depth-bounded by the parser).
    pub fn deep_text(&self) -> String {
        let mut s = String::new();
        fn go(e: &El, s: &mut String, depth: usize) {
            if depth > MAX_DEPTH {
                return;
            }
            for n in &e.kids {
                match n {
                    Node::Text(t) => s.push_str(t),
                    Node::El(c) => go(c, s, depth + 1),
                }
            }
        }
        go(self, &mut s, 0);
        s
    }
    /// First descendant with this name (depth-first, bounded).
    pub fn find(&self, name: &str) -> Option<&El> {
        fn go<'a>(e: &'a El, name: &str, depth: usize) -> Option<&'a El> {
            if depth > MAX_DEPTH {
                return None;
            }
            for c in e.els() {
                if c.name == name {
                    return Some(c);
                }
                if let Some(f) = go(c, name, depth + 1) {
                    return Some(f);
                }
            }
            None
        }
        go(self, name, 0)
    }
    /// Local part of the name.
    pub fn local(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }
}

/// The error `parse_limited` answers when a part holds more elements than it may.
pub const TOO_MANY_ELEMENTS: &str = "too many XML elements";
/// embed: the error `parse_limited` answers when elements nest deeper than it may.
pub const TOO_DEEP: &str = "XML nesting too deep";

/// embed: the most namespace declarations in scope at once; a WordprocessingML part declares a few
/// dozen (39 in the largest of 342 real files checked). Each name is resolved by walking the
/// declarations in scope, so this bounds that walk.
pub const MAX_BINDINGS: usize = 256;
/// embed: the error a parse answers past [`MAX_BINDINGS`].
pub const TOO_MANY_BINDINGS: &str = "too many XML namespace declarations in scope";

/// Parse XML bytes into the root element.
pub fn parse(bytes: &[u8]) -> Result<El, DocxError> {
    parse_limited(bytes, MAX_ELEMENTS, MAX_DEPTH).map(|(root, _)| root)
}

/// embed: [`parse`] with at most `max_elements` elements, nested at most `max_depth` deep (never
/// deeper than [`MAX_DEPTH`]), with at most [`MAX_BINDINGS`] namespace declarations in scope; also
/// answers how many elements it read, so a package can spend one budget across its parts.
pub fn parse_limited(bytes: &[u8], max_elements: usize, max_depth: usize) -> Result<(El, usize), DocxError> {
    let max_depth = max_depth.min(MAX_DEPTH);
    let mut reading = Reading::new(bytes)?;
    let mut count = 0usize;
    // embed: elements are counted, as they always were (a package's budget and its tests count
    // them); declarations are bounded by the cap in scope, attributes by the part's bytes.
    let mut charge = |record: Record| {
        if record != Record::Element {
            return Ok(());
        }
        count += 1;
        if count > max_elements { Err(DocxError::Limit(TOO_MANY_ELEMENTS.into())) } else { Ok(()) }
    };
    let mut stack: Vec<El> = Vec::new();
    let mut root: Option<El> = None;
    loop {
        let (_, token) = reading.next(&mut charge)?;
        match token {
            Token::Start(tag) => {
                let el = El { name: tag.name, attrs: tag.attrs, kids: Vec::new() };
                if !tag.empty {
                    // embed: the caller's depth (at most MAX_DEPTH).
                    if stack.len() >= max_depth {
                        return Err(DocxError::Limit(TOO_DEEP.into()));
                    }
                    stack.push(el);
                } else {
                    attach(&mut stack, &mut root, el);
                }
            }
            Token::End => {
                if let Some(el) = stack.pop() {
                    attach(&mut stack, &mut root, el);
                }
            }
            Token::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    push_text(top, &text(&t)?);
                }
            }
            Token::CData(t) => {
                if let Some(top) = stack.last_mut() {
                    push_text(top, &String::from_utf8_lossy(&t));
                }
            }
            Token::Ref(g) => {
                if let Some(top) = stack.last_mut()
                    && let Some(c) = reference(&g)?
                {
                    let mut utf8 = [0; 4];
                    push_text(top, c.encode_utf8(&mut utf8));
                }
            }
            Token::Eof => break,
            Token::Other => {}
        }
        if root.is_some() && stack.is_empty() {
            break;
        }
    }
    // Unclosed elements at EOF: close them (lenient).
    while let Some(el) = stack.pop() {
        attach(&mut stack, &mut root, el);
    }
    root.map(|root| (root, count)).ok_or_else(|| DocxError::Xml("no root element".into()))
}

// embed: one reading of a part, shared by the desktop parser above and the preserving writer's
// positional tree (`preserve::tree`), so that the two accept the same parts and name every element
// and attribute alike: an element the two named differently would put an edit in the wrong place.

/// What a reading keeps for its caller, charged through the caller's closure before it is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Record {
    Element,
    Attribute,
    /// A namespace declaration, kept by the reader while it is in scope.
    Declaration,
}

/// A start tag, or an element written empty, with its canonical name and attributes.
pub(crate) struct Tag<'a> {
    pub start: BytesStart<'a>,
    /// `prefix:local` with the canonical prefix of the element's namespace (`?` for one not known),
    /// or the local name alone for no namespace.
    pub name: String,
    /// Named as the element is, namespace declarations left out, values unescaped (the raw value
    /// when it cannot be).
    pub attrs: Vec<(String, String)>,
    pub empty: bool,
}

pub(crate) enum Token<'a> {
    Start(Tag<'a>),
    End,
    /// Text between tags: decode it with [`text`] before keeping it.
    Text(BytesText<'a>),
    CData(BytesCData<'a>),
    /// A character or entity reference: resolve it with [`reference`].
    Ref(BytesRef<'a>),
    Eof,
    /// The XML declaration, a comment, a processing instruction or a document type: nothing kept.
    Other,
}

/// A part being read: the reader configured as the desktop parser has always read parts, offsets
/// into the part as given, and the namespace declarations in scope.
pub(crate) struct Reading<'a> {
    reader: NsReader<&'a [u8]>,
    /// The byte-order marks before the reader's first offset.
    skipped: usize,
    /// How many declarations each open element brought into scope, and their sum.
    scopes: Vec<usize>,
    in_scope: usize,
}

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

impl<'a> Reading<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Result<Reading<'a>, DocxError> {
        // Strip a UTF-8 BOM; UTF-16 parts are rare in OOXML and rejected.
        let body = bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes);
        if body.starts_with(&[0xFF, 0xFE]) || body.starts_with(&[0xFE, 0xFF]) {
            return Err(DocxError::Xml("UTF-16 XML parts are not supported".into()));
        }
        // The reader drops a mark at the start of what it is given without counting it in its
        // offsets, whatever was stripped before: counted here, so offsets into the part stay exact. A
        // mark after that one is text before the root, which no one keeps.
        let unseen = if body.starts_with(UTF8_BOM) { UTF8_BOM.len() } else { 0 };
        let mut reader = NsReader::from_reader(body);
        {
            let c = reader.config_mut();
            c.trim_text_start = false;
            c.trim_text_end = false;
            c.expand_empty_elements = false;
            c.check_end_names = false;
        }
        Ok(Reading { reader, skipped: bytes.len().saturating_sub(body.len()).saturating_add(unseen), scopes: Vec::new(), in_scope: 0 })
    }

    /// The reader's position as an offset into the part as given.
    fn offset(&self) -> usize {
        usize::try_from(self.reader.buffer_position()).unwrap_or(usize::MAX).saturating_add(self.skipped)
    }

    /// The next token and where it is in the part. Elements, attributes and namespace declarations
    /// are charged through `charge` before they are kept; text, CDATA and references are the
    /// caller's to charge.
    pub(crate) fn next(&mut self, charge: &mut impl FnMut(Record) -> Result<(), DocxError>) -> Result<(Range<usize>, Token<'a>), DocxError> {
        let before = self.offset();
        let (resolved, event) = self.reader.read_resolved_event().map_err(|e| DocxError::Xml(e.to_string()))?;
        let prefix = match resolved {
            ResolveResult::Bound(ns) => Some(prefix_for(ns.as_ref())),
            _ => None,
        };
        let range = before..self.offset();
        let token = match event {
            Event::Start(start) => Token::Start(self.tag(start, prefix, false, charge)?),
            Event::Empty(start) => Token::Start(self.tag(start, prefix, true, charge)?),
            Event::End(_) => {
                if let Some(declared) = self.scopes.pop() {
                    self.in_scope = self.in_scope.saturating_sub(declared);
                }
                Token::End
            }
            Event::Text(text) => Token::Text(text),
            Event::CData(data) => Token::CData(data),
            Event::GeneralRef(reference) => Token::Ref(reference),
            Event::Eof => Token::Eof,
            _ => Token::Other,
        };
        Ok((range, token))
    }

    fn tag(
        &mut self,
        start: BytesStart<'a>,
        prefix: Option<&'static str>,
        empty: bool,
        charge: &mut impl FnMut(Record) -> Result<(), DocxError>,
    ) -> Result<Tag<'a>, DocxError> {
        charge(Record::Element)?;
        // The declarations first, before any attribute is resolved: resolving walks every
        // declaration in scope, so a tag with more than the cap would make each resolution long.
        let mut declared = 0usize;
        for a in start.attributes().with_checks(false).flatten() {
            if is_declaration(a.key.as_ref()) {
                declared += 1;
                if self.in_scope.saturating_add(declared) > MAX_BINDINGS {
                    return Err(DocxError::Limit(TOO_MANY_BINDINGS.into()));
                }
                charge(Record::Declaration)?;
            }
        }
        let local = String::from_utf8_lossy(start.local_name().as_ref()).into_owned();
        let name = match prefix {
            Some(p) => format!("{p}:{local}"),
            None => local,
        };
        let mut attrs = Vec::new();
        for a in start.attributes().with_checks(false) {
            let Ok(a) = a else { continue };
            let key = a.key;
            if is_declaration(key.as_ref()) {
                continue;
            }
            charge(Record::Attribute)?;
            let (ares, alocal) = self.reader.resolve_attribute(key);
            let alocal = String::from_utf8_lossy(alocal.as_ref()).into_owned();
            let aname = match ares {
                ResolveResult::Bound(ns) => format!("{}:{alocal}", prefix_for(ns.as_ref())),
                _ => alocal,
            };
            let val = match a.unescape_value() {
                Ok(v) => v.into_owned(),
                Err(_) => String::from_utf8_lossy(&a.value).into_owned(),
            };
            attrs.push((aname, val));
        }
        // An element written empty takes its declarations out of scope at once.
        if !empty {
            self.scopes.push(declared);
            self.in_scope = self.in_scope.saturating_add(declared);
        }
        Ok(Tag { start, name, attrs, empty })
    }
}

fn is_declaration(key: &[u8]) -> bool {
    key == b"xmlns" || key.starts_with(b"xmlns:")
}

/// embed: text between tags as it is kept; text that is not UTF-8 refuses the part.
pub(crate) fn text<'b>(text: &'b BytesText<'_>) -> Result<std::borrow::Cow<'b, str>, DocxError> {
    text.decode().map_err(|e| DocxError::Xml(e.to_string()))
}

/// embed: the character a reference stands for: `None` for an entity or character this reader does
/// not know (left out of the text); an entity name that is not UTF-8 refuses the part.
pub(crate) fn reference(reference: &BytesRef<'_>) -> Result<Option<char>, DocxError> {
    Ok(match reference.resolve_char_ref() {
        Ok(Some(c)) => Some(c),
        Ok(None) => {
            let n = reference.decode().map_err(|e| DocxError::Xml(e.to_string()))?;
            quick_xml::escape::resolve_predefined_entity(&n).and_then(|entity| entity.chars().next())
        }
        Err(_) => None,
    })
}

fn push_text(top: &mut El, s: &str) {
    if let Some(Node::Text(prev)) = top.kids.last_mut() {
        prev.push_str(s);
    } else {
        top.kids.push(Node::Text(s.to_string()));
    }
}

fn attach(stack: &mut [El], root: &mut Option<El>, el: El) {
    match stack.last_mut() {
        Some(parent) => parent.kids.push(Node::El(el)),
        None => {
            if root.is_none() {
                *root = Some(el);
            }
        }
    }
}

/// Is `c` allowed in XML 1.0 text?
fn xml_char_ok(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

/// Escape text for element content or attribute values (drops characters XML can't carry).
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            '\t' => o.push_str("&#9;"),
            '\n' => o.push_str("&#10;"),
            '\r' => o.push_str("&#13;"),
            c if xml_char_ok(c) => o.push(c),
            _ => {}
        }
    }
    o
}

/// A string-building XML writer.
#[derive(Default)]
pub struct W {
    pub s: String,
}

impl W {
    pub fn new() -> W {
        W { s: String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n") }
    }
    fn attrs(&mut self, attrs: &[(&str, &str)]) {
        for (k, v) in attrs {
            self.s.push(' ');
            self.s.push_str(k);
            self.s.push_str("=\"");
            self.s.push_str(&esc(v));
            self.s.push('"');
        }
    }
    pub fn open(&mut self, name: &str, attrs: &[(&str, &str)]) {
        self.s.push('<');
        self.s.push_str(name);
        self.attrs(attrs);
        self.s.push('>');
    }
    pub fn empty(&mut self, name: &str, attrs: &[(&str, &str)]) {
        self.s.push('<');
        self.s.push_str(name);
        self.attrs(attrs);
        self.s.push_str("/>");
    }
    pub fn close(&mut self, name: &str) {
        self.s.push_str("</");
        self.s.push_str(name);
        self.s.push('>');
    }
    pub fn text(&mut self, t: &str) {
        self.s.push_str(&esc(t));
    }
    /// `<name>text</name>`.
    pub fn leaf(&mut self, name: &str, attrs: &[(&str, &str)], t: &str) {
        self.open(name, attrs);
        self.text(t);
        self.close(name);
    }
    /// `<name w:val="v"/>`.
    pub fn val(&mut self, name: &str, v: &str) {
        self.empty(name, &[("w:val", v)]);
    }
    pub fn raw(&mut self, s: &str) {
        self.s.push_str(s);
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.s.into_bytes()
    }
}

/// `xmlns:` declarations for the body-part namespaces, plus `mc:Ignorable`.
pub fn body_ns() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = NAMESPACES.iter().map(|(p, u)| (format!("xmlns:{p}"), (*u).to_string())).collect();
    v.push(("mc:Ignorable".into(), "w14 w15 wp14".into()));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_with_namespaces() {
        let x = br#"<?xml version="1.0"?><x:document xmlns:x="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:rr="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><x:p rr:id="r1" x:val="a&amp;b"><x:t xml:space="preserve"> hi &#65;&lt; </x:t></x:p></x:document>"#;
        let e = parse(x).unwrap();
        assert_eq!(e.name, "w:document");
        let p = e.child("w:p").unwrap();
        assert_eq!(p.attr("r:id"), Some("r1"));
        assert_eq!(p.attr("w:val"), Some("a&b"));
        let t = p.child("w:t").unwrap();
        assert_eq!(t.text(), " hi A< ");
        assert_eq!(t.attr("xml:space"), Some("preserve"));
    }

    #[test]
    fn deep_nesting_is_error() {
        let mut s = String::new();
        for _ in 0..1000 {
            s.push_str("<a>");
        }
        assert!(parse(s.as_bytes()).is_err());
    }

    #[test]
    fn garbage_is_error_not_panic() {
        assert!(parse(b"").is_err());
        assert!(parse(b"\xff\xfe<").is_err());
        let _ = parse(b"<a><b></a>");
        let _ = parse(b"<a x='1' x='2'/>");
    }

    // embed: namespace declarations are capped in scope, so resolving a name never walks more than
    // the cap; the element count stays a count of elements.
    #[test]
    fn caps_namespace_declarations_in_scope() {
        let declarations = |count: usize| (0..count).map(|index| format!(" xmlns:p{index}='u'")).collect::<String>();
        assert!(parse(format!("<r{}/>", declarations(MAX_BINDINGS)).as_bytes()).is_ok());
        assert_eq!(parse(format!("<r{}/>", declarations(MAX_BINDINGS + 1)).as_bytes()), Err(DocxError::Limit(TOO_MANY_BINDINGS.into())));
        let three = format!("<r{}><a/></r>", declarations(3));
        assert_eq!(parse_limited(three.as_bytes(), 2, MAX_DEPTH).map(|(_, used)| used), Ok(2));
        let started = std::time::Instant::now();
        assert!(parse(format!("<r{}><a/></r>", declarations(100_000)).as_bytes()).is_err());
        assert!(parse(format!("<r{}>{}</r>", declarations(MAX_BINDINGS), "<z:a/>".repeat(20_000)).as_bytes()).is_ok());
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "took {:?}", started.elapsed());
    }

    // embed: two byte-order marks are read as before: the first stripped here, the second by the
    // reader.
    #[test]
    fn reads_a_part_after_two_byte_order_marks() {
        let root = parse(b"\xef\xbb\xbf\xef\xbb\xbf<a><b/></a>").unwrap();
        assert_eq!((root.name.as_str(), root.els().count()), ("a", 1));
        assert!(parse(b"\xef\xbb\xbf\xff\xfe<\x00a\x00/\x00>\x00").is_err());
    }

    #[test]
    fn escapes() {
        assert_eq!(esc("a<b&\"\u{1}"), "a&lt;b&amp;&quot;");
    }
}

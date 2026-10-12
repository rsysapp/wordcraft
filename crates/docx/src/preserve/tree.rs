//! embed: a positional XML tree for the preserving writer. It reads a part as [`crate::xml`] does,
//! and keeps where each tag, piece of text, reference and CDATA section is in the part's bytes, so a
//! writer can rewrite one element and copy every other byte unchanged. Comments, processing
//! instructions and the XML declaration are kept nowhere but in those bytes: they lie between the
//! ranges of the nodes around them.
//!
//! Bounds: every record the tree keeps (an element, an attribute, a piece of text, a reference, a
//! CDATA section) counts as one element against the caller's limit, because the tree keeps one
//! record for each reference and piece of text where the desktop parser merges them; elements nest
//! no deeper than the caller's depth limit (`ReadLimits::max_depth`, at most [`MAX_DEPTH`]). Parsing
//! and [`PEl::find`] keep their own stacks; the derived `Clone`, `PartialEq`, `Debug` and the drop
//! recurse once per level, so the depth limit bounds them.

use std::ops::Range;

use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

use crate::DocxError;
use crate::xml::{MAX_DEPTH, MAX_ELEMENTS, TOO_DEEP, TOO_MANY_ELEMENTS, prefix_for};

/// An element with where its tags are in the part. Names are canonical as in `xml::El` (`w:p`
/// whatever prefix the part chose); `raw_name` is the name as written, which new markup reuses.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PEl {
    pub name: String,
    pub raw_name: String,
    pub attrs: Vec<(String, String)>,
    /// The start tag, or the whole element when it is written empty (`<w:tab/>`).
    pub start: Range<usize>,
    /// The end tag; `None` for an element written empty, or one the part never closes.
    pub end: Option<Range<usize>>,
    pub kids: Vec<PKid>,
}

/// What lies between an element's tags.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PKid {
    El(PEl),
    /// Literal text: the parser reports every `&…;` on its own, as `Ref`.
    Text(Range<usize>),
    /// A character or entity reference, and the character it stands for (`None`: unknown).
    Ref {
        range: Range<usize>,
        ch: Option<char>,
    },
    /// A `<![CDATA[…]]>` section, delimiters included.
    CData(Range<usize>),
}

impl PEl {
    /// Attribute by canonical name, falling back to the unprefixed local name, as `xml::El::attr`.
    pub fn attr(&self, name: &str) -> Option<&str> {
        if let Some((_, value)) = self.attrs.iter().find(|(key, _)| key == name) {
            return Some(value);
        }
        let local = name.rsplit(':').next().unwrap_or(name);
        if local != name {
            return self.attrs.iter().find(|(key, _)| key == local).map(|(_, value)| value.as_str());
        }
        None
    }

    pub fn els(&self) -> impl Iterator<Item = &PEl> {
        self.kids.iter().filter_map(|kid| match kid {
            PKid::El(element) => Some(element),
            _ => None,
        })
    }

    pub fn child(&self, name: &str) -> Option<&PEl> {
        self.els().find(|element| element.name == name)
    }

    pub fn local(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }

    /// The whole element, tags included.
    pub fn span(&self) -> Range<usize> {
        self.start.start..self.end.as_ref().map_or(self.start.end, |end| end.end)
    }

    /// The first descendant with this name, depth first, as `xml::El::find`. Walks with its own
    /// stack (one entry per level), never by recursion.
    pub fn find(&self, name: &str) -> Option<&PEl> {
        let mut levels = vec![self.kids.iter()];
        while let Some(level) = levels.last_mut() {
            match level.next() {
                Some(PKid::El(child)) => {
                    if child.name == name {
                        return Some(child);
                    }
                    levels.push(child.kids.iter());
                }
                Some(_) => {}
                None => {
                    levels.pop();
                }
            }
        }
        None
    }
}

/// Parse a part as `xml::parse` does (same configuration, same limits, a leading byte-order mark
/// skipped), keeping byte ranges into the part as given.
pub(crate) fn parse(bytes: &[u8]) -> Result<PEl, DocxError> {
    parse_limited(bytes, MAX_ELEMENTS, MAX_DEPTH).map(|(root, _)| root)
}

/// [`parse`] keeping at most `max_records` records (never more than [`MAX_ELEMENTS`]: elements,
/// attributes, pieces of text, references and CDATA sections each count one) with elements nested
/// at most `max_depth` deep (never deeper than [`MAX_DEPTH`]), as `xml::parse_limited` takes a
/// package's limits; also answers how many records it kept, so a package can spend one budget
/// across its parts. Past either limit it refuses with the desktop parser's errors.
pub(crate) fn parse_limited(bytes: &[u8], max_records: usize, max_depth: usize) -> Result<(PEl, usize), DocxError> {
    let max_records = max_records.min(MAX_ELEMENTS);
    let max_depth = max_depth.min(MAX_DEPTH);
    let skip = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 };
    let body = bytes.get(skip..).unwrap_or_default();
    if body.starts_with(&[0xFF, 0xFE]) || body.starts_with(&[0xFE, 0xFF]) {
        return Err(DocxError::Xml("UTF-16 XML parts are not supported".into()));
    }
    let mut reader = NsReader::from_reader(body);
    {
        let config = reader.config_mut();
        config.trim_text_start = false;
        config.trim_text_end = false;
        config.expand_empty_elements = false;
        config.check_end_names = false;
    }
    // Offsets into the part as given, byte-order mark included.
    let at = |reader: &NsReader<&[u8]>| usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX).saturating_add(skip);
    let mut records = Records { kept: 0, limit: max_records };
    let mut stack: Vec<PEl> = Vec::new();
    let mut root: Option<PEl> = None;
    loop {
        let before = at(&reader);
        let (resolved, event) = reader.read_resolved_event().map_err(|error| DocxError::Xml(error.to_string()))?;
        let prefix = match resolved {
            ResolveResult::Bound(namespace) => Some(prefix_for(namespace.as_ref())),
            _ => None,
        };
        let range = before..at(&reader);
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                records.keep()?;
                let local = String::from_utf8_lossy(start.local_name().as_ref()).into_owned();
                let name = match prefix {
                    Some(prefix) => format!("{prefix}:{local}"),
                    None => local,
                };
                let raw_name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
                let mut attrs = Vec::new();
                for attribute in start.attributes().with_checks(false) {
                    let Ok(attribute) = attribute else { continue };
                    let key = attribute.key;
                    if key.as_ref() == b"xmlns" || key.as_ref().starts_with(b"xmlns:") {
                        continue;
                    }
                    records.keep()?;
                    let (bound, local) = reader.resolve_attribute(key);
                    let local = String::from_utf8_lossy(local.as_ref()).into_owned();
                    let key = match bound {
                        ResolveResult::Bound(namespace) => format!("{}:{local}", prefix_for(namespace.as_ref())),
                        _ => local,
                    };
                    let value = match attribute.unescape_value() {
                        Ok(value) => value.into_owned(),
                        Err(_) => String::from_utf8_lossy(&attribute.value).into_owned(),
                    };
                    attrs.push((key, value));
                }
                // Kept for the life of the tree: no room left over from growing.
                attrs.shrink_to_fit();
                let element = PEl { name, raw_name, attrs, start: range, end: None, kids: Vec::new() };
                if matches!(event, Event::Start(_)) {
                    if stack.len() >= max_depth {
                        return Err(DocxError::Limit(TOO_DEEP.into()));
                    }
                    stack.push(element);
                } else {
                    attach(&mut stack, &mut root, element);
                }
            }
            Event::End(_) => {
                if let Some(mut element) = stack.pop() {
                    element.end = Some(range);
                    attach(&mut stack, &mut root, element);
                }
            }
            // Text outside the root is not part of the tree, as in the desktop parser.
            Event::Text(ref text) if !stack.is_empty() => {
                // The desktop parser refuses text it cannot decode; so does this tree, so the two
                // read the same parts.
                text.decode().map_err(|error| DocxError::Xml(error.to_string()))?;
                records.keep()?;
                push(&mut stack, PKid::Text(range));
            }
            Event::CData(_) if !stack.is_empty() => {
                records.keep()?;
                push(&mut stack, PKid::CData(range));
            }
            Event::GeneralRef(ref reference) if !stack.is_empty() => {
                let ch = match reference.resolve_char_ref() {
                    Ok(Some(ch)) => Some(ch),
                    Ok(None) => {
                        let name = reference.decode().map_err(|error| DocxError::Xml(error.to_string()))?;
                        quick_xml::escape::resolve_predefined_entity(&name).and_then(|text| text.chars().next())
                    }
                    Err(_) => None,
                };
                records.keep()?;
                push(&mut stack, PKid::Ref { range, ch });
            }
            Event::Eof => break,
            _ => {}
        }
        if root.is_some() && stack.is_empty() {
            break;
        }
    }
    // Unclosed elements at the end: closed, as the lenient parser does.
    while let Some(element) = stack.pop() {
        attach(&mut stack, &mut root, element);
    }
    let root = root.ok_or_else(|| DocxError::Xml("no root element".into()))?;
    Ok((root, records.kept))
}

/// The records a parse has kept, and how many it may.
struct Records {
    kept: usize,
    limit: usize,
}

impl Records {
    /// Count one more record, or refuse before it is kept.
    fn keep(&mut self) -> Result<(), DocxError> {
        if self.kept >= self.limit {
            return Err(DocxError::Limit(TOO_MANY_ELEMENTS.into()));
        }
        self.kept += 1;
        Ok(())
    }
}

fn push(stack: &mut [PEl], kid: PKid) {
    if let Some(top) = stack.last_mut() {
        top.kids.push(kid);
    }
}

fn attach(stack: &mut [PEl], root: &mut Option<PEl>, mut element: PEl) {
    // Kept for the life of the tree: no room left over from growing.
    element.kids.shrink_to_fit();
    match stack.last_mut() {
        Some(parent) => parent.kids.push(PKid::El(element)),
        None => {
            if root.is_none() {
                *root = Some(element);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

    #[test]
    fn locates_tags_text_and_references() {
        let part = format!(
            r#"<?xml version="1.0"?><w:document xmlns:w="{W}"><w:body><w:p w:rsidR="00A1"><w:r><w:t xml:space="preserve">a&amp;b</w:t></w:r><w:r><w:tab/></w:r></w:p></w:body></w:document>"#
        );
        let bytes = part.as_bytes();
        let root = parse(bytes).unwrap();
        let paragraph = root.child("w:body").unwrap().child("w:p").unwrap();
        assert_eq!(&bytes[paragraph.start.clone()], br#"<w:p w:rsidR="00A1">"#);
        assert_eq!(&bytes[paragraph.span()], br#"<w:p w:rsidR="00A1"><w:r><w:t xml:space="preserve">a&amp;b</w:t></w:r><w:r><w:tab/></w:r></w:p>"#);
        assert_eq!(paragraph.attr("w:rsidR"), Some("00A1"));
        let text = paragraph.child("w:r").unwrap().child("w:t").unwrap();
        assert_eq!(text.kids.len(), 3);
        assert!(matches!(&text.kids[1], PKid::Ref { range, ch: Some('&') } if &bytes[range.clone()] == b"&amp;"));
        let tab = paragraph.els().nth(1).unwrap().child("w:tab").unwrap();
        assert_eq!((&bytes[tab.start.clone()], tab.end.clone()), (&b"<w:tab/>"[..], None));
    }

    #[test]
    fn names_elements_canonically_and_keeps_the_written_name() {
        let part = format!(r#"<x:document xmlns:x="{W}"><x:body><x:p/></x:body></x:document>"#);
        let root = parse(part.as_bytes()).unwrap();
        let paragraph = root.child("w:body").unwrap().child("w:p").unwrap();
        assert_eq!((paragraph.name.as_str(), paragraph.raw_name.as_str(), paragraph.local()), ("w:p", "x:p", "p"));
    }

    #[test]
    fn keeps_offsets_into_the_part_after_a_byte_order_mark() {
        let mut part = vec![0xEF, 0xBB, 0xBF];
        part.extend_from_slice(format!(r#"<w:document xmlns:w="{W}"/>"#).as_bytes());
        assert_eq!(parse(&part).unwrap().start, 3..part.len());
    }

    #[test]
    fn finds_a_descendant_and_refuses_nesting_past_the_limit() {
        let part = format!(r#"<w:document xmlns:w="{W}"><w:body><w:p><w:r><w:t>x</w:t></w:r></w:p></w:body></w:document>"#);
        assert!(parse(part.as_bytes()).unwrap().find("w:t").is_some());
        let deep = format!("{}{}", "<a>".repeat(crate::xml::MAX_DEPTH + 2), "</a>".repeat(crate::xml::MAX_DEPTH + 2));
        assert!(matches!(parse(deep.as_bytes()), Err(DocxError::Limit(_))));
    }

    /// The bytes inside `element` that none of its children cover, in order: what a writer copying
    /// the element child by child must copy between them.
    fn uncovered<'a>(element: &PEl, bytes: &'a [u8]) -> Vec<&'a [u8]> {
        let Some(end) = element.end.clone() else { return Vec::new() };
        let mut gaps = Vec::new();
        let mut at = element.start.end;
        for kid in &element.kids {
            let range = match kid {
                PKid::El(child) => child.span(),
                PKid::Text(range) | PKid::Ref { range, .. } | PKid::CData(range) => range.clone(),
            };
            assert!(range.start >= at, "children in order");
            if range.start > at {
                gaps.push(&bytes[at..range.start]);
            }
            at = range.end;
        }
        assert!(end.start >= at);
        if end.start > at {
            gaps.push(&bytes[at..end.start]);
        }
        gaps
    }

    #[test]
    fn covers_every_byte_through_every_form_word_writes() {
        let part = format!(
            "\u{FEFF}<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<!-- before -->\
             <w:document xmlns:w=\"{W}\" xmlns:mc='http://schemas.openxmlformats.org/markup-compatibility/2006'>\
             <w:body><w:p w:rsidR='00A1' w:rsidP=\"a&quot;'>\"><!-- inside --><?mso-application progid=\"Word.Document\"?>\
             <w:r><w:t xml:space='preserve'> &#65;&#x42;&lt;&gt;&apos;&quot;&unknown;<![CDATA[x<y&z]]>end </w:t>\
             <w:br /><w:tab/></w:r></w:p></w:body></w:document>\r\n"
        );
        let bytes = part.as_bytes();
        let root = parse(bytes).unwrap();
        assert_eq!(&bytes[root.start.clone()][..12], b"<w:document ");
        assert_eq!(&bytes[root.end.clone().unwrap()], b"</w:document>");
        let paragraph = root.find("w:p").unwrap();
        assert_eq!(&bytes[paragraph.start.clone()], "<w:p w:rsidR='00A1' w:rsidP=\"a&quot;'>\">".as_bytes());
        assert_eq!(paragraph.attr("w:rsidP"), Some("a\"'>"));
        // Comments and processing instructions are kept as bytes between children, nowhere else.
        assert_eq!(uncovered(paragraph, bytes), vec![&b"<!-- inside --><?mso-application progid=\"Word.Document\"?>"[..]]);
        let text = paragraph.find("w:t").unwrap();
        assert_eq!(text.attr("xml:space"), Some("preserve"));
        assert!(uncovered(text, bytes).is_empty());
        let kids: Vec<(&[u8], Option<char>)> = text
            .kids
            .iter()
            .map(|kid| match kid {
                PKid::Ref { range, ch } => (&bytes[range.clone()], *ch),
                PKid::Text(range) | PKid::CData(range) => (&bytes[range.clone()], None),
                PKid::El(_) => (&b"<element>"[..], None),
            })
            .collect();
        assert_eq!(
            kids,
            vec![
                (&b" "[..], None),
                (&b"&#65;"[..], Some('A')),
                (&b"&#x42;"[..], Some('B')),
                (&b"&lt;"[..], Some('<')),
                (&b"&gt;"[..], Some('>')),
                (&b"&apos;"[..], Some('\'')),
                (&b"&quot;"[..], Some('"')),
                (&b"&unknown;"[..], None),
                (&b"<![CDATA[x<y&z]]>"[..], None),
                (&b"end "[..], None),
            ]
        );
        assert!(matches!(text.kids[8], PKid::CData(_)));
        let run = paragraph.child("w:r").unwrap();
        let line_break = run.child("w:br").unwrap();
        assert_eq!((&bytes[line_break.span()], line_break.end.clone()), (&b"<w:br />"[..], None));
        // Every element down to the run is covered child by child.
        for element in [&root, root.child("w:body").unwrap(), run] {
            assert!(uncovered(element, bytes).is_empty());
        }
    }

    #[test]
    fn closes_what_the_part_leaves_open_and_keeps_only_the_root() {
        let part = format!(r#"<w:document xmlns:w="{W}"><w:body><w:p>text"#);
        let root = parse(part.as_bytes()).unwrap();
        let paragraph = root.find("w:p").unwrap();
        assert_eq!((root.end.clone(), paragraph.end.clone()), (None, None));
        assert_eq!(paragraph.span(), paragraph.start.clone());
        assert!(matches!(paragraph.kids.as_slice(), [PKid::Text(range)] if &part.as_bytes()[range.clone()] == b"text"));
        // Text before the root and anything after it are not part of the tree.
        let root = parse(b"  <a>x</a><b/>trailing").unwrap();
        assert_eq!((root.name.as_str(), root.span()), ("a", 2..10));
    }

    #[test]
    fn reads_what_the_desktop_parser_reads_and_refuses_what_it_refuses() {
        assert!(parse(b"").is_err());
        assert!(parse(b"\xff\xfe<\x00a\x00/\x00>\x00").is_err());
        assert!(parse(b"<a>\xff</a>").is_err());
        assert!(crate::xml::parse(b"<a>\xff</a>").is_err());
        // Mismatched end tags are forgiven as the desktop parser forgives them.
        let root = parse(b"<a><b></a>").unwrap();
        assert_eq!(root.child("b").unwrap().end, Some(6..10));
    }

    #[test]
    fn counts_every_record_it_keeps_against_the_limit() {
        // Two elements, one attribute, two pieces of text and a reference.
        let part = br#"<a b="1">x&amp;y<c/></a>"#;
        assert_eq!(parse_limited(part, 6, MAX_DEPTH).map(|(_, used)| used), Ok(6));
        assert_eq!(parse_limited(part, 5, MAX_DEPTH), Err(DocxError::Limit(TOO_MANY_ELEMENTS.into())));
        // References are kept one by one, so a run of them is charged one by one, whatever the
        // element count.
        let references = format!("<a>{}</a>", "&amp;".repeat(1_000));
        assert!(matches!(parse_limited(references.as_bytes(), 1_000, MAX_DEPTH), Err(DocxError::Limit(_))));
        assert!(parse_limited(references.as_bytes(), 1_001, MAX_DEPTH).is_ok());
        let sections = format!("<a>{}</a>", "<![CDATA[]]>".repeat(1_000));
        assert!(matches!(parse_limited(sections.as_bytes(), 1_000, MAX_DEPTH), Err(DocxError::Limit(_))));
        let attributes = format!("<a {}/>", (0..1_000).map(|index| format!("a{index}=''")).collect::<Vec<_>>().join(" "));
        assert!(matches!(parse_limited(attributes.as_bytes(), 1_000, MAX_DEPTH), Err(DocxError::Limit(_))));
        assert_eq!(parse_limited(attributes.as_bytes(), 1_001, MAX_DEPTH).map(|(_, used)| used), Ok(1_001));
    }

    #[test]
    fn nests_no_deeper_than_the_callers_limit() {
        let nested = |depth: usize| format!("{}{}", "<a>".repeat(depth), "</a>".repeat(depth));
        assert!(parse_limited(nested(64).as_bytes(), usize::MAX, 64).is_ok());
        assert_eq!(parse_limited(nested(65).as_bytes(), usize::MAX, 64), Err(DocxError::Limit(TOO_DEEP.into())));
        // An element written empty opens nothing.
        assert!(parse_limited(format!("{}<a/>{}", "<a>".repeat(64), "</a>".repeat(64)).as_bytes(), usize::MAX, 64).is_ok());
        // Never deeper than the parser's own limit, whatever the caller asks.
        let past = nested(MAX_DEPTH + 1);
        assert_eq!(parse_limited(past.as_bytes(), usize::MAX, usize::MAX), Err(DocxError::Limit(TOO_DEEP.into())));
    }

    #[test]
    fn walks_the_deepest_tree_it_accepts_on_a_small_stack() {
        let part = format!("{}<t/>{}", "<r>".repeat(MAX_DEPTH), "</r>".repeat(MAX_DEPTH));
        // A quarter of WebAssembly's 1 MiB stack: finding, comparing, copying and dropping the
        // deepest accepted tree stays well within it.
        let walked = std::thread::Builder::new()
            .stack_size(256 << 10)
            .spawn(move || {
                let root = parse(part.as_bytes()).unwrap();
                let found = root.find("t").map(PEl::span);
                let copy = root.clone();
                let equal = copy == root;
                drop(copy);
                drop(root);
                (found, equal)
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(walked, (Some(MAX_DEPTH * 3..MAX_DEPTH * 3 + 4), true));
    }

    #[test]
    fn finds_depth_first_in_document_order() {
        let root = parse(br#"<a><b><c n="1"/></b><c n="2"/></a>"#).unwrap();
        assert_eq!(root.find("c").and_then(|found| found.attr("n")), Some("1"));
        assert!(root.find("a").is_none());
        assert_eq!(root.els().count(), 2);
    }
}

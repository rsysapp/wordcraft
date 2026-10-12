//! embed: a positional XML tree for the preserving writer. It reads a part through the desktop
//! parser's own reading ([`crate::xml`]), so the two accept the same parts and name everything alike,
//! and keeps where each tag, piece of text, reference and CDATA section is in the part's bytes, so a
//! writer can rewrite one element and copy every other byte unchanged. Comments, processing
//! instructions and the XML declaration are kept nowhere but in those bytes: they lie between the
//! ranges of the nodes around them.
//!
//! Bounds: every record the tree keeps (an element, an attribute, a piece of text, a reference, a
//! CDATA section), and every namespace declaration written, counts as one element against the
//! caller's limit, because the tree keeps one record for each reference and piece of text where the
//! desktop parser merges them (the desktop parser counts elements only); elements nest no deeper
//! than the caller's depth limit (`ReadLimits::max_depth`, at most [`MAX_DEPTH`]); what the tree
//! holds is charged as it is kept, against the caller's memory allowance ([`parse_charged`]). Parsing
//! and [`PEl::find`] keep their own stacks; the derived `Clone`, `PartialEq`, `Debug` and the drop
//! recurse once per level, so the depth limit bounds them.

use std::ops::Range;

use crate::DocxError;
use crate::inspect::budget::{units, vector_slot};
use crate::xml::{self, MAX_DEPTH, MAX_ELEMENTS, Reading, TOO_DEEP, TOO_MANY_ELEMENTS, Token};

/// An element with where its tags are in the part. Names are canonical as in `xml::El` (`w:p`
/// whatever prefix the part chose); `raw_name` is the name as written (bytes that are not UTF-8 read
/// as U+FFFD): new markup reuses it only through [`PEl::reusable_name`].
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

    /// The name as written, to write again in new markup; `None` when it holds U+FFFD, which stands
    /// for bytes that were not UTF-8 (or is written so), so writing it would name another element.
    pub fn reusable_name(&self) -> Option<&str> {
        if self.raw_name.contains(char::REPLACEMENT_CHARACTER) { None } else { Some(&self.raw_name) }
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

/// [`parse_limited`] within the parser's own limits. Tests only: a production caller reads within a
/// package's read limits (`ReadLimits`) and a memory allowance, through [`parse_charged`].
#[cfg(test)]
pub(crate) fn parse(bytes: &[u8]) -> Result<PEl, DocxError> {
    parse_limited(bytes, MAX_ELEMENTS, MAX_DEPTH).map(|(root, _)| root)
}

/// [`parse_charged`] with no memory allowance of its own. Tests only.
#[cfg(test)]
pub(crate) fn parse_limited(bytes: &[u8], max_records: usize, max_depth: usize) -> Result<(PEl, usize), DocxError> {
    parse_charged(bytes, max_records, max_depth, u64::MAX).map(|(parsed, used)| (parsed.root, used))
}

/// The error [`parse_charged`] answers past its memory allowance.
pub(crate) const TOO_MUCH_MEMORY: &str = "more memory than a part's tree may hold";

/// A parsed part, and the most memory its parse was charged.
pub(crate) struct Parsed {
    pub root: PEl,
    pub charged: u64,
}

/// Parse a part through the desktop parser's own reading (`xml::Reading`: the same configuration,
/// byte-order marks, names, attributes and refusals), keeping byte ranges into the part as given.
///
/// At most `max_records` records (never more than [`MAX_ELEMENTS`]): each element, attribute, piece
/// of text, reference, CDATA section and namespace declaration written counts one. The desktop
/// parser counts elements only, so this can refuse a part it reads (and the writer then falls back),
/// never the other way round. Elements nest at most `max_depth` deep (never deeper than
/// [`MAX_DEPTH`]). Answers how many records it counted, so a package can spend one budget across
/// its parts. Past either limit it refuses with the desktop parser's errors.
///
/// What the tree holds is charged as each record is kept (its room in its parent's growing list,
/// its names and attributes, and its place on the open elements' stack while it is open), and the
/// parse refuses with [`TOO_MUCH_MEMORY`] before the charge passes `max_bytes`. The answer carries
/// the most it was charged.
pub(crate) fn parse_charged(bytes: &[u8], max_records: usize, max_depth: usize, max_bytes: u64) -> Result<(Parsed, usize), DocxError> {
    let max_depth = max_depth.min(MAX_DEPTH);
    let mut reading = Reading::new(bytes)?;
    let mut records = Records { kept: 0, limit: max_records.min(MAX_ELEMENTS) };
    let mut memory = Charge { held: 0, peak: 0, limit: max_bytes };
    let mut stack: Vec<PEl> = Vec::new();
    let mut root: Option<PEl> = None;
    loop {
        let (range, token) = reading.next(&mut |_| records.keep())?;
        match token {
            Token::Start(tag) => {
                let raw_name = String::from_utf8_lossy(tag.start.name().as_ref()).into_owned();
                let mut attrs = tag.attrs;
                // Kept for the life of the tree: no room left over from growing.
                attrs.shrink_to_fit();
                let element = PEl { name: tag.name, raw_name, attrs, start: range, end: None, kids: Vec::new() };
                memory.hold(vector_slot::<PKid>().saturating_add(names_bytes(&element)))?;
                if tag.empty {
                    attach(&mut stack, &mut root, element);
                } else {
                    if stack.len() >= max_depth {
                        return Err(DocxError::Limit(TOO_DEEP.into()));
                    }
                    memory.hold(vector_slot::<PEl>())?;
                    stack.push(element);
                }
            }
            Token::End => {
                if let Some(mut element) = stack.pop() {
                    memory.release(vector_slot::<PEl>());
                    element.end = Some(range);
                    attach(&mut stack, &mut root, element);
                }
            }
            // Text, CDATA and references outside the root are not part of the tree, as in the
            // desktop parser.
            Token::Text(text) if !stack.is_empty() => {
                xml::text(&text)?;
                records.keep()?;
                memory.hold(vector_slot::<PKid>())?;
                push(&mut stack, PKid::Text(range));
            }
            Token::CData(_) if !stack.is_empty() => {
                records.keep()?;
                memory.hold(vector_slot::<PKid>())?;
                push(&mut stack, PKid::CData(range));
            }
            Token::Ref(reference) if !stack.is_empty() => {
                let ch = xml::reference(&reference)?;
                records.keep()?;
                memory.hold(vector_slot::<PKid>())?;
                push(&mut stack, PKid::Ref { range, ch });
            }
            Token::Eof => break,
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
    Ok((Parsed { root, charged: memory.peak }, records.kept))
}

/// What an element's names and attributes hold.
fn names_bytes(element: &PEl) -> u64 {
    let attributes = element.attrs.iter().fold(0usize, |sum, (key, value)| sum.saturating_add(key.capacity()).saturating_add(value.capacity()));
    units(element.name.capacity().saturating_add(element.raw_name.capacity()).saturating_add(attributes))
        .saturating_add(units(element.attrs.capacity()).saturating_mul(units(std::mem::size_of::<(String, String)>())))
}

/// What a parse holds, refused past its allowance.
struct Charge {
    held: u64,
    peak: u64,
    limit: u64,
}

impl Charge {
    fn hold(&mut self, bytes: u64) -> Result<(), DocxError> {
        let held = self.held.checked_add(bytes).filter(|held| *held <= self.limit).ok_or_else(|| DocxError::Limit(TOO_MUCH_MEMORY.into()))?;
        self.held = held;
        self.peak = self.peak.max(held);
        Ok(())
    }

    fn release(&mut self, bytes: u64) {
        self.held = self.held.saturating_sub(bytes);
    }
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
    use crate::xml::{MAX_BINDINGS, TOO_MANY_BINDINGS};

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
        // Every namespace declaration written is charged too, though the tree keeps none of them.
        let declarations = format!("<a {}/>", (0..MAX_BINDINGS).map(|index| format!("xmlns:p{index}='u'")).collect::<Vec<_>>().join(" "));
        assert_eq!(parse_limited(declarations.as_bytes(), MAX_BINDINGS + 1, MAX_DEPTH).map(|(_, used)| used), Ok(MAX_BINDINGS + 1));
        assert_eq!(parse_limited(declarations.as_bytes(), MAX_BINDINGS, MAX_DEPTH), Err(DocxError::Limit(TOO_MANY_ELEMENTS.into())));
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
        // A quarter of WebAssembly's 1 MiB stack: finding, comparing, copying, printing and dropping
        // the deepest accepted tree stays within it.
        let walked = std::thread::Builder::new()
            .stack_size(256 << 10)
            .spawn(move || {
                let root = parse(part.as_bytes()).unwrap();
                let found = root.find("t").map(PEl::span);
                let copy = root.clone();
                let equal = copy == root;
                let printed = format!("{root:?}").contains("name: \"t\"");
                drop(copy);
                drop(root);
                (found, equal, printed)
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(walked, (Some(MAX_DEPTH * 3..MAX_DEPTH * 3 + 4), true, true));
    }

    #[test]
    fn finds_depth_first_in_document_order() {
        let root = parse(br#"<a><b><c n="1"/></b><c n="2"/></a>"#).unwrap();
        assert_eq!(root.find("c").and_then(|found| found.attr("n")), Some("1"));
        assert!(root.find("a").is_none());
        assert_eq!(root.els().count(), 2);
    }

    #[test]
    fn keeps_exact_offsets_after_two_byte_order_marks() {
        // The desktop parser strips one mark and its reader strips a second; the ranges must still
        // point at the tags.
        let mut part = vec![0xEF, 0xBB, 0xBF, 0xEF, 0xBB, 0xBF];
        part.extend_from_slice(b"<a><b/></a>");
        let root = parse(&part).unwrap();
        assert_eq!(&part[root.start.clone()], b"<a>");
        assert_eq!(&part[root.child("b").unwrap().span()], b"<b/>");
        check_tiling(&root, &part).unwrap();
        assert_eq!(compare(&part), Ok(true));
        // A third mark is no mark: text before the root, kept by neither parser.
        let mut part = vec![0xEF, 0xBB, 0xBF, 0xEF, 0xBB, 0xBF, 0xEF, 0xBB, 0xBF];
        part.extend_from_slice(b"<a/>");
        assert_eq!(parse(&part).unwrap().start, 9..13);
        assert_eq!(compare(&part), Ok(true));
    }

    #[test]
    fn never_offers_a_written_name_it_could_not_read() {
        let root = parse(b"<\xffa \xffb='1'><w:p xmlns:w='urn:x'/></\xffa>").unwrap();
        assert_eq!(root.reusable_name(), None);
        assert_eq!(root.child("?:p").unwrap().reusable_name(), Some("w:p"));
        // A prefix the part bound to WordprocessingML, written in bytes that are not UTF-8: the
        // element is a `w:document`, but its written prefix cannot be written again.
        let part = [&b"<\xff:document xmlns:\xff='"[..], W.as_bytes(), b"'/>"].concat();
        let root = parse(&part).unwrap();
        assert_eq!((root.name.as_str(), root.reusable_name()), ("w:document", None));
        // A name that is written U+FFFD cannot be told from one that was not UTF-8: refused alike.
        let part = format!("<\u{FFFD}:document xmlns:\u{FFFD}='{W}'/>");
        assert_eq!(parse(part.as_bytes()).unwrap().reusable_name(), None);
    }

    #[test]
    fn caps_the_namespace_declarations_in_scope() {
        let declarations = |count: usize, from: usize| (from..from + count).map(|index| format!(" xmlns:p{index}='u{index}'")).collect::<String>();
        let started = std::time::Instant::now();
        // As many as are allowed, each resolution walking them all.
        let part = format!("<r{}>{}</r>", declarations(MAX_BINDINGS, 0), "<z:a/>".repeat(20_000));
        assert!(parse(part.as_bytes()).is_ok());
        assert_eq!(parse(format!("<r{}/>", declarations(MAX_BINDINGS + 1, 0)).as_bytes()), Err(DocxError::Limit(TOO_MANY_BINDINGS.into())));
        // In scope across nested elements, released when an element closes or is written empty.
        let half = MAX_BINDINGS / 2;
        let nested = format!("<r{}><s{}/></r>", declarations(half, 0), declarations(MAX_BINDINGS - half + 1, half));
        assert_eq!(parse(nested.as_bytes()), Err(DocxError::Limit(TOO_MANY_BINDINGS.into())));
        let siblings = format!(
            "<r>{}{}</r>",
            format!("<s{}/>", declarations(MAX_BINDINGS, 0)).repeat(50),
            format!("<s{}></s>", declarations(MAX_BINDINGS, 0)).repeat(50)
        );
        assert!(parse(siblings.as_bytes()).is_ok());
        // Far more than the cap is refused at once, never walked one resolution at a time.
        assert!(parse(format!("<r{}><a/></r>", declarations(100_000, 0)).as_bytes()).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "took {:?}", started.elapsed());
    }

    /// The tree as the desktop parser would have built it: text merged, references resolved.
    fn to_el(element: &PEl, bytes: &[u8]) -> crate::xml::El {
        let mut converted = crate::xml::El { name: element.name.clone(), attrs: element.attrs.clone(), kids: Vec::new() };
        let push = |kids: &mut Vec<crate::xml::Node>, text: &str| match kids.last_mut() {
            Some(crate::xml::Node::Text(previous)) => previous.push_str(text),
            _ => kids.push(crate::xml::Node::Text(text.to_string())),
        };
        for kid in &element.kids {
            match kid {
                PKid::El(child) => converted.kids.push(crate::xml::Node::El(to_el(child, bytes))),
                PKid::Text(range) => push(&mut converted.kids, std::str::from_utf8(&bytes[range.clone()]).unwrap()),
                PKid::Ref { ch: Some(ch), .. } => push(&mut converted.kids, &ch.to_string()),
                PKid::Ref { ch: None, .. } => {}
                PKid::CData(range) => push(&mut converted.kids, &String::from_utf8_lossy(&bytes[range.start + 9..range.end - 3])),
            }
        }
        converted
    }

    /// Every range of `element` and its descendants is what it claims to be, the children follow one
    /// another, and only comments and processing instructions lie between them.
    fn check_tiling(element: &PEl, bytes: &[u8]) -> Result<(), String> {
        let start = &bytes[element.start.clone()];
        let written = String::from_utf8_lossy(start.get(1..).unwrap_or_default()).into_owned();
        if !(start.starts_with(b"<") && start.ends_with(b">") && written.starts_with(element.raw_name.as_str())) {
            return Err(format!("start tag {written:?} of {}", element.raw_name));
        }
        let mut at = element.start.end;
        for kid in &element.kids {
            let range = match kid {
                PKid::El(child) => {
                    check_tiling(child, bytes)?;
                    child.span()
                }
                PKid::Text(range) => {
                    let text = &bytes[range.clone()];
                    if text.is_empty() || text.contains(&b'<') || text.contains(&b'&') {
                        return Err(format!("text {:?}", String::from_utf8_lossy(text)));
                    }
                    range.clone()
                }
                PKid::Ref { range, .. } => {
                    let text = &bytes[range.clone()];
                    if !(text.starts_with(b"&") && text.ends_with(b";")) {
                        return Err(format!("reference {:?}", String::from_utf8_lossy(text)));
                    }
                    range.clone()
                }
                PKid::CData(range) => {
                    let text = &bytes[range.clone()];
                    if !(text.starts_with(b"<![CDATA[") && text.ends_with(b"]]>")) {
                        return Err(format!("CDATA {:?}", String::from_utf8_lossy(text)));
                    }
                    range.clone()
                }
            };
            if range.start < at {
                return Err("children overlap".into());
            }
            only_comments(&bytes[at..range.start])?;
            at = range.end;
        }
        if let Some(end) = &element.end {
            let tag = &bytes[end.clone()];
            if !(tag.starts_with(b"</") && tag.ends_with(b">")) || end.start < at {
                return Err(format!("end tag {:?}", String::from_utf8_lossy(tag)));
            }
            only_comments(&bytes[at..end.start])?;
        }
        Ok(())
    }

    fn only_comments(mut gap: &[u8]) -> Result<(), String> {
        while !gap.is_empty() {
            let rest = if let Some(rest) = gap.strip_prefix(b"<!--") {
                let end = rest.windows(3).position(|window| window == b"-->").ok_or("unterminated comment")?;
                &rest[end + 3..]
            } else if let Some(rest) = gap.strip_prefix(b"<?") {
                let end = rest.windows(2).position(|window| window == b"?>").ok_or("unterminated instruction")?;
                &rest[end + 2..]
            } else {
                return Err(format!("bytes between children {:?}", String::from_utf8_lossy(gap)));
            };
            gap = rest;
        }
        Ok(())
    }

    /// Both parsers accept the part and read it alike, with every range in place (`Ok(true)`), or
    /// both refuse it (`Ok(false)`).
    fn compare(bytes: &[u8]) -> Result<bool, String> {
        match (crate::xml::parse(bytes), parse(bytes)) {
            (Ok(desktop), Ok(tree)) => {
                let converted = to_el(&tree, bytes);
                if converted != desktop {
                    return Err(format!("read otherwise:\n desktop {desktop:?}\n tree    {converted:?}"));
                }
                check_tiling(&tree, bytes).map(|()| true)
            }
            (Err(_), Err(_)) => Ok(false),
            (desktop, tree) => Err(format!("one refuses: desktop {:?}, tree {:?}", desktop.err(), tree.err())),
        }
    }

    #[test]
    fn reads_every_probed_case_as_the_desktop_parser_does() {
        // Each case with whether the desktop parser accepted it before the two shared a reading.
        let cases: Vec<(Vec<u8>, bool)> = vec![
            (b"\xef\xbb\xbf\xef\xbb\xbf<a><b/></a>".to_vec(), true),
            (b"\xef\xbb\xbf\xef\xbb\xbf\xef\xbb\xbf<a/>".to_vec(), true),
            (format!("\u{FEFF}<?xml version = '1.0'   encoding='UTF-8'\r\n standalone = \"yes\" ?>\r<w:document xmlns:w=\"{W}\"/>").into_bytes(), true),
            (format!("<w:document xmlns:w=\"{W}\">\r\n<w:p>a\rb\r\nc</w:p>\r</w:document>").into_bytes(), true),
            (format!("<w:document\nxmlns:w = '{W}'\r\n w:a = \"1\"\tw:b='2' ><w:p\n/></w:document>").into_bytes(), true),
            (format!("<w:d xmlns:w='{W}'><w:t><![CDATA[a]]b]]]>x<![CDATA[]]]]><![CDATA[]]>]]></w:t></w:d>").into_bytes(), true),
            (format!("<w:d xmlns:w='{W}'><!-- a -- b ---><w:p/><!----><!-- - --></w:d>").into_bytes(), true),
            (format!("<w:d xmlns:w='{W}'><?a?><?b c=\">\"?><w:p/><?x ?></w:d>").into_bytes(), true),
            (
                format!(
                    "<w:d xmlns:w='{W}'><w:t>&#65;&#x41;&#X41;&#0065;&#x00041;&#1114111;&#x10FFFF;&#0;&#xD800;&#xFFFFFFFF;&amp;&lt;&gt;&apos;&quot;&nbsp;&AMP;</w:t></w:d>"
                )
                .into_bytes(),
                true,
            ),
            (format!("<w:d xmlns:w='{W}'><w:p/><w:p></w:p><w:p ></w:p ><w:p\t/></w:d>").into_bytes(), true),
            (format!("<w:d xmlns:w='{W}'><x:p xmlns:x='{W}'><w:r xmlns:w='urn:other'><w:t/></w:r><w:r/></x:p><w:q/></w:d>").into_bytes(), true),
            (
                format!("<document xmlns='{W}' xmlns:mc='http://schemas.openxmlformats.org/markup-compatibility/2006' mc:Ignorable='w14 wp14'><body xmlns=''><p a='1' w:b='2'/></body><p/></document>").into_bytes(),
                true,
            ),
            (format!("<w:d xmlns:w='{W}' xmlns='urn:x' mc:Ignorable='w14'><p xmlns:xmlns2='z' xmlnsfoo='1' xmlns:w='{W}'/></w:d>").into_bytes(), true),
            ("<a>caf\u{e9} \u{1F600}\u{4E2D}&amp;\u{1F600}</a>".as_bytes().to_vec(), true),
            (b"<a>x &amp y</a>".to_vec(), false),
            (b"<a>x & y</a>".to_vec(), false),
            (b"<a>x &# ; y</a>".to_vec(), true),
            (b"<a>&#xZZ;</a>".to_vec(), true),
            (b"<a>&\xff;</a>".to_vec(), true),
            (b"<a b='&\xff;'/>".to_vec(), true),
            (b"<\xffa \xffb='1'>t</\xffa>".to_vec(), true),
            (b"<a>\xc3</a>".to_vec(), false),
            (b"\xff<a/>".to_vec(), true),
            (b"<!DOCTYPE a [<!ENTITY e \"x\">]><a>&e;</a>".to_vec(), true),
            (b"<a xmlns:p='u' xmlns:p='v'><p:b/></a>".to_vec(), true),
            (b"<a xmlns:xml='wrong'/>".to_vec(), false),
            (b"<a xmlns:xmlns='x'/>".to_vec(), false),
            (b"<a xmlns:p=''><p:b/></a>".to_vec(), true),
            (b"<a b='1' b='2' c d='3'/>".to_vec(), true),
            (b"<a b='1>".to_vec(), false),
            (b"<a><![CDATA[unterminated</a>".to_vec(), false),
            (b"<a><!-- unterminated</a>".to_vec(), false),
            (b"</x><a/>".to_vec(), false),
            (b"<a></b></c></a>".to_vec(), true),
            (b"<a/>trailing &bad garbage <<<".to_vec(), true),
            (b"   \r\n  <a>\r</a>".to_vec(), true),
        ];
        for (index, (case, accepted)) in cases.iter().enumerate() {
            assert_eq!(compare(case), Ok(*accepted), "case {index}: {:?}", String::from_utf8_lossy(case));
        }
    }

    /// Pieces of parts, well formed and not, that the generated parts are made of.
    const PIECES: &[&[u8]] = &[
        b"<a>",
        b"</a>",
        b"<w:p>",
        b"</w:p>",
        b"<x:p>",
        b"</x:p>",
        b"<p>",
        b"</p>",
        b"<a/>",
        b"<w:t/>",
        b"<q:r/>",
        b"<a xmlns:w='http://schemas.openxmlformats.org/wordprocessingml/2006/main'>",
        b"<w:p xmlns:w='urn:o'>",
        b"<p xmlns='http://schemas.openxmlformats.org/wordprocessingml/2006/main'>",
        b"<x:p xmlns:x='http://schemas.openxmlformats.org/wordprocessingml/2006/main' x:a='1' a='2'>",
        b"<a mc:Ignorable='w14' xmlns:mc='http://schemas.openxmlformats.org/markup-compatibility/2006'>",
        b"<p xmlns=''>",
        b"<a b=\"&amp;\" c='&#x41;'>",
        b"<a\n b = '1'\r\n/>",
        b"text",
        b" ",
        b"\r\n",
        b"\r",
        b"\xc3\xa9",
        b"\xf0\x9f\x98\x80",
        b"\xff",
        b"\xc3",
        b"&amp;",
        b"&#65;",
        b"&#X41;",
        b"&#0;",
        b"&#xD800;",
        b"&#x1F600;",
        b"&bogus;",
        b"&",
        b"&#;",
        b";",
        b"]]>",
        b"]]",
        b"<![CDATA[",
        b"<![CDATA[x]]]]>",
        b"<!-- c -- d -->",
        b"<!--",
        b"-->",
        b"<?pi x?>",
        b"<?xml version='1.0'?>",
        b"\xef\xbb\xbf",
        b"<",
        b">",
        b"/>",
        b"</",
        b"'",
        b"\"",
        b"=",
    ];

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 4_096, ..proptest::prelude::ProptestConfig::default() })]

        #[test]
        fn reads_generated_parts_as_the_desktop_parser_does(
            marks in 0..4usize,
            rooted in proptest::prelude::any::<bool>(),
            pieces in proptest::collection::vec(0..PIECES.len(), 1..30),
        ) {
            // Leading byte-order marks: one is stripped by the parser, one by its reader.
            let mut part = b"\xef\xbb\xbf".repeat(marks);
            if rooted {
                part.extend_from_slice(b"<root>");
            }
            for piece in pieces {
                part.extend_from_slice(PIECES[piece]);
            }
            let compared = compare(&part);
            proptest::prop_assert!(compared.is_ok(), "{:?}: {:?}", String::from_utf8_lossy(&part), compared);
        }
    }
}

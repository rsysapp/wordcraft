//! embed: a strict XML reading for [`super::inspect`]. The desktop parser forgives what Word would
//! refuse or read otherwise (mismatched end tags, unclosed elements, duplicate attributes, unknown
//! entities, unbound prefixes); an inspection must not, because two readers that disagree on a
//! part's content let a save show one thing and be checked as another. Anything not plainly
//! well-formed UTF-8 XML 1.0 is an error.
//!
//! Names are written `prefix:local` with the canonical prefix of a known namespace (the same
//! prefixes as [`crate::xml`]), `{uri}local` for any other namespace, and the bare local name for
//! none, so a producer's own prefixes never matter.

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use quick_xml::events::{BytesStart, Event};
use quick_xml::name::{QName, ResolveResult};
use quick_xml::reader::NsReader;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum XNode {
    El(XEl),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct XEl {
    pub name: Rc<str>,
    /// In document order, without namespace declarations; no two share a name.
    pub attrs: Vec<(Rc<str>, String)>,
    pub kids: Vec<XNode>,
}

impl XEl {
    pub(super) fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(key, _)| &**key == name).map(|(_, value)| value.as_str())
    }

    pub(super) fn els(&self) -> impl Iterator<Item = &XEl> {
        self.kids.iter().filter_map(|node| match node {
            XNode::El(el) => Some(el),
            XNode::Text(_) => None,
        })
    }

    /// The first child of this name.
    pub(super) fn child(&self, name: &str) -> Option<&XEl> {
        self.els().find(|el| &*el.name == name)
    }

    pub(super) fn has_element_kids(&self) -> bool {
        self.els().next().is_some()
    }

    /// The direct text, joined.
    pub(super) fn text(&self) -> String {
        let mut out = String::new();
        for node in &self.kids {
            if let XNode::Text(text) = node {
                out.push_str(text);
            }
        }
        out
    }
}

/// Element and attribute names, shared by every part of both packages.
#[derive(Default)]
pub(super) struct Names(HashMap<String, Rc<str>>);

impl Names {
    fn get(&mut self, name: String) -> Rc<str> {
        if let Some(found) = self.0.get(&name) {
            return found.clone();
        }
        let shared: Rc<str> = Rc::from(name.as_str());
        self.0.insert(name, shared.clone());
        shared
    }
}

/// What a package's parts may still spend: one element budget for all of them, and a depth.
pub(super) struct Budget {
    pub elements_left: usize,
    pub max_depth: usize,
}

/// Namespaces outside the WordprocessingML body set, with the prefixes the desktop parser uses.
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

fn qualified(namespace: &[u8], local: &str) -> Result<String, String> {
    for (prefix, uri) in crate::xml::NAMESPACES.iter().chain(OTHER_NAMESPACES) {
        if uri.as_bytes() == namespace {
            return Ok(format!("{prefix}:{local}"));
        }
    }
    let uri = std::str::from_utf8(namespace).map_err(|_| "a namespace that is not UTF-8".to_string())?;
    Ok(format!("{{{}}}{local}", escape(uri)))
}

fn resolved(result: ResolveResult<'_>, local: &[u8]) -> Result<String, String> {
    let local = std::str::from_utf8(local).map_err(|_| "a name that is not UTF-8".to_string())?;
    match result {
        ResolveResult::Bound(namespace) => qualified(namespace.as_ref(), local),
        ResolveResult::Unbound => Ok(local.to_string()),
        ResolveResult::Unknown(prefix) => Err(format!("an undeclared prefix {:?}", String::from_utf8_lossy(&prefix))),
    }
}

/// Attributes whose values name namespaces by prefix: their prefixes are read in scope, so a
/// rebound prefix cannot change their meaning unseen.
fn holds_prefixes(element: &str, attribute: &str) -> bool {
    matches!(attribute, "mc:Ignorable" | "mc:ProcessContent" | "mc:MustUnderstand" | "xsi:type")
        || (element == "mc:Choice" && attribute == "Requires")
}

/// Is `c` allowed in XML 1.0 content?
fn xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

fn checked(text: &str) -> Result<(), String> {
    if text.chars().all(xml_char) { Ok(()) } else { Err("a character XML does not allow".into()) }
}

/// XML 1.0 end-of-line handling (2.11): `\r\n` and a lone `\r` read as `\n`.
fn line_ends(raw: &str) -> String {
    raw.replace("\r\n", "\n").replace('\r', "\n")
}

fn entity(name: &str) -> Result<char, String> {
    if let Some(number) = name.strip_prefix('#') {
        let value = match number.strip_prefix('x') {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => number.parse::<u32>().ok(),
        };
        let character = value.and_then(char::from_u32).filter(|c| xml_char(*c));
        return character.ok_or_else(|| format!("a bad character reference &{name};"));
    }
    match name {
        "lt" => Ok('<'),
        "gt" => Ok('>'),
        "amp" => Ok('&'),
        "apos" => Ok('\''),
        "quot" => Ok('"'),
        _ => Err(format!("an unknown entity &{name};")),
    }
}

/// An attribute value as XML 1.0 reads it (3.3.3): line ends, then literal white space as spaces,
/// then references.
fn attribute_value(raw: &[u8]) -> Result<String, String> {
    let raw = std::str::from_utf8(raw).map_err(|_| "an attribute value that is not UTF-8".to_string())?;
    let spaced: String = line_ends(raw).chars().map(|c| if matches!(c, '\t' | '\n') { ' ' } else { c }).collect();
    let mut out = String::with_capacity(spaced.len());
    let mut rest = spaced.as_str();
    while let Some(at) = rest.find(['&', '<']) {
        out.push_str(rest.get(..at).unwrap_or(""));
        let tail = rest.get(at..).unwrap_or("");
        if tail.starts_with('<') {
            return Err("< in an attribute value".into());
        }
        let end = tail.find(';').ok_or("an unterminated reference")?;
        out.push(entity(tail.get(1..end).unwrap_or(""))?);
        rest = tail.get(end + 1..).unwrap_or("");
    }
    out.push_str(rest);
    checked(&out)?;
    Ok(out)
}

/// Parse a part. Elements count against `budget` (one budget across the package).
pub(super) fn parse(bytes: &[u8], names: &mut Names, budget: &mut Budget) -> Result<XEl, String> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
    let mut reader = NsReader::from_reader(bytes);
    {
        let config = reader.config_mut();
        config.trim_text_start = false;
        config.trim_text_end = false;
        config.expand_empty_elements = false;
        config.check_end_names = true;
        config.check_comments = true;
        config.allow_unmatched_ends = false;
        config.allow_dangling_amp = false;
    }
    let mut stack: Vec<XEl> = Vec::new();
    let mut root: Option<XEl> = None;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let (result, event) = reader.read_resolved_event_into(&mut buf).map_err(|error| format!("malformed XML: {error}"))?;
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                if root.is_some() {
                    return Err("content after the root element".into());
                }
                budget.elements_left = budget.elements_left.checked_sub(1).ok_or(super::TOO_MANY_ELEMENTS)?;
                let name = resolved(result, start.local_name().as_ref())?;
                let element = element(&reader, name, start, names)?;
                if matches!(event, Event::Start(_)) {
                    if stack.len() >= budget.max_depth {
                        return Err(super::TOO_DEEP.into());
                    }
                    stack.push(element);
                } else {
                    close(&mut stack, &mut root, element);
                }
            }
            Event::End(_) => {
                let element = stack.pop().ok_or("an end tag without a start")?;
                close(&mut stack, &mut root, element);
            }
            Event::Text(text) => {
                let text = text.xml10_content().map_err(|_| "text that is not UTF-8".to_string())?;
                checked(&text)?;
                push_text(&mut stack, &text)?;
            }
            Event::CData(data) => {
                let text = data.xml10_content().map_err(|_| "text that is not UTF-8".to_string())?;
                checked(&text)?;
                push_text(&mut stack, &text)?;
            }
            Event::GeneralRef(reference) => {
                let name = reference.decode().map_err(|_| "a reference that is not UTF-8".to_string())?;
                let character = entity(&name)?;
                push_text(&mut stack, character.encode_utf8(&mut [0; 4]))?;
            }
            Event::Decl(declaration) => {
                if let Some(encoding) = declaration.encoding() {
                    let encoding = encoding.map_err(|_| "a bad XML declaration".to_string())?;
                    if !encoding.eq_ignore_ascii_case(b"utf-8") {
                        return Err("an XML part not in UTF-8".into());
                    }
                }
            }
            Event::DocType(_) => return Err("a document type declaration".into()),
            Event::Comment(_) | Event::PI(_) => {}
            Event::Eof => break,
        }
    }
    if !stack.is_empty() {
        return Err("an element left open".into());
    }
    root.ok_or_else(|| "no root element".to_string())
}

fn element(reader: &NsReader<&[u8]>, name: String, start: &BytesStart<'_>, names: &mut Names) -> Result<XEl, String> {
    let mut attrs: Vec<(Rc<str>, String)> = Vec::new();
    let mut seen = BTreeSet::new();
    for attribute in start.attributes().with_checks(true) {
        let attribute = attribute.map_err(|error| format!("a malformed attribute: {error}"))?;
        let key = attribute.key.as_ref();
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            continue;
        }
        let (namespace, local) = reader.resolve_attribute(attribute.key);
        let key = resolved(namespace, local.as_ref())?;
        let mut value = attribute_value(&attribute.value)?;
        if holds_prefixes(&name, &key) {
            value = qualified_list(reader, &key, &value)?;
        }
        if !seen.insert(key.clone()) {
            return Err(format!("the attribute {key} twice"));
        }
        attrs.push((names.get(key), value));
    }
    Ok(XEl { name: names.get(name), attrs, kids: Vec::new() })
}

/// `p1 p2:local` with each prefix replaced by its namespace in scope (an unprefixed `xsi:type`
/// takes the default namespace).
fn qualified_list(reader: &NsReader<&[u8]>, attribute: &str, value: &str) -> Result<String, String> {
    let mut out = Vec::new();
    for token in value.split_whitespace() {
        let (prefix, local) = match token.split_once(':') {
            Some(parts) => parts,
            None if attribute == "xsi:type" => ("", token),
            None => (token, ""),
        };
        let probe = if prefix.is_empty() { "x".to_string() } else { format!("{prefix}:x") };
        let (namespace, _) = reader.resolve(QName(probe.as_bytes()), false);
        let uri = match namespace {
            ResolveResult::Bound(namespace) => {
                std::str::from_utf8(namespace.as_ref()).map_err(|_| "a namespace that is not UTF-8".to_string())?.to_string()
            }
            ResolveResult::Unbound => String::new(),
            ResolveResult::Unknown(_) => format!("?{prefix}"),
        };
        out.push(format!("{{{}}}{}", escape(&uri), escape(local)));
    }
    Ok(out.join(" "))
}

fn push_text(stack: &mut [XEl], text: &str) -> Result<(), String> {
    let Some(top) = stack.last_mut() else {
        // Outside the root only white space may stand.
        return if text.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r')) { Ok(()) } else { Err("text outside the root element".into()) };
    };
    if let Some(XNode::Text(previous)) = top.kids.last_mut() {
        previous.push_str(text);
    } else {
        top.kids.push(XNode::Text(text.to_string()));
    }
    Ok(())
}

fn close(stack: &mut [XEl], root: &mut Option<XEl>, mut element: XEl) {
    // A vector's first growth leaves room for four; most elements hold one child or none.
    element.kids.shrink_to_fit();
    element.attrs.shrink_to_fit();
    match stack.last_mut() {
        Some(parent) => parent.kids.push(XNode::El(element)),
        None => *root = Some(element),
    }
}

/// Escape for the canonical form, so distinct trees never print the same.
pub(super) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '{' => out.push_str("&#123;"),
            '}' => out.push_str("&#125;"),
            c => out.push(c),
        }
    }
    out
}

pub(super) fn is_space(text: &str) -> bool {
    text.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r'))
}

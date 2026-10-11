//! embed: a strict XML reading for [`super::inspect`]. The desktop parser forgives what Word would
//! refuse or read otherwise (mismatched end tags, unclosed elements, duplicate attributes, unknown
//! entities, unbound prefixes); an inspection must not, because two readers that disagree on a
//! part's content let a save show one thing and be checked as another. Anything not plainly
//! well-formed UTF-8 XML 1.0 is an error.
//!
//! Names are written `prefix:local` with the canonical prefix of a known namespace (the same
//! prefixes as [`crate::xml`]), `{uri}local` for any other namespace, and the bare local name for
//! none, so a producer's own prefixes never matter.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use quick_xml::events::{BytesStart, Event};
use quick_xml::name::{QName, ResolveResult};
use quick_xml::reader::NsReader;

use super::budget::{Budget, units};

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

/// Element and attribute names, shared by every part of both packages. A name in a namespace this
/// module does not know is written `{n}local`, `n` being the namespace's number here, so a long
/// namespace is kept once, never spelled out in each name. Everything kept is charged.
#[derive(Default)]
pub(super) struct Names {
    names: HashSet<Rc<str>>,
    namespaces: HashMap<Box<[u8]>, usize>,
}

/// The most distinct names one inspection keeps; no WordprocessingML package has a fraction of
/// them.
const MAX_NAMES: usize = 100_000;
/// Memory charged for each name or namespace kept, besides its bytes: the shared string and its
/// table entry.
const NAME_BYTES: u64 = 96;

impl Names {
    fn get(&mut self, name: &str, budget: &mut Budget) -> Result<Rc<str>, String> {
        // A lookup hashes and compares the name.
        budget.work(16 + units(name.len()) / 4)?;
        if let Some(found) = self.names.get(name) {
            return Ok(found.clone());
        }
        if self.names.len() >= MAX_NAMES {
            return Err("more distinct XML names than an inspection keeps".into());
        }
        budget.hold(NAME_BYTES.saturating_add(units(name.len())))?;
        let shared: Rc<str> = Rc::from(name);
        self.names.insert(shared.clone());
        Ok(shared)
    }

    /// The number of a namespace this module has no prefix for.
    fn namespace(&mut self, uri: &[u8], budget: &mut Budget) -> Result<usize, String> {
        budget.work(4 + units(uri.len()) / 4)?;
        if let Some(number) = self.namespaces.get(uri) {
            return Ok(*number);
        }
        if self.namespaces.len() >= MAX_NAMES {
            return Err("more distinct XML namespaces than an inspection keeps".into());
        }
        budget.hold(NAME_BYTES.saturating_add(units(uri.len())))?;
        let number = self.namespaces.len();
        self.namespaces.insert(uri.into(), number);
        Ok(number)
    }
}

/// What a package's parts may still spend: one element budget for all of them, and a depth.
pub(super) struct Limits {
    pub elements_left: usize,
    pub max_depth: usize,
}

/// Memory charged for each element of a parsed tree: its node in its parent's children, three times
/// (a growing vector holds up to twice its length, and its old room too while it moves; vectors are
/// shrunk to fit as elements close), and the allocator's share.
const ELEMENT_BYTES: u64 = 3 * std::mem::size_of::<XNode>() as u64 + 16;
/// Memory charged for each attribute, besides its value: its entry (three times, as for elements),
/// the reader's record of its key, and the duplicate check's.
const ATTRIBUTE_BYTES: u64 = 3 * std::mem::size_of::<(Rc<str>, String)>() as u64 + 64;
/// The longest element, attribute or namespace name read; a longer one is no WordprocessingML.
const MAX_NAME: usize = 4_096;

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

fn qualified(namespace: &[u8], local: &str, names: &mut Names, budget: &mut Budget) -> Result<String, String> {
    for (prefix, uri) in crate::xml::NAMESPACES.iter().chain(OTHER_NAMESPACES) {
        if uri.as_bytes() == namespace {
            return Ok(format!("{prefix}:{local}"));
        }
    }
    std::str::from_utf8(namespace).map_err(|_| "a namespace that is not UTF-8".to_string())?;
    Ok(format!("{{{}}}{local}", names.namespace(namespace, budget)?))
}

fn resolved(result: ResolveResult<'_>, local: &[u8], names: &mut Names, budget: &mut Budget) -> Result<String, String> {
    let local = std::str::from_utf8(local).map_err(|_| "a name that is not UTF-8".to_string())?;
    if local.len() > MAX_NAME || matches!(&result, ResolveResult::Bound(namespace) if namespace.as_ref().len() > MAX_NAME) {
        return Err("a name longer than any WordprocessingML name".into());
    }
    // Braces and colons are no name characters; refusing them keeps `{n}local` and `prefix:local`
    // apart from any local name.
    if local.is_empty() || local.contains(['{', '}', ':']) {
        return Err(format!("an element or attribute named {local:?}"));
    }
    // Resolving a prefix walks the bindings in scope and compares the namespace with the known ones.
    budget.work(8 + units(local.len()) / 4)?;
    match result {
        ResolveResult::Bound(namespace) => qualified(namespace.as_ref(), local, names, budget),
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
    // One pass into one string no longer than the raw value: line ends, then literal white space
    // as spaces (`\r\n` is one), then references.
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find(['&', '<', '\r', '\n', '\t']) {
        out.push_str(rest.get(..at).unwrap_or(""));
        let tail = rest.get(at..).unwrap_or("");
        if let Some(after) = tail.strip_prefix("\r\n") {
            out.push(' ');
            rest = after;
            continue;
        }
        match tail.chars().next() {
            Some('<') => return Err("< in an attribute value".into()),
            Some('&') => {
                let end = tail.find(';').ok_or("an unterminated reference")?;
                out.push(entity(tail.get(1..end).unwrap_or(""))?);
                rest = tail.get(end + 1..).unwrap_or("");
            }
            _ => {
                out.push(' ');
                rest = tail.get(1..).unwrap_or("");
            }
        }
    }
    out.push_str(rest);
    checked(&out)?;
    Ok(out)
}

/// A parsed part and the memory it holds (to release once the tree is dropped).
pub(super) struct Parsed {
    pub root: XEl,
    pub held: u64,
    /// At least as many as the items a walk of this tree can make: each character of text, and two
    /// marks for each element (a container's start and end).
    pub most_items: u64,
}

/// Parse a part. Elements count against `limits` (one element budget across the package); work
/// and the tree's memory against `budget`, charged as the tree grows.
pub(super) fn parse(bytes: &[u8], names: &mut Names, limits: &mut Limits, budget: &mut Budget) -> Result<Parsed, String> {
    let mut held: u64 = 0;
    let mut most_items: u64 = 4;
    let result = parse_into(bytes, names, limits, budget, &mut held, &mut most_items);
    match result {
        Ok(root) => Ok(Parsed { root, held, most_items }),
        Err(error) => {
            budget.release(held);
            Err(error)
        }
    }
}

/// How many items a text node can make: a character each inside a text element, else one mark.
fn item_bound(stack: &[XEl], length: usize) -> u64 {
    match stack.last().map(|top| &*top.name) {
        Some("w:t" | "w:delText" | "w:instrText" | "w:delInstrText") => super::budget::units(length),
        _ => 1,
    }
}

fn charge(budget: &mut Budget, held: &mut u64, bytes: u64) -> Result<(), String> {
    budget.hold(bytes)?;
    budget.work(1 + bytes / super::budget::BYTES_PER_UNIT_PARSED)?;
    *held = held.saturating_add(bytes);
    Ok(())
}

fn parse_into(
    bytes: &[u8],
    names: &mut Names,
    limits: &mut Limits,
    budget: &mut Budget,
    held: &mut u64,
    most_items: &mut u64,
) -> Result<XEl, String> {
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
    // What each open element's namespace declarations hold in the reader.
    let mut open_declarations: Vec<u64> = Vec::new();
    let mut root: Option<XEl> = None;
    loop {
        // Events borrow from the part's bytes: nothing is copied but the text kept.
        let (result, event) = reader.read_resolved_event().map_err(|error| format!("malformed XML: {error}"))?;
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                if root.is_some() {
                    return Err("content after the root element".into());
                }
                limits.elements_left = limits.elements_left.checked_sub(1).ok_or(super::TOO_MANY_ELEMENTS)?;
                let name = resolved(result, start.local_name().as_ref(), names, budget)?;
                charge(budget, held, ELEMENT_BYTES)?;
                let mut declarations = 0u64;
                let element = element(&reader, name, start, names, budget, held, &mut declarations)?;
                budget.hold(declarations)?;
                *most_items = most_items.saturating_add(2);
                if matches!(event, Event::Start(_)) {
                    if stack.len() >= limits.max_depth {
                        budget.release(declarations);
                        return Err(super::TOO_DEEP.into());
                    }
                    stack.push(element);
                    open_declarations.push(declarations);
                } else {
                    budget.release(declarations);
                    close(&mut stack, &mut root, element);
                }
            }
            Event::End(_) => {
                let element = stack.pop().ok_or("an end tag without a start")?;
                budget.release(open_declarations.pop().unwrap_or(0));
                close(&mut stack, &mut root, element);
            }
            Event::Text(text) => {
                let text = text.xml10_content().map_err(|_| "text that is not UTF-8".to_string())?;
                checked(&text)?;
                charge(budget, held, TEXT_FACTOR.saturating_mul(units(text.len())))?;
                *most_items = most_items.saturating_add(item_bound(&stack, text.len()));
                push_text(&mut stack, &text)?;
            }
            Event::CData(data) => {
                let text = data.xml10_content().map_err(|_| "text that is not UTF-8".to_string())?;
                checked(&text)?;
                charge(budget, held, TEXT_FACTOR.saturating_mul(units(text.len())))?;
                *most_items = most_items.saturating_add(item_bound(&stack, text.len()));
                push_text(&mut stack, &text)?;
            }
            Event::GeneralRef(reference) => {
                let name = reference.decode().map_err(|_| "a reference that is not UTF-8".to_string())?;
                let character = entity(&name)?;
                charge(budget, held, TEXT_FACTOR.saturating_mul(units(character.len_utf8())))?;
                *most_items = most_items.saturating_add(1);
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

/// Memory charged per byte of text kept: twice, as a text node built from pieces (text between
/// references) grows by doubling.
const TEXT_FACTOR: u64 = 2;

/// Memory charged for each namespace declaration while its element is open, besides its bytes: the
/// reader's binding record.
const DECLARATION_BYTES: u64 = 64;

/// The most attributes one element may carry; real ones carry a few dozen.
pub(super) const MAX_ATTRIBUTES: usize = 512;

fn element(
    reader: &NsReader<&[u8]>,
    name: String,
    start: &BytesStart<'_>,
    names: &mut Names,
    budget: &mut Budget,
    held: &mut u64,
    declarations: &mut u64,
) -> Result<XEl, String> {
    let mut attrs: Vec<(Rc<str>, String)> = Vec::new();
    // Names are interned, so one name is one pointer.
    let mut seen: HashSet<*const u8> = HashSet::new();
    let mut declared = 0usize;
    let mut list_bytes = 0usize;
    for attribute in start.attributes().with_checks(true) {
        let attribute = attribute.map_err(|error| format!("a malformed attribute: {error}"))?;
        if attrs.len().saturating_add(declared) >= MAX_ATTRIBUTES {
            return Err("an element with more attributes than any real one".into());
        }
        let key = attribute.key.as_ref();
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            // The reader keeps each declaration while its element is open (held by the caller
            // until the element closes); the tree keeps none.
            declared = declared.saturating_add(1);
            // The reader's binding buffer grows by doubling: twice the bytes.
            *declarations = declarations
                .saturating_add(DECLARATION_BYTES)
                .saturating_add(units(key.len().saturating_add(attribute.value.len())).saturating_mul(2));
            budget.work(4 + units(attribute.value.len()) / 4)?;
            continue;
        }
        // Held as it is read: its entry and the records the checks keep.
        charge(budget, held, ATTRIBUTE_BYTES)?;
        let (namespace, local) = reader.resolve_attribute(attribute.key);
        let key = resolved(namespace, local.as_ref(), names, budget)?;
        let value = if holds_prefixes(&name, &key) {
            // The raw value while it is expanded; the expansion is what is kept.
            let raw_bytes = units(attribute.value.len());
            budget.hold(raw_bytes)?;
            let raw = attribute_value(&attribute.value);
            let expanded = raw.and_then(|raw| qualified_list(reader, &key, &raw, names, budget, held, &mut list_bytes));
            budget.release(raw_bytes);
            expanded?
        } else {
            charge(budget, held, units(attribute.value.len()))?;
            attribute_value(&attribute.value)?
        };
        let key = names.get(&key, budget)?;
        if !seen.insert(Rc::as_ptr(&key).cast::<u8>()) {
            return Err(format!("the attribute {key} twice"));
        }
        attrs.push((key, value));
    }
    Ok(XEl { name: names.get(&name, budget)?, attrs, kids: Vec::new() })
}

/// The most names one qualified list holds, and the most bytes all of an element's lists may take
/// once expanded; real ones hold a few.
const MAX_LIST_NAMES: usize = 1_024;
const MAX_LIST_BYTES: usize = 64 * 1024;

/// `p1 p2:local` with each prefix replaced by its namespace in scope (an unprefixed `xsi:type`
/// takes the default namespace), written as element names are (`w:local`, `{n}local`), so a
/// long namespace costs its number, not its length. Each token's expansion is charged, as memory
/// and as work, before it is made; `spent` counts the element's expanded bytes so far.
fn qualified_list(
    reader: &NsReader<&[u8]>,
    attribute: &str,
    value: &str,
    names: &mut Names,
    budget: &mut Budget,
    held: &mut u64,
    spent: &mut usize,
) -> Result<String, String> {
    // First the length, so the expansion is charged and allocated once, at its size.
    let mut length = 0usize;
    for (count, token) in value.split_whitespace().enumerate() {
        if count >= MAX_LIST_NAMES {
            return Err(format!("a {attribute} list longer than any real one"));
        }
        length = length.saturating_add(qualified_token(reader, attribute, token, names, budget)?.len()).saturating_add(1);
        if spent.saturating_add(length) > MAX_LIST_BYTES {
            return Err(format!("a {attribute} list longer than any real one"));
        }
    }
    *spent = spent.saturating_add(length);
    // Kept in the tree with the attribute: held before it is made.
    charge(budget, held, units(length))?;
    let mut out = String::with_capacity(length);
    for token in value.split_whitespace() {
        out.push_str(&qualified_token(reader, attribute, token, names, budget)?);
        out.push(' ');
    }
    Ok(out)
}

/// One token of a prefix list, its prefix resolved in scope; charged as work.
fn qualified_token(reader: &NsReader<&[u8]>, attribute: &str, token: &str, names: &mut Names, budget: &mut Budget) -> Result<String, String> {
    let (prefix, local) = match token.split_once(':') {
        Some(parts) => parts,
        None if attribute == "xsi:type" => ("", token),
        None => (token, ""),
    };
    let probe = if prefix.is_empty() { "x".to_string() } else { format!("{prefix}:x") };
    budget.work(8 + units(token.len()) / 4)?;
    let (namespace, _) = reader.resolve(QName(probe.as_bytes()), false);
    match namespace {
        ResolveResult::Bound(namespace) => {
            let namespace = namespace.as_ref().to_vec();
            qualified(&namespace, local, names, budget)
        }
        ResolveResult::Unbound => Ok(format!(":{local}")),
        ResolveResult::Unknown(_) => Ok(format!("?{prefix}:{local}")),
    }
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

pub(super) fn is_space(text: &str) -> bool {
    text.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r'))
}

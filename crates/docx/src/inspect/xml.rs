//! embed: a strict XML reading for [`super::inspect`]. The desktop parser forgives what Word would
//! refuse or read otherwise (mismatched end tags, unclosed elements, duplicate attributes, unknown
//! entities, unbound prefixes); an inspection must not, because two readers that disagree on a
//! part's content let a save show one thing and be checked as another. Anything not plainly
//! well-formed UTF-8 XML 1.0 is an error.
//!
//! Names are written `prefix:local` with the canonical prefix of a known namespace (the same
//! prefixes as [`crate::xml`]), `{uri}local` for any other namespace, and the bare local name for
//! none, so a producer's own prefixes never matter.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use quick_xml::events::{BytesStart, Event};
use quick_xml::name::{NamespaceResolver, QName, ResolveResult};
use quick_xml::reader::Reader;

use super::budget::{Budget, shared_string, table_room, table_slot, units, vector_slot};

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

    /// The direct text, in pieces, without copying it.
    pub(super) fn texts(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.kids.iter().filter_map(|node| match node {
            XNode::Text(text) => Some(text.as_str()),
            XNode::El(_) => None,
        })
    }

    /// Whether the direct text is white space only (or none).
    pub(super) fn text_is_space(&self) -> bool {
        self.texts().all(is_space)
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
        // The shared string and its entry, and the table's first room with the first name.
        let room = if self.names.is_empty() { table_room::<Rc<str>>() } else { 0 };
        budget.hold(table_slot::<Rc<str>>().saturating_add(shared_string(name.len())).saturating_add(room))?;
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
        let room = if self.namespaces.is_empty() { table_room::<(Box<[u8]>, usize)>() } else { 0 };
        budget.hold(table_slot::<(Box<[u8]>, usize)>().saturating_add(units(uri.len())).saturating_add(room))?;
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

/// Memory charged for each node of a parsed tree (an element, or a text between elements): its slot
/// in its parent's children, a vector growing by doubling (vectors are shrunk to fit as elements
/// close), and the allocator's share.
const NODE_BYTES: u64 = vector_slot::<XNode>() + 16;
/// Memory charged for each attribute, besides its value: its entry in a vector growing by doubling.
const ATTRIBUTE_BYTES: u64 = vector_slot::<(Rc<str>, String)>();
/// Memory held while a part is parsed, whatever it holds: one element's working records (its
/// attributes' duplicate checks, by name and by key as written, the open elements' first rooms, a
/// name being written), each bounded by `MAX_ATTRIBUTES`, `MAX_NAME`, `MAX_ATTRIBUTE_KEY` and the
/// depth.
const PARSE_BYTES: u64 = 256 * 1024;
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
    // Comparing the namespace with the known ones (the walk to find it was charged before it ran).
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
    budget.hold(PARSE_BYTES)?;
    let mut held: u64 = 0;
    let mut most_items: u64 = 4;
    let mut room = ReaderRoom::default();
    let result = parse_into(bytes, names, limits, budget, &mut held, &mut most_items, &mut room);
    // The reader and its bindings are gone with the parse.
    budget.release(room.charged.saturating_add(PARSE_BYTES));
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

/// Memory charged for each record the reader keeps while elements are open: an open element's
/// start in its name buffer, a namespace binding.
const BINDING_BYTES: u64 = 48;

/// What the reader holds for the open elements, and the namespace resolver for the declarations in
/// scope: each open element's raw name, each declaration's prefix and namespace, and a record of
/// each. Their buffers grow by doubling and are never shrunk while the part is read, so the most
/// they ever held is charged (three times, for the doubling) until the part is read, before they
/// grow.
#[derive(Default)]
struct ReaderRoom {
    bytes: u64,
    records: u64,
    charged: u64,
}

impl ReaderRoom {
    /// Room for `bytes` and `records` more than are open now.
    fn reserve(&mut self, bytes: u64, records: u64, budget: &mut Budget) -> Result<(), String> {
        let bytes = self.bytes.saturating_add(bytes);
        let records = self.records.saturating_add(records);
        let wanted = bytes.saturating_add(records.saturating_mul(BINDING_BYTES)).saturating_mul(3);
        if wanted > self.charged {
            budget.hold(wanted - self.charged)?;
            self.charged = wanted;
        }
        Ok(())
    }
}

/// The length of the raw name of the start tag at `at`, if a start tag begins there: the reader
/// copies it into its buffer of open names before the tag is seen here.
fn start_name_length(bytes: &[u8], at: usize) -> Option<usize> {
    let rest = bytes.get(at..)?.strip_prefix(b"<")?;
    if matches!(rest.first(), None | Some(b'/' | b'!' | b'?')) {
        return None;
    }
    Some(rest.iter().position(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>')).unwrap_or(rest.len()))
}

/// The namespace declarations of a start tag: their bytes (prefix and namespace) and how many.
/// Every attribute's key is measured first: none may pass `MAX_ATTRIBUTE_KEY`, so nothing that
/// compares keys (the duplicate check, the resolver's declarations) works on longer ones.
fn declarations(start: &BytesStart<'_>) -> Result<(u64, u64), String> {
    let mut bytes = 0u64;
    let mut count = 0u64;
    for attribute in start.attributes().with_checks(false).flatten() {
        let key = attribute.key.as_ref();
        if key.len() > MAX_ATTRIBUTE_KEY {
            return Err("an attribute name longer than any WordprocessingML name".into());
        }
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            bytes = bytes.saturating_add(units(key.len().saturating_add(attribute.value.len())));
            count = count.saturating_add(1);
        }
    }
    Ok((bytes, count))
}

/// The longest attribute name, prefix included; a longer one is no WordprocessingML.
const MAX_ATTRIBUTE_KEY: usize = 256;

/// The most namespace declarations in scope at once; a WordprocessingML part declares a few dozen.
const MAX_BINDINGS_IN_SCOPE: u64 = 1_024;

/// Declarations in scope a resolution walks for one unit of work: it compares each one's prefix,
/// newest first, until one matches (0.5 to 0.9 ns each, measured).
const BINDINGS_PER_UNIT: u64 = 8;

/// The work of resolving a prefix (or an element's default namespace) with `in_scope` declarations
/// in scope, charged before it runs: at worst it walks them all (and the reserved `xml` binding).
fn walk(in_scope: u64, budget: &mut Budget) -> Result<(), String> {
    budget.work(1 + in_scope.saturating_add(2) / BINDINGS_PER_UNIT)
}

/// The most bytes of an element's raw name: a prefix and a local name each no longer than
/// `MAX_NAME`.
const MAX_RAW_NAME: usize = 2 * MAX_NAME + 1;

fn parse_into(
    bytes: &[u8],
    names: &mut Names,
    limits: &mut Limits,
    budget: &mut Budget,
    held: &mut u64,
    most_items: &mut u64,
    room: &mut ReaderRoom,
) -> Result<XEl, String> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
    let mut reader = Reader::from_reader(bytes);
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
    // The namespaces in scope, kept here (not by the reader) so each declaration is charged before
    // it is kept.
    let mut resolver = NamespaceResolver::default();
    room.reserve(256, 2, budget)?;
    let mut stack: Vec<XEl> = Vec::new();
    // What each open element holds in the reader's room: its name and declarations, and records.
    let mut open: Vec<(u64, u64)> = Vec::new();
    // Namespace declarations in scope: what a resolution may walk.
    let mut in_scope: u64 = 0;
    let mut root: Option<XEl> = None;
    loop {
        let at = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let name_length = start_name_length(bytes, at);
        if let Some(length) = name_length {
            if length > MAX_RAW_NAME {
                return Err("a name longer than any WordprocessingML name".into());
            }
            room.reserve(units(length), 1, budget)?;
        }
        // Events borrow from the part's bytes: nothing is copied but the text kept.
        let event = reader.read_event().map_err(|error| format!("malformed XML: {error}"))?;
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                if root.is_some() {
                    return Err("content after the root element".into());
                }
                limits.elements_left = limits.elements_left.checked_sub(1).ok_or(super::TOO_MANY_ELEMENTS)?;
                let raw_name = units(start.name().as_ref().len());
                if name_length.is_none() || start.name().as_ref().len() > MAX_RAW_NAME {
                    return Err("a start tag the reader saw otherwise".into());
                }
                let (declared, bindings) = declarations(start)?;
                in_scope = in_scope.saturating_add(bindings);
                if in_scope > MAX_BINDINGS_IN_SCOPE {
                    return Err("more namespace declarations in scope than an inspection reads".into());
                }
                let own = (raw_name.saturating_add(declared), bindings.saturating_add(1));
                room.reserve(own.0, own.1, budget)?;
                resolver.push(start).map_err(|error| format!("malformed XML: {error}"))?;
                walk(in_scope, budget)?;
                let (result, local) = resolver.resolve_element(start.name());
                let name = resolved(result, local.as_ref(), names, budget)?;
                charge(budget, held, NODE_BYTES)?;
                let element = element(&resolver, in_scope, name, start, names, budget, held)?;
                *most_items = most_items.saturating_add(2);
                if matches!(event, Event::Start(_)) {
                    if stack.len() >= limits.max_depth {
                        return Err(super::TOO_DEEP.into());
                    }
                    stack.push(element);
                    room.bytes = room.bytes.saturating_add(own.0);
                    room.records = room.records.saturating_add(own.1);
                    open.push(own);
                } else {
                    resolver.pop();
                    in_scope = in_scope.saturating_sub(bindings);
                    close(&mut stack, &mut root, element);
                }
            }
            Event::End(_) => {
                let element = stack.pop().ok_or("an end tag without a start")?;
                resolver.pop();
                let (bytes, records) = open.pop().unwrap_or((0, 0));
                room.bytes = room.bytes.saturating_sub(bytes);
                room.records = room.records.saturating_sub(records);
                // An element's records are its declarations and itself.
                in_scope = in_scope.saturating_sub(records.saturating_sub(1));
                close(&mut stack, &mut root, element);
            }
            Event::Text(text) => {
                // Text with line ends to normalise is decoded into a copy as long as the raw text:
                // held while it is read.
                let raw = units(text.len());
                budget.hold(raw)?;
                let pushed = (|| {
                    let text = text.xml10_content().map_err(|_| "text that is not UTF-8".to_string())?;
                    checked(&text)?;
                    *most_items = most_items.saturating_add(item_bound(&stack, text.len()));
                    push_text(&mut stack, text, budget, held)
                })();
                budget.release(raw);
                pushed?;
            }
            Event::CData(data) => {
                let raw = units(data.len());
                budget.hold(raw)?;
                let pushed = (|| {
                    let text = data.xml10_content().map_err(|_| "text that is not UTF-8".to_string())?;
                    checked(&text)?;
                    *most_items = most_items.saturating_add(item_bound(&stack, text.len()));
                    push_text(&mut stack, text, budget, held)
                })();
                budget.release(raw);
                pushed?;
            }
            Event::GeneralRef(reference) => {
                let name = reference.decode().map_err(|_| "a reference that is not UTF-8".to_string())?;
                let character = entity(&name)?;
                *most_items = most_items.saturating_add(1);
                push_text(&mut stack, Cow::Borrowed(character.encode_utf8(&mut [0; 4])), budget, held)?;
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

/// The most attributes one element may carry; real ones carry a few dozen.
pub(super) const MAX_ATTRIBUTES: usize = 512;

fn element(
    resolver: &NamespaceResolver,
    in_scope: u64,
    name: String,
    start: &BytesStart<'_>,
    names: &mut Names,
    budget: &mut Budget,
    held: &mut u64,
) -> Result<XEl, String> {
    let mut attrs: Vec<(Rc<str>, String)> = Vec::new();
    // Names are interned, so one name is one pointer.
    let mut seen: HashSet<*const u8> = HashSet::new();
    // The keys as written, each at most `MAX_ATTRIBUTE_KEY` bytes: a key written twice is refused
    // through a set, not by comparing every pair (the reader's own check).
    let mut written: HashSet<&[u8]> = HashSet::new();
    let mut declared = 0usize;
    let mut list_bytes = 0usize;
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute.map_err(|error| format!("a malformed attribute: {error}"))?;
        if attrs.len().saturating_add(declared) >= MAX_ATTRIBUTES {
            return Err("an element with more attributes than any real one".into());
        }
        let key = attribute.key.into_inner();
        if key.len() > MAX_ATTRIBUTE_KEY {
            return Err("an attribute name longer than any WordprocessingML name".into());
        }
        budget.work(1 + units(key.len()) / 8)?;
        if !written.insert(key) {
            return Err(format!("the attribute {} twice", String::from_utf8_lossy(key)));
        }
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            // The resolver keeps each declaration while its element is open, charged to the
            // reader's room; the tree keeps none.
            declared = declared.saturating_add(1);
            budget.work(4 + units(attribute.value.len()) / 4)?;
            continue;
        }
        // Held as it is read: its entry.
        charge(budget, held, ATTRIBUTE_BYTES)?;
        // Only a prefixed attribute name walks the declarations; an unprefixed one has no namespace.
        if key.contains(&b':') {
            walk(in_scope, budget)?;
        }
        let (namespace, local) = resolver.resolve_attribute(attribute.key);
        let key = resolved(namespace, local.as_ref(), names, budget)?;
        let value = if holds_prefixes(&name, &key) {
            // The raw value while it is expanded; the expansion is what is kept.
            let raw_bytes = units(attribute.value.len());
            budget.hold(raw_bytes)?;
            let raw = attribute_value(&attribute.value);
            let lists = Lists { resolver, in_scope, attribute: &key };
            let expanded = raw.and_then(|raw| qualified_list(&lists, &raw, names, budget, held, &mut list_bytes));
            budget.release(raw_bytes);
            expanded?
        } else {
            // Read into a string with room for the raw value, no more.
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
/// What a prefix list is read against: the declarations in scope (and how many), and the
/// attribute it is the value of.
struct Lists<'r, 'k> {
    resolver: &'r NamespaceResolver,
    in_scope: u64,
    attribute: &'k str,
}

fn qualified_list(
    lists: &Lists<'_, '_>,
    value: &str,
    names: &mut Names,
    budget: &mut Budget,
    held: &mut u64,
    spent: &mut usize,
) -> Result<String, String> {
    let attribute = lists.attribute;
    // First the length, so the expansion is charged and allocated once, at its size.
    let mut length = 0usize;
    for (count, token) in value.split_whitespace().enumerate() {
        if count >= MAX_LIST_NAMES {
            return Err(format!("a {attribute} list longer than any real one"));
        }
        length = length.saturating_add(qualified_token(lists, token, names, budget)?.len()).saturating_add(1);
        if spent.saturating_add(length) > MAX_LIST_BYTES {
            return Err(format!("a {attribute} list longer than any real one"));
        }
    }
    *spent = spent.saturating_add(length);
    // Kept in the tree with the attribute: held before it is made.
    charge(budget, held, units(length))?;
    let mut out = String::with_capacity(length);
    for token in value.split_whitespace() {
        out.push_str(&qualified_token(lists, token, names, budget)?);
        out.push(' ');
    }
    Ok(out)
}

/// One token of a prefix list, its prefix resolved in scope; charged as work.
fn qualified_token(lists: &Lists<'_, '_>, token: &str, names: &mut Names, budget: &mut Budget) -> Result<String, String> {
    let (prefix, local) = match token.split_once(':') {
        Some(parts) => parts,
        None if lists.attribute == "xsi:type" => ("", token),
        None => (token, ""),
    };
    let probe = if prefix.is_empty() { "x".to_string() } else { format!("{prefix}:x") };
    budget.work(8 + units(token.len()) / 4)?;
    walk(lists.in_scope, budget)?;
    let (namespace, _) = lists.resolver.resolve(QName(probe.as_bytes()), true);
    match namespace {
        ResolveResult::Bound(namespace) => {
            let namespace = namespace.as_ref().to_vec();
            qualified(&namespace, local, names, budget)
        }
        ResolveResult::Unbound => Ok(format!(":{local}")),
        ResolveResult::Unknown(_) => Ok(format!("?{prefix}:{local}")),
    }
}

/// Text kept in the tree, its room charged before it is made: a new text node (its slot in its
/// parent's children, and its characters, kept at their length), or more characters for the text
/// node before it (text between references, comments or CDATA), whose room grows by doubling with
/// the old room held beside the new while it moves.
fn push_text(stack: &mut [XEl], text: Cow<'_, str>, budget: &mut Budget, held: &mut u64) -> Result<(), String> {
    let Some(top) = stack.last_mut() else {
        // Outside the root only white space may stand.
        return if is_space(&text) { Ok(()) } else { Err("text outside the root element".into()) };
    };
    if let Some(XNode::Text(previous)) = top.kids.last_mut() {
        let needed = previous.len().saturating_add(text.len());
        let old = previous.capacity();
        if needed > old {
            let grown = needed.max(old.saturating_mul(2));
            charge(budget, held, units(grown))?;
            previous.reserve_exact(grown.saturating_sub(previous.len()));
            budget.release(units(old));
            *held = held.saturating_sub(units(old));
        }
        previous.push_str(&text);
    } else {
        // A decoded copy is moved into the node as it is; borrowed text is copied at its length.
        let length = match &text {
            Cow::Owned(owned) => owned.capacity(),
            Cow::Borrowed(borrowed) => borrowed.len(),
        };
        charge(budget, held, NODE_BYTES.saturating_add(units(length)))?;
        top.kids.push(XNode::Text(text.into_owned()));
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

//! embed: a story part (the main document, a header, a footer, the notes) as a stream of characters
//! and marks, each with the tracked changes around it and its formatting.
//!
//! The walker descends only into elements whose meaning it knows: blocks, paragraphs, runs,
//! tables, content controls, hyperlinks and tracked changes. Every other element is one opaque
//! mark carrying its whole canonical XML, so a change inside it is a change of the mark, never
//! skipped (Ruling W1-1). A tracked-change wrapper holding something other than runs and range
//! markers is refused rather than guessed at.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use super::AuthorRevisions;
use super::opc::Canon;
use super::xml::{XEl, XNode, escape, is_space};

/// Pieces at or above this are marks (`MARK_BASE + index`); below it, characters.
pub(super) const MARK_BASE: u32 = 0x0011_0000;

/// A character or mark, with its tracked-change wrappers and formatting (ids in [`Tables`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Item {
    pub piece: u32,
    pub wraps: u32,
    pub format: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum WrapKind {
    Ins,
    Del,
    MoveTo,
    MoveFrom,
    RowIns,
    RowDel,
    CellIns,
    CellDel,
}

impl WrapKind {
    /// Content that goes when the change is rejected (and stays plain when it is accepted).
    pub(super) fn inserts(self) -> bool {
        matches!(self, WrapKind::Ins | WrapKind::MoveTo | WrapKind::RowIns | WrapKind::CellIns)
    }

    fn of_run_level(name: &str) -> Option<WrapKind> {
        match name {
            "w:ins" => Some(WrapKind::Ins),
            "w:del" => Some(WrapKind::Del),
            "w:moveTo" => Some(WrapKind::MoveTo),
            "w:moveFrom" => Some(WrapKind::MoveFrom),
            _ => None,
        }
    }
}

/// A tracked change around content: its kind, author, date, and anything else on the element.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Wrap {
    pub kind: WrapKind,
    pub author: String,
    pub date: String,
    pub extra: String,
}

/// A tracked formatting change: who, when, and the properties before it (a string id).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct FormatRev {
    pub author: String,
    pub date: String,
    pub extra: String,
    pub old: u32,
}

/// One kind of properties (`rPr`, `pPr`…): its canonical value (a string id, 0 for none) and its
/// tracked change, if any.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Slot {
    pub name: &'static str,
    pub value: u32,
    pub rev: Option<FormatRev>,
}

/// Every property slot of an item that is not empty, sorted by name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct Format {
    pub slots: Vec<Slot>,
}

impl Format {
    /// A slot's value and change; an absent slot is empty.
    pub(super) fn slot(&self, name: &str) -> (u32, Option<&FormatRev>) {
        self.slots.iter().find(|slot| slot.name == name).map(|slot| (slot.value, slot.rev.as_ref())).unwrap_or((EMPTY, None))
    }
}

pub(super) struct MarkInfo {
    /// The element's name, for a loss report.
    pub element: Rc<str>,
    /// The comment a comment anchor (`w:commentRangeStart`, `…End`, `w:commentReference`) is for.
    pub comment: Option<String>,
}

/// The empty string (and the empty format and wrapper list): id 0 in each table.
pub(super) const EMPTY: u32 = 0;

/// Everything items point at, shared by both packages so equal ids mean equal content.
pub(super) struct Tables {
    string_ids: HashMap<Rc<str>, u32>,
    strings: Vec<Rc<str>>,
    mark_ids: HashMap<Rc<str>, u32>,
    pub marks: Vec<MarkInfo>,
    wrap_ids: HashMap<Vec<Wrap>, u32>,
    pub wraps: Vec<Vec<Wrap>>,
    format_ids: HashMap<Format, u32>,
    pub formats: Vec<Format>,
    /// An id was asked for past the last one; the inspection must not stand.
    overflow: bool,
}

impl Tables {
    pub(super) fn new() -> Tables {
        let mut tables = Tables {
            string_ids: HashMap::new(),
            strings: Vec::new(),
            mark_ids: HashMap::new(),
            marks: Vec::new(),
            wrap_ids: HashMap::new(),
            wraps: Vec::new(),
            format_ids: HashMap::new(),
            formats: Vec::new(),
            overflow: false,
        };
        tables.string("");
        tables.wrap_list(Vec::new());
        tables.format(Format::default());
        tables
    }

    fn next_id(len: usize) -> Result<u32, String> {
        u32::try_from(len).ok().filter(|id| *id < u32::MAX >> 1).ok_or_else(|| "too many distinct pieces".to_string())
    }

    /// The next id, or the last one with the overflow recorded (the caller refuses the inspection).
    fn next_or_flag(&mut self, len: usize) -> u32 {
        Tables::next_id(len).unwrap_or_else(|_| {
            self.overflow = true;
            u32::MAX
        })
    }

    pub(super) fn overflowed(&self) -> bool {
        self.overflow
    }

    pub(super) fn string(&mut self, text: &str) -> u32 {
        if let Some(id) = self.string_ids.get(text) {
            return *id;
        }
        let id = self.next_or_flag(self.strings.len());
        let shared: Rc<str> = Rc::from(text);
        self.strings.push(shared.clone());
        self.string_ids.insert(shared, id);
        id
    }

    pub(super) fn mark(&mut self, text: &str, element: &Rc<str>, comment: Option<String>) -> Result<u32, String> {
        let id = match self.mark_ids.get(text) {
            Some(id) => *id,
            None => {
                let id = Tables::next_id(self.marks.len())?;
                self.marks.push(MarkInfo { element: element.clone(), comment });
                self.mark_ids.insert(Rc::from(text), id);
                id
            }
        };
        MARK_BASE.checked_add(id).ok_or_else(|| "too many distinct marks".to_string())
    }

    pub(super) fn wrap_list(&mut self, wraps: Vec<Wrap>) -> u32 {
        if let Some(id) = self.wrap_ids.get(&wraps) {
            return *id;
        }
        let id = self.next_or_flag(self.wraps.len());
        self.wraps.push(wraps.clone());
        self.wrap_ids.insert(wraps, id);
        id
    }

    pub(super) fn format(&mut self, format: Format) -> u32 {
        if let Some(id) = self.format_ids.get(&format) {
            return *id;
        }
        let id = self.next_or_flag(self.formats.len());
        self.formats.push(format.clone());
        self.format_ids.insert(format, id);
        id
    }

    pub(super) fn mark_info(&self, piece: u32) -> Option<&MarkInfo> {
        let index = piece.checked_sub(MARK_BASE)?;
        self.marks.get(usize::try_from(index).ok()?)
    }

    pub(super) fn wraps_of(&self, id: u32) -> &[Wrap] {
        usize::try_from(id).ok().and_then(|index| self.wraps.get(index)).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(super) fn format_of(&self, id: u32) -> Option<&Format> {
        usize::try_from(id).ok().and_then(|index| self.formats.get(index))
    }
}

#[derive(Clone, Copy)]
pub(super) enum RevisionKind {
    Insertion,
    Deletion,
    Format,
}

/// Every tracked change in a package, by author and date.
#[derive(Default)]
pub(super) struct Revisions {
    pub by_pair: BTreeMap<(String, String), AuthorRevisions>,
}

impl Revisions {
    fn note(&mut self, author: &str, date: &str, kind: RevisionKind) {
        let entry = self
            .by_pair
            .entry((author.to_string(), date.to_string()))
            .or_insert_with(|| AuthorRevisions { author: author.to_string(), ..AuthorRevisions::default() });
        match kind {
            RevisionKind::Insertion => entry.insertions = entry.insertions.saturating_add(1),
            RevisionKind::Deletion => entry.deletions = entry.deletions.saturating_add(1),
            RevisionKind::Format => entry.format_changes = entry.format_changes.saturating_add(1),
        }
    }
}

/// Range markers that may stand inside a tracked change beside its runs.
const RANGE_MARKERS: &[&str] = &[
    "w:bookmarkStart",
    "w:bookmarkEnd",
    "w:commentRangeStart",
    "w:commentRangeEnd",
    "w:permStart",
    "w:permEnd",
    "w:moveFromRangeStart",
    "w:moveFromRangeEnd",
    "w:moveToRangeStart",
    "w:moveToRangeEnd",
    "w:proofErr",
];

#[derive(Clone, Copy)]
enum Context {
    Block,
    Inline,
}

/// Walk a story part's tree into items.
pub(super) fn walk(
    root: &XEl,
    canon: &Canon<'_>,
    tables: &mut Tables,
    revisions: &mut Revisions,
    items_left: &mut usize,
) -> Result<Vec<Item>, String> {
    let mut walker = Walker { canon, tables, revisions, items: Vec::new(), stack: Vec::new(), stack_id: Some(EMPTY), items_left };
    walker.root(root)?;
    Ok(walker.items)
}

struct Walker<'a, 'c> {
    canon: &'a Canon<'c>,
    tables: &'a mut Tables,
    revisions: &'a mut Revisions,
    items: Vec<Item>,
    /// The tracked changes around the current position, outermost first.
    stack: Vec<Wrap>,
    /// The stack's id in the tables, until it changes.
    stack_id: Option<u32>,
    items_left: &'a mut usize,
}

/// The first child of this name, by identity, so a second one is never mistaken for it.
fn first<'e>(el: &'e XEl, name: &str) -> Option<&'e XEl> {
    el.child(name)
}

fn is(child: &XEl, other: Option<&XEl>) -> bool {
    other.is_some_and(|other| std::ptr::eq(child, other))
}

fn empty_marker(el: &XEl) -> bool {
    matches!(&*el.name, "w:proofErr" | "w:lastRenderedPageBreak") && el.kids.is_empty()
}

impl Walker<'_, '_> {
    fn push(&mut self, piece: u32, format: u32) -> Result<(), String> {
        *self.items_left = self.items_left.checked_sub(1).ok_or("more text than an inspection reads")?;
        let wraps = match self.stack_id {
            Some(id) => id,
            None => {
                let id = self.tables.wrap_list(self.stack.clone());
                self.stack_id = Some(id);
                id
            }
        };
        self.items.push(Item { piece, wraps, format });
        Ok(())
    }

    fn push_wrap(&mut self, wrap: Wrap) {
        self.stack.push(wrap);
        self.stack_id = None;
    }

    fn truncate(&mut self, depth: usize) {
        self.stack.truncate(depth);
        self.stack_id = None;
    }

    fn mark(&mut self, text: &str, element: &Rc<str>, comment: Option<String>, format: u32) -> Result<(), String> {
        let piece = self.tables.mark(text, element, comment)?;
        self.push(piece, format)
    }

    /// An element compared whole.
    fn opaque(&mut self, el: &XEl, format: u32) -> Result<(), String> {
        let comment = matches!(&*el.name, "w:commentRangeStart" | "w:commentRangeEnd" | "w:commentReference")
            .then(|| el.attr("w:id").unwrap_or("").to_string());
        let text = self.canon.string(el);
        self.mark(&text, &el.name, comment, format)
    }

    /// Text where only elements belong: white space is layout, anything else is kept as a mark.
    fn stray(&mut self, text: &str) -> Result<(), String> {
        if is_space(text) {
            return Ok(());
        }
        let name: Rc<str> = Rc::from("#text");
        self.mark(&format!("#text:{}", escape(text)), &name, None, EMPTY)
    }

    fn start(&mut self, el: &XEl, extra: &[Option<&XEl>], format: u32) -> Result<(), String> {
        let mut text = String::new();
        self.canon.start_tag(el, &mut text);
        for property in extra.iter().flatten() {
            self.canon.element(property, &mut text);
        }
        self.mark(&text, &el.name, None, format)
    }

    fn end(&mut self, el: &XEl) -> Result<(), String> {
        self.mark(&format!("</{}>", escape(&el.name)), &el.name, None, EMPTY)
    }

    fn note(&mut self, el: &XEl, kind: RevisionKind) -> Wrap {
        let author = el.attr("w:author").unwrap_or("").to_string();
        let date = el.attr("w:date").unwrap_or("").to_string();
        self.revisions.note(&author, &date, kind);
        let mut extra = String::new();
        self.canon.start_tag(el, &mut extra);
        Wrap { kind: WrapKind::Ins, author, date, extra }
    }

    fn wrap_of(&mut self, el: &XEl, kind: WrapKind) -> Wrap {
        let revision = if kind.inserts() { RevisionKind::Insertion } else { RevisionKind::Deletion };
        Wrap { kind, ..self.note(el, revision) }
    }

    /// The canonical value of a property element without `skip` children, as a string id; empty
    /// properties read as none.
    fn value(&mut self, property: Option<&XEl>, skip: &dyn Fn(&XEl) -> bool) -> u32 {
        let Some(property) = property else { return EMPTY };
        let text = self.canon.filtered_string(property, &|child| !skip(child));
        if text == format!("<{0}></{0}>", escape(&property.name)) { EMPTY } else { self.tables.string(&text) }
    }

    /// A property slot: `property` without its tracked change (`change_name`, whose `inner` child
    /// holds the properties before it) and without the `skip` children.
    fn slot(&mut self, name: &'static str, property: Option<&XEl>, change_name: &str, inner: &str, skip: &dyn Fn(&XEl) -> bool) -> Option<Slot> {
        let change = property.and_then(|property| first(property, change_name));
        let value = self.value(property, &|child| is(child, change) || skip(child));
        let rev = change.map(|change| {
            let old_properties = first(change, inner);
            let old = self.value(old_properties, &|_| false);
            let mut extra = String::new();
            self.canon.filtered(change, &|child| !is(child, old_properties), &mut extra);
            let noted = self.note(change, RevisionKind::Format);
            FormatRev { author: noted.author, date: noted.date, extra, old }
        });
        (value != EMPTY || rev.is_some()).then_some(Slot { name, value, rev })
    }

    fn format(&mut self, mut slots: Vec<Slot>) -> u32 {
        slots.sort_by_key(|slot| slot.name);
        self.tables.format(Format { slots })
    }

    fn root(&mut self, root: &XEl) -> Result<(), String> {
        match &*root.name {
            "w:document" => {
                self.start(root, &[], EMPTY)?;
                for node in &root.kids {
                    match node {
                        XNode::El(child) if &*child.name == "w:body" => {
                            self.start(child, &[], EMPTY)?;
                            self.children(child, Context::Block, &[])?;
                        }
                        XNode::El(child) => self.opaque(child, EMPTY)?,
                        XNode::Text(text) => self.stray(text)?,
                    }
                }
                Ok(())
            }
            "w:hdr" | "w:ftr" => {
                self.start(root, &[], EMPTY)?;
                self.children(root, Context::Block, &[])
            }
            "w:footnotes" | "w:endnotes" => {
                self.start(root, &[], EMPTY)?;
                for node in &root.kids {
                    match node {
                        XNode::El(child) if matches!(&*child.name, "w:footnote" | "w:endnote") => {
                            self.start(child, &[], EMPTY)?;
                            self.children(child, Context::Block, &[])?;
                            self.end(child)?;
                        }
                        XNode::El(child) => self.opaque(child, EMPTY)?,
                        XNode::Text(text) => self.stray(text)?,
                    }
                }
                Ok(())
            }
            other => Err(format!("a story part whose root is {other}")),
        }
    }

    /// The children of `el` in `context`, except the property elements in `skip`.
    fn children(&mut self, el: &XEl, context: Context, skip: &[Option<&XEl>]) -> Result<(), String> {
        for node in &el.kids {
            match node {
                XNode::El(child) if skip.iter().any(|skipped| is(child, *skipped)) => {}
                XNode::El(child) => match context {
                    Context::Block => self.block(child)?,
                    Context::Inline => self.inline(child)?,
                },
                XNode::Text(text) => self.stray(text)?,
            }
        }
        Ok(())
    }

    fn block(&mut self, el: &XEl) -> Result<(), String> {
        match &*el.name {
            "w:p" => self.paragraph(el),
            "w:tbl" => self.table(el),
            "w:sdt" => self.sdt(el, Context::Block),
            "w:customXml" => self.container(el, Some("w:customXmlPr"), Context::Block),
            "w:sectPr" => {
                let slot = self.slot("sectPr", Some(el), "w:sectPrChange", "w:sectPr", &|_| false);
                let format = self.format(slot.into_iter().collect());
                let name: Rc<str> = Rc::from("w:sectPr");
                self.mark("§w:sectPr", &name, None, format)
            }
            _ if empty_marker(el) => Ok(()),
            _ => self.opaque(el, EMPTY),
        }
    }

    fn inline(&mut self, el: &XEl) -> Result<(), String> {
        match &*el.name {
            "w:r" => self.run(el),
            "w:ins" | "w:del" | "w:moveFrom" | "w:moveTo" => self.wrap(el),
            "w:hyperlink" | "w:fldSimple" | "w:dir" | "w:bdo" => self.container(el, None, Context::Inline),
            "w:smartTag" => self.container(el, Some("w:smartTagPr"), Context::Inline),
            "w:customXml" => self.container(el, Some("w:customXmlPr"), Context::Inline),
            "w:sdt" => self.sdt(el, Context::Inline),
            _ if empty_marker(el) => Ok(()),
            _ => self.opaque(el, EMPTY),
        }
    }

    /// A tracked insertion, deletion or move around runs.
    fn wrap(&mut self, el: &XEl) -> Result<(), String> {
        let Some(kind) = WrapKind::of_run_level(&el.name) else { return self.opaque(el, EMPTY) };
        let wrap = self.wrap_of(el, kind);
        self.push_wrap(wrap);
        for node in &el.kids {
            match node {
                XNode::El(child) if &*child.name == "w:r" => self.run(child)?,
                XNode::El(child) if WrapKind::of_run_level(&child.name).is_some() => self.wrap(child)?,
                XNode::El(child) if empty_marker(child) => {}
                XNode::El(child) if RANGE_MARKERS.contains(&&*child.name) => self.opaque(child, EMPTY)?,
                XNode::El(child) => return Err(format!("{} inside a tracked change", child.name)),
                XNode::Text(text) => self.stray(text)?,
            }
        }
        self.stack.pop();
        self.stack_id = None;
        Ok(())
    }

    fn container(&mut self, el: &XEl, property_name: Option<&str>, context: Context) -> Result<(), String> {
        let property = property_name.and_then(|name| first(el, name));
        self.start(el, &[property], EMPTY)?;
        self.children(el, context, &[property])?;
        self.end(el)
    }

    fn sdt(&mut self, el: &XEl, context: Context) -> Result<(), String> {
        let properties = first(el, "w:sdtPr");
        let end_properties = first(el, "w:sdtEndPr");
        let content = first(el, "w:sdtContent");
        let content_tag = content.map(|content| {
            let mut tag = String::new();
            self.canon.start_tag(content, &mut tag);
            tag
        });
        let mut text = String::new();
        self.canon.start_tag(el, &mut text);
        for property in [properties, end_properties].into_iter().flatten() {
            self.canon.element(property, &mut text);
        }
        text.push_str(content_tag.as_deref().unwrap_or("<no content>"));
        self.mark(&text, &el.name, None, EMPTY)?;
        for node in &el.kids {
            match node {
                XNode::El(child) if is(child, properties) || is(child, end_properties) => {}
                XNode::El(child) if is(child, content) => self.children(child, context, &[])?,
                XNode::El(child) => self.opaque(child, EMPTY)?,
                XNode::Text(text) => self.stray(text)?,
            }
        }
        self.end(el)
    }

    fn paragraph(&mut self, p: &XEl) -> Result<(), String> {
        let properties = first(p, "w:pPr");
        self.children(p, Context::Inline, &[properties])?;
        // The paragraph mark: its own tracked changes sit in `w:pPr/w:rPr`.
        let mark_properties = properties.and_then(|properties| first(properties, "w:rPr"));
        let section = properties.and_then(|properties| first(properties, "w:sectPr"));
        let mut pushed = 0;
        if let Some(mark_properties) = mark_properties {
            for child in mark_properties.els() {
                if let Some(kind) = WrapKind::of_run_level(&child.name) {
                    let wrap = self.wrap_of(child, kind);
                    self.push_wrap(wrap);
                    pushed += 1;
                }
            }
        }
        let is_wrap = |child: &XEl| WrapKind::of_run_level(&child.name).is_some();
        let mut slots = Vec::new();
        slots.extend(self.slot("pPr", properties, "w:pPrChange", "w:pPr", &|child| is(child, mark_properties) || is(child, section)));
        slots.extend(self.slot("pPr.rPr", mark_properties, "w:rPrChange", "w:rPr", &is_wrap));
        slots.extend(self.slot("pPr.sectPr", section, "w:sectPrChange", "w:sectPr", &|_| false));
        let format = self.format(slots);
        let mut text = String::from("¶");
        self.canon.start_tag(p, &mut text);
        self.mark(&text, &p.name, None, format)?;
        let keep = self.stack.len().saturating_sub(pushed);
        self.truncate(keep);
        Ok(())
    }

    fn run(&mut self, r: &XEl) -> Result<(), String> {
        let properties = first(r, "w:rPr");
        let mut slots: Vec<Slot> = Vec::new();
        let mut tag = String::new();
        self.canon.start_tag(r, &mut tag);
        if tag != "<w:r>" {
            slots.push(Slot { name: "r", value: self.tables.string(&tag), rev: None });
        }
        slots.extend(self.slot("rPr", properties, "w:rPrChange", "w:rPr", &|_| false));
        let run_format = self.format(slots.clone());
        let deleting = self.stack.iter().any(|wrap| matches!(wrap.kind, WrapKind::Del | WrapKind::MoveFrom));
        for node in &r.kids {
            match node {
                XNode::El(child) if is(child, properties) => {}
                XNode::El(child) => {
                    let kind = match &*child.name {
                        "w:t" if !deleting => Some("text"),
                        "w:delText" if deleting => Some("text"),
                        "w:instrText" if !deleting => Some("instr"),
                        "w:delInstrText" if deleting => Some("instr"),
                        _ => None,
                    };
                    match kind {
                        // Text where Word reads it as text: its characters, with the text element's
                        // own attributes (`xml:space`) as part of their formatting.
                        Some(kind) if !child.has_element_kids() => {
                            // Text reads exactly as written when it keeps its white space
                            // (`xml:space="preserve"`) or has none a reader could trim or collapse;
                            // otherwise how it reads is the reader's, so the attribute is kept and
                            // any change of it is a change.
                            let content = child.text();
                            let plain = !content.starts_with(char::is_whitespace)
                                && !content.ends_with(char::is_whitespace)
                                && !content.contains(|c: char| c.is_whitespace() && c != ' ')
                                && !content.contains("  ");
                            let space = child.attr("xml:space");
                            let reading =
                                if plain || space == Some("preserve") { String::new() } else { format!(" xml:space={}", space.unwrap_or("none")) };
                            let attributes_only = XEl {
                                name: child.name.clone(),
                                attrs: child.attrs.iter().filter(|(key, _)| &**key != "xml:space").cloned().collect(),
                                kids: Vec::new(),
                            };
                            let mut tag = String::new();
                            self.canon.start_tag(&attributes_only, &mut tag);
                            let attributes = tag.split_once(' ').map(|(_, rest)| rest.trim_end_matches('>')).unwrap_or("");
                            let text_slot = Slot { name: "text", value: self.tables.string(&format!("{kind}{reading} {attributes}")), rev: None };
                            let mut with_text = slots.clone();
                            with_text.push(text_slot);
                            let format = self.format(with_text);
                            for kid in &child.kids {
                                if let XNode::Text(text) = kid {
                                    for character in text.chars() {
                                        self.push(u32::from(character), format)?;
                                    }
                                }
                            }
                        }
                        // Anything else, including text in the wrong place for its kind (deleted
                        // text outside a deletion), is compared whole.
                        _ if empty_marker(child) => {}
                        _ => self.opaque(child, run_format)?,
                    }
                }
                XNode::Text(text) => self.stray(text)?,
            }
        }
        Ok(())
    }

    fn table(&mut self, table: &XEl) -> Result<(), String> {
        let properties = first(table, "w:tblPr");
        let grid = first(table, "w:tblGrid");
        // A table whose every row is one tracked insertion or deletion goes with it: its own
        // start and end carry that change too.
        let row_wraps: Vec<Vec<Wrap>> = table
            .els()
            .filter(|row| &*row.name == "w:tr")
            .map(|row| row_changes(row).into_iter().map(|(kind, el)| self.silent_wrap(el, kind)).collect())
            .collect();
        let shared: Vec<Wrap> = match row_wraps.split_first() {
            Some((first_row, rest)) if !first_row.is_empty() && rest.iter().all(|row| row == first_row) => first_row.clone(),
            _ => Vec::new(),
        };
        let depth = self.stack.len();
        for wrap in &shared {
            self.push_wrap(wrap.clone());
        }
        let mut slots = Vec::new();
        slots.extend(self.slot("tblPr", properties, "w:tblPrChange", "w:tblPr", &|_| false));
        slots.extend(self.slot("tblGrid", grid, "w:tblGridChange", "w:tblGrid", &|_| false));
        let format = self.format(slots);
        self.start(table, &[], format)?;
        for node in &table.kids {
            match node {
                XNode::El(child) if is(child, properties) || is(child, grid) => {}
                XNode::El(child) if &*child.name == "w:tr" => self.row(child, !shared.is_empty())?,
                XNode::El(child) => self.block(child)?,
                XNode::Text(text) => self.stray(text)?,
            }
        }
        self.end(table)?;
        self.truncate(depth);
        Ok(())
    }

    /// A wrapper for a change already noted by its own row.
    fn silent_wrap(&self, el: &XEl, kind: WrapKind) -> Wrap {
        let mut extra = String::new();
        self.canon.start_tag(el, &mut extra);
        Wrap { kind, author: el.attr("w:author").unwrap_or("").to_string(), date: el.attr("w:date").unwrap_or("").to_string(), extra }
    }

    fn row(&mut self, row: &XEl, shared: bool) -> Result<(), String> {
        let properties = first(row, "w:trPr");
        let exceptions = first(row, "w:tblPrEx");
        let depth = self.stack.len();
        for (kind, el) in row_changes(row) {
            let wrap = self.wrap_of(el, kind);
            // A change the whole table carries is already on the stack.
            if !shared {
                self.push_wrap(wrap);
            }
        }
        let is_change = |child: &XEl| matches!(&*child.name, "w:ins" | "w:del");
        let mut slots = Vec::new();
        slots.extend(self.slot("trPr", properties, "w:trPrChange", "w:trPr", &is_change));
        slots.extend(self.slot("tblPrEx", exceptions, "w:tblPrExChange", "w:tblPrEx", &|_| false));
        let format = self.format(slots);
        self.start(row, &[], format)?;
        for node in &row.kids {
            match node {
                XNode::El(child) if is(child, properties) || is(child, exceptions) => {}
                XNode::El(child) if &*child.name == "w:tc" => self.cell(child)?,
                XNode::El(child) => self.block(child)?,
                XNode::Text(text) => self.stray(text)?,
            }
        }
        self.end(row)?;
        self.truncate(depth);
        Ok(())
    }

    fn cell(&mut self, cell: &XEl) -> Result<(), String> {
        let properties = first(cell, "w:tcPr");
        let depth = self.stack.len();
        if let Some(properties) = properties {
            for child in properties.els() {
                let kind = match &*child.name {
                    "w:cellIns" => WrapKind::CellIns,
                    "w:cellDel" => WrapKind::CellDel,
                    _ => continue,
                };
                let wrap = self.wrap_of(child, kind);
                self.push_wrap(wrap);
            }
        }
        let is_change = |child: &XEl| matches!(&*child.name, "w:cellIns" | "w:cellDel");
        let slot = self.slot("tcPr", properties, "w:tcPrChange", "w:tcPr", &is_change);
        let format = self.format(slot.into_iter().collect());
        self.start(cell, &[], format)?;
        self.children(cell, Context::Block, &[properties])?;
        self.end(cell)?;
        self.truncate(depth);
        Ok(())
    }
}

/// A row's own tracked insertion or deletion (`w:trPr/w:ins`, `w:trPr/w:del`).
fn row_changes(row: &XEl) -> Vec<(WrapKind, &XEl)> {
    let Some(properties) = first(row, "w:trPr") else { return Vec::new() };
    properties
        .els()
        .filter_map(|child| match &*child.name {
            "w:ins" => Some((WrapKind::RowIns, child)),
            "w:del" => Some((WrapKind::RowDel, child)),
            _ => None,
        })
        .collect()
}

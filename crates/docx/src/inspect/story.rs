//! embed: a story part (the main document, a header, a footer, the notes) as a stream of characters
//! and marks, each with the tracked changes around it, its formatting and (once compared) the
//! context a reader shows it in.
//!
//! The walker descends only into elements whose meaning it knows: blocks, paragraphs, runs,
//! tables, content controls, hyperlinks and tracked changes. Every other element is one opaque
//! mark identified by the digest of its whole canonical form, so a change inside it is a change of
//! the mark, never skipped. A tracked-change wrapper holding something other than runs and empty
//! range markers is refused rather than guessed at.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use super::budget::{Budget, shared_string, table_room, table_slot, tree_room, tree_slot, units, vector_room, vector_slot};
use super::digest::{Digest, Hasher};
use super::opc::{Canon, finish};
use super::xml::{XEl, XNode, is_space};

/// Pieces at or above this are marks (`MARK_BASE + index`); below it, characters.
pub(super) const MARK_BASE: u32 = 0x0011_0000;

/// The empty value, the empty format, the empty wrapper list, the empty text and the plain
/// context: id 0 in each table.
pub(super) const EMPTY: u32 = 0;

/// The longest attribute value copied out of a tree into what an inspection keeps or works with: an
/// author, a date, a comment's or a paragraph's id, a style's name. Real ones are a few dozen bytes;
/// a longer one is refused before it is copied (a style named past it may hide what it formats).
pub(super) const MAX_KEPT_VALUE: usize = 1_024;

/// `value` if it is no longer than [`MAX_KEPT_VALUE`], else why it is refused.
pub(super) fn kept_value<'v>(what: &str, value: &'v str) -> Result<&'v str, String> {
    if value.len() > MAX_KEPT_VALUE { Err(format!("{what} of {} bytes, longer than an inspection keeps", value.len())) } else { Ok(value) }
}

/// The memory of one item, with an eighth over its 16 bytes for what the allocator keeps beside it
/// (its vector's spare capacity is charged too, as it grows).
pub(super) const ITEM_BYTES: u64 = 18;

/// The memory `items` holds: its whole capacity.
pub(super) fn items_held(items: &Vec<Item>) -> u64 {
    units(items.capacity()).saturating_mul(ITEM_BYTES)
}
// Memory charged for each entry the tables intern, besides what it keeps on its own (a text's
// characters, a digest's bytes, a format's slots, a mark's comment id): its entry in its hash
// table and, for those kept in order, in its growing vector.
const CONTENT_BYTES: u64 = table_slot::<(Digest, u32)>();
const TEXT_BYTES: u64 = table_slot::<(Rc<str>, u32)>() + vector_slot::<Rc<str>>();
const WRAP_BYTES: u64 = table_slot::<(Wrap, u32)>() + vector_slot::<Wrap>();
const STACK_BYTES: u64 = table_slot::<((u32, u32), u32)>() + vector_slot::<(u32, u32)>();
const FORMAT_BYTES: u64 = table_slot::<(Format, u32)>() + vector_slot::<Format>();
/// A mark's record, and a name of its own (`#text`, `w:sectPr`) when it has one.
const MARK_BYTES: u64 = table_slot::<(Digest, u32)>() + vector_slot::<MarkInfo>() + 32;
const CONTEXT_BYTES: u64 = table_slot::<(Context, u32)>() + vector_slot::<Context>();
/// The tables' first rooms.
const TABLES_ROOM: u64 = table_room::<(Digest, u32)>() * 2
    + table_room::<(Rc<str>, u32)>()
    + table_room::<(Wrap, u32)>()
    + table_room::<((u32, u32), u32)>()
    + table_room::<(Format, u32)>()
    + table_room::<(Context, u32)>()
    + vector_room::<Rc<str>>()
    + vector_room::<Wrap>()
    + vector_room::<(u32, u32)>()
    + vector_room::<Format>()
    + vector_room::<MarkInfo>()
    + vector_room::<Context>();

/// A character or mark, with its tracked-change wrappers, formatting and context (ids in
/// [`Tables`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Item {
    pub piece: u32,
    pub wraps: u32,
    pub format: u32,
    pub context: u32,
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

/// A tracked change around content: its kind, author and date (text ids), and its element's
/// compared form (a content id).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Wrap {
    pub kind: WrapKind,
    pub author: u32,
    pub date: u32,
    pub extra: u32,
}

/// A tracked formatting change: who, when, its element, the properties before it, and whether
/// those old run properties hid the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct FormatRev {
    pub author: u32,
    pub date: u32,
    pub extra: u32,
    pub old: u32,
    pub old_hidden: bool,
}

/// One kind of properties (`rPr`, `pPr`…): its value (a content id, [`EMPTY`] for none) and its
/// tracked change, if any.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Slot {
    pub name: &'static str,
    pub value: u32,
    pub rev: Option<FormatRev>,
}

/// Every property slot of an item that is not empty, sorted by name; and what they say about it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct Format {
    pub slots: Vec<Slot>,
    /// The run is hidden (`w:vanish`, `w:webHidden`, `w:specVanish`), directly or through its
    /// character style, its paragraph's style or the document defaults.
    pub hidden: bool,
    /// The characters are field code (`w:instrText`).
    pub instr: bool,
    /// The tracked formatting change (author and date) that hides it: one whose recorded old
    /// properties would leave it shown.
    pub hidden_by: Option<(u32, u32)>,
}

impl Format {
    /// A slot's value and change; an absent slot is empty.
    pub(super) fn slot(&self, name: &str) -> (u32, Option<&FormatRev>) {
        self.slots.iter().find(|slot| slot.name == name).map(|slot| (slot.value, slot.rev.as_ref())).unwrap_or((EMPTY, None))
    }
}

/// What a mark is, where its meaning reaches beyond itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MarkKind {
    Plain,
    /// A paragraph mark: its properties govern the paragraph's content.
    Paragraph,
    /// The body's own section properties.
    Section,
    FieldBegin,
    FieldSeparate,
    FieldEnd,
    /// A simple field's start, with its instruction (a content id).
    SimpleField(u32),
    SimpleFieldEnd,
    /// A drawing, picture or embedded object.
    Object,
}

pub(super) struct MarkInfo {
    /// The element's name, for a loss report.
    pub element: Rc<str>,
    /// The comment an empty comment anchor (`w:commentRangeStart`, `…End`, `w:commentReference`)
    /// is for. An anchor holding anything is no anchor but content.
    pub comment: Option<String>,
    pub kind: MarkKind,
    /// What an opaque element holds that a reader can show: for the facts about insertions.
    pub holds: Holds,
}

/// What an opaque element holds: content at all (text, or any child), hidden runs, field
/// characters and code, directional controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Holds {
    pub content: bool,
    pub hidden: u32,
    pub field_codes: u32,
    pub bidi_controls: u32,
}

/// Directional formatting characters: they reorder what follows them.
pub(super) fn is_bidi_control(character: u32) -> bool {
    matches!(character, 0x061C | 0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069)
}

/// Where opaque content stands: the style of the paragraph around it and whether a table around it
/// has a style that may hide.
#[derive(Clone, Copy)]
struct Around<'s> {
    hiding: &'s StyleHiding,
    paragraph_style: Option<&'s str>,
    in_hiding_table: bool,
}

/// What an element holds below itself, read through once (charged as work). Hidden text is counted
/// by the rule runs outside follow: each run whose effective properties may hide (its direct
/// properties, its character style, the style of the paragraph it is in (a paragraph inside, such
/// as a text box's, or the one around the element), a table's style, the document defaults), each
/// paragraph mark the same way, and any other run properties that hide by themselves.
fn scan(el: &XEl, around: Around<'_>, budget: &mut Budget) -> Result<Holds, String> {
    fn children(el: &XEl, around: Around<'_>, holds: &mut Holds, budget: &mut Budget) -> Result<(), String> {
        for node in &el.kids {
            match node {
                XNode::El(child) => {
                    budget.work(1)?;
                    holds.content = true;
                    let mut inner = around;
                    match &*child.name {
                        "w:fldChar" | "w:fldSimple" | "w:instrText" | "w:delInstrText" => holds.field_codes = holds.field_codes.saturating_add(1),
                        "w:r" => {
                            budget.work(4)?;
                            if around.hiding.effective(child.child("w:rPr"), around.paragraph_style, around.in_hiding_table, budget)? {
                                holds.hidden = holds.hidden.saturating_add(1);
                            }
                        }
                        "w:p" => {
                            budget.work(4)?;
                            let properties = child.child("w:pPr");
                            inner.paragraph_style =
                                properties.and_then(|properties| properties.child("w:pStyle")).and_then(|style| style.attr("w:val"));
                            // Its mark, hidden through its own run properties or its styles.
                            let mark = properties.and_then(|properties| properties.child("w:rPr"));
                            if around.hiding.effective(mark, inner.paragraph_style, around.in_hiding_table, budget)? {
                                holds.hidden = holds.hidden.saturating_add(1);
                            }
                        }
                        "w:tbl" => {
                            budget.work(4)?;
                            let style =
                                child.child("w:tblPr").and_then(|properties| properties.child("w:tblStyle")).and_then(|style| style.attr("w:val"));
                            inner.in_hiding_table |= around.hiding.table(style, budget)?;
                        }
                        // Run properties a run or a paragraph mark were counted by.
                        "w:rPr" if matches!(&*el.name, "w:r" | "w:pPr") => {}
                        "w:rPr" if hides(Some(child)) => holds.hidden = holds.hidden.saturating_add(1),
                        _ => {}
                    }
                    children(child, inner, holds, budget)?;
                }
                XNode::Text(text) => {
                    budget.work(1 + units(text.len()) / 16)?;
                    if !is_space(text) {
                        holds.content = true;
                    }
                    let controls = text.chars().filter(|c| is_bidi_control(u32::from(*c))).count();
                    holds.bidi_controls = holds.bidi_controls.saturating_add(u32::try_from(controls).unwrap_or(u32::MAX));
                }
            }
        }
        Ok(())
    }
    let mut holds = Holds::default();
    children(el, around, &mut holds, budget)?;
    Ok(holds)
}

/// Where a reader shows an item: inside which fields (and in their code or their result), in a
/// paragraph with which properties, in a section with which properties. The `_old` values are the
/// properties a tracked change of the governing mark records as before it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct Context {
    pub field: u32,
    pub paragraph: u32,
    pub paragraph_old: Option<u32>,
    pub section: u32,
    pub section_old: Option<u32>,
}

/// Everything items point at, shared by both packages so equal ids mean equal content.
pub(super) struct Tables {
    content_ids: HashMap<Digest, u32>,
    contents: u32,
    text_ids: HashMap<Rc<str>, u32>,
    texts: Vec<Rc<str>>,
    wrap_ids: HashMap<Wrap, u32>,
    wraps: Vec<Wrap>,
    stack_ids: HashMap<(u32, u32), u32>,
    /// (parent stack, wrap): a wrapper list as a chain, so each distinct list costs one entry.
    stacks: Vec<(u32, u32)>,
    format_ids: HashMap<Format, u32>,
    formats: Vec<Format>,
    mark_ids: HashMap<Digest, u32>,
    marks: Vec<MarkInfo>,
    context_ids: HashMap<Context, u32>,
    contexts: Vec<Context>,
    /// Memory interned since it was last charged.
    pending: u64,
    /// An id was asked for past the last one; the inspection must not stand.
    overflow: bool,
}

impl Tables {
    pub(super) fn new() -> Tables {
        let mut tables = Tables {
            content_ids: HashMap::new(),
            contents: 1,
            text_ids: HashMap::new(),
            texts: Vec::new(),
            wrap_ids: HashMap::new(),
            wraps: Vec::new(),
            stack_ids: HashMap::new(),
            stacks: vec![(EMPTY, EMPTY)],
            format_ids: HashMap::new(),
            formats: Vec::new(),
            mark_ids: HashMap::new(),
            marks: Vec::new(),
            context_ids: HashMap::new(),
            contexts: Vec::new(),
            pending: TABLES_ROOM.saturating_add(TEXT_BYTES).saturating_add(shared_string(0)),
            overflow: false,
        };
        tables.insert_text("");
        tables.format(Format::default());
        tables.context(Context::default());
        tables
    }

    /// The next id for a table of `len` entries, whose entry holds `bytes`, or the last one with the
    /// overflow recorded (the inspection is then refused).
    fn next(&mut self, len: usize, bytes: u64) -> u32 {
        let id = self.next_id(len);
        if id != u32::MAX {
            self.pending = self.pending.saturating_add(bytes);
        }
        id
    }

    /// The next id for a table of `len` entries, or the last one with the overflow recorded.
    fn next_id(&mut self, len: usize) -> u32 {
        match u32::try_from(len).ok().filter(|id| *id < u32::MAX >> 1) {
            Some(id) => id,
            None => {
                self.overflow = true;
                u32::MAX
            }
        }
    }

    pub(super) fn overflowed(&self) -> bool {
        self.overflow
    }

    /// Hold what was interned since the last charge.
    pub(super) fn charge(&mut self, budget: &mut Budget) -> Result<(), String> {
        let pending = std::mem::take(&mut self.pending);
        budget.hold(pending)
    }

    /// A content id for a digest.
    pub(super) fn content(&mut self, digest: Digest) -> u32 {
        if let Some(id) = self.content_ids.get(&digest) {
            return *id;
        }
        let id = self.next(usize::try_from(self.contents).unwrap_or(usize::MAX), CONTENT_BYTES);
        self.contents = self.contents.saturating_add(1);
        self.pending = self.pending.saturating_add(digest.held());
        self.content_ids.insert(digest, id);
        id
    }

    /// A text kept as written (an author, a date), for reporting: no longer than
    /// [`MAX_KEPT_VALUE`], and its entry and copy held before it is copied.
    pub(super) fn text(&mut self, what: &str, text: &str, budget: &mut Budget) -> Result<u32, String> {
        let text = kept_value(what, text)?;
        if let Some(id) = self.text_ids.get(text) {
            return Ok(*id);
        }
        budget.hold(TEXT_BYTES.saturating_add(shared_string(text.len())))?;
        Ok(self.insert_text(text))
    }

    fn insert_text(&mut self, text: &str) -> u32 {
        let id = self.next_id(self.texts.len());
        let shared: Rc<str> = Rc::from(text);
        self.texts.push(shared.clone());
        self.text_ids.insert(shared, id);
        id
    }

    pub(super) fn text_of(&self, id: u32) -> &str {
        usize::try_from(id).ok().and_then(|index| self.texts.get(index)).map(|text| &**text).unwrap_or("")
    }

    /// The stack `parent` with `wrap` inside it.
    pub(super) fn push_stack(&mut self, parent: u32, wrap: Wrap) -> u32 {
        let wrap_id = match self.wrap_ids.get(&wrap) {
            Some(id) => *id,
            None => {
                let id = self.next(self.wraps.len(), WRAP_BYTES);
                self.wraps.push(wrap);
                self.wrap_ids.insert(wrap, id);
                id
            }
        };
        if let Some(id) = self.stack_ids.get(&(parent, wrap_id)) {
            return *id;
        }
        let id = self.next(self.stacks.len(), STACK_BYTES);
        self.stacks.push((parent, wrap_id));
        self.stack_ids.insert((parent, wrap_id), id);
        id
    }

    /// A stack's parent and innermost wrapper; `None` for the empty stack.
    pub(super) fn stack_top(&self, id: u32) -> Option<(u32, Wrap)> {
        if id == EMPTY {
            return None;
        }
        let (parent, wrap) = *self.stacks.get(usize::try_from(id).ok()?)?;
        Some((parent, *self.wraps.get(usize::try_from(wrap).ok()?)?))
    }

    /// A stack's wrappers, outermost first.
    pub(super) fn wraps_of(&self, id: u32) -> Vec<Wrap> {
        let mut out = Vec::new();
        let mut at = id;
        // Stacks are at most as deep as the XML nests; the bound only guards a cycle no code makes.
        while let Some((parent, wrap)) = self.stack_top(at) {
            out.push(wrap);
            at = parent;
            if out.len() > 4 * crate::xml::MAX_DEPTH {
                break;
            }
        }
        out.reverse();
        out
    }

    pub(super) fn format(&mut self, mut format: Format) -> u32 {
        if let Some(id) = self.format_ids.get(&format) {
            return *id;
        }
        let id = self.next(self.formats.len(), FORMAT_BYTES);
        // Each slot, in the vector's copy and the map key's, both made to size.
        format.slots.shrink_to_fit();
        self.pending = self.pending.saturating_add(units(format.slots.len()).saturating_mul(2 * std::mem::size_of::<Slot>() as u64));
        self.formats.push(format.clone());
        self.format_ids.insert(format, id);
        id
    }

    pub(super) fn format_of(&self, id: u32) -> Option<&Format> {
        usize::try_from(id).ok().and_then(|index| self.formats.get(index))
    }

    pub(super) fn mark(&mut self, digest: Digest, info: impl FnOnce() -> MarkInfo) -> Result<u32, String> {
        let id = match self.mark_ids.get(&digest) {
            Some(id) => *id,
            None => {
                let id = self.next(self.marks.len(), MARK_BYTES);
                // The comment id it keeps.
                let info = info();
                self.pending = self.pending.saturating_add(units(info.comment.as_ref().map_or(0, String::len)));
                self.marks.push(info);
                self.pending = self.pending.saturating_add(digest.held());
                self.mark_ids.insert(digest, id);
                id
            }
        };
        MARK_BASE.checked_add(id).ok_or_else(|| "too many distinct marks".to_string())
    }

    pub(super) fn mark_info(&self, piece: u32) -> Option<&MarkInfo> {
        let index = piece.checked_sub(MARK_BASE)?;
        self.marks.get(usize::try_from(index).ok()?)
    }

    pub(super) fn context(&mut self, context: Context) -> u32 {
        if let Some(id) = self.context_ids.get(&context) {
            return *id;
        }
        let id = self.next(self.contexts.len(), CONTEXT_BYTES);
        self.contexts.push(context);
        self.context_ids.insert(context, id);
        id
    }

    pub(super) fn context_of(&self, id: u32) -> Context {
        usize::try_from(id).ok().and_then(|index| self.contexts.get(index)).copied().unwrap_or_default()
    }
}

#[derive(Clone, Copy)]
pub(super) enum RevisionKind {
    Insertion,
    Deletion,
    Format,
}

/// How many tracked changes one author and date made.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Counts {
    pub insertions: u32,
    pub deletions: u32,
    pub format_changes: u32,
}

/// Every tracked change in a package, by author and date (text ids).
#[derive(Default)]
pub(super) struct Revisions {
    pub by_pair: BTreeMap<(u32, u32), Counts>,
}

/// Memory charged for each author and date kept: the entry and its share of the tree's nodes.
const PAIR_BYTES: u64 = tree_slot::<(u32, u32), Counts>();

impl Revisions {
    pub(super) fn note(&mut self, author: u32, date: u32, kind: RevisionKind, budget: &mut Budget) -> Result<(), String> {
        if !self.by_pair.contains_key(&(author, date)) {
            let room = if self.by_pair.is_empty() { tree_room::<(u32, u32), Counts>() } else { 0 };
            budget.hold(PAIR_BYTES.saturating_add(room))?;
        }
        let entry = self.by_pair.entry((author, date)).or_default();
        match kind {
            RevisionKind::Insertion => entry.insertions = entry.insertions.saturating_add(1),
            RevisionKind::Deletion => entry.deletions = entry.deletions.saturating_add(1),
            RevisionKind::Format => entry.format_changes = entry.format_changes.saturating_add(1),
        }
        Ok(())
    }
}

/// Range markers that may stand, empty, inside a tracked change beside its runs.
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

/// Elements that draw something of their own: pictures, shapes, embedded objects.
const OBJECTS: &[&str] = &["w:drawing", "w:pict", "w:object", "w:embed", "mc:AlternateContent"];

#[derive(Clone, Copy)]
enum Place {
    Block,
    Inline,
}

/// Walk a story part's tree into items, charging work and memory as it goes.
pub(super) fn walk(
    root: &XEl,
    canon: &Canon<'_>,
    tables: &mut Tables,
    revisions: &mut Revisions,
    items_left: &mut usize,
    most_items: u64,
    hiding: &StyleHiding,
    budget: &mut Budget,
) -> Result<Vec<Item>, String> {
    // Room for every item the tree can make, held and reserved once: a vector growing by doubling
    // holds three times its length while it moves.
    let room = most_items.min(units(*items_left));
    budget.hold(room.saturating_mul(ITEM_BYTES))?;
    let items = Vec::with_capacity(usize::try_from(room).unwrap_or(0));
    budget.release(room.saturating_mul(ITEM_BYTES).saturating_sub(items_held(&items)));
    let mut walker = Walker {
        canon,
        tables,
        revisions,
        items,
        stack: vec![EMPTY],
        items_left,
        budget,
        hiding,
        paragraph_style: ParagraphStyle::default(),
        hiding_tables: 0,
    };
    let result = walker.root(root);
    let held = items_held(&walker.items);
    match result {
        Ok(()) => {
            // Keep only what is used: a story's spare capacity would be held for the whole inspection.
            // The vector moves to its new room, both held while it moves.
            let mut items = walker.items;
            let used = units(items.len()).saturating_mul(ITEM_BYTES);
            if used < held {
                if let Err(error) = walker.budget.hold(used) {
                    walker.budget.release(held);
                    return Err(error);
                }
                items.shrink_to_fit();
                walker.budget.release(held.saturating_add(used).saturating_sub(items_held(&items)));
            }
            Ok(items)
        }
        Err(error) => {
            walker.budget.release(held);
            Err(error)
        }
    }
}

struct Walker<'a, 'c> {
    canon: &'a Canon<'c>,
    tables: &'a mut Tables,
    revisions: &'a mut Revisions,
    items: Vec<Item>,
    /// The ids of the tracked-change stacks entered, outermost first; the last is current.
    stack: Vec<u32>,
    items_left: &'a mut usize,
    budget: &'a mut Budget,
    hiding: &'a StyleHiding,
    paragraph_style: ParagraphStyle,
    /// How many of the tables around the current position have a style that may hide.
    hiding_tables: u32,
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

/// Whether run properties say the run is hidden: `Some` when they set it either way.
fn vanish(properties: Option<&XEl>) -> Option<bool> {
    let mut said = None;
    for child in properties?.els() {
        if matches!(&*child.name, "w:vanish" | "w:webHidden" | "w:specVanish") {
            let on = !matches!(child.attr("w:val"), Some("0" | "false" | "off"));
            said = Some(said.unwrap_or(false) || on);
        }
    }
    said
}

/// Run properties that hide what they format, by themselves.
fn hides(properties: Option<&XEl>) -> bool {
    vanish(properties).unwrap_or(false)
}

/// Which styles may hide what they format, read from the styles part, and whether the document's
/// defaults and default styles do. Hidden text through a style is hidden text as surely as direct
/// `w:vanish`, and what cannot be settled counts as hidden: Word's `vanish` is a toggle, so the
/// levels can cancel, but any `vanish` a run can reach through its styles makes it hidden here. A
/// reader may show text this reports hidden, never the reverse.
#[derive(Default)]
pub(super) struct StyleHiding {
    /// Style id → what its definitions say.
    styles: HashMap<String, StyleEntry>,
    /// The document defaults' run properties hide.
    defaults: bool,
    /// The default paragraph, character and table styles hide (each `w:default` style of its type,
    /// whichever a reader takes).
    default_paragraph: bool,
    default_character: bool,
    default_table: bool,
}

#[derive(Default)]
struct StyleEntry {
    /// Its own run properties, or any of its conditional table formats' (`w:tblStylePr`), hide.
    own: bool,
    based: Option<String>,
    /// Defined twice, based on different styles: which a reader takes is not settled.
    conflicting: bool,
    /// Which kinds of default style it is.
    default_of: u8,
    /// Anything reachable from it hides: worked out once all styles are read.
    hides: bool,
}

const DEFAULT_PARAGRAPH: u8 = 1;
const DEFAULT_CHARACTER: u8 = 2;
const DEFAULT_TABLE: u8 = 4;

/// The longest style chain followed; a longer one (or a loop) reads as hiding, since what lies
/// past it is unknown.
const MAX_STYLE_CHAIN: usize = 32;

/// Whether run properties hide what they format, by themselves (any of `w:vanish`, `w:webHidden`,
/// `w:specVanish` on).
fn may_hide(properties: Option<&XEl>) -> bool {
    properties.is_some_and(|properties| {
        properties.els().any(|child| {
            matches!(&*child.name, "w:vanish" | "w:webHidden" | "w:specVanish") && !matches!(child.attr("w:val"), Some("0" | "false" | "off"))
        })
    })
}

impl StyleHiding {
    pub(super) fn read(root: &XEl, budget: &mut Budget) -> Result<StyleHiding, String> {
        // Sized from the styles there are, so the table never grows.
        let count = root.els().filter(|child| &*child.name == "w:style").count();
        budget.work(units(count))?;
        budget.hold(
            units(count).saturating_add(1).saturating_mul(table_slot::<(String, StyleEntry)>()).saturating_add(table_room::<(String, StyleEntry)>()),
        )?;
        let mut hiding = StyleHiding { styles: HashMap::with_capacity(count), ..StyleHiding::default() };
        for child in root.els() {
            budget.work(8)?;
            match &*child.name {
                "w:docDefaults" => {
                    let defaults = child.child("w:rPrDefault").and_then(|default| default.child("w:rPr"));
                    hiding.defaults |= may_hide(defaults);
                }
                "w:style" => {
                    let Some(id) = child.attr("w:styleId") else { continue };
                    let based = child.child("w:basedOn").and_then(|based| based.attr("w:val"));
                    let own = may_hide(child.child("w:rPr"))
                        || child.els().any(|format| &*format.name == "w:tblStylePr" && may_hide(format.child("w:rPr")));
                    let default_of = match (child.attr("w:default"), child.attr("w:type")) {
                        (Some("1" | "true" | "on"), Some("paragraph") | None) => DEFAULT_PARAGRAPH,
                        (Some("1" | "true" | "on"), Some("character")) => DEFAULT_CHARACTER,
                        (Some("1" | "true" | "on"), Some("table")) => DEFAULT_TABLE,
                        _ => 0,
                    };
                    match hiding.styles.get_mut(id) {
                        Some(entry) => {
                            entry.own |= own;
                            entry.conflicting |= entry.based.as_deref() != based;
                            entry.default_of |= default_of;
                        }
                        None => {
                            budget.hold(units(id.len().saturating_add(based.map_or(0, str::len))))?;
                            let entry = StyleEntry { own, based: based.map(str::to_string), default_of, ..StyleEntry::default() };
                            hiding.styles.insert(id.to_string(), entry);
                        }
                    }
                }
                _ => {}
            }
        }
        // What each style reaches, worked out once: a run asks a single lookup per style.
        budget.work(units(hiding.styles.len()).saturating_mul(units(MAX_STYLE_CHAIN)))?;
        budget.hold(units(hiding.styles.len()))?;
        let reached: Vec<bool> = hiding.styles.keys().map(|id| hiding.reaches(id)).collect();
        for (entry, hides) in hiding.styles.values_mut().zip(reached) {
            entry.hides = hides;
            hiding.default_paragraph |= hides && entry.default_of & DEFAULT_PARAGRAPH != 0;
            hiding.default_character |= hides && entry.default_of & DEFAULT_CHARACTER != 0;
            hiding.default_table |= hides && entry.default_of & DEFAULT_TABLE != 0;
        }
        budget.release(units(hiding.styles.len()));
        Ok(hiding)
    }

    /// Whether any style reachable from `id` (itself, then the styles it is based on) hides; a loop
    /// or a chain past `MAX_STYLE_CHAIN` does.
    fn reaches(&self, id: &str) -> bool {
        let mut at = id;
        for _ in 0..MAX_STYLE_CHAIN {
            let Some(entry) = self.styles.get(at) else { return false };
            if entry.own || entry.conflicting {
                return true;
            }
            match entry.based.as_deref() {
                Some(based) => at = based,
                None => return false,
            }
        }
        true
    }

    /// Whether a style, or the default style of its kind when none is named or the named one is
    /// missing, may hide.
    fn style(&self, id: Option<&str>, default: bool, budget: &mut Budget) -> Result<bool, String> {
        Ok(match id {
            // A name longer than any kept value is no style a reader settles: it may hide.
            Some(id) if id.len() > MAX_KEPT_VALUE => true,
            Some(id) => {
                // The lookup hashes and compares the name.
                budget.work(2 + units(id.len()) / 8)?;
                self.styles.get(id).map_or(default, |entry| entry.hides)
            }
            None => default,
        })
    }

    /// Whether a table of this style (the default table style when it names none) may hide its
    /// cells' text.
    pub(super) fn table(&self, style: Option<&str>, budget: &mut Budget) -> Result<bool, String> {
        self.style(style, self.default_table, budget)
    }

    /// Whether a run with these direct properties, in a paragraph of this style, inside a table
    /// whose style may hide (`in_hiding_table`), is hidden. Direct formatting decides when it says;
    /// otherwise any level that may hide counts: the character style, the paragraph style, the
    /// table style, the document defaults.
    pub(super) fn effective(
        &self,
        properties: Option<&XEl>,
        paragraph_style: Option<&str>,
        in_hiding_table: bool,
        budget: &mut Budget,
    ) -> Result<bool, String> {
        if let Some(said) = vanish(properties) {
            return Ok(said);
        }
        let character_style = properties.and_then(|properties| properties.child("w:rStyle")).and_then(|style| style.attr("w:val"));
        Ok(self.style(character_style, self.default_character, budget)?
            || self.style(paragraph_style, self.default_paragraph, budget)?
            || in_hiding_table
            || self.defaults)
    }
}

/// A paragraph's style, and the style a tracked change of its properties records before it.
#[derive(Default)]
struct ParagraphStyle {
    now: Option<String>,
    change: Option<(u32, u32, Option<String>)>,
}

impl Walker<'_, '_> {
    fn current(&self) -> u32 {
        self.stack.last().copied().unwrap_or(EMPTY)
    }

    fn push(&mut self, piece: u32, format: u32) -> Result<(), String> {
        *self.items_left = self.items_left.checked_sub(1).ok_or("more text than an inspection reads")?;
        self.budget.work(3)?;
        // The room was reserved from a bound on what the tree can make; never grow past it.
        if self.items.len() == self.items.capacity() {
            return Err("more items than the part can make".into());
        }
        self.tables.charge(self.budget)?;
        let wraps = self.current();
        self.items.push(Item { piece, wraps, format, context: EMPTY });
        Ok(())
    }

    fn enter(&mut self, wrap: Wrap) {
        let id = self.tables.push_stack(self.current(), wrap);
        self.stack.push(id);
    }

    fn leave_to(&mut self, depth: usize) {
        self.stack.truncate(depth.max(1));
    }

    fn mark(&mut self, digest: Digest, info: impl FnOnce() -> MarkInfo, format: u32) -> Result<(), String> {
        let piece = self.tables.mark(digest, info)?;
        self.push(piece, format)
    }

    /// An element compared whole.
    fn opaque(&mut self, el: &XEl, format: u32) -> Result<(), String> {
        let anchor = matches!(&*el.name, "w:commentRangeStart" | "w:commentRangeEnd" | "w:commentReference");
        // Only an empty anchor is an anchor: anything inside one is content a reader may show.
        let comment = (anchor && el.kids.is_empty()).then(|| kept_value("a comment anchor's id", el.attr("w:id").unwrap_or(""))).transpose()?;
        let kind = match (&*el.name, el.attr("w:fldCharType")) {
            ("w:fldChar", Some("begin")) => MarkKind::FieldBegin,
            ("w:fldChar", Some("separate")) => MarkKind::FieldSeparate,
            ("w:fldChar", Some("end")) => MarkKind::FieldEnd,
            (name, _) if OBJECTS.contains(&name) => MarkKind::Object,
            _ => MarkKind::Plain,
        };
        let around = Around { hiding: self.hiding, paragraph_style: self.paragraph_style.now.as_deref(), in_hiding_table: self.hiding_tables > 0 };
        let holds = scan(el, around, self.budget)?;
        let mut hasher = Hasher::default();
        hasher.field(b'O', &[]);
        self.canon.element(el, &mut hasher);
        // What it hides depends on the styles around it, which the two packages need not share:
        // the same content hiding otherwise is another mark.
        if holds.hidden > 0 {
            hasher.field(b'h', &holds.hidden.to_le_bytes());
        }
        let digest = finish(hasher, self.budget)?;
        let element = el.name.clone();
        // The id is copied only for a mark the tables do not have yet, which charges it.
        self.mark(digest, || MarkInfo { element, comment: comment.map(str::to_string), kind, holds }, format)
    }

    /// Text where only elements belong: white space is layout, anything else is kept as a mark.
    fn stray(&mut self, text: &str) -> Result<(), String> {
        if is_space(text) {
            return Ok(());
        }
        let mut hasher = Hasher::default();
        hasher.field(b'#', text.as_bytes());
        let digest = finish(hasher, self.budget)?;
        self.mark(digest, || MarkInfo { element: Rc::from("#text"), comment: None, kind: MarkKind::Plain, holds: Holds::default() }, EMPTY)
    }

    /// A container's start: its start tag and its property elements.
    fn start(&mut self, el: &XEl, properties: &[Option<&XEl>], kind: MarkKind, format: u32) -> Result<(), String> {
        let mut hasher = Hasher::default();
        hasher.field(b'S', &[]);
        self.canon.start_tag(el, &mut hasher);
        for property in properties {
            match property {
                Some(property) => self.canon.element(property, &mut hasher),
                None => hasher.field(b'-', &[]),
            }
        }
        let digest = finish(hasher, self.budget)?;
        let element = el.name.clone();
        self.mark(digest, || MarkInfo { element, comment: None, kind, holds: Holds::default() }, format)
    }

    fn end(&mut self, el: &XEl, kind: MarkKind) -> Result<(), String> {
        let mut hasher = Hasher::default();
        hasher.field(b'E', el.name.as_bytes());
        let digest = finish(hasher, self.budget)?;
        let element = el.name.clone();
        self.mark(digest, || MarkInfo { element, comment: None, kind, holds: Holds::default() }, EMPTY)
    }

    /// A tracked change's author and date, kept as texts.
    fn author_date(&mut self, el: &XEl) -> Result<(u32, u32), String> {
        let author = self.tables.text("a w:author", el.attr("w:author").unwrap_or(""), self.budget)?;
        let date = self.tables.text("a w:date", el.attr("w:date").unwrap_or(""), self.budget)?;
        Ok((author, date))
    }

    fn note(&mut self, el: &XEl, kind: RevisionKind) -> Result<(u32, u32, u32), String> {
        let (author, date) = self.author_date(el)?;
        self.revisions.note(author, date, kind, self.budget)?;
        let digest = self.canon.start_digest(el, self.budget)?;
        Ok((author, date, self.tables.content(digest)))
    }

    fn wrap_of(&mut self, el: &XEl, kind: WrapKind) -> Result<Wrap, String> {
        let revision = if kind.inserts() { RevisionKind::Insertion } else { RevisionKind::Deletion };
        let (author, date, extra) = self.note(el, revision)?;
        Ok(Wrap { kind, author, date, extra })
    }

    /// A wrapper for a change already noted by its own row.
    fn silent_wrap(&mut self, el: &XEl, kind: WrapKind) -> Result<Wrap, String> {
        let (author, date) = self.author_date(el)?;
        let digest = self.canon.start_digest(el, self.budget)?;
        Ok(Wrap { kind, author, date, extra: self.tables.content(digest) })
    }

    /// The value of a property element without the children `skip` accepts, as a content id;
    /// properties with nothing compared in them read as none, unless `present_counts` (a section's
    /// properties: whether a paragraph ends a section at all is what they say first).
    fn value(&mut self, property: Option<&XEl>, skip: &dyn Fn(&XEl) -> bool, present_counts: bool) -> Result<u32, String> {
        let Some(property) = property else { return Ok(EMPTY) };
        let kept_children = property.kids.iter().any(|node| match node {
            XNode::El(child) => !skip(child),
            XNode::Text(text) => !is_space(text),
        });
        if !present_counts && !kept_children && !self.canon.has_attributes(property) {
            return Ok(EMPTY);
        }
        let digest = self.canon.digest(property, &|child| !skip(child), self.budget)?;
        Ok(self.tables.content(digest))
    }

    /// A property slot: `property` without its tracked change (`change_name`, whose `inner` child
    /// holds the properties before it) and without the `skip` children. A change that records no
    /// properties before it explains nothing: its old value is one no base can have.
    fn slot(
        &mut self,
        name: &'static str,
        property: Option<&XEl>,
        change_name: &str,
        inner: &str,
        skip: &dyn Fn(&XEl) -> bool,
    ) -> Result<Option<Slot>, String> {
        let present_counts = matches!(name, "sectPr" | "pPr.sectPr");
        let change = property.and_then(|property| first(property, change_name));
        let value = self.value(property, &|child| is(child, change) || skip(child), present_counts)?;
        let rev = match change {
            Some(change) => {
                let old_properties = first(change, inner);
                let old = match old_properties {
                    Some(_) => self.value(old_properties, &|_| false, present_counts)?,
                    None => {
                        let mut hasher = Hasher::default();
                        hasher.field(b'!', b"no properties recorded");
                        self.tables.content(finish(hasher, self.budget)?)
                    }
                };
                let digest = self.canon.digest(change, &|child| !is(child, old_properties), self.budget)?;
                let extra = self.tables.content(digest);
                let (author, date, _) = self.note(change, RevisionKind::Format)?;
                let old_hidden = self.hiding.effective(old_properties, self.paragraph_style.now.as_deref(), self.hiding_tables > 0, self.budget)?;
                Some(FormatRev { author, date, extra, old, old_hidden })
            }
            None => None,
        };
        Ok((value != EMPTY || rev.is_some()).then_some(Slot { name, value, rev }))
    }

    fn format(&mut self, mut slots: Vec<Slot>, hidden: bool, instr: bool) -> u32 {
        slots.sort_by_key(|slot| slot.name);
        self.tables.format(Format { slots, hidden, instr, hidden_by: None })
    }

    /// Whether run properties hide in this paragraph, and which tracked change hides them: the
    /// run's own (`rev`) or its paragraph's, if its recorded old properties would show the run.
    fn hiding(&mut self, properties: Option<&XEl>, rev: Option<&FormatRev>) -> Result<(bool, Option<(u32, u32)>), String> {
        let style = self.paragraph_style.now.as_deref();
        let tables = self.hiding_tables > 0;
        let hidden = self.hiding.effective(properties, style, tables, self.budget)?;
        if !hidden {
            return Ok((false, None));
        }
        if let Some(rev) = rev.filter(|rev| !rev.old_hidden) {
            return Ok((true, Some((rev.author, rev.date))));
        }
        let by_paragraph = match &self.paragraph_style.change {
            Some((author, date, old)) if !self.hiding.effective(properties, old.as_deref(), tables, self.budget)? => Some((*author, *date)),
            _ => None,
        };
        Ok((true, by_paragraph))
    }

    fn hidden_format(&mut self, mut slots: Vec<Slot>, hiding: (bool, Option<(u32, u32)>), instr: bool) -> u32 {
        slots.sort_by_key(|slot| slot.name);
        self.tables.format(Format { slots, hidden: hiding.0, instr, hidden_by: hiding.1 })
    }

    fn root(&mut self, root: &XEl) -> Result<(), String> {
        match &*root.name {
            "w:document" => {
                self.start(root, &[], MarkKind::Plain, EMPTY)?;
                for node in &root.kids {
                    match node {
                        XNode::El(child) if &*child.name == "w:body" => {
                            self.start(child, &[], MarkKind::Plain, EMPTY)?;
                            self.children(child, Place::Block, &[])?;
                        }
                        XNode::El(child) => self.opaque(child, EMPTY)?,
                        XNode::Text(text) => self.stray(text)?,
                    }
                }
                Ok(())
            }
            "w:hdr" | "w:ftr" => {
                self.start(root, &[], MarkKind::Plain, EMPTY)?;
                self.children(root, Place::Block, &[])
            }
            "w:footnotes" | "w:endnotes" => {
                self.start(root, &[], MarkKind::Plain, EMPTY)?;
                for node in &root.kids {
                    match node {
                        XNode::El(child) if matches!(&*child.name, "w:footnote" | "w:endnote") => {
                            self.start(child, &[], MarkKind::Plain, EMPTY)?;
                            self.children(child, Place::Block, &[])?;
                            self.end(child, MarkKind::Plain)?;
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

    /// The children of `el` in `place`, except the property elements in `skip`.
    fn children(&mut self, el: &XEl, place: Place, skip: &[Option<&XEl>]) -> Result<(), String> {
        for node in &el.kids {
            match node {
                XNode::El(child) if skip.iter().any(|skipped| is(child, *skipped)) => {}
                XNode::El(child) => match place {
                    Place::Block => self.block(child)?,
                    Place::Inline => self.inline(child)?,
                },
                XNode::Text(text) => self.stray(text)?,
            }
        }
        Ok(())
    }

    fn block(&mut self, el: &XEl) -> Result<(), String> {
        self.budget.work(4)?;
        match &*el.name {
            "w:p" => self.paragraph(el),
            "w:tbl" => self.table(el),
            "w:sdt" => self.sdt(el, Place::Block),
            "w:customXml" => self.container(el, Some("w:customXmlPr"), Place::Block, MarkKind::Plain),
            "w:sectPr" => {
                let slot = self.slot("sectPr", Some(el), "w:sectPrChange", "w:sectPr", &|_| false)?;
                let format = self.format(slot.into_iter().collect(), false, false);
                let mut hasher = Hasher::default();
                hasher.field(b'Z', &[]);
                let digest = finish(hasher, self.budget)?;
                self.mark(
                    digest,
                    || MarkInfo { element: Rc::from("w:sectPr"), comment: None, kind: MarkKind::Section, holds: Holds::default() },
                    format,
                )
            }
            _ if empty_marker(el) => Ok(()),
            _ => self.opaque(el, EMPTY),
        }
    }

    fn inline(&mut self, el: &XEl) -> Result<(), String> {
        self.budget.work(4)?;
        match &*el.name {
            "w:r" => self.run(el),
            "w:ins" | "w:del" | "w:moveFrom" | "w:moveTo" => self.wrap(el),
            "w:fldSimple" => {
                let mut hasher = Hasher::default();
                hasher.field(b'I', el.attr("w:instr").unwrap_or("").as_bytes());
                let instruction = self.tables.content(finish(hasher, self.budget)?);
                self.start(el, &[], MarkKind::SimpleField(instruction), EMPTY)?;
                self.children(el, Place::Inline, &[])?;
                self.end(el, MarkKind::SimpleFieldEnd)
            }
            "w:hyperlink" | "w:dir" | "w:bdo" => self.container(el, None, Place::Inline, MarkKind::Plain),
            "w:smartTag" => self.container(el, Some("w:smartTagPr"), Place::Inline, MarkKind::Plain),
            "w:customXml" => self.container(el, Some("w:customXmlPr"), Place::Inline, MarkKind::Plain),
            "w:sdt" => self.sdt(el, Place::Inline),
            _ if empty_marker(el) => Ok(()),
            _ => self.opaque(el, EMPTY),
        }
    }

    /// A tracked insertion, deletion or move around runs.
    fn wrap(&mut self, el: &XEl) -> Result<(), String> {
        let Some(kind) = WrapKind::of_run_level(&el.name) else { return self.opaque(el, EMPTY) };
        let wrap = self.wrap_of(el, kind)?;
        let depth = self.stack.len();
        self.enter(wrap);
        for node in &el.kids {
            match node {
                XNode::El(child) if &*child.name == "w:r" => self.run(child)?,
                XNode::El(child) if WrapKind::of_run_level(&child.name).is_some() => self.wrap(child)?,
                XNode::El(child) if empty_marker(child) => {}
                // A range marker holding anything could carry content a reader shows untracked.
                XNode::El(child) if RANGE_MARKERS.contains(&&*child.name) && child.kids.is_empty() => self.opaque(child, EMPTY)?,
                XNode::El(child) => return Err(format!("{} inside a tracked change", child.name)),
                XNode::Text(text) => self.stray(text)?,
            }
        }
        self.leave_to(depth);
        Ok(())
    }

    fn container(&mut self, el: &XEl, property_name: Option<&str>, place: Place, kind: MarkKind) -> Result<(), String> {
        let property = property_name.and_then(|name| first(el, name));
        self.start(el, &[property], kind, EMPTY)?;
        self.children(el, place, &[property])?;
        self.end(el, MarkKind::Plain)
    }

    fn sdt(&mut self, el: &XEl, place: Place) -> Result<(), String> {
        let properties = first(el, "w:sdtPr");
        let end_properties = first(el, "w:sdtEndPr");
        let content = first(el, "w:sdtContent");
        let mut hasher = Hasher::default();
        hasher.field(b'S', &[]);
        self.canon.start_tag(el, &mut hasher);
        for property in [properties, end_properties] {
            match property {
                Some(property) => self.canon.element(property, &mut hasher),
                None => hasher.field(b'-', &[]),
            }
        }
        match content {
            Some(content) => self.canon.start_tag(content, &mut hasher),
            None => hasher.field(b'-', &[]),
        }
        let digest = finish(hasher, self.budget)?;
        let element = el.name.clone();
        self.mark(digest, || MarkInfo { element, comment: None, kind: MarkKind::Plain, holds: Holds::default() }, EMPTY)?;
        for node in &el.kids {
            match node {
                XNode::El(child) if is(child, properties) || is(child, end_properties) => {}
                XNode::El(child) if is(child, content) => self.children(child, place, &[])?,
                XNode::El(child) => self.opaque(child, EMPTY)?,
                XNode::Text(text) => self.stray(text)?,
            }
        }
        self.end(el, MarkKind::Plain)
    }

    fn paragraph(&mut self, p: &XEl) -> Result<(), String> {
        let properties = first(p, "w:pPr");
        // The style's name is copied for the paragraph's runs: no longer than a kept value.
        let style_of = |properties: Option<&XEl>| -> Result<Option<String>, String> {
            let style = properties.and_then(|properties| properties.child("w:pStyle")).and_then(|style| style.attr("w:val"));
            style.map(|style| kept_value("a paragraph style's name", style).map(str::to_string)).transpose()
        };
        let change = properties.and_then(|properties| first(properties, "w:pPrChange"));
        let recorded = match change {
            Some(change) => {
                let (author, date) = self.author_date(change)?;
                Some((author, date, style_of(first(change, "w:pPr"))?))
            }
            None => None,
        };
        self.paragraph_style = ParagraphStyle { now: style_of(properties)?, change: recorded };
        self.children(p, Place::Inline, &[properties])?;
        // The paragraph mark: its own tracked changes sit in `w:pPr/w:rPr`.
        let mark_properties = properties.and_then(|properties| first(properties, "w:rPr"));
        let section = properties.and_then(|properties| first(properties, "w:sectPr"));
        let depth = self.stack.len();
        if let Some(mark_properties) = mark_properties {
            for child in mark_properties.els() {
                if let Some(kind) = WrapKind::of_run_level(&child.name) {
                    let wrap = self.wrap_of(child, kind)?;
                    self.enter(wrap);
                }
            }
        }
        let is_wrap = |child: &XEl| WrapKind::of_run_level(&child.name).is_some();
        let mut slots = Vec::new();
        slots.extend(self.slot("pPr", properties, "w:pPrChange", "w:pPr", &|child| is(child, mark_properties) || is(child, section))?);
        slots.extend(self.slot("pPr.rPr", mark_properties, "w:rPrChange", "w:rPr", &is_wrap)?);
        slots.extend(self.slot("pPr.sectPr", section, "w:sectPrChange", "w:sectPr", &|_| false)?);
        // A hidden mark hides the paragraph's end: its paragraph runs into the next.
        let mark_rev = slots.iter().find(|slot| slot.name == "pPr.rPr").and_then(|slot| slot.rev);
        let hiding = self.hiding(mark_properties, mark_rev.as_ref())?;
        let format = self.hidden_format(slots, hiding, false);
        let mut hasher = Hasher::default();
        hasher.field(b'P', &[]);
        self.canon.start_tag(p, &mut hasher);
        let digest = finish(hasher, self.budget)?;
        let element = p.name.clone();
        self.mark(digest, || MarkInfo { element, comment: None, kind: MarkKind::Paragraph, holds: Holds::default() }, format)?;
        self.leave_to(depth);
        Ok(())
    }

    fn run(&mut self, r: &XEl) -> Result<(), String> {
        let properties = first(r, "w:rPr");
        let mut slots: Vec<Slot> = Vec::new();
        if self.canon.has_attributes(r) {
            let digest = self.canon.start_digest(r, self.budget)?;
            slots.push(Slot { name: "r", value: self.tables.content(digest), rev: None });
        }
        slots.extend(self.slot("rPr", properties, "w:rPrChange", "w:rPr", &|_| false)?);
        let rev = slots.iter().find(|slot| slot.name == "rPr").and_then(|slot| slot.rev);
        let hidden = self.hiding(properties, rev.as_ref())?;
        let run_format = self.hidden_format(slots.clone(), hidden, false);
        let deleting = self.tables.wraps_of(self.current()).iter().any(|wrap| matches!(wrap.kind, WrapKind::Del | WrapKind::MoveFrom));
        for node in &r.kids {
            match node {
                XNode::El(child) if is(child, properties) => {}
                XNode::El(child) => {
                    let kind = match &*child.name {
                        "w:t" if !deleting => Some(false),
                        "w:delText" if deleting => Some(false),
                        "w:instrText" if !deleting => Some(true),
                        "w:delInstrText" if deleting => Some(true),
                        _ => None,
                    };
                    match kind {
                        // Text where Word reads it as text: its characters, with the text element's
                        // own attributes as part of their formatting.
                        Some(instr) if !child.has_element_kids() => {
                            let format = self.text_format(child, &slots, hidden, instr)?;
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

    /// The format of a text element's characters: the run's, and how the element reads.
    fn text_format(&mut self, child: &XEl, slots: &[Slot], hidden: (bool, Option<(u32, u32)>), instr: bool) -> Result<u32, String> {
        // Text reads exactly as written when it keeps its white space (`xml:space="preserve"`)
        // or has none a reader could trim or collapse; otherwise how it reads is the reader's,
        // so the attribute is kept and any change of it is a change.
        // Read in place, never copied: the text can be as long as its part.
        let characters = || child.texts().flat_map(str::chars);
        let mut previous_space = false;
        let inner = characters().all(|c| {
            let double = c == ' ' && previous_space;
            previous_space = c == ' ';
            !double && (!c.is_whitespace() || c == ' ')
        });
        let plain = inner && !characters().next().is_some_and(char::is_whitespace) && !characters().next_back().is_some_and(char::is_whitespace);
        let space = child.attr("xml:space");
        let exact = plain || space == Some("preserve");
        let mut hasher = Hasher::default();
        hasher.field(b'k', if instr { b"instr" } else { b"text" });
        hasher.field(b'x', if exact { b"" } else { space.unwrap_or("none").as_bytes() });
        // The element's other attributes, read in place (never copied: a value can be as long as
        // its part), as the start tag of an element named `text`.
        self.canon.start_tag_of("text", child.attrs.iter().filter(|(key, _)| &**key != "xml:space"), &mut hasher);
        let digest = finish(hasher, self.budget)?;
        let mut with_text = slots.to_vec();
        with_text.push(Slot { name: "text", value: self.tables.content(digest), rev: None });
        Ok(self.hidden_format(with_text, hidden, instr))
    }

    fn table(&mut self, table: &XEl) -> Result<(), String> {
        let properties = first(table, "w:tblPr");
        let grid = first(table, "w:tblGrid");
        // A table whose every row is one tracked insertion or deletion goes with it: its own
        // start and end carry that change too. Each row's changes are compared with the first
        // row's, one row at a time.
        let mut shared: Option<Vec<Wrap>> = None;
        let mut all_alike = true;
        for row in table.els().filter(|row| &*row.name == "w:tr") {
            let mut wraps = Vec::new();
            for (kind, el) in row_changes(row) {
                wraps.push(self.silent_wrap(el, kind)?);
            }
            match &shared {
                None => shared = Some(wraps),
                Some(first_row) => all_alike &= *first_row == wraps,
            }
        }
        let shared: Vec<Wrap> = shared.filter(|first_row| all_alike && !first_row.is_empty()).unwrap_or_default();
        // Text in the table's cells is hidden when its style (or the default table style) may hide.
        let style = properties.and_then(|properties| properties.child("w:tblStyle")).and_then(|style| style.attr("w:val"));
        let hides = self.hiding.table(style, self.budget)?;
        let depth = self.stack.len();
        for wrap in &shared {
            self.enter(*wrap);
        }
        let mut slots = Vec::new();
        slots.extend(self.slot("tblPr", properties, "w:tblPrChange", "w:tblPr", &|_| false)?);
        slots.extend(self.slot("tblGrid", grid, "w:tblGridChange", "w:tblGrid", &|_| false)?);
        let format = self.format(slots, false, false);
        self.start(table, &[], MarkKind::Plain, format)?;
        if hides {
            self.hiding_tables = self.hiding_tables.saturating_add(1);
        }
        let rows = (|| {
            for node in &table.kids {
                match node {
                    XNode::El(child) if is(child, properties) || is(child, grid) => {}
                    XNode::El(child) if &*child.name == "w:tr" => self.row(child, !shared.is_empty())?,
                    XNode::El(child) => self.block(child)?,
                    XNode::Text(text) => self.stray(text)?,
                }
            }
            Ok::<(), String>(())
        })();
        if hides {
            self.hiding_tables = self.hiding_tables.saturating_sub(1);
        }
        rows?;
        self.end(table, MarkKind::Plain)?;
        self.leave_to(depth);
        Ok(())
    }

    fn row(&mut self, row: &XEl, shared: bool) -> Result<(), String> {
        let properties = first(row, "w:trPr");
        let exceptions = first(row, "w:tblPrEx");
        let depth = self.stack.len();
        for (kind, el) in row_changes(row) {
            let wrap = self.wrap_of(el, kind)?;
            // A change the whole table carries is already on the stack.
            if !shared {
                self.enter(wrap);
            }
        }
        let is_change = |child: &XEl| matches!(&*child.name, "w:ins" | "w:del");
        let mut slots = Vec::new();
        slots.extend(self.slot("trPr", properties, "w:trPrChange", "w:trPr", &is_change)?);
        slots.extend(self.slot("tblPrEx", exceptions, "w:tblPrExChange", "w:tblPrEx", &|_| false)?);
        let format = self.format(slots, false, false);
        self.start(row, &[], MarkKind::Plain, format)?;
        for node in &row.kids {
            match node {
                XNode::El(child) if is(child, properties) || is(child, exceptions) => {}
                XNode::El(child) if &*child.name == "w:tc" => self.cell(child)?,
                XNode::El(child) => self.block(child)?,
                XNode::Text(text) => self.stray(text)?,
            }
        }
        self.end(row, MarkKind::Plain)?;
        self.leave_to(depth);
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
                let wrap = self.wrap_of(child, kind)?;
                self.enter(wrap);
            }
        }
        let is_change = |child: &XEl| matches!(&*child.name, "w:cellIns" | "w:cellDel");
        let slot = self.slot("tcPr", properties, "w:tcPrChange", "w:tcPr", &is_change)?;
        let format = self.format(slot.into_iter().collect(), false, false);
        self.start(cell, &[], MarkKind::Plain, format)?;
        self.children(cell, Place::Block, &[properties])?;
        self.end(cell, MarkKind::Plain)?;
        self.leave_to(depth);
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

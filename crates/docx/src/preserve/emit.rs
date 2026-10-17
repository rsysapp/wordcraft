//! embed: a paragraph of the source rewritten by its steps.
//!
//! The paragraph's markup is walked in the order it is written: its properties, its nodes (runs and
//! other elements), the start and end tags of the wrappers around them (hyperlinks, content
//! controls, tracked changes), and the bytes between them (white space, comments, processing
//! instructions), which are copied where they stand. A wrapper that holds no node is copied whole,
//! like a node. A node no step touches is copied whole too (through [`Source::copy`], which carries
//! the text-box paragraphs already rewritten inside it).
//!
//! Wrappers are opened as the content written needs them, from a stack of open ones. A touched run
//! is cut into pieces of one status at character boundaries; each piece repeats the run's start tag
//! and properties. Its wrappers follow from its change: an accepted character leaves its `w:ins`, a
//! rejected one its `w:del`, a character deleted now gets the session's `w:del` innermost (6C-43,
//! 6C-44: after leaving the `w:ins` it was accepted from, or the `w:del` it was rejected from).
//! Typed text is a run of its own inside the non-revision wrappers of the character it continues (a
//! hyperlink, a content control), never inside another author's tracked change, in the session's
//! `w:ins`, with that character's run properties without `w:rPrChange` (6C-5). A revision wrapper
//! opened a second time gets a fresh `w:id`; any other wrapper opened twice is refused, so a
//! hyperlink or content control is never split.
//!
//! A new paragraph mark closes every open wrapper, ends the paragraph and starts the next. Every half
//! but the last gets a new start tag (fresh `w14:paraId`, `w14:textId`) and the source's properties
//! without `w:sectPr`, its `w:pPrChange` with a fresh id (6C-24), and the session's `w:ins` first in
//! its mark's `w:rPr`; the last keeps the source's tags and properties.
//!
//! An element with no character of its own inside a tracked change (a content control's properties,
//! a proofing mark) goes with the characters beside it in the same wrappers, so a decided change
//! takes a whole inserted control and never leaves a shell. Markers (bookmarks, comment ranges)
//! stay where they are, inside the session's deletion when their character is deleted (6C-46 M8).
//! Locked characters are never written otherwise than as they are, and nothing is typed among them.
//!
//! What it works with is charged to the save's budget and given back; the paragraph it returns stays
//! charged by its room ([`held_by`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

use wordcraft_doc::para::OBJ;

use super::Unpreservable;
use super::plan::{Change, Revision, Step};
use super::scan::{Child, ChildKind, Loc, Node, Props, RunNode, ScannedPara, WrapperKind};
use crate::inspect::budget::{BYTES_PER_UNIT_COPIED, Budget, table_room, table_slot, tree_room, tree_slot, units, vector_slot};
use crate::xml::esc;

/// What may sit inside a new deletion besides runs: range markers and proofing marks, which Word
/// does not track (the inspector's range markers).
const DELETABLE: &[&str] = &[
    "w:bookmarkStart",
    "w:bookmarkEnd",
    "w:commentRangeStart",
    "w:commentRangeEnd",
    "w:proofErr",
    "w:permStart",
    "w:permEnd",
    "w:moveFromRangeStart",
    "w:moveFromRangeEnd",
    "w:moveToRangeStart",
    "w:moveToRangeEnd",
];
/// A paragraph mark's own tracked changes: left out when its properties are copied.
const MARK_CHANGES: &[&str] = &["w:ins", "w:del", "w:moveFrom", "w:moveTo", "w:rPrChange"];
/// Elements beside which typing stays outside whatever they hold: alternative content and
/// equations are one node each, written whole.
const SEALED_WHOLE: &[&str] = &["mc:AlternateContent", "m:oMath", "m:oMathPara"];

const SAME: &Change = &Change::Same;

/// The highest tracked-change id Word reads (`w:id` is a 32-bit signed number there).
const MAX_ID: u64 = i32::MAX as u64;

/// Fresh ids for new tracked changes and paragraphs, unique in the package.
pub(crate) struct Ids {
    next_revision: u64,
    paragraph_ids: HashSet<u32>,
    next_paragraph: u32,
}

impl Ids {
    /// `highest_revision_id`: the highest `w:id` in the package's WordprocessingML parts;
    /// `paragraph_ids`: every `w14:paraId` and `w14:textId` in them. The set is the caller's, charged
    /// by it: each new paragraph adds two ids to it.
    pub fn new(highest_revision_id: u64, paragraph_ids: HashSet<u32>) -> Ids {
        Ids { next_revision: highest_revision_id.saturating_add(1), paragraph_ids, next_paragraph: 0 }
    }

    /// A new `w:id`: Word reads ids as 32-bit signed numbers, so none past `i32::MAX` is made.
    pub(super) fn revision(&mut self) -> Result<String, Unpreservable> {
        let id = self.next_revision;
        if id > MAX_ID {
            return Err(Unpreservable::TooManyChanges);
        }
        self.next_revision = id.saturating_add(1);
        Ok(id.to_string())
    }

    /// A `w14:paraId`/`w14:textId` value: eight hexadecimal digits below 0x80000000, unused.
    fn paragraph(&mut self) -> Result<String, Unpreservable> {
        for _ in 0..0x7FFF_FFFFu32 {
            self.next_paragraph = self.next_paragraph % 0x7FFF_FFFF + 1;
            if self.paragraph_ids.insert(self.next_paragraph) {
                return Ok(format!("{:08X}", self.next_paragraph));
            }
        }
        Err(Unpreservable::TooManyChanges)
    }
}

/// A part's bytes with the paragraphs already rewritten inside it (start → (end, bytes)), so a
/// copy of a range carries them: a text box's paragraph inside a drawing inside a paragraph.
pub(crate) struct Source<'a> {
    pub bytes: &'a [u8],
    pub rewritten: &'a BTreeMap<usize, (usize, Vec<u8>)>,
}

/// A copy that would cut a paragraph already rewritten.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Straddled;

impl Source<'_> {
    pub fn copy(&self, range: Range<usize>, out: &mut Vec<u8>) -> Result<(), Straddled> {
        self.each(range, |piece| out.extend_from_slice(piece))
    }

    /// Each piece a copy of `range` is made of, in order. A paragraph already rewritten that reaches
    /// past either end of the range cannot be copied in part: refused, never copied as it was.
    fn each(&self, range: Range<usize>, mut piece: impl FnMut(&[u8])) -> Result<(), Straddled> {
        if range.start >= range.end {
            return Ok(());
        }
        if self.rewritten.range(..range.start).next_back().is_some_and(|(_, (end, _))| *end > range.start) {
            return Err(Straddled);
        }
        let mut at = range.start;
        for (&start, (end, bytes)) in self.rewritten.range(range.start..range.end) {
            // One inside a span already written whole is written with it.
            if start < at {
                continue;
            }
            if *end > range.end {
                return Err(Straddled);
            }
            piece(self.bytes.get(at..start).unwrap_or_default());
            piece(bytes);
            at = *end;
        }
        piece(self.bytes.get(at..range.end).unwrap_or_default());
        Ok(())
    }

    fn length(&self, range: Range<usize>) -> Result<usize, Straddled> {
        let mut length = 0usize;
        self.each(range, |piece| length = length.saturating_add(piece.len()))?;
        Ok(length)
    }

    /// A tag as written (the scan read it, so it is text).
    fn text(&self, range: Range<usize>) -> Option<&str> {
        self.bytes.get(range).and_then(|bytes| std::str::from_utf8(bytes).ok())
    }
}

/// Memory the rewritten paragraph holds, as [`rewrite`] leaves it charged to the save's budget.
pub(crate) fn held_by(written: &Vec<u8>) -> u64 {
    units(written.capacity())
}

/// What happens to a source character: its change, or gone with its decided change.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Status<'a> {
    Keep(&'a Change),
    Remove,
}

const KEPT: Status<'static> = Status::Keep(SAME);

enum Insertion<'a> {
    Text { text: &'a str, revision: &'a Revision, anchor: Option<usize> },
    Mark,
}

/// Where an insertion goes: before an item (a node, or a wrapper that holds none), before a
/// character of a run's child, or at the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum At {
    Before(usize),
    Child { node: usize, child: usize, raw: usize },
    End,
}

/// An open wrapper: one of the source's, or a new tracked change of this session.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Entry<'a> {
    Source(usize),
    New { deleted: bool, revision: &'a Revision },
}

enum RunProps {
    None,
    /// Byte ranges of the run properties to copy (without the formatting change).
    Copy(Vec<Range<usize>>),
    /// The paragraph mark's properties, without its own tracked changes.
    Mark,
}

/// What the paragraph's markup is made of, in the order it is written.
#[derive(Clone, Copy, Debug)]
enum Token {
    /// The paragraph's properties.
    Props,
    Item(Item),
    /// A wrapper's start tag; it is written when the content inside needs it.
    Open(usize),
    /// A wrapper's end tag; it is written when the content after needs it.
    Close(usize),
}

/// What bytes between tags follow in the source: something written already, a start tag or an end
/// tag that may be written later.
#[derive(Clone, Copy, Debug)]
enum After {
    Written,
    Open(usize),
    Close(usize),
}

/// The tag a held gap is written right after.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Attach {
    Open(usize),
    Close(usize),
}

/// Something written as a whole or as pieces: a node, or a wrapper that holds no node.
#[derive(Clone, Copy, Debug)]
enum Item {
    Node(usize),
    Empty(usize),
}

#[derive(Clone)]
enum Part {
    Whole(usize),
    Chars { child: usize, from: usize, to: usize },
}

struct Piece<'a> {
    status: Status<'a>,
    parts: Vec<Part>,
}

enum Segment<'a, 'i> {
    Run(Piece<'a>),
    Insert(&'i [Insertion<'a>]),
}

/// The save's budget as the rewriting uses it: what it holds is counted, to be given back.
struct Ledger<'b> {
    budget: &'b mut Budget,
    held: u64,
}

impl Ledger<'_> {
    fn hold(&mut self, bytes: u64) -> Result<(), Unpreservable> {
        self.budget.hold(bytes).map_err(|_| Unpreservable::TooManyChanges)?;
        self.held = self.held.saturating_add(bytes);
        Ok(())
    }

    fn release(&mut self, bytes: u64) {
        let bytes = bytes.min(self.held);
        self.budget.release(bytes);
        self.held -= bytes;
    }

    fn work(&mut self, units: u64) -> Result<(), Unpreservable> {
        if self.budget.spend(units) { Ok(()) } else { Err(Unpreservable::TooManyChanges) }
    }

    /// Room for `count` elements of `T`, charged before the vector is made to that size.
    fn vector<T>(&mut self, count: usize) -> Result<Vec<T>, Unpreservable> {
        self.hold(units(count).saturating_mul(units(std::mem::size_of::<T>())))?;
        Ok(Vec::with_capacity(count))
    }
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn with_local(raw_name: &str, new_local: &str) -> String {
    match raw_name.split_once(':') {
        Some((prefix, _)) => format!("{prefix}:{new_local}"),
        None => new_local.to_string(),
    }
}

/// `<name …>` from `<name …/>`, for an element written empty that now gets content.
fn open_tag(tag: &str, written_empty: bool) -> String {
    if !written_empty {
        return tag.to_string();
    }
    let trimmed = tag.trim_end();
    format!("{}>", trimmed.strip_suffix("/>").unwrap_or(trimmed).trim_end())
}

/// A start tag with its name replaced, attributes as written.
fn rename_tag(tag: &str, old: &str, new: &str) -> String {
    format!("<{new}{}", tag.get(1usize.saturating_add(old.len())..).unwrap_or(">"))
}

/// A start tag with the attributes whose local name `changes` lists set to new values (the others
/// as written); none are added.
fn retag(tag: &str, changes: &[(&str, String)]) -> String {
    let empty = tag.trim_end().ends_with("/>");
    let inner = tag.trim().trim_start_matches('<').trim_end_matches('>').trim_end_matches('/').trim_end();
    let name_length = inner.find(char::is_whitespace).unwrap_or(inner.len());
    let start = quick_xml::events::BytesStart::from_content(inner, name_length);
    let mut out = format!("<{}", inner.get(..name_length).unwrap_or(""));
    for attribute in start.attributes().with_checks(false).flatten() {
        let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
        let value = match changes.iter().find(|(name, _)| *name == local(&key)) {
            Some((_, value)) => value.clone(),
            None => String::from_utf8_lossy(&attribute.value).replace('"', "&quot;"),
        };
        out.push_str(&format!(" {key}=\"{value}\""));
    }
    out.push_str(if empty { "/>" } else { ">" });
    out
}

/// A start tag with `xml:space="preserve"`: set where it is written, added last where it is not.
fn preserving_space(tag: &str) -> String {
    let empty = tag.trim_end().ends_with("/>");
    let inner = tag.trim().trim_start_matches('<').trim_end_matches('>').trim_end_matches('/').trim_end();
    let name_length = inner.find(char::is_whitespace).unwrap_or(inner.len());
    let start = quick_xml::events::BytesStart::from_content(inner, name_length);
    let mut out = format!("<{}", inner.get(..name_length).unwrap_or(""));
    let mut set = false;
    for attribute in start.attributes().with_checks(false).flatten() {
        let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
        let value = if key == "xml:space" {
            set = true;
            "preserve".to_string()
        } else {
            String::from_utf8_lossy(&attribute.value).replace('"', "&quot;")
        };
        out.push_str(&format!(" {key}=\"{value}\""));
    }
    if !set {
        out.push_str(" xml:space=\"preserve\"");
    }
    out.push_str(if empty { "/>" } else { ">" });
    out
}

/// Where an element's start tag ends (after its `>`), quotes respected.
fn start_tag_end(bytes: &[u8], span: &Range<usize>) -> Option<usize> {
    let mut quote = None;
    for (offset, byte) in bytes.get(span.clone())?.iter().enumerate() {
        match (quote, byte) {
            (None, b'"' | b'\'') => quote = Some(*byte),
            (Some(open), _) if open == *byte => quote = None,
            (None, b'>') => return span.start.checked_add(offset)?.checked_add(1),
            _ => {}
        }
    }
    None
}

/// Whether a change wraps its character in this session's deletion, and how its text is named:
/// `Some(true)` as deleted text (`w:t` → `w:delText`), `Some(false)` as text again.
fn renaming(change: &Change) -> Option<bool> {
    match change {
        Change::Deleted(_) | Change::AcceptedThenDeleted(_) => Some(true),
        Change::Rejected => Some(false),
        Change::Same | Change::Accepted | Change::RejectedThenDeleted(_) => None,
    }
}

fn deletes(change: &Change) -> bool {
    matches!(change, Change::Deleted(_) | Change::AcceptedThenDeleted(_) | Change::RejectedThenDeleted(_))
}

fn props_span(props: &Props) -> Range<usize> {
    props.start.start..props.end.as_ref().map_or(props.start.end, |end| end.end)
}

fn node_span(node: &Node) -> Range<usize> {
    match node {
        Node::Run(run) => run.span(),
        Node::Other { span, .. } => span.clone(),
    }
}

fn wrapper_span(paragraph: &ScannedPara, index: usize) -> Range<usize> {
    paragraph.wrappers.get(index).map_or(0..0, |wrapper| wrapper.start.start..wrapper.end.as_ref().map_or(wrapper.start.end, |end| end.end))
}

/// Whether `wrapper` is `within` or inside it.
fn is_within(paragraph: &ScannedPara, wrapper: Option<usize>, within: Option<usize>) -> bool {
    let Some(within) = within else { return true };
    let mut at = wrapper;
    let mut steps = 0usize;
    while let Some(index) = at
        && steps <= paragraph.wrappers.len()
    {
        if index == within {
            return true;
        }
        at = paragraph.wrappers.get(index).and_then(|wrapper| wrapper.parent).filter(|parent| *parent < index);
        steps += 1;
    }
    false
}

/// Whether a wrapper of a tracked change stands around `wrapper` (or is it).
fn revised(paragraph: &ScannedPara, wrapper: Option<usize>) -> bool {
    paragraph.chain(wrapper).iter().any(|index| paragraph.wrappers.get(*index).is_some_and(|wrapper| wrapper.kind != WrapperKind::Other))
}

/// Where text or a mark put before unit `unit` stands: before a node (`Ok(Some(node), None)`), before
/// a character of a run (`Ok(None, Some(at))`), or at the end (`Ok(None, None)`).
fn position(paragraph: &ScannedPara, unit: usize, part: &str) -> Result<(Option<usize>, Option<At>), Unpreservable> {
    Ok(match paragraph.units.get(unit).map(|unit| &unit.loc) {
        None => (None, None),
        Some(Loc::Text { node, child, raw }) => (None, Some(At::Child { node: *node, child: *child, raw: *raw })),
        Some(Loc::Single { node, child }) => (None, Some(At::Child { node: *node, child: *child, raw: 0 })),
        Some(Loc::Atomic { first, .. }) => (Some(*first), None),
        Some(Loc::Marker { node }) => (Some(*node), None),
        Some(Loc::Locked { node: Some(node), child: Some(child) }) => (None, Some(At::Child { node: *node, child: *child, raw: 0 })),
        Some(Loc::Locked { node: Some(node), child: None }) => (Some(*node), None),
        Some(Loc::Locked { node: None, .. }) => return Err(Unpreservable::Locked { part: part.to_string() }),
        // A paragraph with no node has nothing to stand before: the end.
        Some(Loc::Outside { at_end: false }) => ((!paragraph.nodes.is_empty()).then_some(0), None),
        Some(Loc::Outside { at_end: true }) => (None, None),
    })
}

/// The wrappers an insertion is written in, of the source's: the non-revision wrappers of the
/// character typing continued (none for a paragraph mark, or typing that continued the mark).
fn insertion_wrappers(paragraph: &ScannedPara, insertion: &Insertion<'_>) -> Vec<usize> {
    let Insertion::Text { anchor: Some(anchor), .. } = insertion else { return Vec::new() };
    let innermost = match unit_nodes(paragraph, *anchor) {
        Some((node, _)) => paragraph.nodes.get(node).and_then(Node::wrapper),
        None => None,
    };
    paragraph
        .chain(innermost)
        .into_iter()
        .take_while(|index| paragraph.wrappers.get(*index).is_some_and(|wrapper| wrapper.kind == WrapperKind::Other))
        .collect()
}

/// The item an insertion naturally before item `item` is written before. Where writing it there
/// would leave a hyperlink or content control that goes on after it, or enter one again that
/// closed before it, it moves earlier past the items with no character of their own (a content
/// control's properties, a proofing mark), which the model does not place, to the latest place
/// where it does neither. A mark before a content control's first character goes before the
/// control, not after its properties; typing that continues a hyperlink's last character stays
/// in the hyperlink though a proofing mark follows it. Where no such place exists it stays, and
/// the writer refuses the split.
fn hoisted(
    paragraph: &ScannedPara,
    items: &[(Item, usize)],
    own: &[Option<Status<'_>>],
    item: usize,
    insertion: &Insertion<'_>,
    ledger: &mut Ledger<'_>,
) -> Result<usize, Unpreservable> {
    let wanted = insertion_wrappers(paragraph, insertion);
    let innermost = |index: usize| match items.get(index).map(|(item, _)| *item) {
        Some(Item::Node(node)) => paragraph.nodes.get(node).and_then(Node::wrapper),
        Some(Item::Empty(wrapper)) => paragraph.wrappers.get(wrapper).and_then(|wrapper| wrapper.parent),
        None => None,
    };
    let other = |wrapper: &usize| paragraph.wrappers.get(*wrapper).is_some_and(|wrapper| wrapper.kind == WrapperKind::Other);
    let fits = |at: usize| {
        let previous = at.checked_sub(1).map(|before| paragraph.chain(innermost(before))).unwrap_or_default();
        let next = paragraph.chain(innermost(at));
        let leaves = previous.iter().any(|wrapper| other(wrapper) && next.contains(wrapper) && !wanted.contains(wrapper));
        // Entered again: one that started before the item before this place, and is no longer open.
        let previous_start = at.checked_sub(1).and_then(|before| items.get(before)).map_or(0, |(_, start)| *start);
        let enters_again = wanted.iter().any(|wrapper| {
            !previous.contains(wrapper) && paragraph.wrappers.get(*wrapper).is_some_and(|wrapper| wrapper.start.start < previous_start)
        });
        !leaves && !enters_again
    };
    if fits(item) {
        return Ok(item);
    }
    let mut at = item;
    while let Some(before) = at.checked_sub(1) {
        ledger.work(1)?;
        let characterless = match items.get(before).map(|(item, _)| *item) {
            Some(Item::Node(index)) => own.get(index).copied().flatten().is_none(),
            Some(Item::Empty(_)) => true,
            None => false,
        };
        if !characterless {
            break;
        }
        at = before;
        if fits(at) {
            return Ok(at);
        }
    }
    Ok(item)
}

/// The nodes unit `unit` stands in, when it stands in nodes.
fn unit_nodes(paragraph: &ScannedPara, unit: usize) -> Option<(usize, usize)> {
    match paragraph.units.get(unit)?.loc {
        Loc::Text { node, .. } | Loc::Single { node, .. } | Loc::Marker { node } | Loc::Locked { node: Some(node), .. } => Some((node, node)),
        Loc::Atomic { first, last } => Some((first, last)),
        Loc::Locked { node: None, .. } | Loc::Outside { .. } => None,
    }
}

/// Whether text or a paragraph mark put before unit `at` would land inside something locked: a
/// field's instruction or result, a field written as one element, a text box. A locked object that
/// stands whole in a run of its own child (a comment reference, a page number) or is one node
/// (alternative content, an equation) can have typing beside it.
fn sealed(paragraph: &ScannedPara, at: usize) -> bool {
    let inside = |index: usize| {
        let Some(unit) = paragraph.units.get(index) else { return false };
        match unit.loc {
            Loc::Locked { node: Some(_), child: Some(_) } => unit.ch != OBJ,
            Loc::Locked { node: Some(node), child: None } => {
                !matches!(paragraph.nodes.get(node), Some(Node::Other { name, .. }) if SEALED_WHOLE.contains(&name.as_str()))
            }
            Loc::Locked { node: None, .. } => true,
            _ => false,
        }
    };
    at.checked_sub(1).is_some_and(inside) || inside(at)
}

/// A typed character as run content: text, or the element the reader reads it from.
enum Typed {
    Text(char),
    Element(&'static str, Option<&'static str>),
}

/// Whether XML can hold `ch` (`esc` drops the others).
fn xml_char(ch: char) -> bool {
    matches!(ch, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

fn typed_char(ch: char) -> Option<Typed> {
    Some(match ch {
        '\t' => Typed::Element("tab", None),
        '\n' => Typed::Element("br", None),
        '\u{000C}' => Typed::Element("br", Some("page")),
        '\u{000E}' => Typed::Element("br", Some("column")),
        '\u{2011}' => Typed::Element("noBreakHyphen", None),
        '\u{00AD}' => Typed::Element("softHyphen", None),
        // What the reader would not read back as typed: a carriage return (a space), another
        // control character or an object mark (nothing), what XML cannot hold.
        '\r' | OBJ | '\u{FFFE}' | '\u{FFFF}' => return None,
        ch if (ch as u32) < 0x20 => return None,
        ch => Typed::Text(ch),
    })
}

/// The paragraph rewritten by `steps` (from `plan`): what the steps do not touch is copied as written.
/// `prefix` is the part's WordprocessingML prefix, `part` its name (said in refusals). `ids` gives
/// new tracked changes and paragraphs ids unique in the package. What it works with is charged to
/// `budget` and given back; the paragraph returned stays charged ([`held_by`]), and a refusal gives
/// back everything.
pub(crate) fn rewrite(
    source: &Source<'_>,
    prefix: &str,
    part: &str,
    paragraph: &ScannedPara,
    steps: &[Step],
    ids: &mut Ids,
    budget: &mut Budget,
) -> Result<Vec<u8>, Unpreservable> {
    let mut ledger = Ledger { budget, held: 0 };
    // Nothing changes (6C-48 M1): the paragraph as written.
    let unchanged = steps.len() == paragraph.units.len()
        && steps.iter().enumerate().all(|(index, step)| matches!(step, Step::Keep { unit, change: Change::Same } if *unit == index));
    let written =
        if unchanged { copied(source, part, paragraph, &mut ledger) } else { write(source, prefix, part, paragraph, steps, ids, &mut ledger) };
    match written {
        Ok(out) => {
            // What it worked with is gone; the paragraph stays, charged by its room.
            let kept = held_by(&out);
            if ledger.held >= kept {
                ledger.release(ledger.held.saturating_sub(kept));
            } else if let Err(refused) = ledger.hold(kept.saturating_sub(ledger.held)) {
                ledger.release(ledger.held);
                return Err(refused);
            }
            Ok(out)
        }
        Err(refused) => {
            ledger.release(ledger.held);
            Err(refused)
        }
    }
}

/// The paragraph's span, copied, charged as `write` charges its output.
fn copied(source: &Source<'_>, part: &str, paragraph: &ScannedPara, ledger: &mut Ledger<'_>) -> Result<Vec<u8>, Unpreservable> {
    let unmapped = || Unpreservable::Unmapped { part: part.to_string() };
    let length = source.length(paragraph.span()).map_err(|_| unmapped())?;
    ledger.work(units(length) / BYTES_PER_UNIT_COPIED + 1)?;
    let mut out = ledger.vector::<u8>(length)?;
    source.copy(paragraph.span(), &mut out).map_err(|_| unmapped())?;
    // What the allocator gave beyond the room asked for is charged too.
    ledger.hold(units(out.capacity().saturating_sub(length)))?;
    Ok(out)
}

fn write(
    source: &Source<'_>,
    prefix: &str,
    part: &str,
    paragraph: &ScannedPara,
    steps: &[Step],
    ids: &mut Ids,
    ledger: &mut Ledger<'_>,
) -> Result<Vec<u8>, Unpreservable> {
    let count = paragraph.units.len();
    let (nodes, wrappers) = (paragraph.nodes.len(), paragraph.wrappers.len());
    ledger.work(units(count).saturating_add(units(nodes)).saturating_add(units(wrappers)).saturating_add(units(steps.len())))?;
    let locked = || Unpreservable::Locked { part: part.to_string() };
    // New markup names its attributes with the part's prefix; a part written in a default
    // namespace has none for them.
    let new_markup = steps.iter().any(|step| {
        matches!(
            step,
            Step::Insert { .. }
                | Step::Break { .. }
                | Step::Keep { change: Change::Deleted(_) | Change::AcceptedThenDeleted(_) | Change::RejectedThenDeleted(_), .. }
        )
    });
    if new_markup && prefix.is_empty() {
        return Err(Unpreservable::Namespace { part: part.to_string() });
    }

    // Each source character's status, in order; insertions where they go.
    let mut status: Vec<Status<'_>> = ledger.vector(count)?;
    status.resize(count, KEPT);
    // Insertions with the unit they go before, placed once the paragraph's items are known.
    let mut placing: Vec<(usize, Insertion<'_>)> = Vec::new();
    let mut marks: Vec<&Revision> = Vec::new();
    let mut next = 0usize;
    for step in steps {
        match step {
            Step::Keep { unit, .. } | Step::Remove { unit } => {
                // Every character once, in order: anything else is not a plan of this paragraph.
                let slot = status.get_mut(*unit).filter(|_| *unit == next).ok_or(Unpreservable::Structure)?;
                *slot = match step {
                    Step::Keep { change, .. } => Status::Keep(change),
                    _ => Status::Remove,
                };
                next = next.saturating_add(1);
            }
            Step::Insert { .. } | Step::Break { .. } => {
                if sealed(paragraph, next) {
                    return Err(locked());
                }
                ledger.hold(vector_slot::<(usize, Insertion<'_>)>())?;
                let insertion = match step {
                    Step::Insert { text, revision, anchor } => Insertion::Text { text, revision, anchor: *anchor },
                    Step::Break { revision } => {
                        ledger.hold(vector_slot::<&Revision>())?;
                        marks.push(revision);
                        Insertion::Mark
                    }
                    _ => continue,
                };
                placing.push((next, insertion));
            }
        }
    }
    if next != count {
        return Err(Unpreservable::Structure);
    }
    // What is written beside other content is never changed: a marker the reader moved in from
    // between blocks stays there; a locked character stays as it is.
    for (unit, slot) in paragraph.units.iter().zip(status.iter_mut()) {
        if *slot == KEPT {
            continue;
        }
        match unit.loc {
            Loc::Outside { .. } => *slot = KEPT,
            Loc::Locked { .. } => return Err(locked()),
            _ => {}
        }
    }

    // A node's own status: the object it is part of, or its first character's.
    let mut own: Vec<Option<Status<'_>>> = ledger.vector(nodes)?;
    own.resize(nodes, None);
    let mut atomic: Vec<bool> = ledger.vector(nodes)?;
    atomic.resize(nodes, false);
    // The first rooms of the tables made below.
    ledger.hold(
        table_room::<((usize, usize), Status<'_>)>()
            .saturating_add(table_room::<(usize, Status<'_>)>())
            .saturating_add(tree_room::<At, Vec<Insertion<'_>>>()),
    )?;
    let mut locked_children: HashMap<(usize, usize), Status<'_>> = HashMap::new();
    for (unit, unit_status) in paragraph.units.iter().zip(&status) {
        match unit.loc {
            Loc::Atomic { first, last } => {
                for node in first..=last.min(nodes.saturating_sub(1)) {
                    if let (Some(slot), Some(whole)) = (own.get_mut(node), atomic.get_mut(node)) {
                        *slot = Some(*unit_status);
                        *whole = true;
                    }
                }
            }
            Loc::Text { node, .. } | Loc::Single { node, .. } | Loc::Marker { node } | Loc::Locked { node: Some(node), .. } => {
                if let Some(slot) = own.get_mut(node)
                    && slot.is_none()
                {
                    *slot = Some(*unit_status);
                }
                if let Loc::Locked { node, child: Some(child) } = unit.loc
                    && let Some(node) = node
                {
                    ledger.hold(table_slot::<((usize, usize), Status<'_>)>())?;
                    locked_children.insert((node, child), *unit_status);
                }
            }
            Loc::Locked { node: None, .. } | Loc::Outside { .. } => {}
        }
    }

    // Which wrappers hold a node; the others are written whole, like a node.
    let mut holds: Vec<bool> = ledger.vector(wrappers)?;
    holds.resize(wrappers, false);
    for node in &paragraph.nodes {
        let mut at = node.wrapper();
        while let Some(index) = at {
            match holds.get_mut(index) {
                Some(slot) if !*slot => *slot = true,
                _ => break,
            }
            at = paragraph.wrappers.get(index).and_then(|wrapper| wrapper.parent).filter(|parent| *parent < index);
        }
    }
    // Which wrappers hold a tracked change.
    let mut changes_inside: Vec<bool> = ledger.vector(wrappers)?;
    changes_inside.resize(wrappers, false);
    for index in (0..wrappers).rev() {
        let Some(wrapper) = paragraph.wrappers.get(index) else { continue };
        if (wrapper.kind != WrapperKind::Other || changes_inside.get(index).copied().unwrap_or(false))
            && let Some(slot) = wrapper.parent.filter(|parent| *parent < index).and_then(|parent| changes_inside.get_mut(parent))
        {
            *slot = true;
        }
    }
    let mut tokens: Vec<(Range<usize>, Token)> = ledger.vector(nodes.saturating_add(wrappers.saturating_mul(2)).saturating_add(1))?;
    if let Some(props) = &paragraph.ppr {
        tokens.push((props_span(props), Token::Props));
    }
    for (index, node) in paragraph.nodes.iter().enumerate() {
        tokens.push((node_span(node), Token::Item(Item::Node(index))));
    }
    for (index, wrapper) in paragraph.wrappers.iter().enumerate() {
        if holds.get(index).copied().unwrap_or(false) {
            tokens.push((wrapper.start.clone(), Token::Open(index)));
            if let Some(end) = &wrapper.end {
                tokens.push((end.clone(), Token::Close(index)));
            }
        } else if wrapper.parent.is_none_or(|parent| holds.get(parent).copied().unwrap_or(false)) {
            tokens.push((wrapper_span(paragraph, index), Token::Item(Item::Empty(index))));
        }
    }
    tokens.sort_by_key(|(span, _)| span.start);

    let adopted = adopted_statuses(paragraph, &tokens, &own, ledger)?;

    // Each node's place among the items.
    let mut items: Vec<(Item, usize)> = ledger.vector(tokens.len())?;
    items.extend(tokens.iter().filter_map(|(span, token)| if let Token::Item(item) = token { Some((*item, span.start)) } else { None }));
    let mut item_of: Vec<usize> = ledger.vector(nodes)?;
    item_of.resize(nodes, 0);
    for (index, (item, _)) in items.iter().enumerate() {
        if let Item::Node(node) = item
            && let Some(slot) = item_of.get_mut(*node)
        {
            *slot = index;
        }
    }
    let inserted = placing.len();
    // An element written empty stays so while nothing is put in it.
    if inserted == 0 && paragraph.end.is_none() {
        return copied(source, part, paragraph, ledger);
    }
    let mut insertions: BTreeMap<At, Vec<Insertion<'_>>> = BTreeMap::new();
    for (unit, insertion) in placing {
        let at = match position(paragraph, unit, part)? {
            (Some(node), _) => {
                let item = item_of.get(node).copied().ok_or_else(|| Unpreservable::Unmapped { part: part.to_string() })?;
                At::Before(hoisted(paragraph, &items, &own, item, &insertion, ledger)?)
            }
            (None, Some(At::Child { node, child, raw })) => {
                // The first character of a run: what leads into it may be passed over too.
                let starts = unit.checked_sub(1).and_then(|before| unit_nodes(paragraph, before)).is_none_or(|(_, last)| last < node);
                let item = item_of.get(node).copied().ok_or_else(|| Unpreservable::Unmapped { part: part.to_string() })?;
                match hoisted(paragraph, &items, &own, item, &insertion, ledger)? {
                    earlier if starts && earlier < item => At::Before(earlier),
                    _ => At::Child { node, child, raw },
                }
            }
            _ => At::End,
        };
        ledger.hold(tree_slot::<At, Vec<Insertion<'_>>>())?;
        insertions.entry(at).or_default().push(insertion);
    }

    let mut writer = Writer {
        source,
        prefix,
        part,
        paragraph,
        ids,
        ledger,
        marks,
        index: 0,
        out: Vec::new(),
        open: Vec::new(),
        opened: Vec::new(),
        held: Vec::new(),
        props_written: false,
        written: 0,
    };
    writer.opened = writer.ledger.vector(wrappers)?;
    writer.opened.resize(wrappers, false);
    writer.reserve(paragraph.span().len().saturating_add(256))?;
    writer.begin_paragraph()?;
    let content_end = paragraph.end.as_ref().map_or(paragraph.start.end, |end| end.start);
    let mut at = paragraph.start.end;
    let mut item_index = 0usize;
    // What the bytes before the next token follow: each gap stays with the tag or item before it.
    let mut after = After::Written;
    for (span, token) in &tokens {
        if span.start < at || span.end > content_end {
            return Err(Unpreservable::Unmapped { part: part.to_string() });
        }
        writer.gap(at..span.start, after)?;
        match *token {
            Token::Props => {
                writer.props()?;
                after = After::Written;
            }
            // A start tag is written when the content inside needs it.
            Token::Open(index) => after = After::Open(index),
            // An end tag is written when the content after needs the wrapper closed, so typing that
            // continues its last character stays inside it.
            Token::Close(index) => {
                // A hyperlink or content control whose content went with changes decided inside it
                // stays, empty; one inside a tracked change goes with what was decided around it.
                let emptied = writer.opened.get(index).is_some_and(|opened| !opened)
                    && changes_inside.get(index).copied().unwrap_or(false)
                    && paragraph.wrappers.get(index).is_some_and(|wrapper| wrapper.kind == WrapperKind::Other && !revised(paragraph, wrapper.parent));
                if emptied {
                    writer.emptied(index)?;
                } else if !writer.open.contains(&Entry::Source(index)) {
                    // Never written: what followed its start tag stays where it stood.
                    writer.release_held(&|attach| attach != Attach::Open(index))?;
                }
                after = After::Close(index);
            }
            Token::Item(item) => {
                writer.insertions(insertions.get(&At::Before(item_index)).map(Vec::as_slice))?;
                item_index = item_index.saturating_add(1);
                match item {
                    Item::Node(index) => match paragraph.nodes.get(index) {
                        Some(Node::Run(run)) if atomic.get(index).copied().unwrap_or(false) => {
                            writer.whole_run(run, own.get(index).copied().flatten().unwrap_or(KEPT))?;
                        }
                        Some(Node::Run(run)) => writer.run(index, run, &status, &insertions, &locked_children)?,
                        Some(Node::Other { name, wrapper, span }) => {
                            let item_status = own.get(index).copied().flatten().or(adopted.get(&index).copied()).unwrap_or(KEPT);
                            writer.other(name, *wrapper, span.clone(), item_status)?;
                        }
                        None => {}
                    },
                    Item::Empty(index) => {
                        let parent = paragraph.wrappers.get(index).and_then(|wrapper| wrapper.parent);
                        let item_status = adopted.get(&nodes.saturating_add(index)).copied().unwrap_or(KEPT);
                        writer.other("", parent, wrapper_span(paragraph, index), item_status)?;
                    }
                }
                after = After::Written;
            }
        }
        at = span.end;
    }
    writer.gap(at..content_end, after)?;
    writer.insertions(insertions.get(&At::End).map(Vec::as_slice))?;
    writer.end_paragraph(true)?;
    // Every insertion was placed where its character stands.
    if writer.written != inserted {
        return Err(Unpreservable::Structure);
    }
    let Writer { out, opened, ledger, .. } = writer;
    drop(opened);
    let _ = ledger;
    Ok(out)
}

/// What an element without characters of its own goes with, keyed by node (or by the node count
/// plus the wrapper's index, for a wrapper that holds no node): a content control's or smart tag's
/// properties, a proofing mark, an empty wrapper.
///
/// Inside a tracked change it takes the status of the nearest element after it (else before it) that
/// has one, inside the same wrappers, so accepting or rejecting an inserted control takes the whole
/// control, never a shell or a split. A deletion made now takes only what may sit inside a deletion;
/// what may not is decided as the deleted characters' change was, and stays. Outside tracked changes
/// an element goes into a deletion made now only when the elements on both sides went into it.
fn adopted_statuses<'a>(
    paragraph: &ScannedPara,
    tokens: &[(Range<usize>, Token)],
    own: &[Option<Status<'a>>],
    ledger: &mut Ledger<'_>,
) -> Result<HashMap<usize, Status<'a>>, Unpreservable> {
    let nodes = paragraph.nodes.len();
    // The items in order: (key, innermost wrapper around it, its own status, its name).
    let mut items: Vec<(usize, Option<usize>, Option<Status<'a>>, &str)> = ledger.vector(tokens.len())?;
    for (_, token) in tokens {
        match token {
            Token::Item(Item::Node(index)) => {
                let (wrapper, name) = match paragraph.nodes.get(*index) {
                    Some(Node::Other { name, wrapper, .. }) => (*wrapper, name.as_str()),
                    Some(node) => (node.wrapper(), ""),
                    None => (None, ""),
                };
                items.push((*index, wrapper, own.get(*index).copied().flatten(), name));
            }
            Token::Item(Item::Empty(index)) => {
                items.push((nodes.saturating_add(*index), paragraph.wrappers.get(*index).and_then(|wrapper| wrapper.parent), None, ""));
            }
            _ => {}
        }
    }
    // The nearest item with a status of its own after and before each.
    let mut after: Vec<Option<usize>> = ledger.vector(items.len())?;
    after.resize(items.len(), None);
    let mut last = None;
    for index in (0..items.len()).rev() {
        if let Some(slot) = after.get_mut(index) {
            *slot = last;
        }
        if items.get(index).is_some_and(|item| item.2.is_some()) {
            last = Some(index);
        }
    }
    let mut adopted = HashMap::new();
    let mut before = None;
    for (index, &(key, wrapper, own_status, name)) in items.iter().enumerate() {
        if own_status.is_some() {
            before = Some(index);
            continue;
        }
        let is_node = key < nodes;
        if is_node && !matches!(paragraph.nodes.get(key), Some(Node::Other { .. })) {
            continue;
        }
        let inside = |other: usize| items.get(other).is_some_and(|item| is_within(paragraph, item.1, wrapper)).then_some(other);
        let status_of = |other: Option<usize>| other.and_then(inside).and_then(|other| items.get(other)).and_then(|item| item.2);
        let (next, previous) = (status_of(after.get(index).copied().flatten()), status_of(before));
        let deletable = DELETABLE.contains(&name);
        let status = if revised(paragraph, wrapper) {
            match next.or(previous) {
                Some(Status::Keep(change)) if deletes(change) && !deletable => Some(Status::Keep(match change {
                    Change::AcceptedThenDeleted(_) => &Change::Accepted,
                    Change::RejectedThenDeleted(_) => &Change::Rejected,
                    _ => SAME,
                })),
                other => other,
            }
        } else {
            match (next, previous) {
                (Some(Status::Keep(change)), Some(other)) if deletable && deletes(change) && other == Status::Keep(change) => Some(other),
                _ => None,
            }
        };
        if let Some(status) = status {
            ledger.hold(table_slot::<(usize, Status<'_>)>())?;
            adopted.insert(key, status);
        }
    }
    Ok(adopted)
}

fn push_part(parts: &mut Vec<Part>, part: Part) {
    if let (Some(Part::Chars { child, to, .. }), Part::Chars { child: next_child, from, to: next_to }) = (parts.last_mut(), &part)
        && *child == *next_child
        && *to == *from
    {
        *to = *next_to;
        return;
    }
    parts.push(part);
}

/// The piece being built ends here; children held before the run's first character join it.
fn boundary<'a>(segments: &mut Vec<Segment<'a, '_>>, current: &mut Option<Piece<'a>>, held: &mut Vec<Part>) {
    if let Some(piece) = current.take() {
        segments.push(Segment::Run(piece));
    }
    if !held.is_empty() {
        segments.push(Segment::Run(Piece { status: KEPT, parts: std::mem::take(held) }));
    }
}

/// A part of a run with its status (`None`: a character the reader drops, which goes with what
/// surrounds it).
fn add<'a>(segments: &mut Vec<Segment<'a, '_>>, current: &mut Option<Piece<'a>>, held: &mut Vec<Part>, wanted: Option<Status<'a>>, part: Part) {
    let continues = match (&*current, &wanted) {
        (Some(_), None) => true,
        (Some(piece), Some(status)) => piece.status == *status,
        (None, _) => false,
    };
    if continues {
        if let Some(piece) = current.as_mut() {
            push_part(&mut piece.parts, part);
        }
        return;
    }
    let Some(status) = wanted else {
        push_part(held, part);
        return;
    };
    if let Some(done) = current.take() {
        segments.push(Segment::Run(done));
    }
    let mut parts = std::mem::take(held);
    push_part(&mut parts, part);
    *current = Some(Piece { status, parts });
}

struct Writer<'a, 'l, 'b> {
    source: &'a Source<'a>,
    prefix: &'a str,
    part: &'a str,
    paragraph: &'a ScannedPara,
    ids: &'a mut Ids,
    ledger: &'l mut Ledger<'b>,
    /// This save's new paragraph marks in order: paragraph `index` ends with `marks[index]` while it is not the last.
    marks: Vec<&'a Revision>,
    index: usize,
    out: Vec<u8>,
    open: Vec<Entry<'a>>,
    /// Source wrappers opened at least once: a second opening is a split.
    opened: Vec<bool>,
    /// Bytes after a start or end tag not written yet, in order: each is written right after its
    /// tag, or where it stood when the tag is never written.
    held: Vec<(Range<usize>, Attach)>,
    /// The source's properties were written with its own mark.
    props_written: bool,
    /// Insertions written.
    written: usize,
}

impl<'a> Writer<'a, '_, '_> {
    fn w(&self, local: &str) -> String {
        format!("{}:{local}", self.prefix)
    }

    fn locked(&self) -> Unpreservable {
        Unpreservable::Locked { part: self.part.to_string() }
    }

    fn unmapped(&self) -> Unpreservable {
        Unpreservable::Unmapped { part: self.part.to_string() }
    }

    /// Room for `more` bytes, charged before it is made: the old room and the new are both held
    /// while the bytes move.
    fn reserve(&mut self, more: usize) -> Result<(), Unpreservable> {
        let needed = self.out.len().saturating_add(more);
        let capacity = self.out.capacity();
        if needed <= capacity {
            return Ok(());
        }
        let room = needed.max(capacity.saturating_mul(2));
        self.ledger.hold(units(room))?;
        self.out.reserve_exact(room.saturating_sub(self.out.len()));
        self.ledger.release(units(capacity));
        let actual = self.out.capacity();
        if actual > room {
            self.ledger.hold(units(actual - room))?;
        }
        Ok(())
    }

    fn push(&mut self, text: &str) -> Result<(), Unpreservable> {
        self.ledger.work(units(text.len()) / BYTES_PER_UNIT_COPIED + 1)?;
        self.reserve(text.len())?;
        self.out.extend_from_slice(text.as_bytes());
        Ok(())
    }

    fn copy(&mut self, range: Range<usize>) -> Result<(), Unpreservable> {
        let source = self.source;
        let length = source.length(range.clone()).map_err(|_| self.unmapped())?;
        self.ledger.work(units(length) / BYTES_PER_UNIT_COPIED + 1)?;
        self.reserve(length)?;
        let out = &mut self.out;
        source.each(range, |piece| out.extend_from_slice(piece)).map_err(|_| self.unmapped())
    }

    fn tag(&self, range: Range<usize>) -> Result<&'a str, Unpreservable> {
        let source = self.source;
        source.text(range).ok_or_else(|| self.unmapped())
    }

    /// Bytes between tags, after `after`: written now when what they follow is written, else held
    /// until it is.
    fn gap(&mut self, gap: Range<usize>, after: After) -> Result<(), Unpreservable> {
        if gap.is_empty() {
            return Ok(());
        }
        let attach = match after {
            After::Written => None,
            After::Open(index) => (!self.open.contains(&Entry::Source(index))).then_some(Attach::Open(index)),
            After::Close(index) => self.open.contains(&Entry::Source(index)).then_some(Attach::Close(index)),
        };
        match attach {
            None => self.copy(gap),
            Some(attach) => {
                self.ledger.hold(vector_slot::<(Range<usize>, Attach)>())?;
                self.held.push((gap, attach));
                Ok(())
            }
        }
    }

    /// Write the held gaps `keep` does not keep back, in order.
    fn release_held(&mut self, keep: &dyn Fn(Attach) -> bool) -> Result<(), Unpreservable> {
        if self.held.iter().all(|(_, attach)| keep(*attach)) {
            return Ok(());
        }
        let held = std::mem::take(&mut self.held);
        let mut kept = Vec::new();
        let mut result = Ok(());
        for (gap, attach) in held {
            if keep(attach) || result.is_err() {
                kept.push((gap, attach));
                continue;
            }
            self.ledger.release(vector_slot::<(Range<usize>, Attach)>());
            result = self.copy(gap);
        }
        self.held = kept;
        result
    }

    /// A new tracked change's start tag. The session is recorded as it is or not at all: a change
    /// with no date, or an author XML cannot hold as given, is refused (6C-48 M3).
    fn change_tag(&mut self, kind: &str, revision: &Revision, empty: bool) -> Result<String, Unpreservable> {
        if revision.date.is_empty() {
            return Err(Unpreservable::Verification("a tracked change of this session has no date".into()));
        }
        if !revision.author.chars().chain(revision.date.chars()).all(xml_char) {
            return Err(Unpreservable::Verification(
                "the name or date of this session's tracked changes holds a character a Word file cannot hold".into(),
            ));
        }
        let id = self.ids.revision()?;
        Ok(format!(
            "<{name} {id_name}=\"{id}\" {author_name}=\"{author}\" {date_name}=\"{date}\"{end}",
            name = self.w(kind),
            id_name = self.w("id"),
            author_name = self.w("author"),
            date_name = self.w("date"),
            author = esc(&revision.author),
            date = esc(&revision.date),
            end = if empty { "/>" } else { ">" },
        ))
    }

    fn begin_paragraph(&mut self) -> Result<(), Unpreservable> {
        let paragraph = self.paragraph;
        let start = open_tag(self.tag(paragraph.start.clone())?, paragraph.end.is_none());
        match self.marks.get(self.index).copied() {
            // The source's own mark: its tags and properties as written (where they stand when the
            // paragraph is not split).
            None => {
                self.push(&start)?;
                if self.index > 0
                    && let Some(props) = &paragraph.ppr
                {
                    self.copy(props_span(props))?;
                    self.props_written = true;
                }
            }
            Some(mark) => {
                let (paragraph_id, text_id) = (self.ids.paragraph()?, self.ids.paragraph()?);
                let tag = retag(&start, &[("paraId", paragraph_id), ("textId", text_id)]);
                self.push(&tag)?;
                self.mark_props(mark)?;
            }
        }
        Ok(())
    }

    /// The source's properties where they stand: written there only while the paragraph being
    /// written ends with the source's own mark.
    fn props(&mut self) -> Result<(), Unpreservable> {
        if self.index == self.marks.len()
            && !self.props_written
            && let Some(props) = &self.paragraph.ppr
        {
            self.copy(props_span(props))?;
            self.props_written = true;
        }
        Ok(())
    }

    /// A paragraph ending with a new mark: the source's properties without its section (which stays
    /// with the source's own mark), its formatting change with a fresh id (as Word copies it to the
    /// paragraph Enter makes, 6C-24), the mark inserted.
    fn mark_props(&mut self, mark: &Revision) -> Result<(), Unpreservable> {
        let insertion = self.change_tag("ins", mark, true)?;
        let paragraph = self.paragraph;
        let Some(props) = &paragraph.ppr else {
            let text = format!("<{ppr}><{rpr}>{insertion}</{rpr}></{ppr}>", ppr = self.w("pPr"), rpr = self.w("rPr"));
            return self.push(&text);
        };
        let new_rpr = format!("<{rpr}>{insertion}</{rpr}>", rpr = self.w("rPr"));
        self.push(&open_tag(self.tag(props.start.clone())?, props.end.is_none()))?;
        let mut at = props.start.end;
        let mut wrote_rpr = false;
        for (name, span) in &props.children {
            self.copy(at..span.start)?;
            at = span.end;
            match name.as_str() {
                "w:sectPr" => {}
                "w:rPr" if !wrote_rpr => {
                    wrote_rpr = true;
                    match props.rpr.as_deref() {
                        Some(rpr) => self.mark_rpr(rpr, &insertion)?,
                        None => self.push(&new_rpr)?,
                    }
                }
                "w:pPrChange" => {
                    // The mark's properties stand before the tracked change of the paragraph's.
                    if !wrote_rpr {
                        wrote_rpr = true;
                        self.push(&new_rpr)?;
                    }
                    let source = self.source;
                    let tag_end = start_tag_end(source.bytes, span).ok_or_else(|| self.unmapped())?;
                    let id = self.ids.revision()?;
                    let tag = retag(self.tag(span.start..tag_end)?, &[("id", id)]);
                    self.push(&tag)?;
                    self.copy(tag_end..span.end)?;
                }
                _ => self.copy(span.clone())?,
            }
        }
        if !wrote_rpr {
            self.push(&new_rpr)?;
        }
        if let Some(end) = &props.end {
            self.copy(at..end.start)?;
        }
        self.push(&format!("</{}>", props.raw_name))
    }

    /// The mark's run properties with this session's insertion first and without the source mark's
    /// own tracked changes.
    fn mark_rpr(&mut self, rpr: &Props, insertion: &str) -> Result<(), Unpreservable> {
        self.push(&open_tag(self.tag(rpr.start.clone())?, rpr.end.is_none()))?;
        self.push(insertion)?;
        let mut at = rpr.start.end;
        for (child, span) in &rpr.children {
            self.copy(at..span.start)?;
            if !MARK_CHANGES.contains(&child.as_str()) {
                self.copy(span.clone())?;
            }
            at = span.end;
        }
        if let Some(end) = &rpr.end {
            self.copy(at..end.start)?;
        }
        self.push(&format!("</{}>", rpr.raw_name))
    }

    fn end_paragraph(&mut self, last: bool) -> Result<(), Unpreservable> {
        self.sync(&[])?;
        let paragraph = self.paragraph;
        match (&paragraph.end, last) {
            (Some(end), true) => self.copy(end.clone()),
            _ => self.push(&format!("</{}>", paragraph.raw_name)),
        }
    }

    /// The wrappers an item of the source is written in: what stood before it in the source is
    /// written before it.
    fn sync(&mut self, target: &[Entry<'a>]) -> Result<(), Unpreservable> {
        self.settle(target)?;
        let open: Vec<Entry<'a>> = self.open.clone();
        self.release_held(&|attach| matches!(attach, Attach::Close(index) if open.contains(&Entry::Source(index))))
    }

    /// Close what `target` leaves and open what it enters, each tag with the bytes after it.
    fn settle(&mut self, target: &[Entry<'a>]) -> Result<(), Unpreservable> {
        let common = self.open.iter().zip(target).take_while(|(open, wanted)| open == wanted).count();
        while self.open.len() > common {
            if let Some(entry) = self.open.pop() {
                self.close(entry)?;
            }
        }
        for entry in target.iter().skip(common) {
            self.open_entry(*entry)?;
            self.open.push(*entry);
        }
        Ok(())
    }

    /// A source wrapper whose content went, written empty where it stood.
    fn emptied(&mut self, index: usize) -> Result<(), Unpreservable> {
        let paragraph = self.paragraph;
        let Some(wrapper) = paragraph.wrappers.get(index) else { return Err(self.unmapped()) };
        let Some(end) = wrapper.end.clone() else { return Err(self.unmapped()) };
        self.sync_chain(wrapper.parent, SAME)?;
        if let Some(slot) = self.opened.get_mut(index) {
            *slot = true;
        }
        self.copy(wrapper.start.clone())?;
        self.release_held(&|attach| attach != Attach::Open(index))?;
        self.copy(end)
    }

    fn close(&mut self, entry: Entry<'a>) -> Result<(), Unpreservable> {
        match entry {
            Entry::Source(index) => match self.paragraph.wrappers.get(index).and_then(|wrapper| wrapper.end.clone()) {
                Some(end) => {
                    self.copy(end)?;
                    self.release_held(&|attach| attach != Attach::Close(index))
                }
                None => Err(self.unmapped()),
            },
            Entry::New { deleted, .. } => {
                let text = format!("</{}>", self.w(if deleted { "del" } else { "ins" }));
                self.push(&text)
            }
        }
    }

    fn open_entry(&mut self, entry: Entry<'a>) -> Result<(), Unpreservable> {
        match entry {
            Entry::Source(index) => {
                let paragraph = self.paragraph;
                let Some(wrapper) = paragraph.wrappers.get(index) else { return Err(self.unmapped()) };
                let tag = self.tag(wrapper.start.clone())?;
                let first = self.opened.get(index).is_some_and(|opened| !opened);
                if first {
                    if let Some(slot) = self.opened.get_mut(index) {
                        *slot = true;
                    }
                    // What stood before its start tag after a tag never written goes first.
                    let start = wrapper.start.start;
                    let open = self.open.clone();
                    self.release_held(&|attach| match attach {
                        Attach::Open(other) => {
                            open.contains(&Entry::Source(other)) || paragraph.wrappers.get(other).is_none_or(|other| other.start.start >= start)
                        }
                        Attach::Close(other) => open.contains(&Entry::Source(other)),
                    })?;
                    self.push(tag)?;
                    self.release_held(&|attach| attach != Attach::Open(index))
                } else if wrapper.kind == WrapperKind::Other {
                    Err(Unpreservable::SplitWrapper { part: self.part.to_string() })
                } else {
                    // The rest of a split tracked change: the same change, a fresh id.
                    let id = self.ids.revision()?;
                    self.push(&retag(tag, &[("id", id)]))
                }
            }
            Entry::New { deleted, revision } => {
                let tag = self.change_tag(if deleted { "del" } else { "ins" }, revision, false)?;
                self.push(&tag)
            }
        }
    }

    /// The wrappers content inside `innermost` stands in once its change is applied: an accepted
    /// insertion leaves its `w:ins`, a rejected deletion its `w:del`, a deletion made now is the
    /// session's `w:del`, innermost.
    fn target(&self, innermost: Option<usize>, change: &'a Change) -> Result<Vec<Entry<'a>>, Unpreservable> {
        let paragraph = self.paragraph;
        let chain = paragraph.chain(innermost);
        let kind = |index: &usize| paragraph.wrappers.get(*index).map(|wrapper| wrapper.kind);
        let (leaves, deletion) = match change {
            Change::Same => (None, None),
            Change::Accepted => (Some(WrapperKind::Inserted), None),
            Change::Rejected => (Some(WrapperKind::Deleted), None),
            Change::Deleted(revision) => {
                // Deleted text is deleted again only after its deletion was rejected (6C-44).
                if chain.iter().any(|index| kind(index) == Some(WrapperKind::Deleted)) {
                    return Err(Unpreservable::Altered);
                }
                (None, Some(revision))
            }
            Change::AcceptedThenDeleted(revision) => (Some(WrapperKind::Inserted), Some(revision)),
            Change::RejectedThenDeleted(revision) => (Some(WrapperKind::Deleted), Some(revision)),
        };
        let mut entries: Vec<Entry<'a>> =
            chain.iter().filter(|index| leaves.is_none() || kind(index) != leaves).map(|index| Entry::Source(*index)).collect();
        if let Some(revision) = deletion {
            entries.push(Entry::New { deleted: true, revision });
        }
        Ok(entries)
    }

    fn sync_chain(&mut self, innermost: Option<usize>, change: &'a Change) -> Result<(), Unpreservable> {
        let target = self.target(innermost, change)?;
        self.sync(&target)
    }

    /// An element written whole (a bookmark, a content control's properties, a field written as one
    /// element, an equation), or a wrapper that holds no node.
    fn other(&mut self, name: &str, innermost: Option<usize>, span: Range<usize>, status: Status<'a>) -> Result<(), Unpreservable> {
        // Gone with its decided change.
        let Status::Keep(change) = status else { return Ok(()) };
        if deletes(change) && !DELETABLE.contains(&name) {
            return Err(self.locked());
        }
        self.sync_chain(innermost, change)?;
        self.copy(span)
    }

    /// A run's start tag and properties: for its first piece with what stands before them, for the
    /// others the properties alone.
    fn run_open(&mut self, run: &RunNode, first: bool) -> Result<(), Unpreservable> {
        self.push(&open_tag(self.tag(run.start.clone())?, run.end.is_none()))?;
        match (run.rpr.clone(), run.rpr_change.clone()) {
            // What stands before the properties, unless a child does (it is written with its piece).
            (Some(rpr), _) if first && Self::properties_first(run) => self.copy(run.start.end..rpr.end),
            (Some(rpr), _) if first => self.copy(rpr),
            // A later piece's copy of a formatting change is a change of its own: a fresh id
            // (6C-48 M4).
            (Some(rpr), Some(change)) => {
                let source = self.source;
                let tag_end = start_tag_end(source.bytes, &change).ok_or_else(|| self.unmapped())?;
                self.copy(rpr.start..change.start)?;
                let id = self.ids.revision()?;
                let tag = retag(self.tag(change.start..tag_end)?, &[("id", id)]);
                self.push(&tag)?;
                self.copy(tag_end..rpr.end)
            }
            (Some(rpr), None) => self.copy(rpr),
            (None, _) => Ok(()),
        }
    }

    fn run_close(&mut self, run: &RunNode) -> Result<(), Unpreservable> {
        match run.end.clone() {
            Some(end) => self.copy(end),
            None => self.push(&format!("</{}>", run.raw_name)),
        }
    }

    /// A run that is part of an object (a field's runs, a drawing's): all of it takes one status.
    fn whole_run(&mut self, run: &RunNode, status: Status<'a>) -> Result<(), Unpreservable> {
        let Status::Keep(change) = status else { return Ok(()) };
        self.sync_chain(run.wrapper, change)?;
        let rename = renaming(change);
        if rename.is_none() {
            return self.copy(run.span());
        }
        self.run_open(run, true)?;
        for (index, child) in run.children.iter().enumerate() {
            self.gap_before(run, index)?;
            self.child_whole(child, rename)?;
        }
        self.gap_after(run)?;
        self.run_close(run)
    }

    /// Where the bytes before child `index` of a run begin: after the child before it, or the run's
    /// properties, or its start tag. `None` when the properties are not first.
    fn child_gap(run: &RunNode, index: usize) -> Option<Range<usize>> {
        if !Self::properties_first(run) {
            return None;
        }
        let start = run.children.get(index)?.span.start;
        let previous = match index.checked_sub(1) {
            Some(before) => run.children.get(before)?.span.end,
            None => run.rpr.as_ref().map_or(run.start.end, |rpr| rpr.end),
        };
        (previous <= start).then_some(previous..start)
    }

    fn gap_before(&mut self, run: &RunNode, index: usize) -> Result<(), Unpreservable> {
        match Self::child_gap(run, index) {
            Some(gap) => self.copy(gap),
            None => Ok(()),
        }
    }

    /// Whether a run's properties stand before its children, so the bytes between its children are
    /// only white space, comments and processing instructions. Otherwise they are not copied: they
    /// would hold the properties, which every piece writes.
    fn properties_first(run: &RunNode) -> bool {
        run.rpr.as_ref().is_none_or(|rpr| run.children.first().is_none_or(|child| rpr.end <= child.span.start))
    }

    fn gap_after(&mut self, run: &RunNode) -> Result<(), Unpreservable> {
        let (Some(last), Some(end)) = (run.children.last(), &run.end) else { return Ok(()) };
        if !Self::properties_first(run) {
            return Ok(());
        }
        if last.span.end <= end.start { self.copy(last.span.end..end.start) } else { Ok(()) }
    }

    /// A child copied whole, renamed when a deletion is added (`w:t` → `w:delText`, `w:instrText`
    /// → `w:delInstrText`) or rejected (the reverse).
    fn child_whole(&mut self, child: &Child, rename: Option<bool>) -> Result<(), Unpreservable> {
        let (start, end, raw_name) = match &child.kind {
            ChildKind::Text { start, end, raw_name, .. } | ChildKind::Other { start, end, raw_name } => (start, end, raw_name),
            ChildKind::Single { .. } => return self.copy(child.span.clone()),
        };
        let new_local = match (local(raw_name), rename) {
            ("t", Some(true)) => "delText",
            ("delText", Some(false)) => "t",
            ("instrText", Some(true)) => "delInstrText",
            ("delInstrText", Some(false)) => "instrText",
            _ => return self.copy(child.span.clone()),
        };
        let name = with_local(raw_name, new_local);
        self.push(&rename_tag(self.tag(start.clone())?, raw_name, &name))?;
        if let Some(end) = end {
            self.copy(start.end..end.start)?;
            self.push(&format!("</{name}>"))?;
        }
        Ok(())
    }

    fn run(
        &mut self,
        node: usize,
        run: &RunNode,
        status: &[Status<'a>],
        insertions: &BTreeMap<At, Vec<Insertion<'a>>>,
        locked: &HashMap<(usize, usize), Status<'a>>,
    ) -> Result<(), Unpreservable> {
        let inside = |child: usize, raw: usize| insertions.get(&At::Child { node, child, raw });
        let status_of = |unit: Option<usize>| unit.and_then(|unit| status.get(unit)).copied();
        let touched = run.children.iter().enumerate().any(|(index, child)| match &child.kind {
            ChildKind::Text { chars, .. } => {
                chars.iter().enumerate().any(|(raw, character)| inside(index, raw).is_some() || status_of(character.unit).is_some_and(|s| s != KEPT))
            }
            ChildKind::Single { unit } => inside(index, 0).is_some() || status_of(*unit).is_some_and(|s| s != KEPT),
            ChildKind::Other { .. } => inside(index, 0).is_some(),
        });
        if !touched {
            self.sync_chain(run.wrapper, SAME)?;
            return self.copy(run.span());
        }
        // The pieces hold at most one part per character and child.
        let characters = run.children.iter().fold(0usize, |sum, child| match &child.kind {
            ChildKind::Text { chars, .. } => sum.saturating_add(chars.len()),
            _ => sum.saturating_add(1),
        });
        let charge = units(characters.saturating_add(1)).saturating_mul(vector_slot::<Segment<'a, '_>>().saturating_add(vector_slot::<Part>()));
        self.ledger.hold(charge)?;
        let mut segments: Vec<Segment<'a, '_>> = Vec::new();
        let mut current: Option<Piece<'a>> = None;
        let mut held: Vec<Part> = Vec::new();
        for (index, child) in run.children.iter().enumerate() {
            if let Some(list) = inside(index, 0).filter(|_| !matches!(child.kind, ChildKind::Text { .. })) {
                boundary(&mut segments, &mut current, &mut held);
                segments.push(Segment::Insert(list));
            }
            // Whatever is locked where it stands keeps its status; a child that is no character
            // (a field character, a drawing, a page break Word rendered) never changes.
            let kept = locked.get(&(node, index)).copied();
            match &child.kind {
                ChildKind::Text { chars, .. } => {
                    for (raw, character) in chars.iter().enumerate() {
                        if let Some(list) = inside(index, raw) {
                            boundary(&mut segments, &mut current, &mut held);
                            segments.push(Segment::Insert(list));
                        }
                        let wanted = status_of(character.unit).or(kept);
                        add(&mut segments, &mut current, &mut held, wanted, Part::Chars { child: index, from: raw, to: raw.saturating_add(1) });
                    }
                }
                ChildKind::Single { unit } => {
                    add(&mut segments, &mut current, &mut held, Some(status_of(*unit).or(kept).unwrap_or(KEPT)), Part::Whole(index));
                }
                ChildKind::Other { .. } => add(&mut segments, &mut current, &mut held, Some(kept.unwrap_or(KEPT)), Part::Whole(index)),
            }
        }
        boundary(&mut segments, &mut current, &mut held);
        let mut first = true;
        let mut result = Ok(());
        for segment in &segments {
            result = match segment {
                Segment::Insert(list) => self.insertions(Some(*list)),
                Segment::Run(piece) => {
                    let written = self.run_piece(run, piece, first);
                    first &= !matches!(piece.status, Status::Keep(_));
                    written
                }
            };
            if result.is_err() {
                break;
            }
        }
        drop(segments);
        self.ledger.release(charge);
        result
    }

    fn run_piece(&mut self, run: &RunNode, piece: &Piece<'a>, first: bool) -> Result<(), Unpreservable> {
        let Status::Keep(change) = piece.status else { return Ok(()) };
        self.sync_chain(run.wrapper, change)?;
        let rename = renaming(change);
        self.run_open(run, first)?;
        let last_child = run.children.len().saturating_sub(1);
        for part in &piece.parts {
            match part {
                Part::Whole(index) => {
                    self.gap_before(run, *index)?;
                    if let Some(child) = run.children.get(*index) {
                        self.child_whole(child, rename)?;
                    }
                    if *index == last_child {
                        self.gap_after(run)?;
                    }
                }
                Part::Chars { child, from, to } => {
                    let Some(whole) = run.children.get(*child) else { continue };
                    let ChildKind::Text { chars, raw_name, start, .. } = &whole.kind else { continue };
                    let ends = *to == chars.len();
                    if *from == 0 {
                        self.gap_before(run, *child)?;
                    }
                    if *from == 0 && ends {
                        self.child_whole(whole, rename)?;
                    } else {
                        let name = match (local(raw_name), rename) {
                            ("t", Some(true)) => with_local(raw_name, "delText"),
                            ("delText", Some(false)) => with_local(raw_name, "t"),
                            _ => raw_name.clone(),
                        };
                        // The source's start tag, its attributes kept, its spaces kept (6C-48 M5).
                        let tag = preserving_space(&rename_tag(self.tag(start.clone())?, raw_name, &name));
                        self.push(&tag)?;
                        // Characters written next to each other are copied together.
                        let mut stretch: Option<Range<usize>> = None;
                        for character in chars.get(*from..*to).unwrap_or_default() {
                            match &mut stretch {
                                Some(range) if range.end == character.bytes.start => range.end = character.bytes.end,
                                _ => {
                                    if let Some(range) = stretch.replace(character.bytes.clone()) {
                                        self.copy(range)?;
                                    }
                                }
                            }
                        }
                        if let Some(range) = stretch {
                            self.copy(range)?;
                        }
                        self.push(&format!("</{name}>"))?;
                    }
                    if ends && *child == last_child {
                        self.gap_after(run)?;
                    }
                }
            }
        }
        self.run_close(run)
    }

    fn insertions(&mut self, list: Option<&[Insertion<'a>]>) -> Result<(), Unpreservable> {
        for insertion in list.into_iter().flatten() {
            self.written = self.written.saturating_add(1);
            match insertion {
                Insertion::Mark => {
                    self.end_paragraph(false)?;
                    self.index = self.index.saturating_add(1);
                    self.begin_paragraph()?;
                }
                Insertion::Text { text, revision, anchor } => self.typed(text, revision, *anchor)?,
            }
        }
        Ok(())
    }

    /// The wrappers and run properties typing continued from.
    fn anchor(&self, anchor: Option<usize>) -> Result<(Option<usize>, RunProps), Unpreservable> {
        let paragraph = self.paragraph;
        let of_node = |node: usize| -> Option<(Option<usize>, RunProps)> {
            match paragraph.nodes.get(node)? {
                Node::Run(run) => {
                    let props = match (run.rpr.clone(), run.rpr_change.clone()) {
                        (Some(rpr), Some(change)) => vec![rpr.start..change.start, change.end..rpr.end],
                        (Some(rpr), None) => vec![rpr],
                        (None, _) => Vec::new(),
                    };
                    Some((run.wrapper, RunProps::Copy(props)))
                }
                Node::Other { wrapper, .. } => Some((*wrapper, RunProps::None)),
            }
        };
        let Some(unit) = anchor else { return Ok((None, RunProps::Mark)) };
        let found = match paragraph.units.get(unit).map(|unit| &unit.loc) {
            None => return Err(Unpreservable::Structure),
            Some(Loc::Text { node, .. } | Loc::Single { node, .. } | Loc::Marker { node }) => of_node(*node),
            Some(Loc::Atomic { first, .. }) => of_node(*first),
            // A comment reference, ruby or a page number inside a run: that run.
            Some(Loc::Locked { node: Some(node), child: Some(_) }) => of_node(*node),
            Some(Loc::Outside { .. }) => Some((None, RunProps::None)),
            // A field written as one element, or anything inside something copied whole.
            Some(Loc::Locked { .. }) => None,
        };
        found.ok_or_else(|| self.locked())
    }

    fn typed(&mut self, text: &str, revision: &'a Revision, anchor: Option<usize>) -> Result<(), Unpreservable> {
        let content: Vec<Typed> = text
            .chars()
            .map(typed_char)
            .collect::<Option<_>>()
            .ok_or_else(|| Unpreservable::Verification("typed text holds a character a Word file cannot hold".into()))?;
        let (innermost, props) = self.anchor(anchor)?;
        // Inside the hyperlink or content control typing continued in; never inside a tracked change.
        let paragraph = self.paragraph;
        let mut target: Vec<Entry<'a>> = paragraph
            .chain(innermost)
            .into_iter()
            .take_while(|index| paragraph.wrappers.get(*index).is_some_and(|wrapper| wrapper.kind == WrapperKind::Other))
            .map(Entry::Source)
            .collect();
        target.push(Entry::New { deleted: false, revision });
        self.settle(&target)?;
        self.push(&format!("<{}>", self.w("r")))?;
        match props {
            RunProps::None => {}
            RunProps::Copy(ranges) => {
                for range in ranges {
                    self.copy(range)?;
                }
            }
            RunProps::Mark => self.mark_properties_as_run_properties()?,
        }
        self.content(&content)?;
        self.push(&format!("</{}>", self.w("r")))
    }

    /// The mark's run properties for typed text: without the mark's own tracked changes (its
    /// insertion or deletion, its formatting change).
    fn mark_properties_as_run_properties(&mut self) -> Result<(), Unpreservable> {
        let paragraph = self.paragraph;
        let Some(rpr) = paragraph.ppr.as_ref().and_then(|props| props.rpr.as_deref()) else { return Ok(()) };
        if rpr.children.iter().all(|(name, _)| MARK_CHANGES.contains(&name.as_str())) {
            return Ok(());
        }
        self.push(&open_tag(self.tag(rpr.start.clone())?, rpr.end.is_none()))?;
        for (name, span) in &rpr.children {
            if !MARK_CHANGES.contains(&name.as_str()) {
                self.copy(span.clone())?;
            }
        }
        self.push(&format!("</{}>", rpr.raw_name))
    }

    /// Typed characters as run content: text in `w:t`, the rest as their elements.
    fn content(&mut self, content: &[Typed]) -> Result<(), Unpreservable> {
        let mut plain = String::new();
        for typed in content {
            match typed {
                Typed::Text(ch) => plain.push(*ch),
                Typed::Element(name, kind) => {
                    self.text_run(&mut plain)?;
                    let tag = match kind {
                        Some(kind) => format!("<{} {}=\"{kind}\"/>", self.w(name), self.w("type")),
                        None => format!("<{}/>", self.w(name)),
                    };
                    self.push(&tag)?;
                }
            }
        }
        self.text_run(&mut plain)
    }

    fn text_run(&mut self, plain: &mut String) -> Result<(), Unpreservable> {
        if plain.is_empty() {
            return Ok(());
        }
        let tag = format!("<{t} xml:space=\"preserve\">{}</{t}>", esc(plain), t = self.w("t"));
        plain.clear();
        self.push(&tag)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use std::collections::{BTreeMap, BTreeSet, HashSet};
    use std::ops::Range;

    use super::*;
    use crate::inspect::budget::{Budget, units};
    use crate::inspect::{MAX_MEMORY, MAX_WORK};
    use crate::package::{Package, ReadLimits, zip_entries};
    use crate::preserve::plan::{Change, Revision, Step};
    use crate::preserve::scan::{Loc, ScannedPara, Unit, WrapperKind, scan_package};
    use wordcraft_doc::{CharProps, Document, Paragraph, Revision as Recorded, RevisionKind};

    const W: &str =
        r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml""#;

    fn sari() -> Revision {
        Revision { author: "Sari Wulan".into(), date: "2026-10-10T08:00:00Z".into() }
    }

    const SARI_ATTRIBUTES: &str = r#"w:author="Sari Wulan" w:date="2026-10-10T08:00:00Z""#;

    /// The body's first paragraph, scanned, with its part's bytes.
    fn scanned(paragraph: &str) -> (Vec<u8>, ScannedPara) {
        scanned_at(paragraph, 0)
    }

    /// The body's paragraph at `index`, scanned, with its part's bytes.
    fn scanned_at(body: &str, index: u32) -> (Vec<u8>, ScannedPara) {
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let document = format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#);
        let bytes = zip_entries(&[("_rels/.rels".into(), rels.as_bytes().to_vec()), ("word/document.xml".into(), document.into_bytes())]).unwrap();
        let pkg = Package::open_with(&bytes, &ReadLimits::default()).unwrap();
        let wanted = BTreeMap::from([("word/document.xml".to_string(), BTreeSet::from([index]))]);
        let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
        let mut part = scan_package(&pkg, &wanted, &mut budget).unwrap().remove("word/document.xml").unwrap();
        let paragraph = part.paragraphs.remove(&index).unwrap();
        (part.bytes.to_vec(), paragraph)
    }

    /// `rewrite` with a save's budget of its own: what stays held is exactly the output's room, and
    /// nothing once it refuses.
    fn rewritten(paragraph: &str, steps: &[Step], used: &[u32]) -> Result<String, Unpreservable> {
        rewritten_at(paragraph, 0, steps, used)
    }

    fn rewritten_at(body: &str, index: u32, steps: &[Step], used: &[u32]) -> Result<String, Unpreservable> {
        let (bytes, scanned) = scanned_at(body, index);
        let none = BTreeMap::new();
        let mut ids = Ids::new(10, used.iter().copied().collect());
        let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
        let out = rewrite(&Source { bytes: &bytes, rewritten: &none }, "w", "word/document.xml", &scanned, steps, &mut ids, &mut budget);
        match &out {
            Ok(out) => assert_eq!(budget.held(), held_by(out)),
            Err(_) => assert_eq!(budget.held(), 0),
        }
        Ok(String::from_utf8(out?).unwrap())
    }

    fn keep(range: Range<usize>, change: Change) -> Vec<Step> {
        range.map(|unit| Step::Keep { unit, change: change.clone() }).collect()
    }

    #[test]
    fn types_into_a_run_with_its_properties_but_not_its_formatting_change() {
        let change = r#"<w:rPrChange w:id="9" w:author="Ana Setia" w:date="2026-10-01T09:00:00Z"><w:rPr/></w:rPrChange>"#;
        let source = format!(r#"<w:p><w:r><w:rPr><w:b/>{change}</w:rPr><w:t>Rent due.</w:t></w:r></w:p>"#);
        let steps =
            [keep(0..4, Change::Same), vec![Step::Insert { text: " is".into(), revision: sari(), anchor: Some(3) }], keep(4..9, Change::Same)]
                .concat();
        assert_eq!(
            rewritten(&source, &steps, &[]).unwrap(),
            format!(
                r#"<w:p><w:r><w:rPr><w:b/>{change}</w:rPr><w:t xml:space="preserve">Rent</w:t></w:r><w:ins w:id="11" {SARI_ATTRIBUTES}><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve"> is</w:t></w:r></w:ins><w:r><w:rPr><w:b/>{again}</w:rPr><w:t xml:space="preserve"> due.</w:t></w:r></w:p>"#,
                // 6C-48 M4 (amends the brief): a copy of the formatting change after the first has an id of its own.
                again = change.replace(r#"w:id="9""#, r#"w:id="12""#)
            )
        );
    }

    #[test]
    fn deletes_inside_another_author_s_insertion_without_leaving_it() {
        let source = r#"<w:p><w:ins w:id="5" w:author="Ana Setia" w:date="2026-10-01T09:00:00Z"><w:r><w:t>Rent is due</w:t></w:r></w:ins></w:p>"#;
        let steps = [keep(0..5, Change::Same), keep(5..7, Change::Deleted(sari())), keep(7..11, Change::Same)].concat();
        assert_eq!(
            rewritten(source, &steps, &[]).unwrap(),
            format!(
                r#"<w:p><w:ins w:id="5" w:author="Ana Setia" w:date="2026-10-01T09:00:00Z"><w:r><w:t xml:space="preserve">Rent </w:t></w:r><w:del w:id="11" {SARI_ATTRIBUTES}><w:r><w:delText xml:space="preserve">is</w:delText></w:r></w:del><w:r><w:t xml:space="preserve"> due</w:t></w:r></w:ins></w:p>"#
            )
        );
    }

    #[test]
    fn accepts_the_middle_of_an_insertion_and_reopens_the_rest_with_a_fresh_id() {
        let source = r#"<w:p><w:ins w:id="5" w:author="Ana" w:date="A"><w:r><w:t>ab</w:t></w:r><w:r><w:t>cd</w:t></w:r><w:r><w:t>ef</w:t></w:r></w:ins></w:p>"#;
        let steps = [keep(0..2, Change::Same), keep(2..4, Change::Accepted), keep(4..6, Change::Same)].concat();
        assert_eq!(
            rewritten(source, &steps, &[]).unwrap(),
            r#"<w:p><w:ins w:id="5" w:author="Ana" w:date="A"><w:r><w:t>ab</w:t></w:r></w:ins><w:r><w:t>cd</w:t></w:r><w:ins w:id="11" w:author="Ana" w:date="A"><w:r><w:t>ef</w:t></w:r></w:ins></w:p>"#
        );
    }

    #[test]
    fn splits_a_paragraph_with_a_new_mark_and_fresh_paragraph_ids() {
        let props = r#"<w:pPr><w:pStyle w:val="Clause"/><w:rPr><w:b/></w:rPr><w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr></w:pPr>"#;
        let source = format!(r#"<w:p w14:paraId="00000001" w:rsidR="00A1">{props}<w:r><w:t>One. Two.</w:t></w:r></w:p>"#);
        let steps = [keep(0..4, Change::Same), vec![Step::Break { revision: sari() }], keep(4..9, Change::Same)].concat();
        assert_eq!(
            rewritten(&source, &steps, &[1]).unwrap(),
            format!(
                r#"<w:p w14:paraId="00000002" w:rsidR="00A1"><w:pPr><w:pStyle w:val="Clause"/><w:rPr><w:ins w:id="11" {SARI_ATTRIBUTES}/><w:b/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">One.</w:t></w:r></w:p><w:p w14:paraId="00000001" w:rsidR="00A1">{props}<w:r><w:t xml:space="preserve"> Two.</w:t></w:r></w:p>"#
            )
        );
    }

    #[test]
    fn rejects_a_deletion_back_to_text_and_drops_a_rejected_insertion() {
        let source = r#"<w:p><w:del w:id="3" w:author="Ana" w:date="A"><w:r><w:delText>old </w:delText></w:r></w:del><w:ins w:id="4" w:author="Ana" w:date="A"><w:r><w:t>new</w:t></w:r></w:ins></w:p>"#;
        let steps = [keep(0..4, Change::Rejected), (4..7).map(|unit| Step::Remove { unit }).collect()].concat();
        assert_eq!(rewritten(source, &steps, &[]).unwrap(), r#"<w:p><w:r><w:t>old </w:t></w:r></w:p>"#);
    }

    #[test]
    fn types_into_an_empty_paragraph_with_its_mark_s_properties() {
        let source = r#"<w:p><w:pPr><w:rPr><w:ins w:id="2" w:author="Ana" w:date="A"/><w:i/></w:rPr></w:pPr></w:p>"#;
        let steps = vec![Step::Insert { text: "New".into(), revision: sari(), anchor: None }];
        assert_eq!(
            rewritten(source, &steps, &[]).unwrap(),
            format!(
                r#"<w:p><w:pPr><w:rPr><w:ins w:id="2" w:author="Ana" w:date="A"/><w:i/></w:rPr></w:pPr><w:ins w:id="11" {SARI_ATTRIBUTES}><w:r><w:rPr><w:i/></w:rPr><w:t xml:space="preserve">New</w:t></w:r></w:ins></w:p>"#
            )
        );
    }

    #[test]
    fn writes_typed_tabs_and_breaks_as_elements() {
        let steps = [vec![Step::Insert { text: "\tA\nB".into(), revision: sari(), anchor: None }], keep(0..1, Change::Same)].concat();
        assert_eq!(
            rewritten("<w:p><w:r><w:t>x</w:t></w:r></w:p>", &steps, &[]).unwrap(),
            format!(
                r#"<w:p><w:ins w:id="11" {SARI_ATTRIBUTES}><w:r><w:tab/><w:t xml:space="preserve">A</w:t><w:br/><w:t xml:space="preserve">B</w:t></w:r></w:ins><w:r><w:t>x</w:t></w:r></w:p>"#
            )
        );
    }

    #[test]
    fn refuses_to_split_a_hyperlink_and_to_delete_a_field_written_as_one_element() {
        let link = r#"<w:p><w:hyperlink w:anchor="x"><w:r><w:t>abcd</w:t></w:r></w:hyperlink></w:p>"#;
        let split = [keep(0..2, Change::Same), vec![Step::Break { revision: sari() }], keep(2..4, Change::Same)].concat();
        assert!(matches!(rewritten(link, &split, &[]), Err(Unpreservable::SplitWrapper { .. })));
        let field =
            r#"<w:p><w:r><w:t xml:space="preserve">Date: </w:t></w:r><w:fldSimple w:instr=" DATE "><w:r><w:t>1 May</w:t></w:r></w:fldSimple></w:p>"#;
        let delete = [keep(0..6, Change::Same), keep(6..7, Change::Deleted(sari()))].concat();
        assert!(matches!(rewritten(field, &delete, &[]), Err(Unpreservable::Locked { .. })));
    }

    #[test]
    fn accepts_and_rejects_an_inserted_content_control_whole() {
        let source = r#"<w:p><w:ins w:id="5" w:author="Ana" w:date="A"><w:sdt><w:sdtPr><w:tag w:val="t"/></w:sdtPr><w:sdtContent><w:r><w:t>ab</w:t></w:r></w:sdtContent></w:sdt></w:ins></w:p>"#;
        assert_eq!(
            rewritten(source, &keep(0..2, Change::Accepted), &[]).unwrap(),
            r#"<w:p><w:sdt><w:sdtPr><w:tag w:val="t"/></w:sdtPr><w:sdtContent><w:r><w:t>ab</w:t></w:r></w:sdtContent></w:sdt></w:p>"#
        );
        let removed: Vec<Step> = (0..2).map(|unit| Step::Remove { unit }).collect();
        assert_eq!(rewritten(source, &removed, &[]).unwrap(), "<w:p></w:p>");
    }

    #[test]
    fn copies_ranges_with_the_paragraphs_already_rewritten_inside_them() {
        let bytes = b"abcdefgh";
        let rewritten = BTreeMap::from([(2usize, (6usize, b"OUTER".to_vec())), (3, (4, b"in".to_vec())), (7, (8, b"!".to_vec()))]);
        let mut out = Vec::new();
        Source { bytes, rewritten: &rewritten }.copy(0..8, &mut out).unwrap();
        assert_eq!(out, b"abOUTERg!");
        let used: HashSet<u32> = HashSet::from([1, 2]);
        assert_eq!(Ids::new(0, used).paragraph().unwrap(), "00000003");
    }

    // ---- Round trips: each rewrite read back by the desktop reader and inspected against its source.

    const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    const NAMESPACES: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math""#;
    const ANA: &str = r#"w:author="Ana Setia" w:date="2026-10-01T09:00:00Z""#;
    const BOB: &str = r#"w:author="Bob Hartono" w:date="2026-10-02T10:00:00Z""#;

    fn package_of(document: &[u8]) -> Vec<u8> {
        zip_entries(&[
            ("[Content_Types].xml".into(), CONTENT_TYPES.as_bytes().to_vec()),
            ("_rels/.rels".into(), ROOT_RELS.as_bytes().to_vec()),
            ("word/document.xml".into(), document.to_vec()),
        ])
        .unwrap()
    }

    fn package(body: &str) -> Vec<u8> {
        package_of(
            format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {NAMESPACES}><w:body>{body}</w:body></w:document>"#)
                .as_bytes(),
        )
    }

    /// A paragraph as Word shows it: its text, runs (tracked changes by author and date, not by
    /// record), objects and properties; the mark's own tracked change is not read into the model.
    #[derive(Debug, PartialEq)]
    struct Shown {
        text: String,
        runs: Vec<(usize, CharProps, Option<String>, Option<String>)>,
        props: wordcraft_doc::ParaProps,
        /// The paragraph's formatting change by author and date (the reader reads `w:pPrChange`
        /// since the upstream merge; its record's index depends on reading order).
        props_change: Option<String>,
        mark: CharProps,
        objects: Vec<wordcraft_doc::InlineObject>,
        section: bool,
    }

    fn shown(doc: &Document, paragraph: &Paragraph) -> Shown {
        let record =
            |index: Option<u32>| index.and_then(|index| doc.revisions.get(index as usize)).map(|record| format!("{}|{}", record.author, record.date));
        // A formatting change's record by author and date, its index left out.
        fn change<T>(doc: &Document, change: &mut Option<Box<wordcraft_doc::props::PropChange<T>>>) -> Option<String> {
            let change = change.as_mut()?;
            let record = doc.revisions.get(change.rev as usize).map(|record| format!("{}|{}", record.author, record.date));
            change.rev = 0;
            record
        }
        let mut runs: Vec<(usize, CharProps, Option<String>, Option<String>)> = Vec::new();
        for run in &paragraph.runs {
            let mut props = run.props.clone();
            // embed: the run's formatting change goes by its record too.
            let format = change(doc, &mut props.fmt_change);
            let (ins, del) = (record(props.ins.take()), record(props.del.take()));
            let del = match format {
                Some(format) => Some(format!("{}; formatted {format}", del.unwrap_or_default())),
                None => del,
            };
            match runs.last_mut() {
                Some(last) if last.1 == props && last.2 == ins && last.3 == del => last.0 += run.len,
                _ => runs.push((run.len, props, ins, del)),
            }
        }
        let mut mark = CharProps { ins: None, del: None, ..paragraph.mark.clone() };
        let mut props = paragraph.props.clone();
        let props_change = change(doc, &mut props.fmt_change);
        let props_change = match props.num_change.as_mut() {
            Some(numbering) => {
                let numbered = doc.revisions.get(numbering.rev as usize).map(|record| format!("{}|{}", record.author, record.date));
                numbering.rev = 0;
                Some(format!("{}; numbering {}", props_change.unwrap_or_default(), numbered.unwrap_or_default()))
            }
            None => props_change,
        };
        // The mark's formatting change: its record is kept with the paragraph's.
        let props_change = match change(doc, &mut mark.fmt_change) {
            Some(marked) => Some(format!("{}; mark {marked}", props_change.unwrap_or_default())),
            None => props_change,
        };
        Shown {
            text: paragraph.text.clone(),
            runs,
            props,
            props_change,
            mark,
            objects: paragraph.objects.clone(),
            section: paragraph.section.is_some(),
        }
    }

    /// A session record in `after`.
    fn session(after: &mut Document, kind: RevisionKind) -> u32 {
        after.revisions.push(Recorded { kind, author: "Sari Wulan".into(), date: "2026-10-10T08:00:00Z".into(), move_name: None, read: false });
        u32::try_from(after.revisions.len() - 1).unwrap()
    }

    /// The formatting of the character at `at`, without its tracked changes.
    fn plain(paragraph: &Paragraph, at: usize) -> CharProps {
        CharProps { ins: None, del: None, ..paragraph.props_of_char(at).clone() }
    }

    fn type_text(paragraph: &mut Paragraph, at: usize, text: &str, props: &CharProps, record: u32) {
        paragraph.insert_text(at, text, &CharProps { ins: Some(record), del: None, ..props.clone() }).unwrap();
    }

    fn delete(paragraph: &mut Paragraph, from: usize, to: usize, record: u32) {
        paragraph.format(from, to, &|props| props.del = Some(record)).unwrap();
    }

    /// Accept or reject the tracked changes in `from..to`, as the engine does.
    fn decide(paragraph: &mut Paragraph, from: usize, to: usize, accept: bool) {
        let ranges: Vec<(usize, usize, bool, bool)> = paragraph
            .run_ranges()
            .filter(|(range, _)| range.end > from && range.start < to)
            .map(|(range, props)| (range.start.max(from), range.end.min(to), props.ins.is_some(), props.del.is_some()))
            .collect();
        for (start, end, ins, del) in ranges.into_iter().rev() {
            if (del && accept) || (ins && !accept) {
                paragraph.delete(start, end).unwrap();
            } else if ins || del {
                paragraph
                    .format(start, end, &|props| {
                        props.ins = None;
                        props.del = None;
                    })
                    .unwrap();
            }
        }
    }

    /// Split at `at` as an edit session does (Task 10): both halves keep the source mark's
    /// formatting, the earlier one's mark inserted.
    fn split(paragraph: &Paragraph, at: usize, record: u32) -> (Paragraph, Paragraph) {
        let mut first = paragraph.clone();
        let mut second = first.split_off(at).unwrap();
        first.mark = CharProps { ins: Some(record), ..paragraph.mark.clone() };
        first.section = None;
        second.mark = paragraph.mark.clone();
        (first, second)
    }

    /// The highest `w:id` in a part, and its `w14:paraId`/`w14:textId` values.
    fn used_ids(bytes: &[u8]) -> (u64, HashSet<u32>) {
        let text = String::from_utf8_lossy(bytes);
        let values = |name: &str| -> Vec<String> {
            text.split(&format!(" {name}=\"")).skip(1).filter_map(|rest| rest.split('"').next()).map(str::to_string).collect()
        };
        let highest = values("w:id").iter().filter_map(|value| value.parse::<u64>().ok()).max().unwrap_or(0);
        let paragraphs =
            ["w14:paraId", "w14:textId"].iter().flat_map(|name| values(name)).filter_map(|value| u32::from_str_radix(&value, 16).ok()).collect();
        (highest, paragraphs)
    }

    struct RoundTrip {
        /// The rewritten paragraph.
        written: String,
        /// The inspector's finding, source against output (W1 refuses some sources until Task 7).
        inspected: Result<crate::inspect::Inspection, crate::inspect::InspectError>,
    }

    /// Edit the body's first paragraph (`edit` makes the edited paragraphs from the source's, adding
    /// this session's records to the document), plan it, rewrite it and put it back in the package.
    /// The result must read back as the edited paragraphs, everything else as it was.
    fn round_trip(body: &str, edit: impl FnOnce(&Paragraph, &mut Document) -> Vec<Paragraph>) -> Result<RoundTrip, Unpreservable> {
        let bytes = package(body);
        let before = crate::read(&bytes).unwrap();
        let original = before.body[0].as_para().unwrap().clone();
        let mut after = before.clone();
        let edited = edit(&original, &mut after);
        let group: Vec<&Paragraph> = edited.iter().collect();
        let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
        let steps = crate::preserve::plan::plan(&original, &before, &group, &after, &mut budget)?;
        let pkg = Package::open_with(&bytes, &ReadLimits::default()).unwrap();
        let wanted = BTreeMap::from([("word/document.xml".to_string(), BTreeSet::from([0u32]))]);
        let mut part = scan_package(&pkg, &wanted, &mut budget).unwrap().remove("word/document.xml").unwrap();
        let scanned = part.paragraphs.remove(&0).unwrap();
        assert_eq!(scanned.text(), original.text, "the scan reads what the reader reads");
        // As the writer's caller: every `w:id` and paragraph id the part uses (ids from 101 here).
        let (highest, paragraph_ids) = used_ids(&part.bytes);
        let mut ids = Ids::new(highest.max(100), paragraph_ids);
        let none = BTreeMap::new();
        let held = budget.held();
        let written =
            rewrite(&Source { bytes: &part.bytes, rewritten: &none }, &part.prefix, "word/document.xml", &scanned, &steps, &mut ids, &mut budget)?;
        assert_eq!(budget.held() - held, held_by(&written));
        let span = scanned.span();
        let document = [&part.bytes[..span.start], &written[..], &part.bytes[span.end..]].concat();
        let output = package_of(&document);
        let reread = crate::read(&output).unwrap();
        let read_back: Vec<Shown> = reread.body.iter().map(|block| shown(&reread, block.as_para().unwrap())).collect();
        let expected: Vec<Shown> = edited
            .iter()
            .map(|paragraph| shown(&after, paragraph))
            .chain(before.body.iter().skip(1).map(|block| shown(&before, block.as_para().unwrap())))
            .collect();
        assert_eq!(read_back, expected, "read back as edited: {}", String::from_utf8_lossy(&written));
        let inspected = crate::inspect::inspect(&bytes, &output);
        Ok(RoundTrip { written: String::from_utf8(written).unwrap(), inspected })
    }

    /// The inspector finds nothing but this session's tracked changes (`insertions`, `deletions`) and
    /// the decided ones.
    fn only_tracked(round_trip: &RoundTrip, insertions: u32, deletions: u32, resolved: crate::inspect::Resolved) {
        let context = &round_trip.written;
        let inspection = round_trip.inspected.as_ref().unwrap_or_else(|refused| panic!("{refused:?} {context}"));
        assert!(inspection.losses.is_empty(), "{inspection:?} {context}");
        assert_eq!(
            (inspection.untracked_changes, inspection.altered_revisions, inspection.too_many_changes),
            (0, 0, false),
            "{inspection:?} {context}"
        );
        assert!(inspection.added_parts.is_empty() && inspection.changed_relationships.is_empty() && inspection.content_types.is_empty());
        assert_eq!(inspection.comments, crate::inspect::CommentChanges::default(), "{context}");
        let new: Vec<(&str, u32, u32, u32)> =
            inspection.new_revisions.iter().map(|entry| (entry.author.as_str(), entry.insertions, entry.deletions, entry.format_changes)).collect();
        let expected = if insertions + deletions == 0 { Vec::new() } else { vec![("Sari Wulan", insertions, deletions, 0)] };
        assert_eq!(new, expected, "{inspection:?} {context}");
        assert_eq!(inspection.resolved, resolved, "{context}");
    }

    fn undecided() -> crate::inspect::Resolved {
        crate::inspect::Resolved::default()
    }

    #[test]
    fn round_trips_typing_inside_beside_and_after_a_hyperlink() {
        let body = r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:hyperlink w:anchor="terms"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>the terms</w:t></w:r></w:hyperlink><w:r><w:t xml:space="preserve"> now.</w:t></w:r></w:p>"#;
        // Inside the link, continuing its text: the typed run sits in the hyperlink.
        let inside = round_trip(body, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let mut edited = original.clone();
            type_text(&mut edited, 8, "full ", &plain(original, 7), record);
            vec![edited]
        })
        .unwrap();
        only_tracked(&inside, 1, 0, undecided());
        assert!(inside.written.contains(&format!(
            r#"<w:hyperlink w:anchor="terms"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t xml:space="preserve">the </w:t></w:r><w:ins w:id="101" {SARI_ATTRIBUTES}><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t xml:space="preserve">full </w:t></w:r></w:ins>"#
        )));
        // At the link's end, continuing it: still inside.
        let at_end = round_trip(body, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let mut edited = original.clone();
            type_text(&mut edited, 13, "!", &plain(original, 12), record);
            vec![edited]
        })
        .unwrap();
        only_tracked(&at_end, 1, 0, undecided());
        assert!(at_end.written.contains(r#"<w:t xml:space="preserve">!</w:t></w:r></w:ins></w:hyperlink>"#), "{}", at_end.written);
        // After the link, formatted as the text after it: outside.
        let outside = round_trip(body, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let mut edited = original.clone();
            type_text(&mut edited, 13, ",", &plain(original, 13), record);
            vec![edited]
        })
        .unwrap();
        only_tracked(&outside, 1, 0, undecided());
        assert!(outside.written.contains(r#"</w:hyperlink><w:ins"#), "{}", outside.written);
    }

    #[test]
    fn round_trips_typing_at_the_start_and_after_a_bookmark() {
        let body =
            r#"<w:p><w:bookmarkStart w:id="0" w:name="clause"/><w:r><w:rPr><w:b/></w:rPr><w:t>Rent.</w:t></w:r><w:bookmarkEnd w:id="0"/></w:p>"#;
        let start = round_trip(body, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let mut edited = original.clone();
            // After the bookmark's start, before "Rent": bold, as the text it continues.
            let at = edited.text.find('R').unwrap();
            type_text(&mut edited, at, "The ", &plain(original, at), record);
            vec![edited]
        })
        .unwrap();
        only_tracked(&start, 1, 0, undecided());
        assert!(
            start.written.starts_with(&format!(
                r#"<w:p><w:bookmarkStart w:id="0" w:name="clause"/><w:ins w:id="101" {SARI_ATTRIBUTES}><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">The </w:t></w:r></w:ins>"#
            )),
            "{}",
            start.written
        );
        // At the very end, after the bookmark's end, formatted as the mark.
        let end = round_trip(body, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let mut edited = original.clone();
            let at = edited.len();
            type_text(&mut edited, at, " Paid.", &CharProps::default(), record);
            vec![edited]
        })
        .unwrap();
        only_tracked(&end, 1, 0, undecided());
    }

    #[test]
    fn round_trips_typing_into_an_empty_paragraph_with_its_mark() {
        let body = r#"<w:p><w:pPr><w:jc w:val="center"/><w:rPr><w:i/></w:rPr></w:pPr></w:p><w:p><w:r><w:t>Next.</w:t></w:r></w:p>"#;
        let typed = round_trip(body, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let mut edited = original.clone();
            type_text(&mut edited, 0, "Title", &original.mark, record);
            vec![edited]
        })
        .unwrap();
        only_tracked(&typed, 1, 0, undecided());
    }

    #[test]
    fn round_trips_typing_beside_a_field() {
        let field = r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>3</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#;
        let body = format!(r#"<w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r>{field}<w:r><w:t xml:space="preserve"> of 9</w:t></w:r></w:p>"#);
        let after_field = round_trip(&body, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let mut edited = original.clone();
            // Right after the field, formatted as the text after it.
            let at = "Page ".len() + wordcraft_doc::para::OBJ.len_utf8();
            type_text(&mut edited, at, ",", &plain(original, at), record);
            vec![edited]
        })
        .unwrap();
        only_tracked(&after_field, 1, 0, undecided());
        assert!(after_field.written.contains(&format!(r#"{field}<w:ins"#)), "{}", after_field.written);
    }

    #[test]
    fn refuses_typing_beside_a_field_written_as_one_element_or_inside_a_fields_result() {
        let simple =
            r#"<w:p><w:r><w:t xml:space="preserve">Date: </w:t></w:r><w:fldSimple w:instr=" DATE "><w:r><w:t>1 May</w:t></w:r></w:fldSimple></w:p>"#;
        let typed = round_trip(simple, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let mut edited = original.clone();
            let at = edited.len();
            type_text(&mut edited, at, "!", &plain(original, at - wordcraft_doc::para::OBJ.len_utf8()), record);
            vec![edited]
        });
        assert!(matches!(typed, Err(Unpreservable::Locked { .. })), "{:?}", typed.as_ref().map(|done| &done.written));
        // A field whose result runs on into the next paragraph: its result here is locked, and
        // typing in it (or at the paragraph's end, still inside the field) is refused.
        let spilled = r#"<w:p><w:r><w:t>A</w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> TOC </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>Entry</w:t></w:r></w:p><w:p><w:r><w:t>More</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#;
        let (_, scanned) = scanned(spilled);
        let units = scanned.units.len();
        for at in 2..=units {
            let steps = [
                keep(0..at, Change::Same),
                vec![Step::Insert { text: "x".into(), revision: sari(), anchor: Some(0) }],
                keep(at..units, Change::Same),
            ]
            .concat();
            assert!(matches!(rewritten(spilled, &steps, &[]), Err(Unpreservable::Locked { .. })), "typing before unit {at}");
        }
        // Right before the field's object, typing would stand beside its begin; the scan locks that
        // object as it locks a field nested in another's result, so it is refused too.
        let before_object =
            [keep(0..1, Change::Same), vec![Step::Insert { text: "x".into(), revision: sari(), anchor: Some(0) }], keep(1..units, Change::Same)]
                .concat();
        assert!(matches!(rewritten(spilled, &before_object, &[]), Err(Unpreservable::Locked { .. })));
        // Away from it, typing is written.
        let away = [vec![Step::Insert { text: "x".into(), revision: sari(), anchor: Some(0) }], keep(0..units, Change::Same)].concat();
        assert!(rewritten(spilled, &away, &[]).is_ok());
        // The next paragraph starts inside the field: nothing is typed among its locked characters,
        // nor after the last of them (the scan does not say where the field ends).
        let (_, next) = scanned_at(spilled, 1);
        let units = next.units.len();
        assert_eq!(units, "More".len());
        for at in 0..=units {
            let steps =
                [keep(0..at, Change::Same), vec![Step::Insert { text: "x".into(), revision: sari(), anchor: None }], keep(at..units, Change::Same)]
                    .concat();
            assert!(matches!(rewritten_at(spilled, 1, &steps, &[]), Err(Unpreservable::Locked { .. })), "typing before unit {at}");
        }
    }

    #[test]
    fn round_trips_typing_inside_another_authors_insertion_and_deletion() {
        let inserted = format!(r#"<w:p><w:ins w:id="5" {ANA}><w:r><w:t>Rent due</w:t></w:r></w:ins></w:p>"#);
        let typed = round_trip(&inserted, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let mut edited = original.clone();
            type_text(&mut edited, 4, " is", &plain(original, 3), record);
            vec![edited]
        })
        .unwrap();
        only_tracked(&typed, 1, 0, undecided());
        assert_eq!(
            typed.written,
            format!(
                r#"<w:p><w:ins w:id="5" {ANA}><w:r><w:t xml:space="preserve">Rent</w:t></w:r></w:ins><w:ins w:id="101" {SARI_ATTRIBUTES}><w:r><w:t xml:space="preserve"> is</w:t></w:r></w:ins><w:ins w:id="102" {ANA}><w:r><w:t xml:space="preserve"> due</w:t></w:r></w:ins></w:p>"#
            )
        );
        let deleted = format!(
            r#"<w:p><w:r><w:t xml:space="preserve">Rent </w:t></w:r><w:del w:id="5" {ANA}><w:r><w:delText>monthly</w:delText></w:r></w:del></w:p>"#
        );
        let typed = round_trip(&deleted, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let mut edited = original.clone();
            type_text(&mut edited, 9, "-", &plain(original, 8), record);
            vec![edited]
        })
        .unwrap();
        only_tracked(&typed, 1, 0, undecided());
        assert!(typed.written.contains(&format!(r#"</w:del><w:ins w:id="101" {SARI_ATTRIBUTES}>"#)), "{}", typed.written);
        assert!(typed.written.contains(&format!(r#"<w:del w:id="102" {ANA}>"#)), "{}", typed.written);
    }

    #[test]
    fn round_trips_enter_with_the_source_properties_and_fresh_ids() {
        let props = r#"<w:pPr><w:pStyle w:val="Clause"/><w:rPr><w:b/></w:rPr><w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr></w:pPr>"#;
        let body = format!(
            r#"<w:p w14:paraId="00000001" w14:textId="00000001">{props}<w:r><w:t>One. Two.</w:t></w:r></w:p><w:p><w:r><w:t>Next.</w:t></w:r></w:p>"#
        );
        let split_once = round_trip(&body, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let (first, second) = split(original, 4, record);
            vec![first, second]
        })
        .unwrap();
        only_tracked(&split_once, 1, 0, undecided());
        assert_eq!(
            split_once.written,
            format!(
                r#"<w:p w14:paraId="00000002" w14:textId="00000003"><w:pPr><w:pStyle w:val="Clause"/><w:rPr><w:ins w:id="101" {SARI_ATTRIBUTES}/><w:b/></w:rPr></w:pPr><w:r><w:t xml:space="preserve">One.</w:t></w:r></w:p><w:p w14:paraId="00000001" w14:textId="00000001">{props}<w:r><w:t xml:space="preserve"> Two.</w:t></w:r></w:p>"#
            )
        );
    }

    #[test]
    fn copies_a_paragraph_formatting_change_to_the_half_enter_makes() {
        // 6C-24: Word copies the paragraph's tracked formatting change to the paragraph Enter makes,
        // with an id of its own; the section stays with the source's own mark.
        let change = format!(r#"<w:pPrChange w:id="7" {ANA}><w:pPr><w:jc w:val="left"/></w:pPr></w:pPrChange>"#);
        let props = format!(r#"<w:pPr><w:jc w:val="center"/><w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>{change}</w:pPr>"#);
        let body = format!(r#"<w:p>{props}<w:r><w:t>One. Two.</w:t></w:r></w:p><w:p><w:r><w:t>Next.</w:t></w:r></w:p>"#);
        let split_once = round_trip(&body, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let (first, second) = split(original, 4, record);
            vec![first, second]
        })
        .unwrap();
        only_tracked(&split_once, 1, 0, undecided());
        assert!(
            split_once.written.starts_with(&format!(
                r#"<w:p><w:pPr><w:jc w:val="center"/><w:rPr><w:ins w:id="101" {SARI_ATTRIBUTES}/></w:rPr><w:pPrChange w:id="102" {ANA}><w:pPr><w:jc w:val="left"/></w:pPr></w:pPrChange></w:pPr>"#
            )),
            "{}",
            split_once.written
        );
        assert!(split_once.written.contains(&format!(r#"</w:p><w:p>{props}"#)), "{}", split_once.written);
    }

    #[test]
    fn refuses_enter_inside_a_content_control_or_a_smart_tag() {
        for wrapped in [
            r#"<w:p><w:sdt><w:sdtPr><w:tag w:val="t"/></w:sdtPr><w:sdtContent><w:r><w:t>abcd</w:t></w:r></w:sdtContent></w:sdt></w:p>"#,
            r#"<w:p><w:smartTag w:uri="urn:x" w:element="place"><w:r><w:t>abcd</w:t></w:r></w:smartTag></w:p>"#,
            r#"<w:p><w:customXml w:element="party"><w:r><w:t>abcd</w:t></w:r></w:customXml></w:p>"#,
        ] {
            let split = [keep(0..2, Change::Same), vec![Step::Break { revision: sari() }], keep(2..4, Change::Same)].concat();
            assert!(matches!(rewritten(wrapped, &split, &[]), Err(Unpreservable::SplitWrapper { .. })), "{wrapped}");
            // At its edge, a mark is outside it.
            let before = [vec![Step::Break { revision: sari() }], keep(0..4, Change::Same)].concat();
            assert!(rewritten(wrapped, &before, &[]).is_ok(), "{wrapped}");
        }
    }

    #[test]
    fn round_trips_accepting_and_rejecting_everything() {
        let body = format!(
            r#"<w:p><w:r><w:t xml:space="preserve">Rent </w:t></w:r><w:ins w:id="3" {ANA}><w:r><w:t xml:space="preserve">is due </w:t></w:r></w:ins><w:del w:id="4" {BOB}><w:r><w:delText>monthly</w:delText></w:r></w:del><w:r><w:t>.</w:t></w:r></w:p>"#
        );
        let accepted = round_trip(&body, |original, _| {
            let mut edited = original.clone();
            let length = edited.len();
            decide(&mut edited, 0, length, true);
            vec![edited]
        })
        .unwrap();
        only_tracked(&accepted, 0, 0, crate::inspect::Resolved { accepted: 2, rejected: 0 });
        assert_eq!(
            accepted.written,
            r#"<w:p><w:r><w:t xml:space="preserve">Rent </w:t></w:r><w:r><w:t xml:space="preserve">is due </w:t></w:r><w:r><w:t>.</w:t></w:r></w:p>"#
        );
        let rejected = round_trip(&body, |original, _| {
            let mut edited = original.clone();
            let length = edited.len();
            decide(&mut edited, 0, length, false);
            vec![edited]
        })
        .unwrap();
        only_tracked(&rejected, 0, 0, crate::inspect::Resolved { accepted: 0, rejected: 2 });
        assert_eq!(
            rejected.written,
            r#"<w:p><w:r><w:t xml:space="preserve">Rent </w:t></w:r><w:r><w:t>monthly</w:t></w:r><w:r><w:t>.</w:t></w:r></w:p>"#
        );
    }

    #[test]
    fn round_trips_deciding_part_of_a_change_and_keeps_the_rest_with_a_fresh_id() {
        let body = format!(r#"<w:p><w:ins w:id="5" {ANA}><w:r><w:t>ab</w:t></w:r><w:r><w:t>cd</w:t></w:r><w:r><w:t>ef</w:t></w:r></w:ins></w:p>"#);
        let middle = round_trip(&body, |original, _| {
            let mut edited = original.clone();
            decide(&mut edited, 2, 4, true);
            vec![edited]
        })
        .unwrap();
        assert_eq!(
            middle.written,
            format!(
                r#"<w:p><w:ins w:id="5" {ANA}><w:r><w:t>ab</w:t></w:r></w:ins><w:r><w:t>cd</w:t></w:r><w:ins w:id="101" {ANA}><w:r><w:t>ef</w:t></w:r></w:ins></w:p>"#
            )
        );
        let inspection = middle.inspected.as_ref().unwrap();
        assert!(inspection.losses.is_empty() && inspection.new_revisions.is_empty(), "{inspection:?}");
        assert_eq!((inspection.untracked_changes, inspection.altered_revisions), (0, 0), "{inspection:?}");
    }

    #[test]
    fn round_trips_an_insertion_accepted_then_deleted_and_a_deletion_rejected_then_made_again() {
        // 6C-43: Ana's insertion accepted, then deleted by Sari: her w:ins goes, Sari's w:del wraps it.
        let inserted =
            format!(r#"<w:p><w:r><w:t xml:space="preserve">Rent </w:t></w:r><w:ins w:id="5" {ANA}><w:r><w:t>monthly</w:t></w:r></w:ins></w:p>"#);
        let accepted_then_deleted = round_trip(&inserted, |original, after| {
            let record = session(after, RevisionKind::Delete);
            let mut edited = original.clone();
            decide(&mut edited, 5, 12, true);
            delete(&mut edited, 5, 12, record);
            vec![edited]
        })
        .unwrap();
        assert_eq!(
            accepted_then_deleted.written,
            format!(
                r#"<w:p><w:r><w:t xml:space="preserve">Rent </w:t></w:r><w:del w:id="101" {SARI_ATTRIBUTES}><w:r><w:delText>monthly</w:delText></w:r></w:del></w:p>"#
            )
        );
        only_tracked(&accepted_then_deleted, 0, 1, crate::inspect::Resolved { accepted: 1, rejected: 0 });
        // 6C-44: Bob's deletion rejected, then deleted again by Sari: the deletion is Sari's.
        let deleted = format!(
            r#"<w:p><w:r><w:t xml:space="preserve">Rent </w:t></w:r><w:del w:id="5" {BOB}><w:r><w:delText>monthly</w:delText></w:r></w:del></w:p>"#
        );
        let rejected_then_deleted = round_trip(&deleted, |original, after| {
            let record = session(after, RevisionKind::Delete);
            let mut edited = original.clone();
            decide(&mut edited, 5, 12, false);
            delete(&mut edited, 5, 12, record);
            vec![edited]
        })
        .unwrap();
        assert_eq!(
            rejected_then_deleted.written,
            format!(
                r#"<w:p><w:r><w:t xml:space="preserve">Rent </w:t></w:r><w:del w:id="101" {SARI_ATTRIBUTES}><w:r><w:delText>monthly</w:delText></w:r></w:del></w:p>"#
            )
        );
        only_tracked(&rejected_then_deleted, 0, 1, crate::inspect::Resolved { accepted: 0, rejected: 1 });
        // Inserted by Ana and deleted by Bob; Bob's deletion rejected, then made again by Sari: Ana's
        // insertion stays around Sari's deletion, as Word nests them.
        let both = format!(
            r#"<w:p><w:r><w:t xml:space="preserve">Rent </w:t></w:r><w:ins w:id="5" {ANA}><w:del w:id="6" {BOB}><w:r><w:delText>monthly</w:delText></w:r></w:del></w:ins></w:p>"#
        );
        let nested = round_trip(&both, |original, after| {
            let record = session(after, RevisionKind::Delete);
            let mut edited = original.clone();
            edited.format(5, 12, &|props| props.del = Some(record)).unwrap();
            vec![edited]
        })
        .unwrap();
        assert_eq!(
            nested.written,
            format!(
                r#"<w:p><w:r><w:t xml:space="preserve">Rent </w:t></w:r><w:ins w:id="5" {ANA}><w:del w:id="101" {SARI_ATTRIBUTES}><w:r><w:delText>monthly</w:delText></w:r></w:del></w:ins></w:p>"#
            )
        );
        only_tracked(&nested, 0, 1, crate::inspect::Resolved { accepted: 0, rejected: 1 });
    }

    /// Review Focus 6 until Task 7: W1's inspector refuses a source holding `element` inside a
    /// tracked change, so the round trip is checked by reading back only. Task 7 (6C-15) makes the
    /// inspector read them: it replaces this with `only_tracked(round_trip, 0, 0, resolved)`.
    fn inspected_after_task_7(round_trip: &RoundTrip, element: &str, _resolved: crate::inspect::Resolved) {
        assert_eq!(round_trip.inspected, Err(crate::inspect::InspectError::Base(format!("word/document.xml: {element} inside a tracked change"))));
    }

    #[test]
    fn round_trips_deciding_an_inserted_content_control_smart_tag_and_equation_whole() {
        for (element, inserted) in [
            ("w:sdt", r#"<w:sdt><w:sdtPr><w:tag w:val="t"/></w:sdtPr><w:sdtContent><w:r><w:t>ab</w:t></w:r></w:sdtContent></w:sdt>"#),
            (
                "w:smartTag",
                r#"<w:smartTag w:uri="urn:x" w:element="place"><w:smartTagPr><w:attr w:name="n" w:val="v"/></w:smartTagPr><w:r><w:t>ab</w:t></w:r></w:smartTag>"#,
            ),
            ("m:oMath", r#"<m:oMath><m:r><m:t>x+1</m:t></m:r></m:oMath>"#),
        ] {
            let body = format!(
                r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:ins w:id="5" {ANA}>{inserted}</w:ins><w:r><w:t xml:space="preserve"> here.</w:t></w:r></w:p>"#
            );
            let accepted = round_trip(&body, |original, _| {
                let mut edited = original.clone();
                let length = edited.len();
                decide(&mut edited, 0, length, true);
                vec![edited]
            });
            // embed: refused since the upstream merge (the reader's content control markers, which
            // the planner does not compare yet): fail closed, never a guess.
            if element == "w:sdt" {
                assert!(matches!(accepted, Err(Unpreservable::Unsupported(_))), "{:?}", accepted.as_ref().err());
                continue;
            }
            let accepted = accepted.unwrap();
            inspected_after_task_7(&accepted, element, crate::inspect::Resolved { accepted: 1, rejected: 0 });
            assert_eq!(
                accepted.written,
                format!(r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r>{inserted}<w:r><w:t xml:space="preserve"> here.</w:t></w:r></w:p>"#)
            );
            let rejected = round_trip(&body, |original, _| {
                let mut edited = original.clone();
                let length = edited.len();
                decide(&mut edited, 0, length, false);
                vec![edited]
            })
            .unwrap();
            inspected_after_task_7(&rejected, element, crate::inspect::Resolved { accepted: 0, rejected: 1 });
            assert_eq!(
                rejected.written,
                r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:r><w:t xml:space="preserve"> here.</w:t></w:r></w:p>"#
            );
        }
    }

    #[test]
    fn keeps_the_bytes_between_elements_and_wrappers_that_hold_nothing() {
        // 6C-42: white space, comments and processing instructions between nodes and wrapper tags,
        // and an empty hyperlink, are copied where they stand; only the touched run changes.
        let body = format!(
            "<w:p w:rsidR=\"00A1\">\n  <w:pPr><w:jc w:val=\"left\"/></w:pPr>\n  <!-- first -->\n  <w:hyperlink w:anchor=\"a\">\n    <w:r><w:t>See</w:t></w:r>\n  </w:hyperlink>\n  <?keep this?>\n  <w:hyperlink w:anchor=\"empty\"></w:hyperlink><w:smartTag w:uri=\"urn:x\" w:element=\"e\"/>\n  <w:ins w:id=\"3\" {ANA}>\n    <w:r><w:t xml:space=\"preserve\"> it</w:t></w:r>\n  </w:ins>\n  <w:r><w:t>, now.</w:t></w:r>\n</w:p>"
        );
        let typed = round_trip(&body, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let mut edited = original.clone();
            let at = edited.len() - 1;
            type_text(&mut edited, at, " please", &plain(original, at - 1), record);
            vec![edited]
        })
        .unwrap();
        only_tracked(&typed, 1, 0, undecided());
        let untouched = format!(
            "<w:p w:rsidR=\"00A1\">\n  <w:pPr><w:jc w:val=\"left\"/></w:pPr>\n  <!-- first -->\n  <w:hyperlink w:anchor=\"a\">\n    <w:r><w:t>See</w:t></w:r>\n  </w:hyperlink>\n  <?keep this?>\n  <w:hyperlink w:anchor=\"empty\"></w:hyperlink><w:smartTag w:uri=\"urn:x\" w:element=\"e\"/>\n  <w:ins w:id=\"3\" {ANA}>\n    <w:r><w:t xml:space=\"preserve\"> it</w:t></w:r>\n  </w:ins>\n  "
        );
        assert!(typed.written.starts_with(&untouched), "{}", typed.written);
        assert!(typed.written.ends_with("</w:t></w:r>\n</w:p>"), "{}", typed.written);
        // Accepting the insertion leaves its wrapper and keeps the bytes inside it.
        let accepted = round_trip(&body, |original, _| {
            let mut edited = original.clone();
            let length = edited.len();
            decide(&mut edited, 0, length, true);
            vec![edited]
        })
        .unwrap();
        only_tracked(&accepted, 0, 0, crate::inspect::Resolved { accepted: 1, rejected: 0 });
        assert!(
            accepted
                .written
                .contains("<w:smartTag w:uri=\"urn:x\" w:element=\"e\"/>\n  \n    <w:r><w:t xml:space=\"preserve\"> it</w:t></w:r>\n  \n  <w:r>"),
            "{}",
            accepted.written
        );
    }

    #[test]
    fn leaves_markers_in_place_inside_a_deletion() {
        // 6C-46 M8: bookmarks and comment ranges in deleted text stay where they are; Word does not
        // track their deletion, and the reader reads them deleted with the text around them.
        let body = r#"<w:p><w:r><w:t xml:space="preserve">Pay the </w:t></w:r><w:bookmarkStart w:id="1" w:name="amount"/><w:r><w:t>full</w:t></w:r><w:bookmarkEnd w:id="1"/><w:r><w:t xml:space="preserve"> rent.</w:t></w:r></w:p>"#;
        let deleted = round_trip(body, |original, after| {
            let record = session(after, RevisionKind::Delete);
            let mut edited = original.clone();
            let (from, to) = (edited.text.find("full").unwrap() - wordcraft_doc::para::OBJ.len_utf8(), edited.text.find(" rent").unwrap() + 1);
            delete(&mut edited, from, to, record);
            vec![edited]
        })
        .unwrap();
        only_tracked(&deleted, 0, 1, undecided());
        assert_eq!(
            deleted.written,
            format!(
                r#"<w:p><w:r><w:t xml:space="preserve">Pay the </w:t></w:r><w:del w:id="101" {SARI_ATTRIBUTES}><w:bookmarkStart w:id="1" w:name="amount"/><w:r><w:delText>full</w:delText></w:r><w:bookmarkEnd w:id="1"/><w:r><w:delText xml:space="preserve"> </w:delText></w:r></w:del><w:r><w:t xml:space="preserve">rent.</w:t></w:r></w:p>"#
            )
        );
    }

    #[test]
    fn keeps_what_is_no_character_out_of_a_deletion() {
        // A field's character and a page break Word rendered, in a run whose text is deleted: they
        // stay as they are, outside the deletion.
        let body = r#"<w:p><w:r><w:t xml:space="preserve">Due </w:t></w:r><w:r><w:lastRenderedPageBreak/><w:t>today</w:t></w:r></w:p>"#;
        let deleted = round_trip(body, |original, after| {
            let record = session(after, RevisionKind::Delete);
            let mut edited = original.clone();
            delete(&mut edited, 4, 9, record);
            vec![edited]
        })
        .unwrap();
        only_tracked(&deleted, 0, 1, undecided());
        assert_eq!(
            deleted.written,
            format!(
                r#"<w:p><w:r><w:t xml:space="preserve">Due </w:t></w:r><w:r><w:lastRenderedPageBreak/></w:r><w:del w:id="101" {SARI_ATTRIBUTES}><w:r><w:delText>today</w:delText></w:r></w:del></w:p>"#
            )
        );
    }

    #[test]
    fn round_trips_deleting_a_field_whole() {
        let field = r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>3</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#;
        let body = format!(r#"<w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r>{field}</w:p>"#);
        let deleted = round_trip(&body, |original, after| {
            let record = session(after, RevisionKind::Delete);
            let mut edited = original.clone();
            let length = edited.len();
            delete(&mut edited, 5, length, record);
            vec![edited]
        })
        .unwrap();
        only_tracked(&deleted, 0, 1, undecided());
        assert!(
            deleted.written.contains(r#"<w:r><w:delInstrText xml:space="preserve"> PAGE </w:delInstrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:delText>3</w:delText></w:r>"#),
            "{}",
            deleted.written
        );
    }

    #[test]
    fn round_trips_typing_before_and_at_the_start_of_a_content_control() {
        let body = r#"<w:p><w:r><w:t xml:space="preserve">Party: </w:t></w:r><w:sdt><w:sdtPr><w:tag w:val="party"/></w:sdtPr><w:sdtContent><w:r><w:t>Lessee</w:t></w:r></w:sdtContent></w:sdt></w:p>"#;
        // Continuing the text before the control: the typing goes before the control, not after its
        // properties.
        // embed: through the reader, refused since the upstream merge (its content control markers,
        // which the planner does not compare yet); the writer's own placement is checked below.
        let before = round_trip(body, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let mut edited = original.clone();
            type_text(&mut edited, 7, "the ", &plain(original, 6), record);
            vec![edited]
        });
        assert!(matches!(before, Err(Unpreservable::Unsupported(_))), "{:?}", before.as_ref().err());
        let before = rewritten(
            body,
            &[keep(0..7, Change::Same), vec![Step::Insert { text: "the ".into(), revision: sari(), anchor: Some(6) }], keep(7..13, Change::Same)]
                .concat(),
            &[],
        )
        .unwrap();
        assert!(before.contains(r#"</w:t></w:r></w:ins><w:sdt><w:sdtPr>"#), "{before}");
        // Typing that continues the control's first character, written in the control.
        let rendered = rewritten(
            body,
            &[keep(0..7, Change::Same), vec![Step::Insert { text: "The ".into(), revision: sari(), anchor: Some(7) }], keep(7..13, Change::Same)]
                .concat(),
            &[],
        )
        .unwrap();
        assert!(rendered.contains(&format!(r#"<w:sdtContent><w:ins w:id="11" {SARI_ATTRIBUTES}><w:r><w:t xml:space="preserve">The </w:t></w:r></w:ins><w:r><w:t>Lessee</w:t></w:r>"#)), "{rendered}");
    }

    #[test]
    fn round_trips_a_paste_of_two_paragraphs_inside_one() {
        // 6C-34 at the writer: two paragraphs pasted in the middle of one. The earlier halves end
        // with marks inserted now; the middle paragraph is new; the last keeps the source's mark.
        let body = r#"<w:p w14:paraId="00000001"><w:pPr><w:jc w:val="both"/></w:pPr><w:r><w:t>Rent due.</w:t></w:r></w:p>"#;
        let pasted = round_trip(body, |original, after| {
            let record = session(after, RevisionKind::Insert);
            let props = plain(original, 4);
            let (mut head, mut tail) = split(original, 4, record);
            type_text(&mut head, 4, " first", &props, record);
            let mut middle = Paragraph::new();
            middle.props = original.props.clone();
            middle.mark = CharProps { ins: Some(record), ..original.mark.clone() };
            type_text(&mut middle, 0, "Second paragraph", &props, record);
            type_text(&mut tail, 0, "Third", &props, record);
            vec![head, middle, tail]
        })
        .unwrap();
        // Two marks and three typed runs.
        only_tracked(&pasted, 5, 0, undecided());
        assert_eq!(pasted.written.matches("<w:p ").count(), 3, "{}", pasted.written);
        assert!(pasted.written.contains(r#"</w:p><w:p w14:paraId="00000001"><w:pPr><w:jc w:val="both"/></w:pPr><w:ins"#), "{}", pasted.written);
        // The new paragraphs' ids are unused in the part, and differ.
        assert!(
            pasted.written.starts_with(r#"<w:p w14:paraId="00000002">"#) && pasted.written.contains(r#"</w:p><w:p w14:paraId="00000004">"#),
            "{}",
            pasted.written
        );
    }

    #[test]
    fn refuses_what_it_cannot_write_as_typed() {
        for text in ["a\rb", "a\u{1}b", "a\u{FFFE}b", "a\u{FFFC}b"] {
            let steps = [vec![Step::Insert { text: text.into(), revision: sari(), anchor: None }], keep(0..1, Change::Same)].concat();
            assert!(matches!(rewritten("<w:p><w:r><w:t>x</w:t></w:r></w:p>", &steps, &[]), Err(Unpreservable::Verification(_))), "{text:?}");
        }
        // Page and column breaks, hyphens: their elements.
        let steps =
            [vec![Step::Insert { text: "\u{000C}\u{000E}\u{2011}\u{00AD}<&".into(), revision: sari(), anchor: None }], keep(0..1, Change::Same)]
                .concat();
        assert!(rewritten("<w:p><w:r><w:t>x</w:t></w:r></w:p>", &steps, &[]).unwrap().contains(
            r#"<w:r><w:br w:type="page"/><w:br w:type="column"/><w:noBreakHyphen/><w:softHyphen/><w:t xml:space="preserve">&lt;&amp;</w:t></w:r>"#
        ));
    }

    #[test]
    fn refuses_new_markup_in_a_part_written_in_a_default_namespace_and_steps_that_are_not_the_paragraphs() {
        let (bytes, scanned) = scanned("<w:p><w:r><w:t>ab</w:t></w:r></w:p>");
        let none = BTreeMap::new();
        let source = Source { bytes: &bytes, rewritten: &none };
        let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
        let mut write = |steps: &[Step]| rewrite(&source, "", "word/document.xml", &scanned, steps, &mut Ids::new(0, HashSet::new()), &mut budget);
        let typed = [vec![Step::Insert { text: "x".into(), revision: sari(), anchor: None }], keep(0..2, Change::Same)].concat();
        assert!(matches!(write(&typed), Err(Unpreservable::Namespace { .. })));
        // Decisions need no new markup.
        assert!(write(&keep(0..2, Change::Same)).is_ok());
        // Every character once, in order.
        assert_eq!(write(&keep(0..1, Change::Same)), Err(Unpreservable::Structure));
        assert_eq!(write(&[keep(1..2, Change::Same), keep(0..1, Change::Same)].concat()), Err(Unpreservable::Structure));
        assert_eq!(write(&keep(0..3, Change::Same)), Err(Unpreservable::Structure));
        let anchored_nowhere = [vec![Step::Insert { text: "x".into(), revision: sari(), anchor: Some(9) }], keep(0..2, Change::Same)].concat();
        let mut ids = Ids::new(0, HashSet::new());
        assert_eq!(rewrite(&source, "w", "word/document.xml", &scanned, &anchored_nowhere, &mut ids, &mut budget), Err(Unpreservable::Structure));
    }

    #[test]
    fn refuses_past_the_budget_holding_nothing() {
        let body = format!("<w:p>{}</w:p>", "<w:r><w:t xml:space=\"preserve\">Rent is due monthly. </w:t></w:r>".repeat(200));
        let (bytes, scanned) = scanned(&body);
        let count = scanned.units.len();
        let steps = [keep(0..count / 2, Change::Same), keep(count / 2..count, Change::Deleted(sari()))].concat();
        let none = BTreeMap::new();
        let source = Source { bytes: &bytes, rewritten: &none };
        for (memory, work) in [(1_000, MAX_WORK), (MAX_MEMORY, 10)] {
            let mut budget = Budget::new(work, memory);
            budget.hold(100).unwrap();
            let written = rewrite(&source, "w", "word/document.xml", &scanned, &steps, &mut Ids::new(0, HashSet::new()), &mut budget);
            assert_eq!(written, Err(Unpreservable::TooManyChanges));
            assert_eq!(budget.held(), 100, "what it held is given back");
        }
        let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
        let written = rewrite(&source, "w", "word/document.xml", &scanned, &steps, &mut Ids::new(0, HashSet::new()), &mut budget).unwrap();
        assert_eq!(budget.held(), held_by(&written));
        assert!(held_by(&written) >= units(written.len()));
    }

    const MEASURING: &str = "WORDCRAFT_EMIT_MEASURE";

    /// A long paragraph with every kind of step: decisions on Ana's insertions and Bob's deletions,
    /// deletions now, typing, and two new marks.
    fn long_case() -> (String, Vec<Step>) {
        let mut body = String::from("<w:p w14:paraId=\"00000001\"><w:pPr><w:jc w:val=\"both\"/><w:rPr><w:b/></w:rPr></w:pPr>");
        let mut steps = Vec::new();
        let mut unit = 0usize;
        for piece in 0..2_000usize {
            let text = format!("w{piece:04}abcd ");
            let run = format!(r#"<w:r><w:rPr><w:rFonts w:ascii="Font {}"/></w:rPr><w:t xml:space="preserve">{text}</w:t></w:r>"#, piece % 40);
            let length = text.len();
            match piece % 5 {
                1 => {
                    body.push_str(&format!(r#"<w:ins w:id="{piece}" {ANA}>{run}</w:ins>"#));
                    steps.extend(if piece % 2 == 0 {
                        keep(unit..unit + length, Change::Accepted)
                    } else {
                        (unit..unit + length).map(|unit| Step::Remove { unit }).collect()
                    });
                }
                3 => {
                    body.push_str(&format!(
                        r#"<w:del w:id="{piece}" {BOB}>{}</w:del>"#,
                        run.replace("w:t ", "w:delText ").replace("</w:t>", "</w:delText>")
                    ));
                    steps.extend(if piece % 2 == 0 {
                        keep(unit..unit + length, Change::Rejected)
                    } else {
                        (unit..unit + length).map(|unit| Step::Remove { unit }).collect()
                    });
                }
                _ => {
                    body.push_str(&run);
                    steps.extend(keep(unit..unit + 4, Change::Same));
                    steps.extend(keep(unit + 4..unit + length, if piece % 3 == 0 { Change::Deleted(sari()) } else { Change::Same }));
                }
            }
            unit += length;
            if piece % 100 == 0 {
                steps.push(Step::Insert { text: "NEW ".into(), revision: sari(), anchor: Some(unit - 1) });
            }
            if piece == 700 || piece == 1_400 {
                steps.push(Step::Break { revision: sari() });
            }
        }
        body.push_str("</w:p>");
        (body, steps)
    }

    #[test]
    fn holds_no_more_than_it_charges() {
        if std::env::var_os(MEASURING).is_some() {
            // I2: a paste of 300,000 characters, 100,000 typed tabs, 20,000 insertions in one run.
            let one = "<w:p><w:r><w:t>x</w:t></w:r></w:p>".to_string();
            let typed = |text: String| [vec![Step::Insert { text, revision: sari(), anchor: Some(0) }], keep(0..1, Change::Same)].concat();
            let many = format!("<w:p><w:r><w:t>{}</w:t></w:r></w:p>", "a".repeat(20_000));
            let mut scattered = vec![Step::Insert { text: "b".into(), revision: sari(), anchor: None }];
            for unit in 0..20_000 {
                if unit > 0 {
                    scattered.push(Step::Insert { text: "b".into(), revision: sari(), anchor: Some(unit - 1) });
                }
                scattered.push(Step::Keep { unit, change: Change::Same });
            }
            let cases = [
                ("long case", long_case()),
                ("long paste", (one.clone(), typed("abcdefghij".repeat(30_000)))),
                ("long tabs", (one, typed("\t".repeat(100_000)))),
                ("scattered insertions", (many, scattered)),
            ];
            for (name, (body, steps)) in cases {
                measure(name, &body, &steps);
            }
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "preserve::emit::tests::holds_no_more_than_it_charges", "--test-threads=1", "--nocapture"])
            .env(MEASURING, "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn measure(name: &str, body: &str, steps: &[Step]) {
        {
            let (bytes, scanned) = scanned(body);
            let none = BTreeMap::new();
            let source = Source { bytes: &bytes, rewritten: &none };
            let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
            // The caller's set of paragraph ids, with room for those the new marks take.
            let mut used = HashSet::with_capacity(16);
            used.insert(1);
            let mut ids = Ids::new(10_000, used);
            let start = wordcraft_testkit::counting::reset_peak();
            let written = rewrite(&source, "w", "word/document.xml", &scanned, steps, &mut ids, &mut budget).unwrap();
            let allocated = u64::try_from(wordcraft_testkit::counting::peak() - start).unwrap();
            let kept = u64::try_from(wordcraft_testkit::counting::held() - start).unwrap();
            eprintln!(
                "{name}: {} bytes written; allocated {allocated} at most, {kept} kept; charged {} at most, {} kept",
                written.len(),
                budget.peak(),
                budget.held()
            );
            assert!(written.len() > bytes.len() / 2);
            assert!(allocated <= budget.peak(), "{name}: allocated {allocated}, charged {}", budget.peak());
            assert!(kept <= budget.held(), "{name}: kept {kept}, charged {}", budget.held());
            assert_eq!(budget.held(), held_by(&written));
        }
    }

    #[test]
    fn round_trips_a_bookmark_inside_a_decided_insertion() {
        // Accepted, the bookmark stays where it is; rejected, it goes with the inserted text, as the
        // engine removes its character, and the inspector counts nothing lost.
        let body = format!(
            r#"<w:p><w:r><w:t xml:space="preserve">Pay </w:t></w:r><w:ins w:id="5" {ANA}><w:r><w:t xml:space="preserve">the </w:t></w:r><w:bookmarkStart w:id="1" w:name="b"/><w:r><w:t xml:space="preserve">full </w:t></w:r><w:bookmarkEnd w:id="1"/></w:ins><w:r><w:t>rent.</w:t></w:r></w:p>"#
        );
        let decided = |accept: bool| {
            round_trip(&body, |original, _| {
                let mut edited = original.clone();
                let length = edited.len();
                decide(&mut edited, 0, length, accept);
                vec![edited]
            })
            .unwrap()
        };
        let accepted = decided(true);
        only_tracked(&accepted, 0, 0, crate::inspect::Resolved { accepted: 1, rejected: 0 });
        assert!(
            accepted
                .written
                .contains(r#"<w:bookmarkStart w:id="1" w:name="b"/><w:r><w:t xml:space="preserve">full </w:t></w:r><w:bookmarkEnd w:id="1"/>"#)
        );
        let rejected = decided(false);
        only_tracked(&rejected, 0, 0, crate::inspect::Resolved { accepted: 0, rejected: 1 });
        assert_eq!(rejected.written, r#"<w:p><w:r><w:t xml:space="preserve">Pay </w:t></w:r><w:r><w:t>rent.</w:t></w:r></w:p>"#);
    }

    #[test]
    fn writes_into_a_paragraph_written_empty_and_a_run_whose_properties_come_last() {
        let typed = [Step::Insert { text: "New".into(), revision: sari(), anchor: None }];
        assert_eq!(
            rewritten("<w:p/>", &typed, &[]).unwrap(),
            format!(r#"<w:p><w:ins w:id="11" {SARI_ATTRIBUTES}><w:r><w:t xml:space="preserve">New</w:t></w:r></w:ins></w:p>"#)
        );
        let split = [Step::Insert { text: "a".into(), revision: sari(), anchor: None }, Step::Break { revision: sari() }];
        assert_eq!(
            rewritten(r#"<w:p w14:paraId="00000001"/>"#, &split, &[1]).unwrap(),
            format!(
                r#"<w:p w14:paraId="00000002"><w:pPr><w:rPr><w:ins w:id="11" {SARI_ATTRIBUTES}/></w:rPr></w:pPr><w:ins w:id="12" {SARI_ATTRIBUTES}><w:r><w:t xml:space="preserve">a</w:t></w:r></w:ins></w:p><w:p w14:paraId="00000001"></w:p>"#
            )
        );
        // The reader formats a run by its first properties wherever they stand; each piece repeats
        // them once, and the text before them is written once.
        let body = r#"<w:p><w:r><w:t>ab</w:t><w:rPr><w:b/></w:rPr></w:r></w:p>"#;
        let deleted = round_trip(body, |original, after| {
            let record = session(after, RevisionKind::Delete);
            let mut edited = original.clone();
            delete(&mut edited, 1, 2, record);
            vec![edited]
        })
        .unwrap();
        assert_eq!(
            deleted.written,
            format!(
                r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">a</w:t></w:r><w:del w:id="101" {SARI_ATTRIBUTES}><w:r><w:rPr><w:b/></w:rPr><w:delText xml:space="preserve">b</w:delText></w:r></w:del></w:p>"#
            )
        );
    }

    #[test]
    fn keeps_the_bytes_inside_a_run_it_cuts() {
        let body = "<w:p><w:r>\n  <w:rPr><w:b/></w:rPr>\n  <w:t>ab</w:t>\n  <w:tab/>\n</w:r></w:p>";
        let deleted = round_trip(body, |original, after| {
            let record = session(after, RevisionKind::Delete);
            let mut edited = original.clone();
            delete(&mut edited, 1, 2, record);
            vec![edited]
        })
        .unwrap();
        only_tracked(&deleted, 0, 1, undecided());
        assert_eq!(
            deleted.written,
            format!(
                "<w:p><w:r>\n  <w:rPr><w:b/></w:rPr>\n  <w:t xml:space=\"preserve\">a</w:t></w:r><w:del w:id=\"101\" {SARI_ATTRIBUTES}><w:r><w:rPr><w:b/></w:rPr><w:delText xml:space=\"preserve\">b</w:delText></w:r></w:del><w:r><w:rPr><w:b/></w:rPr>\n  <w:tab/>\n</w:r></w:p>"
            )
        );
    }

    /// A small generator, so the cases below are the same on every run.
    struct Random(u64);

    impl Random {
        fn below(&mut self, bound: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            usize::try_from(self.0 % u64::try_from(bound.max(1)).unwrap()).unwrap()
        }
    }

    /// A paragraph of runs, other authors' insertions and deletions, hyperlinks, bookmarks and
    /// proofing marks.
    fn generated_paragraph(random: &mut Random) -> String {
        let mut body = String::from("<w:p>");
        if random.below(2) == 0 {
            body.push_str(r#"<w:pPr><w:jc w:val="both"/><w:rPr><w:i/></w:rPr></w:pPr>"#);
        }
        for piece in 0..random.below(7) + 1 {
            let bold = if random.below(3) == 0 { "<w:rPr><w:b/></w:rPr>" } else { "" };
            let text = format!("w{piece} ab");
            let run = format!(r#"<w:r>{bold}<w:t xml:space="preserve">{text} </w:t></w:r>"#);
            match random.below(9) {
                0 => body.push_str(&format!(r#"<w:ins w:id="{piece}" {ANA}>{run}</w:ins>"#)),
                1 => body.push_str(&format!(
                    r#"<w:del w:id="{piece}" {BOB}><w:r>{bold}<w:delText xml:space="preserve">{text} </w:delText></w:r></w:del>"#
                )),
                2 => body.push_str(&format!(r#"<w:hyperlink w:anchor="h{piece}">{run}</w:hyperlink>"#)),
                3 => body.push_str(&format!(r#"<w:bookmarkStart w:id="{piece}" w:name="b{piece}"/>{run}<w:bookmarkEnd w:id="{piece}"/>"#)),
                4 => body.push_str(&format!(r#"<w:proofErr w:type="spellStart"/>{run}<w:proofErr w:type="spellEnd"/>"#)),
                5 => body.push_str(&format!(r#"<w:hyperlink w:anchor="i{piece}"><w:ins w:id="{piece}" {ANA}>{run}</w:ins></w:hyperlink>"#)),
                6 => body.push_str(&format!(r#"<w:sdt><w:sdtPr><w:tag w:val="c{piece}"/></w:sdtPr><w:sdtContent>{run}</w:sdtContent></w:sdt>"#)),
                7 => body.push_str(&format!(
                    r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>{piece}</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>{run}"#
                )),
                _ => body.push_str(&run),
            }
        }
        body.push_str("</w:p>");
        body
    }

    /// A character boundary of `paragraph`, at random.
    fn boundary_in(paragraph: &Paragraph, random: &mut Random) -> usize {
        let boundaries: Vec<usize> = paragraph.text.char_indices().map(|(at, _)| at).chain(std::iter::once(paragraph.len())).collect();
        boundaries[random.below(boundaries.len())]
    }

    /// Edits as an edit session makes them: decisions, deletions of text not deleted yet, typing
    /// with the formatting of the character before, and Enter.
    fn generated_edit(original: &Paragraph, after: &mut Document, random: &mut Random) -> Vec<Paragraph> {
        let inserted = session(after, RevisionKind::Insert);
        let deleted = session(after, RevisionKind::Delete);
        let mut edited = original.clone();
        for _ in 0..random.below(3) + 1 {
            let (mut from, mut to) = (boundary_in(&edited, random), boundary_in(&edited, random));
            if from > to {
                std::mem::swap(&mut from, &mut to);
            }
            match random.below(4) {
                0 => decide(&mut edited, from, to, random.below(2) == 0),
                1 => {
                    let untouched: Vec<(usize, usize)> = edited
                        .run_ranges()
                        .filter(|(range, props)| {
                            range.end > from && range.start < to && props.del.is_none() && props.ins.is_none_or(|record| record < inserted)
                        })
                        .map(|(range, _)| (range.start.max(from), range.end.min(to)))
                        .collect();
                    for (start, end) in untouched {
                        delete(&mut edited, start, end, deleted);
                    }
                }
                _ => {
                    let props = match edited.text[..from].chars().next_back() {
                        Some(ch) => plain(&edited, from - ch.len_utf8()),
                        None => edited.mark.clone(),
                    };
                    type_text(&mut edited, from, "xy ", &CharProps { ins: None, del: None, ..props }, inserted);
                }
            }
        }
        if random.below(3) == 0 {
            let at = boundary_in(&edited, random);
            let (first, second) = split(&edited, at, inserted);
            return vec![first, second];
        }
        vec![edited]
    }

    #[test]
    fn writes_what_the_model_shows_for_generated_edits() {
        let mut random = Random(0x9E37_79B9_7F4A_7C15);
        let (mut written, mut refused, mut without_controls) = (0, BTreeMap::new(), 0);
        for case in 0..400 {
            let body = generated_paragraph(&mut random);
            let seed = random.below(usize::MAX);
            let done = round_trip(&body, |original, after| generated_edit(original, after, &mut Random(u64::try_from(seed).unwrap() | 1)));
            // embed: since the upstream merge the reader marks a content control's ends with
            // objects the planner does not compare yet: such a paragraph is always refused.
            let control = body.contains("<w:sdt>");
            if control {
                assert!(done.is_err(), "case {case}: a content control was written: {body}");
            } else {
                without_controls += 1;
            }
            match done {
                Ok(done) => {
                    written += 1;
                    // Read back as the model (`round_trip`); nothing lost or altered. Untracked
                    // characters are not asserted here: W1's diff can pair a decided change's
                    // characters with equal ones beside it (an accepted " it" before " now." reads
                    // as rejected plus three untracked characters), one case in this set.
                    let inspection = done.inspected.as_ref().unwrap();
                    assert!(
                        inspection.losses.is_empty() && inspection.altered_revisions == 0 && !inspection.too_many_changes,
                        "case {case}: {inspection:?} {body} => {}",
                        done.written
                    );
                    assert!(inspection.new_revisions.iter().all(|entry| entry.author == "Sari Wulan"), "case {case}: {inspection:?}");
                }
                // What the planner refuses (Task 5: a formatting or mark it cannot record, an
                // ambiguous pairing it fails closed on) and what the writer refuses (a split hyperlink).
                Err(reason) => {
                    assert!(
                        matches!(
                            reason,
                            Unpreservable::SplitWrapper { .. }
                                | Unpreservable::Formatting
                                | Unpreservable::Altered
                                | Unpreservable::Structure
                                | Unpreservable::Untracked
                        ) || (control && matches!(reason, Unpreservable::Unsupported(_))),
                        "case {case}: {reason:?} {body}"
                    );
                    *refused.entry(format!("{reason:?}")).or_insert(0) += 1;
                }
            }
        }
        eprintln!("{written} written of {without_controls} without a content control, refused: {refused:?}");
        assert!(written * 2 > without_controls, "{written} written of {without_controls} without a content control, refused: {refused:?}");
    }

    #[test]
    fn keeps_a_hyperlink_or_content_control_whose_content_went_with_a_change_decided_inside_it() {
        for (open, close) in [
            (r#"<w:hyperlink w:anchor="terms">"#, "</w:hyperlink>"),
            (r#"<w:sdt><w:sdtPr><w:tag w:val="t"/></w:sdtPr><w:sdtContent>"#, "</w:sdtContent></w:sdt>"),
        ] {
            let body = format!(
                r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r>{open}<w:ins w:id="5" {ANA}><w:r><w:t>terms</w:t></w:r></w:ins>{close}<w:r><w:t>.</w:t></w:r></w:p>"#
            );
            let rejected = round_trip(&body, |original, _| {
                let mut edited = original.clone();
                let length = edited.len();
                decide(&mut edited, 0, length, false);
                vec![edited]
            });
            // embed: the reader marks a content control's ends with objects since the upstream
            // merge, which the planner refuses until the scan and the writer place them.
            if open.starts_with("<w:sdt>") {
                assert!(matches!(rejected, Err(Unpreservable::Unsupported(_))), "{:?}", rejected.as_ref().err());
                continue;
            }
            let rejected = rejected.unwrap();
            only_tracked(&rejected, 0, 0, crate::inspect::Resolved { accepted: 0, rejected: 1 });
            assert_eq!(
                rejected.written,
                format!(r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r>{open}{close}<w:r><w:t>.</w:t></w:r></w:p>"#)
            );
        }
    }

    // ---- Fix round 1 (6C-48).

    /// The paragraph swept without the no-op short cut: what the walk itself writes.
    fn swept(body: &str, steps: &[Step]) -> (Vec<u8>, Vec<u8>) {
        let (bytes, scanned) = scanned(body);
        let none = BTreeMap::new();
        let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
        let mut ledger = Ledger { budget: &mut budget, held: 0 };
        let mut ids = Ids::new(100, HashSet::new());
        let out = write(&Source { bytes: &bytes, rewritten: &none }, "w", "word/document.xml", &scanned, steps, &mut ids, &mut ledger).unwrap();
        (bytes[scanned.span()].to_vec(), out)
    }

    /// Gap bytes (white space, comments, processing instructions) after every tag outside text and
    /// run properties, inside the paragraph.
    fn inject(body: &str, random: &mut Random) -> String {
        const GAPS: &[&str] = &["", "", " ", "\n  ", "<!--c-->", "<?pi x?>", "\n<!-- a > b -->\t"];
        let mut out = String::new();
        let (mut in_text, mut in_rpr) = (false, false);
        let mut rest = body;
        while let Some(close) = rest.find('>') {
            let (tag_part, after) = rest.split_at(close + 1);
            out.push_str(tag_part);
            let tag = &tag_part[tag_part.rfind('<').unwrap_or(0)..];
            let closing = tag.starts_with("</");
            let empty = tag.ends_with("/>");
            let name: String =
                tag.trim_start_matches("</").trim_start_matches('<').chars().take_while(|c| !c.is_whitespace() && *c != '>' && *c != '/').collect();
            if matches!(name.as_str(), "w:t" | "w:delText" | "w:instrText" | "w:delInstrText") && !empty {
                in_text = !closing;
            }
            if name == "w:rPr" && !empty {
                in_rpr = !closing;
            }
            if !(in_text || in_rpr || (name == "w:p" && closing) || after.is_empty()) {
                out.push_str(GAPS[random.below(GAPS.len())]);
            }
            rest = after;
        }
        out.push_str(rest);
        out
    }

    fn gapped_corpus() -> Vec<String> {
        let mut bodies: Vec<String> = vec![
            // The review's two reproductions (6C-48 I1).
            format!(r#"<w:p><w:hyperlink w:anchor="a"><w:ins w:id="1" {ANA}><w:r><w:t>x</w:t></w:r></w:ins> </w:hyperlink></w:p>"#),
            format!(r#"<w:p><w:del w:id="1" {BOB}><w:r><w:delText>x</w:delText></w:r></w:del><!--after del--><w:sdt><w:sdtPr/><w:sdtContent><w:r><w:t>y</w:t></w:r></w:sdtContent></w:sdt></w:p>"#),
            "<w:p/>".into(),
            "<w:p >\n</w:p >".into(),
            r#"<w:p><w:hyperlink w:anchor="a"><w:hyperlink w:anchor="b"></w:hyperlink><w:r><w:t>x</w:t></w:r><w:smartTag w:uri="u" w:element="e"><w:customXml w:element="z"></w:customXml></w:smartTag></w:hyperlink><w:r><w:t>y</w:t></w:r></w:p>"#.into(),
            r#"<w:p><w:sdt><w:sdtPr><w:alias w:val="x"/></w:sdtPr><w:sdtEndPr/><w:sdtContent><w:ins w:id="7" w:author="A" w:date="D"><w:r><w:t>q</w:t></w:r></w:ins></w:sdtContent></w:sdt></w:p>"#.into(),
            r#"<w:p><w:moveFrom w:id="3" w:author="A" w:date="D"><w:r><w:t>mv</w:t></w:r></w:moveFrom><w:moveTo w:id="4" w:author="A" w:date="D"><w:r><w:t>mv</w:t></w:r></w:moveTo></w:p>"#.into(),
            r#"<w:p><w:r><w:t>a</w:t></w:r><w:ins w:id="1" w:author="A" w:date="D"></w:ins><w:del w:id="2" w:author="A" w:date="D"/><w:r><w:t>b</w:t></w:r></w:p>"#.into(),
        ];
        let mut random = Random(0x1234_5678_9ABC_DEF1);
        let hand_made = bodies.len();
        for index in 0..hand_made {
            let gapped = inject(&bodies[index], &mut random);
            bodies.push(gapped);
        }
        for _ in 0..300 {
            let body = generated_paragraph(&mut random);
            bodies.push(inject(&body, &mut random));
        }
        bodies
    }

    #[test]
    fn sweeps_untouched_markup_byte_for_byte() {
        // 6C-42, 6C-48 I1: every gap stays attached to the tag before it, so the walk itself, with
        // nothing changed, writes the source.
        for body in gapped_corpus() {
            let (_, scanned) = scanned(&body);
            let (source, written) = swept(&body, &keep(0..scanned.units.len(), Change::Same));
            assert_eq!(String::from_utf8_lossy(&written), String::from_utf8_lossy(&source));
        }
    }

    #[test]
    fn keeps_the_bytes_before_and_after_a_change_as_they_were() {
        // A deletion in the last run leaves everything before that run as written; typing before
        // the first character leaves everything after its run as written.
        let mut checked = 0;
        for body in gapped_corpus() {
            let (bytes, scanned) = scanned(&body);
            let count = scanned.units.len();
            let editable = |unit: &&Unit| {
                let Loc::Text { node, .. } = unit.loc else { return false };
                let chain = scanned.chain(scanned.nodes[node].wrapper());
                !chain.iter().any(|wrapper| scanned.wrappers[*wrapper].kind == WrapperKind::Deleted)
            };
            let source = &bytes[scanned.span()];
            if let Some(last) = scanned.units.iter().rposition(|unit| editable(&unit)) {
                let Loc::Text { node, .. } = scanned.units[last].loc else { panic!("an editable unit is text") };
                let mut steps = keep(0..count, Change::Same);
                steps[last] = Step::Keep { unit: last, change: Change::Deleted(sari()) };
                let written = rewritten(&body, &steps, &[]).unwrap();
                let before = node_span(&scanned.nodes[node]).start - scanned.span().start;
                assert!(written.as_bytes().starts_with(&source[..before]), "{body}\n=> {written}");
                checked += 1;
            }
            if let Some(first) = scanned.units.iter().position(|unit| editable(&unit)) {
                let Loc::Text { node, .. } = scanned.units[first].loc else { panic!("an editable unit is text") };
                let steps = [
                    keep(0..first, Change::Same),
                    vec![Step::Insert { text: "z".into(), revision: sari(), anchor: Some(first) }],
                    keep(first..count, Change::Same),
                ]
                .concat();
                if let Ok(written) = rewritten(&body, &steps, &[]) {
                    let after = node_span(&scanned.nodes[node]).end - scanned.span().start;
                    assert!(written.as_bytes().ends_with(&source[after..]), "{body}\n=> {written}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 500, "{checked}");
    }

    #[test]
    fn copies_a_paragraph_nothing_changes() {
        // 6C-48 M1: a paragraph written empty stays written empty.
        for body in ["<w:p/>", r#"<w:p w14:paraId="00000001" />"#, "<w:p><w:r><w:t>a</w:t></w:r></w:p>"] {
            let (_, scanned) = scanned(body);
            assert_eq!(rewritten(body, &keep(0..scanned.units.len(), Change::Same), &[]).unwrap(), body);
        }
    }

    #[test]
    fn refuses_ids_past_what_word_reads_and_writes_the_session_as_it_is() {
        let body = "<w:p><w:r><w:t>ab</w:t></w:r></w:p>";
        let (bytes, scanned) = scanned(body);
        let none = BTreeMap::new();
        let source = Source { bytes: &bytes, rewritten: &none };
        let deleted = [keep(0..1, Change::Deleted(sari())), keep(1..2, Change::Same)].concat();
        let write_with = |highest: u64, steps: &[Step]| {
            let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
            let written = rewrite(&source, "w", "word/document.xml", &scanned, steps, &mut Ids::new(highest, HashSet::new()), &mut budget);
            if written.is_err() {
                assert_eq!(budget.held(), 0);
            }
            written
        };
        // 6C-48 M2: the last id Word reads is i32::MAX.
        let top = u64::try_from(i32::MAX).unwrap();
        assert!(String::from_utf8(write_with(top - 1, &deleted).unwrap()).unwrap().contains(&format!(r#"w:id="{top}""#)));
        assert_eq!(write_with(top, &deleted), Err(Unpreservable::TooManyChanges));
        assert_eq!(write_with(u64::MAX, &deleted), Err(Unpreservable::TooManyChanges));
        // 6C-48 M3: the writer records the session as it is, or not at all.
        for revision in
            [Revision { author: "Sari".into(), date: String::new() }, Revision { author: "A \u{1}B".into(), date: "2026-10-10T08:00:00Z".into() }]
        {
            let steps = [keep(0..1, Change::Deleted(revision.clone())), keep(1..2, Change::Same)].concat();
            assert!(matches!(write_with(10, &steps), Err(Unpreservable::Verification(_))), "{revision:?}");
            let typed = [vec![Step::Insert { text: "z".into(), revision: revision.clone(), anchor: Some(0) }], keep(0..2, Change::Same)].concat();
            assert!(matches!(write_with(10, &typed), Err(Unpreservable::Verification(_))), "{revision:?}");
        }
        let escaped = Revision { author: "A <&\"'> B\tC".into(), date: "2026-10-10T08:00:00Z".into() };
        let steps = [keep(0..1, Change::Deleted(escaped)), keep(1..2, Change::Same)].concat();
        assert!(String::from_utf8(write_with(10, &steps).unwrap()).unwrap().contains(r#"w:author="A &lt;&amp;&quot;&apos;&gt; B&#9;C""#));
    }

    #[test]
    fn gives_each_copy_of_a_formatting_change_after_the_first_an_id_of_its_own() {
        // 6C-48 M4.
        let body = format!(r#"<w:p><w:r><w:rPr><w:b/><w:rPrChange w:id="9" {ANA}><w:rPr/></w:rPrChange></w:rPr><w:t>abc</w:t></w:r></w:p>"#);
        let steps = [keep(0..1, Change::Same), keep(1..2, Change::Deleted(sari())), keep(2..3, Change::Same)].concat();
        let written = rewritten(&body, &steps, &[]).unwrap();
        let ids: Vec<&str> = written.split(r#"<w:rPrChange w:id=""#).skip(1).map(|rest| rest.split('"').next().unwrap()).collect();
        assert_eq!(ids, ["9", "12", "13"], "{written}");
    }

    #[test]
    fn keeps_a_cut_texts_attributes() {
        // 6C-48 M5.
        let body = r#"<w:p><w:r><w:t w14:foo="1">a b</w:t><w:t xml:space="default" w14:bar="2">cd</w:t></w:r></w:p>"#;
        let steps = [keep(0..2, Change::Same), keep(2..4, Change::Deleted(sari())), keep(4..5, Change::Same)].concat();
        let written = rewritten(body, &steps, &[]).unwrap();
        assert_eq!(
            written,
            format!(
                r#"<w:p><w:r><w:t w14:foo="1" xml:space="preserve">a </w:t></w:r><w:del w:id="11" {SARI_ATTRIBUTES}><w:r><w:delText w14:foo="1" xml:space="preserve">b</w:delText></w:r><w:r><w:delText xml:space="preserve" w14:bar="2">c</w:delText></w:r></w:del><w:r><w:t xml:space="preserve" w14:bar="2">d</w:t></w:r></w:p>"#
            )
        );
    }

    #[test]
    fn refuses_to_copy_across_a_paragraph_already_rewritten() {
        // 6C-48 M6: a rewritten paragraph that reaches past either end of a copy cannot be copied
        // in part.
        let bytes = b"abcdefgh";
        let rewritten = BTreeMap::from([(2usize, (6usize, b"IN".to_vec()))]);
        let source = Source { bytes, rewritten: &rewritten };
        let mut out = Vec::new();
        assert!(source.copy(0..8, &mut out).is_ok());
        assert_eq!(out, b"abINgh");
        assert!(source.copy(0..4, &mut Vec::new()).is_err());
        assert!(source.copy(4..8, &mut Vec::new()).is_err());
        assert!(source.copy(6..8, &mut Vec::new()).is_ok());
    }

    /// A body in a document whose root writes WordprocessingML with prefix `w`, its first paragraph
    /// scanned.
    fn scanned_with_root(body: &str) -> (Vec<u8>, ScannedPara) {
        scanned_at(body, 0)
    }

    #[test]
    fn refuses_new_markup_where_the_parts_prefix_names_another_namespace() {
        // 6C-48 I3: the prefix new markup is written with must name WordprocessingML where it is
        // written; rebound on the paragraph or around it, the writer refuses.
        let main = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        let run = r#"<x:r><x:t>ab</x:t></x:r>"#;
        let rebound = [
            format!(r#"<x:p xmlns:x="{main}" xmlns:w="urn:other">{run}</x:p>"#),
            format!(r#"<w:sdt><w:sdtContent xmlns:x="{main}" xmlns:w="urn:other"><x:p>{run}</x:p></w:sdtContent></w:sdt>"#),
            format!(r#"<w:tbl><w:tr><w:tc xmlns:x="{main}" xmlns:w="urn:other"><x:p>{run}</x:p></w:tc></w:tr></w:tbl>"#),
            format!(r#"<x:p xmlns:x="{main}"><x:hyperlink x:anchor="a" xmlns:w="urn:other">{run}</x:hyperlink></x:p>"#),
        ];
        let typed = [vec![Step::Insert { text: "Z".into(), revision: sari(), anchor: Some(0) }], keep(0..2, Change::Same)].concat();
        let deleted = keep(0..2, Change::Deleted(sari()));
        for body in &rebound {
            let (_, scanned) = scanned_with_root(body);
            assert_eq!(scanned.units.len(), 2, "{body}");
            assert!(matches!(rewritten(body, &typed, &[]), Err(Unpreservable::Namespace { .. })), "{body}");
            assert!(matches!(rewritten(body, &deleted, &[]), Err(Unpreservable::Namespace { .. })), "{body}");
            // Decisions write no new markup.
            assert!(rewritten(body, &keep(0..2, Change::Same), &[]).is_ok(), "{body}");
        }
        // Rebound on the body, with the paragraph written in another prefix.
        let body_rebound = format!(r#"<x:p xmlns:x="{main}">{run}</x:p>"#);
        let document = format!(r#"<w:document {W}><w:body xmlns:w="urn:other" xmlns:x="{main}">{body_rebound}</w:body></w:document>"#);
        let (bytes, scanned) = scanned_document(&document);
        let none = BTreeMap::new();
        let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
        let written = rewrite(
            &Source { bytes: &bytes, rewritten: &none },
            "w",
            "word/document.xml",
            &scanned,
            &typed,
            &mut Ids::new(10, HashSet::new()),
            &mut budget,
        );
        assert!(matches!(written, Err(Unpreservable::Namespace { .. })), "{written:?}");
        // Another prefix bound to WordprocessingML, `w` untouched, or `w` bound again to it: written.
        for body in [format!(r#"<x:p xmlns:x="{main}">{run}</x:p>"#), format!(r#"<x:p xmlns:x="{main}" xmlns:w="{main}">{run}</x:p>"#)] {
            assert!(rewritten(&body, &typed, &[]).unwrap().contains("<w:ins "), "{body}");
        }
    }

    /// The first body paragraph of `document` (a whole `word/document.xml`), scanned.
    fn scanned_document(document: &str) -> (Vec<u8>, ScannedPara) {
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        let bytes =
            zip_entries(&[("_rels/.rels".into(), rels.as_bytes().to_vec()), ("word/document.xml".into(), document.as_bytes().to_vec())]).unwrap();
        let pkg = Package::open_with(&bytes, &ReadLimits::default()).unwrap();
        let wanted = BTreeMap::from([("word/document.xml".to_string(), BTreeSet::from([0u32]))]);
        let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
        let mut part = scan_package(&pkg, &wanted, &mut budget).unwrap().remove("word/document.xml").unwrap();
        let paragraph = part.paragraphs.remove(&0).unwrap();
        (part.bytes.to_vec(), paragraph)
    }
}

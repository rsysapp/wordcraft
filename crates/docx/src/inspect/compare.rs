//! embed: a story of the edited package, with the save's own tracked changes undone, against the
//! same story of the base. What differs is either a base change resolved, or a fact the host
//! refuses on: content changed without tracking, a base change altered, an element lost.
//!
//! Undoing the save's changes drops what it inserted. What it inserted can still change how a
//! reader shows base content beside it: field characters that turn base text into field code or
//! a field's result, a paragraph mark whose properties govern base text, a section break. So
//! every item carries its context, worked out on the stream as the reader sees it (nothing
//! undone), and a base item whose context differs is a change.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::{BuildHasher, Hasher as _};

use super::LossKind;
use super::budget::{Budget, table_slot, tree_room, tree_slot, units, vector_slot};
use super::diff::{Step, script, script_bytes};
use super::digest::Hasher;
use super::opc::finish;
use super::story::{Context, EMPTY, FormatRev, Item, MARK_BASE, MarkKind, Tables, Wrap, WrapKind, is_bidi_control};

/// Memory charged for the open fields while contexts are worked out: at most `MAX_FIELD_DEPTH`
/// hashers.
const FRAMES_BYTES: u64 = 64 * 1024;

/// Fields nested deeper than this are refused: no document needs them, and each open field's code
/// is followed character by character.
const MAX_FIELD_DEPTH: usize = 32;

/// The save's own changes, known by author and date (text ids).
pub(super) type Pairs = HashSet<(u32, u32)>;

/// Undoes the save's own changes: its insertions dropped, its deletions and formatting changes
/// unmarked (formatting back to what it was before), the anchors of its new comments dropped.
pub(super) struct Projector<'a> {
    pub new_pairs: &'a Pairs,
    pub new_comments: &'a BTreeSet<String>,
    stacks: HashMap<u32, Option<u32>>,
    formats: HashMap<u32, u32>,
    /// Memory its caches took since it was last charged.
    pending: u64,
}

/// Memory charged for each entry of the projector's caches.
const STACK_CACHE_BYTES: u64 = table_slot::<(u32, Option<u32>)>();
const FORMAT_CACHE_BYTES: u64 = table_slot::<(u32, u32)>();
/// Memory charged for each comment id in a set of ids, besides the id.
pub(super) const ID_BYTES: u64 = tree_slot::<String, ()>();

const fn larger(one: u64, other: u64) -> u64 {
    if one > other { one } else { other }
}

/// A loss as the tally keeps it.
type LossKey = (String, LossKind, Option<String>);

/// Memory charged for each record an inspection reports or tallies (an added part, a loss, a
/// changed content type or relationship, an author's counts), besides its strings: the record in
/// its map or growing vector, and its copy into the answer. The most any of them takes.
pub(super) const RECORD_BYTES: u64 = larger(
    larger(
        tree_slot::<LossKey, u32>() + std::mem::size_of::<super::Loss>() as u64,
        tree_slot::<u32, super::AuthorRevisions>() + std::mem::size_of::<super::AuthorRevisions>() as u64,
    ),
    larger(
        larger(vector_slot::<super::AddedPart>(), vector_slot::<super::ContentTypeChange>()),
        larger(vector_slot::<super::RelationshipChange>(), tree_slot::<String, u32>() + std::mem::size_of::<super::AuthorCount>() as u64),
    ),
);
/// Memory charged for each change the tally finds resolved: its entry in a set.
const RESOLUTION_BYTES: u64 = tree_slot::<Resolution, ()>();

/// Memory an entry new to one of the tally's maps or sets holds, besides what it owns: its share of
/// the tree (`slot`), and the tree's first node when it is the first entry.
fn tree_entry(empty: bool, slot: u64, first: u64) -> u64 {
    if empty { slot.saturating_add(first) } else { slot }
}

impl<'a> Projector<'a> {
    pub(super) fn new(new_pairs: &'a Pairs, new_comments: &'a BTreeSet<String>) -> Self {
        Projector { new_pairs, new_comments, stacks: HashMap::new(), formats: HashMap::new(), pending: 0 }
    }

    /// Hold what the caches took since the last charge.
    pub(super) fn charge(&mut self, budget: &mut Budget) -> Result<(), String> {
        budget.hold(std::mem::take(&mut self.pending))
    }

    pub(super) fn is_new(&self, author: u32, date: u32) -> bool {
        self.new_pairs.contains(&(author, date))
    }

    /// A stack without the save's wrappers; `None` when a wrapper of the save inserted it.
    fn stack(&mut self, tables: &mut Tables, id: u32) -> Option<u32> {
        if let Some(cached) = self.stacks.get(&id) {
            return *cached;
        }
        let projected = match tables.stack_top(id) {
            None => Some(EMPTY),
            Some((parent, wrap)) => {
                let new = self.is_new(wrap.author, wrap.date);
                if new && wrap.kind.inserts() {
                    None
                } else {
                    match self.stack(tables, parent) {
                        None => None,
                        Some(parent) if new => Some(parent),
                        Some(parent) => Some(tables.push_stack(parent, wrap)),
                    }
                }
            }
        };
        self.stacks.insert(id, projected);
        self.pending = self.pending.saturating_add(STACK_CACHE_BYTES);
        projected
    }

    /// A format with the save's formatting changes reverted.
    pub(super) fn format(&mut self, tables: &mut Tables, id: u32) -> u32 {
        if let Some(cached) = self.formats.get(&id) {
            return *cached;
        }
        let projected = match tables.format_of(id).cloned() {
            Some(mut format) => {
                for slot in &mut format.slots {
                    if let Some(rev) = slot.rev.take() {
                        // Word lists no revision for a table's, row's or cell's property change,
                        // so what the save changed there stands, untracked.
                        if self.is_new(rev.author, rev.date) && UNSHOWN_CHANGES.contains(&slot.name) {
                            continue;
                        }
                        if self.is_new(rev.author, rev.date) {
                            slot.value = rev.old;
                            if matches!(slot.name, "rPr" | "pPr.rPr") {
                                format.hidden = rev.old_hidden;
                            }
                        } else {
                            slot.rev = Some(rev);
                        }
                    }
                }
                format.slots.retain(|slot| slot.value != EMPTY || slot.rev.is_some());
                // Which change hides it is a fact about the save, not what is compared.
                format.hidden_by = None;
                tables.format(format)
            }
            None => id,
        };
        self.formats.insert(id, projected);
        self.pending = self.pending.saturating_add(FORMAT_CACHE_BYTES);
        projected
    }

    /// Undo the save's changes in `items`, in place.
    pub(super) fn project(&mut self, tables: &mut Tables, items: &mut Vec<Item>, budget: &mut Budget) -> Result<(), String> {
        let mut out_of_work = false;
        items.retain_mut(|item| {
            if out_of_work || !budget.spend(2) {
                out_of_work = true;
                return false;
            }
            if let Some(comment) = tables.mark_info(item.piece).and_then(|mark| mark.comment.as_ref())
                && self.new_comments.contains(comment)
            {
                return false;
            }
            let Some(wraps) = self.stack(tables, item.wraps) else { return false };
            item.wraps = wraps;
            item.format = self.format(tables, item.format);
            true
        });
        if out_of_work {
            return Err(super::budget::TOO_MUCH_WORK.into());
        }
        self.charge(budget)?;
        tables.charge(budget)
    }
}

/// Property slots whose tracked changes Word does not show as revisions (`w:tblPrChange`,
/// `w:tblGridChange`, `w:tblPrExChange`, `w:trPrChange`, `w:tcPrChange`): the save's changes there
/// are not undone as tracked.
const UNSHOWN_CHANGES: &[&str] = &["tblPr", "tblGrid", "tblPrEx", "trPr", "tcPr"];

// ---- Context --------------------------------------------------------------------------------

/// An open field: in its code (with the code so far) or in its result (with its instruction).
struct Frame {
    instruction: Option<u32>,
    code: Hasher,
}

fn describe(frames: &[Frame], tables: &mut Tables, budget: &mut Budget) -> Result<u32, String> {
    if frames.is_empty() {
        return Ok(EMPTY);
    }
    let mut hasher = Hasher::default();
    for frame in frames {
        match frame.instruction {
            None => hasher.field(b'C', &[]),
            Some(instruction) => hasher.field(b'R', &instruction.to_le_bytes()),
        }
    }
    Ok(tables.content(finish(hasher, budget)?))
}

/// Give each item the context a reader shows it in. `view` gives the formatting as compared (the
/// edited package's with the save's formatting changes reverted, so a tracked change of the
/// governing mark that records the base's properties explains them).
pub(super) fn assign_contexts(
    items: &mut [Item],
    tables: &mut Tables,
    view: &mut dyn FnMut(&mut Tables, u32) -> u32,
    budget: &mut Budget,
) -> Result<(), String> {
    // Fields, forwards: an item is outside any field, in a field's code, or in its result.
    // Each item's field description (four bytes), and the open fields' code so far.
    let fields_held = units(items.len()).saturating_mul(4).saturating_add(FRAMES_BYTES);
    budget.hold(fields_held)?;
    let mut fields: Vec<u32> = Vec::with_capacity(items.len());
    let mut frames: Vec<Frame> = Vec::new();
    let mut current = EMPTY;
    for item in items.iter() {
        budget.work(2 + units(frames.len()))?;
        fields.push(current);
        let kind = tables.mark_info(item.piece).map(|mark| mark.kind);
        match kind {
            Some(MarkKind::FieldBegin) => {
                if frames.len() >= MAX_FIELD_DEPTH {
                    return Err("fields nested deeper than an inspection reads".into());
                }
                frames.push(Frame { instruction: None, code: Hasher::default() });
            }
            Some(MarkKind::FieldSeparate) => {
                if let Some(frame) = frames.last_mut()
                    && frame.instruction.is_none()
                {
                    let code = std::mem::take(&mut frame.code);
                    frame.instruction = Some(tables.content(finish(code, budget)?));
                }
            }
            Some(MarkKind::SimpleField(instruction)) => {
                if frames.len() >= MAX_FIELD_DEPTH {
                    return Err("fields nested deeper than an inspection reads".into());
                }
                frames.push(Frame { instruction: Some(instruction), code: Hasher::default() });
            }
            Some(MarkKind::FieldEnd | MarkKind::SimpleFieldEnd) => {
                frames.pop();
            }
            _ => {
                for frame in frames.iter_mut().filter(|frame| frame.instruction.is_none()) {
                    frame.code.update(&item.piece.to_le_bytes());
                }
                continue;
            }
        }
        current = describe(&frames, tables, budget)?;
    }
    // Paragraph and section, backwards: an item is governed by the next paragraph mark, and its
    // section by the next mark that ends one (or the body's own section properties).
    let mut paragraph = (EMPTY, None);
    let mut section = (EMPTY, None);
    let slot = |tables: &Tables, format: u32, name: &str| -> (u32, Option<u32>) {
        let (value, rev) = tables.format_of(format).map(|format| format.slot(name)).unwrap_or((EMPTY, None));
        (value, rev.map(|rev: &FormatRev| rev.old))
    };
    for (item, field) in items.iter_mut().zip(fields).rev() {
        budget.work(2)?;
        match tables.mark_info(item.piece).map(|mark| mark.kind) {
            Some(MarkKind::Paragraph) => {
                let format = view(tables, item.format);
                paragraph = slot(tables, format, "pPr");
                let ends = slot(tables, format, "pPr.sectPr");
                if ends != (EMPTY, None) {
                    section = ends;
                }
            }
            Some(MarkKind::Section) => {
                let format = view(tables, item.format);
                section = slot(tables, format, "sectPr");
            }
            _ => {}
        }
        item.context =
            tables.context(Context { field, paragraph: paragraph.0, paragraph_old: paragraph.1, section: section.0, section_old: section.1 });
    }
    budget.release(fields_held);
    tables.charge(budget)
}

// ---- Facts about the save's own changes --------------------------------------------------------

/// What one author's new tracked changes hold.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Holds {
    pub hidden: u32,
    pub field_codes: u32,
    pub bidi_controls: u32,
    pub objects: u32,
}

/// Memory charged for each move followed: its entry, and the two hashers' short buffers of what
/// left and arrived (each at most 256 bytes, growing by doubling).
const MOVE_BYTES: u64 = tree_slot::<(u32, u32), (Hasher, Hasher)>() + 2 * 384;
/// Memory charged for each stack whose facts are cached.
const STACK_FACTS_BYTES: u64 = table_slot::<(u32, StackFacts)>();
/// Memory charged for each author whose insertions' holds are counted.
const HOLDS_BYTES: u64 = tree_slot::<u32, Holds>();

/// The entry for `author` in `holds`, its memory charged when it is new.
fn holds_of<'h>(holds: &'h mut BTreeMap<u32, Holds>, author: u32, budget: &mut Budget) -> Result<&'h mut Holds, String> {
    if !holds.contains_key(&author) {
        budget.hold(HOLDS_BYTES)?;
    }
    Ok(holds.entry(author).or_default())
}

/// The author of the save's innermost insertion around a stack, and the save's move it is in.
type StackFacts = (Option<u32>, Option<(bool, u32, u32)>);

/// What the save's insertions hold, per author; and how many of its moves arrive as other text
/// than they left (an untracked change).
pub(super) fn facts(
    items: &[Item],
    tables: &Tables,
    new_pairs: &Pairs,
    holds: &mut BTreeMap<u32, Holds>,
    budget: &mut Budget,
) -> Result<u32, String> {
    let mut stacks: HashMap<u32, StackFacts> = HashMap::new();
    let mut moves: BTreeMap<(u32, u32), (Hasher, Hasher)> = BTreeMap::new();
    for item in items {
        budget.work(2)?;
        if !stacks.contains_key(&item.wraps) {
            budget.hold(STACK_FACTS_BYTES)?;
        }
        let (inserter, moved) = *stacks.entry(item.wraps).or_insert_with(|| {
            let wraps: Vec<Wrap> = tables.wraps_of(item.wraps);
            let new = |wrap: &&Wrap| new_pairs.contains(&(wrap.author, wrap.date));
            let inserter = wraps.iter().rev().filter(new).find(|wrap| wrap.kind.inserts()).map(|wrap| wrap.author);
            let moved = wraps.iter().rev().filter(new).find_map(|wrap| match wrap.kind {
                WrapKind::MoveFrom => Some((false, wrap.author, wrap.date)),
                WrapKind::MoveTo => Some((true, wrap.author, wrap.date)),
                _ => None,
            });
            (inserter, moved)
        });
        if let Some((arrived, author, date)) = moved {
            if !moves.contains_key(&(author, date)) {
                budget.hold(MOVE_BYTES)?;
            }
            let (from, to) = moves.entry((author, date)).or_default();
            if arrived { to } else { from }.update(&item.piece.to_le_bytes());
        }
        // A tracked formatting change of the save that hides what it formats (directly or
        // through a style).
        if let Some((author, date)) = tables.format_of(item.format).and_then(|format| format.hidden_by)
            && new_pairs.contains(&(author, date))
        {
            let entry = holds_of(holds, author, budget)?;
            entry.hidden = entry.hidden.saturating_add(1);
        }
        let Some(author) = inserter else { continue };
        let entry = holds_of(holds, author, budget)?;
        let format = tables.format_of(item.format);
        let mark = tables.mark_info(item.piece).map(|mark| mark.kind);
        if format.is_some_and(|format| format.hidden) {
            entry.hidden = entry.hidden.saturating_add(1);
        }
        if format.is_some_and(|format| format.instr)
            || matches!(mark, Some(MarkKind::FieldBegin | MarkKind::FieldSeparate | MarkKind::FieldEnd | MarkKind::SimpleField(_)))
        {
            entry.field_codes = entry.field_codes.saturating_add(1);
        }
        if item.piece < MARK_BASE && is_bidi_control(item.piece) {
            entry.bidi_controls = entry.bidi_controls.saturating_add(1);
        }
        // Opaque content (a text box, a picture, anything compared whole) is reported as an
        // object, and what it holds is counted with the rest.
        let held = tables.mark_info(item.piece).map(|mark| mark.holds).unwrap_or_default();
        if matches!(mark, Some(MarkKind::Object)) || held.content {
            entry.objects = entry.objects.saturating_add(1);
        }
        entry.hidden = entry.hidden.saturating_add(held.hidden);
        entry.field_codes = entry.field_codes.saturating_add(held.field_codes);
        entry.bidi_controls = entry.bidi_controls.saturating_add(held.bidi_controls);
    }
    let mut differing = 0u32;
    for (_, (from, to)) in moves {
        if from.finish() != to.finish() {
            differing = differing.saturating_add(1);
        }
    }
    Ok(differing)
}

/// A field of the base open at the current position, and what the save did inside it.
struct OpenField {
    /// Its start was there before the save (not inserted by it).
    base: bool,
    /// A tracked change of the save is inside it.
    touched: bool,
    /// Every item of it, start to end, is in a deletion of the save.
    all_deleted: bool,
}

/// How a stack relates to the save: the author of its innermost change by the save, whether a
/// change of the save inserts it, whether one deletes it.
type StackRole = (Option<u32>, bool, bool);
/// Memory charged for each stack whose role is cached.
const ROLE_BYTES: u64 = table_slot::<(u32, StackRole)>();

/// A base field is its start, code, separator, result and end together. The save may delete it
/// whole; any other tracked change inside it (code narrowed, a separator deleted, a character
/// inserted) changes what the field shows once accepted or updated, so each such field is one
/// untracked change. Every tracked change of the save touching a base field counts in its
/// author's `field_codes`.
pub(super) fn field_structure(
    items: &[Item],
    tables: &Tables,
    new_pairs: &Pairs,
    holds: &mut BTreeMap<u32, Holds>,
    budget: &mut Budget,
) -> Result<u32, String> {
    let mut roles: HashMap<u32, StackRole> = HashMap::new();
    let mut open: Vec<OpenField> = Vec::new();
    let mut changed = 0u32;
    let close = |field: OpenField, changed: &mut u32| {
        if field.base && field.touched && !field.all_deleted {
            *changed = changed.saturating_add(1);
        }
    };
    for item in items {
        budget.work(3 + units(open.len()))?;
        if !roles.contains_key(&item.wraps) {
            budget.hold(ROLE_BYTES)?;
        }
        let (author, inserted, deleted) = *roles.entry(item.wraps).or_insert_with(|| {
            let wraps = tables.wraps_of(item.wraps);
            let new: Vec<&Wrap> = wraps.iter().filter(|wrap| new_pairs.contains(&(wrap.author, wrap.date))).collect();
            let inserted = new.iter().any(|wrap| wrap.kind.inserts());
            let deleted = !inserted && new.iter().any(|wrap| matches!(wrap.kind, WrapKind::Del | WrapKind::MoveFrom));
            (new.last().map(|wrap| wrap.author), inserted, deleted)
        });
        // A formatting change of the save inside a field also touches it.
        let formatted = tables.format_of(item.format).and_then(|format| {
            format.slots.iter().filter_map(|slot| slot.rev).find(|rev| new_pairs.contains(&(rev.author, rev.date))).map(|rev| rev.author)
        });
        let kind = tables.mark_info(item.piece).map(|mark| mark.kind);
        if matches!(kind, Some(MarkKind::FieldBegin | MarkKind::SimpleField(_))) {
            if open.len() >= MAX_FIELD_DEPTH {
                return Err("fields nested deeper than an inspection reads".into());
            }
            open.push(OpenField { base: !inserted, touched: false, all_deleted: true });
        }
        // Each change touching base fields counts once for each of them, in its author's field
        // codes.
        let toucher = author.or(formatted);
        let mut base_fields = 0u32;
        for field in open.iter_mut().filter(|field| field.base) {
            field.touched |= toucher.is_some();
            field.all_deleted &= deleted;
            base_fields = base_fields.saturating_add(1);
        }
        if let Some(author) = toucher
            && base_fields > 0
        {
            let entry = holds_of(holds, author, budget)?;
            entry.field_codes = entry.field_codes.saturating_add(base_fields);
        }
        if matches!(kind, Some(MarkKind::FieldEnd | MarkKind::SimpleFieldEnd))
            && let Some(field) = open.pop()
        {
            close(field, &mut changed);
        }
    }
    while let Some(field) = open.pop() {
        close(field, &mut changed);
    }
    Ok(changed)
}

// ---- The comparison ---------------------------------------------------------------------------

/// A base change accepted or rejected, told apart by its kind, author, date and element.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Resolution {
    Wrap(Wrap),
    Format(&'static str, FormatRev),
}

/// What the differences mean, summed over every story. Each entry is charged as it is made, at its
/// room, before it is kept.
#[derive(Default)]
pub(super) struct Tally {
    pub untracked: u32,
    pub altered: u32,
    pub accepted: BTreeSet<Resolution>,
    pub rejected: BTreeSet<Resolution>,
    pub losses: BTreeMap<(String, LossKind, Option<String>), u32>,
    pub comments_altered: BTreeSet<String>,
}

impl Tally {
    /// Keep a resolution found, charged first; false when memory ran out.
    fn resolve(&mut self, accepted: bool, resolution: Resolution, budget: &mut Budget) -> bool {
        let set = if accepted { &mut self.accepted } else { &mut self.rejected };
        if set.contains(&resolution) {
            return true;
        }
        if !budget.try_hold(tree_entry(set.is_empty(), RESOLUTION_BYTES, tree_room::<Resolution, ()>())) {
            return false;
        }
        set.insert(resolution);
        true
    }

    /// Keep a base comment as altered, its id charged before it is copied; false when memory ran out.
    pub(super) fn alter_comment(&mut self, id: &str, budget: &mut Budget) -> bool {
        if self.comments_altered.contains(id) {
            return true;
        }
        let entry = tree_entry(self.comments_altered.is_empty(), ID_BYTES, tree_room::<String, ()>());
        if !budget.try_hold(entry.saturating_add(units(id.len()))) {
            return false;
        }
        self.comments_altered.insert(id.to_string());
        true
    }

    /// Count an element of `part` lost, a new loss charged before it is made; false when memory ran
    /// out.
    fn lose(&mut self, part: &str, element: &str, budget: &mut Budget) -> bool {
        // The key, made to look the loss up (two names, each within a name's limit).
        let key = (part.to_string(), LossKind::ElementMissing, Some(element.to_string()));
        if let Some(count) = self.losses.get_mut(&key) {
            *count = count.saturating_add(1);
            return true;
        }
        let strings = units(part.len().saturating_add(element.len()));
        if !budget.try_hold(tree_entry(self.losses.is_empty(), RECORD_BYTES, tree_room::<LossKey, u32>()).saturating_add(strings)) {
            return false;
        }
        self.losses.insert(key, 1);
        true
    }
}

/// The comments the tally must know about: those of the base, and those the save removed.
pub(super) struct CommentIds<'a> {
    pub base: &'a BTreeSet<String>,
    pub removed: &'a BTreeSet<String>,
}

struct Story<'a> {
    tables: &'a Tables,
    comments: &'a CommentIds<'a>,
    part: &'a str,
    tally: &'a mut Tally,
}

impl<'a> Story<'a> {
    /// The comment a mark anchors, if it is an (empty) comment anchor.
    fn comment(&self, piece: u32) -> Option<&'a str> {
        self.tables.mark_info(piece).and_then(|mark| mark.comment.as_deref())
    }

    /// Whether a reader shows the item in another context than the base did, unexplained.
    fn context_changed(&self, before: u32, after: u32) -> bool {
        if before == after {
            return false;
        }
        let (before, after) = (self.tables.context_of(before), self.tables.context_of(after));
        // A resolved change of the governing mark (accepted keeps its value, rejected restores the
        // old) is counted on that mark; here it explains the context.
        let explained = |now: u32, base: u32, base_old: Option<u32>| now == base || base_old == Some(now);
        before.field != after.field
            || !explained(after.paragraph, before.paragraph, before.paragraph_old)
            || !explained(after.section, before.section, before.section_old)
    }

    /// The same piece in both: its wrappers, formatting and context say what happened to it. False
    /// when the tally ran out of memory.
    fn same(&mut self, before: &Item, after: &Item, budget: &mut Budget) -> bool {
        if before == after {
            return true;
        }
        let comment = self.comment(before.piece).filter(|id| self.comments.base.contains(*id));
        let mut untracked = self.context_changed(before.context, after.context);
        let mut altered = false;
        if before.wraps != after.wraps {
            // Kept with fewer wrappers: each one gone was resolved (an insertion accepted, a
            // deletion rejected). Any other change of wrappers alters a base change.
            let before_wraps = self.tables.wraps_of(before.wraps);
            let after_wraps = self.tables.wraps_of(after.wraps);
            let mut remaining = before_wraps.iter();
            let mut gone: Vec<&Wrap> = Vec::new();
            let mut kept_in_order = true;
            for wrap in &after_wraps {
                loop {
                    match remaining.next() {
                        Some(candidate) if candidate == wrap => break,
                        Some(candidate) => gone.push(candidate),
                        None => {
                            kept_in_order = false;
                            break;
                        }
                    }
                }
                if !kept_in_order {
                    break;
                }
            }
            gone.extend(remaining);
            if kept_in_order {
                for wrap in gone {
                    if !self.tally.resolve(wrap.kind.inserts(), Resolution::Wrap(*wrap), budget) {
                        return false;
                    }
                }
            } else {
                altered = true;
            }
        }
        if before.format != after.format {
            let empty = super::story::Format::default();
            let before_format = self.tables.format_of(before.format).unwrap_or(&empty);
            let after_format = self.tables.format_of(after.format).unwrap_or(&empty);
            let names: BTreeSet<&'static str> = before_format.slots.iter().chain(&after_format.slots).map(|slot| slot.name).collect();
            for name in names {
                let (before_value, before_rev) = before_format.slot(name);
                let (after_value, after_rev) = after_format.slot(name);
                if before_value == after_value && before_rev == after_rev {
                    continue;
                }
                match (before_rev, after_rev) {
                    // The base's formatting change resolved: accepted keeps the new formatting,
                    // rejected restores the old.
                    (Some(rev), None) if after_value == before_value || after_value == rev.old => {
                        if !self.tally.resolve(after_value == before_value, Resolution::Format(name, *rev), budget) {
                            return false;
                        }
                    }
                    _ if before_value != after_value => untracked = true,
                    _ => altered = true,
                }
            }
        }
        match comment {
            Some(id) if untracked || altered => self.tally.alter_comment(id, budget),
            _ => {
                if untracked {
                    self.tally.untracked = self.tally.untracked.saturating_add(1);
                }
                if altered {
                    self.tally.altered = self.tally.altered.saturating_add(1);
                }
                true
            }
        }
    }

    /// Base content the edited story no longer has. False when the tally ran out of memory.
    fn deleted(&mut self, before: &Item, budget: &mut Budget) -> bool {
        if let Some(id) = self.comment(before.piece) {
            // An empty anchor of a removed comment goes with it; any other anchor gone alters its
            // comment.
            return self.comments.removed.contains(id) || self.tally.alter_comment(id, budget);
        }
        match self.tables.wraps_of(before.wraps).last() {
            // Gone with its change: an insertion rejected, a deletion accepted.
            Some(wrap) => self.tally.resolve(!wrap.kind.inserts(), Resolution::Wrap(*wrap), budget),
            None => match self.tables.mark_info(before.piece) {
                Some(mark) => self.tally.lose(self.part, &mark.element, budget),
                None => {
                    self.tally.untracked = self.tally.untracked.saturating_add(1);
                    true
                }
            },
        }
    }

    /// Content the base does not have, after this save's own changes were undone. False when the
    /// tally ran out of memory.
    fn inserted(&mut self, after: &Item, budget: &mut Budget) -> bool {
        if let Some(id) = self.comment(after.piece)
            && self.comments.base.contains(id)
        {
            return self.tally.alter_comment(id, budget);
        }
        if after.wraps == EMPTY {
            self.tally.untracked = self.tally.untracked.saturating_add(1);
        } else {
            self.tally.altered = self.tally.altered.saturating_add(1);
        }
        true
    }
}

/// A word (characters with the same changes, formatting and context, up to a white space), or a
/// mark on its own: `items[start..start + len]`. Words are matched whole, everything included; what
/// lies between matched words is then matched character by character, by the character alone, so
/// a resolved change or a formatting difference is seen as the same character kept.
pub(super) struct Token {
    start: usize,
    len: usize,
    hash: u64,
}

/// A token's memory, with its vector's slack while it grows.
const TOKEN_BYTES: u64 = vector_slot::<Token>();

pub(super) fn tokens(items: &[Item], hasher: &RandomState, budget: &mut Budget) -> Option<Vec<Token>> {
    let mut out = Vec::new();
    let mut start = 0;
    let close = |out: &mut Vec<Token>, budget: &mut Budget, start: usize, end: usize| -> bool {
        if end > start {
            if !budget.try_hold(TOKEN_BYTES) {
                return false;
            }
            let mut state = hasher.build_hasher();
            for item in items.get(start..end).unwrap_or(&[]) {
                state.write_u32(item.piece);
                state.write_u32(item.wraps);
                state.write_u32(item.format);
                state.write_u32(item.context);
            }
            out.push(Token { start, len: end - start, hash: state.finish() });
        }
        true
    };
    for (index, item) in items.iter().enumerate() {
        if !budget.spend(2) {
            return None;
        }
        if index > start
            && items
                .get(index - 1)
                .is_some_and(|previous| (previous.wraps, previous.format, previous.context) != (item.wraps, item.format, item.context))
        {
            if !close(&mut out, budget, start, index) {
                return None;
            }
            start = index;
        }
        if item.piece >= MARK_BASE {
            if !close(&mut out, budget, start, index) || !close(&mut out, budget, index, index + 1) {
                return None;
            }
            start = index + 1;
        } else if char::from_u32(item.piece).is_some_and(char::is_whitespace) {
            if !close(&mut out, budget, start, index + 1) {
                return None;
            }
            start = index + 1;
        }
    }
    if !close(&mut out, budget, start, items.len()) {
        return None;
    }
    Some(out)
}

fn pieces<'a>(items: &'a [Item], token: &Token) -> &'a [Item] {
    items.get(token.start..token.start.saturating_add(token.len)).unwrap_or(&[])
}

/// Compare one story. False when a bound stopped it (`max_changes`, work or memory).
pub(super) fn compare_story(
    part: &str,
    base: &[Item],
    edited: &[Item],
    tables: &Tables,
    comments: &CommentIds<'_>,
    max_changes: usize,
    budget: &mut Budget,
    tally: &mut Tally,
) -> bool {
    let hasher = RandomState::new();
    let base_tokens = tokens(base, &hasher, budget);
    let edited_tokens = tokens(edited, &hasher, budget);
    let held = units(base_tokens.as_ref().map_or(0, Vec::len).saturating_add(edited_tokens.as_ref().map_or(0, Vec::len))).saturating_mul(TOKEN_BYTES);
    let result = match (base_tokens, edited_tokens) {
        (Some(base_tokens), Some(edited_tokens)) => {
            let mut story = Story { tables, comments, part, tally };
            compare_tokens(base, edited, &base_tokens, &edited_tokens, max_changes, budget, &mut story)
        }
        _ => false,
    };
    budget.release(held);
    result
}

fn compare_tokens(
    base: &[Item],
    edited: &[Item],
    base_tokens: &[Token],
    edited_tokens: &[Token],
    max_changes: usize,
    budget: &mut Budget,
    story: &mut Story<'_>,
) -> bool {
    let same_token = |i: usize, j: usize| match (base_tokens.get(i), edited_tokens.get(j)) {
        (Some(a), Some(b)) => a.hash == b.hash && pieces(base, a) == pieces(edited, b),
        _ => false,
    };
    let steps_held = script_bytes(base_tokens.len(), edited_tokens.len(), max_changes);
    if !budget.try_hold(steps_held) {
        return false;
    }
    let Some(steps) = script(base_tokens.len(), edited_tokens.len(), &same_token, max_changes, budget) else {
        budget.release(steps_held);
        return false;
    };
    let done = walk_tokens(base, edited, base_tokens, edited_tokens, steps, max_changes, budget, story);
    budget.release(steps_held);
    done
}

fn walk_tokens(
    base: &[Item],
    edited: &[Item],
    base_tokens: &[Token],
    edited_tokens: &[Token],
    steps: Vec<Step>,
    max_changes: usize,
    budget: &mut Budget,
    story: &mut Story<'_>,
) -> bool {
    // Runs of deleted and inserted tokens between matched ones, compared character by character.
    let mut deleted: Option<(usize, usize)> = None;
    let mut inserted: Option<(usize, usize)> = None;
    let item_range = |tokens: &[Token], range: Option<(usize, usize)>| -> (usize, usize) {
        match range.and_then(|(first, last)| Some((tokens.get(first)?, tokens.get(last)?))) {
            Some((first, last)) => (first.start, last.start.saturating_add(last.len)),
            None => (0, 0),
        }
    };
    for step in steps.into_iter().map(Some).chain(std::iter::once(None)) {
        match step {
            Some(Step::Delete(index)) => deleted = Some(deleted.map_or((index, index), |(first, _)| (first, index))),
            Some(Step::Insert(index)) => inserted = Some(inserted.map_or((index, index), |(first, _)| (first, index))),
            other => {
                let (from, to) = item_range(base_tokens, deleted.take());
                let (edited_from, edited_to) = item_range(edited_tokens, inserted.take());
                if !compare_stretch(base.get(from..to).unwrap_or(&[]), edited.get(edited_from..edited_to).unwrap_or(&[]), max_changes, budget, story)
                {
                    return false;
                }
                // Matched words are equal whole, everything included: nothing to tally.
                if let Some(Step::Same { left, right, len }) = other
                    && (base_tokens.get(left..left.saturating_add(len)).is_none() || edited_tokens.get(right..right.saturating_add(len)).is_none())
                {
                    return false;
                }
            }
        }
    }
    true
}

fn compare_stretch(base: &[Item], edited: &[Item], max_changes: usize, budget: &mut Budget, story: &mut Story<'_>) -> bool {
    if base.is_empty() && edited.is_empty() {
        return true;
    }
    let same = |i: usize, j: usize| matches!((base.get(i), edited.get(j)), (Some(a), Some(b)) if a.piece == b.piece);
    let steps_held = script_bytes(base.len(), edited.len(), max_changes);
    if !budget.try_hold(steps_held) {
        return false;
    }
    let Some(steps) = script(base.len(), edited.len(), &same, max_changes, budget) else {
        budget.release(steps_held);
        return false;
    };
    let done = tally_steps(base, edited, steps, story, budget);
    budget.release(steps_held);
    done
}

fn tally_steps(base: &[Item], edited: &[Item], steps: Vec<Step>, story: &mut Story<'_>, budget: &mut Budget) -> bool {
    for step in steps {
        match step {
            Step::Same { left, right, len } => {
                let (Some(before), Some(after)) = (base.get(left..left.saturating_add(len)), edited.get(right..right.saturating_add(len))) else {
                    return false;
                };
                for (old, new) in before.iter().zip(after) {
                    if !story.same(old, new, budget) {
                        return false;
                    }
                }
            }
            Step::Delete(left) => {
                let Some(before) = base.get(left) else { return false };
                if !story.deleted(before, budget) {
                    return false;
                }
            }
            Step::Insert(right) => {
                let Some(after) = edited.get(right) else { return false };
                if !story.inserted(after, budget) {
                    return false;
                }
            }
        }
    }
    true
}

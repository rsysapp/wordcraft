//! embed: what a save changes in one paragraph of its source, as steps over its characters.
//!
//! The source paragraph is the reader's model of it (read again from the source package, `before`);
//! the edited paragraphs are the edited document's (`after`) on the same origin, in order: one, or
//! several after Enter or a paste of several paragraphs. With this session's insertions and new
//! paragraph marks taken out, what is left must be the source paragraph, character for character,
//! except where a tracked change of the source was decided.
//!
//! A diff (the inspector's bounded Myers) finds that. It pairs a source character with an edited one
//! when they are the same character, formatting and object and the edited one's tracked changes are
//! the source's, decided (an insertion accepted, a deletion rejected) or followed by this session's
//! deletion; a decision is no difference, so a long accepted insertion costs no more than an
//! unchanged paragraph. A source character left out went with its decided change (an insertion
//! rejected, a deletion accepted). Where several pairings are as short, the diff's suffix-first
//! rule chooses (an accepted insertion beside a rejected deletion of the same letters); the text and
//! tracked changes written are the same either way. Where its choice leaves out a character that
//! cannot go (a plain one beside an equal decided one), the pairs around it are paired again
//! (`realign`). On each stretch it could not pair on either side, a second diff on characters alone
//! tells an altered tracked change from text changed without one; both are refused.
//!
//! This session's revisions are the records not read from the file (`Revision::read`, a watermark
//! on each record, 6C-45); a record read from the file is the file's by what it records (kind,
//! author, date), wherever it stands, and never the session's even in its name and date.
//!
//! Everything the planner holds is charged to the save's budget before it is allocated; what it
//! gives back (the steps) stays charged, [`held_by`] says how much.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use wordcraft_doc::para::OBJ;
use wordcraft_doc::{CharProps, Document, InlineObject, Paragraph, RevisionKind};

use super::Unpreservable;
use crate::inspect::MAX_CHANGES;
use crate::inspect::budget::{Budget, table_room, table_sized, table_slot, units, vector_room, vector_slot};
use crate::inspect::diff::{self, script, script_bytes};

/// A tracked change's identity, as the inspector knows it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Revision {
    pub author: String,
    pub date: String,
}

/// What happens to a source character that stays.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Change {
    Same,
    /// Its insertion was accepted: the `w:ins` around it goes.
    Accepted,
    /// Its deletion was rejected: the `w:del` around it goes, `w:delText` becomes `w:t`.
    Rejected,
    /// Deleted in this session.
    Deleted(Revision),
    /// Another author's insertion accepted, then deleted in this session (6C-43): the `w:ins` around
    /// it goes and the session's `w:del` wraps it.
    AcceptedThenDeleted(Revision),
    /// Another author's deletion rejected, then deleted again in this session (6C-44): the deletion
    /// is the session's, so the source's `w:del` gives way to the session's.
    RejectedThenDeleted(Revision),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    /// Source character `unit` stays.
    Keep { unit: usize, change: Change },
    /// Source character `unit` goes with its decided change (a rejected insertion, an accepted deletion).
    Remove { unit: usize },
    /// Text typed before the next kept or removed character; `anchor`: the source character whose
    /// formatting the typing continued (`None`: the paragraph mark's).
    Insert { text: String, revision: Revision, anchor: Option<usize> },
    /// A new paragraph mark: the paragraph ends here, and the next one begins.
    Break { revision: Revision },
}

/// The steps that make the source paragraph `original` into `edited`: the edited paragraphs on its
/// origin, in order. `before` is the source document read again (its revision records name the
/// source's changes), `after` the edited one. `budget` is the save's (one for every paragraph): what
/// the plan works with is charged to it and given back, what it returns stays charged
/// ([`held_by`]); a refusal gives back everything.
pub(crate) fn plan(
    original: &Paragraph,
    before: &Document,
    edited: &[&Paragraph],
    after: &Document,
    budget: &mut Budget,
) -> Result<Vec<Step>, Unpreservable> {
    let mut planner = Planner::new(before, after, budget);
    let result = planner.plan(original, edited);
    let Planner { formats, objects, mut ledger, .. } = planner;
    drop((formats, objects));
    match result {
        Ok(steps) => {
            // What the planner worked with is gone; the steps stay, charged as they are.
            let kept = held_by(&steps);
            if ledger.held >= kept {
                ledger.release(ledger.held.saturating_sub(kept));
            } else if let Err(refused) = ledger.hold(kept.saturating_sub(ledger.held)) {
                ledger.release(ledger.held);
                return Err(refused);
            }
            Ok(steps)
        }
        Err(refused) => {
            ledger.release(ledger.held);
            Err(refused)
        }
    }
}

/// Memory the steps hold, as [`plan`] leaves it charged to the save's budget.
pub(crate) fn held_by(steps: &[Step]) -> u64 {
    let revision = |revision: &Revision| units(revision.author.capacity()).saturating_add(units(revision.date.capacity()));
    let strings = steps.iter().fold(0u64, |total, step| {
        total.saturating_add(match step {
            Step::Keep { change: Change::Deleted(key), .. } | Step::Break { revision: key } => revision(key),
            Step::Insert { text, revision: key, .. } => units(text.capacity()).saturating_add(revision(key)),
            Step::Keep { .. } | Step::Remove { .. } => 0,
        })
    });
    units(steps.len()).saturating_mul(size::<Step>()).saturating_add(strings)
}

const fn size<T>() -> u64 {
    std::mem::size_of::<T>() as u64
}

/// A tracked change on a character: a source record by its index in `before` (the first one that
/// records the same change), or this session's record `i` of `after` as `before`'s count + `i`.
type Record = Option<u32>;

/// Bytes of a string or a formatting compared or hashed per unit of work.
const BYTES_PER_UNIT: u64 = 16;

/// An object of an edited paragraph equal to none of the source's.
const NEW_OBJECT: u32 = u32::MAX;
/// A source U+FFFC without its object (a model that does not hold one): equal to nothing.
const MISSING_OBJECT: u32 = u32::MAX - 1;

/// One character as the diffs compare it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Char {
    ch: char,
    /// Its formatting without tracked changes: an index into the plan's formats.
    format: u32,
    /// For U+FFFC, its object: the index of the first equal object of the source paragraph.
    object: Option<u32>,
    ins: Record,
    del: Record,
}

#[derive(Clone, Copy, Debug)]
enum Item {
    Char(Char),
    /// A new paragraph mark, inserted by this record.
    Mark(u32),
}

/// What happens to a source character an edited one stands for (as [`Change`], by record).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decided {
    Same,
    Accepted,
    Rejected,
    Deleted(u32),
    AcceptedThenDeleted(u32),
    RejectedThenDeleted(u32),
}

/// Which records are this session's: those numbered past the source's records.
#[derive(Clone, Copy)]
struct Era {
    source: usize,
}

impl Era {
    fn is_new(self, record: Record) -> bool {
        record.is_some_and(|index| usize::try_from(index).is_ok_and(|index| index >= self.source))
    }
}

/// What the planner holds of the save's budget.
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
        self.hold(units(count).saturating_mul(size::<T>()))?;
        Ok(Vec::with_capacity(count))
    }
}

/// Formats without tracked changes, each kept once, so the diffs compare formatting by index.
#[derive(Default)]
struct Formats {
    distinct: Vec<CharProps>,
    /// A format's hash to the first format with it; `next` chains the others with the same hash.
    first: HashMap<u64, u32>,
    next: Vec<u32>,
}

/// Objects of the source paragraph, each kept once by reference.
#[derive(Default)]
struct Objects<'a> {
    distinct: Vec<&'a InlineObject>,
    first: HashMap<u64, u32>,
    next: Vec<u32>,
}

const END_OF_CHAIN: u32 = u32::MAX;

/// The bytes a formatting holds besides itself.
fn props_heap(props: &CharProps) -> u64 {
    [&props.style, &props.font, &props.lang, &props.link].into_iter().flatten().fold(0u64, |total, text| total.saturating_add(units(text.capacity())))
}

/// A number's bits for a hash, 0.0 and -0.0 alike (they are equal formats).
fn bits(number: Option<f32>) -> Option<u32> {
    number.map(|number| if number == 0.0 { 0 } else { number.to_bits() })
}

/// A hash of the formatting (equal formats hash alike, so formats differing in a size or a spacing
/// do not meet in one chain).
fn format_hash(props: &CharProps) -> u64 {
    let mut hasher = DefaultHasher::new();
    (bits(props.size), bits(props.spacing), bits(props.scale), bits(props.position), bits(props.kern)).hash(&mut hasher);
    (&props.style, &props.font, props.bold, props.italic, props.underline, props.underline_color, props.strike, props.double_strike)
        .hash(&mut hasher);
    (props.color, props.highlight, props.shading, props.vert_align, props.caps, props.small_caps, props.hidden).hash(&mut hasher);
    (props.outline, props.shadow, props.emboss, props.engrave, &props.lang, props.no_proof, props.rtl, &props.link).hash(&mut hasher);
    hasher.finish()
}

/// The text an object holds, for what comparing it costs.
fn object_weight(object: &InlineObject) -> usize {
    match object {
        InlineObject::Image { media, alt, .. } => media.len().saturating_add(alt.len()),
        InlineObject::Shape { .. } | InlineObject::CommentStart { .. } | InlineObject::CommentEnd { .. } => 0,
        InlineObject::Field { instr, result, .. } => instr.len().saturating_add(result.len()),
        InlineObject::NoteRef { custom, .. } => custom.len(),
        InlineObject::BookmarkStart { name } | InlineObject::BookmarkEnd { name } => name.len(),
        InlineObject::Equation { linear, .. } => linear.len(),
        InlineObject::Opaque { format, xml, text } => format.len().saturating_add(xml.len()).saturating_add(text.len()),
    }
}

/// A hash of the object's kind and words (equal objects hash alike; sizes are compared when hashes meet).
fn object_hash(object: &InlineObject) -> u64 {
    let mut hasher = DefaultHasher::new();
    std::mem::discriminant(object).hash(&mut hasher);
    match object {
        InlineObject::Image { media, alt, .. } => (media, alt).hash(&mut hasher),
        InlineObject::Shape { story, .. } => story.hash(&mut hasher),
        InlineObject::Field { instr, result, locked } => (instr, result, locked).hash(&mut hasher),
        InlineObject::NoteRef { kind, id, custom } => (kind, id, custom).hash(&mut hasher),
        InlineObject::BookmarkStart { name } | InlineObject::BookmarkEnd { name } => name.hash(&mut hasher),
        InlineObject::CommentStart { id } | InlineObject::CommentEnd { id } => id.hash(&mut hasher),
        InlineObject::Equation { linear, display } => (linear, display).hash(&mut hasher),
        InlineObject::Opaque { format, xml, text } => (format, xml, text).hash(&mut hasher),
    }
    hasher.finish()
}

/// Which paragraph characters are read from: the source's records are `before`'s, the edited ones
/// `after`'s; objects equal to the source's take its index, others are new.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Source,
    Edited,
}

/// The file's records: each one's first index in `before` by what it records.
struct Sources<'a> {
    by_record: HashMap<(RevisionKind, &'a str, &'a str), u32>,
    /// For each record of `before`, the first index recording the same change.
    first: Vec<u32>,
}

/// Text typed in this session, waiting for its place among the decided removals around it.
struct Typed {
    text: String,
    record: u32,
    anchor: Option<usize>,
}

struct Planner<'a, 'b> {
    before: &'a Document,
    after: &'a Document,
    /// The file's records, read when a record is first named.
    sources: Option<Sources<'a>>,
    formats: Formats,
    objects: Objects<'a>,
    ledger: Ledger<'b>,
}

impl<'a, 'b> Planner<'a, 'b> {
    fn new(before: &'a Document, after: &'a Document, budget: &'b mut Budget) -> Planner<'a, 'b> {
        Planner { before, after, sources: None, formats: Formats::default(), objects: Objects::default(), ledger: Ledger { budget, held: 0 } }
    }

    /// An edit script between `n` and `m` characters, its steps' room held (given back by the
    /// caller with the amount answered once it is done with them).
    fn diff(&mut self, n: usize, m: usize, same: &dyn Fn(usize, usize) -> bool) -> Result<(Vec<diff::Step>, u64), Unpreservable> {
        let held = script_bytes(n, m, MAX_CHANGES);
        self.ledger.hold(held)?;
        match script(n, m, same, MAX_CHANGES, self.ledger.budget) {
            Some(steps) => Ok((steps, held)),
            None => {
                self.ledger.release(held);
                Err(Unpreservable::TooManyChanges)
            }
        }
    }

    fn era(&self) -> Era {
        Era { source: self.before.revisions.len() }
    }

    /// The file's records by what they record, made once per plan.
    fn sources(&mut self) -> Result<&Sources<'a>, Unpreservable> {
        if self.sources.is_none() {
            let records = &self.before.revisions;
            self.ledger.hold(
                table_sized::<((RevisionKind, &str, &str), u32)>(records.len()).saturating_add(units(records.len()).saturating_mul(size::<u32>())),
            )?;
            let mut by_record = HashMap::with_capacity(records.len());
            let mut first = Vec::with_capacity(records.len());
            for (index, record) in records.iter().enumerate() {
                self.ledger.work(1u64.saturating_add(units(record.author.len().saturating_add(record.date.len())) / BYTES_PER_UNIT))?;
                let index = u32::try_from(index).map_err(|_| Unpreservable::TooManyChanges)?;
                first.push(*by_record.entry((record.kind, record.author.as_str(), record.date.as_str())).or_insert(index));
            }
            self.sources = Some(Sources { by_record, first });
        }
        self.sources.as_ref().ok_or(Unpreservable::Altered)
    }

    /// A source character's record: one of `before`'s.
    fn source_record(&mut self, record: Record) -> Result<Record, Unpreservable> {
        let Some(index) = record else { return Ok(None) };
        let first = usize::try_from(index).ok().and_then(|index| self.sources().ok()?.first.get(index).copied());
        first.map(Some).ok_or(Unpreservable::Altered)
    }

    /// An edited character's record: a record read from the file is the file's record that records
    /// the same change (refused when the file holds none); any other is this session's.
    fn edited_record(&mut self, record: Record) -> Result<Record, Unpreservable> {
        let Some(index) = record else { return Ok(None) };
        let after: &'a Document = self.after;
        let found = usize::try_from(index).ok().and_then(|index| after.revisions.get(index)).ok_or(Unpreservable::Altered)?;
        if found.read {
            self.ledger.work(1u64.saturating_add(units(found.author.len().saturating_add(found.date.len())) / BYTES_PER_UNIT))?;
            let key = (found.kind, found.author.as_str(), found.date.as_str());
            return self.sources()?.by_record.get(&key).copied().map(Some).ok_or(Unpreservable::Altered);
        }
        let source = u32::try_from(self.before.revisions.len()).map_err(|_| Unpreservable::TooManyChanges)?;
        source.checked_add(index).map(Some).ok_or(Unpreservable::TooManyChanges)
    }

    /// The revision this session's record names, its strings charged before they are copied.
    fn revision(&mut self, record: u32) -> Result<Revision, Unpreservable> {
        let index = usize::try_from(record).ok().and_then(|record| record.checked_sub(self.before.revisions.len()));
        let found = index.and_then(|index| self.after.revisions.get(index)).ok_or(Unpreservable::Altered)?;
        self.ledger.hold(units(found.author.len()).saturating_add(units(found.date.len())))?;
        Ok(Revision { author: found.author.clone(), date: found.date.clone() })
    }

    /// The index of `props`' formatting without its tracked changes, kept once.
    fn format(&mut self, props: &CharProps) -> Result<u32, Unpreservable> {
        let transient = size::<CharProps>().saturating_add(props_heap(props));
        self.ledger.work(1u64.saturating_add(props_heap(props) / BYTES_PER_UNIT))?;
        self.ledger.hold(transient)?;
        let mut plain = props.clone();
        plain.ins = None;
        plain.del = None;
        let hash = format_hash(&plain);
        let mut at = self.formats.first.get(&hash).copied().unwrap_or(END_OF_CHAIN);
        let mut last = None;
        while let Some(found) = usize::try_from(at).ok().and_then(|index| self.formats.distinct.get(index)) {
            self.ledger.work(1u64.saturating_add(props_heap(found) / BYTES_PER_UNIT))?;
            if *found == plain {
                self.ledger.release(transient);
                return Ok(at);
            }
            last = Some(at);
            at = usize::try_from(at).ok().and_then(|index| self.formats.next.get(index)).copied().unwrap_or(END_OF_CHAIN);
        }
        // A new format: its room in the lists and the table (the clone is already charged).
        let index = u32::try_from(self.formats.distinct.len()).ok().filter(|index| *index < MISSING_OBJECT).ok_or(Unpreservable::TooManyChanges)?;
        if index == 0 {
            self.ledger.hold(vector_room::<CharProps>().saturating_add(vector_room::<u32>()).saturating_add(table_room::<(u64, u32)>()))?;
        }
        self.ledger.hold(vector_slot::<CharProps>().saturating_add(vector_slot::<u32>()).saturating_add(table_slot::<(u64, u32)>()))?;
        self.formats.distinct.push(plain);
        self.formats.next.push(END_OF_CHAIN);
        match last.and_then(|last| usize::try_from(last).ok()).and_then(|last| self.formats.next.get_mut(last)) {
            Some(link) => *link = index,
            None => {
                self.formats.first.insert(hash, index);
            }
        }
        Ok(index)
    }

    /// The index of the first source object equal to `object`; for the source, kept if new.
    fn object(&mut self, object: Option<&'a InlineObject>, side: Side) -> Result<u32, Unpreservable> {
        let Some(object) = object else {
            return Ok(if side == Side::Source { MISSING_OBJECT } else { NEW_OBJECT });
        };
        let weight = units(object_weight(object)) / BYTES_PER_UNIT;
        self.ledger.work(1u64.saturating_add(weight))?;
        let hash = object_hash(object);
        let mut at = self.objects.first.get(&hash).copied().unwrap_or(END_OF_CHAIN);
        let mut last = None;
        while let Some(found) = usize::try_from(at).ok().and_then(|index| self.objects.distinct.get(index)) {
            self.ledger.work(1u64.saturating_add(weight))?;
            if *found == object {
                return Ok(at);
            }
            last = Some(at);
            at = usize::try_from(at).ok().and_then(|index| self.objects.next.get(index)).copied().unwrap_or(END_OF_CHAIN);
        }
        if side == Side::Edited {
            return Ok(NEW_OBJECT);
        }
        let index = u32::try_from(self.objects.distinct.len()).ok().filter(|index| *index < MISSING_OBJECT).ok_or(Unpreservable::TooManyChanges)?;
        if index == 0 {
            self.ledger.hold(vector_room::<&InlineObject>().saturating_add(vector_room::<u32>()).saturating_add(table_room::<(u64, u32)>()))?;
        }
        self.ledger.hold(vector_slot::<&InlineObject>().saturating_add(vector_slot::<u32>()).saturating_add(table_slot::<(u64, u32)>()))?;
        self.objects.distinct.push(object);
        self.objects.next.push(END_OF_CHAIN);
        match last.and_then(|last| usize::try_from(last).ok()).and_then(|last| self.objects.next.get_mut(last)) {
            Some(link) => *link = index,
            None => {
                self.objects.first.insert(hash, index);
            }
        }
        Ok(index)
    }

    /// Every character of `paragraph`, in order, to `push`. The runs must cover its text exactly.
    fn chars(&mut self, paragraph: &'a Paragraph, side: Side, push: &mut dyn FnMut(Char)) -> Result<(), Unpreservable> {
        let mut objects = paragraph.objects.iter();
        let mut covered = 0usize;
        for (range, props) in paragraph.run_ranges() {
            covered = range.end;
            let text = paragraph.text.get(range).ok_or(Unpreservable::Structure)?;
            self.ledger.work(units(text.len()))?;
            let format = self.format(props)?;
            let (ins, del) = match side {
                Side::Source => (self.source_record(props.ins)?, self.source_record(props.del)?),
                Side::Edited => (self.edited_record(props.ins)?, self.edited_record(props.del)?),
            };
            for ch in text.chars() {
                let object = if ch == OBJ { Some(self.object(objects.next(), side)?) } else { None };
                push(Char { ch, format, object, ins, del });
            }
        }
        if covered != paragraph.text.len() {
            return Err(Unpreservable::Structure);
        }
        Ok(())
    }

    fn plan(&mut self, original: &'a Paragraph, edited: &[&'a Paragraph]) -> Result<Vec<Step>, Unpreservable> {
        let era = self.era();
        // Paragraph properties are the source's: an edit session changes none (Enter keeps them).
        let Some((last, heads)) = edited.split_last() else { return Err(Unpreservable::Structure) };
        if edited.iter().any(|paragraph| paragraph.props != original.props) {
            return Err(Unpreservable::Formatting);
        }
        if last.section != original.section || heads.iter().any(|paragraph| paragraph.section.is_some()) {
            return Err(Unpreservable::Formatting);
        }
        // Every half ends with the source's mark properties in the file, so each mark in the model
        // must be formatted as the source's (I4).
        let mark_format = self.format(&original.mark)?;
        for paragraph in edited {
            if self.format(&paragraph.mark)? != mark_format {
                return Err(Unpreservable::Formatting);
            }
        }
        // The source's own mark ends the last paragraph, as it was; every earlier one ends with a mark
        // inserted now.
        let (last_ins, last_del) = (self.edited_record(last.mark.ins)?, self.edited_record(last.mark.del)?);
        if (last_ins, last_del) != (self.source_record(original.mark.ins)?, self.source_record(original.mark.del)?) {
            return Err(Unpreservable::Structure);
        }
        let mut marks = self.ledger.vector::<u32>(heads.len())?;
        for head in heads {
            match (self.edited_record(head.mark.ins)?, self.edited_record(head.mark.del)?) {
                (Some(record), None) if era.is_new(Some(record)) => marks.push(record),
                _ => return Err(Unpreservable::Structure),
            }
        }

        let count = |paragraph: &Paragraph| paragraph.text.chars().count();
        self.ledger.work(units(original.text.len()))?;
        let mut old = self.ledger.vector::<Char>(count(original))?;
        self.chars(original, Side::Source, &mut |c| old.push(c))?;
        let mut total = heads.len();
        for paragraph in edited {
            self.ledger.work(units(paragraph.text.len()))?;
            total = total.saturating_add(count(paragraph));
        }
        let mut items = self.ledger.vector::<Item>(total)?;
        let mut mark_at = self.ledger.vector::<usize>(heads.len())?;
        for (index, paragraph) in edited.iter().enumerate() {
            self.chars(paragraph, Side::Edited, &mut |c| items.push(Item::Char(c)))?;
            if let Some(mark) = marks.get(index) {
                mark_at.push(items.len());
                items.push(Item::Mark(*mark));
            }
        }
        // This session's insertions and marks taken out: what is left must be the source.
        let is_kept = |item: &Item| matches!(item, Item::Char(c) if !era.is_new(c.ins));
        let mut projected = self.ledger.vector::<usize>(items.iter().filter(|item| is_kept(item)).count())?;
        projected.extend(items.iter().enumerate().filter(|(_, item)| is_kept(item)).map(|(index, _)| index));
        let at = |items: &[Item], projected: &[usize], j: usize| match projected.get(j).and_then(|index| items.get(*index)) {
            Some(Item::Char(c)) => Some(*c),
            _ => None,
        };

        let mut matched = self.ledger.vector::<Option<(usize, Decided)>>(projected.len())?;
        matched.resize(projected.len(), None);
        let mut removed = self.ledger.vector::<bool>(old.len())?;
        removed.resize(old.len(), false);
        let mut left = self.ledger.vector::<usize>(old.len())?;
        let mut right = self.ledger.vector::<usize>(projected.len())?;

        // A decision, or this session's deletion after one, is no difference (I2).
        let same = |i: usize, j: usize| match (old.get(i), at(&items, &projected, j)) {
            (Some(o), Some(n)) => o.ch == n.ch && o.format == n.format && o.object == n.object && transition(o, &n, era).is_ok(),
            _ => false,
        };
        let (steps, held) = self.diff(old.len(), projected.len(), &same)?;
        // Stretches of source characters only, gone: checked once every pair is known (`slide`).
        let mut gone: Vec<(usize, usize)> = Vec::new();
        self.ledger.hold(vector_room::<(usize, usize)>())?;
        for step in steps.into_iter().map(Some).chain(std::iter::once(None)) {
            match step {
                Some(diff::Step::Delete(i)) => left.push(i),
                Some(diff::Step::Insert(j)) => right.push(j),
                other => {
                    match (left.first(), left.last(), right.is_empty()) {
                        (Some(first), Some(last), true) => {
                            self.ledger.hold(vector_slot::<(usize, usize)>())?;
                            gone.push((*first, last.saturating_add(1)));
                        }
                        _ => self.stretch(&old, &items, &projected, &left, &right, &mut matched, &mut removed)?,
                    }
                    left.clear();
                    right.clear();
                    if let Some(diff::Step::Same { left: from, right: to, len }) = other {
                        self.ledger.work(units(len))?;
                        for offset in 0..len {
                            let (i, j) = (from.saturating_add(offset), to.saturating_add(offset));
                            let (Some(o), Some(n)) = (old.get(i), at(&items, &projected, j)) else { return Err(Unpreservable::Untracked) };
                            let decided = transition(o, &n, era)?;
                            if let Some(slot) = matched.get_mut(j) {
                                *slot = Some((i, decided));
                            }
                        }
                    }
                }
            }
        }
        self.ledger.release(held);
        drop((left, right));
        let mut pair = self.ledger.vector::<Option<usize>>(old.len())?;
        pair.resize(old.len(), None);
        for (j, slot) in matched.iter().enumerate() {
            if let Some((i, _)) = slot
                && let Some(entry) = pair.get_mut(*i)
            {
                *entry = Some(j);
            }
        }
        let edited = |j: usize| at(&items, &projected, j);
        for (start, end) in gone {
            self.realign(&old, &edited, &mut pair, &mut matched, &mut removed, start, end)?;
        }
        drop(pair);
        self.steps(&old, &items, &projected, &mark_at, &matched, &removed, mark_format)
    }

    /// Source characters `start..end` the diff left out, which must have gone with their decided
    /// changes. Where one of them cannot (it had no tracked change), the diff may have paired a
    /// neighbour instead where both pairings were as short: an accepted space beside a plain one, a
    /// deleted "a" rejected beside a plain "a" deleted now. Around the stretch, a window of the pairs
    /// beside it is paired again so that only characters with a tracked change are left out (a
    /// search in a band as wide as the stretch); the window grows until that is possible, or holds
    /// every pair it can, when it is refused.
    #[allow(clippy::too_many_arguments)]
    fn realign(
        &mut self,
        old: &[Char],
        edited: &dyn Fn(usize) -> Option<Char>,
        pair: &mut [Option<usize>],
        matched: &mut [Option<(usize, Decided)>],
        removed: &mut [bool],
        start: usize,
        end: usize,
    ) -> Result<(), Unpreservable> {
        let era = self.era();
        let can_go = |i: usize| old.get(i).is_some_and(|o| o.ins.is_some() || o.del.is_some());
        let relates = |i: usize, j: usize| match (old.get(i), edited(j)) {
            (Some(o), Some(n)) => o.ch == n.ch && o.format == n.format && o.object == n.object && transition(o, &n, era).is_ok(),
            _ => false,
        };
        let partner = |pair: &[Option<usize>], i: usize| pair.get(i).copied().flatten();
        self.ledger.work(units(end.saturating_sub(start)))?;
        let (lo, hi) = if (start..end).all(can_go) {
            (start, end)
        } else {
            // The edited characters around the stretch are next to each other (it holds no edited one).
            let middle = match start.checked_sub(1).and_then(|before| partner(pair, before)) {
                Some(j) => j.saturating_add(1),
                None => partner(pair, end).unwrap_or(0),
            };
            let gone = end - start;
            let mut reach = 1usize;
            loop {
                let (mut lo, mut jlo) = (start, middle);
                while start - lo < reach && lo > 0 && jlo > 0 && partner(pair, lo - 1) == Some(jlo - 1) {
                    (lo, jlo) = (lo - 1, jlo - 1);
                }
                let (mut hi, mut jhi) = (end, middle);
                while hi - end < reach && partner(pair, hi) == Some(jhi) {
                    (hi, jhi) = (hi + 1, jhi + 1);
                }
                if let Some(paired) = self.band(lo, hi, jlo, gone, &can_go, &relates)? {
                    for (offset, partner) in paired.into_iter().enumerate() {
                        if let Some(entry) = pair.get_mut(lo + offset) {
                            *entry = partner;
                        }
                    }
                    break (lo, hi);
                }
                // Grown as far as the pairs beside it go: refused.
                if start - lo < reach && hi - end < reach {
                    return Err(Unpreservable::Untracked);
                }
                reach = reach.saturating_mul(2);
            }
        };
        for i in lo..hi {
            match partner(pair, i) {
                Some(j) => {
                    let (Some(o), Some(n)) = (old.get(i), edited(j)) else { return Err(Unpreservable::Untracked) };
                    if let Some(slot) = matched.get_mut(j) {
                        *slot = Some((i, transition(o, &n, era)?));
                    }
                }
                None if can_go(i) => {
                    if let Some(slot) = removed.get_mut(i) {
                        *slot = true;
                    }
                }
                None => return Err(Unpreservable::Untracked),
            }
        }
        Ok(())
    }

    /// Source characters `lo..hi` paired in order with the edited ones from `jlo`, all but `gone` of
    /// them, leaving out only those that `can_go`: each one's partner, or `None` when there is no
    /// such pairing. A table of whether the rest can still be paired, by place and by how many were
    /// left out so far (never more than `gone`), filled from the end.
    fn band(
        &mut self,
        lo: usize,
        hi: usize,
        jlo: usize,
        gone: usize,
        can_go: &dyn Fn(usize) -> bool,
        relates: &dyn Fn(usize, usize) -> bool,
    ) -> Result<Option<Vec<Option<usize>>>, Unpreservable> {
        let width = gone.saturating_add(1);
        let length = hi.saturating_sub(lo);
        let cells = length.saturating_add(1).saturating_mul(width);
        self.ledger.work(units(cells).saturating_mul(2))?;
        let mut table = self.ledger.vector::<bool>(cells)?;
        table.resize(cells, false);
        let cell = |i: usize, skipped: usize| i.saturating_mul(width).saturating_add(skipped);
        // Edited character `i - skipped` of the window, while there is one.
        let pairs = |i: usize, skipped: usize| i - skipped < length - gone && relates(lo + i, jlo + i - skipped);
        if let Some(done) = table.get_mut(cell(length, gone)) {
            *done = true;
        }
        for i in (0..length).rev() {
            for skipped in 0..width.min(i.saturating_add(1)) {
                let leave = skipped < gone && can_go(lo + i) && table.get(cell(i + 1, skipped + 1)).copied().unwrap_or(false);
                let take = pairs(i, skipped) && table.get(cell(i + 1, skipped)).copied().unwrap_or(false);
                if let Some(slot) = table.get_mut(cell(i, skipped)) {
                    *slot = leave || take;
                }
            }
        }
        if !table.first().copied().unwrap_or(false) {
            self.ledger.release(units(cells));
            return Ok(None);
        }
        let mut paired = self.ledger.vector::<Option<usize>>(length)?;
        let mut skipped = 0usize;
        for i in 0..length {
            if pairs(i, skipped) && table.get(cell(i + 1, skipped)).copied().unwrap_or(false) {
                paired.push(Some(jlo + i - skipped));
            } else {
                paired.push(None);
                skipped += 1;
            }
        }
        Ok(Some(paired))
    }

    /// A stretch the diff could not pair: a source character there gone must have gone with its
    /// tracked change; the same character on both sides is a tracked change altered, anything else
    /// a change without one.
    fn stretch(
        &mut self,
        old: &[Char],
        items: &[Item],
        projected: &[usize],
        left: &[usize],
        right: &[usize],
        matched: &mut [Option<(usize, Decided)>],
        removed: &mut [bool],
    ) -> Result<(), Unpreservable> {
        if left.is_empty() && right.is_empty() {
            return Ok(());
        }
        let era = self.era();
        let source = |a: usize| left.get(a).and_then(|i| old.get(*i).map(|o| (*i, *o)));
        let edited = |b: usize| match right.get(b).and_then(|j| projected.get(*j).map(|index| (*j, items.get(*index)))) {
            Some((j, Some(Item::Char(c)))) => Some((j, *c)),
            _ => None,
        };
        let same = |a: usize, b: usize| match (source(a), edited(b)) {
            (Some((_, o)), Some((_, n))) => o.ch == n.ch && o.format == n.format && o.object == n.object,
            _ => false,
        };
        let (steps, held) = self.diff(left.len(), right.len(), &same)?;
        for step in steps {
            match step {
                diff::Step::Same { left: from, right: to, len } => {
                    self.ledger.work(units(len))?;
                    for offset in 0..len {
                        let (Some((i, o)), Some((j, n))) = (source(from.saturating_add(offset)), edited(to.saturating_add(offset))) else {
                            return Err(Unpreservable::Untracked);
                        };
                        let decided = transition(&o, &n, era)?;
                        if let Some(slot) = matched.get_mut(j) {
                            *slot = Some((i, decided));
                        }
                    }
                }
                diff::Step::Delete(a) => {
                    let Some((i, o)) = source(a) else { return Err(Unpreservable::Untracked) };
                    if o.ins.is_none() && o.del.is_none() {
                        return Err(Unpreservable::Untracked);
                    }
                    if let Some(slot) = removed.get_mut(i) {
                        *slot = true;
                    }
                }
                diff::Step::Insert(_) => return Err(Unpreservable::Untracked),
            }
        }
        self.ledger.release(held);
        Ok(())
    }

    /// The steps, in the order of the edited paragraphs: around each kept character, the decided
    /// removals before it and what was typed there, the typing beside the character it continued.
    fn steps(
        &mut self,
        old: &[Char],
        items: &[Item],
        projected: &[usize],
        mark_at: &[usize],
        matched: &[Option<(usize, Decided)>],
        removed: &[bool],
        mark_format: u32,
    ) -> Result<Vec<Step>, Unpreservable> {
        let era = self.era();
        let mut steps: Vec<Step> = Vec::new();
        self.ledger.hold(vector_room::<Step>().saturating_add(vector_room::<Typed>()))?;
        let mut pending: Vec<Typed> = Vec::new();
        let mut at = Place { next_old: 0, last_kept: None, kept_in_paragraph: false, seen: 0, marks: 0 };
        // The anchor last chosen, for the formatting and place it was chosen for.
        let mut chosen: Option<((u32, usize, usize, usize), Option<usize>)> = None;
        for item in items {
            self.ledger.work(1)?;
            match item {
                Item::Mark(record) => {
                    let gap_end = next_kept(matched, at.seen).unwrap_or(old.len());
                    self.settle(&mut steps, &mut pending, removed, &mut at.next_old, gap_end, false)?;
                    let revision = self.revision(*record)?;
                    self.push(&mut steps, Step::Break { revision })?;
                    at.kept_in_paragraph = false;
                    at.marks = at.marks.saturating_add(1);
                }
                Item::Char(c) if era.is_new(c.ins) => {
                    if c.ch == OBJ {
                        return Err(Unpreservable::InsertedObject);
                    }
                    let (Some(record), None) = (c.ins, c.del) else { return Err(Unpreservable::Altered) };
                    let key = (c.format, at.next_old, at.seen, at.marks);
                    let anchor = match chosen {
                        Some((chosen_key, anchor)) if chosen_key == key => anchor,
                        _ => {
                            let anchor = self.anchor(c.format, &at, old, projected, mark_at, matched, removed, mark_format)?;
                            chosen = Some((key, anchor));
                            anchor
                        }
                    };
                    let bytes = vector_slot::<u8>().saturating_mul(units(c.ch.len_utf8()));
                    match pending.last_mut() {
                        Some(group) if group.record == record && group.anchor == anchor => {
                            self.ledger.hold(bytes)?;
                            group.text.push(c.ch);
                        }
                        _ => {
                            self.ledger.hold(vector_slot::<Typed>().saturating_add(8).saturating_add(bytes))?;
                            pending.push(Typed { text: c.ch.to_string(), record, anchor });
                        }
                    }
                }
                Item::Char(_) => {
                    let Some(Some((unit, decided))) = matched.get(at.seen).copied() else { return Err(Unpreservable::Untracked) };
                    at.seen = at.seen.saturating_add(1);
                    if unit < at.next_old {
                        return Err(Unpreservable::Untracked);
                    }
                    self.settle(&mut steps, &mut pending, removed, &mut at.next_old, unit, true)?;
                    let change = match decided {
                        Decided::Same => Change::Same,
                        Decided::Accepted => Change::Accepted,
                        Decided::Rejected => Change::Rejected,
                        Decided::Deleted(record) => Change::Deleted(self.revision(record)?),
                        Decided::AcceptedThenDeleted(record) => Change::AcceptedThenDeleted(self.revision(record)?),
                        Decided::RejectedThenDeleted(record) => Change::RejectedThenDeleted(self.revision(record)?),
                    };
                    self.push(&mut steps, Step::Keep { unit, change })?;
                    at.next_old = unit.saturating_add(1);
                    at.last_kept = Some(unit);
                    at.kept_in_paragraph = true;
                }
            }
        }
        self.settle(&mut steps, &mut pending, removed, &mut at.next_old, old.len(), true)?;
        steps.shrink_to_fit();
        Ok(steps)
    }

    /// The source character whose formatting typing continued, formatted `format`: the character
    /// before it in its paragraph; the first after it there (a paragraph's start, a selection typed
    /// over); a character a decision removed there (typing inside a change rejected later); a
    /// character of an earlier half (Enter, then typing; a paste of several paragraphs); the mark
    /// (an empty paragraph); a character of a later half (a paste at a paragraph's start). The
    /// engine types with the formatting before the caret, so this is its order (M3, M4).
    fn anchor(
        &mut self,
        format: u32,
        at: &Place,
        old: &[Char],
        projected: &[usize],
        mark_at: &[usize],
        matched: &[Option<(usize, Decided)>],
        removed: &[bool],
        mark_format: u32,
    ) -> Result<Option<usize>, Unpreservable> {
        let next = next_kept(matched, at.seen);
        let in_paragraph = match (projected.get(at.seen), mark_at.get(at.marks)) {
            (Some(position), Some(mark)) => position < mark,
            _ => true,
        };
        let (next_here, next_later) = if in_paragraph { (next, None) } else { (None, next) };
        let (before_here, before_earlier) = if at.kept_in_paragraph { (at.last_kept, None) } else { (None, at.last_kept) };
        let formatted = |unit: usize| old.get(unit).is_some_and(|o| o.format == format);
        for unit in [before_here, next_here].into_iter().flatten() {
            if formatted(unit) {
                return Ok(Some(unit));
            }
        }
        for unit in at.next_old..next.unwrap_or(old.len()) {
            self.ledger.work(1)?;
            if removed.get(unit).copied().unwrap_or(false) && formatted(unit) {
                return Ok(Some(unit));
            }
        }
        if before_earlier.is_some_and(formatted) {
            return Ok(before_earlier);
        }
        if mark_format == format {
            return Ok(None);
        }
        match next_later {
            Some(unit) if formatted(unit) => Ok(Some(unit)),
            _ => Err(Unpreservable::Formatting),
        }
    }

    /// The decided removals from `next_old` to `gap_end` (all of them when `through`, else as many
    /// as the typing's places need), with the typing waiting in `pending` placed by its anchor:
    /// after the character it continued when that is before or among them, else after them; typed
    /// text keeps its order.
    fn settle(
        &mut self,
        steps: &mut Vec<Step>,
        pending: &mut Vec<Typed>,
        removed: &[bool],
        next_old: &mut usize,
        gap_end: usize,
        through: bool,
    ) -> Result<(), Unpreservable> {
        let start = *next_old;
        let gap = gap_end.saturating_sub(start);
        let mut emitted = 0usize;
        let mut floor = 0usize;
        for group in pending.drain(..) {
            let place = match group.anchor {
                Some(unit) if unit < start => 0,
                Some(unit) if unit < gap_end => unit - start + 1,
                _ if through => gap,
                _ => 0,
            };
            floor = floor.max(place);
            while emitted < floor {
                self.remove(steps, removed, start.saturating_add(emitted))?;
                emitted += 1;
            }
            let Typed { mut text, record, anchor } = group;
            text.shrink_to_fit();
            let revision = self.revision(record)?;
            self.push(steps, Step::Insert { text, revision, anchor })?;
        }
        if through {
            while emitted < gap {
                self.remove(steps, removed, start.saturating_add(emitted))?;
                emitted += 1;
            }
        }
        *next_old = start.saturating_add(emitted);
        Ok(())
    }

    /// A step into the list, its room charged first (its strings were charged as they were made).
    fn push(&mut self, steps: &mut Vec<Step>, step: Step) -> Result<(), Unpreservable> {
        self.ledger.hold(vector_slot::<Step>())?;
        steps.push(step);
        Ok(())
    }

    /// Source character `unit`, gone with its decided change.
    fn remove(&mut self, steps: &mut Vec<Step>, removed: &[bool], unit: usize) -> Result<(), Unpreservable> {
        if !removed.get(unit).copied().unwrap_or(false) {
            return Err(Unpreservable::Untracked);
        }
        self.push(steps, Step::Remove { unit })
    }
}

/// Where the steps stand in the edited paragraphs.
struct Place {
    /// The first source character with no step yet.
    next_old: usize,
    last_kept: Option<usize>,
    /// Whether a source character was kept in this paragraph (since the last mark).
    kept_in_paragraph: bool,
    /// Source characters kept so far, and marks passed.
    seen: usize,
    marks: usize,
}

/// The source character kept next, after `seen` were.
fn next_kept(matched: &[Option<(usize, Decided)>], seen: usize) -> Option<usize> {
    matched.get(seen).copied().flatten().map(|(unit, _)| unit)
}

/// A source character `o` that an edited one `n` stands for: the decision on its tracked changes,
/// and this session's deletion after it (6C-43, 6C-44).
fn transition(o: &Char, n: &Char, era: Era) -> Result<Decided, Unpreservable> {
    let accepted = if o.ins == n.ins {
        false
    } else if o.ins.is_some() && n.ins.is_none() {
        true
    } else {
        return Err(Unpreservable::Altered);
    };
    let deletion = if o.del == n.del {
        Decided::Same
    } else if o.del.is_some() && n.del.is_none() {
        Decided::Rejected
    } else if let (true, Some(record)) = (era.is_new(n.del), n.del) {
        match o.del {
            None => Decided::Deleted(record),
            // The engine never marks deleted text again: the source's deletion was rejected, then
            // the text deleted by this session, which Word records as this session's.
            Some(_) => Decided::RejectedThenDeleted(record),
        }
    } else {
        return Err(Unpreservable::Altered);
    };
    match (accepted, deletion) {
        (false, change) => Ok(change),
        (true, Decided::Same) => Ok(Decided::Accepted),
        (true, Decided::Deleted(record)) => Ok(Decided::AcceptedThenDeleted(record)),
        _ => Err(Unpreservable::Altered),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use wordcraft_doc::{CharProps, Document, InlineObject, Paragraph, Revision as Recorded, RevisionKind};

    use super::*;
    use crate::inspect::budget::Budget;
    use crate::inspect::{MAX_MEMORY, MAX_WORK};

    const ANA: (&str, &str) = ("Ana Setia", "2026-10-01T09:00:00Z");
    const SARI: (&str, &str) = ("Sari Wulan", "2026-10-10T08:00:00Z");

    fn doc(revisions: &[(&str, &str)]) -> Document {
        let mut d = Document::new();
        d.revisions = revisions
            .iter()
            .map(|(author, date)| Recorded { kind: RevisionKind::Insert, author: (*author).into(), date: (*date).into(), read: false })
            .collect();
        d
    }

    /// The documents as a save sees them: `before` read from the file (every record read), `after`
    /// read from the same file and edited (its first records read, as `before`'s, the rest made since).
    fn as_read(before: &Document, after: &Document) -> (Document, Document) {
        let (mut before, mut after) = (before.clone(), after.clone());
        let count = before.revisions.len();
        before.revisions.iter_mut().for_each(|record| record.read = true);
        after.revisions.iter_mut().enumerate().for_each(|(index, record)| record.read = index < count);
        (before, after)
    }

    /// A paragraph from (text, inserted by revision, deleted by revision, bold) pieces.
    fn para(pieces: &[(&str, Option<u32>, Option<u32>, bool)]) -> Paragraph {
        let mut p = Paragraph::new();
        for (text, ins, del, bold) in pieces {
            let off = p.len();
            p.insert_text(off, text, &CharProps { ins: *ins, del: *del, bold: bold.then_some(true), ..CharProps::default() }).unwrap();
        }
        p
    }

    fn key((author, date): (&str, &str)) -> Revision {
        Revision { author: author.into(), date: date.into() }
    }

    fn keep(range: std::ops::Range<usize>, change: Change) -> Vec<Step> {
        range.map(|unit| Step::Keep { unit, change: change.clone() }).collect()
    }

    fn typed(text: &str, revision: (&str, &str), anchor: Option<usize>) -> Step {
        Step::Insert { text: text.into(), revision: key(revision), anchor }
    }

    /// `plan` with a save's budget of its own; what stays held afterwards is exactly what the steps
    /// hold, and nothing once it refuses.
    fn planned(original: &Paragraph, before: &Document, edited: &[&Paragraph], after: &Document) -> Result<Vec<Step>, Unpreservable> {
        let (before, after) = as_read(before, after);
        planned_exactly(original, &before, edited, &after)
    }

    /// [`planned`] with the documents' records as given.
    fn planned_exactly(original: &Paragraph, before: &Document, edited: &[&Paragraph], after: &Document) -> Result<Vec<Step>, Unpreservable> {
        let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
        let result = plan(original, before, edited, after, &mut budget);
        match &result {
            Ok(steps) => assert_eq!(budget.held(), held_by(steps)),
            Err(_) => assert_eq!(budget.held(), 0),
        }
        result
    }

    /// A paragraph with an object at `at` (bytes into the text), formatted by `props`.
    fn with_object(mut paragraph: Paragraph, at: usize, object: InlineObject, props: &CharProps) -> Paragraph {
        paragraph.insert_object(at, object, props).unwrap();
        paragraph
    }

    fn bold() -> CharProps {
        CharProps { bold: Some(true), ..CharProps::default() }
    }

    #[test]
    fn keeps_an_unchanged_paragraph() {
        let original = para(&[("Rent.", None, None, false)]);
        assert_eq!(planned(&original, &doc(&[]), &[&original], &doc(&[])).unwrap(), keep(0..5, Change::Same));
    }

    #[test]
    fn places_typing_before_the_next_character_with_the_character_it_continues() {
        let original = para(&[("Rent due.", None, None, false)]);
        let edited = para(&[("Rent", None, None, false), (" is", Some(0), None, false), (" due.", None, None, false)]);
        let steps = planned(&original, &doc(&[]), &[&edited], &doc(&[SARI])).unwrap();
        let expected =
            [keep(0..4, Change::Same), vec![Step::Insert { text: " is".into(), revision: key(SARI), anchor: Some(3) }], keep(4..9, Change::Same)]
                .concat();
        assert_eq!(steps, expected);
    }

    #[test]
    fn continues_the_first_character_when_typing_at_the_start() {
        let original = para(&[("Rent.", None, None, true)]);
        let edited = para(&[("Now ", Some(0), None, true), ("Rent.", None, None, true)]);
        let steps = planned(&original, &doc(&[]), &[&edited], &doc(&[SARI])).unwrap();
        assert_eq!(steps[0], Step::Insert { text: "Now ".into(), revision: key(SARI), anchor: Some(0) });
    }

    #[test]
    fn marks_text_deleted_in_this_session() {
        let original = para(&[("Rent monthly.", None, None, false)]);
        let edited = para(&[("Rent ", None, None, false), ("monthly", None, Some(0), false), (".", None, None, false)]);
        let steps = planned(&original, &doc(&[]), &[&edited], &doc(&[SARI])).unwrap();
        assert_eq!(steps, [keep(0..5, Change::Same), keep(5..12, Change::Deleted(key(SARI))), keep(12..13, Change::Same)].concat());
    }

    #[test]
    fn puts_a_new_mark_between_the_halves_of_a_split() {
        let original = para(&[("One. Two.", None, None, false)]);
        let mut first = para(&[("One.", None, None, false)]);
        first.mark.ins = Some(0);
        let second = para(&[(" Two.", None, None, false)]);
        let steps = planned(&original, &doc(&[]), &[&first, &second], &doc(&[SARI])).unwrap();
        assert_eq!(steps, [keep(0..4, Change::Same), vec![Step::Break { revision: key(SARI) }], keep(4..9, Change::Same)].concat());
    }

    #[test]
    fn reads_accepting_and_rejecting_as_decisions() {
        let original = para(&[("Rent", None, None, false), (" is due", Some(0), None, false), (" monthly", None, Some(0), false)]);
        let accepted = para(&[("Rent is due", None, None, false)]);
        let steps = planned(&original, &doc(&[ANA]), &[&accepted], &doc(&[])).unwrap();
        let removed: Vec<Step> = (11..19).map(|unit| Step::Remove { unit }).collect();
        assert_eq!(steps, [keep(0..4, Change::Same), keep(4..11, Change::Accepted), removed].concat());
        let rejected = para(&[("Rent monthly", None, None, false)]);
        let steps = planned(&original, &doc(&[ANA]), &[&rejected], &doc(&[])).unwrap();
        let removed: Vec<Step> = (4..11).map(|unit| Step::Remove { unit }).collect();
        assert_eq!(steps, [keep(0..4, Change::Same), removed, keep(11..19, Change::Rejected)].concat());
    }

    #[test]
    fn writes_a_deletion_rejected_and_made_again_as_this_sessions() {
        // 6C-44 (amends the brief's `leaves_deleted_text_deleted_again_as_it_was`): the engine never
        // marks deleted text again, so Ana's deletion now in Sari's name was rejected, then deleted by
        // Sari; Word records that deletion as Sari's.
        let original = para(&[("Rent ", None, None, false), ("monthly", None, Some(0), false)]);
        let edited = para(&[("Rent ", None, None, false), ("monthly", None, Some(1), false)]);
        let steps = planned(&original, &doc(&[ANA]), &[&edited], &doc(&[ANA, SARI])).unwrap();
        assert_eq!(steps, [keep(0..5, Change::Same), keep(5..12, Change::RejectedThenDeleted(key(SARI)))].concat());
    }

    /// The revision a step deletes its character with now, if any.
    fn deleted_now(step: &Step) -> Option<&Revision> {
        match step {
            Step::Keep { change: Change::Deleted(key) | Change::AcceptedThenDeleted(key) | Change::RejectedThenDeleted(key), .. } => Some(key),
            _ => None,
        }
    }

    #[test]
    fn credits_every_deletion_after_accepting_everything_to_this_session() {
        // C1 (P01): Ana inserted "Tenant ", Bob deleted "Tenant ". Accept all, then Sari deletes
        // "Tenant ". Either reading of which "Tenant " stays writes it deleted by Sari; never Bob's
        // deletion kept as it was.
        const BOB: (&str, &str) = ("Bob Hartono", "2026-10-02T10:00:00Z");
        let original =
            para(&[("the ", None, None, false), ("Tenant ", Some(0), None, false), ("Tenant ", None, Some(1), false), ("shall", None, None, false)]);
        let edited = para(&[("the ", None, None, false), ("Tenant ", None, Some(2), false), ("shall", None, None, false)]);
        let steps = planned(&original, &doc(&[ANA, BOB]), &[&edited], &doc(&[ANA, BOB, SARI])).unwrap();
        assert!(steps.iter().filter_map(deleted_now).all(|found| *found == key(SARI)));
        assert_eq!(steps.iter().filter_map(deleted_now).count(), "Tenant ".len());
        assert_eq!(steps.iter().filter(|step| matches!(step, Step::Keep { change: Change::Same, .. })).count(), "the shall".len());
        // Suffix first (diff::script): Bob's "Tenant " stays, deleted now by Sari; Ana's goes.
        let removed: Vec<Step> = (4..11).map(|unit| Step::Remove { unit }).collect();
        assert_eq!(
            steps,
            [keep(0..4, Change::Same), removed, keep(11..18, Change::RejectedThenDeleted(key(SARI))), keep(18..23, Change::Same)].concat()
        );
        // P01b, the smallest form.
        let original = para(&[("a", Some(0), None, false), ("a", None, Some(1), false)]);
        let edited = para(&[("a", None, Some(2), false)]);
        let steps = planned(&original, &doc(&[ANA, BOB]), &[&edited], &doc(&[ANA, BOB, SARI])).unwrap();
        assert_eq!(steps, vec![Step::Remove { unit: 0 }, Step::Keep { unit: 1, change: Change::RejectedThenDeleted(key(SARI)) }]);
    }

    #[test]
    fn pairs_again_where_the_diff_left_out_a_character_that_cannot_go() {
        const BOB: (&str, &str) = ("Bob Hartono", "2026-10-02T10:00:00Z");
        // M2 (P03): accept all, then Sari deletes the plain "a" beside Bob's deleted "a". The diff
        // pairs Bob's "a" (as short a script): the plain one is paired again, Bob's goes.
        let long: String = (0..1_500).map(|index| char::from(b'a' + u8::try_from(index % 26).unwrap())).collect();
        let original = para(&[
            ("Rent", None, None, false),
            (&long, Some(0), None, false),
            ("z", None, None, false),
            ("a", None, None, false),
            ("a", None, Some(1), false),
        ]);
        let edited = para(&[("Rent", None, None, false), (&long, None, None, false), ("z", None, None, false), ("a", None, Some(2), false)]);
        let steps = planned(&original, &doc(&[ANA, BOB]), &[&edited], &doc(&[ANA, BOB, SARI])).unwrap();
        let expected = [
            keep(0..4, Change::Same),
            keep(4..1_504, Change::Accepted),
            keep(1_504..1_505, Change::Same),
            keep(1_505..1_506, Change::Deleted(key(SARI))),
            vec![Step::Remove { unit: 1_506 }],
        ]
        .concat();
        assert_eq!(steps, expected);
        // Where what must go is not one stretch: Bob's "a" and Ana's "z" go, the plain "a" is
        // deleted now.
        let original = para(&[("q", None, None, false), ("a", None, Some(1), false), ("a", None, None, false), ("z", Some(0), None, false)]);
        let edited = para(&[("q", None, None, false), ("a", None, Some(2), false)]);
        let steps = planned(&original, &doc(&[ANA, BOB]), &[&edited], &doc(&[ANA, BOB, SARI])).unwrap();
        let expected =
            [keep(0..1, Change::Same), vec![Step::Remove { unit: 1 }], keep(2..3, Change::Deleted(key(SARI))), vec![Step::Remove { unit: 3 }]]
                .concat();
        assert_eq!(steps, expected);
        // Paired again two pairs away: Bob's "ab" goes, the plain "ab" is deleted now.
        let original = para(&[("x", None, None, false), ("ab", None, None, false), ("ab", None, Some(1), false)]);
        let edited = para(&[("x", None, None, false), ("ab", None, Some(2), false)]);
        let steps = planned(&original, &doc(&[ANA, BOB]), &[&edited], &doc(&[ANA, BOB, SARI])).unwrap();
        let expected =
            [keep(0..1, Change::Same), keep(1..3, Change::Deleted(key(SARI))), vec![Step::Remove { unit: 3 }, Step::Remove { unit: 4 }]].concat();
        assert_eq!(steps, expected);
        // No pairing leaves out only what can go: refused.
        let original = para(&[("q", None, None, false), ("a", None, None, false), ("b", None, None, false)]);
        let edited = para(&[("q", None, None, false), ("b", None, Some(0), false)]);
        assert_eq!(planned(&original, &doc(&[]), &[&edited], &doc(&[SARI])), Err(Unpreservable::Untracked));
    }

    #[test]
    fn reads_an_ambiguous_decision_by_the_suffix_first() {
        // M1 (P02, P02b): accept all, where an insertion and a deletion hold the same letter. The text
        // written is the same either way; which source run survives is the diff's suffix-first rule,
        // pinned here so that changing it is a decision.
        const BOB: (&str, &str) = ("Bob Hartono", "2026-10-02T10:00:00Z");
        let original = para(&[("ab", Some(0), None, false), ("b", None, Some(1), false)]);
        let accepted = para(&[("ab", None, None, false)]);
        let steps = planned(&original, &doc(&[ANA, BOB]), &[&accepted], &doc(&[ANA, BOB])).unwrap();
        assert_eq!(steps, [keep(0..1, Change::Accepted), vec![Step::Remove { unit: 1 }], keep(2..3, Change::Rejected)].concat());
        let original = para(&[("a", Some(0), None, false), ("a", None, Some(1), false)]);
        let accepted = para(&[("a", None, None, false)]);
        let steps = planned(&original, &doc(&[ANA, BOB]), &[&accepted], &doc(&[ANA, BOB])).unwrap();
        assert_eq!(steps, vec![Step::Remove { unit: 0 }, Step::Keep { unit: 1, change: Change::Rejected }]);
    }

    #[test]
    fn refuses_untracked_text_formatting_objects_and_paragraph_settings() {
        let original = para(&[("Rent monthly.", None, None, false)]);
        let untracked = para(&[("Rent weekly.", None, None, false)]);
        assert_eq!(planned(&original, &doc(&[]), &[&untracked], &doc(&[])), Err(Unpreservable::Untracked));
        let bold_typing = para(&[("Rent", None, None, false), ("!", Some(0), None, true), (" monthly.", None, None, false)]);
        assert_eq!(planned(&original, &doc(&[]), &[&bold_typing], &doc(&[SARI])), Err(Unpreservable::Formatting));
        let mut object = original.clone();
        object
            .insert_object(0, wordcraft_doc::InlineObject::BookmarkStart { name: "x".into() }, &CharProps { ins: Some(0), ..CharProps::default() })
            .unwrap();
        assert_eq!(planned(&original, &doc(&[]), &[&object], &doc(&[SARI])), Err(Unpreservable::InsertedObject));
        let mut restyled = original.clone();
        restyled.props.style = Some("Heading1".into());
        assert_eq!(planned(&original, &doc(&[]), &[&restyled], &doc(&[])), Err(Unpreservable::Formatting));
    }

    #[test]
    fn refuses_a_split_without_a_new_mark() {
        let original = para(&[("One. Two.", None, None, false)]);
        let first = para(&[("One.", None, None, false)]);
        let second = para(&[(" Two.", None, None, false)]);
        assert_eq!(planned(&original, &doc(&[]), &[&first, &second], &doc(&[])), Err(Unpreservable::Structure));
    }

    // Review Focus 3: typing where Word could show it wrongly. The anchor is a character whose
    // formatting (its hyperlink included) the typed text has, so copying its run properties writes
    // what the model shows.

    #[test]
    fn anchors_typing_next_to_a_hyperlink_where_its_link_says() {
        let link = CharProps { link: Some("https://example.com/terms".into()), ..CharProps::default() };
        let mut original = para(&[("See ", None, None, false)]);
        original.insert_text(4, "terms", &link).unwrap();
        original.insert_text(9, " now", &CharProps::default()).unwrap();
        // Typing that continues the link stays with its last character.
        let mut inside = original.clone();
        inside.insert_text(9, "!", &CharProps { ins: Some(0), ..link.clone() }).unwrap();
        let steps = planned(&original, &doc(&[]), &[&inside], &doc(&[SARI])).unwrap();
        assert_eq!(steps, [keep(0..9, Change::Same), vec![typed("!", SARI, Some(8))], keep(9..13, Change::Same)].concat());
        // Typing without the link at its end takes the character after it.
        let mut outside = original.clone();
        outside.insert_text(9, "!", &CharProps { ins: Some(0), ..CharProps::default() }).unwrap();
        let steps = planned(&original, &doc(&[]), &[&outside], &doc(&[SARI])).unwrap();
        assert_eq!(steps[9], typed("!", SARI, Some(9)));
        // A paragraph that ends with the link: typing after it without the link takes the mark's.
        let mut ending = para(&[]);
        ending.insert_text(0, "terms", &link).unwrap();
        let mut after = ending.clone();
        after.insert_text(5, ".", &CharProps { ins: Some(0), ..CharProps::default() }).unwrap();
        let steps = planned(&ending, &doc(&[]), &[&after], &doc(&[SARI])).unwrap();
        assert_eq!(steps, [keep(0..5, Change::Same), vec![typed(".", SARI, None)]].concat());
    }

    #[test]
    fn anchors_typing_next_to_a_field_and_a_bookmark() {
        let field = InlineObject::Field { instr: "PAGE".into(), result: "3".into(), locked: false };
        let original = with_object(para(&[("Page ", None, None, false), (" of", None, None, false)]), 5, field, &bold());
        // After the field, with its formatting: the field.
        let mut after_field = original.clone();
        let at = 5 + wordcraft_doc::para::OBJ.len_utf8();
        after_field.insert_text(at, "!", &CharProps { ins: Some(0), ..bold() }).unwrap();
        let steps = planned(&original, &doc(&[]), &[&after_field], &doc(&[SARI])).unwrap();
        assert_eq!(steps, [keep(0..6, Change::Same), vec![typed("!", SARI, Some(5))], keep(6..9, Change::Same)].concat());

        let marked = with_object(para(&[("Rent", None, None, true)]), 0, InlineObject::BookmarkStart { name: "rent".into() }, &CharProps::default());
        let at = wordcraft_doc::para::OBJ.len_utf8();
        // After a bookmark at the start, as the bookmark is formatted: the bookmark.
        let mut plain = marked.clone();
        plain.insert_text(at, "x", &CharProps { ins: Some(0), ..CharProps::default() }).unwrap();
        assert_eq!(planned(&marked, &doc(&[]), &[&plain], &doc(&[SARI])).unwrap()[1], typed("x", SARI, Some(0)));
        // Formatted as the text after it: that text.
        let mut continued = marked.clone();
        continued.insert_text(at, "Now ", &CharProps { ins: Some(0), ..bold() }).unwrap();
        assert_eq!(planned(&marked, &doc(&[]), &[&continued], &doc(&[SARI])).unwrap()[1], typed("Now ", SARI, Some(1)));
    }

    #[test]
    fn types_into_an_empty_paragraph_with_its_mark() {
        let mut original = para(&[]);
        original.mark = bold();
        let mut edited = original.clone();
        edited.insert_text(0, "New", &CharProps { ins: Some(0), ..bold() }).unwrap();
        assert_eq!(planned(&original, &doc(&[]), &[&edited], &doc(&[SARI])).unwrap(), vec![typed("New", SARI, None)]);
        let mut plain = original.clone();
        plain.insert_text(0, "New", &CharProps { ins: Some(0), ..CharProps::default() }).unwrap();
        assert_eq!(planned(&original, &doc(&[]), &[&plain], &doc(&[SARI])), Err(Unpreservable::Formatting));
    }

    #[test]
    fn types_inside_another_authors_insertion_and_deletion_as_this_sessions_own() {
        // Inside Ana's insertion: the typing is Sari's, anchored to Ana's character.
        let original = para(&[("Rent", None, None, false), (" is due", Some(0), None, false)]);
        let mut edited = original.clone();
        edited.insert_text(7, " now", &CharProps { ins: Some(1), ..CharProps::default() }).unwrap();
        let steps = planned(&original, &doc(&[ANA]), &[&edited], &doc(&[ANA, SARI])).unwrap();
        assert_eq!(steps, [keep(0..7, Change::Same), vec![typed(" now", SARI, Some(6))], keep(7..11, Change::Same)].concat());
        // Inside Ana's deletion: the same.
        let original = para(&[("Rent ", None, None, false), ("monthly", None, Some(0), false)]);
        let mut edited = original.clone();
        edited.insert_text(8, "X", &CharProps { ins: Some(1), ..CharProps::default() }).unwrap();
        let steps = planned(&original, &doc(&[ANA]), &[&edited], &doc(&[ANA, SARI])).unwrap();
        assert_eq!(steps, [keep(0..8, Change::Same), vec![typed("X", SARI, Some(7))], keep(8..12, Change::Same)].concat());
    }

    #[test]
    fn continues_a_replaced_selection_from_the_character_after_it() {
        // "b" selected and typed over: the typing took b's formatting, and b is deleted after it.
        let original = para(&[("a", None, None, false), ("b", None, None, true), ("c", None, None, false)]);
        let edited = para(&[("a", None, None, false), ("X", Some(0), None, true), ("b", None, Some(1), true), ("c", None, None, false)]);
        let steps = planned(&original, &doc(&[]), &[&edited], &doc(&[SARI, SARI])).unwrap();
        assert_eq!(
            steps,
            [keep(0..1, Change::Same), vec![typed("X", SARI, Some(1))], keep(1..2, Change::Deleted(key(SARI))), keep(2..3, Change::Same)].concat()
        );
    }

    #[test]
    fn plans_a_paste_of_several_paragraphs_inside_one() {
        // 6C-34: "A¶B¶C" pasted between x and y. The middle paragraph is new; the last carries the
        // source's own mark. Typing in a paragraph with no source character of its own continues the
        // character before it in an earlier half when the mark is formatted otherwise.
        let original = para(&[("xy", None, None, true)]);
        let mut head = para(&[("x", None, None, true), ("A", Some(0), None, true)]);
        head.mark.ins = Some(0);
        let mut middle = para(&[("B", Some(0), None, true)]);
        middle.mark.ins = Some(0);
        let tail = para(&[("C", Some(0), None, true), ("y", None, None, true)]);
        let steps = planned(&original, &doc(&[]), &[&head, &middle, &tail], &doc(&[SARI])).unwrap();
        let mark = Step::Break { revision: key(SARI) };
        let expected = [
            keep(0..1, Change::Same),
            vec![typed("A", SARI, Some(0)), mark.clone(), typed("B", SARI, Some(0)), mark, typed("C", SARI, Some(1))],
            keep(1..2, Change::Same),
        ]
        .concat();
        assert_eq!(steps, expected);
        // "A¶B" pasted at the start: the first half has no source character before it, so it
        // continues the first one after it, in the later half.
        let mut head = para(&[("A", Some(0), None, true)]);
        head.mark.ins = Some(0);
        let tail = para(&[("B", Some(0), None, true), ("xy", None, None, true)]);
        let steps = planned(&original, &doc(&[]), &[&head, &tail], &doc(&[SARI])).unwrap();
        let expected =
            [vec![typed("A", SARI, Some(0)), Step::Break { revision: key(SARI) }, typed("B", SARI, Some(0))], keep(0..2, Change::Same)].concat();
        assert_eq!(steps, expected);
    }

    #[test]
    fn refuses_a_paragraph_whose_runs_do_not_cover_its_text() {
        // A model the planner cannot read character by character: a unit would get no step.
        let original = para(&[("abc", None, None, false)]);
        for length in [2, 4] {
            let mut short = original.clone();
            short.runs[0].len = length;
            assert_eq!(planned(&short, &doc(&[]), &[&short], &doc(&[])), Err(Unpreservable::Structure));
            assert_eq!(planned(&original, &doc(&[]), &[&short], &doc(&[])), Err(Unpreservable::Structure));
        }
    }

    #[test]
    fn plans_a_rejected_split_as_the_paragraph_it_was() {
        // 6C-21: rejecting this session's own paragraph mark joins the halves again (the reverse of
        // Enter); nothing of the split is left to write.
        let original = para(&[("One. Two.", None, None, false)]);
        let joined = original.clone();
        assert_eq!(planned(&original, &doc(&[]), &[&joined], &doc(&[SARI])).unwrap(), keep(0..9, Change::Same));
    }

    #[test]
    fn writes_this_sessions_changes_after_it_accepted_everything_on_record() {
        // 6C-21: accepting in an edit session decides Ana's insertion and leaves the session's own
        // typing and paragraph mark tracked, as they were.
        let original = para(&[("Rent", None, None, false), (" is due", Some(0), None, false)]);
        let mut head = para(&[("Rent is due", None, None, false), (" now", Some(1), None, false)]);
        head.mark.ins = Some(1);
        let tail = para(&[]);
        let steps = planned(&original, &doc(&[ANA]), &[&head, &tail], &doc(&[ANA, SARI])).unwrap();
        let expected =
            [keep(0..4, Change::Same), keep(4..11, Change::Accepted), vec![typed(" now", SARI, Some(10)), Step::Break { revision: key(SARI) }]]
                .concat();
        assert_eq!(steps, expected);
    }

    #[test]
    fn knows_this_sessions_revisions_by_their_records_not_their_names() {
        // F21: the file holds an insertion in the session's own name and date; the session's typing
        // is a record of its own (made after the read), so it is typed, not the file's.
        let original = para(&[("Rent", None, None, false), (" is", Some(0), None, false)]);
        let edited = para(&[("Rent", None, None, false), (" is", Some(0), None, false), (" due", Some(1), None, false)]);
        let steps = planned(&original, &doc(&[SARI]), &[&edited], &doc(&[SARI, SARI])).unwrap();
        assert_eq!(steps, [keep(0..7, Change::Same), vec![typed(" due", SARI, Some(6))]].concat());
    }

    #[test]
    fn knows_this_sessions_records_by_their_watermark_not_their_place() {
        // 6C-45: a full application's accept all clears every record (as Task 9's `decide_all`
        // does); typing afterwards makes a record at 0, not read from the file.
        let (before, _) = as_read(&doc(&[ANA]), &doc(&[]));
        let original = para(&[("Rent", None, None, false), (" is", Some(0), None, false)]);
        let edited = para(&[("Rent is", None, None, false), (" due", Some(0), None, false)]);
        let steps = planned_exactly(&original, &before, &[&edited], &doc(&[SARI])).unwrap();
        assert_eq!(steps, [keep(0..4, Change::Same), keep(4..7, Change::Accepted), vec![typed(" due", SARI, Some(6))]].concat());
        // The file's records in another order: still the file's, by what they record.
        let mut after = doc(&[SARI, SARI, ANA]);
        after.revisions[2].read = true;
        let edited = para(&[("Rent", None, None, false), (" is", Some(2), None, false), (" due", Some(0), None, false)]);
        let steps = planned_exactly(&original, &before, &[&edited], &after).unwrap();
        assert_eq!(steps, [keep(0..7, Change::Same), vec![typed(" due", SARI, Some(6))]].concat());
        // A record read from the file that the file does not hold, or no record at all: refused.
        let mut after = doc(&[SARI]);
        after.revisions[0].read = true;
        let edited = para(&[("Rent", None, None, false), (" is", Some(0), None, false)]);
        assert_eq!(planned_exactly(&original, &before, &[&edited], &after), Err(Unpreservable::Altered));
        let edited = para(&[("Rent", None, None, false), (" is", Some(0), None, false), (" due", Some(5), None, false)]);
        assert_eq!(planned(&original, &doc(&[ANA]), &[&edited], &doc(&[ANA, SARI])), Err(Unpreservable::Altered));
    }

    #[test]
    fn refuses_a_mark_the_source_paragraph_does_not_end_with() {
        let original = para(&[("One. Two.", None, None, false)]);
        // The last half ends with the source's own mark, which was not inserted now.
        let mut first = para(&[("One.", None, None, false)]);
        first.mark.ins = Some(0);
        let mut second = para(&[(" Two.", None, None, false)]);
        second.mark.ins = Some(0);
        assert_eq!(planned(&original, &doc(&[]), &[&first, &second], &doc(&[SARI])), Err(Unpreservable::Structure));
        // Nor deleted.
        let mut deleted = original.clone();
        deleted.mark.del = Some(0);
        assert_eq!(planned(&original, &doc(&[]), &[&deleted], &doc(&[SARI])), Err(Unpreservable::Structure));
        // A new mark cannot be a deleted one either.
        let mut first = para(&[("One.", None, None, false)]);
        first.mark.ins = Some(0);
        first.mark.del = Some(0);
        let second = para(&[(" Two.", None, None, false)]);
        assert_eq!(planned(&original, &doc(&[]), &[&first, &second], &doc(&[SARI])), Err(Unpreservable::Structure));
        // The section break stays with the source's mark.
        let mut sectioned = original.clone();
        sectioned.section = Some(Box::default());
        assert_eq!(planned(&original, &doc(&[]), &[&sectioned], &doc(&[])), Err(Unpreservable::Formatting));
        assert_eq!(planned(&original, &doc(&[]), &[], &doc(&[])), Err(Unpreservable::Structure));
    }

    #[test]
    fn refuses_text_removed_or_an_object_changed_without_a_tracked_change() {
        let original = para(&[("Rent monthly.", None, None, false)]);
        let removed = para(&[("Rent .", None, None, false)]);
        assert_eq!(planned(&original, &doc(&[]), &[&removed], &doc(&[])), Err(Unpreservable::Untracked));
        let page = |result: &str| InlineObject::Field { instr: "PAGE".into(), result: result.into(), locked: false };
        let original = with_object(para(&[("Page ", None, None, false)]), 5, page("3"), &CharProps::default());
        let recalculated = with_object(para(&[("Page ", None, None, false)]), 5, page("4"), &CharProps::default());
        assert_eq!(planned(&original, &doc(&[]), &[&recalculated], &doc(&[])), Err(Unpreservable::Untracked));
        // Deleting the object in this session is a change the writer may place (or refuse as locked).
        let mut deleted = original.clone();
        deleted.format(5, 5 + wordcraft_doc::para::OBJ.len_utf8(), &|props: &mut CharProps| props.del = Some(0)).unwrap();
        let steps = planned(&original, &doc(&[]), &[&deleted], &doc(&[SARI])).unwrap();
        assert_eq!(steps, [keep(0..5, Change::Same), keep(5..6, Change::Deleted(key(SARI)))].concat());
    }

    #[test]
    fn writes_an_insertion_accepted_then_deleted_as_this_sessions_deletion() {
        // 6C-43 (P06): Ana's insertion accepted, then deleted by Sari.
        let original = para(&[("Rent", None, None, false), (" is due", Some(0), None, false)]);
        let edited = para(&[("Rent", None, None, false), (" is due", None, Some(1), false)]);
        let steps = planned(&original, &doc(&[ANA]), &[&edited], &doc(&[ANA, SARI])).unwrap();
        assert_eq!(steps, [keep(0..4, Change::Same), keep(4..11, Change::AcceptedThenDeleted(key(SARI)))].concat());
    }

    #[test]
    fn refuses_a_decided_change_it_cannot_write_as_one() {
        let original = para(&[("Rent", None, None, false), (" is due", Some(0), None, false)]);
        // Ana's insertion now in Bob's name: an earlier change altered.
        let edited = para(&[("Rent", None, None, false), (" is due", Some(1), None, false)]);
        assert_eq!(planned(&original, &doc(&[ANA, SARI]), &[&edited], &doc(&[ANA, SARI])), Err(Unpreservable::Altered));
        // Typed and deleted in this session at once: the engine removes such text instead.
        let typed_deleted = para(&[("Rent", None, None, false), (" is due", Some(0), None, false), ("!", Some(1), Some(1), false)]);
        assert_eq!(planned(&original, &doc(&[ANA]), &[&typed_deleted], &doc(&[ANA, SARI])), Err(Unpreservable::Altered));
    }

    #[test]
    fn refuses_more_decisions_than_one_diff_may_make() {
        let mut original = Paragraph::new();
        for _ in 0..=crate::inspect::MAX_CHANGES {
            let off = original.len();
            original.insert_text(off, "a", &CharProps::default()).unwrap();
            let off = original.len();
            original.insert_text(off, "b", &CharProps { ins: Some(0), ..CharProps::default() }).unwrap();
        }
        let rejected = para(&[(&"a".repeat(crate::inspect::MAX_CHANGES + 1), None, None, false)]);
        assert_eq!(planned(&original, &doc(&[ANA]), &[&rejected], &doc(&[ANA])), Err(Unpreservable::TooManyChanges));
    }

    #[test]
    fn charges_the_saves_budget_and_gives_back_what_it_refuses() {
        let original = para(&[("Rent due.", None, None, false)]);
        let edited = para(&[("Rent", None, None, false), (" is", Some(0), None, false), (" due.", None, None, false)]);
        // One budget for the save: each paragraph's steps stay held, and its work is spent.
        let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
        let first = plan(&original, &doc(&[]), &[&edited], &doc(&[SARI]), &mut budget).unwrap();
        let spent = MAX_WORK - budget.work_left();
        assert!(spent > 0);
        let second = plan(&original, &doc(&[]), &[&edited], &doc(&[SARI]), &mut budget).unwrap();
        assert_eq!(budget.held(), held_by(&first) + held_by(&second));
        assert!(held_by(&first) >= u64::try_from(first.len() * std::mem::size_of::<Step>()).unwrap());
        assert!(MAX_WORK - budget.work_left() >= 2 * spent);
        // Too little memory or work: refused, holding nothing more than before.
        let mut small = Budget::new(MAX_WORK, 1_000);
        assert_eq!(plan(&original, &doc(&[]), &[&edited], &doc(&[SARI]), &mut small), Err(Unpreservable::TooManyChanges));
        assert_eq!(small.held(), 0);
        let mut tired = Budget::new(10, MAX_MEMORY);
        assert_eq!(plan(&original, &doc(&[]), &[&edited], &doc(&[SARI]), &mut tired), Err(Unpreservable::TooManyChanges));
        assert_eq!(tired.held(), 0);
    }

    #[test]
    fn accepts_an_insertion_longer_than_one_diff_compares_with_its_tracked_changes() {
        // 1,500 inserted characters accepted: decisions are no differences (I2), so MAX_CHANGES counts
        // only what goes.
        let inserted: String = (0..1_500).map(|index| char::from(b'a' + u8::try_from(index % 26).unwrap())).collect();
        let original = para(&[("Rent", None, None, false), (&inserted, Some(0), None, false), (" now", None, Some(0), false)]);
        let accepted = para(&[("Rent", None, None, false), (&inserted, None, None, false)]);
        let steps = planned(&original, &doc(&[ANA]), &[&accepted], &doc(&[ANA])).unwrap();
        let removed: Vec<Step> = (1_504..1_508).map(|unit| Step::Remove { unit }).collect();
        assert_eq!(steps, [keep(0..4, Change::Same), keep(4..1_504, Change::Accepted), removed].concat());
        // Checked as strictly there: a character changed without a tracked change is still refused.
        let mut untracked = accepted.clone();
        untracked.insert_text(2, "x", &CharProps::default()).unwrap();
        assert_eq!(planned(&original, &doc(&[ANA]), &[&untracked], &doc(&[ANA])), Err(Unpreservable::Untracked));
    }

    const BOB: (&str, &str) = ("Bob Hartono", "2026-10-02T10:00:00Z");

    /// A long paragraph of 400 formatted pieces, Ana's insertions and Bob's deletions among them,
    /// and its edited group: decisions on the pieces `decided` picks, this session's deletions,
    /// typing and two paragraph marks.
    fn long_case(decided: &dyn Fn(usize) -> bool) -> (Paragraph, Vec<Paragraph>) {
        let props = |piece: usize, ins: Option<u32>, del: Option<u32>| CharProps {
            font: Some(format!("Font {}", piece % 40)),
            ins,
            del,
            ..CharProps::default()
        };
        let tracked = |piece: usize| match piece % 5 {
            1 => (Some(0), None),
            3 => (None, Some(1)),
            _ => (None, None),
        };
        let mut original = Paragraph::new();
        let mut edited = vec![Paragraph::new()];
        for piece in 0..400 {
            let text = format!("w{piece:04}abcd ");
            let (ins, del) = tracked(piece);
            let off = original.len();
            original.insert_text(off, &text, &props(piece, ins, del)).unwrap();
            let current = edited.last_mut().unwrap();
            let off = current.len();
            match (ins, del) {
                // An insertion accepted is plain; a deletion accepted is gone.
                (Some(_), _) | (_, Some(_)) if decided(piece) => {
                    if ins.is_some() {
                        current.insert_text(off, &text, &props(piece, None, None)).unwrap();
                    }
                }
                (None, None) if piece % 7 == 0 => {
                    current.insert_text(off, &text, &props(piece, None, Some(2))).unwrap();
                }
                _ => {
                    current.insert_text(off, &text, &props(piece, ins, del)).unwrap();
                }
            }
            // Typing after a piece that stays (a piece gone leaves nothing of its formatting there).
            if piece % 9 == 0 && !(piece % 5 == 3 && decided(piece)) {
                let off = current.len();
                current.insert_text(off, "NEW", &props(piece, Some(2), None)).unwrap();
            }
            if piece == 150 || piece == 300 {
                current.mark.ins = Some(2);
                edited.push(Paragraph::new());
            }
        }
        (original, edited)
    }

    /// A paragraph of 20,000 characters with one piece in three deleted now and a word typed: the
    /// diffs find almost nothing, so the steps (a revision for each deleted character) weigh most.
    fn deleted_case() -> (Paragraph, Vec<Paragraph>) {
        let (mut original, mut edited) = (Paragraph::new(), Paragraph::new());
        for piece in 0..2_000 {
            let text = format!("w{piece:04}abcd ");
            let props = CharProps { font: Some(format!("Font {}", piece % 40)), ..CharProps::default() };
            let off = original.len();
            original.insert_text(off, &text, &props).unwrap();
            let del = (piece % 3 == 0).then_some(2);
            let off = edited.len();
            edited.insert_text(off, &text, &CharProps { del, ..props.clone() }).unwrap();
            if piece == 1_000 {
                let off = edited.len();
                edited.insert_text(off, "NEW ", &CharProps { ins: Some(2), ..props }).unwrap();
            }
        }
        (original, vec![edited])
    }

    /// Set in the process `holds_no_more_than_it_charges` starts to measure in.
    const MEASURING: &str = "WORDCRAFT_PLAN_MEASURE";

    /// Everything the planner allocates is charged first: measured by the counting allocator (the
    /// scan's tests install it in this binary) in a process of its own, since its counts are the
    /// whole process's; for a plan through the first diff, and one through the character-only diff
    /// (every decision taken, too many differences for the first).
    #[test]
    fn holds_no_more_than_it_charges() {
        if std::env::var_os(MEASURING).is_some() {
            let (before, after) = as_read(&doc(&[ANA, BOB]), &doc(&[ANA, BOB, SARI]));
            let some: &dyn Fn(usize) -> bool = &|piece| piece < 100;
            let all: &dyn Fn(usize) -> bool = &|_| true;
            for (name, case) in [("first diff", long_case(some)), ("every decision taken", long_case(all)), ("steps", deleted_case())] {
                let (original, edited) = case;
                let group: Vec<&Paragraph> = edited.iter().collect();
                let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
                let start = wordcraft_testkit::counting::reset_peak();
                let steps = plan(&original, &before, &group, &after, &mut budget).unwrap();
                let allocated = u64::try_from(wordcraft_testkit::counting::peak() - start).unwrap();
                let kept = u64::try_from(wordcraft_testkit::counting::held() - start).unwrap();
                eprintln!(
                    "{name}: {} steps; allocated {allocated} at most, {kept} kept; charged {} at most, {} kept",
                    steps.len(),
                    budget.peak(),
                    budget.held()
                );
                assert!(steps.len() > 4_000);
                assert!(allocated <= budget.peak(), "{name}: allocated {allocated}, charged {}", budget.peak());
                assert!(kept <= budget.held(), "{name}: kept {kept}, charged {}", budget.held());
                assert_eq!(budget.held(), held_by(&steps));
            }
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "preserve::plan::tests::holds_no_more_than_it_charges", "--test-threads=1", "--nocapture"])
            .env(MEASURING, "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn places_typing_beside_the_character_it_continued_before_decided_removals() {
        // I1 (P14): "!" typed after "terms" (in its link), then Ana's "XY" after it rejected. The
        // typing stays next to "terms", so the writer keeps it in the hyperlink.
        let link = CharProps { link: Some("https://example.com/terms".into()), ..CharProps::default() };
        let mut original = Paragraph::new();
        original.insert_text(0, "terms", &link).unwrap();
        original.insert_text(5, "XY", &CharProps { ins: Some(0), ..CharProps::default() }).unwrap();
        original.insert_text(7, " now", &CharProps::default()).unwrap();
        let mut edited = Paragraph::new();
        edited.insert_text(0, "terms", &link).unwrap();
        edited.insert_text(5, "!", &CharProps { ins: Some(1), ..link.clone() }).unwrap();
        edited.insert_text(6, " now", &CharProps::default()).unwrap();
        let steps = planned(&original, &doc(&[ANA]), &[&edited], &doc(&[ANA, SARI])).unwrap();
        let removed: Vec<Step> = (5..7).map(|unit| Step::Remove { unit }).collect();
        assert_eq!(steps, [keep(0..5, Change::Same), vec![typed("!", SARI, Some(4))], removed, keep(7..11, Change::Same)].concat());
    }

    #[test]
    fn continues_a_character_its_decision_removed() {
        // M4 (P08): "Z" typed after Ana's bold "Q", then only Q rejected: the typing continued Q.
        let original = para(&[("a", None, None, false), ("Q", Some(0), None, true), ("b", None, None, false)]);
        let edited = para(&[("a", None, None, false), ("Z", Some(1), None, true), ("b", None, None, false)]);
        let steps = planned(&original, &doc(&[ANA]), &[&edited], &doc(&[ANA, SARI])).unwrap();
        assert_eq!(steps, [keep(0..1, Change::Same), vec![Step::Remove { unit: 1 }, typed("Z", SARI, Some(1))], keep(2..3, Change::Same)].concat());
    }

    #[test]
    fn continues_an_earlier_half_before_the_mark() {
        // M3 (P07): Enter at the end, then typing: the engine continued the last character.
        let original = para(&[("Rent.", None, None, false)]);
        let mut head = original.clone();
        head.mark.ins = Some(0);
        let tail = para(&[("New", Some(0), None, false)]);
        let steps = planned(&original, &doc(&[]), &[&head, &tail], &doc(&[SARI])).unwrap();
        assert_eq!(steps, [keep(0..5, Change::Same), vec![Step::Break { revision: key(SARI) }, typed("New", SARI, Some(4))]].concat());
        // A pasted middle paragraph took the caret's formatting, which the mark has too.
        let original = para(&[("xy", None, None, false)]);
        let mut head = para(&[("x", None, None, false), ("A", Some(0), None, false)]);
        head.mark.ins = Some(0);
        let mut middle = para(&[("B", Some(0), None, false)]);
        middle.mark.ins = Some(0);
        let tail = para(&[("C", Some(0), None, false), ("y", None, None, false)]);
        let steps = planned(&original, &doc(&[]), &[&head, &middle, &tail], &doc(&[SARI])).unwrap();
        assert_eq!(steps[3], typed("B", SARI, Some(0)));
    }

    #[test]
    fn refuses_a_mark_formatted_otherwise_than_the_sources() {
        // I4 (P12): bold applied to an empty paragraph's mark (the desktop formats the mark there).
        let original = Paragraph::new();
        let mut bolder = original.clone();
        bolder.mark.bold = Some(true);
        assert_eq!(planned(&original, &doc(&[]), &[&bolder], &doc(&[])), Err(Unpreservable::Formatting));
        // A half's mark: each half gets the source's mark in the file.
        let original = para(&[("One. Two.", None, None, false)]);
        let mut first = para(&[("One.", None, None, false)]);
        first.mark.ins = Some(0);
        let mut second = para(&[(" Two.", None, None, false)]);
        second.mark.italic = Some(true);
        assert_eq!(planned(&original, &doc(&[]), &[&first, &second], &doc(&[SARI])), Err(Unpreservable::Formatting));
    }

    #[test]
    fn refuses_a_mark_that_carries_typing_deleted_from_an_empty_paragraph() {
        // I3 (P11): typing into an empty paragraph and deleting it leaves the session's insertion on
        // the mark (`Paragraph::delete` copies the character's formatting into it). Refused until the
        // engine stops copying tracked changes into a mark (Task 10).
        let original = Paragraph::new();
        let mut edited = original.clone();
        edited.insert_text(0, "x", &CharProps { ins: Some(0), ..CharProps::default() }).unwrap();
        edited.delete(0, 1).unwrap();
        assert_eq!(edited.mark.ins, Some(0));
        assert_eq!(planned(&original, &doc(&[]), &[&edited], &doc(&[SARI])), Err(Unpreservable::Structure));
    }

    #[test]
    fn plans_two_thousand_decided_paragraphs_in_one_saves_work() {
        // I2: decisions cost no diff work. 2,000 paragraphs of 600 characters each, accepted, then
        // rejected (an insertion, and a deletion), each under one save's budget; every plan's steps
        // stay charged, as a save holds them.
        let text = "abcdefghijklmnopqrstuvwxy ".repeat(24);
        let plain = para(&[(&text, None, None, false)]);
        let inserted = para(&[(&text, Some(0), None, false)]);
        let deleted = para(&[(&text, None, Some(0), false)]);
        let empty = Paragraph::new();
        let (before, after) = as_read(&doc(&[ANA]), &doc(&[ANA]));
        for (name, original, edited) in
            [("accepted", &inserted, &plain), ("rejected insertion", &inserted, &empty), ("rejected deletion", &deleted, &plain)]
        {
            let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
            for _ in 0..2_000 {
                let steps = plan(original, &before, &[edited], &after, &mut budget);
                assert!(steps.is_ok(), "{name}: {steps:?} after {} work", MAX_WORK - budget.work_left());
            }
            assert!(MAX_WORK - budget.work_left() < MAX_WORK / 4, "{name}: {}", MAX_WORK - budget.work_left());
        }
    }

    #[test]
    fn keeps_formats_apart_by_their_numbers_too() {
        // M5 (P13): formats differing only in size hash apart, so 10,000 of them cost little.
        let mut original = Paragraph::new();
        for index in 0..10_000u16 {
            let off = original.len();
            original.insert_text(off, "a", &CharProps { size: Some(8.0 + f32::from(index) * 0.5), ..CharProps::default() }).unwrap();
        }
        let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
        assert!(plan(&original, &doc(&[]), &[&original], &doc(&[]), &mut budget).is_ok());
        assert!(MAX_WORK - budget.work_left() < 500_000, "{}", MAX_WORK - budget.work_left());
        // Equal formats hash alike, -0.0 and 0.0 included.
        let zero = |size: f32| CharProps { size: Some(size), spacing: Some(size), ..CharProps::default() };
        assert_eq!(format_hash(&zero(0.0)), format_hash(&zero(-0.0)));
        assert_ne!(format_hash(&zero(0.0)), format_hash(&zero(1.0)));
    }

    #[test]
    fn charges_a_diff_its_steps_exactly() {
        let (before, after) = (doc(&[]), doc(&[]));
        let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
        let mut planner = Planner::new(&before, &after, &mut budget);
        let (left, right) = (b"abcdef", b"abXdf");
        let (steps, held) = planner.diff(left.len(), right.len(), &|i, j| left.get(i) == right.get(j)).unwrap();
        assert!(!steps.is_empty());
        assert_eq!(held, script_bytes(left.len(), right.len(), MAX_CHANGES));
        assert_eq!(planner.ledger.held, held);
        planner.ledger.release(held);
        assert_eq!(planner.ledger.held, 0);
    }

    #[test]
    fn charges_a_format_its_room_exactly() {
        let (before, after) = (doc(&[]), doc(&[]));
        let mut budget = Budget::new(MAX_WORK, MAX_MEMORY);
        let mut planner = Planner::new(&before, &after, &mut budget);
        let font = CharProps { font: Some("Serif".into()), ins: Some(3), ..CharProps::default() };
        let kept = size::<CharProps>() + 5;
        let slot = vector_slot::<CharProps>() + vector_slot::<u32>() + table_slot::<(u64, u32)>();
        let room = vector_room::<CharProps>() + vector_room::<u32>() + table_room::<(u64, u32)>();
        assert_eq!(planner.format(&font).unwrap(), 0);
        assert_eq!(planner.ledger.held, kept + room + slot);
        // The same format without its tracked change: found, nothing more held.
        assert_eq!(planner.format(&CharProps { ins: None, ..font.clone() }).unwrap(), 0);
        assert_eq!(planner.ledger.held, kept + room + slot);
        assert_eq!(planner.format(&CharProps::default()).unwrap(), 1);
        assert_eq!(planner.ledger.held, kept + room + 2 * slot + size::<CharProps>());
    }
}

//! embed: a story of the edited package, with the save's own tracked changes undone, against the
//! same story of the base. What differs is either a base change resolved, or a fact the host
//! refuses on: content changed without tracking, a base change altered, an element lost.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{BuildHasher, Hasher};

use super::LossKind;
use super::diff::{Op, Work, script};
use super::story::{FormatRev, Item, MARK_BASE, Tables, Wrap};

/// The edited story with this save's own changes undone: its insertions dropped, its deletions and
/// formatting changes unmarked (formatting back to what it was before), the anchors of its new
/// comments dropped.
pub(super) struct Projector<'a> {
    pub tables: &'a mut Tables,
    pub new_pairs: &'a BTreeSet<(String, String)>,
    pub new_comments: &'a BTreeSet<String>,
    wraps: HashMap<u32, Option<u32>>,
    formats: HashMap<u32, u32>,
}

impl<'a> Projector<'a> {
    pub(super) fn new(tables: &'a mut Tables, new_pairs: &'a BTreeSet<(String, String)>, new_comments: &'a BTreeSet<String>) -> Self {
        Projector { tables, new_pairs, new_comments, wraps: HashMap::new(), formats: HashMap::new() }
    }

    fn is_new(&self, author: &str, date: &str) -> bool {
        self.new_pairs.contains(&(author.to_string(), date.to_string()))
    }

    fn wraps(&mut self, id: u32) -> Option<u32> {
        if let Some(cached) = self.wraps.get(&id) {
            return *cached;
        }
        let list = self.tables.wraps_of(id).to_vec();
        let projected = if list.iter().any(|wrap| wrap.kind.inserts() && self.is_new(&wrap.author, &wrap.date)) {
            None
        } else {
            let kept: Vec<Wrap> = list.into_iter().filter(|wrap| !self.is_new(&wrap.author, &wrap.date)).collect();
            Some(self.tables.wrap_list(kept))
        };
        self.wraps.insert(id, projected);
        projected
    }

    fn format(&mut self, id: u32) -> u32 {
        if let Some(cached) = self.formats.get(&id) {
            return *cached;
        }
        let projected = match self.tables.format_of(id).cloned() {
            Some(mut format) => {
                for slot in &mut format.slots {
                    if let Some(rev) = slot.rev.take() {
                        if self.is_new(&rev.author, &rev.date) {
                            slot.value = rev.old;
                        } else {
                            slot.rev = Some(rev);
                        }
                    }
                }
                format.slots.retain(|slot| slot.value != super::story::EMPTY || slot.rev.is_some());
                self.tables.format(format)
            }
            None => id,
        };
        self.formats.insert(id, projected);
        projected
    }

    pub(super) fn project(&mut self, items: &[Item]) -> Vec<Item> {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            if let Some(comment) = self.tables.mark_info(item.piece).and_then(|mark| mark.comment.as_ref())
                && self.new_comments.contains(comment)
            {
                continue;
            }
            let Some(wraps) = self.wraps(item.wraps) else { continue };
            let format = self.format(item.format);
            out.push(Item { piece: item.piece, wraps, format });
        }
        out
    }
}

/// A base change accepted or rejected, told apart by its kind, author, date and attributes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Resolution {
    Wrap(Wrap),
    Format(&'static str, FormatRev),
}

/// What the differences mean, summed over every story.
#[derive(Default)]
pub(super) struct Tally {
    pub untracked: u32,
    pub altered: u32,
    pub accepted: BTreeSet<Resolution>,
    pub rejected: BTreeSet<Resolution>,
    pub losses: BTreeMap<(String, LossKind, Option<String>), u32>,
    pub comments_altered: BTreeSet<String>,
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

impl Story<'_> {
    /// The base comment a mark anchors, if it is a comment anchor.
    fn comment(&self, piece: u32) -> Option<&str> {
        self.tables.mark_info(piece).and_then(|mark| mark.comment.as_deref())
    }

    /// The same piece in both: its wrappers and formatting say what happened to it.
    fn same(&mut self, before: &Item, after: &Item) {
        if before == after {
            return;
        }
        let comment = self.comment(before.piece).filter(|id| self.comments.base.contains(*id)).map(str::to_string);
        let mut untracked = false;
        let mut altered = false;
        if before.wraps != after.wraps {
            // Kept with fewer wrappers: each one gone was resolved (an insertion accepted, a
            // deletion rejected). Any other change of wrappers alters a base change.
            let before_wraps = self.tables.wraps_of(before.wraps);
            let after_wraps = self.tables.wraps_of(after.wraps);
            let mut remaining = before_wraps.iter();
            let mut gone: Vec<&Wrap> = Vec::new();
            let mut kept_in_order = true;
            for wrap in after_wraps {
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
                    let resolution = Resolution::Wrap(wrap.clone());
                    if wrap.kind.inserts() {
                        self.tally.accepted.insert(resolution);
                    } else {
                        self.tally.rejected.insert(resolution);
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
                    (Some(rev), None) if after_value == before_value => {
                        self.tally.accepted.insert(Resolution::Format(name, rev.clone()));
                    }
                    (Some(rev), None) if after_value == rev.old => {
                        self.tally.rejected.insert(Resolution::Format(name, rev.clone()));
                    }
                    _ if before_value != after_value => untracked = true,
                    _ => altered = true,
                }
            }
        }
        match comment {
            Some(id) if untracked || altered => {
                self.tally.comments_altered.insert(id);
            }
            _ => {
                if untracked {
                    self.tally.untracked = self.tally.untracked.saturating_add(1);
                }
                if altered {
                    self.tally.altered = self.tally.altered.saturating_add(1);
                }
            }
        }
    }

    /// Base content the edited story no longer has.
    fn deleted(&mut self, before: &Item) {
        if let Some(id) = self.comment(before.piece) {
            if !self.comments.removed.contains(id) {
                self.tally.comments_altered.insert(id.to_string());
            }
            return;
        }
        match self.tables.wraps_of(before.wraps).last() {
            // Gone with its change: an insertion rejected, a deletion accepted.
            Some(wrap) if wrap.kind.inserts() => {
                self.tally.rejected.insert(Resolution::Wrap(wrap.clone()));
            }
            Some(wrap) => {
                self.tally.accepted.insert(Resolution::Wrap(wrap.clone()));
            }
            None => match self.tables.mark_info(before.piece) {
                Some(mark) => {
                    let key = (self.part.to_string(), LossKind::ElementMissing, Some(mark.element.to_string()));
                    let count = self.tally.losses.entry(key).or_default();
                    *count = count.saturating_add(1);
                }
                None => self.tally.untracked = self.tally.untracked.saturating_add(1),
            },
        }
    }

    /// Content the base does not have, after this save's own changes were undone.
    fn inserted(&mut self, after: &Item) {
        if let Some(id) = self.comment(after.piece)
            && self.comments.base.contains(id)
        {
            self.tally.comments_altered.insert(id.to_string());
            return;
        }
        if self.tables.wraps_of(after.wraps).is_empty() {
            self.tally.untracked = self.tally.untracked.saturating_add(1);
        } else {
            self.tally.altered = self.tally.altered.saturating_add(1);
        }
    }
}

/// A word (characters with the same changes and formatting, up to a white space), or a mark on its
/// own: `items[start..start + len]`. Words are matched whole, changes and formatting included;
/// what lies between matched words is then matched character by character, by the character alone,
/// so a resolved change or a formatting difference is seen as the same character kept.
struct Token {
    start: usize,
    len: usize,
    hash: u64,
}

fn tokens(items: &[Item], hasher: &RandomState, work: &mut Work) -> Option<Vec<Token>> {
    let mut out = Vec::new();
    let mut start = 0;
    let close = |out: &mut Vec<Token>, start: usize, end: usize| {
        if end > start {
            let mut state = hasher.build_hasher();
            for item in items.get(start..end).unwrap_or(&[]) {
                state.write_u32(item.piece);
                state.write_u32(item.wraps);
                state.write_u32(item.format);
            }
            out.push(Token { start, len: end - start, hash: state.finish() });
        }
    };
    for (index, item) in items.iter().enumerate() {
        if !work.spend(1) {
            return None;
        }
        // A word ends where its changes or formatting do, so a word matched whole is equal whole.
        if index > start && items.get(index - 1).is_some_and(|previous| (previous.wraps, previous.format) != (item.wraps, item.format)) {
            close(&mut out, start, index);
            start = index;
        }
        if item.piece >= MARK_BASE {
            close(&mut out, start, index);
            close(&mut out, index, index + 1);
            start = index + 1;
        } else if char::from_u32(item.piece).is_some_and(char::is_whitespace) {
            close(&mut out, start, index + 1);
            start = index + 1;
        }
    }
    close(&mut out, start, items.len());
    Some(out)
}

fn pieces<'a>(items: &'a [Item], token: &Token) -> &'a [Item] {
    items.get(token.start..token.start.saturating_add(token.len)).unwrap_or(&[])
}

/// Compare one story. False when a bound stopped it (`max_changes` or the work budget).
pub(super) fn compare_story(
    part: &str,
    base: &[Item],
    edited: &[Item],
    tables: &Tables,
    comments: &CommentIds<'_>,
    max_changes: usize,
    work: &mut Work,
    tally: &mut Tally,
) -> bool {
    let mut story = Story { tables, comments, part, tally };
    let hasher = RandomState::new();
    let (Some(base_tokens), Some(edited_tokens)) = (tokens(base, &hasher, work), tokens(edited, &hasher, work)) else { return false };
    let same_token = |i: usize, j: usize| match (base_tokens.get(i), edited_tokens.get(j)) {
        (Some(a), Some(b)) => a.hash == b.hash && pieces(base, a) == pieces(edited, b),
        _ => false,
    };
    let Some(ops) = script(base_tokens.len(), edited_tokens.len(), &same_token, max_changes, work) else { return false };
    // Runs of deleted and inserted tokens between matched ones, compared character by character.
    let mut deleted: Option<(usize, usize)> = None;
    let mut inserted: Option<(usize, usize)> = None;
    let item_range = |tokens: &[Token], range: Option<(usize, usize)>| -> (usize, usize) {
        match range.and_then(|(first, last)| Some((tokens.get(first)?, tokens.get(last)?))) {
            Some((first, last)) => (first.start, last.start.saturating_add(last.len)),
            None => (0, 0),
        }
    };
    for op in ops.into_iter().map(Some).chain(std::iter::once(None)) {
        match op {
            Some(Op::Delete(index)) => deleted = Some(deleted.map_or((index, index), |(first, _)| (first, index))),
            Some(Op::Insert(index)) => inserted = Some(inserted.map_or((index, index), |(first, _)| (first, index))),
            other => {
                let (from, to) = item_range(&base_tokens, deleted.take());
                let (edited_from, edited_to) = item_range(&edited_tokens, inserted.take());
                if !compare_stretch(
                    base.get(from..to).unwrap_or(&[]),
                    edited.get(edited_from..edited_to).unwrap_or(&[]),
                    max_changes,
                    work,
                    &mut story,
                ) {
                    return false;
                }
                if let Some(Op::Same(left, right)) = other {
                    let (Some(before), Some(after)) = (base_tokens.get(left), edited_tokens.get(right)) else { return false };
                    for (old, new) in pieces(base, before).iter().zip(pieces(edited, after)) {
                        story.same(old, new);
                    }
                }
            }
        }
    }
    true
}

fn compare_stretch(base: &[Item], edited: &[Item], max_changes: usize, work: &mut Work, story: &mut Story<'_>) -> bool {
    if base.is_empty() && edited.is_empty() {
        return true;
    }
    let same = |i: usize, j: usize| matches!((base.get(i), edited.get(j)), (Some(a), Some(b)) if a.piece == b.piece);
    let Some(ops) = script(base.len(), edited.len(), &same, max_changes, work) else { return false };
    for op in ops {
        match op {
            Op::Same(left, right) => {
                if let (Some(before), Some(after)) = (base.get(left), edited.get(right)) {
                    story.same(before, after);
                }
            }
            Op::Delete(left) => {
                if let Some(before) = base.get(left) {
                    story.deleted(before);
                }
            }
            Op::Insert(right) => {
                if let Some(after) = edited.get(right) {
                    story.inserted(after);
                }
            }
        }
    }
    true
}

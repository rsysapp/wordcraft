//! embed: each structure an inspection keeps, built on its own, for tests that check (with a
//! counting allocator) that what it holds never passes what it charged. Each probe prepares what
//! the structure is built from, calls `mark` (the test starts counting there), builds the
//! structure, and answers the most it charged while building, keeping the structure alive until
//! the answer is dropped. Not part of the inspection.

use std::any::Any;
use std::collections::BTreeSet;

use super::budget::Budget;
use super::comments;
use super::compare::{CommentIds, Pairs, Projector, Tally, assign_contexts, compare_story, facts, field_structure};
use super::digest::Hasher;
use super::opc::{Canon, ContentTypes, Rels};
use super::package::{StrictPackage, same_bytes};
use super::story::{self, Context, Format, Holds, MarkInfo, MarkKind, RevisionKind, Revisions, Slot, StyleHiding, Tables, Wrap, WrapKind};
use super::xml::{self, Limits, Names};
use super::{INSPECT_LIMITS, MAX_CHANGES, inspect_budgeted};

/// What a probe charged, with what it built.
pub struct Measured {
    pub charged: u64,
    _keep: Box<dyn Any>,
}

fn limits() -> Limits {
    Limits { elements_left: INSPECT_LIMITS.max_elements, max_depth: INSPECT_LIMITS.max_depth }
}

fn unlimited() -> Budget {
    Budget::new(u64::MAX, u64::MAX)
}

/// Run `build` after `mark`, answering the most it charged above what was held before it.
fn measure(budget: &mut Budget, mark: &mut dyn FnMut(), build: impl FnOnce(&mut Budget) -> Result<Box<dyn Any>, String>) -> Result<Measured, String> {
    let before = budget.held();
    budget.reset_peak();
    mark();
    let keep = build(budget)?;
    Ok(Measured { charged: budget.peak().saturating_sub(before), _keep: keep })
}

/// A parsed tree: elements, attributes, text, names, namespaces, prefix lists.
pub fn tree(xml: &[u8], mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let mut budget = unlimited();
    measure(&mut budget, mark, |budget| {
        let mut names = Names::default();
        let parsed = xml::parse(xml, &mut names, &mut limits(), budget)?;
        Ok(Box::new((parsed.root, names)))
    })
}

fn parsed(xml: &[u8]) -> Result<(super::xml::XEl, Names), String> {
    let mut names = Names::default();
    let parsed = xml::parse(xml, &mut names, &mut limits(), &mut unlimited())?;
    Ok((parsed.root, names))
}

/// A part's relationships.
pub fn relationships(xml: &[u8], mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let (root, _names) = parsed(xml)?;
    measure(&mut unlimited(), mark, |budget| Ok(Box::new(Rels::parse(&root, budget)?)))
}

/// A package's content types.
pub fn content_types(xml: &[u8], mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let (root, _names) = parsed(xml)?;
    measure(&mut unlimited(), mark, |budget| Ok(Box::new(ContentTypes::parse(&root, budget)?)))
}

/// A comments part's comments.
pub fn comments(xml: &[u8], mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let (root, _names) = parsed(xml)?;
    let rels = Rels::default();
    let canon = Canon { rels: &rels, bookmarks: None };
    measure(&mut unlimited(), mark, |budget| {
        budget.hold(super::opc::HASHING_BYTES)?;
        Ok(Box::new(comments::read(&root, None, &canon, budget)?))
    })
}

/// What a styles part's styles hide.
pub fn styles(xml: &[u8], mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let (root, _names) = parsed(xml)?;
    measure(&mut unlimited(), mark, |budget| Ok(Box::new(StyleHiding::read(&root, budget)?)))
}

/// Distinct authors and dates of tracked changes.
pub fn revision_pairs(count: u32, mark: &mut dyn FnMut()) -> Result<Measured, String> {
    measure(&mut unlimited(), mark, |budget| {
        let mut revisions = Revisions::default();
        for date in 0..count {
            revisions.note(0, date, RevisionKind::Insertion, budget)?;
        }
        Ok(Box::new(revisions))
    })
}

/// Which of the tables a probe fills.
pub enum Entries {
    /// Content identities of this many bytes each (up to 256 kept exact).
    Contents(usize),
    /// Texts of this many bytes each.
    Texts(usize),
    /// Tracked-change stacks, each one wrapper deeper.
    Stacks,
    /// Formats of this many slots each.
    Formats(usize),
    /// Marks of identities this many bytes long.
    Marks(usize),
    /// Comment anchors whose ids are this many bytes long.
    Anchors(usize),
    Contexts,
}

/// `count` distinct entries of one table.
pub fn table_entries(entries: Entries, count: u32, mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let mut tables = Tables::new();
    let mut budget = unlimited();
    tables.charge(&mut budget)?;
    measure(&mut budget, mark, |budget| {
        for n in 0..count {
            match &entries {
                Entries::Contents(size) => {
                    let mut hasher = Hasher::default();
                    hasher.update(&n.to_le_bytes());
                    hasher.update(&vec![b'c'; size.saturating_sub(4)]);
                    tables.content(hasher.finish());
                }
                Entries::Texts(size) => {
                    tables.text(&format!("{n:0size$}"));
                }
                Entries::Stacks => {
                    let wrap = Wrap { kind: WrapKind::Ins, author: 0, date: n, extra: 0 };
                    tables.push_stack(0, wrap);
                }
                Entries::Formats(slots) => {
                    let slots = (0..*slots)
                        .map(|slot| Slot {
                            name: if slot % 2 == 0 { "rPr" } else { "pPr" },
                            value: n.saturating_add(u32::try_from(slot).unwrap_or(0)),
                            rev: None,
                        })
                        .collect();
                    tables.format(Format { slots, hidden: false, instr: false, hidden_by: None });
                }
                Entries::Marks(size) => {
                    let mut hasher = Hasher::default();
                    hasher.update(&n.to_le_bytes());
                    hasher.update(&vec![b'm'; size.saturating_sub(4)]);
                    tables.mark(hasher.finish(), || MarkInfo {
                        element: "w:x".into(),
                        comment: None,
                        kind: MarkKind::Plain,
                        holds: Holds::default(),
                    })?;
                }
                Entries::Anchors(size) => {
                    let id = format!("{n:0size$}");
                    let mut hasher = Hasher::default();
                    hasher.update(&n.to_le_bytes());
                    let info =
                        || MarkInfo { element: "w:commentReference".into(), comment: Some(id), kind: MarkKind::Plain, holds: Holds::default() };
                    tables.mark(hasher.finish(), info)?;
                }
                Entries::Contexts => {
                    tables.context(Context { field: n, ..Context::default() });
                }
            }
            tables.charge(budget)?;
        }
        Ok(Box::new(tables))
    })
}

/// A story's items (and what its walk interns).
pub fn story(xml: &[u8], mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let mut names = Names::default();
    let parsed = xml::parse(xml, &mut names, &mut limits(), &mut unlimited())?;
    let rels = Rels::default();
    let bookmarks = super::opc::bookmarks(&parsed.root);
    let canon = Canon { rels: &rels, bookmarks: Some(&bookmarks) };
    let hiding = StyleHiding::default();
    let mut tables = Tables::new();
    let mut budget = unlimited();
    tables.charge(&mut budget)?;
    measure(&mut budget, mark, |budget| {
        budget.hold(super::opc::HASHING_BYTES)?;
        let mut revisions = Revisions::default();
        let mut items_left = super::MAX_STORY_ITEMS;
        let items = story::walk(&parsed.root, &canon, &mut tables, &mut revisions, &mut items_left, parsed.most_items, &hiding, budget)?;
        Ok(Box::new((items, tables, revisions)))
    })
}

/// A story walked, ready for the comparison's later steps.
struct Walked {
    items: Vec<story::Item>,
    tables: Tables,
    revisions: Revisions,
    budget: Budget,
}

fn walked(xml: &[u8], tables: Option<Tables>) -> Result<Walked, String> {
    let mut names = Names::default();
    let mut budget = unlimited();
    let parsed = xml::parse(xml, &mut names, &mut limits(), &mut budget)?;
    let rels = Rels::default();
    let bookmarks = super::opc::bookmarks(&parsed.root);
    let canon = Canon { rels: &rels, bookmarks: Some(&bookmarks) };
    let mut tables = tables.unwrap_or_else(Tables::new);
    let mut revisions = Revisions::default();
    let mut items_left = super::MAX_STORY_ITEMS;
    let items =
        story::walk(&parsed.root, &canon, &mut tables, &mut revisions, &mut items_left, parsed.most_items, &StyleHiding::default(), &mut budget)?;
    Ok(Walked { items, tables, revisions, budget })
}

/// The contexts of a story's items.
pub fn contexts(xml: &[u8], mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let Walked { mut items, mut tables, mut budget, .. } = walked(xml, None)?;
    measure(&mut budget, mark, |budget| {
        assign_contexts(&mut items, &mut tables, &mut |_, format| format, budget)?;
        Ok(Box::new((items, tables)))
    })
}

/// A story projected with every tracked change taken as the save's own (`new`), or as the base's.
pub fn projection(xml: &[u8], new: bool, mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let Walked { mut items, mut tables, revisions, mut budget } = walked(xml, None)?;
    let pairs: Pairs = if new { revisions.by_pair.keys().copied().collect() } else { Pairs::new() };
    let comments = BTreeSet::new();
    measure(&mut budget, mark, |budget| {
        let mut projector = Projector::new(&pairs, &comments);
        assign_contexts(&mut items, &mut tables, &mut |tables, format| projector.format(tables, format), budget)?;
        projector.charge(budget)?;
        projector.project(&mut tables, &mut items, budget)?;
        Ok(Box::new((items, tables)))
    })
}

/// The facts about a story's tracked changes, all taken as the save's own.
pub fn facts_of(xml: &[u8], mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let Walked { items, tables, revisions, mut budget } = walked(xml, None)?;
    let pairs: Pairs = revisions.by_pair.keys().copied().collect();
    measure(&mut budget, mark, |budget| {
        let mut holds = std::collections::BTreeMap::new();
        facts(&items, &tables, &pairs, &mut holds, budget)?;
        Ok(Box::new((items, tables, holds)))
    })
}

/// The base fields of a story and the save's changes to them, all changes taken as the save's own.
pub fn fields_of(xml: &[u8], mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let Walked { items, tables, revisions, mut budget } = walked(xml, None)?;
    let pairs: Pairs = revisions.by_pair.keys().copied().collect();
    measure(&mut budget, mark, |budget| {
        let mut holds = std::collections::BTreeMap::new();
        field_structure(&items, &tables, &pairs, &mut holds, budget)?;
        Ok(Box::new((items, tables, holds)))
    })
}

/// Two stories compared (tokens, the edit script, the tally), both read with nothing projected.
pub fn comparison(base: &[u8], edited: &[u8], mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let first = walked(base, None)?;
    let second = walked(edited, Some(first.tables))?;
    let Walked { items: after, tables, mut budget, .. } = second;
    let before = first.items;
    let base_ids = BTreeSet::new();
    let removed = BTreeSet::new();
    let ids = CommentIds { base: &base_ids, removed: &removed };
    measure(&mut budget, mark, |budget| {
        let mut tally = Tally::default();
        compare_story("word/document.xml", &before, &after, &tables, &ids, MAX_CHANGES, budget, &mut tally);
        budget.hold(std::mem::take(&mut tally.pending))?;
        Ok(Box::new((before, after, tables, tally)))
    })
}

/// A zip package opened strictly (its directory, layout and index).
pub fn package(zip: &[u8], mark: &mut dyn FnMut()) -> Result<Measured, String> {
    measure(&mut unlimited(), mark, |budget| {
        let package = StrictPackage::open(zip, &INSPECT_LIMITS, budget)?;
        // The package borrows the bytes; keep what it holds by its index only.
        let names: Vec<String> = package.names().map(str::to_string).collect();
        drop(package);
        Ok(Box::new(names))
    })
}

/// One part read from a package.
pub fn read_part(zip: &[u8], name: &str, mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let mut budget = unlimited();
    let mut package = StrictPackage::open(zip, &INSPECT_LIMITS, &mut budget)?;
    measure(&mut budget, mark, |budget| Ok(Box::new(package.read(name, budget)?)))
}

/// One part compared between two packages, read side by side.
pub fn compare_part(base: &[u8], edited: &[u8], name: &str, mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let mut budget = unlimited();
    let mut first = StrictPackage::open(base, &INSPECT_LIMITS, &mut budget)?;
    let mut second = StrictPackage::open(edited, &INSPECT_LIMITS, &mut budget)?;
    measure(&mut budget, mark, |budget| Ok(Box::new(same_bytes(&mut first, &mut second, name, budget).map_err(|error| format!("{error:?}"))?)))
}

/// A whole inspection.
pub fn inspection(base: &[u8], edited: &[u8], mark: &mut dyn FnMut()) -> Result<Measured, String> {
    measure(&mut unlimited(), mark, |budget| {
        Ok(Box::new(inspect_budgeted(base, edited, &INSPECT_LIMITS, MAX_CHANGES, budget).map_err(|error| format!("{error:?}"))?))
    })
}

/// An edit script on its own: `edits` elements of each of two sequences differ, and the script may
/// make no more edits than it needs, so what it charges is no more than it can use.
pub fn edit_script(edits: usize, mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let left: Vec<usize> = (0..edits).collect();
    let right: Vec<usize> = (edits..2 * edits).collect();
    let same = |i: usize, j: usize| left.get(i) == right.get(j);
    let max_changes = 2 * edits;
    measure(&mut unlimited(), mark, |budget| {
        budget.hold(super::diff::script_bytes(left.len(), right.len(), max_changes))?;
        let steps = super::diff::script(left.len(), right.len(), &same, max_changes, budget).ok_or("no script")?;
        Ok(Box::new(steps))
    })
}

/// One tally over many stories: each pair (base, edited) compared in turn, as the parts of one
/// package are. The stories are walked with one set of tables and revisions, before counting.
pub fn tally(pairs: &[(Vec<u8>, Vec<u8>)], mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let mut tables = Tables::new();
    let mut revisions = Revisions::default();
    let mut budget = unlimited();
    let mut walked = Vec::new();
    for (base, edited) in pairs {
        let side = |xml: &[u8], tables: &mut Tables, revisions: &mut Revisions, budget: &mut Budget| -> Result<Vec<story::Item>, String> {
            let mut names = Names::default();
            let parsed = xml::parse(xml, &mut names, &mut limits(), budget)?;
            let rels = Rels::default();
            let bookmarks = super::opc::bookmarks(&parsed.root);
            let canon = Canon { rels: &rels, bookmarks: Some(&bookmarks) };
            let mut items_left = super::MAX_STORY_ITEMS;
            let items = story::walk(&parsed.root, &canon, tables, revisions, &mut items_left, parsed.most_items, &StyleHiding::default(), budget)?;
            budget.release(parsed.held);
            Ok(items)
        };
        let before = side(base, &mut tables, &mut revisions, &mut budget)?;
        let after = side(edited, &mut tables, &mut revisions, &mut budget)?;
        walked.push((before, after));
    }
    tables.charge(&mut budget)?;
    let base_ids = BTreeSet::new();
    let removed = BTreeSet::new();
    let ids = CommentIds { base: &base_ids, removed: &removed };
    measure(&mut budget, mark, |budget| {
        let mut tally = Tally::default();
        for (before, after) in &walked {
            if !compare_story("word/document.xml", before, after, &tables, &ids, MAX_CHANGES, budget, &mut tally) {
                return Err("a story over its bounds".into());
            }
            budget.hold(std::mem::take(&mut tally.pending))?;
        }
        Ok(Box::new((walked, tables, tally)))
    })
}

/// `count` parts kept by a side, each an XML part with its compared form.
pub fn parts_kept(count: u32, mark: &mut dyn FnMut()) -> Result<Measured, String> {
    measure(&mut unlimited(), mark, |budget| {
        let mut kept = super::Kept::default();
        for n in 0..count {
            let mut hasher = Hasher::default();
            hasher.update(&n.to_le_bytes());
            kept.keep(&format!("word/part{n}.xml"), super::Part::Xml(hasher.finish()), budget)?;
        }
        Ok(Box::new(kept))
    })
}

/// A package's part names, copied to be read in turn.
pub fn part_names(zip: &[u8], mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let mut budget = unlimited();
    let package = StrictPackage::open(zip, &INSPECT_LIMITS, &mut budget)?;
    measure(&mut budget, mark, |budget| Ok(Box::new(super::part_names(&package, budget)?.0)))
}

/// A step of the comparison of two packages.
pub enum Phase {
    /// Parts added, missing and altered, and their content types.
    Parts,
    /// Relationships added and removed.
    Relationships,
    /// Comments added, removed and altered.
    Comments,
    /// The save's own revisions, by author.
    Revisions,
}

/// One step of comparing two packages, both read first.
pub fn compared(base: &[u8], edited: &[u8], phase: Phase, mark: &mut dyn FnMut()) -> Result<Measured, String> {
    let mut budget = unlimited();
    let mut names = Names::default();
    let mut tables = Tables::new();
    let mut base = super::Side::read(base, &INSPECT_LIMITS, &mut names, &mut tables, &mut budget)?;
    let mut edited = super::Side::read(edited, &INSPECT_LIMITS, &mut names, &mut tables, &mut budget)?;
    let error = |error: super::InspectError| format!("{error:?}");
    measure(&mut budget, mark, |budget| {
        let mut inspection = super::Inspection::default();
        let mut tally = Tally::default();
        let kept: Box<dyn Any> = match phase {
            Phase::Parts => Box::new(super::compare_parts(&mut base, &mut edited, &mut inspection, &mut tally, budget).map_err(error)?),
            Phase::Relationships => Box::new(super::compare_relationships(&base, &edited, &mut inspection, budget).map_err(error)?),
            Phase::Comments => {
                let sets = super::compare_comments(&base, &edited, &mut tally, budget).map_err(error)?;
                Box::new((sets.base_ids, sets.removed, sets.new_comments, sets.added, sets.replies))
            }
            Phase::Revisions => Box::new(super::new_revisions(&base, &edited, &tables, budget).map_err(error)?),
        };
        Ok(Box::new((kept, inspection, tally)))
    })
}

//! embed: compare a saved package with the package it was edited from. A host checks every save
//! from the embed build with this. Facts only; what is acceptable is the host's rule.
//!
//! **Every fact is certain, or the call fails.** Whatever this module cannot read the way any
//! other reader would (a damaged or ambiguous zip, XML that is not plainly well-formed, a part
//! over a limit, a tracked change around content it does not know) is an [`InspectError`], never a
//! skipped part. A host refuses a save on any error, and on any fact its rule does not allow.
//!
//! How it compares. Both packages are read raw (not through WordCraft's document model, which
//! would hide what the reader drops):
//! - **Stories** (the main document, headers, footers, footnotes, endnotes; known by content
//!   type) become streams of characters and marks, each with its tracked-change wrappers, its
//!   formatting and its context: the fields it is code or result of (with their instructions), and
//!   the properties of the paragraph and section a reader shows it in, worked out on the stream as
//!   saved. Revisions are known by author and date; those the base lacks are the save's own. The
//!   edited stream with the save's own changes undone (its insertions dropped, its deletions
//!   unmarked, its formatting changes reverted, its new comments' empty anchors dropped) must equal
//!   the base, context included, except where a base change was accepted or rejected. So content the
//!   save inserts can still be caught changing how base content reads: field characters around it,
//!   a paragraph mark or section break governing it. A word-level diff, then a character-level diff
//!   of each stretch that differs (Myers, each bounded by `max_changes`), finds what differs.
//! - **Comments** are compared by meaning: added, replies, removed, altered.
//! - **Relationships** are compared as sets of (type, target, mode) per part, and a relationship id
//!   inside XML reads as what it points at, so renumbering is not a change and retargeting is.
//! - **Content types** are compared by the type each part gets.
//! - **Other XML parts** are compared by the SHA-256 of a canonical form, produced as it is hashed
//!   (nothing printed or kept); binaries byte for byte, read side by side.
//!
//! Every step is bounded: elements, depth and part sizes by [`INSPECT_LIMITS`], story text by
//! [`MAX_STORY_ITEMS`], every diff by `max_changes`, all reading, hashing, walking and comparing by
//! [`MAX_WORK`], and what the inspection holds by [`memory_limit`] (charged before it is allocated).
//!
//! Bookkeeping that carries no content is ignored: `w:rsid*`, `w14:paraId`, `w14:textId` and
//! `mc:Ignorable` attributes, a tracked change's `w:id`, a bookmark's id (its end reads as its
//! name), empty `w:proofErr` and `w:lastRenderedPageBreak`; in `word/settings.xml` the `w:rsids`
//! and `w:trackRevisions` elements; in `docProps/core.xml` the last author, modified and printed
//! dates and revision number; in `docProps/app.xml` everything but `HyperlinkBase` (the rest are
//! statistics a writer recomputes).
//!
//! What the facts promise. When the edited package, with the save's own tracked changes undone,
//! equals the base in every compared respect, every field is at its default except
//! `new_revisions`, `comments.added` and `comments.replies`; when it differs, at least one other
//! field is not. Which field carries a difference, and how many it counts, comes from one alignment
//! of the two streams and can shift when the alignment is ambiguous: a moved comment anchor may
//! also count untracked characters, and white space at the edge of a resolved change may read as
//! accepted or as rejected. A character changed without tracking is never counted only as a
//! comment or a resolution. A host judges every field; none is advisory.

mod budget;
mod comments;
mod compare;
mod diff;
mod digest;
mod opc;
mod package;
mod story;
mod xml;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use crate::package::{ReadLimits, rt};
use budget::Budget;
use compare::{CommentIds, Holds, Pairs, Projector, Tally, assign_contexts, compare_story, facts, field_structure};
use digest::{Digest, Hasher};
use opc::{Bookmarks, Canon, ContentTypes, Rel, Rels};
use package::StrictPackage;
use story::{Item, Revisions, Tables};
use xml::{Limits, Names};

/// The most token differences compared in one diff; beyond it the comparison stops and says so, so
/// a rewritten document cannot make the check run without end.
pub const MAX_CHANGES: usize = 2_000;

/// What an inspection reads: the embed build's limits (the same numbers as the UI crate's
/// `host::open::EMBED_LIMITS`), not the desktop's 256 MiB a part with no element cap. A host's
/// server inspects inside a small memory budget, so a hostile save is refused as unreadable rather
/// than exhausting it. Each package gets these limits.
pub const INSPECT_LIMITS: ReadLimits = ReadLimits { max_part_bytes: 64 << 20, max_total_bytes: 192 << 20, max_elements: 2_000_000, max_depth: 64 };

/// The most characters and marks read from one package's stories (about 12 bytes each). A long
/// agreement of two thousand pages holds about six million.
pub const MAX_STORY_ITEMS: usize = 8_000_000;

/// Work one inspection may spend, whatever the clock: decompressing, parsing, hashing, walking and
/// comparing, counted in steps (an element, a character, 64 bytes hashed or read, a diff
/// comparison). Past it, reading is refused as an error and a comparison stops with
/// `too_many_changes`.
pub const MAX_WORK: u64 = 400_000_000;

/// The most memory one inspection holds, whatever the uploads.
pub const MAX_MEMORY: u64 = 512 << 20;
/// Memory an inspection may hold per byte uploaded (base and save together)…
pub const MEMORY_PER_UPLOADED_BYTE: u64 = 8;
/// …but never less than this, so an ordinary agreement, whose text compresses well, is read.
pub const MEMORY_FLOOR: u64 = 384 << 20;

/// Work charged for each part, besides its bytes: classifying it, its map entries, its tree's setup.
const PART_WORK: u64 = 2_000;

/// Memory every inspection holds whatever it reads.
const FIXED_BYTES: u64 = 1 << 20;

/// Memory charged for each part's entries in what a side keeps of it, besides its name.
const PART_BYTES: u64 = 384;

/// Why a package over its element budget is unreadable.
const TOO_MANY_ELEMENTS: &str = "more XML elements than an inspection reads";
/// Why a package nested deeper than the limit is unreadable.
const TOO_DEEP: &str = "XML nested deeper than an inspection reads";

/// What a save changed against its base. A field at its default means "nothing of this kind".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inspection {
    /// Tracked changes the base does not have (by author and date), summed per author and sorted
    /// by author. Counts are elements: one `w:ins` around three runs is one insertion.
    pub new_revisions: Vec<AuthorRevisions>,
    /// Tracked changes of the base that were accepted or rejected.
    pub resolved: Resolved,
    /// Base content whose tracked changes were altered otherwise than by accepting or rejecting
    /// them (a change re-attributed, re-dated, moved onto other content), and content wrapped in a
    /// change the base already had that the base does not hold.
    pub altered_revisions: u32,
    /// Characters, marks or formatting that differ from the base without a tracked change of this
    /// save: an untracked edit, an untracked formatting change, a tracked insertion
    /// whose content Word would show plainly.
    pub untracked_changes: u32,
    /// Parts or story elements of the base missing or different, outside tracked changes.
    pub losses: Vec<Loss>,
    /// The comparison stopped at a bound (`max_changes` in one diff, or [`MAX_WORK`]). Every count
    /// is then incomplete: a host must refuse.
    pub too_many_changes: bool,
    /// Parts the base does not have: macros, embedded objects, alternative chunks,
    /// custom XML, anything. Relationship parts and `[Content_Types].xml` are reported through
    /// [`Self::changed_relationships`] and [`Self::content_types`]; the comments parts through
    /// [`Self::comments`].
    pub added_parts: Vec<AddedPart>,
    /// Relationships added or removed, per source part. A relationship is its type,
    /// target and mode; its id is not compared. The main part's links to its comments parts are
    /// reported through [`Self::comments`] instead.
    pub changed_relationships: Vec<RelationshipChange>,
    /// Parts in both packages whose content type differs.
    pub content_types: Vec<ContentTypeChange>,
    /// What happened to comments.
    pub comments: CommentChanges,
}

/// One author's new tracked changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorRevisions {
    /// `w:author` as written; an empty string when the change names none.
    pub author: String,
    /// `w:ins`, `w:moveTo`, row and cell insertions (`w:trPr/w:ins`, `w:cellIns`).
    pub insertions: u32,
    /// `w:del`, `w:moveFrom`, row and cell deletions.
    pub deletions: u32,
    /// Tracked formatting changes (`w:rPrChange`, `w:pPrChange`, `w:sectPrChange`, table ones).
    pub format_changes: u32,
    /// The earliest and latest `w:date` among these changes, as written (ISO 8601 sorts as text);
    /// `None` when none has a date.
    pub earliest_date: Option<String>,
    pub latest_date: Option<String>,
    /// Changes with no `w:date`.
    pub undated: u32,
    /// Characters and marks inside these insertions that are hidden (`w:vanish`, `w:webHidden`,
    /// `w:specVanish`), plus content these formatting changes hide.
    pub hidden_text: u32,
    /// Field characters and field code inside these insertions: a field can show other text than
    /// its code reads, and change it when updated.
    pub field_codes: u32,
    /// Directional formatting characters (U+202A–U+202E, U+2066–U+2069, U+200E, U+200F,
    /// U+061C) inside these insertions: they reorder the text after them.
    pub bidi_controls: u32,
    /// Drawings, pictures and embedded objects inside these insertions: they can cover text.
    pub objects: u32,
}

/// Base tracked changes resolved, counted once per kind, author, date and attributes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Resolved {
    /// Insertions kept as plain content, deletions whose content is gone, formatting changes whose
    /// new formatting stayed.
    pub accepted: u32,
    /// Insertions whose content is gone, deletions kept as plain content, formatting changes whose
    /// old formatting came back.
    pub rejected: u32,
}

/// Something of the base the save does not keep as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loss {
    /// The part's name in the package.
    pub part: String,
    /// What was lost.
    pub kind: LossKind,
    /// For [`LossKind::ElementMissing`], the element (`w:bookmarkStart`, `w:p`, `w:tbl`…).
    pub element: Option<String>,
    /// How many: 1 for a part; the number of such elements gone for an element.
    pub count: u32,
}

/// What kind of thing a [`Loss`] lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LossKind {
    /// A part of the base the save does not have.
    PartMissing,
    /// A part of the base whose content differs (canonically for XML, byte for byte otherwise).
    PartAltered,
    /// A story element of the base gone without a tracked change.
    ElementMissing,
}

/// A part the save added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedPart {
    /// The part's name in the package.
    pub part: String,
    /// The content type `[Content_Types].xml` gives it, if any.
    pub content_type: Option<String>,
}

/// Which side has a relationship the other lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The save has it; the base does not.
    Added,
    /// The base has it; the save does not.
    Removed,
}

/// A relationship one package has and the other does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipChange {
    /// The part the relationship belongs to; `""` for the package's own (`_rels/.rels`).
    pub source: String,
    /// Added by the save, or removed from the base.
    pub change: Change,
    /// The relationship type URI.
    pub kind: String,
    /// The target as written: a part path relative to the source, or a URI when external.
    pub target: String,
    /// `TargetMode="External"`.
    pub external: bool,
}

/// A part whose content type changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentTypeChange {
    /// The part's name in the package.
    pub part: String,
    /// The content type the base gives it, if any.
    pub base: Option<String>,
    /// The content type the save gives it, if any.
    pub edited: Option<String>,
}

/// Comments by meaning. A comment is known by its id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommentChanges {
    /// New comments that answer none, per author (sorted by author).
    pub added: Vec<AuthorCount>,
    /// New comments that answer another (`w15:paraIdParent`), per author.
    pub replies: Vec<AuthorCount>,
    /// Base comments the save no longer has.
    pub removed: u32,
    /// Base comments whose author, date, initials, text, done state, parent or anchors (where
    /// their range starts and ends, where their reference stands) changed.
    pub altered: u32,
}

/// How many comments one author added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorCount {
    /// `w:author` as written; an empty string when the comment names none.
    pub author: String,
    /// How many.
    pub count: u32,
}

/// Which file could not be read for certain, and why (in words for a log, not for a reader). A
/// host refuses the save on either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectError {
    /// The stored version the save was edited from.
    Base(String),
    /// The save.
    Edited(String),
}

/// What `edited` changed against `base`, read within [`INSPECT_LIMITS`] and each diff bounded by
/// [`MAX_CHANGES`].
pub fn inspect(base: &[u8], edited: &[u8]) -> Result<Inspection, InspectError> {
    inspect_with_limit(base, edited, MAX_CHANGES)
}

/// [`inspect`] with each diff bounded by `max_changes`.
pub fn inspect_with_limit(base: &[u8], edited: &[u8], max_changes: usize) -> Result<Inspection, InspectError> {
    inspect_within(base, edited, &INSPECT_LIMITS, max_changes)
}

/// [`inspect`] with each package read within `limits` and each diff bounded by `max_changes`.
pub fn inspect_within(base: &[u8], edited: &[u8], limits: &ReadLimits, max_changes: usize) -> Result<Inspection, InspectError> {
    let mut budget = Budget::new(MAX_WORK, memory_limit(base.len(), edited.len()));
    inspect_budgeted(base, edited, limits, max_changes, &mut budget)
}

/// [`inspect`], also answering the most memory the inspection charged to its budget. For tests that
/// check, with a counting allocator, that the inspection never holds more than it charged.
#[doc(hidden)]
pub fn inspect_measured(base: &[u8], edited: &[u8]) -> (Result<Inspection, InspectError>, u64) {
    let mut budget = Budget::new(MAX_WORK, memory_limit(base.len(), edited.len()));
    let result = inspect_budgeted(base, edited, &INSPECT_LIMITS, MAX_CHANGES, &mut budget);
    (result, budget.peak())
}

fn inspect_budgeted(base: &[u8], edited: &[u8], limits: &ReadLimits, max_changes: usize, budget: &mut Budget) -> Result<Inspection, InspectError> {
    // What any inspection holds besides what it charges piece by piece: decompressor windows, the
    // reader's state, small tables.
    budget.hold(FIXED_BYTES).map_err(InspectError::Edited)?;
    let mut names = Names::default();
    let mut tables = Tables::new();
    let base = Side::read(base, limits, &mut names, &mut tables, budget).map_err(InspectError::Base)?;
    let edited = Side::read(edited, limits, &mut names, &mut tables, budget).map_err(InspectError::Edited)?;
    let inspection = compare(base, edited, &mut tables, max_changes, budget)?;
    // Ids that ran out would make different pieces read as one; no fact stands then.
    if tables.overflowed() {
        return Err(InspectError::Edited("more distinct content than an inspection reads".into()));
    }
    if budget.memory_refused {
        return Err(InspectError::Edited(budget::TOO_MUCH_MEMORY.into()));
    }
    Ok(inspection)
}

/// The memory one inspection may hold for uploads of these sizes.
pub fn memory_limit(base: usize, edited: usize) -> u64 {
    let uploaded = budget::units(base).saturating_add(budget::units(edited));
    uploaded.saturating_mul(MEMORY_PER_UPLOADED_BYTE).clamp(MEMORY_FLOOR, MAX_MEMORY)
}

fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// How a part is compared.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Story,
    Xml,
    Binary,
}

/// One package, read.
struct Side<'a> {
    package: StrictPackage<'a>,
    types: ContentTypes,
    /// Source part (`""` for the package) → its relationships.
    rels: BTreeMap<String, Rels>,
    main: String,
    /// The parts the main part names as its comments and their state.
    comment_parts: BTreeSet<String>,
    comments: comments::Comments,
    kinds: BTreeMap<String, Kind>,
    stories: BTreeMap<String, Vec<Item>>,
    /// Each other XML part's compared form, as a digest.
    canonical: BTreeMap<String, Digest>,
    revisions: Revisions,
}

/// The one internal target of `source`'s relationship of `kind`, as a part name; `None` when there
/// is none, an error when there are several or it points outside the package.
fn single_target(rels: &BTreeMap<String, Rels>, source: &str, kind: &str) -> Result<Option<String>, String> {
    let Some(rels) = rels.get(source) else { return Ok(None) };
    let found: Vec<&Rel> = rels.of_type(kind).collect();
    let [rel] = found.as_slice() else {
        return if found.is_empty() { Ok(None) } else { Err(format!("{} relationships of type {kind}", found.len())) };
    };
    if rel.external {
        return Err(format!("an external relationship of type {kind}"));
    }
    let dir = source.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    Ok(Some(crate::package::resolve(dir, &rel.target)))
}

/// A part's bytes, held only while it is parsed.
fn parse_part(
    package: &mut StrictPackage<'_>,
    name: &str,
    names: &mut Names,
    limits: &mut Limits,
    budget: &mut Budget,
) -> Result<xml::Parsed, String> {
    let bytes = package.read(name, budget)?.ok_or_else(|| format!("no part {name}"))?;
    let parsed = xml::parse(&bytes, names, limits, budget).map_err(|error| format!("{name}: {error}"));
    budget.release(budget::units(bytes.len()));
    parsed
}

impl<'a> Side<'a> {
    fn read(bytes: &'a [u8], limits: &ReadLimits, names: &mut Names, tables: &mut Tables, budget: &mut Budget) -> Result<Side<'a>, String> {
        let mut package = StrictPackage::open(bytes, limits, budget)?;
        let mut limits = Limits { elements_left: limits.max_elements, max_depth: limits.max_depth.min(crate::xml::MAX_DEPTH) };
        let all: Vec<String> = package.names().map(str::to_string).collect();

        let parsed = parse_part(&mut package, "[Content_Types].xml", names, &mut limits, budget)?;
        let types = ContentTypes::parse(&parsed.root, budget);
        budget.release(parsed.held);
        let types = types?;
        let mut rels = BTreeMap::new();
        for name in &all {
            if let Some(source) = opc::rels_source(name) {
                let parsed = parse_part(&mut package, name, names, &mut limits, budget)?;
                let part = Rels::parse(&parsed.root, budget).map_err(|error| format!("{name}: {error}"));
                budget.release(parsed.held);
                rels.insert(source, part?);
            }
        }
        let main = single_target(&rels, "", rt::OFFICE_DOC)?.ok_or("no main document part")?;
        if !package.has(&main) {
            return Err(format!("no main document part {main}"));
        }
        if !types.of(&main).is_some_and(|kind| opc::MAIN_TYPES.contains(&kind)) {
            return Err(format!("{main} is not a Word document part"));
        }
        let mut comment_parts = BTreeSet::new();
        let mut comment_roles = [None, None];
        for (slot, (kind, content_type)) in [(rt::COMMENTS, opc::COMMENTS_TYPE), (rt::COMMENTS_EX, opc::COMMENTS_EX_TYPE)].into_iter().enumerate() {
            let Some(part) = single_target(&rels, &main, kind)? else { continue };
            if !package.has(&part) || types.of(&part) != Some(content_type) {
                return Err(format!("comments part {part} missing or of another content type"));
            }
            if !comment_parts.insert(part.clone()) {
                return Err(format!("{part} named for two comment roles"));
            }
            if let Some(role) = comment_roles.get_mut(slot) {
                *role = Some(part);
            }
        }
        let [comments_part, extended_part] = comment_roles;
        // Parts whose bookkeeping is skipped; the defaults when the package does not name them.
        let settings = single_target(&rels, &main, rt::SETTINGS)?.unwrap_or_else(|| "word/settings.xml".into());
        let app = single_target(&rels, "", rt::APP)?.unwrap_or_else(|| "docProps/app.xml".into());
        let core = single_target(&rels, "", rt::CORE)?.unwrap_or_else(|| "docProps/core.xml".into());

        let empty = Rels::default();
        let mut revisions = Revisions::default();
        let mut kinds = BTreeMap::new();
        let mut stories = BTreeMap::new();
        let mut canonical = BTreeMap::new();
        let mut items_left = MAX_STORY_ITEMS;
        for name in &all {
            if name == "[Content_Types].xml" || opc::rels_source(name).is_some() || comment_parts.contains(name) {
                continue;
            }
            // Its entries in the kinds, stories and canonical maps, and the names they keep.
            budget.hold(PART_BYTES.saturating_add(budget::units(name.len()).saturating_mul(3)))?;
            budget.work(PART_WORK)?;
            let content_type = types.of(name);
            let part_rels = rels.get(name.as_str()).unwrap_or(&empty);
            if content_type.is_some_and(opc::is_story_type) {
                let parsed = parse_part(&mut package, name, names, &mut limits, budget)?;
                let marks: Bookmarks = opc::bookmarks(&parsed.root);
                let canon = Canon { rels: part_rels, bookmarks: Some(&marks) };
                let items = story::walk(&parsed.root, &canon, tables, &mut revisions, &mut items_left, parsed.most_items, budget)
                    .map_err(|error| format!("{name}: {error}"));
                drop(parsed.root);
                budget.release(parsed.held);
                kinds.insert(name.clone(), Kind::Story);
                stories.insert(name.clone(), items?);
            } else if opc::is_xml(name, content_type) {
                let parsed = parse_part(&mut package, name, names, &mut limits, budget)?;
                let root = &parsed.root;
                let canon = Canon { rels: part_rels, bookmarks: None };
                let digest = if *name == settings {
                    canon.digest(root, &|child| !matches!(&*child.name, "w:rsids" | "w:trackRevisions"), budget)
                } else if *name == core {
                    canon.digest(
                        root,
                        &|child| !matches!(&*child.name, "cp:lastModifiedBy" | "dcterms:modified" | "cp:revision" | "cp:lastPrinted"),
                        budget,
                    )
                } else if *name == app {
                    let mut hasher = Hasher::default();
                    for child in root.els().filter(|child| child.name.rsplit([':', '}']).next() == Some("HyperlinkBase")) {
                        canon.element(child, &mut hasher);
                    }
                    opc::finish(hasher, budget)
                } else {
                    canon.digest(root, &|_| true, budget)
                };
                budget.release(parsed.held);
                kinds.insert(name.clone(), Kind::Xml);
                canonical.insert(name.clone(), digest?);
            } else {
                kinds.insert(name.clone(), Kind::Binary);
            }
        }
        let comments = match &comments_part {
            Some(part) => {
                let parsed = parse_part(&mut package, part, names, &mut limits, budget)?;
                let extended = match extended_part.as_deref() {
                    Some(extended) => Some(parse_part(&mut package, extended, names, &mut limits, budget)?),
                    None => None,
                };
                let canon = Canon { rels: rels.get(part.as_str()).unwrap_or(&empty), bookmarks: None };
                let read = comments::read(&parsed.root, extended.as_ref().map(|extended| &extended.root), &canon, budget)
                    .map_err(|error| format!("{part}: {error}"));
                budget.release(parsed.held.saturating_add(extended.map_or(0, |extended| extended.held)));
                read?
            }
            None if extended_part.is_some() => return Err("a commentsExtended part without comments".into()),
            None => comments::Comments::new(),
        };
        tables.charge(budget)?;
        Ok(Side { package, types, rels, main, comment_parts, comments, kinds, stories, canonical, revisions })
    }

    /// The relationships compared for `source`: the main part's links to its comments parts are
    /// left out, since the comments are compared by meaning.
    fn relationship_set(&self, source: &str) -> BTreeSet<&Rel> {
        let Some(rels) = self.rels.get(source) else { return BTreeSet::new() };
        let mut set = rels.set();
        if source == self.main {
            set.retain(|rel| !(opc::is_type(&rel.kind, rt::COMMENTS) || opc::is_type(&rel.kind, rt::COMMENTS_EX)));
        }
        set
    }
}

fn compare(
    mut base: Side<'_>,
    mut edited: Side<'_>,
    tables: &mut Tables,
    max_changes: usize,
    budget: &mut Budget,
) -> Result<Inspection, InspectError> {
    let mut inspection = Inspection::default();
    let mut tally = Tally::default();

    // A part that is a comments part on one side must be one on the other, or absent there.
    for (one, other) in [(&base, &edited), (&edited, &base)] {
        if let Some(part) = one.comment_parts.iter().find(|part| other.package.has(part) && !other.comment_parts.contains(*part)) {
            return Err(InspectError::Edited(format!("{part} is a comments part in one package only")));
        }
    }

    // Parts: added, missing, altered.
    let all: BTreeSet<String> = base.kinds.keys().chain(edited.kinds.keys()).cloned().collect();
    let mut story_parts = Vec::new();
    for name in &all {
        let mut altered = false;
        match (base.kinds.get(name).copied(), edited.kinds.get(name).copied()) {
            (None, Some(_)) => inspection.added_parts.push(AddedPart { part: name.clone(), content_type: edited.types.of(name).map(str::to_string) }),
            (Some(_), None) => {
                *tally.losses.entry((name.clone(), LossKind::PartMissing, None)).or_default() += 1;
            }
            (Some(Kind::Story), Some(Kind::Story)) => story_parts.push(name.clone()),
            (Some(Kind::Xml), Some(Kind::Xml)) => altered = base.canonical.get(name) != edited.canonical.get(name),
            (Some(Kind::Binary), Some(Kind::Binary)) => altered = !package::same_bytes(&mut base.package, &mut edited.package, name, budget)?,
            (Some(_), Some(_)) => altered = true,
            (None, None) => {}
        }
        if altered {
            *tally.losses.entry((name.clone(), LossKind::PartAltered, None)).or_default() += 1;
        }
        if base.kinds.contains_key(name) && edited.kinds.contains_key(name) {
            let (before, after) = (base.types.of(name), edited.types.of(name));
            if before != after {
                inspection.content_types.push(ContentTypeChange {
                    part: name.clone(),
                    base: before.map(str::to_string),
                    edited: after.map(str::to_string),
                });
            }
        }
    }

    // Relationships.
    let sources: BTreeSet<String> = base.rels.keys().chain(edited.rels.keys()).cloned().collect();
    for source in &sources {
        let (before, after) = (base.relationship_set(source), edited.relationship_set(source));
        for (change, rel) in
            before.difference(&after).map(|rel| (Change::Removed, rel)).chain(after.difference(&before).map(|rel| (Change::Added, rel)))
        {
            inspection.changed_relationships.push(RelationshipChange {
                source: source.clone(),
                change,
                kind: rel.kind.clone(),
                target: rel.target.clone(),
                external: rel.external,
            });
        }
    }

    // Comments.
    let base_ids: BTreeSet<String> = base.comments.keys().cloned().collect();
    let edited_ids: BTreeSet<String> = edited.comments.keys().cloned().collect();
    let removed: BTreeSet<String> = base_ids.difference(&edited_ids).cloned().collect();
    let new_comments: BTreeSet<String> = edited_ids.difference(&base_ids).cloned().collect();
    let mut added: BTreeMap<String, u32> = BTreeMap::new();
    let mut replies: BTreeMap<String, u32> = BTreeMap::new();
    for id in &new_comments {
        if let Some(comment) = edited.comments.get(id) {
            let counts = if comment.parent.is_some() { &mut replies } else { &mut added };
            let entry = counts.entry(comment.author.clone()).or_default();
            *entry = entry.saturating_add(1);
        }
    }
    for id in base_ids.intersection(&edited_ids) {
        if base.comments.get(id) != edited.comments.get(id) {
            tally.comments_altered.insert(id.clone());
        }
    }

    // Revisions: the save's own are those whose author and date the base does not have.
    let new_pairs: Pairs = edited.revisions.by_pair.keys().filter(|pair| !base.revisions.by_pair.contains_key(*pair)).copied().collect();
    let mut by_author: BTreeMap<u32, AuthorRevisions> = BTreeMap::new();
    for ((author, date), counts) in edited.revisions.by_pair.iter().filter(|(pair, _)| new_pairs.contains(*pair)) {
        let entry =
            by_author.entry(*author).or_insert_with(|| AuthorRevisions { author: tables.text_of(*author).to_string(), ..AuthorRevisions::default() });
        entry.insertions = entry.insertions.saturating_add(counts.insertions);
        entry.deletions = entry.deletions.saturating_add(counts.deletions);
        entry.format_changes = entry.format_changes.saturating_add(counts.format_changes);
        let date = tables.text_of(*date);
        if date.is_empty() {
            let elements = counts.insertions.saturating_add(counts.deletions).saturating_add(counts.format_changes);
            entry.undated = entry.undated.saturating_add(elements);
        } else {
            if entry.earliest_date.as_deref().is_none_or(|earliest| date < earliest) {
                entry.earliest_date = Some(date.to_string());
            }
            if entry.latest_date.as_deref().is_none_or(|latest| date > latest) {
                entry.latest_date = Some(date.to_string());
            }
        }
    }

    // Stories.
    let ids = CommentIds { base: &base_ids, removed: &removed };
    let mut projector = Projector::new(&new_pairs, &new_comments);
    let mut holds: BTreeMap<u32, Holds> = BTreeMap::new();
    for name in &story_parts {
        let (Some(mut before), Some(mut after)) = (base.stories.remove(name), edited.stories.remove(name)) else { continue };
        let held = story::items_held(&before).saturating_add(story::items_held(&after));
        let compared = (|| -> Result<bool, String> {
            assign_contexts(&mut before, tables, &mut |_, format| format, budget)?;
            assign_contexts(&mut after, tables, &mut |tables, format| projector.format(tables, format), budget)?;
            let moves = facts(&after, tables, &new_pairs, &mut holds, budget)?;
            let fields = field_structure(&after, tables, &new_pairs, &mut holds, budget)?;
            tally.untracked = tally.untracked.saturating_add(moves).saturating_add(fields);
            projector.project(tables, &mut after, budget)?;
            Ok(compare_story(name, &before, &after, tables, &ids, max_changes, budget, &mut tally))
        })();
        budget.release(held);
        match compared {
            Ok(complete) => inspection.too_many_changes |= !complete,
            Err(error) => return Err(InspectError::Edited(format!("{name}: {error}"))),
        }
    }
    for (author, held) in holds {
        let entry =
            by_author.entry(author).or_insert_with(|| AuthorRevisions { author: tables.text_of(author).to_string(), ..AuthorRevisions::default() });
        entry.hidden_text = held.hidden;
        entry.field_codes = held.field_codes;
        entry.bidi_controls = held.bidi_controls;
        entry.objects = held.objects;
    }
    inspection.new_revisions = by_author.into_values().collect();

    inspection.resolved = Resolved { accepted: count(tally.accepted.len()), rejected: count(tally.rejected.len()) };
    inspection.altered_revisions = tally.altered;
    inspection.untracked_changes = tally.untracked;
    inspection.losses = tally.losses.into_iter().map(|((part, kind, element), count)| Loss { part, kind, element, count }).collect();
    let altered: BTreeSet<&String> = tally.comments_altered.iter().filter(|id| !removed.contains(*id)).collect();
    inspection.comments = CommentChanges {
        added: added.into_iter().map(|(author, count)| AuthorCount { author, count }).collect(),
        replies: replies.into_iter().map(|(author, count)| AuthorCount { author, count }).collect(),
        removed: count(removed.len()),
        altered: count(altered.len()),
    };
    Ok(inspection)
}

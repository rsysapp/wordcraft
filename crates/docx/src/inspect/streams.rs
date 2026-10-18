//! embed: the save check's streams, decoded, for Clauflow's version comparison. Both packages are
//! read exactly as `inspect()` reads them (the same parser, limits, budget and walker, one shared
//! table so equal format ids mean equal properties across the pair), then each item is decoded
//! into what a comparison shows: characters with their formatting, tracked changes and field role;
//! paragraph ends with the model reader's origin; tables, fields, links, notes, comments, objects
//! and sections; anything else as an opaque element and its digest.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use super::InspectError;
use super::budget::{self, Budget};
use super::opc::Canon;
use super::story::{Format, FormatRev, Item, MARK_BASE, MarkKind, Tables, Wrap, WrapKind};
use super::stream_extras::{CommentMark, Extra, Extras};
use super::xml::{Names, XEl};
use super::{FIXED_BYTES, INSPECT_LIMITS, MAX_WORK, Side, memory_limit, opc};
use crate::ReadLimits;

/// What reading both sides may spend: the save check's own limits by default.
#[derive(Clone, Copy, Debug)]
pub struct StreamLimits {
    pub read: ReadLimits,
    pub work: u64,
    pub memory: u64,
}

/// [`INSPECT_LIMITS`], [`MAX_WORK`], and [`memory_limit`] of the two sizes.
pub fn default_limits(left_len: usize, right_len: usize) -> StreamLimits {
    StreamLimits { read: INSPECT_LIMITS, work: MAX_WORK, memory: memory_limit(left_len, right_len) }
}

/// Both packages decoded, with one shared table: equal `FormatId`s mean equal canonical
/// properties across the pair.
pub struct StreamPair {
    pub left: Streams,
    pub right: Streams,
    pub work_spent: u64,
    pub peak_memory: u64,
}

pub struct Streams {
    /// Body first, then headers and footers, footnotes, endnotes and text boxes, each in
    /// document order.
    pub stories: Vec<Story>,
    pub comments: Vec<Comment>,
    pub settings: Settings,
    /// Every part that is not a story or a comments part: canonical XML digest or bytes digest,
    /// lowercase hex SHA-256 (as the inspector compares them).
    pub parts: BTreeMap<String, String>,
    /// The part names' sorted order, so an `origin` and a part name can be related as the
    /// model reader does (`Origin.part`).
    pub part_order: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoryId {
    Body,
    Header { part: String },
    Footer { part: String },
    Footnote { id: String },
    Endnote { id: String },
    /// A text box: the part it is in, the origin index of the paragraph that anchors it, and
    /// its order among that paragraph's text boxes.
    TextBox { part: String, anchor: u32, ordinal: u32 },
}

pub struct Story {
    pub id: StoryId,
    pub part: String,
    pub items: Vec<StreamItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FormatId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevisionKind {
    Inserted,
    Deleted,
    MovedTo,
    MovedFrom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    pub kind: RevisionKind,
    pub author: String,
    pub date: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatRevision {
    pub author: String,
    pub date: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldRole {
    None,
    Instruction,
    Result,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderSlot {
    Default,
    First,
    Even,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    Footnote,
    Endnote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Drawing,
    Picture,
    Embedded,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Media {
    pub part: String,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamItem {
    Char { ch: char, format: FormatId, hidden: bool, revision: Option<Revision>, format_revision: Option<FormatRevision>, field: FieldRole },
    /// The end of a paragraph: its `w:p`'s pre-order index in its part (the model's
    /// `Origin.index`), its properties, and a tracked change of the mark itself.
    ParagraphEnd { origin: u32, props: FormatId, revision: Option<Revision>, format_revision: Option<FormatRevision> },
    TableStart { props: FormatId },
    TableEnd,
    RowStart { props: FormatId, revision: Option<Revision> },
    RowEnd,
    CellStart { props: FormatId, revision: Option<Revision> },
    CellEnd,
    FieldBegin,
    FieldSeparate,
    FieldEnd,
    SimpleFieldStart { instruction: String },
    SimpleFieldEnd,
    HyperlinkStart { target: Option<String>, anchor: Option<String> },
    HyperlinkEnd,
    Object { kind: ObjectKind, digest: String, media: Vec<Media>, alt: Option<String> },
    NoteReference { kind: NoteKind, id: String },
    CommentStart { id: String },
    CommentEnd { id: String },
    CommentReference { id: String },
    SectionBreak { props: FormatId, headers: Vec<(HeaderSlot, String)>, footers: Vec<(HeaderSlot, String)> },
    ContentControlStart { props: FormatId },
    ContentControlEnd,
    /// Anything else, by element name and the digest of its canonical form.
    Opaque { element: String, digest: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub date: Option<String>,
    pub text: String,
    pub parent: Option<String>,
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub track_revisions: bool,
    /// `w:documentProtection`'s `w:edit` value when protection is enforced.
    pub protection: Option<String>,
    /// The digest of the settings part without `w:rsids`, `w:proofState` and the other
    /// elements Word rewrites on every save (`SETTINGS_CHURN`); `w:trackRevisions` is reported on
    /// its own. Empty when the package has no settings part.
    pub other_digest: String,
}

/// Elements of `word/settings.xml` that Word rewrites on a plain re-save; never a difference.
pub const SETTINGS_CHURN: &[&str] = &["w:rsids", "w:proofState"];

/// What streams mode records while one package is read.
#[derive(Default)]
pub(super) struct SideRecord {
    /// Story part name → its record.
    pub stories: BTreeMap<String, Extras>,
    pub settings: Option<Settings>,
    pub settings_part: String,
    pub part_order: Vec<String>,
}

/// Both packages decoded. Errors name the side as [`super::inspect`] does: the left package is
/// the base, the right the edited one.
pub fn read_pair(left: &[u8], right: &[u8], limits: &StreamLimits) -> Result<StreamPair, InspectError> {
    let mut budget = Budget::new(limits.work, limits.memory);
    budget.hold(opc::HASHING_BYTES.saturating_add(FIXED_BYTES)).map_err(InspectError::Base)?;
    let mut names = Names::default();
    let mut tables = Tables::new();
    let (mut left_record, mut right_record) = (SideRecord::default(), SideRecord::default());
    let left_side = Side::read_with(left, &limits.read, &mut names, &mut tables, &mut budget, &mut false, Some(&mut left_record)).map_err(InspectError::Base)?;
    let right_side = Side::read_with(right, &limits.read, &mut names, &mut tables, &mut budget, &mut false, Some(&mut right_record)).map_err(InspectError::Edited)?;
    // Ids that ran out would make different pieces read as one.
    if tables.overflowed() {
        return Err(InspectError::Edited("more distinct content than an inspection reads".into()));
    }
    let left = decode(&left_side, left_record, &tables, &mut budget).map_err(InspectError::Base)?;
    let right = decode(&right_side, right_record, &tables, &mut budget).map_err(InspectError::Edited)?;
    if budget.memory_refused {
        return Err(InspectError::Edited(budget::TOO_MUCH_MEMORY.into()));
    }
    Ok(StreamPair { left, right, work_spent: limits.work.saturating_sub(budget.work_left()), peak_memory: budget.peak() })
}

/// The settings part's facts: the tracking switch, enforced protection, and the digest of the rest.
pub(super) fn settings_of(root: &XEl, canon: &Canon<'_>, budget: &mut Budget) -> Result<Settings, String> {
    let on = |el: &XEl, name: &str| !matches!(el.attr(name), Some("0" | "false" | "off"));
    let track_revisions = root.els().any(|child| &*child.name == "w:trackRevisions" && on(child, "w:val"));
    let protection = root
        .els()
        .find(|child| &*child.name == "w:documentProtection")
        .filter(|protection| protection.attr("w:enforcement").is_some_and(|value| matches!(value, "1" | "true" | "on")))
        .map(|protection| protection.attr("w:edit").unwrap_or("").to_string());
    let digest = canon.digest(root, &|child| !SETTINGS_CHURN.contains(&&*child.name) && &*child.name != "w:trackRevisions", budget)?;
    Ok(Settings { track_revisions, protection, other_digest: digest.hex() })
}

/// The order stories are listed in: body, headers, footers, footnotes, endnotes.
fn rank(kind: Option<&str>) -> Option<u8> {
    let kind = kind?;
    if opc::MAIN_TYPES.contains(&kind) {
        return Some(0);
    }
    [("header+xml", 1), ("footer+xml", 2), ("footnotes+xml", 3), ("endnotes+xml", 4)]
        .into_iter()
        .find(|(suffix, _)| kind.ends_with(&format!(".{suffix}")) || kind.ends_with(suffix))
        .map(|(_, rank)| rank)
}

fn decode(side: &Side<'_>, mut record: SideRecord, tables: &Tables, budget: &mut Budget) -> Result<Streams, String> {
    let mut parts = BTreeMap::new();
    for (name, digest) in &side.kept.canonical {
        if *name != record.settings_part {
            budget.hold(budget::units(name.len()).saturating_add(64).saturating_add(budget::tree_slot::<String, String>()))?;
            parts.insert(name.clone(), digest.hex());
        }
    }
    let mut decoder = Decoder { tables, parts: &parts, revisions: HashMap::new(), formats: HashMap::new() };
    let mut stories: Vec<(u8, Story)> = Vec::new();
    let mut boxes: Vec<(u8, String, Story)> = Vec::new();
    for (name, items) in &side.kept.stories {
        let Some(rank) = rank(side.types.of(name)) else { continue };
        let extras = record.stories.remove(name).unwrap_or_default();
        let facts = &extras.by_item;
        let digests = &extras.mark_digests;
        match rank {
            3 | 4 => {
                // One story per note, separators left out.
                let mut index = 0;
                while index < items.len() {
                    if let Some(Extra::NoteStart { kind, id, separator }) = facts.get(&index) {
                        let end = (index + 1..items.len()).find(|at| matches!(facts.get(at), Some(Extra::NoteEnd))).unwrap_or(items.len());
                        if !separator {
                            let decoded = decoder.items(items, index + 1..end, facts, digests, budget)?;
                            let id = match kind {
                                NoteKind::Footnote => StoryId::Footnote { id: id.clone() },
                                NoteKind::Endnote => StoryId::Endnote { id: id.clone() },
                            };
                            stories.push((rank, Story { id, part: name.clone(), items: decoded }));
                        }
                        index = end;
                    }
                    index += 1;
                }
            }
            _ => {
                let id = match rank {
                    0 => StoryId::Body,
                    1 => StoryId::Header { part: name.clone() },
                    _ => StoryId::Footer { part: name.clone() },
                };
                let decoded = decoder.items(items, 0..items.len(), facts, digests, budget)?;
                stories.push((rank, Story { id, part: name.clone(), items: decoded }));
            }
        }
        for text_box in &extras.text_boxes {
            let decoded = decoder.items(&text_box.items, 0..text_box.items.len(), &text_box.by_item, digests, budget)?;
            let id = StoryId::TextBox { part: name.clone(), anchor: text_box.anchor, ordinal: text_box.ordinal };
            boxes.push((rank, name.clone(), Story { id, part: name.clone(), items: decoded }));
        }
    }
    // Stable: within a rank, parts in name order and notes in document order, as read.
    stories.sort_by_key(|(rank, _)| *rank);
    boxes.sort_by(|(rank_a, part_a, a), (rank_b, part_b, b)| {
        let place = |story: &Story| match story.id {
            StoryId::TextBox { anchor, ordinal, .. } => (anchor, ordinal),
            _ => (0, 0),
        };
        (rank_a, part_a, place(a)).cmp(&(rank_b, part_b, place(b)))
    });
    let mut all: Vec<Story> = stories.into_iter().map(|(_, story)| story).collect();
    all.extend(boxes.into_iter().map(|(_, _, story)| story));

    let mut comments: Vec<Comment> = Vec::with_capacity(side.comments.len());
    for (id, comment) in &side.comments {
        let detail = comment.detail.clone();
        budget.hold(budget::units(std::mem::size_of::<Comment>()).saturating_add(budget::units(id.len().saturating_mul(2))))?;
        comments.push(Comment {
            id: id.clone(),
            author: comment.author.clone(),
            date: detail.as_ref().and_then(|detail| detail.date.clone()),
            text: detail.map(|detail| detail.text).unwrap_or_default(),
            parent: comment.parent.clone(),
            done: comment.done,
        });
    }
    // Word numbers comments from 0: in number order, not text order.
    comments.sort_by(|a, b| (a.id.parse::<i64>().ok(), &a.id).cmp(&(b.id.parse::<i64>().ok(), &b.id)));
    let settings = record.settings.take().unwrap_or(Settings { track_revisions: false, protection: None, other_digest: String::new() });
    Ok(Streams { stories: all, comments, settings, parts, part_order: record.part_order })
}

/// Which wraps a revision is read from.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Level {
    Text,
    Row,
    Cell,
}

/// What a format says, decoded once.
#[derive(Clone)]
struct FormatFacts {
    hidden: bool,
    instr: bool,
    run: Option<FormatRevision>,
    paragraph: Option<FormatRevision>,
}

struct Decoder<'t> {
    tables: &'t Tables,
    parts: &'t BTreeMap<String, String>,
    revisions: HashMap<(u32, Level), Option<Revision>>,
    formats: HashMap<u32, FormatFacts>,
}

impl Decoder<'_> {
    fn text(&self, id: u32) -> String {
        self.tables.text_of(id).to_string()
    }

    fn date(&self, id: u32) -> Option<String> {
        Some(self.tables.text_of(id)).filter(|date| !date.is_empty()).map(str::to_string)
    }

    fn format_revision(&self, rev: Option<&FormatRev>) -> Option<FormatRevision> {
        rev.map(|rev| FormatRevision { author: self.text(rev.author), date: self.date(rev.date) })
    }

    /// The innermost tracked change of `level` around an item.
    fn revision(&mut self, wraps: u32, level: Level) -> Option<Revision> {
        if let Some(found) = self.revisions.get(&(wraps, level)) {
            return found.clone();
        }
        let kind_of = |wrap: &Wrap| match (level, wrap.kind) {
            (Level::Text, WrapKind::Ins) | (Level::Row, WrapKind::RowIns) | (Level::Cell, WrapKind::CellIns) => Some(RevisionKind::Inserted),
            (Level::Text, WrapKind::Del) | (Level::Row, WrapKind::RowDel) | (Level::Cell, WrapKind::CellDel) => Some(RevisionKind::Deleted),
            (Level::Text, WrapKind::MoveTo) => Some(RevisionKind::MovedTo),
            (Level::Text, WrapKind::MoveFrom) => Some(RevisionKind::MovedFrom),
            _ => None,
        };
        let found = self
            .tables
            .wraps_of(wraps)
            .iter()
            .rev()
            .find_map(|wrap| kind_of(wrap).map(|kind| Revision { kind, author: self.text(wrap.author), date: self.date(wrap.date) }));
        self.revisions.insert((wraps, level), found.clone());
        found
    }

    fn format(&mut self, id: u32) -> FormatFacts {
        if let Some(facts) = self.formats.get(&id) {
            return facts.clone();
        }
        let empty = Format::default();
        let format = self.tables.format_of(id).unwrap_or(&empty);
        let paragraph = format.slot("pPr").1.or(format.slot("pPr.rPr").1);
        let facts = FormatFacts {
            hidden: format.hidden,
            instr: format.instr,
            run: self.format_revision(format.slot("rPr").1),
            paragraph: self.format_revision(paragraph),
        };
        self.formats.insert(id, facts.clone());
        facts
    }

    fn items(
        &mut self,
        items: &[Item],
        range: Range<usize>,
        facts: &BTreeMap<usize, Extra>,
        digests: &BTreeMap<u32, String>,
        budget: &mut Budget,
    ) -> Result<Vec<StreamItem>, String> {
        let mut out = Vec::new();
        // Open fields, innermost last: whether each is in its result yet.
        let mut fields: Vec<bool> = Vec::new();
        let digest = |piece: u32| digests.get(&piece).cloned().unwrap_or_default();
        for index in range {
            let Some(item) = items.get(index) else { break };
            budget.work(3)?;
            if item.piece < MARK_BASE {
                let format = self.format(item.format);
                let field = match (format.instr, fields.last()) {
                    (true, _) | (false, Some(false)) => FieldRole::Instruction,
                    (false, Some(true)) => FieldRole::Result,
                    (false, None) => FieldRole::None,
                };
                let revision = self.revision(item.wraps, Level::Text);
                let ch = char::from_u32(item.piece).unwrap_or(char::REPLACEMENT_CHARACTER);
                out.push(StreamItem::Char { ch, format: FormatId(item.format), hidden: format.hidden, revision, format_revision: format.run, field });
                continue;
            }
            let Some(mark) = self.tables.mark_info(item.piece) else { continue };
            let fact = facts.get(&index);
            let props = FormatId(item.format);
            let decoded = match (mark.kind, fact) {
                (_, Some(Extra::Root | Extra::NoteStart { .. } | Extra::NoteEnd)) => None,
                (MarkKind::Paragraph, fact) => {
                    let (origin, section) = match fact {
                        Some(Extra::Paragraph { origin, section }) => (*origin, section.as_ref()),
                        _ => (u32::MAX, None),
                    };
                    let revision = self.revision(item.wraps, Level::Text);
                    let format_revision = self.format(item.format).paragraph;
                    out.push(StreamItem::ParagraphEnd { origin, props, revision, format_revision });
                    section.map(|(format, refs)| StreamItem::SectionBreak {
                        props: FormatId(*format),
                        headers: refs.headers.clone(),
                        footers: refs.footers.clone(),
                    })
                }
                (MarkKind::Section, fact) => {
                    let (headers, footers) = match fact {
                        Some(Extra::Section(refs)) => (refs.headers.clone(), refs.footers.clone()),
                        _ => (Vec::new(), Vec::new()),
                    };
                    Some(StreamItem::SectionBreak { props, headers, footers })
                }
                (MarkKind::FieldBegin, _) => {
                    fields.push(false);
                    Some(StreamItem::FieldBegin)
                }
                (MarkKind::FieldSeparate, _) => {
                    if let Some(open) = fields.last_mut() {
                        *open = true;
                    }
                    Some(StreamItem::FieldSeparate)
                }
                (MarkKind::FieldEnd, _) => {
                    fields.pop();
                    Some(StreamItem::FieldEnd)
                }
                (MarkKind::SimpleField(_), fact) => {
                    fields.push(true);
                    let instruction = match fact {
                        Some(Extra::SimpleField(instruction)) => instruction.clone(),
                        _ => String::new(),
                    };
                    Some(StreamItem::SimpleFieldStart { instruction })
                }
                (MarkKind::SimpleFieldEnd, _) => {
                    fields.pop();
                    Some(StreamItem::SimpleFieldEnd)
                }
                (_, Some(Extra::Hyperlink { target, anchor })) => Some(StreamItem::HyperlinkStart { target: target.clone(), anchor: anchor.clone() }),
                (_, Some(Extra::ContentControl(format))) => Some(StreamItem::ContentControlStart { props: FormatId(*format) }),
                (_, Some(Extra::NoteReference(kind, id))) => Some(StreamItem::NoteReference { kind: *kind, id: id.clone() }),
                (_, Some(Extra::Comment(CommentMark::Start, id))) => Some(StreamItem::CommentStart { id: id.clone() }),
                (_, Some(Extra::Comment(CommentMark::End, id))) => Some(StreamItem::CommentEnd { id: id.clone() }),
                (_, Some(Extra::Comment(CommentMark::Reference, id))) => Some(StreamItem::CommentReference { id: id.clone() }),
                (_, Some(Extra::Object { kind, media, alt })) => Some(StreamItem::Object {
                    kind: *kind,
                    digest: digest(item.piece),
                    media: media.iter().map(|part| Media { part: part.clone(), sha256: self.parts.get(part).cloned().unwrap_or_default() }).collect(),
                    alt: alt.clone(),
                }),
                (_, Some(Extra::Open)) => Some(match &*mark.element {
                    "w:tbl" => StreamItem::TableStart { props },
                    "w:tr" => StreamItem::RowStart { props, revision: self.revision(item.wraps, Level::Row) },
                    "w:tc" => StreamItem::CellStart { props, revision: self.revision(item.wraps, Level::Cell) },
                    element => StreamItem::Opaque { element: element.to_string(), digest: digest(item.piece) },
                }),
                // A container's end says nothing its start did not, except for those it closes.
                (_, Some(Extra::Close)) => match &*mark.element {
                    "w:tbl" => Some(StreamItem::TableEnd),
                    "w:tr" => Some(StreamItem::RowEnd),
                    "w:tc" => Some(StreamItem::CellEnd),
                    "w:hyperlink" => Some(StreamItem::HyperlinkEnd),
                    "w:sdt" => Some(StreamItem::ContentControlEnd),
                    _ => None,
                },
                _ => Some(StreamItem::Opaque { element: mark.element.to_string(), digest: digest(item.piece) }),
            };
            if let Some(decoded) = decoded {
                out.push(decoded);
            }
        }
        let held = out.iter().fold(budget::units(out.capacity()).saturating_mul(budget::units(std::mem::size_of::<StreamItem>())), |held, item| {
            held.saturating_add(item_strings(item))
        });
        budget.hold(held)?;
        Ok(out)
    }
}

/// The memory a decoded item's strings hold.
fn item_strings(item: &StreamItem) -> u64 {
    let revision = |revision: &Option<Revision>| {
        revision.as_ref().map_or(0, |revision| budget::units(revision.author.len().saturating_add(revision.date.as_ref().map_or(0, String::len))))
    };
    match item {
        StreamItem::Char { revision: changed, format_revision, .. } | StreamItem::ParagraphEnd { revision: changed, format_revision, .. } => revision(changed)
            .saturating_add(format_revision.as_ref().map_or(0, |rev| budget::units(rev.author.len().saturating_add(rev.date.as_ref().map_or(0, String::len))))),
        StreamItem::RowStart { revision: changed, .. } | StreamItem::CellStart { revision: changed, .. } => revision(changed),
        StreamItem::Opaque { element, digest } => budget::units(element.len().saturating_add(digest.len())),
        StreamItem::Object { digest, media, alt, .. } => {
            media.iter().fold(budget::units(digest.len().saturating_add(alt.as_ref().map_or(0, String::len))), |held, media| {
                held.saturating_add(budget::units(media.part.len().saturating_add(media.sha256.len()).saturating_add(48)))
            })
        }
        StreamItem::SimpleFieldStart { instruction: text }
        | StreamItem::NoteReference { id: text, .. }
        | StreamItem::CommentStart { id: text }
        | StreamItem::CommentEnd { id: text }
        | StreamItem::CommentReference { id: text } => budget::units(text.len()),
        StreamItem::HyperlinkStart { target, anchor } => budget::units(target.as_ref().map_or(0, String::len).saturating_add(anchor.as_ref().map_or(0, String::len))),
        StreamItem::SectionBreak { headers, footers, .. } => {
            headers.iter().chain(footers).fold(0u64, |held, (_, part)| held.saturating_add(budget::units(part.len()).saturating_add(32)))
        }
        _ => 0,
    }
}

//! embed: streams mode's record, for Clauflow's version comparison: what the decoded reader needs
//! that the save check keeps only as digests (paragraph origins, link targets, note and comment
//! ids, header references, field instructions, objects, text boxes). The walker fills it only when
//! asked; `inspect()` never asks, so the save check reads and answers exactly as before. Kept in
//! its own file so the walker's diff against upstream stays a handful of hooks.

use std::collections::BTreeMap;

use super::budget::{Budget, tree_slot, units, vector_slot};
use super::opc::Canon;
use super::story::Item;
use super::streams::{HeaderSlot, NoteKind, ObjectKind};
use super::xml::{XEl, XNode};

/// Memory charged for each fact recorded, besides its strings.
const EXTRA_BYTES: u64 = tree_slot::<usize, Extra>();
/// Memory charged for each mark's digest recorded: its entry and its 64 hex characters.
const DIGEST_BYTES: u64 = tree_slot::<u32, String>() + 64;
/// Memory charged for each text box's entry in the list of a part's text boxes.
pub(super) const TEXT_BOX_BYTES: u64 = vector_slot::<TextBox>();
/// Memory charged for each paragraph's origin.
const ORIGIN_BYTES: u64 = vector_slot::<(usize, u32)>();

/// What streams mode records for one story part.
#[derive(Default)]
pub(super) struct Extras {
    /// The part's folder, for resolving relationship targets.
    pub dir: String,
    /// Item index → what that item is beyond its mark.
    pub by_item: BTreeMap<usize, Extra>,
    /// Text boxes, each walked as its own story.
    pub text_boxes: Vec<TextBox>,
    /// Each `w:p` element's pre-order index in the part, by address, sorted by address.
    origins: Vec<(usize, u32)>,
    /// A mark's digest, as lowercase hex SHA-256, by piece.
    pub mark_digests: BTreeMap<u32, String>,
}

/// A text box's content, walked as a story of its own.
pub(super) struct TextBox {
    /// The origin of the paragraph whose run holds it.
    pub anchor: u32,
    /// Its order among that paragraph's text boxes.
    pub ordinal: u32,
    pub items: Vec<Item>,
    pub by_item: BTreeMap<usize, Extra>,
}

/// One item's facts beyond its mark.
pub(super) enum Extra {
    /// The start of the part's root or body: no content of its own.
    Root,
    /// A container's start (table, row, cell, content control, smart tag…).
    Open,
    /// A container's end.
    Close,
    /// A paragraph mark: its origin, and the section its properties end, if any (that section
    /// properties' format, alone, and its header and footer references).
    Paragraph {
        origin: u32,
        section: Option<(u32, SectionRefs)>,
    },
    /// Section properties at block level (the body's last section).
    Section(SectionRefs),
    Hyperlink {
        target: Option<String>,
        anchor: Option<String>,
    },
    SimpleField(String),
    /// A content control's start, with its properties interned as a format.
    ContentControl(u32),
    NoteStart {
        kind: NoteKind,
        id: String,
        separator: bool,
    },
    NoteEnd,
    NoteReference(NoteKind, String),
    Comment(CommentMark, String),
    Object {
        kind: ObjectKind,
        media: Vec<String>,
        alt: Option<String>,
    },
    /// What an opaque element's digest hides that a comparison reads as text (`StreamItem::Opaque`
    /// `detail`). Kept per item: identical elements share one digest.
    Detail(String),
}

impl Extra {
    /// What the fact holds beyond its entry.
    fn held(&self) -> u64 {
        let strings =
            |list: &[(HeaderSlot, String)]| list.iter().fold(0u64, |held, (_, part)| held.saturating_add(units(part.len()).saturating_add(32)));
        match self {
            Extra::Paragraph { section: Some((_, refs)), .. } | Extra::Section(refs) => strings(&refs.headers).saturating_add(strings(&refs.footers)),
            Extra::Hyperlink { target, anchor } => {
                units(target.as_ref().map_or(0, String::len)).saturating_add(units(anchor.as_ref().map_or(0, String::len)))
            }
            Extra::SimpleField(text)
            | Extra::NoteStart { id: text, .. }
            | Extra::NoteReference(_, text)
            | Extra::Comment(_, text)
            | Extra::Detail(text) => units(text.len()),
            Extra::Object { media, alt, .. } => {
                media.iter().fold(units(alt.as_ref().map_or(0, String::len)), |held, part| held.saturating_add(units(part.len()).saturating_add(24)))
            }
            _ => 0,
        }
    }
}

/// A section's header and footer references, as part names.
#[derive(Default)]
pub(super) struct SectionRefs {
    pub headers: Vec<(HeaderSlot, String)>,
    pub footers: Vec<(HeaderSlot, String)>,
}

#[derive(Clone, Copy)]
pub(super) enum CommentMark {
    Start,
    End,
    Reference,
}

impl Extras {
    /// The record for a part at `name`, its paragraphs numbered as the model reader numbers them.
    pub(super) fn new(root: &XEl, name: &str, budget: &mut Budget) -> Result<Extras, String> {
        let mut count = 0usize;
        count_paragraphs(root, &mut count, 0, budget)?;
        budget.hold(units(count).saturating_mul(ORIGIN_BYTES))?;
        let mut origins = Vec::with_capacity(count);
        number_paragraphs(root, &mut 0, &mut origins, 0);
        origins.sort_unstable();
        let dir = name.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("").to_string();
        budget.hold(units(dir.len()))?;
        Ok(Extras { dir, origins, ..Extras::default() })
    }

    /// The pre-order index of a `w:p` element of this part.
    pub(super) fn origin_of(&self, paragraph: &XEl) -> Option<u32> {
        let address = paragraph as *const XEl as usize;
        self.origins.binary_search_by_key(&address, |(at, _)| *at).ok().and_then(|index| self.origins.get(index)).map(|(_, origin)| *origin)
    }

    /// Record a fact for item `index`, charged before it is kept (a fact replacing another
    /// releases it).
    pub(super) fn record(&mut self, index: usize, extra: Extra, budget: &mut Budget) -> Result<(), String> {
        budget.hold(EXTRA_BYTES.saturating_add(extra.held()))?;
        if let Some(previous) = self.by_item.insert(index, extra) {
            budget.release(EXTRA_BYTES.saturating_add(previous.held()));
        }
        Ok(())
    }

    /// Record a mark's digest the first time the mark is met.
    pub(super) fn digest(&mut self, piece: u32, hex: impl FnOnce() -> String, budget: &mut Budget) -> Result<(), String> {
        if self.mark_digests.contains_key(&piece) {
            return Ok(());
        }
        budget.work(4)?;
        budget.hold(DIGEST_BYTES)?;
        self.mark_digests.insert(piece, hex());
        Ok(())
    }
}

fn count_paragraphs(el: &XEl, count: &mut usize, depth: usize, budget: &mut Budget) -> Result<(), String> {
    if depth > crate::xml::MAX_DEPTH {
        return Ok(());
    }
    for child in el.els() {
        budget.work(1)?;
        if &*child.name == "w:p" {
            *count = count.saturating_add(1);
        }
        count_paragraphs(child, count, depth + 1, budget)?;
    }
    Ok(())
}

/// The model reader's numbering (`read::number_paragraphs`): pre-order over every `w:p`, both
/// alternative-content branches and text boxes included, so `origin` equals `Origin.index`.
fn number_paragraphs(el: &XEl, next: &mut u32, by_address: &mut Vec<(usize, u32)>, depth: usize) {
    if depth > crate::xml::MAX_DEPTH {
        return;
    }
    for child in el.els() {
        if &*child.name == "w:p" {
            by_address.push((child as *const XEl as usize, *next));
            *next = next.saturating_add(1);
        }
        number_paragraphs(child, next, by_address, depth + 1);
    }
}

/// An internal relationship's target as a part name, or an external one's target as written.
fn target(canon: &Canon<'_>, dir: &str, id: &str) -> Option<(String, bool)> {
    let rel = canon.rels.get(id)?;
    Some(if rel.external { (rel.target.clone(), true) } else { (crate::package::resolve(dir, &rel.target), false) })
}

/// A hyperlink's target (its relationship's) and anchor.
pub(super) fn hyperlink(el: &XEl, canon: &Canon<'_>, dir: &str) -> Extra {
    let target = el.attr("r:id").and_then(|id| target(canon, dir, id)).map(|(target, _)| target);
    Extra::Hyperlink { target, anchor: el.attr("w:anchor").map(str::to_string) }
}

/// A section's header and footer references, resolved to part names.
pub(super) fn section_refs(section: &XEl, canon: &Canon<'_>, dir: &str) -> SectionRefs {
    let mut refs = SectionRefs::default();
    for child in section.els() {
        let list = match &*child.name {
            "w:headerReference" => &mut refs.headers,
            "w:footerReference" => &mut refs.footers,
            _ => continue,
        };
        let slot = match child.attr("w:type") {
            Some("first") => HeaderSlot::First,
            Some("even") => HeaderSlot::Even,
            _ => HeaderSlot::Default,
        };
        if let Some((part, false)) = child.attr("r:id").and_then(|id| target(canon, dir, id)) {
            list.push((slot, part));
        }
    }
    refs
}

/// The branch a reader takes first: an alternative-content element's first `mc:Choice` or
/// `mc:Fallback`, the element itself otherwise.
fn first_branch(el: &XEl) -> &XEl {
    if &*el.name == "mc:AlternateContent" { el.els().find(|branch| matches!(&*branch.name, "mc:Choice" | "mc:Fallback")).unwrap_or(el) } else { el }
}

/// An object's kind, the parts its pictures show and its alternative text, from its first branch.
pub(super) fn object(el: &XEl, canon: &Canon<'_>, dir: &str, budget: &mut Budget) -> Result<Extra, String> {
    let branch = first_branch(el);
    let mut kind = kind_of(&el.name);
    let (mut media, mut alt) = (Vec::new(), None);
    let mut stack = vec![(branch, 0usize)];
    while let Some((at, depth)) = stack.pop() {
        budget.work(1)?;
        if depth > crate::xml::MAX_DEPTH {
            continue;
        }
        if kind == ObjectKind::Other && !std::ptr::eq(at, branch) && kind_of(&at.name) != ObjectKind::Other {
            kind = kind_of(&at.name);
        }
        let reference = match &*at.name {
            "a:blip" => at.attr("r:embed").or_else(|| at.attr("r:link")),
            "v:imagedata" => at.attr("r:id"),
            "wp:docPr" if alt.is_none() => {
                alt = at.attr("descr").map(str::to_string);
                None
            }
            _ => None,
        };
        if let Some((part, false)) = reference.and_then(|id| target(canon, dir, id))
            && !media.contains(&part)
        {
            media.push(part);
        }
        // Children in document order: pushed in reverse.
        for child in at.els().collect::<Vec<_>>().into_iter().rev() {
            stack.push((child, depth + 1));
        }
    }
    Ok(Extra::Object { kind, media, alt })
}

fn kind_of(name: &str) -> ObjectKind {
    match name {
        "w:drawing" => ObjectKind::Drawing,
        "w:pict" => ObjectKind::Picture,
        "w:object" | "w:embed" => ObjectKind::Embedded,
        _ => ObjectKind::Other,
    }
}

/// The text boxes in an object's first branch, in document order; one inside another is found by
/// that text box's own walk.
pub(super) fn text_boxes<'e>(el: &'e XEl, budget: &mut Budget) -> Result<Vec<&'e XEl>, String> {
    fn find<'e>(el: &'e XEl, out: &mut Vec<&'e XEl>, depth: usize, budget: &mut Budget) -> Result<(), String> {
        if depth > crate::xml::MAX_DEPTH {
            return Ok(());
        }
        for child in el.els() {
            budget.work(1)?;
            match &*child.name {
                "w:txbxContent" => out.push(child),
                "mc:AlternateContent" => find(first_branch(child), out, depth + 1, budget)?,
                _ => find(child, out, depth + 1, budget)?,
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    find(first_branch(el), &mut out, 0, budget)?;
    Ok(out)
}

/// The most items walking `el`'s children can make, counted as the parser bounds a whole part: two
/// per element, a text element's characters, one per other text.
pub(super) fn item_bound(el: &XEl, budget: &mut Budget) -> Result<u64, String> {
    let text_element = matches!(&*el.name, "w:t" | "w:delText" | "w:instrText" | "w:delInstrText");
    let mut bound = 0u64;
    for node in &el.kids {
        budget.work(1)?;
        bound = bound.saturating_add(match node {
            XNode::El(child) => item_bound(child, budget)?.saturating_add(2),
            XNode::Text(text) if text_element => units(text.len()),
            XNode::Text(_) => 1,
        });
    }
    Ok(bound)
}

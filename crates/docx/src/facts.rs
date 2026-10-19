//! embed: what each paragraph shows that only the document model knows, for Clauflow's version
//! comparison: its list label as displayed, its list and level, its outline level, the text of
//! runs a reader would not see, and its text with every tracked change accepted with a fingerprint
//! of it (what Clauflow's locations are checked against). Paragraphs are named by part and origin,
//! as the save check's decoded streams name them (`inspect::streams`).

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::rc::Rc;

use wordcraft_doc::numbering::Counters;
use wordcraft_doc::para::OBJ;
use wordcraft_doc::props::{Rgb, TextColor};
use wordcraft_doc::styles::{PartSet, TableStyleProps};
use wordcraft_doc::table::Table;
use wordcraft_doc::{Block, Blocks, CharProps, Document, InlineObject, MAX_TABLE_DEPTH, ParaProps, Paragraph, PartKind, Path, StoryRef};

use crate::inspect::streams::sha256_hex;
use crate::package::Package;
use crate::{DocxError, ReadLimits};

/// How a run is kept from being seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conceal {
    Hidden,
    Colour,
    TinyFont,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParagraphFacts {
    /// The package part (by name) and `Origin.index`.
    pub part: String,
    pub origin: u32,
    /// The list label as displayed ("3.2(a)"), without its suffix.
    pub label: Option<String>,
    /// The list (`numId`) and level, when the paragraph is a list item.
    pub list: Option<(u32, u8)>,
    /// The abstract list (`abstractNumId`) whose counters the label follows: lists sharing one
    /// count together, so a restart through another list keeps it.
    pub abstract_list: Option<u32>,
    /// 0 for Heading 1 … 8, from the resolved paragraph properties.
    pub outline_level: Option<u8>,
    /// The text of runs that would not be seen: hidden, coloured like what is behind them
    /// (font colour within a small distance of highlight, shading or page colour), or below 2 pt.
    pub concealed: Vec<(Conceal, String)>,
    /// [`accepted_text`] of the paragraph, and its fingerprint.
    pub accepted: String,
    pub fingerprint: Fingerprint,
}

/// What a location names a paragraph's text by, so the host can tell it found the paragraph the
/// comparison meant: the engine's `Fingerprint`, computed the same way.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Fingerprint {
    /// Length in UTF-16 units of the accepted text.
    pub length: u32,
    /// First 16 hex digits of the SHA-256 of the accepted text with runs of spaces and tabs
    /// collapsed to one space and trimmed.
    pub hash: String,
}

/// The characters the document model keeps for an inline object, with every change accepted. The
/// rules are those Task A0 measured against the comparison engine's own block texts: an object
/// the engine writes as a digest placeholder is one character on both sides, so the two agree
/// without the embed having to form the digest.
fn accepted_object_text(object: &InlineObject) -> &str {
    match object {
        // Zero-width markers (bookmarks, comment ranges, permissions, field and control bounds).
        marker if marker.is_marker() => "",
        // What the object reads as: a field's cached result, an equation's linear form, a ruby
        // base, an opaque element's text.
        InlineObject::Field { .. } | InlineObject::Equation { .. } | InlineObject::Ruby { .. } | InlineObject::Opaque { .. } => object.plain_text(),
        // Note references, pictures, shapes, groups, charts and positional tabs: one character.
        // (`plain_text` gives a positional tab "\t" and a note reference nothing.)
        _ => "\u{FFFC}",
    }
}

/// Calls `visit(piece, start, end)` for each stretch of the paragraph's accepted text, in order,
/// until it breaks: `piece` is the text, `start..end` the bytes of `paragraph.text` it comes from.
/// Characters in tracked deletions (and move sources) are not visited; an object is one piece
/// (empty for a marker). The k-th U+FFFC is the k-th object, deleted ones counted.
pub fn for_each_accepted(paragraph: &Paragraph, mut visit: impl FnMut(&str, usize, usize) -> ControlFlow<()>) {
    let deleted = paragraph.deleted_ranges();
    let mut next_deleted = 0usize;
    let mut next_object = 0usize;
    let mut buffer = [0u8; 4];
    for (start, character) in paragraph.text.char_indices() {
        let end = start.saturating_add(character.len_utf8());
        let object = if character == OBJ {
            let object = paragraph.objects.get(next_object);
            next_object = next_object.saturating_add(1);
            Some(object)
        } else {
            None
        };
        while deleted.get(next_deleted).is_some_and(|range| range.end <= start) {
            next_deleted = next_deleted.saturating_add(1);
        }
        if deleted.get(next_deleted).is_some_and(|range| range.start <= start) {
            continue;
        }
        let piece = match object {
            Some(Some(object)) => accepted_object_text(object),
            // An object character the model holds no object for stays as written.
            _ => character.encode_utf8(&mut buffer),
        };
        if visit(piece, start, end).is_break() {
            return;
        }
    }
}

/// The paragraph's text as it reads with every tracked change accepted: runs inside a tracked
/// deletion are left out, insertions are kept. Objects and symbols read as Task A0 defined: see
/// `accepted_object_text`; the model's own tab, line, page and column break characters, hyphens
/// and symbol glyphs are kept, and hidden or coloured text is kept (the engine keeps it too).
pub fn accepted_text(paragraph: &Paragraph) -> String {
    let mut accepted = String::with_capacity(paragraph.text.len());
    for_each_accepted(paragraph, |piece, _, _| {
        accepted.push_str(piece);
        ControlFlow::Continue(())
    });
    accepted
}

/// The fingerprint of an accepted text, as the comparison engine makes it
/// (`services/document/src/compare/changes.rs`): the length in UTF-16 units of the text, and the
/// first 16 hex digits of the SHA-256 of the text with runs of spaces and tabs collapsed to one
/// space and trimmed.
pub fn fingerprint(accepted: &str) -> Fingerprint {
    let mut normalised = String::with_capacity(accepted.len());
    for character in accepted.chars() {
        if character == ' ' || character == '\t' {
            if !normalised.ends_with(' ') {
                normalised.push(' ');
            }
        } else {
            normalised.push(character);
        }
    }
    let hash = sha256_hex(normalised.trim().as_bytes());
    Fingerprint { length: u32::try_from(accepted.encode_utf16().count()).unwrap_or(u32::MAX), hash: hash.get(..16).unwrap_or(&hash).to_string() }
}

/// A UTF-16 offset in the [`accepted_text`] to the byte offset in `paragraph.text` that unit comes
/// from, which is the unit the model's own positions use. A unit inside a deletion's neighbours
/// maps to the next kept character (so "sixty" after a deleted "thirty" starts after it), every
/// unit of a field's result maps to the field's own character, and the offset one past the last
/// unit maps to just after the last kept character. `None` past that.
pub fn model_offset(paragraph: &Paragraph, accepted_utf16: u32) -> Option<usize> {
    let target = usize::try_from(accepted_utf16).ok()?;
    let mut units_before = 0usize;
    let mut after_last_kept = 0usize;
    let mut found = None;
    for_each_accepted(paragraph, |piece, start, end| {
        let units = piece.encode_utf16().count();
        // A marker has no unit of its own.
        if units == 0 {
            return ControlFlow::Continue(());
        }
        if target < units_before.saturating_add(units) {
            found = Some(start);
            return ControlFlow::Break(());
        }
        units_before = units_before.saturating_add(units);
        after_last_kept = end;
        ControlFlow::Continue(())
    });
    found.or((units_before == target).then_some(after_last_kept))
}

/// Where a range of accepted text that ends at UTF-16 offset `accepted_utf16` (exclusive) ends in
/// the model, as a byte offset into `paragraph.text`: just after the last kept unit before it.
/// [`model_offset`] answers where the unit *at* the offset starts, which skips a deletion that
/// follows the range, so a range ending just before a tracked deletion would swallow the deleted
/// wording. Offset 0 is an empty range at [`model_offset`]'s answer. `None` past the end of the
/// accepted text. A unit inside a field's result, or an object, ends at the end of that field or
/// object, as it starts at its start.
pub fn model_end_offset(paragraph: &Paragraph, accepted_utf16: u32) -> Option<usize> {
    if accepted_utf16 == 0 {
        return model_offset(paragraph, 0);
    }
    let last_unit = usize::try_from(accepted_utf16).ok()?.checked_sub(1)?;
    let mut units_before = 0usize;
    let mut found = None;
    for_each_accepted(paragraph, |piece, _, end| {
        let units = piece.encode_utf16().count();
        // A marker has no unit of its own.
        if units == 0 {
            return ControlFlow::Continue(());
        }
        if last_unit < units_before.saturating_add(units) {
            found = Some(end);
            return ControlFlow::Break(());
        }
        units_before = units_before.saturating_add(units);
        ControlFlow::Continue(())
    });
    found
}

/// Finds body paragraphs by (part name, `Origin.index`), what Clauflow's locations name.
pub struct Locator {
    /// (part index, `Origin.index`) → the paragraph's path, for every body paragraph.
    paths: HashMap<(u32, u32), Path>,
    part_indexes: HashMap<String, u32>,
    part_names: Vec<String>,
}

impl Locator {
    /// `part_names` are the package's, sorted: the order origins count parts in.
    pub fn new(document: &Document, part_names: &[String]) -> Self {
        let mut paths = HashMap::new();
        for path in document.para_paths(StoryRef::Body) {
            if let Some(origin) = document.para(StoryRef::Body, &path).and_then(|paragraph| paragraph.origin) {
                paths.insert((origin.part, origin.index), path);
            }
        }
        let part_indexes = part_names.iter().enumerate().filter_map(|(index, name)| Some((name.clone(), u32::try_from(index).ok()?))).collect();
        Locator { paths, part_indexes, part_names: part_names.to_vec() }
    }

    /// Any body paragraph, including one inside a table (the engine names a row or cell by its
    /// first paragraph's origin).
    pub fn path(&self, part: &str, origin: u32) -> Option<Path> {
        self.paths.get(&(*self.part_indexes.get(part)?, origin)).cloned()
    }

    /// The top-level body block (`[i]`) a paragraph path belongs to.
    pub fn block_of(path: &Path) -> Path {
        Path(path.0.iter().take(1).copied().collect())
    }

    /// The (part name, origin) of the body paragraph at `path`, or of the first paragraph of the
    /// table at `path` (the engine names a table by its first paragraph).
    pub fn name(&self, document: &Document, path: &Path) -> Option<(String, u32)> {
        let origin = match document.block(StoryRef::Body, path)? {
            Block::Para(paragraph) => paragraph.origin?,
            Block::Table(table) => first_origin(table, 0)?,
        };
        Some((self.part_names.get(usize::try_from(origin.part).ok()?)?.clone(), origin.index))
    }
}

/// The origin of the first paragraph of `table` in document order (a table's first cell may hold
/// none, or only a nested table).
fn first_origin(table: &Table, depth: usize) -> Option<wordcraft_doc::Origin> {
    if depth > MAX_TABLE_DEPTH {
        return None;
    }
    table.rows.iter().flat_map(|row| &row.cells).flat_map(|cell| &cell.blocks).find_map(|block| match block.as_ref() {
        Block::Para(paragraph) => paragraph.origin,
        Block::Table(inner) => first_origin(inner, depth.saturating_add(1)),
    })
}

/// The package's part names, sorted: what [`Locator::new`] takes, read from the archive's entry
/// list alone (nothing is inflated or parsed), for a caller that already holds the model.
pub fn part_names(bytes: &[u8]) -> Result<Vec<String>, DocxError> {
    let mut names = crate::package::entry_order(bytes)?;
    // The package keeps its parts in a sorted map; origins count parts in that order.
    names.sort();
    Ok(names)
}

/// The model of `bytes` and the package's part names, sorted: what [`Locator::new`] takes. The
/// package is opened twice, as [`paragraph_facts`] does, and only the names are kept.
pub fn read_with_part_names(bytes: &[u8], limits: &ReadLimits) -> Result<(Document, Vec<String>), DocxError> {
    // Origins name parts by their index in the package's sorted part names.
    let part_names: Vec<String> = Package::open_with(bytes, limits)?.part_names().map(str::to_string).collect();
    let document = crate::read_with_limits(bytes, limits)?;
    Ok((document, part_names))
}

/// Font colours this close to what is behind them (largest channel difference) are not seen.
const COLOUR_DISTANCE: u8 = 24;
/// Text smaller than this, in points, is not seen.
const TINY_POINTS: f32 = 2.0;

/// Facts for every paragraph of the body, headers, footers, notes and text boxes, in document
/// order. Paragraphs without an origin (a drop cap merged into its paragraph) are left out.
pub fn paragraph_facts(bytes: &[u8], limits: &ReadLimits) -> Result<Vec<ParagraphFacts>, DocxError> {
    // Only the part names are kept, so the inflated package is not held beside the model.
    let (doc, part_names) = read_with_part_names(bytes, limits)?;
    let rank = |kind: PartKind| match kind {
        PartKind::Header => Some(1),
        PartKind::Footer => Some(2),
        PartKind::Footnote => Some(3),
        PartKind::Endnote => Some(4),
        PartKind::TextBox => Some(5),
        PartKind::Comment => None,
    };
    let mut stories: Vec<(u8, StoryRef)> = vec![(0, StoryRef::Body)];
    stories.extend(doc.parts.iter().filter_map(|(id, part)| rank(part.kind).map(|rank| (rank, StoryRef::Part(*id)))));
    // Stable: parts of one kind stay in the model's order.
    stories.sort_by_key(|(rank, _)| *rank);
    let mut walk = Walk::new(&doc, &part_names);
    for (_, story) in stories {
        // One story's lists count on their own, as layout counts them.
        walk.counters = Counters::default();
        if let Some(blocks) = doc.story(story) {
            walk.blocks(blocks, None, 0)?;
        }
    }
    Ok(walk.out)
}

/// The most work a reading may take, in the units [`Walk::charge`] counts: the model's style and
/// list lookups scan every style or list, so a file with many of each and many runs is refused
/// rather than ground through (the save check has the same limit, `inspect::MAX_WORK`).
const MAX_WORK: u64 = 400_000_000;

/// What the table cell a paragraph is in gives its text, as layout gives it: the innermost
/// cell's table style formatting for its region (header row, bands, …), and what is painted
/// behind the cell (its own shading, else its table style region's, else its table's, else the
/// cell around its table).
struct InCell {
    para: ParaProps,
    chr: CharProps,
    fill: Option<Rgb>,
}

/// One walk over the stories, in document order.
struct Walk<'a> {
    doc: &'a Document,
    part_names: &'a [String],
    counters: Counters,
    /// Table styles with their chains merged, by id.
    table_styles: HashMap<&'a str, Option<Rc<TableStyleProps>>>,
    /// Style chain lengths, by id.
    chains: HashMap<&'a str, u64>,
    work_left: u64,
    out: Vec<ParagraphFacts>,
}

impl<'a> Walk<'a> {
    fn new(doc: &'a Document, part_names: &'a [String]) -> Walk<'a> {
        Walk {
            doc,
            part_names,
            counters: Counters::default(),
            table_styles: HashMap::new(),
            chains: HashMap::new(),
            work_left: MAX_WORK,
            out: Vec::new(),
        }
    }

    fn charge(&mut self, units: u64) -> Result<(), DocxError> {
        self.work_left =
            self.work_left.checked_sub(units).ok_or_else(|| DocxError::Limit("paragraph facts: too many style and list lookups".into()))?;
        Ok(())
    }

    /// One scan of the style sheet.
    fn style_scan(&self) -> u64 {
        u64::try_from(self.doc.styles.styles.len()).unwrap_or(u64::MAX).saturating_add(1)
    }

    /// Charge resolving through style `id`'s chain (a scan per style in it), learning the chain's
    /// length the first time.
    fn charge_chain(&mut self, id: Option<&'a str>) -> Result<(), DocxError> {
        let id = id.unwrap_or("Normal");
        let length = match self.chains.get(id) {
            Some(length) => *length,
            None => {
                // The chain is at most 32 styles (`StyleSheet::chain`).
                self.charge(self.style_scan().saturating_mul(33))?;
                let length = u64::try_from(self.doc.styles.chain(id).len()).unwrap_or(u64::MAX);
                self.chains.insert(id, length);
                length
            }
        };
        self.charge(self.style_scan().saturating_mul(length.saturating_add(1)))
    }

    /// Charge a list label: each of the few levels it reads scans the lists and abstract lists.
    fn charge_label(&mut self) -> Result<(), DocxError> {
        let numbering = &self.doc.numbering;
        let scan = u64::try_from(numbering.nums.len().saturating_add(numbering.abstracts.len())).unwrap_or(u64::MAX).saturating_add(1);
        self.charge(scan.saturating_mul(24))
    }

    fn table_style(&mut self, id: &'a str) -> Result<Option<Rc<TableStyleProps>>, DocxError> {
        if let Some(found) = self.table_styles.get(id) {
            return Ok(found.clone());
        }
        self.charge(self.style_scan().saturating_mul(33))?;
        let found = self.doc.styles.table_style(id).map(Rc::new);
        self.table_styles.insert(id, found.clone());
        Ok(found)
    }

    /// Paragraphs and tables, to the depth the model lists paragraphs to (`Document::para_paths`).
    fn blocks(&mut self, blocks: &'a Blocks, cell: Option<&InCell>, depth: usize) -> Result<(), DocxError> {
        if depth > MAX_TABLE_DEPTH {
            return Ok(());
        }
        for block in blocks {
            match block.as_ref() {
                Block::Para(paragraph) => self.paragraph(paragraph, cell)?,
                Block::Table(table) => self.table(table, cell, depth)?,
            }
        }
        Ok(())
    }

    fn table(&mut self, table: &'a Table, outer: Option<&InCell>, depth: usize) -> Result<(), DocxError> {
        let style = match table.props.style.as_deref() {
            Some(id) => self.table_style(id)?,
            None => None,
        };
        for (row_index, row) in table.rows.iter().enumerate() {
            // Grid columns are counted from the row's first cell, as layout counts them.
            let mut column = 0usize;
            for cell in &row.cells {
                let (mut para, mut chr, mut region_fill) = (ParaProps::default(), CharProps::default(), None);
                if let Some(style) = &style {
                    let region = PartSet::of_cell(
                        &table.props.look,
                        style.parts.band_size.unwrap_or(1),
                        (row_index, table.rows.len()),
                        (column, cell.span(), table.cols()),
                    );
                    para = style.para.clone();
                    // Lowest precedence first (ECMA-376 §17.7.6).
                    for part in region.iter() {
                        let format = style.part(part);
                        chr.overlay(&format.chr);
                        para.align = format.align.or(para.align);
                        region_fill = format.fill.or(region_fill);
                    }
                }
                let fill = cell.props.shading.or(region_fill).or(table.props.shading).or(outer.and_then(|outer| outer.fill));
                self.blocks(&cell.blocks, Some(&InCell { para, chr, fill }), depth.saturating_add(1))?;
                column = column.saturating_add(cell.span());
            }
        }
        Ok(())
    }

    fn paragraph(&mut self, paragraph: &'a Paragraph, cell: Option<&InCell>) -> Result<(), DocxError> {
        let doc = self.doc;
        let style = paragraph.props.style.as_deref();
        self.charge_chain(style)?;
        let resolved = doc.styles.resolve_para_in(&paragraph.props, cell.map(|cell| &cell.para));
        let numbering = paragraph.props.numbering.or(resolved.numbering).filter(|numbering| numbering.num != 0);
        // Counted even for a paragraph left out, so the labels after it stay right.
        let label = match numbering {
            Some(numbering) => {
                self.charge_label()?;
                self.counters.next_label(&doc.numbering, numbering.num, numbering.level).map(|(text, _)| text)
            }
            None => None,
        };
        let Some(origin) = paragraph.origin else { return Ok(()) };
        let Some(part) = usize::try_from(origin.part).ok().and_then(|index| self.part_names.get(index)) else { return Ok(()) };
        let mut concealed: Vec<(Conceal, String)> = Vec::new();
        // How the run before was concealed: neighbouring runs concealed the same way are one passage.
        let mut previous: Option<Conceal> = None;
        let mut start = 0usize;
        for run in &paragraph.runs {
            let end = start.saturating_add(run.len);
            let text = paragraph.text.get(start..end).unwrap_or("");
            start = end;
            if text.is_empty() {
                continue;
            }
            self.charge_chain(style)?;
            self.charge_chain(run.props.style.as_deref())?;
            let character = doc.styles.resolve_char_in(style, cell.map(|cell| &cell.chr), &run.props);
            let behind = character
                .highlight
                .or(character.shading)
                .or(resolved.shading)
                .or(cell.and_then(|cell| cell.fill))
                .or(doc.settings.page_color)
                .unwrap_or(Rgb(255, 255, 255));
            let conceal = if character.hidden {
                Some(Conceal::Hidden)
            } else if matches!(character.color, TextColor::Rgb(colour) if distance(colour, behind) <= COLOUR_DISTANCE) {
                Some(Conceal::Colour)
            } else if character.size < TINY_POINTS {
                Some(Conceal::TinyFont)
            } else {
                None
            };
            match (conceal, concealed.last_mut()) {
                (Some(conceal), Some((kind, passage))) if previous == Some(conceal) && *kind == conceal => passage.push_str(text),
                (Some(conceal), _) => concealed.push((conceal, text.to_string())),
                (None, _) => {}
            }
            previous = conceal;
        }
        let accepted = accepted_text(paragraph);
        self.out.push(ParagraphFacts {
            part: part.clone(),
            origin: origin.index,
            label,
            list: numbering.map(|numbering| (numbering.num, numbering.level)),
            abstract_list: numbering.and_then(|numbering| doc.numbering.abstract_of(numbering.num)).map(|list| list.id),
            outline_level: resolved.outline_level.filter(|level| *level < 9),
            concealed,
            fingerprint: fingerprint(&accepted),
            accepted,
        });
        Ok(())
    }
}

/// The largest difference between two colours' channels.
fn distance(a: Rgb, b: Rgb) -> u8 {
    a.0.abs_diff(b.0).max(a.1.abs_diff(b.1)).max(a.2.abs_diff(b.2))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use crate::inspect::streams::sha256_hex;
    use wordcraft_doc::para::{Float, NoteKind, PTabAlign, PTabBase};
    use wordcraft_doc::props::TabLeader;
    use wordcraft_doc::{InlineObject, Path, Run};

    const LIMITS: ReadLimits = ReadLimits { max_part_bytes: 64 << 20, max_total_bytes: 192 << 20, max_elements: 2_000_000, max_depth: 64 };

    #[test]
    fn labels_are_materialised_per_list_and_level() {
        let bytes = test_package::numbered(&[(0, "Definitions"), (1, "Goods"), (1, "Services"), (0, "Payment")]);
        let facts = paragraph_facts(&bytes, &LIMITS).unwrap();
        let labels: Vec<Option<&str>> = facts.iter().map(|fact| fact.label.as_deref()).collect();
        assert_eq!(labels, vec![Some("1."), Some("1.1"), Some("1.2"), Some("2.")]);
        assert_eq!(facts[1].list, Some((1, 1)));
        assert_eq!(facts[0].list, Some((1, 0)));
    }

    #[test]
    fn a_restart_through_another_list_keeps_the_abstract_list_its_counters_follow() {
        let bytes = test_package::restarted(&[(0, "Definitions"), (0, "Price")], 1, 5);
        let facts = paragraph_facts(&bytes, &LIMITS).unwrap();
        let seen: Vec<(Option<&str>, Option<(u32, u8)>, Option<u32>)> =
            facts.iter().map(|fact| (fact.label.as_deref(), fact.list, fact.abstract_list)).collect();
        assert_eq!(seen, vec![(Some("1."), Some((1, 0)), Some(0)), (Some("5."), Some((2, 0)), Some(0))]);
    }

    #[test]
    fn headings_carry_their_outline_level() {
        let bytes = test_package::styled(&[("Heading1", "Payment"), ("Normal", "Text")]);
        let facts = paragraph_facts(&bytes, &LIMITS).unwrap();
        assert_eq!(facts[0].outline_level, Some(0));
        assert_eq!(facts[1].outline_level, None);
        assert_eq!(facts[1].label, None);
    }

    #[test]
    fn concealed_runs_are_listed() {
        let bytes = test_package::runs(&[
            ("Visible ", ""),
            ("secret", "<w:vanish/>"),
            ("white", r#"<w:color w:val="FFFFFF"/>"#),
            ("tiny", r#"<w:sz w:val="2"/>"#),
            (" more", r#"<w:sz w:val="2"/>"#),
            ("grey on grey", r#"<w:color w:val="404040"/><w:shd w:val="clear" w:fill="383838"/>"#),
            (" red is seen", r#"<w:color w:val="FF0000"/>"#),
        ]);
        let facts = paragraph_facts(&bytes, &LIMITS).unwrap();
        assert_eq!(
            facts[0].concealed,
            vec![
                (Conceal::Hidden, "secret".into()),
                (Conceal::Colour, "white".into()),
                (Conceal::TinyFont, "tiny more".into()),
                (Conceal::Colour, "grey on grey".into()),
            ]
        );
    }

    #[test]
    fn concealment_by_a_table_style_is_listed() {
        // The style hides the header row's text and colours the rest white.
        let bytes = test_package::table_styled(&["Hidden header", "Hidden too"], &["White cell", "Also white"]);
        let facts = paragraph_facts(&bytes, &LIMITS).unwrap();
        let concealed: Vec<&(Conceal, String)> = facts.iter().flat_map(|fact| &fact.concealed).collect();
        assert_eq!(
            concealed,
            vec![
                &(Conceal::Hidden, "Hidden header".into()),
                &(Conceal::Hidden, "Hidden too".into()),
                &(Conceal::Colour, "White cell".into()),
                &(Conceal::Colour, "Also white".into()),
            ]
        );
    }

    #[test]
    fn text_coloured_like_its_cell_is_concealed_and_light_text_on_a_dark_cell_is_not() {
        let bytes = test_package::shaded_cells(&[("000000", "000000", "Black on black"), ("000000", "FFFFFF", "White on black")]);
        let facts = paragraph_facts(&bytes, &LIMITS).unwrap();
        assert_eq!(facts[0].concealed, vec![(Conceal::Colour, "Black on black".into())]);
        assert_eq!(facts[1].concealed, vec![]);
    }

    #[test]
    fn an_inner_cell_without_shading_sits_on_the_cell_around_it() {
        let bytes = test_package::nested_in_black_cell("Black in a black cell");
        let facts = paragraph_facts(&bytes, &LIMITS).unwrap();
        let fact = facts.iter().find(|fact| !fact.concealed.is_empty()).unwrap();
        assert_eq!(fact.concealed, vec![(Conceal::Colour, "Black in a black cell".into())]);
    }

    #[test]
    fn style_lookups_past_the_work_limit_are_refused() {
        // Runs name character styles among many (the reader keeps about 10,000): each lookup
        // scans them all.
        let bytes = test_package::many_styles(10_000, 50_000);
        assert!(matches!(paragraph_facts(&bytes, &LIMITS), Err(DocxError::Limit(_))));
    }

    #[test]
    fn a_long_ordinary_agreement_is_read_within_the_limit() {
        let facts = paragraph_facts(&test_package::many_styles(300, 100_000), &LIMITS).unwrap();
        assert_eq!(facts.len(), 1);
        let facts = paragraph_facts(&test_package::many_lists(50, 20_000), &LIMITS).unwrap();
        assert_eq!(facts.len(), 20_000);
    }

    #[test]
    fn list_lookups_past_the_work_limit_are_refused() {
        let bytes = test_package::many_lists(9_000, 50_000);
        assert!(matches!(paragraph_facts(&bytes, &LIMITS), Err(DocxError::Limit(_))));
    }

    #[test]
    fn origins_and_parts_name_the_paragraphs() {
        let bytes = test_package::numbered(&[(0, "One"), (0, "Two")]);
        let facts = paragraph_facts(&bytes, &LIMITS).unwrap();
        assert_eq!((facts[1].part.as_str(), facts[1].origin), ("word/document.xml", 1));
    }

    #[test]
    fn headers_count_their_own_lists() {
        let bytes = test_package::with_header(&[(0, "Body one"), (0, "Body two")], &[(0, "Header item")]);
        let facts = paragraph_facts(&bytes, &LIMITS).unwrap();
        let header = facts.iter().find(|fact| fact.part == "word/header1.xml").unwrap();
        assert_eq!((header.origin, header.label.as_deref()), (0, Some("1.")));
        assert_eq!(facts[0].part, "word/document.xml", "the body comes first");
    }

    fn text_run(text: &str) -> String {
        format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
    }

    fn deleted_run(text: &str) -> String {
        format!(
            r#"<w:del w:id="1" w:author="Ana" w:date="2026-10-01T09:00:00Z"><w:r><w:delText xml:space="preserve">{text}</w:delText></w:r></w:del>"#
        )
    }

    fn inserted_run(text: &str) -> String {
        format!(r#"<w:ins w:id="2" w:author="Ana" w:date="2026-10-01T09:00:00Z">{}</w:ins>"#, text_run(text))
    }

    /// The model of a package whose body is `paragraphs`, each the content of one `w:p`.
    fn document_of(paragraphs: &[String]) -> Document {
        let body: String = paragraphs.iter().map(|content| format!("<w:p>{content}</w:p>")).collect();
        crate::read_with_limits(&test_package::custom_with_comment(&body), &LIMITS).unwrap()
    }

    fn first_paragraph(document: &Document) -> &Paragraph {
        document.para(StoryRef::Body, &Path(vec![0])).unwrap()
    }

    /// "Rent is due thirty[deleted]sixty[inserted] days."
    fn rent_paragraph() -> Document {
        document_of(&[format!("{}{}{}{}", text_run("Rent is due "), deleted_run("thirty"), inserted_run("sixty"), text_run(" days."))])
    }

    fn accepted_of(content: &str) -> String {
        accepted_text(first_paragraph(&document_of(&[content.to_string()])))
    }

    #[test]
    fn accepted_text_leaves_deleted_runs_out_and_keeps_insertions() {
        assert_eq!(accepted_text(first_paragraph(&rent_paragraph())), "Rent is due sixty days.");
    }

    #[test]
    fn fingerprints_collapse_spaces_and_tabs_like_the_engine() {
        let fingerprint = fingerprint("  Rent\t is   due ");
        assert_eq!(fingerprint.length, 17);
        assert_eq!(fingerprint.hash, &sha256_hex(b"Rent is due")[..16]);
    }

    #[test]
    fn fingerprints_count_utf16_units_not_characters() {
        // One emoji is a surrogate pair; the web counts two units.
        assert_eq!(fingerprint("a😀").length, 3);
    }

    #[test]
    fn accepted_offsets_skip_deleted_runs() {
        let document = rent_paragraph();
        let paragraph = first_paragraph(&document);
        // "sixty" starts at accepted offset 12; in the model, after "thirty", at 18.
        assert_eq!(model_offset(paragraph, 0), Some(0));
        assert_eq!(model_offset(paragraph, 12), Some(18));
        assert_eq!(model_offset(paragraph, 23), Some(29));
        assert_eq!(model_offset(paragraph, 24), None);
    }

    #[test]
    fn a_range_ends_after_its_last_kept_unit_not_after_a_following_deletion() {
        // "Rent is due [thirty]sixty days.": "sixty" is accepted units 12..17.
        let document = rent_paragraph();
        let paragraph = first_paragraph(&document);
        assert_eq!(model_end_offset(paragraph, 17), Some(23), "just after sixty");
        assert_eq!(model_end_offset(paragraph, 12), Some(12), "just after the space, before the deleted thirty");
        assert_eq!(model_offset(paragraph, 12), Some(18), "while a start skips the deletion");
        assert_eq!(model_end_offset(paragraph, 0), Some(0));
        assert_eq!(model_end_offset(paragraph, 23), Some(29));
        assert_eq!(model_end_offset(paragraph, 24), None);
        // A trailing deletion is never part of the range.
        let trailing = document_of(&[format!("{}{}", text_run("abc"), deleted_run("def"))]);
        assert_eq!(model_end_offset(first_paragraph(&trailing), 3), Some(3));
        // Units are UTF-16, offsets bytes: "é😀" is 3 units, 6 bytes.
        let wide = document_of(&[format!("{}{}{}", text_run("é😀"), deleted_run("x"), text_run("y"))]);
        assert_eq!(model_end_offset(first_paragraph(&wide), 3), Some(6));
        assert_eq!(model_end_offset(first_paragraph(&wide), 4), Some(8));
    }

    #[test]
    fn model_offsets_are_bytes_of_the_paragraph_text_and_accepted_offsets_are_utf16_units() {
        // "é" is two bytes and one unit; the emoji four bytes and two units; "x" is deleted.
        let document = document_of(&[format!("{}{}{}", text_run("é😀"), deleted_run("x"), text_run("y"))]);
        let paragraph = first_paragraph(&document);
        assert_eq!(accepted_text(paragraph), "é😀y");
        assert_eq!(model_offset(paragraph, 1), Some(2));
        assert_eq!(model_offset(paragraph, 3), Some(7), "y follows the deleted x");
        assert_eq!(model_offset(paragraph, 4), Some(8));
        assert_eq!(model_offset(paragraph, 5), None);
    }

    #[test]
    fn the_end_of_the_accepted_text_stops_before_a_trailing_deletion() {
        let document = document_of(&[format!("{}{}", text_run("abc"), deleted_run("def"))]);
        let paragraph = first_paragraph(&document);
        assert_eq!(accepted_text(paragraph), "abc");
        assert_eq!(model_offset(paragraph, 3), Some(3));
        assert_eq!(model_offset(paragraph, 4), None);
    }

    #[test]
    fn comment_range_markers_contribute_nothing() {
        let content = format!(
            r#"{}<w:commentRangeStart w:id="0"/>{}<w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r>{}"#,
            text_run("Term: "),
            text_run("two years"),
            text_run(".")
        );
        let document = document_of(&[content]);
        let paragraph = first_paragraph(&document);
        assert!(paragraph.objects.iter().any(wordcraft_doc::InlineObject::is_marker), "the model holds the markers");
        assert_eq!(accepted_text(paragraph), "Term: two years.");
        // "two" starts at accepted offset 6, after the start marker.
        let offset = model_offset(paragraph, 6).unwrap();
        assert_eq!(paragraph.text.get(offset..offset + 3), Some("two"));
    }

    #[test]
    fn a_field_contributes_its_result_and_maps_to_its_own_position() {
        let field = r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> DATE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>18 October 2026</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#;
        let document = document_of(&[format!("{}{field}{}", text_run("On "), text_run(" ok"))]);
        let paragraph = first_paragraph(&document);
        assert_eq!(accepted_text(paragraph), "On 18 October 2026 ok");
        // Every unit of the result maps to the field's one character in the model.
        let at_field = model_offset(paragraph, 3);
        assert_eq!(at_field, Some(3));
        assert_eq!(model_offset(paragraph, 10), at_field);
        // The text after it maps past the field.
        assert_eq!(model_offset(paragraph, 18), Some(3 + '\u{FFFC}'.len_utf8()));
    }

    #[test]
    fn a_deleted_object_does_not_shift_the_next_one() {
        // "￼x￼": the first field sits in a deleted run, the second is kept. The k-th object
        // counts every U+FFFC, deleted or not.
        let field = |result: &str| InlineObject::Field {
            instr: " DATE ".into(),
            result: result.into(),
            locked: false,
            dirty: false,
            code: false,
            form: String::new(),
        };
        let deleted = CharProps { del: Some(0), ..CharProps::default() };
        let mut paragraph = Paragraph::new();
        paragraph.text = "\u{FFFC}x\u{FFFC}".into();
        paragraph.runs =
            vec![Run { len: 3, props: deleted }, Run { len: 1, props: CharProps::default() }, Run { len: 3, props: CharProps::default() }];
        paragraph.objects = vec![field("OLD"), field("NEW")];
        assert_eq!(accepted_text(&paragraph), "xNEW");
        assert_eq!(model_offset(&paragraph, 0), Some(3));
        assert_eq!(model_offset(&paragraph, 1), Some(4));
    }

    #[test]
    fn a_note_reference_a_picture_and_a_positional_tab_are_one_object_character_each() {
        // Built by hand: `with_text` strips object characters.
        let mut paragraph = Paragraph::new();
        paragraph.text = "Price\u{FFFC}\u{FFFC}\u{FFFC}".into();
        paragraph.runs = vec![Run { len: paragraph.text.len(), props: CharProps::default() }];
        paragraph.objects = vec![
            InlineObject::NoteRef { kind: NoteKind::Footnote, id: 1, custom: String::new() },
            InlineObject::Image { media: "image1".into(), w: 10.0, h: 10.0, alt: String::new(), float: Float::default(), crop: [0.0; 4], ole: None },
            InlineObject::PositionalTab { align: PTabAlign::Right, relative_to: PTabBase::Margin, leader: TabLeader::None },
        ];
        // Not the 9-unit placeholder the engine reads for a note reference, and not the "\t" the
        // model's own plain text gives a positional tab.
        assert_eq!(accepted_text(&paragraph), "Price\u{FFFC}\u{FFFC}\u{FFFC}");
        assert_eq!(fingerprint(&accepted_text(&paragraph)).length, 8);
    }

    #[test]
    fn a_picture_in_a_document_is_one_object_character() {
        let picture =
            r#"<w:r><w:pict><v:rect xmlns:v="urn:schemas-microsoft-com:vml" style="width:20pt;height:20pt" fillcolor="red"/></w:pict></w:r>"#;
        assert_eq!(accepted_of(&format!("{}{picture}{}", text_run("See "), text_run(" here"))), "See \u{FFFC} here");
    }

    #[test]
    fn tabs_and_breaks_keep_the_characters_the_model_stores() {
        let content = format!(
            "{}<w:r><w:tab/></w:r>{}<w:r><w:br/></w:r>{}<w:r><w:br w:type=\"page\"/></w:r>{}<w:r><w:br w:type=\"column\"/></w:r>{}<w:r><w:noBreakHyphen/><w:softHyphen/></w:r>",
            text_run("1."),
            text_run("Definitions"),
            text_run("a"),
            text_run("b"),
            text_run("c")
        );
        assert_eq!(accepted_of(&content), "1.\tDefinitions\na\u{000C}b\u{000E}c\u{2011}\u{00AD}");
    }

    #[test]
    fn a_symbol_is_its_glyph_and_a_deleted_tab_is_left_out() {
        let content = format!(
            r#"{}<w:r><w:sym w:font="Wingdings" w:char="F0FC"/></w:r><w:del w:id="3" w:author="Ana"><w:r><w:tab/></w:r></w:del>{}"#,
            text_run("ok "),
            text_run("end")
        );
        assert_eq!(accepted_of(&content), "ok \u{F0FC}end");
    }

    #[test]
    fn hidden_and_coloured_text_stays_in_the_accepted_text() {
        let content = r#"<w:r><w:rPr><w:vanish/></w:rPr><w:t>secret </w:t></w:r><w:r><w:t>shown</w:t></w:r>"#;
        assert_eq!(accepted_of(content), "secret shown");
    }

    fn sample_with_table() -> Vec<u8> {
        let cell = |text: &str| format!("<w:tc><w:p>{}</w:p></w:tc>", text_run(text));
        let row = |first: &str, second: &str| format!("<w:tr>{}{}</w:tr>", cell(first), cell(second));
        // Origins by pre-order: 0 opening, 1..=4 the cells, 5 closing.
        let body = format!(
            "<w:p>{}</w:p><w:tbl><w:tblPr/><w:tblGrid><w:gridCol/><w:gridCol/></w:tblGrid>{}{}</w:tbl><w:p>{}</w:p>",
            text_run("Opening paragraph."),
            row("a1", "a2"),
            row("b1", "b2"),
            text_run("Closing paragraph.")
        );
        test_package::custom(&body)
    }

    #[test]
    fn the_part_names_from_the_entry_list_are_the_packages_sorted_names() {
        let bytes = sample_with_table();
        let (_, from_package) = read_with_part_names(&bytes, &LIMITS).unwrap();
        assert_eq!(part_names(&bytes).unwrap(), from_package);
        assert!(part_names(b"not a zip").is_err());
    }

    #[test]
    fn the_locator_finds_body_paragraphs_and_tables_by_their_first_paragraph() {
        let bytes = sample_with_table();
        let (document, part_names) = read_with_part_names(&bytes, &LIMITS).unwrap();
        let locator = Locator::new(&document, &part_names);
        let body = "word/document.xml";
        assert_eq!(locator.path(body, 0), Some(Path(vec![0])));
        // Origin 3 is the first paragraph of the table's second row, first cell.
        assert_eq!(locator.path(body, 3), Some(Path(vec![1, 1, 0, 0])));
        assert_eq!(Locator::block_of(&Path(vec![1, 1, 0, 0])), Path(vec![1]));
        assert_eq!(Locator::block_of(&Path(vec![2])), Path(vec![2]));
        // Origin 1 is the first paragraph inside the table: the table block is [1].
        assert_eq!(locator.name(&document, &Path(vec![1])), Some((body.to_string(), 1)));
        assert_eq!(locator.name(&document, &Path(vec![0])), Some((body.to_string(), 0)));
        assert_eq!(locator.name(&document, &Path(vec![1, 1, 0, 0])), Some((body.to_string(), 3)));
        assert_eq!(locator.path(body, 5), Some(Path(vec![2])));
        assert_eq!(locator.path(body, 99), None);
        assert_eq!(locator.path("word/header1.xml", 0), None);
        assert_eq!(locator.name(&document, &Path(vec![9])), None);
    }

    #[test]
    fn a_locator_name_finds_its_own_path_again() {
        let bytes = sample_with_table();
        let (document, part_names) = read_with_part_names(&bytes, &LIMITS).unwrap();
        let locator = Locator::new(&document, &part_names);
        for path in document.para_paths(StoryRef::Body) {
            let (part, origin) = locator.name(&document, &path).unwrap();
            assert_eq!(locator.path(&part, origin), Some(path));
        }
    }

    #[test]
    fn paragraph_facts_carry_the_model_fingerprint() {
        let facts = paragraph_facts(&sample_with_table(), &LIMITS).unwrap();
        let first = facts.iter().find(|fact| fact.origin == 0).unwrap();
        assert_eq!(first.accepted, "Opening paragraph.");
        assert_eq!(first.fingerprint, fingerprint("Opening paragraph."));
    }

    #[test]
    fn paragraph_facts_fingerprint_the_accepted_text() {
        let bytes = test_package::custom(&format!("<w:p>{}{}{}</w:p>", text_run("Due in "), deleted_run("thirty"), inserted_run("sixty")));
        let facts = paragraph_facts(&bytes, &LIMITS).unwrap();
        assert_eq!(facts[0].accepted, "Due in sixty");
        assert_eq!(facts[0].fingerprint.length, 12);
    }

    /// Packages built from zip entries: a multilevel list ("%1." and "%1.%2"), and the styles
    /// Heading1 (outline level 0) and Normal.
    mod test_package {
        use crate::package::zip_entries;

        const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
        const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

        fn zip(body: &str, header: Option<&str>) -> Vec<u8> {
            build(body, header, "", "")
        }

        fn build(body: &str, header: Option<&str>, extra_styles: &str, extra_nums: &str) -> Vec<u8> {
            build_with_comments(body, header, extra_styles, extra_nums, false)
        }

        /// With `comments`, a comments part holding comment 0: the model keeps comment ranges only
        /// for comments it knows.
        fn build_with_comments(body: &str, header: Option<&str>, extra_styles: &str, extra_nums: &str, comments: bool) -> Vec<u8> {
            let comments_type = r#"<Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/>"#;
            let header_type =
                r#"<Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>"#;
            let types = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>{}{}</Types>"#,
                if header.is_some() { header_type } else { "" },
                if comments { comments_type } else { "" }
            );
            let root_rels = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="word/document.xml"/></Relationships>"#
            );
            let document_rels = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/styles" Target="styles.xml"/><Relationship Id="rId2" Type="{REL}/numbering" Target="numbering.xml"/>{}{}</Relationships>"#,
                if header.is_some() { format!(r#"<Relationship Id="rId3" Type="{REL}/header" Target="header1.xml"/>"#) } else { String::new() },
                if comments { format!(r#"<Relationship Id="rId4" Type="{REL}/comments" Target="comments.xml"/>"#) } else { String::new() }
            );
            let section = if header.is_some() { r#"<w:sectPr><w:headerReference w:type="default" r:id="rId3"/></w:sectPr>"# } else { "" };
            let styles = format!(
                r#"<w:styles {W}><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:rPr><w:sz w:val="22"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:pPr><w:outlineLvl w:val="0"/></w:pPr></w:style>{extra_styles}</w:styles>"#
            );
            let numbering = format!(
                r#"<w:numbering {W}><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1.%2"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>{extra_nums}</w:numbering>"#
            );
            let mut entries = vec![
                ("[Content_Types].xml".to_string(), types.into_bytes()),
                ("_rels/.rels".to_string(), root_rels.into_bytes()),
                ("word/_rels/document.xml.rels".to_string(), document_rels.into_bytes()),
                ("word/document.xml".to_string(), format!(r#"<w:document {W}><w:body>{body}{section}</w:body></w:document>"#).into_bytes()),
                ("word/styles.xml".to_string(), styles.into_bytes()),
                ("word/numbering.xml".to_string(), numbering.into_bytes()),
            ];
            if let Some(header) = header {
                entries.push(("word/header1.xml".to_string(), format!(r#"<w:hdr {W}>{header}</w:hdr>"#).into_bytes()));
            }
            if comments {
                let comment =
                    r#"<w:comment w:id="0" w:author="Ana" w:date="2026-10-01T09:00:00Z"><w:p><w:r><w:t>Check</w:t></w:r></w:p></w:comment>"#;
                entries.push(("word/comments.xml".to_string(), format!(r#"<w:comments {W}>{comment}</w:comments>"#).into_bytes()));
            }
            zip_entries(&entries).unwrap()
        }

        fn list_items(items: &[(u8, &str)]) -> String {
            items
                .iter()
                .map(|(level, text)| {
                    format!(
                        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="{level}"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
                    )
                })
                .collect()
        }

        /// A package whose body is `body`, as written.
        pub fn custom(body: &str) -> Vec<u8> {
            zip(body, None)
        }

        /// A package whose body is `body`, with comment 0 defined.
        pub fn custom_with_comment(body: &str) -> Vec<u8> {
            build_with_comments(body, None, "", "", true)
        }

        pub fn numbered(items: &[(u8, &str)]) -> Vec<u8> {
            zip(&list_items(items), None)
        }

        /// List items in list 1, the one at `restart` in list 2: the same abstract list, its
        /// first level started again at `start`.
        pub fn restarted(items: &[(u8, &str)], restart: usize, start: u32) -> Vec<u8> {
            let body: String = items
                .iter()
                .enumerate()
                .map(|(index, (level, text))| {
                    let list = if index == restart { 2 } else { 1 };
                    format!(
                        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="{level}"/><w:numId w:val="{list}"/></w:numPr></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
                    )
                })
                .collect();
            let nums = format!(
                r#"<w:num w:numId="2"><w:abstractNumId w:val="0"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="{start}"/></w:lvlOverride></w:num>"#
            );
            build(&body, None, "", &nums)
        }

        pub fn with_header(body: &[(u8, &str)], header: &[(u8, &str)]) -> Vec<u8> {
            zip(&list_items(body), Some(&list_items(header)))
        }

        pub fn styled(paragraphs: &[(&str, &str)]) -> Vec<u8> {
            let body: String = paragraphs
                .iter()
                .map(|(style, text)| format!(r#"<w:p><w:pPr><w:pStyle w:val="{style}"/></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#))
                .collect();
            zip(&body, None)
        }

        fn cell(text: &str, properties: &str, run_properties: &str) -> String {
            format!(
                r#"<w:tc><w:tcPr>{properties}</w:tcPr><w:p><w:r><w:rPr>{run_properties}</w:rPr><w:t xml:space="preserve">{text}</w:t></w:r></w:p></w:tc>"#
            )
        }

        /// A two-row table in a style that hides its header row and colours its text white.
        pub fn table_styled(header: &[&str], body: &[&str]) -> Vec<u8> {
            let style = r#"<w:style w:type="table" w:styleId="Sly"><w:name w:val="Sly"/><w:rPr><w:color w:val="FFFFFF"/></w:rPr><w:tblStylePr w:type="firstRow"><w:rPr><w:vanish/></w:rPr></w:tblStylePr></w:style>"#;
            let row = |texts: &[&str]| format!("<w:tr>{}</w:tr>", texts.iter().map(|text| cell(text, "", "")).collect::<String>());
            let table = format!(
                r#"<w:tbl><w:tblPr><w:tblStyle w:val="Sly"/><w:tblLook w:firstRow="1" w:lastRow="0" w:firstColumn="0" w:lastColumn="0" w:noHBand="1" w:noVBand="1"/></w:tblPr><w:tblGrid><w:gridCol/><w:gridCol/></w:tblGrid>{}{}</w:tbl><w:p/>"#,
                row(header),
                row(body)
            );
            build(&table, None, style, "")
        }

        /// One row of cells, each `(fill, font colour, text)`.
        pub fn shaded_cells(cells: &[(&str, &str, &str)]) -> Vec<u8> {
            let cells: String = cells
                .iter()
                .map(|(fill, colour, text)| {
                    cell(text, &format!(r#"<w:shd w:val="clear" w:fill="{fill}"/>"#), &format!(r#"<w:color w:val="{colour}"/>"#))
                })
                .collect();
            zip(&format!("<w:tbl><w:tblPr/><w:tblGrid><w:gridCol/><w:gridCol/></w:tblGrid><w:tr>{cells}</w:tr></w:tbl><w:p/>"), None)
        }

        /// A table in a black cell; the inner cell has no shading of its own.
        pub fn nested_in_black_cell(text: &str) -> Vec<u8> {
            let inner = format!(
                "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol/></w:tblGrid><w:tr>{}</w:tr></w:tbl><w:p/>",
                cell(text, "", r#"<w:color w:val="000000"/>"#)
            );
            let outer = format!(
                r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol/></w:tblGrid><w:tr><w:tc><w:tcPr><w:shd w:val="clear" w:fill="000000"/></w:tcPr>{inner}</w:tc></w:tr></w:tbl><w:p/>"#
            );
            zip(&outer, None)
        }

        /// `styles` character styles, and `runs` runs each in one of them.
        pub fn many_styles(styles: usize, runs: usize) -> Vec<u8> {
            let extra: String = (0..styles)
                .map(|index| format!(r#"<w:style w:type="character" w:styleId="C{index}"><w:name w:val="c{index}"/></w:style>"#))
                .collect();
            let body: String = (0..runs)
                .map(|index| format!(r#"<w:r><w:rPr><w:rStyle w:val="C{}"/></w:rPr><w:t>x</w:t></w:r>"#, styles - 1 - index % styles))
                .collect();
            build(&format!("<w:p>{body}</w:p>"), None, &extra, "")
        }

        /// `lists` lists, and `items` paragraphs in the last of them.
        pub fn many_lists(lists: usize, items: usize) -> Vec<u8> {
            let nums: String = (2..lists + 2).map(|id| format!(r#"<w:num w:numId="{id}"><w:abstractNumId w:val="0"/></w:num>"#)).collect();
            let body: String = (0..items)
                .map(|_| format!(r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="{}"/></w:numPr></w:pPr></w:p>"#, lists + 1))
                .collect();
            build(&body, None, "", &nums)
        }

        pub fn runs(runs: &[(&str, &str)]) -> Vec<u8> {
            let body: String = runs
                .iter()
                .map(|(text, properties)| format!(r#"<w:r><w:rPr>{properties}</w:rPr><w:t xml:space="preserve">{text}</w:t></w:r>"#))
                .collect();
            zip(&format!("<w:p>{body}</w:p>"), None)
        }
    }
}

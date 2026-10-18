//! embed: what each paragraph shows that only the document model knows, for Clauflow's version
//! comparison: its list label as displayed, its list and level, its outline level, and the text of
//! runs a reader would not see. Paragraphs are named by part and origin, as the save check's
//! decoded streams name them (`inspect::streams`).

use std::collections::HashMap;
use std::rc::Rc;

use wordcraft_doc::numbering::Counters;
use wordcraft_doc::props::{Rgb, TextColor};
use wordcraft_doc::styles::{PartSet, TableStyleProps};
use wordcraft_doc::table::Table;
use wordcraft_doc::{Block, Blocks, CharProps, Document, MAX_TABLE_DEPTH, ParaProps, Paragraph, PartKind, StoryRef};

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
    /// 0 for Heading 1 … 8, from the resolved paragraph properties.
    pub outline_level: Option<u8>,
    /// The text of runs that would not be seen: hidden, coloured like what is behind them
    /// (font colour within a small distance of highlight, shading or page colour), or below 2 pt.
    pub concealed: Vec<(Conceal, String)>,
}

/// Font colours this close to what is behind them (largest channel difference) are not seen.
const COLOUR_DISTANCE: u8 = 24;
/// Text smaller than this, in points, is not seen.
const TINY_POINTS: f32 = 2.0;

/// Facts for every paragraph of the body, headers, footers, notes and text boxes, in document
/// order. Paragraphs without an origin (a drop cap merged into its paragraph) are left out.
pub fn paragraph_facts(bytes: &[u8], limits: &ReadLimits) -> Result<Vec<ParagraphFacts>, DocxError> {
    // Origins name parts by their index in the package's sorted part names. Only the names are
    // kept, so the inflated package is not held beside the model.
    let part_names: Vec<String> = Package::open_with(bytes, limits)?.part_names().map(str::to_string).collect();
    let doc = crate::read_with_limits(bytes, limits)?;
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
        self.out.push(ParagraphFacts {
            part: part.clone(),
            origin: origin.index,
            label,
            list: numbering.map(|numbering| (numbering.num, numbering.level)),
            outline_level: resolved.outline_level.filter(|level| *level < 9),
            concealed,
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
            let header_type =
                r#"<Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>"#;
            let types = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>{}</Types>"#,
                if header.is_some() { header_type } else { "" }
            );
            let root_rels = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="word/document.xml"/></Relationships>"#
            );
            let document_rels = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/styles" Target="styles.xml"/><Relationship Id="rId2" Type="{REL}/numbering" Target="numbering.xml"/>{}</Relationships>"#,
                if header.is_some() { format!(r#"<Relationship Id="rId3" Type="{REL}/header" Target="header1.xml"/>"#) } else { String::new() }
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

        pub fn numbered(items: &[(u8, &str)]) -> Vec<u8> {
            zip(&list_items(items), None)
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

//! The view in document terms (embed API 0.3): which top-level block is at the top of the view and
//! how far into it, and the way back, putting a fraction of a block at the top.
//!
//! Positions are worked out on one axis: the pages stacked without their gaps, in layout points
//! (`absolute`). A block runs from the top of its first line to the bottom of its last, which may be
//! on a later page, so a block cut by a page break still has one height and a fraction of it one
//! place. Both directions go through `block_bounds`, so a fraction read from the view puts the same
//! place back when it is sent to `scroll_to_block`.

use wordcraft_doc::{Block, Blocks, Document, MAX_TABLE_DEPTH, Path, Pos, StoryRef};
use wordcraft_docx::facts::Locator;
use wordcraft_layout::DocLayout;

use super::wire::{BlockRef, LocateRequest};
use crate::WordApp;

/// What the host page can know of the view; the unit `Viewport` events are made from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewReading {
    pub top: Option<BlockRef>,
    pub fraction: f32,
    /// The view's height in document points (layout points, not screen points): how much of the
    /// document it shows, so it grows as the zoom falls.
    pub visible_height: f32,
    /// 1-based page at the top of the view.
    pub page: u32,
    pub pages: u32,
}

/// A reading as it is compared with the last one told: the fraction to a thousandth and the height
/// to a tenth of a point, so a sub-pixel drift is not a change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ViewKey {
    top: Option<BlockRef>,
    fraction_milli: i32,
    page: u32,
    pages: u32,
    height_tenths: i64,
}

impl ViewReading {
    /// The reading with its numbers rounded as [`ViewKey`] compares them, and that key.
    pub(crate) fn rounded(mut self) -> (ViewReading, ViewKey) {
        let fraction_milli = (self.fraction * 1000.0).round() as i32;
        let height_tenths = (self.visible_height * 10.0).round() as i64;
        self.fraction = fraction_milli as f32 / 1000.0;
        self.visible_height = height_tenths as f32 / 10.0;
        let key = ViewKey { top: self.top.clone(), fraction_milli, page: self.page, pages: self.pages, height_tenths };
        (self, key)
    }
}

/// What the viewport reports and `scrollToBlock` needs of an open document.
#[derive(Default)]
pub(crate) struct ViewportState {
    /// The open document's paragraphs by part and origin. `None` until a document is open.
    locator: Option<Locator>,
    /// The package's sorted part names, kept to rebuild the locator after an edit moved paragraphs.
    part_names: Vec<String>,
    /// The content version the locator's paths were made for (edit mode changes them).
    locator_version: u64,
    /// The token of a `scrollToBlock` not yet answered; the `viewport` that follows the scroll
    /// carries it, and clears it.
    pub(crate) scroll_token: Option<u32>,
    /// `locate` requests not answered yet: they wait for a document that is open and laid out.
    pub(crate) pending_locates: Vec<LocateRequest>,
    /// The last reading the host was told; `None` until the first after an open.
    pub(crate) last: Option<ViewKey>,
    /// The person's own input reached the canvas this frame (`state::person_input`).
    pub(crate) person_input: bool,
    /// The person's input started a move that is still under way (a key's or a bar drag's glide
    /// takes frames after the input); it ends with a frame in which the view did not change, or
    /// when a host call moves the view.
    pub(crate) person_moving: bool,
    /// The view changed since the last input of the person's, so the glide has begun.
    pub(crate) moved_since_input: bool,
    /// Egui's time of the person's last input.
    pub(crate) last_input_time: f64,
}

impl ViewportState {
    /// A new document is open: its locator, and a first reading that is a baseline again. A pending
    /// token still gets its answer.
    pub(crate) fn opened(&mut self, doc: &Document, content_version: u64, part_names: Option<Vec<String>>) {
        self.part_names = part_names.unwrap_or_default();
        self.locator = (!self.part_names.is_empty()).then(|| Locator::new(doc, &self.part_names));
        self.locator_version = content_version;
        self.last = None;
    }

    /// The locator for `doc` as it is now: edits move paragraphs, so a locator made at the open is
    /// remade once the content differs from the version it was made for.
    fn locator_for(&mut self, doc: &Document, content_version: u64) -> Option<&Locator> {
        if self.locator.is_some() && self.locator_version != content_version {
            self.locator = Some(Locator::new(doc, &self.part_names));
            self.locator_version = content_version;
        }
        self.locator.as_ref()
    }

    /// The index of the top-level body block that `block` names, which is what `block_bounds` takes.
    /// `None` when the document has no such part or paragraph.
    fn top_level_block(&mut self, doc: &Document, content_version: u64, block: &BlockRef) -> Option<usize> {
        let path = self.locator_for(doc, content_version)?.path(&block.part, block.index)?;
        usize::try_from(Locator::block_of(&path).0.first().copied()?).ok()
    }
}

/// Where each page starts on the stacked axis; one entry more than there are pages, the last being
/// the end of the document.
fn page_starts(layout: &DocLayout) -> Vec<f32> {
    let mut starts = Vec::with_capacity(layout.pages.len().saturating_add(1));
    let mut at = 0.0_f32;
    starts.push(at);
    for page in &layout.pages {
        at += page.h;
        starts.push(at);
    }
    starts
}

/// The point `y` below the top of page `page` on the stacked axis.
fn absolute(starts: &[f32], page: usize, y: f32) -> Option<f32> {
    let value = starts.get(page)? + y;
    value.is_finite().then_some(value)
}

/// The page and the points below its top that the stacked-axis position `at` is.
fn on_page(starts: &[f32], at: f32) -> Option<(usize, f32)> {
    let pages = starts.len().checked_sub(1)?;
    let page = (0..pages).rev().find(|page| starts.get(*page).is_some_and(|start| *start <= at)).unwrap_or(0);
    Some((page, (at - starts.get(page)?).max(0.0)))
}

/// The first (or, with `from_end`, last) paragraph of `blocks` in document order, and its text's
/// length: a table is searched into, cell by cell. `prefix` is the path of the container.
fn edge_paragraph(blocks: &Blocks, prefix: &[u32], from_end: bool, depth: usize) -> Option<(Path, usize)> {
    if depth > MAX_TABLE_DEPTH {
        return None;
    }
    let order: Vec<usize> = if from_end { (0..blocks.len()).rev().collect() } else { (0..blocks.len()).collect() };
    for index in order {
        let mut path = prefix.to_vec();
        path.push(u32::try_from(index).ok()?);
        match blocks.get(index).map(|block| &**block)? {
            Block::Para(paragraph) => return Some((Path(path), paragraph.text.len())),
            Block::Table(table) => {
                let rows: Vec<usize> = if from_end { (0..table.rows.len()).rev().collect() } else { (0..table.rows.len()).collect() };
                for row in rows {
                    let cells = table.rows.get(row).map_or(0, |row| row.cells.len());
                    let cells: Vec<usize> = if from_end { (0..cells).rev().collect() } else { (0..cells).collect() };
                    for cell in cells {
                        let mut cell_prefix = path.clone();
                        cell_prefix.push(u32::try_from(row).ok()?);
                        cell_prefix.push(u32::try_from(cell).ok()?);
                        let cell_blocks = &table.rows.get(row)?.cells.get(cell)?.blocks;
                        if let Some(found) = edge_paragraph(cell_blocks, &cell_prefix, from_end, depth.saturating_add(1)) {
                            return Some(found);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Where the paragraph at (`path`, `offset`) sits on the stacked axis: the top of its caret, or with
/// `bottom` the bottom of it. `None` when it is not laid out.
fn caret_edge(layout: &DocLayout, starts: &[f32], path: &Path, offset: usize, bottom: bool) -> Option<f32> {
    let caret = layout.caret_on(&Pos { story: StoryRef::Body, path: path.clone(), off: offset }, 0)?;
    absolute(starts, caret.page, if bottom { caret.top + caret.height } else { caret.top })
}

/// The top and the bottom of the top-level body block `block` on the stacked axis. A paragraph runs
/// from its first line to its last. A table runs from the highest first line of its first row's
/// cells to the lowest last line of its last row's cells (the cells of a row differ in height). A
/// block with nothing laid out (wording the clean display removed) has no bounds.
fn block_bounds(doc: &Document, layout: &DocLayout, starts: &[f32], block: usize) -> Option<(f32, f32)> {
    let index = u32::try_from(block).ok()?;
    let (tops, bottoms): (Vec<(Path, usize)>, Vec<(Path, usize)>) = match &**doc.body.get(block)? {
        Block::Para(paragraph) => (vec![(Path::top(block), 0)], vec![(Path::top(block), paragraph.text.len())]),
        Block::Table(table) => {
            let cells_of = |row: Option<&wordcraft_doc::Row>, row_index: usize, from_end: bool| -> Vec<(Path, usize)> {
                let Some(row) = row else { return Vec::new() };
                let Ok(row_number) = u32::try_from(row_index) else { return Vec::new() };
                row.cells
                    .iter()
                    .enumerate()
                    .filter_map(|(cell_index, cell)| {
                        let prefix = [index, row_number, u32::try_from(cell_index).ok()?];
                        edge_paragraph(&cell.blocks, &prefix, from_end, 1)
                    })
                    .collect()
            };
            let last_row = table.rows.len().saturating_sub(1);
            (cells_of(table.rows.first(), 0, false), cells_of(table.rows.get(last_row), last_row, true))
        }
    };
    let top = tops.iter().filter_map(|(path, offset)| caret_edge(layout, starts, path, *offset, false)).reduce(f32::min)?;
    let bottom = bottoms.iter().filter_map(|(path, offset)| caret_edge(layout, starts, path, *offset, true)).reduce(f32::max)?;
    Some((top, bottom.max(top)))
}

/// Where the view is, or `None` while the pages are not drawn yet (the first frame of an open, or a
/// layout that has not caught up with the canvas).
pub(crate) fn read(app: &mut WordApp, viewport: &mut ViewportState) -> Option<ViewReading> {
    let canvas = app.canvas.canvas_rect?;
    let scale = app.canvas.scale;
    if app.canvas.page_rects.is_empty() || !(scale.is_finite() && scale > 0.0) {
        return None;
    }
    let layout = app.session.layout();
    // The canvas draws the pages of the layout it was handed; rects of another count are older.
    if layout.pages.len() != app.canvas.page_rects.len() {
        return None;
    }
    let starts = page_starts(&layout);
    let view_top = canvas.min.y;
    let page = app.canvas.page_rects.iter().position(|rect| rect.max.y > view_top).unwrap_or(app.canvas.page_rects.len().saturating_sub(1));
    let page_rect = app.canvas.page_rects.get(page)?;
    let page_height = layout.pages.get(page)?.h;
    let into_page = ((view_top - page_rect.min.y) / scale).clamp(0.0, page_height);
    let at = absolute(&starts, page, into_page)?;

    let doc = &app.session.doc;
    let first = layout.pages.get(page).map_or(0, |p| p.first_block).saturating_sub(1);
    let last = layout.pages.get(page + 1).map_or(usize::MAX, |next| next.first_block);
    let last = last.min(doc.body.len().saturating_sub(1));
    let mut found: Option<(usize, f32, f32)> = None;
    let mut previous: Option<(usize, f32, f32)> = None;
    for block in first..=last {
        let Some((top, bottom)) = block_bounds(doc, &layout, &starts, block) else { continue };
        previous = Some((block, top, bottom));
        if bottom > at {
            found = Some((block, top, bottom));
            break;
        }
    }
    // Below the last block of the last page: the view ends at that block's end.
    let reading = found.or(previous).map(|(block, top, bottom)| {
        let height = bottom - top;
        let fraction = if height > 0.0 { ((at - top) / height).clamp(0.0, 1.0) } else { 0.0 };
        let name = viewport.locator.as_ref().and_then(|locator| locator.name(doc, &Path::top(block)));
        (name.map(|(part, index)| BlockRef { part, index }), fraction)
    });
    let (top, fraction) = reading.unwrap_or((None, 0.0));
    Some(ViewReading {
        // A block without a name (made in edit mode) is not one the host can ask for: `top` is null.
        fraction: if top.is_some() { fraction } else { 0.0 },
        top,
        visible_height: canvas.height() / scale,
        page: super::state::page_number(page),
        pages: u32::try_from(layout.pages.len()).unwrap_or(u32::MAX),
    })
}

/// Where `fraction` of the block `request` names is, as the page and the points below its top that
/// `scroll_to_doc` takes. `None` when the document has no such block or it is not laid out.
pub(crate) fn target(app: &mut WordApp, viewport: &mut ViewportState, block: &BlockRef, fraction: f32) -> Option<(usize, f32)> {
    let content_version = app.session.content_version();
    let layout = app.session.layout();
    let doc = &app.session.doc;
    let top_level = viewport.top_level_block(doc, content_version, block)?;
    let starts = page_starts(&layout);
    let (top, bottom) = block_bounds(doc, &layout, &starts, top_level)?;
    on_page(&starts, top + fraction.clamp(0.0, 1.0) * (bottom - top))
}

/// The top of each of `blocks` on the stacked axis `read` and `target` use, in the request's order
/// (`None` for a block the document lacks or has nothing laid out for), and the height of all the
/// pages on that axis. The layout is taken once for the whole request.
pub(crate) fn locate(app: &mut WordApp, viewport: &mut ViewportState, blocks: &[BlockRef]) -> (Vec<Option<f32>>, f32) {
    let content_version = app.session.content_version();
    let layout = app.session.layout();
    let doc = &app.session.doc;
    let starts = page_starts(&layout);
    let positions = blocks
        .iter()
        .map(|block| {
            let top_level = viewport.top_level_block(doc, content_version, block)?;
            block_bounds(doc, &layout, &starts, top_level).map(|(top, _)| top)
        })
        .collect();
    (positions, starts.last().copied().unwrap_or(0.0))
}

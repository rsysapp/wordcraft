//! Host marks: passages the host page highlights (discussions, risks), painted above the page and
//! below the selection; a click inside one tells the host which.
//!
//! Every mark is looked for in one pass over the document (`FirstMatches`, which gives each the
//! match `TextIndex::find_where` would), when it is painted rather than when it arrives, and the
//! result is kept: matches and their rectangles in page space are found again only when the marks,
//! the document or the layout change. The pass and the placing of rectangles stop after a time
//! budget per frame and go on in the next, so a long list on a long document never stalls a frame;
//! painting touches only the rectangles on visible pages.
//!
//! A mark may instead name a place (embed API 0.3, `LocationSpec`): the paragraph by block and
//! fingerprint, a range of its accepted text. It needs no search; it is placed from the locator and
//! the layout in the same pass, and refused when the paragraph is not the one the host meant.
//!
//! Limits: the budget is looked at after each passage folded, after each paragraph searched,
//! every `WORK_PER_CHECK` units of work inside one paragraph (a place, a passage compared there,
//! and every 64 bytes compared; also between the passages sharing one place's prefix), and after
//! each mark placed. Between two looks the work is bounded by: folding one passage (linear in its
//! length as sent; folding stops at `MAX_NEEDLE_CHARS` folded characters), building the automaton
//! once (at most `MAX_MARKS` × 64 bytes of prefixes: about 5 ms and 1.5 MB measured natively),
//! about 16 KiB of passage compared plus one passage, and the document's text index, built once per
//! document and shared with jumps. Crafted marks sharing a prefix with a long run of the same text
//! still take many frames in all, but no frame spends much more than the budget on them.

use std::sync::{Arc, Weak};

use egui::{Color32, Rect, pos2, vec2};
use wordcraft_doc::{Document, Pos, StoryRef};
use wordcraft_docx::facts::{Locator, accepted_text, fingerprint, model_end_offset, model_offset};
use wordcraft_layout::DocLayout;

use super::bridge::HostEvent;
use super::text_match::{FirstMatches, TextIndex};
use super::viewport::block_span;
use super::wire::{LocationSpec, MAX_MARKS, MarkSpec, MarkStyle, page_index, parse_color};
use crate::WordApp;

/// How wide a bar is, in screen points whatever the zoom, and how far its right edge stands from
/// the text it marks.
const BAR_WIDTH: f32 = 3.0;
const BAR_GAP: f32 = 3.0;
/// An outline's line, in screen points.
const OUTLINE_WIDTH: f32 = 1.5;

/// Milliseconds of one frame spent looking for marks and placing them; the rest go on in later
/// frames. One pass over a long document, or placing 2,000 marks, can take longer than a frame.
const SEARCH_BUDGET_MS: f64 = 4.0;

/// A host mark as the host sent it, its colour parsed; a text mark's text is in the search.
struct HostMark {
    id: String,
    page: Option<u32>,
    color: Color32,
    /// The colour at full alpha, for the lines of an outline and a bar.
    solid: Color32,
    style: MarkStyle,
    /// Where the mark is, when it was sent by location rather than by text.
    at: Option<LocationSpec>,
}

#[derive(Default)]
pub(crate) struct Marks {
    marks: Vec<HostMark>,
    /// The marks' texts, one pass over the document for all of them; parallel to `marks`.
    search: FirstMatches,
    /// The document revision and layout that the search and `rects` belong to.
    against: Option<(u64, Weak<DocLayout>)>,
    /// How many of the search's matches have been placed in `rects`.
    placed: usize,
    /// The found marks' rectangles in page space: (page index, rectangle, index in `marks`), by
    /// page and, within a page, in the host's order, so the last one on a page is painted on top.
    rects: Vec<(usize, wordcraft_geom::Rect, usize)>,
    /// What was painted last frame: screen rectangle and index in `marks`, the topmost last.
    hits: Vec<(Rect, usize)>,
    /// Ids of the location marks refused in this pass over the document: their paragraph is not
    /// there, its text differs from the fingerprint, or the range lies outside it.
    refused: Vec<String>,
    /// The refused ids were handed on (or there were none) since the marks were last set.
    told: bool,
}

impl Marks {
    /// Replace every mark; an empty list clears them. Marks past [`MAX_MARKS`] are dropped.
    pub(crate) fn set(&mut self, marks: Vec<MarkSpec>) {
        // The host side already refuses a bad colour; one that slips through is skipped like an unfound mark.
        // A location mark has no text to search for: its empty one folds to nothing and finds nothing.
        let (marks, texts): (Vec<HostMark>, Vec<String>) = marks
            .into_iter()
            .take(MAX_MARKS)
            .filter_map(|mark| {
                let color = parse_color(&mark.color)?;
                let solid = mark.color.get(..7).and_then(parse_color)?;
                let host_mark = HostMark { color, solid, id: mark.id, page: mark.page, style: mark.style, at: mark.at };
                Some((host_mark, mark.text.unwrap_or_default()))
            })
            .unzip();
        // Folded within the frame budget when first painted, not here: 2,000 long passages take a while.
        let search = FirstMatches::new(texts);
        *self = Marks { marks, search, ..Marks::default() };
    }

    pub(crate) fn clear(&mut self) {
        *self = Marks::default();
    }

    /// Some mark has not been looked for, or not placed, in the document and layout last painted.
    pub(crate) fn pending(&self) -> bool {
        !self.search.done() || self.placed < self.marks.len()
    }

    /// The ids of the location marks refused, once every mark has been placed for the first time
    /// since the marks were set; `None` before that, afterwards, and when none was refused.
    pub(crate) fn take_refused(&mut self) -> Option<Vec<String>> {
        if self.told || self.pending() {
            return None;
        }
        self.told = true;
        (!self.refused.is_empty()).then(|| std::mem::take(&mut self.refused))
    }

    /// Screen rectangles painted last frame, with their marks' ids, the topmost last.
    pub(crate) fn painted(&self) -> Vec<(Rect, String)> {
        self.hits.iter().filter_map(|(rect, k)| self.marks.get(*k).map(|mark| (*rect, mark.id.clone()))).collect()
    }

    /// Go on looking for the marks in this document and layout, then placing them, for at most
    /// [`SEARCH_BUDGET_MS`]; `true` while work remains.
    pub(crate) fn resolve(&mut self, index: &mut TextIndex, doc: &Document, rev: u64, layout: &Arc<DocLayout>, locator: Option<&Locator>) -> bool {
        // A layout is replaced, never changed in place, so the pointer names it; the `Weak` keeps the
        // allocation, so a later layout cannot reuse the address.
        let current = self.against.as_ref().is_some_and(|(at, seen)| *at == rev && std::ptr::eq(seen.as_ptr(), Arc::as_ptr(layout)));
        if !current {
            self.against = Some((rev, Arc::downgrade(layout)));
            self.search.restart();
            self.placed = 0;
            self.rects.clear();
            self.refused.clear();
        }
        if !self.pending() {
            return false;
        }
        index.refresh(doc, rev);
        // A monotonic clock (`performance.now()` on the web): the wall clock `now_ms` reads can step
        // back, and on the web it ticks in whole milliseconds or coarser.
        let started = web_time::Instant::now();
        let in_budget = || started.elapsed().as_secs_f64() * 1000.0 < SEARCH_BUDGET_MS;
        let Marks { marks, search, .. } = self;
        let pages = layout.pages.len();
        // A mark's first match on its page when it names one (none when the document lacks that
        // page), else its first in document order.
        // Equal texts share a match, so its page is asked once, not once per mark that wants it.
        let mut last_page: Option<(wordcraft_doc::Pos, Option<usize>)> = None;
        let on_its_page = |k: usize, start: &wordcraft_doc::Pos, _: &wordcraft_doc::Pos| match marks.get(k).map(|mark| mark.page) {
            Some(None) => true,
            Some(Some(page)) => {
                let found_on = match &last_page {
                    Some((at, on)) if at == start => *on,
                    _ => {
                        let on = layout.page_of(start);
                        last_page = Some((start.clone(), on));
                        on
                    }
                };
                page_index(page, pages).is_some_and(|page| found_on == Some(page))
            }
            None => false,
        };
        index.find_first(search, on_its_page, in_budget);
        if !self.search.done() {
            return true;
        }
        let before = self.rects.len();
        while let Some(found) = self.search.found().get(self.placed) {
            let k = self.placed;
            self.placed += 1;
            if let Some(at) = self.marks.get(k).and_then(|mark| mark.at.as_ref().map(|at| (at, mark.style))) {
                match place_location(doc, layout, locator, at.0, at.1) {
                    Some(placed) => self.rects.extend(placed.into_iter().map(|(page, rect)| (page, rect, k))),
                    None => self.refused.extend(self.marks.get(k).map(|mark| mark.id.clone())),
                }
            } else if let Some((start, end)) = found {
                // Body text only (the index holds the body), which the page hint does not affect.
                let rects = layout.selection_rects(doc, start, end, 0).into_iter().filter(|(_, rect)| usable(rect));
                self.rects.extend(rects.map(|(page, rect)| (page, rect, k)));
            }
            if !in_budget() {
                break;
            }
        }
        if self.rects.len() != before {
            // Stable: within a page the host's order stays, so the topmost is still the last sent.
            self.rects.sort_by_key(|(page, _, _)| *page);
        }
        self.pending()
    }
}

/// The rectangles (page index, page-space rectangle) of a location mark, or `None` when the mark is
/// refused: no paragraph is named so, its accepted text is not the one the fingerprint names (the
/// host compared another file than the one open), or the range does not lie inside that text. A
/// mark that is valid but shows nothing (an empty range as a fill or an outline, wording the clean
/// display left out) has no rectangles.
fn place_location(
    doc: &Document,
    layout: &DocLayout,
    locator: Option<&Locator>,
    at: &LocationSpec,
    style: MarkStyle,
) -> Option<Vec<(usize, wordcraft_geom::Rect)>> {
    let path = locator?.path(&at.block.part, at.block.index)?;
    let paragraph = doc.para(StoryRef::Body, &path)?;
    if fingerprint(&accepted_text(paragraph)) != at.fingerprint {
        return None;
    }
    let length = at.fingerprint.length;
    let (from, to) = (at.from.unwrap_or(0), at.to.unwrap_or(length));
    if from > to || to > length {
        return None;
    }
    let whole = at.from.is_none() && at.to.is_none();
    let position = |off: usize| Pos { story: StoryRef::Body, path: path.clone(), off };
    // The start is where the first unit of the range stands; the end is just after its last unit, so
    // a deletion that follows the range is not part of it.
    let start = position(model_offset(paragraph, from)?);
    let end = position(model_end_offset(paragraph, to)?);
    let words = |a: &Pos, b: &Pos| -> Vec<(usize, wordcraft_geom::Rect)> {
        layout.selection_rects(doc, a, b, 0).into_iter().filter(|(_, rect)| usable(rect)).collect()
    };
    Some(match style {
        MarkStyle::Fill | MarkStyle::Outline if from == to => Vec::new(),
        MarkStyle::Fill | MarkStyle::Outline => words(&start, &end).into_iter().filter(|(_, rect)| rect.w > 0.0).collect(),
        MarkStyle::Bar if from == to => {
            // The place between two characters: the line it is on, in the margin beside the paragraph.
            let caret = layout.caret_on(&start, 0)?;
            let beside = words(&position(0), &position(paragraph.text.len()));
            let left = beside.iter().filter(|(page, _)| *page == caret.page).map(|(_, rect)| rect.x).reduce(f32::min).unwrap_or(caret.x);
            vec![(caret.page, bar_beside(left, caret.top, caret.top + caret.height))]
        }
        MarkStyle::Bar if whole => {
            // No range: the whole top-level block, which for a table is every row of it.
            let (first, last) = block_span(doc, usize::try_from(*path.0.first()?).ok()?)?;
            bars_beside(&words(&first, &last))
        }
        MarkStyle::Bar => bars_beside(&words(&start, &end)),
    })
}

/// A bar's place: the text it stands beside begins at `x`, and it runs from `top` to `bottom` (page
/// points). Its width is not in page points, so its rectangle has none: `paint` draws the strip left
/// of `x`.
fn bar_beside(x: f32, top: f32, bottom: f32) -> wordcraft_geom::Rect {
    wordcraft_geom::Rect::new(x, top, 0.0, (bottom - top).max(0.0))
}

/// One bar per page the `lines` are on, covering them, beside the leftmost of them.
fn bars_beside(lines: &[(usize, wordcraft_geom::Rect)]) -> Vec<(usize, wordcraft_geom::Rect)> {
    let mut pages: Vec<usize> = lines.iter().map(|(page, _)| *page).collect();
    pages.sort_unstable();
    pages.dedup();
    pages
        .into_iter()
        .filter_map(|page| {
            let on_page = || lines.iter().filter(move |(at, _)| *at == page).map(|(_, rect)| rect);
            let left = on_page().map(|rect| rect.x).reduce(f32::min)?;
            let top = on_page().map(|rect| rect.y).reduce(f32::min)?;
            let bottom = on_page().map(|rect| rect.y + rect.h).reduce(f32::max)?;
            Some((page, bar_beside(left, top, bottom)))
        })
        .collect()
}

fn usable(rect: &wordcraft_geom::Rect) -> bool {
    [rect.x, rect.y, rect.w, rect.h].iter().all(|value| value.is_finite()) && rect.w >= 0.0 && rect.h > 0.0
}

/// Paint the marks on the visible pages (`pages`: screen rectangles by page index) and, when this
/// frame's click on the canvas landed in one, tell the host: the topmost wins. A drag is never a
/// click, so selecting text across a mark activates nothing.
pub(crate) fn paint(app: &mut WordApp, painter: &egui::Painter, pages: &[Rect], layout: &Arc<DocLayout>, scale: f32, canvas: &egui::Response) {
    // No marks, and no clicks on them, while the document is edited (embed API 0.2).
    if app.host.as_ref().is_some_and(|host| host.edit.is_some()) {
        return;
    }
    let Some(host) = app.host.as_mut() else { return };
    if host.resolve_marks(&app.session.doc, app.session.rev(), app.session.content_version(), layout) {
        painter.ctx().request_repaint();
    }
    let marks = &mut host.marks;
    marks.hits.clear();
    let clip = painter.clip_rect();
    for (page, page_rect) in pages.iter().enumerate() {
        if !page_rect.intersects(clip) {
            continue;
        }
        let first = marks.rects.partition_point(|(p, _, _)| *p < page);
        let after = marks.rects.partition_point(|(p, _, _)| *p <= page);
        for (_, rect, k) in marks.rects.get(first..after).unwrap_or_default() {
            let Some(mark) = marks.marks.get(*k) else { continue };
            let (left, top) = (page_rect.min.x + rect.x * scale, page_rect.min.y + rect.y * scale);
            let on_screen = match mark.style {
                MarkStyle::Bar => Rect::from_min_size(pos2((left - BAR_GAP - BAR_WIDTH).max(page_rect.min.x), top), vec2(BAR_WIDTH, rect.h * scale)),
                MarkStyle::Fill | MarkStyle::Outline => Rect::from_min_size(pos2(left, top), vec2(rect.w * scale, rect.h * scale)),
            };
            if !on_screen.is_finite() || !on_screen.intersects(clip) {
                continue;
            }
            match mark.style {
                // Rounded as the PDF editor's embed build draws its marks.
                MarkStyle::Fill => painter.rect_filled(on_screen, egui::CornerRadius::same(2), mark.color),
                MarkStyle::Outline => painter.rect_stroke(
                    on_screen,
                    egui::CornerRadius::same(2),
                    egui::Stroke::new(OUTLINE_WIDTH, mark.solid),
                    egui::StrokeKind::Inside,
                ),
                // A thin strip needs its colour whole to be seen.
                MarkStyle::Bar => painter.rect_filled(on_screen, egui::CornerRadius::ZERO, mark.solid),
            };
            marks.hits.push((on_screen, *k));
        }
    }
    if canvas.clicked()
        && let Some(at) = canvas.interact_pointer_pos()
        && let Some(mark) = marks.hits.iter().rev().find(|(rect, _)| rect.contains(at)).and_then(|(_, k)| marks.marks.get(*k))
    {
        host.bridge.emit(HostEvent::MarkActivated { id: mark.id.clone() });
    }
}

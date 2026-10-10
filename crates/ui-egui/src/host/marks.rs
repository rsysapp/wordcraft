//! Host marks: passages the host page highlights (discussions, risks), painted above the page and
//! below the selection; a click inside one tells the host which.
//!
//! Every mark is looked for in one pass over the document (`FirstMatches`, which gives each the
//! match `TextIndex::find_where` would), when it is painted rather than when it arrives, and the
//! result is kept: matches and their rectangles in page space are found again only when the marks,
//! the document or the layout change. The pass and the placing of rectangles stop after a time
//! budget per frame and go on in the next, so a long list on a long document never stalls a frame;
//! painting touches only the rectangles on visible pages.

use std::sync::{Arc, Weak};

use egui::{Color32, Rect, pos2, vec2};
use wordcraft_doc::Document;
use wordcraft_layout::DocLayout;

use super::bridge::HostEvent;
use super::text_match::{FirstMatches, TextIndex};
use super::wire::{MAX_MARKS, MarkSpec, page_index, parse_color};
use crate::WordApp;

/// Milliseconds of one frame spent looking for marks and placing them; the rest go on in later
/// frames. One pass over a long document, or placing 2,000 marks, can take longer than a frame.
const SEARCH_BUDGET_MS: f64 = 4.0;

/// A host mark as the host sent it, its colour parsed; its text is in the search.
struct HostMark {
    id: String,
    page: Option<u32>,
    color: Color32,
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
}

impl Marks {
    /// Replace every mark; an empty list clears them. Marks past [`MAX_MARKS`] are dropped.
    pub(crate) fn set(&mut self, marks: Vec<MarkSpec>) {
        // The host side already refuses a bad colour; one that slips through is skipped like an unfound mark.
        let (marks, texts): (Vec<HostMark>, Vec<String>) = marks
            .into_iter()
            .take(MAX_MARKS)
            .filter_map(|mark| Some((HostMark { color: parse_color(&mark.color)?, id: mark.id, page: mark.page }, mark.text)))
            .unzip();
        let search = FirstMatches::new(texts.iter().map(String::as_str));
        *self = Marks { marks, search, ..Marks::default() };
    }

    pub(crate) fn clear(&mut self) {
        *self = Marks::default();
    }

    /// Some mark has not been looked for, or not placed, in the document and layout last painted.
    pub(crate) fn pending(&self) -> bool {
        !self.search.done() || self.placed < self.marks.len()
    }

    /// Screen rectangles painted last frame, with their marks' ids, the topmost last.
    pub(crate) fn painted(&self) -> Vec<(Rect, String)> {
        self.hits.iter().filter_map(|(rect, k)| self.marks.get(*k).map(|mark| (*rect, mark.id.clone()))).collect()
    }

    /// Go on looking for the marks in this document and layout, then placing them, for at most
    /// [`SEARCH_BUDGET_MS`]; `true` while work remains.
    pub(crate) fn resolve(&mut self, index: &mut TextIndex, doc: &Document, rev: u64, layout: &Arc<DocLayout>) -> bool {
        // A layout is replaced, never changed in place, so the pointer names it; the `Weak` keeps the
        // allocation, so a later layout cannot reuse the address.
        let current = self.against.as_ref().is_some_and(|(at, seen)| *at == rev && std::ptr::eq(seen.as_ptr(), Arc::as_ptr(layout)));
        if !current {
            self.against = Some((rev, Arc::downgrade(layout)));
            self.search.restart();
            self.placed = 0;
            self.rects.clear();
        }
        if !self.pending() {
            return false;
        }
        index.refresh(doc, rev);
        let started = crate::now_ms();
        let in_budget = || crate::now_ms() - started < SEARCH_BUDGET_MS;
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
            if let Some((start, end)) = found {
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

fn usable(rect: &wordcraft_geom::Rect) -> bool {
    [rect.x, rect.y, rect.w, rect.h].iter().all(|value| value.is_finite()) && rect.w >= 0.0 && rect.h > 0.0
}

/// Paint the marks on the visible pages (`pages`: screen rectangles by page index) and, when this
/// frame's click on the canvas landed in one, tell the host: the topmost wins. A drag is never a
/// click, so selecting text across a mark activates nothing.
pub(crate) fn paint(app: &mut WordApp, painter: &egui::Painter, pages: &[Rect], layout: &Arc<DocLayout>, scale: f32, canvas: &egui::Response) {
    let Some(host) = app.host.as_mut() else { return };
    if host.resolve_marks(&app.session.doc, app.session.rev(), layout) {
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
            let on_screen =
                Rect::from_min_size(pos2(page_rect.min.x + rect.x * scale, page_rect.min.y + rect.y * scale), vec2(rect.w * scale, rect.h * scale));
            let Some(mark) = marks.marks.get(*k) else { continue };
            if !on_screen.is_finite() || !on_screen.intersects(clip) {
                continue;
            }
            // Rounded as the PDF editor's embed build draws its marks.
            painter.rect_filled(on_screen, egui::CornerRadius::same(2), mark.color);
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

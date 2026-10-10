//! Host marks: passages the host page highlights (discussions, risks), painted above the page and
//! below the selection; a click inside one tells the host which.
//!
//! A mark is looked for when it is painted, not when it arrives, and the result is kept: its
//! match and its rectangles in page space are found again only when the marks, the document or the
//! layout change. Looking for a long list runs within a time budget per frame, so 2,000 marks on a
//! long document never stall one frame; painting touches only the rectangles on visible pages.

use std::sync::{Arc, Weak};

use egui::{Color32, Rect, pos2, vec2};
use wordcraft_doc::{Document, Pos};
use wordcraft_layout::DocLayout;

use super::bridge::HostEvent;
use super::text_match::TextIndex;
use super::wire::{MAX_MARKS, MarkSpec, page_index, parse_color};
use crate::WordApp;

/// Milliseconds of one frame spent looking for marks; the rest are looked for in later frames.
const SEARCH_BUDGET_MS: f64 = 4.0;

/// A host mark as the host sent it, its colour parsed.
struct WantedMark {
    id: String,
    text: String,
    page: Option<u32>,
    color: Color32,
}

/// A host mark found in the open document; its first match's rectangles are in [`Marks::rects`].
pub(crate) struct ResolvedMark {
    pub id: String,
    pub color: Color32,
}

#[derive(Default)]
pub(crate) struct Marks {
    wanted: Vec<WantedMark>,
    /// The document revision and layout that `found` and `rects` belong to.
    against: Option<(u64, Weak<DocLayout>)>,
    /// How many of `wanted` have been looked for against `against`.
    searched: usize,
    found: Vec<ResolvedMark>,
    /// The found marks' rectangles in page space: (page index, rectangle, index in `found`), by
    /// page and, within a page, in the host's order, so the last one on a page is painted on top.
    rects: Vec<(usize, wordcraft_geom::Rect, usize)>,
    /// What was painted last frame: screen rectangle and index in `found`, the topmost last.
    hits: Vec<(Rect, usize)>,
}

impl Marks {
    /// Replace every mark; an empty list clears them. Marks past [`MAX_MARKS`] are dropped.
    pub(crate) fn set(&mut self, marks: Vec<MarkSpec>) {
        // The host side already refuses a bad colour; one that slips through is skipped like an unfound mark.
        let wanted = marks
            .into_iter()
            .take(MAX_MARKS)
            .filter_map(|mark| Some(WantedMark { color: parse_color(&mark.color)?, id: mark.id, text: mark.text, page: mark.page }))
            .collect();
        *self = Marks { wanted, ..Marks::default() };
    }

    pub(crate) fn clear(&mut self) {
        *self = Marks::default();
    }

    /// Some mark has not been looked for in the document and layout last painted.
    pub(crate) fn pending(&self) -> bool {
        self.searched < self.wanted.len()
    }

    /// Screen rectangles painted last frame, with their marks' ids, the topmost last.
    pub(crate) fn painted(&self) -> Vec<(Rect, String)> {
        self.hits.iter().filter_map(|(rect, k)| self.found.get(*k).map(|mark| (*rect, mark.id.clone()))).collect()
    }

    /// Look for the marks not yet looked for in this document and layout, for at most
    /// [`SEARCH_BUDGET_MS`]; `true` while some remain.
    pub(crate) fn resolve(&mut self, index: &mut TextIndex, doc: &Document, rev: u64, layout: &Arc<DocLayout>) -> bool {
        // A layout is replaced, never changed in place, so the pointer names it; the `Weak` keeps the
        // allocation, so a later layout cannot reuse the address.
        let current = self.against.as_ref().is_some_and(|(at, seen)| *at == rev && std::ptr::eq(seen.as_ptr(), Arc::as_ptr(layout)));
        if !current {
            self.against = Some((rev, Arc::downgrade(layout)));
            self.searched = 0;
            self.found.clear();
            self.rects.clear();
        }
        if !self.pending() {
            return false;
        }
        index.refresh(doc, rev);
        let started = crate::now_ms();
        let before = self.rects.len();
        while let Some(wanted) = self.wanted.get(self.searched) {
            self.searched += 1;
            if let Some((start, end)) = first_match(index, layout, wanted) {
                // Body text only (the index holds the body), which the page hint does not affect.
                let rects: Vec<_> = layout.selection_rects(doc, &start, &end, 0).into_iter().filter(|(_, rect)| usable(rect)).collect();
                if !rects.is_empty() {
                    let k = self.found.len();
                    self.rects.extend(rects.into_iter().map(|(page, rect)| (page, rect, k)));
                    self.found.push(ResolvedMark { id: wanted.id.clone(), color: wanted.color });
                }
            }
            if crate::now_ms() - started >= SEARCH_BUDGET_MS {
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

/// The mark's first match: on its page when it names one (none when the document lacks that page),
/// else the first in document order.
fn first_match(index: &TextIndex, layout: &DocLayout, wanted: &WantedMark) -> Option<(Pos, Pos)> {
    let on_page = match wanted.page {
        Some(page) => Some(page_index(page, layout.pages.len())?),
        None => None,
    };
    index.find_where(&wanted.text, |start, _| on_page.is_none_or(|page| layout.page_of(start) == Some(page)), 0)
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
            let Some(mark) = marks.found.get(*k) else { continue };
            if !on_screen.is_finite() || !on_screen.intersects(clip) {
                continue;
            }
            painter.rect_filled(on_screen, 0.0, mark.color);
            marks.hits.push((on_screen, *k));
        }
    }
    if canvas.clicked()
        && let Some(at) = canvas.interact_pointer_pos()
        && let Some(mark) = marks.hits.iter().rev().find(|(rect, _)| rect.contains(at)).and_then(|(_, k)| marks.found.get(*k))
    {
        host.bridge.emit(HostEvent::MarkActivated { id: mark.id.clone() });
    }
}

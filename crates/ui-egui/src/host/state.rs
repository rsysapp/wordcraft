//! The host page's side of a running app: its commands apply at the start of a frame
//! (`before_frame`, from `WordApp::logic`); its events are produced at the end (`after_frame`,
//! from `WordApp::ui`) and reach the page after that frame.

use egui::{Rect, Vec2};
use wordcraft_doc::Pos;
use wordcraft_engine::Selection;

use super::bridge::{HostBridge, HostCommand, HostEvent};
use super::text_match::TextIndex;
use super::wire::{JumpTarget, occurrence_index, page_index};
use crate::WordApp;

/// Points left above a passage the host jumps to, so it does not sit on the view's edge. The PDF
/// editor's embed build leaves the same above its matches, so both previews place a passage alike.
const JUMP_MARGIN_PT: f32 = 60.0;

/// A host jump that was found but whose scroll the canvas has not applied yet.
struct PendingJump {
    target: JumpTarget,
    /// The scroll it replaced, put back when a newer jump finds nothing.
    displaced: Option<(usize, f32)>,
}

/// What places the pages on screen: the canvas's scale and size, and where the first page sits.
#[derive(Clone, Copy, PartialEq)]
struct ViewGeometry {
    scale: f32,
    size: Vec2,
    first_page: Option<Vec2>,
}

pub struct HostState {
    pub bridge: HostBridge,
    /// Set by Ctrl/⌘+F; the view bar focuses its find field on the next frame.
    pub(crate) focus_find: bool,
    pub(crate) find_query: String,
    /// Where the current find began: typing refines the search from here, not from the last match.
    pub(crate) find_anchor: Option<Pos>,
    /// The page the view bar or the host last jumped to. It stays the current page until a person
    /// scrolls (wheel, scroll bar, touch drag, a scroll key or a selection drag that moves the view)
    /// or the next jump or find replaces it. "Most visible" cannot name a page that never wins that
    /// vote (the last one, or any page when the whole document is in view), so Next page would never
    /// reach it; and layout settling, a resize or a zoom are not the person moving the view, so none
    /// of them drops it.
    pub(crate) page_target: Option<usize>,
    /// Points below the target page's top that the jump put at the top of the view (0 for a page; a
    /// passage less its margin), so a resize can put the same place back.
    target_y: f32,
    /// A scroll the host or the view bar asked for is due in this frame's canvas, so the view moving
    /// is not a person's doing.
    scroll_queued: bool,
    pending_jump: Option<PendingJump>,
    last_view: Option<ViewGeometry>,
    last_page: Option<usize>,
    /// The document's text as jumps match it; rebuilt when the document changes.
    index: TextIndex,
}

impl HostState {
    /// Make `page` the current page (see `page_target`), its top at the top of the view.
    pub(crate) fn jump_to(&mut self, page: usize) {
        self.aim(page, 0.0);
    }

    /// Make `page` the current page with the point `y` below its top at the top of the view.
    fn aim(&mut self, page: usize, y: f32) {
        self.page_target = Some(page);
        self.target_y = y;
        self.scroll_queued = true;
    }

    /// The scroll that puts the target back at the top of the view after the pages moved, if there
    /// is a target.
    pub(crate) fn target_scroll(&mut self) -> Option<(usize, f32)> {
        let page = self.page_target?;
        self.scroll_queued = true;
        Some((page, self.target_y))
    }

    /// The page the view bar and `pageChanged` report: the jump's target while it holds, else the
    /// most visible page.
    pub(crate) fn current_page(&self, visible: Option<usize>) -> Option<usize> {
        self.page_target.or(visible)
    }

    /// The page jumps made current (tests).
    #[doc(hidden)]
    pub fn page_target(&self) -> Option<usize> {
        self.page_target
    }

    pub fn new(bridge: HostBridge) -> Self {
        HostState {
            bridge,
            focus_find: false,
            find_query: String::new(),
            find_anchor: None,
            page_target: None,
            target_y: 0.0,
            scroll_queued: false,
            pending_jump: None,
            last_view: None,
            last_page: None,
            index: TextIndex::default(),
        }
    }
}

pub(crate) fn before_frame(app: &mut WordApp, ctx: &egui::Context) {
    // Taken out while commands run, so they can borrow the rest of the app.
    let Some(mut host) = app.host.take() else { return };
    // Read before the canvas's scroll area consumes the frame's scroll delta.
    if host.page_target.is_some() && scrolled_by_person(ctx, app.canvas.canvas_rect, app.canvas.focused) {
        host.page_target = None;
    }
    for command in host.bridge.take_commands() {
        match command {
            HostCommand::Open { name, bytes } => open(app, &mut host, &name, &bytes),
            HostCommand::JumpTo(target) => jump(app, &mut host, target),
        }
    }
    app.host = Some(host);
}

pub(crate) fn after_frame(app: &mut WordApp, ctx: &egui::Context) {
    let Some(mut host) = app.host.take() else { return };
    // The canvas took the queued scroll this frame: a waiting jump has landed.
    if app.canvas.scroll_to_doc.is_none() {
        host.pending_jump = None;
    }
    follow_the_view(app, &mut host, ctx);
    // The target page is in view too (the jump put its top there, or as near as the end allows), and
    // the host's page indicator should say what the bar says.
    if let Some(page) = host.current_page(app.canvas.visible_page)
        && host.last_page != Some(page)
    {
        host.last_page = Some(page);
        host.bridge.emit(HostEvent::PageChanged { page: page_number(page) });
    }
    app.host = Some(host);
}

/// Width of the strip along the canvas's right and bottom edges where the scroll bars sit.
const SCROLL_BAR_STRIP: f32 = 20.0;

/// When the pages moved on screen this frame: a resize or a fitted zoom puts the target back at the
/// top of the view (it is not a person scrolling, so the target stays); a selection drag that moved
/// the view (the canvas keeps the selection's end in sight) is a person scrolling and drops it.
fn follow_the_view(app: &mut WordApp, host: &mut HostState, ctx: &egui::Context) {
    let ours = std::mem::take(&mut host.scroll_queued);
    let Some(canvas) = app.canvas.canvas_rect else { return };
    let now =
        ViewGeometry { scale: app.canvas.scale, size: canvas.size(), first_page: app.canvas.page_rects.first().map(|page| page.min - canvas.min) };
    let Some(before) = host.last_view.replace(now) else { return };
    let rescaled = (before.scale, before.size) != (now.scale, now.size);
    // A non-finite geometry differs from itself every frame; following it would repaint forever.
    if rescaled && now.scale.is_finite() && now.size.is_finite() {
        if app.canvas.scroll_to_doc.is_none()
            && let Some(target) = host.target_scroll()
        {
            app.canvas.scroll_to_doc = Some(target);
            ctx.request_repaint();
        }
    } else if !rescaled && before.first_page != now.first_page && !ours && selecting_by_drag(ctx, canvas) {
        host.page_target = None;
    }
}

/// Whether the primary button is held after a press on the document (not on a scroll bar, which
/// `scrolled_by_person` counts): a person selecting text.
fn selecting_by_drag(ctx: &egui::Context, canvas: Rect) -> bool {
    ctx.input(|input| {
        input.pointer.primary_down()
            && input
                .pointer
                .press_origin()
                .is_some_and(|at| canvas.contains(at) && at.x <= canvas.max.x - SCROLL_BAR_STRIP && at.y <= canvas.max.y - SCROLL_BAR_STRIP)
    })
}

/// Whether a person is scrolling the document this frame: the wheel or a trackpad over the canvas,
/// a drag that began on a scroll bar or a touch drag, or a key that moves the view while the canvas
/// has the keyboard. Layout settling, resizes and zoom (a Ctrl+wheel is a zoom, not a scroll) are not.
fn scrolled_by_person(ctx: &egui::Context, canvas: Option<egui::Rect>, canvas_focused: bool) -> bool {
    use egui::Key;
    let Some(canvas) = canvas else { return false };
    ctx.input(|input| {
        let wheel = input.smooth_scroll_delta != egui::Vec2::ZERO && input.pointer.hover_pos().is_some_and(|at| canvas.contains(at));
        let origin_in = |strip_only: bool| {
            input.pointer.press_origin().is_some_and(|at| {
                canvas.contains(at) && (!strip_only || at.x > canvas.max.x - SCROLL_BAR_STRIP || at.y > canvas.max.y - SCROLL_BAR_STRIP)
            })
        };
        let dragging = input.pointer.is_decidedly_dragging();
        let bar_drag = dragging && origin_in(true);
        let touch_drag = dragging && input.any_touches() && origin_in(false);
        let scroll_key = canvas_focused
            && [Key::PageUp, Key::PageDown, Key::Home, Key::End, Key::ArrowUp, Key::ArrowDown, Key::ArrowLeft, Key::ArrowRight]
                .into_iter()
                .any(|key| input.key_pressed(key));
        wheel || bar_drag || touch_drag || scroll_key
    })
}

/// Show the host's page or passage; `jumpNotFound` when there is none. A newer jump replaces one
/// whose scroll has not landed yet, found or not, as in the PDF editor's embed build.
fn jump(app: &mut WordApp, host: &mut HostState, target: JumpTarget) {
    let displaced = match host.pending_jump.take() {
        Some(waiting) => waiting.displaced,
        None => app.canvas.scroll_to_doc,
    };
    match find_target(app, host, &target) {
        Some((page, y)) => {
            app.canvas.scroll_to_doc = Some((page, y));
            host.aim(page, y);
            host.pending_jump = Some(PendingJump { target, displaced });
        }
        None => {
            app.canvas.scroll_to_doc = displaced;
            host.page_target = None;
            host.bridge.emit(HostEvent::JumpNotFound { target });
        }
    }
}

/// Where `target` is: (page index, points below its top to put at the top of the view). A text
/// jump selects its passage (as find does; not reported to the host as a person's selection).
fn find_target(app: &mut WordApp, host: &mut HostState, target: &JumpTarget) -> Option<(usize, f32)> {
    let layout = app.session.layout();
    let pages = layout.pages.len();
    match target {
        JumpTarget::Page { page } => {
            let index = page_index(*page, pages)?;
            super::view::go_to_page(app, index);
            Some((index, 0.0))
        }
        JumpTarget::Text { text, page, occurrence } => {
            // No page searches the whole document; a page the document lacks finds nothing.
            let on_page = match page {
                Some(page) => Some(page_index(*page, pages)?),
                None => None,
            };
            let nth = occurrence_index(*occurrence)?;
            host.index.refresh(&app.session.doc, app.session.rev());
            let (start, end) = host.index.find_where(text, |start, _| on_page.is_none_or(|p| layout.page_of(start) == Some(p)), nth)?;
            let hint = on_page.or_else(|| layout.page_of(&start)).unwrap_or(0);
            let caret = layout.caret_on(&start, hint)?;
            app.session.sel = Selection { anchor: start, focus: end };
            app.session.page_hint = caret.page;
            // `max` also turns a NaN into 0; the canvas keeps the offset within the page.
            Some((caret.page, (caret.top - JUMP_MARGIN_PT).max(0.0)))
        }
    }
}

/// A failed open keeps the document that was showing.
fn open(app: &mut WordApp, host: &mut HostState, name: &str, bytes: &[u8]) {
    match super::open::open_document(name, bytes) {
        Ok(doc) => {
            // A jump found in the document being replaced never lands, so the host hears it failed.
            if let Some(waiting) = host.pending_jump.take() {
                host.bridge.emit(HostEvent::JumpNotFound { target: waiting.target });
            }
            app.session.set_document(doc);
            app.session.page_hint = 0;
            app.session.find = Default::default();
            app.canvas.scroll_to_doc = Some((0, 0.0));
            app.canvas.visible_page = None;
            host.last_page = None;
            host.find_query.clear();
            host.find_anchor = None;
            host.page_target = None;
            host.scroll_queued = true;
            let pages = app.session.layout().pages.len();
            host.bridge.emit(HostEvent::Opened { pages: u32::try_from(pages).unwrap_or(u32::MAX) });
        }
        Err(message) => host.bridge.emit(HostEvent::OpenFailed { message }),
    }
}

/// 1-based page number for the host.
pub(crate) fn page_number(index: usize) -> u32 {
    u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::{HostState, page_number};
    use crate::host::HostBridge;

    #[test]
    fn a_jump_target_is_the_current_page_until_it_is_dropped() {
        let mut host = HostState::new(HostBridge::new());
        assert_eq!(host.current_page(Some(3)), Some(3));
        host.jump_to(4);
        assert_eq!(host.current_page(Some(3)), Some(4));
        assert_eq!(host.current_page(None), Some(4));
        host.page_target = None;
        assert_eq!(host.current_page(Some(2)), Some(2));
    }

    #[test]
    fn a_rescroll_puts_back_the_place_the_jump_showed() {
        let mut host = HostState::new(HostBridge::new());
        assert_eq!(host.target_scroll(), None);
        host.aim(2, 140.0);
        assert_eq!(host.target_scroll(), Some((2, 140.0)));
        host.jump_to(5);
        assert_eq!(host.target_scroll(), Some((5, 0.0)), "a page jump shows the page's top");
        host.page_target = None;
        assert_eq!(host.target_scroll(), None);
    }

    #[test]
    fn page_numbers_are_one_based_and_saturate() {
        assert_eq!(page_number(0), 1);
        assert_eq!(page_number(41), 42);
        assert_eq!(page_number(usize::MAX), u32::MAX);
        assert_eq!(page_number(u32::MAX as usize), u32::MAX);
    }
}

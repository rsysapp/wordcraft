//! The host page's side of a running app: its commands apply at the start of a frame
//! (`before_frame`, from `WordApp::logic`); its events are produced at the end (`after_frame`,
//! from `WordApp::ui`) and reach the page after that frame.

use super::bridge::{HostBridge, HostCommand, HostEvent};
use crate::WordApp;
use wordcraft_doc::Pos;

pub struct HostState {
    pub bridge: HostBridge,
    /// Set by Ctrl/⌘+F; the view bar focuses its find field on the next frame.
    pub(crate) focus_find: bool,
    pub(crate) find_query: String,
    /// Where the current find began: typing refines the search from here, not from the last match.
    pub(crate) find_anchor: Option<Pos>,
    /// The page the view bar last jumped to. It stays the current page until a person scrolls (wheel,
    /// scroll bar, touch drag or a scroll key) or the next jump or find replaces it. "Most visible"
    /// cannot name a page that never wins that vote (the last one, or any page when the whole document
    /// is in view), so Next page would never reach it; and layout settling, a resize or a zoom are
    /// not the person moving the view, so none of them drops it.
    pub(crate) page_target: Option<usize>,
    last_page: Option<usize>,
}

impl HostState {
    /// Make `page` the current page (see `page_target`).
    pub(crate) fn jump_to(&mut self, page: usize) {
        self.page_target = Some(page);
    }

    /// The page the view bar and `pageChanged` report: the jump's target while it holds, else the
    /// most visible page.
    pub(crate) fn current_page(&self, visible: Option<usize>) -> Option<usize> {
        self.page_target.or(visible)
    }

    pub fn new(bridge: HostBridge) -> Self {
        HostState { bridge, focus_find: false, find_query: String::new(), find_anchor: None, page_target: None, last_page: None }
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
        }
    }
    app.host = Some(host);
}

pub(crate) fn after_frame(app: &mut WordApp) {
    let Some(mut host) = app.host.take() else { return };
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

/// A failed open keeps the document that was showing.
fn open(app: &mut WordApp, host: &mut HostState, name: &str, bytes: &[u8]) {
    match super::open::open_document(name, bytes) {
        Ok(doc) => {
            app.session.set_document(doc);
            app.session.page_hint = 0;
            app.session.find = Default::default();
            app.canvas.scroll_to_doc = Some((0, 0.0));
            app.canvas.visible_page = None;
            host.last_page = None;
            host.find_query.clear();
            host.find_anchor = None;
            host.page_target = None;
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
    fn page_numbers_are_one_based_and_saturate() {
        assert_eq!(page_number(0), 1);
        assert_eq!(page_number(41), 42);
        assert_eq!(page_number(usize::MAX), u32::MAX);
        assert_eq!(page_number(u32::MAX as usize), u32::MAX);
    }
}

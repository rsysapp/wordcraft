//! The host page's side of a running app: its commands apply at the start of a frame
//! (`before_frame`, from `WordApp::logic`); its events are produced at the end (`after_frame`,
//! from `WordApp::ui`) and reach the page after that frame.

use super::bridge::{HostBridge, HostCommand, HostEvent};
use crate::WordApp;
use wordcraft_doc::Pos;

/// A page the view bar jumped to, kept as the current page until the view moves on its own.
/// "Most visible" cannot name a page that never wins that vote (the last one, or any page when
/// the whole document is in view), so Next page would never reach it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PageTarget {
    pub(crate) page: usize,
    /// The visible page on the frame after the jump (outer `None`: not seen yet). A change from it
    /// means the view moved by itself.
    seen_visible: Option<Option<usize>>,
}

pub struct HostState {
    pub bridge: HostBridge,
    /// Set by Ctrl/⌘+F; the view bar focuses its find field on the next frame.
    pub(crate) focus_find: bool,
    pub(crate) find_query: String,
    /// Where the current find began: typing refines the search from here, not from the last match.
    pub(crate) find_anchor: Option<Pos>,
    pub(crate) page_target: Option<PageTarget>,
    last_page: Option<usize>,
}

impl HostState {
    /// Make `page` the current page (see [`PageTarget`]).
    pub(crate) fn jump_to(&mut self, page: usize) {
        self.page_target = Some(PageTarget { page, seen_visible: None });
    }

    /// The page the view bar and `pageChanged` report: the jump's target while the view stays
    /// where the jump put it, else the most visible page.
    pub(crate) fn current_page(&self, visible: Option<usize>) -> Option<usize> {
        self.page_target.map(|target| target.page).or(visible)
    }

    /// Drop the target once the visible page differs from the one the jump itself produced.
    fn settle_page_target(&mut self, visible: Option<usize>) {
        let Some(target) = self.page_target.as_mut() else { return };
        match target.seen_visible {
            None => target.seen_visible = Some(visible),
            Some(seen) if seen != visible => self.page_target = None,
            Some(_) => {}
        }
    }

    pub fn new(bridge: HostBridge) -> Self {
        HostState { bridge, focus_find: false, find_query: String::new(), find_anchor: None, page_target: None, last_page: None }
    }
}

pub(crate) fn before_frame(app: &mut WordApp) {
    // Taken out while commands run, so they can borrow the rest of the app.
    let Some(mut host) = app.host.take() else { return };
    for command in host.bridge.take_commands() {
        match command {
            HostCommand::Open { name, bytes } => open(app, &mut host, &name, &bytes),
        }
    }
    app.host = Some(host);
}

pub(crate) fn after_frame(app: &mut WordApp) {
    let Some(mut host) = app.host.take() else { return };
    host.settle_page_target(app.canvas.visible_page);
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
    fn a_jump_target_holds_until_the_visible_page_changes_from_the_one_after_the_jump() {
        let mut host = HostState::new(HostBridge::new());
        host.jump_to(4);
        // The jump's own frame: the visible page is whatever the jump produced, even if it is not the target.
        host.settle_page_target(Some(3));
        assert_eq!(host.current_page(Some(3)), Some(4));
        host.settle_page_target(Some(3));
        assert_eq!(host.current_page(Some(3)), Some(4));
        host.settle_page_target(Some(2));
        assert_eq!(host.current_page(Some(2)), Some(2));
        assert_eq!(host.current_page(None), None);
    }

    #[test]
    fn page_numbers_are_one_based_and_saturate() {
        assert_eq!(page_number(0), 1);
        assert_eq!(page_number(41), 42);
        assert_eq!(page_number(usize::MAX), u32::MAX);
        assert_eq!(page_number(u32::MAX as usize), u32::MAX);
    }
}

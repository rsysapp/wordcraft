//! The host page's side of a running app: its commands apply at the start of a frame
//! (`before_frame`, from `WordApp::logic`); its events are produced at the end (`after_frame`,
//! from `WordApp::ui`) and reach the page after that frame.

use super::bridge::{HostBridge, HostCommand, HostEvent};
use crate::WordApp;

pub struct HostState {
    pub bridge: HostBridge,
    last_page: Option<usize>,
}

impl HostState {
    pub fn new(bridge: HostBridge) -> Self {
        HostState { bridge, last_page: None }
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
    if let Some(page) = app.canvas.visible_page
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
    use super::page_number;

    #[test]
    fn page_numbers_are_one_based_and_saturate() {
        assert_eq!(page_number(0), 1);
        assert_eq!(page_number(41), 42);
        assert_eq!(page_number(usize::MAX), u32::MAX);
        assert_eq!(page_number(u32::MAX as usize), u32::MAX);
    }
}

//! What a build shows and allows. View mode shows the document with page navigation, zoom and
//! find, and refuses every edit: at `WordApp::run` (UI commands, saving, downloads, dialogs) and,
//! through `Session::view_only`, at `Session::run` (engine commands), not merely by hiding them.

use serde_json::{Value, json};

use super::HostEvent;
use crate::WordApp;

/// The refusal text both gates answer with.
pub use wordcraft_engine::view_only::refusal;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EmbedProfile {
    /// The whole application (desktop and web app).
    #[default]
    Full,
    /// Embed build, view mode.
    View,
}

impl EmbedProfile {
    pub fn is_view(self) -> bool {
        self == EmbedProfile::View
    }

    /// Whether UI code that writes to the document directly (outside `WordApp::run`) may do so.
    pub fn allows_editing(self) -> bool {
        self == EmbedProfile::Full
    }
}

/// Switch `app` to `profile`.
pub(crate) fn apply(app: &mut WordApp, profile: EmbedProfile) {
    app.profile = profile;
    if !profile.is_view() {
        return;
    }
    app.session.view_only = true;
    app.autosave = false;
    app.ui.show_discord = false;
    app.ui.backstage = false;
    app.dialog = None;
    let view = &mut app.session.view;
    view.ruler = false;
    // No spelling squiggles on a document nobody may edit here.
    view.proofing = false;
    view.marks = false;
    view.fit = "pageWidth".into();
    // A viewer takes keyboard focus only when the person clicks it.
    app.canvas.want_focus = false;
}

/// View mode's gate in front of `WordApp::run`; `Some` answers without running the command.
pub(crate) fn view_gate(app: &mut WordApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !app.profile.is_view() {
        return None;
    }
    // Ctrl/⌘+F and Find without text open no dialog or pane in view mode; Ctrl/⌘+F goes to the view
    // bar's find field, or to the host when there is no bar (it has a find field of its own).
    // "Without text" is the engine's reading: a `text` that is not a string counts as none and would
    // open the pane.
    if id == "edit.find" && wordcraft_engine::p::str(params, "text").is_none() {
        if let Some(host) = app.host.as_mut() {
            if host.view_bar {
                host.focus_find = true;
            } else {
                host.bridge.emit(HostEvent::FindRequested);
            }
        }
        return Some(Ok(json!({})));
    }
    // While the host page has the document in edit mode, edit mode's gate decides.
    if let Some(rules) = app.session.edit_rules() {
        return super::edit::gate(app, id, params, rules);
    }
    if wordcraft_engine::view_only::allows(id) { None } else { Some(Err(refusal(id))) }
}

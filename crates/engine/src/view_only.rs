//! View-only sessions (the embed build's view mode). While [`Session::view_only`] is set,
//! `Session::run` refuses every command not listed here: every command that reaches
//! `Session::run`, whoever sent it. An allow-list rather than `CommandSpec::mutates`, because Undo,
//! Redo and Save are registered as pure.
//!
//! This guards the engine only. UI paths that bypass `run` (the web save and export download in
//! `App::run`, the comments pane writing the document directly, the backstage author and autosave
//! settings) are closed by the UI guard in the embed build, not here.
//!
//! [`Session::view_only`]: crate::Session::view_only

/// Navigation, selection, copy, find and zoom.
pub const COMMANDS: &[&str] = &[
    "caret.left",
    "caret.right",
    "caret.up",
    "caret.down",
    "caret.wordLeft",
    "caret.wordRight",
    "caret.home",
    "caret.end",
    "caret.paraUp",
    "caret.paraDown",
    "caret.docStart",
    "caret.docEnd",
    "caret.pageUp",
    "caret.pageDown",
    "caret.set",
    "select.all",
    "select.range",
    "select.word",
    "select.sentence",
    "select.paragraph",
    "select.line",
    "select.text",
    "select.collapse",
    "select.extend",
    "edit.copy",
    "edit.find",
    "edit.findNext",
    "edit.findPrevious",
    "edit.goto",
    "view.zoom",
    "view.zoom100",
    "view.zoomIn",
    "view.zoomOut",
    "view.pageWidth",
    "view.onePage",
    "view.state",
];

/// Whether a view-only session may run `id`.
pub fn allows(id: &str) -> bool {
    COMMANDS.contains(&id)
}

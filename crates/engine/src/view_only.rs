//! View-only sessions (the embed build's view mode). While [`Session::view_only`] is set,
//! `Session::run` refuses every command not listed here, whatever asked for it (keyboard, menus,
//! control channel). An allow-list rather than `CommandSpec::mutates`, because Undo, Redo and
//! Save are registered as pure.
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

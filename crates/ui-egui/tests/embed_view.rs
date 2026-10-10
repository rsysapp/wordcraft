//! The embed build's view mode: the document with page navigation, zoom and find only; no
//! chrome, no brand, no edit path.

mod common;

use common::*;
use serde_json::json;

/// Words only the full window paints (title bar, ribbon, status bar, Backstage, community links).
const CHROME_AND_BRAND: &[&str] = &[
    "AutoSave",
    "Discord",
    "ArtCraft",
    "Search commands",
    "Share",
    "Help",
    "About",
    "Insert",
    "Review",
    " words",
    "English (United States)",
    "Focus",
];

#[test]
fn view_mode_draws_the_document_without_chrome_or_brand() {
    let (h, _bridge) = view_app(clauses(40), 1.0);
    let text = drawn_text(&h);
    for word in CHROME_AND_BRAND {
        assert!(!text.contains(word), "view mode drew {word:?}:\n{text}");
    }
    assert!(!h.state().canvas.page_rects.is_empty(), "no pages laid out");
}

#[test]
fn view_mode_never_sets_a_window_title() {
    let (mut h, _bridge) = view_app(clauses(3), 1.0);
    h.state_mut().session.dirty = true;
    frames(&mut h, 2);
    assert!(!viewport_commands(&h).iter().any(|c| matches!(c, egui::ViewportCommand::Title(_))));
}

#[test]
fn application_commands_are_refused_in_view_mode() {
    let (mut h, _bridge) = view_app(clauses(3), 1.0);
    let app = h.state_mut();
    let params = json!({"name": "about", "tab": "File", "text": "x", "path": "refused.docx", "value": true});
    for id in [
        "ui.discord",
        "ui.tab",
        "ui.backstage",
        "ui.dialog",
        "ui.openFileDialog",
        "ui.dark",
        "file.save",
        "file.saveAs",
        "file.exportPdf",
        "file.print",
        "file.open",
        "file.new",
        "text.insert",
        "review.newComment",
        "edit.undo",
    ] {
        let result = app.run(id, params.clone());
        assert!(matches!(&result, Err(why) if why.contains("viewing only")), "{id} ran in view mode: {result:?}");
    }
    assert!(app.canvas.open_url.is_none());
    assert!(app.dialog.is_none());
    assert!(!app.ui.backstage);
}

#[test]
fn find_without_text_opens_no_dialog_or_pane() {
    let (mut h, _bridge) = view_app(clauses(3), 1.0);
    let app = h.state_mut();
    assert!(app.run("edit.find", json!({})).is_ok());
    assert!(app.dialog.is_none());
    assert!(!app.session.view.nav_pane);
}

#[test]
fn find_with_text_that_is_not_a_string_opens_no_pane_either() {
    // The engine reads a non-string `text` as no text at all and would open the Find pane.
    let (mut h, _bridge) = view_app(clauses(3), 1.0);
    let app = h.state_mut();
    for text in [json!(null), json!(5), json!(["Clause"])] {
        assert!(app.run("edit.find", json!({ "text": text })).is_ok());
        assert!(app.dialog.is_none());
        assert!(!app.session.view.nav_pane, "text {text} opened the Find pane");
    }
}

#[test]
fn the_engine_refuses_edits_that_skip_the_app_gate() {
    // UI code that calls `Session::run` directly never passes `WordApp::run`'s gate.
    let (mut h, _bridge) = view_app(clauses(3), 1.0);
    let app = h.state_mut();
    let before = serde_json::to_string(&app.session.doc).unwrap();
    for id in ["text.insert", "file.save", "edit.undo", "review.suggestions"] {
        let result = app.session.run(id, &json!({"text": "x"}));
        assert!(matches!(&result, Err(why) if why.to_string().contains("viewing only")), "{id} ran in the engine: {result:?}");
    }
    assert_eq!(serde_json::to_string(&app.session.doc).unwrap(), before);
    assert!(!app.session.dirty);
}

#[test]
fn view_mode_allows_navigation_selection_copy_find_and_zoom() {
    let (mut h, _bridge) = view_app(clauses(3), 1.0);
    let app = h.state_mut();
    for id in ["caret.docEnd", "select.all", "view.zoomIn", "view.pageWidth"] {
        assert!(app.run(id, json!({})).is_ok(), "{id} was refused");
    }
    let copied = app.run("edit.copy", json!({})).unwrap();
    assert!(copied["text"].as_str().is_some_and(|text| text.contains("Clause 3.")), "{copied}");
    let found = app.run("edit.find", json!({"text": "Clause 2"})).unwrap();
    assert_eq!(found["count"], 1);
}

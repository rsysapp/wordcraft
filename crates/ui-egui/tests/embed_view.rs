//! The embed build's view mode: the document with page navigation, zoom and find only; no
//! chrome, no brand, no edit path.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::*;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;
use wordcraft_doc::{Pos, StoryRef};
use wordcraft_engine::{Selection, Session};
use wordcraft_ui_egui::host::{EmbedProfile, HostBridge};
use wordcraft_ui_egui::{Inbox, Services, WordApp};

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

/// The embed build's view mode with platform `services` attached, as a shell might pass them.
fn view_app_with(doc: wordcraft_doc::Document, services: Services) -> Harness<'static, WordApp> {
    let app = WordApp::new(Session::new(doc), services).with_host(HostBridge::new(), EmbedProfile::View);
    let mut h = harness(app, 1.0);
    frames(&mut h, 2);
    h
}

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wordcraft-embed-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn keyboard_and_clipboard_edits_change_nothing() {
    let (mut h, _bridge) = view_app(clauses(5), 1.0);
    let at = screen_of(&mut h, &Pos::body(0, 3));
    click(&mut h, at, egui::PointerButton::Primary);
    assert!(h.state().canvas.focused, "the canvas takes focus when clicked");
    let before = doc_json(&h);
    h.event(egui::Event::Text("typed".into()));
    h.key_press(egui::Key::Backspace);
    h.key_press(egui::Key::Delete);
    h.key_press(egui::Key::Enter);
    h.event(egui::Event::Paste("pasted".into()));
    h.event(egui::Event::Cut);
    for key in [egui::Key::Z, egui::Key::Y, egui::Key::B, egui::Key::S, egui::Key::O, egui::Key::P, egui::Key::N, egui::Key::H] {
        h.key_press_modifiers(egui::Modifiers::COMMAND, key);
    }
    frames(&mut h, 4);
    assert_eq!(doc_json(&h), before);
    let app = h.state();
    assert!(!app.session.dirty);
    assert!(app.dialog.is_none() && !app.ui.backstage);
}

/// A file dropped from disk, as the native shell delivers one.
#[derive(Debug)]
struct FileOnDisk(std::path::PathBuf);

impl egui::DroppedFile for FileOnDisk {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

#[test]
fn dropped_files_open_nothing() {
    let dir = scratch_dir("drop");
    let file = dir.join("other.docx");
    std::fs::write(&file, wordcraft_engine::io::save_bytes("other.docx", &clauses(1)).unwrap()).unwrap();
    let (mut h, _bridge) = view_app(clauses(5), 1.0);
    let before = doc_json(&h);
    h.input_mut().dropped_files.push(std::sync::Arc::new(FileOnDisk(file)));
    frames(&mut h, 3);
    assert_eq!(doc_json(&h), before);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn files_delivered_through_the_inbox_open_nothing() {
    // The web shell feeds its file picker and drag and drop through the inbox.
    let inbox = Inbox::default();
    let mut h = view_app_with(clauses(5), Services { inbox: Some(inbox.clone()), ..Default::default() });
    let before = doc_json(&h);
    let other = wordcraft_engine::io::save_bytes("other.docx", &clauses(1)).unwrap();
    inbox.lock().unwrap().extend([("other.docx".to_string(), other), ("picture.png".to_string(), vec![0x89, b'P', b'N', b'G'])]);
    frames(&mut h, 3);
    assert!(inbox.lock().unwrap().is_empty(), "the inbox was not drained, so this proves nothing");
    assert_eq!(doc_json(&h), before);
}

#[test]
fn the_comments_pane_cannot_rewrite_a_comment() {
    let mut editing = Session::new(clauses(3));
    editing.run("select.text", &json!({"text": "Supplier", "occurrence": 2})).unwrap();
    editing.run("review.newComment", &json!({"text": "Original remark"})).unwrap();
    // A full engine under a view-mode UI: the pane lists the comment, so its editor is reachable
    // and only the UI guard stands between it and the document.
    let mut app = WordApp::new(Session::new(editing.doc.clone()), Services::default());
    app.profile = EmbedProfile::View;
    app.session.view.comments_pane = true;
    let mut h = panes_harness(app);
    let before = doc_json(&h);
    h.get_by_role(Role::MultilineTextInput).focus();
    frames(&mut h, 2);
    h.get_by_role(Role::MultilineTextInput).type_text(" changed");
    frames(&mut h, 2);
    h.key_press(egui::Key::Escape);
    frames(&mut h, 3);
    assert_eq!(doc_json(&h), before, "the Comments pane wrote to the document");
}

#[test]
fn a_right_click_on_selected_text_opens_no_menu() {
    let (mut h, _bridge) = view_app(clauses(5), 1.0);
    let selected = Selection { anchor: Pos::body(0, 0), focus: Pos::body(0, 9) };
    h.state_mut().session.sel = selected.clone();
    let before = doc_json(&h);
    let at = screen_of(&mut h, &Pos::body(0, 3));
    click(&mut h, at, egui::PointerButton::Secondary);
    let text = drawn_text(&h);
    for item in ["Copy", "Cut", "Paste", "Font"] {
        assert!(!text.contains(item), "a menu offered {item} in view mode:\n{text}");
    }
    assert_eq!(h.state().session.sel, selected, "a right-click inside the selection keeps it");
    assert_eq!(doc_json(&h), before);
}

#[test]
fn a_click_never_asks_for_a_text_input_cursor() {
    let (mut h, _bridge) = view_app(clauses(5), 1.0);
    let at = screen_of(&mut h, &Pos::body(0, 3));
    click(&mut h, at, egui::PointerButton::Primary);
    assert!(h.state().canvas.focused);
    assert!(h.output().platform_output.ime.is_none(), "an IME cursor focuses the text input and raises the on-screen keyboard");
}

#[test]
fn copy_still_copies_the_selection() {
    let (mut h, _bridge) = view_app(clauses(5), 1.0);
    let at = screen_of(&mut h, &Pos::body(0, 3));
    click(&mut h, at, egui::PointerButton::Primary);
    h.state_mut().session.sel = Selection { anchor: Pos::body(0, 0), focus: Pos::body(0, 9) };
    h.event(egui::Event::Copy);
    h.step();
    let commands = &h.output().platform_output.commands;
    assert!(commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == "Clause 1.")), "{commands:?}");
}

#[test]
fn double_click_in_the_header_does_not_start_header_editing() {
    let (mut h, _bridge) = view_app(clauses(5), 1.0);
    let before = doc_json(&h);
    let page = h.state().canvas.page_rects.first().copied().expect("a page");
    double_click(&mut h, egui::pos2(page.center().x, page.min.y + 20.0));
    assert_eq!(h.state().session.sel.focus.story, StoryRef::Body);
    assert_eq!(doc_json(&h), before);
}

#[test]
fn go_to_without_a_target_opens_no_dialog() {
    // Go To is navigation, so the gate lets it through; without a page or bookmark the engine asks
    // the UI to open the Go To dialog, which view mode must swallow.
    let (mut h, _bridge) = view_app(clauses(3), 1.0);
    let app = h.state_mut();
    assert!(app.run("edit.goto", json!({})).is_ok());
    assert!(app.dialog.is_none(), "view mode opened the {:?} dialog", app.dialog.as_ref().map(|d| d.name()));
}

#[test]
fn saving_and_exporting_never_download() {
    // The web shell turns these commands into downloads before they reach the engine's gate.
    let downloads: Rc<RefCell<Vec<String>>> = Rc::default();
    let recorder = downloads.clone();
    let services =
        Services { download: Some(Box::new(move |name: &str, _: &[u8]| recorder.borrow_mut().push(name.to_string()))), ..Default::default() };
    let mut h = view_app_with(clauses(3), services);
    for id in ["file.save", "file.saveAs", "file.exportPdf", "file.exportPng"] {
        let result = h.state_mut().run(id, json!({"path": "out.docx"}));
        assert!(matches!(&result, Err(why) if why.contains("viewing only")), "{id}: {result:?}");
    }
    let at = screen_of(&mut h, &Pos::body(0, 3));
    click(&mut h, at, egui::PointerButton::Primary);
    for key in [egui::Key::S, egui::Key::P] {
        h.key_press_modifiers(egui::Modifiers::COMMAND, key);
        h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, key);
    }
    frames(&mut h, 3);
    assert!(downloads.borrow().is_empty(), "downloaded {:?}", downloads.borrow());
}

#[test]
fn autosave_writes_nothing() {
    let dir = scratch_dir("autosave");
    let ready_to_autosave = |h: &mut Harness<'static, WordApp>, name: &str| {
        let path = dir.join(name);
        let app = h.state_mut();
        app.autosave = true;
        app.session.path = Some(path.clone());
        app.session.dirty = true;
        app.canvas.caret_visible_since = 0.0;
        frames(h, 3);
        path
    };
    // The same setup autosaves in the full app, so the view-mode assertion below is not vacuous.
    let mut full = full_app(clauses(3));
    assert!(ready_to_autosave(&mut full, "full.docx").exists(), "the full app did not autosave");

    let (mut h, _bridge) = view_app(clauses(3), 1.0);
    assert!(!h.state().autosave, "view mode starts with AutoSave off");
    let path = ready_to_autosave(&mut h, "view.docx");
    assert!(!path.exists(), "view mode autosaved");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn backstage_options_are_never_drawn() {
    // Backstage's Options page writes the author name and AutoSave directly, outside any command.
    let show_options = |h: &mut Harness<'static, WordApp>| {
        let app = h.state_mut();
        app.ui.backstage = true;
        app.ui.backstage_page = "options".into();
        frames(h, 3);
        drawn_text(h)
    };
    let mut full = full_app(clauses(3));
    assert!(show_options(&mut full).contains("User name:"), "the full app has no Options page to compare with");
    let (mut h, _bridge) = view_app(clauses(3), 1.0);
    let text = show_options(&mut h);
    assert!(!text.contains("User name:") && !text.contains("AutoSave"), "view mode drew Backstage:\n{text}");
}

#[test]
fn replace_is_refused_and_its_controls_are_never_drawn() {
    let (mut h, _bridge) = view_app(clauses(3), 1.0);
    let before = doc_json(&h);
    for id in ["edit.replace", "edit.replaceAll"] {
        let result = h.state_mut().run(id, json!({"text": "Supplier", "with": "Buyer"}));
        assert!(matches!(&result, Err(why) if why.contains("viewing only")), "{id}: {result:?}");
    }
    assert_eq!(doc_json(&h), before);
    let show_replace = |h: &mut Harness<'static, WordApp>| {
        let app = h.state_mut();
        app.dialog = wordcraft_ui_egui::dialogs::Dialog::open("replace", app);
        frames(h, 3);
        drawn_text(h)
    };
    let mut full = full_app(clauses(3));
    assert!(show_replace(&mut full).contains("Replace"), "the full app has no Replace controls to compare with");
    let text = show_replace(&mut h);
    assert!(!text.contains("Replace"), "view mode drew Replace controls:\n{text}");
}

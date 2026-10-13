//! The embed build's view mode: the document with page navigation, zoom and find only; no
//! chrome, no brand, no edit path.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::*;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
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
    let drop_file = |h: &mut Harness<'static, WordApp>| {
        let before = doc_json(h);
        h.input_mut().dropped_files.push(std::sync::Arc::new(FileOnDisk(file.clone())));
        frames(h, 3);
        (before, doc_json(h))
    };
    // The same drop opens the file in the full app, so the view-mode assertion is not vacuous.
    let mut full = full_app(clauses(5));
    let (before, after) = drop_file(&mut full);
    assert_ne!(after, before, "the full app did not open the dropped file");
    assert_eq!(full.state().session.path.as_deref(), Some(file.as_path()));

    let (mut h, _bridge) = view_app(clauses(5), 1.0);
    let (before, after) = drop_file(&mut h);
    assert_eq!(after, before, "view mode opened the dropped file");
    assert_ne!(h.state().session.path.as_deref(), Some(file.as_path()));
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
    // A full engine under each profile: the pane lists the comment, so its editor is reachable and
    // only the UI guard stands between it and the document.
    let edit_the_comment = |profile: EmbedProfile| {
        let mut app = WordApp::new(Session::new(editing.doc.clone()), Services::default());
        app.profile = profile;
        app.session.view.comments_pane = true;
        let mut h = panes_harness(app);
        let before = doc_json(&h);
        h.get_by_role(Role::MultilineTextInput).focus();
        frames(&mut h, 2);
        h.get_by_role(Role::MultilineTextInput).type_text(" changed");
        frames(&mut h, 2);
        h.key_press(egui::Key::Escape);
        frames(&mut h, 3);
        (before, doc_json(&h))
    };
    let (before, after) = edit_the_comment(EmbedProfile::Full);
    assert!(!before.contains("Original remark changed") && after.contains("Original remark changed"), "the full app's pane did not edit the comment");
    let (before, after) = edit_the_comment(EmbedProfile::View);
    assert_eq!(after, before, "the Comments pane wrote to the document");
}

#[test]
fn a_right_click_on_selected_text_opens_no_menu() {
    let selected = Selection { anchor: Pos::body(0, 0), focus: Pos::body(0, 9) };
    let right_click = |h: &mut Harness<'static, WordApp>, on: Pos| {
        h.state_mut().session.sel = selected.clone();
        let at = screen_of(h, &on);
        click(h, at, egui::PointerButton::Secondary);
        drawn_text(h)
    };
    // The same right-click opens the menu in the full app, so the view-mode assertion is not vacuous.
    let mut full = full_app(clauses(5));
    let text = right_click(&mut full, Pos::body(0, 3));
    assert!(
        text.contains("Copy") && text.contains("Cut"),
        "the full app showed no context menu:
{text}"
    );

    let (mut h, _bridge) = view_app(clauses(5), 1.0);
    let before = doc_json(&h);
    let text = right_click(&mut h, Pos::body(0, 3));
    for item in ["Copy", "Cut", "Paste", "Font"] {
        assert!(!text.contains(item), "a menu offered {item} in view mode:\n{text}");
    }
    assert_eq!(h.state().session.sel, selected, "a right-click inside the selection keeps it");
    assert_eq!(doc_json(&h), before);
}

#[test]
fn a_right_click_outside_the_selection_runs_no_menu_preparation() {
    // A right-click outside the selection moves the caret only to prepare the context menu; view
    // mode has none, so the selection stays (and the menu's spelling and thesaurus lookups never run).
    let selected = Selection { anchor: Pos::body(0, 0), focus: Pos::body(0, 9) };
    let right_click_outside = |h: &mut Harness<'static, WordApp>| {
        h.state_mut().session.sel = selected.clone();
        let at = screen_of(h, &Pos::body(2, 20));
        click(h, at, egui::PointerButton::Secondary);
        h.state().session.sel.clone()
    };
    let mut full = full_app(clauses(5));
    assert_ne!(right_click_outside(&mut full), selected, "the full app did not move the caret for its menu");
    let (mut h, _bridge) = view_app(clauses(5), 1.0);
    assert_eq!(right_click_outside(&mut h), selected);
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
fn focus_without_a_click_never_asks_for_a_text_input_cursor_either() {
    // Focus can also come from the host page or Tab, through `want_focus`, with no pointer event.
    let focus_without_a_click = |h: &mut Harness<'static, WordApp>| {
        h.state_mut().canvas.want_focus = true;
        frames(h, 3);
        assert!(h.state().canvas.focused, "the canvas did not take focus");
        h.output().platform_output.ime.is_some()
    };
    let mut full = full_app(clauses(5));
    assert!(focus_without_a_click(&mut full), "the full app published no IME cursor, so this proves nothing");
    let (mut h, _bridge) = view_app(clauses(5), 1.0);
    assert!(!focus_without_a_click(&mut h), "an IME cursor focuses the text input and raises the on-screen keyboard");
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
    // A header to double-click into.
    let mut with_header = wordcraft_engine::Session::new(clauses(5));
    with_header.run("insert.header", &json!({ "text": "Lease of the warehouse" })).expect("a header");
    with_header.run("insert.closeHeader", &json!({})).expect("back to the text");
    let doc = with_header.doc;
    // Control: the same double-click in the full application edits the header, so the helper does
    // double-click.
    let mut full = full_app(doc.clone());
    frames(&mut full, 3);
    let page = full.state().canvas.page_rects.first().copied().expect("a page");
    double_click(&mut full, egui::pos2(page.center().x, page.min.y + 20.0));
    assert!(matches!(full.state().session.sel.focus.story, StoryRef::Part(_)), "no double-click reached the app, so this proves nothing");

    let (mut h, _bridge) = view_app(doc, 1.0);
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
    let mut full = full_app(clauses(3));
    let app = full.state_mut();
    assert!(app.run("edit.goto", json!({})).is_ok());
    assert_eq!(app.dialog.as_ref().map(|d| d.name()), Some("goto"), "the full app opened no Go To dialog, so this proves nothing");

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
    let services = Services {
        download: Some(Box::new(move |name: &str, _: &[u8]| {
            recorder.borrow_mut().push(name.to_string());
            Ok(())
        })),
        ..Default::default()
    };
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

#[test]
fn the_view_bar_shows_the_page_and_moves_between_pages() {
    let (mut h, _bridge) = view_app(clauses(120), 1.0);
    let count = pages(&mut h);
    assert!(drawn_text(&h).contains(&format!("Page 1 of {count}")), "{}", drawn_text(&h));
    h.get_by_label("Next page").click();
    frames(&mut h, 4);
    assert_eq!(h.state().canvas.visible_page, Some(1));
    h.get_by_label("Previous page").click();
    frames(&mut h, 4);
    assert_eq!(h.state().canvas.visible_page, Some(0));
}

/// The scale the engine's Zoom in / Zoom out steps to from a displayed scale (whole tenths of 100%).
fn stepped_scale(displayed: f32, step: f32) -> f32 {
    let per_cent = wordcraft_ui_egui::canvas::PX_PER_PT / 100.0;
    let percent = (displayed / per_cent).round();
    (((percent / 100.0 * 10.0).round() / 10.0 + step).clamp(0.1, 5.0)) * 100.0 * per_cent
}

#[test]
fn the_view_bar_zooms_from_the_fitted_scale_in_wide_and_narrow_windows() {
    // The fitted scale is not the stored zoom (1.0): Zoom in must grow the page and Zoom out shrink it
    // whatever the window is, not step from 100%.
    for size in [egui::vec2(1800.0, 900.0), egui::vec2(1100.0, 800.0), egui::vec2(420.0, 700.0)] {
        let (mut h, _bridge) = view_app(clauses(10), 1.0);
        h.set_size(size);
        frames(&mut h, 3);
        let fitted = h.state().canvas.scale;
        h.get_by_label("Zoom in").click();
        frames(&mut h, 3);
        let zoomed_in = h.state().canvas.scale;
        assert!(zoomed_in > fitted, "{size:?}: Zoom in {fitted} -> {zoomed_in}");
        assert!((zoomed_in - stepped_scale(fitted, 0.1)).abs() < 0.01, "{size:?}: {fitted} -> {zoomed_in}");
        h.get_by_label("Fit page width").click();
        frames(&mut h, 3);
        assert!((h.state().canvas.scale - fitted).abs() < 0.01, "{size:?}: Fit page width did not return to the fitted scale");
        h.get_by_label("Zoom out").click();
        frames(&mut h, 3);
        let zoomed_out = h.state().canvas.scale;
        assert!(zoomed_out < fitted, "{size:?}: Zoom out {fitted} -> {zoomed_out}");
        assert!((zoomed_out - stepped_scale(fitted, -0.1)).abs() < 0.01, "{size:?}: {fitted} -> {zoomed_out}");
        // From a stored zoom the next step is one more tenth.
        h.get_by_label("Zoom out").click();
        frames(&mut h, 3);
        assert!((h.state().canvas.scale - stepped_scale(zoomed_out, -0.1)).abs() < 0.01, "{size:?}");
    }
}

#[test]
fn find_selects_matches_and_steps_through_them() {
    let (mut h, _bridge) = view_app(clauses(30), 1.0);
    h.get_by_role(Role::TextInput).focus();
    frames(&mut h, 2);
    h.get_by_role(Role::TextInput).type_text("schedule 2");
    frames(&mut h, 3);
    assert_eq!(h.state().session.selected_text(), "Schedule 2");
    let first = h.state().session.sel.clone();
    h.get_by_label("Next match").click();
    frames(&mut h, 3);
    assert_ne!(h.state().session.sel, first);
    assert!(h.state().session.selected_text().eq_ignore_ascii_case("schedule 2"));
    assert!(drawn_text(&h).contains("2 of "), "{}", drawn_text(&h));
}

#[test]
fn ctrl_f_focuses_the_find_field() {
    let (mut h, _bridge) = view_app(clauses(5), 1.0);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    frames(&mut h, 3);
    assert!(h.get_by_role(Role::TextInput).is_focused());
    assert!(h.state().dialog.is_none());
}

#[test]
fn the_page_buttons_stop_at_the_ends_instead_of_wrapping() {
    let (mut h, _bridge) = view_app(clauses(120), 1.0);
    let count = pages(&mut h);
    assert!(h.get_by_label("Previous page").accesskit_node().is_disabled(), "Previous is live on page 1");
    assert!(!h.get_by_label("Next page").accesskit_node().is_disabled());
    h.state_mut().canvas.visible_page = Some(count - 1);
    frames(&mut h, 1);
    assert!(h.get_by_label("Next page").accesskit_node().is_disabled(), "Next is live on the last page");
    assert!(!h.get_by_label("Previous page").accesskit_node().is_disabled());
}

#[test]
fn an_empty_document_shows_a_bar_that_cannot_navigate() {
    let (mut h, _bridge) = embed_app();
    frames(&mut h, 3);
    assert!(drawn_text(&h).contains("Page 1 of 1"), "{}", drawn_text(&h));
    assert!(h.get_by_label("Previous page").accesskit_node().is_disabled());
    assert!(h.get_by_label("Next page").accesskit_node().is_disabled());
    // A bar over no pages at all must not index into them.
    let before = doc_json(&h);
    wordcraft_ui_egui::host::view::go_to_page(h.state_mut(), 7);
    wordcraft_ui_egui::host::view::go_to_page(h.state_mut(), usize::MAX);
    frames(&mut h, 3);
    assert_eq!(doc_json(&h), before);
}

#[test]
fn find_reports_no_matches_and_an_empty_query_clears_the_highlight() {
    let (mut h, _bridge) = view_app(clauses(30), 1.0);
    h.get_by_role(Role::TextInput).focus();
    frames(&mut h, 2);
    h.get_by_role(Role::TextInput).type_text("zzz-nothing");
    frames(&mut h, 3);
    assert!(drawn_text(&h).contains("No matches"), "{}", drawn_text(&h));
    assert!(!h.get_by_label("Next match").accesskit_node().is_disabled());
    h.get_by_role(Role::TextInput).focus();
    frames(&mut h, 1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.key_press(egui::Key::Backspace);
    frames(&mut h, 1);
    h.get_by_role(Role::TextInput).type_text("Schedule 2");
    frames(&mut h, 3);
    assert_eq!(h.state().session.selected_text(), "Schedule 2");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.key_press(egui::Key::Backspace);
    frames(&mut h, 3);
    let app = h.state();
    assert!(app.session.find.results.is_empty());
    assert_eq!(app.session.selected_text(), "", "the match stayed highlighted");
    assert!(h.get_by_label("Next match").accesskit_node().is_disabled());
}

#[test]
fn enter_shift_enter_and_escape_work_in_the_find_field() {
    let (mut h, _bridge) = view_app(clauses(30), 1.0);
    h.get_by_role(Role::TextInput).focus();
    frames(&mut h, 2);
    h.get_by_role(Role::TextInput).type_text("schedule 2");
    frames(&mut h, 3);
    let first = h.state().session.sel.clone();
    h.key_press(egui::Key::Enter);
    frames(&mut h, 3);
    let second = h.state().session.sel.clone();
    assert_ne!(second, first, "Enter did not step to the next match");
    assert!(h.get_by_role(Role::TextInput).is_focused(), "Enter left the field");
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Enter);
    frames(&mut h, 3);
    assert_eq!(h.state().session.sel, first, "Shift+Enter did not step back");
    h.key_press(egui::Key::Escape);
    frames(&mut h, 3);
    assert!(!h.get_by_role(Role::TextInput).is_focused());
    assert!(h.get_by_role(Role::TextInput).value().unwrap_or_default().is_empty(), "Escape left the query");
    assert_eq!(h.state().session.selected_text(), "");
}

#[test]
fn the_view_bar_draws_no_replace_control() {
    let (mut h, _bridge) = view_app(clauses(5), 1.0);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    frames(&mut h, 3);
    let text = drawn_text(&h);
    assert!(!text.contains("Replace"), "{text}");
}

/// Type `text` one character at a time into the find field, as a person does.
fn type_into_find(h: &mut Harness<'static, WordApp>, text: &str) {
    h.get_by_role(Role::TextInput).focus();
    frames(h, 2);
    for letter in text.chars() {
        h.get_by_role(Role::TextInput).type_text(&letter.to_string());
        frames(h, 2);
    }
}

#[test]
fn typing_a_query_letter_by_letter_stays_on_its_first_match() {
    let (mut h, _bridge) = view_app(clauses(30), 1.0);
    type_into_find(&mut h, "schedule 2");
    let app = h.state();
    let first = app.session.find.results.first().cloned().expect("matches");
    assert_eq!(first.0, Pos::body(1, first.0.off), "the first match is in clause 2");
    assert_eq!(app.session.selected_text(), "Schedule 2");
    assert_eq!(app.session.sel.ordered().0, first.0, "typing skipped on to a later match");
}

#[test]
fn typing_searches_on_from_the_caret_and_stays_there() {
    let (mut h, _bridge) = view_app(clauses(30), 1.0);
    let caret = Pos::body(4, 0);
    h.state_mut().session.sel = Selection::caret(caret.clone());
    type_into_find(&mut h, "schedule 2");
    let app = h.state();
    let expected = app.session.find.results.iter().find(|(start, _)| *start >= caret).cloned().expect("a match after the caret");
    assert_eq!(app.session.sel.ordered().0, expected.0);
}

#[test]
fn typing_searches_the_body_when_the_caret_is_in_another_story() {
    let (mut h, _bridge) = view_app(clauses(30), 1.0);
    let elsewhere = Pos { story: StoryRef::Part(1), path: wordcraft_doc::Path::top(0), off: 0 };
    h.state_mut().session.sel = Selection::caret(elsewhere);
    type_into_find(&mut h, "schedule 2");
    let app = h.state();
    let first = app.session.find.results.first().cloned().expect("matches in the body");
    assert_eq!(first.0.story, StoryRef::Body);
    assert_eq!(app.session.sel.ordered().0, first.0);
    assert_eq!(app.session.selected_text(), "Schedule 2");
}

#[test]
fn the_match_count_is_not_shown_for_a_selection_that_moved_away() {
    let (mut h, _bridge) = view_app(clauses(30), 1.0);
    type_into_find(&mut h, "schedule 2");
    let count = h.state().session.find.results.len();
    assert!(drawn_text(&h).contains(&format!("1 of {count}")), "{}", drawn_text(&h));
    let at = screen_of(&mut h, &Pos::body(0, 3));
    click(&mut h, at, egui::PointerButton::Primary);
    frames(&mut h, 3);
    let text = drawn_text(&h);
    assert!(!text.contains(&format!("1 of {count}")), "stale position: {text}");
    assert!(text.contains(&format!("{count} matches")), "{text}");
}

#[test]
fn next_page_reaches_a_last_page_that_is_never_the_most_visible() {
    // Zoomed out so every page is in view, the first page stays "most visible" at any scroll offset,
    // so Next page must follow its own target to reach the last page.
    let (mut h, bridge) = view_app(clauses(40), 1.0);
    h.state_mut().run("view.zoom", json!({"value": 15})).unwrap();
    frames(&mut h, 4);
    let count = pages(&mut h);
    assert!(count >= 3, "{count} pages");
    bridge.take_events();
    for _ in 0..count {
        if h.get_by_label("Next page").accesskit_node().is_disabled() {
            break;
        }
        h.get_by_label("Next page").click();
        frames(&mut h, 4);
    }
    assert!(drawn_text(&h).contains(&format!("Page {count} of {count}")), "{}", drawn_text(&h));
    assert!(h.get_by_label("Next page").accesskit_node().is_disabled());
    assert!(!h.get_by_label("Previous page").accesskit_node().is_disabled());
    let events = bridge.take_events();
    assert_eq!(events.last(), Some(&wordcraft_ui_egui::host::HostEvent::PageChanged { page: count as u32 }), "{events:?}");
    // Previous page walks back the same way.
    h.get_by_label("Previous page").click();
    frames(&mut h, 4);
    assert!(drawn_text(&h).contains(&format!("Page {} of {count}", count - 1)), "{}", drawn_text(&h));
}

/// The label's page number ("Page 3 of 8" -> 3).
fn bar_page(h: &Harness<'static, WordApp>) -> Option<usize> {
    let text = drawn_text(h);
    let after = text.split("Page ").nth(1)?;
    after.split_whitespace().next()?.parse().ok()
}

/// Next page until it is disabled; returns the page it ended on.
fn next_to_the_end(h: &mut Harness<'static, WordApp>) -> usize {
    for _ in 0..40 {
        if h.get_by_label("Next page").accesskit_node().is_disabled() {
            break;
        }
        h.get_by_label("Next page").click();
        frames(h, 4);
    }
    bar_page(h).expect("a page label")
}

#[test]
fn a_layout_change_without_input_keeps_the_page_target() {
    let (mut h, bridge) = view_app(clauses(120), 1.0);
    h.get_by_label("Next page").click();
    frames(&mut h, 4);
    assert_eq!(bar_page(&h), Some(2));
    // The layout moves under the target with no scroll input: another zoom (as a settling layout or a
    // window resize would), over several frames.
    h.state_mut().run("view.zoom", json!({"value": 15})).unwrap();
    h.set_size(egui::vec2(1400.0, 1000.0));
    frames(&mut h, 6);
    assert_ne!(h.state().canvas.visible_page, Some(1), "the layout change did not move the visible page, so this proves nothing");
    assert_eq!(bar_page(&h), Some(2), "the target was dropped without any scroll input");
    let count = pages(&mut h);
    bridge.take_events();
    assert_eq!(next_to_the_end(&mut h), count);
    assert!(h.get_by_label("Next page").accesskit_node().is_disabled());
    let events = bridge.take_events();
    assert_eq!(events.last(), Some(&wordcraft_ui_egui::host::HostEvent::PageChanged { page: count as u32 }), "{events:?}");
}

#[test]
fn a_wheel_scroll_drops_the_page_target_and_the_host_follows_what_is_visible() {
    let (mut h, bridge) = view_app(clauses(120), 1.0);
    for _ in 0..2 {
        h.get_by_label("Next page").click();
        frames(&mut h, 4);
    }
    assert_eq!(bar_page(&h), Some(3));
    bridge.take_events();
    let canvas = h.state().canvas.canvas_rect.expect("a canvas");
    h.hover_at(canvas.center());
    frames(&mut h, 1);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 5000.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    frames(&mut h, 8);
    let visible = h.state().canvas.visible_page.expect("a visible page");
    assert!(visible < 2, "the wheel did not scroll up (visible page {visible})");
    assert_eq!(bar_page(&h), Some(visible + 1));
    let events = bridge.take_events();
    assert_eq!(events.last(), Some(&wordcraft_ui_egui::host::HostEvent::PageChanged { page: visible as u32 + 1 }), "{events:?}");
}

#[test]
fn a_scroll_key_in_the_document_drops_the_page_target() {
    let (mut h, _bridge) = view_app(clauses(40), 1.0);
    h.state_mut().run("view.zoom", json!({"value": 15})).unwrap();
    frames(&mut h, 4);
    let count = pages(&mut h);
    assert_eq!(next_to_the_end(&mut h), count);
    // A click in the text is not a scroll; the target stays.
    let at = screen_of(&mut h, &Pos::body(0, 3));
    click(&mut h, at, egui::PointerButton::Primary);
    frames(&mut h, 2);
    assert!(h.state().canvas.focused);
    assert_eq!(bar_page(&h), Some(count));
    h.key_press(egui::Key::ArrowDown);
    frames(&mut h, 3);
    assert_eq!(bar_page(&h), Some(h.state().canvas.visible_page.unwrap_or(0) + 1));
    assert_ne!(bar_page(&h), Some(count));
}

#[test]
fn a_zoom_from_the_bar_keeps_the_page_target_in_view() {
    let (mut h, _bridge) = view_app(clauses(120), 1.0);
    for _ in 0..2 {
        h.get_by_label("Next page").click();
        frames(&mut h, 4);
    }
    for label in ["Zoom out", "Zoom in", "Fit page width"] {
        h.get_by_label(label).click();
        frames(&mut h, 4);
        assert_eq!(bar_page(&h), Some(3), "{label} dropped the target");
        let app = h.state();
        let canvas = app.canvas.canvas_rect.expect("a canvas");
        let top = app.canvas.page_rects[2].min.y;
        assert!((top - canvas.min.y).abs() < 40.0, "{label}: page 3 starts at {top}, the view at {}", canvas.min.y);
    }
}

#[test]
fn dragging_the_scroll_bar_drops_the_page_target() {
    let (mut h, _bridge) = view_app(clauses(120), 1.0);
    for _ in 0..2 {
        h.get_by_label("Next page").click();
        frames(&mut h, 4);
    }
    assert_eq!(bar_page(&h), Some(3));
    let canvas = h.state().canvas.canvas_rect.expect("a canvas");
    let grip = egui::pos2(canvas.max.x - 4.0, canvas.min.y + 120.0);
    h.hover_at(grip);
    frames(&mut h, 2);
    h.event(press(grip, egui::PointerButton::Primary));
    frames(&mut h, 1);
    let end = egui::pos2(grip.x, canvas.max.y - 4.0);
    h.event(egui::Event::PointerMoved(egui::pos2(grip.x, grip.y + 40.0)));
    frames(&mut h, 1);
    h.event(egui::Event::PointerMoved(end));
    frames(&mut h, 2);
    h.event(release(end, egui::PointerButton::Primary));
    frames(&mut h, 4);
    let visible = h.state().canvas.visible_page.expect("a visible page");
    assert_eq!(bar_page(&h), Some(visible + 1), "the scroll bar drag left the target in place");
    assert_ne!(bar_page(&h), Some(3));
}

/// The embed build turns `brand` off; its full application then has no community link either.
#[test]
fn the_community_link_belongs_to_the_brand_feature() {
    let mut app = WordApp::new(Session::new(wordcraft_doc::Document::new()), Services::default());
    let result = app.run("ui.discord", json!({}));
    assert_eq!(app.canvas.open_url.is_some(), cfg!(feature = "brand"), "ui.discord answered {result:?}");
}

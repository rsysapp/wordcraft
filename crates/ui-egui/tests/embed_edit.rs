//! embed: edit mode of the embed build, driven through the host bridge.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;
use wordcraft_engine::doc::{Document, RevisionKind, StoryRef};
use wordcraft_engine::edit_mode::Refusal;
use wordcraft_ui_egui::WordApp;
use wordcraft_ui_egui::host::open::{MAX_OPEN_BYTES, TOO_LARGE};
use wordcraft_ui_egui::host::wire::{AuthorSpec, ModeRequest, PermissionsSpec};
use wordcraft_ui_egui::host::{EditorMode, HostBridge, HostCommand, HostEvent, JumpTarget, MarkSpec};

const DATE: &str = "2026-10-10T08:00:00Z";
const AUTHOR: &str = "Sari Wulan";

fn permissions(accept_reject_changes: bool) -> PermissionsSpec {
    PermissionsSpec { accept_reject_changes, organize_pages: true, fill_forms: true, redact: true, stamps: true, drawing: true }
}

fn edit_on(date: &str, accept_reject_changes: bool) -> HostCommand {
    HostCommand::SetMode(ModeRequest::Edit {
        author: AuthorSpec { id: "u-sari".into(), name: AUTHOR.into() },
        revision_date: date.into(),
        permissions: permissions(accept_reject_changes),
    })
}

fn edit(accept_reject_changes: bool) -> HostCommand {
    edit_on(DATE, accept_reject_changes)
}

fn docx(text: &str) -> Vec<u8> {
    wordcraft_engine::io::save_bytes("doc.docx", &Document::from_text(text)).unwrap()
}

/// An embedded editor with a Word file opened through the bridge.
fn opened() -> (Harness<'static, WordApp>, HostBridge) {
    let (mut harness, bridge) = common::view_app(Document::from_text(""), 1.0);
    bridge.send(HostCommand::Open { name: "lease.docx".into(), bytes: docx("Rent is due monthly.") });
    harness.run_steps(3);
    bridge.take_events();
    (harness, bridge)
}

fn editing(accept_reject_changes: bool) -> (Harness<'static, WordApp>, HostBridge) {
    let (mut harness, bridge) = opened();
    bridge.send(edit(accept_reject_changes));
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::ModeChanged { mode: EditorMode::Edit }]);
    (harness, bridge)
}

fn rent() -> JumpTarget {
    JumpTarget::Text { text: "Rent".into(), page: None, occurrence: 0 }
}

fn body(harness: &Harness<'static, WordApp>) -> String {
    harness.state().session.doc.plain_text(StoryRef::Body)
}

/// The canvas with the keyboard, as after the person clicked into it.
fn focus_canvas(harness: &mut Harness<'static, WordApp>) {
    harness.state_mut().canvas.want_focus = true;
    harness.run_steps(2);
}

#[test]
fn switches_to_edit_mode_and_back() {
    let (mut harness, bridge) = editing(false);
    assert!(harness.state().session.edit_rules().is_some());
    assert_eq!(harness.state().session.revision_date(), Some(DATE));
    bridge.send(HostCommand::SetMode(ModeRequest::View));
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::ModeChanged { mode: EditorMode::View }]);
    assert!(harness.state().session.view_only);
    assert!(harness.state().session.edit_rules().is_none());
}

#[test]
fn stays_in_view_mode_without_an_open_document() {
    let (mut harness, bridge) = common::view_app(Document::from_text(""), 1.0);
    bridge.send(edit(true));
    harness.run_steps(2);
    assert!(bridge.take_events().contains(&HostEvent::ModeChanged { mode: EditorMode::View }));
    assert!(harness.state().session.view_only);
}

// The contract's order for one open (D31): modeChanged, selectionCleared, jumpNotFound, opened.
#[test]
fn opening_a_document_returns_to_view_mode_first_and_answers_a_waiting_jump_before_opened() {
    let (mut harness, bridge) = editing(true);
    // A jump found in the same frame is still waiting for its scroll when the open runs.
    bridge.send(HostCommand::JumpTo(rent()));
    bridge.send(HostCommand::Open { name: "other.docx".into(), bytes: docx("Other.") });
    harness.run_steps(3);
    let events = bridge.take_events();
    assert_eq!(events.first(), Some(&HostEvent::ModeChanged { mode: EditorMode::View }), "{events:?}");
    let not_found = events.iter().position(|event| matches!(event, HostEvent::JumpNotFound { .. }));
    let opened = events.iter().position(|event| matches!(event, HostEvent::Opened { .. }));
    assert!(not_found.is_some() && not_found < opened, "{events:?}");
    assert!(harness.state().session.view_only && harness.state().session.edit_rules().is_none());
}

#[test]
fn sends_an_open_s_events_in_the_contract_s_order_in_view_mode() {
    let (mut harness, bridge) = opened();
    harness.state_mut().run("select.all", json!({})).unwrap();
    harness.run_steps(3);
    assert!(bridge.take_events().iter().any(|event| matches!(event, HostEvent::Selection { .. })));
    bridge.send(HostCommand::JumpTo(rent()));
    bridge.send(HostCommand::Open { name: "other.docx".into(), bytes: docx("Other.") });
    harness.run_steps(3);
    let events = bridge.take_events();
    let at = |wanted: fn(&HostEvent) -> bool| events.iter().position(wanted);
    let cleared = at(|event| matches!(event, HostEvent::SelectionCleared));
    let not_found = at(|event| matches!(event, HostEvent::JumpNotFound { .. }));
    let opened = at(|event| matches!(event, HostEvent::Opened { .. }));
    assert!(cleared.is_some() && not_found.is_some() && opened.is_some(), "{events:?}");
    assert!(cleared < not_found && not_found < opened, "{events:?}");
}

// X8 (D6): an open refused before parsing is an open like any other.
#[test]
fn a_too_large_open_leaves_edit_mode_and_leaves_nothing_to_edit() {
    let (mut harness, bridge) = editing(true);
    bridge.send(HostCommand::OpenTooLarge { name: "huge.docx".into(), len: MAX_OPEN_BYTES + 1 });
    harness.run_steps(2);
    assert_eq!(
        bridge.take_events(),
        vec![HostEvent::ModeChanged { mode: EditorMode::View }, HostEvent::OpenFailed { message: TOO_LARGE.to_string() }]
    );
    bridge.send(edit(true));
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::ModeChanged { mode: EditorMode::View }]);
    assert!(harness.state().session.view_only);
}

#[test]
fn a_failed_open_leaves_nothing_to_edit() {
    let (mut harness, bridge) = editing(false);
    bridge.send(HostCommand::Open { name: "broken.docx".into(), bytes: b"not a package".to_vec() });
    harness.run_steps(2);
    let events = bridge.take_events();
    assert_eq!(events.first(), Some(&HostEvent::ModeChanged { mode: EditorMode::View }), "{events:?}");
    assert!(matches!(events.last(), Some(HostEvent::OpenFailed { .. })), "{events:?}");
    bridge.send(edit(false));
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::ModeChanged { mode: EditorMode::View }]);
}

#[test]
fn types_tracked_changes_in_the_host_s_author_and_date() {
    let (mut harness, _bridge) = editing(false);
    harness.state_mut().run("text.insert", json!({ "text": "Monthly: " })).unwrap();
    let revision = harness.state().session.doc.revisions.last().cloned().unwrap();
    assert_eq!((revision.kind, revision.author.as_str(), revision.date.as_str()), (RevisionKind::Insert, AUTHOR, DATE));
    assert!(harness.state().session.doc.settings.track_changes);
}

// The host's date may carry milliseconds; Word keeps whole seconds.
#[test]
fn keeps_the_host_s_date_to_the_second() {
    let (mut harness, bridge) = opened();
    bridge.send(edit_on("2026-10-10T08:00:00.123Z", false));
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::ModeChanged { mode: EditorMode::Edit }]);
    assert_eq!(harness.state().session.revision_date(), Some(DATE));
}

// W3 obligation 2: a revision date is a real calendar day, refused before a session starts.
#[test]
fn refuses_a_revision_date_the_calendar_does_not_have() {
    use wordcraft_ui_egui::host::wire::parse_mode_request;
    let request = |date: &str| {
        format!(
            r#"{{"mode":"edit","author":{{"id":"u","name":"Sari"}},"revisionDate":"{date}","permissions":{{"acceptRejectChanges":false,"organizePages":true,"fillForms":true,"redact":true,"stamps":true,"drawing":true}}}}"#
        )
    };
    assert!(parse_mode_request(&request("2026-02-31T08:00:00Z")).is_err());
    assert!(parse_mode_request(&request("2028-02-29T08:00:00Z")).is_ok());
    // A request queued without the parser starts nothing either.
    let (mut harness, bridge) = opened();
    bridge.send(edit_on("2026-02-31T08:00:00Z", false));
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::ModeChanged { mode: EditorMode::View }]);
    assert!(harness.state().session.view_only && harness.state().session.edit_rules().is_none());
}

// W3 obligations 1 and 6: the host begins the engine's session once per entry into edit mode; the
// same request again keeps the session (its undo history and its own insertions).
#[test]
fn the_same_edit_request_again_keeps_the_session() {
    let (mut harness, bridge) = editing(false);
    harness.state_mut().run("text.insert", json!({ "text": "abc " })).unwrap();
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::DirtyChanged { dirty: true }]);
    bridge.send(edit(false));
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::ModeChanged { mode: EditorMode::Edit }]);
    assert!(harness.state().session.can_undo(), "the session's history stays");
    // Still this session's own insertion: deleting it removes it rather than marking it deleted.
    harness.state_mut().run("select.word", json!({})).unwrap();
    harness.state_mut().run("caret.docStart", json!({})).unwrap();
    harness.state_mut().run("caret.wordRight", json!({ "extend": true })).unwrap();
    harness.state_mut().run("text.delete", json!({})).unwrap();
    assert!(body(&harness).starts_with("Rent"), "{:?}", body(&harness));
}

// A new date on a clean document is a new session: its history starts afresh.
#[test]
fn another_date_on_a_clean_document_begins_a_new_session() {
    let (mut harness, bridge) = editing(false);
    harness.state_mut().run("text.insert", json!({ "text": "abc " })).unwrap();
    harness.state_mut().run("edit.undo", json!({})).unwrap();
    harness.run_steps(2);
    bridge.take_events();
    bridge.send(edit_on("2026-10-11T08:00:00Z", false));
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::ModeChanged { mode: EditorMode::Edit }]);
    assert!(!harness.state().session.can_undo());
    assert_eq!(harness.state().session.revision_date(), Some("2026-10-11T08:00:00Z"));
}

// W3-3 (P6-18): one date per save. Unsaved changes keep the date they were made with: another date
// is refused, in edit mode and on coming back to it, and the mode in force stays.
#[test]
fn refuses_another_date_while_the_document_has_unsaved_changes() {
    let refused = HostEvent::EditRefused { code: "invalid_edit_session".into() };
    let (mut harness, bridge) = editing(false);
    harness.state_mut().run("text.insert", json!({ "text": "abc " })).unwrap();
    harness.run_steps(2);
    bridge.take_events();
    bridge.send(edit_on("2026-10-11T08:00:00Z", false));
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![refused.clone(), HostEvent::ModeChanged { mode: EditorMode::Edit }]);
    assert_eq!(harness.state().session.revision_date(), Some(DATE));
    assert!(harness.state().session.can_undo(), "the session running goes on");
    bridge.send(HostCommand::SetMode(ModeRequest::View));
    harness.run_steps(2);
    bridge.take_events();
    bridge.send(edit_on("2026-10-11T08:00:00Z", false));
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![refused, HostEvent::ModeChanged { mode: EditorMode::View }]);
    // The same date is the same unsaved work: editing goes on, still dirty.
    bridge.send(edit(false));
    harness.run_steps(2);
    let events = bridge.take_events();
    assert_eq!(events.first(), Some(&HostEvent::ModeChanged { mode: EditorMode::Edit }), "{events:?}");
    assert!(!events.contains(&HostEvent::DirtyChanged { dirty: false }), "{events:?}");
    // A new open is clean, so a new date is fine again.
    bridge.send(HostCommand::Open { name: "lease.docx".into(), bytes: docx("Rent is due monthly.") });
    bridge.send(edit_on("2026-10-11T08:00:00Z", false));
    harness.run_steps(3);
    let events = bridge.take_events();
    assert!(events.contains(&HostEvent::ModeChanged { mode: EditorMode::Edit }), "{events:?}");
    assert_eq!(refusals(&events), 0, "{events:?}");
}

// D4: real input, not `run()`. eframe focuses its text agent, which IME and on-screen keyboards
// type into, only when the app claims the IME; a view-mode profile never does.
#[test]
fn takes_typed_text_ime_and_tab_from_real_input_and_claims_the_ime() {
    let (mut harness, bridge) = editing(false);
    focus_canvas(&mut harness);
    assert!(harness.output().platform_output.ime.is_some(), "edit mode claims the IME");
    harness.event(egui::Event::Text("Monthly ".into()));
    harness.run_steps(2);
    harness.event(egui::Event::Ime(egui::ImeEvent::Commit("é".into())));
    harness.run_steps(2);
    harness.key_press(egui::Key::Tab);
    harness.run_steps(2);
    let text = body(&harness);
    assert!(text.starts_with("Monthly é\tRent"), "{text:?}");
    assert_eq!(harness.state().session.doc.revisions.last().map(|revision| revision.author.clone()).as_deref(), Some(AUTHOR));
    assert!(bridge.take_events().contains(&HostEvent::DirtyChanged { dirty: true }));
}

// W3 obligation 7 (Ruling P6-2): Tab types a tab, Shift+Tab does nothing, and Escape (which arms
// the host's way out: Escape then Tab moves focus out of the editor in the host's wrapper) reaches
// the editor without changing the document or the mode.
#[test]
fn keeps_tab_for_a_tab_character_and_lets_escape_through_harmlessly() {
    let (mut harness, bridge) = editing(false);
    focus_canvas(&mut harness);
    harness.key_press(egui::Key::Tab);
    harness.run_steps(2);
    assert!(body(&harness).starts_with("\tRent"), "{:?}", body(&harness));
    assert!(harness.state().canvas.focused, "Tab stays in the editor");
    let before = common::doc_json(&harness);
    bridge.take_events();
    harness.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Tab);
    harness.run_steps(2);
    assert_eq!(common::doc_json(&harness), before, "Shift+Tab changes no list level or indent");
    // W3-3: and does nothing else: no refusal, told or shown.
    assert_eq!(harness.state().session.refusal, None);
    assert!(bridge.take_events().is_empty());
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert_eq!(common::doc_json(&harness), before);
    assert!(harness.state().session.edit_rules().is_some(), "Escape does not leave edit mode");
    assert!(bridge.take_events().is_empty());
}

#[test]
fn claims_no_ime_in_view_mode() {
    let (mut harness, _bridge) = opened();
    focus_canvas(&mut harness);
    assert!(harness.output().platform_output.ime.is_none());
}

#[test]
fn refuses_edits_outside_edit_mode_s_list_through_the_app() {
    let (mut harness, bridge) = editing(true);
    let before = common::doc_json(&harness);
    let ids = ["edit.pasteFormat", "edit.replaceAll", "para.indent", "review.trackChanges", "file.saveAs", "file.exportPdf", "ui.backstage"];
    for id in ids {
        assert!(harness.state_mut().run(id, json!({})).is_err(), "{id}");
        assert_eq!(harness.state().session.refusal, Some(Refusal::CommandNotAllowed), "{id}");
    }
    assert_eq!(common::doc_json(&harness), before);
    // W3-1: the host hears each refusal once, by its code.
    let told = HostEvent::EditRefused { code: "command_not_allowed".into() };
    assert_eq!(bridge.take_events(), vec![told; ids.len()]);
}

// W3 obligation 3: every refusal carries a code the host can map, the app's own gate's included.
#[test]
fn every_refusal_in_edit_mode_carries_a_code() {
    let (mut harness, _bridge) = editing(false);
    for (id, params, refusal) in [
        ("no.suchCommand", json!({}), Refusal::UnknownCommand),
        ("select.text", json!({}), Refusal::InvalidParameters),
        ("edit.cut", json!({}), Refusal::NotAvailable),
        ("review.acceptAll", json!({}), Refusal::DecisionNotAllowed),
        ("text.newParagraph", json!({}), Refusal::ParagraphBreakNotSupported),
    ] {
        let refused = harness.state_mut().run(id, params).unwrap_err();
        assert_eq!(harness.state().session.refusal, Some(refusal), "{id}: {refused}");
        assert!(refused.contains(refusal.code()), "{refused}");
    }
}

// W3 obligation 4.
#[test]
fn a_refused_paste_names_the_paste() {
    let (mut harness, _bridge) = editing(false);
    let refused = harness.state_mut().run("edit.paste", json!({ "text": "weekly\nor daily" })).unwrap_err();
    assert!(refused.starts_with("paste: "), "{refused}");
    assert_eq!(harness.state().session.refusal, Some(Refusal::ParagraphBreakNotSupported));
}

// Ruling W2-4: Enter is not available in edit mode yet. The key still does something visible: the
// editor says why, until the document changes or the caret moves.
#[test]
fn says_why_enter_did_nothing() {
    let (mut harness, bridge) = editing(false);
    focus_canvas(&mut harness);
    let before = common::doc_json(&harness);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    assert_eq!(common::doc_json(&harness), before);
    // W3-1: the host hears it once, by its code, to say it in its own words.
    assert_eq!(bridge.take_events(), vec![HostEvent::EditRefused { code: "paragraph_break_not_supported".into() }]);
    assert_eq!(harness.state().session.refusal, Some(Refusal::ParagraphBreakNotSupported));
    let notice = "New paragraphs cannot be added while editing yet. Shift+Enter adds a line break.";
    assert!(common::drawn_text(&harness).contains(notice), "the refusal is shown");
    assert!(harness.state().canvas.focused, "the notice takes no focus");
    harness.event(egui::Event::Text("x".into()));
    harness.run_steps(3);
    assert!(body(&harness).starts_with("xRent"), "{:?} {:?}", body(&harness), harness.state().session.refusal);
    assert!(!common::drawn_text(&harness).contains(notice), "typing ends the notice");
}

// W3 obligation 5: only the host's request names the author of the session's changes.
#[test]
fn nothing_but_the_host_s_request_renames_the_author() {
    let (mut harness, _bridge) = editing(false);
    assert!(harness.state_mut().run("file.setAuthor", json!({ "name": "Someone Else" })).is_err());
    harness.state_mut().session.author = "Someone Else".into();
    harness.state_mut().run("text.insert", json!({ "text": "Monthly: " })).unwrap();
    assert_eq!(harness.state().session.doc.revisions.last().map(|revision| revision.author.as_str()), Some(AUTHOR));
}

#[test]
fn shows_accept_and_reject_only_with_the_permission() {
    let (mut harness, _bridge) = editing(false);
    harness.run_steps(2);
    assert!(harness.query_by_label("Accept all").is_none());
    assert!(harness.query_by_label("Undo").is_some());
    let (mut legal, _legal_bridge) = editing(true);
    legal.run_steps(2);
    assert!(legal.query_by_label("Accept all").is_some());
}

#[test]
fn shows_no_edit_strip_in_view_mode() {
    let (mut harness, _bridge) = opened();
    harness.run_steps(2);
    assert!(harness.query_by_label("Undo").is_none());
}

// D20: a control in view mode, so an empty list in edit mode means hidden, not never found.
#[test]
fn hides_host_marks_while_editing_and_shows_them_again_in_view_mode() {
    let (mut harness, bridge) = opened();
    bridge.send(HostCommand::SetMarks(vec![MarkSpec { id: "m".into(), text: "Rent".into(), page: None, color: "#ffe2006b".into() }]));
    harness.run_steps(4);
    let marks = |harness: &Harness<'static, WordApp>| harness.state().host.as_ref().unwrap().mark_rects().len();
    assert_eq!(marks(&harness), 1, "the mark is painted in view mode");
    bridge.send(edit(true));
    harness.run_steps(2);
    assert_eq!(marks(&harness), 0);
    bridge.send(HostCommand::SetMode(ModeRequest::View));
    harness.run_steps(4);
    assert_eq!(marks(&harness), 1);
}

// D9: a selection reported in view mode is cleared before edit mode starts; none is reported
// while editing, and the one made while editing is not reported on leaving.
#[test]
fn clears_a_reported_selection_before_editing_and_reports_none_while_editing() {
    let (mut harness, bridge) = opened();
    harness.state_mut().run("select.all", json!({})).unwrap();
    harness.run_steps(3);
    assert!(bridge.take_events().iter().any(|event| matches!(event, HostEvent::Selection { .. })));
    bridge.send(edit(false));
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::SelectionCleared, HostEvent::ModeChanged { mode: EditorMode::Edit }]);
    harness.state_mut().run("caret.docStart", json!({})).unwrap();
    harness.state_mut().run("select.word", json!({})).unwrap();
    harness.run_steps(3);
    assert!(!bridge.take_events().iter().any(|event| matches!(event, HostEvent::Selection { .. } | HostEvent::SelectionCleared)));
    bridge.send(HostCommand::SetMode(ModeRequest::View));
    harness.run_steps(3);
    assert_eq!(bridge.take_events(), vec![HostEvent::ModeChanged { mode: EditorMode::View }]);
}

// D25: typing moves the caret and may scroll; the page a jump made current must not stick.
#[test]
fn typing_drops_a_jump_s_page_target_so_the_page_follows_the_caret() {
    let (mut harness, bridge) = editing(false);
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 1 }));
    harness.run_steps(2);
    assert_eq!(harness.state().host.as_ref().unwrap().page_target(), Some(0));
    harness.state_mut().run("text.insert", json!({ "text": "x" })).unwrap();
    harness.run_steps(2);
    assert_eq!(harness.state().host.as_ref().unwrap().page_target(), None);
}

#[test]
fn refuses_a_mode_request_that_is_not_well_formed() {
    use wordcraft_ui_egui::host::wire::parse_mode_request;
    let good = r#"{"mode":"edit","author":{"id":"u","name":"Sari"},"revisionDate":"2026-10-10T08:00:00.123Z","permissions":{"acceptRejectChanges":false,"organizePages":true,"fillForms":true,"redact":true,"stamps":true,"drawing":true}}"#;
    assert!(matches!(parse_mode_request(good), Ok(ModeRequest::Edit { .. })));
    assert_eq!(parse_mode_request(r#"{"mode":"view"}"#), Ok(ModeRequest::View));
    for bad in [
        r#"{"mode":"place-signature"}"#,
        &good.replace(r#""name":"Sari""#, r#""name":"  ""#),
        &good.replace(r#""name":"Sari""#, &format!(r#""name":"{}""#, "x".repeat(256))),
        &good.replace("2026-10-10T08:00:00.123Z", "yesterday"),
        &good.replace(r#""drawing":true"#, r#""drawing":"yes""#),
        &good.replace(r#","drawing":true"#, ""),
        &good.replace(r#""author":{"id":"u","name":"Sari"},"#, ""),
    ] {
        assert!(parse_mode_request(bad).is_err(), "{bad}");
    }
}

/// W1's save inspector over a save made through the host path: open, `setMode`, real input,
/// `requestSave`. Only the author's tracked changes, dated with the host's date, and nothing else.
#[test]
fn a_save_through_the_host_passes_the_save_inspector() {
    use wordcraft_docx::inspect::{Resolved, inspect};

    // The agreement as it was: one tracked insertion by an earlier author.
    let mut earlier =
        wordcraft_engine::Session::new(Document::from_text("Rent is due monthly on the first day.\nThe tenant keeps the premises clean."));
    earlier.author = "Ana Setia".into();
    earlier.run("review.trackChanges", &json!({ "value": true })).unwrap();
    earlier.run("select.text", &json!({ "text": "clean" })).unwrap();
    earlier.run("select.collapse", &json!({ "end": true })).unwrap();
    earlier.run("text.insert", &json!({ "text": " and tidy" })).unwrap();
    let base = wordcraft_engine::io::save_bytes("lease.docx", &earlier.doc).unwrap();

    let (mut harness, bridge) = common::view_app(Document::from_text(""), 1.0);
    bridge.send(HostCommand::Open { name: "lease.docx".into(), bytes: base.clone() });
    harness.run_steps(3);
    bridge.send(edit_on("2026-10-10T08:00:00.123Z", false));
    harness.run_steps(2);
    focus_canvas(&mut harness);
    harness.event(egui::Event::Text("Monthly ".into()));
    harness.run_steps(2);
    harness.key_press(egui::Key::Tab);
    harness.run_steps(2);
    // Delete the earlier author's word "Rent" (marked deleted) by selecting it from the caret.
    for _ in 0..4 {
        harness.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::ArrowRight);
        harness.run_steps(1);
    }
    harness.key_press(egui::Key::Delete);
    harness.run_steps(2);
    bridge.take_events();
    bridge.send(HostCommand::RequestSave);
    harness.run_steps(2);
    let events = bridge.take_events();
    let (file_name, bytes) = events
        .iter()
        .find_map(|event| match event {
            HostEvent::SaveRequested { file_name, bytes } => Some((file_name.clone(), bytes.clone())),
            _ => None,
        })
        .expect("a save answer");
    assert_eq!(file_name, "lease.docx");

    let inspection = inspect(&base, &bytes).unwrap();
    assert_eq!((inspection.untracked_changes, inspection.altered_revisions), (0, 0), "{inspection:#?}");
    assert!(inspection.losses.is_empty() && !inspection.too_many_changes, "{inspection:#?}");
    assert!(
        inspection.added_parts.is_empty() && inspection.changed_relationships.is_empty() && inspection.content_types.is_empty(),
        "{inspection:#?}"
    );
    assert_eq!(inspection.comments, Default::default());
    assert_eq!(inspection.resolved, Resolved::default());
    assert_eq!(inspection.new_revisions.len(), 1, "{inspection:#?}");
    let revisions = &inspection.new_revisions[0];
    assert_eq!(revisions.author, AUTHOR);
    assert!(revisions.insertions >= 1 && revisions.deletions >= 1, "{inspection:#?}");
    assert_eq!((revisions.earliest_date.as_deref(), revisions.latest_date.as_deref(), revisions.undated), (Some(DATE), Some(DATE), 0));
    assert_eq!(revisions.format_changes, 0);
    assert_eq!((revisions.hidden_text, revisions.field_codes, revisions.bidi_controls, revisions.objects), (0, 0, 0, 0));
}

// ---- Fix round 1 (Rulings W3-1 to W3-3) ----

const ENTER_NOTICE: &str = "New paragraphs cannot be added while editing yet. Shift+Enter adds a line break.";

fn refusals(events: &[HostEvent]) -> usize {
    events.iter().filter(|event| matches!(event, HostEvent::EditRefused { .. })).count()
}

// W3-1: a paste refused inside the gate's own rewrite (Ctrl/⌘+V to plain paste) is one action.
#[test]
fn tells_the_host_of_a_refused_paste_once() {
    let (mut harness, bridge) = editing(false);
    focus_canvas(&mut harness);
    harness.event(egui::Event::Paste("weekly\nor daily".into()));
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::EditRefused { code: "paragraph_break_not_supported".into() }]);
}

// W3-1: searching and moving through changes that find nothing refuse no edit.
#[test]
fn navigation_that_finds_nothing_tells_and_shows_nothing() {
    let (mut harness, bridge) = editing(false);
    for (id, params) in [
        ("edit.findNext", json!({ "text": "absent" })),
        ("edit.findPrevious", json!({ "text": "absent" })),
        ("edit.goto", json!({ "bookmark": "absent" })),
        ("review.nextChange", json!({})),
        ("review.previousChange", json!({})),
    ] {
        let _ = harness.state_mut().run(id, params);
        assert_eq!(harness.state().session.refusal, None, "{id}");
    }
    harness.run_steps(2);
    let events = bridge.take_events();
    assert_eq!(refusals(&events), 0, "{events:?}");
    assert!(!common::drawn_text(&harness).contains("while editing"), "no notice");
}

// W3-3: the notice ends when the caret moves, not only when the document changes.
#[test]
fn a_caret_move_ends_the_notice() {
    let (mut harness, _bridge) = editing(false);
    focus_canvas(&mut harness);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    assert!(common::drawn_text(&harness).contains(ENTER_NOTICE));
    harness.key_press(egui::Key::ArrowRight);
    harness.run_steps(3);
    assert!(!common::drawn_text(&harness).contains(ENTER_NOTICE));
}

// W3-3: the notice never covers the line being edited: with the caret's line at the view's top, it
// goes to the bottom.
#[test]
fn the_notice_never_covers_the_caret_s_line() {
    let (mut harness, bridge) = common::view_app(Document::from_text(""), 1.0);
    let text = common::clauses(60);
    bridge.send(HostCommand::Open { name: "lease.docx".into(), bytes: common::docx(&text) });
    harness.run_steps(4);
    bridge.send(edit(false));
    harness.run_steps(2);
    focus_canvas(&mut harness);
    let at = wordcraft_engine::doc::Pos::body(20, 0);
    let (page, top) = {
        let app = harness.state_mut();
        app.session.sel = wordcraft_engine::Selection::caret(at.clone());
        let layout = app.session.layout();
        let caret = layout.caret_on(&at, app.session.page_hint).unwrap();
        (caret.page, caret.top)
    };
    harness.state_mut().canvas.scroll_to_doc = Some((page, (top - 2.0).max(0.0)));
    harness.run_steps(3);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    assert!(common::drawn_text(&harness).contains(ENTER_NOTICE));
    let notice = harness.ctx.memory(|memory| memory.area_rect(egui::Id::new("embed_edit_notice"))).expect("the notice is shown");
    let app = harness.state_mut();
    let layout = app.session.layout();
    let caret = layout.caret_on(&app.session.sel.focus, app.session.page_hint).unwrap();
    let line_top = wordcraft_ui_egui::canvas::page_to_screen(app, caret.page, caret.x, caret.top).unwrap().y;
    let line_bottom = wordcraft_ui_egui::canvas::page_to_screen(app, caret.page, caret.x, caret.top + caret.height).unwrap().y;
    let view = app.canvas.canvas_rect.unwrap();
    assert!(line_top < view.top() + 60.0, "the caret's line is at the view's top: {line_top} in {view:?}");
    assert!(notice.bottom() < line_top || notice.top() > line_bottom, "{notice:?} covers {line_top}..{line_bottom}");
}

/// A lease with a header, made before it is opened: edit mode never makes one.
fn with_header() -> Vec<u8> {
    let mut session = wordcraft_engine::Session::new(Document::from_text("Rent is due monthly."));
    session.run("insert.header", &json!({ "text": "Lease of the warehouse" })).unwrap();
    session.run("insert.closeHeader", &json!({})).unwrap();
    wordcraft_engine::io::save_bytes("lease.docx", &session.doc).unwrap()
}

// W3-3: a header the document has is entered, typed into (tracked) and left with Escape; one it has
// not is never made.
#[test]
fn enters_an_existing_header_and_leaves_it_with_escape() {
    let (mut harness, bridge) = common::view_app(Document::from_text(""), 1.0);
    bridge.send(HostCommand::Open { name: "lease.docx".into(), bytes: with_header() });
    harness.run_steps(3);
    bridge.send(edit(false));
    harness.run_steps(2);
    focus_canvas(&mut harness);
    bridge.take_events();
    harness.state_mut().run("insert.editHeader", json!({})).unwrap();
    harness.run_steps(2);
    let story = harness.state().session.sel.focus.story;
    assert!(matches!(story, StoryRef::Part(_)), "{story:?}");
    harness.event(egui::Event::Text(" draft".into()));
    harness.run_steps(2);
    assert!(harness.state().session.doc.plain_text(story).contains("warehouse draft"));
    assert_eq!(harness.state().session.doc.revisions.last().map(|revision| revision.author.as_str()), Some(AUTHOR));
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert_eq!(harness.state().session.sel.focus.story, StoryRef::Body);
    assert_eq!(refusals(&bridge.take_events()), 0);

    let (mut bare, bare_bridge) = editing(false);
    let parts = bare.state().session.doc.parts.len();
    assert!(bare.state_mut().run("insert.editHeader", json!({})).is_err());
    assert_eq!(bare.state().session.doc.parts.len(), parts, "no header was made");
    assert_eq!(bare_bridge.take_events(), vec![HostEvent::EditRefused { code: "command_not_allowed".into() }]);
}

// W3-2: a save holds the document as it was when asked, whatever the same frame does next.
#[test]
fn answers_a_save_before_a_later_open_and_mode_change_in_the_same_frame() {
    let (mut harness, bridge) = editing(false);
    harness.state_mut().run("text.insert", json!({ "text": "Monthly " })).unwrap();
    harness.run_steps(2);
    bridge.take_events();
    bridge.send(HostCommand::RequestSave);
    bridge.send(HostCommand::Open { name: "other.docx".into(), bytes: docx("Other.") });
    bridge.send(edit(false));
    harness.run_steps(2);
    let events = bridge.take_events();
    let at = |wanted: fn(&HostEvent) -> bool| events.iter().position(wanted);
    let saved = at(|event| matches!(event, HostEvent::SaveRequested { .. }));
    let opened = at(|event| matches!(event, HostEvent::Opened { .. }));
    assert!(saved.is_some() && saved < opened, "{events:?}");
    let Some(HostEvent::SaveRequested { file_name, bytes }) = saved.and_then(|index| events.get(index)) else { unreachable!() };
    assert_eq!(file_name, "lease.docx");
    let reopened = wordcraft_engine::io::open_bytes("lease.docx", bytes).unwrap();
    assert!(reopened.plain_text(StoryRef::Body).starts_with("Monthly Rent"), "the save holds the edited lease");
    assert!(events.contains(&HostEvent::ModeChanged { mode: EditorMode::Edit }), "{events:?}");
    assert_eq!(harness.state().session.revision_date(), Some(DATE));
}

#[test]
fn answers_a_save_before_a_later_switch_to_view_mode() {
    let (mut harness, bridge) = editing(false);
    harness.state_mut().run("text.insert", json!({ "text": "Monthly " })).unwrap();
    harness.run_steps(2);
    bridge.take_events();
    bridge.send(HostCommand::RequestSave);
    bridge.send(HostCommand::SetMode(ModeRequest::View));
    harness.run_steps(2);
    let events = bridge.take_events();
    assert!(matches!(events.first(), Some(HostEvent::SaveRequested { .. })), "{events:?}");
    assert_eq!(events.get(1), Some(&HostEvent::ModeChanged { mode: EditorMode::View }));
}

#[test]
fn takes_the_save_shortcut_as_a_save_request_and_never_downloads() {
    let (mut harness, bridge) = editing(false);
    focus_canvas(&mut harness);
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    harness.run_steps(2);
    let events = bridge.take_events();
    assert_eq!(events.iter().filter(|event| matches!(event, HostEvent::SaveRequested { .. })).count(), 1, "{events:?}");
}

#[test]
fn says_there_is_nothing_to_save_in_view_mode() {
    let (mut harness, bridge) = opened();
    bridge.send(HostCommand::RequestSave);
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::SaveFailed { message: "Nothing to save: the document is open for viewing only.".into() }]);
}

#[test]
fn answers_two_save_requests_in_one_frame_once() {
    let (mut harness, bridge) = editing(false);
    bridge.send(HostCommand::RequestSave);
    bridge.send(HostCommand::RequestSave);
    harness.run_steps(2);
    assert_eq!(bridge.take_events().iter().filter(|event| matches!(event, HostEvent::SaveRequested { .. })).count(), 1);
}

#[test]
fn reports_dirty_on_the_first_change_and_clean_after_undoing_it() {
    let (mut harness, bridge) = editing(false);
    harness.state_mut().run("text.insert", json!({ "text": "x" })).unwrap();
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::DirtyChanged { dirty: true }]);
    harness.state_mut().run("edit.undo", json!({})).unwrap();
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::DirtyChanged { dirty: false }]);
}

// W3-3: dirty is whether the document differs from what was opened, not how deep undo goes: past
// the undo limit, undoing everything left still leaves a changed document.
#[test]
fn stays_dirty_when_undo_runs_out_before_the_opened_document() {
    let (mut harness, bridge) = editing(false);
    for _ in 0..520 {
        harness.state_mut().run("text.insert", json!({ "text": "x" })).unwrap();
        harness.state_mut().run("caret.left", json!({})).unwrap();
    }
    harness.run_steps(2);
    while harness.state().session.can_undo() {
        harness.state_mut().run("edit.undo", json!({})).unwrap();
    }
    harness.run_steps(2);
    assert!(body(&harness).contains('x'), "the oldest edits are beyond undo");
    let events = bridge.take_events();
    assert!(events.contains(&HostEvent::DirtyChanged { dirty: true }), "{events:?}");
    assert!(!events.contains(&HostEvent::DirtyChanged { dirty: false }), "{events:?}");
}

#[test]
fn turns_the_comment_command_into_a_comment_request_on_the_selection() {
    let (mut harness, bridge) = editing(false);
    harness.state_mut().run("select.all", json!({})).unwrap();
    harness.run_steps(2);
    bridge.take_events();
    harness.state_mut().run("review.newComment", json!({ "text": "" })).unwrap();
    harness.run_steps(3);
    let events = bridge.take_events();
    assert!(
        events.iter().any(|event| matches!(event, HostEvent::CommentRequested { text, .. } if text.contains("Rent is due monthly"))),
        "{events:?}"
    );
    // Nothing was written into the file.
    assert!(harness.state().session.doc.comments.is_empty());
}

// W3-3: the edit strip wraps onto a second row when the bar is too narrow for it and find.
#[test]
fn wraps_the_edit_strip_in_a_narrow_view_bar() {
    let (mut harness, _bridge) = editing(true);
    harness.set_size(egui::vec2(760.0, 800.0));
    harness.run_steps(4);
    let strip = ["Undo", "Redo", "Previous change", "Next change", "Accept", "Reject", "Accept all", "Reject all"];
    let find = harness.get_by_label("Next match").rect();
    for label in strip {
        let rect = harness.get_by_label(label).rect();
        assert!(rect.top() >= find.bottom(), "{label} is on the second row, below find {find:?}: {rect:?}");
        assert!(rect.right() <= 760.0, "{label} is inside the window: {rect:?}");
        assert!(!rect.intersects(find), "{label} {rect:?} overlaps find {find:?}");
    }
    harness.set_size(egui::vec2(1800.0, 800.0));
    harness.run_steps(4);
    let accept_all = harness.get_by_label("Accept all").rect();
    let find = harness.get_by_label("Next match").rect();
    assert!((accept_all.center().y - find.center().y).abs() < 2.0, "one row when it fits: {accept_all:?}, find {find:?}");
}

#[test]
fn accepts_author_fields_it_does_not_read() {
    use wordcraft_ui_egui::host::wire::parse_mode_request;
    let request = r#"{"mode":"edit","author":{"id":"u","name":"Sari","email":"sari@example.com"},"revisionDate":"2026-10-10T08:00:00Z","permissions":{"acceptRejectChanges":false,"organizePages":true,"fillForms":true,"redact":true,"stamps":true,"drawing":true}}"#;
    assert!(matches!(parse_mode_request(request), Ok(ModeRequest::Edit { .. })));
}

// W3-5: the author of unsaved changes is fixed like their date: another author (id or name) is
// refused while the document is dirty, and starts a new session on a clean one.
#[test]
fn refuses_another_author_while_the_document_has_unsaved_changes() {
    let as_author = |id: &str, name: &str| {
        HostCommand::SetMode(ModeRequest::Edit {
            author: AuthorSpec { id: id.into(), name: name.into() },
            revision_date: DATE.into(),
            permissions: permissions(false),
        })
    };
    let refused = HostEvent::EditRefused { code: "invalid_edit_session".into() };
    let (mut harness, bridge) = editing(false);
    harness.state_mut().run("text.insert", json!({ "text": "abc " })).unwrap();
    harness.run_steps(2);
    bridge.take_events();
    for request in [as_author("u-other", AUTHOR), as_author("u-sari", "Ana Setia")] {
        bridge.send(request);
        harness.run_steps(2);
        assert_eq!(bridge.take_events(), vec![refused.clone(), HostEvent::ModeChanged { mode: EditorMode::Edit }]);
    }
    harness.state_mut().run("text.insert", json!({ "text": "x" })).unwrap();
    assert_eq!(harness.state().session.doc.revisions.last().map(|revision| revision.author.as_str()), Some(AUTHOR));
    // From view mode too: the mode in force stays.
    bridge.send(HostCommand::SetMode(ModeRequest::View));
    harness.run_steps(2);
    bridge.take_events();
    bridge.send(as_author("u-ana", "Ana Setia"));
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![refused, HostEvent::ModeChanged { mode: EditorMode::View }]);
    // A clean document takes another author: a new session.
    let (mut clean, clean_bridge) = editing(false);
    clean_bridge.send(as_author("u-ana", "Ana Setia"));
    clean.run_steps(2);
    assert_eq!(clean_bridge.take_events(), vec![HostEvent::ModeChanged { mode: EditorMode::Edit }]);
    clean.state_mut().run("text.insert", json!({ "text": "x" })).unwrap();
    assert_eq!(clean.state().session.doc.revisions.last().map(|revision| revision.author.as_str()), Some("Ana Setia"));
}

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

// A new date is a new session: its history starts afresh, but the document still differs from what
// was opened, so it stays dirty.
#[test]
fn another_date_begins_a_new_session_and_the_document_stays_dirty() {
    let (mut harness, bridge) = editing(false);
    harness.state_mut().run("text.insert", json!({ "text": "abc " })).unwrap();
    harness.run_steps(2);
    bridge.take_events();
    bridge.send(edit_on("2026-10-11T08:00:00Z", false));
    harness.run_steps(2);
    assert_eq!(bridge.take_events(), vec![HostEvent::ModeChanged { mode: EditorMode::Edit }]);
    assert!(!harness.state().session.can_undo());
    assert_eq!(harness.state().session.revision_date(), Some("2026-10-11T08:00:00Z"));
    // Leaving and coming back is a new entry as well; the edits are still unsaved.
    bridge.send(HostCommand::SetMode(ModeRequest::View));
    bridge.send(edit(false));
    harness.run_steps(2);
    let events = bridge.take_events();
    assert!(!events.contains(&HostEvent::DirtyChanged { dirty: false }), "{events:?}");
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
    harness.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Tab);
    harness.run_steps(2);
    assert_eq!(common::doc_json(&harness), before, "Shift+Tab changes no list level or indent");
    bridge.take_events();
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
    let (mut harness, _bridge) = editing(true);
    let before = common::doc_json(&harness);
    for id in
        ["edit.pasteFormat", "edit.replaceAll", "text.backTab", "para.indent", "review.trackChanges", "file.saveAs", "file.exportPdf", "ui.backstage"]
    {
        assert!(harness.state_mut().run(id, json!({})).is_err(), "{id}");
        assert_eq!(harness.state().session.refusal, Some(Refusal::CommandNotAllowed), "{id}");
    }
    assert_eq!(common::doc_json(&harness), before);
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
// strip says why, until the document next changes.
#[test]
fn says_why_enter_did_nothing() {
    let (mut harness, _bridge) = editing(false);
    focus_canvas(&mut harness);
    let before = common::doc_json(&harness);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    assert_eq!(common::doc_json(&harness), before);
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

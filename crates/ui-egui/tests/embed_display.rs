//! embed: the clean display (API 0.3): tracked changes accepted, no balloons.
mod common;
use common::*;
use wordcraft_ui_egui::WordApp;
use wordcraft_ui_egui::host::wire::parse_open_options;
use wordcraft_ui_egui::host::{Display, HostBridge, HostCommand, OpenOptions};

type Editor = (egui_kittest::Harness<'static, WordApp>, HostBridge);

// DATE and TIME fields are not refreshed on the embed's open path (`io::open_docx_with_limits`), so
// neither display shows today's date in place of the file's own value; nothing here asserts it.
fn open_with(display: Display) -> Editor {
    let (mut harness, bridge) = embed_app();
    bridge.send(HostCommand::Open { name: "lease.docx".into(), bytes: tracked_lease_docx(), options: OpenOptions { display } });
    frames(&mut harness, 4);
    bridge.take_events();
    (harness, bridge)
}

#[test]
fn the_final_display_lays_out_accepted_wording_without_balloons() {
    let (mut harness, _bridge) = open_with(Display::Final);
    let app: &WordApp = harness.state();
    assert!(!app.session.view.show_markup, "tracked changes are shown accepted");
    assert!(!app.session.balloons_shown());
    let text = laid_out_text(&mut harness);
    assert!(!text.contains("thirty"), "the deleted wording is not laid out: {text:?}");
    assert!(text.contains("sixty"), "the inserted wording is: {text:?}");
}

#[test]
fn without_options_the_document_shows_as_stored() {
    let (mut harness, _bridge) = open_with(Display::AsStored);
    assert!(harness.state().session.view.show_markup);
    assert!(harness.state().session.prefs.markup.comments);
    let text = laid_out_text(&mut harness);
    assert!(text.contains("thirty") && text.contains("sixty"), "both wordings, marked up: {text:?}");
}

#[test]
fn the_view_gate_still_refuses_markup_commands() {
    let (mut harness, _bridge) = open_with(Display::Final);
    // The host's own state change is not a command the gate allows: the commands stay refused, and
    // the person cannot turn the markup back on from the embed.
    let result = harness.state_mut().run("review.markup", serde_json::json!({"value": "all"}));
    assert!(result.is_err(), "{result:?}");
    assert!(!harness.state().session.view.show_markup);
}

#[test]
fn the_next_open_shows_the_stored_display_again() {
    let (mut harness, bridge) = open_with(Display::Final);
    bridge.send(HostCommand::Open { name: "lease.docx".into(), bytes: tracked_lease_docx(), options: OpenOptions::default() });
    frames(&mut harness, 4);
    assert!(harness.state().session.view.show_markup);
    assert!(harness.state().session.prefs.markup.comments);
    assert!(laid_out_text(&mut harness).contains("thirty"));
}

#[test]
fn a_host_that_sends_no_options_leaves_the_view_settings_alone() {
    // API 0.2: nothing in the open path touches the markup settings, so a setting made some other
    // way (here the engine directly) survives an open that asks for the stored display.
    let (mut harness, bridge) = embed_app();
    harness.state_mut().session.view.show_markup = false;
    bridge.send(HostCommand::Open { name: "lease.docx".into(), bytes: tracked_lease_docx(), options: OpenOptions::default() });
    frames(&mut harness, 4);
    assert!(!harness.state().session.view.show_markup);
}

#[test]
fn open_options_parse_and_refuse_unknown_displays() {
    assert_eq!(parse_open_options(r#"{"display":"final"}"#).unwrap().display, Display::Final);
    assert_eq!(parse_open_options(r#"{"display":"asStored"}"#).unwrap().display, Display::AsStored);
    assert_eq!(parse_open_options("{}").unwrap(), OpenOptions::default());
    assert!(parse_open_options(r#"{"display":"simple"}"#).is_err());
    assert!(parse_open_options(r#"{"display":3}"#).is_err());
}

//! `find` and the clean display (embed API 0.3, 0.4.1): in the clean display the search reads each
//! paragraph as it is shown, tracked changes accepted, so a phrase across a deleted run is found and
//! wording the display does not show (deleted or hidden) is not. As stored, find is unchanged.

mod common;

use common::*;
use wordcraft_doc::Pos;
use wordcraft_ui_egui::host::view::{FindStep, run_find};
use wordcraft_ui_egui::host::{FindRequest, FindStepSpec, HostBridge, HostCommand, HostEvent};

use egui_kittest::Harness;
use wordcraft_ui_egui::WordApp;

/// "Pay 30 45 days." with "30 " struck out, a second "Pay 45 days" that is plain, and a paragraph with
/// "confidential " hidden.
fn lease() -> Vec<u8> {
    docx_with_changes("Pay 30 45 days.\nPay 45 days again.\nKeep confidential terms secret.", &["30 "], &["confidential "])
}

fn ask(h: &mut Harness<'static, WordApp>, bridge: &HostBridge, text: &str, step: FindStepSpec, select: bool, token: u32) -> (u32, Option<u32>) {
    bridge.send(HostCommand::Find(FindRequest { text: text.into(), match_case: false, step, select, token }));
    frames(h, 3);
    bridge
        .take_events()
        .into_iter()
        .find_map(|event| match event {
            HostEvent::FindResult { count, index, token: answered } if answered == token => Some((count, index)),
            _ => None,
        })
        .expect("a findResult")
}

fn open_as_stored(bytes: Vec<u8>) -> (Harness<'static, WordApp>, HostBridge) {
    let (mut h, bridge) = embed_app();
    bridge.send(HostCommand::Open { name: "changes.docx".into(), bytes, options: Default::default() });
    frames(&mut h, 4);
    bridge.take_events();
    (h, bridge)
}

#[test]
fn a_phrase_across_a_deletion_is_found_and_selected_in_the_clean_display() {
    let (mut h, bridge) = embed_app_final(&lease(), true);
    assert_eq!(laid_out_text(&mut h).lines().next(), Some("Pay 45 days."), "what the display shows");
    assert_eq!(ask(&mut h, &bridge, "Pay 45 days", FindStepSpec::First, true, 1), (2, Some(0)));
    let (start, end) = h.state().session.sel.ordered();
    // The match runs from "Pay" to "days", over the deleted "30 ": the selection covers it.
    assert_eq!((start, end), (Pos::body(0, 0), Pos::body(0, "Pay 30 45 days".len())));
    assert_eq!(ask(&mut h, &bridge, "Pay 45 days", FindStepSpec::Next, true, 2), (2, Some(1)));
    assert_eq!(h.state().session.sel.ordered().0, Pos::body(1, 0));
    assert_eq!(ask(&mut h, &bridge, "Pay 45 days", FindStepSpec::Previous, true, 3), (2, Some(0)));
    assert_eq!(h.state().session.sel.ordered().0, Pos::body(0, 0));
}

#[test]
fn a_count_only_find_counts_what_the_display_shows() {
    let (mut h, bridge) = embed_app_final(&lease(), true);
    let before = h.state().session.sel.clone();
    assert_eq!(ask(&mut h, &bridge, "Pay 45 days", FindStepSpec::First, false, 4), (2, Some(0)));
    assert_eq!(ask(&mut h, &bridge, "45 days", FindStepSpec::First, false, 5), (2, Some(0)));
    assert_eq!(h.state().session.sel, before, "nothing was selected");
}

#[test]
fn wording_the_display_does_not_show_is_not_found() {
    let (mut h, bridge) = embed_app_final(&lease(), true);
    // "30" exists only inside the deletion; "confidential" only as hidden text.
    for (token, text) in [(1, "30"), (2, "30 45"), (3, "confidential"), (4, "Keep confidential")] {
        assert_eq!(ask(&mut h, &bridge, text, FindStepSpec::First, true, token), (0, None), "{text}");
        assert_eq!(ask(&mut h, &bridge, text, FindStepSpec::First, false, token + 10), (0, None), "{text}, counted");
    }
    // The visible words around hidden text join, as they read.
    assert_eq!(ask(&mut h, &bridge, "Keep terms secret", FindStepSpec::First, true, 20), (1, Some(0)));
}

#[test]
fn a_match_inside_one_run_is_found_where_it_always_was() {
    let (mut h, bridge) = embed_app_final(&lease(), true);
    assert_eq!(ask(&mut h, &bridge, "days", FindStepSpec::First, true, 1), (2, Some(0)));
    assert_eq!(h.state().session.sel.ordered(), (Pos::body(0, "Pay 30 45 ".len()), Pos::body(0, "Pay 30 45 days".len())));
    // Case is ignored unless asked for.
    bridge.send(HostCommand::Find(FindRequest { text: "PAY 45".into(), match_case: true, step: FindStepSpec::First, select: false, token: 2 }));
    frames(&mut h, 3);
    assert!(bridge.take_events().contains(&HostEvent::FindResult { count: 0, index: None, token: 2 }));
    // Counting steps from the selection: the first match at or after "days" in the first paragraph is the second.
    assert_eq!(ask(&mut h, &bridge, "PAY 45", FindStepSpec::First, false, 3), (2, Some(1)));
}

#[test]
fn the_view_bar_s_own_find_reads_the_same_text() {
    let (mut h, _bridge) = embed_app_final(&lease(), true);
    run_find(h.state_mut(), "Pay 45 days", FindStep::First);
    assert_eq!(h.state().session.find.results.len(), 2);
    assert_eq!(h.state().session.sel.ordered(), (Pos::body(0, 0), Pos::body(0, "Pay 30 45 days".len())));
    run_find(h.state_mut(), "Pay 45 days", FindStep::Next);
    assert_eq!(h.state().session.sel.ordered().0, Pos::body(1, 0));
}

#[test]
fn as_stored_find_is_what_it_was() {
    let (mut h, bridge) = open_as_stored(lease());
    // The engine reads live text only, cut at a tracked deletion: no match across it, and hidden text
    // is searched.
    assert_eq!(ask(&mut h, &bridge, "Pay 45 days", FindStepSpec::First, true, 1), (1, Some(0)));
    assert_eq!(h.state().session.sel.ordered().0, Pos::body(1, 0), "only the plain one");
    assert_eq!(ask(&mut h, &bridge, "Pay 45 days", FindStepSpec::First, false, 2), (1, Some(0)));
    assert_eq!(ask(&mut h, &bridge, "30", FindStepSpec::First, false, 3), (0, None), "deleted wording is not searched");
    assert_eq!(ask(&mut h, &bridge, "Keep confidential", FindStepSpec::First, false, 4), (1, Some(0)));
}

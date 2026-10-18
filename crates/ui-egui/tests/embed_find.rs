//! `find` (embed API 0.3): the host drives the search, hears the count and the position, and the
//! scroll a stepping find causes comes back as its own, with its token, so the other side of a
//! synchronised pair never mistakes it for the reviewer.

mod common;

use common::*;
use egui_kittest::Harness;
use wordcraft_doc::Pos;
use wordcraft_engine::Selection;
use wordcraft_ui_egui::WordApp;
use wordcraft_ui_egui::host::{Cause, FindRequest, FindStepSpec, HostBridge, HostCommand, HostEvent};

/// Where the first page's top is on screen: it moves whenever the view scrolls.
fn first_page_top(h: &mut Harness<'static, WordApp>) -> f32 {
    wordcraft_ui_egui::canvas::page_to_screen(h.state_mut(), 0, 0.0, 0.0).expect("the first page is laid out").y
}

fn find(h: &mut Harness<'static, WordApp>, bridge: &HostBridge, text: &str, step: FindStepSpec, token: u32) -> (u32, Option<u32>) {
    bridge.send(HostCommand::Find(FindRequest { text: text.into(), match_case: false, step, select: true, token }));
    frames(h, 3);
    bridge
        .take_events()
        .into_iter()
        .find_map(|event| match event {
            HostEvent::FindResult { count, index, token: answered } if answered == token => Some((count, index)),
            _ => None,
        })
        .unwrap()
}

#[test]
fn find_counts_steps_and_wraps() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(12));
    assert_eq!(find(&mut h, &bridge, "Clause 1", FindStepSpec::First, 1), (4, Some(0)));
    assert_eq!(find(&mut h, &bridge, "Clause 1", FindStepSpec::Next, 2), (4, Some(1)));
    assert_eq!(find(&mut h, &bridge, "Clause 1", FindStepSpec::Previous, 3), (4, Some(0)));
    assert_eq!(find(&mut h, &bridge, "Clause 1", FindStepSpec::Previous, 4), (4, Some(3)));
    assert_eq!(find(&mut h, &bridge, "Clause 1", FindStepSpec::Next, 5), (4, Some(0)), "next wraps past the last match");
}

#[test]
fn no_match_answers_zero_and_keeps_the_selection() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(3));
    let before = h.state().session.sel.clone();
    assert_eq!(find(&mut h, &bridge, "Indemnity", FindStepSpec::First, 9), (0, None));
    assert_eq!(h.state().session.sel, before);
}

#[test]
fn a_person_s_selection_survives_a_find_that_matches_nothing() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(3));
    h.state_mut().session.sel = Selection { anchor: Pos::body(1, 0), focus: Pos::body(1, 6) };
    frames(&mut h, 3);
    bridge.take_events();
    let before = h.state().session.sel.clone();
    assert_eq!(find(&mut h, &bridge, "Indemnity", FindStepSpec::First, 9), (0, None));
    assert_eq!(h.state().session.sel, before, "the range is still selected");
    assert_eq!(find(&mut h, &bridge, "Indemnity", FindStepSpec::Next, 10), (0, None));
    assert_eq!(h.state().session.sel, before);
}

#[test]
fn a_count_only_find_moves_nothing() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(40));
    let before = (h.state().session.sel.clone(), first_page_top(&mut h));
    bridge.send(HostCommand::Find(FindRequest { text: "Clause 3".into(), match_case: false, step: FindStepSpec::First, select: false, token: 6 }));
    frames(&mut h, 3);
    let events = bridge.take_events();
    assert!(events.contains(&HostEvent::FindResult { count: 11, index: Some(0), token: 6 }));
    assert_eq!((h.state().session.sel.clone(), first_page_top(&mut h)), before);
    assert!(!events.iter().any(|event| matches!(event, HostEvent::Viewport { .. })), "nothing scrolled: {events:?}");
}

#[test]
fn a_count_only_find_answers_the_position_after_the_caret() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(40));
    // The caret sits in clause 20: the first "Clause 3" at or after it is clause 30, the 2nd match (0-based 1).
    h.state_mut().session.sel = Selection::caret(Pos::body(19, 3));
    frames(&mut h, 2);
    bridge.take_events();
    let before = h.state().session.sel.clone();
    bridge.send(HostCommand::Find(FindRequest { text: "Clause 3".into(), match_case: false, step: FindStepSpec::First, select: false, token: 8 }));
    frames(&mut h, 3);
    assert!(bridge.take_events().contains(&HostEvent::FindResult { count: 11, index: Some(1), token: 8 }));
    assert_eq!(h.state().session.sel, before, "the caret stays");
}

#[test]
fn a_finds_scroll_is_reported_with_its_token() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.take_events();
    bridge.send(HostCommand::Find(FindRequest { text: "Clause 100".into(), match_case: false, step: FindStepSpec::First, select: true, token: 7 }));
    frames(&mut h, 4);
    let viewport = bridge.take_events().into_iter().rev().find(|event| matches!(event, HostEvent::Viewport { .. })).expect("the find scrolled");
    assert!(matches!(viewport, HostEvent::Viewport { cause: Cause::Host, token: Some(7), .. }), "{viewport:?}");
}

#[test]
fn the_match_is_centred_and_not_reported_as_a_selection() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.take_events();
    let before = first_page_top(&mut h);
    bridge.send(HostCommand::Find(FindRequest { text: "Clause 100".into(), match_case: false, step: FindStepSpec::First, select: true, token: 2 }));
    frames(&mut h, 5);
    assert!(first_page_top(&mut h) < before - 500.0, "the view moved down to the match");
    let events = bridge.take_events();
    assert!(!events.iter().any(|event| matches!(event, HostEvent::Selection { .. })), "the editor's own selection is not the person's: {events:?}");
    let (_, end) = h.state().session.sel.ordered();
    let (_, y) = selection_end_on_screen(&mut h, &end);
    assert!(y > WINDOW.y * 0.25 && y < WINDOW.y * 0.75, "the match sits mid-view, not at an edge: y = {y}");
}

#[test]
fn a_find_that_matches_nothing_scrolls_nothing() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.take_events();
    let before = first_page_top(&mut h);
    assert_eq!(find(&mut h, &bridge, "Indemnity", FindStepSpec::First, 4), (0, None));
    assert_eq!(first_page_top(&mut h), before);
}

#[test]
fn empty_text_clears_the_find() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(3));
    find(&mut h, &bridge, "Clause", FindStepSpec::First, 1);
    assert_eq!(find(&mut h, &bridge, "", FindStepSpec::First, 2), (0, None));
    assert!(h.state().session.find.results.is_empty());
}

#[test]
fn match_case_narrows_the_search() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(12));
    let case_sensitive = |text: &str, token| FindRequest { text: text.into(), match_case: true, step: FindStepSpec::First, select: true, token };
    bridge.send(HostCommand::Find(case_sensitive("clause 1", 1)));
    frames(&mut h, 3);
    assert!(bridge.take_events().contains(&HostEvent::FindResult { count: 0, index: None, token: 1 }));
    bridge.send(HostCommand::Find(case_sensitive("Clause 1", 2)));
    frames(&mut h, 3);
    assert!(bridge.take_events().contains(&HostEvent::FindResult { count: 4, index: Some(0), token: 2 }));
    assert_eq!(find(&mut h, &bridge, "clause 1", FindStepSpec::First, 3), (4, Some(0)), "case is ignored unless asked for");
}

#[test]
fn find_works_without_the_view_bar() {
    let (mut h, bridge) = embed_app_with_view_bar(false);
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.take_events();
    assert_eq!(find(&mut h, &bridge, "Clause 10", FindStepSpec::First, 1), (11, Some(0)));
    assert_eq!(find(&mut h, &bridge, "Clause 10", FindStepSpec::Next, 2), (11, Some(1)));
    bridge.send(HostCommand::Find(FindRequest { text: "Clause 100".into(), match_case: false, step: FindStepSpec::First, select: true, token: 3 }));
    frames(&mut h, 4);
    let viewport = bridge.take_events().into_iter().rev().find(|event| matches!(event, HostEvent::Viewport { .. })).expect("the find scrolled");
    assert!(matches!(viewport, HostEvent::Viewport { cause: Cause::Host, token: Some(3), .. }), "{viewport:?}");
}

#[test]
fn find_works_while_the_host_has_the_document_in_edit_mode() {
    use wordcraft_ui_egui::host::wire::{AuthorSpec, ModeRequest, PermissionsSpec};

    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(12));
    bridge.send(HostCommand::SetMode(ModeRequest::Edit {
        author: AuthorSpec { id: "u-sari".into(), name: "Sari Wulan".into() },
        revision_date: "2026-10-10T08:00:00Z".into(),
        permissions: PermissionsSpec {
            accept_reject_changes: false,
            organize_pages: true,
            fill_forms: true,
            redact: true,
            stamps: true,
            drawing: true,
            full_editing: false,
        },
    }));
    frames(&mut h, 3);
    bridge.take_events();
    let text_before = h.state().session.doc.plain_text(wordcraft_doc::StoryRef::Body);
    assert_eq!(find(&mut h, &bridge, "Clause 1", FindStepSpec::First, 1), (4, Some(0)));
    assert_eq!(find(&mut h, &bridge, "Clause 1", FindStepSpec::Next, 2), (4, Some(1)));
    bridge.send(HostCommand::Find(FindRequest { text: "Clause 1".into(), match_case: false, step: FindStepSpec::First, select: false, token: 3 }));
    frames(&mut h, 3);
    let events = bridge.take_events();
    assert!(events.iter().any(|event| matches!(event, HostEvent::FindResult { count: 4, .. })), "{events:?}");
    assert!(!events.iter().any(|event| matches!(event, HostEvent::EditRefused { .. })), "{events:?}");
    assert_eq!(h.state().session.doc.plain_text(wordcraft_doc::StoryRef::Body), text_before, "a find edits nothing");
}

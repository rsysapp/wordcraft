//! `jumpTo`'s optional token (embed API 0.4.1): the `viewport` a jump causes carries it with
//! `cause: 'host'`, `jumpNotFound` echoes it, and one rule decides which unanswered token a newer
//! token-bearing command (`scrollToBlock`, `find`, `jumpTo`) leaves behind. Without a token, a jump
//! is exactly what 0.4.0 did.

mod common;

use common::*;
use wordcraft_docx::facts::fingerprint;
use wordcraft_ui_egui::host::{
    BlockRef, Cause, FindRequest, FindStepSpec, HostCommand, HostEvent, JumpTarget, LocationSpec, ScrollRequest, ZoomRequest, ZoomValue,
};

fn viewports(events: &[HostEvent]) -> Vec<HostEvent> {
    events.iter().filter(|event| matches!(event, HostEvent::Viewport { .. })).cloned().collect()
}

fn tokens(events: &[HostEvent]) -> Vec<Option<u32>> {
    viewports(events)
        .into_iter()
        .map(|event| match event {
            HostEvent::Viewport { token, .. } => token,
            _ => unreachable!(),
        })
        .collect()
}

fn block(index: u32) -> BlockRef {
    BlockRef { part: "word/document.xml".into(), index }
}

#[test]
fn a_page_jump_with_a_token_is_answered_by_a_host_viewport_carrying_it() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 4 }, Some(21)));
    frames(&mut h, 6);
    let events = bridge.take_events();
    let answered: Vec<_> = viewports(&events).into_iter().filter(|event| matches!(event, HostEvent::Viewport { token: Some(_), .. })).collect();
    assert_eq!(answered.len(), 1, "answered once: {events:?}");
    assert!(matches!(answered[0], HostEvent::Viewport { cause: Cause::Host, token: Some(21), page: 4, .. }), "{:?}", answered[0]);
    frames(&mut h, 3);
    assert!(tokens(&bridge.take_events()).iter().all(Option::is_none), "the token is not repeated");
}

#[test]
fn a_text_jump_and_a_location_jump_carry_their_tokens_too() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::JumpTo(JumpTarget::Text { text: "Clause 100.".into(), page: None, occurrence: 0 }, Some(5)));
    frames(&mut h, 6);
    assert!(tokens(&bridge.take_events()).contains(&Some(5)));
    let text = paragraph_text(&h, 40);
    let at = LocationSpec { block: block(40), fingerprint: fingerprint(&text), from: None, to: None };
    bridge.send(HostCommand::JumpTo(JumpTarget::Location { at }, Some(6)));
    frames(&mut h, 6);
    let events = bridge.take_events();
    assert!(viewports(&events).iter().any(|event| matches!(event, HostEvent::Viewport { cause: Cause::Host, token: Some(6), .. })), "{events:?}");
}

#[test]
fn a_jump_to_where_the_view_already_is_is_still_answered() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 1 }, Some(9)));
    frames(&mut h, 6);
    assert!(tokens(&bridge.take_events()).contains(&Some(9)), "a host waiting on its token is answered even when nothing moved");
}

#[test]
fn a_jump_that_finds_nothing_echoes_its_token() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    let missing = JumpTarget::Page { page: 99 };
    bridge.send(HostCommand::JumpTo(missing.clone(), Some(31)));
    frames(&mut h, 3);
    let events = bridge.take_events();
    assert!(events.contains(&HostEvent::JumpNotFound { target: missing.clone(), token: Some(31) }), "{events:?}");
    assert!(viewports(&events).is_empty());
    bridge.send(HostCommand::JumpTo(missing.clone(), None));
    frames(&mut h, 3);
    assert!(bridge.take_events().contains(&HostEvent::JumpNotFound { target: missing, token: None }));
}

#[test]
fn a_jump_without_a_token_is_what_0_4_0_did() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 4 }, None));
    frames(&mut h, 6);
    let events = bridge.take_events();
    assert!(!viewports(&events).is_empty());
    assert!(tokens(&events).iter().all(Option::is_none), "{events:?}");
    assert!(viewports(&events).iter().all(|event| matches!(event, HostEvent::Viewport { cause: Cause::Host, .. })));
}

#[test]
fn a_newer_token_replaces_an_older_one_still_waiting() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    // scrollToBlock, then a jump, then a find, in one frame: only the last token is answered.
    bridge.send(HostCommand::ScrollToBlock(ScrollRequest { block: block(60), fraction: 0.0, token: 1 }));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 3 }, Some(2)));
    frames(&mut h, 6);
    let tokens_seen: Vec<_> = tokens(&bridge.take_events()).into_iter().flatten().collect();
    assert_eq!(tokens_seen, vec![2], "the jump's token replaced scrollToBlock's");
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 2 }, Some(3)));
    bridge.send(HostCommand::Find(FindRequest { text: "Clause 100".into(), match_case: false, step: FindStepSpec::First, select: true, token: 4 }));
    frames(&mut h, 6);
    let tokens_seen: Vec<_> = tokens(&bridge.take_events()).into_iter().flatten().collect();
    assert_eq!(tokens_seen, vec![4], "the find's token replaced the jump's");
    bridge.send(HostCommand::Find(FindRequest { text: "Clause 90".into(), match_case: false, step: FindStepSpec::First, select: true, token: 5 }));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 1 }, Some(6)));
    frames(&mut h, 6);
    let tokens_seen: Vec<_> = tokens(&bridge.take_events()).into_iter().flatten().collect();
    assert_eq!(tokens_seen, vec![6], "and the jump's replaced the find's");
}

#[test]
fn a_jump_replaced_by_one_that_finds_nothing_leaves_no_token_behind() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 3 }, Some(1)));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 99 }, Some(2)));
    frames(&mut h, 6);
    let events = bridge.take_events();
    assert!(events.contains(&HostEvent::JumpNotFound { target: JumpTarget::Page { page: 99 }, token: Some(2) }), "{events:?}");
    assert!(tokens(&events).iter().all(Option::is_none), "the replaced jump is not answered, and its token does not ride a later view: {events:?}");
}

#[test]
fn an_open_clears_the_tokens_waiting_and_answers_a_waiting_jump_with_its_own() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    // A scrollToBlock before an open in one frame: the new document's baseline viewport has no token.
    bridge.send(HostCommand::ScrollToBlock(ScrollRequest { block: block(60), fraction: 0.0, token: 7 }));
    bridge.send(HostCommand::Open { name: "b.docx".into(), bytes: docx(&clauses(30)), options: Default::default() });
    frames(&mut h, 6);
    let events = bridge.take_events();
    assert!(!viewports(&events).is_empty(), "the baseline");
    assert!(tokens(&events).iter().all(Option::is_none), "{events:?}");
    // A jump still waiting when the open arrives is answered `jumpNotFound`, with its token.
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 1 }, Some(8)));
    bridge.send(HostCommand::Open { name: "c.docx".into(), bytes: docx(&clauses(30)), options: Default::default() });
    frames(&mut h, 6);
    let events = bridge.take_events();
    assert!(events.contains(&HostEvent::JumpNotFound { target: JumpTarget::Page { page: 1 }, token: Some(8) }), "{events:?}");
    assert!(tokens(&events).iter().all(Option::is_none), "no token survives an open: {events:?}");
}

#[test]
fn a_zoom_keeps_its_token_on_zoom_changed_only() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::SetZoom(ZoomRequest { zoom: ZoomValue::Percent(200), token: 3 }));
    frames(&mut h, 6);
    let events = bridge.take_events();
    assert!(events.iter().any(|event| matches!(event, HostEvent::ZoomChanged { token: Some(3), .. })));
    assert!(tokens(&events).iter().all(Option::is_none), "the viewport a zoom causes is the host's, without a token: {events:?}");
}

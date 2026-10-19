//! The configuration Clauflow runs (embed API 0.3, tests added for 0.4.1): the clean display
//! (`display: 'final'`) in an editor mounted with `viewBar: false`, over a document with tracked
//! changes. Viewport, locate, scrollToBlock, jumps to a location, find and marks by location are each
//! asserted here, including what the clean display makes of wording it removed.

mod common;

use common::*;
use egui_kittest::Harness;
use wordcraft_doc::Pos;
use wordcraft_docx::facts::{accepted_text, fingerprint};
use wordcraft_ui_egui::WordApp;
use wordcraft_ui_egui::host::{
    BlockRef, Cause, FindRequest, FindStepSpec, HostBridge, HostCommand, HostEvent, JumpTarget, LocateRequest, LocationSpec, MarkSpec, MarkStyle,
    ScrollRequest,
};

const BODY: &str = "word/document.xml";

fn block(index: u32) -> BlockRef {
    BlockRef { part: BODY.into(), index }
}

/// Eighty clauses. Clause 10 has "30 " struck out of "Pay 30 45 days"; clause 11 is struck out entirely
/// (its text, not its paragraph mark).
fn fixture() -> Vec<u8> {
    let text: Vec<String> = (0..80)
        .map(|i| match i {
            10 => "Clause 10. Pay 30 45 days after the invoice.".to_string(),
            11 => "Clause 11. This wording is struck out entirely.".to_string(),
            _ => format!("Clause {i}. The Supplier shall deliver the goods listed in Schedule {i}."),
        })
        .collect();
    docx_with_changes(&text.join("\n"), &["30 ", "Clause 11. This wording is struck out entirely."], &[])
}

fn opened() -> (Harness<'static, WordApp>, HostBridge) {
    embed_app_final(&fixture(), false)
}

fn viewports(events: &[HostEvent]) -> Vec<&HostEvent> {
    events.iter().filter(|event| matches!(event, HostEvent::Viewport { .. })).collect()
}

fn locate(h: &mut Harness<'static, WordApp>, bridge: &HostBridge, blocks: &[u32]) -> Vec<Option<f32>> {
    bridge.send(HostCommand::Locate(LocateRequest { blocks: blocks.iter().map(|index| block(*index)).collect(), token: 77 }));
    frames(h, 2);
    bridge
        .take_events()
        .into_iter()
        .find_map(|event| match event {
            HostEvent::Located { positions, token: 77, .. } => Some(positions),
            _ => None,
        })
        .expect("located")
}

#[test]
fn the_baseline_viewport_names_the_first_block_with_a_fraction_and_an_offset() {
    let (mut h, bridge) = embed_app_with_view_bar(false);
    bridge.send(HostCommand::Open {
        name: "changes.docx".into(),
        bytes: fixture(),
        options: wordcraft_ui_egui::host::OpenOptions { display: wordcraft_ui_egui::host::Display::Final },
    });
    frames(&mut h, 4);
    let events = bridge.take_events();
    let Some(HostEvent::Viewport { top, fraction, offset, page, pages, cause, token, .. }) = viewports(&events).first().cloned().cloned() else {
        panic!("a viewport after opening: {events:?}")
    };
    assert_eq!(top, Some(block(0)));
    assert!((0.0..=1.0).contains(&fraction) && offset.abs() < 0.2, "{fraction} {offset}");
    assert_eq!((page, cause, token), (1, Cause::Host, None));
    assert!(pages > 1);
}

#[test]
fn locate_places_blocks_with_tracked_changes_and_a_paragraph_the_display_emptied_keeps_its_line() {
    let (mut h, bridge) = opened();
    let positions = locate(&mut h, &bridge, &[9, 10, 11, 12, 79, 500]);
    let [nine, ten, eleven, twelve, last, past] = positions[..] else { panic!("{positions:?}") };
    // A paragraph with a deletion inside it is placed like any other, and so is one whose wording the
    // display removed: its paragraph mark stays, so it is an empty line between its neighbours.
    assert!(nine.unwrap() < ten.unwrap() && ten.unwrap() < eleven.unwrap() && eleven.unwrap() < twelve.unwrap(), "{positions:?}");
    assert!(twelve.unwrap() < last.unwrap(), "{positions:?}");
    assert_eq!(past, None, "a block the document lacks");
}

#[test]
fn scroll_to_block_reaches_a_block_with_a_deletion_and_one_the_display_emptied() {
    let (mut h, bridge) = opened();
    for (token, index) in [(1, 10), (2, 11)] {
        bridge.send(HostCommand::ScrollToBlock(ScrollRequest { block: block(index), fraction: 0.0, token }));
        frames(&mut h, 6);
        let events = bridge.take_events();
        let tokened: Vec<_> =
            viewports(&events).into_iter().filter(|event| matches!(event, HostEvent::Viewport { token: Some(t), .. } if *t == token)).collect();
        assert_eq!(tokened.len(), 1, "{events:?}");
        let HostEvent::Viewport { top, cause, .. } = tokened[0] else { unreachable!() };
        assert_eq!((top.clone(), *cause), (Some(block(index)), Cause::Host));
        assert!(!events.iter().any(|event| matches!(event, HostEvent::BlockNotFound { .. })), "{events:?}");
    }
    bridge.send(HostCommand::ScrollToBlock(ScrollRequest { block: block(500), fraction: 0.0, token: 3 }));
    frames(&mut h, 4);
    let events = bridge.take_events();
    assert!(events.contains(&HostEvent::BlockNotFound { block: block(500), token: 3 }), "{events:?}");
    assert!(viewports(&events).is_empty());
}

#[test]
fn a_location_jump_with_a_token_lands_on_a_paragraph_with_a_deletion() {
    let (mut h, bridge) = opened();
    let accepted = paragraph_text(&h, 10);
    assert_eq!(accepted, "Clause 10. Pay 45 days after the invoice.");
    let at = LocationSpec { block: block(10), fingerprint: fingerprint(&accepted), from: Some(11), to: Some(18) };
    bridge.send(HostCommand::JumpTo(JumpTarget::Location { at }, Some(41)));
    frames(&mut h, 6);
    let events = bridge.take_events();
    assert!(!events.iter().any(|event| matches!(event, HostEvent::JumpNotFound { .. })), "{events:?}");
    assert!(viewports(&events).iter().any(|event| matches!(event, HostEvent::Viewport { cause: Cause::Host, token: Some(41), .. })), "{events:?}");
}

#[test]
fn a_location_jump_to_the_emptied_paragraph_is_found_and_a_stale_fingerprint_is_not() {
    let (mut h, bridge) = opened();
    // The paragraph's accepted text is empty: its fingerprint is that of "", and it is still a paragraph.
    let accepted = paragraph_text(&h, 11);
    assert_eq!(accepted, "");
    let at = LocationSpec { block: block(11), fingerprint: fingerprint(&accepted), from: None, to: None };
    bridge.send(HostCommand::JumpTo(JumpTarget::Location { at }, Some(42)));
    frames(&mut h, 6);
    let events = bridge.take_events();
    assert!(!events.iter().any(|event| matches!(event, HostEvent::JumpNotFound { .. })), "{events:?}");
    assert!(viewports(&events).iter().any(|event| matches!(event, HostEvent::Viewport { cause: Cause::Host, token: Some(42), .. })), "{events:?}");
    // The wording the host compared against (the file before the change was accepted) is not what is open.
    let stale = LocationSpec { block: block(11), fingerprint: fingerprint("Clause 11. This wording is struck out entirely."), from: None, to: None };
    bridge.send(HostCommand::JumpTo(JumpTarget::Location { at: stale.clone() }, Some(43)));
    frames(&mut h, 4);
    let events = bridge.take_events();
    assert!(events.contains(&HostEvent::JumpNotFound { target: JumpTarget::Location { at: stale }, token: Some(43) }), "{events:?}");
}

#[test]
fn a_find_with_a_token_reaches_a_phrase_across_a_deletion() {
    let (mut h, bridge) = opened();
    bridge.send(HostCommand::Find(FindRequest { text: "Pay 45 days".into(), match_case: false, step: FindStepSpec::First, select: true, token: 51 }));
    frames(&mut h, 6);
    let events = bridge.take_events();
    assert!(events.contains(&HostEvent::FindResult { count: 1, index: Some(0), token: 51 }), "{events:?}");
    assert!(viewports(&events).iter().any(|event| matches!(event, HostEvent::Viewport { cause: Cause::Host, token: Some(51), .. })), "{events:?}");
    assert_eq!(h.state().session.sel.ordered().0, Pos::body(10, "Clause 10. ".len()), "the match starts at 'Pay'");
}

#[test]
fn a_mark_by_location_after_a_deletion_is_painted_on_its_words() {
    let (mut h, bridge) = opened();
    let accepted = accepted_text(&body_paragraph(&h, 10));
    // "45 days" in "Clause 10. Pay 45 days after the invoice."
    let from = accepted.find("45 days").expect("in the text") as u32;
    let mark = MarkSpec {
        id: "m".into(),
        text: None,
        page: None,
        color: "#16a34a4d".into(),
        style: MarkStyle::Fill,
        at: Some(LocationSpec { block: block(10), fingerprint: fingerprint(&accepted), from: Some(from), to: Some(from + 7) }),
    };
    bridge.send(HostCommand::SetMarks(vec![mark]));
    settle_marks(&mut h);
    let rects = painted_marks(&h);
    assert_eq!(rects.len(), 1);
    assert_eq!(selected_words_under(&mut h, 10, &rects[0]), "45 days");
    assert_close_to_words(&mut h, &rects[0], 10, from..from + 7);
}

//! `viewport` and `scrollToBlock` (embed API 0.3): the host hears where the view is in document
//! terms (the top block, how far into it, the page) and can put another block at the top, with the
//! token of its request coming back so two editors scrolled level never echo each other.

mod common;

use common::*;
use egui_kittest::Harness;
use wordcraft_ui_egui::WordApp;
use wordcraft_ui_egui::host::{BlockRef, Cause, HostCommand, HostEvent, ScrollRequest};

const BODY: &str = "word/document.xml";

fn viewports(events: Vec<HostEvent>) -> Vec<HostEvent> {
    events.into_iter().filter(|event| matches!(event, HostEvent::Viewport { .. })).collect()
}

fn block(index: u32) -> BlockRef {
    BlockRef { part: BODY.into(), index }
}

fn scroll_to(h: &mut Harness<'_, WordApp>, bridge: &wordcraft_ui_egui::host::HostBridge, index: u32, fraction: f32, token: u32) {
    bridge.send(HostCommand::ScrollToBlock(ScrollRequest { block: block(index), fraction, token }));
    frames(h, 4);
}

#[test]
fn the_first_viewport_names_the_first_block_on_page_one() {
    let (mut h, bridge) = embed_app();
    let events = open_doc(&mut h, &bridge, &clauses(120));
    let first = viewports(events).into_iter().next().expect("a viewport after opening");
    assert!(
        matches!(first, HostEvent::Viewport { top: Some(BlockRef { ref part, index: 0 }), page: 1, cause: Cause::Host, token: None, .. } if part == BODY),
        "{first:?}"
    );
}

#[test]
fn the_viewport_follows_the_opened_event_and_carries_the_page_count() {
    let (mut h, bridge) = embed_app();
    let events = open_doc(&mut h, &bridge, &clauses(120));
    let count = pages(&mut h) as u32;
    let opened = events.iter().position(|event| matches!(event, HostEvent::Opened { .. })).expect("opened");
    let viewport = events.iter().position(|event| matches!(event, HostEvent::Viewport { .. })).expect("a viewport");
    assert!(opened < viewport, "{events:?}");
    let Some(HostEvent::Viewport { pages, visible_height, fraction, .. }) = viewports(events).into_iter().next() else { unreachable!() };
    assert_eq!(pages, count);
    assert!(visible_height > 100.0, "the view's height in document points: {visible_height}");
    assert!((0.0..=1.0).contains(&fraction), "{fraction}");
}

#[test]
fn a_host_scroll_is_reported_with_its_token() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.take_events();
    scroll_to(&mut h, &bridge, 80, 0.5, 3);
    let last = viewports(bridge.take_events()).pop().expect("a viewport after the scroll");
    let HostEvent::Viewport { top, fraction, cause, token, page, .. } = last else { unreachable!() };
    assert_eq!(top, Some(block(80)));
    assert!((fraction - 0.5).abs() < 0.1, "{fraction}");
    assert_eq!((cause, token), (Cause::Host, Some(3)));
    assert!(page > 1);
}

#[test]
fn a_scroll_by_the_person_is_reported_as_theirs() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.take_events();
    wheel(&mut h, -600.0);
    frames(&mut h, 3);
    let last = viewports(bridge.take_events()).pop().expect("a viewport after the wheel");
    assert!(matches!(last, HostEvent::Viewport { cause: Cause::User, token: None, .. }), "{last:?}");
}

#[test]
fn a_person_scrolling_after_a_host_scroll_is_not_taken_for_the_host() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    scroll_to(&mut h, &bridge, 40, 0.0, 9);
    bridge.take_events();
    wheel(&mut h, -600.0);
    frames(&mut h, 3);
    let events = viewports(bridge.take_events());
    assert!(!events.is_empty(), "the wheel moved the view");
    assert!(events.iter().all(|event| matches!(event, HostEvent::Viewport { cause: Cause::User, token: None, .. })), "{events:?}");
}

#[test]
fn an_unchanged_view_reports_nothing_more() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.take_events();
    frames(&mut h, 5);
    assert!(viewports(bridge.take_events()).is_empty());
}

#[test]
fn a_host_scroll_that_changes_nothing_is_still_answered_once() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    scroll_to(&mut h, &bridge, 80, 0.25, 1);
    bridge.take_events();
    scroll_to(&mut h, &bridge, 80, 0.25, 2);
    let events = viewports(bridge.take_events());
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(events[0], HostEvent::Viewport { cause: Cause::Host, token: Some(2), .. }), "{:?}", events[0]);
    frames(&mut h, 3);
    assert!(viewports(bridge.take_events()).is_empty(), "the token is answered once");
}

#[test]
fn an_unknown_block_scrolls_nowhere_and_says_so() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    bridge.take_events();
    bridge.send(HostCommand::ScrollToBlock(ScrollRequest { block: block(999), fraction: 0.0, token: 4 }));
    frames(&mut h, 3);
    let events = bridge.take_events();
    assert!(events.contains(&HostEvent::BlockNotFound { block: block(999), token: 4 }), "{events:?}");
    assert!(viewports(events).is_empty());
}

#[test]
fn an_unknown_part_or_no_document_says_so() {
    let (mut h, bridge) = embed_app();
    bridge.send(HostCommand::ScrollToBlock(ScrollRequest { block: block(0), fraction: 0.0, token: 5 }));
    frames(&mut h, 3);
    assert!(bridge.take_events().contains(&HostEvent::BlockNotFound { block: block(0), token: 5 }), "nothing is open");
    open_doc(&mut h, &bridge, &clauses(10));
    let elsewhere = BlockRef { part: "word/header1.xml".into(), index: 0 };
    bridge.send(HostCommand::ScrollToBlock(ScrollRequest { block: elsewhere.clone(), fraction: 0.0, token: 6 }));
    frames(&mut h, 3);
    assert!(bridge.take_events().contains(&HostEvent::BlockNotFound { block: elsewhere, token: 6 }));
}

#[test]
fn a_table_is_named_by_its_first_paragraph_and_scrolled_to_whole() {
    use std::sync::Arc;
    use wordcraft_doc::props::CharProps;
    use wordcraft_doc::{Block, Document, Paragraph, Table, para_block};
    let line = |text: &str| Paragraph::with_text(text, CharProps::default());
    let mut table = Table::new(2, 1, 300.0);
    table.rows[0].cells[0].blocks = vec![para_block(line("Fee schedule"))];
    table.rows[1].cells[0].blocks = vec![para_block(line("Late fee"))];
    let mut body: Vec<_> =
        (0..60).map(|i| para_block(line(&format!("Clause {i}. The Supplier shall deliver the goods listed in Schedule {i}.")))).collect();
    body.push(Arc::new(Block::Table(table)));
    body.extend((60..120).map(|i| para_block(line(&format!("Clause {i}. The Supplier shall deliver the goods listed in Schedule {i}.")))));
    let mut doc = Document::new();
    doc.body = body;
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &doc);
    bridge.take_events();
    // The table's first paragraph is body paragraph 60; its second cell is 61 and belongs to the same block.
    for named in [60, 61] {
        scroll_to(&mut h, &bridge, named, 0.0, 10 + named);
        let last = viewports(bridge.take_events()).pop().expect("a viewport");
        let HostEvent::Viewport { top, fraction, page, .. } = last else { unreachable!() };
        assert_eq!(top, Some(block(60)), "paragraph {named} sits in the table");
        assert!(page > 1, "the table is in the middle of the document: page {page}");
        assert!(fraction < 0.05, "the table's top is at the top of the view: {fraction}");
        scroll_to(&mut h, &bridge, 0, 0.0, 30);
        bridge.take_events();
    }
}

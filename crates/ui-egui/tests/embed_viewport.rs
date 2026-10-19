//! `viewport` and `scrollToBlock` (embed API 0.3): the host hears where the view is in document
//! terms (the top block, how far into it, the page) and can put another block at the top, with the
//! token of its request coming back so two editors scrolled level never echo each other.

mod common;

use common::*;
use egui_kittest::Harness;
use wordcraft_ui_egui::WordApp;
use wordcraft_ui_egui::host::{BlockRef, Cause, HostCommand, HostEvent, JumpTarget, LocateRequest, ScrollRequest, ZoomRequest, ZoomValue};

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

/// Every `viewport` since the last call must be the host's own doing, without a token.
fn assert_all_host_without_token(events: Vec<HostEvent>, what: &str) {
    let events = viewports(events);
    assert!(!events.is_empty(), "{what} moved the view, so a viewport is due");
    assert!(events.iter().all(|event| matches!(event, HostEvent::Viewport { cause: Cause::Host, token: None, .. })), "{what}: {events:?}");
}

#[test]
fn a_host_jump_is_not_the_persons_scroll() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.take_events();
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 5 }, None));
    frames(&mut h, 6);
    let events = bridge.take_events();
    let moved = viewports(events.clone());
    assert!(moved.iter().any(|event| matches!(event, HostEvent::Viewport { page: 5, .. })), "the jump reached page 5: {events:?}");
    assert_all_host_without_token(events, "jumpTo");
}

#[test]
fn a_host_zoom_and_its_reflow_are_not_the_persons_scroll() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    scroll_to(&mut h, &bridge, 80, 0.0, 1);
    bridge.take_events();
    bridge.send(HostCommand::SetZoom(ZoomRequest { zoom: ZoomValue::Percent(300), token: 2 }));
    frames(&mut h, 6);
    assert_all_host_without_token(bridge.take_events(), "setZoom");
}

#[test]
fn a_window_resize_is_not_the_persons_scroll() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    scroll_to(&mut h, &bridge, 80, 0.5, 1);
    bridge.take_events();
    h.set_size(egui::vec2(700.0, 500.0));
    frames(&mut h, 6);
    assert_all_host_without_token(bridge.take_events(), "a resize");
}

#[test]
fn page_down_in_the_document_is_the_persons_scroll() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    // The canvas has the keyboard after a click in the text, as for a person reading.
    let at = screen_of(&mut h, &wordcraft_doc::Pos::body(0, 3));
    click(&mut h, at, egui::PointerButton::Primary);
    frames(&mut h, 2);
    assert!(h.state().canvas.focused);
    bridge.take_events();
    h.key_press(egui::Key::PageDown);
    frames(&mut h, 4);
    let events = viewports(bridge.take_events());
    assert!(!events.is_empty(), "PageDown moved the view");
    assert!(events.iter().all(|event| matches!(event, HostEvent::Viewport { cause: Cause::User, token: None, .. })), "{events:?}");
}

#[test]
fn a_block_cut_by_a_page_break_is_one_block_and_a_fraction_of_it_lands_on_its_second_page() {
    use wordcraft_doc::props::CharProps;
    use wordcraft_doc::{Document, Paragraph, Pos, para_block};
    let line = |text: &str| Paragraph::with_text(text, CharProps::default());
    let long = "The Supplier shall deliver the goods listed in the Schedule within thirty days of the order. ".repeat(40);
    let mut body: Vec<_> = (0..5).map(|i| para_block(line(&format!("Clause {i}. Short.")))).collect();
    body.push(para_block(line(&long)));
    body.extend((6..30).map(|i| para_block(line(&format!("Clause {i}. Short.")))));
    let mut doc = Document::new();
    doc.body = body;
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &doc);
    let (first_page, last_page) = {
        let app = h.state_mut();
        let layout = app.session.layout();
        let first = layout.caret_on(&Pos::body(5, 0), 0).expect("laid out").page;
        let last = layout.caret_on(&Pos::body(5, long.trim_end().len()), 0).expect("laid out").page;
        (first, last)
    };
    assert!(last_page > first_page, "the fixture's paragraph must span a page break: pages {first_page} to {last_page}");
    bridge.take_events();
    scroll_to(&mut h, &bridge, 5, 0.9, 7);
    let last = viewports(bridge.take_events()).pop().expect("a viewport after the scroll");
    let HostEvent::Viewport { top, fraction, page, cause, token, .. } = last else { unreachable!() };
    assert_eq!((top, cause, token), (Some(block(5)), Cause::Host, Some(7)));
    assert!((fraction - 0.9).abs() < 0.1, "the fraction survives the page break: {fraction}");
    assert!(
        page as usize > first_page + 1,
        "the view is on the block's later page, not its first: page {page}, block starts on page {}",
        first_page + 1
    );
}

fn located(events: Vec<HostEvent>, token: u32) -> Option<(Vec<Option<f32>>, f32)> {
    events.into_iter().find_map(|event| match event {
        HostEvent::Located { positions, height, token: answered } if answered == token => Some((positions, height)),
        _ => None,
    })
}

#[test]
fn located_positions_rise_through_the_document_and_unknown_blocks_are_null() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.take_events();
    let blocks = [0, 60, 119, 999].map(block).to_vec();
    bridge.send(HostCommand::Locate(LocateRequest { blocks, token: 5 }));
    frames(&mut h, 2);
    let (positions, height) = located(bridge.take_events(), 5).expect("located");
    assert_eq!(positions.len(), 4);
    assert!(positions[0].unwrap() < positions[1].unwrap() && positions[1].unwrap() < positions[2].unwrap(), "{positions:?}");
    assert!(positions[2].unwrap() < height, "{positions:?} in {height}");
    assert_eq!(positions[3], None);
}

#[test]
fn located_positions_use_the_axis_the_viewport_measures() {
    // The strip draws the view as visibleHeight of `height`; so a block scrolled to the top of the
    // view must be at its located position along the same stacked pages.
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    let count = pages(&mut h) as f32;
    bridge.send(HostCommand::Locate(LocateRequest { blocks: vec![block(0), block(119)], token: 1 }));
    frames(&mut h, 2);
    let (positions, height) = located(bridge.take_events(), 1).expect("located");
    // 612 x 792 points a page in the test document.
    assert!((height - count * 792.0).abs() < 1.0, "{height} for {count} pages");
    assert!(positions[0].unwrap() < 100.0, "the first block is at the top of page one: {positions:?}");
    assert!(positions[1].unwrap() > (count - 1.0) * 792.0, "the last block is on the last page: {positions:?}");
}

#[test]
fn every_paragraph_of_a_table_is_located_at_the_tables_top() {
    use std::sync::Arc;
    use wordcraft_doc::props::CharProps;
    use wordcraft_doc::{Block, Document, Paragraph, Table, para_block};
    let line = |text: &str| Paragraph::with_text(text, CharProps::default());
    let mut table = Table::new(2, 1, 300.0);
    table.rows[0].cells[0].blocks = vec![para_block(line("Fee schedule"))];
    table.rows[1].cells[0].blocks = vec![para_block(line("Late fee"))];
    let mut body: Vec<_> = (0..60).map(|i| para_block(line(&format!("Clause {i}. The Supplier shall deliver the goods in Schedule {i}.")))).collect();
    body.push(Arc::new(Block::Table(table)));
    body.extend((60..120).map(|i| para_block(line(&format!("Clause {i}. The Supplier shall deliver the goods in Schedule {i}.")))));
    let mut doc = Document::new();
    doc.body = body;
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &doc);
    bridge.take_events();
    // Body paragraph 60 opens the table, 61 is its second row: one top-level block, one place.
    bridge.send(HostCommand::Locate(LocateRequest { blocks: vec![block(59), block(60), block(61), block(62)], token: 2 }));
    frames(&mut h, 2);
    let (positions, _) = located(bridge.take_events(), 2).expect("located");
    let [before, first, second, after] = positions[..] else { panic!("{positions:?}") };
    assert_eq!(first, second, "{positions:?}");
    assert!(before.unwrap() < first.unwrap() && first.unwrap() < after.unwrap(), "{positions:?}");
}

#[test]
fn a_locate_before_a_document_is_open_is_answered_once_it_is_laid_out() {
    let (mut h, bridge) = embed_app();
    bridge.send(HostCommand::Locate(LocateRequest { blocks: vec![block(3)], token: 8 }));
    frames(&mut h, 3);
    assert!(located(bridge.take_events(), 8).is_none(), "nothing is open yet: no answer, not a null one");
    let events = open_doc(&mut h, &bridge, &clauses(30));
    let opened = events.iter().position(|event| matches!(event, HostEvent::Opened { .. })).expect("opened");
    let answered = events.iter().position(|event| matches!(event, HostEvent::Located { token: 8, .. })).expect("located after the open");
    assert!(opened < answered, "{events:?}");
    let (positions, _) = located(events, 8).unwrap();
    assert!(positions[0].is_some());
}

#[test]
fn a_locate_is_answered_once() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(30));
    bridge.take_events();
    bridge.send(HostCommand::Locate(LocateRequest { blocks: vec![block(0)], token: 3 }));
    frames(&mut h, 5);
    let answers = bridge.take_events().into_iter().filter(|event| matches!(event, HostEvent::Located { .. })).count();
    assert_eq!(answers, 1);
}

#[test]
fn locating_moves_nothing_and_says_nothing_of_the_view() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.take_events();
    bridge.send(HostCommand::Locate(LocateRequest { blocks: vec![block(100)], token: 4 }));
    frames(&mut h, 4);
    let events = bridge.take_events();
    assert!(viewports(events).is_empty(), "locate does not scroll");
}

#[test]
fn an_empty_locate_is_answered_with_the_height() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.take_events();
    bridge.send(HostCommand::Locate(LocateRequest { blocks: Vec::new(), token: 6 }));
    frames(&mut h, 2);
    let (positions, height) = located(bridge.take_events(), 6).expect("located");
    assert!(positions.is_empty() && height > 0.0);
}

#[test]
fn locate_refuses_more_than_ten_thousand_blocks() {
    let blocks: Vec<_> = (0..10_001).map(|index| serde_json::json!({"part": BODY, "index": index})).collect();
    assert!(wordcraft_ui_egui::host::wire::parse_locate_request(&serde_json::json!({"blocks": blocks, "token": 1}).to_string()).is_err());
    let blocks: Vec<_> = (0..10_000).map(|index| serde_json::json!({"part": BODY, "index": index})).collect();
    let request = wordcraft_ui_egui::host::wire::parse_locate_request(&serde_json::json!({"blocks": blocks, "token": 1}).to_string()).unwrap();
    assert_eq!(request.blocks.len(), 10_000);
}

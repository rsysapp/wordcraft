//! A host `setZoom` keeps the block at the top of the view, and `viewport` carries the view's offset
//! on the stacked document-point axis (embed API 0.4.1).

mod common;

use common::*;
use egui_kittest::Harness;
use wordcraft_ui_egui::WordApp;
use wordcraft_ui_egui::host::{BlockRef, Cause, HostBridge, HostCommand, HostEvent, LocateRequest, ScrollRequest, ZoomRequest, ZoomValue};

const BODY: &str = "word/document.xml";

fn block(index: u32) -> BlockRef {
    BlockRef { part: BODY.into(), index }
}

#[derive(Debug, Clone, PartialEq)]
struct Seen {
    top: Option<BlockRef>,
    fraction: f32,
    offset: f32,
    visible_height: f32,
    cause: Cause,
    token: Option<u32>,
}

fn last_view(events: &[HostEvent]) -> Option<Seen> {
    events.iter().rev().find_map(|event| match event {
        HostEvent::Viewport { top, fraction, offset, visible_height, cause, token, .. } => {
            Some(Seen { top: top.clone(), fraction: *fraction, offset: *offset, visible_height: *visible_height, cause: *cause, token: *token })
        }
        _ => None,
    })
}

fn zoom(h: &mut Harness<'static, WordApp>, bridge: &HostBridge, percent: u32, token: u32) -> Vec<HostEvent> {
    bridge.send(HostCommand::SetZoom(ZoomRequest { zoom: ZoomValue::Percent(percent), token }));
    frames(h, 8);
    bridge.take_events()
}

fn located(h: &mut Harness<'static, WordApp>, bridge: &HostBridge, blocks: &[u32]) -> (Vec<f32>, f32) {
    bridge.send(HostCommand::Locate(LocateRequest { blocks: blocks.iter().map(|index| block(*index)).collect(), token: 900 }));
    frames(h, 2);
    bridge
        .take_events()
        .into_iter()
        .find_map(|event| match event {
            HostEvent::Located { positions, height, token: 900 } => Some((positions.into_iter().map(|p| p.expect("located")).collect(), height)),
            _ => None,
        })
        .expect("located")
}

/// The view the person scrolled to (a wheel, so no jump target pins it), as the host last heard it.
fn person_scrolled(h: &mut Harness<'static, WordApp>, bridge: &HostBridge) -> Seen {
    bridge.take_events();
    wheel(h, -2500.0);
    frames(h, 20);
    let seen = last_view(&bridge.take_events()).expect("the wheel moved the view");
    assert_eq!(seen.cause, Cause::User, "{seen:?}");
    seen
}

fn assert_same_place(before: &Seen, after: &Seen, what: &str) {
    assert_eq!(after.top, before.top, "{what}: the top block moved: {before:?} -> {after:?}");
    assert!((after.fraction - before.fraction).abs() < 0.05, "{what}: the fraction moved: {before:?} -> {after:?}");
    assert!((after.offset - before.offset).abs() < 3.0, "{what}: the offset moved: {before:?} -> {after:?}");
}

#[test]
fn a_host_zoom_keeps_the_top_block_of_a_view_the_person_scrolled() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    let mut before = person_scrolled(&mut h, &bridge);
    for (token, percent) in [(1, 300), (2, 80), (3, 150), (4, 100)] {
        let events = zoom(&mut h, &bridge, percent, token);
        let after = last_view(&events).unwrap_or_else(|| panic!("a viewport after zooming to {percent}%: {events:?}"));
        assert_same_place(&before, &after, &format!("zoom {percent}%"));
        assert_eq!((after.cause, after.token), (Cause::Host, None));
        before = after;
    }
}

#[test]
fn a_host_zoom_keeps_the_block_a_scroll_to_block_put_at_the_top() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::ScrollToBlock(ScrollRequest { block: block(70), fraction: 0.5, token: 1 }));
    frames(&mut h, 6);
    let before = last_view(&bridge.take_events()).expect("the scroll landed");
    assert_eq!(before.top, Some(block(70)));
    for (token, percent) in [(2, 250), (3, 80)] {
        let after = last_view(&zoom(&mut h, &bridge, percent, token)).expect("a viewport after the zoom");
        assert_same_place(&before, &after, &format!("zoom {percent}%"));
    }
}

#[test]
fn zoom_changed_comes_before_the_viewport_it_causes() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    person_scrolled(&mut h, &bridge);
    let events = zoom(&mut h, &bridge, 200, 5);
    let zoomed = events.iter().position(|event| matches!(event, HostEvent::ZoomChanged { token: Some(5), .. })).expect("zoomChanged");
    let moved = events.iter().position(|event| matches!(event, HostEvent::Viewport { .. })).expect("a viewport");
    assert!(zoomed < moved, "{events:?}");
    assert!(matches!(events[moved], HostEvent::Viewport { cause: Cause::Host, token: None, .. }), "{:?}", events[moved]);
}

#[test]
fn the_viewport_offset_is_the_views_top_on_the_axis_locate_uses() {
    let (mut h, bridge) = embed_app();
    let events = open_doc(&mut h, &bridge, &clauses(120));
    let first = last_view(&events).expect("the baseline");
    assert!(first.offset.abs() < 0.2, "the document's start: {}", first.offset);
    let (tops, height) = located(&mut h, &bridge, &[80, 81]);
    bridge.send(HostCommand::ScrollToBlock(ScrollRequest { block: block(80), fraction: 0.0, token: 1 }));
    frames(&mut h, 6);
    let at_top = last_view(&bridge.take_events()).expect("the scroll landed");
    assert!((at_top.offset - tops[0]).abs() < 1.0, "block 80 is at the top: offset {} for a block at {}", at_top.offset, tops[0]);
    bridge.send(HostCommand::ScrollToBlock(ScrollRequest { block: block(80), fraction: 0.5, token: 2 }));
    frames(&mut h, 6);
    let halfway = last_view(&bridge.take_events()).expect("the scroll landed");
    assert!(
        halfway.offset > tops[0] + 0.5 && halfway.offset < tops[1],
        "half way down block 80: {} between {} and {}",
        halfway.offset,
        tops[0],
        tops[1]
    );
    assert!(halfway.offset + halfway.visible_height <= height + 1.0, "the view lies inside the document");
}

#[test]
fn the_viewport_offset_does_not_move_with_the_zoom() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    let before = person_scrolled(&mut h, &bridge);
    assert!(before.offset > 500.0, "scrolled down: {}", before.offset);
    let after = last_view(&zoom(&mut h, &bridge, 80, 1)).expect("a viewport");
    assert!((after.offset - before.offset).abs() < 3.0, "{before:?} -> {after:?}");
    assert!(after.visible_height > before.visible_height, "the view shows more of the document at 80%");
}

#[test]
fn a_person_scrolling_down_raises_the_offset() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.take_events();
    let mut previous = 0.0_f32;
    for _ in 0..3 {
        wheel(&mut h, -700.0);
        frames(&mut h, 12);
        let seen = last_view(&bridge.take_events()).expect("the wheel moved the view");
        assert!(seen.offset > previous + 100.0, "{} after {previous}", seen.offset);
        previous = seen.offset;
    }
}

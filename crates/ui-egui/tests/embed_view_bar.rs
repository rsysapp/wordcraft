//! `MountOptions.viewBar` (embed API 0.3): no view bar, and Ctrl/⌘+F handed to the host.

mod common;

use common::*;
use egui_kittest::kittest::Queryable;
use wordcraft_ui_egui::host::HostEvent;

#[test]
fn without_a_view_bar_nothing_of_it_is_drawn() {
    let (mut h, bridge) = embed_app_with_view_bar(false);
    open_doc(&mut h, &bridge, &clauses(3));
    assert!(h.query_by_label("Find in document").is_none());
    assert!(h.query_by_label_contains("Page 1 of").is_none());
    // The canvas keeps its own 8-point gap above the first page, so "at the top" is measured
    // against a mount with the bar: the canvas moves up by the bar's whole height (34 points).
    let (mut with_bar, with_bar_bridge) = embed_app_with_view_bar(true);
    open_doc(&mut with_bar, &with_bar_bridge, &clauses(3));
    let top = |h: &egui_kittest::Harness<'_, wordcraft_ui_egui::WordApp>| h.state().canvas.canvas_rect.expect("the canvas was drawn").min.y;
    assert_eq!(top(&h), 8.0, "the document starts at the window's top, below the canvas's own gap");
    assert_eq!(top(&with_bar) - top(&h), 34.0, "the bar's height is given back to the document");
}

#[test]
fn the_bar_is_drawn_when_asked_for_and_by_default() {
    let (mut with_bar, bridge) = embed_app_with_view_bar(true);
    open_doc(&mut with_bar, &bridge, &clauses(3));
    assert!(with_bar.query_by_label("Find in document").is_some());
    assert!(with_bar.state().canvas.canvas_rect.expect("the canvas was drawn").min.y > 0.0, "the bar sits above the document");
    let (mut default_app, bridge) = embed_app();
    open_doc(&mut default_app, &bridge, &clauses(3));
    assert_eq!(default_app.state().canvas.canvas_rect, with_bar.state().canvas.canvas_rect);
}

#[test]
fn without_a_view_bar_ctrl_f_asks_the_host() {
    let (mut h, bridge) = embed_app_with_view_bar(false);
    open_doc(&mut h, &bridge, &clauses(3));
    h.state_mut().run("edit.find", serde_json::json!({})).unwrap();
    frames(&mut h, 2);
    assert_eq!(bridge.take_events(), vec![HostEvent::FindRequested]);
    assert!(!h.state().session.view.nav_pane, "no pane opens");
}

#[test]
fn with_a_view_bar_ctrl_f_stays_in_the_editor() {
    let (mut h, bridge) = embed_app_with_view_bar(true);
    open_doc(&mut h, &bridge, &clauses(3));
    h.state_mut().run("edit.find", serde_json::json!({})).unwrap();
    frames(&mut h, 2);
    assert!(bridge.take_events().is_empty(), "the bar's field takes it");
}

#[test]
fn without_a_view_bar_find_with_text_still_works() {
    let (mut h, bridge) = embed_app_with_view_bar(false);
    open_doc(&mut h, &bridge, &clauses(3));
    let found = h.state_mut().run("edit.find", serde_json::json!({"text": "Clause 2"})).unwrap();
    assert_eq!(found["count"], 1);
    frames(&mut h, 2);
    assert!(!bridge.take_events().contains(&HostEvent::FindRequested));
}

#[test]
fn the_view_bar_is_on_by_default() {
    assert!(
        wordcraft_ui_egui::host::wire::parse_mount_options(
            r##"{"mode":"view","theme":{"dark":false,"accent":"#1d4ed8","accentContent":"#ffffff"}}"##
        )
        .unwrap()
        .view_bar
    );
}

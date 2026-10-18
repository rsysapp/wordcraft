//! `setZoom` and `zoomChanged` (embed API 0.3): the host sets the zoom and hears of every change,
//! with the token of the request that caused it, or none when the person did.

mod common;

use common::*;
use egui_kittest::kittest::Queryable;
use wordcraft_ui_egui::host::{Cause, FitWidth, HostCommand, HostEvent, ZoomRequest, ZoomValue};

/// The zoom the pages are drawn at, as the view bar shows it.
fn displayed_percent(h: &egui_kittest::Harness<'_, wordcraft_ui_egui::WordApp>) -> u32 {
    (h.state().canvas.scale / wordcraft_ui_egui::canvas::PX_PER_PT * 100.0).round() as u32
}

/// A gesture's events: every one is the person's, and the last is the zoom the editor settled on.
fn assert_persons_gesture_settles(h: &egui_kittest::Harness<'_, wordcraft_ui_egui::WordApp>, events: &[HostEvent]) {
    assert!(!events.is_empty(), "a gesture reaches the host");
    assert!(
        events.iter().all(|event| matches!(event, HostEvent::ZoomChanged { cause: Cause::User, token: None, page_width: false, .. })),
        "no host event for the person's gesture: {events:?}"
    );
    let Some(HostEvent::ZoomChanged { percent, .. }) = events.last() else { unreachable!("checked non-empty") };
    assert_eq!(*percent, displayed_percent(h), "the last event is the settled zoom: {events:?}");
}

fn zoom_events(events: Vec<HostEvent>) -> Vec<HostEvent> {
    events.into_iter().filter(|event| matches!(event, HostEvent::ZoomChanged { .. })).collect()
}

#[test]
fn a_host_zoom_is_applied_and_reported_with_its_token() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    bridge.send(HostCommand::SetZoom(ZoomRequest { zoom: ZoomValue::Percent(150), token: 7 }));
    frames(&mut h, 3);
    assert_eq!(
        zoom_events(bridge.take_events()),
        vec![HostEvent::ZoomChanged { percent: 150, page_width: false, cause: Cause::Host, token: Some(7) }]
    );
}

#[test]
fn fit_width_is_kept_while_the_window_resizes() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    bridge.send(HostCommand::SetZoom(ZoomRequest { zoom: ZoomValue::Fit(FitWidth::PageWidth), token: 8 }));
    frames(&mut h, 3);
    let first = zoom_events(bridge.take_events());
    assert!(matches!(first.as_slice(), [HostEvent::ZoomChanged { page_width: true, cause: Cause::Host, token: Some(8), .. }]), "{first:?}");
    h.set_size(egui::vec2(700.0, 800.0));
    frames(&mut h, 3);
    // A fitted zoom that follows the window is not the person's doing and is not repeated.
    let after_resize = zoom_events(bridge.take_events());
    assert!(!after_resize.iter().any(|event| matches!(event, HostEvent::ZoomChanged { cause: Cause::User, .. })), "{after_resize:?}");
    assert_eq!(h.state().session.view.fit, "pageWidth", "the fit follows the window");
}

#[test]
fn a_zoom_inside_the_editor_is_reported_as_the_persons() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    h.state_mut().run("view.zoomIn", serde_json::json!({})).unwrap();
    frames(&mut h, 3);
    let events = zoom_events(bridge.take_events());
    assert!(matches!(events.as_slice(), [HostEvent::ZoomChanged { cause: Cause::User, token: None, page_width: false, .. }]), "{events:?}");
}

#[test]
fn zoom_requests_are_parsed_and_bounded() {
    use wordcraft_ui_egui::host::wire::parse_zoom_request;
    assert!(parse_zoom_request(r#"{"zoom":"pageWidth","token":1}"#).is_ok());
    assert!(parse_zoom_request(r#"{"zoom":9,"token":1}"#).is_err());
    assert!(parse_zoom_request(r#"{"zoom":501,"token":1}"#).is_err());
}

#[test]
fn a_window_resize_under_the_fit_is_not_reported_at_all() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    // Open leaves view mode's page-width fit on; the first reading is a baseline, not a change.
    frames(&mut h, 3);
    assert!(zoom_events(bridge.take_events()).is_empty(), "nothing was zoomed");
    let before = h.state().canvas.scale;
    h.set_size(egui::vec2(700.0, 800.0));
    frames(&mut h, 3);
    assert!((h.state().canvas.scale - before).abs() > 1e-3, "the fitted page did follow the window");
    assert_eq!(zoom_events(bridge.take_events()), vec![]);
}

#[test]
fn a_request_that_changes_nothing_is_still_answered() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    for token in [1, 2] {
        bridge.send(HostCommand::SetZoom(ZoomRequest { zoom: ZoomValue::Percent(120), token }));
        frames(&mut h, 3);
        assert_eq!(
            zoom_events(bridge.take_events()),
            vec![HostEvent::ZoomChanged { percent: 120, page_width: false, cause: Cause::Host, token: Some(token) }]
        );
    }
}

#[test]
fn a_host_zoom_leaves_the_fit_and_the_persons_next_zoom_has_no_token() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    bridge.send(HostCommand::SetZoom(ZoomRequest { zoom: ZoomValue::Percent(100), token: 3 }));
    frames(&mut h, 3);
    assert!(h.state().session.view.fit.is_empty(), "a percentage ends the fit");
    bridge.take_events();
    h.state_mut().run("view.zoomOut", serde_json::json!({})).unwrap();
    frames(&mut h, 3);
    let events = zoom_events(bridge.take_events());
    assert!(matches!(events.as_slice(), [HostEvent::ZoomChanged { percent, cause: Cause::User, token: None, .. }] if *percent < 100), "{events:?}");
}

#[test]
fn ctrl_wheel_in_the_canvas_is_reported_as_the_persons() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    let canvas = h.state().canvas.canvas_rect.expect("the canvas was drawn");
    h.event(egui::Event::PointerMoved(canvas.center()));
    frames(&mut h, 1);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, 3.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::COMMAND | egui::Modifiers::CTRL,
    });
    frames(&mut h, 6);
    let events = zoom_events(bridge.take_events());
    assert_persons_gesture_settles(&h, &events);
}

#[test]
fn a_host_zoom_does_not_widen_view_modes_command_gate() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    bridge.send(HostCommand::SetZoom(ZoomRequest { zoom: ZoomValue::Percent(200), token: 1 }));
    frames(&mut h, 3);
    assert!(h.state_mut().run("text.insert", serde_json::json!({"text": "x"})).is_err(), "view mode still refuses edits");
}

#[test]
fn a_pinch_in_the_canvas_is_reported_as_the_persons() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    let canvas = h.state().canvas.canvas_rect.expect("the canvas was drawn");
    h.event(egui::Event::PointerMoved(canvas.center()));
    frames(&mut h, 1);
    h.event(egui::Event::Zoom(0.6));
    frames(&mut h, 6);
    let events = zoom_events(bridge.take_events());
    assert_persons_gesture_settles(&h, &events);
}

#[test]
fn the_first_frame_of_a_second_open_is_a_baseline_not_a_change() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    bridge.send(HostCommand::SetZoom(ZoomRequest { zoom: ZoomValue::Percent(150), token: 1 }));
    frames(&mut h, 3);
    bridge.take_events();
    let events = zoom_events(open_doc(&mut h, &bridge, &clauses(40)));
    assert!(events.is_empty(), "{events:?}");
    // The next change is reported against that baseline.
    h.state_mut().run("view.zoomIn", serde_json::json!({})).unwrap();
    frames(&mut h, 3);
    let events = zoom_events(bridge.take_events());
    assert!(matches!(events.as_slice(), [HostEvent::ZoomChanged { cause: Cause::User, .. }]), "{events:?}");
}

#[test]
fn a_zoom_asked_before_any_open_does_not_make_the_opens_first_frame_a_change() {
    let (mut h, bridge) = embed_app();
    bridge.send(HostCommand::SetZoom(ZoomRequest { zoom: ZoomValue::Percent(150), token: 5 }));
    frames(&mut h, 3);
    assert_eq!(
        zoom_events(bridge.take_events()),
        vec![HostEvent::ZoomChanged { percent: 150, page_width: false, cause: Cause::Host, token: Some(5) }],
        "the request is answered even with nothing open"
    );
    let events = zoom_events(open_doc(&mut h, &bridge, &clauses(10)));
    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn the_persons_fit_page_width_button_is_reported_as_theirs() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    bridge.send(HostCommand::SetZoom(ZoomRequest { zoom: ZoomValue::Percent(100), token: 1 }));
    frames(&mut h, 3);
    bridge.take_events();
    h.get_by_label("Fit page width").click();
    frames(&mut h, 4);
    let events = zoom_events(bridge.take_events());
    assert!(matches!(events.as_slice(), [HostEvent::ZoomChanged { cause: Cause::User, token: None, page_width: true, .. }]), "{events:?}");
    let Some(HostEvent::ZoomChanged { percent, .. }) = events.first() else { unreachable!() };
    assert_eq!(*percent, displayed_percent(&h));
}

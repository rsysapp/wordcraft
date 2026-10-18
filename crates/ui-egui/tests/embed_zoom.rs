//! `setZoom` and `zoomChanged` (embed API 0.3): the host sets the zoom and hears of every change,
//! with the token of the request that caused it, or none when the person did.

mod common;

use common::*;
use wordcraft_ui_egui::host::{Cause, FitWidth, HostCommand, HostEvent, ZoomRequest, ZoomValue};

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
    assert!(
        zoom_events(bridge.take_events()).iter().all(|event| matches!(event, HostEvent::ZoomChanged { page_width: true, cause: Cause::Host, .. }))
    );
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
    assert!(!events.is_empty(), "a wheel zoom reaches the host");
    assert!(
        events.iter().all(|event| matches!(event, HostEvent::ZoomChanged { cause: Cause::User, token: None, page_width: false, .. })),
        "{events:?}"
    );
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
    assert!(!events.is_empty(), "a pinch reaches the host");
    assert!(
        events.iter().all(|event| matches!(event, HostEvent::ZoomChanged { cause: Cause::User, token: None, page_width: false, .. })),
        "{events:?}"
    );
}

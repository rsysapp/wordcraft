//! WordApp runs headless under egui_kittest, the way the desktop and web shells drive it.

mod common;

use common::*;

#[test]
fn the_full_window_draws_its_chrome_and_pages() {
    let mut h = full_app(wordcraft_engine::sample::sample_document());
    frames(&mut h, 4);
    let text = drawn_text(&h);
    assert!(text.contains("AutoSave"), "title bar missing:\n{text}");
    // Without `brand` (the embed build's setting) there is no community button.
    assert_eq!(text.contains("Discord"), cfg!(feature = "brand"), "community button with brand {}:\n{text}", cfg!(feature = "brand"));
    assert!(!h.state().canvas.page_rects.is_empty(), "no pages laid out");
}

#[test]
fn the_window_title_is_sent_only_when_it_changes() {
    let mut h = full_app(clauses(3));
    frames(&mut h, 3);
    assert!(!viewport_commands(&h).iter().any(|c| matches!(c, egui::ViewportCommand::Title(_))), "title re-sent on an idle frame");
    h.state_mut().session.dirty = true;
    frames(&mut h, 1);
    assert!(viewport_commands(&h).iter().any(|c| matches!(c, egui::ViewportCommand::Title(t) if t.contains('•'))), "changed title not sent");
}

#[test]
fn the_clock_is_wall_time_in_milliseconds() {
    // Native builds already pass; this pins the behaviour the wasm build gets from web-time
    // (checked in the browser smoke test, Task 15).
    let first = wordcraft_ui_egui::now_ms();
    assert!(first > 1.7e12, "now_ms() = {first}");
    assert!(wordcraft_ui_egui::now_ms() >= first);
}

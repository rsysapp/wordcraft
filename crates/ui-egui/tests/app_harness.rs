//! WordApp runs headless under egui_kittest, the way the desktop and web shells drive it.

mod common;

use common::*;

#[test]
fn the_full_window_draws_its_chrome_and_pages() {
    let mut h = full_app(wordcraft_engine::sample::sample_document());
    frames(&mut h, 4);
    let text = drawn_text(&h);
    assert!(text.contains("AutoSave"), "title bar missing:\n{text}");
    assert!(text.contains("Discord"), "community button missing:\n{text}");
    assert!(!h.state().canvas.page_rects.is_empty(), "no pages laid out");
}

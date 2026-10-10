//! Shared helpers for WordApp UI tests: an egui_kittest harness that runs the app like the
//! desktop and web shells (`logic`, then `ui`, every frame). Nothing is rendered to pixels, so
//! the tests need no GPU.
#![allow(dead_code)]

use egui_kittest::Harness;
use wordcraft_doc::Document;
use wordcraft_engine::Session;
use wordcraft_ui_egui::{Services, WordApp};

pub const WINDOW: egui::Vec2 = egui::vec2(1100.0, 800.0);

/// A harness running `app` at `pixels_per_point`.
pub fn harness(app: WordApp, pixels_per_point: f32) -> Harness<'static, WordApp> {
    Harness::builder().with_size(WINDOW).with_pixels_per_point(pixels_per_point).with_max_steps(12).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    )
}

/// The full application with `doc` open.
pub fn full_app(doc: Document) -> Harness<'static, WordApp> {
    let mut app = WordApp::new(Session::new(doc), Services::default());
    app.autosave = false;
    harness(app, 1.0)
}

/// Run `n` frames (queued input events each take a frame of their own).
pub fn frames(h: &mut Harness<'_, WordApp>, n: usize) {
    h.run_steps(n);
}

/// `n` numbered clauses: plain paragraphs, enough text for several pages at n = 120.
pub fn clauses(n: usize) -> Document {
    let text: Vec<String> = (1..=n)
        .map(|i| format!("Clause {i}. The Supplier shall deliver the goods listed in Schedule {i} within thirty days of the order."))
        .collect();
    Document::from_text(&text.join("\n"))
}

/// Every shape painted in the last frame, nested shapes flattened, in paint order.
pub fn shapes(h: &Harness<'_, WordApp>) -> Vec<egui::Shape> {
    fn flatten(shape: &egui::Shape, out: &mut Vec<egui::Shape>) {
        match shape {
            egui::Shape::Vec(list) => list.iter().for_each(|s| flatten(s, out)),
            other => out.push(other.clone()),
        }
    }
    let mut out = Vec::new();
    for clipped in &h.output().shapes {
        flatten(&clipped.shape, &mut out);
    }
    out
}

/// All text egui painted in the last frame (page content is a texture, so it never appears).
pub fn drawn_text(h: &Harness<'_, WordApp>) -> String {
    shapes(h)
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Viewport commands the app sent in the last frame.
pub fn viewport_commands(h: &Harness<'_, WordApp>) -> Vec<egui::ViewportCommand> {
    h.output().viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default()
}

use wordcraft_ui_egui::host::{EmbedProfile, HostBridge};

/// The embed build's app in view mode with `doc` loaded, and the bridge a host page would hold.
pub fn view_app(doc: Document, pixels_per_point: f32) -> (Harness<'static, WordApp>, HostBridge) {
    let bridge = HostBridge::new();
    let app = WordApp::new(Session::new(doc), Services::default()).with_host(bridge.clone(), EmbedProfile::View);
    let mut h = harness(app, pixels_per_point);
    bridge.set_context(h.ctx.clone());
    frames(&mut h, 2);
    // Start each test with an empty event queue.
    bridge.take_events();
    (h, bridge)
}

use wordcraft_doc::Pos;

/// The document as JSON: equal strings mean an unchanged document.
pub fn doc_json(h: &Harness<'_, WordApp>) -> String {
    serde_json::to_string(&h.state().session.doc).expect("document serialises")
}

pub fn press(at: egui::Pos2, button: egui::PointerButton) -> egui::Event {
    egui::Event::PointerButton { pos: at, button, pressed: true, modifiers: egui::Modifiers::NONE }
}

pub fn release(at: egui::Pos2, button: egui::PointerButton) -> egui::Event {
    egui::Event::PointerButton { pos: at, button, pressed: false, modifiers: egui::Modifiers::NONE }
}

/// Click at `at`, then let the app settle.
pub fn click(h: &mut Harness<'_, WordApp>, at: egui::Pos2, button: egui::PointerButton) {
    h.event(egui::Event::PointerMoved(at));
    h.event(press(at, button));
    h.event(release(at, button));
    frames(h, 4);
}

pub fn double_click(h: &mut Harness<'_, WordApp>, at: egui::Pos2) {
    h.event(egui::Event::PointerMoved(at));
    for _ in 0..2 {
        h.event(press(at, egui::PointerButton::Primary));
        h.event(release(at, egui::PointerButton::Primary));
    }
    frames(h, 4);
}

/// Screen point (egui points) in the middle of the line at `pos`.
pub fn screen_of(h: &mut Harness<'_, WordApp>, pos: &Pos) -> egui::Pos2 {
    let app = h.state_mut();
    let layout = app.session.layout();
    let caret = layout.caret_on(pos, app.session.page_hint).expect("position is laid out");
    wordcraft_ui_egui::canvas::page_to_screen(app, caret.page, caret.x, caret.top + caret.height / 2.0).expect("page is on screen")
}

/// A harness that draws only the side panes, after installing the app's fonts.
pub fn panes_harness(app: WordApp) -> Harness<'static, WordApp> {
    let mut warm_frames = 0;
    Harness::builder().with_size(WINDOW).with_max_steps(12).build_ui_state(
        move |ui, app: &mut WordApp| {
            if warm_frames < 2 {
                if warm_frames == 0 {
                    wordcraft_ui_egui::theme::install_fonts(ui.ctx());
                }
                warm_frames += 1;
                ui.ctx().request_repaint();
                return;
            }
            wordcraft_ui_egui::panes::show(app, ui);
        },
        app,
    )
}

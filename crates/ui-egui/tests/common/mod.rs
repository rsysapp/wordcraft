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

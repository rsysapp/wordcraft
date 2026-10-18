//! Shared helpers for WordApp UI tests: an egui_kittest harness that runs the app like the
//! desktop and web shells (`logic`, then `ui`, every frame). Nothing is rendered to pixels, so
//! the tests need no GPU.
#![allow(dead_code)]

use egui_kittest::Harness;
use wordcraft_doc::Document;
use wordcraft_engine::Session;
use wordcraft_ui_egui::{Services, WordApp};

pub const WINDOW: egui::Vec2 = egui::vec2(1100.0, 800.0);

/// The view bar's find field, by its name: since the upstream merge it is not the window's only
/// text input.
pub fn find_field() -> impl Fn(&egui_kittest::kittest::AccessKitNode<'_>) -> bool {
    |node| node.role() == egui::accesskit::Role::TextInput && node.label().as_deref() == Some("Find in document")
}

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

/// The embed build's app in view mode with `doc` opened by the host (nothing opened for an empty
/// document), and the bridge a host page would hold.
pub fn view_app(doc: Document, pixels_per_point: f32) -> (Harness<'static, WordApp>, HostBridge) {
    let bridge = HostBridge::new();
    let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(bridge.clone(), EmbedProfile::View);
    let mut h = harness(app, pixels_per_point);
    bridge.set_context(h.ctx.clone());
    // As the embed crate's app creator does.
    wordcraft_ui_egui::host::prepare_context(&h.ctx);
    frames(&mut h, 2);
    // Through the host, as the embed build only ever gets a document: the host is told of pages
    // only after `opened`.
    if doc != Document::new() {
        bridge.send(HostCommand::Open { name: "agreement.docx".into(), bytes: docx(&doc), options: Default::default() });
        frames(&mut h, 4);
    }
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

/// A real double-click at `at`. kittest runs one frame per queued event, a quarter second apart,
/// so two queued clicks are further apart than egui's double-click delay; both clicks go into one
/// frame's input instead.
pub fn double_click(h: &mut Harness<'_, WordApp>, at: egui::Pos2) {
    h.event(egui::Event::PointerMoved(at));
    h.step();
    for _ in 0..2 {
        h.input_mut().events.push(press(at, egui::PointerButton::Primary));
        h.input_mut().events.push(release(at, egui::PointerButton::Primary));
    }
    h.step();
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

use wordcraft_ui_egui::host::{HostCommand, HostEvent};

/// View mode with nothing open yet, as right after `EmbeddedEditor.mount`.
pub fn embed_app() -> (Harness<'static, WordApp>, HostBridge) {
    view_app(Document::new(), 1.0)
}

/// As [`embed_app`], mounted with `MountOptions.viewBar` set to `view_bar`.
pub fn embed_app_with_view_bar(view_bar: bool) -> (Harness<'static, WordApp>, HostBridge) {
    let bridge = HostBridge::new();
    let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(bridge.clone(), EmbedProfile::View).with_view_bar(view_bar);
    let mut h = harness(app, 1.0);
    bridge.set_context(h.ctx.clone());
    wordcraft_ui_egui::host::prepare_context(&h.ctx);
    frames(&mut h, 2);
    bridge.take_events();
    (h, bridge)
}

pub fn docx(doc: &Document) -> Vec<u8> {
    wordcraft_engine::io::save_bytes("fixture.docx", doc).expect("write docx")
}

/// Open `doc` the way a host page does; returns the events that followed.
pub fn open_doc(h: &mut Harness<'_, WordApp>, bridge: &HostBridge, doc: &Document) -> Vec<HostEvent> {
    bridge.send(HostCommand::Open { name: "agreement.docx".into(), bytes: docx(doc), options: Default::default() });
    frames(h, 4);
    bridge.take_events()
}

pub fn pages(h: &mut Harness<'_, WordApp>) -> usize {
    h.state_mut().session.layout().pages.len()
}

/// Where a selection ending at `end` should report itself: the bottom of its line, at its x.
pub fn selection_end_on_screen(h: &mut Harness<'_, WordApp>, end: &Pos) -> (f32, f32) {
    let app = h.state_mut();
    let layout = app.session.layout();
    let caret = layout.caret_on(end, app.session.page_hint).expect("end is laid out");
    let at = wordcraft_ui_egui::canvas::page_to_screen(app, caret.page, caret.x, caret.top + caret.height).expect("page is on screen");
    (at.x, at.y)
}

/// Drag from `from` to `to`; returns the events sent while the pointer was down and after release.
pub fn drag_select(h: &mut Harness<'_, WordApp>, bridge: &HostBridge, from: &Pos, to: &Pos) -> (Vec<HostEvent>, Vec<HostEvent>) {
    let (a, b) = (screen_of(h, from), screen_of(h, to));
    h.event(egui::Event::PointerMoved(a));
    h.event(press(a, egui::PointerButton::Primary));
    h.event(egui::Event::PointerMoved(b));
    h.step();
    frames(h, 2);
    let during = bridge.take_events();
    h.event(release(b, egui::PointerButton::Primary));
    h.step();
    (during, bridge.take_events())
}

/// "Rent is due thirty sixty days.", written with "thirty" as a tracked deletion and "sixty" as a
/// tracked insertion, by an author the host never sees. Built through the engine so the file holds
/// real `w:del` and `w:ins` runs.
pub fn tracked_lease_docx() -> Vec<u8> {
    let mut session = Session::new(Document::from_text("Rent is due thirty days."));
    session.author = "Ana Setia".into();
    session.run("review.trackChanges", &serde_json::json!({ "value": true })).expect("track changes");
    session.run("select.text", &serde_json::json!({ "text": "thirty" })).expect("select the old wording");
    session.run("text.insert", &serde_json::json!({ "text": "sixty" })).expect("replace it");
    assert!(session.doc.revisions.len() >= 2, "a tracked deletion and a tracked insertion: {:?}", session.doc.revisions);
    wordcraft_engine::io::save_bytes("lease.docx", &session.doc).expect("write docx")
}

/// The text the layout places, line by line: what a reader sees on the pages. Wording left out of
/// the layout (tracked deletions in the clean display) is not in it.
pub fn laid_out_text(h: &mut Harness<'_, WordApp>) -> String {
    use wordcraft_layout::Placed;
    use wordcraft_layout::para::ClKind;

    let app = h.state_mut();
    let layout = app.session.layout();
    let mut out = String::new();
    for page in &layout.pages {
        for item in &page.items {
            let Placed::Lines { story, path, para, l0, l1, .. } = item else { continue };
            let Some(paragraph) = app.session.doc.para(*story, path) else { continue };
            for line in &para.lines[*l0..*l1] {
                for cluster in &para.clusters[line.c0..line.c1] {
                    if matches!(cluster.kind, ClKind::Text | ClKind::Space) {
                        out.push_str(paragraph.text.get(cluster.start..cluster.end).unwrap_or(""));
                    }
                }
                out.push('\n');
            }
        }
    }
    out
}

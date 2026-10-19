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

/// A wheel or trackpad scroll of `dy` points over the canvas's centre (negative scrolls down, as
/// egui reports it); the pointer is moved there first and the wheel event sent after a frame.
pub fn wheel(h: &mut Harness<'_, WordApp>, dy: f32) {
    let canvas = h.state().canvas.canvas_rect.expect("the canvas was drawn");
    h.event(egui::Event::PointerMoved(canvas.center()));
    frames(h, 1);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, dy),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
}

/// `events` without the `viewport` ones: tests of page, jump and mode events say nothing of where
/// the view is, which `embed_viewport.rs` pins.
pub fn without_viewports(events: Vec<HostEvent>) -> Vec<HostEvent> {
    events.into_iter().filter(|event| !matches!(event, HostEvent::Viewport { .. })).collect()
}

// ---- marks by location (embed API 0.3) ----

use wordcraft_doc::{Block, Paragraph, Path, StoryRef};
use wordcraft_docx::facts::{accepted_text, model_end_offset, model_offset};

/// Run frames until every host mark is resolved (a long list resolves over several frames).
pub fn settle_marks(h: &mut Harness<'_, WordApp>) {
    for _ in 0..2_000 {
        frames(h, 1);
        if !h.state().host.as_ref().expect("embed host").marks_pending() {
            frames(h, 1);
            return;
        }
    }
    panic!("marks never finished resolving");
}

/// The mark rectangles painted last frame (screen points), topmost last.
pub fn painted_marks(h: &Harness<'_, WordApp>) -> Vec<egui::Rect> {
    h.state().host.as_ref().expect("embed host").mark_rects().into_iter().map(|(rect, _)| rect).collect()
}

/// The body paragraph at top-level `index`, as a copy.
pub fn body_paragraph(h: &Harness<'_, WordApp>, index: usize) -> Paragraph {
    match h.state().session.doc.body.get(index).map(|block| &**block) {
        Some(Block::Para(paragraph)) => paragraph.clone(),
        other => panic!("body block {index} is not a paragraph: {other:?}"),
    }
}

pub fn first_body_paragraph(h: &Harness<'_, WordApp>) -> Paragraph {
    body_paragraph(h, 0)
}

/// The accepted text of top-level body paragraph `index`: what a location's fingerprint names.
pub fn paragraph_text(h: &Harness<'_, WordApp>, index: usize) -> String {
    accepted_text(&body_paragraph(h, index))
}

/// The accepted text of the body paragraph at `path` (a paragraph in a table cell included).
pub fn paragraph_text_at(h: &Harness<'_, WordApp>, path: &Path) -> String {
    accepted_text(h.state().session.doc.para(StoryRef::Body, path).expect("a paragraph at the path"))
}

/// The `Origin.index` of the body paragraph at `path`: the number a location names it by.
pub fn origin_at(h: &Harness<'_, WordApp>, path: &Path) -> u32 {
    h.state().session.doc.para(StoryRef::Body, path).and_then(|paragraph| paragraph.origin).expect("the paragraph has an origin").index
}

/// The embed app in view mode with `bytes` opened in the clean display (tracked changes accepted).
pub fn embed_app_open_final(bytes: &[u8]) -> (Harness<'static, WordApp>, HostBridge) {
    let (mut h, bridge) = embed_app();
    bridge.send(HostCommand::Open {
        name: "lease.docx".into(),
        bytes: bytes.to_vec(),
        options: wordcraft_ui_egui::host::OpenOptions { display: wordcraft_ui_egui::host::Display::Final },
    });
    frames(&mut h, 4);
    bridge.take_events();
    (h, bridge)
}

/// A paragraph, a table of `rows` rows of one cell each, and a closing paragraph: the table is the
/// document's block 1, named by its first paragraph.
pub fn paragraph_table_paragraph(rows: usize) -> Document {
    use std::sync::Arc;
    use wordcraft_doc::props::CharProps;
    use wordcraft_doc::{Table, para_block};
    let line = |text: &str| Paragraph::with_text(text, CharProps::default());
    let mut table = Table::new(rows, 1, 300.0);
    for (row, slot) in table.rows.iter_mut().enumerate() {
        slot.cells[0].blocks = vec![para_block(line(&format!("Fee {row}. Payable within thirty days of the invoice.")))];
    }
    let mut doc = Document::new();
    doc.body = vec![para_block(line("Schedule of fees")), Arc::new(Block::Table(table)), para_block(line("End of the schedule."))];
    doc
}

/// The screen point at the caret of byte `offset` in the body paragraph at `path`: (x, top, bottom).
fn caret_on_screen(h: &mut Harness<'_, WordApp>, path: &Path, offset: usize) -> (f32, f32, f32) {
    let app = h.state_mut();
    let layout = app.session.layout();
    let caret = layout.caret_on(&Pos::new(StoryRef::Body, path.clone(), offset), 0).expect("the position is laid out");
    let top = wordcraft_ui_egui::canvas::page_to_screen(app, caret.page, caret.x, caret.top).expect("the page is on screen");
    let bottom = wordcraft_ui_egui::canvas::page_to_screen(app, caret.page, caret.x, caret.top + caret.height).expect("the page is on screen");
    (top.x, top.y, bottom.y)
}

/// The mark rectangle `rect` starts and ends where accepted units `range` of top-level body
/// paragraph `block` start and end on the screen (within a point and a half).
pub fn assert_close_to_words(h: &mut Harness<'_, WordApp>, rect: &egui::Rect, block: usize, range: std::ops::Range<u32>) {
    let paragraph = body_paragraph(h, block);
    let start = model_offset(&paragraph, range.start).expect("the range starts in the paragraph");
    let end = model_end_offset(&paragraph, range.end).expect("the range ends in the paragraph");
    let path = Path::top(block);
    let (start_x, ..) = caret_on_screen(h, &path, start);
    let (end_x, ..) = caret_on_screen(h, &path, end);
    assert!((rect.min.x - start_x).abs() < 1.5, "the mark starts at {} but the words start at {start_x}", rect.min.x);
    assert!((rect.max.x - end_x).abs() < 1.5, "the mark ends at {} but the words end at {end_x}", rect.max.x);
}

/// The words of top-level body paragraph `block` that lie under `rect`: every character of some width
/// whose middle is inside the rectangle's horizontal span, on the rectangle's line.
pub fn selected_words_under(h: &mut Harness<'_, WordApp>, block: usize, rect: &egui::Rect) -> String {
    let paragraph = body_paragraph(h, block);
    let path = Path::top(block);
    let mut words = String::new();
    for (start, character) in paragraph.text.char_indices() {
        let end = start + character.len_utf8();
        let ((from_x, top, bottom), (to_x, ..)) = (caret_on_screen(h, &path, start), caret_on_screen(h, &path, end));
        // Wording the clean display leaves out has no width.
        if to_x - from_x < 0.5 {
            continue;
        }
        let middle = (from_x + to_x) / 2.0;
        let line_middle = (top + bottom) / 2.0;
        if middle >= rect.min.x && middle <= rect.max.x && line_middle >= rect.min.y && line_middle <= rect.max.y {
            words.push(character);
        }
    }
    words
}

/// How tall the table at top-level block `block` is on the screen: from the top of its first line to
/// the bottom of its last.
pub fn table_height_on_screen(h: &mut Harness<'_, WordApp>, block: usize) -> f32 {
    let rows = match h.state().session.doc.body.get(block).map(|b| &**b) {
        Some(Block::Table(table)) => table.rows.len(),
        other => panic!("block {block} is not a table: {other:?}"),
    };
    let block = u32::try_from(block).expect("block index");
    let first = Path(vec![block, 0, 0, 0]);
    let last = Path(vec![block, u32::try_from(rows - 1).expect("rows"), 0, 0]);
    let last_length = h.state().session.doc.para(StoryRef::Body, &last).expect("the last cell's paragraph").text.len();
    let (_, top, _) = caret_on_screen(h, &first, 0);
    let (_, _, bottom) = caret_on_screen(h, &last, last_length);
    bottom - top
}

// ---- the clean display with tracked changes (Clauflow #42, embed 0.4.1) ----

/// `text` (paragraphs separated by newlines) as a file in which each wording of `deleted` is a tracked
/// deletion and each wording of `hidden` is hidden text. Built through the engine so the file holds
/// real `w:del` runs and real hidden runs. Each wording must occur once.
pub fn docx_with_changes(text: &str, deleted: &[&str], hidden: &[&str]) -> Vec<u8> {
    let mut session = Session::new(Document::from_text(text));
    session.author = "Ana Setia".into();
    for wording in hidden {
        session.run("select.text", &serde_json::json!({ "text": wording })).expect("select the hidden wording");
        session.run("format.hidden", &serde_json::json!({})).expect("hide it");
    }
    session.run("review.trackChanges", &serde_json::json!({ "value": true })).expect("track changes");
    for wording in deleted {
        session.run("select.text", &serde_json::json!({ "text": wording })).expect("select the deleted wording");
        session.run("text.delete", &serde_json::json!({})).expect("delete it");
    }
    wordcraft_engine::io::save_bytes("changes.docx", &session.doc).expect("write docx")
}

/// The embed app mounted with or without its view bar and `bytes` opened in the clean display.
pub fn embed_app_final(bytes: &[u8], view_bar: bool) -> (Harness<'static, WordApp>, HostBridge) {
    let (mut h, bridge) = embed_app_with_view_bar(view_bar);
    bridge.send(HostCommand::Open {
        name: "changes.docx".into(),
        bytes: bytes.to_vec(),
        options: wordcraft_ui_egui::host::OpenOptions { display: wordcraft_ui_egui::host::Display::Final },
    });
    frames(&mut h, 4);
    bridge.take_events();
    (h, bridge)
}

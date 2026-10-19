//! Marks by location (embed API 0.3): a mark names a paragraph by block and fingerprint and a range
//! of its accepted text, is painted only when the paragraph is the one the host meant, and is
//! reported (`locationMismatch`) when it is not. Three styles: fill, outline, bar.
mod common;

use common::*;
use wordcraft_doc::Path;
use wordcraft_docx::facts::{accepted_text, fingerprint};
use wordcraft_ui_egui::host::{BlockRef, Cause, HostCommand, HostEvent, JumpTarget, LocationSpec, MarkSpec, MarkStyle};

const GREEN: &str = "#16a34a4d";

fn location_mark(id: &str, index: u32, text_of_block: &str, from: Option<u32>, to: Option<u32>, style: MarkStyle) -> MarkSpec {
    MarkSpec {
        id: id.into(),
        text: None,
        page: None,
        color: GREEN.into(),
        style,
        at: Some(LocationSpec { block: BlockRef { part: "word/document.xml".into(), index }, fingerprint: fingerprint(text_of_block), from, to }),
    }
}

fn mismatches(events: &[HostEvent]) -> Vec<Vec<String>> {
    events
        .iter()
        .filter_map(|event| match event {
            HostEvent::LocationMismatch { ids } => Some(ids.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_location_mark_paints_its_words() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    let text = paragraph_text(&h, 3);
    bridge.send(HostCommand::SetMarks(vec![location_mark("m", 3, &text, Some(0), Some(6), MarkStyle::Fill)]));
    settle_marks(&mut h);
    let rects = painted_marks(&h);
    assert_eq!(rects.len(), 1);
    assert_close_to_words(&mut h, &rects[0], 3, 0..6);
    assert_eq!(selected_words_under(&mut h, 3, &rects[0]), "Clause");
    assert!(mismatches(&bridge.take_events()).is_empty(), "a matching mark is not reported");
}

#[test]
fn a_mark_whose_fingerprint_differs_is_reported_not_painted() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    bridge.take_events();
    bridge.send(HostCommand::SetMarks(vec![location_mark("stale", 3, "Some other wording.", Some(0), Some(4), MarkStyle::Fill)]));
    settle_marks(&mut h);
    assert!(painted_marks(&h).is_empty());
    assert!(bridge.take_events().contains(&HostEvent::LocationMismatch { ids: vec!["stale".into()] }));
}

#[test]
fn one_stale_mark_does_not_hold_back_the_good_ones_and_is_reported_once() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    bridge.take_events();
    let text = paragraph_text(&h, 3);
    bridge.send(HostCommand::SetMarks(vec![
        location_mark("good", 3, &text, Some(0), Some(6), MarkStyle::Fill),
        location_mark("stale", 4, &text, Some(0), Some(6), MarkStyle::Fill),
        location_mark("gone", 99, &text, Some(0), Some(6), MarkStyle::Fill),
    ]));
    settle_marks(&mut h);
    assert_eq!(painted_marks(&h).len(), 1);
    let events = bridge.take_events();
    assert_eq!(mismatches(&events), vec![vec!["stale".to_string(), "gone".to_string()]], "one event naming both, in the host's order");
    // A zoom lays the document out again and the marks are placed again; the host is not told twice.
    bridge.send(HostCommand::SetZoom(wordcraft_ui_egui::host::ZoomRequest { zoom: wordcraft_ui_egui::host::ZoomValue::Percent(150), token: 1 }));
    frames(&mut h, 4);
    settle_marks(&mut h);
    assert_eq!(painted_marks(&h).len(), 1);
    assert!(mismatches(&bridge.take_events()).is_empty(), "reported once per setMarks");
}

#[test]
fn a_range_outside_the_paragraph_is_reported_not_painted() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    bridge.take_events();
    let text = paragraph_text(&h, 3);
    let length = fingerprint(&text).length;
    bridge.send(HostCommand::SetMarks(vec![
        location_mark("past", 3, &text, Some(0), Some(length + 1), MarkStyle::Fill),
        location_mark("backwards", 3, &text, Some(8), Some(2), MarkStyle::Fill),
        location_mark("huge", 3, &text, Some(u32::MAX), None, MarkStyle::Bar),
        location_mark("to-the-end", 3, &text, Some(length - 5), Some(length), MarkStyle::Fill),
    ]));
    settle_marks(&mut h);
    assert_eq!(painted_marks(&h).len(), 1, "only the range that fits");
    assert_eq!(mismatches(&bridge.take_events()), vec![vec!["past".to_string(), "backwards".to_string(), "huge".to_string()]]);
}

#[test]
fn a_range_after_a_deletion_is_painted_on_its_words() {
    let (mut h, bridge) = embed_app_open_final(&tracked_lease_docx());
    let accepted = accepted_text(&first_body_paragraph(&h));
    assert_eq!(accepted, "Rent is due sixty days.");
    bridge.send(HostCommand::SetMarks(vec![location_mark("ins", 0, &accepted, Some(12), Some(17), MarkStyle::Fill)]));
    settle_marks(&mut h);
    let rects = painted_marks(&h);
    assert_eq!(rects.len(), 1);
    assert_eq!(selected_words_under(&mut h, 0, &rects[0]), "sixty");
    assert_close_to_words(&mut h, &rects[0], 0, 12..17);
}

#[test]
fn a_range_ending_before_a_deletion_does_not_include_it() {
    // As stored (markup shown), "thirty" is laid out; a range over "Rent is due " ends before it.
    let (mut h, bridge) = embed_app();
    bridge.send(HostCommand::Open { name: "lease.docx".into(), bytes: tracked_lease_docx(), options: Default::default() });
    frames(&mut h, 4);
    let accepted = accepted_text(&first_body_paragraph(&h));
    bridge.send(HostCommand::SetMarks(vec![location_mark("before", 0, &accepted, Some(0), Some(12), MarkStyle::Fill)]));
    settle_marks(&mut h);
    let rects = painted_marks(&h);
    assert_eq!(rects.len(), 1);
    assert_eq!(selected_words_under(&mut h, 0, &rects[0]), "Rent is due ");
}

#[test]
fn an_empty_range_paints_a_bar_and_nothing_else() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    let text = paragraph_text(&h, 3);
    bridge.send(HostCommand::SetMarks(vec![
        location_mark("place", 3, &text, Some(5), Some(5), MarkStyle::Bar),
        location_mark("nothing", 3, &text, Some(5), Some(5), MarkStyle::Fill),
        location_mark("outline", 3, &text, Some(5), Some(5), MarkStyle::Outline),
    ]));
    settle_marks(&mut h);
    let rects = painted_marks(&h);
    assert_eq!(rects.len(), 1, "only the bar");
    assert!(rects[0].width() <= 4.0, "a bar, not a fill: {}", rects[0].width());
}

#[test]
fn a_bar_sits_in_the_left_margin_beside_the_lines() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    let text = paragraph_text(&h, 3);
    bridge.send(HostCommand::SetMarks(vec![location_mark("bar", 3, &text, Some(0), Some(20), MarkStyle::Bar)]));
    settle_marks(&mut h);
    let rects = painted_marks(&h);
    assert_eq!(rects.len(), 1);
    let bar = rects[0];
    assert!(bar.width() <= 4.0 && bar.width() >= 2.0, "about three points wide: {}", bar.width());
    let paragraph_left = {
        let app = h.state_mut();
        let layout = app.session.layout();
        let caret = layout.caret_on(&wordcraft_doc::Pos::new(wordcraft_doc::StoryRef::Body, Path::top(3), 0), 0).unwrap();
        wordcraft_ui_egui::canvas::page_to_screen(app, caret.page, caret.x, caret.top).unwrap()
    };
    assert!(bar.max.x <= paragraph_left.x, "the bar ({}) is left of the text ({})", bar.max.x, paragraph_left.x);
    assert!(bar.min.y <= paragraph_left.y + 1.0 && bar.max.y >= paragraph_left.y, "beside the paragraph's first line");
}

#[test]
fn a_bar_without_a_range_spans_the_whole_table() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &paragraph_table_paragraph(6));
    let first_cell_path = Path(vec![1, 0, 0, 0]);
    let first_cell = paragraph_text_at(&h, &first_cell_path);
    let origin = origin_at(&h, &first_cell_path);
    bridge.send(HostCommand::SetMarks(vec![location_mark("table", origin, &first_cell, None, None, MarkStyle::Bar)]));
    settle_marks(&mut h);
    let rects = painted_marks(&h);
    assert_eq!(rects.len(), 1);
    let table_height = table_height_on_screen(&mut h, 1);
    assert!(rects[0].height() >= table_height - 2.0, "the bar ({}) covers the table ({table_height})", rects[0].height());
    assert!(rects[0].height() <= table_height + 40.0, "and not the page: {}", rects[0].height());
}

#[test]
fn a_whole_paragraph_with_no_range_is_filled() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    let text = paragraph_text(&h, 2);
    bridge.send(HostCommand::SetMarks(vec![location_mark("all", 2, &text, None, None, MarkStyle::Fill)]));
    settle_marks(&mut h);
    let rects = painted_marks(&h);
    assert!(!rects.is_empty());
    assert_eq!(selected_words_under(&mut h, 2, &rects[0]).chars().next(), Some('C'));
    assert!(selected_words_under(&mut h, 2, &rects[0]).len() > 30, "the first line is covered");
}

#[test]
fn an_outline_is_a_stroke_in_the_colour_at_full_alpha_and_no_fill() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    let text = paragraph_text(&h, 3);
    bridge.send(HostCommand::SetMarks(vec![location_mark("o", 3, &text, Some(0), Some(6), MarkStyle::Outline)]));
    settle_marks(&mut h);
    assert_eq!(painted_marks(&h).len(), 1);
    let full = egui::Color32::from_rgb(0x16, 0xa3, 0x4a);
    let stroked = shapes(&h).into_iter().filter(|shape| matches!(shape, egui::Shape::Rect(r) if r.stroke.color == full)).count();
    assert_eq!(stroked, 1, "one outlined rectangle");
    assert!(
        shapes(&h)
            .iter()
            .all(|shape| !matches!(shape, egui::Shape::Rect(r) if r.fill == egui::Color32::from_rgba_unmultiplied(0x16, 0xa3, 0x4a, 0x4d))),
        "an outline is not filled"
    );
    let width = shapes(&h).into_iter().find_map(|shape| match shape {
        egui::Shape::Rect(r) if r.stroke.color == full => Some(r.stroke.width),
        _ => None,
    });
    assert_eq!(width, Some(1.5));
}

#[test]
fn a_fill_is_the_colour_as_sent() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    let text = paragraph_text(&h, 3);
    bridge.send(HostCommand::SetMarks(vec![location_mark("f", 3, &text, Some(0), Some(6), MarkStyle::Fill)]));
    settle_marks(&mut h);
    let fill = egui::Color32::from_rgba_unmultiplied(0x16, 0xa3, 0x4a, 0x4d);
    assert!(shapes(&h).iter().any(|shape| matches!(shape, egui::Shape::Rect(r) if r.fill == fill)));
}

#[test]
fn clicking_a_location_mark_activates_it() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    let text = paragraph_text(&h, 3);
    bridge.send(HostCommand::SetMarks(vec![
        location_mark("fill", 3, &text, Some(0), Some(6), MarkStyle::Fill),
        location_mark("bar", 5, &paragraph_text(&h, 5), None, None, MarkStyle::Bar),
    ]));
    settle_marks(&mut h);
    bridge.take_events();
    for (index, expected) in [(0, "fill"), (1, "bar")] {
        let rect = painted_marks(&h)[index];
        click(&mut h, rect.center(), egui::PointerButton::Primary);
        let activated: Vec<String> = bridge
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                HostEvent::MarkActivated { id } => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(activated, vec![expected.to_string()]);
    }
}

#[test]
fn text_marks_still_work_beside_location_marks() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    let text = paragraph_text(&h, 5);
    bridge.send(HostCommand::SetMarks(vec![
        MarkSpec { id: "t".into(), text: Some("Clause 2".into()), at: None, page: None, color: "#dc26264d".into(), style: MarkStyle::Fill },
        location_mark("l", 5, &text, Some(0), Some(6), MarkStyle::Fill),
    ]));
    settle_marks(&mut h);
    assert_eq!(painted_marks(&h).len(), 2);
    let ids: Vec<String> = h.state().host.as_ref().unwrap().mark_rects().into_iter().map(|(_, id)| id).collect();
    assert!(ids.contains(&"t".to_string()) && ids.contains(&"l".to_string()), "{ids:?}");
}

#[test]
fn marks_past_the_cap_are_counted_as_dropped() {
    let marks: Vec<_> = (0..2_005).map(|index| serde_json::json!({"id": index.to_string(), "text": "Clause", "color": "#16a34a4d"})).collect();
    let (parsed, dropped) = wordcraft_ui_egui::host::wire::parse_marks_counting(&serde_json::Value::Array(marks).to_string()).unwrap();
    assert_eq!((parsed.len(), dropped), (2_000, 5));
    let (parsed, dropped) = wordcraft_ui_egui::host::wire::parse_marks_counting("[]").unwrap();
    assert_eq!((parsed.len(), dropped), (0, 0));
}

#[test]
fn a_mark_needs_text_or_a_location_but_not_both() {
    use wordcraft_ui_egui::host::wire::parse_marks;
    assert!(parse_marks(r##"[{"id":"x","color":"#000000"}]"##).is_err());
    assert!(
        parse_marks(r##"[{"id":"x","color":"#000000","text":"a","at":{"block":{"part":"p","index":0},"fingerprint":{"length":1,"hash":"00"}}}]"##)
            .is_err()
    );
}

#[test]
fn a_location_mark_parses_with_its_defaults() {
    use wordcraft_ui_egui::host::wire::parse_marks;
    let marks = parse_marks(
        r##"[{"id":"x","color":"#000000","at":{"block":{"part":"word/document.xml","index":4},"fingerprint":{"length":3,"hash":"ab"}}},
             {"id":"y","color":"#000000","style":"bar","text":"a"},
             {"id":"z","color":"#000000","style":"outline","at":{"block":{"part":"p","index":0},"fingerprint":{"length":1,"hash":"00"},"from":1e30,"to":null}}]"##,
    )
    .unwrap();
    assert_eq!(marks[0].style, MarkStyle::Fill, "fill unless said");
    let at = marks[0].at.as_ref().unwrap();
    assert_eq!((at.from, at.to, at.fingerprint.length), (None, None, 3));
    assert_eq!(marks[1].style, MarkStyle::Bar);
    assert_eq!(marks[2].style, MarkStyle::Outline);
    assert_eq!(marks[2].at.as_ref().unwrap().from, Some(u32::MAX), "a huge offset saturates and is later reported");
    assert!(parse_marks(r##"[{"id":"x","color":"#000000","text":"a","style":"glow"}]"##).is_err());
}

#[test]
fn a_rangeless_bar_spans_its_table_even_when_the_first_cell_is_empty() {
    use std::sync::Arc;
    use wordcraft_doc::props::CharProps;
    use wordcraft_doc::{Block, Document, Paragraph, Table, para_block};
    let line = |text: &str| Paragraph::with_text(text, CharProps::default());
    let mut table = Table::new(6, 1, 300.0);
    for (row, slot) in table.rows.iter_mut().enumerate() {
        let text = if row == 0 { String::new() } else { format!("Fee {row}. Payable within thirty days of the invoice.") };
        slot.cells[0].blocks = vec![para_block(line(&text))];
    }
    let mut doc = Document::new();
    doc.body = vec![para_block(line("Schedule of fees")), Arc::new(Block::Table(table)), para_block(line("End of the schedule."))];
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &doc);
    let first_cell_path = Path(vec![1, 0, 0, 0]);
    let first_cell = paragraph_text_at(&h, &first_cell_path);
    assert_eq!(first_cell, "", "the table's first paragraph has no text");
    let origin = origin_at(&h, &first_cell_path);
    bridge.send(HostCommand::SetMarks(vec![location_mark("table", origin, &first_cell, None, None, MarkStyle::Bar)]));
    settle_marks(&mut h);
    let rects = painted_marks(&h);
    assert_eq!(rects.len(), 1);
    let table_height = table_height_on_screen(&mut h, 1);
    assert!(rects[0].height() >= table_height - 2.0, "the bar ({}) covers the table ({table_height}), not one line", rects[0].height());
    assert!(mismatches(&bridge.take_events()).is_empty());
}

#[test]
fn a_range_after_non_ascii_text_is_painted_on_its_words() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &wordcraft_doc::Document::from_text("Caf\u{e9} \u{1F600} Rent due sixty days."));
    let text = paragraph_text(&h, 0);
    // "Caf\u{e9} " is 5 units and the emoji 2 (a surrogate pair), then a space: "Rent due" is units 8 to 16.
    assert_eq!(text.encode_utf16().skip(8).take(8).collect::<Vec<u16>>(), "Rent due".encode_utf16().collect::<Vec<u16>>());
    bridge.send(HostCommand::SetMarks(vec![location_mark("words", 0, &text, Some(8), Some(16), MarkStyle::Fill)]));
    settle_marks(&mut h);
    let rects = painted_marks(&h);
    assert_eq!(rects.len(), 1);
    assert_close_to_words(&mut h, &rects[0], 0, 8..16);
    assert_eq!(selected_words_under(&mut h, 0, &rects[0]), "Rent due");
}

// ---- jumps to a location (embed API 0.3) ----

const BODY_PART: &str = "word/document.xml";

fn location_jump(index: u32, text_of_block: &str, from: Option<u32>, to: Option<u32>) -> JumpTarget {
    JumpTarget::Location { at: LocationSpec { block: BlockRef { part: BODY_PART.into(), index }, fingerprint: fingerprint(text_of_block), from, to } }
}

#[test]
fn a_location_jump_centres_the_place_and_selects_nothing() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    let text = paragraph_text(&h, 90);
    let selection_before = h.state().session.sel.clone();
    bridge.send(HostCommand::JumpTo(location_jump(90, &text, Some(0), Some(0)), None));
    frames(&mut h, 4);
    assert_eq!(h.state().session.sel, selection_before);
    let middle = h.state().canvas.canvas_rect.expect("the canvas was drawn").center().y;
    let place = screen_of(&mut h, &wordcraft_doc::Pos::body(90, 0));
    assert!((place.y - middle).abs() < 80.0, "centred: {} vs {}", place.y, middle);
}

#[test]
fn a_location_jump_without_a_range_goes_to_the_start_of_the_paragraph() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    let text = paragraph_text(&h, 60);
    bridge.send(HostCommand::JumpTo(location_jump(60, &text, None, None), None));
    frames(&mut h, 4);
    let middle = h.state().canvas.canvas_rect.expect("the canvas was drawn").center().y;
    let place = screen_of(&mut h, &wordcraft_doc::Pos::body(60, 0));
    assert!((place.y - middle).abs() < 80.0, "centred: {} vs {}", place.y, middle);
}

#[test]
fn a_location_jump_near_the_top_of_the_document_stops_at_the_top() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    let text = paragraph_text(&h, 1);
    bridge.send(HostCommand::JumpTo(location_jump(1, &text, None, None), None));
    frames(&mut h, 4);
    let view = h.state().canvas.canvas_rect.expect("the canvas was drawn");
    let place = screen_of(&mut h, &wordcraft_doc::Pos::body(1, 0));
    // Half a view above the place is before the start of the document, so the view stays at the start
    // and the place is where it stands there: above the middle.
    assert!(place.y > view.min.y && place.y < view.center().y - 80.0, "the view cannot go above the document: {} in {view:?}", place.y);
}

#[test]
fn a_location_jump_leaves_a_selection_the_person_made_alone() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    let (from, to) = (wordcraft_doc::Pos::body(2, 0), wordcraft_doc::Pos::body(2, 6));
    drag_select(&mut h, &bridge, &from, &to);
    frames(&mut h, 4);
    let selection_before = h.state().session.sel.clone();
    assert_ne!(selection_before.anchor, selection_before.focus, "something is selected");
    bridge.take_events();
    let text = paragraph_text(&h, 90);
    bridge.send(HostCommand::JumpTo(location_jump(90, &text, Some(0), Some(6)), None));
    frames(&mut h, 4);
    assert_eq!(h.state().session.sel, selection_before);
    let events = bridge.take_events();
    assert!(!events.contains(&HostEvent::SelectionCleared), "the person's selection stays: {events:?}");
    assert!(!events.iter().any(|event| matches!(event, HostEvent::JumpNotFound { .. })), "{events:?}");
}

#[test]
fn a_location_jump_is_reported_as_the_hosts_own_move() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.take_events();
    let text = paragraph_text(&h, 90);
    bridge.send(HostCommand::JumpTo(location_jump(90, &text, Some(0), Some(6)), None));
    frames(&mut h, 6);
    let events = bridge.take_events();
    let viewports: Vec<_> = events.iter().filter(|event| matches!(event, HostEvent::Viewport { .. })).collect();
    assert!(!viewports.is_empty(), "the view moved, so the host hears of it");
    assert!(viewports.iter().all(|event| matches!(event, HostEvent::Viewport { cause: Cause::Host, token: None, .. })), "{viewports:?}");
}

#[test]
fn a_location_jump_with_a_wrong_fingerprint_is_not_found() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    bridge.take_events();
    let target = location_jump(5, "Not this.", None, None);
    bridge.send(HostCommand::JumpTo(target.clone(), None));
    frames(&mut h, 3);
    assert!(bridge.take_events().contains(&HostEvent::JumpNotFound { target, token: None }));
}

#[test]
fn a_location_jump_that_is_not_found_moves_nothing() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 2 }, None));
    frames(&mut h, 4);
    let selection_before = h.state().session.sel.clone();
    let scroll_before = h.state().canvas.visible_page;
    bridge.take_events();
    let text = paragraph_text(&h, 100);
    let unknown_block = location_jump(999, &text, None, None);
    let wrong_part = JumpTarget::Location {
        at: LocationSpec { block: BlockRef { part: "word/other.xml".into(), index: 100 }, fingerprint: fingerprint(&text), from: None, to: None },
    };
    for target in [location_jump(100, "Not this.", None, None), unknown_block, wrong_part] {
        bridge.send(HostCommand::JumpTo(target.clone(), None));
        frames(&mut h, 4);
        let events = bridge.take_events();
        assert!(events.contains(&HostEvent::JumpNotFound { target: target.clone(), token: None }), "{target:?}: {events:?}");
        assert!(!events.iter().any(|event| matches!(event, HostEvent::Viewport { .. })), "nothing moved: {events:?}");
    }
    assert_eq!(h.state().session.sel, selection_before);
    assert_eq!(h.state().canvas.visible_page, scroll_before);
}

#[test]
fn a_location_jump_with_a_bad_range_is_not_found() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    let text = paragraph_text(&h, 5);
    let length = fingerprint(&text).length;
    for (from, to) in [(Some(8), Some(3)), (Some(0), Some(length + 1)), (Some(length + 1), None), (Some(u32::MAX), Some(u32::MAX))] {
        bridge.take_events();
        let target = location_jump(5, &text, from, to);
        bridge.send(HostCommand::JumpTo(target.clone(), None));
        frames(&mut h, 3);
        assert!(bridge.take_events().contains(&HostEvent::JumpNotFound { target, token: None }), "{from:?}..{to:?}");
    }
}

#[test]
fn a_location_jump_agrees_with_the_mark_on_the_same_location() {
    // Marks and jumps share one resolution: whatever a mark refuses, a jump refuses.
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    let text = paragraph_text(&h, 5);
    bridge.take_events();
    bridge.send(HostCommand::SetMarks(vec![location_mark("stale", 5, "Other.", None, None, MarkStyle::Fill)]));
    settle_marks(&mut h);
    let target = location_jump(5, "Other.", None, None);
    bridge.send(HostCommand::JumpTo(target.clone(), None));
    frames(&mut h, 3);
    let events = bridge.take_events();
    assert!(events.contains(&HostEvent::LocationMismatch { ids: vec!["stale".into()] }), "{events:?}");
    assert!(events.contains(&HostEvent::JumpNotFound { target, token: None }), "{events:?}");
    let _ = text;
}

#[test]
fn a_range_after_a_deletion_is_jumped_to_on_its_words() {
    let (mut h, bridge) = embed_app_open_final(&tracked_lease_docx());
    let accepted = accepted_text(&first_body_paragraph(&h));
    bridge.take_events();
    let target = location_jump(0, &accepted, Some(12), Some(17));
    bridge.send(HostCommand::JumpTo(target.clone(), None));
    frames(&mut h, 4);
    assert!(!bridge.take_events().contains(&HostEvent::JumpNotFound { target, token: None }));
}

#[test]
fn a_location_jump_parses() {
    use wordcraft_ui_egui::host::wire::parse_jump_target;
    let target =
        parse_jump_target(r#"{"kind":"location","at":{"block":{"part":"word/document.xml","index":4},"fingerprint":{"length":3,"hash":"ab"}}}"#)
            .unwrap();
    let JumpTarget::Location { at } = target else { panic!("{target:?}") };
    assert_eq!((at.block.index, at.from, at.to, at.fingerprint.length), (4, None, None, 3));
    let huge = parse_jump_target(
        r#"{"kind":"location","at":{"block":{"part":"p","index":0},"fingerprint":{"length":1,"hash":"00"},"from":1e30,"to":null}}"#,
    )
    .unwrap();
    assert!(matches!(huge, JumpTarget::Location { at } if at.from == Some(u32::MAX) && at.to.is_none()));
    assert!(parse_jump_target(r#"{"kind":"location"}"#).is_err());
    // A location jump goes back to the host as it came, in `jumpNotFound`.
    let json = serde_json::to_value(location_jump(4, "x", Some(1), None)).unwrap();
    assert_eq!(json["kind"], "location");
    assert_eq!(json["at"]["block"]["index"], 4);
    assert_eq!(json["at"]["from"], 1);
}

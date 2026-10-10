//! View mode's window: a slim view bar (page navigation, zoom, find) above the document canvas.
//! Replace is never drawn: the view profile refuses it, and nothing here offers it.

use egui::{Rect, Sense, Stroke, Ui, vec2};
use serde_json::json;
use wordcraft_engine::Selection;

use crate::theme::{Tokens, regular};
use crate::{WordApp, icons};

pub fn show(app: &mut WordApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    bar(app, ui);
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| {
        crate::canvas::show(app, ui);
    });
}

fn bar(app: &mut WordApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::top("embed_view_bar")
        .exact_size(34.0)
        .frame(egui::Frame::NONE.fill(t.ribbon).inner_margin(egui::Margin::symmetric(8, 0)).stroke(Stroke::new(1.0, t.border)))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
                page_controls(app, ui);
                ui.separator();
                zoom_controls(app, ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| find_controls(app, ui));
            });
        });
}

fn page_controls(app: &mut WordApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let pages = app.session.layout().pages.len();
    // No wrap-around: the ends disable their button. A document with no pages disables both.
    let current = app.canvas.visible_page.unwrap_or(0).min(pages.saturating_sub(1));
    if icon_button(ui, "chevronUp", "Previous page", current > 0) {
        go_to_page(app, current.saturating_sub(1));
    }
    let text = if pages == 0 { "No pages".to_string() } else { format!("Page {} of {pages}", current + 1) };
    ui.label(egui::RichText::new(text).font(regular(12.0)).color(t.text_dim));
    if icon_button(ui, "chevronDown", "Next page", current.saturating_add(1) < pages) {
        go_to_page(app, current.saturating_add(1));
    }
}

fn zoom_controls(app: &mut WordApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    if icon_button(ui, "minus", "Zoom out", true) {
        let _ = app.run("view.zoomOut", json!({}));
    }
    // What is drawn, so a fitted page shows the zoom it got.
    let percent = (app.canvas.scale / crate::canvas::PX_PER_PT * 100.0).round();
    let percent = if percent.is_finite() { percent } else { 100.0 };
    ui.label(egui::RichText::new(format!("{percent:.0}%")).font(regular(12.0)).color(t.text_dim));
    if icon_button(ui, "plus", "Zoom in", true) {
        let _ = app.run("view.zoomIn", json!({}));
    }
    if icon_button(ui, "pageWidth", "Fit page width", true) {
        let _ = app.run("view.pageWidth", json!({}));
    }
}

/// Which way a find goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindStep {
    First,
    Next,
    Previous,
}

fn find_controls(app: &mut WordApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let Some((mut query, focus)) = app.host.as_mut().map(|host| (std::mem::take(&mut host.find_query), std::mem::take(&mut host.focus_find))) else {
        return;
    };
    // Right to left: the first widget added sits at the right edge.
    let has_query = !query.is_empty();
    let next = icon_button(ui, "chevronDown", "Next match", has_query);
    let previous = icon_button(ui, "chevronUp", "Previous match", has_query);
    if has_query {
        let count = app.session.find.results.len();
        let status =
            if count == 0 { "No matches".to_string() } else { format!("{} of {count}", app.session.find.current.saturating_add(1).min(count)) };
        ui.label(egui::RichText::new(status).font(regular(12.0)).color(t.text_dim));
    }
    let field = ui.add(egui::TextEdit::singleline(&mut query).id_salt("embed_find").hint_text("Find in document").desired_width(180.0));
    if focus {
        field.request_focus();
    }
    let (enter, shift, escape) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.modifiers.shift, i.key_pressed(egui::Key::Escape)));
    let entered = field.lost_focus() && enter;
    // The text field gives up focus on Escape itself, in the same frame, so `lost_focus` is the signal.
    if (field.has_focus() || field.lost_focus()) && escape {
        // Escape clears the search and leaves the field, as it closes the find bar in the other editor.
        query.clear();
        run_find(app, &query, FindStep::First);
        field.surrender_focus();
    } else if field.changed() {
        run_find(app, &query, FindStep::First);
    } else if previous || (entered && shift) {
        run_find(app, &query, FindStep::Previous);
    } else if next || entered {
        run_find(app, &query, FindStep::Next);
    }
    if entered {
        // Enter steps to the next match and keeps the field ready for another Enter.
        field.request_focus();
    }
    if let Some(host) = app.host.as_mut() {
        host.find_query = query;
    }
}

/// Find `query` and select the match; the canvas scrolls to it. An empty query clears the search and
/// the highlight (the selected match).
pub fn run_find(app: &mut WordApp, query: &str, step: FindStep) {
    if query.is_empty() {
        let was_match =
            app.session.find.results.iter().any(|(start, end)| Selection { anchor: start.clone(), focus: end.clone() } == app.session.sel);
        app.session.find.results.clear();
        app.session.find.current = 0;
        if was_match {
            app.session.sel = Selection::caret(app.session.sel.focus.clone());
        }
        return;
    }
    // The options are set every time: a host's earlier regex search must not make typed text a pattern.
    let _ = match step {
        FindStep::First => app.run("edit.find", json!({"text": query, "matchCase": false, "wholeWord": false, "regex": false})),
        FindStep::Next => app.run("edit.findNext", json!({})),
        FindStep::Previous => app.run("edit.findPrevious", json!({})),
    };
}

/// Show page `index` (0-based) at the top of the view, with the caret on its first line.
pub fn go_to_page(app: &mut WordApp, index: usize) {
    let _ = app.session.run("edit.goto", &json!({"page": index.saturating_add(1)}));
    app.canvas.scroll_to_doc = Some((index, 0.0));
}

/// An icon button that screen readers and UI tests know by `label`.
fn icon_button(ui: &mut Ui, icon: &str, label: &str, enabled: bool) -> bool {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(26.0, 24.0), if enabled { Sense::click() } else { Sense::hover() });
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    if enabled && response.hovered() {
        ui.painter().rect_filled(rect, 4.0, t.hover);
    }
    let color = if enabled { t.icon } else { t.text_disabled };
    icons::paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), icon, color, if enabled { t.accent } else { color });
    let clicked = enabled && response.clicked();
    response.on_hover_text(label);
    clicked
}

/// The page that fills most of the viewport (`pages` and `viewport` in the scroll area's content space).
pub fn visible_page(pages: &[egui::Rect], viewport: egui::Rect) -> Option<usize> {
    // f32::min and max skip a NaN operand, so a non-finite rect would otherwise count as fully visible.
    if !viewport.is_finite() {
        return None;
    }
    let mut best: Option<(usize, f32)> = None;
    for (i, page) in pages.iter().enumerate().filter(|(_, page)| page.is_finite()) {
        let overlap = (page.max.y.min(viewport.max.y) - page.min.y.max(viewport.min.y)).max(0.0);
        if overlap > 0.0 && best.is_none_or(|(_, b)| overlap > b) {
            best = Some((i, overlap));
        }
    }
    best.map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use egui::{Rect, pos2};

    use super::visible_page;

    #[test]
    fn the_page_with_the_most_visible_height_wins() {
        let pages = [Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 100.0)), Rect::from_min_max(pos2(0.0, 110.0), pos2(10.0, 210.0))];
        assert_eq!(visible_page(&pages, Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 80.0))), Some(0));
        assert_eq!(visible_page(&pages, Rect::from_min_max(pos2(0.0, 90.0), pos2(10.0, 170.0))), Some(1));
        assert_eq!(visible_page(&pages, Rect::from_min_max(pos2(0.0, 300.0), pos2(10.0, 400.0))), None);
        assert_eq!(visible_page(&[], Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 80.0))), None);
    }

    #[test]
    fn non_finite_geometry_picks_no_page_or_a_real_one() {
        let nan = Rect::from_min_max(pos2(f32::NAN, f32::NAN), pos2(f32::NAN, f32::NAN));
        let page = Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 100.0));
        assert_eq!(visible_page(&[nan], page), None);
        assert_eq!(visible_page(&[nan, page], Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 50.0))), Some(1));
        assert_eq!(visible_page(&[page], nan), None);
    }
}

//! View mode's window: a slim view bar (page navigation, zoom, find) above the document canvas.
//! Replace is never drawn: the view profile refuses it, and nothing here offers it.

use egui::{Rect, Sense, Stroke, Ui, vec2};
use serde_json::json;
use wordcraft_doc::{Pos, StoryRef};
use wordcraft_engine::Selection;

use super::state::HostState;
use crate::theme::{Tokens, regular};
use crate::{WordApp, icons};

pub fn show(app: &mut WordApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    // The edit strip lives in the bar, so edit mode needs it (docs/embed.md, "No view bar").
    if app.host.as_ref().is_none_or(|host| host.view_bar) {
        bar(app, ui);
    }
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| {
        crate::canvas::show(app, ui);
    });
    super::edit::notice(app, ui.ctx());
}

/// One row of the view bar.
const BAR_HEIGHT: f32 = 34.0;
/// What the find controls take at the bar's right: the field, its two buttons and the match count.
const FIND_WIDTH: f32 = 320.0;

fn bar(app: &mut WordApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let editing = app.host.as_ref().is_some_and(|host| host.edit.is_some());
    let rows = if editing { app.host.as_ref().map_or(1, |host| host.bar_rows.clamp(1, 2)) } else { 1 };
    egui::Panel::top("embed_view_bar")
        .exact_size(BAR_HEIGHT * f32::from(rows))
        .frame(egui::Frame::NONE.fill(t.ribbon).inner_margin(egui::Margin::symmetric(8, 0)).stroke(Stroke::new(1.0, t.border)))
        .show(ui, |ui| {
            if !editing {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
                    page_controls(app, ui);
                    ui.separator();
                    zoom_controls(app, ui);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| find_controls(app, ui));
                });
                return;
            }
            // Edit mode: the edit strip follows the zoom controls when the row has room for it and
            // the find controls, and wraps onto a second row when it has not.
            let width = ui.available_width();
            let row = egui::Layout::left_to_right(egui::Align::Center);
            let mut inline = true;
            ui.allocate_ui_with_layout(vec2(width, BAR_HEIGHT), row, |ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
                page_controls(app, ui);
                ui.separator();
                zoom_controls(app, ui);
                inline = ui.available_width() >= super::edit::strip_width(app, ui) + FIND_WIDTH;
                if inline {
                    super::edit::strip(app, ui, true);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| find_controls(app, ui));
            });
            if !inline {
                ui.allocate_ui_with_layout(vec2(width, BAR_HEIGHT), row, |ui| {
                    ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
                    super::edit::strip(app, ui, false);
                });
            }
            // The panel takes the new row count from the next frame on.
            let wanted = if inline { 1 } else { 2 };
            if let Some(host) = app.host.as_mut()
                && host.bar_rows != wanted
            {
                host.bar_rows = wanted;
                ui.ctx().request_repaint();
            }
        });
}

/// Small dimmed text in the bar (page, zoom and match status).
fn status_label(ui: &mut Ui, text: impl Into<String>) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(text.into()).font(regular(12.0)).color(t.text_dim));
}

fn page_controls(app: &mut WordApp, ui: &mut Ui) {
    let pages = app.session.layout().pages.len();
    let visible = app.canvas.visible_page;
    let jumped = app.host.as_ref().and_then(|host| host.current_page(visible));
    // No wrap-around: the ends disable their button. A document with no pages disables both.
    let current = jumped.unwrap_or(0).min(pages.saturating_sub(1));
    if icon_button(ui, "chevronUp", "Previous page", current > 0) {
        go_to_page(app, current.saturating_sub(1));
    }
    // The current page as a pill in the host's accent, with the host's text colour on it.
    let t = Tokens::get(ui.ctx());
    let label = format!("Page {} of {pages}", if pages == 0 { 0 } else { current + 1 });
    let galley = ui.painter().layout_no_wrap(label, regular(12.0), crate::theme::accent_content(ui.ctx()));
    let (pill, _) = ui.allocate_exact_size(galley.size() + vec2(16.0, 6.0), Sense::hover());
    ui.painter().rect_filled(pill, 10.0, t.accent);
    ui.painter().galley(pill.center() - galley.size() / 2.0, galley, t.accent);
    if icon_button(ui, "chevronDown", "Next page", current.saturating_add(1) < pages) {
        go_to_page(app, current.saturating_add(1));
    }
}

fn zoom_controls(app: &mut WordApp, ui: &mut Ui) {
    if icon_button(ui, "minus", "Zoom out", true) {
        step_zoom(app, "view.zoomOut");
    }
    // What is drawn, so a fitted page shows the zoom it got.
    status_label(ui, format!("{:.0}%", displayed_percent(app)));
    if icon_button(ui, "plus", "Zoom in", true) {
        step_zoom(app, "view.zoomIn");
    }
    if icon_button(ui, "pageWidth", "Fit page width", true) {
        let _ = app.run("view.pageWidth", json!({}));
        keep_target_in_view(app);
    }
}

/// The zoom the page is drawn at, in percent (within the engine's 10 to 500).
fn displayed_percent(app: &WordApp) -> f32 {
    let percent = (app.canvas.scale / crate::canvas::PX_PER_PT * 100.0).round();
    if percent.is_finite() { percent.clamp(10.0, 500.0) } else { 100.0 }
}

/// [`displayed_percent`] as the whole percentage `zoomChanged` reports.
pub(crate) fn displayed_percent_whole(app: &WordApp) -> u32 {
    // Rounded and clamped to 10..=500 above, so the conversion cannot lose anything.
    displayed_percent(app) as u32
}

/// Zoom in or out from what is on screen. While a fit is set the engine's stored zoom (the step's
/// starting point) is stale, so a step from it could zoom the wrong way.
fn step_zoom(app: &mut WordApp, command: &str) {
    if !app.session.view.fit.is_empty() {
        let percent = displayed_percent(app);
        let _ = app.run("view.zoom", json!({"value": percent}));
    }
    let _ = app.run(command, json!({}));
    keep_target_in_view(app);
}

/// A zoom moves every page: put the jump's target at the top again, so it stays in view.
pub(crate) fn keep_target_in_view(app: &mut WordApp) {
    if let Some(target) = app.host.as_mut().and_then(|host| host.target_scroll()) {
        app.canvas.scroll_to_doc = Some(target);
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
    let Some((mut query, focus)) = app.host.as_mut().map(|host| (std::mem::take(&mut host.find_query), std::mem::take(&mut host.focus_find))) else {
        return;
    };
    // Right to left: the first widget added sits at the right edge.
    let has_query = !query.is_empty();
    let next = icon_button(ui, "chevronDown", "Next match", has_query);
    let previous = icon_button(ui, "chevronUp", "Previous match", has_query);
    if has_query {
        status_label(ui, match_status(app));
    }
    let field = ui.add(egui::TextEdit::singleline(&mut query).id_salt("embed_find").hint_text("Find in document").desired_width(180.0));
    // A name for screen readers and UI tests whatever the field holds (egui gives a field only its
    // hint, and only while it is empty).
    ui.ctx().accesskit_node_builder(field.id, |node| node.set_label("Find in document"));
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

/// "3 of 12" while the selection is a match, else the bare count: the selection can move off the
/// match (a click in the text), and the old position would then be wrong.
fn match_status(app: &WordApp) -> String {
    let find = &app.session.find;
    let count = find.results.len();
    if count == 0 {
        return "No matches".to_string();
    }
    let (start, end) = app.session.sel.ordered();
    let is_selected = |index: usize| find.results.get(index).is_some_and(|(a, b)| *a == start && *b == end);
    let position = if is_selected(find.current) { Some(find.current) } else { (0..count).find(|&index| is_selected(index)) };
    match position {
        Some(index) => format!("{} of {count}", index.saturating_add(1)),
        None if count == 1 => "1 match".to_string(),
        None => format!("{count} matches"),
    }
}

/// Where a search from the current selection begins: the selection's start in the body. The engine
/// searches only the story the caret is in, so a caret anywhere else would otherwise confine (or
/// empty) the search; it starts at the top of the body instead.
fn body_anchor(app: &WordApp) -> Pos {
    let start = app.session.sel.ordered().0;
    if start.story == StoryRef::Body { start } else { Pos::body(0, 0) }
}

/// Find `query` and select the match; the canvas scrolls to it. An empty query clears the search and
/// the highlight (the selected match). The selection is the editor's own, never reported to the host.
pub fn run_find(app: &mut WordApp, query: &str, step: FindStep) {
    let before = app.session.sel.clone();
    let mut anchor = app.host.as_mut().and_then(|host| host.find_anchor.take());
    let moved = find(app, &mut anchor, query, false, step);
    if let Some(host) = app.host.as_mut() {
        host.find_anchor = anchor;
        if moved {
            host.page_target = None;
        }
    }
    // Escape, or a step that found no match, leaves a person's selection as it was.
    if app.session.sel != before {
        note_programmatic_selection(app);
    }
}

/// The host's `find` that selects its match (`host` is out of the app while host commands run): the
/// view bar's find with the host's case option, and a person's selection kept when nothing matches
/// (the bar's typing parks the caret at the search's start first; a host asking about text that is
/// not there has not asked for that).
pub(crate) fn run_host_find(app: &mut WordApp, host: &mut HostState, query: &str, match_case: bool, step: FindStep) {
    let before = app.session.sel.clone();
    let caret_scroll = app.canvas.scroll_to_caret;
    let moved = find(app, &mut host.find_anchor, query, match_case, step);
    if moved {
        host.page_target = None;
    }
    if !query.is_empty() && app.session.find.results.is_empty() {
        app.session.sel = before.clone();
        app.canvas.scroll_to_caret = caret_scroll;
    }
    if app.session.sel != before {
        host.note_programmatic_selection(&app.session.sel);
    }
}

/// The host's `find` with `select: false`: the search runs into `session.find`, and `current` is
/// the match the same step would go to from the person's selection, but the selection, the scroll
/// and the bar's text are left as they are. The engine's search runs on the session with the
/// selection moved to the search's start only while it runs.
pub(crate) fn count_matches(app: &mut WordApp, query: &str, match_case: bool, step: FindStep) {
    if query.is_empty() {
        app.session.find.results.clear();
        app.session.find.current = 0;
        return;
    }
    let saved = app.session.sel.clone();
    // The engine searches the story the caret is in; anything but the body starts at its top.
    let (start, end) = if saved.ordered().0.story == StoryRef::Body { saved.ordered() } else { (Pos::body(0, 0), Pos::body(0, 0)) };
    app.session.sel = Selection::caret(start.clone());
    let found = app.session.run("edit.find", &json!({"text": query, "matchCase": match_case, "wholeWord": false, "regex": false}));
    app.session.sel = saved;
    if found.is_err() {
        app.session.find.results.clear();
        app.session.find.current = 0;
        return;
    }
    let results = &app.session.find.results;
    // The engine's own choices (`edit.find`, `edit.findNext`, `edit.findPrevious`), made from the
    // person's selection instead of the one it moved.
    let current = match step {
        FindStep::First => results.iter().position(|(from, _)| *from >= start).unwrap_or(0),
        FindStep::Next => results.iter().position(|(from, _)| *from >= end).unwrap_or(0),
        FindStep::Previous => results.iter().rposition(|(_, to)| *to <= start).unwrap_or(results.len().saturating_sub(1)),
    };
    app.session.find.current = current;
}

/// The selection is the editor's doing (find, the page buttons): see
/// [`HostState::note_programmatic_selection`](super::state::HostState::note_programmatic_selection).
fn note_programmatic_selection(app: &mut WordApp) {
    if let Some(host) = app.host.as_mut() {
        host.note_programmatic_selection(&app.session.sel);
    }
}

/// Runs a find step; `true` when the view went to a match (the page a jump made current no longer is).
/// `find_anchor` is where the search began, which typing refines from.
fn find(app: &mut WordApp, find_anchor: &mut Option<Pos>, query: &str, match_case: bool, step: FindStep) -> bool {
    if query.is_empty() {
        let (start, end) = app.session.sel.ordered();
        let was_match = app.session.find.results.iter().any(|(a, b)| *a == start && *b == end);
        app.session.find.results.clear();
        app.session.find.current = 0;
        if was_match {
            app.session.sel = Selection::caret(start);
        }
        *find_anchor = None;
        return false;
    }
    match step {
        FindStep::First => {
            // Each keystroke searches again from where the search began: the engine takes the first
            // match at or after the caret, and the last keystroke left the caret on its match, so
            // a longer query would otherwise skip to a later match.
            let anchor = find_anchor.clone().unwrap_or_else(|| body_anchor(app));
            *find_anchor = Some(anchor.clone());
            app.session.sel = Selection::caret(anchor);
            // The options are set every time: a host's earlier regex search must not make typed text a pattern.
            app.run("edit.find", json!({"text": query, "matchCase": match_case, "wholeWord": false, "regex": false})).is_ok()
        }
        FindStep::Next | FindStep::Previous => {
            let command = if step == FindStep::Next { "edit.findNext" } else { "edit.findPrevious" };
            if app.run(command, json!({})).is_err() {
                return false;
            }
            // Typing on refines the search from the match just stepped to.
            *find_anchor = Some(body_anchor(app));
            true
        }
    }
}

/// Show page `index` (0-based) at the top of the view, with the caret on its first line. The caret
/// is the editor's own selection, never reported to the host.
pub fn go_to_page(app: &mut WordApp, index: usize) {
    let pages = app.session.layout().pages.len();
    if pages == 0 {
        return;
    }
    // Never past the last page: the target is reported to the host as the current page.
    let index = index.min(pages - 1);
    let _ = app.session.run("edit.goto", &json!({"page": index.saturating_add(1)}));
    app.canvas.scroll_to_doc = Some((index, 0.0));
    if let Some(host) = app.host.as_mut() {
        host.jump_to(index);
    }
    note_programmatic_selection(app);
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
    // The icon's own colour throughout: the host's accent is not checked against the bar's background.
    icons::paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), icon, color, color);
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

//! embed: edit mode of the embed build. The host page switches the open document to edit mode with
//! an author, a revision date and permissions; every change is then a tracked change in that
//! author's name and date (the engine's edit session, `Session::begin_edit`), Tab types a tab,
//! comments go to the host, and a save hands the file's bytes to the host instead of downloading
//! it. The view profile's chrome stays: the document, the view bar and an edit strip.

use egui::Ui;
use serde_json::{Value, json};
use wordcraft_engine::Selection;
use wordcraft_engine::edit_mode::{self, EditRefusal, EditRules, Refusal};

use super::bridge::{EditorMode, HostEvent};
use super::state::{HostState, Reading, read_selection};
use super::wire::{ModeRequest, PermissionsSpec, revision_date_seconds};
use crate::WordApp;
use crate::theme::{Tokens, regular};

pub(crate) struct EditSession {
    pub(crate) permissions: PermissionsSpec,
    /// What the engine's session was begun with: the same request again is not a new entry.
    author: String,
    date: String,
    rules: EditRules,
    /// The document revision last seen: a change drops a jump's page target (`follow_typing`).
    rev: u64,
    /// The last refusal and where the caret was then: shown until the document changes or the caret
    /// moves.
    notice: Option<(Refusal, Selection)>,
}

fn mode_in_force(host: &HostState) -> EditorMode {
    if host.edit.is_some() { EditorMode::Edit } else { EditorMode::View }
}

pub(crate) fn set_mode(app: &mut WordApp, host: &mut HostState, request: ModeRequest) {
    let ModeRequest::Edit { author, revision_date, permissions } = request else {
        leave(app, host);
        host.bridge.emit(HostEvent::ModeChanged { mode: EditorMode::View });
        return;
    };
    // `parse_mode_request` refuses a bad date before it is queued; one queued another way still never
    // starts a session.
    let date = revision_date_seconds(&revision_date);
    let (Some(date), true) = (date, host.file_open) else {
        leave(app, host);
        host.bridge.emit(HostEvent::ModeChanged { mode: EditorMode::View });
        return;
    };
    // One date and one author per save: unsaved changes carry the date and the author they were made
    // with, so another date or author waits until the document is opened again (embed API 0.2,
    // P6-18, Ruling W3-5). The mode in force stays.
    let other_date = host.session_date.as_deref().is_some_and(|made| made != date);
    let other_author = host.session_author.as_ref().is_some_and(|(id, name)| *id != author.id || *name != author.name);
    if is_dirty(app, host) && (other_date || other_author) {
        host.bridge.emit(HostEvent::EditRefused { code: Refusal::InvalidEditSession.code().to_string() });
        host.bridge.emit(HostEvent::ModeChanged { mode: mode_in_force(host) });
        return;
    }
    let rules = EditRules { may_decide: permissions.accept_reject_changes, full: permissions.full_editing };
    let running = host.edit.as_mut().filter(|edit| edit.author == author.name && edit.date == date && edit.rules == rules);
    if let Some(edit) = running {
        // The session running: not a new entry, so its history and its own changes stay. The other
        // editor's permissions may have changed; this editor reads none of them.
        edit.permissions = permissions;
        // Only reached with the same id while dirty (another is refused above).
        host.session_author = Some((author.id, author.name));
    } else {
        if let Err(error) = app.session.begin_edit(&author.name, &date, rules) {
            log::warn!("embed: {error}");
            leave(app, host);
            host.bridge.emit(HostEvent::ModeChanged { mode: EditorMode::View });
            return;
        }
        if host.edit.is_none() {
            // A selection the host was told of goes first (embed API 0.2, "Entering edit mode").
            host.clear_reported_selection();
        }
        host.session_date = Some(date.clone());
        host.session_author = Some((author.id.clone(), author.name.clone()));
        host.edit = Some(EditSession { permissions, author: author.name, date, rules, rev: app.session.rev(), notice: None });
    }
    host.bridge.emit(HostEvent::ModeChanged { mode: EditorMode::Edit });
}

fn leave(app: &mut WordApp, host: &mut HostState) {
    if host.edit.take().is_some() {
        // The selection made while editing is reported only once the person changes it.
        host.settle_quietly(&app.session.sel);
    }
    app.session.end_edit();
    app.session.view_only = true;
}

/// Whether the document differs from what was opened: its content version, not the undo depth,
/// which the undo limit and a new session's start both cut short.
fn is_dirty(app: &WordApp, host: &HostState) -> bool {
    app.session.content_version() != host.opened_version
}

/// A document always opens in view mode; the host hears so first when it was editing. Every open
/// runs this, a refused one (`OpenTooLarge`) included (embed API 0.2, "Opening in edit mode").
pub(crate) fn before_open(app: &mut WordApp, host: &mut HostState) {
    let was_editing = host.edit.is_some();
    leave(app, host);
    if was_editing {
        host.bridge.emit(HostEvent::ModeChanged { mode: EditorMode::View });
    }
}

/// Edit mode's gate in front of `WordApp::run`; `Some` answers without running the command.
pub(crate) fn gate(app: &mut WordApp, id: &str, params: &Value, rules: EditRules) -> Option<Result<Value, String>> {
    match id {
        // The editor's own save shortcut: the host decides what a save becomes. Answered now, with
        // the document as it is now.
        "file.save" => {
            if let Some(mut host) = app.host.take() {
                save(app, &mut host);
                app.host = Some(host);
            }
            Some(Ok(json!({})))
        }
        // Comments are the host's discussions, never part of the file.
        "review.newComment" => {
            if let Some(host) = app.host.as_mut() {
                host.comment_asked = true;
            }
            Some(Ok(json!({})))
        }
        // Full editing: everything the ribbon offers, but no file menu, opening or saving of the
        // application's own: the host owns the file.
        _ if rules.full
            && (id == "ui.backstage" || id.starts_with("file.") || (id == "ui.tab" && params.get("tab").and_then(Value::as_str) == Some("File"))) =>
        {
            Some(Err(refuse(app, id, Refusal::CommandNotAllowed)))
        }
        _ if rules.full => None,
        // Ctrl/⌘+V: rich paste would bring formatting nobody tracked.
        "edit.paste" => Some(app.run("edit.pasteText", params.clone())),
        // Shift+Tab does nothing in edit mode (embed API 0.2, "Keyboard in edit mode"): no list
        // level or indent, and nothing to tell either.
        "text.backTab" => Some(Ok(json!({}))),
        // The engine's session runs the rest, Tab as a tracked tab (`edit_mode::TAB`), and refuses
        // with a code what it does not allow.
        _ if edit_mode::allows(id, rules) => None,
        _ => {
            // Not the engine's: the application's own commands (dialogs, panes, Backstage).
            let refusal = if id.starts_with("ui.") || app.session.registry.get(id).is_some() {
                edit_mode::refusal_for(id, rules)
            } else {
                Refusal::UnknownCommand
            };
            Some(Err(refuse(app, id, refusal)))
        }
    }
}

/// A refusal at the host's gate, recorded where the engine records its own (`Session::refusal`).
fn refuse(app: &mut WordApp, id: &str, refusal: Refusal) -> String {
    let message = EditRefusal::new(refusal, id).to_string();
    app.session.refusal = Some(refusal);
    app.session.status.clone_from(&message);
    note_refusal(app);
    message
}

/// After a command `WordApp::run` refused: the host hears the refusal's code once (`editRefused`)
/// and the editor shows it too. Searches and moves that find nothing are no refusal
/// (`edit_mode::NAVIGATION`), so they leave `Session::refusal` empty and say nothing.
pub(crate) fn note_refusal(app: &mut WordApp) {
    let Some(refusal) = app.session.refusal else { return };
    let at = app.session.sel.clone();
    let Some(host) = app.host.as_mut() else { return };
    let Some(edit) = host.edit.as_mut() else { return };
    edit.notice = Some((refusal, at));
    host.bridge.emit(HostEvent::EditRefused { code: refusal.code().to_string() });
}

/// What the notice says for a refusal: the person's words for each code.
fn notice_text(refusal: Refusal) -> &'static str {
    match refusal {
        Refusal::ParagraphBreakNotSupported => "New paragraphs cannot be added while editing yet. Shift+Enter adds a line break.",
        Refusal::DecisionNotAllowed => "Accepting and rejecting changes is not available to you.",
        Refusal::CharacterNotAllowed => "That character cannot be typed while editing.",
        Refusal::OutsideEditableStory => "Comments and text boxes cannot be edited here.",
        Refusal::DocumentProtected => "The document is protected against this change.",
        Refusal::NotAvailable => "That does not apply here.",
        Refusal::CommandNotAllowed | Refusal::UnknownCommand => "Only text changes can be made while editing; they are tracked.",
        Refusal::InvalidParameters | Refusal::CommandFailed | Refusal::InvalidEditSession => "That change could not be made.",
    }
}

/// Before the frame's events: a change to the document (typing moves the caret and may scroll)
/// drops the page a jump made current, so `pageChanged` follows the caret. A change or a caret move
/// ends the refusal shown.
pub(crate) fn follow_typing(app: &WordApp, host: &mut HostState) {
    let rev = app.session.rev();
    let Some(edit) = host.edit.as_mut() else { return };
    if edit.rev != rev {
        edit.rev = rev;
        edit.notice = None;
        host.page_target = None;
    }
    if edit.notice.as_ref().is_some_and(|(_, at)| *at != app.session.sel) {
        edit.notice = None;
    }
}

/// After the frame: a comment asked for, and the dirty state.
pub(crate) fn after_frame(app: &mut WordApp, host: &mut HostState, ctx: &egui::Context) {
    // A selection whose page is not drawn yet answers next frame; anything else ends the request.
    if host.comment_asked {
        if host.edit.is_none() {
            host.comment_asked = false;
        } else {
            match read_selection(app, ctx.zoom_factor()) {
                Reading::Text(HostEvent::Selection { text, copy_text, page, x, y }) => {
                    host.comment_asked = false;
                    host.bridge.emit(HostEvent::CommentRequested { text, copy_text, page, x, y });
                }
                Reading::NotReady => {}
                _ => host.comment_asked = false,
            }
        }
    }
    // Undo back to the opened document makes it clean again.
    if host.edit.is_some() {
        let dirty = is_dirty(app, host);
        if host.dirty != dirty {
            host.dirty = dirty;
            host.bridge.emit(HostEvent::DirtyChanged { dirty });
        }
    }
}

/// Answer a save with the document and the mode as they are at the request: a later command of the
/// same frame (an open, a mode change) must not change what this save holds. Two requests in one
/// frame for the same document in the same mode answer once; any other is answered on its own.
pub(crate) fn save(app: &mut WordApp, host: &mut HostState) {
    let request = (host.frame, app.session.rev(), host.edit.is_some());
    if host.last_save == Some(request) {
        return;
    }
    host.last_save = Some(request);
    if host.edit.is_none() {
        host.bridge.emit(HostEvent::SaveFailed { message: "Nothing to save: the document is open for viewing only.".into() });
        return;
    }
    let file_name = if host.file_name.is_empty() { "document.docx".to_string() } else { host.file_name.clone() };
    // embed: the opened package is kept and only the paragraphs this session changed are rewritten,
    // as tracked changes by the person's name and the host's date (embed API 0.2), with every other
    // part, relationship and content type as the file had them: the host's save check refuses
    // anything a regenerated package would lose. Word's "remove personal information" and "remove
    // date and time" settings (upstream #1062) are the file's own bytes here and change nothing a
    // tracked change says. A change that cannot be written that way is refused with its reason.
    match wordcraft_engine::io::save_docx_preserving(&app.session.doc) {
        Ok(bytes) => host.bridge.emit(HostEvent::SaveRequested { file_name, bytes }),
        Err(message) => {
            log::warn!("embed: an edited document could not be written: {message}");
            host.bridge.emit(HostEvent::SaveFailed { message });
        }
    }
}

fn command_button(app: &mut WordApp, ui: &mut Ui, label: &str, command: &str, enabled: bool) {
    if ui.add_enabled(enabled, egui::Button::new(label)).clicked()
        && let Err(message) = app.run(command, json!({}))
    {
        log::warn!("embed: {message}");
    }
}

/// The strip's buttons: (label, command), in order, with a separator before each group.
fn strip_groups(may_decide: bool) -> Vec<Vec<(&'static str, &'static str)>> {
    let mut groups = vec![
        vec![("Undo", "edit.undo"), ("Redo", "edit.redo")],
        vec![("Previous change", "review.previousChange"), ("Next change", "review.nextChange")],
    ];
    if may_decide {
        groups.push(vec![
            ("Accept", "review.accept"),
            ("Reject", "review.reject"),
            ("Accept all", "review.acceptAll"),
            ("Reject all", "review.rejectAll"),
        ]);
    }
    groups
}

fn strip_permissions(app: &WordApp) -> Option<PermissionsSpec> {
    app.host.as_ref().and_then(|host| host.edit.as_ref()).map(|edit| edit.permissions).filter(|permissions| !permissions.full_editing)
}

/// Whether the host page asked for full editing: the full ribbon shows instead of the view bar.
pub(crate) fn full_editing(app: &WordApp) -> bool {
    app.session.edit_rules().is_some_and(|rules| rules.full)
}

/// Sections as the person counts them (1-based), runs joined: `[1, 2, 3, 5]` is "2–4, 6".
fn section_list(sections: &[usize]) -> String {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for &section in sections {
        match runs.last_mut() {
            Some((_, end)) if end.checked_add(1) == Some(section) => *end = section,
            _ => runs.push((section, section)),
        }
    }
    let number = |section: usize| section.saturating_add(1);
    runs.iter()
        .map(|(start, end)| if start == end { number(*start).to_string() } else { format!("{}–{}", number(*start), number(*end)) })
        .collect::<Vec<_>>()
        .join(", ")
}

/// While the caret is in a header or footer that several sections show (later ones inherit it),
/// editing it edits them all: the strip says so, "Header (shared with sections 1–3)".
fn shared_part_label(app: &mut WordApp) -> Option<String> {
    let wordcraft_engine::doc::StoryRef::Part(id) = app.session.sel.focus.story else { return None };
    let name = match app.session.doc.parts.get(&id)?.kind {
        wordcraft_engine::doc::PartKind::Header => "Header",
        wordcraft_engine::doc::PartKind::Footer => "Footer",
        _ => return None,
    };
    let sections = edit_mode::sections_showing(&app.session.layout(), id);
    (sections.len() > 1).then(|| format!("{name} (shared with sections {})", section_list(&sections)))
}

fn part_label_text(text: String) -> egui::RichText {
    egui::RichText::new(text).font(regular(12.0))
}

/// The width the strip takes in `ui`, separators included (0 outside edit mode).
pub(crate) fn strip_width(app: &mut WordApp, ui: &Ui) -> f32 {
    let Some(permissions) = strip_permissions(app) else { return 0.0 };
    let label = shared_part_label(app);
    let spacing = ui.spacing();
    let font = egui::TextStyle::Button.resolve(ui.style());
    let mut width = match label {
        Some(text) => ui.painter().layout_no_wrap(text, regular(12.0), egui::Color32::PLACEHOLDER).size().x + spacing.item_spacing.x,
        None => 0.0,
    };
    for group in strip_groups(permissions.accept_reject_changes) {
        // A separator: its own spacing and the item spacing on both sides.
        width += 6.0 + spacing.item_spacing.x * 2.0;
        for (label, _) in group {
            let text = ui.painter().layout_no_wrap(label.to_string(), font.clone(), egui::Color32::PLACEHOLDER).size().x;
            width += text + spacing.button_padding.x * 2.0 + spacing.item_spacing.x;
        }
    }
    width
}

/// In the view bar while editing: undo, redo, moving through changes, and deciding them when allowed.
/// `leading` puts a separator before the first group (the strip follows the zoom controls).
pub(crate) fn strip(app: &mut WordApp, ui: &mut Ui, leading: bool) {
    let Some(permissions) = strip_permissions(app) else { return };
    let (can_undo, can_redo) = (app.session.can_undo(), app.session.can_redo());
    if leading {
        ui.separator();
    }
    if let Some(text) = shared_part_label(app) {
        ui.label(part_label_text(text));
    }
    for (index, group) in strip_groups(permissions.accept_reject_changes).into_iter().enumerate() {
        if index > 0 {
            ui.separator();
        }
        for (label, command) in group {
            let enabled = match command {
                "edit.undo" => can_undo,
                "edit.redo" => can_redo,
                _ => true,
            };
            command_button(app, ui, label, command, enabled);
        }
    }
}

/// Points between the notice and the document view's edge.
const NOTICE_MARGIN: f32 = 8.0;
/// The notice's height before it was first laid out.
const NOTICE_HEIGHT: f32 = 32.0;
/// Where the scroll bar runs along the view's bottom edge.
const SCROLL_BAR: f32 = 20.0;

/// The caret line's top and bottom on screen, if it is on a drawn page.
fn caret_band(app: &mut WordApp) -> Option<(f32, f32)> {
    let layout = app.session.layout();
    let caret = layout.caret_on(&app.session.sel.focus, app.session.page_hint)?;
    let top = crate::canvas::page_to_screen(app, caret.page, caret.x, caret.top)?.y;
    let bottom = crate::canvas::page_to_screen(app, caret.page, caret.x, caret.top + caret.height)?.y;
    Some((top, bottom))
}

/// Over the document while editing: why the last thing asked of the editor was refused (Enter, a
/// formatting shortcut), until the document changes or the caret moves. An overlay, so showing it
/// moves nothing and the document keeps the keyboard; at the view's top, or at its bottom when the
/// caret's line is under the top.
pub(crate) fn notice(app: &mut WordApp, ctx: &egui::Context) {
    let Some(refusal) = app.host.as_ref().and_then(|host| host.edit.as_ref()).and_then(|edit| edit.notice.as_ref()).map(|(refusal, _)| *refusal)
    else {
        return;
    };
    let Some(view) = app.canvas.canvas_rect else { return };
    let id = egui::Id::new("embed_edit_notice");
    let height = ctx.memory(|memory| memory.area_rect(id)).map_or(NOTICE_HEIGHT, |rect| rect.height());
    let top_band = (view.top(), view.top() + NOTICE_MARGIN * 2.0 + height);
    let covered = caret_band(app).is_some_and(|(top, bottom)| bottom >= top_band.0 && top <= top_band.1);
    let (pivot, at) = if covered {
        (egui::Align2::CENTER_BOTTOM, view.center_bottom() - egui::vec2(0.0, NOTICE_MARGIN + SCROLL_BAR))
    } else {
        (egui::Align2::CENTER_TOP, view.center_top() + egui::vec2(0.0, NOTICE_MARGIN))
    };
    let t = Tokens::get(ctx);
    egui::Area::new(id).order(egui::Order::Foreground).interactable(false).pivot(pivot).fixed_pos(at).show(ctx, |ui| {
        egui::Frame::NONE
            .fill(t.ribbon)
            .stroke(egui::Stroke::new(1.0, t.border_strong))
            .corner_radius(4.0)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.label(egui::RichText::new(notice_text(refusal)).font(regular(12.0)).color(t.text));
            });
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn section_lists_are_one_based_and_join_runs() {
        assert_eq!(super::section_list(&[0, 1]), "1–2");
        assert_eq!(super::section_list(&[1, 2, 3, 5]), "2–4, 6");
        assert_eq!(super::section_list(&[0, 2]), "1, 3");
        assert_eq!(super::section_list(&[usize::MAX]), usize::MAX.to_string());
    }

    /// D21: the comment request needs the selection's page to have been drawn (the host places its
    /// box from the selection's end). Text added after the canvas drew, in the same frame, leaves a
    /// selection on a page the canvas has not laid out: the request waits for the next frame and
    /// is then answered once, instead of being lost or sent with no place.
    #[test]
    fn a_comment_request_waits_for_the_selection_s_page_to_be_drawn() {
        use std::cell::Cell;
        use std::rc::Rc;

        use serde_json::json;
        use wordcraft_doc::Document;
        use wordcraft_engine::Session;

        use crate::host::bridge::HostBridge;
        use crate::host::wire::{AuthorSpec, ModeRequest, PermissionsSpec};
        use crate::host::{EmbedProfile, HostCommand, HostEvent};
        use crate::{Services, WordApp};

        let bridge = HostBridge::new();
        let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(bridge.clone(), EmbedProfile::View);
        // On: a frame whose canvas drew first, then the document grew and the comment was asked.
        let grow = Rc::new(Cell::new(false));
        let growing = grow.clone();
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 800.0)).with_max_steps(12).build_ui_state(
            move |ui, app: &mut WordApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                if growing.get() {
                    crate::canvas::show(app, ui);
                    app.run("text.insert", json!({ "text": "Rent is due monthly. ".repeat(1500) })).expect("typing");
                    app.run("select.all", json!({})).expect("select all");
                    app.run("review.newComment", json!({ "text": "" })).expect("comment");
                    assert!(app.session.layout().pages.len() > app.canvas.page_rects.len(), "the text made pages the canvas has not drawn");
                    super::super::state::after_frame(app, &ctx);
                } else {
                    app.ui(ui);
                }
            },
            app,
        );
        bridge.set_context(harness.ctx.clone());
        crate::host::prepare_context(&harness.ctx);
        harness.run_steps(2);
        let bytes = wordcraft_engine::io::save_bytes("lease.docx", &Document::from_text("Rent is due monthly.")).expect("writes");
        bridge.send(HostCommand::Open { name: "lease.docx".into(), bytes, options: Default::default() });
        harness.run_steps(3);
        let permissions = PermissionsSpec {
            accept_reject_changes: false,
            organize_pages: true,
            fill_forms: true,
            redact: true,
            stamps: true,
            drawing: true,
            full_editing: false,
        };
        bridge.send(HostCommand::SetMode(ModeRequest::Edit {
            author: AuthorSpec { id: "u-sari".into(), name: "Sari Wulan".into() },
            revision_date: "2026-10-10T08:00:00Z".into(),
            permissions,
        }));
        harness.run_steps(2);
        bridge.take_events();

        grow.set(true);
        harness.step();
        grow.set(false);
        let early = bridge.take_events();
        assert!(!early.iter().any(|event| matches!(event, HostEvent::CommentRequested { .. })), "the page was not drawn yet: {early:?}");
        harness.run_steps(3);
        let events = bridge.take_events();
        let requests: Vec<(&String, &Option<u32>, f32, f32)> = events
            .iter()
            .filter_map(|event| match event {
                HostEvent::CommentRequested { text, page, x, y, .. } => Some((text, page, *x, *y)),
                _ => None,
            })
            .collect();
        assert_eq!(requests.len(), 1, "{events:?}");
        let (text, page, x, y) = requests[0];
        assert!(text.starts_with("Rent is due monthly. Rent is due monthly."), "{text}");
        assert!(page.is_some_and(|number| number >= 1) && x.is_finite() && y.is_finite() && x > 0.0 && y > 0.0, "{requests:?}");
        // Once, not on every later frame; and nothing was written into the file.
        harness.run_steps(3);
        assert!(bridge.take_events().iter().all(|event| !matches!(event, HostEvent::CommentRequested { .. })));
        assert!(harness.state().session.doc.comments.is_empty());
    }
}

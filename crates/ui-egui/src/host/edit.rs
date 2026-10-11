//! embed: edit mode of the embed build. The host page switches the open document to edit mode with
//! an author, a revision date and permissions; every change is then a tracked change in that
//! author's name and date (the engine's edit session, `Session::begin_edit`), Tab types a tab,
//! comments go to the host, and a save hands the file's bytes to the host instead of downloading
//! it. The view profile's chrome stays: the document, the view bar and an edit strip.

use egui::Ui;
use serde_json::{Value, json};
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
    /// The last refusal, shown in the strip until the document next changes.
    notice: Option<Refusal>,
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
    let rules = EditRules { may_decide: permissions.accept_reject_changes };
    let running = host.edit.as_mut().filter(|edit| edit.author == author.name && edit.date == date && edit.rules == rules);
    if let Some(edit) = running {
        // The session running: not a new entry, so its history and its own changes stay. The other
        // editor's permissions may have changed; this editor reads none of them.
        edit.permissions = permissions;
    } else {
        // A new session clears undo history: what the one running changed is still a change.
        host.edited |= host.edit.is_some() && app.session.can_undo();
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
        host.edit = Some(EditSession { permissions, author: author.name, date, rules, rev: app.session.rev(), notice: None });
    }
    host.bridge.emit(HostEvent::ModeChanged { mode: EditorMode::Edit });
}

fn leave(app: &mut WordApp, host: &mut HostState) {
    if host.edit.take().is_some() {
        host.edited |= app.session.can_undo();
        // The selection made while editing is reported only once the person changes it.
        host.settle_quietly(&app.session.sel);
    }
    app.session.end_edit();
    app.session.view_only = true;
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
        // The editor's own save shortcut: the host decides what a save becomes.
        "file.save" => {
            if let Some(host) = app.host.as_mut() {
                host.save_asked = true;
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
        // Ctrl/⌘+V: rich paste would bring formatting nobody tracked.
        "edit.paste" => Some(app.run("edit.pasteText", params.clone())),
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

/// After a command `WordApp::run` refused: the strip shows an edit session's refusal.
pub(crate) fn note_refusal(app: &mut WordApp) {
    let refusal = app.session.refusal;
    if let Some(edit) = app.host.as_mut().and_then(|host| host.edit.as_mut())
        && refusal.is_some()
    {
        edit.notice = refusal;
    }
}

/// What the strip says for a refusal: the person's words for each code.
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
/// drops the page a jump made current, so `pageChanged` follows the caret; and the refusal shown
/// since is over.
pub(crate) fn follow_typing(app: &WordApp, host: &mut HostState) {
    let rev = app.session.rev();
    if let Some(edit) = host.edit.as_mut()
        && edit.rev != rev
    {
        edit.rev = rev;
        edit.notice = None;
        host.page_target = None;
    }
}

/// After the frame: a save or a comment asked for, and the dirty state.
pub(crate) fn after_frame(app: &mut WordApp, host: &mut HostState, ctx: &egui::Context) {
    if std::mem::take(&mut host.save_asked) {
        save(app, host);
    }
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
    // Undo back to the start of the first session leaves nothing to save; a later session's start
    // may not be the opened document (`HostState::edited`).
    if host.edit.is_some() {
        let dirty = host.edited || app.session.can_undo();
        if host.dirty != dirty {
            host.dirty = dirty;
            host.bridge.emit(HostEvent::DirtyChanged { dirty });
        }
    }
}

fn save(app: &mut WordApp, host: &mut HostState) {
    if host.edit.is_none() {
        host.bridge.emit(HostEvent::SaveFailed { message: "Nothing to save: the document is open for viewing only.".into() });
        return;
    }
    let file_name = if host.file_name.is_empty() { "document.docx".to_string() } else { host.file_name.clone() };
    // The writer is chosen by extension; the host's name is kept for the event.
    let target = if file_name.to_ascii_lowercase().ends_with(".docx") { file_name.clone() } else { format!("{file_name}.docx") };
    match wordcraft_engine::io::save_bytes(&target, &app.session.doc) {
        Ok(bytes) => host.bridge.emit(HostEvent::SaveRequested { file_name, bytes }),
        Err(message) => {
            log::warn!("embed: an edited document could not be written: {message}");
            host.bridge.emit(HostEvent::SaveFailed { message: "The document could not be written.".into() });
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

/// In the view bar while editing: undo, redo, moving through changes, and deciding them when allowed.
pub(crate) fn strip(app: &mut WordApp, ui: &mut Ui) {
    let Some(permissions) = app.host.as_ref().and_then(|host| host.edit.as_ref()).map(|edit| edit.permissions) else { return };
    ui.separator();
    let (can_undo, can_redo) = (app.session.can_undo(), app.session.can_redo());
    command_button(app, ui, "Undo", "edit.undo", can_undo);
    command_button(app, ui, "Redo", "edit.redo", can_redo);
    ui.separator();
    command_button(app, ui, "Previous change", "review.previousChange", true);
    command_button(app, ui, "Next change", "review.nextChange", true);
    if permissions.accept_reject_changes {
        ui.separator();
        command_button(app, ui, "Accept", "review.accept", true);
        command_button(app, ui, "Reject", "review.reject", true);
        command_button(app, ui, "Accept all", "review.acceptAll", true);
        command_button(app, ui, "Reject all", "review.rejectAll", true);
    }
}

/// Over the top of the document while editing: why the last thing asked of the editor was refused
/// (Enter, a formatting shortcut), until the document next changes. Nothing is refused silently. An
/// overlay, so showing it moves nothing and the document keeps the keyboard.
pub(crate) fn notice(app: &WordApp, ctx: &egui::Context) {
    let Some(refusal) = app.host.as_ref().and_then(|host| host.edit.as_ref()).and_then(|edit| edit.notice) else { return };
    let Some(view) = app.canvas.canvas_rect else { return };
    let t = Tokens::get(ctx);
    egui::Area::new(egui::Id::new("embed_edit_notice"))
        .order(egui::Order::Foreground)
        .interactable(false)
        .pivot(egui::Align2::CENTER_TOP)
        .fixed_pos(view.center_top() + egui::vec2(0.0, 8.0))
        .show(ctx, |ui| {
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

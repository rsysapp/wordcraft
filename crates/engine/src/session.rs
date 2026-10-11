//! The editing session: document, selection, undo/redo, layout, view state.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wordcraft_doc::edit::Fragment;
use wordcraft_doc::props::CharProps;
use wordcraft_doc::{Document, Path, Pos, StoryRef};
use wordcraft_layout::{DocLayout, LayoutCache, LayoutOptions, ViewMode};

use crate::{CmdError, Registry};

/// A selection: anchor (where it started) and focus (where the caret is).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Selection {
    pub anchor: Pos,
    pub focus: Pos,
}

impl Selection {
    pub fn caret(p: Pos) -> Self {
        Selection { anchor: p.clone(), focus: p }
    }
    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.focus
    }
    /// (start, end) in document order.
    pub fn ordered(&self) -> (Pos, Pos) {
        if self.anchor <= self.focus { (self.anchor.clone(), self.focus.clone()) } else { (self.focus.clone(), self.anchor.clone()) }
    }
}

/// View state that commands can change (ribbon View tab, status bar).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewState {
    pub zoom: f32,
    pub mode: ViewMode,
    pub read_mode: bool,
    pub focus_mode: bool,
    pub marks: bool,
    pub ruler: bool,
    pub gridlines: bool,
    pub nav_pane: bool,
    pub styles_pane: bool,
    pub comments_pane: bool,
    pub multi_page: bool,
    /// Zoom to fit: "pageWidth", "onePage", "multiplePages", or empty.
    pub fit: String,
    pub web_width: f32,
    pub dark_mode: bool,
    pub show_markup: bool,
    pub track_changes_pane: bool,
    /// Check spelling and grammar as you type.
    pub proofing: bool,
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState {
            zoom: 1.0,
            mode: ViewMode::Print,
            read_mode: false,
            focus_mode: false,
            marks: false,
            ruler: true,
            gridlines: false,
            nav_pane: false,
            styles_pane: false,
            comments_pane: false,
            multi_page: false,
            fit: String::new(),
            web_width: 800.0,
            dark_mode: false,
            show_markup: true,
            track_changes_pane: false,
            proofing: true,
        }
    }
}

/// Find/replace state.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FindState {
    pub query: String,
    pub replace: String,
    pub match_case: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// Matches from the last search: (start, end).
    #[serde(skip)]
    pub results: Vec<(Pos, Pos)>,
    pub current: usize,
}

struct Undo {
    label: String,
    doc: Document,
    sel: Selection,
    /// embed: the step accepts or rejects tracked changes; an edit session redoes it only while it
    /// may decide them.
    decision: bool,
}

/// An editing session.
pub struct Session {
    pub doc: Document,
    pub sel: Selection,
    pub view: ViewState,
    /// Formatting picked with a collapsed caret, applied to the next typed text.
    pub pending: Option<CharProps>,
    pub path: Option<std::path::PathBuf>,
    pub dirty: bool,
    pub clipboard: Option<Fragment>,
    /// Plain text mirror of the clipboard (for the system clipboard).
    pub clipboard_text: String,
    pub find: FindState,
    /// Page x the caret tries to keep on Up/Down.
    pub goal_x: Option<f32>,
    /// Page the caret was last on (headers/footers repeat on many pages).
    pub page_hint: usize,
    pub registry: Arc<Registry>,
    /// Author name for comments and tracked changes.
    pub author: String,
    /// Format painter: copied formatting waiting to be applied (and whether it stays on).
    pub painter: Option<(CharProps, wordcraft_doc::props::ParaProps, bool)>,
    /// Last message for the status bar / agents.
    pub status: String,
    history: Vec<Undo>,
    redo: Vec<Undo>,
    /// Typing is coalesced into one undo step until something else happens.
    typing_open: bool,
    rev: u64,
    cache: LayoutCache,
    layout: Option<(u64, f32, ViewMode, Arc<DocLayout>, bool)>,
    /// Picture edits: edited media key → original media key (Reset Picture).
    pub originals: std::collections::HashMap<String, String>,
    /// Last mutating command (Repeat).
    pub last_command: Option<(String, Value)>,
    /// Macro being recorded: (name, steps).
    pub recording: Option<(String, Vec<(String, Value)>)>,
    pub macros: std::collections::BTreeMap<String, Vec<(String, Value)>>,
    pub autocorrect_on: bool,
    pub autocorrect_user: Vec<(String, String)>,
    /// Saved versions: (label, date, document).
    pub versions: Vec<(String, String, Document)>,
    /// Quick Parts / AutoText entries.
    pub building_blocks: std::collections::BTreeMap<String, Fragment>,
    pub autosave: bool,
    /// Citation style: APA, MLA, Chicago, IEEE.
    pub bib_style: String,
    /// Mail merge data source and preview.
    pub merge: crate::cmd::mailings::MergeState,
    /// Requests from commands to the UI (open a dialog, scroll…), drained by the front end.
    pub ui_requests: Vec<Value>,
    /// embed: a view-only session runs only `crate::view_only::COMMANDS`.
    pub view_only: bool,
    /// embed: an edit session runs only `crate::edit_mode` commands. Set only by `begin_edit`.
    pub(crate) edit_rules: Option<crate::edit_mode::EditRules>,
    /// embed: the date the host gives this edit session's tracked changes, instead of the clock.
    pub(crate) revision_date: Option<String>,
    /// embed: the number of revision records the document had when the edit session began. A
    /// record at or past it was made by this session: that, never an author name or a date, is
    /// what makes a change the session's own.
    pub(crate) edit_floor: usize,
    /// embed: why the last command was refused by an edit session (`None` when it was not).
    pub refusal: Option<crate::edit_mode::Refusal>,
    /// embed: the author `begin_edit` was given. `author` is put back to it before every command of
    /// the session, so nothing but `begin_edit` names the author of the session's changes.
    edit_author: String,
}

/// Maximum undo depth.
const MAX_UNDO: usize = 500;

impl Session {
    pub fn new(doc: Document) -> Self {
        let start = doc.start_of(StoryRef::Body);
        Session {
            doc,
            sel: Selection::caret(start),
            view: ViewState::default(),
            pending: None,
            path: None,
            dirty: false,
            clipboard: None,
            clipboard_text: String::new(),
            find: FindState::default(),
            goal_x: None,
            page_hint: 0,
            registry: Arc::new(crate::cmd::registry()),
            author: "WordCraft User".into(),
            painter: None,
            status: String::new(),
            history: Vec::new(),
            redo: Vec::new(),
            typing_open: false,
            rev: 1,
            cache: LayoutCache::new(),
            layout: None,
            originals: Default::default(),
            last_command: None,
            recording: None,
            macros: Default::default(),
            autocorrect_on: true,
            autocorrect_user: Vec::new(),
            versions: Vec::new(),
            building_blocks: Default::default(),
            autosave: true,
            bib_style: "APA".into(),
            merge: Default::default(),
            ui_requests: Vec::new(),
            view_only: false,
            edit_rules: None,
            revision_date: None,
            edit_floor: 0,
            refusal: None,
            edit_author: String::new(),
        }
    }

    /// Document revision (bumped by every change).
    pub fn rev(&self) -> u64 {
        self.rev
    }
    pub fn touch(&mut self) {
        self.rev = self.rev.wrapping_add(1);
        self.dirty = true;
    }

    /// The current layout (recomputed when the document or view changed).
    pub fn layout(&mut self) -> Arc<DocLayout> {
        let ww = self.view.web_width;
        if let Some((r, w, m, l, pf)) = &self.layout
            && *r == self.rev
            && (*w == ww || self.view.mode == ViewMode::Print)
            && *m == self.view.mode
            && *pf == self.view.proofing
        {
            return l.clone();
        }
        let opts = LayoutOptions { view: self.view.mode, web_width: ww, show_hidden: self.view.marks, proofing: self.view.proofing };
        let l = Arc::new(wordcraft_layout::layout(&self.doc, &mut self.cache, &opts));
        self.layout = Some((self.rev, ww, self.view.mode, l.clone(), self.view.proofing));
        l
    }
    /// A layout for output (PDF, images, print): no proofing marks, print view.
    pub fn export_layout(&self) -> Arc<DocLayout> {
        let opts = LayoutOptions { view: ViewMode::Print, web_width: 0.0, show_hidden: false, proofing: false };
        Arc::new(wordcraft_layout::layout(&self.doc, &mut LayoutCache::new(), &opts))
    }

    /// Invalidate the cached layout (fonts changed etc.).
    pub fn relayout(&mut self) {
        self.layout = None;
        self.cache.clear();
    }

    /// Snapshot for undo before a change.
    pub fn checkpoint(&mut self, label: &str) {
        if label == "Typing" && self.typing_open {
            return;
        }
        self.typing_open = label == "Typing";
        self.history.push(Undo { label: label.to_string(), doc: self.doc.clone(), sel: self.sel.clone(), decision: false });
        if self.history.len() > MAX_UNDO {
            self.history.remove(0);
        }
        self.redo.clear();
    }
    /// Close an open typing group (caret moved, other command).
    pub fn close_typing(&mut self) {
        self.typing_open = false;
    }
    pub fn can_undo(&self) -> bool {
        !self.history.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo_label(&self) -> Option<&str> {
        self.history.last().map(|u| u.label.as_str())
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|u| u.label.as_str())
    }
    pub fn undo_labels(&self) -> Vec<String> {
        self.history.iter().rev().map(|u| u.label.clone()).collect()
    }
    pub fn undo(&mut self) -> bool {
        // embed: history belongs to editing; a view-only session never restores other states.
        if self.view_only {
            return false;
        }
        self.typing_open = false;
        let Some(u) = self.history.pop() else { return false };
        let cur = Undo {
            label: u.label.clone(),
            doc: std::mem::replace(&mut self.doc, u.doc),
            sel: std::mem::replace(&mut self.sel, u.sel),
            decision: u.decision,
        };
        self.redo.push(cur);
        self.touch();
        true
    }
    pub fn redo(&mut self) -> bool {
        // embed: see `undo`.
        if self.view_only {
            return false;
        }
        self.typing_open = false;
        let Some(u) = self.redo.pop() else { return false };
        let cur = Undo {
            label: u.label.clone(),
            doc: std::mem::replace(&mut self.doc, u.doc),
            sel: std::mem::replace(&mut self.sel, u.sel),
            decision: u.decision,
        };
        self.history.push(cur);
        self.touch();
        true
    }
    /// Drop history (after open / new).
    pub fn reset_history(&mut self) {
        self.history.clear();
        self.redo.clear();
        self.typing_open = false;
    }

    /// embed: start an edit session, the only way into one: every change is tracked under `author`
    /// and dated `revision_date` (UTC whole seconds), and runs only `crate::edit_mode` commands.
    /// Undo and redo start afresh, so neither reaches past the session's start, and the changes this
    /// session makes are told apart by their records, made from here on. Starting again with other
    /// rules or another date ends the session before and starts a new one: one date per save.
    /// Refused without an author or a valid date, leaving everything as it was. The same author,
    /// date and rules as the session running keep it as it is: that is not a new session.
    pub fn begin_edit(&mut self, author: &str, revision_date: &str, rules: crate::edit_mode::EditRules) -> Result<(), CmdError> {
        let blank = author.trim().is_empty() || author.chars().count() > 255;
        if blank || !crate::edit_mode::is_revision_date(revision_date) {
            return Err(self.refused("begin_edit", crate::edit_mode::Refusal::InvalidEditSession, None));
        }
        if self.edit_rules == Some(rules) && self.revision_date.as_deref() == Some(revision_date) && self.edit_author == author {
            return Ok(());
        }
        self.view_only = false;
        self.author = author.to_string();
        self.edit_author = author.to_string();
        self.edit_rules = Some(rules);
        self.revision_date = Some(revision_date.to_string());
        self.edit_floor = self.doc.revisions.len();
        self.doc.settings.track_changes = true;
        self.pending = None;
        self.refusal = None;
        self.reset_history();
        Ok(())
    }

    /// embed: end the edit session. Its history goes with it; the document keeps its edits.
    pub fn end_edit(&mut self) {
        if self.edit_rules.is_some() {
            self.reset_history();
        }
        self.edit_rules = None;
        self.revision_date = None;
        self.edit_floor = 0;
        self.edit_author.clear();
    }

    /// embed: the edit session's rules, while one runs.
    pub fn edit_rules(&self) -> Option<crate::edit_mode::EditRules> {
        self.edit_rules
    }

    /// embed: the date this edit session's tracked changes carry, while one runs.
    pub fn revision_date(&self) -> Option<&str> {
        self.revision_date.as_deref()
    }

    /// embed: an edit session's refusal of `id`, recorded where the host reads it.
    fn refused(&mut self, id: &str, refusal: crate::edit_mode::Refusal, detail: Option<String>) -> CmdError {
        let error = CmdError::Editing(crate::edit_mode::EditRefusal { refusal, command: id.to_string(), detail });
        self.status = error.to_string();
        self.refusal = Some(refusal);
        error
    }

    /// Replace the document (open/new).
    pub fn set_document(&mut self, doc: Document) {
        // embed: another document ends an edit session: its revision records are not this session's.
        self.end_edit();
        self.doc = doc;
        self.doc.ensure_nonempty();
        self.sel = Selection::caret(self.doc.start_of(StoryRef::Body));
        self.pending = None;
        self.reset_history();
        self.touch();
        self.dirty = false;
        self.relayout();
    }

    /// Make the selection valid for the current document.
    pub fn clamp_selection(&mut self) {
        self.sel.anchor = self.doc.clamp(&self.sel.anchor);
        self.sel.focus = self.doc.clamp(&self.sel.focus);
    }

    /// Run a command by id with JSON params (the single entry point for the UI, CLI, MCP and
    /// control channel). Mutating commands are undoable; a failed command leaves the document as
    /// it was; a panic inside a command becomes an error.
    pub fn run(&mut self, id: &str, params: &Value) -> Result<Value, CmdError> {
        let result = self.run_command(id, params);
        // embed: in an edit session every error is a refusal with a stable code, for the host to map
        // to its own message: an unknown command, bad parameters and a command that does not apply
        // here (nothing selected, nothing to undo) included.
        match result {
            Err(error) if self.edit_rules.is_some() => {
                use crate::edit_mode::Refusal;
                let (refusal, detail) = match error {
                    CmdError::Editing(_) => return Err(error),
                    CmdError::Unknown(_) => (Refusal::UnknownCommand, None),
                    CmdError::Params(detail) => (Refusal::InvalidParameters, Some(detail)),
                    CmdError::Disabled(detail) => (Refusal::NotAvailable, Some(detail)),
                    CmdError::Failed(detail) => (Refusal::CommandFailed, Some(detail)),
                };
                Err(self.refused(id, refusal, detail))
            }
            other => other,
        }
    }

    fn run_command(&mut self, id: &str, params: &Value) -> Result<Value, CmdError> {
        let reg = self.registry.clone();
        self.refusal = None;
        let Some(spec) = reg.get(id) else { return Err(CmdError::Unknown(id.to_string())) };
        // embed: a view-only session refuses every edit path, not just the ones marked as mutating.
        if self.view_only && !crate::view_only::allows(id) {
            return Err(CmdError::Disabled(crate::view_only::refusal(id)));
        }
        // embed: an edit session refuses every command that would not be a tracked change. Tab types
        // a tab character there, never the cell, row, list-level or indent moves `text.tab` makes in
        // the full application (embed API "Keyboard in edit mode").
        if let Some(rules) = self.edit_rules {
            use crate::edit_mode::{Refusal, TAB, allows, editable_story, refusal_for, tab_params};
            // Tracking is on for the whole session, whatever the document's own setting, so no
            // allowed command can make an untracked change (embed API 0.2, "WordCraft tracks every change").
            self.doc.settings.track_changes = true;
            // Only `begin_edit` names the session's author, whatever wrote `author` since.
            if self.author != self.edit_author {
                self.author.clone_from(&self.edit_author);
            }
            if id == TAB {
                return self.run("text.insert", &tab_params());
            }
            if !allows(id, rules) {
                return Err(self.refused(id, refusal_for(id, rules), None));
            }
            // Redo replays a whole document state: one a decision made needs the permission now.
            if id == "edit.redo" && !rules.may_decide && self.redo.last().is_some_and(|step| step.decision) {
                return Err(self.refused(id, Refusal::DecisionNotAllowed, None));
            }
            // Comments and text boxes stay as the file has them (Ruling W2-2).
            if spec.mutates && !(editable_story(&self.doc, self.sel.anchor.story) && editable_story(&self.doc, self.sel.focus.story)) {
                return Err(self.refused(id, Refusal::OutsideEditableStory, None));
            }
        }
        if let Some(why) = (spec.enabled)(self) {
            return Err(CmdError::Disabled(format!("{id}: {why}")));
        }
        // Restrict Editing.
        if spec.mutates
            && let Some(mode) = self.doc.settings.protection.clone()
        {
            let allowed = id.starts_with("review.restrict") || id.starts_with("file.") || id == "edit.undo" || id == "edit.redo";
            let comment_ok = id.starts_with("review.") && (id.contains("Comment") || id == "review.reply");
            // embed: an edit session's refusal carries its code (`document_protected`).
            let protected = |session: &mut Session, detail: String| match session.edit_rules {
                Some(_) => session.refused(id, crate::edit_mode::Refusal::DocumentProtected, Some(detail)),
                None => CmdError::Disabled(detail),
            };
            match mode.as_str() {
                "readOnly" | "forms" if !allowed => return Err(protected(self, format!("{id}: the document is protected (read only)"))),
                "comments" if !allowed && !comment_ok => return Err(protected(self, format!("{id}: only comments are allowed in this document"))),
                "trackedChanges" => self.doc.settings.track_changes = true,
                _ => {}
            }
        }
        // Macro recording and Repeat.
        let record = !matches!(id, "tools.recordMacro" | "tools.macros" | "edit.undo" | "edit.redo" | "edit.repeat")
            && (spec.mutates || id.starts_with("caret.") || id.starts_with("select."));
        if record && let Some((_, steps)) = self.recording.as_mut() {
            steps.push((id.to_string(), params.clone()));
        }
        if spec.mutates && !matches!(id, "edit.undo" | "edit.redo" | "edit.repeat") {
            self.last_command = Some((id.to_string(), params.clone()));
        }
        let before_doc = if spec.mutates { Some((self.doc.clone(), self.sel.clone(), self.history.len(), self.typing_open)) } else { None };
        if spec.mutates && spec.id != "text.insert" {
            self.typing_open = false;
        }
        if spec.mutates {
            let label = if spec.id == "text.insert" { "Typing" } else { spec.label };
            self.checkpoint(label);
            // embed: remember which steps decided tracked changes (see `Undo::decision`).
            if crate::edit_mode::DECIDE.contains(&id)
                && let Some(step) = self.history.last_mut()
            {
                step.decision = true;
            }
        } else if !spec.id.starts_with("caret.") && !spec.id.starts_with("view.") {
            // Non-mutating commands other than caret movement keep the typing group.
        } else if spec.id.starts_with("caret.") {
            self.typing_open = false;
        }
        let run = spec.run;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(self, params)));
        let result = match result {
            Ok(r) => r,
            Err(p) => {
                let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
                Err(CmdError::Failed(format!("{id} panicked: {msg}")))
            }
        };
        match &result {
            Ok(_) => {
                if spec.mutates {
                    self.touch();
                    self.doc.ensure_nonempty();
                }
                self.clamp_selection();
            }
            Err(e) => {
                if let Some((d, s, h, t)) = before_doc {
                    self.doc = d;
                    self.sel = s;
                    self.history.truncate(h);
                    self.typing_open = t;
                }
                self.status = e.to_string();
                self.refusal = e.edit_refusal();
            }
        }
        result
    }

    /// The paragraph path the caret is in.
    pub fn caret_path(&self) -> &Path {
        &self.sel.focus.path
    }
    pub fn story(&self) -> StoryRef {
        self.sel.focus.story
    }

    /// Formatting typing would use at the caret.
    pub fn typing_props(&self) -> CharProps {
        if let Some(p) = &self.pending {
            return p.clone();
        }
        let f = &self.sel.focus;
        self.doc.para_at(f).map(|p| p.props_at(f.off).clone()).unwrap_or_default()
    }

    /// Selected plain text.
    pub fn selected_text(&self) -> String {
        let (a, b) = self.sel.ordered();
        self.doc.copy_range(&a, &b).plain_text()
    }
}

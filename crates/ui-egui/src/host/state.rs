//! The host page's side of a running app: its commands apply at the start of a frame
//! (`before_frame`, from `WordApp::logic`); its events are produced at the end (`after_frame`,
//! from `WordApp::ui`) and reach the page after that frame.

use egui::{Rect, Vec2};
use wordcraft_doc::Pos;
use wordcraft_engine::Selection;

use super::bridge::{Cause, HostBridge, HostCommand, HostEvent};
use super::marks::Marks;
use super::text_match::{TextIndex, visible_fragment_text};
use super::wire::{Display, JumpTarget, OpenOptions, ScrollRequest, ThemeSpec, ZoomRequest, ZoomValue, occurrence_index, page_index};
use crate::WordApp;

/// Points left above a passage the host jumps to, so it does not sit on the view's edge. The PDF
/// editor's embed build leaves the same above its matches, so both previews place a passage alike.
const JUMP_MARGIN_PT: f32 = 60.0;

/// A host jump that was found but whose scroll the canvas has not applied yet.
struct PendingJump {
    target: JumpTarget,
    /// The scroll it replaced, put back when a newer jump finds nothing.
    displaced: Option<(usize, f32)>,
    /// The selection it replaced, put back with the scroll while still in the frame it was found in.
    replaced: SelectionBefore,
    /// The frame (`Context::cumulative_frame_nr`) it was found in.
    frame: u64,
}

/// What a jump's selection replaced: the session's selection and page hint, and what the host
/// state knew of them, so a replaced jump that a newer, failed one undoes leaves no trace.
struct SelectionBefore {
    sel: Selection,
    page_hint: usize,
    settled: Selection,
    clear_reported: bool,
}

/// What places the pages on screen: the canvas's scale and size, and where the first page sits.
#[derive(Clone, Copy, PartialEq)]
struct ViewGeometry {
    scale: f32,
    size: Vec2,
    first_page: Option<Vec2>,
}

pub struct HostState {
    pub bridge: HostBridge,
    /// Set by Ctrl/⌘+F; the view bar focuses its find field on the next frame.
    pub(crate) focus_find: bool,
    /// Whether the editor draws its own view bar (`MountOptions.viewBar`, API 0.3). Without it the
    /// host draws page, zoom and find controls, and Ctrl/⌘+F is handed to the host.
    pub(crate) view_bar: bool,
    pub(crate) find_query: String,
    /// Where the current find began: typing refines the search from here, not from the last match.
    pub(crate) find_anchor: Option<Pos>,
    /// The page the view bar or the host last jumped to. It stays the current page until a person
    /// scrolls (wheel, scroll bar, touch drag, a scroll key or a selection drag that moves the view)
    /// or the next jump or find replaces it. "Most visible" cannot name a page that never wins that
    /// vote (the last one, or any page when the whole document is in view), so Next page would never
    /// reach it; and layout settling, a resize or a zoom are not the person moving the view, so none
    /// of them drops it.
    pub(crate) page_target: Option<usize>,
    /// Points below the target page's top that the jump put at the top of the view (0 for a page; a
    /// passage less its margin), so a resize can put the same place back.
    target_y: f32,
    /// A scroll the host or the view bar asked for is due in this frame's canvas, so the view moving
    /// is not a person's doing.
    scroll_queued: bool,
    pending_jump: Option<PendingJump>,
    last_view: Option<ViewGeometry>,
    last_page: Option<usize>,
    /// The host has been sent `opened`. Until then no page is reported: the empty document the
    /// editor starts with is not one the host opened, so its page 1 would come before any `opened`.
    opened: bool,
    /// The document's text as jumps and marks match it; rebuilt when the document changes.
    index: TextIndex,
    /// The host's marks, and where they were found and painted.
    pub(crate) marks: Marks,
    /// The chrome theme the host chose, if it did; it outranks the app's own dark setting.
    pub(crate) dark: Option<bool>,
    /// The selection as it last settled: a person's (told to the host or not) or one the editor
    /// made itself. A selection that differs from it is a person's new one; a pointer press forgets
    /// a told one (see `settle_selection`).
    settled: Selection,
    /// The host was told of a selection (`selection`) and not yet that it is gone (`selectionCleared`).
    reported: bool,
    /// The editor replaced the selection this frame, so a reported one is cleared when it ends.
    clear_reported: bool,
    /// embed: the edit session while the host page has the document in edit mode (embed API 0.2).
    pub(crate) edit: Option<super::edit::EditSession>,
    /// The opened file's name, which a save keeps; and whether a document is open: the last open
    /// succeeded (a refused open leaves nothing to edit).
    pub(crate) file_name: String,
    pub(crate) file_open: bool,
    /// A comment asked for during the frame, answered after it once its page is drawn
    /// (`edit::after_frame`).
    pub(crate) comment_asked: bool,
    /// The content version the open document had when it opened: it differs from it while dirty.
    pub(crate) opened_version: u64,
    /// The date and the author (id, name) of the edit session begun last since the open: unsaved
    /// changes carry them.
    pub(crate) session_date: Option<String>,
    pub(crate) session_author: Option<(String, String)>,
    /// The dirty state the host was last told (`dirtyChanged`); a new document starts clean.
    pub(crate) dirty: bool,
    /// This frame (`Context::cumulative_frame_nr`), and the frame and document revision a save last
    /// answered: two requests in one frame for the same document answer once.
    pub(crate) frame: u64,
    pub(crate) last_save: Option<(u64, u64, bool)>,
    /// Rows of the view bar while editing: the edit strip goes on a second row when one is too narrow.
    pub(crate) bar_rows: u8,
    /// The last open used `display: 'final'`: the markup view settings it changed are put back by the
    /// next open that asks for the stored display.
    pub(crate) clean_display: bool,
    /// The zoom the host was last in a position to know, `(percent, page-width fit)`. `None` until a
    /// document is open, and again after each open: the first reading is the baseline, not a change.
    last_zoom: Option<(u32, bool)>,
    /// The token of a `setZoom` not yet answered; the next frame's `zoomChanged` carries it.
    zoom_token: Option<u32>,
    /// The open document's locator, the token of a `scrollToBlock` not yet answered and the last view
    /// told to the host (embed API 0.3).
    pub(crate) viewport: super::viewport::ViewportState,
}

impl HostState {
    /// Make `page` the current page (see `page_target`), its top at the top of the view.
    pub(crate) fn jump_to(&mut self, page: usize) {
        self.aim(page, 0.0);
    }

    /// Make `page` the current page with the point `y` below its top at the top of the view.
    fn aim(&mut self, page: usize, y: f32) {
        self.page_target = Some(page);
        self.target_y = y;
        self.scroll_queued = true;
    }

    /// The scroll that puts the target back at the top of the view after the pages moved, if there
    /// is a target.
    pub(crate) fn target_scroll(&mut self) -> Option<(usize, f32)> {
        let page = self.page_target?;
        self.scroll_queued = true;
        Some((page, self.target_y))
    }

    /// The page the view bar and `pageChanged` report: the jump's target while it holds, else the
    /// most visible page.
    pub(crate) fn current_page(&self, visible: Option<usize>) -> Option<usize> {
        self.page_target.or(visible)
    }

    /// The page jumps made current (tests).
    #[doc(hidden)]
    pub fn page_target(&self) -> Option<usize> {
        self.page_target
    }

    /// Screen rectangles of the marks painted last frame, with their ids, the topmost last (tests).
    #[doc(hidden)]
    pub fn mark_rects(&self) -> Vec<(egui::Rect, String)> {
        // Host marks describe the stored version; they are hidden while it is edited.
        if self.edit.is_some() {
            return Vec::new();
        }
        self.marks.painted()
    }

    /// Whether some mark has not been looked for yet (tests).
    #[doc(hidden)]
    pub fn marks_pending(&self) -> bool {
        self.marks.pending()
    }

    /// Look for the marks not yet found in this document and layout; `true` while some remain.
    pub(crate) fn resolve_marks(
        &mut self,
        doc: &wordcraft_doc::Document,
        rev: u64,
        content_version: u64,
        layout: &std::sync::Arc<wordcraft_layout::DocLayout>,
    ) -> bool {
        let locator = self.viewport.locator_for(doc, content_version);
        let pending = self.marks.resolve(&mut self.index, doc, rev, layout, locator);
        if let Some(ids) = self.marks.take_refused() {
            self.bridge.emit(HostEvent::LocationMismatch { ids });
        }
        pending
    }

    /// The editor moved the selection itself (open, jump, find, page buttons): not a person's
    /// selection, so it is never reported, and a reported one is cleared at the end of the frame (its
    /// toolbar would otherwise stay on text the view may have scrolled away).
    pub(crate) fn note_programmatic_selection(&mut self, sel: &Selection) {
        self.settled = sel.clone();
        self.clear_reported = true;
    }

    pub fn new(bridge: HostBridge) -> Self {
        HostState {
            bridge,
            focus_find: false,
            view_bar: true,
            find_query: String::new(),
            find_anchor: None,
            page_target: None,
            target_y: 0.0,
            scroll_queued: false,
            pending_jump: None,
            last_view: None,
            last_page: None,
            opened: false,
            index: TextIndex::default(),
            marks: Marks::default(),
            dark: None,
            settled: Selection::default(),
            reported: false,
            clear_reported: false,
            edit: None,
            file_name: String::new(),
            file_open: false,
            comment_asked: false,
            opened_version: 0,
            session_date: None,
            session_author: None,
            dirty: false,
            frame: 0,
            last_save: None,
            bar_rows: 1,
            clean_display: false,
            last_zoom: None,
            zoom_token: None,
            viewport: Default::default(),
        }
    }

    /// Entering edit mode: a selection the host was told of is cleared, before `modeChanged` (embed
    /// API 0.2, "Entering edit mode").
    pub(crate) fn clear_reported_selection(&mut self) {
        if std::mem::take(&mut self.reported) {
            self.bridge.emit(HostEvent::SelectionCleared);
        }
        self.clear_reported = false;
    }

    /// Leaving edit mode: the selection made while editing counts as settled, so it is reported only
    /// once the person changes it.
    pub(crate) fn settle_quietly(&mut self, sel: &Selection) {
        self.settled = sel.clone();
        self.clear_reported = false;
    }
}

pub(crate) fn before_frame(app: &mut WordApp, ctx: &egui::Context) {
    // Taken out while commands run, so they can borrow the rest of the app.
    let Some(mut host) = app.host.take() else { return };
    host.frame = ctx.cumulative_frame_nr();
    // Read before the canvas's scroll area consumes the frame's scroll delta.
    host.viewport.person_input = person_input(ctx, app.canvas.canvas_rect, app.canvas.focused);
    if host.page_target.is_some() && scrolled_by_person(ctx, app.canvas.canvas_rect, app.canvas.focused) {
        host.page_target = None;
    }
    let mut host_moves_view = false;
    for command in host.bridge.take_commands() {
        host_moves_view |= matches!(
            command,
            HostCommand::Open { .. }
                | HostCommand::OpenTooLarge { .. }
                | HostCommand::JumpTo(_)
                | HostCommand::SetZoom(_)
                | HostCommand::ScrollToBlock(_)
        );
        match command {
            HostCommand::Open { name, bytes, options } => {
                super::edit::before_open(app, &mut host);
                open(app, &mut host, &name, &bytes, options);
            }
            // Refused like any open that fails: the open document stays, edit mode ends (embed API 0.2).
            HostCommand::OpenTooLarge { .. } => {
                super::edit::before_open(app, &mut host);
                refuse_open(&mut host, super::open::TOO_LARGE.to_string());
            }
            HostCommand::JumpTo(target) => jump(app, &mut host, target, ctx.cumulative_frame_nr()),
            // Looked for when next painted, against the layout painted then.
            HostCommand::SetMarks(marks) => host.marks.set(marks),
            HostCommand::SetTheme(theme) => set_theme(app, &mut host, ctx, &theme),
            HostCommand::SetMode(request) => super::edit::set_mode(app, &mut host, request),
            // Answered here, before any later command of the frame changes the document or the mode.
            HostCommand::RequestSave => super::edit::save(app, &mut host),
            HostCommand::SetZoom(request) => set_zoom(app, &mut host, &request),
            HostCommand::ScrollToBlock(request) => scroll_to_block(app, &mut host, &request),
            // Answered after the frame, once a document is open and laid out; moves nothing.
            HostCommand::Locate(request) => host.viewport.pending_locates.push(request),
        }
    }
    // A host call replaces a glide the person's input started; the move is the host's from here.
    if host_moves_view && !host.viewport.person_input {
        host.viewport.person_moving = false;
    }
    app.host = Some(host);
}

/// Dark or light chrome and the host's accent. Pages are painted white whatever the theme.
fn set_theme(app: &mut WordApp, host: &mut HostState, ctx: &egui::Context, theme: &ThemeSpec) {
    let Some((accent, content)) = theme.colors() else {
        log::warn!("embed: theme ignored, accent and accentContent must be #rrggbb");
        return;
    };
    crate::theme::set_accent_override(ctx, Some(crate::theme::AccentOverride { accent, content }));
    // `WordApp::logic` pins egui to this theme (an explicit preference: following the system, a
    // change of the operating system's theme would swap in the other theme under the host's choice).
    host.dark = Some(theme.dark);
    // Re-apply the egui style this frame, even when only the accent changed.
    app.applied_theme = None;
}

/// The zoom the host asked for, applied to the view as host state: view mode's command gate is not
/// consulted (and not widened). It mirrors what the engine's `view.zoom` sets: a percentage ends any
/// fit; the page-width fit shows one page per row.
fn set_zoom(app: &mut WordApp, host: &mut HostState, request: &ZoomRequest) {
    let view = &mut app.session.view;
    match request.zoom {
        ZoomValue::Percent(percent) => {
            view.zoom = percent as f32 / 100.0;
            view.fit.clear();
            view.multi_page = false;
        }
        ZoomValue::Fit(_) => {
            view.fit = "pageWidth".into();
            view.multi_page = false;
            view.single_column = true;
        }
    }
    // A wheel zoom still easing out would overwrite the host's value.
    app.canvas.wheel.cancel();
    super::view::keep_target_in_view(app);
    host.zoom_token = Some(request.token);
}

/// Put a fraction of the block the host named at the top of the view, as a page jump does, but
/// selecting nothing. The `viewport` that follows carries the request's token; a block the document
/// lacks is answered `blockNotFound` at once and moves nothing.
fn scroll_to_block(app: &mut WordApp, host: &mut HostState, request: &ScrollRequest) {
    let Some((page, y)) = super::viewport::target(app, &mut host.viewport, &request.block, request.fraction) else {
        host.bridge.emit(HostEvent::BlockNotFound { block: request.block.clone(), token: request.token });
        return;
    };
    app.canvas.scroll_to_doc = Some((page, y));
    host.aim(page, y);
    host.viewport.scroll_token = Some(request.token);
}

/// Answer the `locate` requests waiting, in the order they came, once a document is open: the layout
/// is made on demand, so an open document has one. Before that they wait, as a jump's host waits for
/// `opened`.
fn answer_locates(app: &mut WordApp, host: &mut HostState) {
    if !host.opened || host.viewport.pending_locates.is_empty() {
        return;
    }
    for request in std::mem::take(&mut host.viewport.pending_locates) {
        let (positions, height) = super::viewport::locate(app, &mut host.viewport, &request.blocks);
        host.bridge.emit(HostEvent::Located { positions, height, token: request.token });
    }
}

/// How long after the person's input a move still counts as theirs when none has begun: a key's
/// scroll starts gliding a frame or two after the key press.
const PERSON_GLIDE_GRACE_SECONDS: f64 = 1.0;

/// Tell the host where the view is, after the frame that drew it, when it changed. Its own
/// `scrollToBlock` is answered once its scroll has landed, with its token, even when the view was
/// already there (it may be waiting on the token). Otherwise the cause is `User` only when the
/// person's own input moved the view in that frame (see [`person_input`]; a wheel's easing counts as
/// the wheel's); everything else is `Host` without a token: the first reading after an open, the
/// host's `jumpTo` and `setZoom` and the reflow they cause, layout settling and a resize. A host's
/// scroll sync makes the side the reviewer moves the leader, so the host's own calls must not look
/// like the reviewer.
fn report_viewport(app: &mut WordApp, host: &mut HostState, ctx: &egui::Context) {
    if !host.opened {
        return;
    }
    // The canvas took the scroll this frame: the view measured below is the one it landed on.
    let landed = host.viewport.scroll_token.is_some() && app.canvas.scroll_to_doc.is_none();
    // Not drawn yet: asked again next frame, the token still waiting.
    let Some(reading) = super::viewport::read(app, &mut host.viewport) else { return };
    let (reading, key) = reading.rounded();
    let changed = host.viewport.last.replace(key.clone()) != Some(key);
    let token = if landed { host.viewport.scroll_token.take() } else { None };
    let now = ctx.input(|input| input.time);
    if host.viewport.person_input || app.canvas.wheel.is_animating() {
        host.viewport.person_moving = true;
        host.viewport.moved_since_input = false;
        host.viewport.last_input_time = now;
    } else if host.viewport.person_moving {
        if changed {
            host.viewport.moved_since_input = true;
        } else if host.viewport.moved_since_input || now - host.viewport.last_input_time > PERSON_GLIDE_GRACE_SECONDS {
            // The glide the person started has come to rest (or never began).
            host.viewport.person_moving = false;
        }
    }
    if !(changed || token.is_some()) {
        return;
    }
    let cause = if token.is_none() && host.viewport.person_moving { Cause::User } else { Cause::Host };
    host.bridge.emit(HostEvent::Viewport {
        top: reading.top,
        fraction: reading.fraction,
        visible_height: reading.visible_height,
        page: reading.page,
        pages: reading.pages,
        cause,
        token,
    });
}

/// Whether the person's own input is on the canvas this frame: a wheel or trackpad scroll, a drag on
/// a scroll bar, a touch drag, a scroll key while the canvas has the keyboard, or a press or drag
/// that began in the document. Read before the canvas consumes the frame's input.
fn person_input(ctx: &egui::Context, canvas: Option<Rect>, canvas_focused: bool) -> bool {
    if scrolled_by_person(ctx, canvas, canvas_focused) {
        return true;
    }
    let Some(canvas) = canvas else { return false };
    ctx.input(|input| {
        (input.pointer.any_pressed() || input.pointer.primary_down()) && input.pointer.press_origin().is_some_and(|at| canvas.contains(at))
    })
}

/// Tell the host of a zoom change, after the frame that drew it. The host's own request is always
/// answered, with its token, even when nothing changed (it may be waiting on the token). Otherwise a
/// change of percent or fit is the person's, except the percent moving with the window under the
/// page-width fit, which the host already knows is on.
fn report_zoom(app: &WordApp, host: &mut HostState) {
    let token = host.zoom_token.take();
    if token.is_none() && !host.opened {
        return;
    }
    let now = (super::view::displayed_percent_whole(app), app.session.view.fit == "pageWidth");
    let before = host.last_zoom.replace(now);
    let (percent, page_width) = now;
    if let Some(token) = token {
        host.bridge.emit(HostEvent::ZoomChanged { percent, page_width, cause: Cause::Host, token: Some(token) });
        return;
    }
    let changed = before.is_some_and(|before| before != now);
    let resized_by_the_fit = before.is_some_and(|(_, was_fitted)| was_fitted) && page_width;
    if changed && !resized_by_the_fit {
        host.bridge.emit(HostEvent::ZoomChanged { percent, page_width, cause: Cause::User, token: None });
    }
}

pub(crate) fn after_frame(app: &mut WordApp, ctx: &egui::Context) {
    let Some(mut host) = app.host.take() else { return };
    // The canvas took the queued scroll this frame: a waiting jump has landed, even when something
    // drawn after the canvas queued another scroll already.
    if std::mem::take(&mut app.canvas.scroll_to_doc_taken) || app.canvas.scroll_to_doc.is_none() {
        host.pending_jump = None;
    }
    // Typing in edit mode drops a jump's page target before the page is reported.
    super::edit::follow_typing(app, &mut host);
    follow_the_view(app, &mut host, ctx);
    // The target page is in view too (the jump put its top there, or as near as the end allows), and
    // the host's page indicator should say what the bar says.
    if host.opened
        && let Some(page) = host.current_page(app.canvas.visible_page)
        && host.last_page != Some(page)
    {
        host.last_page = Some(page);
        host.bridge.emit(HostEvent::PageChanged { page: page_number(page) });
    }
    report_zoom(app, &mut host);
    report_viewport(app, &mut host, ctx);
    answer_locates(app, &mut host);
    // Selecting is part of editing: no selection events in edit mode (embed API 0.2).
    if host.edit.is_none() {
        settle_selection(app, &mut host, ctx);
    }
    super::edit::after_frame(app, &mut host, ctx);
    app.host = Some(host);
}

/// Tell the host of a person's selection once it settles (no button held: a drag reports on
/// release, keys at once), never on every frame; and that it is gone when it is. A press forgets
/// the selection told before, so settling on the same text again is told again.
fn settle_selection(app: &mut WordApp, host: &mut HostState, ctx: &egui::Context) {
    if std::mem::take(&mut host.clear_reported) && std::mem::take(&mut host.reported) {
        host.bridge.emit(HostEvent::SelectionCleared);
    }
    // embed: a press starts a new selection even when it ends on the same text, and the host hides
    // its toolbar on any input, so the release must be told again. Only a press in the document is
    // one (the find bar's buttons select nothing), and only a selection the host was told of is
    // forgotten: one the editor made itself stays unreported.
    let pressed_in_document = ctx.input(|input| {
        input.pointer.any_pressed() && input.pointer.press_origin().is_some_and(|at| app.canvas.canvas_rect.is_some_and(|view| view.contains(at)))
    });
    if host.reported && pressed_in_document {
        host.settled = Selection::default();
    }
    if app.session.sel == host.settled || ctx.input(|input| input.pointer.any_down()) {
        return;
    }
    let reading = if app.session.sel.is_collapsed() { Reading::Blank } else { read_selection(app, ctx.zoom_factor()) };
    match reading {
        // Its page is not drawn yet: try again next frame.
        Reading::NotReady => return,
        // Nothing for the host to act on, so a selection it was told of is cleared (once), or its
        // toolbar would stay on stale text.
        Reading::Blank => {
            if std::mem::take(&mut host.reported) {
                host.bridge.emit(HostEvent::SelectionCleared);
            }
        }
        Reading::Text(event) => {
            host.bridge.emit(event);
            host.reported = true;
        }
    }
    host.settled = app.session.sel.clone();
}

/// A settled selection as the host would hear of it.
pub(super) enum Reading {
    Text(HostEvent),
    /// Nothing but whitespace (or object placeholders) is selected.
    Blank,
    /// The selection's end is not on a drawn page.
    NotReady,
}

/// The selection as the host hears it, placed at the end of its last line: egui points times the
/// zoom factor are CSS pixels from the canvas's top-left (eframe's web backend draws one point per
/// CSS pixel at zoom 1). The PDF editor's embed build places its selections alike.
pub(super) fn read_selection(app: &mut WordApp, zoom: f32) -> Reading {
    let (start, end) = app.session.sel.ordered();
    // What the reader sees, as jumps and marks match it: no tracked deletions or hidden text.
    let visible = visible_fragment_text(&app.session.doc.copy_range(&start, &end));
    let Some((text, copy_text)) = host_texts(&visible) else { return Reading::Blank };
    let layout = app.session.layout();
    let rects = layout.selection_rects(&app.session.doc, &start, &end, app.session.page_hint);
    let Some((page, last)) = rects.last() else { return Reading::NotReady };
    let Some(page_rect) = app.canvas.page_rects.get(*page) else { return Reading::NotReady };
    let scale = app.canvas.scale;
    let x = (page_rect.min.x + (last.x + last.w) * scale) * zoom;
    let y = (page_rect.min.y + (last.y + last.h) * scale) * zoom;
    if !(x.is_finite() && y.is_finite()) {
        return Reading::NotReady;
    }
    let page = layout.page_of(&start).map(page_number);
    Reading::Text(HostEvent::Selection { text, copy_text, page, x, y })
}

/// What Ctrl/⌘+C copies in view mode: the selection's `copyText` as the host hears it (no tracked
/// deletions, hidden text or object placeholders), so the keyboard and a host's Copy button copy the
/// same. `None` when the selection has nothing else in it.
pub(crate) fn copy_text(app: &WordApp) -> Option<String> {
    let (start, end) = app.session.sel.ordered();
    if start == end {
        return None;
    }
    host_texts(&visible_fragment_text(&app.session.doc.copy_range(&start, &end))).map(|(_, copy_text)| copy_text)
}

/// The selection's texts for the host, `(text, copy_text)`: `copy_text` keeps line breaks, `text`
/// has its whitespace collapsed, and neither has the placeholder (U+FFFC) an inline object leaves
/// in the text. `None` when nothing else is selected.
fn host_texts(selected: &str) -> Option<(String, String)> {
    let copy_text: String = selected.chars().filter(|c| *c != '\u{FFFC}').collect();
    let text = copy_text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some((text, copy_text))
}

/// Width of the strip along the canvas's right and bottom edges where the scroll bars sit.
const SCROLL_BAR_STRIP: f32 = 20.0;

/// When the pages moved on screen this frame: a resize or a fitted zoom puts the target back at the
/// top of the view (it is not a person scrolling, so the target stays); a selection drag during which
/// the view moved (the caret scroll a press near the view's edge starts) is a person scrolling and
/// drops it.
fn follow_the_view(app: &mut WordApp, host: &mut HostState, ctx: &egui::Context) {
    let ours = std::mem::take(&mut host.scroll_queued);
    let Some(canvas) = app.canvas.canvas_rect else { return };
    let now =
        ViewGeometry { scale: app.canvas.scale, size: canvas.size(), first_page: app.canvas.page_rects.first().map(|page| page.min - canvas.min) };
    let Some(before) = host.last_view.replace(now) else { return };
    let rescaled = (before.scale, before.size) != (now.scale, now.size);
    // A non-finite geometry differs from itself every frame; following it would repaint forever.
    if rescaled && now.scale.is_finite() && now.size.is_finite() {
        if app.canvas.scroll_to_doc.is_none()
            && let Some(target) = host.target_scroll()
        {
            app.canvas.scroll_to_doc = Some(target);
            ctx.request_repaint();
        }
    } else if !rescaled && before.first_page != now.first_page && !ours && selecting_by_drag(ctx, canvas) {
        host.page_target = None;
    }
}

/// Whether the primary button is held and dragged after a press on the document (not on a scroll
/// bar, which `scrolled_by_person` counts): a person selecting text. A press held without moving
/// is not: the caret scroll from the press itself is no reason to drop the target.
fn selecting_by_drag(ctx: &egui::Context, canvas: Rect) -> bool {
    ctx.input(|input| {
        input.pointer.primary_down()
            && input.pointer.is_decidedly_dragging()
            && input
                .pointer
                .press_origin()
                .is_some_and(|at| canvas.contains(at) && at.x <= canvas.max.x - SCROLL_BAR_STRIP && at.y <= canvas.max.y - SCROLL_BAR_STRIP)
    })
}

/// Whether a person is scrolling the document this frame: the wheel or a trackpad over the canvas,
/// a drag that began on a scroll bar or a touch drag, or a key that moves the view while the canvas
/// has the keyboard. Layout settling, resizes and zoom (a Ctrl+wheel is a zoom, not a scroll) are not.
fn scrolled_by_person(ctx: &egui::Context, canvas: Option<egui::Rect>, canvas_focused: bool) -> bool {
    use egui::Key;
    let Some(canvas) = canvas else { return false };
    ctx.input(|input| {
        let wheel = input.smooth_scroll_delta != egui::Vec2::ZERO && input.pointer.hover_pos().is_some_and(|at| canvas.contains(at));
        let origin_in = |strip_only: bool| {
            input.pointer.press_origin().is_some_and(|at| {
                canvas.contains(at) && (!strip_only || at.x > canvas.max.x - SCROLL_BAR_STRIP || at.y > canvas.max.y - SCROLL_BAR_STRIP)
            })
        };
        let dragging = input.pointer.is_decidedly_dragging();
        let bar_drag = dragging && origin_in(true);
        let touch_drag = dragging && input.any_touches() && origin_in(false);
        let scroll_key = canvas_focused
            && [Key::PageUp, Key::PageDown, Key::Home, Key::End, Key::ArrowUp, Key::ArrowDown, Key::ArrowLeft, Key::ArrowRight]
                .into_iter()
                .any(|key| input.key_pressed(key));
        wheel || bar_drag || touch_drag || scroll_key
    })
}

/// Show the host's page or passage; `jumpNotFound` when there is none. A newer jump replaces one
/// whose scroll has not landed yet, found or not, as in the PDF editor's embed build.
///
/// A found jump's selection (the passage, or the caret at the page's top) is the editor's own: it
/// is never reported and clears a reported one. A jump that finds nothing leaves the selection be,
/// and one that replaces a jump found in this same frame puts back the selection that jump
/// replaced, with its scroll. A jump found in an earlier frame is not undone: by then the end of
/// that frame may have cleared a reported selection, or a person may have selected anew.
fn jump(app: &mut WordApp, host: &mut HostState, target: JumpTarget, frame: u64) {
    let waiting = host.pending_jump.take();
    let displaced = match &waiting {
        Some(waiting) => waiting.displaced,
        None => app.canvas.scroll_to_doc,
    };
    // Without a jump from this frame to undo, what is put back is what there is now: nothing changes.
    let replaced = match waiting {
        Some(waiting) if waiting.frame == frame => waiting.replaced,
        _ => SelectionBefore {
            sel: app.session.sel.clone(),
            page_hint: app.session.page_hint,
            settled: host.settled.clone(),
            clear_reported: host.clear_reported,
        },
    };
    match find_target(app, host, &target) {
        Some((page, y)) => {
            app.canvas.scroll_to_doc = Some((page, y));
            host.aim(page, y);
            // A location jump leaves the selection as it was, so there is no new one to forget.
            if !matches!(target, JumpTarget::Location { .. }) {
                host.note_programmatic_selection(&app.session.sel);
            }
            host.pending_jump = Some(PendingJump { target, displaced, replaced, frame });
        }
        None => {
            app.canvas.scroll_to_doc = displaced;
            app.session.sel = replaced.sel;
            app.session.page_hint = replaced.page_hint;
            host.settled = replaced.settled;
            host.clear_reported = replaced.clear_reported;
            host.page_target = None;
            host.bridge.emit(HostEvent::JumpNotFound { target });
        }
    }
}

/// Where `target` is: (page index, points below its top to put at the top of the view). A text
/// jump selects its passage (as find does; not reported to the host as a person's selection); a
/// location jump selects nothing and puts its place in the middle of the view.
fn find_target(app: &mut WordApp, host: &mut HostState, target: &JumpTarget) -> Option<(usize, f32)> {
    let layout = app.session.layout();
    let pages = layout.pages.len();
    match target {
        JumpTarget::Page { page } => {
            let index = page_index(*page, pages)?;
            super::view::go_to_page(app, index);
            Some((index, 0.0))
        }
        JumpTarget::Text { text, page, occurrence } => {
            // No page searches the whole document; a page the document lacks finds nothing.
            let on_page = match page {
                Some(page) => Some(page_index(*page, pages)?),
                None => None,
            };
            let nth = occurrence_index(*occurrence)?;
            host.index.refresh(&app.session.doc, app.session.rev());
            // Only a match that is laid out counts, with a page or without: one the layout never
            // placed (a cell that continues a vertical merge) cannot be shown.
            let (start, end) = host.index.find_where(text, |start, _| layout.page_of(start).is_some_and(|q| on_page.is_none_or(|p| p == q)), nth)?;
            let hint = on_page.or_else(|| layout.page_of(&start)).unwrap_or(0);
            let caret = layout.caret_on(&start, hint)?;
            app.session.sel = Selection { anchor: start, focus: end };
            app.session.page_hint = caret.page;
            // `max` also turns a NaN into 0; the canvas keeps the offset within the page.
            Some((caret.page, (caret.top - JUMP_MARGIN_PT).max(0.0)))
        }
        JumpTarget::Location { at } => {
            // The same resolution as a location mark's, fingerprint included.
            let content_version = app.session.content_version();
            let locator = host.viewport.locator_for(&app.session.doc, content_version);
            let place = super::marks::resolve_location(&app.session.doc, locator, at)?.start;
            let hint = layout.page_of(&place).unwrap_or(0);
            let caret = layout.caret_on(&place, hint)?;
            super::viewport::centred(app, caret.page, caret.top)
        }
    }
}

/// A failed open keeps the document that was showing.
fn open(app: &mut WordApp, host: &mut HostState, name: &str, bytes: &[u8], options: OpenOptions) {
    match super::open::open_document(name, bytes) {
        Ok(doc) => {
            // The embed API's order for one open: selectionCleared, then jumpNotFound, then opened.
            // The reported selection belonged to the document being replaced.
            if std::mem::take(&mut host.reported) {
                host.bridge.emit(HostEvent::SelectionCleared);
            }
            // A jump found in the document being replaced never lands, so the host hears it failed.
            if let Some(waiting) = host.pending_jump.take() {
                host.bridge.emit(HostEvent::JumpNotFound { target: waiting.target });
            }
            host.file_open = true;
            host.file_name = name.to_string();
            app.session.set_document(doc);
            let part_names = wordcraft_docx::facts::part_names(bytes).ok();
            host.viewport.opened(&app.session.doc, app.session.content_version(), part_names);
            apply_display(app, host, options.display);
            // A new document starts clean; `dirtyChanged` reports its edits from here.
            host.opened_version = app.session.content_version();
            host.session_date = None;
            host.session_author = None;
            host.dirty = false;
            app.session.page_hint = 0;
            app.session.find = Default::default();
            app.canvas.scroll_to_doc = Some((0, 0.0));
            app.canvas.visible_page = None;
            host.last_page = None;
            // The zoom the host was told belongs to the document being replaced: the new one's
            // first reading is a baseline again. A pending `setZoom` token still gets its answer.
            host.last_zoom = None;
            host.find_query.clear();
            host.find_anchor = None;
            // Marks belong to the document they were set on.
            host.marks.clear();
            host.settled = app.session.sel.clone();
            host.clear_reported = false;
            host.page_target = None;
            host.scroll_queued = true;
            let pages = app.session.layout().pages.len();
            host.opened = true;
            host.bridge.emit(HostEvent::Opened { pages: u32::try_from(pages).unwrap_or(u32::MAX) });
        }
        Err(message) => refuse_open(host, message),
    }
}

/// The display the host asked for (embed API 0.3). It is set here, as host state, and not through
/// `review.markup`: view mode's command gate refuses that command and stays as it is.
///
/// `Final` accepts the tracked changes on screen only (the file is never touched): deleted wording
/// leaves the layout, and balloons are off, which `Session::balloons_shown` derives from the markup
/// being hidden. Comments are switched off in the markup preferences too, so no comment margin or
/// highlight is painted. Opening another document as stored undoes only what `Final` changed, so a
/// host that never sends options sees exactly what API 0.2 showed.
fn apply_display(app: &mut WordApp, host: &mut HostState, display: Display) {
    let markup = &mut app.session.prefs.markup;
    match display {
        Display::Final => {
            app.session.view.show_markup = false;
            markup.comments = false;
            host.clean_display = true;
        }
        Display::AsStored if host.clean_display => {
            app.session.view.show_markup = true;
            markup.comments = true;
            host.clean_display = false;
        }
        Display::AsStored => return,
    }
    // Deleted wording leaves (or returns to) the layout.
    app.session.relayout();
}

/// An open that failed or was refused: the document that was showing stays on the canvas, but the
/// host shows its failure panel, so there is no document for edit mode until the next open succeeds.
fn refuse_open(host: &mut HostState, message: String) {
    host.file_open = false;
    host.bridge.emit(HostEvent::OpenFailed { message });
}

/// 1-based page number for the host.
pub(crate) fn page_number(index: usize) -> u32 {
    u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::{HostState, host_texts, page_number};
    use crate::host::HostBridge;

    #[test]
    fn a_jump_target_is_the_current_page_until_it_is_dropped() {
        let mut host = HostState::new(HostBridge::new());
        assert_eq!(host.current_page(Some(3)), Some(3));
        host.jump_to(4);
        assert_eq!(host.current_page(Some(3)), Some(4));
        assert_eq!(host.current_page(None), Some(4));
        host.page_target = None;
        assert_eq!(host.current_page(Some(2)), Some(2));
    }

    #[test]
    fn a_rescroll_puts_back_the_place_the_jump_showed() {
        let mut host = HostState::new(HostBridge::new());
        assert_eq!(host.target_scroll(), None);
        host.aim(2, 140.0);
        assert_eq!(host.target_scroll(), Some((2, 140.0)));
        host.jump_to(5);
        assert_eq!(host.target_scroll(), Some((5, 0.0)), "a page jump shows the page's top");
        host.page_target = None;
        assert_eq!(host.target_scroll(), None);
    }

    #[test]
    fn selection_texts_collapse_whitespace_and_drop_object_placeholders() {
        assert_eq!(host_texts("Pay\u{FFFC} the\n  goods"), Some(("Pay the goods".to_string(), "Pay the\n  goods".to_string())));
        assert_eq!(host_texts("\u{FFFC}a\u{FFFC}"), Some(("a".to_string(), "a".to_string())));
        // Nothing but whitespace or placeholders is nothing for the host to act on.
        for blank in ["", " ", "\n\t ", "\u{FFFC}", " \u{FFFC}\n\u{FFFC} ", "\u{00A0}\u{2003}"] {
            assert_eq!(host_texts(blank), None, "{blank:?}");
        }
    }

    /// A scroll queued after the canvas, in the frame the canvas took a jump's scroll, must not keep
    /// the jump waiting: an open next frame would answer it `jumpNotFound` although it landed.
    #[test]
    fn a_jump_whose_scroll_the_canvas_took_has_landed_whatever_is_queued_after_the_canvas() {
        use std::cell::Cell;
        use std::rc::Rc;

        use wordcraft_doc::Document;
        use wordcraft_engine::Session;

        use crate::host::{EmbedProfile, HostCommand, HostEvent, JumpTarget};
        use crate::{Services, WordApp};

        let bridge = HostBridge::new();
        let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(bridge.clone(), EmbedProfile::View);
        // Off: the app's own frame. On: a frame whose canvas is followed by a scroll queued before
        // the frame's end, as UI drawn after the canvas could queue one.
        let race = Rc::new(Cell::new(false));
        let racing = race.clone();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 800.0)).with_max_steps(12).build_ui_state(
            move |ui, app: &mut WordApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                if racing.get() {
                    crate::canvas::show(app, ui);
                    app.canvas.scroll_to_doc = Some((0, 0.0));
                    super::after_frame(app, &ctx);
                } else {
                    app.ui(ui);
                }
            },
            app,
        );
        bridge.set_context(h.ctx.clone());
        crate::host::prepare_context(&h.ctx);
        h.run_steps(2);
        let text: Vec<String> = (1..=120).map(|i| format!("Clause {i}. The Supplier shall deliver the goods within thirty days.")).collect();
        let bytes = wordcraft_engine::io::save_bytes("a.docx", &Document::from_text(&text.join("\n"))).expect("writes");
        bridge.send(HostCommand::Open { name: "a.docx".into(), bytes: bytes.clone(), options: Default::default() });
        h.run_steps(4);
        bridge.take_events();
        bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 3 }));
        race.set(true);
        h.step();
        race.set(false);
        assert!(h.state().host.as_ref().is_some_and(|host| host.pending_jump.is_none()), "the jump landed");
        bridge.send(HostCommand::Open { name: "a.docx".into(), bytes, options: Default::default() });
        h.run_steps(4);
        let events = bridge.take_events();
        assert!(!events.iter().any(|event| matches!(event, HostEvent::JumpNotFound { .. })), "{events:?}");
    }

    /// An open starts the new document's zoom from a fresh reading: what the host was last told
    /// belongs to the document being replaced. Checked between the open and the frame's end, where
    /// the new baseline is taken, since a host open does not change the zoom itself.
    #[test]
    fn an_open_forgets_the_zoom_the_host_was_last_told() {
        use wordcraft_doc::Document;
        use wordcraft_engine::Session;

        use crate::host::{EmbedProfile, HostCommand};
        use crate::{Services, WordApp};

        let bridge = HostBridge::new();
        let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(bridge.clone(), EmbedProfile::View);
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 800.0)).with_max_steps(12).build_ui_state(
            |ui, app: &mut WordApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        bridge.set_context(h.ctx.clone());
        crate::host::prepare_context(&h.ctx);
        h.run_steps(2);
        let bytes = wordcraft_engine::io::save_bytes("a.docx", &Document::from_text("Clause 1.")).expect("writes");
        bridge.send(HostCommand::Open { name: "a.docx".into(), bytes: bytes.clone(), options: Default::default() });
        h.run_steps(4);
        assert!(h.state().host.as_ref().is_some_and(|host| host.last_zoom.is_some()), "the first open's frames took a baseline");
        bridge.send(HostCommand::Open { name: "a.docx".into(), bytes, options: Default::default() });
        let ctx = h.ctx.clone();
        super::before_frame(h.state_mut(), &ctx);
        assert!(h.state().host.as_ref().is_some_and(|host| host.last_zoom.is_none()), "the second open dropped it");
    }

    #[test]
    fn page_numbers_are_one_based_and_saturate() {
        assert_eq!(page_number(0), 1);
        assert_eq!(page_number(41), 42);
        assert_eq!(page_number(usize::MAX), u32::MAX);
        assert_eq!(page_number(u32::MAX as usize), u32::MAX);
    }
}

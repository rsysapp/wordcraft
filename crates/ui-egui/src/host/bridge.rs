//! The queues between a host page and the running app. The host side (`EmbeddedEditor`) sends
//! commands and takes events; the app drains commands in `WordApp::logic` and emits events.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::Serialize;

use super::wire::{JumpTarget, MarkSpec, ModeRequest, OpenOptions, ThemeSpec};

/// What a host page asks of the editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostCommand {
    /// Open a document, replacing the open one; answered by `Opened` or `OpenFailed`.
    Open { name: String, bytes: Vec<u8>, options: OpenOptions },
    /// A file over `open::MAX_OPEN_BYTES`, of which nothing was copied (`open::open_command`).
    /// Answered by `OpenFailed` (`open::TOO_LARGE`), in order with the other opens.
    OpenTooLarge { name: String, len: usize },
    /// Show a page or a passage; answered by `JumpNotFound` when there is nothing to show.
    JumpTo(JumpTarget),
    /// Replace every host mark; an empty list clears them. Marks not found are skipped silently.
    SetMarks(Vec<MarkSpec>),
    /// Dark or light chrome and the host's accent; pages stay white.
    SetTheme(ThemeSpec),
    /// Switch the open document to view or edit mode; answered by `ModeChanged`.
    SetMode(ModeRequest),
    /// Serialise the document; answered by `SaveRequested` or `SaveFailed`.
    RequestSave,
}

/// What the editor tells the host page; serialises to the embed API's `EditorEvent`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum HostEvent {
    Opened {
        pages: u32,
    },
    OpenFailed {
        message: String,
    },
    PageChanged {
        page: u32,
    },
    Selection {
        text: String,
        copy_text: String,
        page: Option<u32>,
        x: f32,
        y: f32,
    },
    SelectionCleared,
    MarkActivated {
        id: String,
    },
    JumpNotFound {
        target: JumpTarget,
    },
    ModeChanged {
        mode: EditorMode,
    },
    DirtyChanged {
        dirty: bool,
    },
    /// `bytes` is set on the JavaScript object by the embed crate, outside JSON.
    SaveRequested {
        file_name: String,
        #[serde(skip)]
        bytes: Vec<u8>,
    },
    SaveFailed {
        message: String,
    },
    CommentRequested {
        text: String,
        copy_text: String,
        page: Option<u32>,
        x: f32,
        y: f32,
    },
    /// An action refused in edit mode, once per action, by its stable code (`edit_mode::Refusal`).
    EditRefused {
        code: String,
    },
}

/// The mode in force, as `modeChanged` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EditorMode {
    View,
    Edit,
}

/// What `saveFailed` says when a save's value could not be made for the host page (its bytes could
/// not be allocated or attached there).
pub const SAVE_NOT_HANDED_OVER: &str = "The document could not be handed to the page.";

/// The value the host page receives for `event`, made by `encode` (the embed crate's JavaScript
/// object). A save never goes unanswered: when its value cannot be made, the host receives
/// `SaveFailed` ([`SAVE_NOT_HANDED_OVER`]) instead. Any other event that cannot be made is the
/// caller's to log; the error of the first attempt is logged here for a save.
pub fn encode_for_host<V, E: std::fmt::Display>(event: &HostEvent, mut encode: impl FnMut(&HostEvent) -> Result<V, E>) -> Result<V, E> {
    match encode(event) {
        Err(error) if matches!(event, HostEvent::SaveRequested { .. }) => {
            log::error!("embed: a save could not be handed to the page: {error}");
            encode(&HostEvent::SaveFailed { message: SAVE_NOT_HANDED_OVER.into() })
        }
        result => result,
    }
}

#[derive(Default)]
struct Queues {
    commands: VecDeque<HostCommand>,
    events: Vec<HostEvent>,
    ctx: Option<egui::Context>,
    /// Set by [`HostBridge::detach`]; checked under the same lock as each queue, so nothing slips
    /// in after the queues were cleared.
    detached: bool,
}

/// Both ends of the host bridge; clones share the same queues. A poisoned lock still holds valid
/// queues (nothing here panics mid-update), so its contents are used rather than dropped.
#[derive(Clone, Default)]
pub struct HostBridge {
    inner: Arc<Mutex<Queues>>,
}

impl HostBridge {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Queues> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// App side: the context a command wakes (set once the runner created the app).
    pub fn set_context(&self, ctx: egui::Context) {
        let replaced = {
            let mut queues = self.lock();
            if queues.detached {
                return;
            }
            queues.ctx.replace(ctx)
        };
        // A context can hold the last reference to egui's state, so it is dropped outside our lock.
        drop(replaced);
    }

    /// Host side: queue a command and wake the app, which runs it at the start of its next frame.
    pub fn send(&self, command: HostCommand) {
        let ctx = {
            let mut queues = self.lock();
            if queues.detached {
                return;
            }
            queues.commands.push_back(command);
            queues.ctx.clone()
        };
        // Outside the lock: `request_repaint` takes egui's own lock and may run callbacks, and a
        // host thread must not hold ours while it waits for that.
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    }

    /// App side: every command since the last call, oldest first.
    pub fn take_commands(&self) -> Vec<HostCommand> {
        self.lock().commands.drain(..).collect()
    }

    /// App side: tell the host something.
    pub fn emit(&self, event: HostEvent) {
        let mut queues = self.lock();
        if !queues.detached {
            queues.events.push(event);
        }
    }

    /// Host side: every event since the last call, oldest first.
    pub fn take_events(&self) -> Vec<HostEvent> {
        std::mem::take(&mut self.lock().events)
    }

    /// Host side, when the editor is destroyed: drops the app's context and both queues, and makes
    /// every later call do nothing. The context would otherwise outlive the stopped app through the
    /// host's copy of the bridge.
    pub fn detach(&self) {
        let (ctx, commands, events) = {
            let mut queues = self.lock();
            queues.detached = true;
            (queues.ctx.take(), std::mem::take(&mut queues.commands), std::mem::take(&mut queues.events))
        };
        drop((ctx, commands, events));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;
    use crate::host::wire::JumpTarget;

    fn open(name: &str) -> HostCommand {
        HostCommand::Open { name: name.into(), bytes: vec![1, 2], options: OpenOptions::default() }
    }

    /// A context whose repaint callback counts requests. The callback lives as long as the context,
    /// so the counter's strong count shows whether the context is still alive.
    fn counting_context() -> (egui::Context, Arc<AtomicUsize>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let ctx = egui::Context::default();
        let counter = wakes.clone();
        ctx.set_request_repaint_callback(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        (ctx, wakes)
    }

    #[test]
    fn events_serialise_to_the_embed_api_shapes() {
        let cases = [
            (HostEvent::Opened { pages: 3 }, json!({"type": "opened", "pages": 3})),
            (HostEvent::OpenFailed { message: "m".into() }, json!({"type": "openFailed", "message": "m"})),
            (HostEvent::PageChanged { page: 2 }, json!({"type": "pageChanged", "page": 2})),
            (
                HostEvent::Selection { text: "a b".into(), copy_text: "a\nb".into(), page: Some(1), x: 10.5, y: 20.0 },
                json!({"type": "selection", "text": "a b", "copyText": "a\nb", "page": 1, "x": 10.5, "y": 20.0}),
            ),
            (
                HostEvent::Selection { text: "a".into(), copy_text: "a".into(), page: None, x: 0.0, y: 0.0 },
                json!({"type": "selection", "text": "a", "copyText": "a", "page": null, "x": 0.0, "y": 0.0}),
            ),
            (HostEvent::SelectionCleared, json!({"type": "selectionCleared"})),
            (HostEvent::MarkActivated { id: "t1".into() }, json!({"type": "markActivated", "id": "t1"})),
            (
                HostEvent::JumpNotFound { target: JumpTarget::Text { text: "x".into(), page: None, occurrence: 0 } },
                json!({"type": "jumpNotFound", "target": {"kind": "text", "text": "x", "page": null, "occurrence": 0}}),
            ),
            (
                HostEvent::JumpNotFound { target: JumpTarget::Page { page: u32::MAX } },
                json!({"type": "jumpNotFound", "target": {"kind": "page", "page": 4294967295u32}}),
            ),
            (HostEvent::ModeChanged { mode: EditorMode::Edit }, json!({"type": "modeChanged", "mode": "edit"})),
            (HostEvent::ModeChanged { mode: EditorMode::View }, json!({"type": "modeChanged", "mode": "view"})),
            (HostEvent::DirtyChanged { dirty: true }, json!({"type": "dirtyChanged", "dirty": true})),
            // The bytes travel outside JSON (the embed crate sets them on the event object).
            (HostEvent::SaveRequested { file_name: "a.docx".into(), bytes: vec![1, 2] }, json!({"type": "saveRequested", "fileName": "a.docx"})),
            (HostEvent::SaveFailed { message: "m".into() }, json!({"type": "saveFailed", "message": "m"})),
            (HostEvent::EditRefused { code: "command_not_allowed".into() }, json!({"type": "editRefused", "code": "command_not_allowed"})),
            (
                HostEvent::CommentRequested { text: "a b".into(), copy_text: "a\nb".into(), page: Some(1), x: 1.0, y: 2.0 },
                json!({"type": "commentRequested", "text": "a b", "copyText": "a\nb", "page": 1, "x": 1.0, "y": 2.0}),
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(serde_json::to_value(&event).unwrap(), expected, "{event:?}");
        }
    }

    /// `editRefused` carries exactly the embed API's eleven codes (`EditRefusalCode`), one per
    /// engine refusal, in the API's order.
    #[test]
    fn edit_refusals_serialise_to_the_embed_api_s_codes() {
        let api = [
            "command_not_allowed",
            "decision_not_allowed",
            "paragraph_break_not_supported",
            "character_not_allowed",
            "outside_editable_story",
            "invalid_edit_session",
            "unknown_command",
            "invalid_parameters",
            "not_available",
            "document_protected",
            "command_failed",
        ];
        let codes: Vec<&str> = wordcraft_engine::edit_mode::Refusal::ALL.iter().map(|refusal| refusal.code()).collect();
        assert_eq!(codes, api);
        for code in api {
            let event = HostEvent::EditRefused { code: code.to_string() };
            assert_eq!(serde_json::to_value(&event).unwrap(), json!({"type": "editRefused", "code": code}));
        }
    }

    /// A save whose value cannot be made for the page is answered `saveFailed`; other events are
    /// left to the caller, and a value that can be made is passed through untouched.
    #[test]
    fn a_save_that_cannot_be_handed_over_is_answered_save_failed() {
        let save = HostEvent::SaveRequested { file_name: "a.docx".into(), bytes: vec![1, 2] };
        let failing_saves = |event: &HostEvent| match event {
            HostEvent::SaveRequested { .. } => Err("no memory"),
            other => Ok(other.clone()),
        };
        assert_eq!(encode_for_host(&save, failing_saves), Ok(HostEvent::SaveFailed { message: SAVE_NOT_HANDED_OVER.into() }));
        assert_eq!(encode_for_host(&save, |event: &HostEvent| Ok::<_, &str>(event.clone())), Ok(save.clone()));
        let page = HostEvent::PageChanged { page: 1 };
        assert_eq!(encode_for_host(&page, |_: &HostEvent| Err::<HostEvent, _>("broken")), Err("broken"), "only a save is answered another way");
        let mut attempts = 0;
        let nothing_works = |_: &HostEvent| {
            attempts += 1;
            Err::<HostEvent, _>("broken")
        };
        assert_eq!(encode_for_host(&save, nothing_works), Err("broken"));
        assert_eq!(attempts, 2, "one attempt for the save, one for its saveFailed");
    }

    #[test]
    fn the_bridge_keeps_order_and_is_shared_by_its_clones() {
        let host_side = HostBridge::new();
        let app_side = host_side.clone();
        host_side.send(open("a.docx"));
        host_side.send(open("b.docx"));
        assert_eq!(app_side.take_commands(), vec![open("a.docx"), open("b.docx")]);
        assert!(app_side.take_commands().is_empty());
        app_side.emit(HostEvent::PageChanged { page: 1 });
        app_side.emit(HostEvent::SelectionCleared);
        assert_eq!(host_side.take_events(), vec![HostEvent::PageChanged { page: 1 }, HostEvent::SelectionCleared]);
        assert!(host_side.take_events().is_empty());
    }

    #[test]
    fn a_command_wakes_the_app_once_it_has_a_context() {
        let bridge = HostBridge::new();
        let (ctx, wakes) = counting_context();
        bridge.send(open("early.docx"));
        assert_eq!(wakes.load(Ordering::SeqCst), 0, "no app has run a frame yet");
        bridge.set_context(ctx);
        bridge.send(open("late.docx"));
        assert!(wakes.load(Ordering::SeqCst) > 0);
        assert_eq!(bridge.take_commands(), vec![open("early.docx"), open("late.docx")], "an early command is kept");
    }

    #[test]
    fn a_poisoned_lock_does_not_stop_the_bridge() {
        let bridge = HostBridge::new();
        bridge.send(open("a.docx"));
        let poisoner = bridge.clone();
        let outcome = std::thread::spawn(move || {
            let _guard = poisoner.inner.lock();
            std::panic::resume_unwind(Box::new("poison the lock"));
        })
        .join();
        assert!(outcome.is_err() && bridge.inner.is_poisoned(), "the lock is poisoned");
        bridge.send(open("b.docx"));
        bridge.emit(HostEvent::SelectionCleared);
        assert_eq!(bridge.take_commands(), vec![open("a.docx"), open("b.docx")]);
        assert_eq!(bridge.take_events(), vec![HostEvent::SelectionCleared]);
        bridge.detach();
    }

    #[test]
    fn a_detached_bridge_frees_the_context_and_carries_nothing() {
        let bridge = HostBridge::new();
        let (ctx, wakes) = counting_context();
        bridge.set_context(ctx);
        assert_eq!(Arc::strong_count(&wakes), 2, "the bridge keeps the context alive");
        bridge.send(open("a.docx"));
        bridge.emit(HostEvent::SelectionCleared);
        let app_side = bridge.clone();

        bridge.detach();
        assert_eq!(Arc::strong_count(&wakes), 1, "detaching drops the context");
        assert!(app_side.take_commands().is_empty(), "queued commands are dropped");
        assert!(bridge.take_events().is_empty(), "queued events are dropped");

        let wakes_before = wakes.load(Ordering::SeqCst);
        bridge.send(open("b.docx"));
        app_side.emit(HostEvent::PageChanged { page: 2 });
        let (late, late_wakes) = counting_context();
        app_side.set_context(late);
        bridge.send(open("c.docx"));
        assert_eq!(wakes.load(Ordering::SeqCst), wakes_before);
        assert_eq!(late_wakes.load(Ordering::SeqCst), 0);
        assert_eq!(Arc::strong_count(&late_wakes), 1, "a detached bridge does not keep a late context");
        assert!(app_side.take_commands().is_empty(), "a detached bridge queues no command");
        assert!(bridge.take_events().is_empty(), "a detached bridge queues no event");
        bridge.detach();
    }
}

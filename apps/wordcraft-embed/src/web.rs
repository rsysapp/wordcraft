//! `EmbeddedEditor`: the API a host page calls (wasm-bindgen). The app runs inside eframe's
//! `WebRunner`, out of JavaScript's reach, so every call goes through the `HostBridge`. Values
//! arrive as JavaScript values and are read as JSON by the bridge's parsers (`host::wire`), so a
//! malformed argument throws here and queues nothing.
//!
//! Release glue checks no argument types, so each one is checked here: a wrong type would
//! otherwise be coerced (a number as a file name) or fail later, inside event delivery.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast as _, JsValue};
use wordcraft_engine::Session;
use wordcraft_engine::doc::Document;
use wordcraft_ui_egui::host::{self, EmbedProfile, HostBridge, HostCommand, HostEvent, open, wire};
use wordcraft_ui_egui::{Services, WordApp};

#[wasm_bindgen(typescript_custom_section)]
const TS_TYPES: &str = crate::ts::TS_DECLARATIONS;

#[wasm_bindgen]
extern "C" {
    /// `open`'s optional third argument. A typed import, not `unchecked_param_type` on a `JsValue`:
    /// only an `Option` of a named type makes wasm-bindgen write `options?: OpenOptions`.
    #[wasm_bindgen(typescript_type = "OpenOptions")]
    pub type OpenOptionsArgument;
}

/// The host's listener, shared by `EmbeddedEditor` and the app's event delivery.
type Listener = Rc<RefCell<Option<js_sys::Function>>>;

/// The editor mounted on a host page's canvas.
#[wasm_bindgen]
pub struct EmbeddedEditor {
    runner: eframe::WebRunner,
    bridge: HostBridge,
    listener: Listener,
    destroyed: Cell<bool>,
}

#[wasm_bindgen]
impl EmbeddedEditor {
    /// Starts the editor on the canvas. Resolves once the first frame ran.
    #[wasm_bindgen(unchecked_return_type = "Promise<EmbeddedEditor>")]
    pub fn mount(
        #[wasm_bindgen(unchecked_param_type = "HTMLCanvasElement")] canvas: JsValue,
        #[wasm_bindgen(unchecked_param_type = "MountOptions")] options: JsValue,
    ) -> js_sys::Promise {
        wasm_bindgen_futures::future_to_promise(async move { start(canvas, options).await.map(JsValue::from) })
    }

    /// Opens a document, replacing any open one. Answers with an `opened` or `openFailed` event.
    pub fn open(
        &self,
        #[wasm_bindgen(unchecked_param_type = "Uint8Array")] bytes: JsValue,
        #[wasm_bindgen(js_name = "fileName", unchecked_param_type = "string")] file_name: JsValue,
        options: Option<OpenOptionsArgument>,
    ) -> Result<(), JsValue> {
        let file_name = file_name.as_string().ok_or_else(|| type_error("open: fileName must be a string"))?;
        let bytes = bytes.dyn_into::<js_sys::Uint8Array>().map_err(|_| type_error("open: bytes must be a Uint8Array"))?;
        // Options are optional (API 0.3): a host that sends none gets the display API 0.2 had.
        let options = match options {
            Some(options) => wire::parse_open_options(&json_of(options.as_ref())?).map_err(|e| type_error(&e))?,
            None => wire::OpenOptions::default(),
        };
        // Nothing is copied for an editor that will never read it.
        if self.destroyed.get() {
            return Ok(());
        }
        // The length is read once. A file over the cap is refused without copying any of it, and an
        // empty one (also what a detached buffer reads as) is never touched; the rest is copied into
        // wasm memory once, then moved into the queue.
        let len = byte_length(&bytes)?;
        self.send(open::open_command(file_name, len, options, |len| copy_bytes(&bytes, len))?);
        Ok(())
    }

    #[wasm_bindgen(js_name = jumpTo)]
    pub fn jump_to(&self, #[wasm_bindgen(unchecked_param_type = "JumpTarget")] target: JsValue) -> Result<(), JsValue> {
        check_counts(&target, &["page", "occurrence"], "jumpTo")?;
        let target = wire::parse_jump_target(&json_of(&target)?).map_err(|e| type_error(&e))?;
        self.send(HostCommand::JumpTo(target));
        Ok(())
    }

    /// Replaces every host mark. An empty array clears them.
    #[wasm_bindgen(js_name = setMarks)]
    pub fn set_marks(&self, #[wasm_bindgen(unchecked_param_type = "Mark[]")] marks: JsValue) -> Result<(), JsValue> {
        check_mark_pages(&marks)?;
        let marks = wire::parse_marks(&json_of(&marks)?).map_err(|e| type_error(&e))?;
        self.send(HostCommand::SetMarks(marks));
        Ok(())
    }

    #[wasm_bindgen(js_name = setTheme)]
    pub fn set_theme(&self, #[wasm_bindgen(unchecked_param_type = "Theme")] theme: JsValue) -> Result<(), JsValue> {
        let theme = wire::parse_theme(&json_of(&theme)?).map_err(|e| type_error(&e))?;
        self.send(HostCommand::SetTheme(theme));
        Ok(())
    }

    /// Switches the open document to view or edit mode; answered by `modeChanged`.
    #[wasm_bindgen(js_name = setMode)]
    pub fn set_mode(&self, #[wasm_bindgen(unchecked_param_type = "ModeRequest")] request: JsValue) -> Result<(), JsValue> {
        let request = wire::parse_mode_request(&json_of(&request)?).map_err(|e| type_error(&e))?;
        self.send(HostCommand::SetMode(request));
        Ok(())
    }

    /// Serialises the document; answered by `saveRequested` or `saveFailed`.
    #[wasm_bindgen(js_name = requestSave)]
    pub fn request_save(&self) {
        self.send(HostCommand::RequestSave);
    }

    /// The API's redaction step; this editor has no redactions, so it does nothing.
    #[wasm_bindgen(js_name = applyRedactions)]
    pub fn apply_redactions(&self) {}

    /// One listener; a second call replaces the first. Events are delivered after the frame that
    /// produced them.
    #[wasm_bindgen(js_name = setListener)]
    pub fn set_listener(&self, #[wasm_bindgen(unchecked_param_type = "(event: EditorEvent) => void")] listener: JsValue) -> Result<(), JsValue> {
        let listener = listener.dyn_into::<js_sys::Function>().map_err(|_| type_error("setListener: expected a function"))?;
        if !self.destroyed.get()
            && let Ok(mut slot) = self.listener.try_borrow_mut()
        {
            *slot = Some(listener);
        }
        Ok(())
    }

    /// Stops the runner and frees the canvas. The object is unusable afterwards: later calls queue
    /// nothing and deliver nothing (malformed arguments still throw), and a second `destroy` does
    /// nothing.
    ///
    /// Throws nothing by itself: nothing here can fail on a sound instance. After a crash (a trap)
    /// it may throw, and after `free()` wasm-bindgen refuses the call, so a host calls it inside a
    /// `try` there. A crashed instance is discarded whole: the next editor is mounted from a fresh
    /// import of the glue (a new URL) and a new WebAssembly instance, never on this one (the
    /// embed docs, "Crashes").
    pub fn destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        // From here on nothing is queued or delivered: this drops the bridge's copy of the app's
        // egui context and anything still queued, so nothing of the editor outlives it through
        // this object, and the app's later events go nowhere.
        self.bridge.detach();
        // `try_borrow_mut`, not `replace`: this also runs from `Drop`, which must not panic. Event
        // delivery clones the listener out before calling it, so the cell is free here anyway.
        if let Ok(mut slot) = self.listener.try_borrow_mut() {
            *slot = None;
        }
        stop_later(self.runner.clone());
    }
}

/// wasm-bindgen's `free()`, `[Symbol.dispose]()` and garbage collection drop the object without
/// calling `destroy`. The runner's event listeners hold their own copies of it, so without this
/// the editor would keep rendering on the canvas, unreachable. `destroy` is idempotent, so
/// `destroy()` then `free()` is fine. `free()` may run inside a frame (from a focus handler), which
/// is why `destroy` stops the runner from a microtask.
impl Drop for EmbeddedEditor {
    fn drop(&mut self) {
        self.destroy();
    }
}

impl EmbeddedEditor {
    fn send(&self, command: HostCommand) {
        if !self.destroyed.get() {
            self.bridge.send(command);
        }
    }
}

async fn start(canvas: JsValue, options: JsValue) -> Result<EmbeddedEditor, JsValue> {
    let canvas = canvas.dyn_into::<web_sys::HtmlCanvasElement>().map_err(|_| type_error("mount: expected an HTMLCanvasElement"))?;
    let options = wire::parse_mount_options(&json_of(&options)?).map_err(|e| type_error(&e))?;
    // Routes the editor's own warnings to the console; a second mount keeps the first logger.
    eframe::WebLogger::init(log::LevelFilter::Warn).ok();
    // A fresh bridge per mount: a destroyed editor's bridge stays detached.
    let bridge = HostBridge::new();
    // Queued before the runner starts, so the first frame's `logic` applies it before anything is
    // painted: the canvas never shows the editor's own theme first.
    bridge.send(HostCommand::SetTheme(options.theme));
    let listener: Listener = Rc::new(RefCell::new(None));
    let mut resolve_first_frame = None;
    let first_frame = js_sys::Promise::new(&mut |resolve, _reject| resolve_first_frame = Some(resolve));
    let runner = eframe::WebRunner::new();
    let (app_bridge, app_listener) = (bridge.clone(), listener.clone());
    let started = runner
        .start(
            canvas,
            eframe::WebOptions::default(),
            Box::new(move |cc| {
                // Commands wake the app from now on; the theme queued above waits for its frame.
                app_bridge.set_context(cc.egui_ctx.clone());
                host::prepare_context(&cc.egui_ctx);
                let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(app_bridge.clone(), EmbedProfile::View);
                Ok(Box::new(EmbedShell { app, bridge: app_bridge, listener: app_listener, first_frame: resolve_first_frame }))
            }),
        )
        .await;
    let ready = match started {
        Ok(()) => wasm_bindgen_futures::JsFuture::from(first_frame).await.map(drop),
        Err(error) => Err(error),
    };
    if let Err(error) = ready {
        // No `EmbeddedEditor` exists to drop, so stop whatever the runner installed here, the same
        // way `destroy` does.
        bridge.detach();
        stop_later(runner);
        return Err(error);
    }
    Ok(EmbeddedEditor { runner, bridge, listener, destroyed: Cell::new(false) })
}

/// Stops `runner` from a microtask, after whatever is running now. Host code can run inside a
/// frame, while the runner is borrowed: eframe focuses and blurs its text agent mid-frame (focusin
/// and focusout bubble to the page) and opens links with `window.open`, which can fire blur.
/// `WebRunner::destroy` there would panic ("already borrowed"), and a panic aborts the module.
/// Microtasks run only once the frame has returned, and before the next frame or input event, so
/// the runner paints nothing more after the current task.
fn stop_later(runner: eframe::WebRunner) {
    wasm_bindgen_futures::spawn_local(async move { runner.destroy() });
}

/// The app the runner drives: WordCraft in view mode (edit mode on the host's `setMode`), plus
/// handing its events to the host.
struct EmbedShell {
    app: WordApp,
    bridge: HostBridge,
    listener: Listener,
    /// Resolves `mount`'s promise once the first frame ran.
    first_frame: Option<js_sys::Function>,
}

impl eframe::App for EmbedShell {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.app.raw_input_hook(raw);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // A host page opens documents through `open`; files dropped on the canvas are ignored.
        ctx.input_mut(|input| input.raw.dropped_files.clear());
        self.app.logic(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
        if let Some(resolve) = self.first_frame.take()
            && let Err(error) = resolve.call0(&JsValue::NULL)
        {
            log::error!("embed: mount could not resolve: {error:?}");
        }
        let events = self.bridge.take_events();
        if !events.is_empty() {
            // A task, not a call: the listener runs after this frame returned and the runner is no
            // longer borrowed, so it may call back into the editor, even `destroy()`.
            wasm_bindgen_futures::spawn_local(deliver(self.listener.clone(), events));
        }
    }

    // Nothing is read from or written to the host page's storage: no settings, no egui memory.
    // (`save` keeps the trait's default, which stores nothing.)
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

async fn deliver(listener: Listener, events: Vec<HostEvent>) {
    for event in events {
        // Read per event and cloned out of the cell before the call: the listener may replace
        // itself or destroy the editor, after which nothing more is delivered.
        let Some(listener) = listener.try_borrow().ok().and_then(|slot| slot.clone()) else { return };
        // A save that cannot be handed over is answered `saveFailed`: a save never goes unanswered.
        let value = match host::encode_for_host(&event, event_value) {
            Ok(value) => value,
            Err(error) => {
                log::error!("embed: an event could not be delivered: {error}");
                continue;
            }
        };
        if let Err(error) = listener.call1(&JsValue::NULL, &value) {
            log::warn!("embed: the event listener threw: {error:?}");
        }
    }
}

/// The event as the object the host's listener receives. A save's bytes travel as a `Uint8Array`
/// the host owns (a copy outside the module's memory), not as JSON numbers. Every step that can
/// fail answers `Err`, allocating the array included: it goes through a `catch` import, because a
/// `RangeError` thrown through Rust frames would skip their cleanup and never reach the caller.
fn event_value(event: &HostEvent) -> Result<JsValue, String> {
    let json = serde_json::to_string(event).map_err(|error| format!("an event could not be encoded: {error}"))?;
    let value = js_sys::JSON::parse(&json).map_err(|error| format!("an event could not be read back: {error:?}"))?;
    if let HostEvent::SaveRequested { bytes, .. } = event {
        let length = u32::try_from(bytes.len()).map_err(|_| format!("a save of {} bytes is too large to hand over", bytes.len()))?;
        let array: js_sys::Uint8Array =
            OwnedBytes::new(length).map(Into::into).map_err(|error| format!("{length} bytes could not be allocated for a save: {error:?}"))?;
        // Same length by construction, so `copy_from`'s length check holds; its view of the
        // module's memory allocates nothing.
        array.copy_from(bytes);
        js_sys::Reflect::set(&value, &JsValue::from_str("bytes"), &array)
            .map_err(|error| format!("a save's bytes could not be attached: {error:?}"))?;
    }
    Ok(value)
}

/// A JavaScript value as JSON, for the bridge's parsers.
fn json_of(value: &JsValue) -> Result<String, JsValue> {
    js_sys::JSON::stringify(value).ok().and_then(|json| JsValue::from(json).as_string()).ok_or_else(|| type_error("expected a plain object or array"))
}

/// Refuses a page or occurrence that is not a whole number of zero or more, on the JavaScript
/// value. It must happen before [`json_of`]: `JSON.stringify` writes NaN and ±Infinity as `null`,
/// which the parsers read as "no page", so a page-scoped jump or mark would quietly search the
/// whole document. (The parsers refuse fractions and negatives too; this says so earlier.) Whole
/// numbers of any size pass: the parsers saturate them, so they answer `jumpNotFound`. A value that
/// is not an object is left to the parsers, which refuse its shape.
fn check_counts(value: &JsValue, keys: &[&str], what: &str) -> Result<(), JsValue> {
    if !value.is_object() {
        return Ok(());
    }
    for key in keys {
        let field = js_sys::Reflect::get(value, &JsValue::from_str(key)).map_err(|_| type_error(&format!("{what}: {key} could not be read")))?;
        if let Some(number) = field.as_f64()
            && !(number.is_finite() && number.fract() == 0.0 && number >= 0.0)
        {
            return Err(type_error(&format!("{what}: {key} {number} is not a whole number of zero or more")));
        }
    }
    Ok(())
}

/// `check_counts` for each mark's page. Every read goes through `Reflect::get`, which turns a
/// throwing getter or a revoked Proxy into an error here: an exception thrown through Rust code would
/// skip its cleanup and leave this object borrowed. Only the first `MAX_MARKS` marks are read, the
/// ones `parse_marks` keeps, so a wild `length` costs nothing. A value without a usable `length` is
/// left to the parser, which refuses anything but an array.
fn check_mark_pages(marks: &JsValue) -> Result<(), JsValue> {
    if !marks.is_object() {
        return Ok(());
    }
    let length = js_sys::Reflect::get(marks, &JsValue::from_str("length")).map_err(|_| type_error("setMarks: length could not be read"))?;
    let Some(length) = length.as_f64().filter(|length| length.is_finite() && *length >= 0.0) else { return Ok(()) };
    // A float-to-integer `as` saturates; the bound is far below u32::MAX.
    let count = length.min(wire::MAX_MARKS as f64) as u32;
    for index in 0..count {
        let mark = js_sys::Reflect::get_u32(marks, index).map_err(|_| type_error(&format!("setMarks: mark {index} could not be read")))?;
        check_counts(&mark, &["page"], "setMarks")?;
    }
    Ok(())
}

#[wasm_bindgen]
extern "C" {
    /// `Uint8Array.prototype.set.call(target, source)`: `target.set(source)` on a view of wasm memory.
    /// js-sys's own `copy_to`/`to_vec` call this without `catch`; this one returns what it throws. The
    /// glue builds the view of `target` and calls `set` inside one catch wrapper, with no Rust code in
    /// between, so a memory growth cannot detach the view between the two.
    #[wasm_bindgen(catch, js_namespace = Uint8Array, js_name = "prototype.set.call")]
    fn set_into(target: &mut [u8], source: &js_sys::Uint8Array) -> Result<(), JsValue>;

    /// A `Uint8Array` made by [`OwnedBytes::new`].
    #[wasm_bindgen(extends = js_sys::Uint8Array, js_name = Uint8Array)]
    type OwnedBytes;

    /// `new Uint8Array(length)`, answering what it throws (a `RangeError` when the page cannot
    /// allocate it). js-sys's own constructors do not catch.
    #[wasm_bindgen(catch, constructor, js_class = "Uint8Array")]
    fn new(length: u32) -> Result<OwnedBytes, JsValue>;
}

/// The array's `length`, read once. Through `Reflect::get`, so a subclass's throwing getter is an
/// error here.
fn byte_length(bytes: &js_sys::Uint8Array) -> Result<usize, JsValue> {
    let length = js_sys::Reflect::get(bytes, &JsValue::from_str("length")).map_err(|_| type_error("open: the length of bytes could not be read"))?;
    match length.as_f64() {
        // A float-to-integer `as` saturates, and anything past the size cap is refused uncopied.
        Some(length) if length.is_finite() && length.fract() == 0.0 && length >= 0.0 => Ok(length as usize),
        _ => Err(type_error("open: bytes has no usable length")),
    }
}

/// `len` bytes of `bytes`, copied into a buffer of exactly that size.
///
/// Every JavaScript call that could throw here is a `catch` import. An exception thrown through a
/// non-`catch` import unwinds past this method's Rust frames without running their cleanup (there is
/// no unwinding on wasm32), and that cleanup includes releasing the borrow of `self` that
/// wasm-bindgen takes for the call: `free()` would then throw "attempted to take ownership ... while
/// borrowed" and garbage collection would never run `Drop`. js-sys's `to_vec` is such a call: `set`
/// throws for a detached or transferred buffer, and its `copy_to` asserts (an abort) if `length`
/// changes between its two reads. Here `length` is read once, the copy throws into an `Err`, and a
/// source longer than the view (a length that changed since) is refused by `set` with a RangeError,
/// also an `Err`. A shorter one leaves the rest zeroed.
fn copy_bytes(bytes: &js_sys::Uint8Array, len: usize) -> Result<Vec<u8>, JsValue> {
    let mut copy = vec![0; len];
    set_into(&mut copy, bytes).map_err(|_| type_error("open: bytes could not be read (a detached or changed buffer)"))?;
    Ok(copy)
}

/// What a malformed argument throws.
fn type_error(message: &str) -> JsValue {
    js_sys::TypeError::new(message).into()
}

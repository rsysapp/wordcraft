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
use wordcraft_ui_egui::host::{EmbedProfile, HostBridge, HostCommand, HostEvent, wire};
use wordcraft_ui_egui::{Services, WordApp};

#[wasm_bindgen(typescript_custom_section)]
const TS_TYPES: &str = crate::ts::TS_DECLARATIONS;

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
    ) -> Result<(), JsValue> {
        let file_name = file_name.as_string().ok_or_else(|| type_error("open: fileName must be a string"))?;
        let bytes = bytes.dyn_into::<js_sys::Uint8Array>().map_err(|_| type_error("open: bytes must be a Uint8Array"))?;
        // Copied into wasm memory once, then moved into the queue.
        self.send(HostCommand::Open { name: file_name, bytes: bytes.to_vec() });
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
        if js_sys::Array::is_array(&marks) {
            for mark in marks.unchecked_ref::<js_sys::Array>().iter() {
                check_counts(&mark, &["page"], "setMarks")?;
            }
        }
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
    pub fn destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        self.runner.destroy();
        // The runner has dropped the app; this drops the bridge's copy of its egui context and
        // anything still queued, so nothing of the editor outlives it through this object.
        self.bridge.detach();
        // `try_borrow_mut`, not `replace`: this also runs from `Drop`, which must not panic. Event
        // delivery clones the listener out before calling it, so the cell is free here anyway.
        if let Ok(mut slot) = self.listener.try_borrow_mut() {
            *slot = None;
        }
    }
}

/// wasm-bindgen's `free()`, `[Symbol.dispose]()` and garbage collection drop the object without
/// calling `destroy`. The runner's event listeners hold their own copies of it, so without this
/// the editor would keep rendering on the canvas, unreachable. `destroy` is idempotent, so
/// `destroy()` then `free()` is fine. Its runner calls cannot meet a borrowed runner: host code
/// never runs inside a frame (events reach the listener in a later task).
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
                let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(app_bridge.clone(), EmbedProfile::View);
                Ok(Box::new(EmbedShell { app, bridge: app_bridge, listener: app_listener, first_frame: resolve_first_frame }))
            }),
        )
        .await;
    if let Err(error) = started {
        // No `EmbeddedEditor` exists to drop, so unsubscribe whatever the runner installed here.
        runner.destroy();
        bridge.detach();
        return Err(error);
    }
    wasm_bindgen_futures::JsFuture::from(first_frame).await?;
    Ok(EmbeddedEditor { runner, bridge, listener, destroyed: Cell::new(false) })
}

/// The app the runner drives: WordCraft in view mode, plus handing its events to the host.
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
        let value = match serde_json::to_string(&event).map(|json| js_sys::JSON::parse(&json)) {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                log::error!("embed: an event could not be read back: {error:?}");
                continue;
            }
            Err(error) => {
                log::error!("embed: an event could not be encoded: {error}");
                continue;
            }
        };
        if let Err(error) = listener.call1(&JsValue::NULL, &value) {
            log::warn!("embed: the event listener threw: {error:?}");
        }
    }
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

/// What a malformed argument throws.
fn type_error(message: &str) -> JsValue {
    js_sys::TypeError::new(message).into()
}

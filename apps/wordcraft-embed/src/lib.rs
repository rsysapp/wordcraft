//! WordCraft's embed build: a wasm-bindgen module a host page mounts on its own `<canvas>` and
//! drives through `EmbeddedEditor` (docs/embed.md): view mode, edit mode with save (API 0.2), and the
//! display, zoom, scroll, find and marks by location of API 0.3. The behaviour lives
//! in `wordcraft_ui_egui::host`; this crate is the JavaScript-facing glue, and only the browser
//! build has any.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod ts;

#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(target_arch = "wasm32")]
pub use web::EmbeddedEditor;

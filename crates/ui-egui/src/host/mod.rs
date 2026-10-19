//! The embed build: a host page drives WordCraft through a [`HostBridge`] (commands in, events
//! out). Upstream code calls into this module at a few points marked `// embed:`; everything else
//! lives here so upstream merges stay easy.

pub mod bridge;
// embed: find in the clean display (embed API 0.4.1).
pub(crate) mod clean_find;
// embed: edit mode (embed API 0.2).
pub(crate) mod edit;
pub(crate) mod marks;
pub mod open;
// Public only for the embed crate and the tests; not part of the desktop or web app's API.
#[doc(hidden)]
pub mod profile;
#[doc(hidden)]
pub mod state;
pub mod text_match;
#[doc(hidden)]
pub mod view;
// embed: the view in document terms (embed API 0.3).
pub(crate) mod viewport;
pub mod wire;

pub use bridge::{Cause, EditorMode, HostBridge, HostCommand, HostEvent, encode_for_host};
#[doc(hidden)]
pub use profile::EmbedProfile;
#[doc(hidden)]
pub use state::HostState;
pub use wire::{
    BlockRef, Display, FindRequest, FindStepSpec, FitWidth, JumpTarget, LocateRequest, LocationSpec, MarkSpec, MarkStyle, MountOptions, OpenOptions,
    ScrollRequest, ThemeSpec, ZoomRequest, ZoomValue,
};

/// Set up the context an embed build's app runs in; call it once the runner made the context.
///
/// egui sends `ViewportCommand::SetTheme` whenever the theme preference changes, so that a native
/// window's decorations follow it. A host page's canvas has no window decorations, and eframe's web
/// backend cannot act on the command: it only logs a warning into the host page's console, once per
/// `setTheme`. The preference itself is still set (`state::set_theme`), because egui's style for the
/// chosen theme depends on it.
#[doc(hidden)]
pub fn prepare_context(ctx: &egui::Context) {
    ctx.options_mut(|options| options.sync_window_theme = false);
}

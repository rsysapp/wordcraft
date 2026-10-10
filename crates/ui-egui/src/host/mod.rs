//! The embed build: a host page drives WordCraft through a [`HostBridge`] (commands in, events
//! out). Upstream code calls into this module at a few points marked `// embed:`; everything else
//! lives here so upstream merges stay easy.

pub mod bridge;
pub mod open;
// Public only for the embed crate and the tests; not part of the desktop or web app's API.
#[doc(hidden)]
pub mod profile;
#[doc(hidden)]
pub mod state;
pub mod text_match;
#[doc(hidden)]
pub mod view;
pub mod wire;

pub use bridge::{HostBridge, HostCommand, HostEvent};
#[doc(hidden)]
pub use profile::EmbedProfile;
#[doc(hidden)]
pub use state::HostState;
pub use wire::{JumpTarget, MarkSpec, MountOptions, ThemeSpec};

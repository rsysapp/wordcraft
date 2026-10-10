//! The embed build: a host page drives WordCraft through a [`HostBridge`] (commands in, events
//! out). Upstream code calls into this module at a few points marked `// embed:`; everything else
//! lives here so upstream merges stay easy.

pub mod bridge;
pub mod text_match;
pub mod wire;

pub use bridge::{HostBridge, HostCommand, HostEvent};
pub use wire::{JumpTarget, MarkSpec, MountOptions, ThemeSpec};

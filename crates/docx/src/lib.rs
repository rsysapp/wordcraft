//! WordCraft DOCX reader and writer (ECMA-376 / ISO/IEC 29500 WordprocessingML).
//!
//! [`read`] turns a `.docx` package into a [`wordcraft_doc::Document`]; [`write`] does the
//! reverse. Both are written from the public specification. The reader is lenient (unknown
//! markup is skipped, bad numbers fall back to defaults) and bounded (zip entry sizes, total
//! decompressed size, XML depth and element counts are capped), so hostile input yields an
//! error or a best-effort document, never a panic.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod package;
// embed: the writer that keeps the source package.
mod preserve;
mod read;
mod units;
mod write;
mod xml;

// embed: the edited-against-base comparison a host's server runs on every save.
pub mod inspect;

// embed: a caller with less memory or stack than the desktop reads within tighter limits.
pub use package::ReadLimits;
pub use read::{read, read_with_limits};
pub use write::write;
// embed: where a document read from a package keeps it.
pub use preserve::SOURCE_PACKAGE;

/// Errors from reading or writing a DOCX package.
#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum DocxError {
    /// The bytes are not a readable zip archive.
    #[error("not a valid zip package: {0}")]
    Zip(String),
    /// A required part is absent.
    #[error("missing part: {0}")]
    MissingPart(String),
    /// A part is not well-formed XML.
    #[error("malformed XML: {0}")]
    Xml(String),
    /// The input exceeds a safety limit (size, nesting, element count).
    #[error("limit exceeded: {0}")]
    Limit(String),
    /// The package is readable but isn't a WordprocessingML document.
    #[error("not a Word document: {0}")]
    NotWord(String),
    /// embed: a save the preserving writer cannot write without changing something outside the
    /// tracked changes; the message says what, in words a person can act on.
    #[error("{0}")]
    Preserve(String),
}

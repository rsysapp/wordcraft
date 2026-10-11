//! embed: a writer that keeps the source package (filled in by the tasks that follow). A document
//! read from a Word package carries that package under [`SOURCE_PACKAGE`] and each paragraph where
//! it came from (`Paragraph::origin`).

/// The key in `Document::passthrough` under which a document read from a Word package keeps the
/// package's bytes. Not a part name: nothing writes it as a part.
pub const SOURCE_PACKAGE: &str = "wordcraft:source-package";

//! embed: a writer that keeps the source package (filled in by the tasks that follow). A document
//! read from a Word package carries that package under [`SOURCE_PACKAGE`] and each paragraph where
//! it came from (`Paragraph::origin`).

// embed: the scan is read only by the preserving writer (Task 9 removes the allowance); what the
// scan reads (the tree, `Package::tree`) counts as used through it.
#[cfg_attr(not(test), expect(dead_code, reason = "embed: read by the preserving writer, which is not written yet (Task 9)"))]
mod scan;
pub(crate) mod tree;

/// The key in `Document::passthrough` under which a document read from a Word package keeps the
/// package's bytes. Not a part name: nothing writes it as a part.
pub const SOURCE_PACKAGE: &str = "wordcraft:source-package";

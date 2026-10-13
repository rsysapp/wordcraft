//! embed: a writer that keeps the source package (filled in by the tasks that follow). A document
//! read from a Word package carries that package under [`SOURCE_PACKAGE`] and each paragraph where
//! it came from (`Paragraph::origin`).

// embed: the scan is read only by the preserving writer (Task 9 removes the allowance); what the
// scan reads (the tree, `Package::tree`) counts as used through it.
#[cfg_attr(not(test), expect(dead_code, reason = "embed: read by the preserving writer, which is not written yet (Task 9)"))]
mod scan;
// embed: read only by the preserving writer (Task 9 removes the allowance).
#[cfg_attr(not(test), expect(dead_code, reason = "embed: read by the preserving writer, which is not written yet (Task 9)"))]
mod plan;
// embed: read only by the preserving writer (Task 9 removes the allowance).
#[cfg_attr(not(test), expect(dead_code, reason = "embed: read by the preserving writer, which is not written yet (Task 9)"))]
mod emit;
pub(crate) mod tree;

/// The key in `Document::passthrough` under which a document read from a Word package keeps the
/// package's bytes. Not a part name: nothing writes it as a part.
pub const SOURCE_PACKAGE: &str = "wordcraft:source-package";

/// Why a save cannot be written without changing something outside the tracked changes. Shown to
/// the person saving, so in plain words; part names say where.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(dead_code, reason = "embed: the reasons the writer and its checks give (Tasks 6 and 9) are not made yet")]
pub(crate) enum Unpreservable {
    NoSource,
    Structure,
    Formatting,
    Untracked,
    Altered,
    InsertedObject,
    TooManyChanges,
    Unmapped {
        part: String,
    },
    Locked {
        part: String,
    },
    SplitWrapper {
        part: String,
    },
    Outside(&'static str),
    NotStory {
        part: String,
    },
    Namespace {
        part: String,
    },
    Verification(String),
    /// embed: something the reader now reads that this writer was not built to rewrite (an object
    /// of a kind it does not know, a move made in the session): refused rather than guessed.
    Unsupported(&'static str),
}

impl std::fmt::Display for Unpreservable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unpreservable::NoSource => write!(f, "the document was not read from a Word file that can be read again"),
            Unpreservable::Structure => write!(f, "paragraphs were added, removed or moved in a way a Word file cannot record as tracked changes"),
            Unpreservable::Formatting => write!(f, "formatting or paragraph settings changed, which a Word file cannot record as tracked changes"),
            Unpreservable::Untracked => write!(f, "text changed without a tracked change"),
            Unpreservable::Altered => write!(f, "an earlier tracked change was altered"),
            Unpreservable::InsertedObject => write!(f, "an object was inserted"),
            Unpreservable::TooManyChanges => write!(f, "there are too many changes to check at once; save smaller sets of changes"),
            Unpreservable::Unmapped { part } => write!(f, "a paragraph in {part} uses markup that cannot be edited yet"),
            Unpreservable::Locked { part } => write!(f, "the change touches a field, an equation or alternative content in {part}"),
            Unpreservable::SplitWrapper { part } => write!(f, "the change would split a hyperlink or content control in {part}"),
            Unpreservable::Outside(what) => write!(f, "{what} changed"),
            Unpreservable::NotStory { part } => write!(f, "text in {part} cannot be edited here"),
            Unpreservable::Namespace { part } => write!(f, "{part} is written in a form this writer does not edit"),
            Unpreservable::Verification(detail) => write!(f, "the saved file would differ outside the tracked changes ({detail})"),
            Unpreservable::Unsupported(what) => write!(f, "{what} cannot be saved as a tracked change yet"),
        }
    }
}

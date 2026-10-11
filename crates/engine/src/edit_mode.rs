//! embed: what an edit session of the embed build may run. Every change there must be a tracked
//! change, and the engine tracks typed and deleted text only (no formatting revisions), so text
//! entry, deletion, a tab character, plain paste, undo and redo are allowed; deciding tracked
//! changes only for a session that may decide them. A host page sets the rules
//! (`Session::edit_rules`).

use serde_json::{Value, json};

/// Typing, deleting and moving through tracked changes. Not `text.backTab` (Shift+Tab changes list
/// levels and indents, which is formatting), not rich paste. `text.tab` is not run as itself either:
/// in a table it moves between cells and adds rows, at a list item's start it changes the level,
/// over several paragraphs it indents; an edit session runs it as typing a tab (`TAB`).
///
/// Not `text.newParagraph` yet: the Word writer does not record an inserted paragraph mark, so a
/// split would be saved as an untracked change. Enter joins this list once the writer records it
/// (fidelity plan, Task 6 and Ruling 6C-10). Typed or pasted text that would start a new paragraph
/// is refused in an edit session for the same reason (`NEW_PARAGRAPH`).
pub const COMMANDS: &[&str] = &[
    "text.insert",
    "text.backspace",
    "text.delete",
    "text.deleteWordBack",
    "text.deleteWordForward",
    "text.lineBreak",
    "text.nbsp",
    "text.nbHyphen",
    "edit.cut",
    "edit.pasteText",
    "edit.undo",
    "edit.redo",
    "review.nextChange",
    "review.previousChange",
    "review.changes",
];

/// Deciding tracked changes.
pub const DECIDE: &[&str] = &["review.accept", "review.reject", "review.acceptAll", "review.rejectAll"];

/// Tab in an edit session: legal documents use tab characters for clause numbering and alignment,
/// so Tab types one, tracked like any typing (embed API "Keyboard in edit mode").
pub const TAB: &str = "text.tab";

/// What `TAB` runs in an edit session: the character alone, without autoformat.
pub fn tab_params() -> Value {
    json!({ "text": "\t", "raw": true })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditRules {
    pub may_decide: bool,
}

pub fn allows(id: &str, rules: EditRules) -> bool {
    crate::view_only::allows(id) || COMMANDS.contains(&id) || (rules.may_decide && DECIDE.contains(&id))
}

/// Why typed or pasted text holding a paragraph break is refused in an edit session.
pub const NEW_PARAGRAPH: &str = "a new paragraph is not available while editing in the embed build";

pub fn refusal(id: &str) -> String {
    format!("{id}: not available while editing in the embed build")
}

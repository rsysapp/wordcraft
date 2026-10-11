//! embed: what an edit session of the embed build may run. Every change there must be a tracked
//! change, and the engine tracks typed and deleted text only (no formatting revisions), so text
//! entry, deletion, a tab character, plain paste, undo and redo are allowed; deciding tracked
//! changes only for a session that may decide them. A host page starts a session with
//! `Session::begin_edit`, the only way into one.

use serde_json::{Value, json};
use wordcraft_doc::{Document, PartKind, StoryRef};

/// Typing, deleting and moving through tracked changes. Not `text.backTab` (Shift+Tab changes list
/// levels and indents, which is formatting), not rich paste. `text.tab` is not run as itself either:
/// in a table it moves between cells and adds rows, at a list item's start it changes the level,
/// over several paragraphs it indents; an edit session runs it as typing a tab (`TAB`).
///
/// Not `text.newParagraph` yet: the Word writer does not record an inserted paragraph mark, so a
/// split would be saved as an untracked change. Enter joins this list once the writer records it
/// (fidelity plan, Task 6 and Ruling 6C-10). Typed or pasted text that would start a new paragraph
/// is refused in an edit session for the same reason ([`Refusal::ParagraphBreakNotSupported`]).
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

/// Whether an edit session with `rules` runs `id` (`TAB` as a typed tab).
pub fn allows(id: &str, rules: EditRules) -> bool {
    crate::view_only::allows(id) || id == TAB || COMMANDS.contains(&id) || (rules.may_decide && DECIDE.contains(&id))
}

/// Why an edit session refused something. Each has a stable [`code`](Self::code) a host maps to
/// its own message; the engine's wording is for logs and agents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// The command is not on the edit allow-list.
    CommandNotAllowed,
    /// Accepting or rejecting, or redoing a decision, without the permission to decide.
    DecisionNotAllowed,
    /// Enter, or typed or pasted text that would start a new paragraph (Ruling 6C-10).
    ParagraphBreakNotSupported,
    /// Typed or pasted text holding a control character a reader would not see as typed text:
    /// a page or column break, a direction control, another control character.
    CharacterNotAllowed,
    /// The selection is in a comment or a text box: only the document's text, headers, footers
    /// and notes are edited in an edit session.
    OutsideEditableStory,
    /// `Session::begin_edit` without an author, or without a revision date in UTC whole seconds.
    InvalidEditSession,
}

impl Refusal {
    pub const ALL: [Refusal; 6] = [
        Refusal::CommandNotAllowed,
        Refusal::DecisionNotAllowed,
        Refusal::ParagraphBreakNotSupported,
        Refusal::CharacterNotAllowed,
        Refusal::OutsideEditableStory,
        Refusal::InvalidEditSession,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Refusal::CommandNotAllowed => "command_not_allowed",
            Refusal::DecisionNotAllowed => "decision_not_allowed",
            Refusal::ParagraphBreakNotSupported => "paragraph_break_not_supported",
            Refusal::CharacterNotAllowed => "character_not_allowed",
            Refusal::OutsideEditableStory => "outside_editable_story",
            Refusal::InvalidEditSession => "invalid_edit_session",
        }
    }

    pub const fn reason(self) -> &'static str {
        match self {
            Refusal::CommandNotAllowed => "it would make a change that is not a tracked change",
            Refusal::DecisionNotAllowed => "accepting or rejecting tracked changes needs the permission to decide them",
            Refusal::ParagraphBreakNotSupported => "paragraph breaks cannot be typed or pasted while editing yet",
            Refusal::CharacterNotAllowed => "the text holds a break or control character that cannot be typed while editing",
            Refusal::OutsideEditableStory => "only the document's text, headers, footers and notes can be edited; comments cannot",
            Refusal::InvalidEditSession => "an edit session needs an author and a revision date in UTC whole seconds",
        }
    }
}

/// A refusal of an edit session, as [`crate::CmdError::Editing`] carries it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{command}: not available while editing [{code}]: {reason}", code = .refusal.code(), reason = .refusal.reason())]
pub struct EditRefusal {
    pub refusal: Refusal,
    pub command: String,
}

/// The message a command off the allow-list is refused with.
pub fn refusal(id: &str) -> String {
    EditRefusal { refusal: Refusal::CommandNotAllowed, command: id.to_string() }.to_string()
}

/// Why typed or pasted `text` cannot go into an edit session, if it cannot.
pub fn check_text(text: &str) -> Result<(), Refusal> {
    if text.contains(['\r', '\u{2029}']) {
        return Err(Refusal::ParagraphBreakNotSupported);
    }
    if text.chars().any(not_typed) {
        return Err(Refusal::CharacterNotAllowed);
    }
    Ok(())
}

/// Characters the save inspector flags, or that are layout rather than text: control characters
/// other than tab, line break (`\n`, `\u{000B}`) and paragraph break (refused on its own), which
/// includes the page break (`\u{000C}`) and column break (`\u{000E}`); and the directional
/// formatting characters that reorder the text after them.
fn not_typed(c: char) -> bool {
    let control = (c.is_control() || c == '\u{7F}') && !matches!(c, '\t' | '\n' | '\u{000B}' | '\r');
    let direction = matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}');
    control || direction
}

/// The stories an edit session edits: the main text, headers, footers, footnotes and endnotes.
/// Comments stay as the file has them (a host keeps its own comments), and so do text boxes.
pub fn editable_story(doc: &Document, story: StoryRef) -> bool {
    match story {
        StoryRef::Body => true,
        StoryRef::Part(id) => {
            doc.parts.get(&id).is_some_and(|part| matches!(part.kind, PartKind::Header | PartKind::Footer | PartKind::Footnote | PartKind::Endnote))
        }
    }
}

/// Whether `date` is a revision date an edit session stamps: ISO 8601 UTC to the second,
/// `YYYY-MM-DDTHH:MM:SSZ`, as Word stores it.
pub fn is_revision_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    let shape = bytes.len() == 20
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            10 => *byte == b'T',
            13 | 16 => *byte == b':',
            19 => *byte == b'Z',
            _ => byte.is_ascii_digit(),
        });
    let number = |range: std::ops::Range<usize>| date.get(range).and_then(|part| part.parse::<u32>().ok()).unwrap_or(u32::MAX);
    shape
        && (1..=12).contains(&number(5..7))
        && (1..=31).contains(&number(8..10))
        && number(11..13) < 24
        && number(14..16) < 60
        && number(17..19) < 60
}

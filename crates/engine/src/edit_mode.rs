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
    // Into an existing header or footer and back to the text (`Session::run` refuses making one).
    "insert.editHeader",
    "insert.editFooter",
    "insert.closeHeader",
];

/// Searching and moving through the document. One that finds nothing refuses no edit: its error is
/// never an edit session's refusal, so a host shows no notice for it.
pub const NAVIGATION: &[&str] = &["edit.find", "edit.findNext", "edit.findPrevious", "edit.goto", "review.nextChange", "review.previousChange"];

/// Whether `id` enters a header (`Some(true)`) or a footer (`Some(false)`).
pub fn entering_header(id: &str) -> Option<bool> {
    match id {
        "insert.editHeader" => Some(true),
        "insert.editFooter" => Some(false),
        _ => None,
    }
}

/// Whether `after` is `before` with, at most, revision records appended: the document's text,
/// parts and settings are as they were. Records nothing refers to (all else is unchanged) are what
/// a command that changed nothing leaves behind.
pub fn only_records_added(before: &Document, after: &Document) -> bool {
    // Blocks a command did not touch are shared with the copy taken before it; a block it rewrote
    // without changing is compared by value.
    let same_blocks = |a: &wordcraft_doc::Blocks, b: &wordcraft_doc::Blocks| {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| std::sync::Arc::ptr_eq(x, y) || x == y)
    };
    let rest = |doc: &Document| Document { body: Vec::new(), parts: Default::default(), revisions: Vec::new(), ..doc.clone() };
    after.revisions.get(..before.revisions.len()) == Some(before.revisions.as_slice())
        && same_blocks(&before.body, &after.body)
        && before.parts.len() == after.parts.len()
        && before.parts.iter().zip(&after.parts).all(|((a_id, a), (b_id, b))| a_id == b_id && a.kind == b.kind && same_blocks(&a.blocks, &b.blocks))
        && rest(before) == rest(after)
}

/// The header (or footer) page `page` shows, as the layout resolves it the way Word renders it: a
/// section's first-page part on its title page, the even part on even pages when the document has
/// different odd and even headers, and parts inherited from earlier sections ("link to previous").
/// `None` when the page shows none: an edit session never makes one.
pub fn shown_header_footer(doc: &Document, layout: &wordcraft_layout::DocLayout, page: usize, header: bool) -> Option<u32> {
    let shown = layout.pages.get(page).and_then(|page| if header { page.header_story } else { page.footer_story })?;
    doc.parts.get(&shown).is_some_and(|part| matches!(part.kind, PartKind::Header | PartKind::Footer)).then_some(shown)
}

/// The sections (0-based, in order) whose pages show header or footer `part`: more than one when
/// later sections inherit it, so editing it edits them all.
pub fn sections_showing(layout: &wordcraft_layout::DocLayout, part: u32) -> Vec<usize> {
    let mut sections: Vec<usize> =
        layout.pages.iter().filter(|page| page.header_story == Some(part) || page.footer_story == Some(part)).map(|page| page.section).collect();
    sections.sort_unstable();
    sections.dedup();
    sections
}

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
    /// No command has this id.
    UnknownCommand,
    /// The command's parameters are missing or malformed.
    InvalidParameters,
    /// The command does not apply where the selection is (nothing selected, nothing to undo…).
    NotAvailable,
    /// The document's own Restrict Editing setting refuses the change.
    DocumentProtected,
    /// The command failed; the document is as it was.
    CommandFailed,
}

impl Refusal {
    pub const ALL: [Refusal; 11] = [
        Refusal::CommandNotAllowed,
        Refusal::DecisionNotAllowed,
        Refusal::ParagraphBreakNotSupported,
        Refusal::CharacterNotAllowed,
        Refusal::OutsideEditableStory,
        Refusal::InvalidEditSession,
        Refusal::UnknownCommand,
        Refusal::InvalidParameters,
        Refusal::NotAvailable,
        Refusal::DocumentProtected,
        Refusal::CommandFailed,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Refusal::CommandNotAllowed => "command_not_allowed",
            Refusal::DecisionNotAllowed => "decision_not_allowed",
            Refusal::ParagraphBreakNotSupported => "paragraph_break_not_supported",
            Refusal::CharacterNotAllowed => "character_not_allowed",
            Refusal::OutsideEditableStory => "outside_editable_story",
            Refusal::InvalidEditSession => "invalid_edit_session",
            Refusal::UnknownCommand => "unknown_command",
            Refusal::InvalidParameters => "invalid_parameters",
            Refusal::NotAvailable => "not_available",
            Refusal::DocumentProtected => "document_protected",
            Refusal::CommandFailed => "command_failed",
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
            Refusal::UnknownCommand => "there is no such command",
            Refusal::InvalidParameters => "the command's parameters are missing or malformed",
            Refusal::NotAvailable => "the command does not apply here",
            Refusal::DocumentProtected => "the document is protected against this change",
            Refusal::CommandFailed => "the command failed and the document is as it was",
        }
    }
}

/// A refusal of an edit session, as [`crate::CmdError::Editing`] carries it. `detail` is the
/// command's own error, for logs, when the refusal stands for one.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "{command}: not available while editing [{code}]: {reason}{detail}",
    code = .refusal.code(),
    reason = .refusal.reason(),
    detail = .detail.as_deref().map(|detail| format!(" ({detail})")).unwrap_or_default()
)]
pub struct EditRefusal {
    pub refusal: Refusal,
    pub command: String,
    pub detail: Option<String>,
}

impl EditRefusal {
    pub fn new(refusal: Refusal, command: &str) -> Self {
        EditRefusal { refusal, command: command.to_string(), detail: None }
    }
}

/// Why an edit session with `rules` refuses `id`, a command it does not run (`allows` is false).
/// The host's own gate and `Session::run` answer the same.
pub fn refusal_for(id: &str, rules: EditRules) -> Refusal {
    match id {
        _ if DECIDE.contains(&id) && !rules.may_decide => Refusal::DecisionNotAllowed,
        // Enter: off until the writer records an inserted paragraph mark (Ruling 6C-10).
        "text.newParagraph" => Refusal::ParagraphBreakNotSupported,
        _ => Refusal::CommandNotAllowed,
    }
}

/// The message a command off the allow-list is refused with.
pub fn refusal(id: &str) -> String {
    EditRefusal::new(Refusal::CommandNotAllowed, id).to_string()
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
/// `YYYY-MM-DDTHH:MM:SSZ`, as Word stores it, on a day the calendar has.
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
    let (year, month) = (number(0..4), number(5..7));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    // A real calendar day: 2026-02-31 is no date, 2028-02-29 is.
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    shape && (1..=12).contains(&month) && (1..=days).contains(&number(8..10)) && number(11..13) < 24 && number(14..16) < 60 && number(17..19) < 60
}

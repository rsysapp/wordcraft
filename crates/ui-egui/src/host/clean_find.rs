//! Find in the clean display (embed API 0.4.1). The engine's search reads each paragraph's live
//! text and cuts it at every tracked deletion, so a phrase across a deleted run is not found, and it
//! searches hidden text the display does not show. In the clean display the search reads each
//! paragraph as it is shown instead: its accepted text (`wordcraft_docx::facts`) without hidden
//! text, the text the viewport, locate and marks by location name their ranges in. The matches it
//! makes are put in the session's find state, so everything that follows (stepping, the bar's
//! count, the selection, the scroll) is the engine's own.

use std::ops::Range;

use wordcraft_doc::{Document, Paragraph, Pos, StoryRef};
use wordcraft_docx::facts::{for_each_accepted, model_end_offset, model_offset};
use wordcraft_engine::{Selection, Session};

use crate::WordApp;

/// A character the display shows, and where it stands in the paragraph's accepted text.
struct Shown {
    character: char,
    /// UTF-16 offset of the character in the accepted text (hidden text counts: offsets are the ones a
    /// location names).
    unit: u32,
    units: u32,
}

/// The paragraph as the clean display shows it: its accepted text without hidden runs.
fn shown(paragraph: &Paragraph) -> Vec<Shown> {
    let hidden: Vec<Range<usize>> = paragraph.run_ranges().filter(|(_, props)| props.hidden.unwrap_or(false)).map(|(range, _)| range).collect();
    let is_hidden = |at: usize| hidden.get(hidden.partition_point(|range| range.end <= at)).is_some_and(|range| range.start <= at);
    let mut out = Vec::new();
    let mut unit = 0_u32;
    for_each_accepted(paragraph, |piece, start, _| {
        let piece_hidden = is_hidden(start);
        for character in piece.chars() {
            let units = u32::try_from(character.len_utf16()).unwrap_or(1);
            if !piece_hidden {
                out.push(Shown { character, unit, units });
            }
            unit = unit.saturating_add(units);
        }
        std::ops::ControlFlow::Continue(())
    });
    out
}

/// Simple case folding: a character that lowercases to one character becomes it. The engine's own
/// literal search is case-insensitive in the same way.
fn folded(character: char) -> char {
    let mut lower = character.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(only), None) => only,
        _ => character,
    }
}

/// Every match of `query` (literal text; a match never crosses paragraphs) in the body, in document
/// order and without overlaps, as model positions. A match over a deleted run spans it; a match never
/// starts or ends inside one.
pub(crate) fn search(doc: &Document, query: &str, match_case: bool) -> Vec<(Pos, Pos)> {
    let needle: Vec<char> = query.chars().map(|character| if match_case { character } else { folded(character) }).collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for path in doc.para_paths(StoryRef::Body) {
        let Some(paragraph) = doc.para(StoryRef::Body, &path) else { continue };
        let text = shown(paragraph);
        let same = |at: usize| {
            needle.iter().enumerate().all(|(offset, wanted)| {
                text.get(at + offset)
                    .is_some_and(|candidate| if match_case { candidate.character == *wanted } else { folded(candidate.character) == *wanted })
            })
        };
        let mut at = 0;
        while at + needle.len() <= text.len() {
            if !same(at) {
                at += 1;
                continue;
            }
            if let (Some(first), Some(last)) = (text.get(at), text.get(at + needle.len() - 1))
                && let (Some(start), Some(end)) =
                    (model_offset(paragraph, first.unit), model_end_offset(paragraph, last.unit.saturating_add(last.units)))
                && start < end
            {
                let position = |off: usize| Pos { story: StoryRef::Body, path: path.clone(), off };
                found.push((position(start), position(end)));
            }
            at += needle.len();
        }
    }
    found
}

/// Begin a search in the session's find state without selecting anything: the part of the engine's
/// `edit.find` that a count-only find shares with the one that selects.
pub(crate) fn begin(session: &mut Session, query: &str, match_case: bool) {
    session.find.query = query.to_string();
    session.find.match_case = match_case;
    session.find.whole_word = false;
    session.find.regex = false;
    // A new search changes what Reading Highlight marks.
    session.find.highlights = None;
    session.find.results = search(&session.doc, query, match_case);
}

/// The engine's `edit.find` for the clean display: search, remember the search, select the first
/// match at or after the selection's caret.
pub(crate) fn find(app: &mut WordApp, query: &str, match_case: bool) {
    let session = &mut app.session;
    begin(session, query, match_case);
    let caret = session.sel.focus.clone();
    let index = session.find.results.iter().position(|(start, _)| *start >= caret).unwrap_or(0);
    select(session, index);
    app.after_command("edit.find");
}

/// The engine's `edit.findNext` and `edit.findPrevious` for the clean display; `false` when the
/// search has no match (the engine refuses then, and the selection stays).
pub(crate) fn step(app: &mut WordApp, forward: bool) -> bool {
    let session = &mut app.session;
    session.find.highlights = None;
    session.find.results = search(&session.doc, &session.find.query, session.find.match_case);
    let count = session.find.results.len();
    if count == 0 {
        return false;
    }
    let (first, last) = session.sel.ordered();
    let index = if forward {
        session.find.results.iter().position(|(start, _)| *start >= last).unwrap_or(0)
    } else {
        session.find.results.iter().rposition(|(_, end)| *end <= first).unwrap_or(count - 1)
    };
    select(session, index);
    app.after_command(if forward { "edit.findNext" } else { "edit.findPrevious" });
    true
}

fn select(session: &mut Session, index: usize) {
    session.find.current = index;
    if let Some((start, end)) = session.find.results.get(index).cloned() {
        session.sel = Selection { anchor: start, focus: end };
    }
}

#[cfg(test)]
mod tests {
    use wordcraft_doc::props::CharProps;
    use wordcraft_doc::{Document, Paragraph, Run, para_block};

    use super::*;

    /// A document of one paragraph made of `(text, deleted, hidden)` parts.
    fn document(parts: &[(&str, bool, bool)]) -> Document {
        let mut paragraph = Paragraph::new();
        for (text, deleted, hidden) in parts {
            paragraph.text.push_str(text);
            let props = CharProps { del: deleted.then_some(0), hidden: hidden.then_some(true), ..CharProps::default() };
            paragraph.runs.push(Run { len: text.len(), props });
        }
        let mut doc = Document::new();
        doc.body = vec![para_block(paragraph)];
        doc
    }

    fn matched(doc: &Document, query: &str, match_case: bool) -> Vec<String> {
        search(doc, query, match_case)
            .into_iter()
            .map(|(start, end)| doc.para_at(&start).and_then(|paragraph| paragraph.text.get(start.off..end.off)).unwrap_or("").to_string())
            .collect()
    }

    #[test]
    fn a_match_across_a_deleted_run_spans_it_and_a_match_inside_one_does_not_exist() {
        let doc = document(&[("Pay ", false, false), ("30 ", true, false), ("45 days", false, false)]);
        assert_eq!(matched(&doc, "pay 45 days", false), vec!["Pay 30 45 days"]);
        assert_eq!(matched(&doc, "30", false), Vec::<String>::new());
        // A match ending just before the deletion does not take it along.
        assert_eq!(matched(&doc, "Pay", true), vec!["Pay"]);
        assert_eq!(matched(&doc, "pay", true), Vec::<String>::new(), "case counts when asked for");
    }

    #[test]
    fn hidden_text_is_not_shown_and_the_words_around_it_join() {
        let doc = document(&[("Keep ", false, false), ("secret ", false, true), ("terms", false, false)]);
        assert_eq!(matched(&doc, "secret", false), Vec::<String>::new());
        assert_eq!(matched(&doc, "keep terms", false), vec!["Keep secret terms"]);
    }

    #[test]
    fn offsets_are_utf16_units_so_astral_characters_do_not_shift_a_match() {
        let doc = document(&[("\u{1F600} Pay ", false, false), ("30 ", true, false), ("45 days", false, false)]);
        assert_eq!(matched(&doc, "45 days", false), vec!["45 days"]);
        assert_eq!(matched(&doc, "\u{1F600} pay 45", false), vec!["\u{1F600} Pay 30 45"]);
    }

    #[test]
    fn matches_do_not_overlap_and_the_empty_query_matches_nothing() {
        let doc = document(&[("aaaa", false, false)]);
        assert_eq!(matched(&doc, "aa", false), vec!["aa", "aa"]);
        assert!(search(&doc, "", false).is_empty());
    }
}

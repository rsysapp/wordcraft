//! Find in the clean display (embed API 0.4.1). The engine's search reads each paragraph's live
//! text and cuts it at every tracked deletion, so a phrase across a deleted run is not found, and it
//! searches hidden text the display does not show. In the clean display the search reads each
//! paragraph as it is shown instead: its accepted text (`wordcraft_docx::facts`) without hidden
//! text, the text the viewport, locate and marks by location name their ranges in. The matches it
//! makes are put in the session's find state, so everything that follows (stepping, the bar's
//! count, the selection, the scroll) is the engine's own.

use std::ops::Range;

use wordcraft_doc::{Document, Paragraph, Pos, StoryRef};
use wordcraft_docx::facts::for_each_accepted;
use wordcraft_engine::{Selection, Session};

use crate::WordApp;

/// The engine's own cap on the matches of one search (`search_full`), so a one-letter query over a
/// long contract keeps the same bounded work and the same count as the editor's find.
const MAX_MATCHES: usize = 100_000;

/// A character the display shows, and where the model keeps it.
struct Shown {
    character: char,
    /// The character folded for a search that ignores case, made once rather than per comparison.
    folded: char,
    /// The bytes of `paragraph.text` the character comes from: its own, or the whole of the field or
    /// object whose result it is part of. These are what `facts::model_offset` (the start) and
    /// `model_end_offset` (the end) answer for the character's UTF-16 unit; reading them while the
    /// paragraph is walked maps a match without walking the paragraph again for it.
    model: Range<usize>,
}

/// The paragraph as the clean display shows it: its accepted text without hidden runs.
fn shown(paragraph: &Paragraph) -> Vec<Shown> {
    let hidden: Vec<Range<usize>> = paragraph.run_ranges().filter(|(_, props)| props.hidden.unwrap_or(false)).map(|(range, _)| range).collect();
    let is_hidden = |at: usize| hidden.get(hidden.partition_point(|range| range.end <= at)).is_some_and(|range| range.start <= at);
    let mut out = Vec::new();
    for_each_accepted(paragraph, |piece, start, end| {
        // Hidden text is not shown, but it is not in the way of the words around it either.
        if !is_hidden(start) {
            out.extend(piece.chars().map(|character| Shown { character, folded: folded(character), model: start..end }));
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
/// order and without overlaps, as model positions, at most [`MAX_MATCHES`]. A match over a deleted run
/// spans it; a match never starts or ends inside one. The cost is the body's text plus the matches.
pub(crate) fn search(doc: &Document, query: &str, match_case: bool) -> Vec<(Pos, Pos)> {
    #[cfg(test)]
    tests::SEARCHES.with(|count| count.set(count.get() + 1));
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
                text.get(at + offset).is_some_and(|candidate| if match_case { candidate.character == *wanted } else { candidate.folded == *wanted })
            })
        };
        let mut at = 0;
        while at + needle.len() <= text.len() {
            if !same(at) {
                at += 1;
                continue;
            }
            if let (Some(first), Some(last)) = (text.get(at), text.get(at + needle.len() - 1)) {
                let (start, end) = (first.model.start, last.model.end);
                if start < end {
                    let position = |off: usize| Pos { story: StoryRef::Body, path: path.clone(), off };
                    found.push((position(start), position(end)));
                    if found.len() >= MAX_MATCHES {
                        return found;
                    }
                }
            }
            at += needle.len();
        }
    }
    found
}

/// What the session's find results were made from. While it still describes the session, Next and
/// Previous step through the results instead of searching the whole body again.
pub(crate) struct Stamp {
    document_id: u64,
    content_version: u64,
    query: String,
    match_case: bool,
    count: usize,
    first: Option<(Pos, Pos)>,
    last: Option<(Pos, Pos)>,
}

impl Stamp {
    /// The stamp of the results the session holds now. The count and the ends tell results this module
    /// made from another search's, which the editor's own find can leave in the session.
    fn of(session: &Session) -> Self {
        let find = &session.find;
        Self {
            document_id: session.document_id(),
            content_version: session.content_version(),
            query: find.query.clone(),
            match_case: find.match_case,
            count: find.results.len(),
            first: find.results.first().cloned(),
            last: find.results.last().cloned(),
        }
    }
}

/// Begin a search in the session's find state without selecting anything: the part of the engine's
/// `edit.find` that a count-only find shares with the one that selects.
pub(crate) fn begin(session: &mut Session, query: &str, match_case: bool, stamp: &mut Option<Stamp>) {
    session.find.query = query.to_string();
    session.find.match_case = match_case;
    session.find.whole_word = false;
    session.find.regex = false;
    // A new search changes what Reading Highlight marks.
    session.find.highlights = None;
    session.find.results = search(&session.doc, query, match_case);
    *stamp = Some(Stamp::of(session));
}

/// The engine's `edit.find` for the clean display: search, remember the search, select the first
/// match at or after the selection's caret.
pub(crate) fn find(app: &mut WordApp, query: &str, match_case: bool, stamp: &mut Option<Stamp>) {
    let session = &mut app.session;
    begin(session, query, match_case, stamp);
    let caret = session.sel.focus.clone();
    let index = session.find.results.iter().position(|(start, _)| *start >= caret).unwrap_or(0);
    select(session, index);
    app.after_command("edit.find");
}

/// The engine's `edit.findNext` and `edit.findPrevious` for the clean display; `false` when the
/// search has no match (the engine refuses then, and the selection stays). The results are searched
/// again only when the document, the query or the case is not the one they were made from.
pub(crate) fn step(app: &mut WordApp, forward: bool, stamp: &mut Option<Stamp>) -> bool {
    let session = &mut app.session;
    session.find.highlights = None;
    let current = Stamp::of(session);
    let still_valid = stamp.as_ref().is_some_and(|made| {
        made.document_id == current.document_id
            && made.content_version == current.content_version
            && made.query == current.query
            && made.match_case == current.match_case
            && made.count == current.count
            && made.first == current.first
            && made.last == current.last
    });
    if !still_valid {
        session.find.results = search(&session.doc, &session.find.query, session.find.match_case);
        *stamp = Some(Stamp::of(session));
    }
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

    use crate::Services;

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

    /// `paragraphs` paragraphs of about 4,000 characters, "plain + struck-out + plain" runs with
    /// non-ASCII and astral characters, each with 221 shown matches of the letter "a".
    fn long_document(paragraphs: usize) -> Document {
        let filler = "ab cdefghij klmno ".repeat(110);
        let mut doc = Document::new();
        doc.body = (0..paragraphs)
            .map(|_| {
                let mut paragraph = Paragraph::new();
                for (text, deleted) in [(format!("Ad \u{e9}\u{1F600} {filler}"), false), ("alpha ".to_string(), true), (filler.clone(), false)] {
                    paragraph.text.push_str(&text);
                    let props = CharProps { del: deleted.then_some(0), ..CharProps::default() };
                    paragraph.runs.push(Run { len: text.len(), props });
                }
                para_block(paragraph)
            })
            .collect();
        doc
    }

    /// Where the letter "a" (any case) stands in the shown text of `long_document`, from the text
    /// itself: the struck-out run is the one after the first.
    fn long_document_expectation(doc: &Document) -> Vec<(Pos, Pos)> {
        let mut expected = Vec::new();
        for path in doc.para_paths(StoryRef::Body) {
            let paragraph = doc.para(StoryRef::Body, &path).expect("a paragraph");
            let struck = paragraph.deleted_ranges();
            for (at, character) in paragraph.text.char_indices() {
                if struck.iter().any(|range| range.contains(&at)) || character.to_lowercase().next() != Some('a') {
                    continue;
                }
                let position = |off: usize| Pos { story: StoryRef::Body, path: path.clone(), off };
                expected.push((position(at), position(at + character.len_utf8())));
            }
        }
        expected
    }

    thread_local! {
        /// How many times `search` has read the body on this test's thread.
        pub(super) static SEARCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn searches() -> usize {
        SEARCHES.with(std::cell::Cell::get)
    }

    fn selected(app: &WordApp) -> (Pos, Pos) {
        (app.session.sel.anchor.clone(), app.session.sel.focus.clone())
    }

    #[test]
    fn a_long_document_is_searched_and_stepped_in_linear_time() {
        let doc = long_document(300);
        let characters: usize = doc
            .para_paths(StoryRef::Body)
            .iter()
            .filter_map(|path| doc.para(StoryRef::Body, path))
            .map(|paragraph| paragraph.text.chars().count())
            .sum();
        assert!(characters >= 1_000_000, "{characters} characters");
        let expected = long_document_expectation(&doc);
        assert_eq!(expected.len(), 300 * 221);
        let mut app = WordApp::new(Session::new(doc), Services::default());
        let mut stamp = None;
        // Generous: the quadratic mapping took 1.2 s a search here, and 52 searches (the first, then
        // every Next and Previous). The linear one takes a fraction of a second for all of it.
        let bound = std::time::Duration::from_secs(8);
        let started = std::time::Instant::now();
        find(&mut app, "a", false, &mut stamp);
        assert_eq!(app.session.find.results.len(), expected.len());
        assert!(app.session.find.results == expected, "every match, where the text has it");
        assert_eq!(selected(&app), expected[0]);
        for (index, wanted) in expected.iter().enumerate().take(51).skip(1) {
            assert!(step(&mut app, true, &mut stamp));
            assert_eq!(&selected(&app), wanted, "next {index}");
            assert!(started.elapsed() < bound, "step {index} at {:?}", started.elapsed());
        }
        for (index, wanted) in expected.iter().enumerate().take(50).skip(45).rev() {
            assert!(step(&mut app, false, &mut stamp));
            assert_eq!(&selected(&app), wanted, "previous {index}");
        }
        assert!(started.elapsed() < bound, "took {:?}", started.elapsed());
        assert_eq!(searches(), 1, "stepping read the stored results");
    }

    #[test]
    fn stepping_searches_again_only_when_the_search_or_the_document_is_not_the_one_the_results_came_from() {
        let mut app = WordApp::new(Session::new(document(&[("one two one two one", false, false)])), Services::default());
        let mut stamp = None;
        find(&mut app, "one", false, &mut stamp);
        for _ in 0..4 {
            assert!(step(&mut app, true, &mut stamp));
        }
        assert!(step(&mut app, false, &mut stamp));
        assert_eq!(searches(), 1);
        // A change of the document, even one that leaves its text as it was, is a new search.
        app.session.touch();
        assert!(step(&mut app, true, &mut stamp));
        assert_eq!(searches(), 2);
        assert!(step(&mut app, true, &mut stamp));
        assert_eq!(searches(), 2);
        // So is another query or case: here, the editor's own find left its results in the session.
        app.session.find.query = "two".into();
        assert!(step(&mut app, true, &mut stamp));
        assert_eq!(searches(), 3);
        assert_eq!(app.session.find.results.len(), 2);
        app.session.find.match_case = true;
        assert!(step(&mut app, true, &mut stamp));
        assert_eq!(searches(), 4);
        // Results the editor's find swapped in under the same query are not ours to step through.
        app.session.find.results.truncate(1);
        assert!(step(&mut app, true, &mut stamp));
        assert_eq!(searches(), 5);
        assert_eq!(app.session.find.results.len(), 2);
        // No stamp at all (a display change, an open) is a search too.
        stamp = None;
        assert!(step(&mut app, true, &mut stamp));
        assert_eq!(searches(), 6);
    }

    #[test]
    fn a_search_with_no_match_is_not_run_again_by_stepping() {
        let mut app = WordApp::new(Session::new(document(&[("one two", false, false)])), Services::default());
        let mut stamp = None;
        find(&mut app, "zzz", false, &mut stamp);
        assert!(!step(&mut app, true, &mut stamp));
        assert!(!step(&mut app, false, &mut stamp));
        assert_eq!(searches(), 1);
    }

    #[test]
    fn a_search_stops_at_the_engine_s_cap_on_matches() {
        let doc = document(&[(&"x".repeat(MAX_MATCHES + 5_000), false, false)]);
        let found = search(&doc, "x", false);
        assert_eq!(found.len(), MAX_MATCHES);
        assert_eq!(found.last().map(|(start, end)| (start.off, end.off)), Some((MAX_MATCHES - 1, MAX_MATCHES)));
        // Across paragraphs too: the first ones' matches are kept, in order.
        let mut doc = Document::new();
        doc.body = (0..3).map(|_| para_block(Paragraph::with_text(&"x".repeat(MAX_MATCHES / 2 + 1), CharProps::default()))).collect();
        let found = search(&doc, "x", false);
        assert_eq!(found.len(), MAX_MATCHES);
        assert_eq!(found.last().map(|(start, _)| start.path.clone()), Some(Pos::body(1, 0).path));
    }

    /// The mapping this module used before it was made linear: a match's start and end asked of
    /// `facts::model_offset` and `model_end_offset`, each of which walks the paragraph again.
    fn mapped_by_the_facts(doc: &Document, query: &str, match_case: bool) -> Vec<(Pos, Pos)> {
        use wordcraft_docx::facts::{model_end_offset, model_offset};
        let needle: Vec<char> = query.chars().map(|character| if match_case { character } else { folded(character) }).collect();
        let mut found = Vec::new();
        for path in doc.para_paths(StoryRef::Body) {
            let paragraph = doc.para(StoryRef::Body, &path).expect("a paragraph");
            let hidden: Vec<Range<usize>> =
                paragraph.run_ranges().filter(|(_, props)| props.hidden.unwrap_or(false)).map(|(range, _)| range).collect();
            // (character, UTF-16 offset in the accepted text, its units), hidden characters left out.
            let mut text = Vec::new();
            let mut unit = 0_u32;
            for_each_accepted(paragraph, |piece, start, _| {
                let piece_hidden = hidden.iter().any(|range| range.contains(&start));
                for character in piece.chars() {
                    let units = u32::try_from(character.len_utf16()).expect("at most two");
                    if !piece_hidden {
                        text.push((character, unit, units));
                    }
                    unit += units;
                }
                std::ops::ControlFlow::Continue(())
            });
            let mut at = 0;
            while at + needle.len() <= text.len() {
                let same = needle.iter().enumerate().all(|(offset, wanted)| {
                    let candidate = text[at + offset].0;
                    if match_case { candidate == *wanted } else { folded(candidate) == *wanted }
                });
                if !same {
                    at += 1;
                    continue;
                }
                let (first, last) = (text[at], text[at + needle.len() - 1]);
                if let (Some(start), Some(end)) = (model_offset(paragraph, first.1), model_end_offset(paragraph, last.1 + last.2))
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

    #[test]
    fn the_one_pass_mapping_gives_what_model_offset_and_model_end_offset_give() {
        use wordcraft_doc::InlineObject;
        // Insertions, deletions (also at the edges), hidden text, two-byte and astral characters.
        let mut paragraphs = Vec::new();
        for parts in [
            vec![("Pay ", false, false, false), ("30 ", true, false, false), ("45 days", false, false, false)],
            vec![
                ("\u{1F600}\u{e9}a ", false, false, false),
                ("new \u{1F600}a ", false, false, true),
                ("old a ", true, false, false),
                ("\u{e9}A\u{1F600}", false, false, false),
            ],
            vec![
                ("a", true, false, false),
                ("ab", false, false, false),
                ("a", true, false, false),
                ("ba", false, false, false),
                ("a", true, false, false),
            ],
            vec![("Keep ", false, false, false), ("secret \u{1F600}a ", false, true, false), ("terms", false, false, false)],
            vec![("\u{4e2d}\u{6587} a ", false, false, true), ("\u{4e2d}\u{6587}", true, false, false), ("\u{4e2d}a", false, false, true)],
        ] {
            let mut paragraph = Paragraph::new();
            for (text, deleted, hidden, inserted) in parts {
                paragraph.text.push_str(text);
                let props =
                    CharProps { del: deleted.then_some(0), hidden: hidden.then_some(true), ins: inserted.then_some(0), ..CharProps::default() };
                paragraph.runs.push(Run { len: text.len(), props });
            }
            paragraphs.push(paragraph);
        }
        // A field: its result is several units that map to the field's own character.
        let mut with_field = Paragraph::with_text("\u{1F600} Page  of 9 a", CharProps::default());
        with_field
            .insert_object(
                "\u{1F600} Page ".len(),
                InlineObject::Field { instr: "PAGE".into(), result: "12a".into(), locked: false, dirty: false, code: false, form: String::new() },
                &CharProps::default(),
            )
            .expect("a field");
        paragraphs.push(with_field);
        let mut doc = Document::new();
        doc.body = paragraphs.into_iter().map(para_block).collect();
        let mut compared = 0;
        for query in [
            "a",
            "A",
            "ab",
            "ba",
            "aa",
            "a ",
            " a",
            "\u{e9}a",
            "\u{e9}A\u{1F600}",
            "\u{1F600}",
            "\u{1F600}a",
            "pay 45",
            "45 days",
            "keep terms",
            "secret",
            "\u{4e2d}\u{6587}",
            "\u{6587} a",
            "12",
            "12a",
            "page 12a of",
            "e 12",
            "a of 9",
        ] {
            for match_case in [false, true] {
                let expected = mapped_by_the_facts(&doc, query, match_case);
                assert_eq!(search(&doc, query, match_case), expected, "{query:?}, match case {match_case}");
                compared += expected.len();
            }
        }
        assert!(compared > 60, "the comparison saw {compared} matches");
    }

    #[test]
    fn matches_do_not_overlap_and_the_empty_query_matches_nothing() {
        let doc = document(&[("aaaa", false, false)]);
        assert_eq!(matched(&doc, "aa", false), vec!["aa", "aa"]);
        assert!(search(&doc, "", false).is_empty());
    }
}

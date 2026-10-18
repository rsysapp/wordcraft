//! embed: a save from the embed build keeps everything the edit did not change (Plan 6c, P6-4).
//! Clauflow's save check compares the saved package with the one it was opened from and refuses
//! any part, relationship, content type or story element that differs outside the editor's own
//! tracked changes; so a save of a Word-authored file, with no edit or with one tracked
//! insertion, must come back as the original with at most that insertion in `word/document.xml`.
//! The samples are fictional agreements from Clauflow's test data (Word, LibreOffice and
//! generated); the check is the inspector the host runs.

use std::collections::BTreeSet;
use std::io::Read;

use serde_json::json;
use wordcraft_doc::{Pos, StoryRef};
use wordcraft_docx::inspect::{INSPECT_LIMITS, Inspection, inspect};
use wordcraft_engine::edit_mode::EditRules;
use wordcraft_engine::io::{open_docx_with_limits, save_docx_preserving};
use wordcraft_engine::{Selection, Session};

const EDITOR: &str = "Sari Wulan";
const EDITED_AT: &str = "2026-10-17T08:00:00Z";

macro_rules! sample {
    ($name:literal) => {
        ($name, include_bytes!(concat!("fixtures/word-samples/", $name)).as_slice())
    };
}

const SAMPLES: &[(&str, &[u8])] = &[
    sample!("distribution-agreement-sumatra.docx"),
    sample!("fields-and-changes.docx"),
    sample!("perjanjian-kerja-sama-promosi.docx"),
    sample!("software-licence-bilingual.docx"),
    sample!("warehouse-lease-cikarang.docx"),
    sample!("word-authored.docx"),
];

/// The end of the last paragraph of plain text the file itself holds. A body that ends in a table
/// has no paragraph after it in the file, and the reader supplies an empty one for the caret: text
/// typed there has no place in the file to be written to. Typing next to a field written as one
/// element (`w:fldSimple`) is refused too, so the edit goes into the last paragraph without one.
fn end_of_last_source_paragraph(session: &Session) -> Pos {
    let path = session
        .doc
        .para_paths(StoryRef::Body)
        .into_iter()
        .rev()
        .find(|path| {
            session
                .doc
                .para(StoryRef::Body, path)
                .is_some_and(|paragraph| paragraph.origin.is_some() && paragraph.objects.is_empty() && !paragraph.text.is_empty())
        })
        .expect("a paragraph of the file");
    let length = session.doc.para(StoryRef::Body, &path).map_or(0, |paragraph| paragraph.text.len());
    Pos { story: StoryRef::Body, path, off: length }
}

/// Where the typing goes.
#[derive(Clone, Copy)]
enum Caret {
    /// The end of the last paragraph of plain text the file holds.
    EndOfLastParagraph,
    /// The end of the document, as the caret goes there (Ctrl+End).
    DocumentEnd,
}

/// The base opened as the embed build opens it, then saved the way its edit mode saves: with the
/// insertion of `typed` at the end of the document when there is one, nothing when not.
fn save_after(base: &[u8], typed: Option<&str>, at: Caret) -> Result<Vec<u8>, String> {
    let doc = open_docx_with_limits(base, &INSPECT_LIMITS).expect("the sample opens");
    let mut session = Session::new(doc);
    session.begin_edit(EDITOR, EDITED_AT, EditRules { may_decide: false, full: false }).expect("an edit session begins");
    if let Some(text) = typed {
        match at {
            Caret::EndOfLastParagraph => session.sel = Selection::caret(end_of_last_source_paragraph(&session)),
            Caret::DocumentEnd => {
                session.run("caret.docEnd", &json!({})).expect("the caret moves to the end");
            }
        }
        session.run("text.insert", &json!({"text": text})).expect("the text is typed");
    }
    save_docx_preserving(&session.doc)
}

fn parts_of(package: &[u8]) -> BTreeSet<String> {
    let archive = zip::ZipArchive::new(std::io::Cursor::new(package)).expect("a zip");
    archive.file_names().filter(|name| !name.ends_with('/')).map(str::to_string).collect()
}

fn part_bytes(package: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(package)).expect("a zip");
    let mut file = archive.by_name(name).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).expect("a readable part");
    Some(bytes)
}

/// What differs besides the editor's own revisions, in words; empty when nothing does.
fn problems(base: &[u8], saved: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let inspection = match inspect(base, saved) {
        Ok(inspection) => inspection,
        Err(error) => return vec![format!("the save check could not read it: {error:?}")],
    };
    let Inspection {
        new_revisions: _,
        resolved,
        altered_revisions,
        untracked_changes,
        // Per-part breakdowns of `untracked_changes` and `resolved`, which are checked below.
        untracked_by_part: _,
        resolved_by_part: _,
        losses,
        too_many_changes,
        added_parts,
        changed_relationships,
        content_types,
        comments,
    } = &inspection;
    if *resolved != Default::default() {
        found.push(format!("resolved changes: {resolved:?}"));
    }
    if (*altered_revisions, *untracked_changes) != (0, 0) {
        found.push(format!("altered revisions {altered_revisions}, untracked changes {untracked_changes}"));
    }
    if !losses.is_empty() {
        found.push(format!("losses: {losses:?}"));
    }
    if *too_many_changes {
        found.push("too many changes".into());
    }
    if !added_parts.is_empty() {
        found.push(format!("added parts: {added_parts:?}"));
    }
    if !changed_relationships.is_empty() {
        found.push(format!("changed relationships: {changed_relationships:?}"));
    }
    if !content_types.is_empty() {
        found.push(format!("changed content types: {content_types:?}"));
    }
    if (comments.removed, comments.altered) != (0, 0) {
        found.push(format!("comments: {comments:?}"));
    }
    let (base_parts, saved_parts) = (parts_of(base), parts_of(saved));
    if base_parts != saved_parts {
        found.push(format!(
            "parts removed {:?}, added {:?}",
            base_parts.difference(&saved_parts).collect::<Vec<_>>(),
            saved_parts.difference(&base_parts).collect::<Vec<_>>()
        ));
    }
    found
}

#[test]
fn a_save_without_an_edit_is_the_original() {
    let mut failures = Vec::new();
    for (name, base) in SAMPLES {
        let saved = match save_after(base, None, Caret::EndOfLastParagraph) {
            Ok(saved) => saved,
            Err(refusal) => {
                failures.push(format!("{name}: refused: {refusal}"));
                continue;
            }
        };
        let found = problems(base, &saved);
        let unchanged = inspect(base, &saved).map(|inspection| inspection == Inspection::default());
        if !found.is_empty() || unchanged != Ok(true) {
            failures.push(format!("{name}: {found:#?} (inspection unchanged: {unchanged:?})"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every sample saved with one tracked insertion at `caret`: what went wrong, by sample.
fn insertion_failures(caret: Caret, only: impl Fn(&str) -> bool, insertions: &[u32]) -> Vec<String> {
    let mut failures = Vec::new();
    for (name, base) in SAMPLES.iter().filter(|(name, _)| only(name)) {
        let saved = match save_after(base, Some(" Added by the editor."), caret) {
            Ok(saved) => saved,
            Err(refusal) => {
                failures.push(format!("{name}: refused: {refusal}"));
                continue;
            }
        };
        let mut found = problems(base, &saved);
        match inspect(base, &saved) {
            Ok(inspection) => {
                let revisions: Vec<(&str, u32, u32, u32)> = inspection
                    .new_revisions
                    .iter()
                    .map(|author| (author.author.as_str(), author.insertions, author.deletions, author.format_changes))
                    .collect();
                if !matches!(revisions.as_slice(), [(EDITOR, count, 0, 0)] if insertions.contains(count)) {
                    found.push(format!("new revisions {revisions:?}"));
                }
            }
            Err(error) => found.push(format!("the save check could not read it: {error:?}")),
        }
        // Every part but the document is the original's bytes: nothing else was rewritten.
        let base_parts = parts_of(base);
        let changed: Vec<&String> = base_parts
            .iter()
            .filter(|part| part_bytes(base, part) != part_bytes(&saved, part))
            .filter(|part| part.as_str() != "word/document.xml")
            .collect();
        if !changed.is_empty() {
            found.push(format!("parts rewritten besides the document: {changed:?}"));
        }
        if !found.is_empty() {
            failures.push(format!("{name}: {found:#?}"));
        }
    }
    failures
}

#[test]
fn one_tracked_insertion_changes_only_the_document() {
    let failures = insertion_failures(Caret::EndOfLastParagraph, |_| true, &[1]);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Ctrl+End then typing: into the paragraph the reader supplies after a final table (written as a
/// new last paragraph, its mark and text tracked), or after a field written as one element, which
/// is refused with its reason rather than written wrongly.
#[test]
fn typing_at_the_end_of_the_document_is_saved_or_refused_with_its_reason() {
    // One insertion in a paragraph of the file; two in a paragraph added after a final table: its
    // mark and its text.
    let failures = insertion_failures(Caret::DocumentEnd, |name| name != "word-authored.docx", &[1, 2]);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let (_, field_at_the_end) = SAMPLES.iter().find(|(name, _)| *name == "word-authored.docx").expect("the sample");
    let refusal = save_after(field_at_the_end, Some(" Added."), Caret::DocumentEnd).expect_err("next to a field written as one element");
    assert!(refusal.contains("field"), "{refusal}");
}

//! embed: a real edit-mode save, made by the engine with tracking on, is reported by the save
//! inspector in the shape a host accepts: the editor's own tracked changes, comments and replies,
//! and nothing else.

use serde_json::json;
use wordcraft_docx::inspect::{AuthorCount, CommentChanges, INSPECT_LIMITS, Inspection, Resolved, inspect};
use wordcraft_engine::Session;
use wordcraft_engine::io::{open_docx_with_limits, save_bytes};

const BASE_AUTHOR: &str = "Ana Setia";
const EDITOR: &str = "Sari Wulan";

/// An agreement written by WordCraft, with one comment by its first author.
fn base() -> Vec<u8> {
    let mut doc = wordcraft_doc::Document::from_text("Rent is due monthly on the first day.\nThe tenant keeps the premises clean.");
    doc.ensure_nonempty();
    let mut s = Session::new(doc);
    s.author = BASE_AUTHOR.into();
    s.run("select.text", &json!({"text": "premises"})).unwrap();
    s.run("review.newComment", &json!({"text": "Which premises?"})).unwrap();
    save_bytes("base.docx", &s.doc).unwrap()
}

/// The base opened as the embed build opens it, edited by `EDITOR` with tracking on.
fn edit(base: &[u8], change: impl FnOnce(&mut Session)) -> Vec<u8> {
    let doc = open_docx_with_limits(base, &INSPECT_LIMITS).unwrap();
    let mut s = Session::new(doc);
    s.author = EDITOR.into();
    s.run("review.trackChanges", &json!({"value": true})).unwrap();
    change(&mut s);
    save_bytes("edited.docx", &s.doc).unwrap()
}

/// What a host requires of every save besides the editor's own revisions and comments.
fn assert_clean(inspection: &Inspection) {
    assert_eq!(inspection.resolved, Resolved::default());
    assert_eq!((inspection.altered_revisions, inspection.untracked_changes), (0, 0), "{inspection:#?}");
    assert!(inspection.losses.is_empty(), "{inspection:#?}");
    assert!(!inspection.too_many_changes);
    assert!(inspection.added_parts.is_empty(), "{inspection:#?}");
    assert!(inspection.changed_relationships.is_empty(), "{inspection:#?}");
    assert!(inspection.content_types.is_empty(), "{inspection:#?}");
    assert_eq!((inspection.comments.removed, inspection.comments.altered), (0, 0), "{inspection:#?}");
}

#[test]
fn an_unchanged_reopened_save_reports_nothing() {
    let base = base();
    let edited = edit(&base, |_| {});
    assert_eq!(inspect(&base, &edited).unwrap(), Inspection::default());
}

#[test]
fn a_tracked_insertion_and_deletion_by_the_editor_are_the_editor_s() {
    let base = base();
    let edited = edit(&base, |s| {
        s.run("select.text", &json!({"text": "monthly"})).unwrap();
        s.run("text.insert", &json!({"text": "weekly"})).unwrap();
    });
    let inspection = inspect(&base, &edited).unwrap();
    let revisions: Vec<(&str, u32, u32, u32)> =
        inspection.new_revisions.iter().map(|author| (author.author.as_str(), author.insertions, author.deletions, author.format_changes)).collect();
    assert_eq!(revisions, vec![(EDITOR, 1, 1, 0)]);
    let editor = &inspection.new_revisions[0];
    assert!(editor.earliest_date.is_some() && editor.earliest_date == editor.latest_date && editor.undated == 0);
    assert_eq!((editor.hidden_text, editor.field_codes, editor.bidi_controls, editor.objects), (0, 0, 0, 0));
    assert_clean(&inspection);
    assert_eq!(inspection.comments, CommentChanges::default());
}

#[test]
fn a_comment_and_a_reply_by_the_editor_are_the_editor_s() {
    let base = base();
    let edited = edit(&base, |s| {
        s.run("select.text", &json!({"text": "tenant"})).unwrap();
        s.run("review.newComment", &json!({"text": "Name the tenant."})).unwrap();
        // The base's comment is id 0.
        s.run("review.reply", &json!({"id": 0, "text": "The flat on the second floor."})).unwrap();
    });
    let inspection = inspect(&base, &edited).unwrap();
    assert!(inspection.new_revisions.is_empty(), "{inspection:#?}");
    assert_clean(&inspection);
    let one = vec![AuthorCount { author: EDITOR.into(), count: 1 }];
    assert_eq!(inspection.comments, CommentChanges { added: one.clone(), replies: one, removed: 0, altered: 0 });
}

#[test]
fn every_kind_of_edit_in_one_save_is_reported_together() {
    let base = base();
    let edited = edit(&base, |s| {
        s.run("select.text", &json!({"text": "first"})).unwrap();
        s.run("text.insert", &json!({"text": "last"})).unwrap();
        s.run("select.text", &json!({"text": "clean"})).unwrap();
        s.run("review.newComment", &json!({"text": "Define clean."})).unwrap();
        s.run("review.reply", &json!({"id": 0, "text": "All of it."})).unwrap();
    });
    let inspection = inspect(&base, &edited).unwrap();
    assert_eq!(inspection.new_revisions.len(), 1);
    assert_eq!(inspection.new_revisions[0].author, EDITOR);
    assert_clean(&inspection);
}

#[test]
fn an_untracked_edit_from_the_engine_is_refused_by_its_facts() {
    let base = base();
    let edited = edit(&base, |s| {
        s.run("review.trackChanges", &json!({"value": false})).unwrap();
        s.run("select.text", &json!({"text": "monthly"})).unwrap();
        s.run("text.insert", &json!({"text": "weekly"})).unwrap();
    });
    let inspection = inspect(&base, &edited).unwrap();
    assert!(inspection.untracked_changes > 0);
    assert!(inspection.new_revisions.is_empty());
}

#[test]
fn the_first_comment_s_parts_are_reported_as_a_comment_only() {
    let base = save_bytes("base.docx", &wordcraft_doc::Document::from_text("Rent is due monthly.")).unwrap();
    let edited = edit(&base, |s| {
        s.run("select.text", &json!({"text": "monthly"})).unwrap();
        s.run("review.newComment", &json!({"text": "Why monthly?"})).unwrap();
    });
    let inspection = inspect(&base, &edited).unwrap();
    assert_clean(&inspection);
    assert_eq!(inspection.comments.added, vec![AuthorCount { author: EDITOR.into(), count: 1 }]);
}

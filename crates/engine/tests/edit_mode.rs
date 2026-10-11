//! embed: an edit session of the embed build: only tracked text edits, in the host's author and
//! date; deciding tracked changes only when allowed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;
use wordcraft_engine::doc::{Document, Pos, RevisionKind, StoryRef};
use wordcraft_engine::edit_mode::EditRules;
use wordcraft_engine::{Selection, Session};

const DATE: &str = "2026-10-10T08:00:00Z";

fn start_editing(session: &mut Session, may_decide: bool) {
    session.author = "Sari Wulan".into();
    session.revision_date = Some(DATE.into());
    session.edit_rules = Some(EditRules { may_decide });
    session.doc.settings.track_changes = true;
}

fn editing(text: &str, may_decide: bool) -> Session {
    let mut session = Session::new(Document::from_text(text));
    start_editing(&mut session, may_decide);
    session
}

#[test]
fn typing_is_a_tracked_change_in_the_host_s_author_and_date() {
    let mut session = editing("Rent is due.", false);
    session.run("text.insert", &json!({ "text": "Monthly: " })).unwrap();
    let revision = session.doc.revisions.last().unwrap();
    assert_eq!((revision.kind, revision.author.as_str(), revision.date.as_str()), (RevisionKind::Insert, "Sari Wulan", DATE));
}

#[test]
fn refuses_edits_that_would_not_be_tracked() {
    let mut session = editing("Rent is due.", true);
    for id in [
        "edit.pasteFormat",
        "edit.replaceAll",
        "edit.paste",
        "text.backTab",
        "para.indent",
        "review.trackChanges",
        "review.restrict",
        "review.newComment",
        "file.save",
    ] {
        let refused = session.run(id, &json!({})).unwrap_err().to_string();
        assert!(refused.contains("not available while editing"), "{id}: {refused}");
    }
}

#[test]
fn refuses_accepting_or_rejecting_without_the_permission_at_the_engine() {
    let mut session = editing("Rent is due.", false);
    for id in ["review.accept", "review.reject", "review.acceptAll", "review.rejectAll"] {
        assert!(session.run(id, &json!({})).is_err(), "{id}");
    }
    let mut legal = editing("Rent is due.", true);
    legal.run("text.insert", &json!({ "text": "Monthly: " })).unwrap();
    assert!(legal.run("review.acceptAll", &json!({})).is_ok());
}

#[test]
fn deleting_an_earlier_session_s_insertion_marks_it_deleted() {
    let mut session = editing("", false);
    session.run("text.insert", &json!({ "text": "abc" })).unwrap();
    // A later edit session by the same person: their earlier insertion is on record now.
    session.revision_date = Some("2026-10-11T08:00:00Z".into());
    session.run("select.all", &json!({})).unwrap();
    session.run("text.delete", &json!({})).unwrap();
    assert!(session.doc.plain_text(StoryRef::Body).contains("abc"));
}

#[test]
fn deleting_this_session_s_own_insertion_removes_it() {
    let mut session = editing("", false);
    session.run("text.insert", &json!({ "text": "abc" })).unwrap();
    session.run("select.all", &json!({})).unwrap();
    session.run("text.delete", &json!({})).unwrap();
    assert!(!session.doc.plain_text(StoryRef::Body).contains("abc"));
}

// Embed API "Keyboard in edit mode" (P6-2): Tab types a tab as a tracked change, and nothing else.
#[test]
fn tab_types_a_tracked_tab_character_and_shift_tab_does_nothing() {
    let mut session = editing("Rent is due.", false);
    session.run("text.tab", &json!({})).unwrap();
    assert!(session.doc.plain_text(StoryRef::Body).starts_with('\t'));
    let revision = session.doc.revisions.last().unwrap();
    assert_eq!((revision.kind, revision.author.as_str(), revision.date.as_str()), (RevisionKind::Insert, "Sari Wulan", DATE));
    assert!(session.run("text.backTab", &json!({})).is_err());
}

#[test]
fn tab_in_a_table_cell_types_a_tab_without_moving_or_adding_a_row() {
    // The table is made before the edit session: edit mode itself never inserts one.
    let mut session = Session::new(Document::from_text("Rent is due."));
    session.run("insert.table", &json!({ "rows": 1, "cols": 1 })).unwrap();
    let cell = session.doc.para_paths(StoryRef::Body).into_iter().find(|path| path.cell().is_some()).unwrap();
    let (table, ..) = cell.cell().unwrap();
    session.sel = Selection::caret(Pos { story: StoryRef::Body, path: cell.clone(), off: 0 });
    start_editing(&mut session, false);
    session.run("text.tab", &json!({})).unwrap();
    assert_eq!(session.sel.focus.path, cell, "the caret stays in its cell");
    assert_eq!(session.doc.table(StoryRef::Body, &table).unwrap().rows.len(), 1, "no row was added");
    assert!(session.doc.para(StoryRef::Body, &cell).unwrap().plain_text().contains('\t'));
}

#[test]
fn tab_at_the_start_of_a_list_item_types_a_tab_and_keeps_its_level() {
    let mut session = Session::new(Document::from_text("Rent is due."));
    session.run("para.numbering", &json!({})).unwrap();
    let level = |session: &Session| session.doc.para_at(&session.sel.focus).and_then(|para| para.props.numbering).map(|numbering| numbering.level);
    let path = session.sel.focus.path.clone();
    session.sel = Selection::caret(Pos { story: StoryRef::Body, path, off: 0 });
    let before = level(&session);
    assert!(before.is_some(), "a numbered paragraph");
    start_editing(&mut session, false);
    session.run("text.tab", &json!({})).unwrap();
    assert_eq!(level(&session), before);
    assert!(session.doc.plain_text(StoryRef::Body).starts_with('\t'));
}

#[test]
fn leaves_sessions_outside_edit_mode_as_they_were() {
    let mut session = Session::new(Document::from_text("Rent."));
    assert!(session.run("review.trackChanges", &json!({ "value": false })).is_ok());
    assert!(session.run("text.tab", &json!({})).is_ok());
}

// embed: what every save of an edit session must be for the host to take it. The save inspector
// (`wordcraft_docx::inspect`) compares each save with the file the session opened: only tracked
// insertions and deletions by the session's author, dated with the host's date, nothing untracked,
// nothing hidden; decisions on the file's own changes only where the session may decide.
mod saves {
    use serde_json::{Value, json};
    use wordcraft_docx::inspect::{INSPECT_LIMITS, Inspection, Resolved, inspect};
    use wordcraft_engine::doc::{Document, StoryRef};
    use wordcraft_engine::io::{open_docx_with_limits, save_bytes};
    use wordcraft_engine::{CmdError, Session, edit_mode, view_only};

    use super::{DATE, start_editing};

    const EDITOR: &str = "Sari Wulan";
    const BASE_AUTHOR: &str = "Ana Setia";
    const EARLIER: &str = "2026-10-01T09:00:00Z";

    /// An agreement written by WordCraft: one tracked insertion and one tracked deletion by an
    /// earlier author, a numbered clause, a misspelling AutoCorrect knows and an empty paragraph.
    fn agreement() -> Vec<u8> {
        let mut doc = Document::from_text(
            "Rent is due monthly on the first day.\nThe tenant keeps the premises tidy and clean.\nNotice is given in writing.\nThe parties sign teh agreement.\n",
        );
        doc.ensure_nonempty();
        let mut s = Session::new(doc);
        s.run("select.text", &json!({"text": "Notice"})).unwrap();
        s.run("para.numbering", &json!({})).unwrap();
        s.author = BASE_AUTHOR.into();
        s.revision_date = Some(EARLIER.into());
        s.run("review.trackChanges", &json!({"value": true})).unwrap();
        s.run("select.text", &json!({"text": "keeps"})).unwrap();
        s.run("select.collapse", &json!({})).unwrap();
        s.run("text.insert", &json!({"text": "always "})).unwrap();
        s.run("select.text", &json!({"text": "tidy and "})).unwrap();
        s.run("text.delete", &json!({})).unwrap();
        save_bytes("agreement.docx", &s.doc).unwrap()
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Expect {
        /// New tracked changes by the editor, nothing decided.
        Tracked,
        /// The file's own changes decided, and possibly new tracked changes by the editor.
        Decided,
        /// The save is the file as it was.
        Nothing,
    }

    struct Case {
        id: &'static str,
        setup: Vec<(&'static str, Value)>,
        params: Value,
        expect: Expect,
    }

    fn case(id: &'static str, setup: Vec<(&'static str, Value)>, params: Value, expect: Expect) -> Case {
        Case { id, setup, params, expect }
    }

    fn at(text: &str) -> Vec<(&'static str, Value)> {
        vec![("select.text", json!({"text": text})), ("select.collapse", json!({}))]
    }

    fn after(text: &str) -> Vec<(&'static str, Value)> {
        vec![("select.text", json!({"text": text})), ("select.collapse", json!({"end": true}))]
    }

    fn selecting(text: &str) -> Vec<(&'static str, Value)> {
        vec![("select.text", json!({"text": text}))]
    }

    fn cases() -> Vec<Case> {
        use Expect::{Decided, Nothing, Tracked};
        let typed = || vec![("select.text", json!({"text": "monthly"})), ("text.insert", json!({"text": "weekly"}))];
        let mut cases = vec![
            case("text.insert", at("monthly"), json!({"text": "calendar "}), Tracked),
            // AutoCorrect would rewrite the file's own "teh" untracked.
            case("text.insert", after("teh"), json!({"text": " "}), Tracked),
            // AutoFormat would turn "1. " into list numbering, untracked.
            case(
                "text.insert",
                vec![("caret.docEnd", json!({})), ("text.insert", json!({"text": "1"})), ("text.insert", json!({"text": "."}))],
                json!({"text": " "}),
                Tracked,
            ),
            case("text.backspace", after("monthly"), json!({}), Tracked),
            case("text.backspace", selecting("first "), json!({}), Tracked),
            // At a numbered paragraph's start: no numbering removed, no paragraphs joined (6C-10).
            case("text.backspace", at("Notice"), json!({}), Nothing),
            case("text.delete", at("monthly"), json!({}), Tracked),
            // At a paragraph's end: no join (6C-10).
            case("text.delete", after("day."), json!({}), Nothing),
            // Across paragraphs and over the file's own changes: their text is deleted, their marks
            // and the earlier deletion stay as they were.
            case("text.delete", vec![("select.all", json!({}))], json!({}), Tracked),
            case("text.deleteWordBack", after("monthly"), json!({}), Tracked),
            case("text.deleteWordForward", at("monthly"), json!({}), Tracked),
            case("text.lineBreak", at("monthly"), json!({}), Tracked),
            case("text.nbsp", at("monthly"), json!({}), Tracked),
            case("text.nbHyphen", at("monthly"), json!({}), Tracked),
            case(edit_mode::TAB, at("monthly"), json!({}), Tracked),
            case(edit_mode::TAB, at("Notice"), json!({}), Tracked),
            case("edit.cut", vec![("select.text", json!({"text": "premises"})), ("caret.docEnd", json!({"extend": true}))], json!({}), Tracked),
            case("edit.cut", selecting("monthly"), json!({}), Tracked),
            case("edit.pasteText", selecting("monthly"), json!({"text": "weekly"}), Tracked),
            case("edit.undo", typed(), json!({}), Nothing),
            case("edit.redo", [typed(), vec![("edit.undo", json!({}))]].concat(), json!({}), Tracked),
            case("review.nextChange", vec![], json!({}), Nothing),
            case("review.previousChange", vec![], json!({}), Nothing),
            case("review.changes", vec![], json!({}), Nothing),
            case("review.accept", at("always"), json!({}), Decided),
            case("review.reject", selecting("always"), json!({}), Decided),
            case("review.acceptAll", vec![], json!({}), Decided),
            case("review.rejectAll", vec![], json!({}), Decided),
            // This session's own changes stay tracked when everything is accepted.
            case("review.acceptAll", typed(), json!({}), Decided),
            case("review.rejectAll", typed(), json!({}), Decided),
        ];
        // Navigation, selection, copy, find and zoom leave the file as it was, whatever they are given.
        let anything = json!({"text": "monthly", "page": 1, "value": 120, "pos": {"story": "body", "path": [1], "off": 2}});
        cases.extend(view_only::COMMANDS.iter().map(|id| case(id, vec![], anything.clone(), Nothing)));
        cases
    }

    /// The file edited by a session that may decide (deciding commands) or may not (all others).
    fn save_after(base: &[u8], case: &Case) -> Vec<u8> {
        let mut s = Session::new(open_docx_with_limits(base, &INSPECT_LIMITS).unwrap());
        start_editing(&mut s, edit_mode::DECIDE.contains(&case.id));
        for (id, params) in &case.setup {
            s.run(id, params).unwrap_or_else(|error| panic!("{}: setting up with {id}: {error}", case.id));
        }
        let result = s.run(case.id, &case.params);
        // A view command given parameters it does not take may refuse them; it never edits.
        if case.expect != Expect::Nothing || edit_mode::COMMANDS.contains(&case.id) {
            result.unwrap_or_else(|error| panic!("{}: {error}", case.id));
        }
        save_bytes("edited.docx", &s.doc).unwrap()
    }

    fn check(case: &Case, inspection: &Inspection) {
        let name = format!("{} after {:?}", case.id, case.setup);
        assert_eq!((inspection.untracked_changes, inspection.altered_revisions), (0, 0), "{name}: {inspection:#?}");
        assert!(inspection.losses.is_empty() && !inspection.too_many_changes, "{name}: {inspection:#?}");
        assert!(
            inspection.added_parts.is_empty() && inspection.changed_relationships.is_empty() && inspection.content_types.is_empty(),
            "{name}: {inspection:#?}"
        );
        assert_eq!(inspection.comments, Default::default(), "{name}");
        for revisions in &inspection.new_revisions {
            assert_eq!(revisions.author, EDITOR, "{name}: {inspection:#?}");
            assert_eq!(revisions.format_changes, 0, "{name}");
            assert_eq!(
                (revisions.earliest_date.as_deref(), revisions.latest_date.as_deref(), revisions.undated),
                (Some(DATE), Some(DATE), 0),
                "{name}"
            );
            assert_eq!((revisions.hidden_text, revisions.field_codes, revisions.bidi_controls, revisions.objects), (0, 0, 0, 0), "{name}");
        }
        match case.expect {
            Expect::Tracked => {
                assert_eq!(inspection.resolved, Resolved::default(), "{name}: {inspection:#?}");
                assert!(inspection.new_revisions.iter().any(|revisions| revisions.insertions + revisions.deletions > 0), "{name}: {inspection:#?}");
            }
            Expect::Decided => assert!(inspection.resolved.accepted + inspection.resolved.rejected > 0, "{name}: {inspection:#?}"),
            Expect::Nothing => assert_eq!(*inspection, Inspection::default(), "{name}"),
        }
    }

    #[test]
    fn every_allowed_command_saves_only_the_editor_s_dated_tracked_changes() {
        let base = agreement();
        let cases = cases();
        for case in &cases {
            check(case, &inspect(&base, &save_after(&base, case)).unwrap());
        }
        let allowed = edit_mode::COMMANDS.iter().chain(edit_mode::DECIDE).chain(view_only::COMMANDS).chain([&edit_mode::TAB]);
        for id in allowed {
            assert!(cases.iter().any(|case| case.id == *id), "{id} is allowed in an edit session but not saved and inspected here");
        }
    }

    #[test]
    fn accepting_all_keeps_this_session_s_changes_tracked() {
        let base = agreement();
        let mut s = Session::new(open_docx_with_limits(&base, &INSPECT_LIMITS).unwrap());
        start_editing(&mut s, true);
        s.run("select.text", &json!({"text": "monthly"})).unwrap();
        s.run("text.insert", &json!({"text": "weekly"})).unwrap();
        s.run("review.acceptAll", &json!({})).unwrap();
        let inspection = inspect(&base, &save_bytes("edited.docx", &s.doc).unwrap()).unwrap();
        assert_eq!(inspection.resolved, Resolved { accepted: 2, rejected: 0 }, "{inspection:#?}");
        let editor: Vec<(&str, u32, u32)> =
            inspection.new_revisions.iter().map(|revisions| (revisions.author.as_str(), revisions.insertions, revisions.deletions)).collect();
        assert_eq!(editor, vec![(EDITOR, 1, 1)]);
    }

    #[test]
    fn a_session_that_may_not_decide_leaves_the_file_s_own_deletion_when_deleting_over_it() {
        let base = agreement();
        let mut s = Session::new(open_docx_with_limits(&base, &INSPECT_LIMITS).unwrap());
        start_editing(&mut s, false);
        s.run("select.all", &json!({})).unwrap();
        s.run("edit.cut", &json!({})).unwrap();
        let inspection = inspect(&base, &save_bytes("edited.docx", &s.doc).unwrap()).unwrap();
        assert_eq!(inspection.resolved, Resolved::default(), "{inspection:#?}");
        assert_eq!(inspection.untracked_changes, 0);
    }

    #[test]
    fn tracking_is_on_for_the_whole_session_whatever_the_file_says() {
        let base = agreement();
        let mut s = Session::new(open_docx_with_limits(&base, &INSPECT_LIMITS).unwrap());
        start_editing(&mut s, false);
        s.doc.settings.track_changes = false;
        s.run("select.text", &json!({"text": "monthly"})).unwrap();
        s.run("text.insert", &json!({"text": "weekly"})).unwrap();
        assert!(s.doc.settings.track_changes);
        let inspection = inspect(&base, &save_bytes("edited.docx", &s.doc).unwrap()).unwrap();
        assert_eq!(inspection.untracked_changes, 0, "{inspection:#?}");
        assert_eq!(inspection.new_revisions.len(), 1);
    }

    // Ruling 6C-10: the writer cannot record an inserted paragraph mark yet, so nothing in an edit
    // session starts a paragraph: Enter is not offered, and typed or pasted paragraph breaks are refused.
    #[test]
    fn nothing_starts_a_new_paragraph_in_an_edit_session() {
        let mut s = Session::new(Document::from_text("Rent is due."));
        start_editing(&mut s, true);
        let before = serde_json::to_string(&s.doc).unwrap();
        for (id, params) in [
            ("text.newParagraph", json!({})),
            ("text.insert", json!({"text": "a\rb"})),
            ("text.insert", json!({"text": "a\u{2029}b"})),
            ("edit.pasteText", json!({"text": "a\nb"})),
            ("edit.pasteText", json!({"text": "a\r\nb"})),
        ] {
            let refused = s.run(id, &params);
            assert!(matches!(refused, Err(CmdError::Disabled(ref why)) if why.contains("not available while editing")), "{id} {params}: {refused:?}");
        }
        assert_eq!(serde_json::to_string(&s.doc).unwrap(), before);
        assert_eq!(s.doc.plain_text(StoryRef::Body), "Rent is due.");
    }
}

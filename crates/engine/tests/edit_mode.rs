//! embed: an edit session of the embed build: only tracked text edits, in the host's author and
//! date; deciding tracked changes only when allowed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;
use wordcraft_engine::doc::{Document, Pos, RevisionKind, StoryRef};
use wordcraft_engine::edit_mode::EditRules;
use wordcraft_engine::{Selection, Session};

const DATE: &str = "2026-10-10T08:00:00Z";

fn start_editing(session: &mut Session, may_decide: bool) {
    session.begin_edit("Sari Wulan", DATE, EditRules { may_decide }).unwrap();
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
    session.begin_edit("Sari Wulan", "2026-10-11T08:00:00Z", EditRules { may_decide: false }).unwrap();
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
    use wordcraft_engine::doc::{Document, PartKind, StoryRef};
    use wordcraft_engine::edit_mode::{self, EditRules, Refusal};
    use wordcraft_engine::io::{open_docx_with_limits, save_bytes};
    use wordcraft_engine::{CmdError, Session};

    use super::{DATE, start_editing};

    pub const EDITOR: &str = "Sari Wulan";
    const BASE_AUTHOR: &str = "Ana Setia";
    const EARLIER: &str = "2026-10-01T09:00:00Z";

    /// An agreement written by WordCraft: one tracked insertion and one tracked deletion by an
    /// earlier author, a numbered clause, a misspelling AutoCorrect knows, an empty paragraph, a
    /// header, a footer, a footnote and a comment.
    pub fn agreement() -> Vec<u8> {
        let mut doc = Document::from_text(
            "Rent is due monthly on the first day.\nThe tenant keeps the premises tidy and clean.\nNotice is given in writing.\nThe parties sign teh agreement.\n",
        );
        doc.ensure_nonempty();
        let mut s = Session::new(doc);
        s.run("select.text", &json!({"text": "Notice"})).unwrap();
        s.run("para.numbering", &json!({})).unwrap();
        s.run("select.text", &json!({"text": "writing"})).unwrap();
        s.run("select.collapse", &json!({"end": true})).unwrap();
        s.run("references.footnote", &json!({"text": "Delivered by hand or by courier."})).unwrap();
        s.run("insert.header", &json!({"text": "Lease of the warehouse"})).unwrap();
        s.run("insert.footer", &json!({"text": "Confidential draft"})).unwrap();
        // Back to the main text: a footnote moves the caret into the note.
        s.run("caret.set", &json!({"pos": {"story": "body", "path": [0], "off": 0}})).unwrap();
        s.author = BASE_AUTHOR.into();
        s.run("select.text", &json!({"text": "premises"})).unwrap();
        s.run("review.newComment", &json!({"text": "Which premises are meant?"})).unwrap();
        s.run("caret.set", &json!({"pos": {"story": "body", "path": [0], "off": 0}})).unwrap();
        s.begin_edit(BASE_AUTHOR, EARLIER, EditRules { may_decide: false }).unwrap();
        s.run("select.text", &json!({"text": "keeps"})).unwrap();
        s.run("select.collapse", &json!({})).unwrap();
        s.run("text.insert", &json!({"text": "always "})).unwrap();
        s.run("select.text", &json!({"text": "tidy and "})).unwrap();
        s.run("text.delete", &json!({})).unwrap();
        // Read back once, as every file an edit session opens is: a note made here holds its own
        // reference mark as an object, which reading the note's mark does not make again.
        let written = save_bytes("agreement.docx", &s.doc).unwrap();
        save_bytes("agreement.docx", &open_docx_with_limits(&written, &INSPECT_LIMITS).unwrap()).unwrap()
    }

    /// The agreement as an edit session opens it.
    pub fn opened(base: &[u8]) -> Session {
        Session::new(open_docx_with_limits(base, &INSPECT_LIMITS).unwrap())
    }

    pub fn saved(s: &Session) -> Vec<u8> {
        save_bytes("edited.docx", &s.doc).unwrap()
    }

    /// A position in the first part of `kind`, as `caret.set` takes it.
    pub fn in_part(s: &Session, kind: PartKind, off: usize) -> Value {
        let id = s.doc.parts.iter().find(|(_, part)| part.kind == kind).map(|(id, _)| *id).unwrap();
        json!({"pos": {"story": {"part": id}, "path": [0], "off": off}})
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Expect {
        /// New tracked changes by the editor, nothing decided.
        Tracked,
        /// The file's own changes decided, and possibly new tracked changes by the editor.
        Decided,
        /// The save is the file as it was.
        Nothing,
        /// Refused with this code, the save the file as it was.
        Refused(Refusal),
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

    fn cases(base: &[u8]) -> Vec<Case> {
        use Expect::{Decided, Nothing, Refused, Tracked};
        let file = opened(base);
        let header = vec![("caret.set", in_part(&file, PartKind::Header, 5))];
        let footer = vec![("caret.set", in_part(&file, PartKind::Footer, 0))];
        // At the note's end (past its reference mark).
        let footnote = vec![("caret.set", in_part(&file, PartKind::Footnote, usize::MAX))];
        let comment = vec![("caret.set", in_part(&file, PartKind::Comment, 0))];
        let typed = || vec![("select.text", json!({"text": "monthly"})), ("text.insert", json!({"text": "weekly"}))];
        let mut cases = vec![
            case("text.insert", at("monthly"), json!({"text": "calendar "}), Tracked),
            case("text.insert", header.clone(), json!({"text": "main "}), Tracked),
            case("text.insert", footer.clone(), json!({"text": "Second "}), Tracked),
            case("text.insert", footnote.clone(), json!({"text": "Only "}), Tracked),
            case("text.insert", comment.clone(), json!({"text": "Agreed. "}), Refused(Refusal::OutsideEditableStory)),
            case("text.insert", at("monthly"), json!({"text": "a\u{202E}b"}), Refused(Refusal::CharacterNotAllowed)),
            case("text.insert", at("monthly"), json!({"text": "a\u{000C}b"}), Refused(Refusal::CharacterNotAllowed)),
            case("text.insert", at("monthly"), json!({"text": "a\rb"}), Refused(Refusal::ParagraphBreakNotSupported)),
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
            case("text.backspace", comment.clone(), json!({}), Refused(Refusal::OutsideEditableStory)),
            // At a numbered paragraph's start: no numbering removed, no paragraphs joined (6C-10).
            case("text.backspace", at("Notice"), json!({}), Nothing),
            case("text.delete", at("monthly"), json!({}), Tracked),
            case("text.delete", header, json!({}), Tracked),
            // At a paragraph's end: no join (6C-10).
            case("text.delete", after("day."), json!({}), Nothing),
            // Across paragraphs and over the file's own changes: their text is deleted, their marks
            // and the earlier deletion stay as they were.
            case("text.delete", vec![("select.all", json!({}))], json!({}), Tracked),
            case("text.deleteWordBack", after("monthly"), json!({}), Tracked),
            case("text.deleteWordForward", at("monthly"), json!({}), Tracked),
            case("text.deleteWordBack", footnote, json!({}), Tracked),
            case("text.lineBreak", at("monthly"), json!({}), Tracked),
            case("text.nbsp", at("monthly"), json!({}), Tracked),
            case("text.nbHyphen", at("monthly"), json!({}), Tracked),
            case(edit_mode::TAB, at("monthly"), json!({}), Tracked),
            case(edit_mode::TAB, at("Notice"), json!({}), Tracked),
            case(edit_mode::TAB, footer, json!({}), Tracked),
            case("edit.cut", vec![("select.text", json!({"text": "premises"})), ("caret.docEnd", json!({"extend": true}))], json!({}), Tracked),
            case("edit.cut", selecting("monthly"), json!({}), Tracked),
            case("edit.pasteText", selecting("monthly"), json!({"text": "weekly"}), Tracked),
            case("edit.pasteText", selecting("monthly"), json!({"text": "weekly\nor daily"}), Refused(Refusal::ParagraphBreakNotSupported)),
            case("edit.pasteText", comment, json!({"text": "weekly"}), Refused(Refusal::OutsideEditableStory)),
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
        cases.extend(wordcraft_engine::view_only::COMMANDS.iter().map(|id| case(id, vec![], anything.clone(), Nothing)));
        cases
    }

    /// The file edited by a session that may decide (deciding commands) or may not (all others).
    fn save_after(base: &[u8], case: &Case) -> Vec<u8> {
        let mut s = opened(base);
        start_editing(&mut s, edit_mode::DECIDE.contains(&case.id));
        for (id, params) in &case.setup {
            s.run(id, params).unwrap_or_else(|error| panic!("{}: setting up with {id}: {error}", case.id));
        }
        let result = s.run(case.id, &case.params);
        match case.expect {
            Expect::Refused(refusal) => {
                assert_eq!(result.as_ref().map_err(CmdError::edit_refusal), Err(Some(refusal)), "{}", case.id);
                assert_eq!(s.refusal, Some(refusal));
                assert!(s.status.contains(refusal.code()), "{}", s.status);
            }
            // A view command given parameters it does not take may refuse them; it never edits.
            Expect::Nothing if !edit_mode::COMMANDS.contains(&case.id) => {}
            _ => {
                result.unwrap_or_else(|error| panic!("{}: {error}", case.id));
            }
        }
        saved(&s)
    }

    pub fn assert_clean(name: &str, inspection: &Inspection) {
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
    }

    fn check(case: &Case, inspection: &Inspection) {
        let name = format!("{} {} after {:?}", case.id, case.params, case.setup);
        assert_clean(&name, inspection);
        match case.expect {
            Expect::Tracked => {
                assert_eq!(inspection.resolved, Resolved::default(), "{name}: {inspection:#?}");
                assert!(inspection.new_revisions.iter().any(|revisions| revisions.insertions + revisions.deletions > 0), "{name}: {inspection:#?}");
            }
            Expect::Decided => assert!(inspection.resolved.accepted + inspection.resolved.rejected > 0, "{name}: {inspection:#?}"),
            Expect::Nothing | Expect::Refused(_) => assert_eq!(*inspection, Inspection::default(), "{name}"),
        }
    }

    #[test]
    fn every_allowed_command_saves_only_the_editor_s_dated_tracked_changes() {
        let base = agreement();
        let cases = cases(&base);
        for case in &cases {
            check(case, &inspect(&base, &save_after(&base, case)).unwrap());
        }
        // Whatever `allows` lets through, from the whole registry, has a case here.
        let registry = Session::new(Document::default()).registry.clone();
        let mut allowed = 0;
        for spec in registry.all().iter().filter(|spec| edit_mode::allows(spec.id, EditRules { may_decide: true })) {
            allowed += 1;
            assert!(cases.iter().any(|case| case.id == spec.id), "{} is allowed in an edit session but not saved and inspected here", spec.id);
        }
        assert!(allowed > edit_mode::COMMANDS.len(), "the registry looks empty");
    }

    #[test]
    fn accepting_all_keeps_this_session_s_changes_tracked() {
        let base = agreement();
        let mut s = opened(&base);
        start_editing(&mut s, true);
        s.run("select.text", &json!({"text": "monthly"})).unwrap();
        s.run("text.insert", &json!({"text": "weekly"})).unwrap();
        s.run("review.acceptAll", &json!({})).unwrap();
        let inspection = inspect(&base, &saved(&s)).unwrap();
        assert_clean("accept all", &inspection);
        assert_eq!(inspection.resolved, Resolved { accepted: 2, rejected: 0 }, "{inspection:#?}");
        let editor: Vec<(&str, u32, u32)> =
            inspection.new_revisions.iter().map(|revisions| (revisions.author.as_str(), revisions.insertions, revisions.deletions)).collect();
        assert_eq!(editor, vec![(EDITOR, 1, 1)]);
    }

    #[test]
    fn a_session_that_may_not_decide_leaves_the_file_s_own_deletion_when_deleting_over_it() {
        let base = agreement();
        let mut s = opened(&base);
        start_editing(&mut s, false);
        s.run("select.all", &json!({})).unwrap();
        s.run("edit.cut", &json!({})).unwrap();
        let inspection = inspect(&base, &saved(&s)).unwrap();
        assert_clean("cut all", &inspection);
        assert_eq!(inspection.resolved, Resolved::default(), "{inspection:#?}");
    }

    #[test]
    fn tracking_is_on_for_the_whole_session_whatever_the_file_says() {
        let base = agreement();
        let mut s = opened(&base);
        start_editing(&mut s, false);
        s.doc.settings.track_changes = false;
        s.run("select.text", &json!({"text": "monthly"})).unwrap();
        s.run("text.insert", &json!({"text": "weekly"})).unwrap();
        assert!(s.doc.settings.track_changes);
        let inspection = inspect(&base, &saved(&s)).unwrap();
        assert_clean("typing", &inspection);
        assert_eq!(inspection.new_revisions.len(), 1);
    }

    // Ruling 6C-10: the writer cannot record an inserted paragraph mark yet, so nothing in an edit
    // session starts a paragraph: Enter is not offered, and typed or pasted paragraph breaks are refused.
    #[test]
    fn nothing_starts_a_new_paragraph_in_an_edit_session() {
        let mut s = Session::new(Document::from_text("Rent is due."));
        start_editing(&mut s, true);
        let before = serde_json::to_string(&s.doc).unwrap();
        for (id, params, refusal) in [
            ("text.newParagraph", json!({}), Refusal::ParagraphBreakNotSupported),
            ("text.insert", json!({"text": "a\rb"}), Refusal::ParagraphBreakNotSupported),
            ("text.insert", json!({"text": "a\u{2029}b"}), Refusal::ParagraphBreakNotSupported),
            ("edit.pasteText", json!({"text": "a\nb"}), Refusal::ParagraphBreakNotSupported),
            ("edit.pasteText", json!({"text": "a\r\nb"}), Refusal::ParagraphBreakNotSupported),
        ] {
            let refused = s.run(id, &params);
            assert_eq!(refused.as_ref().map_err(CmdError::edit_refusal), Err(Some(refusal)), "{id} {params}");
            assert!(refused.unwrap_err().to_string().contains("not available while editing"));
        }
        assert_eq!(serde_json::to_string(&s.doc).unwrap(), before);
        assert_eq!(s.doc.plain_text(StoryRef::Body), "Rent is due.");
    }

    #[test]
    fn an_edit_session_needs_an_author_and_a_whole_second_utc_date() {
        let mut s = Session::new(Document::from_text("Rent is due."));
        for (author, date) in [
            ("", DATE),
            ("   ", DATE),
            (EDITOR, ""),
            (EDITOR, "2026-10-10"),
            (EDITOR, "2026-10-10T08:00:00.123Z"),
            (EDITOR, "2026-10-10T08:00:00+07:00"),
            (EDITOR, "2026-13-10T08:00:00Z"),
            (EDITOR, "2026-10-10T24:00:00Z"),
        ] {
            let refused = s.begin_edit(author, date, EditRules { may_decide: true });
            assert_eq!(refused.as_ref().map_err(CmdError::edit_refusal), Err(Some(Refusal::InvalidEditSession)), "{author:?} {date:?}");
            assert_eq!((s.edit_rules(), s.revision_date()), (None, None));
        }
        assert!(s.begin_edit(&"x".repeat(256), DATE, EditRules { may_decide: false }).is_err());
        assert!(s.begin_edit(EDITOR, DATE, EditRules { may_decide: false }).is_ok());
        assert_eq!(s.revision_date(), Some(DATE));
        s.end_edit();
        assert_eq!((s.edit_rules(), s.revision_date()), (None, None));
        assert!(s.run("review.trackChanges", &json!({ "value": false })).is_ok(), "after the session the full application runs again");
    }

    #[test]
    fn every_refusal_has_its_own_stable_code() {
        let codes: Vec<&str> = Refusal::ALL.iter().map(|refusal| refusal.code()).collect();
        let mut unique = codes.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), codes.len());
        for code in
            ["paragraph_break_not_supported", "character_not_allowed", "outside_editable_story", "decision_not_allowed", "command_not_allowed"]
        {
            assert!(codes.contains(&code), "{code}");
        }
    }
}

// embed: the review's probes p4–p8 (W2 fix round 1), each a way past the edit session's
// guarantee on 500bc8a.
mod probes {
    use serde_json::json;
    use wordcraft_docx::inspect::{Inspection, Resolved, inspect};
    use wordcraft_engine::CmdError;
    use wordcraft_engine::doc::{PartKind, StoryRef};
    use wordcraft_engine::edit_mode::{EditRules, Refusal};

    use super::saves::{EDITOR, agreement, in_part, opened, saved};
    use super::{DATE, start_editing};

    // p4: a file revision named and dated like the session is still the file's: the session's
    // typing makes records of its own, and deleting the file's insertion marks it deleted, without
    // the permission to decide.
    #[test]
    fn a_file_revision_named_and_dated_like_the_session_is_never_its_own() {
        let mut file = opened(&agreement());
        file.begin_edit(EDITOR, DATE, EditRules { may_decide: false }).unwrap();
        file.run("select.text", &json!({"text": "first"})).unwrap();
        file.run("text.insert", &json!({"text": "second"})).unwrap();
        let base = saved(&file);
        let mut s = opened(&base);
        let records = s.doc.revisions.len();
        start_editing(&mut s, false);
        s.run("select.text", &json!({"text": "day"})).unwrap();
        s.run("select.collapse", &json!({})).unwrap();
        s.run("text.insert", &json!({"text": "working "})).unwrap();
        assert!(s.doc.revisions.len() > records, "the session made its own record");
        s.run("select.text", &json!({"text": "second"})).unwrap();
        s.run("text.delete", &json!({})).unwrap();
        assert!(s.doc.plain_text(StoryRef::Body).contains("second"), "marked deleted, not removed");
        // Nothing of the file's was decided or changed untracked. The inspector knows a change by
        // its author and date, so it cannot tell this session's changes from the file's and
        // reports them as altered: a host refuses such a save, failing closed.
        let inspection = inspect(&base, &saved(&s)).unwrap();
        assert_eq!(inspection.resolved, Resolved::default(), "{inspection:#?}");
        assert_eq!(inspection.untracked_changes, 0, "{inspection:#?}");
        assert!(inspection.losses.is_empty() && inspection.comments == Default::default(), "{inspection:#?}");
    }

    // p5: a decision undone while the session may decide is not redone once it may not.
    #[test]
    fn redo_never_replays_a_decision_after_the_permission_is_gone() {
        let base = agreement();
        let mut s = opened(&base);
        start_editing(&mut s, true);
        s.run("review.acceptAll", &json!({})).unwrap();
        s.run("edit.undo", &json!({})).unwrap();
        start_editing(&mut s, false);
        let _ = s.run("edit.redo", &json!({}));
        assert_eq!(inspect(&base, &saved(&s)).unwrap(), Inspection::default());
    }

    // p6: undo stops at the session's start, whatever came before it.
    #[test]
    fn undo_never_reaches_past_the_session_s_start() {
        let mut s = opened(&agreement());
        s.run("select.text", &json!({"text": "monthly"})).unwrap();
        s.run("text.insert", &json!({"text": "weekly"})).unwrap();
        s.run("para.numbering", &json!({})).unwrap();
        start_editing(&mut s, false);
        let start = saved(&s);
        assert!(!s.can_undo());
        s.run("edit.undo", &json!({})).unwrap();
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(inspect(&start, &saved(&s)).unwrap(), Inspection::default());
    }

    // p7: a refused paragraph break changes nothing, keeps the selection and tells the host why,
    // by its code, in the error and the status.
    #[test]
    fn a_refused_paragraph_break_says_why_by_its_code_and_changes_nothing() {
        let base = agreement();
        let mut s = opened(&base);
        start_editing(&mut s, false);
        s.run("select.text", &json!({"text": "monthly"})).unwrap();
        let selection = s.sel.clone();
        let refused = s.run("edit.pasteText", &json!({"text": "weekly\nor daily"})).unwrap_err();
        assert_eq!(refused.edit_refusal(), Some(Refusal::ParagraphBreakNotSupported));
        assert_eq!(s.refusal, Some(Refusal::ParagraphBreakNotSupported));
        assert!(s.status.contains("paragraph_break_not_supported"), "{}", s.status);
        assert_eq!(s.sel, selection);
        assert_eq!(inspect(&base, &saved(&s)).unwrap(), Inspection::default());
    }

    // p8: a comment's text cannot be reached by moving the caret into it.
    #[test]
    fn a_comment_is_never_written_from_an_edit_session() {
        let base = agreement();
        let mut s = opened(&base);
        start_editing(&mut s, true);
        s.run("caret.set", &in_part(&s, PartKind::Comment, 0)).unwrap();
        for (id, params) in [
            ("text.insert", json!({"text": "Agreed. "})),
            ("text.delete", json!({})),
            ("edit.pasteText", json!({"text": "x"})),
            ("text.tab", json!({})),
        ] {
            let refused = s.run(id, &params);
            assert_eq!(refused.as_ref().map_err(CmdError::edit_refusal), Err(Some(Refusal::OutsideEditableStory)), "{id}");
        }
        let inspection = inspect(&base, &saved(&s)).unwrap();
        assert_eq!(inspection.comments, Default::default(), "{inspection:#?}");
    }
}

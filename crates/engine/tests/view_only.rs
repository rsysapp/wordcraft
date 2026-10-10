//! A view-only session (the embed build's view mode) runs navigation, selection, copy, find and
//! zoom, and refuses everything else before it starts.

use serde_json::json;
use wordcraft_engine::{CmdError, Session, view_only};

fn viewing() -> Session {
    let mut s = Session::new(wordcraft_engine::sample::sample_document());
    s.view_only = true;
    s
}

fn snapshot(s: &Session) -> String {
    serde_json::to_string(&s.doc).expect("document serialises")
}

#[test]
fn every_command_outside_the_allow_list_is_refused_and_changes_nothing() {
    let mut s = viewing();
    let before = snapshot(&s);
    let rev = s.rev();
    // A path in the temp dir: if a file command were ever let through, it writes nowhere that matters.
    let scratch = std::env::temp_dir().join(format!("wordcraft-view-only-{}", std::process::id())).join("out.docx");
    let ids: Vec<&'static str> = s.registry.all().iter().map(|c| c.id).filter(|id| !view_only::allows(id)).collect();
    assert!(ids.len() > 100, "registry looks empty");
    for id in ids {
        for params in [json!({}), json!({"text": "x", "value": true, "path": scratch.to_string_lossy()})] {
            let result = s.run(id, &params);
            assert!(
                matches!(result, Err(CmdError::Disabled(ref why)) if why.contains("viewing only")),
                "{id} ran in a view-only session: {result:?}"
            );
        }
    }
    assert_eq!(s.rev(), rev);
    assert!(!s.dirty);
    assert_eq!(snapshot(&s), before);
}

#[test]
fn the_allow_list_names_only_registered_commands_that_leave_the_document_alone() {
    let s = viewing();
    for id in view_only::COMMANDS {
        let spec = s.registry.get(id).unwrap_or_else(|| panic!("{id} is not a registered command"));
        assert!(!spec.mutates, "{id} mutates the document");
        assert!(!matches!(*id, "edit.undo" | "edit.redo" | "edit.repeat"), "{id} restores other document states");
    }
}

#[test]
fn allowed_commands_run_without_changing_the_document() {
    let mut s = viewing();
    let before = snapshot(&s);
    for id in view_only::COMMANDS {
        let _ = s.run(id, &json!({}));
        let _ = s.run(id, &json!({"text": "the", "page": 2, "value": 150}));
    }
    assert_eq!(snapshot(&s), before);
    assert!(!s.dirty);
    assert!(s.run("caret.docEnd", &json!({})).is_ok());
}

#[test]
fn ordinary_sessions_are_not_restricted() {
    let mut s = Session::new(wordcraft_engine::doc::Document::from_text("abc"));
    assert!(s.run("text.insert", &json!({"text": "x"})).is_ok());
}

#[test]
fn history_methods_are_closed_in_a_view_only_session() {
    let mut s = Session::new(wordcraft_engine::doc::Document::from_text("abc"));
    s.run("text.insert", &json!({"text": "x"})).expect("insert");
    let edited = snapshot(&s);
    s.view_only = true;
    assert!(!s.undo());
    assert!(!s.redo());
    assert_eq!(snapshot(&s), edited);
    s.view_only = false;
    assert!(s.undo(), "undo works again once the session is editable");
}

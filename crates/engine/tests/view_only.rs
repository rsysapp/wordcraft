//! A view-only session (the embed build's view mode) runs navigation, selection, copy, find and
//! zoom, and refuses everything else before it starts.

use std::collections::BTreeMap;

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

/// Everything an allowed command must leave alone: the document (serialised, and as the docx
/// bytes so media and passthrough parts are covered), the revision, dirty flag, path, author,
/// autosave setting and the undo/redo history.
#[derive(PartialEq)]
struct Untouched {
    doc_json: String,
    docx: Vec<u8>,
    /// Not serialised with the document, so compared directly.
    media: BTreeMap<String, Vec<u8>>,
    passthrough: BTreeMap<String, Vec<u8>>,
    rev: u64,
    dirty: bool,
    path: Option<std::path::PathBuf>,
    author: String,
    autosave: bool,
    undo_depth: usize,
    can_redo: bool,
    /// Every view setting except the ones the allow-list exists to change (zoom, fit, navigation pane).
    view_chrome: serde_json::Value,
}

fn untouched(s: &Session) -> Untouched {
    let mut view_chrome = serde_json::to_value(&s.view).expect("view state serialises");
    if let Some(fields) = view_chrome.as_object_mut() {
        // Find opens the navigation pane, and zoom commands set zoom and fit (and, since the upstream
        // merge, whether fitted pages stand in one column): those are the point.
        for zoom_field in ["zoom", "fit", "multiPage", "singleColumn", "navPane"] {
            fields.remove(zoom_field);
        }
    }
    Untouched {
        doc_json: snapshot(s),
        docx: wordcraft_docx::write(&s.doc).expect("document writes as docx"),
        media: s.doc.media.iter().map(|(name, bytes)| (name.clone(), bytes.as_ref().clone())).collect(),
        passthrough: s.doc.passthrough.iter().map(|(name, bytes)| (name.clone(), bytes.as_ref().clone())).collect(),
        rev: s.rev(),
        dirty: s.dirty,
        path: s.path.clone(),
        author: s.author.clone(),
        autosave: s.autosave,
        undo_depth: s.undo_labels().len(),
        can_redo: s.can_redo(),
        view_chrome,
    }
}

/// Names of the fields that differ (a failure message must not dump the document).
fn changed_fields(before: &Untouched, after: &Untouched) -> Vec<&'static str> {
    let checks = [
        ("document", before.doc_json != after.doc_json),
        ("docx bytes", before.docx != after.docx),
        ("media", before.media != after.media),
        ("preserved parts", before.passthrough != after.passthrough),
        ("rev", before.rev != after.rev),
        ("dirty", before.dirty != after.dirty),
        ("path", before.path != after.path),
        ("author", before.author != after.author),
        ("autosave", before.autosave != after.autosave),
        ("undo depth", before.undo_depth != after.undo_depth),
        ("redo", before.can_redo != after.can_redo),
        ("view", before.view_chrome != after.view_chrome),
    ];
    checks.into_iter().filter_map(|(name, differs)| differs.then_some(name)).collect()
}

#[test]
fn allowed_commands_leave_document_file_and_settings_alone() {
    let scratch_dir = std::env::temp_dir().join(format!("wordcraft-view-only-allowed-{}", std::process::id()));
    let scratch = scratch_dir.join("out.docx");
    let scratch_str = scratch.to_string_lossy().to_string();
    // The folder exists, so a file command that is let through really can write into it.
    std::fs::create_dir_all(&scratch_dir).expect("scratch folder");
    // A session that already has history, a dirty flag and a path, so a command that clears any of
    // them is noticed.
    let mut s = Session::new(wordcraft_engine::sample::sample_document());
    s.run("text.insert", &json!({"text": "edited"})).expect("insert");
    // Neither map is serialised with the document, so seed them: an allowed command that dropped
    // an image or a preserved part must be noticed.
    s.doc.add_media(vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4], "png");
    s.doc.passthrough.insert("docProps/custom.xml".to_string(), std::sync::Arc::new(b"<Properties/>".to_vec()));
    assert!(!s.doc.media.is_empty() && !s.doc.passthrough.is_empty());
    s.path = Some(scratch.clone());
    s.view_only = true;
    for id in view_only::COMMANDS {
        for params in [
            json!({}),
            json!({"text": "the", "page": 2, "value": 150, "name": "renamed.docx", "path": scratch_str, "author": "Someone", "bookmark": "x", "paragraph": 2}),
        ] {
            let before = untouched(&s);
            let _ = s.run(id, &params);
            let after = untouched(&s);
            assert!(before == after, "{id} changed {:?} with {params}", changed_fields(&before, &after));
        }
    }
    let written: Vec<_> = std::fs::read_dir(&scratch_dir).expect("scratch folder").flatten().map(|entry| entry.file_name()).collect();
    let _ = std::fs::remove_dir_all(&scratch_dir);
    assert!(written.is_empty(), "an allowed command wrote {written:?}");
    assert!(s.run("caret.docEnd", &json!({})).is_ok());
}

#[test]
fn the_allow_list_has_no_file_tool_insert_or_format_commands() {
    const FORBIDDEN_PREFIXES: &[&str] = &["file.", "tools.", "review.", "mailings.", "insert.", "format.", "document.", "macro"];
    const FORBIDDEN_EDIT: &[&str] =
        &["edit.undo", "edit.redo", "edit.repeat", "edit.cut", "edit.formatPainter", "edit.copyFormat", "edit.pasteFormat"];
    for id in view_only::COMMANDS {
        assert!(!FORBIDDEN_PREFIXES.iter().any(|prefix| id.starts_with(prefix)), "{id} is in a family a viewer must not run");
        assert!(!FORBIDDEN_EDIT.contains(id), "{id} edits or restores the document");
        assert!(!id.starts_with("edit.paste") && !id.starts_with("edit.replace"), "{id} edits the document");
    }
}

#[test]
fn goto_with_an_enormous_page_number_stays_on_the_last_page() {
    let mut s = viewing();
    // 2^32 truncates to 0 as a 32-bit usize (the web build); it must clamp, never underflow.
    for page in [4_294_967_296u64, 4_294_967_297, u64::MAX, 0] {
        assert!(s.run("edit.goto", &json!({"page": page})).is_ok(), "page {page}");
        assert!(s.run("edit.goto", &json!({"paragraph": page})).is_ok(), "paragraph {page}");
    }
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

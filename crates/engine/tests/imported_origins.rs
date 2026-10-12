//! embed: a paragraph brought in from another file, the clipboard, a Quick Part or a merge record is
//! new content: it claims no origin in the package the document was read from (Ruling 6C-33).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use wordcraft_doc::table::{Cell, Table};
use wordcraft_doc::{Block, Document, Origin, Pos, StoryRef};
use wordcraft_engine::{Selection, Session};

/// A document as read from a Word file, so its paragraphs have origins.
fn read_back(doc: &Document) -> Document {
    wordcraft_docx::read(&wordcraft_docx::write(doc).unwrap()).unwrap()
}

/// Every paragraph of the body, cells included, with its origin.
fn origins(doc: &Document) -> Vec<(String, Option<Origin>)> {
    doc.para_paths(StoryRef::Body).iter().filter_map(|path| doc.para(StoryRef::Body, path)).map(|p| (p.plain_text(), p.origin)).collect()
}

fn origins_of(doc: &Document, text: &str) -> Vec<Option<Origin>> {
    origins(doc).into_iter().filter(|(found, _)| found == text).map(|(_, origin)| origin).collect()
}

/// A Word file on disk, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn holding(name: &str, doc: &Document) -> TempFile {
        let path = std::env::temp_dir().join(format!("imported-origins-{}-{name}.docx", std::process::id()));
        std::fs::write(&path, wordcraft_docx::write(doc).unwrap()).unwrap();
        TempFile(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Selects "Two." with the paragraph marks around it, from the end of "One." to the start of "Three.".
fn session_selecting_two() -> Session {
    let mut s = Session::new(read_back(&Document::from_text("One.\nTwo.\nThree.")));
    s.sel = Selection { anchor: Pos::body(0, 4), focus: Pos::body(2, 0) };
    s
}

fn caret_at_end(s: &mut Session) {
    s.sel = Selection::caret(s.doc.end_of(StoryRef::Body));
}

#[test]
fn text_from_a_file_brings_in_no_origins() {
    let mut other = Document::from_text("One.\nTwo.");
    let mut table = Table::new(1, 1, 200.0);
    table.rows[0].cells[0] = Cell::with_text("Cell.");
    other.body.push(Arc::new(Block::Table(table)));
    other.ensure_nonempty();
    let file = TempFile::holding("text-from-file", &other);
    let mut s = Session::new(read_back(&Document::from_text("Keep this.")));
    s.run("insert.textFromFile", &json!({"path": file.0.to_string_lossy()})).unwrap();
    assert_eq!(origins_of(&s.doc, "Two."), [None]);
    assert_eq!(origins_of(&s.doc, "Cell."), [None]);
}

#[test]
fn pasted_paragraphs_bring_in_no_origins() {
    let mut s = session_selecting_two();
    let original = origins_of(&s.doc, "Two.");
    s.run("edit.copy", &json!({})).unwrap();
    caret_at_end(&mut s);
    s.run("edit.paste", &json!({})).unwrap();
    assert_eq!(origins_of(&s.doc, "Two."), [original[0], None]);
}

#[test]
fn a_quick_part_brings_in_no_origins() {
    let mut s = session_selecting_two();
    let original = origins_of(&s.doc, "Two.");
    s.run("insert.quickParts", &json!({"save": "part"})).unwrap();
    caret_at_end(&mut s);
    s.run("insert.quickParts", &json!({"insert": "part"})).unwrap();
    assert_eq!(origins_of(&s.doc, "Two."), [original[0], None]);
}

#[test]
fn merged_records_bring_in_no_origins() {
    let mut s = Session::new(read_back(&Document::from_text("Dear reader.")));
    s.run("mailings.recipients", &json!({"csv": "Name\nAda\nAlan"})).unwrap();
    s.run("mailings.finish", &json!({})).unwrap();
    // One paragraph a record; a page break ends each record but the last.
    assert_eq!(origins(&s.doc).len(), 2);
    assert!(origins(&s.doc).iter().all(|(_, origin)| origin.is_none()), "{:?}", origins(&s.doc));
}

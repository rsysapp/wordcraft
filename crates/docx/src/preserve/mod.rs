//! embed: a writer that keeps the source package. A document read from a Word package carries that
//! package ([`SOURCE_PACKAGE`]) and each paragraph where it came from (`Paragraph::origin`). Saving
//! rewrites only the paragraphs whose characters or tracked changes differ, in place, by their
//! steps (`plan`, `emit`); every other byte of every part is the source's, and every part,
//! relationship and content type of the source stays as it was. A change that cannot be written that
//! way is refused with its reason, never written by the regenerating writer, and every result is
//! checked by the inspector (`crate::inspect`) before it is returned.

mod emit;
mod plan;
mod scan;
pub(crate) mod tree;

use std::collections::{BTreeMap, BTreeSet, HashSet};

use quick_xml::events::Event;
use wordcraft_doc::{Block, CharProps, Document, Origin, Paragraph, RevisionKind, StoryRef};

use crate::DocxError;
use crate::inspect::budget::Budget;
use crate::inspect::{CommentChanges, InspectError, MAX_WORK, memory_limit};
use crate::package::{Package, ReadLimits, entry_order, zip_entries};

/// The key in `Document::passthrough` under which a document read from a Word package keeps the
/// package's bytes. Not a part name: nothing writes it as a part.
pub const SOURCE_PACKAGE: &str = "wordcraft:source-package";

/// Why a save cannot be written without changing something outside the tracked changes. Shown to
/// the person saving, so in plain words; part names say where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unpreservable {
    NoSource,
    Structure,
    Formatting,
    Untracked,
    Altered,
    InsertedObject,
    TooManyChanges,
    Unmapped {
        part: String,
    },
    Locked {
        part: String,
    },
    SplitWrapper {
        part: String,
    },
    Outside(&'static str),
    NotStory {
        part: String,
    },
    Namespace {
        part: String,
    },
    Verification(String),
    /// embed: something the reader now reads that this writer was not built to rewrite (an object
    /// of a kind it does not know, a move made in the session): refused rather than guessed.
    Unsupported(&'static str),
}

impl std::fmt::Display for Unpreservable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unpreservable::NoSource => write!(f, "the document was not read from a Word file that can be read again"),
            Unpreservable::Structure => write!(f, "paragraphs were added, removed or moved in a way a Word file cannot record as tracked changes"),
            Unpreservable::Formatting => write!(f, "formatting or paragraph settings changed, which a Word file cannot record as tracked changes"),
            Unpreservable::Untracked => write!(f, "text changed without a tracked change"),
            Unpreservable::Altered => write!(f, "an earlier tracked change was altered"),
            Unpreservable::InsertedObject => write!(f, "an object was inserted"),
            Unpreservable::TooManyChanges => write!(f, "there are too many changes to check at once; save smaller sets of changes"),
            Unpreservable::Unmapped { part } => write!(f, "a paragraph in {part} uses markup that cannot be edited yet"),
            Unpreservable::Locked { part } => write!(f, "the change touches a field, an equation or alternative content in {part}"),
            Unpreservable::SplitWrapper { part } => write!(f, "the change would split a hyperlink or content control in {part}"),
            Unpreservable::Outside(what) => write!(f, "{what} changed"),
            Unpreservable::NotStory { part } => write!(f, "text in {part} cannot be edited here"),
            Unpreservable::Namespace { part } => write!(f, "{part} is written in a form this writer does not edit"),
            Unpreservable::Verification(detail) => write!(f, "the saved file would differ outside the tracked changes ({detail})"),
            Unpreservable::Unsupported(what) => write!(f, "{what} cannot be saved as a tracked change yet"),
        }
    }
}

/// The story parts whose paragraphs a save may rewrite. Comments are the host's threads.
const STORY_ROOTS: &[&str] = &["w:document", "w:hdr", "w:ftr", "w:footnotes", "w:endnotes"];

/// The document as a Word package that keeps its source package: only changed paragraphs are
/// rewritten, as tracked changes. A change that cannot be written that way is
/// [`DocxError::Preserve`], with a reason a person can act on; nothing is written lossily.
pub fn write_preserving(doc: &Document) -> Result<Vec<u8>, DocxError> {
    preserve(doc).map_err(|reason| DocxError::Preserve(format!("This change cannot be saved without altering the document: {reason}.")))
}

/// How many paragraphs a document's stories have, and which of them (part, position) the
/// preserving writer could not map exactly. Every one must be mapped before an edit of it can be
/// saved; a corpus check reports this.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreservationReport {
    pub paragraphs: usize,
    pub unmapped: Vec<(String, u32)>,
}

/// Where the preserving writer cannot map the paragraphs of the Word package `bytes`.
pub fn preservation_report(bytes: &[u8]) -> Result<PreservationReport, DocxError> {
    let doc = crate::read(bytes)?;
    let pkg = Package::open_with(bytes, &ReadLimits::default())?;
    let mut wanted: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    let mut expected: Vec<(String, u32, &str)> = Vec::new();
    for story in stories(&doc) {
        for paragraph in paragraphs(&doc, story) {
            let Some(origin) = paragraph.origin else { continue };
            let Some(part) = pkg.part_name(origin.part) else { continue };
            wanted.entry(part.to_string()).or_default().insert(origin.index);
            expected.push((part.to_string(), origin.index, paragraph.text.as_str()));
        }
    }
    let mut budget = Budget::new(MAX_WORK, memory_limit(bytes.len(), bytes.len()));
    let scanned = scan::scan_package(&pkg, &wanted, &mut budget)?;
    let unmapped = expected
        .iter()
        .filter(|(part, index, text)| {
            scanned.get(part).and_then(|scanned| scanned.paragraphs.get(index)).is_none_or(|scanned| scanned.text() != *text)
        })
        .map(|(part, index, _)| (part.clone(), *index))
        .collect();
    Ok(PreservationReport { paragraphs: expected.len(), unmapped })
}

fn stories(doc: &Document) -> Vec<StoryRef> {
    std::iter::once(StoryRef::Body).chain(doc.parts.keys().map(|id| StoryRef::Part(*id))).collect()
}

fn paragraphs(doc: &Document, story: StoryRef) -> Vec<&Paragraph> {
    doc.para_paths(story).iter().filter_map(|path| doc.para(story, path)).collect()
}

/// Text typed into the paragraph the reader supplies after a body that ends in a table (Word's own
/// files always hold one there, a generated file may not): the file has no paragraph to rewrite, so
/// the writer appends one before the body's section properties, as a tracked insertion.
struct Appended<'a> {
    edited: &'a Paragraph,
}

/// A source paragraph whose edited group (itself, or its halves after Enter) differs from it.
struct Changed<'a> {
    origin: Origin,
    original: &'a Paragraph,
    edited: Vec<&'a Paragraph>,
}

type Planned<'a> = (String, u32, &'a Paragraph, Vec<plan::Step>);

fn preserve(doc: &Document) -> Result<Vec<u8>, Unpreservable> {
    let source = doc.passthrough.get(SOURCE_PACKAGE).ok_or(Unpreservable::NoSource)?;
    let original = crate::read(source).map_err(|_| Unpreservable::NoSource)?;
    unchanged_outside_paragraphs(&original, doc)?;
    let (changes, appended) = changed_paragraphs(&original, doc)?;
    if changes.is_empty() && appended.is_none() {
        // Nothing changed: the file as it came (the edit session's tracking switch is not saved).
        return Ok(source.to_vec());
    }
    let pkg = Package::open_with(source, &ReadLimits::default()).map_err(|_| Unpreservable::NoSource)?;
    let mut budget = Budget::new(MAX_WORK, memory_limit(source.len(), source.len()));
    let mut wanted: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    let mut planned: Vec<Planned<'_>> = Vec::new();
    for change in &changes {
        let part = pkg.part_name(change.origin.part).ok_or(Unpreservable::Structure)?.to_string();
        let steps = plan::plan(change.original, &original, &change.edited, doc, &mut budget)?;
        wanted.entry(part.clone()).or_default().insert(change.origin.index);
        planned.push((part, change.origin.index, change.original, steps));
    }
    let main = scan::main_part(&pkg);
    if appended.is_some() {
        // The part is walked even when no paragraph of it is rewritten.
        wanted.entry(main.clone()).or_default();
    }
    let scanned = scan::scan_package(&pkg, &wanted, &mut budget).map_err(|_| Unpreservable::NoSource)?;
    let mut ids = ids_of(&pkg);
    let mut rewritten_parts: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for name in wanted.keys() {
        let part = scanned.get(name).ok_or_else(|| Unpreservable::Unmapped { part: name.clone() })?;
        if !STORY_ROOTS.contains(&part.root.as_str()) {
            return Err(Unpreservable::NotStory { part: name.clone() });
        }
        if part.prefix.is_empty() {
            return Err(Unpreservable::Namespace { part: name.clone() });
        }
        let mut here: Vec<&Planned<'_>> = planned.iter().filter(|(part_name, ..)| part_name == name).collect();
        // A text box's paragraph before the paragraph that holds it: copying the outer one carries it.
        here.sort_by_key(|(_, index, ..)| part.paragraphs.get(index).map_or(0, |scanned| scanned.span().len()));
        let mut rewritten: BTreeMap<usize, (usize, Vec<u8>)> = BTreeMap::new();
        for (_, index, original_paragraph, steps) in here {
            let scanned_paragraph = part.paragraphs.get(index).ok_or_else(|| Unpreservable::Unmapped { part: name.clone() })?;
            if scanned_paragraph.text() != original_paragraph.text {
                return Err(Unpreservable::Unmapped { part: name.clone() });
            }
            let bytes = emit::rewrite(
                &emit::Source { bytes: &part.bytes, rewritten: &rewritten },
                &part.prefix,
                name,
                scanned_paragraph,
                steps,
                &mut ids,
                &mut budget,
            )?;
            let span = scanned_paragraph.span();
            rewritten.insert(span.start, (span.end, bytes));
        }
        if let Some(appended) = &appended
            && *name == main
        {
            let (at, bytes) = appended_paragraph(&pkg, &main, &part.prefix, appended, doc, &mut ids, &mut budget)?;
            rewritten.insert(at, (at, bytes));
        }
        let mut out = Vec::with_capacity(part.bytes.len());
        emit::Source { bytes: &part.bytes, rewritten: &rewritten }.copy(0..part.bytes.len(), &mut out).map_err(|_| Unpreservable::Structure)?;
        rewritten_parts.insert(name.clone(), out);
    }
    let order = entry_order(source).map_err(|_| Unpreservable::NoSource)?;
    let entries: Vec<(String, Vec<u8>)> = order
        .into_iter()
        .filter_map(|name| {
            let bytes = rewritten_parts.get(&name).cloned().or_else(|| pkg.get(&name).map(<[u8]>::to_vec))?;
            Some((name, bytes))
        })
        .collect();
    let saved = zip_entries(&entries).map_err(|_| Unpreservable::NoSource)?;
    verify(source, &saved, &original, doc)?;
    Ok(saved)
}

/// An edit session changes paragraphs only; anything else changed could not be written.
fn unchanged_outside_paragraphs(before: &Document, after: &Document) -> Result<(), Unpreservable> {
    let mut settings = after.settings.clone();
    // An edit session switches tracking on; the file keeps its own setting.
    settings.track_changes = before.settings.track_changes;
    let parts = |doc: &Document| doc.parts.iter().map(|(id, part)| (*id, part.kind)).collect::<Vec<_>>();
    let checks = [
        (before.last_section == after.last_section, "the section settings"),
        (before.styles == after.styles, "the styles"),
        (before.numbering == after.numbering, "the numbering"),
        (before.comments == after.comments, "the comments"),
        (before.sources == after.sources, "the sources"),
        (settings == before.settings, "the document settings"),
        (before.media.keys().eq(after.media.keys()), "the pictures"),
        (parts(before) == parts(after), "the notes, headers or text boxes"),
    ];
    match checks.iter().find(|(same, _)| !same) {
        Some((_, what)) => Err(Unpreservable::Outside(what)),
        None => Ok(()),
    }
}

/// Each source paragraph and its edited group, in story order; a paragraph the reader made itself
/// (no origin) must be untouched, and nothing may appear, vanish or move. The one exception is the
/// reader's trailing paragraph after a final table, which may be typed into ([`Appended`]).
fn changed_paragraphs<'a>(before: &'a Document, after: &'a Document) -> Result<(Vec<Changed<'a>>, Option<Appended<'a>>), Unpreservable> {
    let mut changed = Vec::new();
    let mut appended = None;
    for story in stories(before) {
        let mut edited = paragraphs(after, story).into_iter().peekable();
        let originals = paragraphs(before, story);
        let last = originals.len().saturating_sub(1);
        for (position, paragraph) in originals.into_iter().enumerate() {
            let Some(origin) = paragraph.origin else {
                match edited.next() {
                    Some(same) if same.origin.is_none() && same == paragraph => continue,
                    Some(same) if position == last && story == StoryRef::Body && is_trailing_blank(before, paragraph) && same.origin.is_none() => {
                        appended = Some(Appended { edited: same });
                        continue;
                    }
                    _ => return Err(Unpreservable::Structure),
                }
            };
            let mut group = Vec::new();
            while let Some(next) = edited.peek() {
                if next.origin != Some(origin) {
                    break;
                }
                group.push(*next);
                edited.next();
            }
            match group.as_slice() {
                [] => return Err(Unpreservable::Structure),
                [only] if *only == paragraph => {}
                _ => changed.push(Changed { origin, original: paragraph, edited: group }),
            }
        }
        if edited.next().is_some() {
            return Err(Unpreservable::Structure);
        }
    }
    Ok((changed, appended))
}

/// Whether `paragraph` is the empty paragraph the reader puts after a body that ends in a table.
fn is_trailing_blank(before: &Document, paragraph: &Paragraph) -> bool {
    let blocks = &before.body;
    let after_a_table = blocks.len().checked_sub(2).and_then(|index| blocks.get(index)).is_some_and(|block| matches!(&**block, Block::Table(_)));
    let last_is_it = matches!(blocks.last().map(|block| &**block), Some(Block::Para(last)) if std::ptr::eq(last, paragraph));
    after_a_table && last_is_it && paragraph.text.is_empty() && paragraph.objects.is_empty()
}

/// The paragraph typed into the reader's trailing paragraph, written as a new last paragraph of the
/// body, before its section properties: its mark and its text are the session's tracked insertions.
/// Only plain typed text is written (no formatting, tabs, breaks or objects); anything else is
/// refused. Returns where it goes in the part and its bytes.
fn appended_paragraph(
    pkg: &Package,
    main: &str,
    prefix: &str,
    appended: &Appended<'_>,
    after: &Document,
    ids: &mut emit::Ids,
    budget: &mut Budget,
) -> Result<(usize, Vec<u8>), Unpreservable> {
    let edited = appended.edited;
    let plain = !edited.text.is_empty()
        && edited.objects.is_empty()
        && !edited.runs.is_empty()
        && edited.text.chars().all(|ch| !ch.is_control() && ch != wordcraft_doc::para::OBJ)
        && edited.props == Paragraph::new().props;
    if !plain {
        return Err(Unpreservable::Formatting);
    }
    // Every run is typed in this session, as one tracked insertion, with no formatting of its own.
    let mut revision = None;
    for run in &edited.runs {
        let mut props = run.props.clone();
        let inserted = props.ins.take();
        if inserted.is_none() || props.del.is_some() || props != CharProps::default() || (revision.is_some() && revision != inserted) {
            return Err(Unpreservable::Untracked);
        }
        revision = inserted;
    }
    let found = revision.and_then(|id| after.revisions.get(usize::try_from(id).ok()?)).filter(|found| found.kind == RevisionKind::Insert);
    let Some(found) = found else { return Err(Unpreservable::Untracked) };
    let parsed = pkg.tree(main, budget.room()).map_err(|_| Unpreservable::NoSource)?.ok_or(Unpreservable::NoSource)?;
    let body = parsed.root.els().find(|element| element.local() == "body").ok_or(Unpreservable::Structure)?;
    let at = match body.els().last().filter(|element| element.local() == "sectPr") {
        Some(section) => section.start.start,
        None => body.end.as_ref().map(|end| end.start).ok_or(Unpreservable::Structure)?,
    };
    let (author, date) = (crate::xml::esc(&found.author), crate::xml::esc(&found.date));
    let change = |ids: &mut emit::Ids| -> Result<String, Unpreservable> {
        Ok(format!(r#"{prefix}:id="{}" {prefix}:author="{author}" {prefix}:date="{date}""#, ids.revision()?))
    };
    let (mark, text) = (change(ids)?, change(ids)?);
    let paragraph = format!(
        r#"<{prefix}:p><{prefix}:pPr><{prefix}:rPr><{prefix}:ins {mark}/></{prefix}:rPr></{prefix}:pPr><{prefix}:ins {text}><{prefix}:r><{prefix}:t xml:space="preserve">{}</{prefix}:t></{prefix}:r></{prefix}:ins></{prefix}:p>"#,
        crate::xml::esc(&edited.text)
    );
    Ok((at, paragraph.into_bytes()))
}

/// The highest numeric `w:id` and every `w14:paraId`/`w14:textId` in the package's
/// WordprocessingML parts, so new ones are unique.
fn ids_of(pkg: &Package) -> emit::Ids {
    let mut highest = 0u64;
    let mut paragraph_ids = HashSet::new();
    for name in pkg.part_names().filter(|name| name.starts_with("word/") && name.ends_with(".xml")) {
        let Some(bytes) = pkg.get(name) else { continue };
        let mut reader = quick_xml::Reader::from_reader(bytes);
        loop {
            match reader.read_event() {
                Ok(Event::Start(element) | Event::Empty(element)) => {
                    for attribute in element.attributes().with_checks(false).flatten() {
                        let value = String::from_utf8_lossy(&attribute.value);
                        match attribute.key.local_name().as_ref() {
                            b"id" => {
                                if let Ok(id) = value.trim().parse::<u64>() {
                                    highest = highest.max(id);
                                }
                            }
                            b"paraId" | b"textId" => {
                                if let Ok(id) = u32::from_str_radix(value.trim(), 16) {
                                    paragraph_ids.insert(id);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }
    emit::Ids::new(highest, paragraph_ids)
}

/// The host's own check, before the file leaves: nothing but this session's tracked changes and
/// the decided ones may differ.
fn verify(source: &[u8], saved: &[u8], before: &Document, after: &Document) -> Result<(), Unpreservable> {
    let inspection = crate::inspect::inspect(source, saved).map_err(|error| {
        Unpreservable::Verification(match error {
            InspectError::Base(_) => "the original file cannot be checked".to_string(),
            InspectError::Edited(_) => "the saved file cannot be read back".to_string(),
        })
    })?;
    if inspection.too_many_changes {
        return Err(Unpreservable::TooManyChanges);
    }
    let known: HashSet<(&str, &str)> = before.revisions.iter().map(|found| (found.author.as_str(), found.date.as_str())).collect();
    let mine: HashSet<&str> = after
        .revisions
        .iter()
        .filter(|found| !known.contains(&(found.author.as_str(), found.date.as_str())))
        .map(|found| found.author.as_str())
        .collect();
    let mut problems: Vec<String> = inspection.losses.iter().map(|loss| loss.part.clone()).collect();
    if inspection.untracked_changes > 0 {
        problems.push("changes that are not tracked".into());
    }
    if inspection.altered_revisions > 0 {
        problems.push("earlier tracked changes".into());
    }
    if !inspection.added_parts.is_empty() || !inspection.changed_relationships.is_empty() || !inspection.content_types.is_empty() {
        problems.push("the package's parts".into());
    }
    if inspection.comments != CommentChanges::default() {
        problems.push("comments".into());
    }
    if inspection.new_revisions.iter().any(|revision| !mine.contains(revision.author.as_str())) {
        problems.push("tracked changes in another name".into());
    }
    let mut seen = HashSet::new();
    problems.retain(|problem| seen.insert(problem.clone()));
    if problems.is_empty() {
        return Ok(());
    }
    Err(Unpreservable::Verification(problems.into_iter().take(5).collect::<Vec<_>>().join(", ")))
}

//! Text matching for host jumps and marks. The passage and the document are folded the same way,
//! by one pipeline ([`fold_mapped`]): Unicode compatibility forms (NFKC), lower case, curly quotes
//! as straight ones, dashes as a hyphen, every run of whitespace as one space, inline objects
//! (U+FFFC: comment anchors, pictures) skipped. The document side is the text the host sees:
//! field results inlined, tracked deletions left out ([`visible_text`]). A match may cross line
//! breaks inside a paragraph, never a paragraph boundary.

use std::collections::HashMap;
use std::sync::Arc;

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;
use wordcraft_doc::edit::Fragment;
use wordcraft_doc::para::OBJ;
// Nested tables are read at most `MAX_TABLE_DEPTH` deep, as `Document::plain_text` reads them.
use wordcraft_doc::{Block, Document, MAX_TABLE_DEPTH, Paragraph, Path, Pos, StoryRef};

/// Longest passage a host may ask for, in folded characters; longer ones match nothing.
pub const MAX_NEEDLE_CHARS: usize = 10_000;
/// Longest passage a host may ask for as sent, in bytes of UTF-8; longer ones match nothing and are
/// never folded. Whitespace and inline objects fold to nothing, so the folded cap alone would still
/// walk a huge passage of them in one step.
pub const MAX_RAW_PASSAGE_BYTES: usize = 256 * 1024;
/// Most matches [`TextIndex::find`] returns. Callers that need one particular match use
/// [`TextIndex::find_where`], which is not limited by this.
pub const MAX_MATCHES: usize = 10_000;

/// One character as matching compares it, before whitespace runs are collapsed.
fn fold_simple(c: char, out: &mut Vec<char>) {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' => out.push('\''),
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' => out.push('"'),
        '\u{2010}'..='\u{2015}' | '\u{2212}' => out.push('-'),
        c if c.is_whitespace() => out.push(' '),
        c => out.extend(c.to_lowercase()),
    }
}

/// `src` as matching compares it: each folded character with the source byte range it came from.
/// A character and the combining marks after it are normalised together, so a decomposed accent
/// matches a composed one. A whitespace run is one space whose range covers the run; leading
/// whitespace and inline objects produce nothing. `None` once more than `max_chars` folded
/// characters are produced, so an oversized passage is dropped before it is all folded.
fn fold_mapped(src: &str, max_chars: usize) -> Option<Vec<(char, usize, usize)>> {
    let mut out: Vec<(char, usize, usize)> = Vec::new();
    let mut cluster = String::new();
    let mut folded = Vec::new();
    let mut chars = src.char_indices().peekable();
    while let Some((start, base)) = chars.next() {
        if base == OBJ {
            continue;
        }
        cluster.clear();
        cluster.push(base);
        let mut end = start + base.len_utf8();
        while let Some(&(at, mark)) = chars.peek() {
            if !is_combining_mark(mark) {
                break;
            }
            cluster.push(mark);
            end = at + mark.len_utf8();
            chars.next();
        }
        folded.clear();
        for normal in cluster.chars().nfkc() {
            fold_simple(normal, &mut folded);
        }
        for &c in &folded {
            match out.last_mut() {
                Some(last) if c == ' ' && last.0 == ' ' => last.2 = end,
                None if c == ' ' => {}
                _ => out.push((c, start, end)),
            }
        }
        if out.len() > max_chars {
            return None;
        }
    }
    Some(out)
}

/// The text of `src` as matching compares it, without trailing whitespace (a trailing space in a
/// passage is not part of what to find).
pub fn fold(src: &str) -> String {
    let folded: String = fold_mapped(src, usize::MAX).unwrap_or_default().into_iter().map(|(folded_char, _, _)| folded_char).collect();
    folded.trim_end().to_string()
}

/// The host's passage as matching compares it, or `None` when nothing is left to match, it is
/// longer than [`MAX_RAW_PASSAGE_BYTES`] as sent (checked before any folding) or longer than
/// [`MAX_NEEDLE_CHARS`] once folded. Folding stops as soon as the passage is known to be too long.
pub fn fold_query(passage: &str) -> Option<String> {
    if passage.len() > MAX_RAW_PASSAGE_BYTES {
        return None;
    }
    // One character of slack: a trailing space is trimmed below and does not count.
    let folded: String = fold_mapped(passage, MAX_NEEDLE_CHARS + 1)?.into_iter().map(|(folded_char, _, _)| folded_char).collect();
    let folded = folded.trim_end();
    (!folded.is_empty() && folded.chars().count() <= MAX_NEEDLE_CHARS).then(|| folded.to_string())
}

/// Where one folded character came from: its byte offset in the folded text and the source bytes.
#[derive(Clone, Copy, Debug)]
struct Span {
    folded: usize,
    start: usize,
    end: usize,
}

/// A piece of a paragraph's visible text and where it sits in the paragraph's own text.
#[derive(Clone, Copy, Debug)]
struct Seg {
    /// Byte offset and length in the visible text.
    visible: usize,
    len: usize,
    /// Source byte range in `Paragraph::text`.
    src: usize,
    src_end: usize,
    /// Visible bytes are the source bytes one for one (plain text); otherwise an object's result
    /// text, which maps as a whole to the object's bytes.
    exact: bool,
}

/// What a reader of the paragraph sees: field and equation results inlined, tracked deletions and
/// hidden text left out (as the renderer and the host's extraction treat them), other objects nothing. The host's own extraction of the file reads the same text, so
/// a passage taken from it must be found here.
fn visible_text(paragraph: &Paragraph) -> (String, Vec<Seg>) {
    let skipped: Vec<std::ops::Range<usize>> =
        paragraph.run_ranges().filter(|(_, props)| props.del.is_some() || props.hidden.unwrap_or(false)).map(|(range, _)| range).collect();
    let is_skipped = |at: usize| skipped.get(skipped.partition_point(|range| range.end <= at)).is_some_and(|range| range.start <= at);
    let mut visible = String::new();
    let mut segs: Vec<Seg> = Vec::new();
    let mut next_object = 0;
    for (at, c) in paragraph.text.char_indices() {
        let end = at + c.len_utf8();
        // Objects are counted even inside deleted or hidden text: the k-th U+FFFC is `objects[k]`.
        let object = if c == OBJ {
            next_object += 1;
            paragraph.objects.get(next_object - 1)
        } else {
            None
        };
        if is_skipped(at) {
            continue;
        }
        if c == OBJ {
            let result = object.map(|object| object.plain_text()).unwrap_or_default();
            if !result.is_empty() {
                segs.push(Seg { visible: visible.len(), len: result.len(), src: at, src_end: end, exact: false });
                visible.push_str(result);
            }
            continue;
        }
        match segs.last_mut() {
            Some(last) if last.exact && last.src_end == at => {
                last.len += c.len_utf8();
                last.src_end = end;
            }
            _ => segs.push(Seg { visible: visible.len(), len: c.len_utf8(), src: at, src_end: end, exact: true }),
        }
        visible.push(c);
    }
    (visible, segs)
}

/// What a reader sees of a copied range (`Document::copy_range`), each paragraph by the rule jumps
/// and marks match ([`visible_text`]), so a selection's text the host reads back is found again.
/// Laid out as `Document::plain_text` lays text out: a line per paragraph, a table row's cells
/// joined by tabs.
pub fn visible_fragment_text(fragment: &Fragment) -> String {
    let mut out = String::new();
    for block in &fragment.blocks {
        block_text(block, &mut out, 0);
    }
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

fn block_text(block: &Block, out: &mut String, depth: usize) {
    if depth > MAX_TABLE_DEPTH {
        return;
    }
    match block {
        Block::Para(paragraph) => {
            out.push_str(&visible_text(paragraph).0);
            out.push('\n');
        }
        Block::Table(table) => {
            for row in &table.rows {
                let cells: Vec<String> = row
                    .cells
                    .iter()
                    .map(|cell| {
                        let mut text = String::new();
                        for block in &cell.blocks {
                            block_text(block, &mut text, depth.saturating_add(1));
                        }
                        text.trim_end_matches('\n').replace('\n', " ")
                    })
                    .collect();
                out.push_str(&cells.join("\t"));
                out.push('\n');
            }
        }
    }
}

/// Source byte where the visible byte `at` starts.
fn source_of_start(segs: &[Seg], at: usize) -> Option<usize> {
    let seg = segs.get(segs.partition_point(|seg| seg.visible <= at).checked_sub(1)?)?;
    Some(if seg.exact { seg.src + at.saturating_sub(seg.visible) } else { seg.src })
}

/// Source byte where the visible range ending (exclusive) at `end` stops.
fn source_of_end(segs: &[Seg], end: usize) -> Option<usize> {
    let last_byte = end.checked_sub(1)?;
    let seg = segs.get(segs.partition_point(|seg| seg.visible <= last_byte).checked_sub(1)?)?;
    Some(if seg.exact { seg.src + end.saturating_sub(seg.visible) } else { seg.src_end })
}

/// Folded text, with where each folded character came from.
#[derive(Clone, Debug, Default)]
pub struct Folded {
    pub text: String,
    spans: Vec<Span>,
}

impl Folded {
    /// Plain text, every byte its own source.
    pub fn new(src: &str) -> Folded {
        let mut out = Folded::default();
        for (folded_char, start, end) in fold_mapped(src, usize::MAX).unwrap_or_default() {
            out.push(folded_char, start, end);
        }
        out
    }

    /// A paragraph's visible text; positions are bytes of `Paragraph::text`. A match over a field
    /// result covers the field's object, one over a tracked deletion spans it, and none starts or
    /// ends inside deleted text.
    pub fn from_paragraph(paragraph: &Paragraph) -> Folded {
        let (visible, segs) = visible_text(paragraph);
        let mut out = Folded::default();
        for (folded_char, from, to) in fold_mapped(&visible, usize::MAX).unwrap_or_default() {
            if let (Some(start), Some(end)) = (source_of_start(&segs, from), source_of_end(&segs, to)) {
                out.push(folded_char, start, end);
            }
        }
        out
    }

    fn push(&mut self, folded_char: char, start: usize, end: usize) {
        self.spans.push(Span { folded: self.text.len(), start, end });
        self.text.push(folded_char);
    }

    /// Source range of the folded byte range `from..to` (a match found in `text`); `None` unless
    /// both ends sit on folded characters.
    pub fn source_range(&self, from: usize, to: usize) -> Option<(usize, usize)> {
        let first = self.spans.get(self.spans.partition_point(|span| span.folded < from))?;
        let after_last = self.spans.partition_point(|span| span.folded < to);
        let last = self.spans.get(after_last.checked_sub(1)?)?;
        let ends_on_boundary = self.spans.get(after_last).map_or(to == self.text.len(), |next| next.folded == to);
        (from < to && first.folded == from && ends_on_boundary && first.start <= last.end).then_some((first.start, last.end))
    }
}

/// The document's body paragraphs, folded once per document revision.
#[derive(Default)]
pub struct TextIndex {
    rev: Option<u64>,
    paragraphs: Vec<(Path, Folded)>,
}

impl TextIndex {
    /// Rebuild when the document changed (`rev` is `Session::rev()`).
    pub fn refresh(&mut self, doc: &Document, rev: u64) {
        if self.rev == Some(rev) {
            return;
        }
        self.paragraphs = doc
            .para_paths(StoryRef::Body)
            .into_iter()
            .filter_map(|path| doc.para(StoryRef::Body, &path).map(|paragraph| (path.clone(), Folded::from_paragraph(paragraph))))
            .collect();
        self.rev = Some(rev);
    }

    /// Every match of `passage`, lazily, in document order and not overlapping: (start, end).
    pub fn find_iter(&self, passage: &str) -> Matches<'_> {
        Matches { paragraphs: self.paragraphs.iter(), query: fold_query(passage), current: None }
    }

    /// The first [`MAX_MATCHES`] matches of `passage`, in document order.
    pub fn find(&self, passage: &str) -> Vec<(Pos, Pos)> {
        self.find_iter(passage).take(MAX_MATCHES).collect()
    }

    /// The `nth` (0-based) match that `accept` approves, stopping there: a page- or
    /// paragraph-scoped passage is found however many matches come before it.
    pub fn find_where(&self, passage: &str, mut accept: impl FnMut(&Pos, &Pos) -> bool, nth: usize) -> Option<(Pos, Pos)> {
        self.find_iter(passage).filter(|(from, to)| accept(from, to)).nth(nth)
    }
}

/// The lazy matches of one passage; see [`TextIndex::find_iter`].
pub struct Matches<'a> {
    paragraphs: std::slice::Iter<'a, (Path, Folded)>,
    query: Option<String>,
    /// The paragraph being searched and the folded byte offset to continue from.
    current: Option<(&'a Path, &'a Folded, usize)>,
}

impl Iterator for Matches<'_> {
    type Item = (Pos, Pos);

    fn next(&mut self) -> Option<(Pos, Pos)> {
        let query = self.query.as_deref()?;
        loop {
            let Some((path, folded, resume)) = self.current else {
                let (path, folded) = self.paragraphs.next().map(|(path, folded)| (path, folded))?;
                self.current = Some((path, folded, 0));
                continue;
            };
            let found = folded.text.get(resume..).and_then(|rest| rest.find(query)).map(|at| resume + at);
            let Some(at) = found else {
                self.current = None;
                continue;
            };
            let end = at + query.len();
            self.current = Some((path, folded, end));
            if let Some(found) = body_range(path, folded, at, end) {
                return Some(found);
            }
        }
    }
}

/// The document positions of the folded match `at..end` in a body paragraph.
fn body_range(path: &Path, folded: &Folded, at: usize, end: usize) -> Option<(Pos, Pos)> {
    let (start, finish) = folded.source_range(at, end)?;
    Some((Pos::new(StoryRef::Body, path.clone(), start), Pos::new(StoryRef::Body, path.clone(), finish)))
}

/// Bytes of each folded passage the automaton holds (cut back to a character boundary). The rest of
/// a passage is compared where its prefix is found, so the automaton stays small however long the
/// passages are: at most `MAX_MARKS` × 64 bytes of patterns.
const PREFIX_BYTES: usize = 64;

/// Work between two looks at the caller's budget: places the automaton reported plus passages
/// compared there, a passage counting one more for every [`BYTES_PER_WORK`] bytes compared. Bounds
/// the time between looks inside one long paragraph, and inside one place, too.
const WORK_PER_CHECK: usize = 256;

/// Bytes of passage compared that count as one unit of work: between two looks at most
/// `WORK_PER_CHECK` × 64 bytes (16 KiB) plus one passage (at most 40 KB) are compared.
const BYTES_PER_WORK: usize = 64;

/// One distinct folded passage and the passages (by position in the caller's list) it stands for.
struct Needle {
    text: Arc<str>,
    passages: Vec<usize>,
    /// Where its next match may start, as (paragraph, folded byte): its matches do not overlap, as in
    /// [`Matches`], so the same places are tested. `(usize::MAX, 0)` once all its passages are found.
    resume: (usize, usize),
}

/// One search for many passages at once, each wanting its first match that a caller's test accepts
/// (see [`TextIndex::find_first`]). Every passage gets the match [`TextIndex::find_where`] with
/// `nth` 0 would give it, in one pass over the document instead of one per passage. All the work
/// (folding the passages, then the pass) can be spread over several calls.
#[derive(Default)]
pub(crate) struct FirstMatches {
    /// The passages as given, until folded; by position in the caller's list.
    unfolded: Vec<String>,
    /// How many of `unfolded` have been folded.
    folded: usize,
    /// While folding: each distinct folded passage's index in `needles`.
    needle_of: HashMap<Arc<str>, usize>,
    needles: Vec<Needle>,
    /// Built once every passage is folded, over each needle's first [`PREFIX_BYTES`]; `None` when
    /// no passage can match or it could not be built.
    automaton: Option<aho_corasick::AhoCorasick>,
    built: bool,
    /// For each automaton pattern (a distinct prefix), the needles that start with it.
    needles_of: Vec<Vec<usize>>,
    found: Vec<Option<(Pos, Pos)>>,
    /// Passages that cannot match: blank or too long once folded, or every one when the automaton
    /// could not be built.
    unmatchable: usize,
    /// Passages that may still match and have not.
    open: usize,
    /// The paragraph being searched.
    paragraph: usize,
    /// Inside `paragraph`, when the last call stopped there.
    within: Option<Within>,
}

/// Where a pass stopped inside a paragraph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Within {
    /// The end of the last place handled whole and how many places ending there were, if any.
    done: Option<(usize, usize)>,
    /// How many of the needles sharing the next place's prefix were already compared there: one
    /// place can be more work than a look allows (many long passages sharing a prefix).
    partial: usize,
}

impl FirstMatches {
    /// A search for `passages`; one that folds to nothing or is longer than [`MAX_NEEDLE_CHARS`]
    /// finds nothing, as with [`TextIndex::find_iter`]. Nothing is folded yet.
    pub(crate) fn new(passages: Vec<String>) -> FirstMatches {
        let count = passages.len();
        FirstMatches { unfolded: passages, found: vec![None; count], open: count, ..FirstMatches::default() }
    }

    /// Start again from the document's first paragraph (the document or what `accept` tests
    /// changed), keeping what was folded and built.
    pub(crate) fn restart(&mut self) {
        for needle in &mut self.needles {
            needle.resume = (0, 0);
        }
        self.found.iter_mut().for_each(|found| *found = None);
        self.open = self.found.len().saturating_sub(self.unmatchable);
        self.paragraph = 0;
        self.within = None;
    }

    /// Every passage either has its match or the whole document was searched.
    pub(crate) fn done(&self) -> bool {
        self.open == 0
    }

    /// Each passage's match, by its position in the list given to [`FirstMatches::new`].
    pub(crate) fn found(&self) -> &[Option<(Pos, Pos)>] {
        &self.found
    }

    /// Fold the passages not yet folded, asking `more` after each, then build the automaton;
    /// `true` once it is built.
    fn prepare(&mut self, more: &mut impl FnMut() -> bool) -> bool {
        while let Some(raw) = self.unfolded.get_mut(self.folded) {
            let passage = self.folded;
            let raw = std::mem::take(raw);
            self.folded += 1;
            match fold_query(&raw) {
                None => {
                    self.unmatchable += 1;
                    self.open = self.open.saturating_sub(1);
                }
                Some(query) => {
                    let query: Arc<str> = query.into();
                    let next = self.needles.len();
                    // Equal passages share one needle: each place is compared once for all of them.
                    let needle = *self.needle_of.entry(query.clone()).or_insert(next);
                    if needle == next {
                        self.needles.push(Needle { text: query, passages: Vec::new(), resume: (0, 0) });
                    }
                    if let Some(needle) = self.needles.get_mut(needle) {
                        needle.passages.push(passage);
                    }
                }
            }
            if !more() {
                return false;
            }
        }
        if !self.built {
            self.build();
        }
        true
    }

    fn build(&mut self) {
        self.built = true;
        self.unfolded = Vec::new();
        self.needle_of = HashMap::new();
        let mut patterns: Vec<&str> = Vec::new();
        let mut pattern_of: HashMap<&str, usize> = HashMap::new();
        let mut needles_of: Vec<Vec<usize>> = Vec::new();
        for (index, needle) in self.needles.iter().enumerate() {
            let mut cut = needle.text.len().min(PREFIX_BYTES);
            while !needle.text.is_char_boundary(cut) {
                cut -= 1;
            }
            let prefix = needle.text.get(..cut).unwrap_or_default();
            let pattern = *pattern_of.entry(prefix).or_insert_with(|| {
                patterns.push(prefix);
                needles_of.push(Vec::new());
                needles_of.len() - 1
            });
            if let Some(list) = needles_of.get_mut(pattern) {
                list.push(index);
            }
        }
        // Standard semantics with overlapping search report every place each prefix occurs, so a
        // passage inside another (or a prefix of it) is not hidden by the longer one's match.
        let automaton = if patterns.is_empty() {
            None
        } else {
            match aho_corasick::AhoCorasick::builder().match_kind(aho_corasick::MatchKind::Standard).build(&patterns) {
                Ok(automaton) => Some(automaton),
                Err(error) => {
                    log::warn!("host text search could not be built: {error}");
                    None
                }
            }
        };
        if automaton.is_none() {
            self.unmatchable = self.found.len();
            self.open = 0;
        }
        self.automaton = automaton;
        self.needles_of = needles_of;
    }
}

impl TextIndex {
    /// Continue `search` in document order until it is done or `more` answers `false`. `more` is
    /// asked after each passage folded, after each paragraph and every [`WORK_PER_CHECK`] units of
    /// work inside one. `accept(passage, start, end)` tests a match for one passage; a passage keeps
    /// the first match accepted.
    pub(crate) fn find_first(&self, search: &mut FirstMatches, mut accept: impl FnMut(usize, &Pos, &Pos) -> bool, mut more: impl FnMut() -> bool) {
        if search.done() || !search.prepare(&mut more) {
            return;
        }
        let FirstMatches { automaton, needles, needles_of, found, open, paragraph, within, .. } = search;
        let Some(automaton) = automaton.as_ref() else { return };
        let mut work = 0usize;
        while *open > 0 {
            let number = *paragraph;
            let Some((path, folded)) = self.paragraphs.get(number) else {
                *open = 0;
                return;
            };
            let text = folded.text.as_str();
            // Resuming inside the paragraph: every place ending at or after the last one handled
            // starts at most a prefix's length before it, and the automaton reports the places that
            // end at one byte in the same order from there.
            let resumed = within.take().unwrap_or_default();
            let from = resumed.done.map_or(0, |(end, _)| end.saturating_sub(PREFIX_BYTES));
            let places =
                if from <= text.len() { automaton.try_find_overlapping_iter(aho_corasick::Input::new(text).range(from..)).ok() } else { None };
            let mut handled = resumed.done;
            // Needles already compared at the first place not handled whole.
            let mut skip_needles = resumed.partial;
            let mut skipped_at_end = 0usize;
            for place in places.into_iter().flatten() {
                if let Some((end, count)) = resumed.done {
                    if place.end() < end {
                        continue;
                    }
                    if place.end() == end && skipped_at_end < count {
                        skipped_at_end += 1;
                        continue;
                    }
                }
                let sharing = needles_of.get(place.pattern().as_usize()).map(Vec::as_slice).unwrap_or_default();
                // The automaton proved the prefix, so only the rest of each passage is compared.
                let rest = text.get(place.end()..).unwrap_or_default();
                let proven = place.end().saturating_sub(place.start());
                work = work.saturating_add(1);
                for (compared, &index) in sharing.iter().enumerate().skip(std::mem::take(&mut skip_needles)) {
                    if work >= WORK_PER_CHECK {
                        work = 0;
                        if !more() {
                            *within = Some(Within { done: handled, partial: compared });
                            return;
                        }
                    }
                    let Some(needle) = needles.get_mut(index) else { continue };
                    if needle.resume > (number, place.start()) {
                        continue;
                    }
                    let tail = needle.text.get(proven..).unwrap_or_default();
                    // `starts_with` compares at most the tail, and nothing when the text is shorter.
                    let cost = if rest.len() >= tail.len() { tail.len() } else { 0 };
                    work = work.saturating_add(1 + cost / BYTES_PER_WORK);
                    if !rest.starts_with(tail) {
                        continue;
                    }
                    let end = place.end().saturating_add(tail.len());
                    needle.resume = (number, end);
                    let Some((start, finish)) = body_range(path, folded, place.start(), end) else { continue };
                    let mut all_found = true;
                    for passage in &needle.passages {
                        let Some(slot) = found.get_mut(*passage) else { continue };
                        if slot.is_none() && accept(*passage, &start, &finish) {
                            *slot = Some((start.clone(), finish.clone()));
                            *open = open.saturating_sub(1);
                        }
                        all_found &= slot.is_some();
                    }
                    if all_found {
                        needle.resume = (usize::MAX, 0);
                    }
                    if *open == 0 {
                        return;
                    }
                }
                handled = Some(match handled {
                    Some((end, count)) if end == place.end() => (end, count.saturating_add(1)),
                    _ => (place.end(), 1),
                });
                if work >= WORK_PER_CHECK {
                    work = 0;
                    if !more() {
                        *within = Some(Within { done: handled, partial: 0 });
                        return;
                    }
                }
            }
            *paragraph = number.saturating_add(1);
            if !more() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use wordcraft_doc::props::CharProps;
    use wordcraft_doc::{Block, Cell, Document, InlineObject, Paragraph, Run, Table, para_block};

    use super::*;

    fn doc(paragraphs: &[&str]) -> Document {
        let mut d = Document::new();
        d.body = paragraphs.iter().map(|t| para_block(Paragraph::with_text(t, CharProps::default()))).collect();
        d.ensure_nonempty();
        d
    }

    fn source_text(d: &Document, from: &Pos, to: &Pos) -> String {
        d.para_at(from).and_then(|p| p.text.get(from.off..to.off)).unwrap_or("").to_string()
    }

    /// The source text of every match of `passage`.
    fn found(paragraphs: &[&str], passage: &str) -> Vec<String> {
        let d = doc(paragraphs);
        let mut index = TextIndex::default();
        index.refresh(&d, 1);
        index.find(passage).into_iter().map(|(from, to)| source_text(&d, &from, &to)).collect()
    }

    #[test]
    fn case_and_curly_quotes_fold() {
        assert_eq!(found(&["The \u{201C}Supplier\u{201D} shall deliver."], "the \"supplier\" SHALL"), vec!["The \u{201C}Supplier\u{201D} shall"]);
        assert_eq!(found(&["the Buyer\u{2019}s notice"], "buyer's notice"), vec!["Buyer\u{2019}s notice"]);
    }

    #[test]
    fn dashes_and_minus_fold_to_a_hyphen() {
        let dashes = ["\u{2010}", "\u{2011}", "\u{2012}", "\u{2013}", "\u{2014}", "\u{2015}", "\u{2212}", "-"];
        for dash in dashes {
            let text = format!("term {dash} 12 months");
            assert_eq!(found(&[&text], "term - 12 months"), vec![text.clone()], "dash {dash:?}");
            assert_eq!(found(&["term - 12 months"], &text), vec!["term - 12 months"], "needle dash {dash:?}");
        }
    }

    #[test]
    fn whitespace_runs_line_breaks_and_tabs_are_one_space() {
        assert_eq!(
            found(&["net 30\u{00A0}days", "net  30\ndays", "net\t30 days"], "net 30 days"),
            vec!["net 30\u{00A0}days", "net  30\ndays", "net\t30 days"]
        );
    }

    #[test]
    fn compatibility_forms_fold() {
        assert_eq!(found(&["the \u{FB01}nal price"], "final price"), vec!["\u{FB01}nal price"]);
        assert_eq!(found(&["\u{FF21}\u{FF22}\u{FF23} Ltd"], "abc ltd"), vec!["\u{FF21}\u{FF22}\u{FF23} Ltd"]);
    }

    #[test]
    fn decomposed_and_composed_accents_match() {
        assert_eq!(found(&["Cafe\u{301} Utama"], "caf\u{e9} utama"), vec!["Cafe\u{301} Utama"]);
        assert_eq!(found(&["Caf\u{e9} Utama"], "cafe\u{301} utama"), vec!["Caf\u{e9} Utama"]);
    }

    #[test]
    fn a_match_never_crosses_paragraphs() {
        assert!(found(&["end of one", "start of two"], "one start").is_empty());
    }

    #[test]
    fn inline_objects_are_skipped() {
        let f = Folded::new("Sup\u{FFFC}plier shall");
        assert_eq!(f.text, "supplier shall");
        assert_eq!(f.source_range(0, "supplier".len()), Some((0, "Sup\u{FFFC}plier".len())));
        // `Paragraph::with_text` strips objects itself, so only the needle side is checked on a document.
        assert_eq!(found(&["supplier"], "sup\u{FFFC}plier"), vec!["supplier"]);
    }

    #[test]
    fn empty_blank_and_oversized_passages_match_nothing() {
        assert!(found(&["anything"], "").is_empty());
        assert!(found(&["anything"], " \n\t ").is_empty());
        assert!(found(&["anything"], "\u{FFFC}").is_empty());
        let longest = "a".repeat(MAX_NEEDLE_CHARS);
        assert_eq!(found(&[&longest], &longest).len(), 1);
        let too_long = "a".repeat(MAX_NEEDLE_CHARS + 1);
        assert!(found(&[&too_long], &too_long).is_empty());
    }

    #[test]
    fn every_occurrence_in_document_order() {
        assert_eq!(found(&["a cat", "the cat and the CAT"], "cat"), vec!["cat", "cat", "CAT"]);
    }

    #[test]
    fn matches_do_not_overlap() {
        assert_eq!(found(&["aaaa"], "aa").len(), 2);
    }

    #[test]
    fn a_page_scoped_find_reaches_a_match_after_more_than_the_all_matches_cap() {
        // Far more earlier matches than `MAX_MATCHES`, then one distinct paragraph.
        let early = "x ".repeat(MAX_MATCHES + 500);
        let d = doc(&[&early, "x target"]);
        let mut index = TextIndex::default();
        index.refresh(&d, 1);
        assert_eq!(index.find("x").len(), MAX_MATCHES, "the all-matches API is capped");
        let last_paragraph = Path::top(1);
        let hit = index.find_where("x", |from, _| from.path == last_paragraph, 0);
        let described = hit.map(|(from, to)| (source_text(&d, &from, &to), from.path));
        assert_eq!(described, Some(("x".to_string(), last_paragraph)));
    }

    #[test]
    fn find_where_counts_only_accepted_matches() {
        let d = doc(&["cat one", "cat two", "cat three"]);
        let mut index = TextIndex::default();
        index.refresh(&d, 1);
        let second = index.find_where("cat", |_, _| true, 1).map(|(from, _)| from.path);
        assert_eq!(second, Some(Path::top(1)));
        let skipping_first = index.find_where("cat", |from, _| from.path != Path::top(0), 1).map(|(from, _)| from.path);
        assert_eq!(skipping_first, Some(Path::top(2)));
        assert!(index.find_where("cat", |_, _| true, 3).is_none());
        assert!(index.find_where("dog", |_, _| true, 0).is_none());
    }

    #[test]
    fn refresh_follows_the_revision() {
        let mut index = TextIndex::default();
        index.refresh(&doc(&["old text"]), 1);
        index.refresh(&doc(&["new text"]), 1);
        assert_eq!(index.find("old").len(), 1, "the same revision is not rebuilt");
        index.refresh(&doc(&["new text"]), 2);
        assert!(index.find("old").is_empty());
        assert_eq!(index.find("new").len(), 1);
    }

    #[test]
    fn matches_inside_table_cells_are_found() {
        let mut table = Table::new(2, 2, 400.0);
        if let Some(cell) = table.rows.get_mut(1).and_then(|row| row.cells.get_mut(0)) {
            *cell = Cell::with_text("Only in the cell");
        }
        let mut d = doc(&["before"]);
        d.body.push(std::sync::Arc::new(Block::Table(table)));
        let mut index = TextIndex::default();
        index.refresh(&d, 1);
        let hits: Vec<(Vec<u32>, String)> =
            index.find("only IN the cell").into_iter().map(|(from, to)| (from.path.0.clone(), source_text(&d, &from, &to))).collect();
        // Table block 1, row 1, cell 0, paragraph 0.
        assert_eq!(hits, vec![(vec![1, 1, 0, 0], "Only in the cell".to_string())]);
        assert!(index.find("Only in the cell and more").is_empty());
    }

    fn field(result: &str) -> InlineObject {
        InlineObject::Field { instr: "REF clause_five".into(), result: result.into(), locked: false }
    }

    fn found_in_paragraph(paragraph: Paragraph, passage: &str) -> (Document, Vec<(usize, usize)>) {
        let mut d = Document::new();
        d.body = vec![para_block(paragraph)];
        d.ensure_nonempty();
        let mut index = TextIndex::default();
        index.refresh(&d, 1);
        let ranges = index.find(passage).into_iter().map(|(from, to)| (from.off, to.off)).collect();
        (d, ranges)
    }

    /// A paragraph of `(text, tracked deletion)` parts.
    fn paragraph_with_deletions(parts: &[(&str, bool)]) -> Paragraph {
        let mut paragraph = Paragraph::new();
        for (text, deleted) in parts {
            paragraph.text.push_str(text);
            let props = CharProps { del: deleted.then_some(0), ..CharProps::default() };
            paragraph.runs.push(Run { len: text.len(), props });
        }
        paragraph
    }

    #[test]
    fn a_field_result_is_matched_as_text_and_selects_the_field() {
        let mut paragraph = Paragraph::with_text("Clause  applies", CharProps::default());
        assert!(paragraph.insert_object(7, field("5"), &CharProps::default()).is_ok());
        let object_len = '\u{FFFC}'.len_utf8();
        let whole = paragraph.text.len();
        let (_, ranges) = found_in_paragraph(paragraph.clone(), "Clause 5 applies");
        assert_eq!(ranges, vec![(0, whole)]);
        let (_, ranges) = found_in_paragraph(paragraph.clone(), "5");
        assert_eq!(ranges, vec![(7, 7 + object_len)], "a match covering only the result selects the field object");
        let (_, ranges) = found_in_paragraph(paragraph, "clause  5");
        assert_eq!(ranges, vec![(0, 7 + object_len)]);
    }

    #[test]
    fn objects_without_text_are_skipped_at_document_level() {
        let mut paragraph = Paragraph::with_text("Supplier shall", CharProps::default());
        assert!(paragraph.insert_object(3, InlineObject::CommentStart { id: 1 }, &CharProps::default()).is_ok());
        let (d, ranges) = found_in_paragraph(paragraph, "supplier shall");
        let whole = d.para_at(&Pos::body(0, 0)).map(|p| p.text.len());
        assert_eq!(ranges, vec![(0, whole.unwrap_or(0))]);
    }

    #[test]
    fn a_passage_across_a_tracked_deletion_matches_the_text_after_it() {
        let paragraph = paragraph_with_deletions(&[("The fee is ", false), ("ten ", true), ("five dollars.", false)]);
        let (d, ranges) = found_in_paragraph(paragraph, "fee is five dollars");
        let text = d.para_at(&Pos::body(0, 0)).map(|p| p.text.clone()).unwrap_or_default();
        assert_eq!(
            ranges.iter().filter_map(|(a, b)| text.get(*a..*b)).collect::<Vec<_>>(),
            vec!["fee is ten five dollars"],
            "the range spans the deletion"
        );
    }

    #[test]
    fn deleted_text_is_not_matched_and_no_match_ends_inside_it() {
        let paragraph = paragraph_with_deletions(&[("a ", false), ("zzz ", true), ("b", false)]);
        assert!(found_in_paragraph(paragraph.clone(), "zzz").1.is_empty());
        assert!(found_in_paragraph(paragraph.clone(), "a zzz b").1.is_empty());
        assert!(found_in_paragraph(paragraph.clone(), "zz").1.is_empty());
        assert_eq!(found_in_paragraph(paragraph.clone(), "b").1, vec![(6, 7)], "starts after the deletion");
        assert_eq!(found_in_paragraph(paragraph.clone(), "a").1, vec![(0, 1)], "ends before the deletion");
        assert_eq!(found_in_paragraph(paragraph, "a b").1, vec![(0, 7)]);
    }

    #[test]
    fn a_deleted_field_contributes_nothing() {
        let mut paragraph = Paragraph::with_text("Clause  applies", CharProps::default());
        assert!(paragraph.insert_object(7, field("5"), &CharProps::default()).is_ok());
        // Delete exactly the field object's bytes.
        let object_len = '\u{FFFC}'.len_utf8();
        paragraph.runs = vec![
            Run { len: 7, props: CharProps::default() },
            Run { len: object_len, props: CharProps { del: Some(0), ..CharProps::default() } },
            Run { len: paragraph.text.len() - 7 - object_len, props: CharProps::default() },
        ];
        assert!(found_in_paragraph(paragraph.clone(), "Clause 5").1.is_empty());
        assert_eq!(found_in_paragraph(paragraph, "Clause applies").1.len(), 1);
    }

    #[test]
    fn a_fragment_reads_as_the_visible_text_with_its_lines_and_cells() {
        let mut with_field = paragraph_with_deletions(&[("Fee ", false), ("ten ", true), ("is  due", false)]);
        assert!(with_field.insert_object(4, field("5"), &CharProps::default()).is_ok());
        let mut table = Table::new(1, 2, 200.0);
        table.rows[0].cells[0] = Cell::with_text("Left");
        table.rows[0].cells[1].blocks = vec![para_block(paragraph_with_hidden(&[("Right", false), (" secret", true)]))];
        let fragment =
            Fragment { blocks: vec![Block::Para(with_field), Block::Table(table), Block::Para(Paragraph::with_text("End", CharProps::default()))] };
        assert_eq!(visible_fragment_text(&fragment), "Fee 5is  due\nLeft\tRight\nEnd");
        assert_eq!(visible_fragment_text(&Fragment::default()), "");
    }

    /// A paragraph of `(text, hidden)` parts.
    fn paragraph_with_hidden(parts: &[(&str, bool)]) -> Paragraph {
        let mut paragraph = Paragraph::new();
        for (text, hidden) in parts {
            paragraph.text.push_str(text);
            let props = CharProps { hidden: hidden.then_some(true), ..CharProps::default() };
            paragraph.runs.push(Run { len: text.len(), props });
        }
        paragraph
    }

    fn sliced(paragraph: &Paragraph, ranges: &[(usize, usize)]) -> Vec<String> {
        ranges.iter().filter_map(|(from, to)| paragraph.text.get(*from..*to)).map(str::to_string).collect()
    }

    #[test]
    fn hidden_text_is_not_matched_and_the_visible_runs_join() {
        let paragraph = paragraph_with_hidden(&[("Alpha ", false), ("HIDDEN ", true), ("Beta", false)]);
        assert!(found_in_paragraph(paragraph.clone(), "hidden").1.is_empty());
        assert!(found_in_paragraph(paragraph.clone(), "alpha hidden beta").1.is_empty());
        let (_, ranges) = found_in_paragraph(paragraph.clone(), "alpha beta");
        assert_eq!(ranges, vec![(0, paragraph.text.len())], "covers both visible runs and the hidden one between them");
        assert_eq!(sliced(&paragraph, &ranges), vec!["Alpha HIDDEN Beta"]);
        let (_, ranges) = found_in_paragraph(paragraph.clone(), "beta");
        assert_eq!(sliced(&paragraph, &ranges), vec!["Beta"]);
        let (_, ranges) = found_in_paragraph(paragraph.clone(), "alpha");
        assert_eq!(sliced(&paragraph, &ranges), vec!["Alpha"], "ends before the hidden run");
    }

    #[test]
    fn an_object_inside_hidden_text_keeps_the_object_counter_aligned() {
        // "A " + hidden [comment anchor] + " B " + field "5": the field is objects[1].
        let mut paragraph = paragraph_with_hidden(&[("A ", false), ("\u{FFFC}", true), (" B ", false), ("\u{FFFC}", false)]);
        paragraph.objects = vec![InlineObject::Field { instr: "REF hidden_one".into(), result: "WRONG".into(), locked: false }, field("5")];
        let (_, ranges) = found_in_paragraph(paragraph.clone(), "a b 5");
        assert_eq!(ranges, vec![(0, paragraph.text.len())]);
        assert!(found_in_paragraph(paragraph.clone(), "wrong").1.is_empty(), "the hidden field's result is not shown");
        let (_, ranges) = found_in_paragraph(paragraph.clone(), "5");
        assert_eq!(
            ranges,
            vec![(paragraph.text.len() - '\u{FFFC}'.len_utf8(), paragraph.text.len())],
            "the visible field resolves to its own object"
        );
    }

    #[test]
    fn a_run_explicitly_not_hidden_stays_visible() {
        // `hidden: Some(false)` is what a file's `<w:vanish w:val="0"/>` reads as: shown text.
        let mut paragraph = Paragraph::new();
        for (text, hidden) in [("Alpha ", None), ("Shown ", Some(false)), ("Beta", Some(false))] {
            paragraph.text.push_str(text);
            paragraph.runs.push(Run { len: text.len(), props: CharProps { hidden, ..CharProps::default() } });
        }
        let (_, ranges) = found_in_paragraph(paragraph.clone(), "shown");
        assert_eq!(sliced(&paragraph, &ranges), vec!["Shown"]);
        let (_, ranges) = found_in_paragraph(paragraph.clone(), "alpha shown beta");
        assert_eq!(ranges, vec![(0, paragraph.text.len())]);
    }

    #[test]
    fn a_field_result_against_text_on_both_sides_maps_at_the_object_edges() {
        let mut paragraph = Paragraph::with_text("Paynow", CharProps::default());
        assert!(paragraph.insert_object(3, field("Rp 5"), &CharProps::default()).is_ok());
        let object_len = '\u{FFFC}'.len_utf8();
        let object_end = 3 + object_len;
        assert_eq!(Folded::from_paragraph(&paragraph).text, "payrp 5now");
        let (_, ranges) = found_in_paragraph(paragraph.clone(), "payrp 5");
        assert_eq!(ranges, vec![(0, object_end)], "starts in the text, ends at the object's end");
        let (_, ranges) = found_in_paragraph(paragraph.clone(), "5now");
        assert_eq!(ranges, vec![(3, paragraph.text.len())], "starts at the object, ends in the text");
        let (_, ranges) = found_in_paragraph(paragraph.clone(), "p 5");
        assert_eq!(ranges, vec![(3, object_end)], "part of a result selects the whole object");
        let (_, ranges) = found_in_paragraph(paragraph, "rp");
        assert_eq!(ranges, vec![(3, object_end)]);
    }

    #[test]
    fn an_object_without_a_result_does_not_merge_the_text_around_it() {
        let mut paragraph = Paragraph::with_text("abcdef", CharProps::default());
        assert!(paragraph.insert_object(3, InlineObject::BookmarkStart { name: "mark".into() }, &CharProps::default()).is_ok());
        let object_len = '\u{FFFC}'.len_utf8();
        let (visible, segs) = visible_text(&paragraph);
        assert_eq!(visible, "abcdef");
        assert_eq!(segs.len(), 2, "the object separates two source pieces");
        let (_, ranges) = found_in_paragraph(paragraph.clone(), "cd");
        assert_eq!(ranges, vec![(2, 4 + object_len)], "a match across the object spans it");
        let (_, ranges) = found_in_paragraph(paragraph.clone(), "def");
        assert_eq!(ranges, vec![(3 + object_len, 6 + object_len)], "bytes after the object are shifted past it");
        let (_, ranges) = found_in_paragraph(paragraph, "abc");
        assert_eq!(ranges, vec![(0, 3)]);
    }

    #[test]
    fn multi_byte_neighbours_keep_offsets_on_character_boundaries() {
        let text = "\u{5951}\u{7D04} \u{1F600} Supplier \u{1F600}\u{5951}";
        let (d, ranges) = found_in_paragraph(Paragraph::with_text(text, CharProps::default()), "supplier");
        let source = d.para_at(&Pos::body(0, 0)).map(|p| p.text.clone()).unwrap_or_default();
        assert_eq!(ranges.iter().filter_map(|(a, b)| source.get(*a..*b)).collect::<Vec<_>>(), vec!["Supplier"]);
        let (_, ranges) = found_in_paragraph(Paragraph::with_text(text, CharProps::default()), "\u{1F600} supplier \u{1F600}");
        assert_eq!(ranges.iter().filter_map(|(a, b)| source.get(*a..*b)).collect::<Vec<_>>(), vec!["\u{1F600} Supplier \u{1F600}"]);
        assert_eq!(found(&[text], "\u{7D04}"), vec!["\u{7D04}"]);
    }

    #[test]
    fn source_range_refuses_positions_that_are_not_characters() {
        let folded = Folded::new("\u{FB01}x");
        assert_eq!(folded.text, "fix");
        assert_eq!(folded.source_range(0, 3), Some((0, "\u{FB01}x".len())));
        assert_eq!(folded.source_range(0, 4), None, "past the end");
        assert_eq!(folded.source_range(9, 10), None, "out of range");
        assert_eq!(folded.source_range(2, 2), None, "empty");
        assert_eq!(folded.source_range(3, 2), None, "reversed");
        let accented = Folded::new("\u{e9}");
        assert_eq!(accented.text, "\u{e9}");
        assert_eq!(accented.source_range(1, accented.text.len()), None, "inside a two-byte character");
        assert_eq!(accented.source_range(0, 1), None, "ends inside a two-byte character");
    }

    #[test]
    fn a_match_inside_a_folded_cluster_selects_the_whole_source_character() {
        assert_eq!(found(&["a \u{FB01}x"], "i"), vec!["\u{FB01}"]);
        assert_eq!(found(&["a \u{FB01}x"], "ix"), vec!["\u{FB01}x"]);
    }

    #[test]
    fn an_oversized_passage_is_dropped_before_it_is_all_folded() {
        assert!(fold_mapped(&"a".repeat(100), 10).is_none());
        assert_eq!(fold_mapped(&"a".repeat(10), 10).map(|folded| folded.len()), Some(10));
        assert!(fold_query(&"x ".repeat(MAX_NEEDLE_CHARS)).is_none());
        assert_eq!(fold_query(&format!("{} ", "a".repeat(MAX_NEEDLE_CHARS))).map(|folded| folded.len()), Some(MAX_NEEDLE_CHARS));
    }

    #[test]
    fn a_passage_longer_than_the_raw_cap_is_refused_before_it_is_folded() {
        // Whitespace folds to nothing, so these fold to a short, valid passage: only the raw cap
        // refuses the longer one, and it does so without folding it.
        let at_cap = format!("{}Clause 2.", " ".repeat(MAX_RAW_PASSAGE_BYTES - "Clause 2.".len()));
        assert_eq!(at_cap.len(), MAX_RAW_PASSAGE_BYTES);
        assert_eq!(fold_query(&at_cap).as_deref(), Some("clause 2."));
        let over = format!(" {at_cap}");
        assert_eq!(fold_query(&over), None);
        let d = doc(&["Clause 1.", "Clause 2."]);
        let mut index = TextIndex::default();
        index.refresh(&d, 1);
        assert!(index.find_where(&at_cap, |_, _| true, 0).is_some());
        assert!(index.find_where(&over, |_, _| true, 0).is_none());
        let (one_pass, single) = both_ways(&d, &[&at_cap, &over], |_, _| true, false);
        assert_eq!(one_pass, single);
        assert_eq!((one_pass[0].is_some(), one_pass[1].is_some()), (true, false));
    }

    #[test]
    fn fold_is_the_text_of_the_mapped_form() {
        let sample = "  \u{201C}Quoted\u{201D}\u{00A0}\u{00A0}TEXT \u{2014}   \u{FB01}ne\u{FFFC}  \u{2018}x\u{2019}  ";
        let mapped: String = fold_mapped(sample, usize::MAX).unwrap_or_default().into_iter().map(|(folded_char, _, _)| folded_char).collect();
        assert_eq!(Folded::new(sample).text, mapped);
        assert_eq!(fold(sample), mapped.trim_end());
        assert_eq!(fold(sample), "\"quoted\" text - fine 'x'");
    }

    /// Each passage's first accepted match, by the one-pass search and by `find_where` alone.
    fn both_ways(
        d: &Document,
        passages: &[&str],
        accept: impl Fn(usize, &Pos) -> bool,
        stop_at_every_look: bool,
    ) -> (Vec<Option<(Pos, Pos)>>, Vec<Option<(Pos, Pos)>>) {
        let mut index = TextIndex::default();
        index.refresh(d, 1);
        let mut search = FirstMatches::new(passages.iter().map(|passage| passage.to_string()).collect());
        let mut calls = 0;
        while !search.done() {
            calls += 1;
            assert!(calls <= 1_000_000, "the search never finished");
            index.find_first(&mut search, |k, start, _| accept(k, start), || !stop_at_every_look);
        }
        let single = passages.iter().enumerate().map(|(k, passage)| index.find_where(passage, |start, _| accept(k, start), 0)).collect();
        (search.found().to_vec(), single)
    }

    #[test]
    fn one_pass_finds_each_passage_inside_or_after_another() {
        let d = doc(&["The Supplier shall deliver the goods.", "the supplier shall pay; the supplier shall deliver"]);
        // Prefixes, substrings, suffixes and a repeat of another passage: each needs its own first match.
        let passages = [
            "the supplier shall deliver",
            "the supplier",
            "supplier",
            "shall",
            "the supplier shall deliver",
            "deliver",
            "goods.",
            "pay",
            "",
            "absent",
        ];
        let (one_pass, single) = both_ways(&d, &passages, |_, _| true, false);
        assert_eq!(one_pass, single);
        let text = |found: &Option<(Pos, Pos)>| found.as_ref().map(|(from, to)| (from.path.clone(), source_text(&d, from, to)));
        assert_eq!(text(&one_pass[1]), Some((Pos::body(0, 0).path, "The Supplier".to_string())));
        assert_eq!(text(&one_pass[2]), Some((Pos::body(0, 0).path, "Supplier".to_string())), "hidden by the longer passage's match");
        assert_eq!(text(&one_pass[7]), Some((Pos::body(1, 0).path, "pay".to_string())));
        assert_eq!((one_pass[8].clone(), one_pass[9].clone()), (None, None));
    }

    #[test]
    fn passages_that_share_the_automatons_prefix_are_told_apart() {
        // Exactly the automaton's prefix length, then endings that differ only after it.
        let shared = "a shared opening long enough to fill the prefix of the automaton";
        assert_eq!(shared.len(), PREFIX_BYTES);
        let first = format!("{shared} then the first ending");
        let second = format!("{shared} then the second ending");
        let d = doc(&[&first, "unrelated", &second, &format!("x {shared} y")]);
        let passages = [second.clone(), first.clone(), shared.to_string(), format!("{shared} then a third ending"), format!("{shared} then")];
        let refs: Vec<&str> = passages.iter().map(String::as_str).collect();
        for stop in [false, true] {
            let (one_pass, single) = both_ways(&d, &refs, |_, _| true, stop);
            assert_eq!(one_pass, single, "stopping at every look: {stop}");
            let paragraph = |found: &Option<(Pos, Pos)>| found.as_ref().map(|(from, _)| from.path.clone());
            assert_eq!(paragraph(&one_pass[0]), Some(Pos::body(2, 0).path), "the second ending is found past the first");
            assert_eq!(paragraph(&one_pass[1]), Some(Pos::body(0, 0).path));
            assert_eq!(paragraph(&one_pass[2]), Some(Pos::body(0, 0).path), "the prefix alone is a passage too");
            assert_eq!(one_pass[3], None);
            // A page-like filter that skips the first two places of the prefix-only passage.
            let later = |k: usize, start: &Pos| k != 2 || start.path.0.first().is_some_and(|&n| n >= 3);
            let (one_pass, single) = both_ways(&d, &refs, later, stop);
            assert_eq!(one_pass, single);
            assert_eq!(paragraph(&one_pass[2]), Some(Pos::body(3, 0).path));
        }
        // Multi-byte characters around the cut: the prefix ends on a character boundary.
        let accented = format!("{}\u{e9}\u{e9} and more", "e".repeat(PREFIX_BYTES - 1));
        let d = doc(&[&accented]);
        let passages = [accented.as_str(), &accented[..PREFIX_BYTES - 1]];
        let (one_pass, single) = both_ways(&d, &passages, |_, _| true, true);
        assert_eq!(one_pass, single);
        assert!(one_pass.iter().all(Option::is_some));
    }

    #[test]
    fn one_pass_stops_and_resumes_inside_a_long_paragraph() {
        // One paragraph with thousands of overlapping places, so the budget is looked at inside it.
        let long = "ab ".repeat(3_000) + "abc ab abd";
        let d = doc(&["start", &long, "ab abc"]);
        let passages = [
            "ab",
            "b a",
            "ab ab",
            "ab ab ab",
            "abc",
            "abd",
            "ab abc",
            "b ab ab",
            "ab ab ab ab ab ab ab ab ab ab ab ab ab ab ab ab ab ab ab ab ab ab ab x",
        ];
        // Each passage wants a place further along, so the search must go deep into the paragraph.
        let further = |k: usize, start: &Pos| start.path != Pos::body(1, 0).path || start.off >= k * 700;
        let mut index = TextIndex::default();
        index.refresh(&d, 1);
        let mut search = FirstMatches::new(passages.iter().map(|passage| passage.to_string()).collect());
        let mut stops_inside = 0;
        while !search.done() {
            index.find_first(&mut search, |k, start, _| further(k, start), || false);
            stops_inside += usize::from(search.within.is_some());
        }
        assert!(stops_inside > 10, "the search stopped inside the long paragraph only {stops_inside} times");
        let single: Vec<Option<(Pos, Pos)>> =
            passages.iter().enumerate().map(|(k, passage)| index.find_where(passage, |start, _| further(k, start), 0)).collect();
        assert_eq!(search.found(), single.as_slice());
        assert!(search.found().iter().filter(|found| found.is_some()).count() >= 7);
    }

    #[test]
    fn the_search_moves_on_when_one_place_costs_more_than_a_look() {
        // Hundreds of passages share one prefix and never match in full, so every place of that prefix
        // is more work than one look allows; a short passage ends at the same byte. Each call must
        // still get past the places it handled before, or the search would never end.
        let shared = "a shared opening long enough to fill the prefix of the automaton";
        let d = doc(&[&format!("{shared} {shared} {shared} automaton")]);
        let mut passages: Vec<String> = (0..300).map(|k| format!("{shared} but ending {k}")).collect();
        passages.push("automaton".to_string());
        let mut index = TextIndex::default();
        index.refresh(&d, 1);
        let mut search = FirstMatches::new(passages.clone());
        let mut calls = 0;
        while !search.done() {
            calls += 1;
            assert!(calls < 10_000, "the search does not move on");
            index.find_first(&mut search, |_, _, _| true, || false);
        }
        let last = search.found().last().cloned().flatten();
        assert_eq!(last, index.find_where("automaton", |_, _| true, 0));
        assert!(search.found().iter().take(300).all(Option::is_none));
    }

    #[test]
    fn long_passages_sharing_a_prefix_are_compared_within_the_budget_inside_one_place() {
        // Crafted marks: hundreds of long passages share their prefix with a long run of the same
        // text, so every place there means comparing hundreds of long tails. One place is more work
        // than a look allows, so the search stops inside a place and resumes there.
        let d = doc(&[&"a".repeat(600)]);
        let mut passages: Vec<String> = (0..300).map(|k| format!("{}{k}", "a".repeat(400))).collect();
        passages.push("a".repeat(100));
        let mut index = TextIndex::default();
        index.refresh(&d, 1);
        let mut search = FirstMatches::new(passages.clone());
        let (mut calls, mut stopped_inside_a_place) = (0, 0);
        while !search.done() {
            calls += 1;
            assert!(calls < 100_000, "the search does not move on");
            index.find_first(&mut search, |_, _, _| true, || false);
            stopped_inside_a_place += usize::from(search.within.is_some_and(|within| within.partial > 0));
        }
        assert!(stopped_inside_a_place > 100, "stopped inside a place only {stopped_inside_a_place} times");
        let single: Vec<Option<(Pos, Pos)>> = passages.iter().map(|passage| index.find_where(passage, |_, _| true, 0)).collect();
        assert_eq!(search.found(), single.as_slice());
        assert!(search.found().last().is_some_and(Option::is_some));
    }

    #[test]
    fn the_automaton_stays_small_for_the_most_and_longest_passages() {
        // MAX_MARKS passages at the length cap, each with its own opening, so prefixes share little.
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let passages: Vec<String> = (0..crate::host::wire::MAX_MARKS)
            .map(|_| {
                (0..MAX_NEEDLE_CHARS)
                    .map(|_| {
                        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                        char::from(b'a' + ((seed >> 33) % 26) as u8)
                    })
                    .collect()
            })
            .collect();
        let mut search = FirstMatches::new(passages);
        assert!(search.prepare(&mut || true));
        let automaton = search.automaton.as_ref().expect("built");
        assert_eq!(automaton.patterns_len(), crate::host::wire::MAX_MARKS);
        // At most 2,000 × 64 bytes = 128,000 trie states; at a few dozen bytes a state that is a few
        // megabytes. 16 MB leaves room for the library's choice of representation, and is far below
        // the hundreds of megabytes whole passages took.
        assert!(automaton.memory_usage() < 16 * 1024 * 1024, "{} bytes", automaton.memory_usage());
    }

    #[test]
    fn one_pass_accepts_what_find_where_accepts() {
        // Self-overlapping places: find_where tests only non-overlapping ones, and so must one pass.
        let d = doc(&["aaaa aaaa", "xaaay", "ab ab abab"]);
        let passages = ["aa", "aaa", "aa", "abab", "ab", "b ab"];
        let reject_first_paragraph_start = |k: usize, start: &Pos| !(k.is_multiple_of(2) && start.path == Pos::body(0, 0).path && start.off == 0);
        for stop in [false, true] {
            let (one_pass, single) = both_ways(&d, &passages, reject_first_paragraph_start, stop);
            assert_eq!(one_pass, single, "stopping after each paragraph: {stop}");
            assert_eq!(one_pass[0].as_ref().map(|(from, _)| from.off), Some(2), "the next non-overlapping place");
        }
        let never = |_: usize, _: &Pos| false;
        let (one_pass, single) = both_ways(&d, &passages, never, false);
        assert_eq!(one_pass, single);
        assert!(one_pass.iter().all(Option::is_none));
    }

    #[test]
    fn one_pass_agrees_with_find_where_on_a_large_set() {
        let paragraphs: Vec<String> = (1..=300)
            .map(|i| format!("Clause {i}. The \u{201C}Supplier\u{201D} shall deliver the goods in Schedule {} within {} days.", i % 17, i % 23))
            .collect();
        let refs: Vec<&str> = paragraphs.iter().map(String::as_str).collect();
        let d = doc(&refs);
        let passages: Vec<String> = (0..2_000)
            .map(|k| match k % 5 {
                0 => format!("clause {}. the \"supplier\"", k % 400),
                1 => format!("schedule {} within", k % 19),
                2 => format!("within {} days. ", k % 29),
                3 => "the \u{2018}supplier".to_string(),
                _ => format!("deliver the goods in schedule {}", k % 17),
            })
            .collect();
        let passage_refs: Vec<&str> = passages.iter().map(String::as_str).collect();
        // A page hint stands in as "the k-th passage wants paragraph k % 300 or later" for every third one.
        let accept = |k: usize, start: &Pos| !k.is_multiple_of(3) || start.path.0.first().is_some_and(|&n| n as usize >= k % 300);
        let (one_pass, single) = both_ways(&d, &passage_refs, accept, false);
        assert_eq!(one_pass.len(), 2_000);
        assert!(one_pass.iter().filter(|found| found.is_some()).count() > 1_000, "the fixture finds too little");
        assert_eq!(one_pass, single);
    }
}

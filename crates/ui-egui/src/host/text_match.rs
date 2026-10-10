//! Text matching for host jumps and marks. The passage and the document are folded the same way,
//! by one pipeline ([`fold_mapped`]): Unicode compatibility forms (NFKC), lower case, curly quotes
//! as straight ones, dashes as a hyphen, every run of whitespace as one space, inline objects
//! (U+FFFC: comment anchors, pictures) skipped. The document side is the text the host sees:
//! field results inlined, tracked deletions left out ([`visible_text`]). A match may cross line
//! breaks inside a paragraph, never a paragraph boundary.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;
use wordcraft_doc::para::OBJ;
use wordcraft_doc::{Document, Paragraph, Path, Pos, StoryRef};

/// Longest passage a host may ask for, in folded characters; longer ones match nothing.
pub const MAX_NEEDLE_CHARS: usize = 10_000;
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

/// The host's passage as matching compares it, or `None` when nothing is left to match or it is
/// longer than [`MAX_NEEDLE_CHARS`]. Folding stops as soon as the passage is known to be too long.
pub fn fold_query(passage: &str) -> Option<String> {
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
            if let Some((start, finish)) = folded.source_range(at, end) {
                return Some((Pos::new(StoryRef::Body, path.clone(), start), Pos::new(StoryRef::Body, path.clone(), finish)));
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
    fn fold_is_the_text_of_the_mapped_form() {
        let sample = "  \u{201C}Quoted\u{201D}\u{00A0}\u{00A0}TEXT \u{2014}   \u{FB01}ne\u{FFFC}  \u{2018}x\u{2019}  ";
        let mapped: String = fold_mapped(sample, usize::MAX).unwrap_or_default().into_iter().map(|(folded_char, _, _)| folded_char).collect();
        assert_eq!(Folded::new(sample).text, mapped);
        assert_eq!(fold(sample), mapped.trim_end());
        assert_eq!(fold(sample), "\"quoted\" text - fine 'x'");
    }
}

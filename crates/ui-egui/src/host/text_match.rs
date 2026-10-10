//! Text matching for host jumps and marks. The passage and the document are folded the same way,
//! by one pipeline ([`fold_mapped`]): Unicode compatibility forms (NFKC), lower case, curly quotes
//! as straight ones, dashes as a hyphen, every run of whitespace as one space, inline objects
//! (U+FFFC: comment anchors, fields, pictures) skipped. A match may cross line breaks inside a
//! paragraph, never a paragraph boundary.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;
use wordcraft_doc::para::OBJ;
use wordcraft_doc::{Document, Path, Pos, StoryRef};

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
/// whitespace and inline objects produce nothing.
fn fold_mapped(src: &str) -> Vec<(char, usize, usize)> {
    let mut out: Vec<(char, usize, usize)> = Vec::with_capacity(src.len());
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
    }
    out
}

/// The text of `src` as matching compares it, without trailing whitespace (a trailing space in a
/// passage is not part of what to find).
pub fn fold(src: &str) -> String {
    let folded: String = fold_mapped(src).into_iter().map(|(folded_char, _, _)| folded_char).collect();
    folded.trim_end().to_string()
}

/// The host's passage as matching compares it, or `None` when nothing is left to match or it is
/// longer than [`MAX_NEEDLE_CHARS`].
pub fn fold_query(passage: &str) -> Option<String> {
    let folded = fold(passage);
    (!folded.is_empty() && folded.chars().count() <= MAX_NEEDLE_CHARS).then_some(folded)
}

/// Where one folded character came from: its byte offset in the folded text and the source bytes.
#[derive(Clone, Copy, Debug)]
struct Span {
    folded: usize,
    start: usize,
    end: usize,
}

/// Folded text, with where each folded character came from.
#[derive(Clone, Debug, Default)]
pub struct Folded {
    pub text: String,
    spans: Vec<Span>,
}

impl Folded {
    pub fn new(src: &str) -> Folded {
        let mut out = Folded::default();
        for (folded_char, start, end) in fold_mapped(src) {
            out.spans.push(Span { folded: out.text.len(), start, end });
            out.text.push(folded_char);
        }
        out
    }

    /// Source byte range of the folded byte range `from..to` (a match found in `text`).
    pub fn source_range(&self, from: usize, to: usize) -> Option<(usize, usize)> {
        let first = self.spans.get(self.spans.partition_point(|span| span.folded < from))?;
        let last = self.spans.get(self.spans.partition_point(|span| span.folded < to).checked_sub(1)?)?;
        (first.folded == from && first.start <= last.end).then_some((first.start, last.end))
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
            .filter_map(|path| doc.para(StoryRef::Body, &path).map(|paragraph| (path.clone(), Folded::new(&paragraph.text))))
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
    use wordcraft_doc::{Document, Paragraph, para_block};

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
        let mut d = doc(&["before"]);
        d.body.push(std::sync::Arc::new(wordcraft_doc::Block::Table(wordcraft_doc::Table::new(1, 1, 400.0))));
        let mut index = TextIndex::default();
        index.refresh(&d, 1);
        assert_eq!(index.find("before").len(), 1);
    }

    #[test]
    fn fold_is_the_text_of_the_mapped_form() {
        let sample = "  \u{201C}Quoted\u{201D}\u{00A0}\u{00A0}TEXT \u{2014}   \u{FB01}ne\u{FFFC}  \u{2018}x\u{2019}  ";
        let mapped: String = fold_mapped(sample).into_iter().map(|(folded_char, _, _)| folded_char).collect();
        assert_eq!(Folded::new(sample).text, mapped);
        assert_eq!(fold(sample), mapped.trim_end());
        assert_eq!(fold(sample), "\"quoted\" text - fine 'x'");
    }
}

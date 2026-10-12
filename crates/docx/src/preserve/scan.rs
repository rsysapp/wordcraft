//! embed: which characters of a paragraph the reader made into the model, and where each one is.
//!
//! The reader turns each `w:p` into a model paragraph whose text holds one character per visible
//! character and one U+FFFC per object (field, drawing, note reference, bookmark or comment marker).
//! The scan walks the same parts in the same order (notes, comments, the main part, headers and
//! footers at the section properties that name them), with the same per-story state (fields spanning
//! runs and paragraphs, markers waiting between blocks, bookmark, comment and note ids seen so far,
//! the parts made so far), and records for each of those characters where it is: a character of a
//! `w:t`, a one-character child (`w:tab`…), whole nodes (a field from its first run to its last), a
//! marker, or something copied whole and never edited ("locked"). The rules it shares with the reader
//! (`clean_char`, `choice_understood`, the depth and part limits) are the reader's own; the rest is
//! checked by the preserving writer, which refuses to rewrite a paragraph whose scanned characters
//! differ from the model's text.
//!
//! Bounds: everything the scan holds (each part's tree and paragraph numbers while it is walked, the
//! paragraph being walked, every paragraph and part copy it keeps, the ids it remembers) is charged to
//! the caller's memory budget before it is allocated, and the scan is refused past it. Each node
//! names only its innermost wrapper, so what a paragraph holds grows with its markup, not with its
//! nesting.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use wordcraft_doc::para::OBJ;
use wordcraft_doc::table::{MAX_COLS, MAX_ROWS};

use super::tree::{PEl, PKid};
use crate::DocxError;
use crate::inspect::budget::{Budget, table_sized, table_slot, tree_slot, units, vector_slot};
use crate::package::{Package, Rels, rt};
use crate::read::MAX_PARTS;
use crate::read::story::{MAX_FIELD_DEPTH, MAX_STORY_DEPTH, MAX_TABLE_DEPTH, choice_understood, clean_char};
use crate::units::int;
use crate::xml::MAX_DEPTH;

/// The most items a field's result buffers, as the reader's `emit_text`/`emit_obj`.
const MAX_FIELD_ITEMS: usize = 100_000;

/// Why a scan stopped for memory.
pub(crate) const SCAN_TOO_LARGE: &str = "more memory than a save may hold to scan the document";

/// A story part: its bytes, its root element, the prefix it writes WordprocessingML names with,
/// and the paragraphs asked for, by position among the part's `w:p` elements.
pub(crate) struct ScannedPart {
    pub bytes: Arc<Vec<u8>>,
    /// Canonical: `w:document`, `w:hdr`, `w:ftr`, `w:footnotes`, `w:endnotes`, `w:comments`.
    pub root: String,
    /// The root's prefix as written (`""` for a default namespace); only a name the part can write
    /// again (`PEl::reusable_name`), else the part is not scanned.
    pub prefix: String,
    pub paragraphs: BTreeMap<u32, ScannedPara>,
}

// The part's bytes by length only: a part can be many megabytes.
impl std::fmt::Debug for ScannedPart {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScannedPart")
            .field("bytes", &self.bytes.len())
            .field("root", &self.root)
            .field("prefix", &self.prefix)
            .field("paragraphs", &self.paragraphs)
            .finish()
    }
}

/// A paragraph the reader made. Every written name it records (`raw_name`) is one the part can
/// write again (`PEl::reusable_name`); a paragraph holding one that is not is left unscanned.
#[derive(Clone, Debug)]
pub(crate) struct ScannedPara {
    pub start: Range<usize>,
    pub end: Option<Range<usize>>,
    pub raw_name: String,
    pub ppr: Option<Props>,
    /// The paragraph's content, flattened: runs and other elements in order, each naming the
    /// innermost wrapper around it.
    pub nodes: Vec<Node>,
    /// Every wrapper, each after the wrapper around it (its `parent`).
    pub wrappers: Vec<Wrapper>,
    /// One per character of the model paragraph's text, in order.
    pub units: Vec<Unit>,
}

impl ScannedPara {
    /// The paragraph's text as the reader made it.
    pub fn text(&self) -> String {
        self.units.iter().map(|unit| unit.ch).collect()
    }

    pub fn span(&self) -> Range<usize> {
        self.start.start..self.end.as_ref().map_or(self.start.end, |end| end.end)
    }

    /// The wrappers around a node whose innermost wrapper is `innermost`, outermost first
    /// (positions in `wrappers`).
    pub fn chain(&self, innermost: Option<usize>) -> Vec<usize> {
        let mut chain = Vec::new();
        let mut at = innermost;
        // A parent always comes before its child, so this ends; the bound only guards that.
        while let Some(index) = at
            && chain.len() < self.wrappers.len()
        {
            chain.push(index);
            at = self.wrappers.get(index).and_then(|wrapper| wrapper.parent).filter(|parent| *parent < index);
        }
        chain.reverse();
        chain
    }

    /// Where the paragraph may be edited: the longest stretches of units that are characters of a
    /// `w:t` or one-character children (`Loc::Text`, `Loc::Single`), as ranges of positions in
    /// `units`. Objects, markers and everything locked (a field's result among them, 6C-18, and a
    /// text box's paragraphs, 6C-39) lie between them. Deleted text and text inside content controls
    /// count as editable here: a caller choosing edit points filters them by wrapper.
    pub fn editable(&self) -> Vec<Range<usize>> {
        let mut ranges: Vec<Range<usize>> = Vec::new();
        for (index, unit) in self.units.iter().enumerate() {
            if !matches!(unit.loc, Loc::Text { .. } | Loc::Single { .. }) {
                continue;
            }
            match ranges.last_mut() {
                Some(range) if range.end == index => range.end = index + 1,
                _ => ranges.push(index..index + 1),
            }
        }
        ranges
    }
}

/// A properties element (`w:pPr`, the `w:rPr` inside it): its tags and its children by name.
#[derive(Clone, Debug)]
pub(crate) struct Props {
    pub start: Range<usize>,
    pub end: Option<Range<usize>>,
    pub raw_name: String,
    pub children: Vec<(String, Range<usize>)>,
    pub rpr: Option<Box<Props>>,
}

#[derive(Clone, Debug)]
pub(crate) enum Node {
    Run(RunNode),
    /// Anything else, copied whole: a bookmark, a content control's properties, a proofing mark,
    /// a field written as one element, alternative content, an equation.
    Other {
        name: String,
        /// The innermost wrapper around it (`ScannedPara::chain` gives them all).
        wrapper: Option<usize>,
        span: Range<usize>,
    },
}

impl Node {
    /// The innermost wrapper around the node.
    pub fn wrapper(&self) -> Option<usize> {
        match self {
            Node::Run(run) => run.wrapper,
            Node::Other { wrapper, .. } => *wrapper,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RunNode {
    /// The innermost wrapper around it (`ScannedPara::chain` gives them all).
    pub wrapper: Option<usize>,
    pub start: Range<usize>,
    pub end: Option<Range<usize>>,
    pub raw_name: String,
    /// The run's first `w:rPr`, which the reader formats it by.
    pub rpr: Option<Range<usize>>,
    /// The `w:rPrChange` inside `rpr`: left out when the properties are copied to typed text.
    pub rpr_change: Option<Range<usize>>,
    /// Every child element but the first `w:rPr`, in order.
    pub children: Vec<Child>,
}

impl RunNode {
    pub fn span(&self) -> Range<usize> {
        self.start.start..self.end.as_ref().map_or(self.start.end, |end| end.end)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Child {
    pub span: Range<usize>,
    pub kind: ChildKind,
}

#[derive(Clone, Debug)]
pub(crate) enum ChildKind {
    /// `w:t` or `w:delText`, character by character.
    Text { start: Range<usize>, end: Option<Range<usize>>, raw_name: String, deleted: bool, chars: Vec<RawChar> },
    /// A child that is one character (`w:tab`, `w:br`…), and that character's unit (`None` when the
    /// character is not an editable unit where it stands: inside a field's result or a text box).
    Single { unit: Option<usize> },
    /// Any other child (`w:fldChar`, `w:instrText`, `w:drawing`, a `w:t` with CDATA or elements, a
    /// second `w:rPr`…).
    Other { start: Range<usize>, end: Option<Range<usize>>, raw_name: String },
}

#[derive(Clone, Debug)]
pub(crate) struct RawChar {
    /// The character's bytes in the part: itself, or its `&…;` reference.
    pub bytes: Range<usize>,
    /// The unit it became, when it is an editable one (`Loc::Text`); `None` for a character the
    /// reader drops (a control character, an unknown reference) or one that is locked (inside a
    /// field's result or a text box).
    pub unit: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WrapperKind {
    /// `w:ins`, `w:moveTo`.
    Inserted,
    /// `w:del`, `w:moveFrom`.
    Deleted,
    /// `w:hyperlink`, `w:sdt`, `w:sdtContent`, `w:smartTag`, `w:customXml`, `w:dir`, `w:bdo`.
    Other,
}

#[derive(Clone, Debug)]
pub(crate) struct Wrapper {
    pub kind: WrapperKind,
    pub start: Range<usize>,
    pub end: Option<Range<usize>>,
    /// The wrapper around this one (an earlier position in `ScannedPara::wrappers`).
    pub parent: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unit {
    pub ch: char,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Loc {
    /// A character of a `w:t`/`w:delText` child: `raw` is its position in that child's characters.
    Text { node: usize, child: usize, raw: usize },
    /// A one-character child.
    Single { node: usize, child: usize },
    /// An object made of whole nodes that hold nothing but what the object owns, under one wrapper:
    /// a field of runs from its first to its last (their properties, field characters,
    /// instructions, result text and characters, rendered page breaks), a picture or note reference
    /// alone in its run, an equation. Removing those nodes removes the object and nothing else.
    Atomic { first: usize, last: usize },
    /// A bookmark or comment range marker in the paragraph.
    Marker { node: usize },
    /// Inside something copied whole and never edited (alternative content, a field written as
    /// one element, ruby, CDATA, a comment reference, a page number, anything inside a field's
    /// instruction or result, a text box's paragraph, an object whose nodes hold anything it does
    /// not own): where it stands, when known.
    Locked { node: Option<usize>, child: Option<usize> },
    /// A marker the reader moved into the paragraph from between blocks: at its start, or, for
    /// the last paragraph of a story, at its end.
    Outside { at_end: bool },
}

/// The main document part, as the reader finds it.
pub(crate) fn main_part(pkg: &Package) -> String {
    pkg.rels("").by_type(rt::OFFICE_DOC).filter(|rel| !rel.external).map(|rel| rel.target.clone()).unwrap_or_else(|| "word/document.xml".to_string())
}

/// Scan every story part in the reader's order, keeping the paragraphs `wanted` names (part name
/// → positions). A part with nothing wanted is still walked: bookmarks, comments and notes seen
/// there change what the reader makes later.
///
/// The parts are parsed within the package's read limits and element budget (`Package::tree`). As
/// the reader does, it refuses a package whose main part cannot be read and skips a secondary part
/// that is not well formed (its paragraphs stay unscanned). A part refused for any limit refuses the
/// scan, as does a caller's limit reached: the reader may have read that part, and the scan must
/// never go on with state the reader's did not have. What the scan holds is charged to `budget`'s
/// memory, refused past it.
pub(crate) fn scan_package(
    pkg: &Package,
    wanted: &BTreeMap<String, BTreeSet<u32>>,
    budget: &mut Budget,
) -> Result<BTreeMap<String, ScannedPart>, DocxError> {
    // The budget is lent to the walk and given back whatever it answers.
    let memory = Memory { budget: RefCell::new(std::mem::replace(budget, Budget::new(0, 0))), refused: Cell::new(false), net: Cell::new(0) };
    let scanned = scan_with(pkg, wanted, &memory);
    // A refused scan has dropped everything it kept: all it held is given back.
    if scanned.is_err() {
        memory.release(memory.net.get());
    }
    *budget = memory.budget.into_inner();
    scanned
}

fn scan_with(pkg: &Package, wanted: &BTreeMap<String, BTreeSet<u32>>, memory: &Memory) -> Result<BTreeMap<String, ScannedPart>, DocxError> {
    let main = main_part(pkg);
    let rels = pkg.rels(&main);
    let part = |kind: &str| rels.by_type(kind).filter(|rel| !rel.external).map(|rel| rel.target.clone());
    let mut scanner = Scanner {
        pkg,
        wanted,
        memory,
        failure: None,
        bookmarks: HashSet::new(),
        comments: HashSet::new(),
        comments_ended: HashSet::new(),
        footnotes: HashSet::new(),
        endnotes: HashSet::new(),
        headers: HashSet::new(),
        parts_made: 0,
        parts: BTreeMap::new(),
    };
    if let Some(name) = part(rt::FOOTNOTES) {
        secondary(scanner.notes(&name, true))?;
    }
    if let Some(name) = part(rt::ENDNOTES) {
        secondary(scanner.notes(&name, false))?;
    }
    if let Some(name) = part(rt::COMMENTS) {
        secondary(scanner.comments(&name))?;
    }
    let found = scanner.main(&main)?;
    if let Some(failure) = scanner.failure.take() {
        return Err(failure);
    }
    if memory.refused.get() {
        return Err(DocxError::Limit(SCAN_TOO_LARGE.into()));
    }
    if !found {
        return Err(DocxError::MissingPart(main));
    }
    if pkg.limit_reached() {
        return Err(DocxError::Limit("a part is past the read limits (elements or nesting)".into()));
    }
    Ok(scanner.parts)
}

/// A secondary part's outcome: skipped when it is not well formed, as the reader skips it; refused
/// when a limit stopped it.
fn secondary(outcome: Result<bool, DocxError>) -> Result<(), DocxError> {
    match outcome {
        Err(error @ DocxError::Limit(_)) => Err(error),
        _ => Ok(()),
    }
}

/// The caller's budget, lent to the walk: every holder charges it through here.
struct Memory {
    budget: RefCell<Budget>,
    /// A charge was refused: the walk stops and the scan is refused.
    refused: Cell<bool>,
    /// What the scan holds now, all told.
    net: Cell<u64>,
}

impl Memory {
    /// Hold `bytes` more, or refuse (once refused, always).
    fn hold(&self, bytes: u64) -> bool {
        if self.refused.get() {
            return false;
        }
        let held = self.budget.try_borrow_mut().is_ok_and(|mut budget| budget.hold(bytes).is_ok());
        if held {
            self.net.set(self.net.get().saturating_add(bytes));
        } else {
            self.refused.set(true);
        }
        held
    }

    fn release(&self, bytes: u64) {
        let bytes = bytes.min(self.net.get());
        if let Ok(mut budget) = self.budget.try_borrow_mut() {
            budget.release(bytes);
            self.net.set(self.net.get() - bytes);
        }
    }

    /// What may still be held.
    fn room(&self) -> u64 {
        if self.refused.get() {
            return 0;
        }
        self.budget.try_borrow().map_or(0, |budget| budget.room())
    }
}

fn bytes_of_size<T>(count: usize) -> u64 {
    units(count).saturating_mul(units(std::mem::size_of::<T>()))
}

/// What a parsed part holds: each element's record, names and attributes. Walked with its own stack.
fn tree_bytes(root: &PEl) -> u64 {
    let mut total = units(std::mem::size_of::<PEl>());
    let mut stack = vec![root];
    while let Some(element) = stack.pop() {
        total = total
            .saturating_add(units(element.name.capacity()))
            .saturating_add(units(element.raw_name.capacity()))
            .saturating_add(bytes_of_size::<(String, String)>(element.attrs.capacity()))
            .saturating_add(element.attrs.iter().fold(0u64, |sum, (key, value)| sum.saturating_add(units(key.capacity() + value.capacity()))))
            .saturating_add(bytes_of_size::<PKid>(element.kids.capacity()));
        stack.extend(element.els());
    }
    total
}

/// What a kept paragraph holds, its entry in the part's map included.
fn paragraph_bytes(paragraph: &ScannedPara) -> u64 {
    fn props_bytes(props: &Props) -> u64 {
        units(props.raw_name.capacity())
            .saturating_add(bytes_of_size::<(String, Range<usize>)>(props.children.capacity()))
            .saturating_add(props.children.iter().fold(0u64, |sum, (name, _)| sum.saturating_add(units(name.capacity()))))
            .saturating_add(props.rpr.as_ref().map_or(0, |rpr| units(std::mem::size_of::<Props>()).saturating_add(props_bytes(rpr))))
    }
    let nodes = paragraph.nodes.iter().fold(0u64, |sum, node| {
        sum.saturating_add(match node {
            Node::Other { name, .. } => units(name.capacity()),
            Node::Run(run) => units(run.raw_name.capacity()).saturating_add(bytes_of_size::<Child>(run.children.capacity())).saturating_add(
                run.children.iter().fold(0u64, |sum, child| {
                    sum.saturating_add(match &child.kind {
                        ChildKind::Text { raw_name, chars, .. } => {
                            units(raw_name.capacity()).saturating_add(bytes_of_size::<RawChar>(chars.capacity()))
                        }
                        ChildKind::Other { raw_name, .. } => units(raw_name.capacity()),
                        ChildKind::Single { .. } => 0,
                    })
                }),
            ),
        })
    });
    tree_slot::<u32, ScannedPara>()
        .saturating_add(units(paragraph.raw_name.capacity()))
        .saturating_add(paragraph.ppr.as_ref().map_or(0, props_bytes))
        .saturating_add(bytes_of_size::<Node>(paragraph.nodes.capacity()))
        .saturating_add(nodes)
        .saturating_add(bytes_of_size::<Wrapper>(paragraph.wrappers.capacity()))
        .saturating_add(bytes_of_size::<Unit>(paragraph.units.capacity()))
}

/// Every `w:p` of a part by address, numbered in document order (pre-order, every branch of
/// alternative content included), as the reader's `with_part` numbers them.
fn number_paragraphs(root: &PEl, count: usize) -> HashMap<usize, u32> {
    fn go(element: &PEl, next: &mut u32, out: &mut HashMap<usize, u32>, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        for child in element.els() {
            if child.name == "w:p" {
                out.insert(address(child), *next);
                *next = next.saturating_add(1);
            }
            go(child, next, out, depth + 1);
        }
    }
    let mut out = HashMap::with_capacity(count);
    go(root, &mut 0, &mut out, 0);
    out
}

/// How many `w:p` elements `element` holds, as `number_paragraphs` counts them.
fn count_paragraphs(element: &PEl, depth: usize) -> usize {
    if depth > MAX_DEPTH {
        return 0;
    }
    element
        .els()
        .fold(0usize, |count, child| count.saturating_add(usize::from(child.name == "w:p")).saturating_add(count_paragraphs(child, depth + 1)))
}

fn address(element: &PEl) -> usize {
    element as *const PEl as usize
}

/// The reader's per-story state (`read::story::StoryCtx`).
#[derive(Default)]
struct Story {
    fields: Vec<Field>,
    ignored_begins: usize,
    pending: Vec<Unit>,
    /// Text-box nesting: a story deeper than 0 is a text box's.
    depth: usize,
}

struct Field {
    /// Past `separate`: items go to the result.
    result: bool,
    /// Emitted at a paragraph's end; what follows flows into the paragraph, locked.
    spilled: bool,
    /// The result's units, and how many items (the reader's `buf` entries) they came in.
    buf: Vec<Unit>,
    items: usize,
    /// What `buf` is charged.
    charged: u64,
    /// The node it began in.
    first: Option<usize>,
}

/// One paragraph under construction, charging what it holds as it grows.
struct Builder<'m> {
    memory: &'m Memory,
    /// What this paragraph holds, charged: released when it is not kept, or settled to what the kept
    /// paragraph holds.
    held: u64,
    nodes: Vec<Node>,
    wrappers: Vec<Wrapper>,
    units: Vec<Unit>,
    /// Whether every written name recorded can be written again.
    reusable: bool,
    /// The child that made each picture or note object standing alone in a node (by node).
    makers: HashMap<usize, usize>,
    /// The spans of fields that became objects.
    fields: HashSet<(usize, usize)>,
    /// The equations (by node) that hold no WordprocessingML but run properties and text.
    equations: HashSet<usize>,
    /// What an object may own among the children recorded whole, by (node, child), from their
    /// canonical names.
    owned: HashMap<(usize, usize), Owned>,
}

/// A child recorded whole that an object may own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owned {
    /// `w:fldChar`, `w:instrText`, `w:delInstrText`, a `w:sym` the reader drops: a field's own.
    Field,
    /// A `w:t`/`w:delText` copied whole (a custom note mark, CDATA).
    Text,
    /// `w:lastRenderedPageBreak`: where Word last broke the page, owned by whatever holds it.
    Break,
}

fn owned_kind(name: &str) -> Option<Owned> {
    match name {
        "w:fldChar" | "w:instrText" | "w:delInstrText" | "w:sym" => Some(Owned::Field),
        "w:t" | "w:delText" => Some(Owned::Text),
        "w:lastRenderedPageBreak" => Some(Owned::Break),
        _ => None,
    }
}

impl<'m> Builder<'m> {
    fn new(memory: &'m Memory) -> Builder<'m> {
        Builder {
            memory,
            held: 0,
            nodes: Vec::new(),
            wrappers: Vec::new(),
            units: Vec::new(),
            reusable: true,
            makers: HashMap::new(),
            fields: HashSet::new(),
            equations: HashSet::new(),
            owned: HashMap::new(),
        }
    }

    /// Record that the equation at `node` is plain: it holds no WordprocessingML but run properties
    /// and text (and what is inside them). The reader keeps only an equation's linear text, so a
    /// bookmark, comment range, permission range or tracked change inside one is markup the model
    /// does not hold, and the equation is not whole. Walked with a stack of one entry per level,
    /// charged while it is held.
    fn plain_equation(&mut self, node: Option<usize>, equation: &PEl) {
        let Some(node) = node else { return };
        let level = units(std::mem::size_of::<std::slice::Iter<'_, PKid>>());
        let mut levels = vec![equation.els()];
        let mut charged = 0u64;
        let mut plain = true;
        while let Some(at) = levels.last_mut() {
            match at.next() {
                Some(child) if child.name == "w:rPr" || child.name == "w:t" => {}
                Some(child) if child.name.starts_with("w:") => {
                    plain = false;
                    break;
                }
                Some(child) => {
                    if !self.memory.hold(level.saturating_mul(2)) {
                        plain = false;
                        break;
                    }
                    charged = charged.saturating_add(level.saturating_mul(2));
                    levels.push(child.els());
                }
                None => {
                    levels.pop();
                }
            }
        }
        drop(levels);
        self.memory.release(charged);
        if plain && self.hold(table_slot::<usize>()) {
            self.equations.insert(node);
        }
    }

    /// Charge `bytes` more to this paragraph, or refuse.
    fn hold(&mut self, bytes: u64) -> bool {
        let held = self.memory.hold(bytes);
        if held {
            self.held = self.held.saturating_add(bytes);
        }
        held
    }

    /// The written name of `element`, to record; one the part could not write again marks the
    /// paragraph unscannable.
    fn name(&mut self, element: &PEl) -> String {
        match element.reusable_name() {
            Some(name) if self.hold(units(name.len())) => name.to_string(),
            Some(_) => String::new(),
            None => {
                self.reusable = false;
                String::new()
            }
        }
    }

    /// Units into the paragraph; none past the budget.
    fn units(&mut self, units: impl IntoIterator<Item = Unit>) {
        for unit in units {
            if !self.hold(vector_slot::<Unit>()) {
                return;
            }
            self.units.push(unit);
        }
    }

    fn other(&mut self, element: &PEl, wrapper: Option<usize>) -> Option<usize> {
        if !self.hold(vector_slot::<Node>().saturating_add(units(element.name.len()))) {
            return None;
        }
        self.nodes.push(Node::Other { name: element.name.clone(), wrapper, span: element.span() });
        Some(self.nodes.len() - 1)
    }

    fn run(&mut self, element: &PEl, wrapper: Option<usize>) -> Option<usize> {
        if !self.hold(vector_slot::<Node>()) {
            return None;
        }
        let raw_name = self.name(element);
        self.nodes.push(Node::Run(RunNode {
            wrapper,
            start: element.start.clone(),
            end: element.end.clone(),
            raw_name,
            rpr: None,
            rpr_change: None,
            children: Vec::new(),
        }));
        Some(self.nodes.len() - 1)
    }

    fn wrapper(&mut self, wrapper: Wrapper) -> Option<usize> {
        if !self.hold(vector_slot::<Wrapper>()) {
            return None;
        }
        self.wrappers.push(wrapper);
        Some(self.wrappers.len() - 1)
    }

    fn child(&mut self, node: Option<usize>, child: Child) -> Option<usize> {
        if !matches!(node.and_then(|node| self.nodes.get(node)), Some(Node::Run(_))) || !self.hold(vector_slot::<Child>()) {
            return None;
        }
        let Some(Node::Run(run)) = node.and_then(|node| self.nodes.get_mut(node)) else { return None };
        run.children.push(child);
        Some(run.children.len() - 1)
    }

    /// A child recorded whole.
    fn other_child(&mut self, node: Option<usize>, element: &PEl) -> Option<usize> {
        let raw_name = self.name(element);
        let child = self
            .child(node, Child { span: element.span(), kind: ChildKind::Other { start: element.start.clone(), end: element.end.clone(), raw_name } });
        if let (Some(node), Some(child), Some(kind)) = (node, child, owned_kind(&element.name))
            && self.hold(table_slot::<((usize, usize), Owned)>())
        {
            self.owned.insert((node, child), kind);
        }
        child
    }

    fn props(&mut self, element: &PEl) -> Props {
        let raw_name = self.name(element);
        let names = element.els().fold(0u64, |sum, child| sum.saturating_add(units(child.name.len())));
        let count = element.els().count();
        let children = if self.hold(bytes_of_size::<(String, Range<usize>)>(count).saturating_add(names)) {
            element.els().map(|child| (child.name.clone(), child.span())).collect()
        } else {
            Vec::new()
        };
        let rpr = element.child("w:rPr").filter(|_| self.hold(units(std::mem::size_of::<Props>()))).map(|rpr| Box::new(self.props(rpr)));
        Props { start: element.start.clone(), end: element.end.clone(), raw_name, children, rpr }
    }
}

/// The last block of a block list (the reader's `out.last()`), for markers left at its end.
#[derive(Default)]
struct Out {
    last: Option<Last>,
}

#[derive(Clone, Copy)]
enum Last {
    /// A paragraph, by its position in the part (`None`: a table flattened into one).
    Para(Option<u32>),
    Table,
}

/// Inside something copied whole: the paragraph node that holds it, when there is one.
#[derive(Clone, Copy)]
struct Lock(Option<usize>);

/// A note reference waiting for its object (the reader's `note`).
#[derive(Clone, Copy)]
struct Note {
    /// A custom mark follows: the next `w:t` is the mark.
    custom: bool,
    /// The reference stood inside something copied whole.
    locked: bool,
    /// The reference's child in its run.
    child: Option<usize>,
}

struct Ctx<'t> {
    /// The part's canonical name (`Package::part_name`).
    part: &'t str,
    src: &'t [u8],
    rels: &'t Rels,
    ordinals: &'t HashMap<usize, u32>,
}

struct Scanner<'p> {
    pkg: &'p Package,
    wanted: &'p BTreeMap<String, BTreeSet<u32>>,
    memory: &'p Memory,
    /// A part refused for a limit while the walk went on (a header): the scan is refused.
    failure: Option<DocxError>,
    /// Bookmark ids a `w:bookmarkStart` opened (the reader's `bookmarks`).
    bookmarks: HashSet<String>,
    /// Comment ids the comments part holds, as far as it has been read (`comment_map`).
    comments: HashSet<String>,
    comments_ended: HashSet<String>,
    footnotes: HashSet<i64>,
    endnotes: HashSet<i64>,
    /// Header and footer parts already read, by canonical name (`hf_by_path`).
    headers: HashSet<String>,
    /// The parts the reader has made (`doc.parts.len()`), held to `MAX_PARTS` as it holds them.
    parts_made: usize,
    parts: BTreeMap<String, ScannedPart>,
}

/// Remember `id` in `set`, charged for the room a growing table can hold; false past the budget.
fn remember<T: std::hash::Hash + Eq>(memory: &Memory, set: &mut HashSet<T>, id: T, extra: usize) -> bool {
    if set.contains(&id) {
        return true;
    }
    if !memory.hold(table_slot::<T>().saturating_add(units(extra))) {
        return false;
    }
    set.insert(id);
    true
}

/// A unit that is locked where it stands: inside a field (a spilled field's result or
/// instruction) or a text box, copied whole and never edited.
fn locked_unit(unit: Unit) -> Unit {
    let loc = match unit.loc {
        Loc::Text { node, child, .. } | Loc::Single { node, child } => Loc::Locked { node: Some(node), child: Some(child) },
        Loc::Atomic { first, .. } => Loc::Locked { node: Some(first), child: None },
        Loc::Marker { node } => Loc::Locked { node: Some(node), child: None },
        loc => loc,
    };
    Unit { ch: unit.ch, loc }
}

/// One item where the reader's `emit_text`/`emit_obj` put it: the innermost open field's result,
/// nowhere (its instruction), or the paragraph (locked while a spilled field is open).
fn emit(story: &mut Story, b: &mut Builder<'_>, item: Vec<Unit>) {
    match story.fields.last_mut() {
        Some(field) if !field.spilled => {
            if field.result && field.items < MAX_FIELD_ITEMS {
                let charge = vector_slot::<Unit>().saturating_mul(units(item.len()));
                if b.memory.hold(charge) {
                    field.items += 1;
                    field.charged = field.charged.saturating_add(charge);
                    field.buf.extend(item);
                }
            }
        }
        _ => push(story, b, item),
    }
}

/// Straight into the paragraph (markers, comment references, emitted items), locked inside a field.
fn push(story: &Story, b: &mut Builder<'_>, item: Vec<Unit>) {
    let inside = !story.fields.is_empty();
    b.units(item.into_iter().map(|unit| if inside { locked_unit(unit) } else { unit }));
}

fn object(loc: Loc) -> Vec<Unit> {
    vec![Unit { ch: OBJ, loc }]
}

/// An object made of one whole node, or locked inside something copied whole.
fn whole(node: Option<usize>, lock: Option<Lock>) -> Loc {
    match (lock, node) {
        (None, Some(node)) => Loc::Atomic { first: node, last: node },
        (Some(Lock(at)), _) => Loc::Locked { node: at, child: None },
        (None, None) => Loc::Locked { node: None, child: None },
    }
}

fn locked(node: Option<usize>, lock: Option<Lock>, child: Option<usize>) -> Loc {
    Loc::Locked { node: lock.map_or(node, |Lock(at)| at), child }
}

/// The reader's choice of an `mc:AlternateContent` branch (`children_of_choice`).
fn choice(alternate: &PEl) -> Option<&PEl> {
    alternate
        .els()
        .filter(|element| element.name == "mc:Choice")
        .find(|branch| choice_understood(branch.attr("Requires").unwrap_or("")))
        .or_else(|| alternate.child("mc:Fallback"))
        .or_else(|| alternate.child("mc:Choice"))
}

/// `w:tr` or `w:tc` children, through content controls and custom XML (the reader's `collect`).
fn collect<'a>(element: &'a PEl, name: &str, found: &mut Vec<&'a PEl>, depth: usize) {
    if depth > 8 {
        return;
    }
    for child in element.els() {
        if child.name == name {
            found.push(child);
        } else if child.name == "w:sdt" {
            if let Some(content) = child.child("w:sdtContent") {
                collect(content, name, found, depth + 1);
            }
        } else if child.name == "w:customXml" {
            collect(child, name, found, depth + 1);
        }
    }
}

impl Scanner<'_> {
    /// Whether the walk has stopped: a part refused for a limit, or the budget spent.
    fn stopped(&self) -> bool {
        self.failure.is_some() || self.memory.refused.get()
    }

    /// Walks one part, read under `name` as the reader reads it: its tree and paragraph numbers live
    /// for the walk, charged while they do. `Ok(false)`: the part is absent. A part that cannot be
    /// parsed gave the model no paragraphs, so it gives none here either; its error is the caller's.
    fn with_part(&mut self, name: &str, walk: impl FnOnce(&mut Self, &Ctx<'_>, &PEl)) -> Result<bool, DocxError> {
        // Nothing more is parsed once the scan has stopped.
        if self.stopped() {
            return Ok(true);
        }
        let pkg = self.pkg;
        let Some(key) = pkg.part_index(name).and_then(|index| pkg.part_name(index)) else { return Ok(false) };
        let Some(bytes) = pkg.get(key) else { return Ok(false) };
        // The parse charges the tree as it builds it, within what the budget has room for; what
        // the tree then holds stays charged while it is walked.
        let parsed = match pkg.tree(name, self.memory.room()) {
            Ok(Some(parsed)) => parsed,
            Ok(None) => return Ok(false),
            // Past the room the budget had: the scan is refused for memory, as any charge is.
            Err(DocxError::Limit(reason)) if reason == super::tree::TOO_MUCH_MEMORY => {
                self.memory.refused.set(true);
                return Ok(true);
            }
            Err(error) => return Err(error),
        };
        let root = parsed.root;
        let count = count_paragraphs(&root, 0);
        let walked = tree_bytes(&root).saturating_add(table_sized::<(usize, u32)>(count));
        if !self.memory.hold(parsed.charged.max(walked)) {
            return Ok(true);
        }
        self.memory.release(parsed.charged.max(walked) - walked);
        let ordinals = number_paragraphs(&root, count);
        let rels = pkg.rels(name);
        if self.wanted.contains_key(key)
            && !self.parts.contains_key(key)
            && let Some(written) = root.reusable_name()
            && self.memory.hold(units(bytes.len()).saturating_add(tree_slot::<String, ScannedPart>()).saturating_add(units(key.len())))
        {
            let prefix = written.split_once(':').map(|(prefix, _)| prefix.to_string()).unwrap_or_default();
            self.parts.insert(
                key.to_string(),
                ScannedPart { bytes: Arc::new(bytes.to_vec()), root: root.name.clone(), prefix, paragraphs: BTreeMap::new() },
            );
        }
        let ctx = Ctx { part: key, src: bytes, rels: &rels, ordinals: &ordinals };
        walk(self, &ctx, &root);
        drop(ordinals);
        drop(root);
        self.memory.release(walked);
        Ok(true)
    }

    /// The reader's `read_notes`.
    fn notes(&mut self, name: &str, foot: bool) -> Result<bool, DocxError> {
        self.with_part(name, |scanner, ctx, root| {
            let tag = if foot { "w:footnote" } else { "w:endnote" };
            for note in root.els().filter(|element| element.name == tag) {
                if note.attr("w:type").is_some_and(|kind| kind != "normal") {
                    continue;
                }
                let Some(id) = note.attr("w:id").and_then(int) else { continue };
                if scanner.parts_made >= MAX_PARTS || scanner.stopped() {
                    break;
                }
                scanner.story(ctx, note, 0);
                scanner.parts_made += 1;
                let notes = if foot { &mut scanner.footnotes } else { &mut scanner.endnotes };
                remember(scanner.memory, notes, id, 0);
            }
        })
    }

    /// The reader's `read_comments`.
    fn comments(&mut self, name: &str) -> Result<bool, DocxError> {
        self.with_part(name, |scanner, ctx, root| {
            for comment in root.els().filter(|element| element.name == "w:comment") {
                if scanner.parts_made >= MAX_PARTS || scanner.stopped() {
                    break;
                }
                let id = comment.attr("w:id").unwrap_or("").to_string();
                if scanner.comments.contains(&id) {
                    continue;
                }
                scanner.story(ctx, comment, 0);
                scanner.parts_made += 1;
                let extra = id.len();
                remember(scanner.memory, &mut scanner.comments, id, extra);
            }
        })
    }

    /// The main part's body and its last section; refused as the reader refuses it.
    fn main(&mut self, name: &str) -> Result<bool, DocxError> {
        let mut not_word = None;
        let found = self.with_part(name, |scanner, ctx, root| {
            if root.name != "w:document" {
                not_word = Some(root.name.clone());
                return;
            }
            let Some(body) = root.child("w:body") else { return };
            scanner.story(ctx, body, 0);
            if let Some(section) = body.child("w:sectPr") {
                scanner.section(ctx, section);
            }
        })?;
        match not_word {
            Some(root) => Err(DocxError::NotWord(format!("root element is {root}"))),
            None => Ok(found),
        }
    }

    /// The reader's `load_header_footer`: each part once, under its canonical name.
    fn header_footer(&mut self, target: &str) {
        let pkg = self.pkg;
        let name = pkg.part_index(target).and_then(|index| pkg.part_name(index)).unwrap_or(target).to_string();
        if self.headers.contains(&name) || self.parts_made >= MAX_PARTS || self.stopped() {
            return;
        }
        self.parts_made += 1;
        let extra = name.len();
        remember(self.memory, &mut self.headers, name.clone(), extra);
        // A header that is not well formed is an empty one to the reader; one past a limit stops
        // the scan.
        if let Err(error @ DocxError::Limit(_)) = self.with_part(&name, |scanner, ctx, root| scanner.story(ctx, root, 0)) {
            self.failure.get_or_insert(error);
        }
    }

    /// A story's blocks with fresh state, then its leftover markers (the reader's `read_blocks`
    /// and `flush_pending`). `depth`: text-box nesting.
    fn story(&mut self, ctx: &Ctx<'_>, container: &PEl, depth: usize) {
        let mut story = Story { depth, ..Story::default() };
        let mut out = Out::default();
        self.blocks(ctx, &mut story, container, &mut out, 0);
        self.flush(ctx, &mut story, &out);
        // Fields left open at the story's end give their buffers back.
        for field in story.fields.drain(..) {
            self.memory.release(field.charged);
        }
    }

    /// The reader's `read_section`: the headers and footers a section names.
    fn section(&mut self, ctx: &Ctx<'_>, section: &PEl) {
        for reference in section.els().filter(|element| element.name == "w:headerReference" || element.name == "w:footerReference") {
            let Some(rel) = reference.attr("r:id").and_then(|id| ctx.rels.by_id(id)) else { continue };
            if rel.external {
                continue;
            }
            let target = rel.target.clone();
            self.header_footer(&target);
        }
    }

    fn flush(&mut self, ctx: &Ctx<'_>, story: &mut Story, out: &Out) {
        let pending = std::mem::take(&mut story.pending);
        self.memory.release(vector_slot::<Unit>().saturating_mul(units(pending.len())));
        if pending.is_empty() {
            return;
        }
        let Some(Last::Para(Some(ordinal))) = out.last else { return };
        let memory = self.memory;
        if let Some(paragraph) = self.parts.get_mut(ctx.part).and_then(|part| part.paragraphs.get_mut(&ordinal)) {
            // A kept paragraph is charged for its room: the room these need, before it is made.
            let room = paragraph.units.capacity();
            let needed = paragraph.units.len().saturating_add(pending.len());
            if !memory.hold(bytes_of_size::<Unit>(needed.saturating_sub(room))) {
                return;
            }
            paragraph.units.reserve_exact(pending.len());
            memory.hold(bytes_of_size::<Unit>(paragraph.units.capacity().saturating_sub(room.max(needed))));
            paragraph.units.extend(pending.into_iter().map(|unit| Unit { ch: unit.ch, loc: Loc::Outside { at_end: true } }));
        }
    }

    /// The reader's `read_blocks` (drop caps aside: a merged paragraph has no origin, so none of its
    /// paragraphs is wanted).
    fn blocks(&mut self, ctx: &Ctx<'_>, story: &mut Story, parent: &PEl, out: &mut Out, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        for element in parent.els() {
            if self.stopped() {
                return;
            }
            match element.name.as_str() {
                "w:p" => {
                    let ordinal = self.paragraph(ctx, story, element, depth);
                    out.last = Some(Last::Para(ordinal));
                }
                "w:tbl" => {
                    if depth / 4 >= MAX_TABLE_DEPTH {
                        out.last = Some(Last::Para(None));
                    } else {
                        self.table(ctx, story, element, out, depth);
                    }
                }
                "w:sdt" => {
                    if let Some(content) = element.child("w:sdtContent") {
                        self.blocks(ctx, story, content, out, depth + 1);
                    }
                }
                "w:customXml" | "w:smartTag" | "w:ins" | "w:moveTo" => self.blocks(ctx, story, element, out, depth + 1),
                "mc:AlternateContent" => {
                    if let Some(branch) = choice(element) {
                        self.blocks(ctx, story, branch, out, depth + 1);
                    }
                }
                // The guard registers what the marker opens or closes, as the reader's `marker` does,
                // then charges the unit waiting for the next paragraph.
                "w:bookmarkStart" | "w:bookmarkEnd" | "w:commentRangeStart" | "w:commentRangeEnd"
                    if self.marker(element) && self.memory.hold(vector_slot::<Unit>()) =>
                {
                    story.pending.push(Unit { ch: OBJ, loc: Loc::Outside { at_end: false } });
                }
                _ => {}
            }
        }
    }

    /// The reader's `read_table`.
    fn table(&mut self, ctx: &Ctx<'_>, story: &mut Story, table: &PEl, out: &mut Out, depth: usize) {
        let mut rows = Vec::new();
        collect(table, "w:tr", &mut rows, 0);
        let mut any_row = false;
        for row in rows.into_iter().take(MAX_ROWS) {
            let mut cells = Vec::new();
            collect(row, "w:tc", &mut cells, 0);
            let mut read = 0usize;
            for cell in cells.into_iter().take(MAX_COLS) {
                // A legacy horizontal merge (the last `w:hMerge` says) continues the cell before it:
                // the reader reads nothing of it.
                let continues = cell
                    .child("w:tcPr")
                    .and_then(|props| props.els().filter(|element| element.name == "w:hMerge").last())
                    .is_some_and(|merge| merge.attr("w:val") != Some("restart"));
                if continues && read > 0 {
                    continue;
                }
                let mut cell_out = Out::default();
                self.blocks(ctx, story, cell, &mut cell_out, depth + 4);
                self.flush(ctx, story, &cell_out);
                read += 1;
            }
            any_row |= read > 0;
        }
        if any_row {
            out.last = Some(Last::Table);
        }
    }

    /// Whether the reader makes an object of this marker, registering what it opens or closes.
    fn marker(&mut self, element: &PEl) -> bool {
        let id = element.attr("w:id").unwrap_or("").to_string();
        let extra = id.len();
        match element.name.as_str() {
            "w:bookmarkStart" => {
                let name = element.attr("w:name").unwrap_or("");
                if name.is_empty() || name.len() > 256 {
                    return false;
                }
                remember(self.memory, &mut self.bookmarks, id, extra);
                true
            }
            "w:bookmarkEnd" => self.bookmarks.contains(&id),
            "w:commentRangeStart" => self.comments.contains(&id),
            "w:commentRangeEnd" => {
                if !self.comments.contains(&id) {
                    return false;
                }
                remember(self.memory, &mut self.comments_ended, id, extra);
                true
            }
            _ => false,
        }
    }

    /// The reader's `read_para`; answers the paragraph's position in the part.
    fn paragraph(&mut self, ctx: &Ctx<'_>, story: &mut Story, paragraph: &PEl, depth: usize) -> Option<u32> {
        let ordinal = ctx.ordinals.get(&address(paragraph)).copied();
        let mut b = Builder::new(self.memory);
        let pending = std::mem::take(&mut story.pending);
        self.memory.release(vector_slot::<Unit>().saturating_mul(units(pending.len())));
        b.units(pending);
        let props = paragraph.child("w:pPr");
        if let Some(section) = props.and_then(|props| props.child("w:sectPr")) {
            self.section(ctx, section);
        }
        for element in paragraph.els() {
            // The paragraph's properties are recorded apart; a second `w:pPr` is content like any other.
            if !props.is_some_and(|props| std::ptr::eq(props, element)) {
                self.inline(ctx, story, &mut b, element, None, depth + 1, None);
            }
        }
        // The reader's `spill_fields`: an open field cannot keep buffering past the paragraph; the
        // rest of its result flows into the paragraph, still inside the field.
        for field in story.fields.iter_mut() {
            if field.spilled {
                continue;
            }
            field.spilled = true;
            let buffered = std::mem::take(&mut field.buf);
            self.memory.release(std::mem::take(&mut field.charged));
            b.units(
                std::iter::once(Unit { ch: OBJ, loc: Loc::Locked { node: field.first, child: None } }).chain(buffered.into_iter().map(locked_unit)),
            );
        }
        // 6C-39: a text box's paragraphs are read-only.
        if story.depth > 0 {
            let units = std::mem::take(&mut b.units);
            b.units = units.into_iter().map(locked_unit).collect();
        }
        let wanted = ordinal.is_some_and(|ordinal| self.wanted.get(ctx.part).is_some_and(|positions| positions.contains(&ordinal)));
        let mut keep = None;
        if let (true, Some(ordinal)) = (wanted, ordinal) {
            let ppr = props.map(|props| b.props(props));
            let raw_name = b.name(paragraph);
            if b.reusable && !self.memory.refused.get() && self.parts.contains_key(ctx.part) {
                keep = Some((ordinal, raw_name, ppr));
            }
        }
        match keep {
            Some((ordinal, raw_name, ppr)) => {
                let (memory, held) = (b.memory, b.held);
                let scanned = finish(paragraph, raw_name, ppr, b);
                // What the kept paragraph holds replaces what growing it was charged.
                memory.release(held);
                if memory.hold(paragraph_bytes(&scanned))
                    && let Some(part) = self.parts.get_mut(ctx.part)
                {
                    part.paragraphs.insert(ordinal, scanned);
                }
            }
            None => self.memory.release(b.held),
        }
        ordinal
    }

    /// A wrapper around inline content (inside `parent`); nothing is recorded inside something
    /// copied whole. Answers the innermost wrapper for its content.
    fn wrap(&mut self, b: &mut Builder<'_>, element: &PEl, parent: Option<usize>, lock: Option<Lock>) -> Option<usize> {
        if lock.is_some() {
            return parent;
        }
        let kind = match element.name.as_str() {
            "w:ins" | "w:moveTo" => WrapperKind::Inserted,
            "w:del" | "w:moveFrom" => WrapperKind::Deleted,
            _ => WrapperKind::Other,
        };
        b.wrapper(Wrapper { kind, start: element.start.clone(), end: element.end.clone(), parent }).or(parent)
    }

    /// The reader's `read_inline`, for one child of a paragraph or of inline content at `depth`,
    /// inside the wrapper `wrapper`.
    #[allow(clippy::too_many_arguments)]
    fn inline(
        &mut self,
        ctx: &Ctx<'_>,
        story: &mut Story,
        b: &mut Builder<'_>,
        element: &PEl,
        wrapper: Option<usize>,
        depth: usize,
        lock: Option<Lock>,
    ) {
        if depth > MAX_DEPTH || self.stopped() {
            return;
        }
        let node_of = |b: &mut Builder<'_>| if lock.is_none() { b.other(element, wrapper) } else { None };
        match element.name.as_str() {
            "w:r" => {
                let node = if lock.is_none() { b.run(element, wrapper) } else { None };
                let mut note = None;
                for child in element.els() {
                    self.run_child(ctx, story, b, child, node, lock, &mut note, depth + 1);
                }
                if let Some(waiting) = note.take() {
                    note_object(story, b, node, lock, waiting);
                }
            }
            "w:hyperlink" | "w:ins" | "w:moveTo" | "w:del" | "w:moveFrom" | "w:smartTag" | "w:customXml" | "w:dir" | "w:bdo" => {
                let inner = self.wrap(b, element, wrapper, lock);
                for child in element.els() {
                    self.inline(ctx, story, b, child, inner, depth + 1, lock);
                }
            }
            "w:sdt" => {
                // The reader reads the first `w:sdtContent` only; the rest is copied whole.
                let content = element.child("w:sdtContent");
                let outer = self.wrap(b, element, wrapper, lock);
                for child in element.els() {
                    if content.is_some_and(|content| std::ptr::eq(content, child)) {
                        let inner = self.wrap(b, child, outer, lock);
                        for inside in child.els() {
                            self.inline(ctx, story, b, inside, inner, depth + 1, lock);
                        }
                    } else if lock.is_none() {
                        b.other(child, outer);
                    }
                }
            }
            "w:fldSimple" => {
                let node = node_of(b);
                let inner = Some(lock.unwrap_or(Lock(node)));
                self.begin_field(story, node);
                // As the reader: the innermost open field's result begins (an ignored begin leaves
                // the field around it in its result).
                if let Some(field) = story.fields.last_mut() {
                    field.result = true;
                }
                for child in element.els() {
                    self.inline(ctx, story, b, child, wrapper, depth + 1, inner);
                }
                self.end_field(story, b, None, inner);
            }
            "w:bookmarkStart" | "w:bookmarkEnd" | "w:commentRangeStart" | "w:commentRangeEnd" => {
                let node = node_of(b);
                if self.marker(element) {
                    let loc = match (lock, node) {
                        (None, Some(node)) => Loc::Marker { node },
                        _ => locked(node, lock, None),
                    };
                    // Markers go straight into the paragraph, never into a field's result.
                    push(story, b, object(loc));
                }
            }
            "m:oMath" => {
                let node = node_of(b);
                b.plain_equation(node, element);
                emit(story, b, object(whole(node, lock)));
            }
            "m:oMathPara" => {
                let node = node_of(b);
                b.plain_equation(node, element);
                for _ in element.els().filter(|child| child.name == "m:oMath") {
                    emit(story, b, object(whole(node, lock)));
                }
            }
            "mc:AlternateContent" => {
                let node = node_of(b);
                if let Some(branch) = choice(element) {
                    let inner = Some(lock.unwrap_or(Lock(node)));
                    for child in branch.els() {
                        self.inline(ctx, story, b, child, wrapper, depth + 1, inner);
                    }
                }
            }
            _ => {
                node_of(b);
            }
        }
    }

    /// The reader's `read_run_child`. `note`: a note reference waiting for its object.
    #[allow(clippy::too_many_arguments)]
    fn run_child(
        &mut self,
        ctx: &Ctx<'_>,
        story: &mut Story,
        b: &mut Builder<'_>,
        element: &PEl,
        node: Option<usize>,
        lock: Option<Lock>,
        note: &mut Option<Note>,
        depth: usize,
    ) {
        if depth > MAX_DEPTH || self.stopped() {
            return;
        }
        let other = |b: &mut Builder<'_>| if lock.is_some() { None } else { b.other_child(node, element) };
        // The text right after a custom note mark is the mark itself.
        if note.is_some_and(|waiting| waiting.custom) && element.name == "w:t" {
            other(b);
            return;
        }
        if element.name != "w:rPr"
            && let Some(waiting) = note.take()
        {
            note_object(story, b, node, lock, waiting);
        }
        match element.name.as_str() {
            "w:rPr" => {
                // The reader formats the run by its first properties; a later `w:rPr` is copied whole.
                let first = lock.is_none() && matches!(node.and_then(|node| b.nodes.get(node)), Some(Node::Run(run)) if run.rpr.is_none());
                if first {
                    if let Some(Node::Run(run)) = node.and_then(|node| b.nodes.get_mut(node)) {
                        run.rpr = Some(element.span());
                        run.rpr_change = element.child("w:rPrChange").map(PEl::span);
                    }
                } else {
                    other(b);
                }
            }
            "w:t" | "w:delText" => self.text(ctx, story, b, element, node, lock),
            "w:tab" | "w:ptab" => self.single(story, b, element, node, lock, '\t'),
            "w:br" => {
                let ch = match element.attr("w:type") {
                    Some("page") => '\u{000C}',
                    Some("column") => '\u{000E}',
                    _ => '\n',
                };
                self.single(story, b, element, node, lock, ch);
            }
            "w:cr" => self.single(story, b, element, node, lock, '\n'),
            "w:noBreakHyphen" => self.single(story, b, element, node, lock, '\u{2011}'),
            "w:softHyphen" => self.single(story, b, element, node, lock, '\u{00AD}'),
            "w:sym" => {
                let ch = element
                    .attr("w:char")
                    .and_then(|hex| u32::from_str_radix(hex.trim(), 16).ok())
                    .and_then(char::from_u32)
                    .filter(|c| *c as u32 >= 0x20);
                match ch {
                    Some(ch) => self.single(story, b, element, node, lock, ch),
                    None => {
                        other(b);
                    }
                }
            }
            "w:fldChar" => {
                other(b);
                match element.attr("w:fldCharType") {
                    Some("begin") => self.begin_field(story, lock.map_or(node, |Lock(at)| at)),
                    Some("separate") => {
                        if story.ignored_begins == 0
                            && let Some(field) = story.fields.last_mut()
                        {
                            field.result = true;
                        }
                    }
                    Some("end") => self.end_field(story, b, node, lock),
                    _ => {}
                }
            }
            "w:drawing" | "w:pict" | "w:object" => {
                let child = other(b);
                let made = if element.name == "w:drawing" { self.drawing(ctx, story.depth, element) } else { self.vml(ctx, story.depth, element) };
                if made {
                    let loc = whole(node, lock);
                    if let (Loc::Atomic { first, .. }, Some(child)) = (&loc, child) {
                        b.makers.insert(*first, child);
                    }
                    emit(story, b, object(loc));
                }
            }
            "w:footnoteReference" | "w:endnoteReference" => {
                let child = other(b);
                let notes = if element.name == "w:footnoteReference" { &self.footnotes } else { &self.endnotes };
                if notes.contains(&element.attr("w:id").and_then(int).unwrap_or(i64::MIN)) {
                    let custom = element.attr("w:customMarkFollows").is_some_and(|value| !matches!(value, "0" | "false" | "off"));
                    *note = Some(Note { custom, locked: lock.is_some(), child });
                }
            }
            "w:commentReference" => {
                let child = other(b);
                // As the reader: a reference without an id names no comment.
                if let Some(id) = element.attr("w:id")
                    && self.comments.contains(id)
                    && !self.comments_ended.contains(id)
                {
                    remember(self.memory, &mut self.comments_ended, id.to_string(), id.len());
                    push(story, b, object(locked(node, lock, child)));
                }
            }
            "w:pgNum" => {
                let child = other(b);
                emit(story, b, object(locked(node, lock, child)));
            }
            "w:ruby" => {
                other(b);
                let inner = Some(lock.unwrap_or(Lock(node)));
                if let Some(base) = element.child("w:rubyBase") {
                    for run in base.els().filter(|child| child.name == "w:r") {
                        for child in run.els() {
                            self.run_child(ctx, story, b, child, node, inner, note, depth + 1);
                        }
                    }
                }
            }
            "mc:AlternateContent" => {
                other(b);
                if let Some(branch) = choice(element) {
                    let inner = Some(lock.unwrap_or(Lock(node)));
                    for child in branch.els() {
                        self.run_child(ctx, story, b, child, node, inner, note, depth + 1);
                    }
                }
            }
            _ => {
                other(b);
            }
        }
    }

    fn single(&mut self, story: &mut Story, b: &mut Builder<'_>, element: &PEl, node: Option<usize>, lock: Option<Lock>, ch: char) {
        let child = if lock.is_none() { b.child(node, Child { span: element.span(), kind: ChildKind::Single { unit: None } }) } else { None };
        let loc = match (lock, node, child) {
            (None, Some(node), Some(child)) => Loc::Single { node, child },
            _ => locked(node, lock, None),
        };
        emit(story, b, vec![Unit { ch, loc }]);
    }

    /// A `w:t` or `w:delText`: its direct text as the reader reads it (`El::text`: text and
    /// references, unknown references left out, CDATA as written), one item. A child holding CDATA
    /// or elements is copied whole: its characters cannot be placed one by one.
    fn text(&mut self, ctx: &Ctx<'_>, story: &mut Story, b: &mut Builder<'_>, element: &PEl, node: Option<usize>, lock: Option<Lock>) {
        let piece = |range: &Range<usize>| ctx.src.get(range.clone()).unwrap_or_default();
        let cdata = |range: &Range<usize>| {
            String::from_utf8_lossy(ctx.src.get(range.start.saturating_add(9)..range.end.saturating_sub(3)).unwrap_or_default()).into_owned()
        };
        // How many characters, to charge before anything is kept: the tree refuses a part whose text
        // is not UTF-8, as the reader does.
        let count = element.kids.iter().fold(0usize, |count, kid| {
            count.saturating_add(match kid {
                PKid::Text(range) => std::str::from_utf8(piece(range)).map_or(0, |text| text.chars().count()),
                PKid::Ref { .. } => 1,
                PKid::CData(range) => cdata(range).chars().count(),
                PKid::El(_) => 0,
            })
        });
        // The characters while they are placed, the ones kept, and the item's units.
        let transient = bytes_of_size::<(Range<usize>, Option<char>)>(count).saturating_add(bytes_of_size::<Unit>(count));
        if !b.memory.hold(transient) {
            return;
        }
        let mut raw: Vec<(Range<usize>, Option<char>)> = Vec::with_capacity(count);
        let mut whole_only = false;
        for kid in &element.kids {
            match kid {
                PKid::Text(range) => {
                    let text = std::str::from_utf8(piece(range)).unwrap_or("");
                    for (offset, ch) in text.char_indices() {
                        let start = range.start.saturating_add(offset);
                        raw.push((start..start.saturating_add(ch.len_utf8()), Some(ch)));
                    }
                }
                PKid::Ref { range, ch } => raw.push((range.clone(), *ch)),
                PKid::CData(range) => {
                    whole_only = true;
                    raw.extend(cdata(range).chars().map(|ch| (range.clone(), Some(ch))));
                }
                PKid::El(_) => whole_only = true,
            }
        }
        let child = if lock.is_some() {
            None
        } else if whole_only {
            b.other_child(node, element)
        } else if b.hold(bytes_of_size::<RawChar>(raw.len())) {
            let mut chars = Vec::with_capacity(raw.len());
            chars.extend(raw.iter().map(|(bytes, _)| RawChar { bytes: bytes.clone(), unit: None }));
            let raw_name = b.name(element);
            let kind =
                ChildKind::Text { start: element.start.clone(), end: element.end.clone(), raw_name, deleted: element.local() == "delText", chars };
            b.child(node, Child { span: element.span(), kind })
        } else {
            None
        };
        let mut item = Vec::with_capacity(count);
        for (index, (_, ch)) in raw.iter().enumerate() {
            let Some(cleaned) = ch.and_then(clean_char) else { continue };
            let loc = match (lock, node, child, whole_only) {
                (None, Some(node), Some(child), false) => Loc::Text { node, child, raw: index },
                _ => locked(node, lock, child),
            };
            item.push(Unit { ch: cleaned, loc });
        }
        drop(raw);
        emit(story, b, item);
        b.memory.release(transient);
    }

    fn begin_field(&mut self, story: &mut Story, first: Option<usize>) {
        if story.fields.len() >= MAX_FIELD_DEPTH || story.ignored_begins > 0 {
            story.ignored_begins += 1;
            return;
        }
        story.fields.push(Field { result: false, spilled: false, buf: Vec::new(), items: 0, charged: 0, first });
    }

    fn end_field(&mut self, story: &mut Story, b: &mut Builder<'_>, last: Option<usize>, lock: Option<Lock>) {
        if story.ignored_begins > 0 {
            story.ignored_begins -= 1;
            return;
        }
        let Some(field) = story.fields.pop() else { return };
        self.memory.release(field.charged);
        if field.spilled {
            return;
        }
        // Inside another field's instruction: it becomes part of that text, not an object.
        if story.fields.last().is_some_and(|parent| !parent.spilled && !parent.result) {
            return;
        }
        let loc = match (lock, field.first, last) {
            (None, Some(first), Some(last)) if first <= last => {
                b.fields.insert((first, last));
                Loc::Atomic { first, last }
            }
            (Some(Lock(at)), _, _) => Loc::Locked { node: at, child: None },
            (None, first, last) => Loc::Locked { node: last.or(first), child: None },
        };
        emit(story, b, object(loc));
    }

    fn media_exists(&self, ctx: &Ctx<'_>, id: &str) -> bool {
        ctx.rels.by_id(id).is_some_and(|rel| !rel.external && self.pkg.get(&rel.target).is_some())
    }

    /// The reader's `read_drawing`: whether it makes an object (reading a text box inside as a
    /// story one level deeper, as `read_textbox` does).
    fn drawing(&mut self, ctx: &Ctx<'_>, depth: usize, drawing: &PEl) -> bool {
        let Some(frame) = drawing.child("wp:inline").or_else(|| drawing.child("wp:anchor")) else { return false };
        let Some(data) = frame.child("a:graphic").and_then(|graphic| graphic.child("a:graphicData")) else { return false };
        if let Some(blip) = data.find("a:blip") {
            return blip.attr("r:embed").is_some_and(|id| self.media_exists(ctx, id));
        }
        let Some(shape) = data.find("wps:wsp") else { return false };
        if let Some(content) = shape.child("wps:txbx").and_then(|text_box| text_box.child("w:txbxContent"))
            && depth < MAX_STORY_DEPTH
        {
            self.text_box(ctx, content, depth);
        }
        true
    }

    /// The reader's `read_vml`.
    fn vml(&mut self, ctx: &Ctx<'_>, depth: usize, pict: &PEl) -> bool {
        let Some(shape) = pict.els().find(|element| element.name.starts_with("v:") && element.local() != "shapetype") else { return false };
        if let Some(image) = shape.find("v:imagedata") {
            return image.attr("r:id").or_else(|| image.attr("r:pict")).is_some_and(|id| self.media_exists(ctx, id));
        }
        if let Some(content) = shape.find("w:txbxContent")
            && depth < MAX_STORY_DEPTH
        {
            self.text_box(ctx, content, depth);
            return true;
        }
        false
    }

    /// The reader's `read_textbox`: a story one level deeper, then the part it makes.
    fn text_box(&mut self, ctx: &Ctx<'_>, content: &PEl, depth: usize) {
        self.story(ctx, content, depth + 1);
        self.parts_made += 1;
    }
}

/// The object of a note reference: the run, unless the reference or the object stands inside
/// something copied whole.
fn note_object(story: &mut Story, b: &mut Builder<'_>, node: Option<usize>, lock: Option<Lock>, note: Note) {
    let loc = match lock {
        None if note.locked => Loc::Locked { node, child: None },
        lock => whole(node, lock),
    };
    if let (Loc::Atomic { first, .. }, Some(child)) = (&loc, note.child) {
        b.makers.insert(*first, child);
    }
    emit(story, b, object(loc));
}

/// The scanned paragraph, each character and one-character child knowing its unit, every vector
/// holding no more room than it needs.
fn finish(paragraph: &PEl, raw_name: String, ppr: Option<Props>, mut b: Builder<'_>) -> ScannedPara {
    settle_objects(&mut b);
    for (index, unit) in b.units.iter().enumerate() {
        match unit.loc {
            Loc::Text { node, child, raw } => {
                if let Some(Node::Run(run)) = b.nodes.get_mut(node)
                    && let Some(Child { kind: ChildKind::Text { chars, .. }, .. }) = run.children.get_mut(child)
                    && let Some(slot) = chars.get_mut(raw)
                {
                    slot.unit = Some(index);
                }
            }
            Loc::Single { node, child } => {
                if let Some(Node::Run(run)) = b.nodes.get_mut(node)
                    && let Some(Child { kind: ChildKind::Single { unit }, .. }) = run.children.get_mut(child)
                {
                    *unit = Some(index);
                }
            }
            _ => {}
        }
    }
    for node in &mut b.nodes {
        if let Node::Run(run) = node {
            run.children.shrink_to_fit();
        }
    }
    b.nodes.shrink_to_fit();
    b.wrappers.shrink_to_fit();
    b.units.shrink_to_fit();
    ScannedPara {
        start: paragraph.start.clone(),
        end: paragraph.end.clone(),
        raw_name,
        ppr,
        nodes: std::mem::take(&mut b.nodes),
        wrappers: std::mem::take(&mut b.wrappers),
        units: std::mem::take(&mut b.units),
    }
}

/// What an object is made of, to tell what its nodes may hold.
enum Made {
    /// A field from its first run to its last.
    Field,
    /// A picture or note reference: the child that made it.
    By(usize),
    /// An equation: its own element.
    Equation,
}

/// An object stays `Atomic` only when removing its nodes removes it and nothing else: no other unit
/// stands in them and no other object covers them, they sit under one wrapper, and they hold only
/// what the object owns (I1): each node a run, or the equation's own element, and each run child one
/// the object owns. Otherwise it is locked where it begins.
fn settle_objects(b: &mut Builder<'_>) {
    let count = b.nodes.len();
    // How many units stand in, or objects cover, each node.
    let mut cover = vec![0i64; count + 1];
    for unit in &b.units {
        let (first, last) = match unit.loc {
            Loc::Text { node, .. } | Loc::Single { node, .. } | Loc::Marker { node } | Loc::Locked { node: Some(node), .. } => (node, node),
            Loc::Atomic { first, last } => (first, last),
            Loc::Locked { node: None, .. } | Loc::Outside { .. } => continue,
        };
        if first > last || last >= count {
            continue;
        }
        if let Some(slot) = cover.get_mut(first) {
            *slot += 1;
        }
        if let Some(slot) = cover.get_mut(last + 1) {
            *slot -= 1;
        }
    }
    // `prefix[n]`: how many stand in, or cover, the nodes before `n`.
    let (mut running, mut total) = (0i64, 0i64);
    let mut prefix = Vec::with_capacity(count + 1);
    prefix.push(0i64);
    for node in 0..count {
        running += cover.get(node).copied().unwrap_or(0);
        total += running;
        prefix.push(total);
    }
    let (nodes, makers, fields, owned, equations) = (&b.nodes, &b.makers, &b.fields, &b.owned, &b.equations);
    // The children of node `at` an object owns, given which it may own whole.
    let holds_only = |at: usize, run: &RunNode, may: &dyn Fn(usize, Option<Owned>) -> bool| {
        run.children.iter().enumerate().all(|(index, child)| match child.kind {
            ChildKind::Text { .. } | ChildKind::Single { .. } => may(index, Some(Owned::Text)),
            ChildKind::Other { .. } => may(index, owned.get(&(at, index)).copied()),
        })
    };
    for unit in &mut b.units {
        let Loc::Atomic { first, last } = unit.loc else { continue };
        let alone = first <= last
            && last < count
            && prefix.get(last + 1).copied().unwrap_or(0) - prefix.get(first).copied().unwrap_or(0)
                == i64::try_from(last - first + 1).unwrap_or(i64::MAX);
        let span = nodes.get(first..=last).unwrap_or_default();
        let one_wrapper = span.iter().all(|node| node.wrapper() == span.first().and_then(Node::wrapper));
        // A field and a picture both standing alone in one node cannot be told apart: neither is whole.
        let made = match (fields.contains(&(first, last)), (first == last).then(|| makers.get(&first)).flatten()) {
            (true, None) => Some(Made::Field),
            (false, Some(child)) => Some(Made::By(*child)),
            (false, None) if first == last && equations.contains(&first) => Some(Made::Equation),
            _ => None,
        };
        // A field owns its field characters, instructions, result text and characters and page
        // breaks; a picture or note owns the child that made it, a custom mark and page breaks.
        let owns = |at: usize, node: &Node| match (&made, node) {
            (Some(Made::Field), Node::Run(run)) => holds_only(at, run, &|_, kind| kind.is_some()),
            (Some(Made::By(maker)), Node::Run(run)) => holds_only(at, run, &|index, kind| {
                index == *maker || (matches!(kind, Some(Owned::Text | Owned::Break)) && !is_unit_child(run, index))
            }),
            (Some(Made::Equation), Node::Other { .. }) => true,
            _ => false,
        };
        let only_owned = span.iter().enumerate().all(|(offset, node)| owns(first + offset, node));
        if !(alone && one_wrapper && only_owned) {
            unit.loc = Loc::Locked { node: Some(first), child: None };
        }
    }
}

/// Whether a run's child is one the reader makes a character of (`w:t` placed character by
/// character, or a one-character child): a picture's run holding one is not the picture's alone.
fn is_unit_child(run: &RunNode, index: usize) -> bool {
    matches!(run.children.get(index).map(|child| &child.kind), Some(ChildKind::Text { .. } | ChildKind::Single { .. }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

    use std::collections::{BTreeMap, BTreeSet};

    use proptest::prelude::*;

    use super::*;
    use crate::package::{ReadLimits, zip_entries};

    const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math""#;
    const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    /// A package: the body, its relationships (id, type suffix, target) and other parts.
    fn package(body: &str, rels: &[(&str, &str, &str)], parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
        package_of(format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#).into_bytes(), rels, parts)
    }

    /// A package whose main part is `document`, as given.
    fn package_of(document: Vec<u8>, rels: &[(&str, &str, &str)], parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let rels: String =
            rels.iter().map(|(id, kind, target)| format!(r#"<Relationship Id="{id}" Type="{REL}/{kind}" Target="{target}"/>"#)).collect();
        let mut entries = vec![
            ("_rels/.rels".to_string(), format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="word/document.xml"/></Relationships>"#).into_bytes()),
            ("word/_rels/document.xml.rels".to_string(), format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#).into_bytes()),
            ("word/document.xml".to_string(), document),
        ];
        entries.extend(parts.iter().map(|(name, bytes)| (name.to_string(), bytes.clone())));
        zip_entries(&entries).unwrap()
    }

    fn budget() -> Budget {
        Budget::new(crate::inspect::MAX_WORK, crate::inspect::MAX_MEMORY)
    }

    /// The prefix the part's root is written with.
    fn written_prefix(bytes: &[u8]) -> String {
        let text = String::from_utf8_lossy(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)).into_owned();
        let root = text.split('<').find(|tag| !tag.is_empty() && !tag.starts_with('?') && !tag.starts_with('!')).unwrap_or_default();
        let name = root.split(|c: char| c.is_whitespace() || c == '>' || c == '/').next().unwrap_or_default();
        name.split_once(':').map(|(prefix, _)| prefix.to_string()).unwrap_or_default()
    }

    /// The paragraphs the reader made with an origin: (part, position) → text.
    fn read_paragraphs(bytes: &[u8]) -> BTreeMap<(String, u32), String> {
        // Every paragraph of every story, tables descended as deep as the reader built them.
        fn walk<'d>(blocks: &'d wordcraft_doc::Blocks, out: &mut Vec<&'d wordcraft_doc::Paragraph>) {
            for block in blocks {
                match &**block {
                    wordcraft_doc::Block::Para(paragraph) => out.push(paragraph),
                    wordcraft_doc::Block::Table(table) => table.rows.iter().flat_map(|row| &row.cells).for_each(|cell| walk(&cell.blocks, out)),
                }
            }
        }
        let doc = crate::read(bytes).unwrap();
        let pkg = Package::open_with(bytes, &ReadLimits::default()).unwrap();
        let mut paragraphs = Vec::new();
        walk(&doc.body, &mut paragraphs);
        for part in doc.parts.values() {
            walk(&part.blocks, &mut paragraphs);
        }
        let mut read = BTreeMap::new();
        for paragraph in paragraphs {
            let Some(origin) = paragraph.origin else { continue };
            let part = pkg.part_name(origin.part).unwrap().to_string();
            let earlier = read.insert((part.clone(), origin.index), paragraph.text.clone());
            assert!(earlier.is_none(), "{part} #{} read twice", origin.index);
        }
        read
    }

    /// Every paragraph the reader made is scanned, and its characters are the model's text.
    fn scans_as_read(bytes: &[u8]) -> BTreeMap<String, ScannedPart> {
        let expected = read_paragraphs(bytes);
        assert!(!expected.is_empty());
        let pkg = Package::open_with(bytes, &ReadLimits::default()).unwrap();
        let mut wanted: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
        for (part, index) in expected.keys() {
            wanted.entry(part.clone()).or_default().insert(*index);
        }
        let mut budget = budget();
        let scanned = scan_package(&pkg, &wanted, &mut budget).unwrap();
        // Everything held only while walking was given back: what stays charged is what is kept
        // (paragraphs and part copies) and the ids remembered.
        let kept = scanned.iter().fold(0u64, |sum, (name, part)| {
            sum + units(part.bytes.len())
                + tree_slot::<String, ScannedPart>()
                + units(name.len())
                + part.paragraphs.values().map(paragraph_bytes).sum::<u64>()
        });
        // Each id remembered is at most one table entry and its text: bounded by the ids written.
        let ids: usize = (0..)
            .map_while(|index| pkg.part_name(index))
            .filter_map(|name| pkg.get(name))
            .map(|part| String::from_utf8_lossy(part).matches("id=\"").count())
            .sum();
        let remembered = 128 * units(ids) + (4 << 10);
        assert!(budget.held() >= kept && budget.held() - kept <= remembered, "held {} kept {kept} ids {ids}", budget.held());
        for part in scanned.values() {
            assert!(["w:document", "w:hdr", "w:ftr", "w:footnotes", "w:endnotes", "w:comments"].contains(&part.root.as_str()));
            assert_eq!(part.prefix, written_prefix(&part.bytes));
        }
        for ((part, index), text) in &expected {
            let paragraph = scanned.get(part).and_then(|p| p.paragraphs.get(index)).unwrap_or_else(|| panic!("{part} #{index} was not scanned"));
            assert_eq!(&paragraph.text(), text, "{part} #{index}");
            check_locations(&scanned[part].bytes, paragraph);
        }
        // Nothing but what was asked for.
        let scanned_keys: BTreeSet<(String, u32)> =
            scanned.iter().flat_map(|(part, scanned)| scanned.paragraphs.keys().map(|index| (part.clone(), *index))).collect();
        assert_eq!(scanned_keys, expected.keys().cloned().collect());
        scanned
    }

    /// The character a `&…;` reference or a literal stands for.
    fn decode(bytes: &[u8]) -> Option<char> {
        let text = std::str::from_utf8(bytes).ok()?;
        let Some(name) = text.strip_prefix('&').and_then(|rest| rest.strip_suffix(';')) else { return text.chars().next() };
        match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => {
                let number = name.strip_prefix('#')?;
                let value = match number.strip_prefix('x').or_else(|| number.strip_prefix('X')) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => number.parse().ok()?,
                };
                char::from_u32(value)
            }
        }
    }

    /// The name a tag is written with (`<x:p …>`, `</x:p>`, `<x:p/>`).
    fn tag_name(bytes: &[u8], range: &Range<usize>) -> String {
        let tag = String::from_utf8_lossy(&bytes[range.clone()]).into_owned();
        let tag = tag.trim_start_matches('<').trim_start_matches('/');
        tag.split(|c: char| c.is_whitespace() || c == '>' || c == '/').next().unwrap_or_default().to_string()
    }

    /// `range` is a start tag (or an element written empty) written with `name`.
    fn is_start(bytes: &[u8], range: &Range<usize>, name: &str) -> bool {
        let tag = &bytes[range.clone()];
        tag.starts_with(b"<") && !tag.starts_with(b"</") && tag.ends_with(b">") && tag_name(bytes, range) == name
    }

    /// `end` closes the element `start` opens: an end tag of the same name, or none when the start
    /// tag is written empty.
    fn closes(bytes: &[u8], start: &Range<usize>, end: &Option<Range<usize>>) -> bool {
        match end {
            None => bytes[start.clone()].ends_with(b"/>"),
            Some(end) => {
                let tag = &bytes[end.clone()];
                !bytes[start.clone()].ends_with(b"/>")
                    && tag.starts_with(b"</")
                    && tag.ends_with(b">")
                    && tag_name(bytes, end) == tag_name(bytes, start)
            }
        }
    }

    /// Where the tag starting at `from` ends (just past its `>`), reading quoted values whole.
    fn tag_end(bytes: &[u8], from: usize) -> usize {
        let mut quote = None;
        for (offset, byte) in bytes[from..].iter().enumerate() {
            match (quote, *byte) {
                (Some(open), byte) if byte == open => quote = None,
                (None, b'"' | b'\'') => quote = Some(*byte),
                (None, b'>') => return from + offset + 1,
                _ => {}
            }
        }
        bytes.len()
    }

    /// `range` is one whole element whose local name is `local`: its start tag, and either nothing
    /// more (written empty) or everything up to its own end tag.
    fn is_element(bytes: &[u8], range: &Range<usize>, local_name: &str) -> bool {
        if !bytes[range.clone()].starts_with(b"<") || bytes[range.clone()].starts_with(b"</") {
            return false;
        }
        let start = range.start..tag_end(bytes, range.start);
        let written = tag_name(bytes, &start);
        if local(&written) != local_name {
            return false;
        }
        if bytes[start.clone()].ends_with(b"/>") {
            return range.end == start.end;
        }
        let tail = String::from_utf8_lossy(&bytes[range.clone()]).into_owned();
        let Some(close) = tail.rfind("</") else { return false };
        let end = range.start + close..range.end;
        range.end > start.end && tag_name(bytes, &end) == written && tail[close..].trim_end_matches('>').trim_end() == format!("</{written}")
    }

    fn check_props(bytes: &[u8], props: &Props) {
        assert!(is_start(bytes, &props.start, &props.raw_name) && closes(bytes, &props.start, &props.end));
        for (name, range) in &props.children {
            assert!(range.start >= props.start.end && is_element(bytes, range, local(name)), "properties child {name} at {range:?}");
        }
        if let Some(rpr) = &props.rpr {
            check_props(bytes, rpr);
        }
    }

    /// The local part of a written or canonical name.
    fn local(name: &str) -> &str {
        name.rsplit(':').next().unwrap_or(name)
    }

    /// The character a one-character child stands for, read from its tag.
    fn single_char(bytes: &[u8], range: &Range<usize>) -> Option<char> {
        let tag = String::from_utf8_lossy(&bytes[range.clone()]).into_owned();
        // Prefixed or not (a part in the default namespace writes its attributes bare).
        let attribute = |name: &str| {
            [format!(":{name}=\""), format!(" {name}=\"")]
                .iter()
                .find_map(|pattern| tag.split(pattern.as_str()).nth(1))
                .and_then(|rest| rest.split('"').next())
                .map(str::to_string)
        };
        match local(&tag_name(bytes, range)) {
            "tab" | "ptab" => Some('\t'),
            "br" => Some(match attribute("type").as_deref() {
                Some("page") => '\u{000C}',
                Some("column") => '\u{000E}',
                _ => '\n',
            }),
            "cr" => Some('\n'),
            "noBreakHyphen" => Some('\u{2011}'),
            "softHyphen" => Some('\u{00AD}'),
            "sym" => attribute("char").and_then(|hex| u32::from_str_radix(&hex, 16).ok()).and_then(char::from_u32),
            _ => None,
        }
    }

    /// N2: an equation's WordprocessingML (its part's own prefix, or no prefix in a part written in
    /// the default namespace) is only run properties and text and what is inside them.
    fn plain_equation(bytes: &[u8], span: &Range<usize>) -> bool {
        let prefix = written_prefix(bytes);
        let text = String::from_utf8_lossy(&bytes[span.clone()]).into_owned();
        let mut skipping: Option<(String, usize)> = None;
        for piece in text.split('<').skip(1) {
            let tag = piece.split('>').next().unwrap_or_default();
            if tag.starts_with('?') || tag.starts_with('!') {
                continue;
            }
            let closing = tag.starts_with('/');
            let empty = tag.ends_with('/');
            let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or_default();
            if let Some((skipped, depth)) = &mut skipping {
                if name == skipped.as_str() && !empty {
                    if closing {
                        *depth -= 1;
                    } else {
                        *depth += 1;
                    }
                }
                if *depth == 0 {
                    skipping = None;
                }
                continue;
            }
            let wordprocessing =
                if prefix.is_empty() { !name.contains(':') } else { name.strip_prefix(prefix.as_str()).is_some_and(|rest| rest.starts_with(':')) };
            if closing || !wordprocessing {
                continue;
            }
            if matches!(local(name), "rPr" | "t") {
                if !empty {
                    skipping = Some((name.to_string(), 1));
                }
                continue;
            }
            return false;
        }
        true
    }

    /// The span a wrapper covers.
    fn covers(wrapper: &Wrapper) -> Range<usize> {
        wrapper.start.start..wrapper.end.as_ref().map_or(wrapper.start.end, |end| end.end)
    }

    fn node_span(node: &Node) -> Range<usize> {
        match node {
            Node::Run(run) => run.span(),
            Node::Other { span, .. } => span.clone(),
        }
    }

    /// I1: an object's nodes hold only what it owns. A picture or note owns the child that made
    /// it, alone in its run; a field owns runs of field characters, instructions, result text and
    /// characters; an equation its element; and every one of them rendered page breaks.
    fn atomic_is_whole(bytes: &[u8], paragraph: &ScannedPara, first: usize, last: usize) -> bool {
        let span = &paragraph.nodes[first..=last];
        if let [Node::Other { name, span, .. }] = span {
            return (name == "m:oMath" || name == "m:oMathPara") && plain_equation(bytes, span);
        }
        let children: Vec<(&str, bool)> = span
            .iter()
            .flat_map(|node| match node {
                Node::Run(run) => run
                    .children
                    .iter()
                    .map(|child| match &child.kind {
                        ChildKind::Text { raw_name, .. } | ChildKind::Other { raw_name, .. } => {
                            (local(raw_name), matches!(child.kind, ChildKind::Text { .. }))
                        }
                        ChildKind::Single { .. } => ("single", true),
                    })
                    .collect(),
                Node::Other { .. } => vec![("not a run", false)],
            })
            .collect();
        let makers =
            children.iter().filter(|(name, _)| matches!(*name, "drawing" | "pict" | "object" | "footnoteReference" | "endnoteReference")).count();
        if first == last && makers == 1 {
            return children.iter().all(|(name, unit)| {
                matches!(*name, "drawing" | "pict" | "object" | "footnoteReference" | "endnoteReference" | "lastRenderedPageBreak")
                    || (!unit && matches!(*name, "t" | "delText"))
            });
        }
        makers == 0
            && children.iter().all(|(name, unit)| {
                *unit || matches!(*name, "fldChar" | "instrText" | "delInstrText" | "t" | "delText" | "sym" | "lastRenderedPageBreak")
            })
    }

    /// Each editable unit points at bytes that hold its character, inside its run, inside the
    /// paragraph; each character and one-character child points back at its unit; nodes are in
    /// order; every recorded tag is where it says, under the name recorded, and closed by its own
    /// end tag; each wrapper holds exactly the nodes that name it; objects, markers, locked and
    /// outside units are what they say.
    fn check_locations(bytes: &[u8], paragraph: &ScannedPara) {
        let span = paragraph.span();
        assert!(is_start(bytes, &paragraph.start, &paragraph.raw_name) && closes(bytes, &paragraph.start, &paragraph.end));
        if let Some(ppr) = &paragraph.ppr {
            check_props(bytes, ppr);
        }
        for (index, wrapper) in paragraph.wrappers.iter().enumerate() {
            assert!(wrapper.start.start > paragraph.start.start && covers(wrapper).end <= span.end);
            assert!(bytes[wrapper.start.clone()].starts_with(b"<") && closes(bytes, &wrapper.start, &wrapper.end), "wrapper {index}");
            let kind = match local(&tag_name(bytes, &wrapper.start)) {
                "ins" | "moveTo" => WrapperKind::Inserted,
                "del" | "moveFrom" => WrapperKind::Deleted,
                _ => WrapperKind::Other,
            };
            assert_eq!(wrapper.kind, kind);
            if let Some(parent) = wrapper.parent {
                let outer = covers(&paragraph.wrappers[parent]);
                assert!(parent < index && outer.start < wrapper.start.start && covers(wrapper).end <= outer.end, "wrapper {index} inside its parent");
            }
        }
        let mut at = paragraph.start.end;
        for node in &paragraph.nodes {
            let node_span = node_span(node);
            assert!(node_span.start >= at && node_span.end <= span.end, "nodes in order inside the paragraph");
            if let Node::Other { name, span, .. } = node {
                assert!(is_element(bytes, span, local(name)), "node {name} at {span:?}");
            }
            at = node_span.end;
            // A wrapper covers a node exactly when the node names it, itself or through its parents.
            let chain = paragraph.chain(node.wrapper());
            for (index, wrapper) in paragraph.wrappers.iter().enumerate() {
                let inside = covers(wrapper).start < node_span.start && node_span.end <= covers(wrapper).end;
                assert_eq!(inside, chain.contains(&index), "wrapper {index} and node at {node_span:?}");
            }
            if let Node::Run(run) = node {
                assert!(is_start(bytes, &run.start, &run.raw_name) && closes(bytes, &run.start, &run.end));
                assert!(run.rpr.as_ref().is_none_or(|rpr| is_element(bytes, rpr, "rPr") && rpr.start >= run.start.end));
                assert!(run.rpr_change.as_ref().is_none_or(|change| is_element(bytes, change, "rPrChange")));
                // The first `w:rPr` is the run's; any later one is a child.
                assert!(run.children.iter().all(|child| run.rpr.as_ref().is_some_and(|rpr| rpr.end <= child.span.start) || local(&tag_name(bytes, &child.span)) != "rPr"));
                assert!(
                    run.rpr_change.as_ref().is_none_or(|change| run.rpr.as_ref().is_some_and(|rpr| change.start > rpr.start && change.end < rpr.end))
                );
                for child in &run.children {
                    assert!(child.span.start >= run.start.end && child.span.end <= run.span().end);
                    match &child.kind {
                        ChildKind::Text { start, end, raw_name, chars, .. } => {
                            assert_eq!(child.span, start.start..end.as_ref().map_or(start.end, |end| end.end));
                            assert!(is_element(bytes, &child.span, local(raw_name)));
                            assert!(is_start(bytes, start, raw_name) && closes(bytes, start, end));
                            assert!(chars.iter().all(|raw| raw.bytes.start >= start.end));
                        }
                        ChildKind::Other { start, end, raw_name } => {
                            assert!(is_start(bytes, start, raw_name) && closes(bytes, start, end));
                            assert_eq!(child.span, start.start..end.as_ref().map_or(start.end, |end| end.end));
                            assert!(is_element(bytes, &child.span, local(raw_name)));
                        }
                        ChildKind::Single { .. } => {
                            assert!(single_char(bytes, &child.span).is_some());
                            assert!(is_element(bytes, &child.span, local(&tag_name(bytes, &child.span))));
                        }
                    }
                }
            }
        }
        let outside_start = paragraph.units.iter().take_while(|unit| unit.loc == Loc::Outside { at_end: false }).count();
        let outside_end = paragraph.units.iter().rev().take_while(|unit| unit.loc == Loc::Outside { at_end: true }).count();
        let mut markers = BTreeSet::new();
        for (index, unit) in paragraph.units.iter().enumerate() {
            match unit.loc {
                Loc::Text { node, child, raw } => {
                    let Node::Run(run) = &paragraph.nodes[node] else { panic!("text outside a run") };
                    let ChildKind::Text { chars, .. } = &run.children[child].kind else { panic!("text outside a w:t") };
                    assert_eq!(chars[raw].unit, Some(index));
                    let ch = decode(&bytes[chars[raw].bytes.clone()]).and_then(clean_char);
                    assert_eq!(ch, Some(unit.ch), "unit {index}");
                }
                Loc::Single { node, child } => {
                    let Node::Run(run) = &paragraph.nodes[node] else { panic!("a child outside a run") };
                    assert!(matches!(run.children[child].kind, ChildKind::Single { unit: Some(found) } if found == index));
                    assert_eq!(single_char(bytes, &run.children[child].span), Some(unit.ch), "unit {index}");
                }
                Loc::Atomic { first, last } => {
                    assert!(first <= last && last < paragraph.nodes.len() && unit.ch == OBJ);
                    assert!(atomic_is_whole(bytes, paragraph, first, last), "Atomic {{ {first}, {last} }} holds what it does not own");
                    let wrapper = paragraph.nodes[first].wrapper();
                    assert!(paragraph.nodes[first..=last].iter().all(|node| node.wrapper() == wrapper));
                    // No other unit stands in its nodes.
                    for (other, unit) in paragraph.units.iter().enumerate() {
                        let node = match unit.loc {
                            Loc::Text { node, .. } | Loc::Single { node, .. } | Loc::Marker { node } | Loc::Locked { node: Some(node), .. } => node,
                            Loc::Atomic { first: start, .. } if other != index => start,
                            _ => continue,
                        };
                        assert!(!(first..=last).contains(&node), "unit {other} stands in object {index}");
                    }
                }
                Loc::Marker { node } => {
                    assert!(unit.ch == OBJ && markers.insert(node), "one unit for each marker");
                    let Node::Other { name, span, .. } = &paragraph.nodes[node] else { panic!("a marker that is a run") };
                    assert!(matches!(name.as_str(), "w:bookmarkStart" | "w:bookmarkEnd" | "w:commentRangeStart" | "w:commentRangeEnd"));
                    assert_eq!(local(&tag_name(bytes, span)), local(name));
                }
                Loc::Locked { node, child } => {
                    if let Some(node) = node {
                        assert!(node < paragraph.nodes.len());
                        if let Some(child) = child {
                            assert!(matches!(&paragraph.nodes[node], Node::Run(run) if child < run.children.len()));
                        }
                    } else {
                        assert!(child.is_none());
                    }
                }
                Loc::Outside { at_end } => {
                    assert_eq!(unit.ch, OBJ);
                    assert!(if at_end { index >= paragraph.units.len() - outside_end } else { index < outside_start }, "outside units at the ends");
                }
            }
        }
        // Every character a unit was made of points back at it.
        for node in &paragraph.nodes {
            let Node::Run(run) = node else { continue };
            for child in &run.children {
                if let ChildKind::Text { chars, .. } = &child.kind {
                    for raw in chars {
                        if let Some(unit) = raw.unit {
                            assert!(matches!(paragraph.units[unit].loc, Loc::Text { .. }));
                        }
                    }
                }
            }
        }
        for range in paragraph.editable() {
            assert!(paragraph.units[range].iter().all(|unit| matches!(unit.loc, Loc::Text { .. } | Loc::Single { .. })));
        }
    }

    fn body_paragraph(scanned: &BTreeMap<String, ScannedPart>, index: u32) -> &ScannedPara {
        &scanned["word/document.xml"].paragraphs[&index]
    }

    fn bytes_of<'a>(scanned: &'a BTreeMap<String, ScannedPart>, part: &str) -> &'a [u8] {
        scanned[part].bytes.as_slice()
    }

    fn objects(paragraph: &ScannedPara) -> Vec<&Loc> {
        paragraph.units.iter().filter(|unit| unit.ch == OBJ).map(|unit| &unit.loc).collect()
    }

    #[test]
    fn reads_text_tabs_breaks_and_symbols_as_the_reader_does() {
        let bytes = package(
            r#"<w:p><w:pPr><w:jc w:val="both"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">Rent &amp; fees</w:t><w:tab/><w:t>due</w:t></w:r><w:proofErr w:type="spellStart"/><w:r><w:br/><w:sym w:font="Symbol" w:char="F0B7"/><w:noBreakHyphen/><w:t>end</w:t></w:r></w:p>"#,
            &[],
            &[],
        );
        let scanned = scans_as_read(&bytes);
        let paragraph = body_paragraph(&scanned, 0);
        assert_eq!(paragraph.units[0].loc, Loc::Text { node: 0, child: 0, raw: 0 });
        // "&amp;" is one character, located on its reference.
        let Node::Run(run) = &paragraph.nodes[0] else { panic!("a run") };
        let ChildKind::Text { chars, .. } = &run.children[0].kind else { panic!("text") };
        assert_eq!(&bytes_of(&scanned, "word/document.xml")[chars[5].bytes.clone()], b"&amp;");
        assert!(run.rpr.is_some());
        assert!(matches!(paragraph.nodes[1], Node::Other { ref name, .. } if name == "w:proofErr"));
        assert!(matches!(paragraph.units.iter().find(|unit| unit.ch == '\t').map(|unit| &unit.loc), Some(Loc::Single { node: 0, child: 1 })));
        assert!(paragraph.ppr.is_some());
    }

    #[test]
    fn collapses_a_field_into_one_object_over_its_runs() {
        let bytes = package(
            r#"<w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>3</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t xml:space="preserve"> of </w:t></w:r><w:fldSimple w:instr=" NUMPAGES "><w:r><w:t>9</w:t></w:r></w:fldSimple></w:p>"#,
            &[],
            &[],
        );
        let scanned = scans_as_read(&bytes);
        let paragraph = body_paragraph(&scanned, 0);
        assert_eq!(objects(paragraph), vec![&Loc::Atomic { first: 1, last: 5 }, &Loc::Locked { node: Some(7), child: None }]);
    }

    #[test]
    fn knows_tracked_changes_hyperlinks_and_markers() {
        let comments =
            format!(r#"<w:comments {W}><w:comment w:id="0" w:author="Ana"><w:p><w:r><w:t>Why?</w:t></w:r></w:p></w:comment></w:comments>"#);
        let bytes = package(
            r#"<w:p><w:bookmarkStart w:id="1" w:name="Rent"/><w:ins w:id="2" w:author="Ana" w:date="2026-10-01T09:00:00Z"><w:r><w:t xml:space="preserve">new </w:t></w:r></w:ins><w:del w:id="3" w:author="Ana" w:date="2026-10-01T09:00:00Z"><w:r><w:delText xml:space="preserve">old </w:delText></w:r></w:del><w:hyperlink w:anchor="Rent"><w:r><w:t>link</w:t></w:r></w:hyperlink><w:bookmarkEnd w:id="1"/><w:commentRangeStart w:id="0"/><w:r><w:t>noted</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r></w:p>"#,
            &[("rId1", "comments", "comments.xml")],
            &[("word/comments.xml", comments.into_bytes())],
        );
        let scanned = scans_as_read(&bytes);
        let paragraph = body_paragraph(&scanned, 0);
        let kinds: Vec<WrapperKind> = paragraph.wrappers.iter().map(|wrapper| wrapper.kind).collect();
        assert_eq!(kinds, vec![WrapperKind::Inserted, WrapperKind::Deleted, WrapperKind::Other]);
        assert_eq!(paragraph.units[0].loc, Loc::Marker { node: 0 });
        let Node::Run(deleted) = &paragraph.nodes[2] else { panic!("the deleted run") };
        assert_eq!(paragraph.chain(deleted.wrapper), vec![1]);
        assert!(matches!(deleted.children[0].kind, ChildKind::Text { deleted: true, .. }));
        // The comment's own paragraph is scanned too.
        assert_eq!(scanned["word/comments.xml"].paragraphs[&0].text(), "Why?");
    }

    #[test]
    fn sees_through_content_controls_custom_xml_and_smart_tags() {
        let bytes = package(
            r#"<w:sdt><w:sdtPr><w:alias w:val="Clause"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Block control</w:t></w:r></w:p></w:sdtContent></w:sdt><w:p><w:sdt><w:sdtPr><w:alias w:val="Party"/><w:tag w:val="party"/></w:sdtPr><w:sdtContent><w:r><w:t>PT Contoh</w:t></w:r></w:sdtContent></w:sdt><w:customXml w:element="date"><w:r><w:t xml:space="preserve"> on 1 May</w:t></w:r></w:customXml><w:smartTag w:element="place"><w:r><w:t xml:space="preserve"> in Jakarta</w:t></w:r></w:smartTag></w:p>"#,
            &[],
            &[],
        );
        let scanned = scans_as_read(&bytes);
        let paragraph = body_paragraph(&scanned, 1);
        assert!(matches!(paragraph.nodes[0], Node::Other { ref name, wrapper: Some(0), .. } if name == "w:sdtPr"));
        let Node::Run(inside) = &paragraph.nodes[1] else { panic!("the run in the control") };
        assert_eq!(paragraph.chain(inside.wrapper), vec![0, 1]);
        assert_eq!(paragraph.wrappers[1].parent, Some(0));
    }

    #[test]
    fn finds_drawings_note_references_and_text_boxes() {
        let footnotes = format!(
            r#"<w:footnotes {W}><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:t>A note</w:t></w:r></w:p></w:footnote></w:footnotes>"#
        );
        let body = r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:r><w:drawing><wp:inline><wp:extent cx="9525" cy="9525"/><wp:docPr id="1" name="Logo"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:blipFill><a:blip r:embed="rId2"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r><w:r><w:footnoteReference w:id="1"/></w:r><w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wp:inline><wp:extent cx="914400" cy="914400"/><wp:docPr id="2" name="Box"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>In the box</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></mc:Choice><mc:Fallback><w:pict><v:rect><v:textbox><w:txbxContent><w:p><w:r><w:t>In the box</w:t></w:r></w:p></w:txbxContent></v:textbox></v:rect></w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p>"#;
        let bytes = package(
            body,
            &[("rId1", "footnotes", "footnotes.xml"), ("rId2", "image", "media/image1.png")],
            &[("word/footnotes.xml", footnotes.into_bytes()), ("word/media/image1.png", PNG.to_vec())],
        );
        let scanned = scans_as_read(&bytes);
        let paragraph = body_paragraph(&scanned, 0);
        assert_eq!(
            objects(paragraph),
            vec![&Loc::Atomic { first: 1, last: 1 }, &Loc::Atomic { first: 2, last: 2 }, &Loc::Locked { node: Some(3), child: None }]
        );
        // The text box's paragraph (in the chosen branch) is the part's second w:p.
        assert_eq!(body_paragraph(&scanned, 1).text(), "In the box");
        assert!(!scanned["word/document.xml"].paragraphs.contains_key(&2), "the branch the reader leaves is not read");
    }

    #[test]
    fn moves_markers_between_blocks_into_the_next_paragraph_or_the_last() {
        let bytes = package(
            r#"<w:bookmarkStart w:id="5" w:name="All"/><w:p><w:r><w:t>First</w:t></w:r></w:p><w:p><w:r><w:t>Last</w:t></w:r></w:p><w:bookmarkEnd w:id="5"/>"#,
            &[],
            &[],
        );
        let scanned = scans_as_read(&bytes);
        assert_eq!(body_paragraph(&scanned, 0).units[0].loc, Loc::Outside { at_end: false });
        assert_eq!(body_paragraph(&scanned, 1).units.last().map(|unit| &unit.loc), Some(&Loc::Outside { at_end: true }));
    }

    #[test]
    fn scans_cells_and_headers_and_leaves_parts_nobody_asked_for() {
        let header = format!(r#"<w:hdr {W}><w:p><w:r><w:t>Header</w:t></w:r></w:p></w:hdr>"#);
        let bytes = package(
            r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Cell</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:hMerge/></w:tcPr><w:p/></w:tc></w:tr></w:tbl><w:p/><w:sectPr><w:headerReference w:type="default" r:id="rId1"/></w:sectPr>"#,
            &[("rId1", "header", "header1.xml")],
            &[("word/header1.xml", header.into_bytes())],
        );
        let scanned = scans_as_read(&bytes);
        assert!(scanned.contains_key("word/header1.xml"));
        let pkg = Package::open_with(&bytes, &ReadLimits::default()).unwrap();
        assert!(scan_package(&pkg, &BTreeMap::new(), &mut budget()).unwrap().is_empty());
    }

    fn para(text: &str) -> String {
        format!(r#"<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#)
    }

    /// The scan numbers paragraphs as the reader does in every place one can stand: nested cells,
    /// block controls, both branches of alternative content (Choice first), text boxes, notes,
    /// comments, and headers read while the main part is.
    #[test]
    fn agrees_with_the_reader_on_every_paragraphs_index() {
        let footnotes = format!(
            r#"<w:footnotes {W}><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1">{}{}</w:footnote><w:footnote w:type="normal" w:id="2">{}</w:footnote></w:footnotes>"#,
            para("Foot one a"),
            para("Foot one b"),
            para("Foot two")
        );
        let endnotes = format!(
            r#"<w:endnotes {W}><w:endnote w:type="separator" w:id="-1"><w:p/></w:endnote><w:endnote w:id="1">{}</w:endnote></w:endnotes>"#,
            para("End one")
        );
        let comments = format!(
            r#"<w:comments {W}><w:comment w:id="0">{}</w:comment><w:comment w:id="3"><w:tbl><w:tr><w:tc>{}</w:tc></w:tr></w:tbl>{}</w:comment><w:comment w:id="0">{}</w:comment></w:comments>"#,
            para("Comment zero"),
            para("Comment in a cell"),
            para("Comment three"),
            para("A second comment zero the reader skips")
        );
        let header = format!(r#"<w:hdr {W}>{}<w:sdt><w:sdtContent>{}</w:sdtContent></w:sdt></w:hdr>"#, para("Header one"), para("Header two"));
        let footer = format!(r#"<w:ftr {W}>{}</w:ftr>"#, para("Footer one"));
        let first_header = format!(r#"<w:hdr {W}>{}</w:hdr>"#, para("First page header"));
        let text_box = format!(
            r#"<w:p><w:r><w:t>Before box</w:t></w:r><w:r><w:drawing><wp:anchor><wp:extent cx="9525" cy="9525"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent>{}{}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r><w:r><w:t>After box</w:t></w:r></w:p>"#,
            para("Box one"),
            para("Box two")
        );
        let body = format!(
            r#"{}<w:tbl><w:tr><w:tc>{}<w:tbl><w:tr><w:tc>{}<w:tbl><w:tr><w:tc>{}</w:tc></w:tr></w:tbl></w:tc><w:tc>{}</w:tc></w:tr></w:tbl>{}</w:tc></w:tr><w:sdt><w:sdtContent><w:tr><w:tc>{}</w:tc></w:tr></w:sdtContent></w:sdt></w:tbl><w:sdt><w:sdtPr/><w:sdtContent>{}<w:sdt><w:sdtContent>{}</w:sdtContent></w:sdt></w:sdtContent></w:sdt><mc:AlternateContent><mc:Choice Requires="w14">{}{}</mc:Choice><mc:Fallback>{}</mc:Fallback></mc:AlternateContent><mc:AlternateContent><mc:Choice Requires="x9">{}</mc:Choice><mc:Fallback>{}</mc:Fallback></mc:AlternateContent><w:customXml w:element="c">{}</w:customXml><w:ins w:id="9" w:author="Ana">{}</w:ins><w:del w:id="10" w:author="Ana">{}</w:del>{}<w:p><w:pPr><w:sectPr><w:headerReference w:type="first" r:id="rId6"/></w:sectPr></w:pPr><w:r><w:t>Section one ends</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r><w:r><w:endnoteReference w:id="1"/></w:r><w:r><w:footnoteReference w:id="2"/></w:r></w:p>{}<w:sectPr><w:headerReference w:type="default" r:id="rId4"/><w:footerReference w:type="default" r:id="rId5"/><w:headerReference w:type="even" r:id="rId4"/></w:sectPr>"#,
            para("Body first"),
            para("Outer cell"),
            para("Middle cell"),
            para("Inner cell"),
            para("Middle second cell"),
            para("Outer cell after"),
            para("Row in a control"),
            para("Block control"),
            para("Nested block control"),
            para("Choice one"),
            para("Choice two"),
            para("Fallback one"),
            para("Choice the reader leaves"),
            para("Fallback the reader takes"),
            para("Custom XML block"),
            para("Inserted block"),
            para("Deleted block the reader leaves"),
            text_box,
            para("Body last")
        );
        let bytes = package(
            &body,
            &[
                ("rId1", "footnotes", "footnotes.xml"),
                ("rId2", "endnotes", "endnotes.xml"),
                ("rId3", "comments", "comments.xml"),
                ("rId4", "header", "header1.xml"),
                ("rId5", "footer", "footer1.xml"),
                ("rId6", "header", "header2.xml"),
            ],
            &[
                ("word/footnotes.xml", footnotes.into_bytes()),
                ("word/endnotes.xml", endnotes.into_bytes()),
                ("word/comments.xml", comments.into_bytes()),
                ("word/header1.xml", header.into_bytes()),
                ("word/footer1.xml", footer.into_bytes()),
                ("word/header2.xml", first_header.into_bytes()),
            ],
        );
        let read = read_paragraphs(&bytes);
        let scanned = scans_as_read(&bytes);
        // Each text names its paragraph, so an index one off would have failed above; these pin the
        // numbering the two agree on.
        let at = |part: &str, index: u32| scanned[part].paragraphs[&index].text();
        assert_eq!(at("word/document.xml", 0), "Body first");
        assert_eq!(at("word/document.xml", 3), "Inner cell");
        assert_eq!(at("word/document.xml", 9), "Choice one");
        assert!(!read.contains_key(&("word/document.xml".to_string(), 11)), "a Fallback beside an understood Choice is not read");
        assert_eq!(at("word/document.xml", 13), "Fallback the reader takes");
        assert!(read.contains_key(&("word/document.xml".to_string(), 17)) && at("word/document.xml", 17).starts_with("Before box"));
        assert_eq!(at("word/document.xml", 18), "Box one");
        assert_eq!(at("word/footnotes.xml", 3), "Foot one b");
        assert_eq!(at("word/endnotes.xml", 1), "End one");
        assert_eq!(at("word/comments.xml", 1), "Comment in a cell");
        assert!(!scanned["word/comments.xml"].paragraphs.contains_key(&3), "a repeated comment id is skipped");
        assert_eq!(at("word/header1.xml", 1), "Header two");
        assert_eq!(at("word/footer1.xml", 0), "Footer one");
        assert_eq!(at("word/header2.xml", 0), "First page header");
    }

    /// One letter per unit: Text, Single, Atomic, Marker, Locked, Outside.
    fn kinds(paragraph: &ScannedPara) -> String {
        paragraph
            .units
            .iter()
            .map(|unit| match unit.loc {
                Loc::Text { .. } => 'T',
                Loc::Single { .. } => 'S',
                Loc::Atomic { .. } => 'A',
                Loc::Marker { .. } => 'M',
                Loc::Locked { .. } => 'L',
                Loc::Outside { .. } => 'O',
            })
            .collect()
    }

    #[test]
    fn reads_every_run_child_and_wrapper_as_the_reader_does() {
        let bytes = package(
            concat!(
                r#"<w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:t>a</w:t><w:ptab w:relativeTo="margin" w:alignment="right" w:leader="none"/><w:softHyphen/><w:cr/><w:br w:type="page"/><w:br w:type="column"/>"#,
                r#"<w:sym w:font="Symbol" w:char="0001"/><w:sym w:char="zz"/><w:t>&#1;b&#xFFFC;&#13;&bogus;c</w:t><w:t><![CDATA[cd]]></w:t><w:t>e<w:x/>f</w:t><w:instrText>ignored</w:instrText><w:pgNum/></w:r>"#,
                r#"<w:dir w:val="rtl"><w:r><w:t>שלום</w:t></w:r></w:dir><w:bdo w:val="ltr"><w:r><w:t>x</w:t></w:r></w:bdo>"#,
                r#"<w:moveFrom w:id="1" w:author="A"><w:r><w:delText>moved</w:delText></w:r></w:moveFrom><w:moveTo w:id="2" w:author="A"><w:r><w:t>moved</w:t></w:r></w:moveTo>"#,
                r#"<w:r><w:ruby><w:rubyPr/><w:rt><w:r><w:t>top</w:t></w:r></w:rt><w:rubyBase><w:r><w:t>base</w:t></w:r></w:rubyBase></w:ruby></w:r>"#,
                r#"<m:oMath><m:r><m:t>x</m:t></m:r></m:oMath><m:oMathPara><m:oMath/><m:oMath/></m:oMathPara><w:pPr><w:jc w:val="left"/></w:pPr></w:p>"#,
            ),
            &[],
            &[],
        );
        let scanned = scans_as_read(&bytes);
        let paragraph = body_paragraph(&scanned, 0);
        // a; five one-character children; b, a space for the carriage return, c (control characters,
        // U+FFFC and the unknown reference dropped); CDATA and a w:t holding an element, copied
        // whole; the page number; the bidi text; the move; the ruby base; an equation; the two of
        // one paragraph of equations, which share their node and so are locked; the second w:pPr
        // is content, not properties.
        assert_eq!(kinds(paragraph), ["T", "SSSSS", "TTT", "LL", "LL", "L", "TTTT", "T", "TTTTT", "TTTTT", "LLLL", "A", "LL"].concat());
        let kinds: Vec<WrapperKind> = paragraph.wrappers.iter().map(|wrapper| wrapper.kind).collect();
        assert_eq!(kinds, vec![WrapperKind::Other, WrapperKind::Other, WrapperKind::Deleted, WrapperKind::Inserted]);
        let Node::Run(first) = &paragraph.nodes[0] else { panic!("a run") };
        let ChildKind::Text { chars, .. } = &first.children[8].kind else { panic!("the text with references") };
        let units: Vec<Option<usize>> = chars.iter().map(|raw| raw.unit).collect();
        assert_eq!(units, vec![None, Some(6), None, Some(7), None, Some(8)]);
        assert!(matches!(first.children[9].kind, ChildKind::Other { .. }), "CDATA is copied whole");
        assert!(matches!(paragraph.nodes.last(), Some(Node::Other { name, .. }) if name == "w:pPr"));
        assert!(paragraph.ppr.is_some());
        assert_eq!(paragraph.editable(), vec![0..9, 14..29]);
    }

    /// 6C-18: nothing inside a field is edited where it stands: a field's result that spills over a
    /// paragraph's end, and everything in it, is locked until the field ends.
    #[test]
    fn locks_everything_inside_a_field_across_paragraphs() {
        let bytes = package(
            concat!(
                r#"<w:p><w:r><w:t>Contents</w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> TOC \o "1-3" </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>Entry one</w:t></w:r></w:p>"#,
                r#"<w:p><w:bookmarkStart w:id="7" w:name="_Toc1"/><w:hyperlink w:anchor="_Toc1"><w:r><w:t>Entry two</w:t><w:tab/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGEREF _Toc1 </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>2</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:hyperlink><w:bookmarkEnd w:id="7"/></w:p>"#,
                r#"<w:p><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t>After</w:t></w:r></w:p>"#,
                r#"<w:p><w:r><w:t>x</w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>REF a</w:instrText></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>REF b</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>inner</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>outer</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r></w:p>"#,
            ),
            &[],
            &[],
        );
        let scanned = scans_as_read(&bytes);
        let first = body_paragraph(&scanned, 0);
        assert_eq!(kinds(first), format!("{}L{}", "T".repeat(8), "L".repeat(9)));
        assert_eq!(objects(first), vec![&Loc::Locked { node: Some(1), child: None }]);
        assert_eq!(first.editable(), vec![0..8]);
        // The bookmark, the entry, its tab and the page number field inside the result: all locked.
        let second = body_paragraph(&scanned, 1);
        assert_eq!(kinds(second), format!("L{}LL", "L".repeat(10)));
        assert!(second.editable().is_empty());
        assert!(second.nodes.iter().all(|node| !matches!(node, Node::Run(run) if run.children.iter().any(|child| matches!(child.kind, ChildKind::Text { ref chars, .. } if chars.iter().any(|raw| raw.unit.is_some()))))));
        assert_eq!(kinds(body_paragraph(&scanned, 2)), "TTTTT");
        // A field in another's instruction is part of that text; an end or separate with no field
        // open is nothing.
        assert_eq!(kinds(body_paragraph(&scanned, 3)), "TA");
    }

    #[test]
    fn nests_fields_no_deeper_than_the_reader() {
        let begins = r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#.repeat(MAX_FIELD_DEPTH + 2);
        let ends = r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#.repeat(MAX_FIELD_DEPTH + 2);
        let body = format!(
            r#"<w:p><w:r><w:t>a</w:t></w:r>{begins}<w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>result</w:t></w:r>{ends}<w:r><w:t>b</w:t></w:r></w:p>"#
        );
        let scanned = scans_as_read(&package(&body, &[], &[]));
        assert_eq!(body_paragraph(&scanned, 0).text(), format!("a{OBJ}b"));
    }

    /// An object is whole nodes only when nothing else of the paragraph stands in them and they sit
    /// in the same wrappers; otherwise removing it would remove more, so it is locked.
    #[test]
    fn locks_an_object_that_shares_its_nodes_or_spans_wrappers() {
        let footnotes = format!(r#"<w:footnotes {W}><w:footnote w:id="1"><w:p/></w:footnote></w:footnotes>"#);
        let drawing = r#"<w:drawing><wp:inline><wp:extent cx="9525" cy="9525"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:blipFill><a:blip r:embed="rId2"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>"#;
        let body = format!(
            concat!(
                r#"<w:p><w:r><w:t>See</w:t>{drawing}</w:r><w:r><w:footnoteReference w:id="1"/><w:t>x</w:t></w:r><w:r>{drawing}</w:r>"#,
                r#"<w:r><w:t>y</w:t><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
                r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:ins w:id="5" w:author="A"><w:r><w:t>new</w:t></w:r></w:ins><w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
                r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:bookmarkStart w:id="1" w:name="in"/><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#
            ),
            drawing = drawing
        );
        let bytes = package(
            &body,
            &[("rId1", "footnotes", "footnotes.xml"), ("rId2", "image", "media/image1.png")],
            &[("word/footnotes.xml", footnotes.into_bytes()), ("word/media/image1.png", PNG.to_vec())],
        );
        let scanned = scans_as_read(&bytes);
        let paragraph = body_paragraph(&scanned, 0);
        assert_eq!(
            objects(paragraph),
            vec![
                &Loc::Locked { node: Some(0), child: None },
                &Loc::Locked { node: Some(1), child: None },
                &Loc::Atomic { first: 2, last: 2 },
                &Loc::Locked { node: Some(3), child: None },
                &Loc::Locked { node: Some(5), child: None },
                &Loc::Locked { node: Some(10), child: None },
                &Loc::Locked { node: Some(9), child: None },
            ]
        );
    }

    #[test]
    fn places_markers_and_references_as_the_reader_does() {
        // A comment without an id is still read (under the id ""), but a reference without an id
        // names no comment.
        let comments = format!(
            r#"<w:comments {W}><w:comment w:id="0"><w:p/></w:comment><w:comment w:id="3"><w:p/></w:comment><w:comment><w:p/></w:comment></w:comments>"#
        );
        let footnotes = format!(r#"<w:footnotes {W}><w:footnote w:id="1"><w:p/></w:footnote></w:footnotes>"#);
        let long = "n".repeat(257);
        let body = format!(
            concat!(
                r#"<w:p><w:bookmarkStart w:id="1" w:name=""/><w:bookmarkStart w:id="2" w:name="{long}"/><w:bookmarkEnd w:id="1"/><w:bookmarkEnd w:id="99"/><w:commentRangeStart w:id="5"/>"#,
                r#"<w:r><w:commentReference/></w:r><w:r><w:commentReference w:id="0"/></w:r><w:r><w:commentReference w:id="0"/></w:r>"#,
                r#"<w:r><w:footnoteReference w:id="1" w:customMarkFollows="1"/><w:t>*</w:t><w:t>after</w:t></w:r><w:r><w:footnoteReference w:id="9"/></w:r><w:r><w:endnoteReference w:id="1"/></w:r></w:p>"#,
                r#"<w:p><w:commentRangeStart w:id="3"/><w:r><w:t>z</w:t></w:r><w:commentRangeEnd w:id="3"/><w:r><w:commentReference w:id="3"/></w:r><w:r><w:rPr><w:b/></w:rPr><w:footnoteReference w:id="1"/></w:r></w:p>"#,
                r#"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="w14"><w:footnoteReference w:id="1"/></mc:Choice></mc:AlternateContent></w:r></w:p>"#
            ),
            long = long
        );
        let bytes = package(
            &body,
            &[("rId1", "comments", "comments.xml"), ("rId2", "footnotes", "footnotes.xml")],
            &[("word/comments.xml", comments.into_bytes()), ("word/footnotes.xml", footnotes.into_bytes())],
        );
        let scanned = scans_as_read(&bytes);
        let first = body_paragraph(&scanned, 0);
        // The comment reference (copied whole) once; the custom-mark note, every w:t after its
        // reference in the run being the mark.
        assert_eq!(objects(first), vec![&Loc::Locked { node: Some(6), child: Some(0) }, &Loc::Atomic { first: 8, last: 8 }]);
        assert_eq!(first.text(), format!("{OBJ}{OBJ}"));
        let second = body_paragraph(&scanned, 1);
        assert_eq!(kinds(second), "MTMA");
        // A reference inside alternative content stays locked, though its object comes after it.
        assert_eq!(objects(body_paragraph(&scanned, 2)), vec![&Loc::Locked { node: Some(0), child: None }]);
    }

    /// A drawing becomes an object when the reader makes one of it, and a text box is read as its
    /// own story, no deeper than the reader reads them.
    #[test]
    fn reads_drawings_and_text_boxes_as_the_reader_does() {
        fn image(id: &str, frame: &str) -> String {
            format!(
                r#"<w:r><w:drawing><wp:{frame}><wp:extent cx="9525" cy="9525"/><a:graphic><a:graphicData uri="p"><pic:pic><pic:blipFill><a:blip r:embed="{id}"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:{frame}></w:drawing></w:r>"#
            )
        }
        fn boxed(level: usize) -> String {
            let inner = if level == 0 { String::new() } else { boxed(level - 1) };
            format!(
                r#"<w:p><w:r><w:t>level {level}</w:t></w:r><w:r><w:drawing><wp:inline><a:graphic><a:graphicData uri="s"><wps:wsp><wps:txbx><w:txbxContent>{inner}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r><w:r><w:pict><v:shapetype/><v:rect><v:textbox><w:txbxContent><w:p><w:r><w:t>vml {level}</w:t></w:r></w:p></w:txbxContent></v:textbox></v:rect></w:pict></w:r></w:p>"#
            )
        }
        let body = format!(
            concat!(
                r#"<w:p>{missing}{external}{anchored}<w:r><w:drawing><wp:inline><a:graphic><a:graphicData uri="s"><wps:wsp><wps:spPr/></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#,
                r#"<w:r><w:pict><v:shape><v:imagedata r:id="rId2"/></v:shape></w:pict></w:r><w:r><w:object><v:shape><v:imagedata r:pict="rId9"/></v:shape></w:object></w:r><w:r><w:pict><v:line/></w:pict></w:r><w:r><w:drawing/></w:r></w:p>{boxes}"#
            ),
            missing = image("rId9", "inline"),
            external = image("rId3", "inline"),
            anchored = image("rId2", "anchor"),
            boxes = boxed(MAX_STORY_DEPTH + 1)
        );
        let bytes = package(
            &body,
            &[("rId2", "image", "media/image1.png"), ("rId3", "image", "https://example.com/x.png"), ("rId9", "image", "media/none.png")],
            &[("word/media/image1.png", PNG.to_vec())],
        );
        let bytes = {
            // The external image's relationship is marked external.
            let text = String::from_utf8_lossy(&bytes).into_owned();
            drop(text);
            let rels = format!(
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="{REL}/image" Target="media/image1.png"/><Relationship Id="rId3" Type="{REL}/image" Target="https://example.com/x.png" TargetMode="External"/><Relationship Id="rId9" Type="{REL}/image" Target="media/none.png"/></Relationships>"#
            );
            let document = format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#).into_bytes();
            let mut entries = vec![
                ("_rels/.rels".to_string(), format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="word/document.xml"/></Relationships>"#).into_bytes()),
                ("word/_rels/document.xml.rels".to_string(), rels.into_bytes()),
                ("word/document.xml".to_string(), document),
            ];
            entries.push(("word/media/image1.png".to_string(), PNG.to_vec()));
            drop(bytes);
            zip_entries(&entries).unwrap()
        };
        let scanned = scans_as_read(&bytes);
        // The anchored picture, the shape without a text box, the VML picture.
        assert_eq!(
            objects(body_paragraph(&scanned, 0)),
            vec![&Loc::Atomic { first: 2, last: 2 }, &Loc::Atomic { first: 3, last: 3 }, &Loc::Atomic { first: 4, last: 4 }]
        );
        let texts: Vec<String> = scanned["word/document.xml"].paragraphs.values().map(ScannedPara::text).collect();
        // Text boxes are read MAX_STORY_DEPTH deep: a deeper shape is an object with no story, a
        // deeper VML box is nothing.
        for level in 0..=MAX_STORY_DEPTH + 1 {
            let deep = MAX_STORY_DEPTH + 1 - level;
            let read = texts.iter().any(|text| text.starts_with(&format!("level {level}")));
            assert_eq!(read, deep <= MAX_STORY_DEPTH, "level {level}");
        }
    }

    #[test]
    fn follows_the_readers_table_rules() {
        fn nested(level: usize) -> String {
            let inner = if level == 0 { String::new() } else { nested(level - 1) };
            format!(r#"<w:tbl><w:tr><w:tc>{}{inner}</w:tc></w:tr></w:tbl>"#, para(&format!("table {level}")))
        }
        let body = format!(
            concat!(
                r#"{nested}<w:tbl><w:customXml><w:tr><w:tc><w:tcPr><w:hMerge/></w:tcPr>{first}</w:tc><w:tc><w:tcPr><w:hMerge w:val="restart"/></w:tcPr>{restart}</w:tc><w:tc><w:tcPr><w:hMerge/></w:tcPr>{skipped}</w:tc>"#,
                r#"<w:tc><w:tcPr><w:hMerge/><w:hMerge w:val="restart"/></w:tcPr>{last}</w:tc><w:sdt><w:sdtContent><w:tc>{control}</w:tc></w:sdtContent></w:sdt></w:tr></w:customXml></w:tbl>"#,
                r#"{after}<w:tbl/><w:bookmarkStart w:id="1" w:name="kept"/><w:tbl><w:tr><w:tc>{cell}<w:bookmarkStart w:id="2" w:name="cell"/></w:tc><w:tc/></w:tr></w:tbl><w:bookmarkStart w:id="3" w:name="dropped"/>"#
            ),
            nested = nested(MAX_TABLE_DEPTH + 1),
            first = para("first cell read"),
            restart = para("restart read"),
            skipped = para("continuation skipped"),
            last = para("the last merge says restart"),
            control = para("cell in a control"),
            after = para("after the tables"),
            cell = para("cell with a marker"),
        );
        let bytes = package(&body, &[], &[]);
        let read = read_paragraphs(&bytes);
        let scanned = scans_as_read(&bytes);
        let texts: Vec<String> = read.values().cloned().collect();
        assert!(texts.iter().any(|text| text == "table 2") && !texts.iter().any(|text| text.starts_with("table 0")), "deep tables flatten");
        assert!(!texts.iter().any(|text| text == "continuation skipped"));
        assert!(texts.iter().any(|text| text == "the last merge says restart"));
        // A table with no rows is no block: the marker after it waits for the next paragraph, which
        // is in the next table's cell; one left at the end of a story after a table is dropped.
        let cell = scanned["word/document.xml"].paragraphs.values().find(|paragraph| paragraph.text().contains("cell with a marker")).unwrap();
        assert_eq!(kinds(cell).chars().filter(|kind| *kind == 'O').count(), 2);
    }

    /// A paragraph or part whose written names cannot be written again is left unscanned (the
    /// writer would name other elements); the rest of the part is scanned.
    #[test]
    fn leaves_unscanned_what_it_could_not_name_again() {
        let w = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        let document = [
            format!(r#"<w:document xmlns:w="{w}"><w:body><w:p><w:r><w:t>fine</w:t></w:r></w:p><w:p><w:r xmlns:"#).as_bytes(),
            b"\xff",
            format!(r#"="{w}"><"#).as_bytes(),
            b"\xff",
            b":t>odd</",
            b"\xff",
            b":t></w:r></w:p></w:body></w:document>",
        ]
        .concat();
        let bytes = package_of(document, &[], &[]);
        let expected = read_paragraphs(&bytes);
        assert_eq!(expected.values().cloned().collect::<Vec<_>>(), vec!["fine".to_string(), "odd".to_string()]);
        let wanted = BTreeMap::from([("word/document.xml".to_string(), BTreeSet::from([0, 1]))]);
        let pkg = Package::open_with(&bytes, &ReadLimits::default()).unwrap();
        let scanned = scan_package(&pkg, &wanted, &mut budget()).unwrap();
        assert_eq!(scanned["word/document.xml"].paragraphs.keys().copied().collect::<Vec<_>>(), vec![0]);
        // A part whose own prefix cannot be written again is not scanned at all.
        let document = [&b"<\xff:document xmlns:\xff=\""[..], w.as_bytes(), b"\"><\xff:body><\xff:p/></\xff:body></\xff:document>"].concat();
        let bytes = package_of(document, &[], &[]);
        let pkg = Package::open_with(&bytes, &ReadLimits::default()).unwrap();
        assert!(scan_package(&pkg, &BTreeMap::from([("word/document.xml".to_string(), BTreeSet::from([0]))]), &mut budget()).unwrap().is_empty());
    }

    #[test]
    fn refuses_as_the_reader_refuses() {
        let bytes = package(&para("text"), &[], &[]);
        let wanted = BTreeMap::from([("word/document.xml".to_string(), BTreeSet::from([0]))]);
        // Within the package's element budget, and every record counted is spent.
        let tight = ReadLimits { max_elements: 15, ..ReadLimits::default() };
        let pkg = Package::open_with(&bytes, &tight).unwrap();
        let refused = scan_package(&pkg, &wanted, &mut budget());
        assert!(matches!(refused, Err(DocxError::Limit(_))), "{refused:?}");
        assert!(pkg.limit_reached());
        let pkg = Package::open_with(&bytes, &ReadLimits { max_elements: 1_000, ..ReadLimits::default() }).unwrap();
        assert_eq!(scan_package(&pkg, &wanted, &mut budget()).unwrap()["word/document.xml"].paragraphs.len(), 1);
        let shallow = ReadLimits { max_depth: 4, ..ReadLimits::default() };
        assert!(matches!(scan_package(&Package::open_with(&bytes, &shallow).unwrap(), &wanted, &mut budget()), Err(DocxError::Limit(_))));
        // The tree spends what it counts: a second reading of the part finds less left.
        let pkg = Package::open_with(&bytes, &ReadLimits { max_elements: 30, ..ReadLimits::default() }).unwrap();
        assert!(pkg.tree("word/document.xml", u64::MAX).unwrap().is_some());
        assert!(pkg.tree("word/document.xml", u64::MAX).is_err() && pkg.limit_reached());
        assert!(pkg.tree("word/missing.xml", u64::MAX).unwrap().is_none());
        // No main part, or one that is not a document.
        let entries = vec![("_rels/.rels".to_string(), b"<Relationships/>".to_vec())];
        let pkg = Package::open_with(&zip_entries(&entries).unwrap(), &ReadLimits::default()).unwrap();
        assert!(matches!(scan_package(&pkg, &wanted, &mut budget()), Err(DocxError::MissingPart(_))));
        let pkg = Package::open_with(&package_of(format!("<w:hdr {W}/>").into_bytes(), &[], &[]), &ReadLimits::default()).unwrap();
        assert!(matches!(scan_package(&pkg, &wanted, &mut budget()), Err(DocxError::NotWord(_))));
    }

    /// The reader stops making parts at MAX_PARTS: the notes past it are not read, nor a header
    /// named after it, so their ids and bookmarks change nothing later.
    #[test]
    fn stops_making_parts_where_the_reader_stops() {
        let notes: String = (1..=MAX_PARTS + 1).map(|id| format!(r#"<w:footnote w:id="{id}"><w:p/></w:footnote>"#)).collect();
        let footnotes = format!(r#"<w:footnotes {W}>{notes}</w:footnotes>"#);
        let header = format!(r#"<w:hdr {W}><w:p><w:bookmarkStart w:id="h" w:name="in the header"/></w:p></w:hdr>"#);
        let body = format!(
            r#"<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rId2"/></w:sectPr></w:pPr><w:r><w:footnoteReference w:id="{last_read}"/></w:r><w:r><w:footnoteReference w:id="{past}"/></w:r></w:p><w:p><w:bookmarkEnd w:id="h"/><w:r><w:t>end</w:t></w:r></w:p>"#,
            last_read = MAX_PARTS,
            past = MAX_PARTS + 1
        );
        let bytes = package(
            &body,
            &[("rId1", "footnotes", "footnotes.xml"), ("rId2", "header", "header1.xml")],
            &[("word/footnotes.xml", footnotes.into_bytes()), ("word/header1.xml", header.into_bytes())],
        );
        let scanned = scans_as_read(&bytes);
        assert_eq!(body_paragraph(&scanned, 0).text(), OBJ.to_string());
        assert_eq!(body_paragraph(&scanned, 1).text(), "end");
        assert_eq!(scanned["word/footnotes.xml"].paragraphs.len(), MAX_PARTS);
    }

    /// Run children, each standing for one kind the reader reads (or skips).
    const RUN_CHILDREN: &[&str] = &[
        "<w:t>ab</w:t>",
        r#"<w:t xml:space="preserve"> c&amp;d&#x41; </w:t>"#,
        "<w:t>&#1;e&#xFFFC;&#13;&bogus;</w:t>",
        "<w:t><![CDATA[f]]></w:t>",
        "<w:t/>",
        "<w:delText>gone</w:delText>",
        "<w:rPr><w:b/><w:rPrChange w:id=\"8\"><w:rPr/></w:rPrChange></w:rPr>",
        "<w:tab/>",
        "<w:ptab/>",
        "<w:br/>",
        r#"<w:br w:type="page"/>"#,
        "<w:cr/>",
        "<w:noBreakHyphen/>",
        "<w:softHyphen/>",
        r#"<w:sym w:char="F0B7"/>"#,
        r#"<w:sym w:char="0001"/>"#,
        r#"<w:fldChar w:fldCharType="begin"/>"#,
        r#"<w:fldChar w:fldCharType="separate"/>"#,
        r#"<w:fldChar w:fldCharType="end"/>"#,
        "<w:instrText> PAGE </w:instrText>",
        r#"<w:footnoteReference w:id="1"/>"#,
        r#"<w:footnoteReference w:id="1" w:customMarkFollows="1"/>"#,
        r#"<w:footnoteReference w:id="7"/>"#,
        r#"<w:endnoteReference w:id="1"/>"#,
        r#"<w:commentReference w:id="0"/>"#,
        r#"<w:commentReference w:id="4"/>"#,
        "<w:commentReference/>",
        "<w:t>x<w:y/>z</w:t>",
        "<w:pgNum/>",
        "<w:lastRenderedPageBreak/>",
        r#"<w:drawing><wp:inline><a:graphic><a:graphicData uri="p"><pic:pic><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>"#,
        r#"<w:drawing><wp:inline><a:graphic><a:graphicData uri="s"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>box</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:bookmarkStart w:id="3" w:name="b"/></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing>"#,
        r#"<w:pict><v:rect><v:textbox><w:txbxContent><w:p><w:r><w:t>vml</w:t></w:r></w:p></w:txbxContent></v:textbox></v:rect></w:pict>"#,
        "<w:ruby><w:rubyBase><w:r><w:t>base</w:t><w:tab/></w:r></w:rubyBase></w:ruby>",
        r#"<mc:AlternateContent><mc:Choice Requires="wps"><w:t>choice</w:t><w:fldChar w:fldCharType="begin"/></mc:Choice><mc:Fallback><w:t>fallback</w:t></mc:Fallback></mc:AlternateContent>"#,
        // Several notes and comments, markup the reader ignores, a second w:rPr, white space.
        r#"<w:footnoteReference w:id="2"/>"#,
        r#"<w:endnoteReference w:id="2"/>"#,
        r#"<w:commentReference w:id="1"/>"#,
        "<w:rPr><w:i/></w:rPr>",
        "<w:separator/>",
        "<w:annotationRef/>",
        "<w:lastRenderedPageBreak/>",
        " \r\n ",
    ];

    /// Inline content that holds no other inline content.
    const INLINE_LEAVES: &[&str] = &[
        r#"<w:bookmarkStart w:id="1" w:name="one"/>"#,
        r#"<w:bookmarkEnd w:id="1"/>"#,
        r#"<w:bookmarkStart w:id="2" w:name=""/>"#,
        r#"<w:bookmarkEnd w:id="3"/>"#,
        r#"<w:commentRangeStart w:id="0"/>"#,
        r#"<w:commentRangeEnd w:id="0"/>"#,
        r#"<w:proofErr w:type="spellStart"/>"#,
        "<m:oMath><m:r><m:t>x</m:t></m:r></m:oMath>",
        "<m:oMathPara><m:oMath/><m:oMath/></m:oMathPara>",
        r#"<w:commentRangeStart w:id="1"/>"#,
        r#"<w:commentRangeEnd w:id="1"/>"#,
        r#"<w:bookmarkEnd w:id="4"/>"#,
        r#"<w:bookmarkStart w:id="4" w:name="late"/>"#,
        r#"<w:permStart w:id="1" w:edGrp="everyone"/>"#,
        r#"<w:permEnd w:id="1"/>"#,
        r#"<w:moveFromRangeStart w:id="2" w:name="m"/>"#,
        r#"<w:moveFromRangeEnd w:id="2"/>"#,
        "\n  ",
    ];

    /// Inline wrappers: `{}` is their content, `{f}` a second content (a Fallback).
    const WRAPPERS: &[&str] = &[
        r#"<w:hyperlink w:anchor="one">{}</w:hyperlink>"#,
        r#"<w:ins w:id="5" w:author="A">{}</w:ins>"#,
        r#"<w:del w:id="6" w:author="A">{}</w:del>"#,
        r#"<w:moveTo w:id="7" w:author="A">{}</w:moveTo>"#,
        r#"<w:moveFrom w:id="8" w:author="A">{}</w:moveFrom>"#,
        r#"<w:smartTag w:element="x">{}</w:smartTag>"#,
        r#"<w:customXml w:element="x">{}</w:customXml>"#,
        r#"<w:dir w:val="rtl">{}</w:dir>"#,
        r#"<w:bdo w:val="ltr">{}</w:bdo>"#,
        r#"<w:sdt><w:sdtPr><w:alias w:val="a"/></w:sdtPr><w:sdtContent>{}</w:sdtContent><w:sdtContent>{f}</w:sdtContent></w:sdt>"#,
        r#"<w:fldSimple w:instr=" DATE ">{}</w:fldSimple>"#,
        r#"<mc:AlternateContent><mc:Choice Requires="w14">{}</mc:Choice><mc:Fallback>{f}</mc:Fallback></mc:AlternateContent>"#,
        r#"<mc:AlternateContent><mc:Choice Requires="x9">{}</mc:Choice><mc:Fallback>{f}</mc:Fallback></mc:AlternateContent>"#,
    ];

    const PARAGRAPH_PROPERTIES: &[&str] = &[
        "",
        r#"<w:pPr><w:jc w:val="center"/><w:rPr><w:b/></w:rPr></w:pPr>"#,
        r#"<w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rId4"/></w:sectPr></w:pPr>"#,
        r#"<w:pPr><w:framePr w:dropCap="drop" w:lines="2"/></w:pPr>"#,
    ];

    /// Wrappers nested deep: every kind, twenty levels.
    fn deeply_wrapped() -> String {
        let open = r#"<w:hyperlink w:anchor="one"><w:ins w:id="5" w:author="A"><w:sdt><w:sdtContent><w:smartTag w:element="x">"#;
        let close = "</w:smartTag></w:sdtContent></w:sdt></w:ins></w:hyperlink>";
        format!("{}<w:r><w:t>deep</w:t></w:r><w:bookmarkStart w:id=\"1\" w:name=\"one\"/>{}", open.repeat(5), close.repeat(5))
    }

    /// The same part written with the prefix `x`, or in the default namespace, instead of `w`.
    fn written_as(xml: &str, form: usize) -> String {
        match form {
            0 => xml.to_string(),
            1 => xml.replace("<w:", "<x:").replace("</w:", "</x:").replace(" w:", " x:").replace("xmlns:w=", "xmlns:x="),
            _ => xml.replace("<w:", "<").replace("</w:", "</").replace(" w:", " ").replace("xmlns:w=", "xmlns="),
        }
    }

    fn inline_content() -> impl Strategy<Value = String> {
        let run =
            proptest::collection::vec(proptest::sample::select(RUN_CHILDREN), 0..5).prop_map(|children| format!("<w:r>{}</w:r>", children.concat()));
        let leaf = prop_oneof![8 => run, 2 => proptest::sample::select(INLINE_LEAVES).prop_map(str::to_string), 1 => Just(deeply_wrapped())];
        leaf.prop_recursive(4, 40, 4, |inner| {
            (proptest::sample::select(WRAPPERS), proptest::collection::vec(inner.clone(), 0..4), proptest::collection::vec(inner, 0..2))
                .prop_map(|(wrapper, content, fallback)| wrapper.replace("{f}", &fallback.concat()).replace("{}", &content.concat()))
        })
    }

    fn blocks() -> impl Strategy<Value = String> {
        let paragraph = (proptest::sample::select(PARAGRAPH_PROPERTIES), proptest::collection::vec(inline_content(), 0..5))
            .prop_map(|(props, content)| format!("<w:p>{props}{}</w:p>", content.concat()));
        let marker = proptest::sample::select(&INLINE_LEAVES[..6]).prop_map(str::to_string);
        let leaf = prop_oneof![5 => paragraph, 1 => marker, 1 => Just("\n\t".to_string())];
        leaf.prop_recursive(3, 24, 4, |inner| {
            let list = || proptest::collection::vec(inner.clone(), 0..4).prop_map(|blocks| blocks.concat());
            prop_oneof![
                (list(), list()).prop_map(|(first, second)| format!(
                    r#"<w:tbl><w:tr><w:tc>{first}</w:tc><w:tc><w:tcPr><w:hMerge/></w:tcPr>{second}</w:tc><w:tc>{second}</w:tc></w:tr></w:tbl>"#
                )),
                list().prop_map(|content| format!("<w:sdt><w:sdtContent>{content}</w:sdtContent></w:sdt>")),
                (list(), list()).prop_map(|(choice, fallback)| format!(
                    r#"<mc:AlternateContent><mc:Choice Requires="w14">{choice}</mc:Choice><mc:Fallback>{fallback}</mc:Fallback></mc:AlternateContent>"#
                )),
                list().prop_map(|content| format!(r#"<w:ins w:id="9" w:author="A">{content}</w:ins><w:del w:id="10" w:author="A">{content}</w:del>"#)),
                list().prop_map(|content| format!(r#"<w:customXml w:element="c">{content}</w:customXml><w:smartTag w:element="s">{content}</w:smartTag>"#)),
            ]
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

        /// Whatever stories are made of, the scan reads every paragraph the reader made as the
        /// reader made it, and every location it records holds what it says.
        #[test]
        fn scans_generated_stories_as_the_reader_reads_them(
            body in proptest::collection::vec(blocks(), 1..5),
            note in proptest::collection::vec(blocks(), 1..3),
            comment in proptest::collection::vec(blocks(), 1..3),
            header in proptest::collection::vec(blocks(), 1..3),
            second in proptest::collection::vec(blocks(), 1..3),
            form in 0..3usize,
        ) {
            let footnotes = format!(
                r#"<w:footnotes {W}><w:footnote w:type="separator" w:id="-1"><w:p/></w:footnote><w:footnote w:id="1">{}</w:footnote><w:footnote w:id="2">{}</w:footnote></w:footnotes>"#,
                note.concat(),
                second.concat()
            );
            let endnotes = format!(r#"<w:endnotes {W}><w:endnote w:id="2">{}</w:endnote><w:endnote w:id="1">{}</w:endnote></w:endnotes>"#, second.concat(), note.concat());
            let comments = format!(r#"<w:comments {W}><w:comment w:id="1">{}</w:comment><w:comment w:id="0">{}</w:comment></w:comments>"#, second.concat(), comment.concat());
            let header = format!(r#"<w:hdr {W}>{}</w:hdr>"#, header.concat());
            let second_header = format!(r#"<w:hdr {W}>{}</w:hdr>"#, second.concat());
            // Ends with a paragraph, so that some paragraph has an origin whatever was generated.
            let body = format!(r#"{}<w:p><w:r><w:t>end</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rId4"/></w:sectPr>"#, body.concat());
            let document = written_as(&format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#), form);
            // Notes and comments name their own headers through their own relationships.
            let own_header = |target: &str| format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId4" Type="{REL}/header" Target="{target}"/><Relationship Id="rId9" Type="{REL}/image" Target="media/image1.png"/></Relationships>"#).into_bytes();
            let bytes = package_of(
                document.into_bytes(),
                &[
                    ("rId1", "footnotes", "footnotes.xml"),
                    ("rId2", "endnotes", "endnotes.xml"),
                    ("rId3", "comments", "comments.xml"),
                    ("rId4", "header", "header1.xml"),
                    ("rId9", "image", "media/image1.png"),
                ],
                &[
                    ("word/footnotes.xml", written_as(&footnotes, form).into_bytes()),
                    ("word/endnotes.xml", written_as(&endnotes, form).into_bytes()),
                    ("word/comments.xml", written_as(&comments, form).into_bytes()),
                    ("word/header1.xml", written_as(&header, form).into_bytes()),
                    ("word/header2.xml", written_as(&second_header, form).into_bytes()),
                    ("word/_rels/footnotes.xml.rels", own_header("header2.xml")),
                    ("word/_rels/comments.xml.rels", own_header("header2.xml")),
                    ("word/_rels/endnotes.xml.rels", own_header("header1.xml")),
                    ("word/_rels/header1.xml.rels", own_header("header2.xml")),
                    ("word/_rels/header2.xml.rels", own_header("header1.xml")),
                    ("word/media/image1.png", PNG.to_vec()),
                ],
            );
            scans_as_read(&bytes);
        }
    }

    /// A field's result buffers at most MAX_FIELD_ITEMS of the reader's items (a `w:t` is one,
    /// whatever its length), and spills them at the paragraph's end.
    #[test]
    fn buffers_a_fields_result_by_the_readers_items() {
        let body = format!(
            r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>{}</w:t>{}</w:r></w:p>"#,
            "a".repeat(MAX_FIELD_ITEMS + 1),
            "<w:tab/>".repeat(MAX_FIELD_ITEMS)
        );
        let scanned = scans_as_read(&package(&body, &[], &[]));
        let paragraph = body_paragraph(&scanned, 0);
        assert_eq!(paragraph.text(), format!("{OBJ}{}{}", "a".repeat(MAX_FIELD_ITEMS + 1), "\t".repeat(MAX_FIELD_ITEMS - 1)));
        assert!(paragraph.editable().is_empty());
    }

    const PICTURE: &str = r#"<w:drawing><wp:inline><wp:extent cx="9525" cy="9525"/><a:graphic><a:graphicData uri="p"><pic:pic><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>"#;

    /// I1: an object whose nodes hold markup the model does not (a permission range, a bookmark end
    /// before its start, a comment's only reference mark) is locked: deleting it would delete that
    /// markup where no comparison of models can see it.
    #[test]
    fn locks_an_object_whose_nodes_hold_markup_the_model_does_not() {
        let comments = format!(r#"<w:comments {W}><w:comment w:id="0"><w:p/></w:comment></w:comments>"#);
        let body = format!(
            concat!(
                r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>X</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>r</w:t></w:r><w:proofErr w:type="spellStart"/><w:permStart w:id="1"/><w:r><w:commentReference w:id="77"/></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
                r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:bookmarkEnd w:id="9"/><w:r><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
                r#"<w:p><w:bookmarkStart w:id="9" w:name="late"/><w:r><w:t>x</w:t></w:r></w:p>"#,
                r#"<w:p><w:commentRangeStart w:id="0"/><w:r><w:t>a</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r>{picture}<w:commentReference w:id="0"/></w:r></w:p>"#,
                r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:lastRenderedPageBreak/><w:t>2</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r>{picture}</w:r></w:p>"#,
            ),
            picture = PICTURE
        );
        let bytes = package(
            &body,
            &[("rId1", "comments", "comments.xml"), ("rId9", "image", "media/image1.png")],
            &[("word/comments.xml", comments.into_bytes()), ("word/media/image1.png", PNG.to_vec())],
        );
        let scanned = scans_as_read(&bytes);
        assert_eq!(objects(body_paragraph(&scanned, 0)), vec![&Loc::Locked { node: Some(0), child: None }]);
        assert_eq!(objects(body_paragraph(&scanned, 1)), vec![&Loc::Locked { node: Some(0), child: None }]);
        assert_eq!(objects(body_paragraph(&scanned, 3)).last(), Some(&&Loc::Locked { node: Some(3), child: None }));
        // What an object owns keeps it whole: a field of runs with a rendered page break, a picture
        // alone in its run.
        assert_eq!(objects(body_paragraph(&scanned, 4)), vec![&Loc::Atomic { first: 0, last: 4 }, &Loc::Atomic { first: 5, last: 5 }]);
    }

    /// I3: a part the tree refuses for a limit refuses the scan, whatever the package's budget: the
    /// scan never goes on with state the reader's did not have.
    #[test]
    fn refuses_when_a_secondary_part_is_past_a_limit() {
        let declarations: String = (0..=crate::xml::MAX_BINDINGS).map(|index| format!(r#" xmlns:p{index}="u""#)).collect();
        let footnotes = format!(r#"<w:footnotes {W}{declarations}><w:footnote w:id="1"><w:p/></w:footnote></w:footnotes>"#);
        let endnotes = format!(r#"<w:endnotes {W}{declarations}><w:endnote w:id="1"><w:p/></w:endnote></w:endnotes>"#);
        let comments = format!(r#"<w:comments {W}{declarations}><w:comment w:id="0"><w:p/></w:comment></w:comments>"#);
        let header = format!(r#"<w:hdr {W}{declarations}><w:p/></w:hdr>"#);
        let footer = format!(r#"<w:ftr {W}{declarations}><w:p/></w:ftr>"#);
        let wanted = BTreeMap::from([("word/document.xml".to_string(), BTreeSet::from([0]))]);
        let note = r#"<w:p><w:r><w:footnoteReference w:id="1"/></w:r></w:p>"#;
        let end_note = r#"<w:p><w:r><w:endnoteReference w:id="1"/></w:r></w:p>"#;
        let commented = r#"<w:p><w:commentRangeStart w:id="0"/><w:r><w:t>a</w:t></w:r><w:commentRangeEnd w:id="0"/></w:p>"#;
        let named = r#"<w:p/><w:sectPr><w:headerReference w:type="default" r:id="rId1"/></w:sectPr>"#;
        let footed = r#"<w:p><w:pPr><w:sectPr><w:footerReference w:type="default" r:id="rId1"/></w:sectPr></w:pPr></w:p>"#;
        let cases = [
            ("footnotes", "word/footnotes.xml", footnotes, note),
            ("endnotes", "word/endnotes.xml", endnotes, end_note),
            ("comments", "word/comments.xml", comments, commented),
            ("header", "word/header1.xml", header, named),
            ("footer", "word/footer1.xml", footer, footed),
        ];
        for (kind, name, part, body) in cases {
            let target = name.trim_start_matches("word/");
            let bytes = package(body, &[("rId1", kind, target)], &[(name, part.into_bytes())]);
            // The reader skips the part; the scan refuses.
            assert!(crate::read(&bytes).is_ok());
            let pkg = Package::open_with(&bytes, &ReadLimits::default()).unwrap();
            assert!(matches!(scan_package(&pkg, &wanted, &mut budget()), Err(DocxError::Limit(_))), "{kind}");
        }
    }

    /// M1: the reader formats a run by its first `w:rPr`; a later one is a child copied whole.
    #[test]
    fn takes_a_runs_first_properties() {
        let bytes = package(r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:rPr><w:i/></w:rPr><w:t>x</w:t></w:r></w:p>"#, &[], &[]);
        let scanned = scans_as_read(&bytes);
        let Node::Run(run) = &body_paragraph(&scanned, 0).nodes[0] else { panic!("a run") };
        assert_eq!(&bytes_of(&scanned, "word/document.xml")[run.rpr.clone().unwrap()], b"<w:rPr><w:b/></w:rPr>");
        assert!(matches!(&run.children[0].kind, ChildKind::Other { raw_name, .. } if raw_name == "w:rPr"));
    }

    /// 6C-39: a text box's paragraphs are read-only: every unit locked, nothing editable.
    #[test]
    fn locks_every_paragraph_of_a_text_box() {
        let body = r#"<w:p><w:r><w:t>Body</w:t></w:r><w:r><w:pict><v:rect><v:textbox><w:txbxContent><w:p><w:r><w:t>box text</w:t><w:tab/></w:r><w:bookmarkStart w:id="1" w:name="b"/></w:p></w:txbxContent></v:textbox></v:rect></w:pict></w:r><w:r><w:drawing><wp:inline><a:graphic><a:graphicData uri="s"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>shape text</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#;
        let scanned = scans_as_read(&package(body, &[], &[]));
        assert_eq!(body_paragraph(&scanned, 0).editable(), vec![0..4]);
        for index in [1, 2] {
            let paragraph = body_paragraph(&scanned, index);
            assert!(paragraph.units.iter().all(|unit| matches!(unit.loc, Loc::Locked { .. })), "{:?}", paragraph.units);
            assert!(paragraph.editable().is_empty());
        }
    }

    /// I2: what the scan holds grows with the markup, not with its nesting, and all of it is
    /// charged: 200 hyperlinks around 100,000 runs (600 KB of XML) keep 200 wrappers, each node
    /// naming one, within a bounded budget; a smaller budget refuses the scan before passing it.
    #[test]
    fn charges_what_it_holds_and_names_one_wrapper_a_node() {
        let body = format!("<w:p>{}{}{}</w:p>", "<w:hyperlink>".repeat(200), "<w:r/>".repeat(100_000), "</w:hyperlink>".repeat(200));
        let bytes = package(&body, &[], &[]);
        let wanted = BTreeMap::from([("word/document.xml".to_string(), BTreeSet::from([0]))]);
        let pkg = Package::open_with(&bytes, &ReadLimits::default()).unwrap();
        let mut budget = Budget::new(crate::inspect::MAX_WORK, 96 << 20);
        let scanned = scan_package(&pkg, &wanted, &mut budget).unwrap();
        let paragraph = body_paragraph(&scanned, 0);
        assert_eq!((paragraph.nodes.len(), paragraph.wrappers.len()), (100_000, 200));
        assert_eq!(paragraph.chain(paragraph.nodes[0].wrapper()), (0..200).collect::<Vec<_>>());
        // What is kept stays charged: the paragraph and the part's copy.
        let kept = paragraph_bytes(paragraph) + units(scanned["word/document.xml"].bytes.len());
        assert!(budget.held() >= kept);
        // Everything held only while walking was given back: what is left is the kept paragraph, the
        // part's copy and the ids remembered.
        assert!(budget.held() - kept < 4 << 10, "held {} kept {kept}", budget.held());
        // Linear in the markup: at most 128 bytes held at once for each byte of XML (the chains
        // cloned per node held 160 MB here).
        assert!(budget.peak() <= 128 * units(body.len()), "peak {}", budget.peak());
        let mut small = Budget::new(crate::inspect::MAX_WORK, 4 << 20);
        assert!(matches!(scan_package(&pkg, &wanted, &mut small), Err(DocxError::Limit(message)) if message == SCAN_TOO_LARGE));
        assert!(small.peak() <= 4 << 20);
    }

    /// N3: a field whose only extra is a run holding a reference that made nothing (a spent comment
    /// reference, a reference to a note that does not exist) is locked: the run child is not one a
    /// field owns.
    #[test]
    fn locks_a_field_holding_a_reference_that_made_nothing() {
        let comments = format!(r#"<w:comments {W}><w:comment w:id="0"><w:p/></w:comment></w:comments>"#);
        let field = |extra: &str| {
            format!(
                r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r>{extra}</w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#
            )
        };
        let body = format!(
            r#"<w:p><w:commentRangeStart w:id="0"/><w:r><w:t>a</w:t></w:r><w:commentRangeEnd w:id="0"/></w:p>{}{}{}"#,
            field(r#"<w:commentReference w:id="0"/>"#),
            field(r#"<w:footnoteReference w:id="7"/>"#),
            field("<w:t>1</w:t>")
        );
        let bytes = package(&body, &[("rId1", "comments", "comments.xml")], &[("word/comments.xml", comments.into_bytes())]);
        let scanned = scans_as_read(&bytes);
        assert_eq!(objects(body_paragraph(&scanned, 1)), vec![&Loc::Locked { node: Some(0), child: None }]);
        assert_eq!(objects(body_paragraph(&scanned, 2)), vec![&Loc::Locked { node: Some(0), child: None }]);
        assert_eq!(objects(body_paragraph(&scanned, 3)), vec![&Loc::Atomic { first: 0, last: 4 }]);
    }

    /// N2: an equation is whole only when it holds no WordprocessingML but run properties and text:
    /// a bookmark, a comment range, a permission range or a tracked change inside it is markup the
    /// model does not hold (it keeps the equation's linear text), so the equation is locked.
    #[test]
    fn locks_an_equation_holding_wordprocessing_markup() {
        let comments = format!(r#"<w:comments {W}><w:comment w:id="0"><w:p/></w:comment></w:comments>"#);
        let math = |inside: &str| format!(r#"<w:p><m:oMath>{inside}<m:r><w:rPr><w:b/></w:rPr><m:t>x</m:t></m:r></m:oMath></w:p>"#);
        let body = [
            math(r#"<w:bookmarkStart w:id="30" w:name="eq"/>"#),
            r#"<w:p><w:bookmarkEnd w:id="30"/></w:p>"#.to_string(),
            math(r#"<w:commentRangeStart w:id="0"/>"#),
            r#"<w:p><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r></w:p>"#.to_string(),
            math(r#"<w:permStart w:id="1" w:edGrp="everyone"/>"#),
            math(r#"<w:ins w:id="5" w:author="A"><m:r><m:t>y</m:t></m:r></w:ins>"#),
            math(""),
            r#"<w:p><m:oMathPara><m:oMath><m:r><w:rPr><w:i/></w:rPr><w:t>z</w:t></m:r></m:oMath></m:oMathPara></w:p>"#.to_string(),
        ]
        .concat();
        let bytes = package(&body, &[("rId1", "comments", "comments.xml")], &[("word/comments.xml", comments.into_bytes())]);
        let scanned = scans_as_read(&bytes);
        for index in [0, 2, 4, 5] {
            assert_eq!(objects(body_paragraph(&scanned, index)), vec![&Loc::Locked { node: Some(0), child: None }], "paragraph {index}");
        }
        assert_eq!(objects(body_paragraph(&scanned, 6)), vec![&Loc::Atomic { first: 0, last: 0 }]);
        assert_eq!(objects(body_paragraph(&scanned, 7)), vec![&Loc::Atomic { first: 0, last: 0 }]);
    }

    /// N5: a refused scan gives back everything it held, the paragraphs it had kept included.
    #[test]
    fn gives_back_what_it_held_when_it_refuses() {
        let declarations: String = (0..=crate::xml::MAX_BINDINGS).map(|index| format!(r#" xmlns:p{index}="u""#)).collect();
        let header = format!(r#"<w:hdr {W}{declarations}><w:p/></w:hdr>"#);
        let body = format!(r#"{}<w:sectPr><w:headerReference w:type="default" r:id="rId1"/></w:sectPr>"#, para("kept").repeat(2_000));
        let bytes = package(&body, &[("rId1", "header", "header1.xml")], &[("word/header1.xml", header.into_bytes())]);
        let wanted = BTreeMap::from([("word/document.xml".to_string(), (0..2_000).collect::<BTreeSet<u32>>())]);
        let pkg = Package::open_with(&bytes, &ReadLimits::default()).unwrap();
        let mut budget = budget();
        budget.hold(1_000).unwrap();
        assert!(matches!(scan_package(&pkg, &wanted, &mut budget), Err(DocxError::Limit(_))));
        assert_eq!(budget.held(), 1_000);
        assert!(budget.peak() > 1_000_000, "the paragraphs were kept, then given back");
    }

    #[global_allocator]
    static ALLOCATOR: wordcraft_testkit::counting::Counting = wordcraft_testkit::counting::Counting;

    /// Set in the process `parses_no_further_than_the_budget_allows` starts to measure in.
    const MEASURING: &str = "WORDCRAFT_SCAN_MEASURE";

    /// N1: a part's tree is charged as it is parsed, so a part far past the budget is refused
    /// before the scan allocates more than the budget: 1,000,000 paragraphs (7 MB of XML, a small
    /// zip) under a 1 MiB budget. Measured by the counting allocator in a process of its own, since
    /// its counts are the whole process's.
    #[test]
    fn parses_no_further_than_the_budget_allows() {
        if std::env::var_os(MEASURING).is_some() {
            let bytes = package(&"<w:p/>".repeat(1_000_000), &[], &[]);
            let wanted = BTreeMap::from([("word/document.xml".to_string(), BTreeSet::from([0]))]);
            let pkg = Package::open_with(&bytes, &ReadLimits::default()).unwrap();
            let limit: u64 = 1 << 20;
            let mut budget = Budget::new(crate::inspect::MAX_WORK, limit);
            let before = wordcraft_testkit::counting::reset_peak();
            let scanned = scan_package(&pkg, &wanted, &mut budget);
            let allocated = units(wordcraft_testkit::counting::peak().saturating_sub(before));
            eprintln!("allocated {allocated} under a budget of {limit}");
            assert!(matches!(scanned, Err(DocxError::Limit(_))));
            assert!(allocated <= limit, "allocated {allocated} under a budget of {limit}");
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "preserve::scan::tests::parses_no_further_than_the_budget_allows", "--test-threads=1", "--nocapture"])
            .env(MEASURING, "1")
            .status()
            .unwrap();
        assert!(status.success());
    }
}

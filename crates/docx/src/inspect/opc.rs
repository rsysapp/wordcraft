//! embed: the package structure an inspection compares by meaning: relationships, content types and
//! the canonical form of XML parts.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::budget::{Budget, table_room, table_slot, tree_room, tree_slot, units};

/// Memory charged for each relationship kept, besides its strings: its entry in its table.
const REL_BYTES: u64 = table_slot::<(String, Rel)>();
/// Memory charged for each content type kept, besides its strings: its entry in its map.
const TYPE_BYTES: u64 = tree_slot::<String, String>();
/// Memory charged for each bookmark of a story part, besides its id and name.
const BOOKMARK_BYTES: u64 = table_slot::<(String, Option<String>)>();
use super::digest::{Digest, Hasher};
use super::xml::{XEl, XNode, is_space};

// ---- Relationships ------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Rel {
    pub kind: String,
    pub target: String,
    pub external: bool,
}

/// One part's relationships.
#[derive(Default)]
pub(super) struct Rels {
    by_id: HashMap<String, Rel>,
}

impl Rels {
    pub(super) fn parse(root: &XEl, budget: &mut Budget) -> Result<Rels, String> {
        if &*root.name != "rel:Relationships" {
            return Err("a relationship part without Relationships".into());
        }
        if root.attrs.iter().any(|(key, _)| !key.contains(':')) {
            return Err("an attribute on Relationships".into());
        }
        // Sized from the relationships there are, so the table never grows.
        let count = root.els().count();
        budget.work(units(count))?;
        budget.hold(units(count).saturating_mul(REL_BYTES).saturating_add(table_room::<(String, Rel)>()))?;
        let mut by_id = HashMap::with_capacity(count);
        for node in &root.kids {
            match node {
                XNode::Text(text) if is_space(text) => {}
                XNode::Text(_) => return Err("text in a relationship part".into()),
                XNode::El(el) if &*el.name == "rel:Relationship" => {
                    if el.has_element_kids() || !el.text_is_space() {
                        return Err("content inside a relationship".into());
                    }
                    if let Some((key, _)) = el.attrs.iter().find(|(key, _)| !matches!(&**key, "Id" | "Type" | "Target" | "TargetMode")) {
                        return Err(format!("a relationship attribute {key}"));
                    }
                    let (Some(id), Some(kind), Some(target)) = (el.attr("Id"), el.attr("Type"), el.attr("Target")) else {
                        return Err("a relationship without Id, Type or Target".into());
                    };
                    let external = match el.attr("TargetMode") {
                        None | Some("Internal") => false,
                        Some("External") => true,
                        Some(other) => return Err(format!("a relationship TargetMode {other:?}")),
                    };
                    // Kept for the whole inspection: the strings (the entry was held with the table).
                    budget.hold(units(id.len().saturating_add(kind.len()).saturating_add(target.len())))?;
                    budget.work(40 + units(id.len()) / 4)?;
                    if by_id.insert(id.to_string(), Rel { kind: kind.to_string(), target: target.to_string(), external }).is_some() {
                        return Err(format!("two relationships with Id {id}"));
                    }
                }
                XNode::El(el) => return Err(format!("{} in a relationship part", el.name)),
            }
        }
        Ok(Rels { by_id })
    }

    pub(super) fn len(&self) -> usize {
        self.by_id.len()
    }

    pub(super) fn set(&self) -> BTreeSet<&Rel> {
        self.by_id.values().collect()
    }

    pub(super) fn of_type<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Rel> + 'a {
        self.by_id.values().filter(move |rel| is_type(&rel.kind, kind))
    }

    /// embed: the relationship with this id (streams mode resolves link targets and parts by it).
    pub(super) fn get(&self, id: &str) -> Option<&Rel> {
        self.by_id.get(id)
    }
}

/// Is `kind` the relationship type `full`, in its transitional or strict spelling? Exact, unlike the
/// desktop reader's match on the last segment: a type a reader would not take for this one must not
/// be read as it.
pub(super) fn is_type(kind: &str, full: &str) -> bool {
    const TRANSITIONAL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
    const STRICT: &str = "http://purl.oclc.org/ooxml/officeDocument/relationships/";
    kind == full || full.strip_prefix(TRANSITIONAL).is_some_and(|tail| kind.strip_prefix(STRICT) == Some(tail))
}

/// The source part of a relationship part (`""` for the package), or `None` for any other part.
pub(super) fn rels_source(name: &str) -> Option<String> {
    let (dir, file) = match name.rsplit_once('/') {
        Some((dir, file)) => (dir, file),
        None => return None,
    };
    let source_file = file.strip_suffix(".rels")?;
    if dir == "_rels" {
        return Some(source_file.to_string());
    }
    let source_dir = dir.strip_suffix("/_rels")?;
    Some(format!("{source_dir}/{source_file}"))
}

// ---- Content types ------------------------------------------------------------------------------

pub(super) struct ContentTypes {
    /// Lowercased extension → type.
    defaults: BTreeMap<String, String>,
    /// Lowercased part name without its leading `/` → type.
    overrides: BTreeMap<String, String>,
}

impl ContentTypes {
    pub(super) fn parse(root: &XEl, budget: &mut Budget) -> Result<ContentTypes, String> {
        if &*root.name != "ct:Types" {
            return Err("[Content_Types].xml without Types".into());
        }
        budget.hold(2 * tree_room::<String, String>())?;
        let mut defaults = BTreeMap::new();
        let mut overrides = BTreeMap::new();
        for node in &root.kids {
            match node {
                XNode::Text(text) if is_space(text) => {}
                XNode::Text(_) => return Err("text in [Content_Types].xml".into()),
                XNode::El(el) => {
                    let (key_name, map) = match &*el.name {
                        "ct:Default" => ("Extension", &mut defaults),
                        "ct:Override" => ("PartName", &mut overrides),
                        other => return Err(format!("{other} in [Content_Types].xml")),
                    };
                    if el.has_element_kids() || !el.text_is_space() || el.attrs.iter().any(|(key, _)| &**key != key_name && &**key != "ContentType") {
                        return Err("an unexpected content type entry".into());
                    }
                    let (Some(key), Some(kind)) = (el.attr(key_name), el.attr("ContentType")) else {
                        return Err("a content type entry without its attributes".into());
                    };
                    let key = key.strip_prefix('/').unwrap_or(key);
                    // Lowered, a key can grow by half (and its room by doubling): held at that most.
                    let lowered_room = units(key.len()).saturating_mul(3);
                    budget.hold(TYPE_BYTES.saturating_add(lowered_room).saturating_add(units(kind.len())))?;
                    budget.work(40 + units(key.len()) / 4)?;
                    match map.entry(key.to_lowercase()) {
                        std::collections::btree_map::Entry::Occupied(entry) => return Err(format!("two content types for {}", entry.key())),
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            entry.insert(kind.to_string());
                        }
                    }
                }
            }
        }
        Ok(ContentTypes { defaults, overrides })
    }

    /// The content type a reader gives `part`: its override, else its extension's default.
    pub(super) fn of(&self, part: &str) -> Option<&str> {
        let folded = part.to_lowercase();
        if let Some(kind) = self.overrides.get(&folded) {
            return Some(kind);
        }
        let extension = folded.rsplit_once('.').map(|(_, extension)| extension)?;
        self.defaults.get(extension).map(String::as_str)
    }
}

const WML: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.";
pub(super) const MAIN_TYPES: &[&str] = &[
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
    "application/vnd.ms-word.document.macroEnabled.main+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml",
    "application/vnd.ms-word.template.macroEnabledTemplate.main+xml",
];
pub(super) const COMMENTS_TYPE: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml";
pub(super) const COMMENTS_EX_TYPE: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.commentsExtended+xml";

/// A part whose text is the agreement's: the main part, headers, footers and notes.
pub(super) fn is_story_type(kind: &str) -> bool {
    MAIN_TYPES.contains(&kind)
        || kind.strip_prefix(WML).is_some_and(|rest| matches!(rest, "header+xml" | "footer+xml" | "footnotes+xml" | "endnotes+xml"))
}

/// A part read as XML: an XML content type, or none with an `.xml` name.
pub(super) fn is_xml(name: &str, kind: Option<&str>) -> bool {
    match kind {
        Some(kind) => kind.ends_with("+xml") || kind == "application/xml" || kind == "text/xml",
        None => name.to_ascii_lowercase().ends_with(".xml"),
    }
}

// ---- Canonical form -----------------------------------------------------------------------------

/// Elements that are tracked changes: their `w:id` is renumbered by every writer and identifies
/// nothing (a revision is known by its author and date).
pub(super) const REVISION_ELEMENTS: &[&str] = &[
    "w:ins",
    "w:del",
    "w:moveFrom",
    "w:moveTo",
    "w:rPrChange",
    "w:pPrChange",
    "w:sectPrChange",
    "w:tblPrChange",
    "w:tblPrExChange",
    "w:trPrChange",
    "w:tcPrChange",
    "w:tblGridChange",
    "w:numberingChange",
    "w:cellIns",
    "w:cellDel",
    "w:cellMerge",
];

/// Bookmark id → name in one part; `None` for an id two bookmarks share.
pub(super) type Bookmarks = HashMap<String, Option<String>>;

/// A story part's bookmarks, each charged as it is kept; the caller releases what it answers held.
pub(super) fn bookmarks(root: &XEl, budget: &mut Budget) -> Result<(Bookmarks, u64), String> {
    fn walk(el: &XEl, out: &mut Bookmarks, held: &mut u64, budget: &mut Budget) -> Result<(), String> {
        for child in el.els() {
            budget.work(1)?;
            if &*child.name == "w:bookmarkStart"
                && let Some(id) = child.attr("w:id")
            {
                match out.get_mut(id) {
                    Some(entry) => *entry = None,
                    None => {
                        let name = child.attr("w:name").unwrap_or("");
                        let bytes = BOOKMARK_BYTES.saturating_add(units(id.len().saturating_add(name.len())));
                        budget.hold(bytes)?;
                        *held = held.saturating_add(bytes);
                        out.insert(id.to_string(), Some(name.to_string()));
                    }
                }
            }
            walk(child, out, held, budget)?;
        }
        Ok(())
    }
    let mut out = Bookmarks::new();
    let mut held = table_room::<(String, Option<String>)>();
    budget.hold(held)?;
    match walk(root, &mut out, &mut held, budget) {
        Ok(()) => Ok((out, held)),
        Err(error) => {
            budget.release(held);
            Err(error)
        }
    }
}

/// How a part's XML is compared: bookkeeping attributes dropped, relationship ids read as what
/// they point at, bookmark ends as their bookmark's name. The form is hashed as it is produced
/// (framed fields, nothing escaped or kept), so its size never costs memory.
pub(super) struct Canon<'a> {
    pub rels: &'a Rels,
    pub bookmarks: Option<&'a Bookmarks>,
    /// embed: the save check run beside a version comparison ([`super::inspect_paired`]): note
    /// ids are not compared, since the comparison pairs notes by reference order.
    pub paired: bool,
}

/// embed: the elements whose `w:id` names a note.
const NOTE_ID_ELEMENTS: [&str; 4] = ["w:footnoteReference", "w:endnoteReference", "w:footnote", "w:endnote"];

/// An attribute's value as compared.
/// Memory held by canonical hashing, one start tag at a time: its attributes in order, in a vector
/// that grows by doubling (old and new room meeting while it moves).
pub(super) const HASHING_BYTES: u64 = 3 * (super::xml::MAX_ATTRIBUTES * std::mem::size_of::<(&'static str, Value<'static>)>()) as u64;

enum Value<'v> {
    Plain(&'v str),
    Rel(&'v str),
    Bookmark(&'v str),
}

impl Canon<'_> {
    /// An attribute's value as compared, or `None` when it is bookkeeping.
    fn value<'v>(element: &str, key: &str, value: &'v str) -> Option<Value<'v>> {
        if key.starts_with("w:rsid") || matches!(key, "w14:paraId" | "w14:textId" | "mc:Ignorable") {
            return None;
        }
        if key == "w:id" {
            if REVISION_ELEMENTS.contains(&element) || element == "w:bookmarkStart" {
                return None;
            }
            if element == "w:bookmarkEnd" {
                return Some(Value::Bookmark(value));
            }
        }
        if key.starts_with("r:") || key == "o:relid" {
            return Some(Value::Rel(value));
        }
        Some(Value::Plain(value))
    }

    /// embed: [`Canon::value`], without note ids when paired.
    fn compared<'v>(&self, element: &str, key: &str, value: &'v str) -> Option<Value<'v>> {
        if self.paired && key == "w:id" && NOTE_ID_ELEMENTS.contains(&element) {
            return None;
        }
        Canon::value(element, key, value)
    }

    fn write_value(&self, value: &Value<'_>, hasher: &mut Hasher) {
        match value {
            Value::Plain(text) => hasher.field(b'=', text.as_bytes()),
            Value::Rel(id) => match self.rels.by_id.get(*id) {
                Some(rel) => {
                    hasher.field(b'R', rel.kind.as_bytes());
                    hasher.field(b'r', rel.target.as_bytes());
                    hasher.field(b'm', if rel.external { b"external" } else { b"internal" });
                }
                None => hasher.field(b'?', id.as_bytes()),
            },
            Value::Bookmark(id) => match self.bookmarks.and_then(|map| map.get(*id)) {
                Some(Some(name)) => hasher.field(b'N', name.as_bytes()),
                Some(None) => hasher.field(b'S', id.as_bytes()),
                None => hasher.field(b'M', id.as_bytes()),
            },
        }
    }

    /// Whether any attribute of `el` is compared.
    pub(super) fn has_attributes(&self, el: &XEl) -> bool {
        el.attrs.iter().any(|(key, value)| self.compared(&el.name, key, value).is_some())
    }

    /// The start tag: the name and the compared attributes, sorted.
    pub(super) fn start_tag(&self, el: &XEl, hasher: &mut Hasher) {
        self.start_tag_of(&el.name, el.attrs.iter(), hasher);
    }

    /// The start tag of an element named `name` with these attributes.
    pub(super) fn start_tag_of<'e>(&self, name: &str, attrs: impl Iterator<Item = &'e (std::rc::Rc<str>, String)>, hasher: &mut Hasher) {
        let mut attributes: Vec<(&str, Value<'_>)> =
            attrs.filter_map(|(key, value)| self.compared(name, key, value).map(|value| (&**key, value))).collect();
        attributes.sort_by(|a, b| a.0.cmp(b.0));
        hasher.field(b'<', name.as_bytes());
        for (key, value) in &attributes {
            hasher.field(b'@', key.as_bytes());
            self.write_value(value, hasher);
        }
        hasher.field(b'!', &[]);
    }

    pub(super) fn element(&self, el: &XEl, hasher: &mut Hasher) {
        self.filtered(el, &|_| true, hasher);
    }

    /// The element with only the children `keep` accepts (its descendants whole).
    pub(super) fn filtered(&self, el: &XEl, keep: &dyn Fn(&XEl) -> bool, hasher: &mut Hasher) {
        self.start_tag(el, hasher);
        let leaf = !el.has_element_kids();
        for node in &el.kids {
            match node {
                XNode::El(child) if keep(child) => self.element(child, hasher),
                XNode::El(_) => {}
                // White space between elements is layout; in a leaf it is the value.
                XNode::Text(text) if !leaf && is_space(text) => {}
                XNode::Text(text) => hasher.field(b'T', text.as_bytes()),
            }
        }
        hasher.field(b'>', el.name.as_bytes());
    }

    /// The digest of the element without the children `keep` refuses, its hashing charged as work.
    pub(super) fn digest(&self, el: &XEl, keep: &dyn Fn(&XEl) -> bool, budget: &mut Budget) -> Result<Digest, String> {
        let mut hasher = Hasher::default();
        self.filtered(el, keep, &mut hasher);
        finish(hasher, budget)
    }

    /// The digest of the start tag alone.
    pub(super) fn start_digest(&self, el: &XEl, budget: &mut Budget) -> Result<Digest, String> {
        let mut hasher = Hasher::default();
        self.start_tag(el, &mut hasher);
        finish(hasher, budget)
    }
}

/// A finished digest, its bytes charged as work (one unit per 64 bytes hashed, at least one).
pub(super) fn finish(hasher: Hasher, budget: &mut Budget) -> Result<Digest, String> {
    let per_unit = if hasher.is_long() { super::budget::BYTES_PER_UNIT_HASHED } else { super::budget::BYTES_PER_UNIT_KEPT };
    budget.work(1 + hasher.length() / per_unit)?;
    Ok(hasher.finish())
}

//! embed: the package structure an inspection compares by meaning: relationships, content types and
//! the canonical form of XML parts.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::budget::{Budget, units};

/// Memory charged for each relationship or content type kept, besides its strings: the strings'
/// own headers and the table entry with its share of the table's spare room.
const REL_BYTES: u64 = 256;
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
        let mut by_id = HashMap::new();
        for node in &root.kids {
            match node {
                XNode::Text(text) if is_space(text) => {}
                XNode::Text(_) => return Err("text in a relationship part".into()),
                XNode::El(el) if &*el.name == "rel:Relationship" => {
                    if el.has_element_kids() || !is_space(&el.text()) {
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
                    // Kept for the whole inspection: the strings, and the entry with its share of the
                    // table's room.
                    budget.hold(REL_BYTES.saturating_add(units(id.len().saturating_add(kind.len()).saturating_add(target.len()))))?;
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

    pub(super) fn set(&self) -> BTreeSet<&Rel> {
        self.by_id.values().collect()
    }

    pub(super) fn of_type<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Rel> + 'a {
        self.by_id.values().filter(move |rel| is_type(&rel.kind, kind))
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
                    if el.has_element_kids() || !is_space(&el.text()) || el.attrs.iter().any(|(key, _)| &**key != key_name && &**key != "ContentType")
                    {
                        return Err("an unexpected content type entry".into());
                    }
                    let (Some(key), Some(kind)) = (el.attr(key_name), el.attr("ContentType")) else {
                        return Err("a content type entry without its attributes".into());
                    };
                    let key = key.strip_prefix('/').unwrap_or(key).to_lowercase();
                    budget.hold(REL_BYTES.saturating_add(units(key.len().saturating_add(kind.len()))))?;
                    budget.work(40 + units(key.len()) / 4)?;
                    if map.insert(key.clone(), kind.to_string()).is_some() {
                        return Err(format!("two content types for {key}"));
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

pub(super) fn bookmarks(root: &XEl) -> Bookmarks {
    fn walk(el: &XEl, out: &mut Bookmarks) {
        for child in el.els() {
            if &*child.name == "w:bookmarkStart"
                && let Some(id) = child.attr("w:id")
            {
                let name = child.attr("w:name").unwrap_or("").to_string();
                out.entry(id.to_string()).and_modify(|entry| *entry = None).or_insert(Some(name));
            }
            walk(child, out);
        }
    }
    let mut out = Bookmarks::new();
    walk(root, &mut out);
    out
}

/// How a part's XML is compared: bookkeeping attributes dropped, relationship ids read as what
/// they point at, bookmark ends as their bookmark's name. The form is hashed as it is produced
/// (framed fields, nothing escaped or kept), so its size never costs memory.
pub(super) struct Canon<'a> {
    pub rels: &'a Rels,
    pub bookmarks: Option<&'a Bookmarks>,
}

/// An attribute's value as compared.
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
        el.attrs.iter().any(|(key, value)| Canon::value(&el.name, key, value).is_some())
    }

    /// The start tag: the name and the compared attributes, sorted.
    pub(super) fn start_tag(&self, el: &XEl, hasher: &mut Hasher) {
        let mut attributes: Vec<(&str, Value<'_>)> =
            el.attrs.iter().filter_map(|(key, value)| Canon::value(&el.name, key, value).map(|value| (&**key, value))).collect();
        attributes.sort_by(|a, b| a.0.cmp(b.0));
        hasher.field(b'<', el.name.as_bytes());
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

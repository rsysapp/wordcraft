//! OPC package access: bounded zip reading, relationships, part names.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};

use crate::DocxError;
use crate::xml::{self, El};

/// Largest single part we decompress.
pub const MAX_PART: u64 = 256 * 1024 * 1024;
/// Largest total of all decompressed parts.
// embed: also the largest archive a read keeps (`SOURCE_PACKAGE`), so a read holds at most twice this.
pub const MAX_TOTAL: u64 = 512 * 1024 * 1024;
/// Most zip entries we look at.
pub const MAX_ENTRIES: usize = 20_000;

/// Relationship type suffixes (after `.../relationships/`).
pub mod rt {
    pub const OFFICE_DOC: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
    pub const CORE: &str = "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";
    pub const APP: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties";
    pub const CUSTOM: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties";
    pub const STYLES: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
    pub const NUMBERING: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
    pub const SETTINGS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings";
    pub const THEME: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme";
    pub const FOOTNOTES: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
    pub const ENDNOTES: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes";
    pub const COMMENTS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
    pub const COMMENTS_EX: &str = "http://schemas.microsoft.com/office/2011/relationships/commentsExtended";
    pub const HEADER: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
    pub const FOOTER: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
    pub const IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
    pub const HYPERLINK: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
}

/// Does relationship type `t` end with `suffix` (ignoring the transitional/strict prefix)?
pub fn rel_is(t: &str, full: &str) -> bool {
    let tail = |s: &str| s.rsplit('/').next().unwrap_or(s).to_string();
    t == full || tail(t) == tail(full)
}

/// embed: how much of a package [`crate::read_with_limits`] may decompress and parse. A caller with
/// less memory than the desktop (a browser tab) passes tighter limits; the defaults are [`crate::read`]'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadLimits {
    /// Largest single part decompressed, in bytes.
    pub max_part_bytes: u64,
    /// Largest total of all decompressed parts, in bytes. embed: also the largest archive a read
    /// accepts, since the document keeps the archive's bytes (`SOURCE_PACKAGE`) for its lifetime;
    /// the archive is capped on its own, so a read holds at most twice this.
    pub max_total_bytes: u64,
    /// Most XML elements parsed across every part of the package (each part also stays within the
    /// parser's own per-part limit). Every parse counts, so a part read twice counts twice.
    pub max_elements: usize,
    /// Deepest XML element nesting in any part, at most the parser's own limit. The reader recurses
    /// once per level, so this bounds its stack use: a caller with a small stack (WebAssembly's
    /// 1 MiB) passes less than the default.
    pub max_depth: usize,
}

impl Default for ReadLimits {
    fn default() -> Self {
        ReadLimits { max_part_bytes: MAX_PART, max_total_bytes: MAX_TOTAL, max_elements: usize::MAX, max_depth: xml::MAX_DEPTH }
    }
}

/// The decompressed parts of a package (bounded).
pub struct Package {
    files: BTreeMap<String, Vec<u8>>,
    /// embed: elements `xml` may still parse, how deep they may nest, and whether a parse went past
    /// either limit. The reader skips a broken secondary part, so a limit reached is recorded here
    /// and refused at the end.
    elements_left: Cell<usize>,
    max_depth: usize,
    limit_reached: Cell<bool>,
}

impl Package {
    // embed: the reader opens through `open_with`; these two remain for the tests below.
    #[cfg(test)]
    pub fn open(bytes: &[u8]) -> Result<Package, DocxError> {
        Package::open_with(bytes, &ReadLimits::default())
    }

    #[cfg(test)]
    pub fn open_limited(bytes: &[u8], max_part: u64, max_total: u64) -> Result<Package, DocxError> {
        Package::open_with(bytes, &ReadLimits { max_part_bytes: max_part, max_total_bytes: max_total, ..ReadLimits::default() })
    }

    pub fn open_with(bytes: &[u8], limits: &ReadLimits) -> Result<Package, DocxError> {
        let (max_part, max_total) = (limits.max_part_bytes, limits.max_total_bytes);
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| DocxError::Zip(e.to_string()))?;
        if zip.len() > MAX_ENTRIES {
            return Err(DocxError::Limit(format!("{} zip entries", zip.len())));
        }
        let mut files = BTreeMap::new();
        let mut total: u64 = 0;
        for i in 0..zip.len() {
            let mut f = match zip.by_index(i) {
                Ok(f) => f,
                Err(e) => {
                    log::warn!("docx: skipping unreadable zip entry {i}: {e}");
                    continue;
                }
            };
            if f.is_dir() {
                continue;
            }
            let name = f.name().trim_start_matches('/').to_string();
            if f.size() > max_part {
                return Err(DocxError::Limit(format!("part {name} declares {} bytes", f.size())));
            }
            let room = max_part.min(max_total.saturating_sub(total));
            let mut buf = Vec::with_capacity(f.size().min(16 * 1024 * 1024) as usize);
            let read = (&mut f).take(room + 1).read_to_end(&mut buf);
            if let Err(e) = read {
                // A corrupt entry: keep going unless it's a part we can't do without.
                log::warn!("docx: zip entry {name} unreadable: {e}");
                continue;
            }
            if buf.len() as u64 > room {
                return Err(DocxError::Limit(format!("part {name} decompresses past the size limit")));
            }
            total = total.saturating_add(buf.len() as u64);
            files.insert(name, buf);
        }
        Ok(Package { files, elements_left: Cell::new(limits.max_elements), max_depth: limits.max_depth, limit_reached: Cell::new(false) })
    }

    /// embed: whether a part was refused because the package's element budget ran out or it nested
    /// deeper than a caller's tighter depth limit.
    pub fn limit_reached(&self) -> bool {
        self.limit_reached.get()
    }

    /// embed: a part's position among the package's part names (sorted), for paragraph origins:
    /// the exact name first, then one differing only in case, as [`Package::get`] finds parts.
    pub fn part_index(&self, name: &str) -> Option<u32> {
        let name = name.trim_start_matches('/');
        let position = self.files.keys().position(|key| key == name).or_else(|| self.files.keys().position(|key| key.eq_ignore_ascii_case(name)))?;
        u32::try_from(position).ok()
    }

    /// embed: the part name at `index` (see [`Package::part_index`]).
    pub fn part_name(&self, index: u32) -> Option<&str> {
        self.files.keys().nth(usize::try_from(index).ok()?).map(String::as_str)
    }

    /// Part bytes by name (leading `/` ignored; case-insensitive fallback).
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        let n = name.trim_start_matches('/');
        if let Some(v) = self.files.get(n) {
            return Some(v);
        }
        self.files.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.as_slice())
    }

    pub fn xml(&self, name: &str) -> Result<Option<El>, DocxError> {
        // embed: the budget is shared with `tree`.
        self.parse_within(name, xml::parse_limited)
    }

    /// embed: a part's positional tree (the preserving writer's scan), parsed within the same limits
    /// and the same element budget as [`Package::xml`] (what it counts is spent, and a limit reached is
    /// recorded), and within `max_bytes` of memory, charged as the tree is built.
    pub(crate) fn tree(&self, name: &str, max_bytes: u64) -> Result<Option<crate::preserve::tree::Parsed>, DocxError> {
        self.parse_within(name, |bytes, records, depth| crate::preserve::tree::parse_charged(bytes, records, depth, max_bytes))
    }

    /// embed: parse part `name` with `parse` (given the bytes, the elements it may count and the
    /// depth it may nest), spending what it counted from the package's budget.
    fn parse_within<T>(&self, name: &str, parse: impl FnOnce(&[u8], usize, usize) -> Result<(T, usize), DocxError>) -> Result<Option<T>, DocxError> {
        let Some(b) = self.get(name) else { return Ok(None) };
        // embed: one element budget across the package, so many parts cannot each fill the per-part limit.
        let left = self.elements_left.get();
        match parse(b, left.min(xml::MAX_ELEMENTS), self.max_depth) {
            Ok((root, used)) => {
                self.elements_left.set(left.saturating_sub(used));
                Ok(Some(root))
            }
            Err(e) => {
                // Only the caller's tighter limits refuse the file; past the parser's own, a
                // secondary part is skipped as before.
                let spent = left < xml::MAX_ELEMENTS && matches!(&e, DocxError::Limit(m) if m == xml::TOO_MANY_ELEMENTS);
                let too_deep = self.max_depth < xml::MAX_DEPTH && matches!(&e, DocxError::Limit(m) if m == xml::TOO_DEEP);
                if spent || too_deep {
                    self.limit_reached.set(true);
                }
                Err(e)
            }
        }
    }

    /// Relationships of `part` (from `dir/_rels/name.rels`).
    pub fn rels(&self, part: &str) -> Rels {
        let (dir, file) = split_dir(part);
        let path = if dir.is_empty() { format!("_rels/{file}.rels") } else { format!("{dir}/_rels/{file}.rels") };
        let mut rels = Rels::default();
        let Ok(Some(root)) = self.xml(&path) else { return rels };
        for r in root.els().filter(|e| e.local() == "Relationship") {
            let (Some(id), Some(target)) = (r.attr("Id"), r.attr("Target")) else { continue };
            let external = r.attr("TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("External"));
            let resolved = if external { target.to_string() } else { resolve(dir, target) };
            rels.list.push(Rel { id: id.to_string(), kind: r.attr("Type").unwrap_or("").to_string(), target: resolved, external });
        }
        rels
    }
}

#[derive(Clone, Debug)]
pub struct Rel {
    pub id: String,
    pub kind: String,
    /// Package path (internal) or URL (external).
    pub target: String,
    pub external: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Rels {
    pub list: Vec<Rel>,
}

impl Rels {
    pub fn by_id(&self, id: &str) -> Option<&Rel> {
        self.list.iter().find(|r| r.id == id)
    }
    pub fn by_type(&self, full: &str) -> Option<&Rel> {
        self.list.iter().find(|r| rel_is(&r.kind, full))
    }
}

fn split_dir(part: &str) -> (&str, &str) {
    let p = part.trim_start_matches('/');
    match p.rfind('/') {
        Some(i) => (p.get(..i).unwrap_or(""), p.get(i + 1..).unwrap_or(p)),
        None => ("", p),
    }
}

/// Resolve a relative target against a source directory into a package path.
pub fn resolve(dir: &str, target: &str) -> String {
    let target = target.split('#').next().unwrap_or(target);
    let mut parts: Vec<&str> = if target.starts_with('/') { Vec::new() } else { dir.split('/').filter(|s| !s.is_empty()).collect() };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Build a zip from (name, bytes) entries.
pub fn zip_entries(entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>, DocxError> {
    let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).compression_level(Some(6));
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in entries {
        // Already-compressed media gain nothing from deflate.
        let lower = name.to_ascii_lowercase();
        let o = if [".png", ".jpg", ".jpeg", ".gif", ".webp"].iter().any(|e| lower.ends_with(e)) { stored } else { opts };
        zw.start_file(name.as_str(), o).map_err(|e| DocxError::Zip(e.to_string()))?;
        zw.write_all(bytes).map_err(|e| DocxError::Zip(e.to_string()))?;
    }
    let c = zw.finish().map_err(|e| DocxError::Zip(e.to_string()))?;
    Ok(c.into_inner())
}

/// embed: the archive's file names in the order they are stored (folders left out, each name
/// once), so a writer that keeps the package keeps its order too.
#[cfg_attr(not(test), expect(dead_code, reason = "embed: read by the preserving writer, which keeps the source package's order"))]
pub fn entry_order(bytes: &[u8]) -> Result<Vec<String>, DocxError> {
    let zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| DocxError::Zip(e.to_string()))?;
    let mut seen = std::collections::HashSet::new();
    let mut names = Vec::new();
    for index in 0..zip.len().min(MAX_ENTRIES) {
        let Some(name) = zip.name_for_index(index) else { continue };
        if name.ends_with('/') {
            continue;
        }
        let name = name.trim_start_matches('/').to_string();
        if seen.insert(name.clone()) {
            names.push(name);
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_of(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let e: Vec<(String, Vec<u8>)> = entries.iter().map(|(n, b)| (n.to_string(), b.clone())).collect();
        zip_entries(&e).unwrap()
    }

    #[test]
    fn decompression_bomb_is_rejected() {
        let z = zip_of(&[("word/document.xml", vec![b' '; 2_000_000])]);
        assert!(z.len() < 100_000);
        assert!(matches!(Package::open_limited(&z, 1_000_000, 10_000_000), Err(DocxError::Limit(_))));
        assert!(matches!(Package::open_limited(&z, 10_000_000, 1_000_000), Err(DocxError::Limit(_))));
        assert!(Package::open_limited(&z, 10_000_000, 10_000_000).is_ok());
    }

    #[test]
    fn lying_declared_size_is_rejected_or_bounded() {
        let mut z = zip_of(&[("a.xml", b"<a/>".to_vec())]);
        // Patch the uncompressed size in the central directory and local header to ~4 GB.
        let mut patched = 0;
        for sig in [[0x50u8, 0x4B, 0x01, 0x02], [0x50, 0x4B, 0x03, 0x04]] {
            if let Some(pos) = z.windows(4).position(|w| w == sig) {
                let off = if sig[2] == 1 { pos + 24 } else { pos + 22 };
                z[off..off + 4].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
                patched += 1;
            }
        }
        assert_eq!(patched, 2);
        match Package::open(&z) {
            Err(_) => {}
            Ok(p) => assert!(p.get("a.xml").is_none_or(|b| b.len() < 1024)),
        }
    }

    // embed: parts are numbered by their sorted names; the archive's own order is kept apart.
    #[test]
    fn numbers_parts_by_sorted_name_and_lists_entries_in_stored_order() {
        let z = zip_of(&[("word/document.xml", b"<a/>".to_vec()), ("[Content_Types].xml", b"<b/>".to_vec()), ("Word/Header1.xml", b"<c/>".to_vec())]);
        let package = Package::open(&z).unwrap();
        // Byte order: upper case before `[`, `[` before lower case.
        assert_eq!(package.part_index("[Content_Types].xml"), Some(1));
        assert_eq!(package.part_index("/word/document.xml"), Some(2));
        // A name differing only in case finds the part, as `get` does.
        assert_eq!(package.part_index("word/header1.xml"), Some(0));
        assert_eq!(package.part_index("word/missing.xml"), None);
        assert_eq!(package.part_name(0), Some("Word/Header1.xml"));
        assert_eq!(package.part_name(3), None);
        assert_eq!(entry_order(&z).unwrap(), ["word/document.xml", "[Content_Types].xml", "Word/Header1.xml"]);
    }

    #[test]
    fn resolves_targets() {
        assert_eq!(resolve("word", "media/image1.png"), "word/media/image1.png");
        assert_eq!(resolve("word", "../customXml/item1.xml"), "customXml/item1.xml");
        assert_eq!(resolve("word", "/word/styles.xml"), "word/styles.xml");
        assert_eq!(resolve("", "word/document.xml"), "word/document.xml");
        assert_eq!(resolve("word", "../../../x"), "x");
    }
}

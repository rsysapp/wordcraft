//! embed: byte blobs a document holds by name: the source-format parts it keeps verbatim and its
//! media. A document read from a Word package keeps the whole package among the kept parts, and its
//! pictures can be most of the file, so printing never shows the bytes and equality looks at them
//! only when it must.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

/// Byte blobs by name. Reads and writes go through the map it dereferences to.
#[derive(Clone, Default)]
pub struct NamedBytes(BTreeMap<String, Arc<Vec<u8>>>);

/// The source-format parts a document keeps verbatim, by part name.
pub type Passthrough = NamedBytes;

/// A document's media (pictures), by key.
pub type Media = NamedBytes;

impl NamedBytes {
    pub fn new() -> Self {
        NamedBytes(BTreeMap::new())
    }
}

impl<'a> IntoIterator for &'a NamedBytes {
    type Item = (&'a String, &'a Arc<Vec<u8>>);
    type IntoIter = std::collections::btree_map::Iter<'a, String, Arc<Vec<u8>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl Deref for NamedBytes {
    type Target = BTreeMap<String, Arc<Vec<u8>>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for NamedBytes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Equal names, and for each the same shared bytes, else the same length, else the same bytes: a
/// document and its clones (undo snapshots, the renderer's copy) compare without reading them, and
/// two separate reads, which rarely meet, compare the bytes once.
impl PartialEq for NamedBytes {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self.0.iter().zip(other.0.iter()).all(|((name, bytes), (other_name, other_bytes))| {
                name == other_name && (Arc::ptr_eq(bytes, other_bytes) || (bytes.len() == other_bytes.len() && bytes == other_bytes))
            })
    }
}

/// Names and lengths only: a kept package is as large as the file, and pictures nearly so.
impl fmt::Debug for NamedBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.0.iter().map(|(name, bytes)| (name, Length(bytes.len())))).finish()
    }
}

struct Length(usize);

impl fmt::Debug for Length {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} bytes", self.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn kept(bytes: &[u8]) -> Passthrough {
        let mut parts = Passthrough::new();
        parts.insert("package".into(), Arc::new(bytes.to_vec()));
        parts
    }

    #[test]
    fn prints_lengths_not_bytes() {
        assert_eq!(format!("{:?}", kept(&[7; 4096])), r#"{"package": 4096 bytes}"#);
    }

    /// A document's pictures can be most of a 100 MB file: printing it names them only.
    #[test]
    fn a_document_prints_its_media_by_name_and_length() {
        let mut doc = crate::Document::new();
        doc.media.insert("image1.png".into(), Arc::new(vec![7; 1 << 20]));
        let printed = format!("{doc:?}");
        assert!(printed.len() < 100_000, "{} characters", printed.len());
        assert!(printed.contains(r#"media: {"image1.png": 1048576 bytes}"#), "{printed}");
    }

    #[test]
    fn compares_shared_then_equal_bytes() {
        let first = kept(b"one");
        assert_eq!(first.clone(), first);
        assert_eq!(kept(b"one"), first);
        assert_ne!(kept(b"two"), first);
        assert_ne!(kept(b"longer"), first);
        let mut renamed = Passthrough::new();
        renamed.insert("other".into(), Arc::new(b"one".to_vec()));
        assert_ne!(renamed, first);
        assert_ne!(Passthrough::new(), first);
    }
}

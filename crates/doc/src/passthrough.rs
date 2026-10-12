//! embed: the source-format parts a document keeps verbatim, by name. A document read from a Word
//! package keeps the whole package here too, which can be as large as the file, so printing never
//! shows the bytes and equality looks at them only when it must.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

/// Kept parts by name. Reads and writes go through the map it dereferences to.
#[derive(Clone, Default)]
pub struct Passthrough(BTreeMap<String, Arc<Vec<u8>>>);

impl Passthrough {
    pub fn new() -> Self {
        Passthrough(BTreeMap::new())
    }
}

impl Deref for Passthrough {
    type Target = BTreeMap<String, Arc<Vec<u8>>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Passthrough {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Equal names, and for each the same shared bytes, else the same length, else the same bytes: a
/// document and its clones (undo snapshots, the renderer's copy) compare without reading them, and
/// two separate reads, which rarely meet, compare the bytes once.
impl PartialEq for Passthrough {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self.0.iter().zip(other.0.iter()).all(|((name, bytes), (other_name, other_bytes))| {
                name == other_name && (Arc::ptr_eq(bytes, other_bytes) || (bytes.len() == other_bytes.len() && bytes == other_bytes))
            })
    }
}

/// Names and lengths only: a kept package is as large as the file.
impl fmt::Debug for Passthrough {
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

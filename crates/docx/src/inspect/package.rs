//! embed: a strict reading of a zip package for [`super::inspect`]. The desktop reader skips an entry
//! it cannot read and lets a later entry replace an earlier one of the same name; an inspection
//! cannot, because whatever it does not read could be what another reader shows. Every entry must
//! be readable, named once, and found where the central directory and its local header agree.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use zip::ZipArchive;

use crate::package::{MAX_ENTRIES, ReadLimits};

/// A package whose every entry was read once, in full, within the limits.
pub(super) struct StrictPackage<'a> {
    archive: ZipArchive<Cursor<&'a [u8]>>,
    /// Part name → zip index.
    index: BTreeMap<String, usize>,
    max_part: u64,
}

const CENTRAL_HEADER: [u8; 4] = [0x50, 0x4B, 0x01, 0x02];
const LOCAL_HEADER: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];

impl<'a> StrictPackage<'a> {
    pub(super) fn open(bytes: &'a [u8], limits: &ReadLimits) -> Result<Self, String> {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(|error| format!("not a readable zip package: {error}"))?;
        if archive.len() > MAX_ENTRIES {
            return Err(format!("{} zip entries", archive.len()));
        }
        // Bytes before the archive or after its directory are where two readers part ways.
        if archive.offset() != 0 {
            return Err("data before the zip archive".into());
        }
        if !archive.comment().is_empty() || archive.zip64_comment().is_some_and(|comment| !comment.is_empty()) {
            return Err("a zip archive comment".into());
        }
        // The zip crate keeps the last of two entries with one name; count the directory itself.
        let records = central_records(bytes, archive.central_directory_start())?;
        if records != archive.len() {
            return Err("two zip entries with one name".into());
        }
        let mut index = BTreeMap::new();
        let mut folded = BTreeMap::new();
        let mut total: u64 = 0;
        for position in 0..archive.len() {
            let mut entry = archive.by_index(position).map_err(|error| format!("zip entry {position} unreadable: {error}"))?;
            let name = std::str::from_utf8(entry.name_raw()).map_err(|_| format!("zip entry {position} has a name that is not UTF-8"))?.to_string();
            if entry.encrypted() {
                return Err(format!("zip entry {name} is encrypted"));
            }
            let start = usize::try_from(entry.header_start()).map_err(|_| format!("zip entry {name} starts past the file"))?;
            if local_name(bytes, start) != Some(entry.name_raw()) {
                return Err(format!("zip entry {name} has another name in its local header"));
            }
            if let Some(folder) = name.strip_suffix('/') {
                if entry.size() != 0 || folder.is_empty() {
                    return Err(format!("zip folder entry {name} holds data"));
                }
                continue;
            }
            check_name(&name)?;
            if entry.size() > limits.max_part_bytes {
                return Err(format!("part {name} declares {} bytes", entry.size()));
            }
            let room = limits.max_part_bytes.min(limits.max_total_bytes.saturating_sub(total));
            // Read to the end, so the zip crate checks the CRC, without keeping the bytes yet.
            let read = std::io::copy(&mut (&mut entry).take(room.saturating_add(1)), &mut std::io::sink())
                .map_err(|error| format!("zip entry {name} unreadable: {error}"))?;
            if read > room {
                return Err(format!("part {name} decompresses past the size limit"));
            }
            total = total.saturating_add(read);
            // Part names are compared without case (ECMA-376 Part 2, 9.1.1.1).
            if folded.insert(name.to_lowercase(), ()).is_some() {
                return Err(format!("two parts named {name} apart from case"));
            }
            index.insert(name, position);
        }
        Ok(StrictPackage { archive, index, max_part: limits.max_part_bytes })
    }

    pub(super) fn names(&self) -> impl Iterator<Item = &str> {
        self.index.keys().map(String::as_str)
    }

    pub(super) fn has(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }

    /// A part's bytes, by its exact name.
    pub(super) fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        let Some(position) = self.index.get(name).copied() else { return Ok(None) };
        let mut entry = self.archive.by_index(position).map_err(|error| format!("zip entry {name} unreadable: {error}"))?;
        let capacity = usize::try_from(entry.size().min(self.max_part).min(16 << 20)).unwrap_or(0);
        let mut bytes = Vec::with_capacity(capacity);
        (&mut entry)
            .take(self.max_part.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| format!("zip entry {name} unreadable: {error}"))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > self.max_part {
            return Err(format!("part {name} decompresses past the size limit"));
        }
        Ok(Some(bytes))
    }
}

/// How many records the central directory holds, read from its own headers.
fn central_records(bytes: &[u8], start: u64) -> Result<usize, String> {
    let mut at = usize::try_from(start).map_err(|_| "the zip directory starts past the file".to_string())?;
    let mut records = 0usize;
    while bytes.get(at..at.saturating_add(4)) == Some(&CENTRAL_HEADER[..]) {
        let field = |offset: usize| -> Option<usize> {
            let low = *bytes.get(at.checked_add(offset)?)?;
            let high = *bytes.get(at.checked_add(offset)?.checked_add(1)?)?;
            Some(usize::from(u16::from_le_bytes([low, high])))
        };
        let (Some(name), Some(extra), Some(comment)) = (field(28), field(30), field(32)) else {
            return Err("a cut zip directory".into());
        };
        at = at
            .checked_add(46)
            .and_then(|next| next.checked_add(name))
            .and_then(|next| next.checked_add(extra))
            .and_then(|next| next.checked_add(comment))
            .ok_or("a cut zip directory")?;
        records = records.saturating_add(1);
        if records > MAX_ENTRIES {
            return Err("too many zip entries".into());
        }
    }
    Ok(records)
}

/// The name a local file header gives its entry.
fn local_name(bytes: &[u8], start: usize) -> Option<&[u8]> {
    if bytes.get(start..start.checked_add(4)?)? != LOCAL_HEADER {
        return None;
    }
    let field = |offset: usize| -> Option<usize> {
        let low = *bytes.get(start.checked_add(offset)?)?;
        let high = *bytes.get(start.checked_add(offset)?.checked_add(1)?)?;
        Some(usize::from(u16::from_le_bytes([low, high])))
    };
    let length = field(26)?;
    let from = start.checked_add(30)?;
    bytes.get(from..from.checked_add(length)?)
}

/// A part name another reader could only take the same way: relative, slash-separated, no empty or
/// dot segments, no backslash or control characters.
fn check_name(name: &str) -> Result<(), String> {
    let bad = name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.chars().any(char::is_control)
        || name.split('/').any(|segment| segment.is_empty() || segment == "." || segment == "..");
    if bad { Err(format!("a zip entry named {name:?}")) } else { Ok(()) }
}

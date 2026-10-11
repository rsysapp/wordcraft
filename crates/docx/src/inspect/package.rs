//! embed: a strict reading of a zip package for [`super::inspect`]. The desktop reader skips an entry
//! it cannot read and lets a later entry replace an earlier one of the same name; an inspection
//! cannot, because whatever it does not read could be what another reader shows. Every entry must
//! be readable, named once, and described alike by the central directory and its local header
//! (name, flags, method, CRC and sizes); the entries must fill the file from its first byte to the
//! directory, and the directory run to the end record, so there are no bytes another reader could
//! take for an entry this one does not see.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use zip::ZipArchive;

use super::InspectError;
use super::budget::{Budget, units};
use crate::package::{MAX_ENTRIES, ReadLimits};

/// A package whose every entry was read once, in full, within the limits.
pub(super) struct StrictPackage<'a> {
    archive: ZipArchive<Cursor<&'a [u8]>>,
    /// Part name → zip index and decompressed size.
    index: BTreeMap<String, (usize, u64)>,
    max_part: u64,
}

const CENTRAL_HEADER: [u8; 4] = [0x50, 0x4B, 0x01, 0x02];
const LOCAL_HEADER: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];
const DESCRIPTOR: [u8; 4] = [0x50, 0x4B, 0x07, 0x08];
const END_RECORD: [u8; 4] = [0x50, 0x4B, 0x05, 0x06];
const ZIP64_END_RECORD: [u8; 4] = [0x50, 0x4B, 0x06, 0x06];
const ZIP64_LOCATOR: [u8; 4] = [0x50, 0x4B, 0x06, 0x07];
/// General-purpose flags: encrypted, and sizes in a data descriptor after the data.
const ENCRYPTED: u16 = 0x0001;
const DESCRIBED_AFTER: u16 = 0x0008;
/// Extra-field ids: ZIP64 sizes, and Info-ZIP's second spelling of the name.
const ZIP64_EXTRA: u16 = 0x0001;
const UNICODE_PATH: u16 = 0x7075;
/// Memory charged for each entry indexed.
const ENTRY_BYTES: u64 = 192;

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let slice = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes(<[u8; 2]>::try_from(slice).ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(<[u8; 4]>::try_from(slice).ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    let slice = bytes.get(at..at.checked_add(8)?)?;
    Some(u64::from_le_bytes(<[u8; 8]>::try_from(slice).ok()?))
}

/// An entry as one header describes it.
#[derive(Debug, PartialEq, Eq)]
struct Described<'b> {
    name: &'b [u8],
    flags: u16,
    method: u16,
    crc: u32,
    compressed: u64,
    size: u64,
}

/// The ZIP64 values an extra field supplies for the 32-bit fields set to all ones, in order
/// (size, compressed size, offset). A second spelling of the name is refused.
fn zip64(extra: &[u8], wanted: [bool; 3]) -> Result<[Option<u64>; 3], String> {
    let cut = || "a cut zip extra field".to_string();
    let mut at = 0usize;
    let mut found = None;
    while at < extra.len() {
        let (Some(id), Some(length)) = (u16_at(extra, at), u16_at(extra, at.saturating_add(2))) else { return Err(cut()) };
        let start = at.saturating_add(4);
        let end = start.saturating_add(usize::from(length));
        let body = extra.get(start..end).ok_or_else(cut)?;
        if id == UNICODE_PATH {
            return Err("a zip entry with a second name".into());
        }
        if id == ZIP64_EXTRA {
            if found.is_some() {
                return Err("two ZIP64 fields in one header".into());
            }
            found = Some(body);
        }
        at = end;
    }
    let mut out = [None; 3];
    let mut read = 0usize;
    for (slot, want) in out.iter_mut().zip(wanted) {
        if want {
            let body = found.ok_or("ZIP64 sizes without their field")?;
            *slot = Some(u64_at(body, read).ok_or("a cut ZIP64 field")?);
            read = read.saturating_add(8);
        }
    }
    Ok(out)
}

/// A field of a header at `at + offset`.
struct Header<'b> {
    bytes: &'b [u8],
    at: usize,
}

impl<'b> Header<'b> {
    fn u16(&self, offset: usize) -> Result<u16, String> {
        u16_at(self.bytes, self.at.saturating_add(offset)).ok_or_else(|| "a cut zip header".to_string())
    }
    fn u32(&self, offset: usize) -> Result<u32, String> {
        u32_at(self.bytes, self.at.saturating_add(offset)).ok_or_else(|| "a cut zip header".to_string())
    }
    fn slice(&self, offset: usize, length: usize) -> Result<&'b [u8], String> {
        let start = self.at.saturating_add(offset);
        self.bytes.get(start..start.saturating_add(length)).ok_or_else(|| "a cut zip header".to_string())
    }
}

/// One central directory record: what it describes, its local header's offset, and where it ends.
fn central_record(bytes: &[u8], at: usize) -> Result<(Described<'_>, u64, usize), String> {
    let header = Header { bytes, at };
    let (flags, method, crc) = (header.u16(8)?, header.u16(10)?, header.u32(16)?);
    let (compressed, size) = (header.u32(20)?, header.u32(24)?);
    let (name_length, extra_length, comment_length) = (usize::from(header.u16(28)?), usize::from(header.u16(30)?), usize::from(header.u16(32)?));
    let (disk, offset) = (header.u16(34)?, header.u32(42)?);
    let name = header.slice(46, name_length)?;
    let extra = header.slice(46usize.saturating_add(name_length), extra_length)?;
    if disk != 0 && disk != 0xFFFF {
        return Err("a zip archive on several disks".into());
    }
    if comment_length != 0 {
        return Err("a zip entry comment".into());
    }
    let [wide_size, wide_compressed, wide_offset] = zip64(extra, [size == u32::MAX, compressed == u32::MAX, offset == u32::MAX])?;
    let end = at.saturating_add(46).saturating_add(name_length).saturating_add(extra_length);
    let described = Described {
        name,
        flags,
        method,
        crc,
        compressed: wide_compressed.unwrap_or(u64::from(compressed)),
        size: wide_size.unwrap_or(u64::from(size)),
    };
    Ok((described, wide_offset.unwrap_or(u64::from(offset)), end))
}

/// The local header at `at`: what it describes, and where its data starts.
fn local_record(bytes: &[u8], at: usize) -> Result<(Described<'_>, usize), String> {
    let header = Header { bytes, at };
    if header.slice(0, 4)? != LOCAL_HEADER {
        return Err("a zip entry whose local header is missing".into());
    }
    let (flags, method, crc) = (header.u16(6)?, header.u16(8)?, header.u32(14)?);
    let (compressed, size) = (header.u32(18)?, header.u32(22)?);
    let (name_length, extra_length) = (usize::from(header.u16(26)?), usize::from(header.u16(28)?));
    let name = header.slice(30, name_length)?;
    let extra = header.slice(30usize.saturating_add(name_length), extra_length)?;
    let [wide_size, wide_compressed, _] = zip64(extra, [size == u32::MAX, compressed == u32::MAX, false])?;
    let described = Described {
        name,
        flags,
        method,
        crc,
        compressed: wide_compressed.unwrap_or(u64::from(compressed)),
        size: wide_size.unwrap_or(u64::from(size)),
    };
    Ok((described, at.saturating_add(30).saturating_add(name_length).saturating_add(extra_length)))
}

/// The data descriptor at `at` for an entry the directory describes as `central`; its length.
fn descriptor(bytes: &[u8], at: usize, central: &Described<'_>) -> Result<usize, String> {
    let signed = bytes.get(at..at.saturating_add(4)) == Some(&DESCRIPTOR[..]);
    let header = Header { bytes, at: if signed { at.saturating_add(4) } else { at } };
    let wide = central.compressed >= u64::from(u32::MAX) || central.size >= u64::from(u32::MAX);
    let crc = header.u32(0)?;
    let (compressed, size, length) = if wide {
        let wide_field = |offset: usize| u64_at(bytes, header.at.saturating_add(offset)).ok_or_else(|| "a cut zip data descriptor".to_string());
        (wide_field(4)?, wide_field(12)?, 20)
    } else {
        (u64::from(header.u32(4)?), u64::from(header.u32(8)?), 12)
    };
    if (crc, compressed, size) != (central.crc, central.compressed, central.size) {
        return Err("a zip data descriptor that disagrees with the directory".into());
    }
    Ok(length + if signed { 4 } else { 0 })
}

/// Check every entry against its local header, and that entries and directory fill the file.
/// Answers how many entries the directory holds.
fn check_layout(bytes: &[u8], directory: u64, budget: &mut Budget) -> Result<usize, String> {
    let directory = usize::try_from(directory).map_err(|_| "the zip directory starts past the file".to_string())?;
    let mut at = directory;
    let mut extents: Vec<(usize, usize)> = Vec::new();
    while bytes.get(at..at.saturating_add(4)) == Some(&CENTRAL_HEADER[..]) {
        budget.work(1)?;
        budget.hold(32)?;
        let (central, offset, end) = central_record(bytes, at)?;
        let offset = usize::try_from(offset).map_err(|_| "a zip entry past the file".to_string())?;
        let (local, data) = local_record(bytes, offset)?;
        if central.flags & ENCRYPTED != 0 {
            return Err("an encrypted zip entry".into());
        }
        if !matches!(central.method, 0 | 8) {
            return Err("a zip entry compressed another way than stored or deflated".into());
        }
        let after = central.flags & DESCRIBED_AFTER != 0;
        // With a descriptor the local header may leave the CRC and sizes as zero.
        let agrees = local.name == central.name
            && local.flags == central.flags
            && local.method == central.method
            && (after || (local.crc, local.compressed, local.size) == (central.crc, central.compressed, central.size));
        if !agrees {
            return Err(format!("zip entry {} described otherwise by its local header", String::from_utf8_lossy(central.name)));
        }
        let compressed = usize::try_from(central.compressed).map_err(|_| "a zip entry past the file".to_string())?;
        let data_end = data.checked_add(compressed).filter(|end| *end <= directory).ok_or("a zip entry past the directory")?;
        let entry_end = if after { data_end.saturating_add(descriptor(bytes, data_end, &central)?) } else { data_end };
        extents.push((offset, entry_end));
        at = end;
        if extents.len() > MAX_ENTRIES {
            return Err("too many zip entries".into());
        }
    }
    // The entries, in file order, from the first byte to the directory with nothing between.
    extents.sort_unstable();
    let mut next = 0usize;
    for (start, end) in &extents {
        if *start != next {
            return Err("bytes outside the zip entries".into());
        }
        next = *end;
    }
    if next != directory {
        return Err("bytes between the zip entries and the directory".into());
    }
    // The directory runs to the end record (or to the ZIP64 end record and its locator).
    let end_record = bytes.len().checked_sub(22).ok_or("no zip end record")?;
    let ends_at_zip64 = bytes.get(at..at.saturating_add(4)) == Some(&ZIP64_END_RECORD[..])
        && u64_at(bytes, at.saturating_add(4))
            .and_then(|size| usize::try_from(size).ok())
            .and_then(|size| at.checked_add(12)?.checked_add(size))
            .is_some_and(|locator| {
                bytes.get(locator..locator.saturating_add(4)) == Some(&ZIP64_LOCATOR[..]) && locator.saturating_add(20) == end_record
            });
    if bytes.get(end_record..end_record.saturating_add(4)) != Some(&END_RECORD[..]) || !(at == end_record || ends_at_zip64) {
        return Err("bytes between the zip directory and its end record".into());
    }
    Ok(extents.len())
}

impl<'a> StrictPackage<'a> {
    pub(super) fn open(bytes: &'a [u8], limits: &ReadLimits, budget: &mut Budget) -> Result<Self, String> {
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
        let records = check_layout(bytes, archive.central_directory_start(), budget)?;
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
            budget.work(1 + read / 64)?;
            if read > room {
                return Err(format!("part {name} decompresses past the size limit"));
            }
            if read != entry.size() {
                return Err(format!("part {name} decompresses to another size than it declares"));
            }
            total = total.saturating_add(read);
            // Part names are compared without case (ECMA-376 Part 2, 9.1.1.1).
            if folded.insert(name.to_lowercase(), ()).is_some() {
                return Err(format!("two parts named {name} apart from case"));
            }
            budget.hold(ENTRY_BYTES.saturating_add(units(name.len()).saturating_mul(2)))?;
            index.insert(name, (position, read));
        }
        Ok(StrictPackage { archive, index, max_part: limits.max_part_bytes })
    }

    pub(super) fn names(&self) -> impl Iterator<Item = &str> {
        self.index.keys().map(String::as_str)
    }

    pub(super) fn has(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }

    /// A part's bytes, by its exact name. Their memory is held before they are read; the caller
    /// releases it.
    pub(super) fn read(&mut self, name: &str, budget: &mut Budget) -> Result<Option<Vec<u8>>, String> {
        let Some((position, size)) = self.index.get(name).copied() else { return Ok(None) };
        if size > self.max_part {
            return Err(format!("part {name} decompresses past the size limit"));
        }
        budget.hold(size)?;
        let read = (|| -> Result<Vec<u8>, String> {
            let mut entry = self.archive.by_index(position).map_err(|error| format!("zip entry {name} unreadable: {error}"))?;
            let mut bytes = Vec::with_capacity(usize::try_from(size).map_err(|_| "a part too large".to_string())?);
            (&mut entry).take(size.saturating_add(1)).read_to_end(&mut bytes).map_err(|error| format!("zip entry {name} unreadable: {error}"))?;
            if units(bytes.len()) != size {
                return Err(format!("part {name} decompresses to another size than it declares"));
            }
            budget.work(1 + size / 64)?;
            Ok(bytes)
        })();
        if read.is_err() {
            budget.release(size);
        }
        read.map(Some)
    }
}

/// Whether a part has the same bytes in both packages, read side by side without keeping either.
pub(super) fn same_bytes(
    base: &mut StrictPackage<'_>,
    edited: &mut StrictPackage<'_>,
    name: &str,
    budget: &mut Budget,
) -> Result<bool, InspectError> {
    let (Some((base_position, base_size)), Some((edited_position, edited_size))) = (base.index.get(name).copied(), edited.index.get(name).copied())
    else {
        return Ok(false);
    };
    if base_size != edited_size {
        return Ok(false);
    }
    let unreadable = |error: String| format!("zip entry {name} unreadable: {error}");
    let mut before = base.archive.by_index(base_position).map_err(|error| InspectError::Base(unreadable(error.to_string())))?;
    let mut after = edited.archive.by_index(edited_position).map_err(|error| InspectError::Edited(unreadable(error.to_string())))?;
    const CHUNK: usize = 16 * 1024;
    let mut left = vec![0u8; CHUNK];
    let mut right = vec![0u8; CHUNK];
    let mut remaining = base_size;
    while remaining > 0 {
        let chunk = usize::try_from(remaining).map_or(CHUNK, |remaining| remaining.min(CHUNK));
        let (Some(left), Some(right)) = (left.get_mut(..chunk), right.get_mut(..chunk)) else { return Ok(false) };
        before.read_exact(left).map_err(|error| InspectError::Base(unreadable(error.to_string())))?;
        after.read_exact(right).map_err(|error| InspectError::Edited(unreadable(error.to_string())))?;
        budget.work(1 + units(chunk) / 64).map_err(InspectError::Edited)?;
        if left != right {
            return Ok(false);
        }
        remaining = remaining.saturating_sub(units(chunk));
    }
    Ok(true)
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

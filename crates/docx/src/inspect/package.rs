//! embed: a strict reading of a zip package for [`super::inspect`]. The desktop reader skips an entry
//! it cannot read and lets a later entry replace an earlier one of the same name; an inspection
//! cannot, because whatever it does not read could be what another reader shows. Every entry must
//! be readable, named once, and described alike by the central directory and its local header
//! (name, flags, method, CRC and sizes); the entries must fill the file from its first byte to the
//! directory, and the directory run to the end record, so there are no bytes another reader could
//! take for an entry this one does not see.
//!
//! Opening checks the directory and every header; each entry's data is inflated once, when the
//! inspection reads the part (XML) or hashes it (anything else), and [`StrictPackage::finish`]
//! reads whatever no step asked for, so no entry goes unread.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use zip::{CompressionMethod, ZipArchive};

use super::budget::{BYTES_PER_UNIT_WALKED, Budget, UNITS_PER_STORED_BLOCK, consumed, copied, hashed, inflated, tree_slot, units, vector_slot};
use super::digest::{Digest, StreamHasher};
use crate::package::{MAX_ENTRIES, ReadLimits};

/// A package whose directory and headers were read within the limits; its entries are read once
/// each, as the inspection asks for them.
pub(super) struct StrictPackage<'a> {
    archive: ZipArchive<Cursor<&'a [u8]>>,
    bytes: &'a [u8],
    /// Part name → where its entry is and what it declares.
    index: BTreeMap<String, Entry>,
    max_part: u64,
}

/// Where a part's entry is, what it declares, and whether its data was read.
#[derive(Clone, Copy)]
struct Entry {
    position: usize,
    size: u64,
    stored: bool,
    /// Its compressed data, as a range of the package's bytes.
    data: usize,
    compressed: usize,
    read: bool,
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
/// General-purpose flag: the name (and comment) are UTF-8.
const UTF8_NAME: u16 = 0x0800;
/// Extra-field ids: ZIP64 sizes, and Info-ZIP's second spelling of the name.
const ZIP64_EXTRA: u16 = 0x0001;
const UNICODE_PATH: u16 = 0x7075;
/// Memory charged for an open entry stream: its decompressor's state and buffers.
pub(super) const INFLATE_BYTES: u64 = 128 * 1024;
/// Work charged for opening one entry's stream (its decompressor and buffers).
const ENTRY_WORK: u64 = 2_000;
/// Work charged for an entry's record while the package is opened (its local header found).
const RECORD_WORK: u64 = 16;
/// The room an entry's data streams through while it is hashed.
const CHUNK: usize = 16 * 1024;
/// Memory charged for each entry, besides `DIRECTORY_COPIES` times the directory's bytes: the zip
/// reader's record (built in a growing vector, then moved into its index), the layout check's
/// extent, and this index's and the case-folded check's entries.
const ENTRY_BYTES: u64 = 640 + vector_slot::<(usize, usize)>() + tree_slot::<String, Entry>() + tree_slot::<String, ()>();
/// How many times the directory's bytes (names, extra fields, comments) are held: the zip reader
/// keeps each name three times and each extra field twice, this index once, and the case-folded
/// check a lowered copy, which can grow by half and its room by doubling.
const DIRECTORY_COPIES: u64 = 8;

/// How many entries the directory holds, its size in bytes and where it starts, from the end record
/// (or the ZIP64 end record it points at). The directory must end where its end record (or ZIP64
/// end record) begins, so the zip reader finds its records exactly there, with nothing before the
/// archive.
fn directory_size(bytes: &[u8]) -> Result<(usize, u64, u64), String> {
    let end = bytes.len().checked_sub(22).ok_or("no zip end record")?;
    if bytes.get(end..end.saturating_add(4)) != Some(&END_RECORD[..]) {
        return Err("no zip end record where it must be".into());
    }
    let entries = u16_at(bytes, end.saturating_add(10)).ok_or("a cut zip end record")?;
    let size = u32_at(bytes, end.saturating_add(12)).ok_or("a cut zip end record")?;
    let start = u32_at(bytes, end.saturating_add(16)).ok_or("a cut zip end record")?;
    // A ZIP64 end record, when its locator stands before the end record.
    let zip64_record = end
        .checked_sub(20)
        .filter(|locator| bytes.get(*locator..locator.saturating_add(4)) == Some(&ZIP64_LOCATOR[..]))
        .map(|locator| u64_at(bytes, locator.saturating_add(8)).and_then(|at| usize::try_from(at).ok()).ok_or("a cut ZIP64 locator"))
        .transpose()?;
    let (entries, size, start, directory_end) = if entries != u16::MAX && size != u32::MAX && start != u32::MAX {
        let directory_end = match zip64_record {
            Some(record) if u64::from(start).saturating_add(u64::from(size)) == units(record) => record,
            _ => end,
        };
        (usize::from(entries), u64::from(size), u64::from(start), directory_end)
    } else {
        let record = zip64_record.ok_or("a ZIP64 end record without its locator")?;
        if bytes.get(record..record.saturating_add(4)) != Some(&ZIP64_END_RECORD[..]) {
            return Err("a ZIP64 locator pointing at no end record".into());
        }
        let entries = u64_at(bytes, record.saturating_add(32)).and_then(|count| usize::try_from(count).ok()).ok_or("a cut ZIP64 end record")?;
        let size = u64_at(bytes, record.saturating_add(40)).ok_or("a cut ZIP64 end record")?;
        let start = u64_at(bytes, record.saturating_add(48)).ok_or("a cut ZIP64 end record")?;
        (entries, size, start, record)
    };
    if start.checked_add(size) != Some(units(directory_end)) {
        return Err("a zip directory not where its end record says".into());
    }
    Ok((entries, size, start))
}

/// Every name in the directory, read before the zip reader decodes it. The zip reader decodes a
/// name without the UTF-8 flag as CP437, where each byte past ASCII can take three, and spells a
/// flagged name that is not UTF-8 with a replacement character for each bad byte: names another
/// reader takes otherwise than these bytes say (and that the reader would hold at three times
/// their length). So a name must be ASCII, or flagged and UTF-8; and no entry may carry a comment
/// or a second spelling of its name. What the reader decodes then holds no more than its length.
fn check_names(bytes: &[u8], start: u64, size: u64, entries: usize, budget: &mut Budget) -> Result<(), String> {
    let start = usize::try_from(start).map_err(|_| "the zip directory starts past the file".to_string())?;
    let end = usize::try_from(size).ok().and_then(|size| start.checked_add(size)).ok_or("the zip directory runs past the file")?;
    let mut at = start;
    for _ in 0..entries {
        budget.work(1)?;
        if at >= end || bytes.get(at..at.saturating_add(4)) != Some(&CENTRAL_HEADER[..]) {
            return Err("a zip directory that does not hold its entries".into());
        }
        let (described, _, next) = central_record(bytes, at)?;
        let readable = if described.flags & UTF8_NAME != 0 { std::str::from_utf8(described.name).is_ok() } else { described.name.is_ascii() };
        if !readable {
            return Err("a zip entry whose name is in no one encoding (past ASCII without the UTF-8 flag, or flagged but not UTF-8)".into());
        }
        at = next;
    }
    if at > end {
        return Err("a zip directory that does not hold its entries".into());
    }
    Ok(())
}

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
        // What the directory will hold, from its end record, before the zip reader builds it.
        let (entries, directory_bytes, directory_start) = directory_size(bytes)?;
        if entries > MAX_ENTRIES {
            return Err(format!("{entries} zip entries"));
        }
        check_names(bytes, directory_start, directory_bytes, entries, budget)?;
        budget.hold(ENTRY_BYTES.saturating_mul(units(entries)).saturating_add(directory_bytes.saturating_mul(DIRECTORY_COPIES)))?;
        let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(|error| format!("not a readable zip package: {error}"))?;
        if archive.len() > MAX_ENTRIES {
            return Err(format!("{} zip entries", archive.len()));
        }
        // Bytes before the archive or after its directory are where two readers part ways.
        if archive.offset() != 0 || archive.central_directory_start() != directory_start {
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
            budget.work(RECORD_WORK)?;
            // The entry's record and local header, without opening its stream; held while it is
            // open, as a stream would be.
            budget.hold(INFLATE_BYTES)?;
            let found = Self::index_entry(&mut archive, bytes, position, limits, &mut total);
            budget.release(INFLATE_BYTES);
            let Some((name, entry)) = found? else { continue };
            // Part names are compared without case (ECMA-376 Part 2, 9.1.1.1).
            if folded.insert(name.to_lowercase(), ()).is_some() {
                return Err(format!("two parts named {name} apart from case"));
            }
            index.insert(name, entry);
        }
        Ok(StrictPackage { archive, bytes, index, max_part: limits.max_part_bytes })
    }

    /// One entry's name and where it is, from its record and local header; `None` for a folder.
    fn index_entry(
        archive: &mut ZipArchive<Cursor<&'a [u8]>>,
        bytes: &[u8],
        position: usize,
        limits: &ReadLimits,
        total: &mut u64,
    ) -> Result<Option<(String, Entry)>, String> {
        let entry = archive.by_index_raw(position).map_err(|error| format!("zip entry {position} unreadable: {error}"))?;
        let name = std::str::from_utf8(entry.name_raw()).map_err(|_| format!("zip entry {position} has a name that is not UTF-8"))?.to_string();
        if entry.encrypted() {
            return Err(format!("zip entry {name} is encrypted"));
        }
        if let Some(folder) = name.strip_suffix('/') {
            if entry.size() != 0 || entry.compressed_size() > 2 || folder.is_empty() {
                return Err(format!("zip folder entry {name} holds data"));
            }
            return Ok(None);
        }
        check_name(&name)?;
        let declared = entry.size();
        if declared > limits.max_part_bytes {
            return Err(format!("part {name} declares {declared} bytes"));
        }
        if declared > limits.max_part_bytes.min(limits.max_total_bytes.saturating_sub(*total)) {
            return Err(format!("part {name} decompresses past the size limit"));
        }
        *total = total.saturating_add(declared);
        let stored = match entry.compression() {
            CompressionMethod::Stored => true,
            CompressionMethod::Deflated => false,
            _ => return Err(format!("zip entry {name} compressed another way than stored or deflated")),
        };
        let data = usize::try_from(entry.data_start()).map_err(|_| format!("zip entry {name} past the file"))?;
        let compressed = usize::try_from(entry.compressed_size()).map_err(|_| format!("zip entry {name} past the file"))?;
        if data.checked_add(compressed).is_none_or(|end| end > bytes.len()) {
            return Err(format!("zip entry {name} past the file"));
        }
        Ok(Some((name, Entry { position, size: declared, stored, data, compressed, read: false })))
    }

    pub(super) fn names(&self) -> impl Iterator<Item = &str> {
        self.index.keys().map(String::as_str)
    }

    pub(super) fn has(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }

    /// The work of inflating an entry, from what its compressed data is: a copy for stored data
    /// (a stored entry, or a deflate stream of stored blocks only), else what the Huffman-coded
    /// stream gives and what it consumes. Looking through the stream's blocks is charged first.
    fn inflate_work(&self, entry: &Entry, budget: &mut Budget) -> Result<u64, String> {
        if entry.stored {
            return Ok(copied(entry.size));
        }
        let raw = self.bytes.get(entry.data..entry.data.saturating_add(entry.compressed)).ok_or("a zip entry past the file")?;
        budget.work(1 + units(raw.len()) / BYTES_PER_UNIT_WALKED)?;
        Ok(match stored_blocks(raw) {
            Some(blocks) => blocks.saturating_mul(UNITS_PER_STORED_BLOCK).saturating_add(copied(entry.size)),
            None => inflated(entry.size).saturating_add(consumed(units(raw.len()))),
        })
    }

    /// The entry of a part, by its exact name, and its size checked against the part limit.
    fn entry(&self, name: &str) -> Result<Option<Entry>, String> {
        let Some(entry) = self.index.get(name).copied() else { return Ok(None) };
        if entry.size > self.max_part {
            return Err(format!("part {name} decompresses past the size limit"));
        }
        Ok(Some(entry))
    }

    fn mark_read(&mut self, name: &str) {
        if let Some(entry) = self.index.get_mut(name) {
            entry.read = true;
        }
    }

    /// A part's bytes, by its exact name. Their memory is held before they are read; the caller
    /// releases it.
    pub(super) fn read(&mut self, name: &str, budget: &mut Budget) -> Result<Option<Vec<u8>>, String> {
        let Some(entry) = self.entry(name)? else { return Ok(None) };
        let size = entry.size;
        budget.hold(size.saturating_add(INFLATE_BYTES))?;
        let read = (|| -> Result<Vec<u8>, String> {
            // Inflating is charged before it runs, for the size declared (a stream that runs longer
            // is refused once it passes it) and for the compressed data it consumes.
            let work = self.inflate_work(&entry, budget)?;
            budget.work(ENTRY_WORK.saturating_add(work))?;
            let mut stream = self.archive.by_index(entry.position).map_err(|error| format!("zip entry {name} unreadable: {error}"))?;
            let mut bytes = Vec::with_capacity(usize::try_from(size).map_err(|_| "a part too large".to_string())?);
            // Read to the end, so the zip crate checks the CRC.
            (&mut stream).take(size.saturating_add(1)).read_to_end(&mut bytes).map_err(|error| format!("zip entry {name} unreadable: {error}"))?;
            if units(bytes.len()) != size {
                return Err(format!("part {name} decompresses to another size than it declares"));
            }
            Ok(bytes)
        })();
        budget.release(INFLATE_BYTES);
        if read.is_err() {
            budget.release(size);
        }
        let bytes = read?;
        self.mark_read(name);
        Ok(Some(bytes))
    }

    /// A part's SHA-256, its bytes hashed as they are inflated, never kept: a binary part is read
    /// once, and compared by its digest.
    pub(super) fn digest(&mut self, name: &str, budget: &mut Budget) -> Result<Digest, String> {
        let entry = self.entry(name)?.ok_or_else(|| format!("no part {name}"))?;
        // The stream and the chunk it is read into, while it is hashed.
        let held = INFLATE_BYTES.saturating_add(units(CHUNK));
        budget.hold(held)?;
        let digest = self.hash_entry(name, &entry, budget);
        budget.release(held);
        let digest = digest?;
        self.mark_read(name);
        Ok(digest)
    }

    fn hash_entry(&mut self, name: &str, entry: &Entry, budget: &mut Budget) -> Result<Digest, String> {
        let work = self.inflate_work(entry, budget)?;
        budget.work(ENTRY_WORK.saturating_add(work).saturating_add(hashed(entry.size)))?;
        let unreadable = |error: std::io::Error| format!("zip entry {name} unreadable: {error}");
        let stream = self.archive.by_index(entry.position).map_err(|error| format!("zip entry {name} unreadable: {error}"))?;
        let mut stream = stream.take(entry.size.saturating_add(1));
        let mut chunk = vec![0u8; CHUNK];
        let mut hasher = StreamHasher::default();
        let mut total: u64 = 0;
        loop {
            // Read until the stream ends, so the zip crate checks the CRC.
            let read = stream.read(&mut chunk).map_err(unreadable)?;
            if read == 0 {
                break;
            }
            total = total.saturating_add(units(read));
            if total > entry.size {
                return Err(format!("part {name} decompresses past the size it declares"));
            }
            hasher.update(chunk.get(..read).unwrap_or(&[]));
        }
        if total != entry.size {
            return Err(format!("part {name} decompresses to another size than it declares"));
        }
        Ok(hasher.finish())
    }

    /// Read every entry no step asked for, so a package is never accepted with an entry unread.
    pub(super) fn finish(&mut self, budget: &mut Budget) -> Result<(), String> {
        let unread: Vec<String> = self.index.iter().filter(|(_, entry)| !entry.read).map(|(name, _)| name.clone()).collect();
        let held = unread.iter().fold(0u64, |held, name| held.saturating_add(units(name.len())).saturating_add(units(std::mem::size_of::<String>())));
        budget.hold(held)?;
        let checked = unread.iter().try_for_each(|name| self.digest(name, budget).map(drop));
        budget.release(held);
        checked
    }
}

/// How many stored blocks a deflate stream is, when it is stored blocks only and ends exactly at
/// its last byte; `None` for anything else (a Huffman-coded block, a malformed stream, data after
/// its final block). A stored block starts at a byte: three header bits padded to the byte, then
/// its length and the length's complement, then that many bytes.
fn stored_blocks(raw: &[u8]) -> Option<u64> {
    let mut at = 0usize;
    let mut blocks = 0u64;
    loop {
        let header = *raw.get(at)?;
        if header & 0b110 != 0 {
            return None;
        }
        let length = u16_at(raw, at.checked_add(1)?)?;
        let complement = u16_at(raw, at.checked_add(3)?)?;
        if complement != !length {
            return None;
        }
        at = at.checked_add(5)?.checked_add(usize::from(length))?;
        blocks = blocks.saturating_add(1);
        if header & 1 == 1 {
            return (at == raw.len()).then_some(blocks);
        }
        if at >= raw.len() {
            return None;
        }
    }
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

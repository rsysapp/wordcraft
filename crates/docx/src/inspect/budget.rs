//! embed: what one inspection may spend. Work counts parsing, walking, hashing and comparing in
//! units independent of the clock, so a crafted pair of documents stops alike on every target.
//! Memory counts what the inspection holds (part bytes while parsing, parsed trees, story items,
//! interned content, diff state) and is charged before it is allocated, so the limit is never
//! passed first and refused after.

/// Work is counted in units of roughly ten nanoseconds, so that `MAX_WORK` (400 M units) is a
/// few seconds whatever the input: these say how many bytes of each kind of processing one unit
/// pays for. Measured on an 11-core Apple silicon machine, the slowest kind of work runs at under 10.5 ns a
/// unit idle (4.2 s for `MAX_WORK`) and 13.4 ns with every core busy (5.4 s): a host that allows
/// 10 s keeps a margin of two over the idle rate.
/// Inflating is the dearest step: a Huffman-coded stream of literals takes up to 48 ns for every
/// 8 bytes it gives (measured), so each 8 bytes inflated is charged five units (50 ns), and
/// inflating alone cannot run past the work budget's time.
pub(super) fn inflated(bytes: u64) -> u64 {
    bytes.saturating_mul(INFLATE_UNITS_PER_8_BYTES) / 8
}
/// Units charged for every 8 bytes inflated.
pub(super) const INFLATE_UNITS_PER_8_BYTES: u64 = 5;
/// The compressed bytes a Huffman-coded stream consumes cost work of their own, whatever it gives:
/// a stream of empty blocks, each declaring its own code tables (15-bit codes, so sub-tables too),
/// takes 38.8 ns for every compressed byte and gives nothing (measured). So each compressed byte
/// of such a stream is charged four units (40 ns), besides what it gives.
pub(super) fn consumed(bytes: u64) -> u64 {
    bytes.saturating_mul(INFLATE_INPUT_UNITS_PER_BYTE)
}
/// Units charged for every compressed byte of a Huffman-coded stream.
pub(super) const INFLATE_INPUT_UNITS_PER_BYTE: u64 = 4;
/// Stored data (a stored entry, or a deflate stream of stored blocks only) is copied and checked
/// against its CRC: about 0.5 ns a byte (measured), charged at 1.25 ns.
pub(super) fn copied(bytes: u64) -> u64 {
    bytes / BYTES_PER_UNIT_COPIED
}
/// Bytes of stored data copied.
pub(super) const BYTES_PER_UNIT_COPIED: u64 = 8;
/// Each stored block's header, read by the deflate stream: 7.3 ns a block (measured, empty blocks),
/// charged at one unit.
pub(super) const UNITS_PER_STORED_BLOCK: u64 = 1;
/// Compressed bytes looked through to tell a stream of stored blocks only: a step for each block
/// header, at most one in five bytes.
pub(super) const BYTES_PER_UNIT_WALKED: u64 = 32;
/// Bytes of XML parsed.
pub(super) const BYTES_PER_UNIT_PARSED: u64 = 8;
/// Bytes run through SHA-256: 5.05 ns a byte (measured), so two a unit.
pub(super) const BYTES_PER_UNIT_HASHED: u64 = 2;

/// Work charged for hashing `bytes` with SHA-256.
pub(super) fn hashed(bytes: u64) -> u64 {
    bytes / BYTES_PER_UNIT_HASHED
}
/// Bytes kept as an exact identity (copied and later hashed for a table).
pub(super) const BYTES_PER_UNIT_KEPT: u64 = 16;

/// Why work ran out.
pub(super) const TOO_MUCH_WORK: &str = "more work than an inspection does";
/// Why memory ran out.
pub(super) const TOO_MUCH_MEMORY: &str = "more memory than an inspection holds";

pub(super) struct Budget {
    work_left: u64,
    held: u64,
    limit: u64,
    peak: u64,
    /// A charge was refused inside a comparison that can only answer "stopped"; the inspection
    /// is then refused as a whole.
    pub memory_refused: bool,
}

impl Budget {
    pub(super) fn new(work: u64, memory: u64) -> Budget {
        Budget { work_left: work, held: 0, limit: memory, peak: 0, memory_refused: false }
    }

    /// Spend `units` of work; false once the budget is gone.
    pub(super) fn spend(&mut self, units: u64) -> bool {
        match self.work_left.checked_sub(units) {
            Some(left) => {
                self.work_left = left;
                true
            }
            None => {
                self.work_left = 0;
                false
            }
        }
    }

    pub(super) fn work(&mut self, units: u64) -> Result<(), String> {
        if self.spend(units) { Ok(()) } else { Err(TOO_MUCH_WORK.into()) }
    }

    /// Hold `bytes` more, or refuse without holding them.
    pub(super) fn hold(&mut self, bytes: u64) -> Result<(), String> {
        let held = self.held.checked_add(bytes).filter(|held| *held <= self.limit).ok_or(TOO_MUCH_MEMORY)?;
        self.held = held;
        self.peak = self.peak.max(held);
        Ok(())
    }

    /// [`Budget::hold`] for a step that can only answer whether it went on.
    pub(super) fn try_hold(&mut self, bytes: u64) -> bool {
        let held = self.hold(bytes).is_ok();
        if !held {
            self.memory_refused = true;
        }
        held
    }

    pub(super) fn release(&mut self, bytes: u64) {
        self.held = self.held.saturating_sub(bytes);
    }

    pub(super) fn work_left(&self) -> u64 {
        self.work_left
    }

    pub(super) fn held(&self) -> u64 {
        self.held
    }

    /// Start measuring the peak again from what is held now.
    pub(super) fn reset_peak(&mut self) {
        self.peak = self.held;
    }

    /// The most held at once, for measurements.
    pub(super) fn peak(&self) -> u64 {
        self.peak
    }
}

/// A count as bytes or units, saturating.
pub(super) fn units(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

// Collections are charged for the room they can hold, never for their length. Where the room is
// known beforehand it is reserved exactly; otherwise each entry is charged for the most its
// collection can hold per entry at its worst moment, which these give from the entry's size.

/// The size of `T`, as bytes.
const fn size<T>() -> u64 {
    std::mem::size_of::<T>() as u64
}

/// Memory one element of a growing vector can hold: a vector grows by doubling, and holds its old
/// room beside the new (twice the old) while it moves, so three times its length at worst.
pub(super) const fn vector_slot<T>() -> u64 {
    3 * size::<T>()
}

/// Memory a growing vector holds besides [`vector_slot`] per element: its first room is four
/// elements, whatever it holds.
pub(super) const fn vector_room<T>() -> u64 {
    4 * size::<T>()
}

/// Memory one entry of a hash table can hold. A table holds at most seven entries for every eight
/// buckets, each bucket an entry's room and a control byte; when full it doubles its buckets and
/// holds the old table beside the new while it moves. Just past that point it holds three times
/// its old buckets for 7/8 of them in entries: 24/7 buckets per entry.
pub(super) const fn table_slot<T>() -> u64 {
    (24 * (size::<T>() + 1)).div_ceil(7)
}

/// Memory a hash table holds besides [`table_slot`] per entry: its first table (four buckets for up
/// to three entries) and, for each of two tables, its control bytes' tail and alignment.
pub(super) const fn table_room<T>() -> u64 {
    4 * (size::<T>() + 1) + 64
}

/// Memory a hash table made with room for `entries`, and never grown past them, holds: its buckets
/// for that many at its 7/8 load (a power of two, at least four; eight below eight entries), each an
/// entry's room and a control byte, and its control bytes' tail and alignment.
pub(super) fn table_sized<T>(entries: usize) -> u64 {
    let entries = units(entries);
    let buckets = match entries {
        0..4 => 4,
        4..8 => 8,
        _ => entries.saturating_mul(8).div_ceil(7).checked_next_power_of_two().unwrap_or(u64::MAX),
    };
    buckets.saturating_mul(size::<T>() + 1).saturating_add(64)
}

/// The most entries a B-tree node holds, and the fewest a node other than the root keeps.
const TREE_CAPACITY: u64 = 11;
const TREE_FEWEST: u64 = 5;

/// One B-tree node of entries of `K` and `V`, with room for its children's pointers.
const fn tree_node<K, V>() -> u64 {
    TREE_CAPACITY * (size::<K>() + size::<V>()) + 16 + (TREE_CAPACITY + 1) * 8
}

/// Memory one entry of a B-tree map can hold: every node but the root keeps at least five of its
/// eleven entries, so a tree holds at most one node per five entries, and the root.
pub(super) const fn tree_slot<K, V>() -> u64 {
    tree_node::<K, V>().div_ceil(TREE_FEWEST)
}

/// Memory a B-tree map holds besides [`tree_slot`] per entry: its root.
pub(super) const fn tree_room<K, V>() -> u64 {
    tree_node::<K, V>()
}

/// Memory a shared string (`Rc<str>`) of `length` bytes holds: its two counts, and its bytes
/// rounded up to the counts' alignment.
pub(super) fn shared_string(length: usize) -> u64 {
    16u64.saturating_add(units(length).next_multiple_of(8))
}

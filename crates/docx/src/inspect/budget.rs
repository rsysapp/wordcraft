//! embed: what one inspection may spend. Work counts parsing, walking, hashing and comparing in
//! units independent of the clock, so a crafted pair of documents stops alike on every target.
//! Memory counts what the inspection holds (part bytes while parsing, parsed trees, story items,
//! interned content, diff state) and is charged before it is allocated, so the limit is never
//! passed first and refused after.

/// Why work ran out.
pub(super) const TOO_MUCH_WORK: &str = "more work than an inspection does";
/// Why memory ran out.
pub(super) const TOO_MUCH_MEMORY: &str = "more memory than an inspection holds";

pub(super) struct Budget {
    work_left: u64,
    held: u64,
    limit: u64,
    #[cfg_attr(not(test), allow(dead_code))]
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

    /// The most held at once, for measurements.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn peak(&self) -> u64 {
        self.peak
    }
}

/// A count as bytes or units, saturating.
pub(super) fn units(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

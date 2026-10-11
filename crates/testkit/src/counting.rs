//! A global allocator that forwards to the system's and counts what is held: now, and the most since
//! the last [`reset_peak`]. A test binary installs it with
//! `#[global_allocator] static ALLOCATOR: Counting = Counting;` and runs its measurements on one
//! thread, since the counts are the whole process's.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static HELD: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

pub struct Counting;

fn grew(bytes: usize) {
    let held = HELD.fetch_add(bytes, Ordering::Relaxed).saturating_add(bytes);
    PEAK.fetch_max(held, Ordering::Relaxed);
}

fn shrank(bytes: usize) {
    HELD.fetch_sub(bytes, Ordering::Relaxed);
}

// SAFETY: every call forwards to `System` with the caller's own arguments, so `System`'s guarantees
// are the caller's; the counting touches only atomics.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            grew(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            grew(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(pointer, layout) };
        shrank(layout.size());
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Count the new block before the old one goes: a reallocation may hold both while it copies.
        grew(new_size);
        // SAFETY: forwarded unchanged.
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if moved.is_null() {
            shrank(new_size);
        } else {
            shrank(layout.size());
        }
        moved
    }
}

/// Bytes held now.
pub fn held() -> usize {
    HELD.load(Ordering::Relaxed)
}

/// Start a new measurement: the peak becomes what is held now, which is answered.
pub fn reset_peak() -> usize {
    let now = held();
    PEAK.store(now, Ordering::Relaxed);
    now
}

/// The most held since the last [`reset_peak`].
pub fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

//! Pointer scanning: find chains of pointers from a module's static data to a
//! target address, so the address can be found again after a restart.
//!
//! The first step is a [`PointerMap`]: a snapshot of every aligned value in
//! writable memory that points into readable memory, sorted by value on disk.

mod map;

pub use map::PointerMap;

/// Limits for a pointer scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PtrScanSettings {
    /// Maximum number of `[…]` dereference steps in a chain.
    pub max_depth: usize,
    /// Maximum offset added after each dereference.
    pub max_offset: u64,
    /// Alignment of pointer locations (and the scan stride), in bytes.
    pub alignment: u64,
    /// Stop after this many chains.
    pub max_results: usize,
    /// Records sorted in memory before a run is written to disk (16 bytes
    /// each, so the default bounds the sort buffer at 1 GiB).
    pub chunk_entries: usize,
}

impl Default for PtrScanSettings {
    fn default() -> Self {
        PtrScanSettings {
            max_depth: 5,
            max_offset: 0x1000,
            alignment: 8,
            max_results: 1_000_000,
            chunk_entries: 64 * 1024 * 1024,
        }
    }
}

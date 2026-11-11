//! A process's memory map.

use std::io;

/// A single mapped memory region of a process.
#[derive(Clone, Debug)]
pub struct MemoryRegion {
    pub start: u64,
    pub end: u64,
    pub read: bool,
    pub write: bool,
    pub exec: bool,
    pub shared: bool,
    /// Pathname / pseudo-name, e.g. `[heap]`, `[stack]`, or a library path.
    pub path: String,
}

impl MemoryRegion {
    pub fn size(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// Whether this region is a sensible target for value scanning: readable,
    /// writable, and not one of the special kernel maps that either can't be
    /// safely read (`[vvar]`, `[vsyscall]`) or never hold game state.
    pub fn is_scannable(&self) -> bool {
        if !(self.read && self.write) {
            return false;
        }
        !matches!(self.path.as_str(), "[vvar]" | "[vdso]" | "[vsyscall]")
    }

    /// Classify the region for the "real value" hint. Real game state usually
    /// lives on the heap or in a module's writable data/bss; the stack holds
    /// transient locals and copies.
    pub fn kind(&self) -> RegionKind {
        match self.path.as_str() {
            "[heap]" => RegionKind::Heap,
            "[stack]" => RegionKind::Stack,
            "" => RegionKind::Anonymous,
            // A backed pathname (a library / executable). Its writable,
            // non-executable segment is the module's data/bss — where globals
            // and singletons like player stats live. Other `[...]` pseudo-names
            // (e.g. `[stack:tid]`, mapped files' text) fall through to Other.
            p if !p.starts_with('[') && self.write && !self.exec => RegionKind::ModuleData,
            _ => RegionKind::Other,
        }
    }
}

/// Coarse classification of a memory region, used to rank scan candidates by
/// how likely they are to be the authoritative game value (see
/// `scan::address_hint`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RegionKind {
    Heap,
    Stack,
    ModuleData,
    Anonymous,
    Other,
}

/// Find the region containing `addr`, if any.
///
// ponytail: linear scan. `/proc/pid/maps` is address-sorted so a binary search
// is possible, but the hint only runs on a narrowed candidate set (<= a couple
// hundred rows) against a few hundred regions — linear is plenty. Switch to
// binary if it ever shows up in a profile.
pub fn region_for(regions: &[MemoryRegion], addr: u64) -> Option<&MemoryRegion> {
    regions.iter().find(|r| addr >= r.start && addr < r.end)
}

/// Read the memory map of `pid`, sorted by address (`/proc/<pid>/maps` on
/// Linux, the Mach VM region list on macOS).
pub fn read_maps(pid: i32) -> io::Result<Vec<MemoryRegion>> {
    crate::sys::read_maps(pid)
}

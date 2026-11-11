//! Where address expressions and scanners read target memory from.

use std::io;

use crate::maps::{read_maps, MemoryRegion};
use crate::mem::Mem;

/// A readable view of a target's address space plus its memory map.
/// Kept minimal and object-safe so tests can substitute a fake.
pub trait MemSource {
    fn regions(&self) -> &[MemoryRegion];
    /// Read up to `buf.len()` bytes at `addr`; returns the number read.
    fn read(&self, addr: u64, buf: &mut [u8]) -> io::Result<usize>;
}

/// A live process: an open [`Mem`] and a snapshot of its memory map.
pub struct LiveSource {
    mem: Mem,
    regions: Vec<MemoryRegion>,
}

impl LiveSource {
    pub fn open(pid: i32) -> io::Result<LiveSource> {
        let mem = Mem::open(pid)?;
        let regions = read_maps(pid)?;
        Ok(LiveSource { mem, regions })
    }

    /// The process this reads.
    pub fn pid(&self) -> i32 {
        self.mem.pid()
    }

    /// Re-read the memory map (modules may have been loaded or unloaded).
    pub fn refresh_regions(&mut self) -> io::Result<()> {
        self.regions = read_maps(self.mem.pid())?;
        Ok(())
    }
}

impl MemSource for LiveSource {
    fn regions(&self) -> &[MemoryRegion] {
        &self.regions
    }

    fn read(&self, addr: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.mem.read_at(addr, buf)
    }
}

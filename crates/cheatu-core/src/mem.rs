//! Reading and writing another process's memory via `/proc/<pid>/mem`.
//!
//! `read_at`/`write_at` seek to the target virtual address and transfer bytes.
//! This works when the caller is root or holds `CAP_SYS_PTRACE` with a
//! permissive `yama/ptrace_scope`; otherwise `open` fails with `EACCES`.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;

/// An open handle to a process's address space.
pub struct Mem {
    pid: i32,
    file: File,
}

impl Mem {
    /// Open `/proc/<pid>/mem` for reading and writing.
    ///
    /// Falls back to read-only if the process disallows writes, so that
    /// scanning still works even when patching does not.
    pub fn open(pid: i32) -> io::Result<Mem> {
        let path = format!("/proc/{pid}/mem");
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => f,
            Err(_) => OpenOptions::new().read(true).open(&path)?,
        };
        Ok(Mem { pid, file })
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// Read `buf.len()` bytes starting at virtual address `addr`.
    ///
    /// Returns the number of bytes read; a short read (or `Err`) usually means
    /// the region is unmapped or unreadable, and callers should skip it.
    pub fn read_at(&self, addr: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read_at(buf, addr)
    }

    /// Write `buf` at virtual address `addr`. Requires write permission on the
    /// target (root or `CAP_SYS_PTRACE`).
    pub fn write_at(&self, addr: u64, buf: &[u8]) -> io::Result<()> {
        self.file.write_all_at(buf, addr)
    }
}

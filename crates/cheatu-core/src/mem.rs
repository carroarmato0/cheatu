//! Reading and writing another process's memory.
//!
//! `read_at`/`write_at` transfer bytes at a target virtual address. On Linux
//! this goes through `/proc/<pid>/mem` and works when the caller is root or
//! holds `CAP_SYS_PTRACE` with a permissive `yama/ptrace_scope`; on macOS it
//! uses the target's Mach task port, which needs root for other processes.
//! Otherwise `open` fails with `PermissionDenied`.

use std::io;
use std::time::Duration;

use crate::sys;

/// An open handle to a process's address space.
pub struct Mem {
    pid: i32,
    handle: sys::MemHandle,
}

impl Mem {
    /// Open `pid`'s address space for reading and (where allowed) writing.
    ///
    /// On Linux this falls back to read-only if the process disallows writes,
    /// so that scanning still works even when patching does not.
    pub fn open(pid: i32) -> io::Result<Mem> {
        Ok(Mem {
            pid,
            handle: sys::MemHandle::open(pid)?,
        })
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// Read `buf.len()` bytes starting at virtual address `addr`.
    ///
    /// Returns the number of bytes read; a short read (or `Err`) usually means
    /// the region is unmapped or unreadable, and callers should skip it.
    pub fn read_at(&self, addr: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.handle.read_at(addr, buf)
    }

    /// Write `buf` at virtual address `addr`. Requires write permission on the
    /// target (root or `CAP_SYS_PTRACE`).
    pub fn write_at(&self, addr: u64, buf: &[u8]) -> io::Result<()> {
        self.handle.write_at(addr, buf)
    }
}

/// The result of [`probe_address`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The sentinel we wrote was still there after the wait — nothing in the
    /// target overwrote it. Likely the authoritative value (or a stale copy).
    Held,
    /// The target overwrote our sentinel during the wait — this address is a
    /// live copy written from somewhere else, so probably not the source.
    Reverted,
}

/// Actively test whether `addr` is the authoritative value or a live copy.
///
/// The authoritative value is the *source* the game reads and writes; display
/// and logic copies are rewritten *from* it each frame. So we write a distinct
/// sentinel, let the target run for `wait`, then read back: a copy is
/// overwritten ([`ProbeOutcome::Reverted`]); the source is left alone
/// ([`ProbeOutcome::Held`]).
///
/// Destructive: this writes to live target memory. On `Held` the original bytes
/// are restored; on `Reverted` the target already owns the value, so we leave it.
///
/// Heuristic, not proof: "held" is the source *or* a stale cached copy, and a
/// source the game recomputes every frame would itself read as "reverted".
pub fn probe_address(
    mem: &Mem,
    addr: u64,
    size: usize,
    wait: Duration,
) -> io::Result<ProbeOutcome> {
    let mut original = vec![0u8; size];
    mem.read_at(addr, &mut original)?;

    // Minimal perturbation (±1 for little-endian ints, a tiny mantissa change
    // for floats) to reduce the chance of crashing the target.
    // ponytail: single-bit flip; widen the delta if noisy values give false
    // "reverted" reads.
    let mut sentinel = original.clone();
    sentinel[0] ^= 0x01;
    mem.write_at(addr, &sentinel)?;

    std::thread::sleep(wait);

    let mut readback = vec![0u8; size];
    mem.read_at(addr, &mut readback)?;

    if readback == sentinel {
        // Untouched — undo our change and report Held.
        mem.write_at(addr, &original)?;
        Ok(ProbeOutcome::Held)
    } else {
        // The target wrote its own value here; leave it in place.
        Ok(ProbeOutcome::Reverted)
    }
}

/// Whether `pid`'s memory can be opened now — i.e. whether scanning `pid`
/// would succeed without elevation. Reads nothing.
pub fn accessible(pid: i32) -> bool {
    sys::accessible(pid)
}

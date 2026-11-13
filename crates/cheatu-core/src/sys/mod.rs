//! Per-OS backends. Each one provides the same crate-private items, which the
//! public modules (`mem`, `maps`, `process`, `privilege`, `scan`) build on:
//!
//! * `MemHandle` — open / `read_at` / `write_at` on a target's address space
//! * `accessible`, `read_maps`, `raw_processes`, `environ`, `available_memory`
//! * `ELEVATION_TOOL`, `elevation_available`, `relaunch_elevated`

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::*;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("cheatu supports Linux and macOS only");

/// A running process as the backend sees it, before the OS-independent
/// heuristics (Wine/Proton, Chromium role, accessibility) are applied.
pub(crate) struct RawProc {
    pub pid: i32,
    /// Parent pid, 0 when unknown.
    pub ppid: i32,
    pub name: String,
    pub cmdline: String,
    pub rss_bytes: u64,
}

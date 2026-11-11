//! Linux backend: everything goes through `/proc` and `pkexec`.
//!
//! Reading another process's memory requires either running as root or holding
//! `CAP_SYS_PTRACE` (and a permissive `yama/ptrace_scope`).

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use super::RawProc;
use crate::maps::MemoryRegion;

/// An open `/proc/<pid>/mem` handle.
pub(crate) struct MemHandle {
    file: File,
}

impl MemHandle {
    /// Open `/proc/<pid>/mem` for reading and writing.
    ///
    /// Falls back to read-only if the process disallows writes, so that
    /// scanning still works even when patching does not.
    pub(crate) fn open(pid: i32) -> io::Result<MemHandle> {
        let path = format!("/proc/{pid}/mem");
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => f,
            Err(_) => OpenOptions::new().read(true).open(&path)?,
        };
        Ok(MemHandle { file })
    }

    pub(crate) fn read_at(&self, addr: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read_at(buf, addr)
    }

    pub(crate) fn write_at(&self, addr: u64, buf: &[u8]) -> io::Result<()> {
        self.file.write_all_at(buf, addr)
    }
}

/// Whether `/proc/<pid>/mem` can be opened for reading. The kernel's
/// ptrace-access check runs at `open`, so this is the ground truth; it reads
/// nothing and drops the handle.
pub(crate) fn accessible(pid: i32) -> bool {
    OpenOptions::new()
        .read(true)
        .open(format!("/proc/{pid}/mem"))
        .is_ok()
}

/// Read and parse the memory map of `pid`.
///
/// Each line looks like:
/// ```text
/// 55a3b1c00000-55a3b1c21000 rw-p 00000000 00:00 0    [heap]
/// ```
pub(crate) fn read_maps(pid: i32) -> io::Result<Vec<MemoryRegion>> {
    let text = fs::read_to_string(format!("/proc/{pid}/maps"))?;
    let mut regions = Vec::new();

    for line in text.lines() {
        let mut parts = line.splitn(6, char::is_whitespace);
        let Some(range) = parts.next() else { continue };
        let Some(perms) = parts.next() else { continue };
        // Skip offset, dev, inode.
        let _ = parts.next();
        let _ = parts.next();
        let _ = parts.next();
        let path = parts.next().unwrap_or("").trim().to_string();

        let Some((start, end)) = range.split_once('-') else {
            continue;
        };
        let (Ok(start), Ok(end)) = (u64::from_str_radix(start, 16), u64::from_str_radix(end, 16))
        else {
            continue;
        };

        let bytes = perms.as_bytes();
        let region = MemoryRegion {
            start,
            end,
            read: bytes.first() == Some(&b'r'),
            write: bytes.get(1) == Some(&b'w'),
            exec: bytes.get(2) == Some(&b'x'),
            shared: bytes.get(3) == Some(&b's'),
            path,
        };
        regions.push(region);
    }

    Ok(regions)
}

/// All processes visible in `/proc`, unsorted. Kernel threads (which have no
/// command line) are skipped.
pub(crate) fn raw_processes() -> Vec<RawProc> {
    let page_size = page_size();
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<i32>() else {
            continue;
        };

        let comm = fs::read_to_string(format!("/proc/{pid}/comm"))
            .unwrap_or_default()
            .trim()
            .to_string();

        // /proc/<pid>/cmdline is NUL-separated argv.
        let cmdline = fs::read(format!("/proc/{pid}/cmdline"))
            .map(|raw| {
                raw.split(|&b| b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();

        // Kernel threads have an empty cmdline; skip them.
        if cmdline.is_empty() {
            continue;
        }

        out.push(RawProc {
            pid,
            name: comm,
            cmdline,
            rss_bytes: read_rss(pid, page_size),
        });
    }
    out
}

/// The `KEY=VALUE` environment of `pid`, from `/proc/<pid>/environ`. Only
/// readable for our own processes (or as root).
pub(crate) fn environ(pid: i32) -> Option<Vec<String>> {
    let raw = fs::read(format!("/proc/{pid}/environ")).ok()?;
    Some(
        raw.split(|&b| b == 0)
            .map(|kv| String::from_utf8_lossy(kv).into_owned())
            .collect(),
    )
}

/// Resident memory in bytes, from `/proc/<pid>/statm` (field 2 = resident pages).
fn read_rss(pid: i32, page_size: u64) -> u64 {
    let Ok(statm) = fs::read_to_string(format!("/proc/{pid}/statm")) else {
        return 0;
    };
    statm
        .split_whitespace()
        .nth(1)
        .and_then(|p| p.parse::<u64>().ok())
        .map(|pages| pages * page_size)
        .unwrap_or(0)
}

/// System page size in bytes.
fn page_size() -> u64 {
    // SAFETY: sysconf is always safe to call.
    let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if v > 0 {
        v as u64
    } else {
        4096
    }
}

/// `MemAvailable` from `/proc/meminfo`, in bytes.
pub(crate) fn available_memory() -> Option<usize> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|l| l.strip_prefix("MemAvailable:"))
                .and_then(|rest| rest.split_whitespace().next()?.parse::<usize>().ok())
                .map(|kb| kb * 1024)
        })
}

/// Name of the elevation mechanism, for user-facing messages.
pub(crate) const ELEVATION_TOOL: &str = "pkexec";

/// Whether `pkexec` is available to request elevation.
pub(crate) fn elevation_available() -> bool {
    which("pkexec").is_some()
}

/// Re-exec `exe` with `args` through `pkexec`, forwarding the display
/// environment so a GUI can still reach the user's session. Replaces the
/// current process on success; only returns on failure.
pub(crate) fn relaunch_elevated(exe: &Path, args: &[String]) -> io::Error {
    let mut cmd = Command::new("pkexec");
    // `pkexec` scrubs the environment, so re-inject the vars a GUI needs to
    // connect to the running Wayland/X11 session via `env`. HOME and
    // XDG_CONFIG_HOME keep config lookups (e.g. the saved theme) pointing at
    // the invoking user's files instead of /root's.
    cmd.arg("env");
    for key in [
        "HOME",
        "XDG_CONFIG_HOME",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "XAUTHORITY",
        "XDG_SESSION_TYPE",
        "XCURSOR_SIZE",
        "XCURSOR_THEME",
        "QT_QPA_PLATFORM",
        "GDK_BACKEND",
    ] {
        if let Ok(val) = std::env::var(key) {
            cmd.arg(format!("{key}={val}"));
        }
    }
    cmd.arg(exe);
    cmd.args(args);

    // exec() only returns if it fails.
    cmd.exec()
}

/// Minimal `which`: look up `name` on `PATH`.
fn which(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

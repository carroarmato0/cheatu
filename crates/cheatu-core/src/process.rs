//! Process enumeration via `/proc`.

use std::fs;

/// A running process, as shown in the process picker.
#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub pid: i32,
    /// `comm` (short name, up to 15 chars).
    pub name: String,
    /// Full command line (argv joined with spaces), best-effort.
    pub cmdline: String,
    /// Resident set size in bytes (physical memory in use), best-effort.
    pub rss_bytes: u64,
    /// Looks like a Wine/Proton process (Steam Play games run this way).
    pub is_wine: bool,
    /// For multi-process (Chromium/Electron/NW.js) apps, the subprocess role
    /// parsed from `--type=` — e.g. `"renderer"`, `"gpu-process"`, `"utility"`.
    /// `Some("main")` marks the parent process of such an app. `None` otherwise.
    ///
    /// This is the key to games like RPG Maker MV/MZ (NW.js): the game's data
    /// lives in the largest `renderer`.
    pub role: Option<String>,
}

impl ProcInfo {
    /// A single-line label for menus, e.g. `"  12345   612.4 MiB  Game.exe [proton]"`.
    pub fn label(&self) -> String {
        let detail = if self.cmdline.is_empty() {
            self.name.clone()
        } else {
            self.cmdline.clone()
        };
        let tag = if self.is_wine { " [proton]" } else { "" };
        let role = match &self.role {
            Some(r) => format!(" <{r}>"),
            None => String::new(),
        };
        format!(
            "{:>7}  {:>10}  {}{}{}",
            self.pid,
            human_bytes(self.rss_bytes),
            detail,
            tag,
            role
        )
    }
}

/// Format a byte count as a short human string, e.g. `"612.4 MiB"`.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// List all processes visible to the current user, sorted by pid.
///
/// Kernel threads (which have no command line) are skipped so the list stays
/// focused on real, attachable programs such as games.
pub fn list_processes() -> Vec<ProcInfo> {
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

        let is_wine = detect_wine(pid, &cmdline);
        out.push(ProcInfo {
            pid,
            role: detect_role(&cmdline),
            is_wine,
            rss_bytes: read_rss(pid, page_size),
            name: comm,
            cmdline,
        });
    }
    out.sort_by_key(|p| p.pid);
    out
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

/// Heuristic: does this process belong to a Wine/Proton (Steam Play) game?
///
/// The strongest signals live in the environment (which Steam and Proton fill
/// with `SteamAppId`, `WINEPREFIX`, `STEAM_COMPAT_*`, `PROTON_*`); we fall back
/// to a `.exe` in the command line. `environ` is only readable for our own
/// processes, which is exactly the case for a game we launched.
fn detect_wine(pid: i32, cmdline: &str) -> bool {
    if cmdline.to_ascii_lowercase().contains(".exe") {
        return true;
    }
    let Ok(environ) = fs::read(format!("/proc/{pid}/environ")) else {
        return false;
    };
    environ.split(|&b| b == 0).any(|kv| {
        let s = String::from_utf8_lossy(kv);
        s.starts_with("WINEPREFIX=")
            || s.starts_with("STEAM_COMPAT_DATA_PATH=")
            || s.starts_with("STEAM_COMPAT_CLIENT_INSTALL_PATH=")
            || s.starts_with("SteamAppId=")
            || s.starts_with("PROTON")
            || s.starts_with("WINELOADERNOEXEC=")
    })
}

/// Determine the subprocess role of a Chromium/Electron/NW.js process.
///
/// Child processes carry `--type=<role>` (renderer, gpu-process, utility,
/// crashpad-handler, …). The parent has no `--type` but launches a known
/// Chromium-based runtime, which we label `"main"`.
fn detect_role(cmdline: &str) -> Option<String> {
    for tok in cmdline.split_whitespace() {
        if let Some(v) = tok.strip_prefix("--type=") {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    let lower = cmdline.to_ascii_lowercase();
    let is_chromium_runtime = lower.contains("nw.exe")
        || lower.contains("nwjs")
        || lower.contains("chrome")
        || lower.contains("chromium")
        || lower.contains("electron");
    is_chromium_runtime.then(|| "main".to_string())
}

/// Suspend a process (SIGSTOP) so its memory holds still during a scan.
pub fn suspend(pid: i32) -> std::io::Result<()> {
    send_signal(pid, libc::SIGSTOP)
}

/// Resume a previously suspended process (SIGCONT).
pub fn resume(pid: i32) -> std::io::Result<()> {
    send_signal(pid, libc::SIGCONT)
}

fn send_signal(pid: i32, sig: i32) -> std::io::Result<()> {
    // SAFETY: kill is a simple syscall wrapper; we check the return value.
    let rc = unsafe { libc::kill(pid, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
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

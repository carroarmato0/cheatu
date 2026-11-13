//! Process enumeration.

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
    /// Whether the process's memory is readable now, i.e. scanning would work
    /// without elevation. `false` means attaching needs root (or, on Linux,
    /// `CAP_SYS_PTRACE`).
    pub accessible: bool,
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
    let mut out: Vec<ProcInfo> = crate::sys::raw_processes()
        .into_iter()
        .map(|p| ProcInfo {
            pid: p.pid,
            role: detect_role(&p.cmdline),
            is_wine: detect_wine(p.pid, &p.cmdline),
            rss_bytes: p.rss_bytes,
            accessible: crate::mem::accessible(p.pid),
            name: p.name,
            cmdline: p.cmdline,
        })
        .collect();
    out.sort_by_key(|p| p.pid);
    out
}

/// The value of `key` in `pid`'s environment, when it can be read (own
/// processes, or as root).
pub fn env_var(pid: i32, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    crate::sys::environ(pid)?
        .into_iter()
        .find_map(|kv| kv.strip_prefix(&prefix).map(str::to_owned))
}

/// Heuristic: does this process belong to a Wine/Proton (Steam Play) game?
///
/// The strongest signals live in the environment (which Steam and Proton fill
/// with `SteamAppId`, `WINEPREFIX`, `STEAM_COMPAT_*`, `PROTON_*`); we fall back
/// to a `.exe` in the command line. The environment is only readable for our
/// own processes, which is exactly the case for a game we launched.
fn detect_wine(pid: i32, cmdline: &str) -> bool {
    if cmdline.to_ascii_lowercase().contains(".exe") {
        return true;
    }
    let Some(environ) = crate::sys::environ(pid) else {
        return false;
    };
    environ.iter().any(|s| {
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

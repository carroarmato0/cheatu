//! Privilege checks and re-launching with elevated rights.
//!
//! Scanning another user's (or a same-user but non-child) process's memory
//! requires root or `CAP_SYS_PTRACE`. We use `pkexec` (polkit) to request
//! elevation, which shows the standard graphical/terminal auth prompt on KDE,
//! GNOME, and most other desktops.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// Guard flag we append when relaunching, to avoid infinite elevation loops.
pub const ELEVATED_FLAG: &str = "--already-elevated";

/// Flag carrying a pid to auto-attach to after an elevated relaunch.
pub const ATTACH_FLAG: &str = "--attach";

/// Whether the current process is running as root.
pub fn is_root() -> bool {
    // SAFETY: geteuid is always safe to call.
    unsafe { libc::geteuid() == 0 }
}

/// Whether `pkexec` is available to request elevation.
pub fn pkexec_available() -> bool {
    which("pkexec").is_some()
}

/// Re-exec this program through `pkexec`, forwarding the current arguments plus
/// the display environment so a GUI can still reach the user's session.
/// `extra_args` are appended after the forwarded ones (e.g. an [`ATTACH_FLAG`]
/// pid so the elevated instance re-attaches to the target).
///
/// On success this call **replaces** the current process and never returns.
/// It only returns (with an `Err`) if `pkexec` could not be started or the
/// user cancelled/failed authentication.
pub fn relaunch_elevated(extra_args: &[String]) -> io::Error {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return e,
    };

    // Forward everything except our own guard flag and any prior `--attach
    // <pid>` pair (so a re-elevation doesn't carry a stale target).
    let mut args: Vec<String> = Vec::new();
    let mut forwarded = std::env::args().skip(1);
    while let Some(a) = forwarded.next() {
        if a == ATTACH_FLAG {
            forwarded.next(); // drop the pid that follows
            continue;
        }
        if a == ELEVATED_FLAG {
            continue;
        }
        args.push(a);
    }
    args.extend_from_slice(extra_args);

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
    cmd.args(&args);
    cmd.arg(ELEVATED_FLAG);

    // exec() only returns if it fails.
    cmd.exec()
}

/// Whether this invocation is the post-elevation relaunch (guard flag present).
pub fn is_relaunch() -> bool {
    std::env::args().any(|a| a == ELEVATED_FLAG)
}

/// The pid passed via [`ATTACH_FLAG`], if any — set when relaunching elevated so
/// the new instance can re-attach to the target automatically.
pub fn attach_target() -> Option<i32> {
    let mut args = std::env::args();
    while let Some(a) = args.next() {
        if a == ATTACH_FLAG {
            return args.next().and_then(|p| p.parse().ok());
        }
    }
    None
}

/// Minimal `which`: look up `name` on `PATH`.
fn which(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

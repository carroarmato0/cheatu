//! Privilege checks and re-launching with elevated rights.
//!
//! Scanning another user's (or a same-user but non-child) process's memory
//! requires root (or `CAP_SYS_PTRACE` on Linux). On Linux we use `pkexec`
//! (polkit) to request elevation, which shows the standard graphical/terminal
//! auth prompt on KDE, GNOME, and most other desktops. On macOS we use the
//! native administrator prompt.

use std::io;

use crate::sys;

/// Guard flag we append when relaunching, to avoid infinite elevation loops.
pub const ELEVATED_FLAG: &str = "--already-elevated";

/// Flag carrying a pid to auto-attach to after an elevated relaunch.
pub const ATTACH_FLAG: &str = "--attach";

/// Whether the current process is running as root.
pub fn is_root() -> bool {
    // SAFETY: geteuid is always safe to call.
    unsafe { libc::geteuid() == 0 }
}

/// Whether elevation can be requested (`pkexec` on Linux, the admin prompt on
/// macOS).
pub fn elevation_available() -> bool {
    sys::elevation_available()
}

/// The elevation mechanism, for user-facing messages: `"pkexec"` on Linux,
/// `"the macOS admin prompt"` on macOS.
pub fn elevation_tool() -> &'static str {
    sys::ELEVATION_TOOL
}

/// Relaunch this program elevated (`pkexec` on Linux, the admin prompt on
/// macOS), forwarding the current arguments.
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

    args.push(ELEVATED_FLAG.to_string());

    sys::relaunch_elevated(&exe, &args)
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

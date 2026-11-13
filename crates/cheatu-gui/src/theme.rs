//! Light or dark: the user's choice, or the desktop's.
//!
//! winit only reports the system theme on macOS, and cheatu usually runs as
//! root, which sees root's preferences (macOS) or cannot reach the user's
//! session bus at all (Linux). So the desktop's setting is read with the
//! platform's own tools, as the invoking user, every [`POLL`] on a
//! background thread.

use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;

/// How often the desktop's setting is checked while following it.
const POLL: Duration = Duration::from_secs(2);

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

/// The theme setting. `System` follows the desktop.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum ThemePref {
    #[default]
    System,
    Dark,
    Light,
}

impl ThemePref {
    /// The setting saved as `name`; anything unknown or missing follows the
    /// system.
    pub fn from_name(name: Option<&str>) -> ThemePref {
        match name {
            Some("dark") => ThemePref::Dark,
            Some("light") => ThemePref::Light,
            _ => ThemePref::System,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ThemePref::System => "system",
            ThemePref::Dark => "dark",
            ThemePref::Light => "light",
        }
    }

    /// The theme to show, given what the desktop says (if known). Unknown is
    /// dark, cheatu's look before this setting existed.
    pub fn resolve(self, system: Option<Theme>) -> Theme {
        match self {
            ThemePref::Dark => Theme::Dark,
            ThemePref::Light => Theme::Light,
            ThemePref::System => system.unwrap_or(Theme::Dark),
        }
    }
}

/// The desktop's theme, kept current by a background thread while
/// [`set_following`](Self::set_following) is on.
pub struct SystemTheme {
    /// 0 unknown, 1 dark, 2 light.
    current: Arc<AtomicU8>,
    following: Arc<AtomicBool>,
}

fn encode(t: Option<Theme>) -> u8 {
    match t {
        None => 0,
        Some(Theme::Dark) => 1,
        Some(Theme::Light) => 2,
    }
}

impl SystemTheme {
    /// Detect once now (so the first frame is right), then keep watching.
    /// `on_change` runs on the watcher thread whenever the theme changes.
    pub fn start(following: bool, on_change: impl Fn() + Send + 'static) -> SystemTheme {
        let current = Arc::new(AtomicU8::new(if following { encode(detect()) } else { 0 }));
        let following = Arc::new(AtomicBool::new(following));
        let (cur, fol) = (current.clone(), following.clone());
        // Without the thread the theme is detected once and kept.
        let _ = std::thread::Builder::new()
            .name("system-theme".into())
            .spawn(move || loop {
                std::thread::sleep(POLL);
                if !fol.load(Relaxed) {
                    continue;
                }
                let t = encode(detect());
                if cur.swap(t, Relaxed) != t {
                    on_change();
                }
            });
        SystemTheme { current, following }
    }

    pub fn get(&self) -> Option<Theme> {
        match self.current.load(Relaxed) {
            1 => Some(Theme::Dark),
            2 => Some(Theme::Light),
            _ => None,
        }
    }

    /// Start or stop following. Starting detects at once.
    pub fn set_following(&self, on: bool) {
        if on && !self.following.load(Relaxed) {
            self.current.store(encode(detect()), Relaxed);
        }
        self.following.store(on, Relaxed);
    }
}

/// Run `program args…`, as the user who started cheatu when elevated.
fn run(program: &str, args: &[&str]) -> Option<Output> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(std::process::Stdio::null());
    as_invoking_user(&mut cmd);
    cmd.output().ok()
}

fn stdout(out: &Output) -> &str {
    std::str::from_utf8(&out.stdout).unwrap_or("")
}

/// The desktop's light/dark setting, if it can be read.
pub fn detect() -> Option<Theme> {
    detect_platform()
}

#[cfg(target_os = "macos")]
fn detect_platform() -> Option<Theme> {
    // As root, `-g` would read root's preferences; read the invoking user's
    // file instead (cheatu keeps HOME when elevating).
    // SAFETY: geteuid has no preconditions.
    let out = if unsafe { libc::geteuid() } == 0 {
        let home = std::env::var("HOME").ok()?;
        let prefs = format!("{home}/Library/Preferences/.GlobalPreferences");
        run(
            "/usr/bin/defaults",
            &["read", &prefs, "AppleInterfaceStyle"],
        )?
    } else {
        run("/usr/bin/defaults", &["read", "-g", "AppleInterfaceStyle"])?
    };
    let stderr = std::str::from_utf8(&out.stderr).unwrap_or("");
    parse_apple_style(out.status.success(), stdout(&out), stderr)
}

#[cfg(not(target_os = "macos"))]
fn detect_platform() -> Option<Theme> {
    let portal = run(
        "gdbus",
        &[
            "call",
            "--session",
            "--timeout",
            "1",
            "--dest",
            "org.freedesktop.portal.Desktop",
            "--object-path",
            "/org/freedesktop/portal/desktop",
            "--method",
            "org.freedesktop.portal.Settings.Read",
            "org.freedesktop.appearance",
            "color-scheme",
        ],
    );
    if let Some(t) = portal.as_ref().and_then(|o| parse_portal(stdout(o))) {
        return Some(t);
    }
    const IFACE: &str = "org.gnome.desktop.interface";
    if let Some(t) = run("gsettings", &["get", IFACE, "color-scheme"])
        .and_then(|o| parse_color_scheme(stdout(&o)))
    {
        return Some(t);
    }
    if let Some(t) =
        run("gsettings", &["get", IFACE, "gtk-theme"]).and_then(|o| parse_gtk_theme(stdout(&o)))
    {
        return Some(t);
    }
    // The portal answered "no preference": that is light.
    portal.filter(|o| o.status.success()).map(|_| Theme::Light)
}

/// Elevated through pkexec (Linux), run as the invoking user against their
/// session bus: root cannot use another user's session bus.
#[cfg(not(target_os = "macos"))]
fn as_invoking_user(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let Some(uid) = std::env::var("PKEXEC_UID")
        .or_else(|_| std::env::var("SUDO_UID"))
        .ok()
        .and_then(|u| u.parse::<u32>().ok())
    else {
        return;
    };
    // SAFETY: getpwuid returns null or a pointer to static storage, read at
    // once on this thread (the only caller).
    let gid = unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return;
        }
        (*pw).pw_gid
    };
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{uid}"));
    cmd.uid(uid)
        .gid(gid)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={runtime}/bus"),
        )
        .env("XDG_RUNTIME_DIR", runtime);
}

#[cfg(target_os = "macos")]
fn as_invoking_user(_cmd: &mut Command) {}

/// `gdbus call … org.freedesktop.portal.Settings.Read org.freedesktop.appearance
/// color-scheme` output, e.g. `(<<uint32 1>>,)`. 1 is dark, 2 light, 0 "no
/// preference" (`None`, so the caller asks elsewhere).
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn parse_portal(out: &str) -> Option<Theme> {
    let (_, rest) = out.split_once("uint32 ")?;
    let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
    match n.as_str() {
        "1" => Some(Theme::Dark),
        "2" => Some(Theme::Light),
        _ => None,
    }
}

/// `gsettings get org.gnome.desktop.interface color-scheme`, e.g.
/// `'prefer-dark'`. `'default'` says nothing (`None`).
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn parse_color_scheme(out: &str) -> Option<Theme> {
    match out.trim().trim_matches('\'') {
        "prefer-dark" => Some(Theme::Dark),
        "prefer-light" => Some(Theme::Light),
        _ => None,
    }
}

/// `gsettings get org.gnome.desktop.interface gtk-theme`, e.g.
/// `'Adwaita-dark'`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn parse_gtk_theme(out: &str) -> Option<Theme> {
    let name = out.trim().trim_matches('\'');
    if name.is_empty() {
        None
    } else if name.to_ascii_lowercase().contains("dark") {
        Some(Theme::Dark)
    } else {
        Some(Theme::Light)
    }
}

/// `defaults read … AppleInterfaceStyle`: `Dark` when dark; in light mode the
/// key does not exist and `defaults` fails saying so (worded differently
/// across macOS versions, but always naming the key).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_apple_style(success: bool, stdout: &str, stderr: &str) -> Option<Theme> {
    if success {
        Some(if stdout.trim() == "Dark" {
            Theme::Dark
        } else {
            Theme::Light
        })
    } else if stderr.contains("AppleInterfaceStyle") {
        Some(Theme::Light)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pref_names_round_trip_and_default_to_system() {
        for p in [ThemePref::System, ThemePref::Dark, ThemePref::Light] {
            assert_eq!(ThemePref::from_name(Some(p.name())), p);
        }
        assert_eq!(ThemePref::System.name(), "system");
        assert_eq!(ThemePref::from_name(None), ThemePref::System);
        assert_eq!(ThemePref::from_name(Some("plaid")), ThemePref::System);
        // Saved before `system` existed: still honoured.
        assert_eq!(ThemePref::from_name(Some("dark")), ThemePref::Dark);
        assert_eq!(ThemePref::from_name(Some("light")), ThemePref::Light);
    }

    #[test]
    fn resolve_follows_the_desktop_only_for_system() {
        use Theme::*;
        assert_eq!(ThemePref::System.resolve(Some(Light)), Light);
        assert_eq!(ThemePref::System.resolve(Some(Dark)), Dark);
        assert_eq!(ThemePref::System.resolve(None), Dark);
        assert_eq!(ThemePref::Dark.resolve(Some(Light)), Dark);
        assert_eq!(ThemePref::Light.resolve(Some(Dark)), Light);
    }

    #[test]
    fn portal_output() {
        assert_eq!(parse_portal("(<<uint32 1>>,)\n"), Some(Theme::Dark));
        assert_eq!(parse_portal("(<<<uint32 2>>>,)\n"), Some(Theme::Light));
        assert_eq!(parse_portal("(<<uint32 0>>,)\n"), None);
        assert_eq!(parse_portal(""), None);
        assert_eq!(parse_portal("Error: GDBus.Error:…"), None);
    }

    #[test]
    fn gsettings_output() {
        assert_eq!(parse_color_scheme("'prefer-dark'\n"), Some(Theme::Dark));
        assert_eq!(parse_color_scheme("'prefer-light'\n"), Some(Theme::Light));
        assert_eq!(parse_color_scheme("'default'\n"), None);
        assert_eq!(parse_gtk_theme("'Adwaita-dark'\n"), Some(Theme::Dark));
        assert_eq!(parse_gtk_theme("'Breeze-Dark'\n"), Some(Theme::Dark));
        assert_eq!(parse_gtk_theme("'Adwaita'\n"), Some(Theme::Light));
        assert_eq!(parse_gtk_theme(""), None);
    }

    #[test]
    fn apple_style_output() {
        assert_eq!(parse_apple_style(true, "Dark\n", ""), Some(Theme::Dark));
        let missing = "The domain/default pair of (kCFPreferencesAnyApplication, \
                       AppleInterfaceStyle) does not exist\n";
        assert_eq!(parse_apple_style(false, "", missing), Some(Theme::Light));
        // Newer macOS words it differently.
        let missing = "Error: Could not find key 'AppleInterfaceStyle' in domain \
                       'kCFPreferencesAnyApplication'.\n";
        assert_eq!(parse_apple_style(false, "", missing), Some(Theme::Light));
        assert_eq!(parse_apple_style(false, "", "something else broke"), None);
    }
}

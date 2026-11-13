//! The GUI's preferences in `config.json`: theme, the Advanced view and
//! whether the probe is enabled.

use std::io;
use std::path::{Path, PathBuf};

use crate::theme::ThemePref;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct Settings {
    pub theme: ThemePref,
    /// Show the full scanner and every tool instead of the guided steps.
    pub advanced: bool,
    /// Offer the probe (test writes) in the guided view.
    pub probe: bool,
}

/// Path of `config.json`, the platform's convention for user config.
/// XDG base-dir spec: $XDG_CONFIG_HOME/cheatu/config.json, falling back to
/// ~/.config/cheatu/config.json (relative XDG_CONFIG_HOME must be ignored).
#[cfg(not(target_os = "macos"))]
pub fn config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("cheatu")
        .join("config.json")
}

/// ~/Library/Application Support/cheatu/config.json, the macOS convention.
#[cfg(target_os = "macos")]
pub fn config_path() -> PathBuf {
    home_dir().join("Library/Application Support/cheatu/config.json")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn load() -> Settings {
    load_from(&config_path())
}

/// Read `path`. Missing, unreadable or corrupt files and unknown or
/// mistyped keys give the defaults, so a config written by another version
/// never stops cheatu from starting.
pub fn load_from(path: &Path) -> Settings {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Settings::default();
    };
    let flag = |key: &str| {
        v.get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    Settings {
        theme: ThemePref::from_name(v.get("theme").and_then(serde_json::Value::as_str)),
        advanced: flag("advanced"),
        probe: flag("probe"),
    }
}

pub fn save(s: &Settings) -> io::Result<()> {
    save_to(&config_path(), &home_dir(), s)
}

pub fn save_to(path: &Path, home: &Path, s: &Settings) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::json!({
        "theme": s.theme.name(),
        "advanced": s.advanced,
        "probe": s.probe,
    });
    std::fs::write(path, json.to_string())?;
    give_config_to_home_owner(path, home)
}

/// Elevated, cheatu keeps the user's HOME, so it writes the user's config: as
/// root, hand the config directory and file to the owner of `home`, or the
/// user's own cheatu can no longer save there. Also repairs a directory an
/// earlier root instance left behind. Does nothing unless running as root.
pub fn give_config_to_home_owner(path: &Path, home: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    let owner = std::fs::metadata(home)?;
    if owner.uid() == 0 {
        return Ok(());
    }
    // `path` and its parents below `home`. Never through a symlink, and only
    // what root owns: nothing outside the user's files changes hands.
    for p in path
        .ancestors()
        .take_while(|p| *p != home && p.starts_with(home))
    {
        let Ok(m) = std::fs::symlink_metadata(p) else {
            continue;
        };
        if m.uid() == 0 && (m.is_dir() || m.is_file()) {
            std::os::unix::fs::lchown(p, Some(owner.uid()), Some(owner.gid()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> (PathBuf, PathBuf) {
        let home =
            std::env::temp_dir().join(format!("cheatu-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let path = home.join("cfg").join("cheatu").join("config.json");
        (home, path)
    }

    fn with_file(name: &str, text: &str) -> (PathBuf, PathBuf) {
        let (home, path) = scratch(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        (home, path)
    }

    #[test]
    fn missing_config_gives_defaults() {
        let (home, path) = scratch("missing");
        assert_eq!(load_from(&path), Settings::default());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn settings_round_trip() {
        let (home, path) = scratch("roundtrip");
        let s = Settings {
            theme: ThemePref::Light,
            advanced: true,
            probe: true,
        };
        save_to(&path, &home, &s).unwrap();
        assert_eq!(load_from(&path), s);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn old_theme_only_config_loads() {
        let (home, path) = with_file("old", r#"{"theme":"light"}"#);
        let s = load_from(&path);
        assert_eq!(
            s,
            Settings {
                theme: ThemePref::Light,
                ..Settings::default()
            }
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn corrupt_config_gives_defaults() {
        let (home, path) = with_file("corrupt", "{not json");
        assert_eq!(load_from(&path), Settings::default());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let (home, path) = with_file("unknown", r#"{"theme":"dark","future":1,"advanced":"yes"}"#);
        let s = load_from(&path);
        assert_eq!(
            s,
            Settings {
                theme: ThemePref::Dark,
                ..Settings::default()
            }
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn unwritable_config_reports_an_error() {
        let (home, path) = scratch("unwritable");
        // A file where the directory should be.
        std::fs::create_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
        std::fs::write(path.parent().unwrap(), b"").unwrap();
        assert!(save_to(
            &path,
            &home,
            &Settings {
                theme: ThemePref::Light,
                ..Settings::default()
            }
        )
        .is_err());
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// Root saving into a user's HOME must leave the config theirs, and
    /// repair a directory an earlier root instance took. Root only (the
    /// Linux test container).
    #[test]
    fn root_save_gives_config_to_home_owner() {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        let (home, path) = scratch("owner");
        std::os::unix::fs::chown(&home, Some(1000), Some(1000)).unwrap();
        // Left behind by an earlier root instance.
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{}").unwrap();
        save_to(
            &path,
            &home,
            &Settings {
                theme: ThemePref::Light,
                ..Settings::default()
            },
        )
        .unwrap();
        for p in [path.parent().unwrap(), path.as_path()] {
            let m = std::fs::metadata(p).unwrap();
            assert_eq!((m.uid(), m.gid()), (1000, 1000), "{}", p.display());
        }
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn config_lives_in_application_support_on_macos() {
        assert!(config_path().ends_with("Library/Application Support/cheatu/config.json"));
    }
}

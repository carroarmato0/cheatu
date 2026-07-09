//! Enable/disable the DevTools port in an NW.js game's `package.json`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The DevTools port cheatu uses by default.
pub const DEFAULT_PORT: u16 = 9222;

/// Locate the NW.js manifest that carries `chromium-args`.
///
/// NW.js reads the top-level `package.json` (the one whose `main` points into
/// `www/`), so that's the one to edit.
pub fn manifest_path(game_dir: &Path) -> PathBuf {
    game_dir.join("package.json")
}

/// Add `--remote-debugging-port=<port>` to the manifest's `chromium-args`,
/// backing up the original to `package.json.cheatu-bak` first. Idempotent.
///
/// Returns `true` if the file was changed, `false` if it was already enabled.
pub fn enable_remote_debugging(game_dir: &Path, port: u16) -> Result<bool, String> {
    let path = manifest_path(game_dir);
    let text = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut manifest: Value =
        serde_json::from_str(&text).map_err(|e| format!("parse package.json: {e}"))?;

    let args = manifest
        .get("chromium-args")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if args.contains("--remote-debugging-port") {
        return Ok(false);
    }

    // Back up once (don't clobber an existing backup).
    let backup = path.with_extension("json.cheatu-bak");
    if !backup.exists() {
        fs::copy(&path, &backup).map_err(|e| format!("backup: {e}"))?;
    }

    let new_args = if args.is_empty() {
        format!("--remote-debugging-port={port}")
    } else {
        format!("{args} --remote-debugging-port={port}")
    };
    manifest["chromium-args"] = Value::String(new_args);

    let pretty = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    fs::write(&path, pretty).map_err(|e| format!("write: {e}"))?;
    Ok(true)
}

/// Restore the manifest from the backup made by [`enable_remote_debugging`].
pub fn disable_remote_debugging(game_dir: &Path) -> Result<bool, String> {
    let path = manifest_path(game_dir);
    let backup = path.with_extension("json.cheatu-bak");
    if !backup.exists() {
        return Ok(false);
    }
    fs::copy(&backup, &path).map_err(|e| format!("restore: {e}"))?;
    fs::remove_file(&backup).ok();
    Ok(true)
}

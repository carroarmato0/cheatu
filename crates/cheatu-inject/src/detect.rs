//! Detect the engine behind a game directory, to choose a backend.

use std::path::Path;

/// What kind of application a game directory holds.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Engine {
    /// RPG Maker MV (NW.js, `rpg_core.js`).
    RpgMakerMV,
    /// RPG Maker MZ (NW.js, `rmmz_core.js`).
    RpgMakerMZ,
    /// Some other NW.js app (has `nw.exe`/`nw` but no RPG Maker core).
    Nwjs,
    /// Not recognized — use the raw memory scanner.
    Unknown,
}

impl Engine {
    /// Whether the JS-injection backend applies (an NW.js runtime is present).
    pub fn is_nwjs(self) -> bool {
        !matches!(self, Engine::Unknown)
    }

    /// Whether this is an RPG Maker title with a known data model.
    pub fn is_rpgmaker(self) -> bool {
        matches!(self, Engine::RpgMakerMV | Engine::RpgMakerMZ)
    }

    pub fn label(self) -> &'static str {
        match self {
            Engine::RpgMakerMV => "RPG Maker MV",
            Engine::RpgMakerMZ => "RPG Maker MZ",
            Engine::Nwjs => "NW.js app",
            Engine::Unknown => "unknown / native",
        }
    }
}

/// Inspect a game directory and guess its engine from characteristic files.
///
/// RPG Maker games keep their code under `www/js` (MV) or `js` (MZ/newer MV
/// packaging); MV ships `rpg_core.js`, MZ ships `rmmz_core.js`.
pub fn detect_dir(dir: &Path) -> Engine {
    let www = dir.join("www");
    let base = if www.is_dir() { www } else { dir.to_path_buf() };

    let has = |rel: &str| base.join(rel).exists();

    if has("js/rmmz_core.js") {
        Engine::RpgMakerMZ
    } else if has("js/rpg_core.js") {
        Engine::RpgMakerMV
    } else if is_nwjs_dir(dir) {
        Engine::Nwjs
    } else {
        Engine::Unknown
    }
}

fn is_nwjs_dir(dir: &Path) -> bool {
    dir.join("nw.exe").exists()
        || dir.join("nw").exists()
        || dir.join("nw.dll").exists()
        || dir.join("package.json").exists() && dir.join("www").is_dir()
}

/// A discovered game directory and its detected engine.
#[derive(Clone, Debug)]
pub struct FoundGame {
    pub path: std::path::PathBuf,
    pub name: String,
    pub engine: Engine,
}

/// Scan common Steam library locations for NW.js / RPG Maker games, so the GUI
/// can offer a pick-list instead of making the user hunt for the folder.
///
/// Several of these roots are symlinks to the same real directory (e.g.
/// `~/.steam/steam` → `~/.local/share/Steam`), so paths are canonicalized and
/// de-duplicated by their real location — no double entries.
pub fn find_rpgmaker_games() -> Vec<FoundGame> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let Some(home) = std::env::var_os("HOME") else {
        return out;
    };
    let home = Path::new(&home);
    let roots = [
        home.join(".local/share/Steam/steamapps/common"),
        home.join(".steam/steam/steamapps/common"),
        home.join(".steam/steamapps/common"),
        home.join(".steam/root/steamapps/common"),
        home.join(".var/app/com.valvesoftware.Steam/data/Steam/steamapps/common"),
    ];
    for root in roots {
        scan_one_level(&root, &mut seen, &mut out);
    }
    out.sort_by_key(|g| g.name.to_ascii_lowercase());
    out
}

/// Scan an arbitrary directory for NW.js / RPG Maker games — the directory
/// itself and its immediate subfolders. Used by the GUI's "scan this folder"
/// so loose (non-Steam) games can be discovered from a library root.
pub fn scan_dir(root: &Path) -> Vec<FoundGame> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let engine = detect_dir(root);
    if engine.is_nwjs() {
        push_game(root.to_path_buf(), engine, &mut seen, &mut out);
    }
    scan_one_level(root, &mut seen, &mut out);
    out.sort_by_key(|g| g.name.to_ascii_lowercase());
    out
}

/// Add every immediate subdirectory of `root` that is an NW.js game.
fn scan_one_level(
    root: &Path,
    seen: &mut std::collections::HashSet<std::path::PathBuf>,
    out: &mut Vec<FoundGame>,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let engine = detect_dir(&path);
        if engine.is_nwjs() {
            push_game(path, engine, seen, out);
        }
    }
}

/// Canonicalize (resolve symlinks) and record a game once.
fn push_game(
    path: std::path::PathBuf,
    engine: Engine,
    seen: &mut std::collections::HashSet<std::path::PathBuf>,
    out: &mut Vec<FoundGame>,
) {
    let real = std::fs::canonicalize(&path).unwrap_or(path);
    if seen.insert(real.clone()) {
        out.push(FoundGame {
            name: real
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: real,
            engine,
        });
    }
}

//! Is a running process an RPG Maker game? From its program path, find the
//! game folder and ask `cheatu_inject::detect_dir`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cheatu_core::process::ProcInfo;
use cheatu_inject::Engine;

/// Most leading words of a command line tried as the program path (paths
/// may contain spaces).
const MAX_PATH_WORDS: usize = 16;

/// Levels above the program's folder searched for the game's files.
const MAX_LEVELS_UP: usize = 3;

/// A Windows path as Wine sees it (`X:\a\b`), as a host path: `Z:` is the
/// host root; other drives go through the prefix's `dosdevices/x:` link.
pub fn wine_to_unix(path: &str, prefix: Option<&Path>) -> Option<PathBuf> {
    let b = path.as_bytes();
    if b.len() < 3 || b[1] != b':' || b[2] != b'\\' || !b[0].is_ascii_alphabetic() {
        return None;
    }
    let rest = path[3..].replace('\\', "/");
    match b[0].to_ascii_lowercase() {
        b'z' => Some(PathBuf::from("/").join(rest)),
        d => Some(
            prefix?
                .join("dosdevices")
                .join(format!("{}:", d as char))
                .join(rest),
        ),
    }
}

/// The directory holding the program a command line starts: the longest
/// run of leading words (paths may contain spaces) that names a file.
pub fn program_dir(cmdline: &str, prefix: Option<&Path>) -> Option<PathBuf> {
    let words: Vec<&str> = cmdline.split_whitespace().take(MAX_PATH_WORDS).collect();
    (1..=words.len()).find_map(|k| {
        let candidate = words[..k].join(" ");
        let path = wine_to_unix(&candidate, prefix).unwrap_or_else(|| PathBuf::from(&candidate));
        path.is_file()
            .then(|| path.parent().map(Path::to_path_buf))?
    })
}

/// The RPG Maker engine of the game a command line starts, if any.
pub fn detect_game(cmdline: &str, prefix: Option<&Path>) -> Option<Engine> {
    game_dir(cmdline, prefix).map(|(_, e)| e)
}

/// The folder where the game's RPG Maker files are, with the engine.
pub fn game_dir(cmdline: &str, prefix: Option<&Path>) -> Option<(PathBuf, Engine)> {
    let dir = program_dir(cmdline, prefix)?;
    dir.ancestors().take(MAX_LEVELS_UP + 1).find_map(|d| {
        let e = cheatu_inject::detect_dir(d);
        e.is_rpgmaker().then(|| (d.to_path_buf(), e))
    })
}

/// The folder to prefill in RPG Maker mode for a running game: where its
/// RPG Maker files were found.
pub fn rpg_folder_for(cmdline: &str, prefix: Option<&Path>) -> Option<PathBuf> {
    game_dir(cmdline, prefix).map(|(dir, _)| dir)
}

/// Detection results per pid, so the game list does not hit the disk every
/// frame.
#[derive(Default)]
pub struct Detections {
    by_pid: HashMap<i32, Option<Engine>>,
}

impl Detections {
    pub fn get(&mut self, p: &ProcInfo) -> Option<Engine> {
        *self.by_pid.entry(p.pid).or_insert_with(|| {
            let prefix = cheatu_core::process::env_var(p.pid, "WINEPREFIX").map(PathBuf::from);
            detect_game(&p.cmdline, prefix.as_deref())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cheatu-rpgdet-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"").unwrap();
    }

    #[test]
    fn unix_path_detects_mz() {
        let d = tmp("mz");
        touch(&d.join("Sakura/js/rmmz_core.js"));
        touch(&d.join("Sakura/Game"));
        let cmd = format!("{}/Sakura/Game --flag", d.display());
        assert_eq!(detect_game(&cmd, None), Some(Engine::RpgMakerMZ));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn walks_up_three_levels() {
        let d = tmp("walk");
        touch(&d.join("G/www/js/rpg_core.js"));
        touch(&d.join("G/bin/x64/nw"));
        assert_eq!(
            detect_game(&format!("{}/G/bin/x64/nw", d.display()), None),
            Some(Engine::RpgMakerMV)
        );
        touch(&d.join("H/www/js/rpg_core.js"));

        // The program's folder plus three levels up, no further.
        touch(&d.join("H/a/b/c/nw"));
        assert_eq!(
            detect_game(&format!("{}/H/a/b/c/nw", d.display()), None),
            Some(Engine::RpgMakerMV)
        );
        touch(&d.join("H/a/b/c/d/nw"));
        assert_eq!(
            detect_game(&format!("{}/H/a/b/c/d/nw", d.display()), None),
            None
        );
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn z_drive_translates() {
        assert_eq!(
            wine_to_unix("Z:\\home\\u\\G\\Game.exe", None),
            Some(PathBuf::from("/home/u/G/Game.exe"))
        );
    }

    #[test]
    fn c_drive_needs_prefix() {
        assert_eq!(wine_to_unix("C:\\G\\Game.exe", None), None);
        let d = tmp("prefix");
        fs::create_dir_all(d.join("drive_c")).unwrap();
        fs::create_dir_all(d.join("dosdevices")).unwrap();
        std::os::unix::fs::symlink("../drive_c", d.join("dosdevices/c:")).unwrap();
        assert_eq!(
            wine_to_unix("C:\\G\\Game.exe", Some(&d)),
            Some(d.join("dosdevices/c:").join("G/Game.exe"))
        );
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn wine_path_with_spaces_detects() {
        let d = tmp("spaces");
        fs::create_dir_all(d.join("dosdevices")).unwrap();
        std::os::unix::fs::symlink("../drive_c", d.join("dosdevices/c:")).unwrap();
        touch(&d.join("drive_c/Program Files/Sakura/js/rmmz_core.js"));
        touch(&d.join("drive_c/Program Files/Sakura/Game.exe"));
        let cmd = "C:\\Program Files\\Sakura\\Game.exe --no-sandbox";
        assert_eq!(detect_game(cmd, Some(&d)), Some(Engine::RpgMakerMZ));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn rpg_folder_prefill_uses_detected_dir() {
        let d = tmp("prefill");
        touch(&d.join("G/www/js/rpg_core.js"));
        touch(&d.join("G/bin/nw"));
        let cmd = format!("{}/G/bin/nw --x", d.display());
        assert_eq!(rpg_folder_for(&cmd, None), Some(d.join("G")));
        assert_eq!(rpg_folder_for("/bin/ls", None), None);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn not_a_game() {
        assert_eq!(detect_game("/bin/ls -la", None), None);
        assert_eq!(detect_game("", None), None);
    }
}

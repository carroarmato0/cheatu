//! A small dependency-free file dialog drawn with egui: browse folders, pick
//! a file with a given extension, or type a name to save under.
//!
//! cheatu usually runs as root, where native dialogs (GTK, portals, a root
//! NSSavePanel) are unreliable, so it draws its own, in the style of the RPG
//! Maker folder browser.

use std::io;
use std::path::{Path, PathBuf};

/// Pick an existing file, or name one to write.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Open,
    Save,
}

/// The folder to start in: the last one used if it still exists, else
/// `home`, else the filesystem root.
pub fn start_dir(last: Option<&Path>, home: &Path) -> PathBuf {
    match last {
        Some(d) if d.is_dir() => d.to_path_buf(),
        _ if home.is_dir() => home.to_path_buf(),
        _ => PathBuf::from("/"),
    }
}

/// Whether `path` ends in `.ext`, ignoring case.
pub fn has_ext(path: &Path, ext: &str) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    let suffix = format!(".{}", ext.to_ascii_lowercase());
    lower.len() > suffix.len() && lower.ends_with(&suffix)
}

/// The visible contents of a folder.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub dirs: Vec<(String, PathBuf)>,
    /// Only files ending in the dialog's extension.
    pub files: Vec<(String, PathBuf)>,
}

/// List `dir`: sub-folders and files ending in `.ext`, each sorted by name
/// ignoring case. Hidden entries (starting with `.`) are left out.
pub fn list_dir(dir: &Path, ext: &str) -> io::Result<Listing> {
    let mut l = Listing::default();
    for ent in std::fs::read_dir(dir)?.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = ent.path();
        // Follows symlinks, so a linked folder can be entered.
        if path.is_dir() {
            l.dirs.push((name, path));
        } else if has_ext(&path, ext) {
            l.files.push((name, path));
        }
    }
    let by_name = |a: &(String, PathBuf), b: &(String, PathBuf)| {
        a.0.to_lowercase()
            .cmp(&b.0.to_lowercase())
            .then_with(|| a.0.cmp(&b.0))
    };
    l.dirs.sort_by(by_name);
    l.files.sort_by(by_name);
    Ok(l)
}

/// Where a save with file name `name` typed in `dir` goes. Adds `.ext` when
/// the name lacks it; refuses empty names and names with a path in them.
pub fn save_target(dir: &Path, name: &str, ext: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a file name.".into());
    }
    if name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return Err(format!("\"{name}\" is not a valid file name."));
    }
    let file = if has_ext(Path::new(name), ext) {
        name.to_string()
    } else {
        format!("{name}.{ext}")
    };
    Ok(dir.join(file))
}

/// What the user did with the dialog.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Open: an existing file. Save: the file to write (overwriting already
    /// confirmed).
    Picked(PathBuf),
    Cancelled,
}

/// The platform's own open/save panel is used instead of [`FileDialog`]: on
/// macOS a file picked there is reachable even when privacy protection
/// (TCC) keeps an elevated cheatu out of its folder (Downloads, Documents…).
pub fn uses_native() -> bool {
    cfg!(target_os = "macos")
}

/// What a native panel returned, as an [`Outcome`]: a saved file gets the
/// extension when it was typed without it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn native_outcome(mode: Mode, picked: Option<PathBuf>, ext: &str) -> Outcome {
    match picked {
        None => Outcome::Cancelled,
        Some(p) if mode == Mode::Save && !has_ext(&p, ext) => {
            let mut name = p.into_os_string();
            name.push(format!(".{ext}"));
            Outcome::Picked(PathBuf::from(name))
        }
        Some(p) => Outcome::Picked(p),
    }
}

/// Show the platform's own panel and wait for it (it is modal). The panel
/// asks before replacing an existing file.
#[cfg(target_os = "macos")]
pub fn native(mode: Mode, title: &str, ext: &str, start: &Path, name: &str) -> Outcome {
    let dialog = rfd::FileDialog::new()
        .set_title(title)
        .set_directory(start)
        .add_filter(format!(".{ext}"), &[ext]);
    let picked = match mode {
        Mode::Open => dialog.pick_file(),
        Mode::Save => dialog.set_file_name(name).save_file(),
    };
    native_outcome(mode, picked, ext)
}

/// No native panel here (see [`uses_native`]).
#[cfg(not(target_os = "macos"))]
pub fn native(_mode: Mode, _title: &str, _ext: &str, _start: &Path, _name: &str) -> Outcome {
    Outcome::Cancelled
}

/// An open dialog window. Keep it in the app state and call
/// [`FileDialog::show`] every frame until it returns an [`Outcome`].
pub struct FileDialog {
    mode: Mode,
    title: String,
    ext: String,
    home: PathBuf,
    cwd: PathBuf,
    loaded: Option<PathBuf>,
    listing: Listing,
    list_error: Option<String>,
    /// Open: the highlighted file. Save: the typed name.
    selected: Option<PathBuf>,
    filename: String,
    error: Option<String>,
    /// Save: waiting for "replace?" on this existing file.
    confirm_overwrite: Option<PathBuf>,
}

impl FileDialog {
    /// `ext` without the dot, e.g. `"toml"`. `filename` pre-fills the name
    /// field in save mode.
    pub fn new(
        mode: Mode,
        title: &str,
        ext: &str,
        start: PathBuf,
        home: PathBuf,
        filename: &str,
    ) -> FileDialog {
        FileDialog {
            mode,
            title: title.to_string(),
            ext: ext.to_string(),
            home,
            cwd: start,
            loaded: None,
            listing: Listing::default(),
            list_error: None,
            selected: None,
            filename: filename.to_string(),
            error: None,
            confirm_overwrite: None,
        }
    }

    /// The folder being shown, to start the next dialog in.
    pub fn dir(&self) -> &Path {
        &self.cwd
    }

    fn reload(&mut self) {
        match list_dir(&self.cwd, &self.ext) {
            Ok(l) => {
                self.listing = l;
                self.list_error = None;
            }
            Err(e) => {
                self.listing = Listing::default();
                self.list_error = Some(format!("Cannot read this folder: {e}"));
            }
        }
        self.loaded = Some(self.cwd.clone());
    }

    /// Accept the current choice, or explain why not.
    fn accept(&mut self) -> Option<Outcome> {
        self.error = None;
        match self.mode {
            Mode::Open => match &self.selected {
                Some(p) => Some(Outcome::Picked(p.clone())),
                None => {
                    self.error = Some(format!("Choose a .{} file.", self.ext));
                    None
                }
            },
            Mode::Save => match save_target(&self.cwd, &self.filename, &self.ext) {
                Ok(p) if p.is_dir() => {
                    self.error = Some(format!("{} is a folder.", p.display()));
                    None
                }
                Ok(p) if p.exists() => {
                    self.confirm_overwrite = Some(p);
                    None
                }
                Ok(p) => Some(Outcome::Picked(p)),
                Err(e) => {
                    self.error = Some(e);
                    None
                }
            },
        }
    }

    /// Draw the window. Returns the outcome once the user is done.
    pub fn show(&mut self, ctx: &eframe::egui::Context) -> Option<Outcome> {
        use eframe::egui;
        if self.loaded.as_ref() != Some(&self.cwd) {
            self.reload();
        }

        let mut open = true;
        let mut outcome = None;
        let mut nav: Option<PathBuf> = None;
        let mut accept = false;
        let action = match self.mode {
            Mode::Open => "Open",
            Mode::Save => "Save",
        };

        egui::Window::new(self.title.as_str())
            .id(egui::Id::new("cheatu_file_dialog"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([560.0, 440.0])
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                if let Some(target) = self.confirm_overwrite.clone() {
                    ui.label(format!("{} already exists.", target.display()));
                    ui.label("Replace it?");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Replace").clicked() {
                            outcome = Some(Outcome::Picked(target.clone()));
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_overwrite = None;
                        }
                    });
                    return;
                }

                ui.horizontal(|ui| {
                    if ui.button("⬆ Up").clicked() {
                        if let Some(p) = self.cwd.parent() {
                            nav = Some(p.to_path_buf());
                        }
                    }
                    if ui.button("🏠 Home").clicked() {
                        nav = Some(self.home.clone());
                    }
                    if ui.button("⟳ Refresh").clicked() {
                        self.loaded = None;
                    }
                });
                ui.label(egui::RichText::new(self.cwd.display().to_string()).monospace());
                ui.separator();

                let list_h = (ui.available_height() - 80.0).max(120.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(list_h)
                    .show(ui, |ui| {
                        if let Some(e) = &self.list_error {
                            ui.label(egui::RichText::new(e).color(ui.visuals().error_fg_color));
                        }
                        for (name, path) in &self.listing.dirs {
                            if ui.button(format!("📁 {name}")).clicked() {
                                nav = Some(path.clone());
                            }
                        }
                        for (name, path) in &self.listing.files {
                            let is_sel = match self.mode {
                                Mode::Open => self.selected.as_ref() == Some(path),
                                Mode::Save => self.filename.trim() == name,
                            };
                            let resp = ui.selectable_label(is_sel, format!("📄 {name}"));
                            if resp.clicked() {
                                self.selected = Some(path.clone());
                                self.filename = name.clone();
                                self.error = None;
                            }
                            if resp.double_clicked() {
                                accept = true;
                            }
                        }
                        if self.listing.dirs.is_empty()
                            && self.listing.files.is_empty()
                            && self.list_error.is_none()
                        {
                            ui.label(
                                egui::RichText::new(format!("No folders or .{} files.", self.ext))
                                    .weak(),
                            );
                        }
                    });

                ui.separator();
                if self.mode == Mode::Save {
                    ui.horizontal(|ui| {
                        ui.label("File name:");
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.filename)
                                .hint_text(format!("name.{}", self.ext))
                                .desired_width(280.0),
                        );
                        if resp.changed() {
                            self.error = None;
                        }
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            accept = true;
                        }
                    });
                }
                if let Some(e) = &self.error {
                    ui.label(egui::RichText::new(e).color(ui.visuals().error_fg_color));
                }
                ui.horizontal(|ui| {
                    if ui.button(action).clicked() {
                        accept = true;
                    }
                    if ui.button("Cancel").clicked() {
                        outcome = Some(Outcome::Cancelled);
                    }
                    ui.label(
                        egui::RichText::new(format!("Showing .{} files", self.ext))
                            .weak()
                            .small(),
                    );
                });
            });

        if let Some(p) = nav {
            self.cwd = p;
            self.selected = None;
            self.error = None;
        }
        if accept && outcome.is_none() {
            outcome = self.accept();
        }
        if !open && outcome.is_none() {
            outcome = Some(Outcome::Cancelled);
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_panels_on_macos_only() {
        assert_eq!(uses_native(), cfg!(target_os = "macos"));
    }

    #[test]
    fn native_save_gets_the_extension() {
        let o = native_outcome(Mode::Save, Some(PathBuf::from("/d/zelda")), "toml");
        assert_eq!(o, Outcome::Picked(PathBuf::from("/d/zelda.toml")));
        let o = native_outcome(Mode::Save, Some(PathBuf::from("/d/zelda.TOML")), "toml");
        assert_eq!(o, Outcome::Picked(PathBuf::from("/d/zelda.TOML")));
        let o = native_outcome(Mode::Open, Some(PathBuf::from("/d/zelda")), "toml");
        assert_eq!(
            o,
            Outcome::Picked(PathBuf::from("/d/zelda")),
            "open takes the file as is"
        );
        assert_eq!(native_outcome(Mode::Save, None, "toml"), Outcome::Cancelled);
    }

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("cheatu-fd-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn starts_in_last_dir_else_home() {
        let d = scratch("start");
        let home = scratch("home");
        assert_eq!(start_dir(Some(&d), &home), d);
        assert_eq!(start_dir(None, &home), home);
        assert_eq!(
            start_dir(Some(&d.join("gone")), &home),
            home,
            "a deleted last folder falls back to home"
        );
        assert_eq!(
            start_dir(None, &home.join("gone")),
            PathBuf::from("/"),
            "no home falls back to the root"
        );
        std::fs::remove_dir_all(&d).unwrap();
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn extension_match_ignores_case() {
        assert!(has_ext(Path::new("/a/b.toml"), "toml"));
        assert!(has_ext(Path::new("b.TOML"), "toml"));
        assert!(has_ext(Path::new("x.cheatu-ptr"), "cheatu-ptr"));
        assert!(!has_ext(Path::new("b.toml.bak"), "toml"));
        assert!(!has_ext(Path::new("toml"), "toml"));
    }

    #[test]
    fn listing_filters_by_extension_and_sorts() {
        let d = scratch("list");
        for f in ["b.toml", "A.TOML", "notes.txt", ".hidden.toml"] {
            std::fs::write(d.join(f), "").unwrap();
        }
        for sub in ["zeta", "Alpha", ".git"] {
            std::fs::create_dir(d.join(sub)).unwrap();
        }
        // A folder named like a table is still a folder.
        std::fs::create_dir(d.join("dir.toml")).unwrap();

        let l = list_dir(&d, "toml").unwrap();
        let names = |v: &[(String, PathBuf)]| v.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        assert_eq!(names(&l.dirs), ["Alpha", "dir.toml", "zeta"]);
        assert_eq!(names(&l.files), ["A.TOML", "b.toml"]);
        assert_eq!(l.files[1].1, d.join("b.toml"));
        std::fs::remove_dir_all(&d).unwrap();

        assert!(
            list_dir(&d, "toml").is_err(),
            "a missing folder is an error"
        );
    }

    #[test]
    fn save_target_appends_extension() {
        let d = Path::new("/tables");
        assert_eq!(
            save_target(d, "game", "toml").unwrap(),
            Path::new("/tables/game.toml")
        );
        assert_eq!(
            save_target(d, "  game.TOML ", "toml").unwrap(),
            Path::new("/tables/game.TOML")
        );
        assert_eq!(
            save_target(d, "v1.2", "toml").unwrap(),
            Path::new("/tables/v1.2.toml")
        );
    }

    #[test]
    fn save_target_rejects_bad_names() {
        let d = Path::new("/tables");
        for bad in ["", "   ", ".", "..", "a/b", "../x.toml", "a\0b"] {
            assert!(save_target(d, bad, "toml").is_err(), "{bad:?}");
        }
    }
}

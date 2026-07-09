//! cheatu-gui — a cross-platform (Wayland + X11) memory scanner UI.
//!
//! Built on egui/eframe (pure Rust, winit + glow), so it renders identically on
//! KDE, GNOME, and standalone window managers without pulling in GTK or Qt.

// Don't spawn a console window on the odd chance this is cross-built for Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cheatu_core::process::ProcInfo;
use cheatu_core::scan::{FirstScan, NextScan, Scanner, ANY_TYPES};
use cheatu_core::{human_bytes, list_processes, privilege, Mem, ScanType, ScanValue};
use cheatu_inject::{agent, detect_dir, find_rpgmaker_games, scan_dir, Engine, FoundGame};

/// How often the background freeze thread rewrites frozen values.
const FREEZE_INTERVAL: Duration = Duration::from_millis(40);

/// Shared state driving the background freeze thread. The UI updates it each
/// frame; the thread reads it and writes the values into the target — so
/// freezing keeps working even when the GUI window is hidden or not repainting.
#[derive(Default)]
struct FreezeShared {
    /// Target pid, or 0 when not attached.
    pid: i32,
    /// Addresses to hold, with the exact value to write.
    items: Vec<(u64, ScanValue)>,
}

/// Cap on how many candidate rows we render at once (the table is virtualized,
/// but building the snapshot for millions of hits would still be wasteful).
const MAX_DISPLAY: usize = 2000;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([980.0, 660.0])
            .with_min_inner_size([720.0, 460.0])
            .with_title("cheatu"),
        ..Default::default()
    };
    eframe::run_native(
        "cheatu",
        options,
        Box::new(|cc| {
            install_cjk_font(&cc.egui_ctx);
            configure_style(&cc.egui_ctx);
            Ok(Box::new(CheatuApp {
                pause_while_scanning: true,
                ..Default::default()
            }))
        }),
    )
}

/// Load a CJK-capable system font as a fallback so Japanese/Chinese/Korean text
/// (common in RPG Maker game data) renders instead of empty boxes. egui's
/// built-in font is Latin-only. Best-effort: if no CJK font is found, non-Latin
/// glyphs simply stay as tofu.
fn install_cjk_font(ctx: &eframe::egui::Context) {
    use eframe::egui::{FontData, FontDefinitions, FontFamily};

    let Some(path) = cjk_font_path() else {
        return;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };

    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("cjk".to_owned(), FontData::from_owned(bytes));
    // Append as a fallback so the default Latin look is preserved and CJK is
    // used only for glyphs the primary font lacks.
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("cjk".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// Slightly larger text and roomier spacing than egui's dense defaults, so the
/// UI reads more comfortably.
fn configure_style(ctx: &eframe::egui::Context) {
    use eframe::egui::{FontFamily, FontId, TextStyle};
    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(20.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(15.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(14.0, FontFamily::Monospace),
        ),
        (
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        ),
    ]
    .into();
    style.spacing.item_spacing = eframe::egui::vec2(8.0, 6.0);
    style.spacing.button_padding = eframe::egui::vec2(7.0, 3.0);
    ctx.set_style(style);
}

/// Find a CJK font: an explicit override, then fontconfig, then known paths.
fn cjk_font_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CHEATU_CJK_FONT").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    // Ask fontconfig for the best Japanese-covering sans font (covers the CJK
    // ideographs used by Chinese too).
    if let Ok(out) = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", "sans-serif:lang=ja"])
        .output()
    {
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !path.is_empty() && Path::new(&path).is_file() {
            return Some(PathBuf::from(path));
        }
    }
    // Fallback: common install locations across distros.
    [
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJKjp-Regular.otf",
        "/usr/share/fonts/adobe-source-han-sans/SourceHanSans-Regular.otc",
        "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// The next-scan comparison chosen in the UI.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
enum NextMode {
    #[default]
    Exact,
    NotEqual,
    Greater,
    Less,
    Increased,
    Decreased,
    Changed,
    Unchanged,
}

impl NextMode {
    const ALL: [NextMode; 8] = [
        NextMode::Exact,
        NextMode::NotEqual,
        NextMode::Greater,
        NextMode::Less,
        NextMode::Increased,
        NextMode::Decreased,
        NextMode::Changed,
        NextMode::Unchanged,
    ];

    fn label(self) -> &'static str {
        match self {
            NextMode::Exact => "equal to…",
            NextMode::NotEqual => "not equal to…",
            NextMode::Greater => "greater than…",
            NextMode::Less => "less than…",
            NextMode::Increased => "increased",
            NextMode::Decreased => "decreased",
            NextMode::Changed => "changed",
            NextMode::Unchanged => "unchanged",
        }
    }

    /// Whether this comparison reads the value text box.
    fn needs_value(self) -> bool {
        matches!(
            self,
            NextMode::Exact | NextMode::NotEqual | NextMode::Greater | NextMode::Less
        )
    }
}

/// Column used to sort the process picker.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
enum ProcSort {
    #[default]
    Memory,
    Pid,
    Name,
}

/// A snapshot row shown in the results table.
struct DisplayRow {
    addr: u64,
    prev: ScanValue,
}

/// One entry in the bottom "cheat table".
struct SavedEntry {
    desc: String,
    addr: u64,
    ty: ScanType,
    value_text: String,
    frozen: bool,
}

/// Result of a background scan, handed back to the UI thread.
struct ScanOutcome {
    scanner: Scanner,
    message: String,
}

/// Which scan the worker thread should run.
enum Job {
    First(FirstScan),
    Next(NextScan, Option<String>),
}

/// The value-type selection in the UI: a single width, or "Any" (unknown type).
#[derive(Copy, Clone, PartialEq, Eq)]
enum TypeSel {
    Any,
    One(ScanType),
}

impl Default for TypeSel {
    fn default() -> Self {
        TypeSel::One(ScanType::I32)
    }
}

impl TypeSel {
    fn label(self) -> String {
        match self {
            TypeSel::Any => "Any (unknown type)".to_string(),
            TypeSel::One(t) => t.label().to_string(),
        }
    }

    fn is_any(self) -> bool {
        matches!(self, TypeSel::Any)
    }

    /// The types a first scan should try for this selection.
    fn types(self) -> Vec<ScanType> {
        match self {
            TypeSel::Any => ANY_TYPES.to_vec(),
            TypeSel::One(t) => vec![t],
        }
    }
}

/// Which backend the UI is showing.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
enum AppMode {
    #[default]
    Scanner,
    RpgMaker,
}

/// Which database category the RPG Maker panel is showing.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
enum CatTab {
    #[default]
    Items,
    Weapons,
    Armors,
    Variables,
    Switches,
}

/// State and UI for the RPG Maker (JavaScript-injection) backend.
#[derive(Default)]
struct RpgState {
    dir: String,
    found: Vec<FoundGame>,
    scanned: bool,
    snapshot: Option<agent::Snapshot>,
    status: String,
    gold_text: String,
    gkey: String,
    gexpr: String,
    gval: String,

    // Built-in folder browser.
    browsing: bool,
    browse_cwd: PathBuf,
    browse_loaded: Option<PathBuf>,
    browse_dirs: Vec<(String, PathBuf, Engine)>,
    browse_found: Vec<FoundGame>,

    // Database browser (items/weapons/armors/variables/switches).
    catalog: Option<agent::Catalog>,
    /// Non-zero while awaiting a rebuilt catalog; holds the request time (ms) so
    /// we only accept a catalog the agent wrote *after* we asked.
    catalog_req_ts: u64,
    cat_tab: CatTab,
    cat_filter: String,
    give_amount: String,
    var_edits: HashMap<i64, String>,
}

/// Render a JSON value (variable/switch) as a plain string for editing/display.
fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Parse a variable edit string into a JSON number (int if possible).
fn parse_var(s: &str) -> Option<serde_json::Value> {
    let s = s.trim();
    if let Ok(i) = s.parse::<i64>() {
        Some(serde_json::Value::from(i))
    } else {
        s.parse::<f64>().ok().map(serde_json::Value::from)
    }
}

/// The user's home directory, for the browser's default location.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

impl RpgState {
    fn ui(&mut self, ui: &mut eframe::egui::Ui) {
        use eframe::egui;
        const GREEN: egui::Color32 = egui::Color32::from_rgb(120, 230, 140);
        const YELLOW: egui::Color32 = egui::Color32::from_rgb(240, 220, 120);

        ui.heading("RPG Maker mode — JavaScript injection");
        ui.label(
            egui::RichText::new(
                "Stable cheats for NW.js RPG Maker MV/MZ games. cheatu injects a small \
                 plugin and drives it through a file in the game folder — values change \
                 through the game's own code, so nothing corrupts.",
            )
            .weak()
            .small(),
        );
        ui.separator();

        // --- Game selection -------------------------------------------------
        ui.horizontal(|ui| {
            ui.label("Game folder:");
            ui.add(
                egui::TextEdit::singleline(&mut self.dir)
                    .hint_text("/path/to/the/game")
                    .desired_width(320.0),
            );
            if ui.button("📁 Browse…").clicked() {
                self.browsing = true;
                self.browse_cwd = PathBuf::from(self.dir.trim());
                if !self.browse_cwd.is_dir() {
                    self.browse_cwd = home_dir();
                }
                self.browse_loaded = None;
                self.browse_found.clear();
            }
            if ui.button("Scan Steam").clicked() {
                self.found = find_rpgmaker_games();
                self.scanned = true;
            }
        });
        self.browser_window(ui.ctx());
        if !self.found.is_empty() {
            egui::ComboBox::from_id_salt("rpg_game_pick")
                .selected_text("pick a detected game…")
                .width(420.0)
                .show_ui(ui, |ui| {
                    for g in &self.found {
                        if ui
                            .selectable_label(false, format!("{}  ·  {}", g.name, g.engine.label()))
                            .clicked()
                        {
                            self.dir = g.path.to_string_lossy().into_owned();
                        }
                    }
                });
        } else if self.scanned {
            ui.label(
                egui::RichText::new(
                    "No NW.js games found in the usual Steam folders — type the path above.",
                )
                .weak()
                .small(),
            );
        }

        if self.dir.trim().is_empty() {
            return;
        }
        // Resolve symlinks so a path like ~/.steam/... maps to the real folder
        // (and operations don't act on a duplicate view of the same game).
        let dir = std::fs::canonicalize(self.dir.trim())
            .unwrap_or_else(|_| Path::new(self.dir.trim()).to_path_buf());

        // --- Detect + install ----------------------------------------------
        let engine = detect_dir(&dir);
        let installed = agent::is_installed(&dir);
        let outdated = installed && agent::needs_update(&dir);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(format!("Detected: {}", engine.label()));
            ui.separator();
            if !installed {
                if ui
                    .add_enabled(engine.is_nwjs(), egui::Button::new("Install agent"))
                    .clicked()
                {
                    self.status = match agent::install(&dir) {
                        Ok(()) => "Agent installed. Restart the game to load it.".into(),
                        Err(e) => format!("Install failed: {e}"),
                    };
                }
            } else {
                if outdated {
                    ui.colored_label(YELLOW, "agent outdated — update available");
                    if ui.button("Update agent").clicked() {
                        self.status = match agent::install(&dir) {
                            Ok(()) => {
                                "Agent updated. Restart the game to load the new agent.".into()
                            }
                            Err(e) => format!("Update failed: {e}"),
                        };
                    }
                } else {
                    let short = agent::installed_hash(&dir).unwrap_or_default();
                    let short = &short[..8.min(short.len())];
                    ui.colored_label(GREEN, format!("agent installed ({short})"));
                }
                if ui.button("Uninstall").clicked() {
                    self.status = match agent::uninstall(&dir) {
                        Ok(()) => "Agent removed; index.html restored.".into(),
                        Err(e) => format!("Uninstall failed: {e}"),
                    };
                }
            }
        });
        if !engine.is_nwjs() {
            ui.colored_label(YELLOW, "This folder doesn't look like an NW.js game.");
        }
        if !self.status.is_empty() {
            ui.label(egui::RichText::new(&self.status).italics());
        }

        // --- Live snapshot (poll each frame) -------------------------------
        if installed {
            self.snapshot = agent::snapshot(&dir).ok();
        }
        ui.separator();

        // The agent stamps each snapshot with a timestamp; if it's old, the
        // game isn't running (the file is just the last state it left behind).
        let snap = self.snapshot.clone();
        let stale = snap
            .as_ref()
            .map(|s| s.ts == 0 || now_ms().saturating_sub(s.ts) > 3000)
            .unwrap_or(true);
        match snap {
            _ if stale => {
                ui.label(
                    egui::RichText::new(if installed {
                        "Waiting for the game… launch it and load a save."
                    } else {
                        "Install the agent, then launch the game."
                    })
                    .weak(),
                );
            }
            Some(snap) if !snap.rpgmaker => {
                if snap.stopped || snap.error.is_some() {
                    let msg = snap.error.as_deref().unwrap_or("exception");
                    ui.colored_label(egui::Color32::from_rgb(255, 96, 96), format!("⚠ {msg}"));
                    if ui
                        .button("Try recover")
                        .on_hover_text("Resume the game loop after a crash, avoiding a relaunch")
                        .clicked()
                    {
                        let _ = agent::recover(&dir);
                    }
                } else {
                    ui.label(
                        egui::RichText::new(
                            "Game is running, but no active save yet (title screen?).",
                        )
                        .weak(),
                    );
                }
            }
            Some(snap) => self.live_panel(ui, &dir, &snap),
            None => {}
        }
    }

    /// Re-list the current browse directory (folders, marking which are games).
    fn reload_browse(&mut self) {
        self.browse_dirs.clear();
        if let Ok(rd) = std::fs::read_dir(&self.browse_cwd) {
            let mut dirs: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            dirs.sort();
            for p in dirs {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let engine = detect_dir(&p);
                self.browse_dirs.push((name, p, engine));
            }
        }
        self.browse_loaded = Some(self.browse_cwd.clone());
    }

    /// A dependency-free folder browser: navigate the filesystem, see which
    /// folders are RPG Maker games, and pick one (or scan a whole library dir).
    fn browser_window(&mut self, ctx: &eframe::egui::Context) {
        use eframe::egui;
        if !self.browsing {
            return;
        }
        if self.browse_loaded.as_ref() != Some(&self.browse_cwd) {
            self.reload_browse();
        }

        let mut open = true;
        let mut close = false;
        let mut nav: Option<PathBuf> = None;
        let mut pick: Option<String> = None;
        let cwd_engine = detect_dir(&self.browse_cwd);

        egui::Window::new("Choose game folder")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([580.0, 480.0])
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("⬆ Up").clicked() {
                        if let Some(p) = self.browse_cwd.parent() {
                            nav = Some(p.to_path_buf());
                        }
                    }
                    if ui.button("🏠 Home").clicked() {
                        nav = Some(home_dir());
                    }
                    if ui.button("🔎 Scan this folder for games").clicked() {
                        self.browse_found = scan_dir(&self.browse_cwd);
                    }
                });
                ui.label(egui::RichText::new(self.browse_cwd.display().to_string()).monospace());
                if cwd_engine.is_nwjs()
                    && ui
                        .button(format!("✓ Use this folder — {}", cwd_engine.label()))
                        .clicked()
                {
                    pick = Some(self.browse_cwd.display().to_string());
                }
                ui.separator();

                if !self.browse_found.is_empty() {
                    ui.label(egui::RichText::new("Games found under this folder:").strong());
                    for g in &self.browse_found {
                        if ui
                            .selectable_label(
                                false,
                                format!("🎮 {}  ·  {}", g.name, g.engine.label()),
                            )
                            .clicked()
                        {
                            pick = Some(g.path.display().to_string());
                        }
                    }
                    ui.separator();
                }

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(280.0)
                    .show(ui, |ui| {
                        for (name, path, engine) in &self.browse_dirs {
                            if engine.is_nwjs() {
                                ui.horizontal(|ui| {
                                    if ui.button(format!("🎮 {name}")).clicked() {
                                        pick = Some(path.display().to_string());
                                    }
                                    ui.label(
                                        egui::RichText::new(engine.label())
                                            .weak()
                                            .small()
                                            .color(egui::Color32::from_rgb(120, 230, 140)),
                                    );
                                });
                            } else if ui.button(format!("📁 {name}")).clicked() {
                                nav = Some(path.clone());
                            }
                        }
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Use this folder").clicked() {
                        pick = Some(self.browse_cwd.display().to_string());
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });

        if let Some(p) = nav {
            self.browse_cwd = p;
            self.browse_found.clear();
        }
        if let Some(sel) = pick {
            self.dir = sel;
            self.browsing = false;
        }
        if !open || close {
            self.browsing = false;
        }
    }

    /// The controls shown once the agent reports an active game.
    fn live_panel(&mut self, ui: &mut eframe::egui::Ui, dir: &Path, snap: &agent::Snapshot) {
        use eframe::egui;
        let is_frozen = |key: &str| snap.freezes.iter().any(|f| f == key);

        ui.horizontal(|ui| {
            ui.strong(format!("RPG Maker {}", snap.engine));
            if !snap.title.is_empty() {
                ui.label(egui::RichText::new(&snap.title).weak());
            }
        });

        // The running agent may differ from the on-disk plugin until the game
        // is restarted; some features won't work until it reloads.
        if snap.agent_hash.as_deref() != Some(agent::agent_hash().as_str()) {
            ui.colored_label(
                egui::Color32::from_rgb(240, 220, 120),
                "Running agent differs from the installed one — restart the game to load it.",
            );
        }

        // Gold.
        ui.horizontal(|ui| {
            ui.label(format!("Gold: {}", snap.gold.unwrap_or(0)));
            ui.add(
                egui::TextEdit::singleline(&mut self.gold_text)
                    .hint_text("amount")
                    .desired_width(90.0),
            );
            if ui.button("Set").clicked() {
                if let Ok(n) = self.gold_text.trim().parse::<i64>() {
                    let _ = agent::set_expr(dir, "$gameParty._gold", n.into());
                }
            }
            let mut frozen = is_frozen("gold");
            if ui.checkbox(&mut frozen, "Freeze").changed() {
                if frozen {
                    let n = self
                        .gold_text
                        .trim()
                        .parse::<i64>()
                        .unwrap_or_else(|_| snap.gold.unwrap_or(0));
                    let _ = agent::freeze_value(dir, "gold", "$gameParty._gold", n.into());
                } else {
                    let _ = agent::unfreeze(dir, "gold");
                }
            }
        });

        // Whole-party "keep maxed" toggles.
        ui.horizontal(|ui| {
            ui.label("Keep party maxed:");
            for stat in ["hp", "mp", "tp"] {
                let key = agent::max_stat_key(stat, true);
                let mut on = is_frozen(&key);
                if ui.checkbox(&mut on, stat.to_uppercase()).changed() {
                    if on {
                        let _ = agent::freeze_stat_max(dir, stat, true);
                    } else {
                        let _ = agent::unfreeze(dir, &key);
                    }
                }
            }
        });

        // Party list.
        ui.add_space(4.0);
        egui::Grid::new("rpg_party")
            .striped(true)
            .num_columns(5)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                ui.strong("Member");
                ui.strong("Lv");
                ui.strong("HP");
                ui.strong("MP");
                ui.strong("TP");
                ui.end_row();
                for m in &snap.party {
                    ui.label(&m.name);
                    ui.label(m.level.to_string());
                    ui.label(format!("{}/{}", m.hp, m.mhp));
                    ui.label(format!("{}/{}", m.mp, m.mmp));
                    ui.label(m.tp.to_string());
                    ui.end_row();
                }
            });

        // --- Actions -------------------------------------------------------
        // Battle/recovery controls that act *on* the game rather than editing a
        // value, grouped apart from the party stats above.
        ui.add_space(6.0);
        ui.separator();
        ui.strong("Actions");

        // Recovery is always reachable — the remedy whenever the game wedges,
        // whether or not the agent has managed to report the stall yet.
        ui.horizontal(|ui| {
            if ui
                .button("Try recover")
                .on_hover_text(
                    "Resume the game loop after a crash/exception, avoiding a relaunch. \
                     Safe to click any time.",
                )
                .clicked()
            {
                let _ = agent::recover(dir);
            }
            if snap.stopped || snap.error.is_some() {
                let msg = snap.error.as_deref().unwrap_or("game appears frozen");
                ui.colored_label(egui::Color32::from_rgb(255, 96, 96), format!("⚠ {msg}"));
            } else {
                ui.label(
                    egui::RichText::new("(use if the game freezes or throws an error)")
                        .weak()
                        .small(),
                );
            }
        });

        // Force win/lose: powerful but risky on heavily-modified battle
        // systems, so tuck them behind an opt-in disclosure with a warning.
        egui::CollapsingHeader::new("⚠ Force battle result (advanced)")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(
                        "Uses the engine's generic victory/defeat. Games with custom or \
                         multi-wave battle systems can break — e.g. don't force-win the \
                         final wave; win that one normally.",
                    )
                    .weak()
                    .small(),
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(snap.in_battle, egui::Button::new("Force win"))
                        .clicked()
                    {
                        let _ = agent::battle_win(dir);
                    }
                    if ui
                        .add_enabled(snap.in_battle, egui::Button::new("Force lose"))
                        .clicked()
                    {
                        let _ = agent::battle_lose(dir);
                    }
                    if !snap.in_battle {
                        ui.label(egui::RichText::new("(only during a battle)").weak().small());
                    }
                });
            });

        // Custom value (escape hatch) — one compact line, above the big list.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Custom:").weak());
            ui.add(
                egui::TextEdit::singleline(&mut self.gexpr)
                    .hint_text("$gameVariables._data[12]")
                    .desired_width(220.0),
            );
            ui.label("=");
            ui.add(
                egui::TextEdit::singleline(&mut self.gval)
                    .hint_text("value")
                    .desired_width(70.0),
            );
            let expr = self.gexpr.trim().to_string();
            let val = parse_js_value(self.gval.trim());
            if ui
                .add_enabled(!expr.is_empty(), egui::Button::new("Set"))
                .clicked()
            {
                let _ = agent::set_expr(dir, &expr, val.clone());
            }
            let key = if self.gkey.trim().is_empty() {
                expr.clone()
            } else {
                self.gkey.trim().to_string()
            };
            let mut frozen = is_frozen(&key);
            if ui
                .add_enabled(!expr.is_empty(), egui::Checkbox::new(&mut frozen, "Freeze"))
                .changed()
            {
                if frozen {
                    let _ = agent::freeze_value(dir, &key, &expr, val);
                } else {
                    let _ = agent::unfreeze(dir, &key);
                }
            }
        });

        if !snap.freezes.is_empty() {
            ui.label(
                egui::RichText::new(format!("Active freezes: {}", snap.freezes.join(", ")))
                    .weak()
                    .small(),
            );
        }

        // Database last, so its list fills the rest of the window height.
        ui.separator();
        self.database_ui(ui, dir);
    }

    /// Browsable, editable view of the game's database: items/weapons/armors
    /// (give any amount), variables (set), and switches (toggle). Discovered
    /// generically from the running game — works on any MV/MZ title. Auto-loads
    /// and fills the remaining window height.
    fn database_ui(&mut self, ui: &mut eframe::egui::Ui, dir: &Path) {
        use eframe::egui;

        // Accept a catalog only once the agent has written a fresh one (ts
        // at/after our request), so counts reflect recent edits.
        if self.catalog_req_ts > 0 {
            let fresh = agent::catalog_ts(dir).is_some_and(|ts| ts >= self.catalog_req_ts);
            if fresh {
                if let Some(c) = agent::read_catalog(dir) {
                    for v in &c.variables {
                        self.var_edits
                            .entry(v.id)
                            .or_insert_with(|| value_to_string(&v.value));
                    }
                    self.catalog = Some(c);
                }
                self.catalog_req_ts = 0;
            } else if now_ms().saturating_sub(self.catalog_req_ts) > 4000 {
                self.catalog_req_ts = 0; // game not responding; stop waiting
            }
        }
        // Auto-load the first time — no manual button press needed.
        if self.catalog.is_none() && self.catalog_req_ts == 0 {
            let _ = agent::request_catalog(dir);
            self.catalog_req_ts = now_ms();
        }

        ui.horizontal(|ui| {
            ui.strong("Database");
            if ui.button("↻ Refresh").clicked() {
                let _ = agent::request_catalog(dir);
                self.catalog_req_ts = now_ms();
            }
            if self.catalog_req_ts > 0 {
                ui.spinner();
            }
        });

        let Some(counts) = self.catalog.as_ref().map(|c| {
            (
                c.items.len(),
                c.weapons.len(),
                c.armors.len(),
                c.variables.len(),
                c.switches.len(),
            )
        }) else {
            ui.label(egui::RichText::new("Discovering the game's database…").weak());
            return;
        };

        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.cat_tab,
                CatTab::Items,
                format!("Items ({})", counts.0),
            );
            ui.selectable_value(
                &mut self.cat_tab,
                CatTab::Weapons,
                format!("Weapons ({})", counts.1),
            );
            ui.selectable_value(
                &mut self.cat_tab,
                CatTab::Armors,
                format!("Armors ({})", counts.2),
            );
            ui.selectable_value(
                &mut self.cat_tab,
                CatTab::Variables,
                format!("Variables ({})", counts.3),
            );
            ui.selectable_value(
                &mut self.cat_tab,
                CatTab::Switches,
                format!("Switches ({})", counts.4),
            );
        });

        let item_tab = matches!(
            self.cat_tab,
            CatTab::Items | CatTab::Weapons | CatTab::Armors
        );
        ui.horizontal(|ui| {
            ui.label("Filter:");
            ui.add(egui::TextEdit::singleline(&mut self.cat_filter).desired_width(180.0));
            if item_tab {
                ui.label("Bulk +N:");
                ui.add(egui::TextEdit::singleline(&mut self.give_amount).desired_width(50.0));
                ui.label(
                    egui::RichText::new("(− / + step by 1; some items cap at 1)")
                        .weak()
                        .small(),
                );
            }
        });
        let filter = self.cat_filter.to_ascii_lowercase();
        let name_matches =
            |name: &str| filter.is_empty() || name.to_ascii_lowercase().contains(&filter);

        // No max_height: fills the rest of the window.
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match self.cat_tab {
                CatTab::Items | CatTab::Weapons | CatTab::Armors => {
                    let (kind, list) = match self.cat_tab {
                        CatTab::Weapons => (
                            agent::ItemKind::Weapon,
                            self.catalog.as_ref().unwrap().weapons.clone(),
                        ),
                        CatTab::Armors => (
                            agent::ItemKind::Armor,
                            self.catalog.as_ref().unwrap().armors.clone(),
                        ),
                        _ => (
                            agent::ItemKind::Item,
                            self.catalog.as_ref().unwrap().items.clone(),
                        ),
                    };
                    let bulk: i64 = self.give_amount.trim().parse().unwrap_or(10);
                    let mut delta: Option<(i64, i64)> = None; // (item id, amount)
                    for e in &list {
                        if !name_matches(&e.name) {
                            continue;
                        }
                        ui.horizontal(|ui| {
                            if ui.small_button("−").on_hover_text("remove one").clicked() {
                                delta = Some((e.id, -1));
                            }
                            if ui.small_button("＋").on_hover_text("add one").clicked() {
                                delta = Some((e.id, 1));
                            }
                            if ui.button(format!("+{bulk}")).clicked() {
                                delta = Some((e.id, bulk));
                            }
                            ui.label(&e.name);
                            ui.label(egui::RichText::new(format!("×{}", e.count)).weak());
                        });
                    }
                    // Apply after the loop, then re-request the catalog so
                    // the counts (and any engine clamping) refresh.
                    if let Some((id, amount)) = delta {
                        let _ = agent::gain_item(dir, kind, id, amount);
                        let _ = agent::request_catalog(dir);
                        self.catalog_req_ts = now_ms();
                    }
                }
                CatTab::Variables => {
                    let list = self.catalog.as_ref().unwrap().variables.clone();
                    for e in &list {
                        if !name_matches(&e.name) {
                            continue;
                        }
                        ui.horizontal(|ui| {
                            let buf = self
                                .var_edits
                                .entry(e.id)
                                .or_insert_with(|| value_to_string(&e.value));
                            ui.add(egui::TextEdit::singleline(buf).desired_width(80.0));
                            if ui.button("Set").clicked() {
                                if let Some(v) = parse_var(buf) {
                                    let _ = agent::set_variable(dir, e.id, v);
                                }
                            }
                            ui.label(format!("[{}] {}", e.id, e.name));
                        });
                    }
                }
                CatTab::Switches => {
                    let list = self.catalog.as_ref().unwrap().switches.clone();
                    for e in &list {
                        if !name_matches(&e.name) {
                            continue;
                        }
                        let mut on = e.value.as_bool().unwrap_or(false);
                        if ui
                            .checkbox(&mut on, format!("[{}] {}", e.id, e.name))
                            .changed()
                        {
                            let _ = agent::set_switch(dir, e.id, on);
                            if let Some(c) = self.catalog.as_mut() {
                                if let Some(sw) = c.switches.iter_mut().find(|s| s.id == e.id) {
                                    sw.value = serde_json::Value::Bool(on);
                                }
                            }
                        }
                    }
                }
            });
    }
}

/// Parse a value string as JSON (number/bool/etc.), falling back to a string.
fn parse_js_value(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|_| serde_json::Value::String(s.to_string()))
}

/// Current wall-clock time in milliseconds (matches the agent's `Date.now()`).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default)]
struct CheatuApp {
    mode: AppMode,
    rpg: RpgState,

    scanner: Option<Scanner>,
    attached: Option<(i32, String)>,

    // Process picker.
    show_picker: bool,
    proc_filter: String,
    procs: Vec<ProcInfo>,
    picker_sort: ProcSort,
    picker_wine_only: bool,
    picker_selected: Option<i32>,

    // Scan controls.
    type_sel: TypeSel,
    value_text: String,
    unknown_initial: bool,
    next_mode: NextMode,
    /// SIGSTOP the target during a scan so values hold still (default on).
    pause_while_scanning: bool,

    // Results.
    display: Vec<DisplayRow>,
    result_count: usize,

    // Background scan.
    pending: Option<Receiver<ScanOutcome>>,
    scanning: bool,
    status: String,

    // Cheat table.
    saved: Vec<SavedEntry>,

    // Background freeze.
    freeze_state: Arc<Mutex<FreezeShared>>,
    freeze_started: bool,
}

impl CheatuApp {
    fn attach(&mut self, pid: i32) {
        match Scanner::new(pid) {
            Ok(s) => {
                let name = self
                    .procs
                    .iter()
                    .find(|p| p.pid == pid)
                    .map(|p| p.name.clone())
                    .or_else(|| {
                        list_processes()
                            .into_iter()
                            .find(|p| p.pid == pid)
                            .map(|p| p.name)
                    })
                    .unwrap_or_else(|| "?".into());
                self.attached = Some((pid, name));
                self.scanner = Some(s);
                self.display.clear();
                self.result_count = 0;
                self.status = format!("Attached to pid {pid}.");
            }
            Err(e) => {
                let mut msg = format!("Failed to attach to pid {pid}: {e}.");
                if e.kind() == std::io::ErrorKind::PermissionDenied && !privilege::is_root() {
                    msg.push_str(" Try \"Request root access\".");
                }
                self.status = msg;
            }
        }
    }

    fn start_scan(&mut self, first: bool) {
        let Some(mut scanner) = self.scanner.take() else {
            self.status = "Attach to a process first.".into();
            return;
        };

        // A number is required for a value search; comparisons like "changed"
        // are not. We validate loosely (parses as a number) and let the engine
        // decide which candidate types can actually represent it.
        let value_is_number = self.value_text.trim().parse::<f64>().is_ok();

        let job = if first {
            if self.unknown_initial {
                match self.type_sel {
                    TypeSel::One(t) => Job::First(FirstScan::Unknown(t)),
                    TypeSel::Any => {
                        self.status =
                            "Unknown initial value needs a specific type, not “Any”.".into();
                        self.scanner = Some(scanner);
                        return;
                    }
                }
            } else if value_is_number {
                Job::First(FirstScan::Value {
                    value: self.value_text.trim().to_string(),
                    types: self.type_sel.types(),
                })
            } else {
                self.status = "Enter a value to scan for.".into();
                self.scanner = Some(scanner);
                return;
            }
        } else {
            if !scanner.has_scanned() {
                self.status = "Do a first scan before narrowing.".into();
                self.scanner = Some(scanner);
                return;
            }
            let mode = self.next_mode;
            if mode.needs_value() && !value_is_number {
                self.status = "Enter a value for this comparison.".into();
                self.scanner = Some(scanner);
                return;
            }
            let cmp = match mode {
                NextMode::Exact => NextScan::Eq,
                NextMode::NotEqual => NextScan::Ne,
                NextMode::Greater => NextScan::Gt,
                NextMode::Less => NextScan::Lt,
                NextMode::Increased => NextScan::Increased,
                NextMode::Decreased => NextScan::Decreased,
                NextMode::Changed => NextScan::Changed,
                NextMode::Unchanged => NextScan::Unchanged,
            };
            let operand = mode
                .needs_value()
                .then(|| self.value_text.trim().to_string());
            Job::Next(cmp, operand)
        };

        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        self.scanning = true;
        self.status = "Scanning…".into();
        let pause = self.pause_while_scanning;

        std::thread::spawn(move || {
            let pid = scanner.pid();
            // Suspend the target so values hold still and reads don't race the
            // game. Only resume if we actually managed to suspend it.
            let paused = pause && cheatu_core::suspend(pid).is_ok();
            let res = match job {
                Job::First(fs) => {
                    scanner.reset();
                    scanner.first_scan(fs)
                }
                Job::Next(cmp, operand) => scanner.next_scan(cmp, operand.as_deref()),
            };
            if paused {
                let _ = cheatu_core::resume(pid);
            }
            let message = match res {
                Ok(()) => format!("{} results.", scanner.count()),
                Err(e) => format!("Scan error: {e}"),
            };
            let _ = tx.send(ScanOutcome { scanner, message });
        });
    }

    fn rebuild_display(&mut self) {
        self.display.clear();
        if let Some(scanner) = &self.scanner {
            self.result_count = scanner.count();
            for cand in scanner.results().iter().take(MAX_DISPLAY) {
                self.display.push(DisplayRow {
                    addr: cand.addr,
                    prev: cand.prev,
                });
            }
        }
    }

    fn reset_scan(&mut self) {
        if let Some(s) = &mut self.scanner {
            s.reset();
        }
        self.display.clear();
        self.result_count = 0;
        self.status = "Candidate list cleared.".into();
    }

    fn poll_scan(&mut self) {
        let done = self.pending.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(outcome) = done {
            self.status = outcome.message;
            self.scanner = Some(outcome.scanner);
            self.scanning = false;
            self.pending = None;
            self.rebuild_display();
        }
    }

    /// Start the background freeze thread once. It writes frozen values on its
    /// own clock, independent of UI repaints, so freezing keeps working even
    /// when the game covers the cheatu window.
    fn ensure_freeze_thread(&mut self) {
        if self.freeze_started {
            return;
        }
        self.freeze_started = true;
        let state = Arc::clone(&self.freeze_state);
        std::thread::spawn(move || {
            let mut mem: Option<Mem> = None;
            let mut cur_pid = 0i32;
            loop {
                std::thread::sleep(FREEZE_INTERVAL);
                let (pid, items) = {
                    let s = state.lock().unwrap();
                    (s.pid, s.items.clone())
                };
                if pid == 0 {
                    mem = None;
                    cur_pid = 0;
                    continue;
                }
                if pid != cur_pid {
                    mem = Mem::open(pid).ok();
                    cur_pid = pid;
                }
                if let Some(m) = &mem {
                    for (addr, value) in &items {
                        let _ = m.write_at(*addr, &value.to_ne_bytes());
                    }
                }
            }
        });
    }

    /// Publish the current set of frozen values to the background thread.
    fn sync_freezes(&mut self) {
        let pid = self.attached.as_ref().map(|(p, _)| *p).unwrap_or(0);
        let items: Vec<(u64, ScanValue)> = self
            .saved
            .iter()
            .filter(|e| e.frozen)
            .filter_map(|e| e.ty.parse(&e.value_text).map(|v| (e.addr, v)))
            .collect();
        if let Ok(mut s) = self.freeze_state.lock() {
            s.pid = pid;
            s.items = items;
        }
    }
}

impl eframe::App for CheatuApp {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        use eframe::egui;

        self.ensure_freeze_thread();
        self.poll_scan();
        self.sync_freezes();

        // Deferred actions, so UI closures never need to call &mut self methods.
        let mut do_attach: Option<i32> = None;
        let mut do_first = false;
        let mut do_next = false;
        let mut do_reset = false;
        let mut do_elevate = false;
        let mut do_refresh_procs = false;
        let mut add_to_table: Vec<(u64, ScanType)> = Vec::new();
        let mut remove_from_table: Vec<usize> = Vec::new();

        // ---- Header -----------------------------------------------------
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("cheatu");
                ui.separator();
                ui.selectable_value(&mut self.mode, AppMode::Scanner, "🔍 Memory scanner");
                ui.selectable_value(&mut self.mode, AppMode::RpgMaker, "🎮 RPG Maker (JS)");
                ui.separator();
                if self.mode == AppMode::Scanner {
                    match &self.attached {
                        Some((pid, name)) => {
                            ui.label(format!("Attached: {name} (pid {pid})"));
                        }
                        None => {
                            ui.label(egui::RichText::new("No process attached").weak());
                        }
                    }
                    if ui.button("Select process…").clicked() {
                        do_refresh_procs = true;
                        self.show_picker = true;
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if privilege::is_root() {
                        ui.label(egui::RichText::new("root ✓").color(egui::Color32::GREEN));
                    } else {
                        if ui.button("Request root access").clicked() {
                            do_elevate = true;
                        }
                        ui.label(
                            egui::RichText::new("limited privileges").color(egui::Color32::YELLOW),
                        );
                    }
                });
            });
            ui.add_space(4.0);
        });

        // ---- Status bar -------------------------------------------------
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.scanning {
                    ui.spinner();
                }
                ui.label(&self.status);
            });
        });

        // ---- Cheat table (right, scanner mode only) --------------------
        if self.mode == AppMode::Scanner {
            egui::SidePanel::right("cheat_table")
                .resizable(true)
                .default_width(460.0)
                .show(ctx, |ui| {
                    ui.heading("Cheat table");
                    ui.label(
                        egui::RichText::new(
                            "One row per address. Tick ❄ to freeze; edit Value to set it.",
                        )
                        .weak()
                        .small(),
                    );
                    ui.separator();

                    if self.saved.is_empty() {
                        ui.label(egui::RichText::new("No saved addresses yet.").weak());
                        ui.label(
                            egui::RichText::new("Use the “+” button on a scan result.")
                                .weak()
                                .small(),
                        );
                    }

                    egui::ScrollArea::both()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (i, entry) in self.saved.iter_mut().enumerate() {
                                // One entry = one line, Cheat Engine style:
                                //   [❄] description  0xADDR  ty  [value]  [🗑]
                                // The live current value is deliberately omitted: it
                                // changed constantly, shifting the row and making the
                                // delete button hard to hit.
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut entry.frozen, "").on_hover_text("Freeze");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut entry.desc)
                                            .hint_text("description")
                                            .desired_width(96.0),
                                    );
                                    ui.monospace(format!("0x{:012x}", entry.addr));
                                    ui.label(egui::RichText::new(entry.ty.label()).weak().small());
                                    ui.add(
                                        egui::TextEdit::singleline(&mut entry.value_text)
                                            .desired_width(64.0),
                                    );
                                    if ui.small_button("🗑").on_hover_text("remove").clicked() {
                                        remove_from_table.push(i);
                                    }
                                });
                            }
                        });
                });
        }

        // ---- Central panel: scanner controls, or RPG Maker mode --------
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.mode == AppMode::RpgMaker {
                self.rpg.ui(ui);
                return;
            }
            let attached = self.scanner.is_some();
            let busy = self.scanning;

            ui.add_enabled_ui(attached && !busy, |ui| {
                egui::Grid::new("scan_controls")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Value type");
                        egui::ComboBox::from_id_salt("scan_type")
                            .selected_text(self.type_sel.label())
                            .width(180.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.type_sel,
                                    TypeSel::Any,
                                    "Any (unknown type)",
                                )
                                .on_hover_text(
                                    "Try the value as i32/u32/i64/u64/f32/f64 at once — \
                                     use this when you don't know the type.",
                                );
                                ui.separator();
                                for t in ScanType::ALL {
                                    ui.selectable_value(
                                        &mut self.type_sel,
                                        TypeSel::One(t),
                                        t.label(),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("Value");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.value_text)
                                    .hint_text("the number you see in-game, e.g. 100")
                                    .desired_width(200.0),
                            );
                            // Unknown-initial-value needs a concrete width.
                            ui.add_enabled(
                                !self.type_sel.is_any(),
                                egui::Checkbox::new(
                                    &mut self.unknown_initial,
                                    "Unknown initial value",
                                ),
                            )
                            .on_disabled_hover_text(
                                "Pick a specific type to scan for an unknown value.",
                            );
                        });
                        ui.end_row();

                        ui.label("Next scan");
                        egui::ComboBox::from_id_salt("next_mode")
                            .selected_text(self.next_mode.label())
                            .show_ui(ui, |ui| {
                                for m in NextMode::ALL {
                                    ui.selectable_value(&mut self.next_mode, m, m.label());
                                }
                            });
                        ui.end_row();
                    });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new("🔍 First scan"))
                        .on_hover_text("Scan the whole process for this value")
                        .clicked()
                    {
                        do_first = true;
                    }
                    let can_next = self.scanner.as_ref().is_some_and(|s| s.has_scanned());
                    if ui
                        .add_enabled(can_next, egui::Button::new("Next scan"))
                        .on_hover_text("Narrow the current results")
                        .clicked()
                    {
                        do_next = true;
                    }
                    if ui
                        .button("New scan")
                        .on_hover_text("Clear results")
                        .clicked()
                    {
                        do_reset = true;
                    }
                });

                ui.add_space(4.0);
                ui.checkbox(
                    &mut self.pause_while_scanning,
                    "Pause target while scanning",
                )
                .on_hover_text(
                    "Suspend the process (SIGSTOP) during the scan so values hold \
                     still and reads are consistent. Resumes right after.",
                );
            });

            if !attached {
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Attach to a process to begin scanning.").weak());
            }

            ui.separator();
            ui.horizontal(|ui| {
                ui.strong(format!("{} results", self.result_count));
                if self.result_count > self.display.len() {
                    ui.label(
                        egui::RichText::new(format!("(showing first {})", self.display.len()))
                            .weak()
                            .small(),
                    );
                }
            });

            // Results table (virtualized). Each row carries its own type, so
            // "Any" scans show a mix of i32/f32/… hits with the right decoding.
            use egui_extras::{Column, TableBuilder};
            let scanner_ref = self.scanner.as_ref();
            TableBuilder::new(ui)
                .striped(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::auto().at_least(150.0))
                .column(Column::auto().at_least(48.0))
                .column(Column::auto().at_least(90.0))
                .column(Column::auto().at_least(90.0))
                .column(Column::remainder())
                .header(20.0, |mut header| {
                    header.col(|ui| {
                        ui.strong("Address");
                    });
                    header.col(|ui| {
                        ui.strong("Type");
                    });
                    header.col(|ui| {
                        ui.strong("Previous");
                    });
                    header.col(|ui| {
                        ui.strong("Current");
                    });
                    header.col(|ui| {
                        ui.strong("");
                    });
                })
                .body(|body| {
                    body.rows(20.0, self.display.len(), |mut row| {
                        let idx = row.index();
                        let d = &self.display[idx];
                        let ty = d.prev.ty();
                        row.col(|ui| {
                            ui.monospace(format!("0x{:012x}", d.addr));
                        });
                        row.col(|ui| {
                            ui.label(egui::RichText::new(ty.label()).weak());
                        });
                        row.col(|ui| {
                            ui.monospace(d.prev.to_string());
                        });
                        row.col(|ui| {
                            match scanner_ref.and_then(|s| s.read_typed(d.addr, ty).ok()) {
                                Some(cur) => {
                                    // Red when the live value differs from what
                                    // it was at the last scan — makes the value
                                    // you just changed jump out.
                                    let mut text = egui::RichText::new(cur.to_string()).monospace();
                                    if !cur.approx_eq(&d.prev) {
                                        text = text.color(egui::Color32::from_rgb(255, 96, 96));
                                    }
                                    ui.label(text);
                                }
                                None => {
                                    ui.monospace("n/a");
                                }
                            }
                        });
                        row.col(|ui| {
                            if ui
                                .button("＋")
                                .on_hover_text("Add to cheat table")
                                .clicked()
                            {
                                add_to_table.push((d.addr, ty));
                            }
                        });
                    });
                });
        });

        // ---- Process picker window -------------------------------------
        if self.show_picker {
            let mut open = true;
            let mut close = false;
            let mut selected = self.picker_selected;
            let sort = self.picker_sort;
            let mut new_sort = sort;

            // Constrain to the viewport and center it, so the action bar at the
            // bottom is always on-screen no matter how long the list is.
            let screen = ctx.screen_rect();
            let max_h = (screen.height() - 48.0).max(260.0);
            egui::Window::new("Select a process")
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .default_size([700.0, 520.0])
                .max_height(max_h)
                .min_width(540.0)
                .show(ctx, |ui| {
                    // Search / filter controls.
                    ui.horizontal(|ui| {
                        ui.label("🔎");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.proc_filter)
                                .hint_text("filter by name or command")
                                .desired_width(240.0),
                        );
                        ui.checkbox(&mut self.picker_wine_only, "Wine/Proton only");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("⟳ Refresh").clicked() {
                                do_refresh_procs = true;
                            }
                        });
                    });
                    ui.label(
                        egui::RichText::new(
                            "Tip: sort by Memory and tick “Wine/Proton only”. For a Chromium / \
                             NW.js game (RPG Maker MV/MZ), attach to the largest process whose \
                             Role is “renderer”.",
                        )
                        .weak()
                        .small(),
                    );
                    ui.separator();

                    // Build the filtered, sorted view.
                    let filter = self.proc_filter.to_ascii_lowercase();
                    let wine_only = self.picker_wine_only;
                    let mut rows: Vec<&ProcInfo> = self
                        .procs
                        .iter()
                        .filter(|p| {
                            (!wine_only || p.is_wine)
                                && (filter.is_empty()
                                    || p.name.to_ascii_lowercase().contains(&filter)
                                    || p.cmdline.to_ascii_lowercase().contains(&filter)
                                    || p.role.as_deref().is_some_and(|r| r.contains(&filter)))
                        })
                        .collect();
                    match sort {
                        ProcSort::Memory => rows.sort_by_key(|p| std::cmp::Reverse(p.rss_bytes)),
                        ProcSort::Pid => rows.sort_by_key(|p| p.pid),
                        ProcSort::Name => rows.sort_by(|a, b| {
                            a.name
                                .to_ascii_lowercase()
                                .cmp(&b.name.to_ascii_lowercase())
                        }),
                    }
                    let count = rows.len();

                    // Leave room for the action bar at the bottom.
                    // Fixed, screen-derived height leaves room for the action
                    // bar below; the table scrolls internally past that.
                    let screen_h = ui.ctx().screen_rect().height();
                    let table_height = (screen_h - 240.0).clamp(140.0, 820.0);

                    use egui_extras::{Column, TableBuilder};
                    TableBuilder::new(ui)
                        .striped(true)
                        .sense(egui::Sense::click())
                        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                        .max_scroll_height(table_height)
                        .column(Column::auto().at_least(56.0).resizable(true))
                        .column(Column::auto().at_least(88.0).resizable(true))
                        .column(Column::auto().at_least(150.0).resizable(true))
                        .column(Column::auto().at_least(84.0).resizable(true))
                        .column(Column::remainder())
                        .header(22.0, |mut header| {
                            header.col(|ui| {
                                if sort_header(ui, "PID", sort == ProcSort::Pid).clicked() {
                                    new_sort = ProcSort::Pid;
                                }
                            });
                            header.col(|ui| {
                                if sort_header(ui, "Memory", sort == ProcSort::Memory).clicked() {
                                    new_sort = ProcSort::Memory;
                                }
                            });
                            header.col(|ui| {
                                if sort_header(ui, "Process", sort == ProcSort::Name).clicked() {
                                    new_sort = ProcSort::Name;
                                }
                            });
                            header.col(|ui| {
                                ui.strong("Role");
                            });
                            header.col(|ui| {
                                ui.strong("Command");
                            });
                        })
                        .body(|body| {
                            body.rows(22.0, count, |mut row| {
                                let p = rows[row.index()];
                                row.set_selected(selected == Some(p.pid));
                                row.col(|ui| {
                                    ui.monospace(p.pid.to_string());
                                });
                                row.col(|ui| {
                                    ui.monospace(human_bytes(p.rss_bytes));
                                });
                                row.col(|ui| {
                                    let mut text = egui::RichText::new(&p.name).strong();
                                    if p.is_wine {
                                        text = text.color(egui::Color32::from_rgb(120, 170, 255));
                                    }
                                    ui.add(egui::Label::new(text).truncate());
                                    if p.is_wine {
                                        ui.label(
                                            egui::RichText::new("proton")
                                                .small()
                                                .color(egui::Color32::from_rgb(120, 170, 255)),
                                        );
                                    }
                                });
                                row.col(|ui| {
                                    if let Some(role) = &p.role {
                                        // Highlight "renderer" — the game process
                                        // for Chromium/NW.js titles.
                                        let color = if role == "renderer" {
                                            egui::Color32::from_rgb(120, 230, 140)
                                        } else {
                                            egui::Color32::GRAY
                                        };
                                        ui.label(egui::RichText::new(role).small().color(color));
                                    }
                                });
                                row.col(|ui| {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&p.cmdline).weak().small(),
                                        )
                                        .truncate(),
                                    );
                                });
                                let resp = row.response();
                                if resp.clicked() {
                                    selected = Some(p.pid);
                                }
                                if resp.double_clicked() {
                                    do_attach = Some(p.pid);
                                }
                            });
                        });

                    // Action bar.
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("{count} processes")).weak());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if ui
                                .add_enabled(selected.is_some(), egui::Button::new("Attach"))
                                .clicked()
                            {
                                if let Some(pid) = selected {
                                    do_attach = Some(pid);
                                }
                            }
                        });
                    });
                });

            self.picker_selected = selected;
            self.picker_sort = new_sort;
            if !open || close {
                self.show_picker = false;
            }
        }

        // ---- Apply deferred actions ------------------------------------
        if do_refresh_procs {
            self.procs = list_processes();
            // Preselect the most likely game: the heaviest Wine/Proton process,
            // else the heaviest process overall. Saves hunting through helpers.
            let still_valid = self
                .picker_selected
                .is_some_and(|pid| self.procs.iter().any(|p| p.pid == pid));
            if !still_valid {
                self.picker_selected = pick_best_process(&self.procs);
            }
        }
        if let Some(pid) = do_attach {
            self.attach(pid);
            self.show_picker = false;
        }
        if do_elevate {
            if privilege::pkexec_available() {
                self.status = "Requesting elevation via pkexec…".into();
                // On success this replaces the process; only returns on failure.
                let err = privilege::relaunch_elevated();
                self.status = format!("Could not elevate: {err}");
            } else {
                self.status = "pkexec not found; install polkit or run via sudo.".into();
            }
        }
        if do_first {
            self.start_scan(true);
        }
        if do_next {
            self.start_scan(false);
        }
        if do_reset {
            self.reset_scan();
        }
        for (addr, ty) in add_to_table {
            // Avoid duplicates (same address + type).
            if self.saved.iter().any(|e| e.addr == addr && e.ty == ty) {
                continue;
            }
            let value_text = self
                .scanner
                .as_ref()
                .and_then(|s| read_current(s, addr, ty))
                .unwrap_or_default();
            self.saved.push(SavedEntry {
                desc: String::new(),
                addr,
                ty,
                value_text,
                frozen: false,
            });
        }
        // Remove in reverse so indices stay valid.
        remove_from_table.sort_unstable();
        for i in remove_from_table.into_iter().rev() {
            if i < self.saved.len() {
                self.saved.remove(i);
            }
        }

        // Keep current values (scanner results, or the RPG Maker snapshot) live.
        if self.scanner.is_some() || self.mode == AppMode::RpgMaker {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }
}

/// Best guess at the process a user wants to attach to.
///
/// For a Chromium/NW.js game (RPG Maker MV/MZ) the data lives in the largest
/// `renderer`; otherwise fall back to the heaviest Wine/Proton process, then
/// the heaviest process overall.
fn pick_best_process(procs: &[ProcInfo]) -> Option<i32> {
    procs
        .iter()
        .filter(|p| p.role.as_deref() == Some("renderer"))
        .max_by_key(|p| p.rss_bytes)
        .or_else(|| {
            procs
                .iter()
                .filter(|p| p.is_wine)
                .max_by_key(|p| p.rss_bytes)
        })
        .or_else(|| procs.iter().max_by_key(|p| p.rss_bytes))
        .map(|p| p.pid)
}

/// A borderless, clickable column header that shows a ▼ marker when active.
fn sort_header(ui: &mut eframe::egui::Ui, text: &str, active: bool) -> eframe::egui::Response {
    use eframe::egui;
    let label = if active {
        format!("{text} ▼")
    } else {
        text.to_string()
    };
    ui.add(egui::Button::new(egui::RichText::new(label).strong()).frame(false))
}

/// Read the value at `addr` decoded as `ty`, returning a display string.
///
/// A cheat-table entry's type may differ from the active scan type, so this
/// reads raw bytes and decodes with the requested width.
fn read_current(scanner: &Scanner, addr: u64, ty: ScanType) -> Option<String> {
    scanner.read_typed(addr, ty).ok().map(|v| v.to_string())
}

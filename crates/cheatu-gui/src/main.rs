//! cheatu-gui — a cross-platform (Wayland + X11) memory scanner UI.
//!
//! Built on egui/eframe (pure Rust, winit + glow), so it renders identically on
//! KDE, GNOME, and standalone window managers without pulling in GTK or Qt.

// Don't spawn a console window on the odd chance this is cross-built for Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cheat_table;
mod feedback;
mod file_dialog;
mod my_cheats;
mod pointer_window;
mod rpg_detect;
mod screen;
mod settings;
mod start_screen;
mod theme;
mod wizard;
mod wizard_ui;

use feedback::Feedback;
use my_cheats::{Answer, CloseAction, Pending};
use screen::{RpgPick, Screen, ScreenState};
use start_screen::{GameRow, ListAction, ListState};
use theme::{SystemTheme, Theme, ThemePref};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cheatu_core::process::ProcInfo;
use cheatu_core::scan::{
    address_hint, AddrHint, Confidence, FirstScan, NextScan, ScanProgress, Scanner, ANY_TYPES,
};
use cheatu_core::table::{self, CheatTable};
use cheatu_core::{
    list_processes, privilege, probe_address, read_maps, region_for, Mem, ProbeOutcome, ScanType,
    ScanValue,
};
use cheatu_core::{LiveSource, MemSource};
use cheatu_inject::{agent, detect_dir, find_rpgmaker_games, scan_dir, Engine, FoundGame};

use cheat_table::GuiEntry;
use file_dialog::FileDialog;
use pointer_window::PointerWindow;
use wizard_ui::{WizardEvent, WizardWindow};

/// How often the background freeze thread rewrites frozen values.
const FREEZE_INTERVAL: Duration = Duration::from_millis(40);

/// How long the auto-probe lets the target run after writing its sentinel
/// before reading back — a couple of frames. Tunable knob.
const PROBE_WAIT: Duration = Duration::from_millis(200);

/// How many addresses one "Probe all" pass will touch.
///
/// Probing writes to live game memory, so the batch is capped rather than
/// unbounded, and the addresses are probed one at a time — only ever one wrong
/// value in the game at once, restored before the next. At [`PROBE_WAIT`] each
/// this is also about as long as anyone will sit and wait.
const PROBE_BATCH_MAX: usize = 32;

/// Shared state driving the background freeze thread. The UI updates it each
/// frame; the thread reads it and writes the values into the target — so
/// freezing keeps working even when the GUI window is hidden or not repainting.
#[derive(Default)]
struct FreezeShared {
    /// Target pid, or 0 when not attached.
    pid: i32,
    /// Addresses to hold, with the exact value to write.
    items: Vec<(u64, ScanValue)>,
    /// Addresses the thread could not write to since the UI last looked.
    /// The UI re-resolves the entries behind them.
    failed: Vec<u64>,
}

/// The process's memory map is re-read at most this often for resolving
/// address expressions.
const REGIONS_REFRESH: Duration = Duration::from_secs(1);

/// File extension of cheat tables.
const TABLE_EXT: &str = "toml";

/// What the open file dialog is for.
#[derive(Copy, Clone, PartialEq, Eq)]
enum DialogPurpose {
    OpenTable,
    SaveTable,
}

/// Cap on how many candidate rows we render at once (the table is virtualized,
/// but building the snapshot for millions of hits would still be wasteful).
const MAX_DISPLAY: usize = 2000;

/// Only compute the "real value" hint once the candidate set is narrow enough
/// to be worth eyeballing. On a huge un-narrowed list nearly every row would
/// read "heap/anon" — noise, not signal — so we skip the maps read entirely
/// above this count.
const HINT_THRESHOLD: usize = 200;

/// Narrowest the scan results may get: the results table's fixed columns,
/// its "+" column and the gaps and margins around them. The cheat table panel
/// can't be dragged wider than what leaves this much room, so it never covers
/// the results.
const RESULTS_MIN_WIDTH: f32 = 640.0;

/// Width of the results table's "+" / probe column. Below this the add button
/// would be squeezed out of sight.
const RESULT_ACTIONS_MIN_WIDTH: f32 = 64.0;

/// Narrowest the cheat table panel may be dragged.
const CHEAT_TABLE_MIN_WIDTH: f32 = 260.0;

/// Set by build.rs: the git tag when built exactly on one, else the short
/// commit hash, else the crate version (tarball builds without .git).
const VERSION: &str = env!("CHEATU_VERSION");

fn main() -> eframe::Result<()> {
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([1100.0, 700.0])
        // Wide enough for the full header row (tabs, attach status,
        // settings/about buttons) without squishing.
        .with_min_inner_size([960.0, 540.0])
        .with_title("cheatu")
        // Match the installed .desktop file's basename so GNOME (Wayland) and
        // other desktops associate the window with it and show its icon in the
        // taskbar/dock. Also sets the X11 WM_CLASS.
        .with_app_id("io.github.carroarmato0.cheatu");
    // A window icon for environments that use per-window icons (X11, title
    // bars, most non-GNOME docks). GNOME on Wayland ignores this and uses the
    // .desktop icon matched via app_id above instead.
    if let Ok(icon) =
        eframe::icon_data::from_png_bytes(include_bytes!("../assets/cheatu-icon-256.png"))
    {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "cheatu",
        options,
        Box::new(|cc| {
            // Needed to render the SVG logo in the About window.
            egui_extras::install_image_loaders(&cc.egui_ctx);
            install_fonts(&cc.egui_ctx);
            configure_style(&cc.egui_ctx);
            let settings = settings::load();
            let theme = settings.theme;
            // Repair a config directory an older elevated cheatu left owned
            // by root (best-effort; see `give_config_to_home_owner`).
            let _ = settings::give_config_to_home_owner(&settings::config_path(), &home_dir());
            let ctx = cc.egui_ctx.clone();
            let system_theme =
                SystemTheme::start(theme == ThemePref::System, move || ctx.request_repaint());
            Ok(Box::new(CheatuApp {
                pause_while_scanning: true,
                theme,
                system_theme: Some(system_theme),
                advanced: settings.advanced,
                // Set when we were relaunched elevated for a specific target;
                // attached on the first frame so the user lands back on scanning.
                pending_attach: privilege::attach_target(),
                ..Default::default()
            }))
        }),
    )
}

/// Register fonts as fallbacks after egui's default Latin font:
///
/// 1. A small **bundled** monochrome icon font (a subset of Noto Emoji, OFL) so
///    the UI's glyphs (🧪 🔒 🔍 …) render reliably. egui only rasterizes
///    monochrome outlines and can't be trusted to find a system font covering
///    newer emoji; baking the font into the binary also survives being
///    relaunched elevated via pkexec, where the font environment differs.
/// 2. A CJK system font (best-effort) so Japanese/Chinese/Korean RPG Maker text
///    renders instead of empty boxes.
///
/// Regenerate the icon font with `assets/regen-icons.sh` when adding new glyphs.
fn install_fonts(ctx: &eframe::egui::Context) {
    use eframe::egui::{FontData, FontDefinitions, FontFamily};

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "icons".to_owned(),
        FontData::from_static(include_bytes!("../assets/CheatuIcons.ttf")),
    );
    if let Some(bytes) = cjk_font_path().and_then(|p| std::fs::read(&p).ok()) {
        fonts
            .font_data
            .insert("cjk".to_owned(), FontData::from_owned(bytes));
    }

    let has_cjk = fonts.font_data.contains_key("cjk");
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        let fam = fonts.families.entry(family).or_default();
        fam.push("icons".to_owned());
        if has_cjk {
            fam.push("cjk".to_owned());
        }
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
    // Match row pre-sizing to our padded button height (15pt text + 2×3pt
    // padding), so labels next to buttons center on the same line.
    style.spacing.interact_size.y = 22.0;
    // Labels must not grab clicks/hover for text selection, or clicking table
    // rows only works in the gaps between texts and hover highlights flicker.
    style.interaction.selectable_labels = false;
    ctx.set_style(style);
}

fn apply_theme(ctx: &eframe::egui::Context, theme: Theme) {
    use eframe::egui::{Color32, Visuals};
    let mut v = match theme {
        Theme::Dark => Visuals::dark(),
        Theme::Light => Visuals::light(),
    };
    // egui's default grey text is low-contrast; brighten plain labels and
    // resting-widget text while keeping hover/active/disabled semantics.
    let text = match theme {
        Theme::Dark => Color32::from_gray(220),
        Theme::Light => Color32::from_gray(10),
    };
    v.widgets.noninteractive.fg_stroke.color = text;
    v.widgets.inactive.fg_stroke.color = text;
    // `.weak()` text is a 50% blend of the text color toward this fill; the
    // default (≈window bg) lands unreadably dim, so blend toward a mid grey
    // instead: weak text ends up ~gray(170) on dark, ~gray(75) on light.
    v.widgets.noninteractive.weak_bg_fill = match theme {
        Theme::Dark => Color32::from_gray(120),
        Theme::Light => Color32::from_gray(140),
    };
    ctx.set_visuals(v);
}

/// Accent colors: the bright shades tuned for the dark theme wash out on a
/// light background, so pick darker equivalents there.
fn accent(ui: &eframe::egui::Ui, dark: [u8; 3], light: [u8; 3]) -> eframe::egui::Color32 {
    let [r, g, b] = if ui.visuals().dark_mode { dark } else { light };
    eframe::egui::Color32::from_rgb(r, g, b)
}

fn ok_color(ui: &eframe::egui::Ui) -> eframe::egui::Color32 {
    accent(ui, [120, 230, 140], [0, 128, 55])
}

fn warn_color(ui: &eframe::egui::Ui) -> eframe::egui::Color32 {
    accent(ui, [240, 220, 120], [154, 108, 0])
}

fn err_color(ui: &eframe::egui::Ui) -> eframe::egui::Color32 {
    accent(ui, [255, 96, 96], [185, 28, 28])
}

fn wine_color(ui: &eframe::egui::Ui) -> eframe::egui::Color32 {
    accent(ui, [120, 170, 255], [29, 78, 216])
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
        // macOS system fonts: CJK ideographs + kana, then Hangul.
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
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

/// A question in a small window, waiting for an answer.
#[derive(Copy, Clone)]
enum Confirm {
    Unsaved(Pending),
    /// Remove the cheat with this id.
    Remove(u64),
    ClearAll,
}

/// How often the game list refreshes while it is on screen.
const PROC_REFRESH: Duration = Duration::from_secs(3);

/// Column used to sort the results table.
///
/// Only offered while the whole result set is on screen — sorting the first
/// [`MAX_DISPLAY`] of a million would just reshuffle an arbitrary slice.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
enum ResSort {
    #[default]
    Address,
    Type,
    Previous,
    /// Best-looking region first, unranked last.
    Region,
}

/// Sort key for [`ResSort::Region`]: lower sorts first.
fn region_rank(hint: Option<AddrHint>) -> u8 {
    match hint.map(|h| h.confidence) {
        Some(Confidence::Likely) => 0,
        Some(Confidence::Neutral) => 1,
        Some(Confidence::Unlikely) => 2,
        None => 3,
    }
}

/// A snapshot row shown in the results table.
struct DisplayRow {
    addr: u64,
    prev: ScanValue,
    /// "Real value" hint, when the result set is narrow enough to compute it
    /// (see [`HINT_THRESHOLD`]). `None` renders as a muted "—".
    hint: Option<AddrHint>,
}

/// Human explanation for a candidate's "real value" hint, shown on hover.
fn hint_tooltip(h: &AddrHint) -> &'static str {
    match (h.confidence, h.label) {
        (_, "text") => "The on-screen text of the value, not the value itself.",
        (Confidence::Unlikely, "stack") => "Likely a transient stack copy, not the stored value.",
        (Confidence::Likely, "heap") => "Likely the real value — heap-allocated game state.",
        (Confidence::Likely, "module data") => {
            "Likely the real value — a global in a module's data."
        }
        (Confidence::Neutral, _) => "Could be the real value or a copy — region is inconclusive.",
        _ => "Heuristic guess from the address's memory region and type.",
    }
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

/// The value-type selection in the UI: a numeric width, a byte/string
/// pattern search, or "Any" (unknown numeric type).
#[derive(Copy, Clone, PartialEq, Eq)]
enum TypeSel {
    Any,
    One(ScanType),
    Aob,
    Str,
}

impl Default for TypeSel {
    fn default() -> Self {
        TypeSel::One(ScanType::I32)
    }
}

impl TypeSel {
    /// The types a first scan should try for this selection.
    fn types(self) -> Vec<ScanType> {
        match self {
            TypeSel::Any => ANY_TYPES.to_vec(),
            TypeSel::One(t) => vec![t],
            // Handled directly in `start_scan` via `FirstScan::Pattern`.
            TypeSel::Aob | TypeSel::Str => Vec::new(),
        }
    }
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
                    ui.colored_label(warn_color(ui), "agent outdated — update available");
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
                    ui.colored_label(ok_color(ui), format!("agent installed ({short})"));
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
            ui.colored_label(
                warn_color(ui),
                "This folder doesn't look like an NW.js game.",
            );
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
                    ui.colored_label(err_color(ui), format!("⚠ {msg}"));
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
                        .button(format!("Use this folder — {}", cwd_engine.label()))
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
                                            .color(ok_color(ui)),
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
                warn_color(ui),
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
                ui.colored_label(err_color(ui), format!("⚠ {msg}"));
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
                            if ui.small_button("+").on_hover_text("add one").clicked() {
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
    /// What the user chose for an attached RPG Maker game.
    rpg_pick: RpgPick,
    /// "RPG Maker game not running yet? Set it up…" was used.
    rpg_setup: bool,
    /// The attached game's engine, when it is an RPG Maker game.
    attached_rpg: Option<cheatu_inject::Engine>,
    /// The attached RPG Maker game's folder, as detected when attaching.
    attached_dir: Option<PathBuf>,
    detections: rpg_detect::Detections,
    rpg: RpgState,

    scanner: Option<Scanner>,
    attached: Option<(i32, String)>,
    /// Pid to attach to on the next frame (from a `--attach` relaunch arg).
    pending_attach: Option<i32>,
    /// Pid whose attach failed on permissions — drives the "Elevate & attach"
    /// prompt. Cleared on any successful attach.
    needs_elevation: Option<i32>,

    // Process picker.
    show_picker: bool,
    /// The game list's filter, selection and sort.
    list: ListState,
    /// The game list, one row per program (see `start_screen::fold`).
    game_rows: Vec<GameRow>,
    procs_refreshed: Option<Instant>,
    procs: Vec<ProcInfo>,
    /// Column the results table is sorted by.
    res_sort: ResSort,

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
    /// Whether to show the "real value" Hint column — true only for a narrowed
    /// numeric result set (hidden for huge lists and for AoB/string scans, where
    /// a `Bytes` match is the goal, not a demoted "text" hit).
    show_hints: bool,

    // Background scan.
    pending: Option<Receiver<ScanOutcome>>,
    scanning: bool,
    /// Progress and cancellation for the scan on the worker thread.
    scan_progress: Option<Arc<ScanProgress>>,
    /// The pid the main scan has suspended, so exiting mid-scan resumes it.
    scan_paused: pointer_window::PauseSlot,
    status: String,

    // Background probe (active "which one sticks" disambiguation).
    /// In-flight probe result, if any. `None` outcome = the probe failed
    /// (e.g. memory not writable), so the busy state still clears.
    probe_pending: Option<Receiver<(u64, Option<ProbeOutcome>)>>,
    /// Addresses in the batch being probed, oldest first. Empty when idle.
    probe_batch: Vec<u64>,
    /// How many of `probe_batch` have reported back.
    probe_done: usize,
    /// Set to stop the batch after the address being probed right now.
    probe_cancel: Arc<AtomicBool>,
    /// Waiting for the user to confirm a batch probe.
    show_probe_confirm: bool,
    /// Latest probe outcome per address, shown as an icon in the results row.
    probe_results: HashMap<u64, ProbeOutcome>,

    // Cheat table.
    saved: Vec<GuiEntry>,
    /// Reads target memory for address expressions; one per attached process.
    live: Option<LiveSource>,
    /// When `live`'s memory map was last re-read.
    live_refreshed: Option<Instant>,
    /// The one `aob(...)` resolve running in the background.
    aob_pending: Option<Receiver<cheat_table::BgResolve>>,
    /// Bumped on attach and on every expression edit; a background resolve
    /// started under an older generation is dropped when it comes back.
    resolve_generation: u64,
    /// Why `live` could not be opened for the attached process.
    live_error: Option<String>,
    /// File the table was last opened from or saved to.
    table_path: Option<PathBuf>,
    /// `process` of the opened table, saved again when nothing is attached.
    table_process: Option<String>,
    /// Folder the file dialog starts in next time.
    last_table_dir: Option<PathBuf>,
    /// A native panel's answer (macOS), handled with the in-app dialog's.
    native_done: Option<(DialogPurpose, file_dialog::Outcome, PathBuf)>,
    file_dialog: Option<(DialogPurpose, FileDialog)>,
    /// After opening a table: offer to attach to this (pid, name).
    attach_offer: Option<(i32, String)>,
    /// The one "Make AoB signature" running in the background.
    sig_pending: Option<Receiver<cheat_table::BgSignature>>,
    /// The pointer scan window, while open.
    /// The cheat a signature is being made for, and its progress.
    sig_row: Option<u64>,
    sig_progress: Option<Arc<ScanProgress>>,
    /// Where to report the finished signature.
    sig_at: Option<eframe::egui::Rect>,
    pointer_window: Option<PointerWindow>,
    /// Which My cheats rows are unfolded or being renamed.
    cheats_ui: my_cheats::PanelState,
    /// My cheats as last saved or opened, for the unsaved marker.
    saved_mark: my_cheats::Saved,
    /// A question waiting for an answer (unsaved changes, a removal).
    confirm: Option<Confirm>,
    /// Quit or open once the "Save changes?" save finishes.
    after_save: Option<Pending>,
    /// Relaunch elevated (attaching to this game) once "Save changes?" is
    /// answered.
    elevate_now: Option<Option<i32>>,
    /// The user chose to quit without saving.
    allow_close: bool,
    /// Confirmations and errors next to the control that caused them.
    feedback: Feedback,
    /// A restarted game the guided steps followed while a search ran.
    pending_follow: Option<i32>,
    /// The game the guided steps' wizard was started for (or followed to
    /// after a restart); attaching to any other game restarts it.
    wizard_for: Option<i32>,
    /// The guided steps' wizard, while the guided view is shown.
    wizard: Option<WizardWindow>,

    // Background freeze.
    freeze_state: Arc<Mutex<FreezeShared>>,
    freeze_started: bool,

    // Preferences & modals.
    theme: ThemePref,
    /// The desktop's theme, followed while `theme` is `System`.
    system_theme: Option<SystemTheme>,
    /// The theme currently applied to the context.
    applied_theme: Option<Theme>,
    show_about: bool,
    show_settings: bool,
    /// Show the full scanner and every tool instead of the guided steps.
    advanced: bool,
}

impl CheatuApp {
    fn attach(&mut self, pid: i32) {
        self.attach_with(pid, false);
    }

    /// Attach to `pid`. `restarted`: it is the attached game, restarted.
    fn attach_with(&mut self, pid: i32, restarted: bool) {
        match Scanner::new(pid) {
            Ok(s) => {
                let info = self
                    .procs
                    .iter()
                    .find(|p| p.pid == pid)
                    .cloned()
                    .or_else(|| list_processes().into_iter().find(|p| p.pid == pid));
                let name = info.as_ref().map_or_else(|| "?".into(), |p| p.name.clone());
                let previous = self.attached.as_ref().map(|(p, _)| *p);
                if previous != Some(pid) && !restarted {
                    self.rpg_pick = RpgPick::Undecided;
                }
                if !restarted {
                    // What the game list showed for this row (detected over
                    // all of the app's processes), else this process alone.
                    let found = self
                        .game_rows
                        .iter()
                        .find(|r| r.pid == pid)
                        .and_then(|r| Some((r.dir.clone()?, r.engine?)))
                        .or_else(|| info.as_ref().and_then(|p| self.detections.get(p)));
                    self.attached_rpg = found.as_ref().map(|(_, e)| *e);
                    self.attached_dir = found.map(|(d, _)| d);
                    // Prefill RPG Maker mode with the game's folder.
                    if let Some(dir) = &self.attached_dir {
                        self.rpg.dir = dir.to_string_lossy().into_owned();
                    }
                }
                self.rpg_setup = false;
                self.attached = Some((pid, name));
                self.scanner = Some(s);
                self.needs_elevation = None;
                self.display.clear();
                self.result_count = 0;
                self.status = format!("Attached to {}.", self.game_title());
                match LiveSource::open(pid) {
                    Ok(live) => {
                        self.live = Some(live);
                        self.live_error = None;
                    }
                    Err(e) => {
                        let why = format!("cannot read the game's memory for cheats: {e}");
                        self.status = format!("Attached to {}, but {why}.", self.game_title());
                        self.live = None;
                        self.live_error = Some(why);
                    }
                }
                self.live_refreshed = Some(Instant::now());
                // Any background resolve still running read the old process.
                self.resolve_generation += 1;
                // A plain address means nothing in another game (or another
                // run of it): never keep writing it there.
                if previous != Some(pid) {
                    let mut unfrozen = 0;
                    for e in &mut self.saved {
                        if e.entry.frozen && !my_cheats::survives_restart(&e.entry.address) {
                            e.entry.frozen = false;
                            unfrozen += 1;
                        }
                    }
                    if unfrozen > 0 {
                        self.status.push_str(&format!(
                            " Unfroze {unfrozen} cheat(s) that only worked in the previous run."
                        ));
                    }
                }
                // Addresses of the previous process mean nothing here; drop
                // them so nothing is written before the entries re-resolve.
                for e in &mut self.saved {
                    e.resolved = Err("not resolved yet".into());
                    e.write_failed = false;
                    e.needs_resolve = true;
                }
            }
            Err(e) => {
                // A permission error on a same-user process means we lack ptrace
                // rights (restrictive yama scope) — offer to relaunch elevated
                // and re-attach. Any other error isn't fixed by elevation.
                if e.kind() == std::io::ErrorKind::PermissionDenied && !privilege::is_root() {
                    self.needs_elevation = Some(pid);
                    self.status = "This game needs admin rights.".into();
                } else {
                    self.needs_elevation = None;
                    self.status = format!("Could not attach to this game: {e}.");
                }
            }
        }
    }

    /// Let go of the game: back to the game list. Cheats stay in My cheats.
    fn detach(&mut self) {
        // Stop work on the game being left: a search (its result is dropped
        // in `poll_scan`) and a probe batch.
        if let Some(p) = &self.scan_progress {
            p.cancel();
        }
        self.probe_cancel.store(true, Ordering::Relaxed);
        if let Some(p) = &self.sig_progress {
            p.cancel();
        }
        self.attached = None;
        self.scanner = None;
        self.live = None;
        self.live_error = None;
        self.attached_rpg = None;
        self.attached_dir = None;
        self.rpg_pick = RpgPick::Undecided;
        self.display.clear();
        self.result_count = 0;
        self.resolve_generation += 1;
        for e in &mut self.saved {
            e.resolved = Err("not attached".into());
            e.write_failed = false;
            e.needs_resolve = true;
        }
        self.status = "Detached.".into();
    }

    /// The attached game's name for display: an RPG Maker game's folder
    /// (its program is usually just "Game"), else the program name.
    fn game_title(&self) -> String {
        let Some((_, name)) = &self.attached else {
            return String::new();
        };
        if self.attached_rpg.is_some() {
            if let Some(folder) = self.attached_dir.as_deref().and_then(Path::file_name) {
                return folder.to_string_lossy().into_owned();
            }
        }
        name.clone()
    }

    fn screen_state(&self) -> ScreenState {
        ScreenState {
            attached: self.attached.is_some(),
            rpg: self.attached_rpg,
            pick: self.rpg_pick,
            advanced: self.advanced,
            rpg_setup: self.rpg_setup,
        }
    }

    /// Why a scan can't run with the current inputs, for the disabled scan
    /// buttons. Mirrors the guards in [`CheatuApp::start_scan`], which stays
    /// the authority — this only decides whether the button is clickable.
    fn scan_blocker(&self, first: bool) -> Option<&'static str> {
        let Some(scanner) = &self.scanner else {
            return Some("Attach to a game first.");
        };
        let text = self.value_text.trim();
        if !first {
            if !scanner.has_scanned() {
                return Some("Search first, then narrow down.");
            }
            return (self.next_mode.needs_value() && text.parse::<f64>().is_err())
                .then_some("Enter a value for this comparison.");
        }
        match self.type_sel {
            TypeSel::Aob => cheatu_core::parse_aob(text)
                .is_none()
                .then_some("Enter a byte pattern, e.g. 48 65 ?? 6C 6F."),
            TypeSel::Str => self
                .value_text
                .is_empty()
                .then_some("Enter a string to search for."),
            // An unknown-value scan keeps every address, so it needs no value.
            _ if self.unknown_initial => None,
            _ => (cheatu_core::parse_range(text).is_none() && text.parse::<f64>().is_err())
                .then_some("Enter a value to search for, e.g. 100 — or a range, 6400000..6500000."),
        }
    }

    fn start_scan(&mut self, first: bool) {
        let Some(mut scanner) = self.scanner.take() else {
            self.status = "Attach to a game first.".into();
            return;
        };

        // A number is required for a value search; comparisons like "changed"
        // are not. We validate loosely (parses as a number) and let the engine
        // decide which candidate types can actually represent it.
        let value_is_number = self.value_text.trim().parse::<f64>().is_ok();

        let job = if first {
            match self.type_sel {
                TypeSel::Aob => match cheatu_core::parse_aob(self.value_text.trim()) {
                    Some(pattern) => Job::First(FirstScan::Pattern(pattern)),
                    None => {
                        self.status = "Enter a byte pattern, e.g. 48 65 ?? 6C 6F.".into();
                        self.scanner = Some(scanner);
                        return;
                    }
                },
                TypeSel::Str => {
                    let pattern: Vec<Option<u8>> = self.value_text.bytes().map(Some).collect();
                    if pattern.is_empty() {
                        self.status = "Enter a string to search for.".into();
                        self.scanner = Some(scanner);
                        return;
                    }
                    Job::First(FirstScan::Pattern(pattern))
                }
                // Aob/Str matched above, so this is "Any" or a single numeric
                // type; "Any" keeps every address once per type.
                _ if self.unknown_initial => Job::First(FirstScan::Unknown(self.type_sel.types())),
                // A range, for a value you can only bracket rather than read
                // off the screen exactly.
                _ if cheatu_core::parse_range(self.value_text.trim()).is_some() => {
                    let (low, high) = cheatu_core::parse_range(self.value_text.trim())
                        .expect("just checked it parses");
                    Job::First(FirstScan::Range {
                        low,
                        high,
                        types: self.type_sel.types(),
                    })
                }
                _ if value_is_number => Job::First(FirstScan::Value {
                    value: self.value_text.trim().to_string(),
                    types: self.type_sel.types(),
                }),
                _ => {
                    self.status =
                        "Enter a value to search for, e.g. 100 — or a range, 6400000..6500000."
                            .into();
                    self.scanner = Some(scanner);
                    return;
                }
            }
        } else {
            if !scanner.has_scanned() {
                self.status = "Search first, then narrow down.".into();
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
        // Taken before the scanner moves onto the worker thread — it's the only
        // way back in while the scan runs.
        self.scan_progress = Some(scanner.progress());
        self.status = "Scanning…".into();
        let pause = self.pause_while_scanning;
        let pause_slot = self.scan_paused.clone();

        std::thread::spawn(move || {
            let pid = scanner.pid();
            // Suspend the target so values hold still and reads don't race the
            // game. The guard resumes it (only if it was suspended), and the
            // slot lets `on_exit` resume it when the app quits mid-scan.
            let paused = pause
                .then(|| {
                    pointer_window::Paused::new(
                        &pause_slot,
                        pid,
                        cheatu_core::suspend,
                        cheatu_core::resume,
                    )
                })
                .flatten();
            let res = match job {
                Job::First(fs) => {
                    scanner.reset();
                    scanner.first_scan(fs)
                }
                Job::Next(cmp, operand) => scanner.next_scan(cmp, operand.as_deref()),
            };
            drop(paused);
            let message = match res {
                Ok(()) => format!("{} results.", scanner.count()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    format!("Scan cancelled — {} results kept.", scanner.count())
                }
                Err(e) => format!("Scan error: {e}"),
            };
            let _ = tx.send(ScanOutcome { scanner, message });
        });
    }

    fn rebuild_display(&mut self) {
        self.display.clear();
        self.show_hints = false;
        if let Some(scanner) = &self.scanner {
            self.result_count = scanner.count();

            // A `Bytes` result set is an AoB/string scan — the hint doesn't
            // apply (a text match is the goal there). Otherwise, once narrowed,
            // read the target's maps once and rank each candidate by region.
            let is_pattern = scanner
                .results()
                .first()
                .is_some_and(|c| matches!(c.ty(), ScanType::Bytes(_)));
            let regions = (!is_pattern && scanner.count() <= HINT_THRESHOLD)
                .then(|| read_maps(scanner.pid()).ok())
                .flatten();
            self.show_hints = regions.is_some();

            for cand in scanner.results().iter().take(MAX_DISPLAY) {
                let hint = regions.as_ref().map(|regs| {
                    let kind = region_for(regs, cand.addr).map(|r| r.kind());
                    match kind {
                        Some(kind) => address_hint(kind, cand.ty()),
                        // Address not in any current region (freed/moved).
                        None => AddrHint {
                            confidence: Confidence::Neutral,
                            label: "?",
                        },
                    }
                });
                self.display.push(DisplayRow {
                    addr: cand.addr,
                    prev: cand.prev.clone(),
                    hint,
                });
            }
        }
        self.sort_display();
    }

    /// Order the displayed rows by the chosen column. Stable, so rows that tie
    /// keep the address order the scanner produced them in.
    fn sort_display(&mut self) {
        match self.res_sort {
            ResSort::Address => self.display.sort_by_key(|d| d.addr),
            ResSort::Type => self.display.sort_by_key(|d| d.prev.ty().friendly_label()),
            ResSort::Previous => self.display.sort_by(|a, b| {
                a.prev
                    .as_f64()
                    .partial_cmp(&b.prev.as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            ResSort::Region => self.display.sort_by_key(|d| region_rank(d.hint)),
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

    fn undo_scan(&mut self) {
        let undone = self.scanner.as_mut().is_some_and(|s| s.undo());
        if undone {
            self.rebuild_display();
            self.status = format!("Undone. {} results.", self.result_count);
        } else {
            self.status = "Nothing to undo.".into();
        }
    }

    fn poll_scan(&mut self) {
        let done = self.pending.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(outcome) = done {
            let attached = self.attached.as_ref().map(|(p, _)| *p);
            if attached != Some(outcome.scanner.pid()) {
                // Detached, or another game attached, while it ran.
                self.scanning = false;
                self.pending = None;
                self.scan_progress = None;
                return;
            }
            self.status = outcome.message;
            self.scanner = Some(outcome.scanner);
            self.scanning = false;
            self.pending = None;
            self.scan_progress = None;
            self.rebuild_display();
        }
    }

    fn poll_probe(&mut self) {
        // Drain everything that arrived: a batch reports one address at a time
        // and a frame may cover several. The worker dropping its sender is what
        // marks the batch over — whether it finished the list or was cancelled.
        let mut results = Vec::new();
        let mut finished = false;
        if let Some(rx) = &self.probe_pending {
            loop {
                match rx.try_recv() {
                    Ok(msg) => results.push(msg),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                }
            }
        }
        for (addr, outcome) in results {
            self.probe_done += 1;
            match outcome {
                Some(o) => {
                    self.probe_results.insert(addr, o);
                    if self.probe_batch.len() == 1 {
                        self.status = match o {
                            ProbeOutcome::Held => {
                                format!("0x{addr:012x} held — likely the real value.")
                            }
                            ProbeOutcome::Reverted => {
                                format!("0x{addr:012x} reverted — a live copy, probably not it.")
                            }
                        };
                    }
                }
                None if self.probe_batch.len() == 1 => {
                    self.status = format!("Probe of 0x{addr:012x} failed — memory not writable?");
                }
                // In a batch an unreadable address is just one that can't be
                // ruled in or out; the summary reports it as untested.
                None => {}
            }
            if self.probe_done < self.probe_batch.len() {
                self.status = format!(
                    "Probing… {} of {}.",
                    self.probe_done + 1,
                    self.probe_batch.len()
                );
            }
        }
        if finished {
            if self.probe_batch.len() > 1 {
                let held = self.batch_count(ProbeOutcome::Held);
                let reverted = self.batch_count(ProbeOutcome::Reverted);
                let untested = self.probe_done - held - reverted;
                let stopped = if self.probe_done < self.probe_batch.len() {
                    format!(
                        " — cancelled, {} of {} untested",
                        self.probe_batch.len() - self.probe_done,
                        self.probe_batch.len()
                    )
                } else {
                    String::new()
                };
                self.status = format!(
                    "Probed {}: {held} held, {reverted} reverted{}{stopped}.",
                    self.probe_done,
                    if untested > 0 {
                        format!(", {untested} not writable")
                    } else {
                        String::new()
                    }
                );
            }
            self.probe_pending = None;
        }
    }

    /// The addresses a "Probe all" pass would touch: the visible rows, capped
    /// at [`PROBE_BATCH_MAX`], minus any that are frozen — a frozen address is
    /// rewritten every [`FREEZE_INTERVAL`], so it would read as "reverted"
    /// whatever the game does.
    fn probe_all_targets(&self) -> Vec<(u64, usize)> {
        // An "Any" scan can list one address once per type; probing it twice
        // would cost another PROBE_WAIT to learn the same thing.
        let mut seen = std::collections::HashSet::new();
        self.display
            .iter()
            .filter(|d| {
                !self
                    .saved
                    .iter()
                    .any(|e| e.entry.frozen && e.resolved == Ok(d.addr))
            })
            .filter(|d| seen.insert(d.addr))
            .take(PROBE_BATCH_MAX)
            .map(|d| (d.addr, d.prev.ty().size()))
            .collect()
    }

    /// How many of the last batch ended with `want`. Only the addresses the
    /// worker reached count — the rest may still hold an outcome from an
    /// earlier batch.
    fn batch_count(&self, want: ProbeOutcome) -> usize {
        self.probe_batch
            .iter()
            .take(self.probe_done)
            .filter(|a| self.probe_results.get(a) == Some(&want))
            .count()
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
                    let failed: Vec<u64> = items
                        .iter()
                        .filter(|(addr, value)| m.write_at(*addr, &value.to_ne_bytes()).is_err())
                        .map(|(addr, _)| *addr)
                        .collect();
                    if !failed.is_empty() {
                        let mut s = state.lock().unwrap();
                        // Only for the process the items were meant for.
                        if s.pid == pid {
                            for a in failed {
                                if !s.failed.contains(&a) {
                                    s.failed.push(a);
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    /// Publish the current set of frozen values to the background thread.
    /// Entries that do not resolve are left out: never written.
    fn sync_freezes(&mut self) {
        let pid = self.attached.as_ref().map(|(p, _)| *p).unwrap_or(0);
        let items = cheat_table::freeze_items(&self.saved);
        if let Ok(mut s) = self.freeze_state.lock() {
            if s.pid != pid {
                s.failed.clear();
            }
            s.pid = pid;
            s.items = items;
        }
    }

    /// The live source with a memory map at most [`REGIONS_REFRESH`] old.
    fn fresh_live(&mut self) -> Option<&LiveSource> {
        let live = self.live.as_mut()?;
        let stale = self
            .live_refreshed
            .is_none_or(|t| t.elapsed() >= REGIONS_REFRESH);
        if stale {
            // On failure keep the old map; resolving reports what is missing.
            let _ = live.refresh_regions();
            self.live_refreshed = Some(Instant::now());
        }
        self.live.as_ref()
    }

    /// Re-resolve the cheat table's address expressions that are due:
    /// module/pointer expressions here on the schedule in
    /// [`cheat_table::due_for_resolve`], `aob(...)` ones only on demand and
    /// on a background thread, one at a time.
    fn resolve_entries(&mut self) {
        let now = Instant::now();

        // A finished background AoB resolve.
        if let Some(rx) = &self.aob_pending {
            match rx.try_recv() {
                Ok(r) => {
                    self.aob_pending = None;
                    // Dropped if it read another process, the row was removed
                    // or an expression was edited since it started.
                    let pid = self.attached.as_ref().map(|(p, _)| *p).unwrap_or(0);
                    cheat_table::apply_background_result(
                        &mut self.saved,
                        pid,
                        self.resolve_generation,
                        r,
                        now,
                    );
                }
                Err(mpsc::TryRecvError::Disconnected) => self.aob_pending = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        // Writes the freeze thread could not make.
        let failed = self
            .freeze_state
            .lock()
            .map(|mut s| std::mem::take(&mut s.failed))
            .unwrap_or_default();
        cheat_table::mark_write_failures(&mut self.saved, &failed);

        let Some(pid) = self.attached.as_ref().map(|(p, _)| *p) else {
            return;
        };
        if self.live.is_none() {
            return;
        }

        let due: Vec<usize> = (0..self.saved.len())
            .filter(|&i| {
                let e = &self.saved[i];
                !e.uses_aob()
                    && (e.needs_resolve || cheat_table::due_for_resolve(e, now, e.write_failed))
            })
            .collect();
        if !due.is_empty() {
            self.fresh_live();
            if let Some(live) = &self.live {
                for i in due {
                    let e = &mut self.saved[i];
                    let failed_at = if e.write_failed {
                        e.resolved.as_ref().ok().copied()
                    } else {
                        None
                    };
                    let r = e.entry.address.resolve(live).map_err(|err| err.to_string());
                    cheat_table::apply_resolve(e, r, now, failed_at);
                }
            }
        }

        if self.aob_pending.is_none() {
            if let Some(e) = self
                .saved
                .iter_mut()
                .find(|e| e.uses_aob() && (e.needs_resolve || e.write_failed))
            {
                let failed_at = if e.write_failed {
                    e.resolved.as_ref().ok().copied()
                } else {
                    None
                };
                // Picked up: don't start it again while it runs.
                e.needs_resolve = false;
                e.write_failed = false;
                let (id, expr) = (e.id, e.entry.address.clone());
                let generation = self.resolve_generation;
                let (tx, rx) = mpsc::channel();
                self.aob_pending = Some(rx);
                std::thread::spawn(move || {
                    let result = LiveSource::open(pid)
                        .map_err(|err| format!("cannot read the game: {err}"))
                        .and_then(|src| expr.resolve(&src).map_err(|err| err.to_string()));
                    let _ = tx.send(cheat_table::BgResolve {
                        id,
                        expr,
                        pid,
                        generation,
                        failed_at,
                        result,
                    });
                });
            }
        }
    }

    /// Start "Make AoB signature" for row `i` on a background thread: it
    /// scans memory and waits between two reads.
    fn start_signature(&mut self, i: usize, at: Option<eframe::egui::Rect>) {
        let Some(pid) = self.attached.as_ref().map(|(p, _)| *p) else {
            self.status = "Attach to a game first.".into();
            return;
        };
        let Some(e) = self.saved.get(i) else {
            return;
        };
        let Ok(addr) = e.resolved else {
            self.status = "The address does not resolve.".into();
            return;
        };
        if self.sig_pending.is_some() {
            self.status = "An AoB signature is already being made.".into();
            return;
        }
        let (id, expr, generation) = (e.id, e.entry.address.clone(), self.resolve_generation);
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(ScanProgress::default());
        self.sig_pending = Some(rx);
        self.sig_row = Some(id);
        self.sig_progress = Some(progress.clone());
        self.sig_at = at;
        self.status = "Making a signature…".into();
        std::thread::spawn(move || {
            let result = match LiveSource::open(pid) {
                Ok(src) => cheatu_core::make_signature_with(&src, addr, 64, &progress),
                Err(_) => Err(cheatu_core::SignatureError::Unreadable(addr)),
            };
            let _ = tx.send(cheat_table::BgSignature {
                id,
                expr,
                pid,
                generation,
                result,
            });
        });
    }

    /// Apply a finished "Make AoB signature".
    /// The file name a save of My cheats suggests.
    fn table_file_name(&self) -> String {
        self.table_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| {
                self.attached
                    .as_ref()
                    .map(|(_, n)| format!("{n}.{TABLE_EXT}"))
            })
            .unwrap_or_else(|| format!("cheats.{TABLE_EXT}"))
    }

    /// A signature run ended: forget its state; returns where to report.
    fn finish_signature(&mut self) -> Option<eframe::egui::Rect> {
        self.sig_pending = None;
        self.sig_row = None;
        self.sig_progress = None;
        self.sig_at.take()
    }

    fn poll_signature(&mut self) {
        let Some(rx) = &self.sig_pending else {
            return;
        };
        match rx.try_recv() {
            Ok(r) => {
                let cancelled = matches!(r.result, Err(cheatu_core::SignatureError::Cancelled));
                let at = self.finish_signature();
                let pid = self.attached.as_ref().map(|(p, _)| *p).unwrap_or(0);
                let (changed, status) =
                    cheat_table::apply_signature(&mut self.saved, pid, self.resolve_generation, r);
                // An edit: a resolve of the old expression must not land.
                if changed {
                    self.resolve_generation += 1;
                }
                if let Some(at) = at {
                    let now = Instant::now();
                    if changed {
                        self.feedback.ok(
                            at,
                            "✔ Signature made: keeps working after a restart",
                            now,
                        );
                    } else if !cancelled {
                        self.feedback.error(at, status.clone(), now);
                    }
                }
                self.status = status;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.finish_signature();
                self.status = "Making the AoB signature failed.".into();
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    /// Open the pointer scan window for row `i` (or point it there).
    fn open_pointer_window(&mut self, i: usize) {
        let (Some((_, name)), Some(e)) = (self.attached.clone(), self.saved.get(i)) else {
            return;
        };
        let Ok(addr) = e.resolved else {
            self.status = "The address does not resolve.".into();
            return;
        };
        let ty = e.entry.ty;
        match &mut self.pointer_window {
            Some(w) if w.busy() => {
                self.status = "A pointer scan is running; cancel it first.".into();
            }
            Some(w) => w.retarget(addr, name, ty),
            None => {
                self.pointer_window = Some(PointerWindow::open_for(addr, name).with_type(ty));
            }
        }
    }

    /// Why the table can't be saved as it is, if it can't.
    fn save_blocker(&self) -> Option<String> {
        self.saved.iter().enumerate().find_map(|(i, e)| {
            let name = if e.entry.description.is_empty() {
                format!("row {}", i + 1)
            } else {
                format!("\"{}\"", e.entry.description)
            };
            if e.parse_error.is_some() {
                Some(format!("Fix the address expression of {name} first."))
            } else if !cheat_table::savable_value(e.entry.ty, &e.entry.value) {
                Some(format!(
                    "The value of {name} is not a valid {}.",
                    e.entry.ty.label()
                ))
            } else {
                None
            }
        })
    }

    /// Write the table to `path`, with `process` = the attached process.
    fn save_table(&mut self, path: PathBuf) {
        if let Some(why) = self.save_blocker() {
            self.status = format!("Not saved: {why}");
            return;
        }
        let process = self
            .attached
            .as_ref()
            .map(|(_, name)| name.clone())
            .or_else(|| self.table_process.clone());
        let t = CheatTable {
            process: process.clone(),
            entries: self.saved.iter().map(|e| e.entry.clone()).collect(),
        };
        match table::save(&t, &path) {
            Ok(()) => {
                self.status = format!("Saved {} cheats to {}.", t.entries.len(), path.display());
                self.table_process = process;
                self.table_path = Some(path);
                self.saved_mark.mark(&self.saved);
            }
            Err(e) => self.status = format!("Could not save {}: {e}", path.display()),
        }
    }

    /// Open the file dialog for `purpose`.
    fn open_file_dialog(&mut self, purpose: DialogPurpose) {
        let home = home_dir();
        let start = file_dialog::start_dir(self.last_table_dir.as_deref(), &home);
        if file_dialog::uses_native() {
            let outcome = match purpose {
                DialogPurpose::OpenTable => file_dialog::native(
                    file_dialog::Mode::Open,
                    "Open cheat table",
                    TABLE_EXT,
                    &start,
                    "",
                ),
                DialogPurpose::SaveTable => {
                    if let Some(why) = self.save_blocker() {
                        self.status = format!("Not saved: {why}");
                        self.after_save = None;
                        return;
                    }
                    file_dialog::native(
                        file_dialog::Mode::Save,
                        "Save cheat table",
                        TABLE_EXT,
                        &start,
                        &self.table_file_name(),
                    )
                }
            };
            let dir = match &outcome {
                file_dialog::Outcome::Picked(p) => {
                    p.parent().map_or(start.clone(), Path::to_path_buf)
                }
                file_dialog::Outcome::Cancelled => start,
            };
            // Handled where the in-app dialog's answer is.
            self.native_done = Some((purpose, outcome, dir));
            return;
        }
        let dialog = match purpose {
            DialogPurpose::OpenTable => FileDialog::new(
                file_dialog::Mode::Open,
                "Open cheat table",
                TABLE_EXT,
                start,
                home,
                "",
            ),
            DialogPurpose::SaveTable => {
                if let Some(why) = self.save_blocker() {
                    self.status = format!("Not saved: {why}");
                    // Nothing to quit or open after: no save is coming.
                    self.after_save = None;
                    return;
                }
                let name = self.table_file_name();
                FileDialog::new(
                    file_dialog::Mode::Save,
                    "Save cheat table",
                    TABLE_EXT,
                    start,
                    home,
                    &name,
                )
            }
        };
        self.file_dialog = Some((purpose, dialog));
    }

    /// Replace the table with the file's. All freezes start off and target
    /// memory is not touched; the entries resolve on the next frame.
    fn open_table_file(&mut self, path: PathBuf) {
        let t = match cheat_table::open_table(&path) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("Could not open {}: {e}", path.display());
                return;
            }
        };
        let n = t.entries.len();
        self.saved = t.entries;
        self.table_path = Some(path.clone());
        self.saved_mark.mark(&self.saved);
        self.table_process = t.process.clone();
        self.attach_offer = None;
        let mut status = format!(
            "Opened {n} cheats from {} — all freezes off.",
            path.display()
        );
        match (&t.process, &self.attached) {
            (Some(want), None) => {
                let procs = list_processes();
                match cheat_table::processes_named(&procs, want).as_slice() {
                    [] => status.push_str(&format!(" {want} is not running.")),
                    [pid] => self.attach_offer = Some((*pid, want.clone())),
                    _ => {
                        // Several: let the user pick, filtered to the name.
                        self.procs = procs;
                        self.list.filter = want.clone();
                        self.show_picker = true;
                    }
                }
            }
            (Some(want), Some((_, have))) if want != have => {
                status = format!(
                    "Warning: this table was made for {want}, but {have} is attached. {status}"
                );
            }
            _ => {}
        }
        self.status = status;
    }
}

impl CheatuApp {
    /// Carry out what "Save changes?" was asked for. Returns the file dialog
    /// to open, if any.
    fn go_ahead(&mut self, ctx: &eframe::egui::Context, p: Pending) -> Option<DialogPurpose> {
        match p {
            Pending::Quit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
                None
            }
            Pending::Open => Some(DialogPurpose::OpenTable),
            Pending::Elevate(pid) => {
                self.elevate_now = Some(pid);
                None
            }
        }
    }

    /// Remove every cheat (their freezes lift with them: `sync_freezes`).
    fn clear_cheats(&mut self) {
        self.saved.clear();
        self.table_path = None;
        self.table_process = None;
        // Removed on purpose: nothing is left to save.
        self.saved_mark = my_cheats::Saved::default();
        self.status = "Removed all cheats.".into();
    }

    /// The guided steps followed the game to a restart while a search ran:
    /// follow it now that the search is done.
    fn apply_pending_follow(&mut self) {
        if let Some(pid) = self.pending_follow.take() {
            self.attach_restarted(pid);
            self.wizard_for = Some(pid);
        }
    }

    /// Leave the RPG Maker screen opened without a game.
    fn back_to_game_list(&mut self) {
        self.rpg_setup = false;
    }

    /// Follow the guided steps to the same game after it restarted: keep what
    /// the user chose for it.
    fn attach_restarted(&mut self, pid: i32) {
        self.attach_with(pid, true);
    }

    /// "Ask for them" relaunches cheatu, which would drop unsaved cheats:
    /// ask first. Returns whether to relaunch now.
    fn request_elevate(&mut self, pid: Option<i32>) -> bool {
        if self.saved_mark.is_dirty(&self.saved) {
            self.confirm = Some(Confirm::Unsaved(Pending::Elevate(pid)));
            false
        } else {
            true
        }
    }

    /// The preferences as they are now, for saving.
    fn settings(&self) -> settings::Settings {
        settings::Settings {
            theme: self.theme,
            advanced: self.advanced,
        }
    }

    /// Apply the chosen theme, or the desktop's while following it, when it
    /// differs from what is shown.
    fn apply_current_theme(&mut self, ctx: &eframe::egui::Context) {
        let system = self.system_theme.as_ref().and_then(SystemTheme::get);
        let want = self.theme.resolve(system);
        if self.applied_theme != Some(want) {
            apply_theme(ctx, want);
            self.applied_theme = Some(want);
        }
    }
}

impl eframe::App for CheatuApp {
    /// Never leave the target stopped: a scan, pointer scan or wizard scan
    /// may have it suspended, and exiting kills the worker before its guard
    /// can resume it.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        pointer_window::resume_if_paused(&self.scan_paused, cheatu_core::resume);
        if let Some(w) = &mut self.pointer_window {
            w.shutdown();
        }
        if let Some(mut w) = self.wizard.take() {
            w.shutdown();
        }
    }

    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        use eframe::egui;

        self.apply_current_theme(ctx);

        // Quitting with unsaved cheats asks first.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            let dirty = self.saved_mark.is_dirty(&self.saved);
            if my_cheats::close_decision(dirty, None) == CloseAction::Ask {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.confirm = Some(Confirm::Unsaved(Pending::Quit));
            }
        }
        self.ensure_freeze_thread();
        self.poll_scan();
        if !self.scanning {
            self.apply_pending_follow();
        }
        // Notes next to controls would float over others after a scroll.
        if ctx.input(|i| i.smooth_scroll_delta != egui::Vec2::ZERO) {
            self.feedback.scrolled();
        }
        self.poll_probe();
        self.resolve_entries();
        self.poll_signature();
        self.sync_freezes();

        // Re-attach to the target we were relaunched (elevated) for.
        if let Some(pid) = self.pending_attach.take() {
            self.attach(pid);
        }

        // Deferred actions, so UI closures never need to call &mut self methods.
        let mut do_attach: Option<i32> = None;
        // The guided steps followed the game to its restarted process.
        let mut do_follow: Option<i32> = None;
        let mut do_first = false;
        let mut do_next = false;
        let mut do_reset = false;
        let mut do_undo = false;
        let mut do_elevate = false;
        // Some(pid) to elevate-then-attach; None for a plain elevation request.
        let mut elevate_for: Option<i32> = None;
        let mut do_refresh_procs = false;
        let mut add_to_table: Vec<(u64, ScanType, egui::Rect)> = Vec::new();
        let mut remove_from_table: Vec<usize> = Vec::new();
        let mut clear_table = false;
        let mut open_dialog: Option<DialogPurpose> = None;
        // Open was asked for; it may first need "Save changes?".
        let mut want_open = false;
        // An address expression was changed this frame.
        let mut expr_edited = false;
        let mut quick_save = false;
        // Index of the cheat table row whose value should be written once.
        let mut do_set: Option<(usize, egui::Rect)> = None;
        // Row to make an AoB signature for / to pointer scan.
        let mut do_signature: Option<(usize, Option<egui::Rect>)> = None;
        let mut do_ptr_scan: Option<usize> = None;
        // Results column header the user clicked, applied once the table has
        // let go of `self.display`.
        let mut new_res_sort: Option<ResSort> = None;
        // The user asked to probe the whole visible list, and confirmed it.
        let mut ask_probe_all = false;
        let mut probe_all = false;
        // (addr, size) of every candidate to probe this frame.
        let mut do_probe: Vec<(u64, usize)> = Vec::new();

        // ---- Header -----------------------------------------------------
        let screen = screen::screen(&self.screen_state());
        // The guided steps' wizard lives only while its screen is shown;
        // leaving it (Advanced on, detach, another game) stops its work and
        // never leaves the game paused (`WizardWindow::drop`).
        let attached_pid = self.attached.as_ref().map(|(pid, _)| *pid);
        if !screen::wizard_runs(screen)
            || (self.wizard.is_some() && self.wizard_for != attached_pid)
        {
            self.wizard = None;
        } else if self.wizard.is_none() {
            if let Some((pid, name)) = self.attached.clone() {
                match WizardWindow::new(pid, name) {
                    Ok(w) => {
                        self.wizard = Some(w);
                        self.wizard_for = Some(pid);
                    }
                    Err(e) => self.status = format!("Cannot start the guided steps: {e}."),
                }
            }
        }
        let mut do_detach = false;
        let mut advanced_changed = false;
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("cheatu");
                if self.attached.is_some() {
                    ui.separator();
                    ui.menu_button(format!("{} ⏷", self.game_title()), |ui| {
                        if ui.button("Change game…").clicked() {
                            do_detach = true;
                            ui.close_menu();
                        }
                        if ui.button("Detach").clicked() {
                            do_detach = true;
                            ui.close_menu();
                        }
                        if self.attached_rpg.is_some() {
                            ui.separator();
                            if ui
                                .radio(self.rpg_pick == RpgPick::RpgMaker, "RPG Maker mode")
                                .clicked()
                            {
                                self.rpg_pick = RpgPick::RpgMaker;
                                ui.close_menu();
                            }
                            if ui
                                .radio(self.rpg_pick == RpgPick::Memory, "Memory scanner")
                                .clicked()
                            {
                                self.rpg_pick = RpgPick::Memory;
                                ui.close_menu();
                            }
                        }
                    });
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("ℹ").on_hover_text("About cheatu").clicked() {
                        self.show_about = true;
                    }
                    if ui.button("⚙").on_hover_text("Settings").clicked() {
                        self.show_settings = true;
                    }
                    ui.separator();
                    if ui
                        .checkbox(&mut self.advanced, "Advanced")
                        .on_hover_text(
                            "Show the full scanner and every tool; untick for the guided steps",
                        )
                        .changed()
                    {
                        advanced_changed = true;
                    }
                });
            });
            ui.add_space(4.0);
        });
        if do_detach {
            self.detach();
        }

        // ---- Status bar -------------------------------------------------
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.scanning {
                    ui.spinner();
                }
                ui.label(&self.status);
                // A first scan over a multi-gigabyte process takes long enough
                // that a spinner with no way out is its own bug.
                if let (true, Some(p)) = (self.scanning, &self.scan_progress) {
                    if let Some(f) = p.fraction() {
                        ui.add(
                            egui::ProgressBar::new(f)
                                .desired_width(120.0)
                                .show_percentage(),
                        );
                    }
                    if ui
                        .button("Cancel")
                        .on_hover_text("Stop the scan and keep the results you had")
                        .clicked()
                    {
                        p.cancel();
                    }
                }
                // Same for a batch probe: up to PROBE_BATCH_MAX × PROBE_WAIT is
                // long enough to want out of.
                if self.probe_pending.is_some() && !self.probe_batch.is_empty() {
                    ui.spinner();
                    ui.add(
                        egui::ProgressBar::new(
                            self.probe_done as f32 / self.probe_batch.len() as f32,
                        )
                        .desired_width(120.0)
                        .show_percentage(),
                    );
                    if ui
                        .button("Cancel")
                        .on_hover_text(
                            "Stop after the address being probed now — it is restored first",
                        )
                        .clicked()
                    {
                        self.probe_cancel.store(true, Ordering::Relaxed);
                    }
                }
                // Making a signature reads all of the game's memory.
                if let Some(p) = &self.sig_progress {
                    ui.spinner();
                    ui.label("Making a signature…");
                    ui.add(
                        egui::ProgressBar::new(p.fraction().unwrap_or(0.0))
                            .desired_width(120.0)
                            .show_percentage(),
                    );
                    if ui
                        .button("Cancel")
                        .on_hover_text("Stop; the cheat stays as it is")
                        .clicked()
                    {
                        p.cancel();
                    }
                }
            });
        });

        // ---- My cheats (right) ------------------------------------------
        let mut cheat_actions = Vec::new();
        if matches!(screen, Screen::Guided | Screen::Scanner) {
            let max_width =
                (ctx.screen_rect().width() - RESULTS_MIN_WIDTH).max(CHEAT_TABLE_MIN_WIDTH);
            let dirty = self.saved_mark.is_dirty(&self.saved);
            let cx = my_cheats::Ctx {
                advanced: self.advanced,
                attached: self.attached.is_some(),
                can_read: self.attached.is_some() && self.live.is_some(),
                aob_busy: self.aob_pending.is_some(),
                sig_busy: self.sig_pending.is_some(),
                sig_row: self.sig_row,
                live_error: self.live_error.as_deref(),
                scanner: self.scanner.as_ref(),
                path: self.table_path.as_deref(),
                dirty,
            };
            // Narrow while empty, so the main area gets the room.
            let default_width = if self.saved.is_empty() { 300.0 } else { 460.0 };
            egui::SidePanel::right("cheat_table")
                .resizable(true)
                .default_width(default_width)
                .width_range(CHEAT_TABLE_MIN_WIDTH..=max_width)
                .show(ctx, |ui| {
                    cheat_actions = my_cheats::panel(ui, &mut self.cheats_ui, &mut self.saved, &cx);
                });
        }
        for action in cheat_actions {
            use my_cheats::Action;
            match action {
                Action::Open => want_open = true,
                Action::Save => quick_save = true,
                Action::SaveAs => open_dialog = Some(DialogPurpose::SaveTable),
                Action::ClearAll => self.confirm = Some(Confirm::ClearAll),
                Action::Set(i, at) => do_set = Some((i, at)),
                Action::Remove(i) => {
                    if let Some(e) = self.saved.get(i) {
                        self.confirm = Some(Confirm::Remove(e.id));
                    }
                }
                Action::PointerScan(i) => do_ptr_scan = Some(i),
                Action::Signature(i, at) => do_signature = Some((i, Some(at))),
                Action::MakeLasting(i, at) => {
                    let addr = self.saved.get(i).and_then(|e| e.resolved.clone().ok());
                    let regions = self.live.as_ref().map(|l| l.regions());
                    if let (Some(addr), Some(regions)) = (addr, regions) {
                        match my_cheats::make_lasting_tool(regions, addr) {
                            my_cheats::MakeLasting::Signature => do_signature = Some((i, Some(at))),
                            my_cheats::MakeLasting::PointerScan => do_ptr_scan = Some(i),
                        }
                    }
                }
                Action::ChangeType(i, ty) => {
                    if let Some(e) = self.saved.get_mut(i) {
                        e.entry.ty = ty;
                        e.needs_resolve = true;
                    }
                }
                Action::ExprEdited => expr_edited = true,
            }
        }

        // ---- Central panel: scanner controls, or RPG Maker mode --------
        // Why a scan can't run right now, computed before the closure: asking
        // `self` inside it would borrow the whole struct while the widgets hold
        // `&mut self.value_text`.
        let has_scanned = self.scanner.as_ref().is_some_and(|s| s.has_scanned());
        let first_block = self.scan_blocker(true);
        let next_block = self.scan_blocker(false);
        egui::CentralPanel::default().show(ctx, |ui| {
            // Admin rights, wherever the attach was refused.
            if let Some(pid) = self.needs_elevation {
                if !privilege::is_root() && privilege::elevation_available() {
                    ui.horizontal(|ui| {
                        ui.label("This game needs admin rights.");
                        if ui.button("Ask for them").clicked() && self.request_elevate(Some(pid)) {
                            do_elevate = true;
                            elevate_for = Some(pid);
                        }
                    });
                    ui.separator();
                }
            }
            match screen {
                Screen::RpgMaker => {
                    if self.attached.is_none() && ui.link("← Back to the game list").clicked() {
                        self.back_to_game_list();
                    }
                    self.rpg.ui(ui);
                    return;
                }
                Screen::ChooseGame => {
                    // Keep the list current while it is on screen.
                    if self
                        .procs_refreshed
                        .is_none_or(|t| t.elapsed() >= PROC_REFRESH)
                    {
                        do_refresh_procs = true;
                    }
                    match start_screen::start_screen(
                        ui,
                        &mut self.list,
                        &self.procs,
                        &self.game_rows,
                        self.advanced,
                    ) {
                        Some(ListAction::Attach(pid)) => do_attach = Some(pid),
                        Some(ListAction::Refresh) => do_refresh_procs = true,
                        Some(ListAction::OpenTable) => want_open = true,
                        Some(ListAction::RpgSetup) => self.rpg_setup = true,
                        None => {}
                    }
                    return;
                }
                Screen::RpgChoice => {
                    let name = self.game_title();
                    if let Some(engine) = self.attached_rpg {
                        if let Some(pick) = start_screen::rpg_choice(ui, &name, engine) {
                            self.rpg_pick = pick;
                        }
                    }
                    return;
                }
                Screen::Guided => {
                    let Some(mut w) = self.wizard.take() else {
                        return;
                    };
                    start_screen::step_bar(ui, w.step());
                    ui.add_space(6.0);
                    w.set_pause(self.pause_while_scanning);
                    let events = egui::ScrollArea::vertical()
                        .show(ui, |ui| w.show_in(ui))
                        .inner;
                    for event in events {
                        match event {
                            WizardEvent::Save(entry) => {
                                let attached = self.attached.as_ref().map(|(pid, _)| *pid);
                                match cheat_table::wizard_entry(entry, w.pid(), attached) {
                                    Ok((entry, status)) => {
                                        self.status = status;
                                        self.saved.push(GuiEntry::new(entry, false));
                                    }
                                    Err(e) => self.status = e,
                                }
                            }
                            // Follow the guided steps to the restarted game, so
                            // My cheats re-resolve there.
                            WizardEvent::Reattached(pid) if self.scanning => {
                                self.pending_follow = Some(pid);
                                self.status = "The game restarted; cheatu follows it once the \
                                               search is done."
                                    .into();
                            }
                            WizardEvent::Reattached(pid) => {
                                do_follow = Some(pid);
                                self.wizard_for = Some(pid);
                            }
                            WizardEvent::ScanManually { unknown } => {
                                self.advanced = true;
                                advanced_changed = true;
                                if unknown {
                                    self.unknown_initial = true;
                                }
                            }
                        }
                    }
                    self.wizard = Some(w);
                    return;
                }
                Screen::Scanner => {}
            }
            let attached = self.scanner.is_some();
            let busy = self.scanning;

            ui.add_enabled_ui(attached && !busy, |ui| {
                egui::Grid::new("scan_controls")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Value type");
                        ui.horizontal(|ui| {
                            let selected_text = match self.type_sel {
                                TypeSel::Any => "Any (unknown type)".to_string(),
                                TypeSel::One(t) => t.friendly_label(),
                                TypeSel::Aob => "Array of Bytes".to_string(),
                                TypeSel::Str => "String".to_string(),
                            };
                            egui::ComboBox::from_id_salt("scan_type")
                                .selected_text(selected_text)
                                .width(140.0)
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
                                    for t in ScanType::SIZE_GROUPS {
                                        // Keep the current sign when just switching size.
                                        let signed = match self.type_sel {
                                            TypeSel::One(cur) if cur.with_sign(true) == t => {
                                                cur.is_signed()
                                            }
                                            _ => true,
                                        };
                                        ui.selectable_value(
                                            &mut self.type_sel,
                                            TypeSel::One(t.with_sign(signed)),
                                            t.friendly_label(),
                                        );
                                    }
                                    ui.separator();
                                    ui.selectable_value(
                                        &mut self.type_sel,
                                        TypeSel::Aob,
                                        "Array of Bytes",
                                    );
                                    ui.selectable_value(&mut self.type_sel, TypeSel::Str, "String");
                                });

                            if let TypeSel::One(t) = self.type_sel {
                                if !t.is_float() {
                                    let mut signed = t.is_signed();
                                    if ui.checkbox(&mut signed, "Signed").changed() {
                                        self.type_sel = TypeSel::One(t.with_sign(signed));
                                    }
                                }
                            }
                        });
                        ui.end_row();

                        ui.label("Value");
                        ui.horizontal(|ui| {
                            let hint = match self.type_sel {
                                TypeSel::Aob => "hex bytes, wildcards as ??, e.g. 48 65 ?? 6C 6F",
                                TypeSel::Str => "the text you see in-game, e.g. Player1",
                                _ => "e.g. 100, or a range 6400000..6500000",
                            };
                            // An unknown-value first scan ignores the value —
                            // but once it has run, this field is the next-scan
                            // operand again, so only lock it until then.
                            let needs_value = !self.unknown_initial || has_scanned;
                            let resp = ui
                                .add_enabled(
                                    needs_value,
                                    egui::TextEdit::singleline(&mut self.value_text)
                                        .hint_text(hint)
                                        .desired_width(200.0),
                                )
                                .on_disabled_hover_text(
                                    "Not used — an unknown-value scan keeps every address.",
                                );
                            // Enter runs the scan the buttons would. The loop is
                            // scan, change the value in-game, scan again — a trip
                            // to the mouse every iteration is the friction.
                            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                if has_scanned {
                                    do_next = next_block.is_none();
                                } else {
                                    do_first = first_block.is_none();
                                }
                                // Enter drops focus, so take it back — otherwise
                                // every scan after the first needs the mouse again.
                                resp.request_focus();
                            }
                            resp.on_hover_text(
                                "The value to search for. If you can only bracket it — gold is \
                                 somewhere over 6.4M — enter a range like 6400000..6500000 and \
                                 the first scan keeps everything inside it.",
                            );
                            // Unknown-initial-value needs a numeric type — with
                            // "Any" every address is stored once per type, which
                            // only fits for modest processes.
                            ui.add_enabled(
                                !matches!(self.type_sel, TypeSel::Aob | TypeSel::Str),
                                egui::Checkbox::new(
                                    &mut self.unknown_initial,
                                    "Unknown initial value",
                                ),
                            )
                            .on_disabled_hover_text(
                                "Pick a numeric type to scan for an unknown value.",
                            );
                        });
                        ui.end_row();

                        if show_narrow(has_scanned) {
                            ui.label("Compare");
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt("next_mode")
                                    .selected_text(self.next_mode.label())
                                    .show_ui(ui, |ui| {
                                        for m in NextMode::ALL {
                                            ui.selectable_value(&mut self.next_mode, m, m.label());
                                        }
                                    });
                                if !self.next_mode.needs_value() {
                                    ui.label(
                                        egui::RichText::new("ignores the value box").weak().small(),
                                    );
                                }
                            });
                            ui.end_row();
                        }
                    });

                ui.add_space(6.0);
                let narrow = show_narrow(has_scanned);
                ui.horizontal(|ui| {
                    // Disabled with the reason shown below, rather than letting
                    // the click through to a rejection in the status bar.
                    if ui
                        .add_enabled(first_block.is_none(), egui::Button::new("🔍 Search"))
                        .on_hover_text("Search the whole game for this value")
                        .clicked()
                    {
                        do_first = true;
                    }
                    if narrow {
                        if ui
                            .add_enabled(next_block.is_none(), egui::Button::new("Narrow down"))
                            .on_hover_text("Keep the results that match the comparison")
                            .clicked()
                        {
                            do_next = true;
                        }
                        if ui
                            .button("New search")
                            .on_hover_text("Clear the results")
                            .clicked()
                        {
                            do_reset = true;
                        }
                        let can_undo =
                            !self.scanning && self.scanner.as_ref().is_some_and(|s| s.can_undo());
                        if ui
                            .add_enabled(can_undo, egui::Button::new("↩ Undo"))
                            .on_hover_text("Restore the results from before the last step")
                            .clicked()
                        {
                            do_undo = true;
                        }
                    }
                });
                // Why a button is disabled, in plain sight.
                let reason = if narrow { next_block } else { first_block };
                if let Some(r) = reason.filter(|_| !busy) {
                    ui.label(egui::RichText::new(r).small().weak());
                }

                ui.add_space(4.0);
                ui.checkbox(
                    &mut self.pause_while_scanning,
                    "Pause the game while searching",
                )
                .on_hover_text(
                    "Suspend the game (SIGSTOP) during a search so values hold still \
                         and reads are consistent. It resumes right after.",
                );
            });

            if !attached {
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Attach to a game to start searching.").weak());
            }

            ui.separator();
            // Sorting an arbitrary first-2000 slice of a million candidates
            // would be theatre, so the headers only sort once the whole set is
            // on screen — which is also the point where triage starts.
            let can_sort = self.result_count <= self.display.len();
            ui.horizontal(|ui| {
                ui.strong(format!("{} results", self.result_count));
                if !can_sort {
                    ui.label(
                        egui::RichText::new(format!(
                            "— showing the first {}. Narrow further to see and sort them all.",
                            self.display.len()
                        ))
                        .weak()
                        .small(),
                    );
                }
                // Probing one row at a time is fine for three candidates and
                // tedious for thirty, which is exactly when you need it.
                if !self.display.is_empty() {
                    let n = self.display.len().min(PROBE_BATCH_MAX);
                    if ui
                        .add_enabled(
                            n > 0 && !self.scanning && self.probe_pending.is_none(),
                            egui::Button::new(format!("🧪 Probe {n}")).small(),
                        )
                        .on_hover_text(
                            "Write a test value to each of these addresses in turn and see \
                             which ones the game overwrites. Writes to live game memory.",
                        )
                        .clicked()
                    {
                        ask_probe_all = true;
                    }
                }
            });

            // Results table (virtualized). Each row carries its own type, so
            // "Any" scans show a mix of i32/f32/… hits with the right decoding.
            use egui_extras::{Column, TableBuilder};
            let scanner_ref = self.scanner.as_ref();
            // Hidden for huge lists and AoB/string scans; see `rebuild_display`.
            let show_hints = self.show_hints;
            // Auto-probe state, hoisted so the row closure needn't borrow &mut self.
            // The scanner (Advanced) always offers the probe.
            let enable_probe = true;
            let probe_busy = self.scanning || self.probe_pending.is_some();
            let probe_results = &self.probe_results;
            let saved = &self.saved;
            let res_sort = self.res_sort;
            let mut table = TableBuilder::new(ui)
                .striped(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                // Fixed widths (not `auto`) so a constantly-changing float in
                // "Current" can't reflow the row and make the + button jump.
                // `clip` truncates over-long values instead of widening the cell.
                // Kept narrow so they plus the "+" column fit in
                // RESULTS_MIN_WIDTH; wider ones squeezed the "+" out of sight.
                .column(Column::exact(150.0))
                .column(Column::exact(72.0).clip(true))
                .column(Column::exact(96.0).clip(true))
                .column(Column::exact(96.0).clip(true));
            if show_hints {
                table = table.column(Column::exact(104.0).clip(true));
            }
            table
                .column(Column::remainder().at_least(RESULT_ACTIONS_MIN_WIDTH))
                .header(20.0, |mut header| {
                    // A sortable header, or a plain one while the list is
                    // truncated, explaining why it won't sort.
                    let mut sort_col = |header: &mut egui_extras::TableRow, col, text| {
                        header.col(|ui| {
                            if !can_sort {
                                ui.strong(text).on_hover_text(
                                    "Narrow the results until they all fit to sort them.",
                                );
                            } else if sort_header(ui, text, res_sort == col).clicked() {
                                new_res_sort = Some(col);
                            }
                        });
                    };
                    sort_col(&mut header, ResSort::Address, "Address");
                    sort_col(&mut header, ResSort::Type, "Type");
                    sort_col(&mut header, ResSort::Previous, "Previous");
                    header.col(|ui| {
                        // Not sortable: it's read live, so any order would be
                        // stale by the next frame.
                        ui.strong("Current");
                    });
                    if show_hints {
                        header.col(|ui| {
                            let resp = if !can_sort {
                                ui.strong("Region")
                            } else {
                                let r = sort_header(ui, "Region", res_sort == ResSort::Region);
                                if r.clicked() {
                                    new_res_sort = Some(ResSort::Region);
                                }
                                r
                            };
                            resp.on_hover_text(
                                "Which kind of memory the address lives in — a weak guess at \
                                 whether it's the real value. It can't tell apart two \
                                 candidates in the same region; the probe (🧪) can.",
                            );
                        });
                    }
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
                            ui.label(egui::RichText::new(ty.friendly_label()).weak());
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
                                        text = text.color(err_color(ui));
                                    }
                                    ui.label(text);
                                }
                                None => {
                                    ui.monospace("n/a");
                                }
                            }
                        });
                        if show_hints {
                            row.col(|ui| match &d.hint {
                                Some(h) => {
                                    // Filled ⚫ for a likely-real value, hollow ⚪
                                    // (weak) otherwise, matching the mockup.
                                    let text = if h.confidence == Confidence::Likely {
                                        egui::RichText::new(format!("⚫ {}", h.label))
                                            .small()
                                            .color(egui::Color32::from_rgb(0x4c, 0xaf, 0x50))
                                    } else {
                                        egui::RichText::new(format!("⚪ {}", h.label))
                                            .small()
                                            .weak()
                                    };
                                    ui.label(text).on_hover_text(hint_tooltip(h));
                                }
                                None => {
                                    ui.label(egui::RichText::new("—").weak());
                                }
                            });
                        }
                        row.col(|ui| {
                            let plus = ui.button("+").on_hover_text("Add to My cheats");
                            if plus.clicked() {
                                add_to_table.push((d.addr, ty, plus.rect));
                            }
                            if enable_probe {
                                let frozen = saved
                                    .iter()
                                    .any(|e| e.entry.frozen && e.resolved == Ok(d.addr));
                                let btn =
                                    ui.add_enabled(!probe_busy && !frozen, egui::Button::new("🧪"));
                                let btn = if frozen {
                                    btn.on_disabled_hover_text("Unfreeze to probe")
                                } else {
                                    btn.on_hover_text(
                                        "Probe: write a test value, wait, read back. \
                                         A live copy reverts; the real value holds. \
                                         Writes to live game memory.",
                                    )
                                };
                                if btn.clicked() {
                                    do_probe.push((d.addr, ty.size()));
                                }
                            }
                            match probe_results.get(&d.addr) {
                                Some(ProbeOutcome::Held) => {
                                    ui.label(
                                        egui::RichText::new("✔ holds")
                                            .small()
                                            .color(egui::Color32::from_rgb(0x4c, 0xaf, 0x50)),
                                    )
                                    .on_hover_text(
                                        "Held the test value — likely the real value (or a \
                                         stale copy). Not proof.",
                                    );
                                }
                                Some(ProbeOutcome::Reverted) => {
                                    ui.label(egui::RichText::new("✖ reverted").small().weak())
                                        .on_hover_text(
                                            "The game overwrote the test value — a live copy, \
                                             probably not the source.",
                                        );
                                }
                                None => {}
                            }
                        });
                    });
                });
        });

        // ---- Process list window (a table's game runs more than once) -----
        if self.show_picker {
            let mut open = true;
            let mut close = false;
            let screen_rect = ctx.screen_rect();
            let max_h = (screen_rect.height() - 48.0).max(260.0);
            egui::Window::new("Choose the game")
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .default_size([700.0, 520.0])
                .max_height(max_h)
                .min_width(540.0)
                .show(ctx, |ui| {
                    let height = (screen_rect.height() - 240.0).clamp(140.0, 820.0);
                    match start_screen::game_list(
                        ui,
                        &mut self.list,
                        &self.procs,
                        &self.game_rows,
                        self.advanced,
                        height,
                    ) {
                        Some(ListAction::Attach(pid)) => do_attach = Some(pid),
                        Some(ListAction::Refresh) => do_refresh_procs = true,
                        _ => {}
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if ui
                                .add_enabled(
                                    self.list.selected.is_some(),
                                    egui::Button::new("Attach"),
                                )
                                .clicked()
                            {
                                do_attach = self.list.selected;
                            }
                        });
                    });
                });
            if !open || close {
                self.show_picker = false;
            }
        }

        // ---- Confirm a batch probe --------------------------------------
        // One probe is a small, reversible poke; thirty in a row is a real
        // chance to upset the game, so this one asks first.
        if self.show_probe_confirm {
            let n = self.display.len().min(PROBE_BATCH_MAX);
            egui::Window::new("Probe these addresses?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!(
                        "cheatu will write a test value to {n} address{} in the running game, \
                         one at a time, restoring each one the game doesn't overwrite.",
                        if n == 1 { "" } else { "es" }
                    ));
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            "This changes live game memory. A game that reads one of these \
                             values at the wrong moment can misbehave or crash — save first.",
                        )
                        .color(warn_color(ui)),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Takes about {:.0} seconds. Frozen rows are skipped.",
                            n as f32 * PROBE_WAIT.as_secs_f32()
                        ))
                        .weak()
                        .small(),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Probe").clicked() {
                            probe_all = true;
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_probe_confirm = false;
                        }
                    });
                });
        }
        if ask_probe_all {
            self.show_probe_confirm = true;
        }
        if probe_all {
            self.show_probe_confirm = false;
            do_probe = self.probe_all_targets();
        }

        // ---- About ------------------------------------------------------
        egui::Window::new("About cheatu")
            .open(&mut self.show_about)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add(
                        egui::Image::new(egui::include_image!(
                            "../../../packaging/assets/io.github.carroarmato0.cheatu.svg"
                        ))
                        .fit_to_exact_size(egui::vec2(96.0, 96.0)),
                    );
                    ui.add_space(4.0);
                    ui.heading("cheatu");
                    ui.label(egui::RichText::new(VERSION).weak());
                    ui.add_space(4.0);
                    ui.label("A memory scanner and cheat tool for games on Linux and macOS,\nincluding Wine/Proton and RPG Maker titles.");
                    ui.add_space(4.0);
                    ui.hyperlink("https://github.com/carroarmato0/cheatu");
                    ui.add_space(4.0);
                });
            });

        // ---- Settings ---------------------------------------------------
        let mut theme = self.theme;
        egui::Window::new("Settings")
            .open(&mut self.show_settings)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Theme:");
                    ui.selectable_value(&mut theme, ThemePref::System, "System")
                        .on_hover_text("Follow the desktop's light or dark setting");
                    ui.selectable_value(&mut theme, ThemePref::Dark, "Dark");
                    ui.selectable_value(&mut theme, ThemePref::Light, "Light");
                });
            });
        if theme != self.theme {
            self.theme = theme;
            if let Some(st) = &self.system_theme {
                st.set_following(theme == ThemePref::System);
            }
            if let Err(e) = settings::save(&self.settings()) {
                self.status = format!("Could not save the settings: {e}.");
            }
        }

        // A search started before the edit must not land on the new
        // expression (or on an A -> B -> A edit's lost re-resolve).
        if expr_edited {
            self.resolve_generation += 1;
        }

        // ---- Cheat table file dialog and attach offer --------------------
        // ---- Confirmations ----------------------------------------------
        if want_open {
            if self.saved_mark.is_dirty(&self.saved) {
                self.confirm = Some(Confirm::Unsaved(Pending::Open));
            } else {
                open_dialog = Some(DialogPurpose::OpenTable);
            }
        }
        if let Some(c) = self.confirm {
            let file = self
                .table_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned());
            let mut answer: Option<Answer> = None;
            let mut yes = false;
            let mut no = false;
            let title = match c {
                Confirm::Unsaved(_) => "Unsaved changes",
                Confirm::Remove(_) | Confirm::ClearAll => "Remove",
            };
            egui::Window::new(title)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| match c {
                    Confirm::Unsaved(_) => {
                        ui.label(match &file {
                            Some(f) => format!("Save changes to {f}?"),
                            None => "Save your cheats?".to_string(),
                        });
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                answer = Some(Answer::Save);
                            }
                            if ui.button("Don't save").clicked() {
                                answer = Some(Answer::DontSave);
                            }
                            if ui.button("Cancel").clicked() {
                                answer = Some(Answer::Cancel);
                            }
                        });
                    }
                    Confirm::Remove(id) => {
                        let name = self
                            .saved
                            .iter()
                            .find(|e| e.id == id)
                            .map_or("this cheat", |e| e.entry.description.as_str());
                        ui.label(format!("Remove \"{name}\"?"));
                        ui.horizontal(|ui| {
                            yes = ui.button("Remove").clicked();
                            no = ui.button("Cancel").clicked();
                        });
                    }
                    Confirm::ClearAll => {
                        ui.label(format!("Remove all {} cheats?", self.saved.len()));
                        ui.horizontal(|ui| {
                            yes = ui.button("Remove all").clicked();
                            no = ui.button("Cancel").clicked();
                        });
                    }
                });
            match c {
                Confirm::Unsaved(pending) => {
                    if let Some(a) = answer {
                        self.confirm = None;
                        match my_cheats::close_decision(true, Some(a)) {
                            CloseAction::Stay | CloseAction::Ask => {}
                            CloseAction::Close => {
                                open_dialog = open_dialog.or(self.go_ahead(ctx, pending));
                            }
                            CloseAction::SaveThenClose => match self.table_path.clone() {
                                Some(path) => {
                                    self.save_table(path);
                                    // A failed save keeps everything open; the
                                    // status bar says why.
                                    if !self.saved_mark.is_dirty(&self.saved) {
                                        open_dialog = open_dialog.or(self.go_ahead(ctx, pending));
                                    }
                                }
                                None => {
                                    self.after_save = Some(pending);
                                    open_dialog = Some(DialogPurpose::SaveTable);
                                }
                            },
                        }
                    }
                }
                Confirm::Remove(id) if yes => {
                    self.confirm = None;
                    if let Some(i) = self.saved.iter().position(|e| e.id == id) {
                        remove_from_table.push(i);
                    }
                }
                Confirm::ClearAll if yes => {
                    self.confirm = None;
                    clear_table = true;
                }
                _ if no => self.confirm = None,
                _ => {}
            }
        }

        if quick_save {
            match self.table_path.clone() {
                Some(path) => self.save_table(path),
                None => open_dialog = Some(DialogPurpose::SaveTable),
            }
        }
        if let Some(purpose) = open_dialog {
            self.open_file_dialog(purpose);
        }
        // The in-app dialog's answer, or the native panel's (macOS).
        let mut done = None;
        if let Some((purpose, dialog)) = &mut self.file_dialog {
            if let Some(outcome) = dialog.show(ctx) {
                done = Some((*purpose, outcome, dialog.dir().to_path_buf()));
            }
        }
        if done.is_some() {
            self.file_dialog = None;
        }
        if let Some((purpose, outcome, dir)) = done.or_else(|| self.native_done.take()) {
            self.last_table_dir = Some(dir);
            if let file_dialog::Outcome::Picked(path) = outcome {
                match purpose {
                    DialogPurpose::OpenTable => self.open_table_file(path),
                    DialogPurpose::SaveTable => {
                        self.save_table(path);
                        // Saved on the way to quitting or opening.
                        if let Some(p) = self.after_save.take() {
                            if !self.saved_mark.is_dirty(&self.saved) {
                                if let Some(next) = self.go_ahead(ctx, p) {
                                    self.open_file_dialog(next);
                                }
                            }
                        }
                    }
                }
            } else {
                self.after_save = None;
            }
        }
        // A native panel opened just now answers on the next frame.
        if self.native_done.is_some() {
            ctx.request_repaint();
        }
        // ---- Pointer scan window -----------------------------------------
        if let Some(i) = do_ptr_scan {
            self.open_pointer_window(i);
        }
        if let Some(mut w) = self.pointer_window.take() {
            w.pause_target = self.pause_while_scanning;
            // "Filter by value" reads the scanner's value type; with no
            // single type selected, the scanned row's.
            if let TypeSel::One(t) = self.type_sel {
                w.value_ty = t;
            } else if let Some(t) = w.entry_type() {
                w.value_ty = t;
            }
            self.fresh_live();
            if let Some(entry) = w.show(ctx, self.live.as_ref()) {
                self.status = format!("Added {} to the cheat table.", entry.address);
                self.saved.push(GuiEntry::new(entry, false));
            }
            if w.is_open() {
                self.pointer_window = Some(w);
            }
        }

        if let Some((pid, name)) = self.attach_offer.clone() {
            let mut close = false;
            egui::Window::new("Attach to the table's game?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("These cheats are for {name}, which is running."));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Attach").clicked() {
                            do_attach = Some(pid);
                            close = true;
                        }
                        if ui.button("Not now").clicked() {
                            close = true;
                        }
                    });
                });
            if close {
                self.attach_offer = None;
            }
        }

        // ---- Apply deferred actions ------------------------------------
        if advanced_changed {
            if let Err(e) = settings::save(&self.settings()) {
                self.status = format!("Could not save the settings: {e}.");
            }
        }
        if do_refresh_procs {
            self.procs = list_processes();
            self.procs_refreshed = Some(Instant::now());
            let detections = &mut self.detections;
            detections.retain_running(&self.procs);
            self.game_rows = start_screen::fold(&self.procs, &mut |p| detections.get(p));
            // Preselect the most likely game, unless the user picked one
            // that is still running.
            let still_valid = start_screen::selection_valid(
                self.list.selected,
                &self.game_rows,
                &self.procs,
                self.advanced,
            );
            if !still_valid {
                self.list.selected = start_screen::best_guess(&self.game_rows);
            }
        }
        if let Some(pid) = do_attach {
            self.attach(pid);
            self.show_picker = false;
        }
        if let Some(pid) = do_follow {
            self.attach_restarted(pid);
        }
        // "Save changes?" answered on the way to relaunching elevated.
        if let Some(pid) = self.elevate_now.take() {
            do_elevate = true;
            elevate_for = pid;
        }
        if do_elevate {
            if privilege::elevation_available() {
                self.status = format!("Requesting elevation via {}…", privilege::elevation_tool());
                // Carry the target pid so the elevated instance re-attaches.
                let extra: Vec<String> = match elevate_for {
                    Some(pid) => vec![privilege::ATTACH_FLAG.to_string(), pid.to_string()],
                    None => Vec::new(),
                };
                // On success this replaces the process; only returns on failure.
                let err = privilege::relaunch_elevated(&extra);
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
        if do_undo {
            self.undo_scan();
        }
        for (addr, ty, at) in add_to_table {
            if cheat_table::already_saved(&self.saved, addr, ty) {
                self.feedback.ok(at, "Already in My cheats", Instant::now());
                continue;
            }
            self.feedback.ok(at, "✔ Added to My cheats", Instant::now());
            // `"module"+offset` when the address is in a module's image, so
            // the entry still works after the game restarts.
            let mut entry = match self.fresh_live() {
                Some(live) => cheat_table::entry_for_address(live.regions(), addr, ty),
                None => cheat_table::entry_for_address(&[], addr, ty),
            };
            if let Some(v) = self
                .scanner
                .as_ref()
                .and_then(|s| s.read_typed(addr, ty).ok())
            {
                entry.value = cheat_table::value_text(&v);
            }
            let mut row = GuiEntry::new(entry, false);
            row.resolved = Ok(addr);
            row.last_resolve = Instant::now();
            row.needs_resolve = false;
            self.saved.push(row);
        }
        if let Some(col) = new_res_sort.filter(|c| *c != self.res_sort) {
            self.res_sort = col;
            self.sort_display();
        }
        if clear_table {
            self.clear_cheats();
        }
        // A one-shot write. Freezing writes continuously; this is for the far
        // more common "set it once and see what happens".
        // Only ever to a resolved address.
        if let Some((entry, at)) = do_set.and_then(|(i, at)| Some((self.saved.get(i)?, at))) {
            let value = entry.entry.ty.parse(&entry.entry.value);
            let name = &entry.entry.description;
            let result = match (&self.scanner, &entry.resolved, value) {
                (None, _, _) => Err("Attach to the game first.".to_string()),
                (_, Err(e), _) => Err(format!("Not written: not found in the game ({e}).")),
                (_, _, None) => Err("Not a valid value for this kind of number.".into()),
                (Some(scanner), Ok(addr), Some(v)) => match scanner.write_value(*addr, &v) {
                    Ok(()) => Ok(format!("✔ Wrote {v} to {name}")),
                    Err(e) => Err(format!("Writing {name} failed: {e}")),
                },
            };
            let now = Instant::now();
            match result {
                Ok(msg) => {
                    self.feedback.ok(at, msg.clone(), now);
                    self.status = msg;
                }
                Err(msg) => {
                    self.feedback.error(at, msg.clone(), now);
                    self.status = msg;
                }
            }
        }
        if let Some((i, at)) = do_signature {
            self.start_signature(i, at);
        }
        // Remove in reverse so indices stay valid.
        remove_from_table.sort_unstable();
        for i in remove_from_table.into_iter().rev() {
            if i < self.saved.len() {
                self.saved.remove(i);
            }
        }

        // Auto-probe a candidate in the background (opens its own Mem handle, like
        // the freeze thread, so it doesn't need the Scanner). One at a time.
        if !do_probe.is_empty() && self.probe_pending.is_none() {
            if let Some((pid, _)) = self.attached {
                let (tx, rx) = mpsc::channel();
                self.probe_pending = Some(rx);
                self.probe_batch = do_probe.iter().map(|(addr, _)| *addr).collect();
                self.probe_done = 0;
                self.probe_cancel.store(false, Ordering::Relaxed);
                self.status = match do_probe.as_slice() {
                    [(addr, _)] => format!("Probing 0x{addr:012x}…"),
                    batch => format!("Probing {} addresses…", batch.len()),
                };
                let cancel = Arc::clone(&self.probe_cancel);
                std::thread::spawn(move || {
                    let mem = Mem::open(pid).ok();
                    // One at a time: only ever one perturbed value in the game,
                    // and it is restored before the next is touched. Cancelling
                    // is checked between addresses, never mid-probe, so a
                    // sentinel is always cleaned up before we stop.
                    for (addr, size) in do_probe {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let outcome = mem
                            .as_ref()
                            .and_then(|m| probe_address(m, addr, size, PROBE_WAIT).ok());
                        if tx.send((addr, outcome)).is_err() {
                            break;
                        }
                    }
                });
            }
        }

        self.feedback.show(ctx, Instant::now());

        // Keep current values (scanner results, or the RPG Maker snapshot) live.
        if self.scanner.is_some()
            || self.scanning
            || self.aob_pending.is_some()
            || self.sig_pending.is_some()
            || self.pointer_window.as_ref().is_some_and(|w| w.busy())
            || self.wizard.as_ref().is_some_and(|w| w.needs_repaint())
            || screen == Screen::RpgMaker
        {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }
}

/// The comparison, Narrow down and Undo only mean something after a first
/// search.
fn show_narrow(has_scanned: bool) -> bool {
    has_scanned
}

/// A borderless, clickable column header that shows a ⬇ marker when active.
fn sort_header(ui: &mut eframe::egui::Ui, text: &str, active: bool) -> eframe::egui::Response {
    use eframe::egui;
    let label = if active {
        format!("{text} ⬇")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn own_pid() -> i32 {
        std::process::id() as i32
    }

    fn cheat(address: &str, value: &str, frozen: bool) -> GuiEntry {
        GuiEntry::new(
            cheatu_core::table::TableEntry {
                description: "Gold".into(),
                address: cheatu_core::AddrExpr::parse(address).unwrap(),
                ty: ScanType::I32,
                value: value.into(),
                frozen,
            },
            false,
        )
    }

    /// C1: the RPG Maker screen reached without a game must lead back.
    #[test]
    fn back_from_rpg_setup_returns_to_game_list() {
        let mut app = CheatuApp {
            rpg_setup: true,
            ..Default::default()
        };
        assert_eq!(screen::screen(&app.screen_state()), Screen::RpgMaker);
        app.back_to_game_list();
        assert_eq!(screen::screen(&app.screen_state()), Screen::ChooseGame);
    }

    /// I1: following a restarted RPG Maker game keeps the user's choice.
    #[test]
    fn following_a_restart_keeps_the_rpg_choice() {
        let mut app = CheatuApp {
            attached: Some((1, "Game".into())),
            rpg_pick: RpgPick::Memory,
            attached_rpg: Some(cheatu_inject::Engine::RpgMakerMZ),
            ..Default::default()
        };
        app.attach_restarted(own_pid());
        assert_eq!(app.rpg_pick, RpgPick::Memory);
        assert_eq!(app.attached_rpg, Some(cheatu_inject::Engine::RpgMakerMZ));
        assert_eq!(screen::screen(&app.screen_state()), Screen::Guided);
    }

    /// I2: a search that finishes after Detach must not bring the old game back.
    #[test]
    fn stale_scan_result_is_dropped() {
        let (tx, rx) = mpsc::channel();
        let mut app = CheatuApp {
            pending: Some(rx),
            scanning: true,
            ..Default::default()
        };
        tx.send(ScanOutcome {
            scanner: Scanner::new(own_pid()).unwrap(),
            message: "1 result.".into(),
        })
        .unwrap();
        app.poll_scan();
        assert!(app.scanner.is_none(), "not attached to that game any more");
        assert!(!app.scanning);
    }

    /// I2: Detach stops a probe batch.
    #[test]
    fn detach_cancels_a_probe_batch() {
        let mut app = CheatuApp {
            attached: Some((own_pid(), "x".into())),
            ..Default::default()
        };
        app.detach();
        assert!(app.probe_cancel.load(Ordering::Relaxed));
    }

    /// I3: a save that cannot start must not leave Quit armed for later.
    #[test]
    fn blocked_save_dialog_forgets_pending_quit() {
        let mut app = CheatuApp {
            saved: vec![cheat("\"game\"+0x10", "not a number", false)],
            after_save: Some(Pending::Quit),
            ..Default::default()
        };
        app.open_file_dialog(DialogPurpose::SaveTable);
        assert!(app.file_dialog.is_none());
        assert_eq!(app.after_save, None);
    }

    /// I4: "Ask for them" relaunches cheatu; unsaved cheats are asked about.
    #[test]
    fn elevating_with_unsaved_cheats_asks_first() {
        let mut app = CheatuApp {
            saved: vec![cheat("\"game\"+0x10", "1", false)],
            ..Default::default()
        };
        assert!(!app.request_elevate(Some(5)));
        assert!(matches!(
            app.confirm,
            Some(Confirm::Unsaved(Pending::Elevate(Some(5))))
        ));
        let mut clean = CheatuApp::default();
        assert!(clean.request_elevate(Some(5)));
    }

    /// M4: attaching uses what the game list detected for the row.
    #[test]
    fn attach_uses_the_rows_detection() {
        let mut app = CheatuApp {
            game_rows: vec![start_screen::GameRow {
                pid: own_pid(),
                name: "Game".into(),
                rss: 1,
                wine: false,
                engine: Some(cheatu_inject::Engine::RpgMakerMZ),
                dir: Some(PathBuf::from("/games/Sakura Quest")),
                locked: false,
                helpers: 2,
                renderer: true,
            }],
            ..Default::default()
        };
        app.attach(own_pid());
        assert_eq!(app.attached_rpg, Some(cheatu_inject::Engine::RpgMakerMZ));
        assert_eq!(app.rpg.dir, "/games/Sakura Quest");
        // M8: the title stays the detected folder while the field is edited.
        app.rpg.dir = "/somewhere/else".into();
        assert_eq!(app.game_title(), "Sakura Quest");
    }

    /// Making a signature shows its progress on the row, and Detach stops it.
    #[test]
    fn signature_shows_progress_and_detach_cancels_it() {
        let mut app = CheatuApp {
            attached: Some((own_pid(), "me".into())),
            saved: vec![cheat("0x1000", "1", false)],
            ..Default::default()
        };
        app.saved[0].resolved = Ok(0x1000);
        app.start_signature(0, None);
        let id = app.saved[0].id;
        assert_eq!(app.sig_row, Some(id));
        let progress = app.sig_progress.clone().expect("progress to show");
        app.detach();
        assert!(progress.is_cancelled(), "detach stops it");
    }

    /// M5: removing every cheat on purpose leaves nothing to save.
    #[test]
    fn remove_all_leaves_nothing_unsaved() {
        let mut app = CheatuApp {
            saved: vec![cheat("\"game\"+0x10", "1", false)],
            ..Default::default()
        };
        app.saved_mark.mark(&app.saved);
        app.clear_cheats();
        assert!(!app.saved_mark.is_dirty(&app.saved));
    }

    /// M7: a restart followed during a search is attached once it is done.
    #[test]
    fn restart_followed_after_the_search() {
        let mut app = CheatuApp {
            attached: Some((1, "Game".into())),
            pending_follow: Some(own_pid()),
            ..Default::default()
        };
        app.apply_pending_follow();
        assert_eq!(app.attached.as_ref().map(|(p, _)| *p), Some(own_pid()));
        assert_eq!(app.wizard_for, Some(own_pid()));
        assert_eq!(app.pending_follow, None);
    }

    /// I5: a plain address means nothing in another game: never freeze into it.
    #[test]
    fn attaching_unfreezes_session_only_cheats() {
        let mut app = CheatuApp {
            attached: Some((1, "old".into())),
            saved: vec![
                cheat("0x1000", "1", true),
                cheat("\"game\"+0x10", "1", true),
            ],
            ..Default::default()
        };
        app.attach(own_pid());
        assert!(!app.saved[0].entry.frozen, "session only: unfrozen");
        assert!(app.saved[1].entry.frozen, "keeps working after a restart");
    }

    fn addrs(app: &CheatuApp) -> Vec<u64> {
        app.display.iter().map(|d| d.addr).collect()
    }

    /// Each header orders by what it says, and the "Region" column puts the
    /// candidates worth looking at first.
    #[test]
    fn sorting_results_orders_by_the_clicked_column() {
        let likely = AddrHint {
            confidence: Confidence::Likely,
            label: "heap",
        };
        let unlikely = AddrHint {
            confidence: Confidence::Unlikely,
            label: "stack",
        };
        let mut app = CheatuApp {
            display: vec![
                DisplayRow {
                    addr: 0x30,
                    prev: ScanValue::I32(5),
                    hint: Some(unlikely),
                },
                DisplayRow {
                    addr: 0x10,
                    prev: ScanValue::F32(1.5),
                    hint: None,
                },
                DisplayRow {
                    addr: 0x20,
                    prev: ScanValue::I32(-3),
                    hint: Some(likely),
                },
            ],
            ..Default::default()
        };

        app.res_sort = ResSort::Address;
        app.sort_display();
        assert_eq!(addrs(&app), [0x10, 0x20, 0x30]);

        app.res_sort = ResSort::Previous;
        app.sort_display();
        assert_eq!(addrs(&app), [0x20, 0x10, 0x30], "-3 < 1.5 < 5");

        app.res_sort = ResSort::Region;
        app.sort_display();
        assert_eq!(
            addrs(&app),
            [0x20, 0x30, 0x10],
            "likely, then unlikely, then unranked"
        );

        app.res_sort = ResSort::Type;
        app.sort_display();
        let labels: Vec<String> = app
            .display
            .iter()
            .map(|d| d.prev.ty().friendly_label())
            .collect();
        let mut expected = labels.clone();
        expected.sort();
        assert_eq!(labels, expected);
    }

    /// A cancelled batch accounts only for what it actually tested; the worker
    /// dropping its sender is what marks the batch over.
    #[test]
    fn a_cancelled_probe_batch_reports_only_what_it_tested() {
        let (tx, rx) = mpsc::channel();
        let mut app = CheatuApp {
            probe_pending: Some(rx),
            probe_batch: vec![0x10, 0x20, 0x30, 0x40],
            ..Default::default()
        };
        tx.send((0x10, Some(ProbeOutcome::Held))).unwrap();
        tx.send((0x20, Some(ProbeOutcome::Reverted))).unwrap();
        app.poll_probe();
        assert_eq!(app.probe_done, 2);
        assert!(app.probe_pending.is_some(), "the batch is still running");
        assert!(app.status.contains("3 of 4"), "{}", app.status);

        // Cancelling makes the worker stop and drop its sender.
        drop(tx);
        app.poll_probe();
        assert!(app.probe_pending.is_none(), "the batch is over");
        assert!(app.status.contains("cancelled"), "{}", app.status);
        assert!(app.status.contains("1 held"), "{}", app.status);
        assert!(app.status.contains("2 of 4 untested"), "{}", app.status);
    }

    /// "Probe all" skips frozen rows and stops at the cap, so it can't write to
    /// an unbounded number of addresses in a live game.
    #[test]
    fn probe_all_skips_frozen_rows_and_stops_at_the_cap() {
        let mut app = CheatuApp {
            display: (0..PROBE_BATCH_MAX as u64 + 10)
                .map(|i| DisplayRow {
                    addr: 0x1000 + i * 4,
                    prev: ScanValue::I32(1),
                    hint: None,
                })
                .collect(),
            ..Default::default()
        };
        assert_eq!(app.probe_all_targets().len(), PROBE_BATCH_MAX);
        assert_eq!(app.probe_all_targets()[0], (0x1000, 4));

        let mut frozen = GuiEntry::new(
            cheatu_core::table::TableEntry {
                description: String::new(),
                address: cheatu_core::AddrExpr::parse("0x1000").unwrap(),
                ty: ScanType::I32,
                value: "1".into(),
                frozen: true,
            },
            false,
        );
        frozen.resolved = Ok(0x1000);
        app.saved.push(frozen);
        let targets = app.probe_all_targets();
        assert_eq!(targets.len(), PROBE_BATCH_MAX, "the cap still fills up");
        assert!(
            !targets.iter().any(|(addr, _)| *addr == 0x1000),
            "a frozen address would read as reverted whatever the game does"
        );
    }

    /// The scan buttons are enabled exactly when `start_scan` would accept the
    /// inputs — a click should never reach a rejection in the status bar.
    #[test]
    fn narrow_controls_hidden_before_first_search() {
        assert!(!show_narrow(false));
        assert!(show_narrow(true));
    }

    #[test]
    fn scan_blocker_tracks_what_the_inputs_allow() {
        let mut app = CheatuApp::default();
        assert!(app.scan_blocker(true).is_some(), "nothing attached");

        app.scanner = Some(Scanner::new(std::process::id() as i32).unwrap());
        assert!(app.scan_blocker(true).is_some(), "no value entered");
        app.value_text = "100".into();
        assert!(app.scan_blocker(true).is_none());
        app.value_text = "6400000..6500000".into();
        assert!(app.scan_blocker(true).is_none(), "a range is a first scan");

        // An unknown-value scan keeps every address, so it needs no value.
        app.value_text.clear();
        app.unknown_initial = true;
        assert!(app.scan_blocker(true).is_none());
        app.unknown_initial = false;

        // Narrowing needs a first scan to narrow...
        assert_eq!(
            app.scan_blocker(false),
            Some("Search first, then narrow down."),
            "nothing searched yet"
        );
        app.scanner
            .as_mut()
            .unwrap()
            .first_scan(FirstScan::Value {
                value: "424242".into(),
                types: vec![ScanType::I32],
            })
            .unwrap();
        // ...and a value only for the comparisons that read one.
        app.next_mode = NextMode::Changed;
        assert!(app.scan_blocker(false).is_none());
        app.next_mode = NextMode::Greater;
        assert!(
            app.scan_blocker(false).is_some(),
            "\"greater than\" needs a value"
        );
        app.value_text = "5".into();
        assert!(app.scan_blocker(false).is_none());
    }
}

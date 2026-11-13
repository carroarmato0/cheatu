//! The start screen: the list of running programs, one row per game.

use cheatu_core::human_bytes;
use cheatu_core::process::ProcInfo;
use cheatu_inject::Engine;
use eframe::egui;

use crate::{ok_color, sort_header, warn_color, wine_color};

/// Column used to sort the Advanced process list.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub enum ProcSort {
    #[default]
    Memory,
    Pid,
    Name,
}

/// The list's filter, selection and sort, kept across frames.
#[derive(Default)]
pub struct ListState {
    pub filter: String,
    pub wine_only: bool,
    /// The pid Attach uses.
    pub selected: Option<i32>,
    pub sort: ProcSort,
}

pub enum ListAction {
    Attach(i32),
    Refresh,
    OpenTable,
    RpgSetup,
}

/// One row of the basic game list: a program, with the process to attach
/// to. A multi-process (NW.js/Chromium) app is one row.
#[derive(Clone, Debug, PartialEq)]
pub struct GameRow {
    /// The process Attach uses.
    pub pid: i32,
    pub name: String,
    /// Memory of all its processes together.
    pub rss: u64,
    pub wine: bool,
    pub engine: Option<Engine>,
    /// The process cannot be read without admin rights.
    pub locked: bool,
    /// Other processes folded into this row.
    pub helpers: usize,
    /// It is a multi-process app whose `pid` is a renderer.
    pub renderer: bool,
}

/// The program a command line starts, to group a multi-process app: the
/// outermost macOS app bundle when there is one (its helpers are apps inside
/// it), else the text before the first ` --` option.
fn program_key(cmdline: &str) -> &str {
    if let Some(i) = cmdline.find(".app/") {
        return &cmdline[..i + 4];
    }
    cmdline.split(" --").next().unwrap_or(cmdline).trim()
}

/// One row per program. Processes with a Chromium role (`--type=…` or the
/// runtime's main process) that start the same program are folded: the row
/// attaches to their largest renderer, where an NW.js game keeps its data.
pub fn fold(
    procs: &[ProcInfo],
    engines: &mut dyn FnMut(&ProcInfo) -> Option<Engine>,
) -> Vec<GameRow> {
    // Programs that are multi-process apps; their other processes (a macOS
    // app's main process has no role) join them.
    let multi: std::collections::HashSet<&str> = procs
        .iter()
        .filter(|p| p.role.is_some())
        .map(|p| program_key(&p.cmdline))
        .collect();
    let mut rows: Vec<GameRow> = Vec::new();
    // Row index per program, for the multi-process apps.
    let mut groups: Vec<(String, usize, Vec<&ProcInfo>)> = Vec::new();
    for p in procs {
        if !multi.contains(program_key(&p.cmdline)) {
            rows.push(GameRow {
                pid: p.pid,
                name: p.name.clone(),
                rss: p.rss_bytes,
                wine: p.is_wine,
                engine: engines(p),
                locked: !p.accessible,
                helpers: 0,
                renderer: false,
            });
            continue;
        }
        let key = program_key(&p.cmdline);
        match groups.iter_mut().find(|(k, _, _)| k == key) {
            Some((_, _, members)) => members.push(p),
            None => {
                groups.push((key.to_string(), rows.len(), vec![p]));
                // Placeholder, filled in below.
                rows.push(GameRow {
                    pid: p.pid,
                    name: String::new(),
                    rss: 0,
                    wine: false,
                    engine: None,
                    locked: false,
                    helpers: 0,
                    renderer: false,
                });
            }
        }
    }
    for (key, at, members) in groups {
        let renderer = members
            .iter()
            .filter(|p| p.role.as_deref() == Some("renderer"))
            .max_by_key(|p| p.rss_bytes);
        let chosen = renderer
            .or_else(|| members.iter().max_by_key(|p| p.rss_bytes))
            .expect("a group has members");
        // The app's name: its bundle, else its main process.
        let bundle = key
            .strip_suffix(".app")
            .and_then(|k| k.rsplit('/').next())
            .map(str::to_owned);
        let main = members
            .iter()
            .find(|p| p.role.as_deref() == Some("main"))
            .or_else(|| members.iter().find(|p| p.role.is_none()))
            .unwrap_or(&members[0]);
        rows[at] = GameRow {
            pid: chosen.pid,
            name: bundle.unwrap_or_else(|| main.name.clone()),
            rss: members.iter().map(|p| p.rss_bytes).sum(),
            wine: members.iter().any(|p| p.is_wine),
            engine: members.iter().find_map(|p| engines(p)),
            locked: !chosen.accessible,
            helpers: members.len() - 1,
            renderer: renderer.is_some(),
        };
    }
    rows
}

/// The row most likely to be the game: an RPG Maker game, else a Wine/Proton
/// program, else a multi-process app's renderer, else the largest program.
pub fn best_guess(rows: &[GameRow]) -> Option<i32> {
    let largest = |f: &dyn Fn(&GameRow) -> bool| {
        rows.iter()
            .filter(|r| f(r))
            .max_by_key(|r| r.rss)
            .map(|r| r.pid)
    };
    largest(&|r| r.engine.is_some())
        .or_else(|| largest(&|r| r.wine))
        .or_else(|| largest(&|r| r.renderer))
        .or_else(|| largest(&|_| true))
}

/// The three steps of making a cheat, with `current` (1–3) highlighted and
/// the ones before it ticked.
pub fn step_bar(ui: &mut egui::Ui, current: u8) {
    ui.horizontal(|ui| {
        for (n, label) in [(1, "Choose your game"), (2, "Find a value"), (3, "Keep it")] {
            let text = if n < current {
                egui::RichText::new(format!("✔ {n} {label}")).color(ok_color(ui))
            } else if n == current {
                egui::RichText::new(format!("{n} {label}")).strong()
            } else {
                egui::RichText::new(format!("{n} {label}")).weak()
            };
            ui.label(text);
            if n < 3 {
                ui.label(egui::RichText::new("›").weak());
            }
        }
    });
}

fn matches(filter: &str, name: &str, cmdline: &str) -> bool {
    filter.is_empty()
        || name.to_ascii_lowercase().contains(filter)
        || cmdline.to_ascii_lowercase().contains(filter)
}

/// The filter row and the list. Basic: one row per program with tags;
/// Advanced: every process with PID, Role and command line. A double-click
/// attaches; the caller draws the Attach button for `st.selected`.
pub fn game_list(
    ui: &mut egui::Ui,
    st: &mut ListState,
    procs: &[ProcInfo],
    rows: &[GameRow],
    advanced: bool,
    height: f32,
) -> Option<ListAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.label("🔎");
        ui.add(
            egui::TextEdit::singleline(&mut st.filter)
                .hint_text("Filter by name…")
                .desired_width(240.0),
        );
        ui.checkbox(&mut st.wine_only, "Wine/Proton only");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("⟳ Refresh").clicked() {
                action = Some(ListAction::Refresh);
            }
        });
    });
    let filter = st.filter.to_ascii_lowercase();
    use egui_extras::{Column, TableBuilder};
    if advanced {
        let mut shown: Vec<&ProcInfo> = procs
            .iter()
            .filter(|p| {
                (!st.wine_only || p.is_wine)
                    && (matches(&filter, &p.name, &p.cmdline)
                        || p.role.as_deref().is_some_and(|r| r.contains(&filter)))
            })
            .collect();
        match st.sort {
            ProcSort::Memory => shown.sort_by_key(|p| std::cmp::Reverse(p.rss_bytes)),
            ProcSort::Pid => shown.sort_by_key(|p| p.pid),
            ProcSort::Name => shown.sort_by_key(|p| p.name.to_ascii_lowercase()),
        }
        if !st
            .selected
            .is_some_and(|pid| shown.iter().any(|p| p.pid == pid))
        {
            st.selected = shown.first().map(|p| p.pid);
        }
        let sort = st.sort;
        TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .max_scroll_height(height)
            .column(Column::auto().at_least(56.0).resizable(true))
            .column(Column::auto().at_least(88.0).resizable(true))
            .column(Column::auto().at_least(150.0).resizable(true))
            .column(Column::auto().at_least(84.0).resizable(true))
            .column(Column::remainder())
            .header(22.0, |mut header| {
                header.col(|ui| {
                    if sort_header(ui, "PID", sort == ProcSort::Pid).clicked() {
                        st.sort = ProcSort::Pid;
                    }
                });
                header.col(|ui| {
                    if sort_header(ui, "Memory", sort == ProcSort::Memory).clicked() {
                        st.sort = ProcSort::Memory;
                    }
                });
                header.col(|ui| {
                    if sort_header(ui, "Process", sort == ProcSort::Name).clicked() {
                        st.sort = ProcSort::Name;
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
                body.rows(22.0, shown.len(), |mut row| {
                    let p = shown[row.index()];
                    row.set_selected(st.selected == Some(p.pid));
                    row.col(|ui| {
                        ui.monospace(p.pid.to_string());
                    });
                    row.col(|ui| {
                        ui.monospace(human_bytes(p.rss_bytes));
                    });
                    row.col(|ui| {
                        if !p.accessible {
                            ui.label(egui::RichText::new("🔒").color(warn_color(ui)))
                                .on_hover_text("Needs admin rights");
                        }
                        let mut text = egui::RichText::new(&p.name).strong();
                        if p.is_wine {
                            text = text.color(wine_color(ui));
                        }
                        ui.add(egui::Label::new(text).truncate());
                    });
                    row.col(|ui| {
                        if let Some(role) = &p.role {
                            let color = if role == "renderer" {
                                ok_color(ui)
                            } else {
                                ui.visuals().weak_text_color()
                            };
                            ui.label(egui::RichText::new(role).small().color(color));
                        }
                    });
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(&p.cmdline).weak().small())
                                .truncate(),
                        );
                    });
                    let resp = row.response();
                    if resp.clicked() {
                        st.selected = Some(p.pid);
                    }
                    if resp.double_clicked() {
                        action = Some(ListAction::Attach(p.pid));
                    }
                });
            });
    } else {
        let mut shown: Vec<&GameRow> = rows
            .iter()
            .filter(|r| (!st.wine_only || r.wine) && matches(&filter, &r.name, ""))
            .collect();
        shown.sort_by_key(|r| std::cmp::Reverse(r.rss));
        if !st
            .selected
            .is_some_and(|pid| shown.iter().any(|r| r.pid == pid))
        {
            st.selected = shown.first().map(|r| r.pid);
        }
        TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .max_scroll_height(height)
            .column(Column::remainder().at_least(240.0))
            .column(Column::exact(96.0))
            .header(22.0, |mut header| {
                header.col(|ui| {
                    ui.strong("Program");
                });
                header.col(|ui| {
                    ui.strong("Memory");
                });
            })
            .body(|body| {
                body.rows(24.0, shown.len(), |mut row| {
                    let r = shown[row.index()];
                    row.set_selected(st.selected == Some(r.pid));
                    row.col(|ui| {
                        ui.add(egui::Label::new(egui::RichText::new(&r.name).strong()).truncate());
                        if let Some(e) = r.engine.filter(|e| e.is_rpgmaker()) {
                            ui.label(egui::RichText::new(e.label()).small().color(ok_color(ui)));
                        }
                        if r.wine {
                            ui.label(egui::RichText::new("Wine").small().color(wine_color(ui)));
                        }
                        if r.locked {
                            ui.label(egui::RichText::new("🔒").small().color(warn_color(ui)))
                                .on_hover_text("Needs admin rights: cheatu asks when you attach");
                        }
                    });
                    row.col(|ui| {
                        ui.label(human_bytes(r.rss));
                    });
                    let resp = row.response();
                    if resp.clicked() {
                        st.selected = Some(r.pid);
                    }
                    if resp.double_clicked() {
                        action = Some(ListAction::Attach(r.pid));
                    }
                });
            });
    }
    action
}

/// The start screen: the steps, the game list and what to do next.
pub fn start_screen(
    ui: &mut egui::Ui,
    st: &mut ListState,
    procs: &[ProcInfo],
    rows: &[GameRow],
    advanced: bool,
) -> Option<ListAction> {
    step_bar(ui, 1);
    ui.add_space(6.0);
    ui.heading("Which game do you want to cheat in?");
    ui.label(
        egui::RichText::new("Start the game first. The most likely one is already selected.")
            .weak(),
    );
    ui.add_space(6.0);
    let height = (ui.available_height() - 90.0).max(140.0);
    let mut action = game_list(ui, st, procs, rows, advanced, height);
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let attach = egui::Button::new(
            egui::RichText::new("Attach")
                .strong()
                .size(15.0)
                .color(ui.visuals().selection.stroke.color),
        )
        .fill(ui.visuals().selection.bg_fill);
        if ui.add_enabled(st.selected.is_some(), attach).clicked() {
            action = st.selected.map(ListAction::Attach);
        }
        if ui.button("📂 Open a saved cheat table…").clicked() {
            action = Some(ListAction::OpenTable);
        }
    });
    if ui
        .link("RPG Maker game not running yet? Set it up…")
        .clicked()
    {
        action = Some(ListAction::RpgSetup);
    }
    action
}

/// After attaching to an RPG Maker game: RPG Maker mode or the scanner.
pub fn rpg_choice(ui: &mut egui::Ui, name: &str, engine: Engine) -> Option<crate::screen::RpgPick> {
    use crate::screen::RpgPick;
    let mut pick = None;
    ui.add_space(8.0);
    ui.heading(format!("{name} is an {} game", engine.label()));
    ui.label(egui::RichText::new("cheatu can change it in two ways.").weak());
    ui.add_space(10.0);
    let card =
        |ui: &mut egui::Ui, recommended: bool, title: &str, bullets: &[&str], button: &str| {
            let mut clicked = false;
            let mut frame = egui::Frame::group(ui.style()).inner_margin(12.0);
            if recommended {
                frame = frame.stroke(egui::Stroke::new(2.0_f32, ui.visuals().selection.bg_fill));
            }
            frame.show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(300.0);
                    if recommended {
                        ui.label(
                            egui::RichText::new("RECOMMENDED")
                                .small()
                                .strong()
                                .color(ui.visuals().selection.bg_fill),
                        );
                    }
                    ui.label(egui::RichText::new(title).strong().size(16.0));
                    for b in bullets {
                        ui.label(format!("• {b}"));
                    }
                    ui.add_space(6.0);
                    let mut btn = egui::Button::new(button);
                    if recommended {
                        btn = btn.fill(ui.visuals().selection.bg_fill);
                    }
                    clicked = ui.add(btn).clicked();
                })
            });
            clicked
        };
    ui.horizontal_top(|ui| {
        if card(
            ui,
            true,
            "🎮 RPG Maker mode",
            &[
                "Gold, items, party HP/MP, switches",
                "Through the game's own code: nothing corrupts",
                "One-time setup, then restart the game",
            ],
            "Use RPG Maker mode",
        ) {
            pick = Some(RpgPick::RpgMaker);
        }
        if card(
            ui,
            false,
            "🔍 Memory scanner",
            &[
                "Any number you can see",
                "Works right now, no restart",
                "Cheats may break after an update",
            ],
            "Scan memory",
        ) {
            pick = Some(RpgPick::Memory);
        }
    });
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("Switch later from the game's name in the header (⏷).")
            .weak()
            .small(),
    );
    pick
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: i32, name: &str, cmd: &str, rss: u64, role: Option<&str>) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.into(),
            cmdline: cmd.into(),
            rss_bytes: rss,
            is_wine: false,
            role: role.map(str::to_owned),
            accessible: true,
        }
    }

    fn no_engine(_: &ProcInfo) -> Option<Engine> {
        None
    }

    #[test]
    fn fold_merges_nwjs_helpers_into_one_row() {
        let procs = vec![
            p(10, "Game", "/g/Game", 50, Some("main")),
            p(
                11,
                "Game Helper",
                "/g/Game --type=renderer --x",
                300,
                Some("renderer"),
            ),
            p(
                12,
                "Game Helper",
                "/g/Game --type=renderer --y",
                600,
                Some("renderer"),
            ),
            p(
                13,
                "Game Helper",
                "/g/Game --type=gpu-process",
                80,
                Some("gpu-process"),
            ),
        ];
        let rows = fold(&procs, &mut no_engine);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(
            (r.pid, r.name.as_str(), r.rss, r.helpers, r.renderer),
            (12, "Game", 1030, 3, true)
        );
    }

    /// macOS: helpers are separate programs inside the main app's bundle.
    #[test]
    fn fold_groups_macos_helpers_by_app_bundle() {
        let app = "/Applications/Brave Browser.app/Contents";
        let fw = format!("{app}/Frameworks/H.framework");
        let procs = vec![
            // Listed first, and the main process has no Chromium role.
            p(1, "chrome_crashpad_handler", &format!("{fw}/Helpers/chrome_crashpad_handler --type=crashpad-handler"), 5, Some("crashpad-handler")),
            p(2, "Brave Browser", &format!("{app}/MacOS/Brave Browser"), 100, None),
            p(3, "Brave Helper (Renderer)", &format!("{fw}/Helpers/Brave Helper (Renderer).app/Contents/MacOS/Brave Helper (Renderer) --type=renderer"), 400, Some("renderer")),
            p(4, "Brave Helper (GPU)", &format!("{fw}/Helpers/Brave Helper (GPU).app/Contents/MacOS/Brave Helper (GPU) --type=gpu-process"), 50, Some("gpu-process")),
        ];
        let rows = fold(&procs, &mut no_engine);
        assert_eq!(rows.len(), 1, "{rows:?}");
        let r = &rows[0];
        assert_eq!(
            (r.pid, r.name.as_str(), r.helpers, r.rss),
            (3, "Brave Browser", 3, 555)
        );
    }

    #[test]
    fn fold_keeps_ordinary_processes() {
        let procs = vec![p(1, "a", "/bin/a", 5, None), p(2, "b", "/bin/b", 7, None)];
        let rows = fold(&procs, &mut no_engine);
        assert_eq!(rows.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![1, 2]);
        assert!(rows.iter().all(|r| r.helpers == 0 && !r.renderer));
    }

    #[test]
    fn fold_takes_the_engine_of_any_member() {
        let procs = vec![
            p(10, "Game", "/g/Game", 50, Some("main")),
            p(
                11,
                "Game Helper",
                "/g/Game --type=renderer",
                300,
                Some("renderer"),
            ),
        ];
        let rows = fold(&procs, &mut |q: &ProcInfo| {
            (q.pid == 10).then_some(Engine::RpgMakerMZ)
        });
        assert_eq!(rows[0].engine, Some(Engine::RpgMakerMZ));
        assert_eq!(rows[0].pid, 11);
    }

    #[test]
    fn best_guess_is_preselected() {
        let mut procs = vec![
            p(1, "big", "/bin/big", 9000, None),
            p(2, "wine", "C:\\x.exe", 100, None),
        ];
        procs[1].is_wine = true;
        let rows = fold(&procs, &mut no_engine);
        assert_eq!(best_guess(&rows), Some(2));
        let rpg = vec![
            p(3, "rpg", "/r/Game", 10, None),
            p(4, "big", "/bin/big", 9000, None),
        ];
        let rows = fold(&rpg, &mut |q: &ProcInfo| {
            (q.pid == 3).then_some(Engine::RpgMakerMV)
        });
        assert_eq!(best_guess(&rows), Some(3));
        let plain = vec![p(5, "a", "/a", 10, None), p(6, "b", "/b", 20, None)];
        assert_eq!(best_guess(&fold(&plain, &mut no_engine)), Some(6));
        assert_eq!(best_guess(&[]), None);
    }
}

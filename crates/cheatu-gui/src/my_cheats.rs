//! The My cheats panel (Cheat table in Advanced view): one line per cheat in
//! the basic view, every detail and tool in Advanced.

use std::collections::HashSet;
use std::path::Path;

use cheatu_core::table::TableEntry;
use cheatu_core::{AddrExpr, MemoryRegion, ScanType, Scanner};
use eframe::egui;

use crate::cheat_table::GuiEntry;
use crate::{err_color, ok_color, read_current, warn_color};

/// Whether a cheat keeps working after the game restarts: its address does
/// not start from a plain number (a module, a pointer chain from one, or a
/// byte pattern).
pub fn survives_restart(e: &AddrExpr) -> bool {
    e.is_static()
}

/// How "Make it last…" finds a lasting address for `addr`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MakeLasting {
    PointerScan,
    Signature,
}

/// A value in a module's image is found again by its bytes; anything else
/// (heap) needs a pointer chain.
pub fn make_lasting_tool(regions: &[MemoryRegion], addr: u64) -> MakeLasting {
    if cheatu_core::module_containing(regions, addr).is_some() {
        MakeLasting::Signature
    } else {
        MakeLasting::PointerScan
    }
}

/// What was last saved or opened, to tell whether My cheats has changes.
#[derive(Default)]
pub struct Saved {
    snapshot: Option<Vec<TableEntry>>,
}

impl Saved {
    /// The cheats were just saved or opened.
    pub fn mark(&mut self, entries: &[GuiEntry]) {
        self.snapshot = Some(entries.iter().map(|e| e.entry.clone()).collect());
    }

    /// My cheats differ from the last save or open. A new, empty list is not
    /// a change.
    pub fn is_dirty(&self, entries: &[GuiEntry]) -> bool {
        match &self.snapshot {
            None => !entries.is_empty(),
            Some(s) => {
                s.len() != entries.len() || s.iter().zip(entries).any(|(a, b)| *a != b.entry)
            }
        }
    }
}

/// What the app must do after a frame of the panel.
pub enum Action {
    Open,
    Save,
    SaveAs,
    ClearAll,
    /// Write row `.0`'s value once; feedback goes next to `.1`.
    Set(usize, egui::Rect),
    Remove(usize),
    MakeLasting(usize),
    PointerScan(usize),
    Signature(usize),
    ChangeType(usize, ScanType),
    /// An address expression was changed.
    ExprEdited,
}

/// Facts the panel shows but does not own.
pub struct Ctx<'a> {
    pub advanced: bool,
    pub attached: bool,
    pub can_read: bool,
    pub aob_busy: bool,
    pub sig_busy: bool,
    pub live_error: Option<&'a str>,
    pub scanner: Option<&'a Scanner>,
    pub path: Option<&'a Path>,
    pub dirty: bool,
}

/// Which rows are unfolded and which one is being renamed.
#[derive(Default)]
pub struct PanelState {
    open: HashSet<u64>,
    renaming: Option<u64>,
}

/// The live value of a cheat as text, or why there is none.
fn live_value(e: &GuiEntry, cx: &Ctx) -> Result<String, String> {
    if !cx.attached {
        return Err("not attached".into());
    }
    if let Some(why) = cx.live_error {
        return Err(why.into());
    }
    match &e.resolved {
        Ok(addr) => Ok(cx
            .scanner
            .and_then(|s| read_current(s, *addr, e.entry.ty))
            .unwrap_or_else(|| "n/a".into())),
        Err(err) if e.uses_aob() && cx.aob_busy && err == "not resolved yet" => {
            Err("searching…".into())
        }
        Err(err) => Err(err.clone()),
    }
}

fn freeze_toggle(ui: &mut egui::Ui, e: &mut GuiEntry, label: bool) {
    let text = match (label, e.entry.frozen) {
        (false, _) => "❄",
        (true, true) => "❄ Frozen",
        (true, false) => "❄ Freeze",
    };
    if ui
        .selectable_label(e.entry.frozen, text)
        .on_hover_text("Keep writing this value into the game")
        .clicked()
    {
        e.entry.frozen = !e.entry.frozen;
    }
}

/// Value field and Set; `Some(rect)` when Set was clicked.
fn value_and_set(ui: &mut egui::Ui, e: &mut GuiEntry) -> Option<egui::Rect> {
    ui.add(egui::TextEdit::singleline(&mut e.entry.value).desired_width(64.0));
    let valid = e.entry.ty.parse(&e.entry.value).is_some();
    let resolved = e.resolved.is_ok();
    let set = ui
        .add_enabled(
            valid && resolved && !e.entry.frozen,
            egui::Button::new("Set").small(),
        )
        .on_hover_text("Write this value into the game once")
        .on_disabled_hover_text(if e.entry.frozen {
            "Frozen: this value is already written all the time."
        } else if !resolved {
            "Not found in the game right now."
        } else {
            "Not a valid value for this kind of number."
        });
    set.clicked().then_some(set.rect)
}

/// Draw the panel and return what the app must do.
pub fn panel(
    ui: &mut egui::Ui,
    st: &mut PanelState,
    entries: &mut [GuiEntry],
    cx: &Ctx,
) -> Vec<Action> {
    let mut out = Vec::new();
    ui.horizontal(|ui| {
        ui.heading(if cx.advanced {
            "Cheat table"
        } else {
            "My cheats"
        });
        if let Some(name) = cx.path.and_then(Path::file_name) {
            ui.label(egui::RichText::new(name.to_string_lossy()).weak());
        }
        if cx.dirty {
            ui.label(egui::RichText::new("•").strong().color(warn_color(ui)))
                .on_hover_text("Unsaved changes");
        }
    });
    ui.horizontal(|ui| {
        if ui
            .small_button("📂 Open…")
            .on_hover_text("Open saved cheats (freezes start off)")
            .clicked()
        {
            out.push(Action::Open);
        }
        let save_hint = match cx.path {
            Some(p) => format!("Save to {}", p.display()),
            None => "Save to a new file".to_string(),
        };
        if ui
            .add_enabled(!entries.is_empty(), egui::Button::new("💾 Save").small())
            .on_hover_text(save_hint)
            .clicked()
        {
            out.push(Action::Save);
        }
        if cx.advanced
            && ui
                .add_enabled(!entries.is_empty(), egui::Button::new("Save as…").small())
                .clicked()
        {
            out.push(Action::SaveAs);
        }
        ui.menu_button("⋯", |ui| {
            if ui
                .add_enabled(!entries.is_empty(), egui::Button::new("Save as…"))
                .clicked()
            {
                out.push(Action::SaveAs);
                ui.close_menu();
            }
            if ui
                .add_enabled(!entries.is_empty(), egui::Button::new("Remove all…"))
                .clicked()
            {
                out.push(Action::ClearAll);
                ui.close_menu();
            }
        });
    });
    ui.separator();
    if entries.is_empty() {
        ui.label(egui::RichText::new("Nothing yet. Cheats you find land here.").weak());
        return out;
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, e) in entries.iter_mut().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                if cx.advanced {
                    advanced_row(ui, i, e, cx, &mut out);
                } else {
                    basic_row(ui, st, i, e, cx, &mut out);
                }
            }
        });
    out
}

fn basic_row(
    ui: &mut egui::Ui,
    st: &mut PanelState,
    i: usize,
    e: &mut GuiEntry,
    cx: &Ctx,
    out: &mut Vec<Action>,
) {
    let lasting = survives_restart(&e.entry.address);
    let open = st.open.contains(&e.id);
    ui.horizontal(|ui| {
        freeze_toggle(ui, e, true);
        ui.add(egui::Label::new(egui::RichText::new(&e.entry.description).strong()).truncate());
        if let Some(rect) = value_and_set(ui, e) {
            out.push(Action::Set(i, rect));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button(if open { "⏷" } else { "⏵" })
                .on_hover_text(if open { "Hide details" } else { "Details" })
                .clicked()
            {
                if open {
                    st.open.remove(&e.id);
                } else {
                    st.open.insert(e.id);
                }
            }
            if !lasting {
                ui.label(
                    egui::RichText::new("⚠ this session only")
                        .small()
                        .color(warn_color(ui)),
                );
            }
        });
    });
    if !open {
        return;
    }
    ui.indent(("details", e.id), |ui| {
        if lasting {
            ui.label(
                egui::RichText::new("✔ Keeps working after a restart")
                    .small()
                    .color(ok_color(ui)),
            );
        } else {
            ui.label(
                egui::RichText::new("⚠ Works until the game restarts.")
                    .small()
                    .color(warn_color(ui)),
            );
            let usable = cx.can_read && e.resolved.is_ok() && !cx.sig_busy;
            if ui
                .add_enabled(usable, egui::Button::new("Make it last…").small())
                .on_hover_text("Find this value in a way that survives restarts")
                .on_disabled_hover_text("Attach to the game, and let the cheat be found first.")
                .clicked()
            {
                out.push(Action::MakeLasting(i));
            }
        }
        let now = match live_value(e, cx) {
            Ok(v) => format!("Now {v} in the game · {}", e.entry.ty.friendly_label()),
            Err(why) => format!("{why} · {}", e.entry.ty.friendly_label()),
        };
        ui.label(egui::RichText::new(now).small().weak());
        if e.frozen_when_saved && !e.entry.frozen {
            ui.label(
                egui::RichText::new("Was frozen when saved; freezes start off after opening.")
                    .small()
                    .weak(),
            );
        }
        if st.renaming == Some(e.id) {
            ui.horizontal(|ui| {
                let r = ui
                    .add(egui::TextEdit::singleline(&mut e.entry.description).desired_width(140.0));
                if ui.small_button("Done").clicked()
                    || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    st.renaming = None;
                }
            });
        }
        ui.horizontal(|ui| {
            if ui.small_button("Rename").clicked() {
                st.renaming = Some(e.id);
            }
            if ui.small_button("Remove…").clicked() {
                out.push(Action::Remove(i));
            }
        });
    });
}

fn advanced_row(ui: &mut egui::Ui, i: usize, e: &mut GuiEntry, cx: &Ctx, out: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        freeze_toggle(ui, e, false);
        ui.add(
            egui::TextEdit::singleline(&mut e.entry.description)
                .hint_text("name")
                .desired_width(96.0),
        );
        ui.label(
            egui::RichText::new(e.entry.ty.friendly_label())
                .weak()
                .small(),
        );
        if let Some(rect) = value_and_set(ui, e) {
            out.push(Action::Set(i, rect));
        }
        ui.menu_button("⋯", |ui| {
            ui.label(egui::RichText::new("Change type").weak().small());
            for t in ScanType::ALL {
                let label = format!("{} ({t})", t.friendly_label());
                if ui.selectable_label(e.entry.ty == t, label).clicked() {
                    out.push(Action::ChangeType(i, t));
                    ui.close_menu();
                }
            }
        });
    });
    ui.horizontal(|ui| {
        let resp = ui.add(
            egui::TextEdit::singleline(&mut e.expr_text)
                .font(egui::TextStyle::Monospace)
                .hint_text("address expression")
                .desired_width((ui.available_width() - 150.0).clamp(160.0, 420.0)),
        );
        if resp.changed() {
            e.parse_error = AddrExpr::parse(&e.expr_text).err().map(|x| x.to_string());
        }
        // Applied when editing ends, not per keystroke: a half-typed address
        // must never be written to.
        if resp.lost_focus() {
            if let Ok(expr) = AddrExpr::parse(&e.expr_text) {
                if expr != e.entry.address {
                    e.entry.address = expr;
                    e.resolved = Err("not resolved yet".into());
                    e.needs_resolve = true;
                    out.push(Action::ExprEdited);
                }
            }
        }
        match (live_value(e, cx), &e.resolved) {
            (Ok(v), Ok(addr)) => {
                ui.monospace(format!("→ 0x{addr:012x}"));
                ui.label(egui::RichText::new(format!("= {v}")).monospace().weak())
                    .on_hover_text("The value in the game right now");
            }
            (Err(why), _) => {
                ui.label(egui::RichText::new(why).small().color(err_color(ui)));
            }
            (Ok(_), Err(why)) => {
                ui.label(
                    egui::RichText::new(why.as_str())
                        .small()
                        .color(err_color(ui)),
                );
            }
        }
    });
    if let Some(err) = &e.parse_error {
        ui.label(egui::RichText::new(err).small().color(err_color(ui)));
    }
    ui.horizontal(|ui| {
        let usable = cx.can_read && e.resolved.is_ok();
        if ui
            .add_enabled(usable, egui::Button::new("🎯 Pointer scan…").small())
            .on_hover_text("Find pointer chains from static data to this address")
            .on_disabled_hover_text("Attach, and let the address resolve first.")
            .clicked()
        {
            out.push(Action::PointerScan(i));
        }
        if ui
            .add_enabled(
                usable && !cx.sig_busy,
                egui::Button::new("🧬 AoB signature").small(),
            )
            .on_hover_text("Replace the address with a byte pattern search that survives restarts")
            .on_disabled_hover_text(if cx.sig_busy {
                "A signature is already being made."
            } else {
                "Attach, and let the address resolve first."
            })
            .clicked()
        {
            out.push(Action::Signature(i));
        }
        let busy = e.uses_aob() && cx.aob_busy;
        if ui
            .add_enabled(
                cx.attached && !busy,
                egui::Button::new("⟳ Re-resolve").small(),
            )
            .on_disabled_hover_text(if busy {
                "An aob() search is already running."
            } else {
                "Attach to the game first."
            })
            .clicked()
        {
            e.needs_resolve = true;
        }
        if ui.small_button("🗑").on_hover_text("Remove").clicked() {
            out.push(Action::Remove(i));
        }
    });
}

/// Something that would lose unsaved cheats, waiting on an answer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Pending {
    Quit,
    Open,
    /// Relaunch with admin rights (and attach to this game).
    Elevate(Option<i32>),
}

/// The answer to "Save changes?".
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Save,
    DontSave,
    Cancel,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CloseAction {
    /// Go ahead.
    Close,
    /// Ask "Save changes?" first.
    Ask,
    /// Stay: the user cancelled.
    Stay,
    /// Save, and go ahead once that worked.
    SaveThenClose,
}

/// Whether quitting (or opening another table) may go ahead now.
pub fn close_decision(dirty: bool, answer: Option<Answer>) -> CloseAction {
    match (dirty, answer) {
        (false, _) => CloseAction::Close,
        (true, None) => CloseAction::Ask,
        (true, Some(Answer::Cancel)) => CloseAction::Stay,
        (true, Some(Answer::Save)) => CloseAction::SaveThenClose,
        (true, Some(Answer::DontSave)) => CloseAction::Close,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cheatu_core::ScanType;

    fn expr(s: &str) -> AddrExpr {
        AddrExpr::parse(s).unwrap()
    }

    fn entry(desc: &str) -> GuiEntry {
        GuiEntry::new(
            TableEntry {
                description: desc.into(),
                address: expr("\"game\"+0x10"),
                ty: ScanType::I32,
                value: "1".into(),
                frozen: false,
            },
            false,
        )
    }

    #[test]
    fn pointer_chain_survives() {
        assert!(survives_restart(&expr("[[\"game\"+0x10]+0x8]+0x4")));
    }

    #[test]
    fn aob_survives() {
        assert!(survives_restart(&expr("aob(\"41 42 ?? 43\")+0x4")));
    }

    #[test]
    fn module_offset_survives() {
        assert!(survives_restart(&expr("\"game\"+0x10")));
    }

    #[test]
    fn plain_number_is_session_only() {
        assert!(!survives_restart(&expr("0x7ff012345678")));
        assert!(!survives_restart(&expr("[0x1000]+0x8")));
    }

    #[test]
    fn new_empty_table_is_clean() {
        let s = Saved::default();
        assert!(!s.is_dirty(&[]));
        assert!(s.is_dirty(&[entry("Gold")]), "a cheat nobody saved yet");
    }

    #[test]
    fn dirty_after_edit_clean_after_save() {
        let mut s = Saved::default();
        let mut cheats = vec![entry("Gold")];
        s.mark(&cheats);
        assert!(!s.is_dirty(&cheats));
        cheats[0].entry.frozen = true;
        assert!(s.is_dirty(&cheats));
        s.mark(&cheats);
        assert!(!s.is_dirty(&cheats));
        cheats.push(entry("Hearts"));
        assert!(s.is_dirty(&cheats));
    }

    #[test]
    fn editing_back_to_saved_is_clean() {
        let mut s = Saved::default();
        let mut cheats = vec![entry("Gold")];
        s.mark(&cheats);
        cheats[0].entry.value = "999".into();
        assert!(s.is_dirty(&cheats));
        cheats[0].entry.value = "1".into();
        assert!(!s.is_dirty(&cheats));
    }

    #[test]
    fn make_lasting_picks_signature_for_module_data() {
        let regions = vec![
            MemoryRegion {
                start: 0x1000,
                end: 0x2000,
                read: true,
                write: true,
                exec: false,
                shared: false,
                path: "/games/game".into(),
            },
            MemoryRegion {
                start: 0x9000,
                end: 0xa000,
                read: true,
                write: true,
                exec: false,
                shared: false,
                path: "[heap]".into(),
            },
        ];
        assert_eq!(make_lasting_tool(&regions, 0x1800), MakeLasting::Signature);
        assert_eq!(
            make_lasting_tool(&regions, 0x9800),
            MakeLasting::PointerScan
        );
    }

    #[test]
    fn close_decision_clean_closes() {
        assert_eq!(close_decision(false, None), CloseAction::Close);
    }

    #[test]
    fn close_decision_dirty_asks() {
        assert_eq!(close_decision(true, None), CloseAction::Ask);
    }

    #[test]
    fn close_decision_cancel_stays() {
        assert_eq!(
            close_decision(true, Some(Answer::Cancel)),
            CloseAction::Stay
        );
    }

    #[test]
    fn close_decision_save_saves_then_closes() {
        assert_eq!(
            close_decision(true, Some(Answer::Save)),
            CloseAction::SaveThenClose
        );
    }

    #[test]
    fn close_decision_dont_save_closes() {
        assert_eq!(
            close_decision(true, Some(Answer::DontSave)),
            CloseAction::Close
        );
    }
}

//! The pointer scan window: settings, a background map build and chain
//! search, and the results with their filters, save/open and "Add to table".

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use cheatu_core::pointer_scan::{
    find_chains, PointerChain, PointerMap, PtrResults, PtrScanSettings,
};
use cheatu_core::table::TableEntry;
use cheatu_core::{AddrExpr, LiveSource, MemSource, ScanProgress, ScanType, ScanValue};
use eframe::egui;

use crate::cheat_table::value_text;
use crate::file_dialog::{FileDialog, Mode, Outcome};

/// Most chains listed in the window.
pub const MAX_ROWS: usize = 1000;

/// Indices of `results.chains`, shortest chain first (stable otherwise), at
/// most `limit`.
fn shortest_first(results: &PtrResults, limit: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..results.chains.len()).collect();
    order.sort_by_key(|&i| results.chains[i].offsets.len());
    order.truncate(limit);
    order
}

/// The listed rows: each chain as its address expression, shortest first,
/// at most `limit`.
pub fn chain_rows(results: &PtrResults, limit: usize) -> Vec<String> {
    shortest_first(results, limit)
        .into_iter()
        .map(|i| results.chains[i].to_expr().to_string())
        .collect()
}

/// The settings typed into the window, checked like the CLI's.
pub fn validate_settings(
    depth: &str,
    max_offset: &str,
    alignment: &str,
    max_results: &str,
) -> Result<PtrScanSettings, String> {
    let max_depth = parse_num(depth)
        .and_then(|d| usize::try_from(d).ok())
        .filter(|d| (1..=8).contains(d))
        .ok_or("Depth must be a number from 1 to 8.")?;
    let max_offset = parse_num(max_offset)
        .filter(|m| (0x8..=0x100000).contains(m))
        .ok_or("Max offset must be from 0x8 to 0x100000.")?;
    let alignment = parse_num(alignment)
        .filter(|a| [1, 2, 4, 8].contains(a))
        .ok_or("Alignment must be 1, 2, 4 or 8.")?;
    let max_results = parse_num(max_results)
        .and_then(|c| usize::try_from(c).ok())
        .filter(|c| (1..=10_000_000).contains(c))
        .ok_or("Result cap must be from 1 to 10000000.")?;
    Ok(PtrScanSettings {
        max_depth,
        max_offset,
        alignment,
        max_results,
        ..PtrScanSettings::default()
    })
}

/// A number as the CLI takes it: `0x` hex or decimal; `_` separators are
/// allowed.
fn parse_num(s: &str) -> Option<u64> {
    let s: String = s.trim().chars().filter(|&c| c != '_').collect();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

/// What the scan is doing, for the progress line.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Building the pointer map: reading memory, then sorting it on disk.
    Map,
    /// Searching the map for chains.
    Search,
}

/// The progress label: the map build reports memory read, so once it is
/// complete the remaining time is spent sorting.
pub fn phase_label(phase: Phase, fraction: Option<f32>) -> &'static str {
    match phase {
        Phase::Map if fraction.is_some_and(|f| f >= 1.0) => "Sorting…",
        Phase::Map => "Reading memory…",
        Phase::Search => "Searching chains…",
    }
}

/// A hex address (`0x…` or bare hex digits), if `s` is one.
pub fn hex_address(s: &str) -> Option<Result<u64, String>> {
    let s = s.trim();
    let digits = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(u64::from_str_radix(digits, 16).map_err(|_| format!("address {s} is out of range")))
}

/// The table entry "Add to table" makes for `chain`.
pub fn entry_for_chain(chain: &PointerChain, ty: ScanType, value: Option<String>) -> TableEntry {
    TableEntry {
        description: "Pointer chain".into(),
        address: chain.to_expr(),
        ty,
        value: value.unwrap_or_else(|| "0".into()),
        frozen: false,
    }
}

/// Extension of pointer scan results files.
const PTR_EXT: &str = "cheatu-ptr";

/// Resumes a target suspended for the map snapshot, also when the build
/// fails, is cancelled or panics.
struct Paused(i32);

impl Paused {
    fn new(pid: i32) -> Option<Paused> {
        cheatu_core::suspend(pid).ok().map(|()| Paused(pid))
    }
}

impl Drop for Paused {
    fn drop(&mut self) {
        let _ = cheatu_core::resume(self.0);
    }
}

/// What a background job sends back.
enum Done {
    Scan(Result<PtrResults, String>),
    /// The filtered copy and how many chains remain.
    Filter(Result<(PtrResults, usize), String>),
}

/// How the results are filtered.
enum FilterBy {
    /// An address expression or hex, resolved in the process.
    Address(String),
    Value(ScanType, ScanValue),
}

enum JobKind {
    Scan {
        phase: Arc<AtomicU8>,
        map: Arc<ScanProgress>,
        search: Arc<ScanProgress>,
    },
    Filter,
}

/// A scan or filter running on a background thread.
struct Job {
    /// The process it reads; its result is dropped if another one is
    /// attached by the time it is done.
    pid: i32,
    kind: JobKind,
    rx: Receiver<Done>,
}

impl Job {
    fn cancel(&self) {
        if let JobKind::Scan { map, search, .. } = &self.kind {
            map.cancel();
            search.cancel();
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum DialogKind {
    Save,
    Open,
}

/// The pointer scan window for one target address.
pub struct PointerWindow {
    open: bool,
    target: u64,
    process: String,
    /// The process `target` belongs to, taken on the first frame.
    target_pid: Option<i32>,
    /// Type of the row the scan was started from.
    entry_ty: Option<ScanType>,
    /// The GUI's current value type, for "Filter by value". Set every frame.
    pub value_ty: ScanType,
    /// Mirrors "pause target while scanning". Set every frame.
    pub pause_target: bool,
    depth: String,
    max_offset: String,
    alignment: String,
    max_results: String,
    results: Option<PtrResults>,
    /// The file the results were opened from (their settings are shown, but
    /// never used for a scan).
    loaded_from: Option<PathBuf>,
    /// `results.chains` indices of the listed rows, and their text.
    order: Vec<usize>,
    rows: Vec<String>,
    /// Index into `order`.
    selected: Option<usize>,
    filter_addr: String,
    filter_value: String,
    job: Option<Job>,
    message: String,
    dialog: Option<(DialogKind, FileDialog)>,
    last_dir: Option<PathBuf>,
}

impl Drop for PointerWindow {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel();
        }
    }
}

impl PointerWindow {
    pub fn open_for(target: u64, process: String) -> Self {
        let d = PtrScanSettings::default();
        PointerWindow {
            open: true,
            target,
            process,
            target_pid: None,
            entry_ty: None,
            value_ty: ScanType::I32,
            pause_target: true,
            depth: d.max_depth.to_string(),
            max_offset: format!("{:#x}", d.max_offset),
            alignment: d.alignment.to_string(),
            max_results: d.max_results.to_string(),
            results: None,
            loaded_from: None,
            order: Vec::new(),
            rows: Vec::new(),
            selected: None,
            filter_addr: String::new(),
            filter_value: String::new(),
            job: None,
            message: String::new(),
            dialog: None,
            last_dir: None,
        }
    }

    /// The type "Add to table" gives new rows: the scanned row's.
    pub fn with_type(mut self, ty: ScanType) -> Self {
        self.entry_ty = Some(ty);
        self
    }

    /// Point the open window at another row, keeping the results.
    pub fn retarget(&mut self, target: u64, process: String, ty: ScanType) {
        self.target = target;
        self.process = process;
        self.target_pid = None;
        self.entry_ty = Some(ty);
        self.open = true;
    }

    /// The type of the row the scan was started from.
    pub fn entry_type(&self) -> Option<ScanType> {
        self.entry_ty
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// A scan or filter is running.
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn set_results(&mut self, r: PtrResults, loaded_from: Option<PathBuf>) {
        self.order = shortest_first(&r, MAX_ROWS);
        self.rows = chain_rows(&r, MAX_ROWS);
        self.selected = None;
        self.results = Some(r);
        self.loaded_from = loaded_from;
    }

    fn start_scan(&mut self, pid: i32, settings: PtrScanSettings) {
        let phase = Arc::new(AtomicU8::new(0));
        let map_p = Arc::new(ScanProgress::default());
        let search_p = Arc::new(ScanProgress::default());
        let (tx, rx) = mpsc::channel();
        let (target, process, pause) = (self.target, self.process.clone(), self.pause_target);
        let (ph, mp, sp) = (phase.clone(), map_p.clone(), search_p.clone());
        std::thread::spawn(move || {
            let run = || -> Result<PtrResults, String> {
                let src = LiveSource::open(pid).map_err(|e| format!("cannot read process: {e}"))?;
                let map = {
                    // Held only while the snapshot is taken.
                    let _paused = if pause { Paused::new(pid) } else { None };
                    PointerMap::build(&src, &settings, &mp)
                }
                .map_err(scan_error)?;
                ph.store(1, Ordering::Relaxed);
                let found = find_chains(&map, &src, target, &settings, &sp).map_err(scan_error)?;
                Ok(PtrResults {
                    process,
                    target,
                    settings,
                    chains: found.chains,
                    truncated: found.truncated,
                })
            };
            let _ = tx.send(Done::Scan(run()));
        });
        self.message.clear();
        self.job = Some(Job {
            pid,
            kind: JobKind::Scan {
                phase,
                map: map_p,
                search: search_p,
            },
            rx,
        });
    }

    fn start_filter(&mut self, pid: i32, by: FilterBy) {
        let Some(mut res) = self.results.clone() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let run = || -> Result<(PtrResults, usize), String> {
                let src = LiveSource::open(pid).map_err(|e| format!("cannot read process: {e}"))?;
                let n = match by {
                    FilterBy::Address(text) => {
                        let addr = match hex_address(&text) {
                            Some(a) => a?,
                            None => AddrExpr::parse(text.trim())
                                .map_err(|e| format!("bad expression: {e}"))?
                                .resolve(&src)
                                .map_err(|e| e.to_string())?,
                        };
                        res.filter_by_address(&src, addr)
                    }
                    FilterBy::Value(ty, v) => res.filter_by_value(&src, ty, &v),
                };
                Ok((res, n))
            };
            let _ = tx.send(Done::Filter(run()));
        });
        self.message = "Filtering…".into();
        self.job = Some(Job {
            pid,
            kind: JobKind::Filter,
            rx,
        });
    }

    /// Take a finished job's result, if it still belongs to `pid`.
    fn poll_job(&mut self, pid: Option<i32>) {
        let Some(job) = &self.job else {
            return;
        };
        let done = match job.rx.try_recv() {
            Ok(d) => d,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.job = None;
                self.message = "The pointer scan stopped unexpectedly.".into();
                return;
            }
        };
        let job_pid = job.pid;
        self.job = None;
        if pid != Some(job_pid) {
            self.message = "Result dropped: another process was attached meanwhile.".into();
            return;
        }
        match done {
            Done::Scan(Ok(r)) => {
                self.message = format!(
                    "{} chains found{}.",
                    r.chains.len(),
                    if r.truncated {
                        " (a cap was hit: more may exist)"
                    } else {
                        ""
                    }
                );
                self.set_results(r, None);
            }
            Done::Filter(Ok((r, n))) => {
                self.message = format!("{n} chains remain.");
                let from = self.loaded_from.take();
                self.set_results(r, from);
            }
            Done::Scan(Err(e)) | Done::Filter(Err(e)) => self.message = e,
        }
    }

    fn open_dialog(&mut self, kind: DialogKind) {
        let home = crate::home_dir();
        let start = crate::file_dialog::start_dir(self.last_dir.as_deref(), &home);
        let dialog = match kind {
            DialogKind::Open => FileDialog::new(
                Mode::Open,
                "Open pointer scan results",
                PTR_EXT,
                start,
                home,
                "",
            ),
            DialogKind::Save => FileDialog::new(
                Mode::Save,
                "Save pointer scan results",
                PTR_EXT,
                start,
                home,
                &format!("{}.{PTR_EXT}", self.process),
            ),
        };
        self.dialog = Some((kind, dialog));
    }

    fn file_picked(&mut self, kind: DialogKind, path: PathBuf) {
        match kind {
            DialogKind::Save => {
                let Some(r) = &self.results else {
                    return;
                };
                self.message = match r.save(&path) {
                    Ok(()) => format!("Saved {} chains to {}.", r.chains.len(), path.display()),
                    Err(e) => format!("Could not save {}: {e}", path.display()),
                };
            }
            DialogKind::Open => match PtrResults::load(&path) {
                Ok(r) => {
                    self.message = format!(
                        "Opened {} chains for {} (target was {:#x}).",
                        r.chains.len(),
                        r.process,
                        r.target
                    );
                    self.set_results(r, Some(path));
                }
                Err(e) => self.message = format!("Could not open {}: {e}", path.display()),
            },
        }
    }

    /// The live value at `chain`'s address, as the table stores it.
    fn current_value(src: &LiveSource, chain: &PointerChain, ty: ScanType) -> Option<String> {
        let addr = chain.resolve(src).ok()?;
        let mut buf = vec![0u8; ty.size()];
        match src.read(addr, &mut buf) {
            Ok(n) if n == buf.len() => ScanValue::from_ne_bytes(ty, &buf).map(|v| value_text(&v)),
            _ => None,
        }
    }

    /// Draw the window. Returns a new table entry when the user clicks
    /// "Add to table".
    pub fn show(&mut self, ctx: &egui::Context, src: Option<&LiveSource>) -> Option<TableEntry> {
        let pid = src.map(|s| s.pid());
        if self.target_pid.is_none() {
            self.target_pid = pid;
        }
        self.poll_job(pid);

        let settings = validate_settings(
            &self.depth,
            &self.max_offset,
            &self.alignment,
            &self.max_results,
        );
        let filter_addr_ok = {
            let t = self.filter_addr.trim();
            !t.is_empty() && (hex_address(t).is_some() || AddrExpr::parse(t).is_ok())
        };
        let filter_value = self.value_ty.parse(self.filter_value.trim());
        let same_process = pid.is_some() && pid == self.target_pid;

        let mut do_scan = false;
        let mut do_cancel = false;
        let mut do_dialog = None;
        let mut do_filter = None;
        let mut add = None;
        let mut open = self.open;
        egui::Window::new("Pointer scan")
            .open(&mut open)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Target:");
                    ui.monospace(format!("0x{:012x}", self.target));
                    ui.label(egui::RichText::new(format!("in {}", self.process)).weak());
                });
                if pid.is_some() && !same_process {
                    ui.label(
                        egui::RichText::new(
                            "Another process is attached; open this window from a row again.",
                        )
                        .small()
                        .color(ui.visuals().warn_fg_color),
                    );
                }
                ui.add_space(4.0);
                egui::Grid::new("ptr_settings")
                    .num_columns(4)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Depth");
                        ui.add(egui::TextEdit::singleline(&mut self.depth).desired_width(60.0))
                            .on_hover_text("Dereference steps, 1 to 8");
                        ui.label("Max offset");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.max_offset).desired_width(80.0),
                        )
                        .on_hover_text("Per step, 0x8 to 0x100000");
                        ui.end_row();
                        ui.label("Alignment");
                        ui.add(egui::TextEdit::singleline(&mut self.alignment).desired_width(60.0))
                            .on_hover_text("1, 2, 4 or 8 bytes");
                        ui.label("Result cap");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.max_results).desired_width(80.0),
                        )
                        .on_hover_text("Stop after this many chains, 1 to 10000000");
                        ui.end_row();
                    });
                if let Err(e) = &settings {
                    ui.label(
                        egui::RichText::new(e)
                            .small()
                            .color(ui.visuals().error_fg_color),
                    );
                }
                ui.horizontal(|ui| {
                    let blocker = if settings.is_err() {
                        Some("Fix the settings first.")
                    } else if pid.is_none() {
                        Some("Attach to a process first.")
                    } else if !same_process {
                        Some("The target belongs to another process.")
                    } else if self.job.is_some() {
                        Some("A pointer scan or filter is running.")
                    } else {
                        None
                    };
                    if ui
                        .add_enabled(blocker.is_none(), egui::Button::new("Scan"))
                        .on_hover_text(if self.pause_target {
                            "Build a pointer map (the target is paused while memory is read), \
                             then search it"
                        } else {
                            "Build a pointer map, then search it"
                        })
                        .on_disabled_hover_text(blocker.unwrap_or(""))
                        .clicked()
                    {
                        do_scan = true;
                    }
                    if ui
                        .add_enabled(self.job.is_none(), egui::Button::new("Open results…"))
                        .clicked()
                    {
                        do_dialog = Some(DialogKind::Open);
                    }
                    if ui
                        .add_enabled(
                            self.job.is_none() && self.results.is_some(),
                            egui::Button::new("Save results…"),
                        )
                        .clicked()
                    {
                        do_dialog = Some(DialogKind::Save);
                    }
                });

                if let Some(job) = &self.job {
                    ui.horizontal(|ui| match &job.kind {
                        JobKind::Scan { phase, map, search } => {
                            let (ph, f) = if phase.load(Ordering::Relaxed) == 0 {
                                (Phase::Map, map.fraction())
                            } else {
                                (Phase::Search, search.fraction())
                            };
                            ui.add(
                                egui::ProgressBar::new(f.unwrap_or(0.0))
                                    .desired_width(220.0)
                                    .text(phase_label(ph, f)),
                            );
                            if ui.button("Cancel").clicked() {
                                do_cancel = true;
                            }
                        }
                        JobKind::Filter => {
                            ui.spinner();
                            ui.label("Filtering…");
                        }
                    });
                }

                if let Some(r) = &self.results {
                    ui.separator();
                    ui.label(format!(
                        "{} chains to 0x{:x}{}{}",
                        r.chains.len(),
                        r.target,
                        if r.truncated { ", truncated" } else { "" },
                        if r.chains.len() > self.rows.len() {
                            format!(" (showing the first {})", self.rows.len())
                        } else {
                            String::new()
                        }
                    ));
                    if let Some(path) = &self.loaded_from {
                        let s = &r.settings;
                        ui.label(
                            egui::RichText::new(format!(
                                "From {} — process {}, depth {}, max offset {:#x}, \
                                 alignment {}, result cap {} (not used for new scans)",
                                path.display(),
                                r.process,
                                s.max_depth,
                                s.max_offset,
                                s.alignment,
                                s.max_results
                            ))
                            .small()
                            .weak(),
                        );
                    }
                    let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 4.0;
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .auto_shrink([false, true])
                        .show_rows(ui, row_h, self.rows.len(), |ui, range| {
                            for i in range {
                                let text = egui::RichText::new(&self.rows[i]).monospace();
                                if ui
                                    .selectable_label(self.selected == Some(i), text)
                                    .clicked()
                                {
                                    self.selected = Some(i);
                                }
                            }
                        });
                    let can_filter = self.job.is_none() && same_process;
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.filter_addr)
                                .font(egui::TextStyle::Monospace)
                                .hint_text("new address or expression")
                                .desired_width(220.0),
                        );
                        if ui
                            .add_enabled(
                                can_filter && filter_addr_ok,
                                egui::Button::new("Filter with new address"),
                            )
                            .on_hover_text("Keep the chains that lead to this address now")
                            .clicked()
                        {
                            do_filter = Some(FilterBy::Address(self.filter_addr.clone()));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.filter_value)
                                .hint_text("value")
                                .desired_width(120.0),
                        );
                        ui.label(egui::RichText::new(self.value_ty.friendly_label()).weak());
                        if ui
                            .add_enabled(
                                can_filter && filter_value.is_some(),
                                egui::Button::new("Filter by value"),
                            )
                            .on_hover_text(
                                "Keep the chains whose target holds this value now, \
                                 as the scanner's value type",
                            )
                            .clicked()
                        {
                            if let Some(v) = filter_value.clone() {
                                do_filter = Some(FilterBy::Value(self.value_ty, v));
                            }
                        }
                    });
                    if !self.filter_addr.trim().is_empty() && !filter_addr_ok {
                        ui.label(
                            egui::RichText::new("Not an address or expression.")
                                .small()
                                .color(ui.visuals().error_fg_color),
                        );
                    }
                    let chain = self
                        .selected
                        .and_then(|i| self.order.get(i))
                        .and_then(|&i| r.chains.get(i));
                    if ui
                        .add_enabled(chain.is_some(), egui::Button::new("Add to table"))
                        .on_disabled_hover_text("Select a chain first.")
                        .clicked()
                    {
                        if let Some(c) = chain {
                            let ty = self.entry_ty.unwrap_or(self.value_ty);
                            let value = src
                                .filter(|_| same_process)
                                .and_then(|s| Self::current_value(s, c, ty))
                                .or_else(|| match ty {
                                    ScanType::Bytes(n) => Some(vec!["00"; n].join(" ")),
                                    _ => None,
                                });
                            add = Some(entry_for_chain(c, ty, value));
                        }
                    }
                }
                if !self.message.is_empty() {
                    ui.separator();
                    ui.label(&self.message);
                }
            });
        self.open = open;

        if do_cancel {
            if let Some(job) = &self.job {
                job.cancel();
            }
        }
        if do_scan {
            if let (Some(pid), Ok(s)) = (pid, settings) {
                self.start_scan(pid, s);
            }
        }
        if let (Some(by), Some(pid)) = (do_filter, pid) {
            self.start_filter(pid, by);
        }
        if let Some(kind) = do_dialog {
            self.open_dialog(kind);
        }
        if let Some((kind, dialog)) = &mut self.dialog {
            let kind = *kind;
            if let Some(outcome) = dialog.show(ctx) {
                self.last_dir = Some(dialog.dir().to_path_buf());
                self.dialog = None;
                if let Outcome::Picked(path) = outcome {
                    self.file_picked(kind, path);
                }
            }
        }
        if add.is_some() {
            self.message = "Added to the cheat table.".into();
        }
        add
    }
}

fn scan_error(e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::Interrupted {
        "Pointer scan cancelled.".into()
    } else {
        format!("Pointer scan failed: {e}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(module: &str, base: u64, offsets: &[u64]) -> PointerChain {
        PointerChain {
            module: module.into(),
            base_offset: base,
            offsets: offsets.to_vec(),
        }
    }

    fn results(chains: Vec<PointerChain>) -> PtrResults {
        PtrResults {
            process: "game.exe".into(),
            target: 0x1234,
            settings: PtrScanSettings::default(),
            chains,
            truncated: false,
        }
    }

    #[test]
    fn rows_are_expressions_shortest_first() {
        let r = results(vec![
            chain("game.exe", 0x30, &[0x8, 0x10, 0x18]),
            chain("game.exe", 0x10, &[0x40]),
            chain("lib.so", 0x20, &[0x8, 0x48]),
            chain("game.exe", 0x18, &[0x4]),
        ]);
        assert_eq!(
            chain_rows(&r, MAX_ROWS),
            vec![
                "[\"game.exe\"+0x10]+0x40",
                "[\"game.exe\"+0x18]+0x4",
                "[[\"lib.so\"+0x20]+0x8]+0x48",
                "[[[\"game.exe\"+0x30]+0x8]+0x10]+0x18",
            ]
        );
    }

    #[test]
    fn rows_stop_at_the_limit() {
        let chains = (0..1500u64)
            .map(|i| chain("m", i * 8, &[0x8, 0x10]))
            .chain(std::iter::once(chain("m", 0x9999, &[0x1])))
            .collect();
        let rows = chain_rows(&results(chains), MAX_ROWS);
        assert_eq!(rows.len(), 1000);
        assert_eq!(rows[0], "[\"m\"+0x9999]+0x1", "the short one comes first");
        assert!(chain_rows(&results(Vec::new()), MAX_ROWS).is_empty());
        assert!(chain_rows(&results(vec![chain("m", 0, &[0])]), 0).is_empty());
    }

    #[test]
    fn row_indices_point_at_the_listed_chain() {
        let r = results(vec![
            chain("m", 0x30, &[0x8, 0x10]),
            chain("m", 0x10, &[0x40]),
        ]);
        assert_eq!(shortest_first(&r, 10), vec![1, 0]);
        assert_eq!(shortest_first(&r, 1), vec![1]);
    }

    #[test]
    fn default_settings_text_is_valid() {
        let d = PtrScanSettings::default();
        let s = validate_settings("5", "0x1000", "8", "1000000").unwrap();
        assert_eq!(s, d);
    }

    #[test]
    fn settings_are_checked_like_the_cli() {
        let ok = |d, o, a, c| validate_settings(d, o, a, c);
        let s = ok("8", "0x100000", "1", "10000000").unwrap();
        assert_eq!(
            (s.max_depth, s.max_offset, s.alignment, s.max_results),
            (8, 0x100000, 1, 10_000_000)
        );
        let s = ok(" 1 ", "8", "4", "1").unwrap();
        assert_eq!((s.max_depth, s.max_offset, s.alignment), (1, 8, 4));
        assert_eq!(ok("2", "4096", "2", "1_000").unwrap().max_offset, 4096);
        assert_eq!(ok("2", "4096", "2", "1_000").unwrap().max_results, 1000);

        for (d, o, a, c) in [
            ("0", "0x1000", "8", "10"),
            ("9", "0x1000", "8", "10"),
            ("x", "0x1000", "8", "10"),
            ("5", "0x7", "8", "10"),
            ("5", "0x100001", "8", "10"),
            ("5", "zz", "8", "10"),
            ("5", "0x1000", "3", "10"),
            ("5", "0x1000", "16", "10"),
            ("5", "0x1000", "0", "10"),
            ("5", "0x1000", "8", "0"),
            ("5", "0x1000", "8", "10000001"),
            ("5", "0x1000", "8", ""),
        ] {
            assert!(ok(d, o, a, c).is_err(), "{d} {o} {a} {c} must be refused");
        }
        assert!(ok("9", "0x1000", "8", "10").unwrap_err().contains("1 to 8"));
        assert!(ok("5", "0x7", "8", "10")
            .unwrap_err()
            .contains("0x8 to 0x100000"));
        assert!(ok("5", "0x1000", "3", "10")
            .unwrap_err()
            .contains("1, 2, 4 or 8"));
        assert!(ok("5", "0x1000", "8", "0")
            .unwrap_err()
            .contains("1 to 10000000"));
    }

    #[test]
    fn phase_labels() {
        assert_eq!(phase_label(Phase::Map, None), "Reading memory…");
        assert_eq!(phase_label(Phase::Map, Some(0.5)), "Reading memory…");
        assert_eq!(phase_label(Phase::Map, Some(1.0)), "Sorting…");
        assert_eq!(phase_label(Phase::Search, Some(1.0)), "Searching chains…");
        assert_eq!(phase_label(Phase::Search, None), "Searching chains…");
    }

    #[test]
    fn hex_addresses() {
        assert_eq!(hex_address("0x1A2b"), Some(Ok(0x1a2b)));
        assert_eq!(hex_address("7ff0"), Some(Ok(0x7ff0)));
        assert_eq!(hex_address(" 0X10 "), Some(Ok(0x10)));
        assert!(hex_address("0x1ffffffffffffffff").unwrap().is_err());
        assert_eq!(hex_address("\"game\"+0x10"), None);
        assert_eq!(hex_address("[0x10]"), None);
        assert_eq!(hex_address("0x"), None);
        assert_eq!(hex_address(""), None);
    }

    #[test]
    fn add_to_table_entry() {
        let c = chain("game.exe", 0x1a2b30, &[0x48, 0x10]);
        let e = entry_for_chain(&c, ScanType::F32, Some("1.5".into()));
        assert_eq!(e.description, "Pointer chain");
        assert_eq!(e.address, c.to_expr());
        assert_eq!(e.address.to_string(), "[[\"game.exe\"+0x1a2b30]+0x48]+0x10");
        assert_eq!(e.ty, ScanType::F32);
        assert_eq!(e.value, "1.5");
        assert!(!e.frozen);

        let e = entry_for_chain(&c, ScanType::I32, None);
        assert_eq!(e.value, "0");
    }
}

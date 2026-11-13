//! The cheat wizard window: renders [`Wizard`]'s steps and runs its backend
//! calls on a worker thread.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cheatu_core::pointer_scan::{find_chains, PointerChain, PointerMap, PtrScanSettings};
use cheatu_core::process::ProcInfo;
use cheatu_core::scan::{FirstScan, NextScan, ScanProgress, Scanner};
use cheatu_core::table::TableEntry;
use cheatu_core::{
    list_processes, probe_address, AddrExpr, LiveSource, Mem, MemSource, ProbeOutcome, ScanType,
    ScanValue,
};
use eframe::egui;

use crate::cheat_table::{module_offset_expr, savable_value, value_text};
use crate::pointer_window::{phase_label, resume_if_paused, PauseSlot, Paused, Phase};
use crate::wizard::{Input, Step, VerifyPhase, Wizard, WizardBackend, MAX_CANDIDATES, MAX_CHAINS};

/// How often `WaitRestart` looks for the restarted game.
pub const RESTART_POLL: Duration = Duration::from_secs(2);

/// The value a test write puts in: the current value + 1 (wrapping for
/// integers, + 1.0 for floats). `None` for byte arrays.
pub fn plus_one(v: &ScanValue) -> Option<ScanValue> {
    Some(match *v {
        ScanValue::I8(x) => ScanValue::I8(x.wrapping_add(1)),
        ScanValue::U8(x) => ScanValue::U8(x.wrapping_add(1)),
        ScanValue::I16(x) => ScanValue::I16(x.wrapping_add(1)),
        ScanValue::U16(x) => ScanValue::U16(x.wrapping_add(1)),
        ScanValue::I32(x) => ScanValue::I32(x.wrapping_add(1)),
        ScanValue::U32(x) => ScanValue::U32(x.wrapping_add(1)),
        ScanValue::I64(x) => ScanValue::I64(x.wrapping_add(1)),
        ScanValue::U64(x) => ScanValue::U64(x.wrapping_add(1)),
        ScanValue::F32(x) => ScanValue::F32(x + 1.0),
        ScanValue::F64(x) => ScanValue::F64(x + 1.0),
        ScanValue::Bytes(_) => return None,
    })
}

/// The pids already called `name` when the wizard starts waiting for a
/// restart, plus `old_pid`, sorted. None of them counts as the restarted
/// game: multi-process games (NW.js/Electron renderers, Proton helpers) run
/// several processes with the same name.
pub fn known_pids(procs: &[ProcInfo], name: &str, old_pid: i32) -> Vec<i32> {
    let mut pids: Vec<i32> = procs
        .iter()
        .filter(|p| p.name == name)
        .map(|p| p.pid)
        .chain(std::iter::once(old_pid))
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// The pid of the restarted game: once the old process (`old_pid`) is gone,
/// a process called `name` that is not in `known` (see [`known_pids`]); the
/// newest (highest pid) when there are several.
pub fn restarted_pid(procs: &[ProcInfo], name: &str, old_pid: i32, known: &[i32]) -> Option<i32> {
    if procs.iter().any(|p| p.pid == old_pid) {
        return None;
    }
    procs
        .iter()
        .filter(|p| p.name == name && !known.contains(&p.pid))
        .map(|p| p.pid)
        .max()
}

/// Keep the chains that land on any of `addrs`; returns how many remain.
pub fn keep_landing(
    chains: &mut Vec<PointerChain>,
    addrs: &[u64],
    resolve: impl Fn(&PointerChain) -> Option<u64>,
) -> usize {
    chains.retain(|c| resolve(c).is_some_and(|a| addrs.contains(&a)));
    chains.len()
}

/// Whether `WaitRestart` should look for the game again.
pub fn poll_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= RESTART_POLL)
}

/// What the worker is doing, for the "Working…" line.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Scan,
    Map,
    Search,
    Probe,
    Filter,
    Attach,
}

impl Stage {
    const ALL: [Stage; 7] = [
        Stage::Idle,
        Stage::Scan,
        Stage::Map,
        Stage::Search,
        Stage::Probe,
        Stage::Filter,
        Stage::Attach,
    ];
}

/// Shared between the window and the worker: progress, cancel and pause.
#[derive(Default)]
pub struct Activity {
    stage: AtomicU8,
    /// The current scanner's progress (a new one after a re-attach).
    scan: Mutex<Arc<ScanProgress>>,
    map: Arc<ScanProgress>,
    search: Arc<ScanProgress>,
    /// Mirrors "pause target while scanning".
    pause: AtomicBool,
    /// The pid while a wizard scan has it suspended.
    paused: PauseSlot,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Activity {
    fn set(&self, s: Stage) {
        self.stage.store(s as u8, Ordering::Relaxed);
    }

    fn stage(&self) -> Stage {
        let i = self.stage.load(Ordering::Relaxed) as usize;
        Stage::ALL.get(i).copied().unwrap_or(Stage::Idle)
    }

    /// The progress of the running stage, when it can be cancelled.
    fn progress(&self) -> Option<Arc<ScanProgress>> {
        match self.stage() {
            Stage::Scan => Some(lock(&self.scan).clone()),
            Stage::Map => Some(self.map.clone()),
            Stage::Search => Some(self.search.clone()),
            _ => None,
        }
    }

    /// Cancel only the running stage: a cancel flag left on an idle one
    /// would stop its next run at once.
    fn cancel(&self) {
        if let Some(p) = self.progress() {
            p.cancel();
        }
    }
}

/// [`WizardBackend`] over the attached process: its own [`Scanner`] and
/// [`LiveSource`], so the main scan results are untouched.
pub struct RealBackend {
    scanner: Scanner,
    mem: Mem,
    live: LiveSource,
    process: String,
    activity: Arc<Activity>,
    /// The type of the last first scan.
    ty: ScanType,
    /// Values replaced by `poke`, for `restore`.
    poked: HashMap<u64, ScanValue>,
    chains: Vec<PointerChain>,
    /// Pids that already carried the process name when the wizard started
    /// waiting for a restart; recorded after each pointer scan and filter,
    /// the steps that lead to `WaitRestart`.
    known: Vec<i32>,
}

impl RealBackend {
    pub fn new(pid: i32, process: String, activity: Arc<Activity>) -> io::Result<RealBackend> {
        let scanner = Scanner::new(pid)?;
        let mem = Mem::open(pid)?;
        let live = LiveSource::open(pid)?;
        *lock(&activity.scan) = scanner.progress();
        Ok(RealBackend {
            scanner,
            mem,
            live,
            process,
            activity,
            ty: ScanType::I32,
            poked: HashMap::new(),
            chains: Vec::new(),
            known: Vec::new(),
        })
    }

    pub fn pid(&self) -> i32 {
        self.scanner.pid()
    }

    /// Suspend the target for a snapshot when "pause target while scanning"
    /// is on; resumed when the guard drops (or on app exit).
    fn pause_guard(&self) -> Option<Paused> {
        if !self.activity.pause.load(Ordering::Relaxed) {
            return None;
        }
        Paused::new(
            &self.activity.paused,
            self.pid(),
            cheatu_core::suspend,
            cheatu_core::resume,
        )
    }

    fn refresh(&mut self) {
        // A stale map only makes module names less fresh; keep going.
        let _ = self.live.refresh_regions();
    }

    fn read(&self, addr: u64, ty: ScanType) -> Option<ScanValue> {
        self.scanner.read_typed(addr, ty).ok()
    }

    /// The type a candidate is tracked as.
    /// Record the pids that are not a restart of the game (see
    /// [`known_pids`]).
    fn record_known(&mut self) {
        self.known = known_pids(&list_processes(), &self.process, self.pid());
    }

    fn ty_at(&self, addr: u64) -> ScanType {
        self.scanner
            .results()
            .iter()
            .find(|c| c.addr == addr)
            .map(|c| c.ty())
            .unwrap_or(self.ty)
    }
}

impl WizardBackend for RealBackend {
    fn first_scan(&mut self, ty: ScanType, value: &str) -> io::Result<usize> {
        self.activity.set(Stage::Scan);
        self.ty = ty;
        self.scanner.reset();
        {
            let _paused = self.pause_guard();
            self.scanner.first_scan(FirstScan::Value {
                value: value.trim().to_string(),
                types: vec![ty],
            })?;
        }
        self.refresh();
        Ok(self.scanner.count())
    }

    fn next_scan(&mut self, cmp: NextScan, value: Option<&str>) -> io::Result<usize> {
        self.activity.set(Stage::Scan);
        {
            let _paused = self.pause_guard();
            self.scanner.next_scan(cmp, value.map(str::trim))?;
        }
        self.refresh();
        Ok(self.scanner.count())
    }

    fn candidates(&self) -> Vec<u64> {
        self.scanner.results().iter().map(|c| c.addr).collect()
    }

    fn probe(&mut self, addr: u64) -> io::Result<ProbeOutcome> {
        self.activity.set(Stage::Probe);
        probe_address(&self.mem, addr, self.ty_at(addr).size(), crate::PROBE_WAIT)
    }

    fn poke(&mut self, addr: u64) -> io::Result<()> {
        self.activity.set(Stage::Probe);
        let cur = self.scanner.read_typed(addr, self.ty_at(addr))?;
        let new =
            plus_one(&cur).ok_or_else(|| io::Error::other("this type cannot take a test value"))?;
        self.scanner.write_value(addr, &new)?;
        self.poked.insert(addr, cur);
        Ok(())
    }

    fn restore(&mut self, addr: u64) -> io::Result<()> {
        let old = self
            .poked
            .remove(&addr)
            .ok_or_else(|| io::Error::other("no test value to undo there"))?;
        self.scanner.write_value(addr, &old)
    }

    fn module_offset(&self, addr: u64) -> Option<AddrExpr> {
        module_offset_expr(self.live.regions(), addr)
    }

    fn pointer_scan(&mut self, addr: u64) -> io::Result<usize> {
        self.activity.set(Stage::Map);
        self.refresh();
        let settings = PtrScanSettings::default();
        let map = {
            let _paused = self.pause_guard();
            PointerMap::build(&self.live, &settings, &self.activity.map)?
        };
        self.activity.set(Stage::Search);
        let found = find_chains(&map, &self.live, addr, &settings, &self.activity.search)?;
        self.chains = found.chains;
        self.record_known();
        Ok(self.chains.len())
    }

    fn filter_chains(&mut self, addrs: &[u64]) -> io::Result<usize> {
        self.activity.set(Stage::Filter);
        self.refresh();
        let live = &self.live;
        let n = keep_landing(&mut self.chains, addrs, |c| c.resolve(live).ok());
        self.record_known();
        Ok(n)
    }

    /// Shortest chain first, so the wizard saves the simplest one.
    fn chains(&self) -> Vec<PointerChain> {
        let mut c = self.chains.clone();
        c.sort_by_key(|c| c.offsets.len());
        c
    }

    fn process_running(&self, name: &str) -> Option<i32> {
        restarted_pid(&list_processes(), name, self.pid(), &self.known)
    }

    fn reattach(&mut self, pid: i32) -> io::Result<()> {
        self.activity.set(Stage::Attach);
        let scanner = Scanner::new(pid)?;
        let mem = Mem::open(pid)?;
        let live = LiveSource::open(pid)?;
        *lock(&self.activity.scan) = scanner.progress();
        self.scanner = scanner;
        self.mem = mem;
        self.live = live;
        self.poked.clear();
        Ok(())
    }

    fn process_name(&self) -> Option<String> {
        Some(self.process.clone())
    }
}

struct Inner {
    wizard: Wizard,
    backend: RealBackend,
}

/// What the window draws, copied out after each backend call so the UI
/// never waits for the worker.
#[derive(Clone)]
struct View {
    step: Step,
    error: Option<String>,
    session_only: bool,
    can_keep_plain: bool,
    ty: ScanType,
    pid: i32,
    /// The address chosen so far.
    chosen: Option<u64>,
    /// Current values of the candidates listed in `Verify`.
    values: Vec<Option<String>>,
    /// The live value at the `Save` step's address.
    current: Option<String>,
    chains: usize,
}

fn view_of(inner: &mut Inner) -> View {
    let ty = inner.wizard.value_type();
    let b = &mut inner.backend;
    let text = |b: &RealBackend, a: u64| b.read(a, ty).map(|v| value_text(&v));
    let values = match &inner.wizard.step {
        Step::Verify { remaining, .. } => remaining.iter().map(|&a| text(b, a)).collect(),
        _ => Vec::new(),
    };
    let current = match &inner.wizard.step {
        Step::Save { expr } => {
            b.refresh();
            expr.resolve(&b.live).ok().and_then(|a| text(b, a))
        }
        _ => None,
    };
    View {
        step: inner.wizard.step.clone(),
        error: inner.wizard.error.clone(),
        session_only: inner.wizard.is_session_only(),
        can_keep_plain: inner.wizard.can_keep_plain_address(),
        ty,
        pid: b.pid(),
        chosen: inner.wizard.chosen_address(),
        values,
        current,
        chains: b.chains.len(),
    }
}

/// Put back a test value still in the game (the user started over or
/// closed the wizard while being asked about it).
fn undo_test_write(inner: &mut Inner) {
    if let Step::Verify {
        phase: VerifyPhase::Testing { current, .. },
        ..
    } = inner.wizard.step
    {
        let _ = inner.backend.restore(current);
    }
}

/// How long closing or exiting waits for a cancelled worker.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

/// After a cancel: wait up to `limit` for the worker (`handle`, if any) to
/// finish, so its pointer map is dropped and its test write, if it made one,
/// is in the step; then put the test value back. Returns whether the wizard
/// state could be reached (the worker finished and the lock was free).
fn finish_then_undo(handle: Option<JoinHandle<()>>, inner: &Mutex<Inner>, limit: Duration) -> bool {
    if let Some(handle) = handle {
        let deadline = Instant::now() + limit;
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if !handle.is_finished() {
            return false;
        }
        let _ = handle.join();
    }
    match inner.try_lock() {
        Ok(mut g) => {
            undo_test_write(&mut g);
            true
        }
        Err(std::sync::TryLockError::Poisoned(p)) => {
            undo_test_write(&mut p.into_inner());
            true
        }
        Err(std::sync::TryLockError::WouldBlock) => false,
    }
}

/// One backend call in flight.
struct Job {
    rx: Receiver<View>,
    handle: JoinHandle<()>,
    /// A background "is the game back?" poll: the step stays on screen.
    quiet: bool,
}

/// What the app must do after a frame of the wizard.
pub enum WizardEvent {
    /// Add this entry to the cheat table.
    Save(TableEntry),
    /// The wizard attached to the restarted game; the app should follow.
    Reattached(i32),
    /// Leave the guided steps for the full scanner, with an unknown-value
    /// search ready when `unknown`.
    ScanManually { unknown: bool },
}

/// The guided steps, drawn in the main area.
pub struct WizardWindow {
    /// The name of the cheat just added, while "Find another value" shows.
    saved: Option<String>,
    inner: Arc<Mutex<Inner>>,
    activity: Arc<Activity>,
    view: View,
    job: Option<Job>,
    /// Type picked on the first step.
    ty: ScanType,
    value: String,
    name: String,
    save_value: String,
    freeze: bool,
    /// The Save form was filled for this expression.
    form_for: Option<AddrExpr>,
    /// The chosen address was found by a test write and still holds it.
    tested: bool,
    last_poll: Option<Instant>,
}

impl Drop for WizardWindow {
    /// Closing the wizard (or exiting) stops its work, never leaves the game
    /// paused, and puts back a test value it is still asking about.
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl WizardWindow {
    /// Start the wizard on the attached process `pid` called `process`.
    pub fn new(pid: i32, process: String) -> io::Result<WizardWindow> {
        let activity = Arc::new(Activity::default());
        let backend = RealBackend::new(pid, process, activity.clone())?;
        let ty = ScanType::I32;
        let mut inner = Inner {
            wizard: Wizard::new(ty),
            backend,
        };
        let view = view_of(&mut inner);
        Ok(WizardWindow {
            saved: None,
            inner: Arc::new(Mutex::new(inner)),
            activity,
            view,
            job: None,
            ty,
            value: String::new(),
            name: String::new(),
            save_value: String::new(),
            freeze: true,
            form_for: None,
            tested: false,
            last_poll: None,
        })
    }

    /// Where the guided steps are on the step bar.
    pub fn step(&self) -> u8 {
        if self.saved.is_some() {
            3
        } else {
            guided_step(&self.view.step)
        }
    }

    /// Mirrors "pause target while scanning". Set every frame.
    pub fn set_pause(&self, on: bool) {
        self.activity.pause.store(on, Ordering::Relaxed);
    }

    /// Working, or waiting for the game to restart: keep repainting.
    pub fn needs_repaint(&self) -> bool {
        self.job.is_some() || matches!(self.view.step, Step::WaitRestart { .. })
    }

    /// The pid the wizard works on (it follows a restarted game).
    pub fn pid(&self) -> i32 {
        self.view.pid
    }

    /// Closing or exiting: cancel the work and resume a paused target, wait
    /// up to [`SHUTDOWN_WAIT`] for the worker (so a pointer map's temp files
    /// are removed), then put back a test value still in the game.
    pub fn shutdown(&mut self) {
        self.activity.cancel();
        resume_if_paused(&self.activity.paused, cheatu_core::resume);
        let handle = self.job.take().map(|j| j.handle);
        finish_then_undo(handle, &self.inner, SHUTDOWN_WAIT);
        // A worker that paused only after the resume above.
        resume_if_paused(&self.activity.paused, cheatu_core::resume);
    }

    fn dispatch(&mut self, input: Input, quiet: bool) {
        if self.job.is_some() {
            return;
        }
        if matches!(input, Input::StartOver) {
            self.tested = false;
            self.value.clear();
        }
        let inner = self.inner.clone();
        let ty = self.ty;
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut g = lock(&inner);
            if matches!(input, Input::StartOver) {
                undo_test_write(&mut g);
            }
            // The type can only change before the first scan.
            if g.wizard.step == Step::AskValue && g.wizard.value_type() != ty {
                g.wizard = Wizard::new(ty);
            }
            let Inner { wizard, backend } = &mut *g;
            wizard.handle(input, backend);
            backend.activity.set(Stage::Idle);
            let _ = tx.send(view_of(&mut g));
        });
        self.job = Some(Job { rx, handle, quiet });
    }

    /// Take a finished call's result.
    fn poll(&mut self, events: &mut Vec<WizardEvent>) {
        let Some(job) = &self.job else {
            return;
        };
        match job.rx.try_recv() {
            Ok(view) => {
                self.job = None;
                if view.pid != self.view.pid {
                    events.push(WizardEvent::Reattached(view.pid));
                }
                if view.step != self.view.step {
                    self.value.clear();
                }
                if matches!(
                    view.step,
                    Step::AskValue | Step::WaitRestart { .. } | Step::Refind { .. }
                ) {
                    self.tested = false;
                }
                self.view = view;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.job = None;
                self.view.error =
                    Some("The wizard stopped unexpectedly. You can start over.".into());
            }
        }
    }

    /// Draw the current step in `ui` and return what the app must do.
    pub fn show_in(&mut self, ui: &mut egui::Ui) -> Vec<WizardEvent> {
        let mut events = Vec::new();
        self.poll(&mut events);

        if self.job.is_none()
            && matches!(self.view.step, Step::WaitRestart { .. })
            && poll_due(self.last_poll, Instant::now())
        {
            self.last_poll = Some(Instant::now());
            self.dispatch(Input::Restarted, true);
        }
        if let Step::Save { expr } = &self.view.step {
            if self.form_for.as_ref() != Some(expr) {
                self.form_for = Some(expr.clone());
                self.save_value = self.view.current.clone().unwrap_or_else(|| "0".into());
            }
        }

        let mut input = None;
        let mut save = false;
        if let Some(name) = self.saved.clone() {
            ui.heading(format!("✔ Added {name} to My cheats"));
            ui.label("Freeze or change it on the right. Want another cheat?");
            ui.add_space(6.0);
            if ui.button("Find another value").clicked() {
                self.saved = None;
                self.name.clear();
                input = Some(Input::StartOver);
            }
        } else if self.job.as_ref().is_some_and(|j| !j.quiet) {
            self.working_ui(ui);
        } else {
            self.step_ui(ui, &mut input, &mut save, &mut events);
            if let Some(e) = &self.view.error {
                ui.label(
                    egui::RichText::new(e)
                        .small()
                        .color(ui.visuals().error_fg_color),
                );
            }
            if self.view.step != Step::AskValue {
                ui.separator();
                if ui
                    .add_enabled(self.job.is_none(), egui::Button::new("Start over"))
                    .clicked()
                {
                    input = Some(Input::StartOver);
                }
            }
        }

        if save {
            if let Step::Save { expr } = &self.view.step {
                let mut description = self.name.trim().to_string();
                if self.view.session_only {
                    description.push_str(" (this session only)");
                }
                events.push(WizardEvent::Save(TableEntry {
                    description,
                    address: expr.clone(),
                    ty: self.view.ty,
                    value: self.save_value.trim().to_string(),
                    frozen: self.freeze,
                }));
                self.saved = Some(self.name.trim().to_string());
            }
        }
        if let Some(i) = input {
            if matches!(
                (&self.view.step, &i),
                (
                    Step::Verify {
                        phase: VerifyPhase::Testing { .. },
                        ..
                    },
                    Input::Confirm(true)
                )
            ) {
                self.tested = true;
            }
            self.dispatch(i, false);
        }
        events
    }

    fn working_ui(&self, ui: &mut egui::Ui) {
        let progress = self.activity.progress();
        let f = progress.as_ref().and_then(|p| p.fraction());
        let label = match self.activity.stage() {
            Stage::Scan => "Scanning memory…",
            Stage::Map => phase_label(Phase::Map, f),
            Stage::Search => phase_label(Phase::Search, f),
            Stage::Probe => "Writing test values…",
            Stage::Filter => "Checking the pointer chains…",
            Stage::Attach => "Attaching to the restarted game…",
            Stage::Idle => "Working…",
        };
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!("Working… {label}"));
        });
        if let Some(p) = progress {
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(f.unwrap_or(0.0))
                        .desired_width(240.0)
                        .show_percentage(),
                );
                if ui.button("Cancel").clicked() {
                    p.cancel();
                }
            });
        }
    }

    /// A value field with a button; `Some` when submitted (button or Enter).
    fn value_row(&mut self, ui: &mut egui::Ui, button: &str) -> Option<Input> {
        let mut go = false;
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.value)
                    .hint_text("value")
                    .desired_width(140.0),
            );
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                go = true;
            }
            ui.label(egui::RichText::new(self.view.ty.friendly_label()).weak());
            if ui
                .add_enabled(!self.value.trim().is_empty(), egui::Button::new(button))
                .clicked()
            {
                go = true;
            }
        });
        (go && !self.value.trim().is_empty()).then(|| Input::Value(self.value.trim().to_string()))
    }

    /// Increased / decreased / unchanged, for values without a number.
    fn changed_buttons(ui: &mut egui::Ui) -> Option<Input> {
        let mut out = None;
        ui.label(
            egui::RichText::new("No number on screen (a bar, a gauge)? Say how it changed:")
                .small()
                .weak(),
        );
        ui.horizontal(|ui| {
            for (label, cmp) in [
                ("Increased", NextScan::Increased),
                ("Decreased", NextScan::Decreased),
                ("Unchanged", NextScan::Unchanged),
            ] {
                if ui.button(label).clicked() {
                    out = Some(Input::Changed(cmp));
                }
            }
        });
        out
    }

    fn candidate_list(&self, ui: &mut egui::Ui, remaining: &[u64], pick: bool) -> Option<Input> {
        let mut out = None;
        egui::Grid::new("wizard_candidates")
            .num_columns(3)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                for (i, a) in remaining.iter().enumerate() {
                    ui.monospace(format!("0x{a:012x}"));
                    let v = self.view.values.get(i).cloned().flatten();
                    ui.label(v.unwrap_or_else(|| "unreadable".into()));
                    if pick && ui.button("Use this").clicked() {
                        out = Some(Input::Pick(i));
                    }
                    ui.end_row();
                }
            });
        out
    }

    fn session_only_warning(ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new(
                "The plain address may not be the real one, and the cheat won't survive a \
                 restart of the game: it is for this session only.",
            )
            .small()
            .color(ui.visuals().warn_fg_color),
        );
    }

    fn tested_note(&self, ui: &mut egui::Ui) {
        if self.tested {
            ui.label(
                egui::RichText::new("This address now holds your value + 1 — that was the test.")
                    .small()
                    .weak(),
            );
        }
    }

    fn step_ui(
        &mut self,
        ui: &mut egui::Ui,
        input: &mut Option<Input>,
        save: &mut bool,
        events: &mut Vec<WizardEvent>,
    ) {
        let step = self.view.step.clone();
        match step {
            Step::AskValue => {
                ui.heading("What value do you want to change?");
                ui.label("Type the number exactly as the game shows it, e.g. your rupees.");
                ui.horizontal(|ui| {
                    ui.label("Kind of number:");
                    egui::ComboBox::from_id_salt("wizard_type")
                        .selected_text(kind_label(self.ty))
                        .show_ui(ui, |ui| {
                            for t in GUIDED_KINDS {
                                ui.selectable_value(&mut self.ty, t, kind_label(t));
                            }
                        });
                });
                // The type shown next to the field is the one being picked.
                self.view.ty = self.ty;
                *input = self.value_row(ui, "Search");
                ui.label(
                    egui::RichText::new(
                        "Most counters are 4 Bytes; if the value is not found, try 2 Bytes \
                         or Float.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Not a number on screen (a bar, a gauge)?").weak(),
                    );
                    if ui.link("Search for an unknown value").clicked() {
                        events.push(WizardEvent::ScanManually { unknown: true });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Already know how this works?").weak());
                    if ui.link("Scan manually").clicked() {
                        events.push(WizardEvent::ScanManually { unknown: false });
                    }
                });
            }
            Step::Narrowing { count } => {
                ui.heading("Narrow it down");
                ui.label(format!(
                    "{count} places in memory hold that value. Change it in the game (spend or \
                     earn some), then enter the new value. This repeats until at most \
                     {MAX_CANDIDATES} are left."
                ));
                *input = self.value_row(ui, "Narrow down");
                if let Some(i) = Self::changed_buttons(ui) {
                    *input = Some(i);
                }
            }
            Step::Verify { remaining, phase } => {
                ui.heading("Find the real one");
                match phase {
                    VerifyPhase::Consent => {
                        ui.label(format!(
                            "{} places are left; usually only one of them is the value the \
                             game really uses.",
                            remaining.len()
                        ));
                        ui.label(
                            "cheatu can find it by writing test values: first it drops the \
                             copies the game overwrites at once, then it adds 1 to each \
                             remaining address in turn and asks whether the game shows the \
                             change. Allow these test writes?",
                        );
                        self.candidate_list(ui, &remaining, false);
                        ui.horizontal(|ui| {
                            if ui.button("Yes, test them").clicked() {
                                *input = Some(Input::Confirm(true));
                            }
                            if ui.button("No, I'll pick one").clicked() {
                                *input = Some(Input::Confirm(false));
                            }
                        });
                    }
                    VerifyPhase::Pick => {
                        ui.label("Pick the one to use:");
                        *input = self.candidate_list(ui, &remaining, true);
                    }
                    VerifyPhase::Testing { current, left } => {
                        ui.label(format!(
                            "cheatu wrote the current value + 1 to 0x{current:x}. Look at \
                             the game: did the value change? (Some games only update the \
                             screen after something happens, like opening a menu.)"
                        ));
                        ui.horizontal(|ui| {
                            if ui.button("Yes, it changed").clicked() {
                                *input = Some(Input::Confirm(true));
                            }
                            let no = if left.is_empty() {
                                "No".to_string()
                            } else {
                                format!("No, try the next one ({} left)", left.len())
                            };
                            if ui.button(no).clicked() {
                                *input = Some(Input::Confirm(false));
                            }
                        });
                    }
                }
            }
            Step::MakeLasting => {
                ui.heading("Make it last");
                self.tested_note(ui);
                if let Some(a) = self.view.chosen {
                    ui.horizontal(|ui| {
                        ui.label("Found it at");
                        ui.monospace(format!("0x{a:x}"));
                    });
                }
                ui.label(
                    "This address is in memory that moves when the game restarts. The \
                     pointer scanner can find a path to it from the game's own code and \
                     data that survives restarts. It reads all of the game's memory, which \
                     can take a minute.",
                );
                if self.activity.pause.load(Ordering::Relaxed) {
                    ui.label(
                        egui::RichText::new("The game is paused while its memory is read.")
                            .small()
                            .weak(),
                    );
                }
                if ui.button("Run the pointer scan").clicked() {
                    *input = Some(Input::Confirm(true));
                }
                ui.add_space(4.0);
                Self::session_only_warning(ui);
                if ui
                    .button("Keep the plain address (this session only)")
                    .clicked()
                {
                    *input = Some(Input::Confirm(false));
                }
            }
            Step::WaitRestart { process } => {
                ui.heading("Make it last: restart the game");
                ui.label(format!(
                    "{} pointer chains lead to the value. Now restart the game: quit \
                     {process} completely, start it again and load your save. cheatu stays \
                     open and attaches to it when it is back.",
                    self.view.chains
                ));
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Waiting for {process} to start again…"));
                });
                ui.label(
                    egui::RichText::new(format!(
                        "Each restart drops chains that broke; the wizard stops when at most \
                         {MAX_CHAINS} remain."
                    ))
                    .small()
                    .weak(),
                );
                if ui
                    .add_enabled(
                        self.view.chains > 0 && self.job.is_none(),
                        egui::Button::new("Stop and keep the best chain"),
                    )
                    .clicked()
                {
                    *input = Some(Input::Stop);
                }
            }
            Step::Refind { count } => {
                ui.heading("Make it last: find the value again");
                if count == 0 {
                    ui.label("The game is back. What is the value now?");
                    *input = self.value_row(ui, "Search");
                } else {
                    ui.label(format!(
                        "{count} places in memory hold that value. Change it in the game, then \
                         enter the new value."
                    ));
                    *input = self.value_row(ui, "Narrow down");
                    if let Some(i) = Self::changed_buttons(ui) {
                        *input = Some(i);
                    }
                }
                if ui
                    .add_enabled(
                        self.view.chains > 0,
                        egui::Button::new("Stop and keep the best chain"),
                    )
                    .clicked()
                {
                    *input = Some(Input::Stop);
                }
            }
            Step::Save { expr } => {
                ui.heading("Save it");
                self.tested_note(ui);
                ui.horizontal(|ui| {
                    ui.label("Address:");
                    ui.monospace(expr.to_string());
                });
                if self.view.session_only {
                    ui.label(
                        egui::RichText::new(
                            "Session only: this cheat is a plain address and will not work \
                             after the game restarts.",
                        )
                        .color(ui.visuals().warn_fg_color),
                    );
                }
                let ty = self.view.ty;
                egui::Grid::new("wizard_save")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Name");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.name)
                                .hint_text("e.g. Rupees")
                                .desired_width(200.0),
                        );
                        ui.end_row();
                        ui.label("Value");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.save_value)
                                    .desired_width(120.0),
                            );
                            ui.label(egui::RichText::new(ty.friendly_label()).weak());
                        });
                        ui.end_row();
                    });
                ui.radio_value(&mut self.freeze, true, "Keep it frozen at this value");
                ui.radio_value(
                    &mut self.freeze,
                    false,
                    "Save the value without freezing (use Set in My cheats)",
                );
                let blocker = if self.name.trim().is_empty() {
                    Some("Give the cheat a name.")
                } else if !savable_value(ty, self.save_value.trim()) {
                    Some("Not a valid value for this type.")
                } else {
                    None
                };
                if ui
                    .add_enabled(blocker.is_none(), egui::Button::new("Add to My cheats"))
                    .on_disabled_hover_text(blocker.unwrap_or(""))
                    .clicked()
                {
                    *save = true;
                }
            }
            Step::Failed(msg) => {
                ui.heading("The wizard could not finish");
                self.tested_note(ui);
                ui.label(egui::RichText::new(msg).color(ui.visuals().error_fg_color));
                if self.view.can_keep_plain {
                    Self::session_only_warning(ui);
                    if ui
                        .button("Keep the plain address (this session only)")
                        .clicked()
                    {
                        *input = Some(Input::KeepPlainAddress);
                    }
                }
            }
        }
    }
}

/// The kinds of number the guided steps offer, most common first. One
/// type per label: signedness is not something to ask a beginner.
pub const GUIDED_KINDS: [ScanType; 6] = [
    ScanType::I32,
    ScanType::I16,
    ScanType::U8,
    ScanType::I64,
    ScanType::F32,
    ScanType::F64,
];

fn kind_label(t: ScanType) -> String {
    t.friendly_label()
}

/// Where the guided steps are on the step bar: 2 while finding the value, 3
/// from checking it onwards.
pub fn guided_step(step: &Step) -> u8 {
    match step {
        Step::AskValue | Step::Narrowing { .. } => 2,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_label_has_no_rust_type_names() {
        let labels: Vec<String> = GUIDED_KINDS.iter().map(|&t| kind_label(t)).collect();
        assert_eq!(
            labels,
            ["4 Bytes", "2 Bytes", "Byte", "8 Bytes", "Float", "Double"]
        );
        for l in &labels {
            for rust in [
                "i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64", "f32", "f64",
            ] {
                assert!(!l.contains(rust), "{l}");
            }
        }
    }

    #[test]
    fn step_bar_position() {
        assert_eq!(guided_step(&Step::AskValue), 2);
        assert_eq!(guided_step(&Step::Narrowing { count: 9 }), 2);
        assert_eq!(guided_step(&Step::MakeLasting), 3);
        assert_eq!(guided_step(&Step::Failed("x".into())), 3);
    }
    use cheatu_core::scan::NextScan;
    use cheatu_core::ScanType;
    use std::sync::Arc;

    fn proc(pid: i32, name: &str) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.into(),
            cmdline: String::new(),
            rss_bytes: 0,
            is_wine: false,
            role: None,
            accessible: true,
        }
    }

    #[test]
    fn poke_value_is_current_plus_one() {
        let p = |v| plus_one(&v).map(|v| v.to_string());
        assert_eq!(p(ScanValue::I32(5)).as_deref(), Some("6"));
        assert_eq!(p(ScanValue::I16(-1)).as_deref(), Some("0"));
        assert_eq!(p(ScanValue::U8(255)).as_deref(), Some("0"), "wraps");
        assert_eq!(p(ScanValue::I8(127)).as_deref(), Some("-128"), "wraps");
        assert_eq!(p(ScanValue::U64(u64::MAX)).as_deref(), Some("0"));
        assert_eq!(p(ScanValue::F32(1.5)).as_deref(), Some("2.5"));
        assert_eq!(p(ScanValue::F64(-0.25)).as_deref(), Some("0.75"));
        assert!(plus_one(&ScanValue::Bytes(vec![1, 2])).is_none());
        assert!(matches!(
            plus_one(&ScanValue::U16(7)),
            Some(ScanValue::U16(8))
        ));
    }

    #[test]
    fn restart_ignores_siblings_and_waits_for_the_old_game_to_exit() {
        let at_wait = [proc(10, "game"), proc(11, "other"), proc(12, "game")];
        // Recorded when the wizard starts waiting: the old game and its
        // same-named sibling (a renderer, a Proton helper).
        let known = known_pids(&at_wait, "game", 10);
        assert_eq!(known, vec![10, 12]);
        assert_eq!(
            known_pids(&at_wait[..2], "game", 10),
            vec![10],
            "old pid always in"
        );

        // Nothing changed: the sibling is not a restart.
        assert_eq!(restarted_pid(&at_wait, "game", 10, &known), None);
        // The old game is still alive: no restart, even with a new process.
        let both = [proc(10, "game"), proc(12, "game"), proc(40, "game")];
        assert_eq!(restarted_pid(&both, "game", 10, &known), None);
        // The old game is gone and a new one is up: that one.
        let after = [proc(12, "game"), proc(40, "game"), proc(41, "other")];
        assert_eq!(restarted_pid(&after, "game", 10, &known), Some(40));
        // Old gone, only the sibling left: keep waiting.
        assert_eq!(restarted_pid(&after[..1], "game", 10, &known), None);
        assert_eq!(restarted_pid(&after, "gam", 10, &known), None, "exact name");
        // Several new ones: the newest (highest pid).
        let many = [proc(12, "game"), proc(50, "game"), proc(45, "game")];
        assert_eq!(restarted_pid(&many, "game", 10, &known), Some(50));
    }

    fn chain(off: u64) -> PointerChain {
        PointerChain {
            module: "m".into(),
            base_offset: 0x10,
            offsets: vec![off],
        }
    }

    #[test]
    fn keep_landing_keeps_chains_on_any_address() {
        let mut chains = vec![chain(1), chain(2), chain(3), chain(4)];
        // Each chain lands on 0x1000 + its offset; offset 4 does not resolve.
        let n = keep_landing(&mut chains, &[0x1001, 0x1003, 0x9999], |c| {
            (c.offsets[0] != 4).then(|| 0x1000 + c.offsets[0])
        });
        assert_eq!(n, 2);
        assert_eq!(chains, vec![chain(1), chain(3)]);
        assert_eq!(keep_landing(&mut chains, &[], |_| Some(0x1001)), 0);
        assert!(chains.is_empty());
    }

    #[test]
    fn restart_poll_every_two_seconds() {
        let t = Instant::now();
        assert!(poll_due(None, t), "the first look is immediate");
        assert!(!poll_due(Some(t), t));
        assert!(!poll_due(Some(t), t + Duration::from_millis(1999)));
        assert!(poll_due(Some(t), t + RESTART_POLL));
    }

    // The real backend against this test process's own memory.

    static STATIC_MARK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(7);

    fn backend() -> RealBackend {
        RealBackend::new(std::process::id() as i32, "self".into(), Arc::default())
            .expect("open own process")
    }

    #[test]
    fn real_backend_scans_pokes_and_restores() {
        use crate::wizard::WizardBackend;
        // Built at run time so the value is not also a constant in the binary.
        let mut v = Box::new(0i32);
        *v = std::hint::black_box(0x2A5C_0000) + std::hint::black_box(0x1D7B);
        let addr = &*v as *const i32 as u64;
        let read = |b: &i32| unsafe { std::ptr::read_volatile(b) };

        let mut b = backend();
        assert_eq!(b.process_name().as_deref(), Some("self"));
        let n = b.first_scan(ScanType::I32, &read(&v).to_string()).unwrap();
        assert!(n >= 1);
        assert!(b.candidates().contains(&addr), "{addr:#x} not found");

        *v += 1;
        b.next_scan(NextScan::Increased, None).unwrap();
        assert!(b.candidates().contains(&addr));
        b.next_scan(NextScan::Eq, Some(&read(&v).to_string()))
            .unwrap();
        assert!(b.candidates().contains(&addr));

        let before = read(&v);
        b.poke(addr).unwrap();
        assert_eq!(read(&v), before + 1, "poke writes value + 1");
        b.restore(addr).unwrap();
        assert_eq!(read(&v), before, "restore puts the old value back");
        assert!(b.restore(addr).is_err(), "nothing left to restore");

        assert_eq!(b.process_running("no such process name"), None);
        drop(v);
    }

    #[test]
    fn closing_mid_test_write_puts_the_value_back() {
        use crate::wizard::WizardBackend;
        let mut v = Box::new(0i32);
        *v = std::hint::black_box(0x1357_0000) + std::hint::black_box(0x2468);
        let before = *v;
        let addr = &*v as *const i32 as u64;
        let read = |b: &i32| unsafe { std::ptr::read_volatile(b) };
        let inner = Arc::new(Mutex::new(Inner {
            wizard: Wizard::new(ScanType::I32),
            backend: backend(),
        }));
        // The worker is still writing test values when the window closes.
        let started = Arc::new(std::sync::Barrier::new(2));
        let handle = {
            let (inner, started) = (inner.clone(), started.clone());
            std::thread::spawn(move || {
                let mut g = lock(&inner);
                started.wait();
                std::thread::sleep(Duration::from_millis(200));
                g.backend.poke(addr).unwrap();
                g.wizard.step = Step::Verify {
                    remaining: vec![addr],
                    phase: VerifyPhase::Testing {
                        current: addr,
                        left: Vec::new(),
                    },
                };
            })
        };
        started.wait();
        assert!(finish_then_undo(Some(handle), &inner, SHUTDOWN_WAIT));
        assert_eq!(read(&v), before, "test value undone");
        // Nothing running: undo right away.
        assert!(finish_then_undo(None, &inner, SHUTDOWN_WAIT));
        drop(v);
    }

    #[test]
    fn real_backend_module_offset_only_for_static_memory() {
        use crate::wizard::WizardBackend;
        let b = backend();
        let s = &STATIC_MARK as *const _ as u64;
        let expr = b.module_offset(s).expect("a static lies in the image");
        assert!(expr.to_string().starts_with('"'), "{expr}");
        assert_eq!(expr.resolve(&b.live).unwrap(), s);
        let heap = Box::new(STATIC_MARK.load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(b.module_offset(&*heap as *const u64 as u64), None);
    }

    #[test]
    fn real_backend_chains_are_shortest_first() {
        use crate::wizard::WizardBackend;
        let mut b = backend();
        b.chains = vec![
            PointerChain {
                module: "m".into(),
                base_offset: 0,
                offsets: vec![1, 2, 3],
            },
            chain(5),
        ];
        assert_eq!(b.chains()[0], chain(5));
    }
}

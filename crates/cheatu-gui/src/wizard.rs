//! The cheat wizard as a state machine, without any egui.
//!
//! The egui window (Task 12) renders [`Wizard::step`] and forwards button
//! presses as [`Input`]. All scanner and process access goes through
//! [`WizardBackend`], so the flow can be tested with a scripted fake.

use std::io;

use cheatu_core::pointer_scan::PointerChain;
use cheatu_core::scan::NextScan;
use cheatu_core::{AddrExpr, Expr, ProbeOutcome, ScanType};

/// Narrowing stops when at most this many candidates remain.
pub const MAX_CANDIDATES: usize = 10;
/// After a restart, chain filtering stops when at most this many chains remain.
pub const MAX_CHAINS: usize = 3;

/// What the wizard needs from the scanner, the target and the process list.
pub trait WizardBackend {
    /// First scan for `value` as `ty`; returns the candidate count.
    fn first_scan(&mut self, ty: ScanType, value: &str) -> io::Result<usize>;
    /// Next scan; `value` is `Some` for value comparisons.
    fn next_scan(&mut self, cmp: NextScan, value: Option<&str>) -> io::Result<usize>;
    /// The current candidate addresses.
    fn candidates(&self) -> Vec<u64>;
    /// Write a sentinel and see whether the target overwrites it.
    fn probe(&mut self, addr: u64) -> io::Result<ProbeOutcome>;
    /// Write the current value + 1 at `addr` and leave it there.
    fn poke(&mut self, addr: u64) -> io::Result<()>;
    /// Put the value that `poke` replaced back.
    fn restore(&mut self, addr: u64) -> io::Result<()>;
    /// `"module"+offset` when `addr` lies in a module image.
    fn module_offset(&self, addr: u64) -> Option<AddrExpr>;
    /// Run the pointer scanner for `addr`; returns the chain count.
    fn pointer_scan(&mut self, addr: u64) -> io::Result<usize>;
    /// Keep the saved chains that land on any of `addrs` (the candidates
    /// for the value after a restart); returns the chain count.
    fn filter_chains(&mut self, addrs: &[u64]) -> io::Result<usize>;
    /// The chains still alive.
    fn chains(&self) -> Vec<PointerChain>;
    /// The pid of a running process with this name, if any.
    fn process_running(&self, name: &str) -> Option<i32>;
    /// Attach to another pid (after a restart of the game).
    fn reattach(&mut self, pid: i32) -> io::Result<()>;
    /// Name of the attached process.
    fn process_name(&self) -> Option<String>;
}

/// Sub-state of [`Step::Verify`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyPhase {
    /// Asking consent to write test values (`Confirm`).
    Consent,
    /// Consent refused: the user picks a candidate (`Pick(index)` into the
    /// shown `remaining` list).
    Pick,
    /// Testing one candidate at a time: `current` was poked (value + 1) and
    /// the user is asked "Did the value in the game change?" (`Confirm`).
    /// `left` are the candidates still to test after `current`.
    Testing { current: u64, left: Vec<u64> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    AskValue,
    /// `count` candidates; the user enters a value or one of the buttons.
    Narrowing {
        count: usize,
    },
    /// `remaining` are the candidates shown to the user.
    Verify {
        remaining: Vec<u64>,
        phase: VerifyPhase,
    },
    /// Chosen address is not in a module: ask to run the pointer scanner
    /// (`Confirm(true)`), or keep the plain address (`Confirm(false)`).
    MakeLasting,
    /// Waiting for the game to restart (`Restarted`), or `Stop` to keep the
    /// remaining chains.
    WaitRestart {
        process: String,
    },
    /// Re-attached after a restart: `count` candidates for the new scan
    /// (0 before the first scan). Needs `Value` / `Changed` until at most
    /// `MAX_CANDIDATES` remain; then the chains are filtered with all of them.
    Refind {
        count: usize,
    },
    Save {
        expr: AddrExpr,
    },
    /// A user-facing English sentence. `StartOver` always works.
    Failed(String),
}

#[derive(Clone, Debug)]
pub enum Input {
    Value(String),
    Changed(NextScan),
    Confirm(bool),
    /// Index into the list shown in [`VerifyPhase::Pick`].
    Pick(usize),
    Restarted,
    Stop,
    StartOver,
    /// Keep the plain address as a cheat for this session only.
    KeepPlainAddress,
}

pub struct Wizard {
    pub step: Step,
    /// Set when the last input was invalid; cleared by a valid one.
    pub error: Option<String>,
    ty: ScanType,
    process: Option<String>,
    chosen: Option<u64>,
    plain_offer: bool,
    session_only: bool,
}

impl Wizard {
    pub fn new(ty: ScanType) -> Self {
        Wizard {
            step: Step::AskValue,
            error: None,
            ty,
            process: None,
            chosen: None,
            plain_offer: false,
            session_only: false,
        }
    }

    pub fn value_type(&self) -> ScanType {
        self.ty
    }

    /// The address chosen so far (the latest one after a restart).
    pub fn chosen_address(&self) -> Option<u64> {
        self.chosen
    }

    /// Whether the saved cheat is a plain address that will not survive a
    /// restart of the game.
    pub fn is_session_only(&self) -> bool {
        self.session_only
    }

    /// Whether the UI should offer "keep the plain address (this session only)".
    pub fn can_keep_plain_address(&self) -> bool {
        self.plain_offer
    }

    pub fn handle(&mut self, input: Input, backend: &mut dyn WizardBackend) {
        if matches!(input, Input::StartOver) {
            *self = Wizard::new(self.ty);
            return;
        }
        let step = self.step.clone();
        match (step, input) {
            (Step::AskValue, Input::Value(v)) => {
                if !self.valid(&v) {
                    return;
                }
                if self.process.is_none() {
                    self.process = backend.process_name();
                }
                let r = backend.first_scan(self.ty, &v);
                self.after_scan(r, backend, "The value was not found in the game's memory.");
            }
            (Step::Narrowing { .. }, Input::Value(v)) => {
                if !self.valid(&v) {
                    return;
                }
                let r = backend.next_scan(NextScan::Eq, Some(&v));
                self.after_scan(r, backend, "No address matches that value any more.");
            }
            (Step::Narrowing { .. } | Step::Refind { count: 1.. }, Input::Changed(cmp)) => {
                self.error = None;
                let r = backend.next_scan(cmp, None);
                self.after_scan(r, backend, "No address matches that change any more.");
            }
            (Step::Refind { .. }, Input::Value(v)) => {
                if !self.valid(&v) {
                    return;
                }
                // The first value after a restart starts a new scan; later
                // ones narrow it.
                let r = if matches!(self.step, Step::Refind { count: 0 }) {
                    backend.first_scan(self.ty, &v)
                } else {
                    backend.next_scan(NextScan::Eq, Some(&v))
                };
                self.after_scan(r, backend, "The value was not found after the restart.");
            }
            (Step::Verify { remaining, phase }, input) => {
                self.verify(remaining, phase, input, backend)
            }
            (Step::MakeLasting, Input::Confirm(true)) => {
                let Some(addr) = self.chosen else { return };
                let Some(process) = self.process.clone() else {
                    self.fail_plain("Cannot tell which process is attached.");
                    return;
                };
                match backend.pointer_scan(addr) {
                    Err(e) => self.fail_plain(format!("The pointer scan failed: {e}.")),
                    Ok(0) => self.fail_plain("No pointer chain leads to that address."),
                    Ok(_) => self.step = Step::WaitRestart { process },
                }
            }
            (Step::MakeLasting, Input::Confirm(false) | Input::KeepPlainAddress)
            | (Step::Failed(_), Input::KeepPlainAddress) => self.keep_plain(),
            (Step::WaitRestart { process }, Input::Restarted) => {
                let Some(pid) = backend.process_running(&process) else {
                    return; // not back yet: keep waiting
                };
                match backend.reattach(pid) {
                    Ok(()) => {
                        // The old address is stale after a restart.
                        self.chosen = None;
                        self.step = Step::Refind { count: 0 };
                    }
                    Err(e) => {
                        self.fail_plain(format!("Could not attach to the restarted game: {e}."))
                    }
                }
            }
            (Step::WaitRestart { .. } | Step::Refind { .. }, Input::Stop) => {
                match backend.chains().first() {
                    Some(c) => {
                        self.step = Step::Save { expr: c.to_expr() };
                    }
                    None => self.fail_plain("No pointer chain is left to save."),
                }
            }
            _ => {}
        }
    }

    /// Validate a value for the type; sets or clears `error`.
    fn valid(&mut self, v: &str) -> bool {
        if self.ty.parse(v).is_some() {
            self.error = None;
            true
        } else {
            self.error = Some(format!(
                "'{}' is not a valid value for this type.",
                v.trim()
            ));
            false
        }
    }

    fn fail(&mut self, msg: impl Into<String>) {
        self.step = Step::Failed(msg.into());
    }

    /// Failed, offering the session-only plain address when one is chosen.
    fn fail_plain(&mut self, msg: impl Into<String>) {
        let mut msg: String = msg.into();
        if self.chosen.is_some() {
            self.plain_offer = true;
            msg.push_str(
                " You can keep the plain address as a cheat for this session only, or start over.",
            );
        } else {
            msg.push_str(" You can start over.");
        }
        self.fail(msg);
    }

    fn keep_plain(&mut self) {
        if let Some(addr) = self.chosen {
            self.session_only = true;
            self.step = Step::Save {
                expr: AddrExpr(Expr::Num(addr)),
            };
        }
    }

    /// Shared handling of a scan result in AskValue, Narrowing and Refind.
    fn after_scan(
        &mut self,
        r: io::Result<usize>,
        backend: &mut dyn WizardBackend,
        none_msg: &str,
    ) {
        let refind = matches!(self.step, Step::Refind { .. });
        let count = match r {
            Err(e) => return self.fail_plain(format!("The scan failed: {e}.")),
            Ok(0) => return self.fail_plain(none_msg),
            Ok(n) => n,
        };
        if refind {
            if count <= MAX_CANDIDATES {
                let addrs = backend.candidates();
                // The trait cannot tell which candidate a surviving chain
                // lands on, so `chosen` is the first one.
                self.chosen = addrs.first().copied();
                self.filter(&addrs, backend);
            } else {
                self.step = Step::Refind { count };
            }
        } else if count <= MAX_CANDIDATES {
            self.enter_verify(backend);
        } else {
            self.step = Step::Narrowing { count };
        }
    }

    fn filter(&mut self, addrs: &[u64], backend: &mut dyn WizardBackend) {
        match backend.filter_chains(addrs) {
            Err(e) => self.fail_plain(format!("Filtering the pointer chains failed: {e}.")),
            Ok(0) => self.fail_plain("No pointer chain survived the restart."),
            Ok(n) if n <= MAX_CHAINS => match backend.chains().first() {
                Some(c) => self.step = Step::Save { expr: c.to_expr() },
                None => self.fail_plain("No pointer chain survived the restart."),
            },
            Ok(_) => {
                let Some(process) = self.process.clone() else {
                    return self.fail_plain("Cannot tell which process is attached.");
                };
                self.step = Step::WaitRestart { process };
            }
        }
    }

    fn enter_verify(&mut self, backend: &mut dyn WizardBackend) {
        let cands = backend.candidates();
        match cands.len() {
            0 => self.fail("No candidate address is left. You can start over."),
            1 => self.choose(cands[0], backend),
            _ => {
                self.step = Step::Verify {
                    remaining: cands,
                    phase: VerifyPhase::Consent,
                }
            }
        }
    }

    /// The address is decided: a module address is final, anything else
    /// needs a pointer chain to last.
    fn choose(&mut self, addr: u64, backend: &mut dyn WizardBackend) {
        self.chosen = Some(addr);
        self.step = match backend.module_offset(addr) {
            Some(expr) => Step::Save { expr },
            None => Step::MakeLasting,
        };
    }

    fn verify(
        &mut self,
        remaining: Vec<u64>,
        phase: VerifyPhase,
        input: Input,
        backend: &mut dyn WizardBackend,
    ) {
        match (phase, input) {
            (VerifyPhase::Consent, Input::Confirm(false)) => {
                self.step = Step::Verify {
                    remaining,
                    phase: VerifyPhase::Pick,
                }
            }
            (VerifyPhase::Consent, Input::Confirm(true)) => {
                let mut kept = Vec::new();
                for &a in &remaining {
                    match backend.probe(a) {
                        Ok(ProbeOutcome::Reverted) => {}
                        Ok(ProbeOutcome::Held) => kept.push(a),
                        Err(e) => {
                            return self.fail(format!(
                                "Writing a test value failed: {e}. You can start over."
                            ))
                        }
                    }
                }
                match kept.len() {
                    0 => self.fail("The game overwrote every candidate, so none of them is the real value. You can start over."),
                    1 => self.choose(kept[0], backend),
                    _ => self.test_next(kept, backend),
                }
            }
            (VerifyPhase::Pick, Input::Pick(i)) => match remaining.get(i) {
                Some(&a) => {
                    self.error = None;
                    self.choose(a, backend)
                }
                None => self.error = Some("That candidate does not exist.".into()),
            },
            (VerifyPhase::Testing { current, left }, Input::Confirm(true)) => {
                let _ = left;
                self.choose(current, backend)
            }
            (VerifyPhase::Testing { current, left }, Input::Confirm(false)) => {
                if let Err(e) = backend.restore(current) {
                    return self.fail(format!(
                        "Restoring the value failed: {e}. You can start over."
                    ));
                }
                self.test_next(left, backend)
            }
            (phase, _) => self.step = Step::Verify { remaining, phase },
        }
    }

    /// Poke the first of `todo` and ask the user; fails when none is left.
    fn test_next(&mut self, mut todo: Vec<u64>, backend: &mut dyn WizardBackend) {
        if todo.is_empty() {
            return self
                .fail("None of the candidates changed the value in the game. You can start over.");
        }
        let current = todo.remove(0);
        if let Err(e) = backend.poke(current) {
            return self.fail(format!(
                "Writing a test value failed: {e}. You can start over."
            ));
        }
        let mut shown = vec![current];
        shown.extend(&todo);
        self.step = Step::Verify {
            remaining: shown,
            phase: VerifyPhase::Testing {
                current,
                left: todo,
            },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};

    #[derive(Default)]
    struct Fake {
        first: VecDeque<io::Result<usize>>,
        next: VecDeque<io::Result<usize>>,
        cands: Vec<u64>,
        probes: HashMap<u64, ProbeOutcome>,
        module: Option<AddrExpr>,
        ptr: usize,
        filters: VecDeque<io::Result<usize>>,
        chains: Vec<PointerChain>,
        running: VecDeque<Option<i32>>,
        reattach_err: bool,
        name: Option<String>,
        log: Vec<String>,
    }

    fn io_err() -> io::Error {
        io::Error::other("boom")
    }

    impl WizardBackend for Fake {
        fn first_scan(&mut self, _: ScanType, v: &str) -> io::Result<usize> {
            self.log.push(format!("first {v}"));
            self.first.pop_front().unwrap_or(Ok(0))
        }
        fn next_scan(&mut self, _: NextScan, v: Option<&str>) -> io::Result<usize> {
            self.log.push(format!("next {v:?}"));
            self.next.pop_front().unwrap_or(Ok(0))
        }
        fn candidates(&self) -> Vec<u64> {
            self.cands.clone()
        }
        fn probe(&mut self, a: u64) -> io::Result<ProbeOutcome> {
            self.log.push(format!("probe {a:x}"));
            Ok(self.probes.get(&a).copied().unwrap_or(ProbeOutcome::Held))
        }
        fn poke(&mut self, a: u64) -> io::Result<()> {
            self.log.push(format!("poke {a:x}"));
            Ok(())
        }
        fn restore(&mut self, a: u64) -> io::Result<()> {
            self.log.push(format!("restore {a:x}"));
            Ok(())
        }
        fn module_offset(&self, _: u64) -> Option<AddrExpr> {
            self.module.clone()
        }
        fn pointer_scan(&mut self, _: u64) -> io::Result<usize> {
            Ok(self.ptr)
        }
        fn filter_chains(&mut self, a: &[u64]) -> io::Result<usize> {
            let a: Vec<String> = a.iter().map(|x| format!("{x:x}")).collect();
            self.log.push(format!("filter {}", a.join(",")));
            self.filters.pop_front().unwrap_or(Ok(0))
        }
        fn chains(&self) -> Vec<PointerChain> {
            self.chains.clone()
        }
        fn process_running(&self, _: &str) -> Option<i32> {
            self.running.front().copied().flatten()
        }
        fn reattach(&mut self, _: i32) -> io::Result<()> {
            if self.reattach_err {
                Err(io_err())
            } else {
                Ok(())
            }
        }
        fn process_name(&self) -> Option<String> {
            self.name.clone()
        }
    }

    fn chain(off: u64) -> PointerChain {
        PointerChain {
            module: "m".into(),
            base_offset: 0x10,
            offsets: vec![off],
        }
    }

    fn val(w: &mut Wizard, b: &mut Fake, s: &str) {
        w.handle(Input::Value(s.into()), b);
    }

    fn base() -> Fake {
        Fake {
            name: Some("game".into()),
            ..Default::default()
        }
    }

    fn module_expr() -> AddrExpr {
        AddrExpr::parse("\"m\"+0x20").unwrap()
    }

    #[test]
    fn normal_flow_module_address() {
        let mut b = base();
        b.first.push_back(Ok(50));
        b.next.push_back(Ok(1));
        b.cands = vec![0x1000];
        b.module = Some(module_expr());
        let mut w = Wizard::new(ScanType::I32);
        val(&mut w, &mut b, "100");
        assert_eq!(w.step, Step::Narrowing { count: 50 });
        val(&mut w, &mut b, "101");
        assert_eq!(
            w.step,
            Step::Save {
                expr: module_expr()
            }
        );
        assert!(!w.is_session_only());
    }

    #[test]
    fn narrowing_with_changed_button_and_zero_fails() {
        let mut b = base();
        b.first.push_back(Ok(50));
        b.next.push_back(Ok(30));
        b.next.push_back(Ok(0));
        let mut w = Wizard::new(ScanType::I32);
        val(&mut w, &mut b, "1");
        w.handle(Input::Changed(NextScan::Increased), &mut b);
        assert_eq!(w.step, Step::Narrowing { count: 30 });
        assert_eq!(b.log.last().unwrap(), "next None");
        w.handle(Input::Changed(NextScan::Changed), &mut b);
        assert!(matches!(w.step, Step::Failed(_)));
    }

    fn to_verify(b: &mut Fake) -> Wizard {
        b.first.push_back(Ok(3));
        b.cands = vec![0x10, 0x20, 0x30];
        let mut w = Wizard::new(ScanType::I32);
        val(&mut w, b, "5");
        w
    }

    fn verify_step(w: &Wizard) -> (&Vec<u64>, &VerifyPhase) {
        match &w.step {
            Step::Verify { remaining, phase } => (remaining, phase),
            s => panic!("{s:?}"),
        }
    }

    #[test]
    fn verify_refused_consent_picks_candidate() {
        let mut b = base();
        b.module = Some(module_expr());
        let mut w = to_verify(&mut b);
        assert_eq!(verify_step(&w).1, &VerifyPhase::Consent);
        w.handle(Input::Confirm(false), &mut b);
        assert_eq!(verify_step(&w).1, &VerifyPhase::Pick);
        w.handle(Input::Pick(7), &mut b);
        assert!(w.error.is_some());
        assert_eq!(verify_step(&w).1, &VerifyPhase::Pick);
        w.handle(Input::Pick(1), &mut b);
        assert_eq!(w.chosen_address(), Some(0x20));
        assert!(matches!(w.step, Step::Save { .. }));
        assert!(w.error.is_none());
        assert!(b.log.iter().all(|l| !l.starts_with("probe")));
    }

    #[test]
    fn verify_consent_drops_reverted_and_single_survivor_goes_on() {
        let mut b = base();
        b.probes.insert(0x10, ProbeOutcome::Reverted);
        b.probes.insert(0x30, ProbeOutcome::Reverted);
        b.module = Some(module_expr());
        let mut w = to_verify(&mut b);
        w.handle(Input::Confirm(true), &mut b);
        assert_eq!(w.chosen_address(), Some(0x20));
        assert!(matches!(w.step, Step::Save { .. }));
    }

    #[test]
    fn verify_all_reverted_fails() {
        let mut b = base();
        for a in [0x10, 0x20, 0x30] {
            b.probes.insert(a, ProbeOutcome::Reverted);
        }
        let mut w = to_verify(&mut b);
        w.handle(Input::Confirm(true), &mut b);
        assert!(matches!(w.step, Step::Failed(_)));
    }

    #[test]
    fn verify_poke_each_until_user_confirms() {
        let mut b = base();
        b.module = Some(module_expr());
        let mut w = to_verify(&mut b);
        w.handle(Input::Confirm(true), &mut b);
        assert_eq!(
            verify_step(&w).1,
            &VerifyPhase::Testing {
                current: 0x10,
                left: vec![0x20, 0x30]
            }
        );
        w.handle(Input::Confirm(false), &mut b);
        assert_eq!(
            verify_step(&w).1,
            &VerifyPhase::Testing {
                current: 0x20,
                left: vec![0x30]
            }
        );
        w.handle(Input::Confirm(true), &mut b);
        assert_eq!(w.chosen_address(), Some(0x20));
        assert!(matches!(w.step, Step::Save { .. }));
        let pokes: Vec<_> = b
            .log
            .iter()
            .filter(|l| l.starts_with("poke") || l.starts_with("restore"))
            .cloned()
            .collect();
        assert_eq!(pokes, ["poke 10", "restore 10", "poke 20"]);
    }

    #[test]
    fn verify_none_confirmed_fails() {
        let mut b = base();
        let mut w = to_verify(&mut b);
        w.handle(Input::Confirm(true), &mut b);
        for _ in 0..3 {
            w.handle(Input::Confirm(false), &mut b);
        }
        assert!(matches!(w.step, Step::Failed(_)));
        assert!(b.log.contains(&"restore 30".to_string()));
    }

    fn heap_to_wait(b: &mut Fake) -> Wizard {
        b.first.push_back(Ok(1));
        b.cands = vec![0x5000];
        b.ptr = 40;
        let mut w = Wizard::new(ScanType::I32);
        val(&mut w, b, "7");
        assert_eq!(w.step, Step::MakeLasting);
        w.handle(Input::Confirm(true), b);
        w
    }

    #[test]
    fn normal_flow_with_restart() {
        let mut b = base();
        let mut w = heap_to_wait(&mut b);
        assert_eq!(
            w.step,
            Step::WaitRestart {
                process: "game".into()
            }
        );
        b.running.push_back(Some(99));
        b.first.push_back(Ok(1));
        b.cands = vec![0x9000];
        b.filters.push_back(Ok(2));
        b.chains = vec![chain(1), chain(2)];
        w.handle(Input::Restarted, &mut b);
        assert_eq!(w.step, Step::Refind { count: 0 });
        val(&mut w, &mut b, "7");
        assert!(b.log.contains(&"filter 9000".to_string()));
        assert_eq!(
            w.step,
            Step::Save {
                expr: chain(1).to_expr()
            }
        );
        assert!(!w.is_session_only());
    }

    #[test]
    fn restart_repeats_until_few_chains_or_stop() {
        let mut b = base();
        let mut w = heap_to_wait(&mut b);
        b.running.push_back(Some(99));
        b.first.push_back(Ok(1));
        b.first.push_back(Ok(1));
        b.cands = vec![0x9000];
        b.filters.push_back(Ok(8));
        b.filters.push_back(Ok(5));
        b.chains = vec![chain(1), chain(2)];
        w.handle(Input::Restarted, &mut b);
        val(&mut w, &mut b, "7");
        assert!(matches!(w.step, Step::WaitRestart { .. }));
        w.handle(Input::Restarted, &mut b);
        val(&mut w, &mut b, "7");
        assert!(matches!(w.step, Step::WaitRestart { .. }));
        w.handle(Input::Stop, &mut b);
        assert_eq!(
            w.step,
            Step::Save {
                expr: chain(1).to_expr()
            }
        );
    }

    #[test]
    fn refind_filters_with_all_candidates_when_few() {
        let mut b = base();
        let mut w = heap_to_wait(&mut b);
        b.running.push_back(Some(99));
        b.first.push_back(Ok(2));
        b.cands = vec![0x9000, 0x9100];
        b.filters.push_back(Ok(1));
        b.chains = vec![chain(1)];
        w.handle(Input::Restarted, &mut b);
        val(&mut w, &mut b, "7");
        assert!(b.log.contains(&"filter 9000,9100".to_string()));
        assert_eq!(w.chosen_address(), Some(0x9000));
        assert!(matches!(w.step, Step::Save { .. }));
    }

    #[test]
    fn refind_keeps_narrowing_above_max_candidates() {
        let mut b = base();
        let mut w = heap_to_wait(&mut b);
        b.running.push_back(Some(99));
        b.first.push_back(Ok(40));
        b.next.push_back(Ok(11));
        b.next.push_back(Ok(10));
        b.cands = vec![0x9000, 0x9100];
        b.filters.push_back(Ok(1));
        b.chains = vec![chain(1)];
        w.handle(Input::Restarted, &mut b);
        val(&mut w, &mut b, "7");
        assert_eq!(w.step, Step::Refind { count: 40 });
        val(&mut w, &mut b, "8");
        assert_eq!(w.step, Step::Refind { count: 11 });
        assert!(b.log.iter().all(|l| !l.starts_with("filter")));
        val(&mut w, &mut b, "9");
        assert!(b.log.contains(&"filter 9000,9100".to_string()));
        assert!(matches!(w.step, Step::Save { .. }));
    }

    #[test]
    fn value_never_found() {
        let mut b = base();
        b.first.push_back(Ok(0));
        let mut w = Wizard::new(ScanType::I32);
        val(&mut w, &mut b, "1");
        assert_eq!(
            w.step,
            Step::Failed(
                "The value was not found in the game's memory. You can start over.".into()
            )
        );
        w.handle(Input::StartOver, &mut b);
        assert_eq!(w.step, Step::AskValue);
    }

    #[test]
    fn no_chain_survives_offers_plain_address() {
        let mut b = base();
        let mut w = heap_to_wait(&mut b);
        b.running.push_back(Some(99));
        b.first.push_back(Ok(1));
        b.cands = vec![0x9000];
        b.filters.push_back(Ok(0));
        w.handle(Input::Restarted, &mut b);
        val(&mut w, &mut b, "7");
        assert!(matches!(w.step, Step::Failed(_)));
        assert!(w.can_keep_plain_address());
        w.handle(Input::KeepPlainAddress, &mut b);
        assert_eq!(
            w.step,
            Step::Save {
                expr: AddrExpr(Expr::Num(0x9000))
            }
        );
        assert!(w.is_session_only());
    }

    #[test]
    fn no_pointer_chains_at_all_offers_plain_address() {
        let mut b = base();
        b.first.push_back(Ok(1));
        b.cands = vec![0x5000];
        b.ptr = 0;
        let mut w = Wizard::new(ScanType::I32);
        val(&mut w, &mut b, "7");
        w.handle(Input::Confirm(true), &mut b);
        assert!(matches!(w.step, Step::Failed(_)));
        assert!(w.can_keep_plain_address());
        w.handle(Input::KeepPlainAddress, &mut b);
        assert_eq!(
            w.step,
            Step::Save {
                expr: AddrExpr(Expr::Num(0x5000))
            }
        );
    }

    #[test]
    fn declining_pointer_scan_keeps_plain_address() {
        let mut b = base();
        b.first.push_back(Ok(1));
        b.cands = vec![0x5000];
        let mut w = Wizard::new(ScanType::I32);
        val(&mut w, &mut b, "7");
        w.handle(Input::Confirm(false), &mut b);
        assert!(matches!(w.step, Step::Save { .. }));
        assert!(w.is_session_only());
    }

    #[test]
    fn process_disappears() {
        let mut b = base();
        let mut w = heap_to_wait(&mut b);
        b.running.push_back(None);
        w.handle(Input::Restarted, &mut b);
        assert!(matches!(w.step, Step::WaitRestart { .. }));
        b.running.clear();
        b.running.push_back(Some(5));
        b.first.push_back(Err(io_err()));
        w.handle(Input::Restarted, &mut b);
        val(&mut w, &mut b, "7");
        assert!(matches!(w.step, Step::Failed(_)));
    }

    #[test]
    fn reattach_error_fails() {
        let mut b = base();
        let mut w = heap_to_wait(&mut b);
        b.running.push_back(Some(5));
        b.reattach_err = true;
        w.handle(Input::Restarted, &mut b);
        assert!(matches!(w.step, Step::Failed(_)));
    }

    #[test]
    fn invalid_value_stays_on_step() {
        let mut b = base();
        let mut w = Wizard::new(ScanType::I16);
        val(&mut w, &mut b, "abc");
        assert_eq!(w.step, Step::AskValue);
        assert!(w.error.is_some());
        val(&mut w, &mut b, "40000");
        assert_eq!(w.step, Step::AskValue);
        assert!(w.error.is_some());
        assert!(b.log.is_empty());
        b.first.push_back(Ok(50));
        val(&mut w, &mut b, "100");
        assert!(w.error.is_none());
        assert_eq!(w.step, Step::Narrowing { count: 50 });
    }

    #[test]
    fn start_over_resets_from_anywhere() {
        let mut b = base();
        let mut w = heap_to_wait(&mut b);
        w.handle(Input::StartOver, &mut b);
        assert_eq!(w.step, Step::AskValue);
        assert_eq!(w.chosen_address(), None);
        assert!(!w.can_keep_plain_address());
    }
}

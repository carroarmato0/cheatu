//! The GUI's cheat table: per-entry state on top of a [`TableEntry`], when to
//! re-resolve an address expression, what the freeze thread may write, and
//! the glue between the table file and the GUI rows. No egui code here.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cheatu_core::process::ProcInfo;
use cheatu_core::table::{self, TableEntry, TableError};
use cheatu_core::{
    module_containing, valid_module_name, AddrExpr, Expr, MemoryRegion, ScanType, ScanValue,
    SignatureError,
};

/// A frozen entry re-resolves this often: pointers can change when the game
/// loads a new area, but not every 40 ms freeze tick.
pub const FROZEN_RESOLVE_EVERY: Duration = Duration::from_secs(1);
/// An entry that does not resolve is retried this often.
pub const FAILED_RESOLVE_EVERY: Duration = Duration::from_secs(3);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// One row of the GUI cheat table.
pub struct GuiEntry {
    /// What is saved. `entry.address` is the last expression that parsed.
    pub entry: TableEntry,
    /// The expression as the user is typing it.
    pub expr_text: String,
    /// The last resolved address, or why it did not resolve.
    pub resolved: Result<u64, String>,
    /// When `resolved` was last computed.
    pub last_resolve: Instant,
    /// The file's `frozen` flag when the table was opened.
    pub frozen_when_saved: bool,
    /// Why `expr_text` does not parse, if it does not.
    pub parse_error: Option<String>,
    /// The freeze thread could not write to `resolved`; re-resolve now.
    pub write_failed: bool,
    /// Re-resolve on the next pass whatever the schedule says (attach, open,
    /// edit, "Re-resolve").
    pub needs_resolve: bool,
    /// Unique for the life of the program, to match background results.
    pub id: u64,
}

impl GuiEntry {
    pub fn new(entry: TableEntry, frozen_when_saved: bool) -> GuiEntry {
        GuiEntry {
            expr_text: entry.address.to_string(),
            entry,
            resolved: Err("not resolved yet".into()),
            last_resolve: Instant::now(),
            frozen_when_saved,
            parse_error: None,
            write_failed: false,
            needs_resolve: true,
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// True when the expression uses `aob(...)`, which scans the whole
    /// process and so is resolved only on demand, off the UI thread.
    pub fn uses_aob(&self) -> bool {
        has_aob(&self.entry.address.0)
    }
}

fn has_aob(e: &Expr) -> bool {
    match e {
        Expr::Aob { .. } => true,
        Expr::Num(_) | Expr::Module(_) => false,
        Expr::Deref(inner) => has_aob(inner),
        Expr::Add(l, r) | Expr::Sub(l, r) => has_aob(l) || has_aob(r),
    }
}

/// Whether `e` should be re-resolved now: right after a failed write, every
/// [`FAILED_RESOLVE_EVERY`] while it does not resolve, and every
/// [`FROZEN_RESOLVE_EVERY`] while it is frozen.
pub fn due_for_resolve(e: &GuiEntry, now: Instant, write_failed: bool) -> bool {
    if write_failed {
        return true;
    }
    let age = now.saturating_duration_since(e.last_resolve);
    (e.resolved.is_err() && age >= FAILED_RESOLVE_EVERY)
        || (e.entry.frozen && age >= FROZEN_RESOLVE_EVERY)
}

/// What the freeze thread writes: frozen entries that resolve and whose value
/// parses. An entry that does not resolve is never written.
pub fn freeze_items(entries: &[GuiEntry]) -> Vec<(u64, ScanValue)> {
    entries
        .iter()
        .filter(|e| e.entry.frozen)
        .filter_map(|e| {
            let addr = *e.resolved.as_ref().ok()?;
            Some((addr, e.entry.ty.parse(&e.entry.value)?))
        })
        .collect()
}

/// Mark the frozen entries the freeze thread could not write to, so they
/// re-resolve right away.
pub fn mark_write_failures(entries: &mut [GuiEntry], failed: &[u64]) {
    for e in entries.iter_mut().filter(|e| e.entry.frozen) {
        if e.resolved.as_ref().is_ok_and(|a| failed.contains(a)) {
            e.write_failed = true;
        }
    }
}

/// Store a resolve result. `failed_at` is the address a write just failed at,
/// when this resolve was triggered by that failure: resolving to the same
/// address again means it is not writable, so the entry turns into an error
/// (never written, retried on the error schedule) instead of being retried
/// every freeze tick.
pub fn apply_resolve(
    e: &mut GuiEntry,
    r: Result<u64, String>,
    now: Instant,
    failed_at: Option<u64>,
) {
    e.resolved = match r {
        Ok(a) if failed_at == Some(a) => Err(format!("write to {a:#x} failed")),
        other => other,
    };
    e.last_resolve = now;
    e.write_failed = false;
    e.needs_resolve = false;
}

/// The result of a background (`aob(...)`) resolve, with what it was
/// started for.
pub struct BgResolve {
    /// The row it was started for.
    pub id: u64,
    /// The expression it resolved.
    pub expr: AddrExpr,
    /// The process it read.
    pub pid: i32,
    /// The app's resolve generation when it started; bumped on attach and on
    /// every expression edit.
    pub generation: u64,
    /// The address a write had failed at, if that triggered it.
    pub failed_at: Option<u64>,
    pub result: Result<u64, String>,
}

/// Apply a finished background resolve to its row, if it still belongs
/// there: same process, same generation, same expression. A stale result is
/// dropped and its row is asked to resolve again (it was taken off the queue
/// when the search started). Returns whether the result was applied.
pub fn apply_background_result(
    entries: &mut [GuiEntry],
    pid: i32,
    generation: u64,
    r: BgResolve,
    now: Instant,
) -> bool {
    let Some(e) = entries.iter_mut().find(|e| e.id == r.id) else {
        return false;
    };
    if r.pid != pid || r.generation != generation || r.expr != e.entry.address {
        e.needs_resolve = true;
        return false;
    }
    apply_resolve(e, r.result, now, r.failed_at);
    true
}

/// `"module"+offset` for an address in a module's image whose file name can be
/// written in an expression (see [`cheatu_core::valid_module_name`]).
pub fn module_offset_expr(regions: &[MemoryRegion], addr: u64) -> Option<AddrExpr> {
    let (name, base) = module_containing(regions, addr).filter(|(n, _)| valid_module_name(n))?;
    Some(AddrExpr(Expr::Add(
        Box::new(Expr::Module(name)),
        Box::new(Expr::Num(addr - base)),
    )))
}

/// The entry the wizard's Save adds, and the status bar text. `wizard_pid` is
/// the process the wizard found the address in, `attached` the one the cheat
/// table runs against: when they differ the entry is added unfrozen, so it is
/// never written into the wrong process. An expression that would not save
/// is refused.
pub fn wizard_entry(
    entry: TableEntry,
    wizard_pid: i32,
    attached: Option<i32>,
) -> Result<(TableEntry, String), String> {
    let mut entry = entry;
    entry
        .address
        .validate()
        .map_err(|e| format!("The wizard's address cannot be saved in a table: {e}."))?;
    let added = format!(
        "Added \"{}\" ({}) to the cheat table.",
        entry.description, entry.address
    );
    if attached == Some(wizard_pid) {
        return Ok((entry, added));
    }
    entry.frozen = false;
    let now = match attached {
        Some(pid) => format!("cheatu is attached to pid {pid}"),
        None => "cheatu is not attached".into(),
    };
    Ok((
        entry,
        format!(
            "{added} Not frozen: the wizard found it in pid {wizard_pid}, but {now}; \
             check the address before freezing it."
        ),
    ))
}

/// A new entry for a scan result: `"module"+offset` when the address is in a
/// module's image, else the plain address. The value is a zero of the type;
/// the caller fills in the live value when it can read it.
pub fn entry_for_address(regions: &[MemoryRegion], addr: u64, ty: ScanType) -> TableEntry {
    let address = module_offset_expr(regions, addr).unwrap_or(AddrExpr(Expr::Num(addr)));
    let value = match ty {
        ScanType::Bytes(n) => vec!["00"; n].join(" "),
        _ => "0".into(),
    };
    TableEntry {
        description: String::new(),
        address,
        ty,
        value,
        frozen: false,
    }
}

/// What the status bar says when no unique signature exists.
pub const NOT_UNIQUE: &str = "No unique pattern within 64 bytes; for heap values use Pointer scan";

/// The result of a background "Make AoB signature", with what it was started
/// for.
pub struct BgSignature {
    /// The row it was started for.
    pub id: u64,
    /// The row's expression when it started.
    pub expr: AddrExpr,
    /// The process it read.
    pub pid: i32,
    /// The app's resolve generation when it started.
    pub generation: u64,
    pub result: Result<AddrExpr, SignatureError>,
}

/// Apply a finished signature to its row: on success, if the row is still
/// the one it was made for (same process, generation and expression), the
/// expression is replaced like an edit and the row re-resolves. Returns
/// whether the row changed (the caller bumps the resolve generation) and the
/// status bar text.
pub fn apply_signature(
    entries: &mut [GuiEntry],
    pid: i32,
    generation: u64,
    r: BgSignature,
) -> (bool, String) {
    let expr = match r.result {
        Ok(expr) => expr,
        Err(SignatureError::NotUnique) => return (false, NOT_UNIQUE.into()),
        Err(e) => return (false, format!("No AoB signature: {e}.")),
    };
    let row = entries
        .iter_mut()
        .find(|e| e.id == r.id)
        .filter(|e| r.pid == pid && r.generation == generation && e.entry.address == r.expr);
    let Some(e) = row else {
        return (
            false,
            "AoB signature discarded: the row or the process changed while it was made.".into(),
        );
    };
    e.expr_text = expr.to_string();
    e.entry.address = expr;
    e.parse_error = None;
    e.resolved = Err("not resolved yet".into());
    e.write_failed = false;
    e.needs_resolve = true;
    (true, format!("AoB signature: {}", e.expr_text))
}

/// A table read from disk, ready for the GUI.
pub struct OpenedTable {
    /// The process the table was made for.
    pub process: Option<String>,
    pub entries: Vec<GuiEntry>,
}

/// Open a table file. Every freeze starts off; the file's flag is kept as
/// `frozen_when_saved`. Never touches target memory.
pub fn open_table(path: &Path) -> Result<OpenedTable, TableError> {
    let t = table::load(path)?;
    let entries = t
        .entries
        .into_iter()
        .map(|mut e| {
            let was_frozen = std::mem::replace(&mut e.frozen, false);
            GuiEntry::new(e, was_frozen)
        })
        .collect();
    Ok(OpenedTable {
        process: t.process,
        entries,
    })
}

/// A value as the table file stores it: hex bytes for `Bytes`, else the
/// plain number.
pub fn value_text(v: &ScanValue) -> String {
    match v {
        ScanValue::Bytes(b) => b
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    }
}

/// Whether `s` is a value the table file accepts for `ty`.
pub fn savable_value(ty: ScanType, s: &str) -> bool {
    match ty {
        ScanType::Bytes(_) => {
            let mut toks = s.split_whitespace().peekable();
            toks.peek().is_some()
                && toks.all(|t| t.len() <= 2 && t.bytes().all(|b| b.is_ascii_hexdigit()))
        }
        _ => ty.parse(s).is_some(),
    }
}

/// Pids of running processes whose name is exactly `name`.
pub fn processes_named(procs: &[ProcInfo], name: &str) -> Vec<i32> {
    procs
        .iter()
        .filter(|p| p.name == name)
        .map(|p| p.pid)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(expr: &str, frozen: bool) -> GuiEntry {
        let mut e = GuiEntry::new(
            TableEntry {
                description: String::new(),
                address: AddrExpr::parse(expr).unwrap(),
                ty: ScanType::I32,
                value: "7".into(),
                frozen,
            },
            false,
        );
        e.needs_resolve = false;
        e
    }

    #[test]
    fn resolve_schedule_frozen_every_second() {
        let t0 = Instant::now();
        let mut e = entry("0x1000", true);
        e.resolved = Ok(0x1000);
        e.last_resolve = t0;
        assert!(!due_for_resolve(&e, t0 + Duration::from_millis(999), false));
        assert!(due_for_resolve(&e, t0 + Duration::from_secs(1), false));

        e.entry.frozen = false;
        assert!(
            !due_for_resolve(&e, t0 + Duration::from_secs(60), false),
            "a resolved, unfrozen entry is left alone"
        );
    }

    #[test]
    fn resolve_schedule_failed_every_three_seconds() {
        let t0 = Instant::now();
        let mut e = entry("0x1000", false);
        e.resolved = Err("pointer at 0x10 unreadable".into());
        e.last_resolve = t0;
        assert!(!due_for_resolve(
            &e,
            t0 + Duration::from_millis(2999),
            false
        ));
        assert!(due_for_resolve(&e, t0 + Duration::from_secs(3), false));
    }

    #[test]
    fn resolve_schedule_immediately_after_failed_write() {
        let t0 = Instant::now();
        let mut e = entry("0x1000", true);
        e.resolved = Ok(0x1000);
        e.last_resolve = t0;
        assert!(due_for_resolve(&e, t0, true));
    }

    #[test]
    fn stale_entry_not_fed_to_freeze() {
        let mut ok = entry("0x1000", true);
        ok.resolved = Ok(0x1000);
        let mut stale = entry("0x2000", true);
        stale.resolved = Err("module \"game\" not loaded".into());
        let mut unfrozen = entry("0x3000", false);
        unfrozen.resolved = Ok(0x3000);
        let mut bad_value = entry("0x4000", true);
        bad_value.resolved = Ok(0x4000);
        bad_value.entry.value = "lots".into();

        let items = freeze_items(&[ok, stale, unfrozen, bad_value]);
        assert_eq!(items.len(), 1, "only the resolved, frozen, valid entry");
        assert_eq!(items[0].0, 0x1000);
        assert!(matches!(items[0].1, ScanValue::I32(7)));
    }

    fn temp_file(name: &str, text: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("cheatu-gui-{}-{name}", std::process::id()));
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn opened_table_starts_unfrozen() {
        let p = temp_file(
            "frozen.toml",
            r#"version = 1
process = "game.exe"

[[entry]]
description = "gold"
address = '"game.exe"+0x10'
type = "i32"
value = "999"
frozen = true

[[entry]]
description = "hp"
address = "0x2000"
type = "f32"
value = "1.5"
frozen = true
"#,
        );
        let t = open_table(&p).unwrap();
        std::fs::remove_file(&p).unwrap();
        assert_eq!(t.process.as_deref(), Some("game.exe"));
        assert_eq!(t.entries.len(), 2);
        for e in &t.entries {
            assert!(!e.entry.frozen, "opening never freezes");
            assert!(e.frozen_when_saved);
            assert!(e.needs_resolve);
        }
        assert_eq!(t.entries[0].expr_text, "\"game.exe\"+0x10");
        assert!(freeze_items(&t.entries).is_empty());
    }

    fn region(start: u64, end: u64, path: &str) -> MemoryRegion {
        MemoryRegion {
            start,
            end,
            read: true,
            write: true,
            exec: false,
            shared: false,
            path: path.into(),
        }
    }

    #[test]
    fn entry_for_static_address_uses_module() {
        let regions = [
            region(0x10000, 0x11000, "/games/game.exe"),
            region(0x11000, 0x12000, "/games/game.exe"),
            region(0x50000, 0x60000, "[heap]"),
        ];
        let e = entry_for_address(&regions, 0x11040, ScanType::F32);
        assert_eq!(e.address.to_string(), "\"game.exe\"+0x1040");
        assert_eq!(e.ty, ScanType::F32);
        assert!(!e.frozen);
        assert!(savable_value(e.ty, &e.value));

        let heap = entry_for_address(&regions, 0x50010, ScanType::I32);
        assert_eq!(heap.address.to_string(), "0x50010");
    }

    #[test]
    fn unnameable_module_falls_back_to_plain_address() {
        let regions = [region(0x10000, 0x11000, "/games/we\"ird.exe")];
        assert_eq!(module_offset_expr(&regions, 0x10040), None);
        let e = entry_for_address(&regions, 0x10040, ScanType::I32);
        assert_eq!(e.address.to_string(), "0x10040");
        let ok = [region(0x10000, 0x11000, "/games/game.exe")];
        assert_eq!(
            module_offset_expr(&ok, 0x10040).unwrap().to_string(),
            "\"game.exe\"+0x40"
        );
    }

    #[test]
    fn wizard_entry_stays_unfrozen_for_another_process() {
        let mut e = entry("[\"game\"+0x10]+0x8", true).entry;
        e.description = "Gold".into();
        let (same, status) = wizard_entry(e.clone(), 10, Some(10)).unwrap();
        assert!(same.frozen);
        assert!(status.contains("Gold"), "{status}");

        let (other, status) = wizard_entry(e.clone(), 10, Some(20)).unwrap();
        assert!(!other.frozen, "never frozen into another process");
        assert!(
            status.contains("pid 10") && status.contains("pid 20"),
            "{status}"
        );
        let (detached, _) = wizard_entry(e.clone(), 10, None).unwrap();
        assert!(!detached.frozen);

        e.address = AddrExpr(Expr::Module("we\"ird".into()));
        assert!(wizard_entry(e, 10, Some(10)).is_err());
    }

    #[test]
    fn aob_expressions_are_detected() {
        assert!(entry("[aob(\"48 8B ??\", \"game\")+0x3]+0x10", false).uses_aob());
        assert!(!entry("[\"game\"+0x10]+0x8", false).uses_aob());
    }

    #[test]
    fn bytes_values_are_saved_as_hex() {
        assert_eq!(value_text(&ScanValue::Bytes(b"Hi".to_vec())), "48 69");
        assert_eq!(value_text(&ScanValue::I32(-3)), "-3");
        assert!(savable_value(ScanType::Bytes(2), "48 69"));
        assert!(!savable_value(ScanType::Bytes(2), "Hi"));
        assert!(savable_value(ScanType::U8, "255"));
        assert!(!savable_value(ScanType::U8, "256"));
        let zero = entry_for_address(&[], 0x10, ScanType::Bytes(3));
        assert_eq!(zero.value, "00 00 00");
        assert_eq!(zero.ty, ScanType::Bytes(3));
    }

    #[test]
    fn processes_are_matched_by_exact_name() {
        let p = |pid, name: &str| ProcInfo {
            pid,
            name: name.into(),
            cmdline: String::new(),
            rss_bytes: 0,
            is_wine: false,
            role: None,
            accessible: true,
        };
        let procs = [p(1, "game.exe"), p(2, "game.exe.bak"), p(3, "game.exe")];
        assert_eq!(processes_named(&procs, "game.exe"), vec![1, 3]);
        assert!(processes_named(&procs, "Game.exe").is_empty());
    }
    #[test]
    fn write_failure_marks_only_frozen_entry_at_that_address() {
        let mut a = entry("0x1000", true);
        a.resolved = Ok(0x1000);
        let mut b = entry("0x1000", false);
        b.resolved = Ok(0x1000);
        let mut c = entry("0x2000", true);
        c.resolved = Ok(0x2000);
        let mut v = vec![a, b, c];
        mark_write_failures(&mut v, &[0x1000]);
        assert!(v[0].write_failed);
        assert!(!v[1].write_failed, "not frozen, not written by the thread");
        assert!(!v[2].write_failed);
    }

    #[test]
    fn same_address_after_failed_write_becomes_an_error() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(5);
        let mut e = entry("0x1000", true);
        e.resolved = Ok(0x1000);
        e.write_failed = true;
        e.needs_resolve = true;

        // The pointer moved: follow it.
        apply_resolve(&mut e, Ok(0x3000), later, Some(0x1000));
        assert_eq!(e.resolved, Ok(0x3000));
        assert!(!e.write_failed && !e.needs_resolve);
        assert_eq!(e.last_resolve, later);

        // Same address again: stop writing there.
        e.write_failed = true;
        apply_resolve(&mut e, Ok(0x3000), later, Some(0x3000));
        assert!(e.resolved.as_ref().unwrap_err().contains("0x3000"));
        assert!(freeze_items(std::slice::from_ref(&e)).is_empty());

        // A plain resolve is stored as is.
        apply_resolve(&mut e, Err("boom".into()), later, None);
        assert_eq!(e.resolved, Err("boom".into()));
    }
    fn bg(e: &GuiEntry, pid: i32, generation: u64, result: Result<u64, String>) -> BgResolve {
        BgResolve {
            id: e.id,
            expr: e.entry.address.clone(),
            pid,
            generation,
            failed_at: None,
            result,
        }
    }

    #[test]
    fn matching_background_result_is_applied() {
        let now = Instant::now();
        let mut v = vec![entry("aob(\"48 8B\")", true)];
        let r = bg(&v[0], 7, 3, Ok(0x4000));
        assert!(apply_background_result(&mut v, 7, 3, r, now));
        assert_eq!(v[0].resolved, Ok(0x4000));
        assert!(!v[0].needs_resolve);
    }

    #[test]
    fn background_result_from_older_generation_is_ignored() {
        let now = Instant::now();
        let mut v = vec![entry("aob(\"48 8B\")", true)];
        v[0].resolved = Err("not resolved yet".into());
        // Started before a re-attach (generation 3, old pid 7).
        let r = bg(&v[0], 7, 3, Ok(0x4000));
        assert!(!apply_background_result(&mut v, 9, 4, r, now));
        assert_eq!(v[0].resolved, Err("not resolved yet".into()));
        assert!(
            v[0].needs_resolve,
            "must be searched again in the new process"
        );
        assert!(freeze_items(&v).is_empty());

        // Same generation but another process is just as stale.
        let r = bg(&v[0], 7, 4, Ok(0x4000));
        assert!(!apply_background_result(&mut v, 9, 4, r, now));
        assert!(v[0].resolved.is_err());
    }

    #[test]
    fn background_result_for_deleted_row_is_ignored() {
        let now = Instant::now();
        let gone = entry("aob(\"48 8B\")", true);
        let mut v = vec![entry("aob(\"48 8B\")", true)];
        v[0].resolved = Err("x".into());
        let r = bg(&gone, 7, 3, Ok(0x4000));
        assert!(!apply_background_result(&mut v, 7, 3, r, now));
        assert_eq!(v[0].resolved, Err("x".into()));
    }

    #[test]
    fn background_result_after_edit_back_and_forth_is_ignored() {
        let now = Instant::now();
        let mut v = vec![entry("aob(\"48 8B\")", true)];
        // Search for A started at generation 3...
        let r = bg(&v[0], 7, 3, Ok(0x4000));
        // ...then the row was edited A -> B -> A, bumping the generation and
        // asking for a fresh resolve.
        v[0].needs_resolve = true;
        assert!(!apply_background_result(&mut v, 7, 5, r, now));
        assert!(v[0].needs_resolve, "the requested re-resolve is kept");
        assert!(v[0].resolved.is_err());
    }
    fn sig(
        e: &GuiEntry,
        pid: i32,
        generation: u64,
        result: Result<AddrExpr, SignatureError>,
    ) -> BgSignature {
        BgSignature {
            id: e.id,
            expr: e.entry.address.clone(),
            pid,
            generation,
            result,
        }
    }

    #[test]
    fn signature_replaces_the_expression_like_an_edit() {
        let mut v = vec![entry("0x7f001000", true)];
        v[0].resolved = Ok(0x7f00_1000);
        let new = AddrExpr::parse("aob(\"48 8B ?? 05\", \"game\")+0x3").unwrap();
        let r = sig(&v[0], 7, 3, Ok(new.clone()));
        let (changed, status) = apply_signature(&mut v, 7, 3, r);
        assert!(changed);
        assert_eq!(v[0].entry.address, new);
        assert_eq!(v[0].expr_text, new.to_string());
        assert!(v[0].parse_error.is_none());
        assert!(v[0].needs_resolve, "re-resolved like an edit");
        assert!(v[0].resolved.is_err());
        assert!(status.contains(&new.to_string()), "{status}");
    }

    #[test]
    fn signature_not_unique_suggests_pointer_scan() {
        let mut v = vec![entry("0x7f001000", false)];
        v[0].resolved = Ok(0x7f00_1000);
        let r = sig(&v[0], 7, 3, Err(SignatureError::NotUnique));
        let (changed, status) = apply_signature(&mut v, 7, 3, r);
        assert!(!changed);
        assert_eq!(
            status,
            "No unique pattern within 64 bytes; for heap values use Pointer scan"
        );
        assert_eq!(v[0].entry.address.to_string(), "0x7f001000");
        assert_eq!(v[0].resolved, Ok(0x7f00_1000));

        let r = sig(&v[0], 7, 3, Err(SignatureError::Unreadable(0x10)));
        let (changed, status) = apply_signature(&mut v, 7, 3, r);
        assert!(!changed);
        assert!(status.contains("0x10"), "{status}");
    }

    #[test]
    fn stale_signature_is_dropped() {
        let new = AddrExpr::parse("aob(\"48 8B\")").unwrap();
        let mut v = vec![entry("0x1000", false)];
        // Another process, another generation, an edited row, a removed row.
        for (pid, generation) in [(9, 3), (7, 4)] {
            let r = sig(&v[0], 7, 3, Ok(new.clone()));
            let (changed, _) = apply_signature(&mut v, pid, generation, r);
            assert!(!changed);
            assert_eq!(v[0].entry.address.to_string(), "0x1000");
        }
        let mut r = sig(&v[0], 7, 3, Ok(new.clone()));
        r.expr = AddrExpr::parse("0x2000").unwrap();
        assert!(!apply_signature(&mut v, 7, 3, r).0);
        let gone = entry("0x1000", false);
        let r = sig(&gone, 7, 3, Ok(new));
        let (changed, status) = apply_signature(&mut v, 7, 3, r);
        assert!(!changed);
        assert!(!status.is_empty());
        assert_eq!(v[0].entry.address.to_string(), "0x1000");
    }
}

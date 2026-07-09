//! A file-backed scan session, for interactive reverse-engineering.
//!
//! Unlike the REPL, each run is independent: candidates are saved to a file
//! between runs, so you can scan, let the target's value change, then narrow —
//! even minutes later or from a separate invocation.
//!
//! Usage (state file via $CHEATU_SESSION, default /tmp/cheatu-session.txt):
//!
//!   cargo run --example session -- <pid> first-any <value>
//!   cargo run --example session -- <pid> first <type> <value>
//!   cargo run --example session -- <pid> next <value>
//!   cargo run --example session -- <pid> next <eq|ne|gt|lt> <value>
//!   cargo run --example session -- <pid> next <changed|unchanged|inc|dec>
//!   cargo run --example session -- <pid> list [n]
//!
//! The state file holds one candidate per line: `<hexaddr> <type> <hexbytes>`.

use std::collections::BTreeMap;
use std::fs;

use cheatu_core::scan::{Candidate, FirstScan, NextScan, Scanner, ANY_TYPES};
use cheatu_core::{ScanType, ScanValue};

fn session_path() -> String {
    std::env::var("CHEATU_SESSION").unwrap_or_else(|_| "/tmp/cheatu-session.txt".to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: session <pid> <first-any|first|next|list> [...]");
        std::process::exit(2);
    }
    let pid: i32 = args[0].parse().expect("pid must be a number");
    let cmd = args[1].as_str();

    match cmd {
        "first-any" => {
            let value = args.get(2).expect("usage: <pid> first-any <value>");
            first(pid, value, ANY_TYPES.to_vec());
        }
        "first" => {
            let ty = ScanType::from_label(args.get(2).expect("type")).expect("bad type");
            let value = args.get(3).expect("usage: <pid> first <type> <value>");
            first(pid, value, vec![ty]);
        }
        "next" => next(pid, &args[2..]),
        "hold" => {
            let value = args.get(2).expect("usage: <pid> hold <value> <ms>");
            let ms: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3000);
            hold(pid, value, ms);
        }
        "list" => {
            let n = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20);
            list(pid, n);
        }
        other => {
            eprintln!("unknown command {other:?}");
            std::process::exit(2);
        }
    }
}

fn first(pid: i32, value: &str, types: Vec<ScanType>) {
    let mut scanner = Scanner::new(pid).expect("attach");
    scanner
        .first_scan(FirstScan::Value {
            value: value.to_string(),
            types,
        })
        .expect("scan");
    save(&scanner);
    report(&scanner);
}

fn next(pid: i32, rest: &[String]) {
    let candidates = load();
    let mut scanner = Scanner::from_candidates(pid, candidates).expect("attach");

    let (cmp, operand): (NextScan, Option<String>) = match rest.first().map(String::as_str) {
        Some("inc") | Some("increased") => (NextScan::Increased, None),
        Some("dec") | Some("decreased") => (NextScan::Decreased, None),
        Some("changed") => (NextScan::Changed, None),
        Some("unchanged") | Some("same") => (NextScan::Unchanged, None),
        Some("eq") => (NextScan::Eq, rest.get(1).cloned()),
        Some("ne") => (NextScan::Ne, rest.get(1).cloned()),
        Some("gt") => (NextScan::Gt, rest.get(1).cloned()),
        Some("lt") => (NextScan::Lt, rest.get(1).cloned()),
        // Bare value -> equals.
        Some(v) => (NextScan::Eq, Some(v.to_string())),
        None => {
            eprintln!("usage: next <value|eq N|ne N|gt N|lt N|changed|unchanged|inc|dec>");
            std::process::exit(2);
        }
    };

    scanner.next_scan(cmp, operand.as_deref()).expect("scan");
    save(&scanner);
    report(&scanner);
}

/// Aggressively write `value` to every candidate for `ms` milliseconds,
/// sampling what's actually there — to see if the target fights back.
fn hold(pid: i32, value: &str, ms: u64) {
    use std::time::{Duration, Instant};
    let candidates = load();
    let scanner = Scanner::from_candidates(pid, candidates).expect("attach");
    let targets: Vec<(u64, ScanValue)> = scanner
        .results()
        .iter()
        .filter_map(|c| c.ty().parse(value).map(|v| (c.addr, v)))
        .collect();
    println!(
        "holding {} address(es) at {value} for {ms} ms…",
        targets.len()
    );

    let start = Instant::now();
    let mut writes: u64 = 0;
    let mut next_sample = Duration::from_millis(0);
    while start.elapsed() < Duration::from_millis(ms) {
        for (addr, v) in &targets {
            let _ = scanner.write_value(*addr, v);
            writes += 1;
        }
        if start.elapsed() >= next_sample {
            if let Some((addr, _)) = targets.first() {
                let ty = scanner.results()[0].ty();
                let live = scanner
                    .read_typed(*addr, ty)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|_| "?".into());
                println!("  t={:>4}ms  reads {live}", start.elapsed().as_millis());
            }
            next_sample += Duration::from_millis(250);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    println!("done ({writes} writes).");
}

fn list(pid: i32, n: usize) {
    let candidates = load();
    let scanner = Scanner::from_candidates(pid, candidates).expect("attach");
    report(&scanner);
    println!("--- first {n} ---");
    for cand in scanner.results().iter().take(n) {
        let ty = cand.ty();
        let cur = scanner
            .read_typed(cand.addr, ty)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "<gone>".into());
        println!(
            "0x{:012x}  {:>3}  was {}  now {}",
            cand.addr,
            ty.label(),
            cand.prev,
            cur
        );
    }
}

/// Print total count plus a per-type histogram (which types survived).
fn report(scanner: &Scanner) {
    let total = scanner.count();
    let mut hist: BTreeMap<&str, usize> = BTreeMap::new();
    for c in scanner.results() {
        *hist.entry(c.ty().label()).or_insert(0) += 1;
    }
    let breakdown: Vec<String> = hist.iter().map(|(t, n)| format!("{t}={n}")).collect();
    println!("CANDIDATES: {total}   [{}]", breakdown.join(" "));
}

fn save(scanner: &Scanner) {
    let mut out = String::new();
    for c in scanner.results() {
        let bytes: String = c
            .prev
            .to_ne_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        out.push_str(&format!("{:x} {} {}\n", c.addr, c.prev.ty().label(), bytes));
    }
    fs::write(session_path(), out).expect("write session file");
}

fn load() -> Vec<Candidate> {
    let text = fs::read_to_string(session_path()).expect("read session file (run a first scan?)");
    let mut out = Vec::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(addr), Some(ty), Some(hex)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let Ok(addr) = u64::from_str_radix(addr, 16) else {
            continue;
        };
        let Some(ty) = ScanType::from_label(ty) else {
            continue;
        };
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
            .collect();
        if let Some(prev) = ScanValue::from_ne_bytes(ty, &bytes) {
            out.push(Candidate { addr, prev });
        }
    }
    out
}

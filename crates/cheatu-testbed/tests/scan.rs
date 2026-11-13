//! Value scans and static expressions against a real, separate process.

mod common;

use std::time::{Duration, Instant};

use cheatu_core::{module_base, AddrExpr, FirstScan, MemSource, NextScan, ScanType, Scanner};
use common::{attach, write, Testbed, MODULE};

#[test]
fn child_dies_on_drop() {
    let tb = Testbed::spawn();
    let pid = tb.pid();
    drop(tb);
    let start = Instant::now();
    while std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
    {
        assert!(start.elapsed() < Duration::from_secs(5), "pid {pid} alive");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn exact_scan_finds_gold_and_decoy() {
    let mut tb = Testbed::spawn();
    let Some(_src) = attach(&tb) else { return };
    let mut scanner = Scanner::new(tb.pid()).unwrap();
    scanner
        .first_scan(FirstScan::Value {
            value: "100".into(),
            types: vec![ScanType::U32],
        })
        .unwrap();
    tb.cmd("fight");
    scanner.next_scan(NextScan::Eq, Some("115")).unwrap();
    let found: Vec<u64> = scanner.results().iter().map(|c| c.addr).collect();
    for name in ["gold", "gold_shown"] {
        let a = tb.addr(name);
        assert!(found.contains(&a), "{name} at {a:#x} not in {found:x?}");
    }
}

#[test]
fn writing_decoy_has_no_lasting_effect() {
    let mut tb = Testbed::spawn();
    let Some(src) = attach(&tb) else { return };
    let a = tb.addr("gold_shown");
    write(&tb, a, &999u32.to_ne_bytes());
    let mut b = [0u8; 4];
    src.read(a, &mut b).unwrap();
    assert_eq!(u32::from_ne_bytes(b), 999, "the write landed");
    let status = tb.cmd("status");
    assert!(
        status.starts_with("gold=100 kills=0 gold_shown=100 "),
        "{status}"
    );
}

#[test]
fn write_gold_is_seen_by_game() {
    let mut tb = Testbed::spawn();
    let Some(_src) = attach(&tb) else { return };
    let a = tb.addr("gold");
    write(&tb, a, &5000u32.to_ne_bytes());
    assert_eq!(tb.get("gold"), "5000");
}

#[test]
fn static_exprs_survive_restart() {
    let mut tb = Testbed::spawn();
    let Some(src) = attach(&tb) else { return };
    let base = module_base(src.regions(), MODULE).unwrap();
    let exprs: Vec<(&str, AddrExpr)> = ["gold", "kills"]
        .into_iter()
        .map(|name| {
            let text = format!("\"{MODULE}\"+0x{:x}", tb.addr(name) - base);
            (name, AddrExpr::parse(&text).unwrap())
        })
        .collect();
    drop(src);
    drop(tb);

    let mut tb2 = Testbed::spawn();
    let src2 = attach(&tb2).unwrap();
    for (name, expr) in exprs {
        assert_eq!(
            expr.resolve(&src2).unwrap(),
            tb2.addr(name),
            "{name}: {expr}"
        );
    }
}

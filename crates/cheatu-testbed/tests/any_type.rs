//! An "Any (unknown type)" search lists each place once per width.

mod common;

use std::collections::HashMap;

use cheatu_core::{FirstScan, Scanner, ANY_TYPES};
use common::{attach, Testbed};

#[test]
fn any_search_lists_each_place_once_per_width() {
    let tb = Testbed::spawn();
    let Some(_src) = attach(&tb) else { return };
    let mut scanner = Scanner::new(tb.pid()).unwrap();
    scanner
        .first_scan(FirstScan::Value {
            value: "100".into(),
            types: ANY_TYPES.to_vec(),
        })
        .unwrap();
    let mut seen: HashMap<(u64, String), usize> = HashMap::new();
    for c in scanner.results() {
        *seen.entry((c.addr, c.ty().friendly_label())).or_default() += 1;
    }
    let twice = seen.values().filter(|&&n| n > 1).count();
    assert_eq!(twice, 0, "{twice} places listed twice under the same width");
}

#[test]
fn any_search_finds_large_unsigned_values() {
    let mut tb = Testbed::spawn();
    let Some(_src) = attach(&tb) else { return };
    let gold = tb.addr("gold");
    common::write(&tb, gold, &3_000_000_000u32.to_ne_bytes());
    let mut scanner = Scanner::new(tb.pid()).unwrap();
    scanner
        .first_scan(FirstScan::Value {
            value: "3000000000".into(),
            types: ANY_TYPES.to_vec(),
        })
        .unwrap();
    assert!(
        scanner
            .results()
            .iter()
            .any(|c| c.addr == gold && c.ty() == cheatu_core::ScanType::U32),
        "gold not found as unsigned 4 Bytes"
    );
}

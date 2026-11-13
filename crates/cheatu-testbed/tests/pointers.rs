//! Pointer chains to the heap `hp`, across a restart and a new level.

mod common;

use cheatu_core::pointer_scan::{find_chains, PointerMap, PtrResults, PtrScanSettings};
use cheatu_core::{ScanProgress, ScanType, ScanValue};
use common::{attach, Testbed, MODULE};

fn settings() -> PtrScanSettings {
    PtrScanSettings {
        max_results: 10_000,
        ..PtrScanSettings::default()
    }
}

/// Pointer-scan `hp` in a fresh testbed, then restart it. Returns the new
/// testbed (attached) and the chains, or `None` when attaching is not allowed.
fn scan_then_restart() -> Option<(Testbed, cheatu_core::LiveSource, PtrResults)> {
    let mut tb = Testbed::spawn();
    let src = attach(&tb)?;
    let target = tb.addr("hp");
    let progress = ScanProgress::default();
    let map = PointerMap::build(&src, &settings(), &progress).unwrap();
    let found = find_chains(&map, &src, target, &settings(), &progress).unwrap();
    assert!(
        found.chains.iter().any(|c| c.module == MODULE),
        "no chain rooted in {MODULE}: {:?}",
        found.chains
    );
    let results = PtrResults {
        process: MODULE.into(),
        target,
        settings: settings(),
        chains: found.chains,
        truncated: found.truncated,
    };
    drop((map, src, tb));

    let tb2 = Testbed::spawn();
    let src2 = attach(&tb2).unwrap();
    Some((tb2, src2, results))
}

// On macOS `Player` sits right after `Level`, so a short chain through
// `Level` itself also reaches `hp`, but only the full chain survives `next`.
#[test]
fn chain_to_hp_survives_restart_and_next() {
    let Some((mut tb, mut src, mut results)) = scan_then_restart() else {
        return;
    };
    let hp = tb.addr("hp");
    assert!(
        results.filter_by_address(&src, hp) >= 1,
        "no chain survived the restart"
    );

    tb.cmd("next");
    src.refresh_regions().unwrap();
    let hp = tb.addr("hp");
    assert!(
        results.filter_by_address(&src, hp) >= 1,
        "no chain survived `next`"
    );
    for c in &results.chains {
        assert_eq!(c.resolve(&src), Ok(hp), "{}", c.to_expr());
    }
}

#[test]
fn filter_by_value_narrows() {
    let Some((mut tb, src, mut results)) = scan_then_restart() else {
        return;
    };
    tb.cmd("fight");
    let hp = tb.addr("hp");
    let left = results.filter_by_value(&src, ScanType::I32, &ScanValue::I32(93));
    assert!(left >= 1);
    assert!(results.chains.iter().any(|c| c.resolve(&src) == Ok(hp)));
}

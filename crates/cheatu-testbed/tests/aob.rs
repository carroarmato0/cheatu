//! `aob(…)` expressions and the signature helper on a real process.

mod common;

use cheatu_core::{make_signature, AddrExpr, LiveSource};
use common::{attach, Testbed};

/// Restart: drop the old testbed and attach to a new one.
fn restart(tb: Testbed, src: LiveSource) -> Option<(Testbed, LiveSource)> {
    drop((src, tb));
    let tb = Testbed::spawn();
    let src = attach(&tb)?;
    Some((tb, src))
}

#[test]
fn aob_marker_survives_restart_and_next() {
    let mut tb = Testbed::spawn();
    let Some(src) = attach(&tb) else { return };
    let hex = tb.cmd("marker");
    let pattern: Vec<String> = (0..hex.len())
        .step_by(2)
        .map(|i| hex[i..i + 2].to_string())
        .collect();
    let expr = AddrExpr::parse(&format!("aob(\"{}\")", pattern.join(" "))).unwrap();
    assert_eq!(expr.resolve(&src), Ok(tb.addr("marker")));

    let Some((mut tb, mut src)) = restart(tb, src) else {
        return;
    };
    assert_eq!(expr.resolve(&src), Ok(tb.addr("marker")), "after restart");

    tb.cmd("next");
    src.refresh_regions().unwrap();
    assert_eq!(expr.resolve(&src), Ok(tb.addr("marker")), "after next");

    while !tb.cmd("fight").starts_with("died") {}
    src.refresh_regions().unwrap();
    assert_eq!(expr.resolve(&src), Ok(tb.addr("marker")), "after dying");
}

#[test]
fn signature_for_ammo_survives_restart() {
    let mut tb = Testbed::spawn();
    let Some(src) = attach(&tb) else { return };
    let expr = make_signature(&src, tb.addr("ammo"), 2, 64).unwrap();

    let Some((mut tb, mut src)) = restart(tb, src) else {
        return;
    };
    assert_eq!(
        expr.resolve(&src),
        Ok(tb.addr("ammo")),
        "after restart: {expr}"
    );

    tb.cmd("next");
    src.refresh_regions().unwrap();
    assert_eq!(
        expr.resolve(&src),
        Ok(tb.addr("ammo")),
        "after next: {expr}"
    );
}

/// The signature does not hold the value itself: after a shot changed the
/// ammo it still finds it.
#[test]
fn signature_for_ammo_survives_a_value_change() {
    let mut tb = Testbed::spawn();
    let Some(src) = attach(&tb) else { return };
    let expr = make_signature(&src, tb.addr("ammo"), 2, 64).unwrap();
    tb.cmd("shoot");
    assert_eq!(
        expr.resolve(&src),
        Ok(tb.addr("ammo")),
        "after a shot: {expr}"
    );
}

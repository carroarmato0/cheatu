//! End-to-end tests that exercise the scan engine against the test process's
//! own address space (always accessible via /proc/self/mem, so no root needed).

use std::hint::black_box;

use cheatu_core::scan::{FirstScan, NextScan, Scanner, ANY_TYPES};
use cheatu_core::{ScanType, ScanValue};

fn own_pid() -> i32 {
    std::process::id() as i32
}

fn exact(value: &str, ty: ScanType) -> FirstScan {
    FirstScan::Value {
        value: value.to_string(),
        types: vec![ty],
    }
}

#[test]
fn first_scan_finds_a_known_value() {
    // A distinctive value, on the heap so it lives in a scannable rw region.
    let boxed = Box::new(0x0123_4567_89ab_cdefu64);
    let addr = &*boxed as *const u64 as u64;

    let mut sc = Scanner::new(own_pid()).unwrap();
    sc.first_scan(exact("81985529216486895", ScanType::U64)).unwrap();

    assert!(
        sc.results().iter().any(|c| c.addr == addr),
        "known value was not found at its actual address (found {} candidates)",
        sc.count()
    );

    // Narrowing by "unchanged" must retain it.
    sc.next_scan(NextScan::Unchanged, None).unwrap();
    assert!(sc.results().iter().any(|c| c.addr == addr));

    black_box(&boxed);
}

#[test]
fn write_then_read_roundtrips() {
    let boxed = Box::new(1000u32);
    let addr = &*boxed as *const u32 as u64;

    let sc = Scanner::new(own_pid()).unwrap();

    // Read it back through the engine.
    let read = sc.read_typed(addr, ScanType::U32).unwrap();
    assert_eq!(read.to_string(), "1000");

    // Write a new value and confirm the underlying variable changed.
    sc.write_value(addr, &ScanValue::U32(1337)).unwrap();
    assert_eq!(*boxed, 1337);

    black_box(&boxed);
}

#[test]
fn next_scan_changed_tracks_updates() {
    let mut counter = Box::new(42i32);
    let addr = &*counter as *const i32 as u64;

    let mut sc = Scanner::new(own_pid()).unwrap();
    sc.first_scan(exact("42", ScanType::I32)).unwrap();
    assert!(sc.results().iter().any(|c| c.addr == addr));

    // Mutate, then an "increased" scan should still contain our address.
    *counter = 99;
    black_box(&counter);
    sc.next_scan(NextScan::Increased, None).unwrap();
    assert!(
        sc.results().iter().any(|c| c.addr == addr),
        "address dropped after value increased 42 -> 99"
    );
}

#[test]
fn read_typed_decodes_independent_of_scan_type() {
    let boxed = Box::new(-5i16);
    let addr = &*boxed as *const i16 as u64;

    let sc = Scanner::new(own_pid()).unwrap();
    let v = sc.read_typed(addr, ScanType::I16).unwrap();
    assert_eq!(v.to_string(), "-5");

    black_box(&boxed);
}

#[test]
fn any_scan_finds_value_regardless_of_type() {
    // A float value stored blind: an "Any" scan should find it as f32 without
    // us telling the scanner it's a float.
    let as_float = Box::new(1234.5f32);
    let faddr = &*as_float as *const f32 as u64;
    // And an integer value at another address should also turn up.
    let as_int = Box::new(1234i32);
    let iaddr = &*as_int as *const i32 as u64;

    let mut sc = Scanner::new(own_pid()).unwrap();
    sc.first_scan(FirstScan::Value {
        value: "1234.5".to_string(),
        types: ANY_TYPES.to_vec(),
    })
    .unwrap();

    // The float is found and tracked as f32.
    let hit = sc.results().iter().find(|c| c.addr == faddr);
    assert!(hit.is_some(), "float value not found by Any scan");
    assert_eq!(hit.unwrap().ty(), ScanType::F32);

    // "1234.5" can't be an integer, so the i32 at iaddr must NOT be a hit.
    assert!(
        !sc.results().iter().any(|c| c.addr == iaddr),
        "integer address wrongly matched a fractional value"
    );

    // Now an integer-valued Any scan should find the i32 (as i32 and/or others).
    let mut sc2 = Scanner::new(own_pid()).unwrap();
    sc2.first_scan(FirstScan::Value {
        value: "1234".to_string(),
        types: ANY_TYPES.to_vec(),
    })
    .unwrap();
    assert!(
        sc2.results()
            .iter()
            .any(|c| c.addr == iaddr && c.ty() == ScanType::I32),
        "integer value not found as i32 by Any scan"
    );

    black_box((&as_float, &as_int));
}

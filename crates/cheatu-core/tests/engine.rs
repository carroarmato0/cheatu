//! End-to-end tests that exercise the scan engine against the test process's
//! own address space (always accessible via /proc/self/mem, so no root needed).

use std::hint::black_box;

use cheatu_core::scan::{parse_aob, FirstScan, NextScan, Scanner, ANY_TYPES};
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
    sc.first_scan(exact("81985529216486895", ScanType::U64))
        .unwrap();

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
fn undo_restores_the_pre_narrowing_candidate_set() {
    let counter = Box::new(42i32);
    let addr = &*counter as *const i32 as u64;

    let mut sc = Scanner::new(own_pid()).unwrap();
    sc.first_scan(exact("42", ScanType::I32)).unwrap();
    let before = sc.count();
    assert!(sc.results().iter().any(|c| c.addr == addr));
    assert!(
        !sc.can_undo(),
        "fresh first scan should have nothing to undo"
    );

    // An accidental "decreased" scan drops our unchanged address.
    black_box(&counter);
    sc.next_scan(NextScan::Decreased, None).unwrap();
    assert!(!sc.results().iter().any(|c| c.addr == addr));

    // Undo brings the pre-scan set back.
    assert!(sc.can_undo());
    assert!(sc.undo());
    assert_eq!(sc.count(), before);
    assert!(sc.results().iter().any(|c| c.addr == addr));
    assert!(!sc.can_undo(), "history is exhausted after one undo");

    black_box(&counter);
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

#[test]
fn narrowing_by_displayed_integer_keeps_fractional_float() {
    // Full HP is an exact 200.0, so the first scan lands on it cleanly.
    let mut hp = Box::new(200.0f32);
    let addr = &*hp as *const f32 as u64;

    let mut sc = Scanner::new(own_pid()).unwrap();
    sc.first_scan(exact("200", ScanType::F32)).unwrap();
    assert!(
        sc.results().iter().any(|c| c.addr == addr),
        "float not found by first scan at full HP"
    );

    // HP drifts to a fractional value; the display still reads 199.
    *hp = 199.37;

    // Narrowing by the displayed integer must NOT cull the real float.
    sc.next_scan(NextScan::Eq, Some("199")).unwrap();
    assert!(
        sc.results().iter().any(|c| c.addr == addr),
        "fractional float wrongly culled when narrowing by displayed integer"
    );

    // But a decimal operand forces an exact match, which 199.37 fails.
    sc.next_scan(NextScan::Eq, Some("199.0")).unwrap();
    assert!(
        !sc.results().iter().any(|c| c.addr == addr),
        "decimal operand should force exact float match"
    );

    black_box(&hp);
}

#[test]
fn parse_aob_handles_wildcards() {
    assert_eq!(
        parse_aob("48 65 ?? 6C 6F"),
        Some(vec![Some(0x48), Some(0x65), None, Some(0x6C), Some(0x6F),])
    );
    assert_eq!(
        parse_aob("zz"),
        None,
        "invalid hex token must fail to parse"
    );
    assert_eq!(parse_aob(""), None, "empty pattern must fail to parse");
}

#[test]
fn pattern_scan_finds_a_string() {
    let text: Box<[u8]> = Box::from(*b"CHEATU_MARKER_1234");
    let addr = text.as_ptr() as u64;

    let mut sc = Scanner::new(own_pid()).unwrap();
    let pattern: Vec<Option<u8>> = b"CHEATU_MARKER_1234".iter().map(|&b| Some(b)).collect();
    sc.first_scan(FirstScan::Pattern(pattern)).unwrap();

    assert!(
        sc.results().iter().any(|c| c.addr == addr),
        "string pattern not found at its actual address"
    );

    black_box(&text);
}

#[test]
fn pattern_scan_matches_wildcards() {
    let bytes: Box<[u8]> = Box::from(*b"\xAA\xBB\xCC\xDD");
    let addr = bytes.as_ptr() as u64;

    let mut sc = Scanner::new(own_pid()).unwrap();
    let pattern = parse_aob("AA ?? CC DD").unwrap();
    sc.first_scan(FirstScan::Pattern(pattern)).unwrap();

    let hit = sc.results().iter().find(|c| c.addr == addr);
    assert!(hit.is_some(), "wildcard AoB pattern not found");
    assert_eq!(
        hit.unwrap().prev.to_string(),
        "AA BB CC DD",
        "matched bytes should include the concrete byte at the wildcard position"
    );

    black_box(&bytes);
}

// ---- "Real value" hint heuristic --------------------------------------------

use cheatu_core::scan::{address_hint, Confidence};
use cheatu_core::{region_for, MemoryRegion, RegionKind};

fn region(start: u64, end: u64, exec: bool, path: &str) -> MemoryRegion {
    MemoryRegion {
        start,
        end,
        read: true,
        write: true,
        exec,
        shared: false,
        path: path.to_string(),
    }
}

#[test]
fn region_kind_classifies_by_path() {
    assert_eq!(region(0, 1, false, "[heap]").kind(), RegionKind::Heap);
    assert_eq!(region(0, 1, false, "[stack]").kind(), RegionKind::Stack);
    // A library's writable, non-exec segment is module data/bss.
    assert_eq!(
        region(0, 1, false, "/usr/lib/libfoo.so").kind(),
        RegionKind::ModuleData
    );
    // Its executable segment is not.
    assert_eq!(
        region(0, 1, true, "/usr/lib/libfoo.so").kind(),
        RegionKind::Other
    );
    assert_eq!(region(0, 1, false, "").kind(), RegionKind::Anonymous);
}

#[test]
fn address_hint_ranks_region_and_type() {
    assert_eq!(
        address_hint(RegionKind::Heap, ScanType::I32).confidence,
        Confidence::Likely
    );
    assert_eq!(
        address_hint(RegionKind::ModuleData, ScanType::I32).confidence,
        Confidence::Likely
    );
    assert_eq!(
        address_hint(RegionKind::Stack, ScanType::I32).confidence,
        Confidence::Unlikely
    );
    assert_eq!(
        address_hint(RegionKind::Anonymous, ScanType::I32).confidence,
        Confidence::Neutral
    );
    // A byte/string match is display text regardless of region.
    let text = address_hint(RegionKind::Heap, ScanType::Bytes(3));
    assert_eq!(text.confidence, Confidence::Unlikely);
    assert_eq!(text.label, "text");
}

#[test]
fn probe_holds_and_restores_untouched_memory() {
    use std::time::Duration;

    use cheatu_core::mem::{probe_address, ProbeOutcome};
    use cheatu_core::Mem;

    // A value nothing else writes to, so the sentinel must survive the wait.
    let mut v = Box::new(0x1122_3344u32);
    let addr = &*v as *const u32 as u64;

    let mem = Mem::open(own_pid()).unwrap();
    let outcome = probe_address(&mem, addr, 4, Duration::from_millis(10)).unwrap();

    assert_eq!(outcome, ProbeOutcome::Held);
    // Held must restore the original bytes.
    assert_eq!(*v, 0x1122_3344, "probe should restore the original value");
    black_box(&mut v);
}

#[test]
fn region_for_finds_containing_region() {
    let regions = vec![
        region(0x1000, 0x2000, false, "[heap]"),
        region(0x3000, 0x4000, false, "[stack]"),
    ];
    assert_eq!(
        region_for(&regions, 0x1500).map(|r| r.kind()),
        Some(RegionKind::Heap)
    );
    assert_eq!(
        region_for(&regions, 0x3000).map(|r| r.kind()),
        Some(RegionKind::Stack)
    );
    // End is exclusive; gaps return None.
    assert!(region_for(&regions, 0x2000).is_none());
    assert!(region_for(&regions, 0x9999).is_none());
}

//! macOS backend tests. Like `engine.rs`, they target the test process's own
//! task, which is always accessible without root.
#![cfg(target_os = "macos")]

use std::hint::black_box;

use cheatu_core::{read_maps, region_for, Mem, RegionKind};

fn own_pid() -> i32 {
    std::process::id() as i32
}

#[test]
fn maps_contain_heap_holding_a_boxed_value() {
    let b = black_box(Box::new(0x00C0_FFEEu32));
    let addr = &*b as *const u32 as u64;
    let maps = read_maps(own_pid()).unwrap();
    let r = region_for(&maps, addr).expect("boxed value is mapped");
    assert!(r.read && r.write);
    assert_eq!(r.kind(), RegionKind::Heap, "region: {r:?}");
}

#[test]
fn maps_contain_stack_holding_a_local() {
    let local = black_box(0x5151_5151u64);
    let addr = &local as *const u64 as u64;
    let maps = read_maps(own_pid()).unwrap();
    let r = region_for(&maps, addr).expect("local is mapped");
    assert!(r.read && r.write);
    assert_eq!(r.kind(), RegionKind::Stack, "region: {r:?}");
}

#[test]
fn maps_are_sorted_and_non_overlapping() {
    let maps = read_maps(own_pid()).unwrap();
    assert!(
        maps.len() > 10,
        "expected a real address space, got {maps:?}"
    );
    for w in maps.windows(2) {
        assert!(w[0].start < w[0].end, "empty region {:?}", w[0]);
        assert!(w[0].end <= w[1].start, "{:?} overlaps {:?}", w[0], w[1]);
    }
}

#[test]
fn maps_name_the_test_executable() {
    let exe = std::env::current_exe().unwrap();
    let exe = exe.canonicalize().unwrap();
    let maps = read_maps(own_pid()).unwrap();
    assert!(
        maps.iter().any(|r| std::path::Path::new(&r.path) == exe),
        "no region backed by {exe:?}"
    );
}

#[test]
fn read_at_unmapped_address_errors() {
    let mem = Mem::open(own_pid()).unwrap();
    let mut buf = [0u8; 4];
    assert!(mem.read_at(0x8, &mut buf).is_err());
}

#[test]
fn write_to_read_only_page_restores_protection() {
    // A private anonymous page we make read-only ourselves.
    // SAFETY: plain mmap/mprotect of a fresh page we own; unmapped at the end.
    let page = unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            16384,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        );
        assert_ne!(p, libc::MAP_FAILED);
        *(p as *mut u32) = 0x1234_5678;
        assert_eq!(libc::mprotect(p, 16384, libc::PROT_READ), 0);
        p
    };
    let addr = page as u64;

    let mem = Mem::open(own_pid()).unwrap();
    mem.write_at(addr, &0x8765_4321u32.to_le_bytes()).unwrap();

    let mut back = [0u8; 4];
    mem.read_at(addr, &mut back).unwrap();
    assert_eq!(u32::from_le_bytes(back), 0x8765_4321);
    // SAFETY: the page is mapped and readable.
    assert_eq!(
        unsafe { std::ptr::read_volatile(page as *const u32) },
        0x8765_4321
    );

    let maps = read_maps(own_pid()).unwrap();
    let r = region_for(&maps, addr).unwrap();
    assert!(
        r.read && !r.write,
        "original read-only protection restored: {r:?}"
    );

    // SAFETY: unmapping the page we mapped above.
    unsafe { libc::munmap(page, 16384) };
}

#[test]
fn open_nonexistent_pid_errors() {
    let err = Mem::open(i32::MAX).err().expect("no such process");
    assert_ne!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
}

#[test]
fn process_list_contains_self() {
    let me = cheatu_core::list_processes()
        .into_iter()
        .find(|p| p.pid == own_pid())
        .expect("self listed");
    assert!(!me.name.is_empty());
    assert!(me.cmdline.contains("macos"), "cmdline: {}", me.cmdline);
    assert!(me.rss_bytes > 0);
    assert!(me.accessible);
    assert!(!me.is_wine);
}

#[test]
fn process_list_is_sorted_by_pid() {
    let procs = cheatu_core::list_processes();
    assert!(procs.len() > 5, "expected several processes");
    for w in procs.windows(2) {
        assert!(w[0].pid < w[1].pid);
    }
}

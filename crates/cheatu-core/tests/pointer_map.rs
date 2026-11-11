//! The disk-backed pointer map. Live tests scan the test process itself;
//! failure, cancel and exact-content tests use an in-memory fake source.
//!
//! Every test holds `LOCK`: the cleanup checks look for leftover
//! `cheatu-ptrmap-<pid>-*` directories, which a concurrently running test in
//! this binary would otherwise create.

use std::io;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, MutexGuard};

use cheatu_core::pointer_scan::{PointerMap, PtrScanSettings};
use cheatu_core::source::{LiveSource, MemSource};
use cheatu_core::{MemoryRegion, ScanProgress};

static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn live() -> LiveSource {
    LiveSource::open(std::process::id() as i32).expect("open own process")
}

/// Temp directories this process's pointer maps have left behind.
fn leftover_dirs() -> Vec<String> {
    let prefix = format!("cheatu-ptrmap-{}-", std::process::id());
    std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&prefix))
        .collect()
}

struct Holder {
    ptr: *const u64,
    _pad: u64,
}

#[test]
fn finds_known_pointers() {
    let _g = lock();
    let target = Box::new(0xDEAD_BEEF_u64);
    let addr = &*target as *const u64 as u64;
    let holder = Box::new(Holder {
        ptr: &*target,
        _pad: 1,
    });
    let field = std::hint::black_box(&holder.ptr) as *const *const u64 as u64;

    let map = PointerMap::build(
        &live(),
        &PtrScanSettings::default(),
        &ScanProgress::default(),
    )
    .expect("build");
    let hits: Vec<_> = map.pointers_into(addr, addr + 1).collect();
    assert!(
        hits.contains(&(addr, field)),
        "field {field:#x} not among {hits:x?}"
    );
    assert!(hits.iter().all(|&(v, _)| v == addr));
    assert!(!map.is_static(field), "heap location is not static");
    std::hint::black_box(&holder);
    drop(map);
    assert!(leftover_dirs().is_empty(), "drop removes the temp dir");
}

static ROOT: AtomicPtr<u64> = AtomicPtr::new(std::ptr::null_mut());

#[test]
fn static_locations_marked() {
    let _g = lock();
    let target = Box::into_raw(Box::new(0x1234_u64));
    ROOT.store(target, Ordering::SeqCst);
    let root_loc = &ROOT as *const AtomicPtr<u64> as u64;
    let heap_holder = Box::new(Holder {
        ptr: target,
        _pad: 2,
    });
    let heap_loc = std::hint::black_box(&heap_holder.ptr) as *const *const u64 as u64;

    let map = PointerMap::build(
        &live(),
        &PtrScanSettings::default(),
        &ScanProgress::default(),
    )
    .expect("build");
    let hits: Vec<_> = map
        .pointers_into(target as u64, target as u64 + 1)
        .collect();
    assert!(hits.contains(&(target as u64, root_loc)));
    assert!(hits.contains(&(target as u64, heap_loc)));
    assert!(map.is_static(root_loc), "static AtomicPtr is static");
    assert!(!map.is_static(heap_loc), "heap location is not static");

    std::hint::black_box(&heap_holder);
    ROOT.store(std::ptr::null_mut(), Ordering::SeqCst);
    unsafe { drop(Box::from_raw(target)) };
}

#[test]
fn small_chunks_still_sorted() {
    let _g = lock();
    let target = Box::new(0x5555_u64);
    let addr = &*target as *const u64 as u64;
    let holder = Box::new(Holder {
        ptr: &*target,
        _pad: 3,
    });
    let field = std::hint::black_box(&holder.ptr) as *const *const u64 as u64;

    let settings = PtrScanSettings {
        chunk_entries: 1024,
        ..PtrScanSettings::default()
    };
    let map = PointerMap::build(&live(), &settings, &ScanProgress::default()).expect("build");
    assert!(
        map.len() > 4 * 1024,
        "needs several runs, got {}",
        map.len()
    );

    // Allocate once and compare iterators: freed buffers stay mapped in this
    // process and would be scanned (and slow down) the next live test.
    let mut all = Vec::with_capacity(map.len() as usize);
    all.extend(map.pointers_into(0, u64::MAX));
    assert_eq!(all.len() as u64, map.len());
    assert!(
        all.windows(2).all(|w| w[0] <= w[1]),
        "sorted by value, location"
    );
    assert!(all.contains(&(addr, field)));

    // Range queries agree with a filter over the full list, at sampled bounds.
    let step = (all.len() / 37).max(1);
    for i in (0..all.len()).step_by(step) {
        let lo = all[i].0;
        let hi = all[(i + step * 3).min(all.len() - 1)].0;
        let want = all.iter().copied().filter(|&(v, _)| v >= lo && v < hi);
        assert!(
            map.pointers_into(lo, hi).eq(want),
            "range [{lo:#x}, {hi:#x})"
        );
    }
    std::hint::black_box(&holder);
}

/// Synthetic address space. Reads of any address at or past `fail_from`
/// fail; `fail_chunk` makes one specific chunk fail.
struct Fake {
    regions: Vec<MemoryRegion>,
    data: Vec<(u64, Vec<u8>)>,
    fail_from: u64,
    fail_at: Option<u64>,
}

impl MemSource for Fake {
    fn regions(&self) -> &[MemoryRegion] {
        &self.regions
    }

    fn read(&self, addr: u64, buf: &mut [u8]) -> io::Result<usize> {
        if addr >= self.fail_from || self.fail_at == Some(addr) {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        for (base, bytes) in &self.data {
            let end = base + bytes.len() as u64;
            if addr >= *base && addr < end {
                let off = (addr - base) as usize;
                let n = buf.len().min(bytes.len() - off);
                buf[..n].copy_from_slice(&bytes[off..off + n]);
                return Ok(n);
            }
        }
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
}

fn rw(start: u64, len: u64, path: &str) -> MemoryRegion {
    MemoryRegion {
        start,
        end: start + len,
        read: true,
        write: true,
        exec: false,
        shared: false,
        path: path.into(),
    }
}

const CHUNK: u64 = 4 * 1024 * 1024;

/// One module image at 0x10000 (read-only + rw data) and `n` anonymous rw
/// regions of one chunk each, starting at 0x1000_0000. Every region is
/// filled with pointers to 0x10000 + i * 8.
fn fake(n: u64) -> Fake {
    let mut regions = vec![
        MemoryRegion {
            write: false,
            ..rw(0x10000, 0x10000, "/usr/lib/libgame.dylib")
        },
        rw(0x20000, 0x10000, "/usr/lib/libgame.dylib"),
    ];
    for i in 0..n {
        regions.push(rw(0x1000_0000 + i * CHUNK, CHUNK, ""));
    }
    let data = regions
        .iter()
        .map(|r| {
            let mut bytes = vec![0u8; r.size() as usize];
            for (i, slot) in bytes
                .as_chunks_mut::<8>()
                .0
                .iter_mut()
                .enumerate()
                .take(4096)
            {
                *slot = (0x10000 + i as u64 * 8).to_le_bytes();
            }
            (r.start, bytes)
        })
        .collect();
    Fake {
        regions,
        data,
        fail_from: u64::MAX,
        fail_at: None,
    }
}

#[test]
fn exact_contents_from_fake() {
    let _g = lock();
    let src = fake(1);
    let progress = ScanProgress::default();
    let map = PointerMap::build(&src, &PtrScanSettings::default(), &progress).unwrap();
    // Only rw regions are scanned: the rw module data and the anonymous one.
    assert_eq!(map.len(), 2 * 4096);
    assert_eq!(map.failed_chunks(), 0);
    assert_eq!(progress.fraction(), Some(1.0));
    let hits: Vec<_> = map.pointers_into(0x10008, 0x10010).collect();
    assert_eq!(hits, vec![(0x10008, 0x20008), (0x10008, 0x1000_0008)]);
    assert!(map.is_static(0x20008));
    assert!(map.is_static(0x10000));
    assert!(!map.is_static(0x1000_0008));
    assert!(!map.is_static(0x30000));
}

#[test]
fn values_outside_readable_memory_are_ignored() {
    let _g = lock();
    let mut src = fake(1);
    // Zero, and an address in the gap between regions.
    let bytes = &mut src.data[2].1;
    bytes[..8].copy_from_slice(&0u64.to_le_bytes());
    bytes[8..16].copy_from_slice(&0x5000_0000u64.to_le_bytes());
    let map =
        PointerMap::build(&src, &PtrScanSettings::default(), &ScanProgress::default()).unwrap();
    assert_eq!(map.len(), 2 * 4096 - 2);
    assert_eq!(map.pointers_into(0, 0x10000).count(), 0);
}

#[test]
fn empty_map_works() {
    let _g = lock();
    let mut src = fake(1);
    for (_, bytes) in &mut src.data {
        bytes.fill(0);
    }
    let map =
        PointerMap::build(&src, &PtrScanSettings::default(), &ScanProgress::default()).unwrap();
    assert_eq!(map.len(), 0);
    assert_eq!(map.pointers_into(0, u64::MAX).count(), 0);
    drop(map);
    assert!(leftover_dirs().is_empty());
}

#[test]
fn few_read_failures_are_skipped() {
    let _g = lock();
    let mut src = fake(4);
    src.fail_at = Some(0x1000_0000 + CHUNK);
    let map =
        PointerMap::build(&src, &PtrScanSettings::default(), &ScanProgress::default()).unwrap();
    assert_eq!(map.failed_chunks(), 1);
    assert_eq!(map.len(), 4 * 4096);
}

#[test]
fn read_failure_cleans_up() {
    let _g = lock();
    // The rw module data region reads fine; every anonymous chunk after it
    // fails, so most chunks fail: the process is gone.
    let mut src = fake(5);
    src.fail_from = 0x1000_0000;
    let settings = PtrScanSettings {
        chunk_entries: 1024,
        ..PtrScanSettings::default()
    };
    let err = PointerMap::build(&src, &settings, &ScanProgress::default())
        .err()
        .expect("build fails");
    assert_ne!(err.kind(), io::ErrorKind::Interrupted);
    assert!(leftover_dirs().is_empty(), "left {:?}", leftover_dirs());
}

#[test]
fn cancel_stops_and_cleans_up() {
    let _g = lock();
    let src = fake(2);
    let progress = ScanProgress::default();
    progress.cancel();
    let err = PointerMap::build(&src, &PtrScanSettings::default(), &progress)
        .err()
        .expect("cancelled");
    assert_eq!(err.kind(), io::ErrorKind::Interrupted);
    assert!(leftover_dirs().is_empty(), "left {:?}", leftover_dirs());
}

#[test]
fn default_settings() {
    let s = PtrScanSettings::default();
    assert_eq!(
        (
            s.max_depth,
            s.max_offset,
            s.alignment,
            s.max_results,
            s.chunk_entries
        ),
        (5, 0x1000, 8, 1_000_000, 64 * 1024 * 1024)
    );
}

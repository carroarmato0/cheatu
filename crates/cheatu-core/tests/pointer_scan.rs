//! Pointer chain search, the results file and the post-restart filters.
//!
//! Live tests build `static ROOT -> A (pointer at +0x18) -> B (value at
//! +0x40)` in the test process and scan it. The process holds many other
//! pointers, so they assert the expected chain is *among* the results.
//! Exact-result, cancel and file tests use an in-memory fake source.
//!
//! Every live test holds `LOCK`: they share `ROOT`, and concurrent pointer
//! maps would slow each other down.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, MutexGuard};

use cheatu_core::pointer_scan::{
    find_chains, ChainSearch, PointerChain, PointerMap, PtrResults, PtrScanSettings,
};
use cheatu_core::source::{LiveSource, MemSource};
use cheatu_core::{
    module_containing, AddrExpr, MemoryRegion, ResolveError, ScanProgress, ScanType, ScanValue,
};

static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn live() -> LiveSource {
    LiveSource::open(std::process::id() as i32).expect("open own process")
}

#[repr(C)]
struct B {
    _pad: [u64; 8],
    value: i32,
    _tail: i32,
}

#[repr(C)]
struct A {
    _pad: [u64; 3],
    b: *mut B,
}

static ROOT: AtomicPtr<A> = AtomicPtr::new(std::ptr::null_mut());

/// A live `ROOT -> A -> B` structure; freed on drop.
struct Structure {
    a: *mut A,
    b: *mut B,
}

impl Structure {
    fn new(value: i32) -> Structure {
        let b = Box::into_raw(Box::new(B {
            _pad: [0; 8],
            value,
            _tail: 0,
        }));
        let a = Box::into_raw(Box::new(A { _pad: [0; 3], b }));
        ROOT.store(a, Ordering::SeqCst);
        Structure { a, b }
    }

    /// Address of `B::value`, the scan target.
    fn target(&self) -> u64 {
        unsafe { std::ptr::addr_of!((*self.b).value) as u64 }
    }
}

impl Drop for Structure {
    fn drop(&mut self) {
        if ROOT.load(Ordering::SeqCst) == self.a {
            ROOT.store(std::ptr::null_mut(), Ordering::SeqCst);
        }
        unsafe {
            drop(Box::from_raw(self.a));
            drop(Box::from_raw(self.b));
        }
    }
}

/// The chain the live structure should produce.
fn expected_chain(src: &dyn MemSource) -> PointerChain {
    let root_loc = &ROOT as *const AtomicPtr<A> as u64;
    let (module, base) = module_containing(src.regions(), root_loc).expect("ROOT in a module");
    PointerChain {
        module,
        base_offset: root_loc - base,
        offsets: vec![0x18, 0x40],
    }
}

fn search(src: &dyn MemSource, target: u64, settings: &PtrScanSettings) -> ChainSearch {
    let progress = ScanProgress::default();
    let map = PointerMap::build(src, settings, &progress).expect("build map");
    find_chains(&map, src, target, settings, &progress).expect("search")
}

/// Live searches only need depth 2; deeper ones are slow in a test process
/// full of pointers and add nothing to what is checked.
fn shallow() -> PtrScanSettings {
    PtrScanSettings {
        max_depth: 2,
        ..PtrScanSettings::default()
    }
}

#[test]
fn finds_known_chain() {
    let _g = lock();
    let s = Structure::new(7);
    let src = live();
    let found = search(&src, s.target(), &PtrScanSettings::default());
    let want = expected_chain(&src);
    assert!(
        found.chains.contains(&want),
        "{want:x?} not among {} chains",
        found.chains.len()
    );
    // Shortest first.
    assert!(found
        .chains
        .windows(2)
        .all(|w| w[0].offsets.len() <= w[1].offsets.len()));
    assert_eq!(want.resolve(&src), Ok(s.target()));
}

#[test]
fn respects_depth_and_offset_limits() {
    let _g = lock();
    let s = Structure::new(8);
    let src = live();
    let want = expected_chain(&src);

    let depth1 = PtrScanSettings {
        max_depth: 1,
        ..shallow()
    };
    let found = search(&src, s.target(), &depth1);
    assert!(!found.chains.contains(&want));
    assert!(found.chains.iter().all(|c| c.offsets.len() == 1));

    let small = PtrScanSettings {
        max_offset: 0x20,
        ..shallow()
    };
    let found = search(&src, s.target(), &small);
    assert!(!found.chains.contains(&want));
    assert!(found
        .chains
        .iter()
        .flat_map(|c| &c.offsets)
        .all(|&o| o <= 0x20));
}

#[test]
fn result_cap() {
    let _g = lock();
    let s = Structure::new(9);
    let src = live();
    let capped = PtrScanSettings {
        max_results: 3,
        ..shallow()
    };
    let found = search(&src, s.target(), &capped);
    assert!(found.chains.len() <= 3, "got {}", found.chains.len());
}

fn results_for(src: &dyn MemSource, target: u64, settings: PtrScanSettings) -> PtrResults {
    let found = search(src, target, &settings);
    PtrResults {
        process: "self".into(),
        target,
        settings,
        chains: found.chains,
        truncated: found.truncated,
    }
}

#[test]
fn restart_simulation_keeps_real_chain() {
    let _g = lock();
    let old = Structure::new(10);
    let src = live();
    let mut results = results_for(&src, old.target(), shallow());
    let want = expected_chain(&src);
    assert!(results.chains.contains(&want));
    let before = results.chains.len();

    // "Restart": the structure moves; allocate the new one before freeing the
    // old so the addresses differ.
    let new = Structure::new(10);
    assert_ne!(new.target(), old.target());
    drop(old);

    let left = results.filter_by_address(&src, new.target());
    assert_eq!(left, results.chains.len());
    assert!(results.chains.contains(&want), "real chain survives");
    assert!(left >= 1 && left <= before);
    for c in &results.chains {
        assert_eq!(c.resolve(&src), Ok(new.target()));
    }
}

#[test]
fn filter_by_value_works() {
    let _g = lock();
    let s = Structure::new(4242);
    let src = live();
    let mut results = results_for(&src, s.target(), shallow());
    let want = expected_chain(&src);
    let left = results.filter_by_value(&src, ScanType::I32, &ScanValue::I32(4242));
    assert_eq!(left, results.chains.len());
    assert!(results.chains.contains(&want));

    unsafe { (*s.b).value = 1 };
    let left = results.filter_by_value(&src, ScanType::I32, &ScanValue::I32(4242));
    assert!(!results.chains.contains(&want), "{left} left");
}

#[test]
fn null_intermediate_dropped() {
    let _g = lock();
    let s = Structure::new(11);
    let src = live();
    let mut results = results_for(&src, s.target(), shallow());
    let want = expected_chain(&src);
    assert!(results.chains.contains(&want));

    let b = unsafe { (*s.a).b };
    unsafe { (*s.a).b = std::ptr::null_mut() };
    results.filter_by_address(&src, s.target());
    assert!(!results.chains.contains(&want));
    unsafe { (*s.a).b = b };
}

// ---------------------------------------------------------------------------
// Fake source

struct Fake {
    regions: Vec<MemoryRegion>,
    data: Vec<(u64, Vec<u8>)>,
}

impl MemSource for Fake {
    fn regions(&self) -> &[MemoryRegion] {
        &self.regions
    }

    fn read(&self, addr: u64, buf: &mut [u8]) -> io::Result<usize> {
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

impl Fake {
    fn put(&mut self, addr: u64, value: u64) {
        for (base, bytes) in &mut self.data {
            if addr >= *base && addr + 8 <= *base + bytes.len() as u64 {
                let off = (addr - *base) as usize;
                bytes[off..off + 8].copy_from_slice(&value.to_le_bytes());
                return;
            }
        }
        panic!("{addr:#x} not mapped");
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

const HEAP: u64 = 0x1000_0000;
const T: u64 = HEAP + 0x3040;

/// `libgame.dylib` text at 0x10000 and data at 0x20000, a heap at
/// 0x1000_0000 (`A` at +0x100, `B` at +0x3000). Chains to `T`:
/// * `"libgame.dylib"+0x10020 -> +0x10` (depth 1)
/// * `"libgame.dylib"+0x10010 -> +0x18 -> +0x40` (depth 2)
fn fake() -> Fake {
    let regions = vec![
        MemoryRegion {
            write: false,
            ..rw(0x10000, 0x10000, "/usr/lib/libgame.dylib")
        },
        rw(0x20000, 0x1000, "/usr/lib/libgame.dylib"),
        rw(HEAP, 0x4000, ""),
    ];
    let data = regions
        .iter()
        .map(|r| (r.start, vec![0u8; r.size() as usize]))
        .collect();
    let mut f = Fake { regions, data };
    f.put(0x20010, HEAP + 0x100);
    f.put(HEAP + 0x118, HEAP + 0x3000);
    f.put(0x20020, T - 0x10);
    f
}

fn chain(base_offset: u64, offsets: &[u64]) -> PointerChain {
    PointerChain {
        module: "libgame.dylib".into(),
        base_offset,
        offsets: offsets.to_vec(),
    }
}

#[test]
fn exact_chains_from_fake() {
    let src = fake();
    let found = search(&src, T, &PtrScanSettings::default());
    assert_eq!(
        found.chains,
        vec![chain(0x10020, &[0x10]), chain(0x10010, &[0x18, 0x40])]
    );
    assert!(!found.truncated);
    for c in &found.chains {
        assert_eq!(c.resolve(&src), Ok(T));
    }
}

#[test]
fn result_cap_sets_truncated() {
    let src = fake();
    let settings = PtrScanSettings {
        max_results: 1,
        ..PtrScanSettings::default()
    };
    let found = search(&src, T, &settings);
    assert_eq!(found.chains, vec![chain(0x10020, &[0x10])]);
    assert!(found.truncated);
}

#[test]
fn cancel_returns_interrupted() {
    let src = fake();
    let settings = PtrScanSettings::default();
    let progress = ScanProgress::default();
    let map = PointerMap::build(&src, &settings, &progress).unwrap();
    progress.cancel();
    let err = find_chains(&map, &src, T, &settings, &progress).expect_err("cancelled");
    assert_eq!(err.kind(), io::ErrorKind::Interrupted);
}

#[test]
fn to_expr_matches_resolve() {
    let src = fake();
    for c in [chain(0x10020, &[0x10]), chain(0x10010, &[0x18, 0x40])] {
        assert_eq!(c.to_expr().resolve(&src), c.resolve(&src));
    }
    let c = chain(0x10010, &[0x18, 0x40]);
    assert_eq!(
        c.to_expr().to_string(),
        "[[\"libgame.dylib\"+0x10010]+0x18]+0x40"
    );
    assert_eq!(AddrExpr::parse(&c.to_expr().to_string()), Ok(c.to_expr()));
    // A broken chain fails the same way both ways: T holds 0, so the third
    // step lands on 0x8, which is unmapped.
    let bad = chain(0x10010, &[0x18, 0x40, 0x8, 0]);
    assert_eq!(bad.to_expr().resolve(&src), bad.resolve(&src));
    assert!(matches!(
        bad.resolve(&src),
        Err(ResolveError::Unreadable(_))
    ));
}

// ---------------------------------------------------------------------------
// Results file

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("cheatu-ptrtest-{}-{name}", std::process::id()))
}

fn sample() -> PtrResults {
    PtrResults {
        process: "game".into(),
        target: 0x7f00_1234,
        settings: PtrScanSettings {
            max_depth: 3,
            ..PtrScanSettings::default()
        },
        chains: vec![
            chain(0x10, &[0x8]),
            PointerChain {
                module: "other.so".into(),
                base_offset: 0x20,
                offsets: vec![0, 0x18, 0xff8],
            },
            chain(0x30, &[0x40, 0x48]),
        ],
        truncated: true,
    }
}

#[test]
fn results_file_round_trips() {
    let path = temp_path("roundtrip.cheatu-ptr");
    let r = sample();
    r.save(&path).unwrap();
    assert_eq!(PtrResults::load(&path).unwrap(), r);

    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..9], b"CHEATUPTR");

    let mut wrong = bytes.clone();
    wrong[0] = b'X';
    std::fs::write(&path, &wrong).unwrap();
    assert_eq!(
        PtrResults::load(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    std::fs::remove_file(&path).unwrap();
}

/// `CHEATUPTR`, version, header length, then the header bytes.
fn header_end(bytes: &[u8]) -> usize {
    17 + u32::from_le_bytes(bytes[13..17].try_into().unwrap()) as usize
}

#[test]
fn bad_results_files_rejected() {
    let path = temp_path("bad.cheatu-ptr");
    sample().save(&path).unwrap();
    let good = std::fs::read(&path).unwrap();
    let invalid = |bytes: &[u8]| {
        std::fs::write(&path, bytes).unwrap();
        PtrResults::load(&path).unwrap_err().kind()
    };

    // Truncated mid-chain and mid-header.
    assert_eq!(invalid(&good[..good.len() - 3]), io::ErrorKind::InvalidData);
    assert_eq!(invalid(&good[..20]), io::ErrorKind::InvalidData);
    assert_eq!(invalid(&good[..5]), io::ErrorKind::InvalidData);

    // Unknown version.
    let mut b = good.clone();
    b[9..13].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(invalid(&b), io::ErrorKind::InvalidData);

    // Header length over 1 MiB, with no allocation of that size.
    let mut b = good.clone();
    b[13..17].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(invalid(&b), io::ErrorKind::InvalidData);

    // Module index out of range (first chain's index).
    let start = header_end(&good);
    let mut b = good.clone();
    b[start..start + 4].copy_from_slice(&99u32.to_le_bytes());
    assert_eq!(invalid(&b), io::ErrorKind::InvalidData);

    // Depth over the header's max_depth (3), and depth 0.
    for depth in [200u8, 0] {
        let mut b = good.clone();
        b[start + 12] = depth;
        assert_eq!(invalid(&b), io::ErrorKind::InvalidData);
    }

    // Garbage header JSON.
    let mut b = good.clone();
    b[17] = b'!';
    assert_eq!(invalid(&b), io::ErrorKind::InvalidData);

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn save_rejects_unloadable_chains() {
    let path = temp_path("unloadable.cheatu-ptr");
    let mut r = sample();
    r.chains.push(chain(0x50, &[8; 4])); // max_depth is 3
    assert_eq!(
        r.save(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(!path.exists(), "nothing written");
}

/// A results file written byte by byte, so `load` can be fed what `save`
/// refuses to write.
fn raw_results(modules: &[&str], max_depth: usize, chains: &[(u32, Vec<u64>)]) -> Vec<u8> {
    let header = format!(
        "{{\"process\":\"g\",\"target\":1,\"settings\":{{\"max_depth\":{max_depth},\
         \"max_offset\":4096,\"alignment\":8,\"max_results\":10,\"chunk_entries\":10}},\
         \"modules\":{},\"truncated\":false}}",
        serde_json::to_string(modules).unwrap()
    );
    let mut b = b"CHEATUPTR".to_vec();
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&(header.len() as u32).to_le_bytes());
    b.extend_from_slice(header.as_bytes());
    for (module, offsets) in chains {
        b.extend_from_slice(&module.to_le_bytes());
        b.extend_from_slice(&0x10u64.to_le_bytes());
        b.push(offsets.len() as u8);
        for o in offsets {
            b.extend_from_slice(&o.to_le_bytes());
        }
    }
    b
}

const LONG_NAME: &str = "a123456789b123456789c123456789d123456789e123456789f123456789\
    g123456789h123456789i123456789j123456789k123456789l123456789m123456789n123456789\
    o123456789p123456789q123456789r123456789s123456789t123456789u123456789v123456789\
    w123456789x123456789y123456789z1234";

#[test]
fn results_with_unexpressible_chains_are_refused() {
    assert_eq!(LONG_NAME.len(), 255);
    let path = temp_path("names.cheatu-ptr");
    let load = |bytes: Vec<u8>| {
        std::fs::write(&path, bytes).unwrap();
        PtrResults::load(&path)
    };
    // The worst case that is accepted still makes a loadable expression.
    let ok = load(raw_results(&[LONG_NAME], 40, &[(0, vec![u64::MAX; 32])])).unwrap();
    assert!(ok.chains[0].to_expr().validate().is_ok());

    let too_long = format!("{LONG_NAME}x");
    for name in ["", "we\"ird", "tab\there", too_long.as_str()] {
        let e = load(raw_results(&[name], 5, &[(0, vec![8])])).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{name:?}");
        assert!(e.to_string().contains("module name"), "{name:?}: {e}");

        let mut r = sample();
        r.chains[0].module = name.to_string();
        let e = r.save(&path).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{name:?}");
    }
    let e = load(raw_results(&["m"], 40, &[(0, vec![8; 33])])).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);

    let mut r = sample();
    r.settings.max_depth = 40;
    r.chains.push(chain(0x50, &[8; 33]));
    assert_eq!(
        r.save(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn results_save_replaces_instead_of_rewriting_in_place() {
    let path = temp_path("replace.cheatu-ptr");
    let other = temp_path("replace-link.cheatu-ptr");
    let _ = std::fs::remove_file(&other);
    std::fs::write(&path, "old").unwrap();
    std::fs::hard_link(&path, &other).unwrap();
    sample().save(&path).unwrap();
    assert_eq!(std::fs::read(&other).unwrap(), b"old");
    assert_eq!(PtrResults::load(&path).unwrap(), sample());
    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&other).ok();
}

#[test]
fn results_fifo_is_refused_promptly() {
    let path = temp_path("fifo.cheatu-ptr");
    let _ = std::fs::remove_file(&path);
    let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let (tx, rx) = std::sync::mpsc::channel();
    let p = path.clone();
    std::thread::spawn(move || {
        let _ = tx.send(PtrResults::load(&p).map(|_| ()));
    });
    let e = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("load returned within 5 s")
        .unwrap_err();
    assert!(e.to_string().contains("not a regular file"), "{e}");
    std::fs::remove_file(&path).ok();
}

#[test]
fn chains_through_unnameable_modules_are_skipped() {
    let mut src = fake();
    for r in &mut src.regions {
        if r.path == "/usr/lib/libgame.dylib" {
            r.path = "/usr/lib/lib\"game.dylib".into();
        }
    }
    let found = search(&src, T, &PtrScanSettings::default());
    assert!(found.chains.is_empty(), "{:?}", found.chains);
}

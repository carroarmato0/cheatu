//! AoB signature helper, against an in-memory fake source. A byte that
//! "changes between the two reads" is simulated with a read counter.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

use cheatu_core::source::MemSource;
use cheatu_core::{
    make_signature, make_signature_with, AddrExpr, Expr, MemoryRegion, ScanProgress, SignatureError,
};

struct Fake {
    regions: Vec<MemoryRegion>,
    data: Vec<(u64, Vec<u8>)>,
    reads: AtomicUsize,
    /// Reads of 1 KiB or more: passes over memory, not context or checks.
    big_reads: AtomicUsize,
    /// Address whose byte differs on the second read only.
    flip: Option<u64>,
}

impl MemSource for Fake {
    fn regions(&self) -> &[MemoryRegion] {
        &self.regions
    }
    fn read(&self, addr: u64, buf: &mut [u8]) -> io::Result<usize> {
        let nth = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        if buf.len() >= 1024 {
            self.big_reads.fetch_add(1, Ordering::SeqCst);
        }
        for (base, bytes) in &self.data {
            let end = base + bytes.len() as u64;
            if addr >= *base && addr < end {
                let off = (addr - base) as usize;
                let n = buf.len().min(bytes.len() - off);
                buf[..n].copy_from_slice(&bytes[off..off + n]);
                if let (2, Some(f)) = (nth, self.flip) {
                    if f >= addr && f < addr + n as u64 {
                        buf[(f - addr) as usize] ^= 0xFF;
                    }
                }
                return Ok(n);
            }
        }
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
}

fn region(start: u64, len: u64, path: &str) -> MemoryRegion {
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

fn random_bytes(len: usize) -> Vec<u8> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 32) as u8
        })
        .collect()
}

fn fake(regions: Vec<MemoryRegion>, data: Vec<(u64, Vec<u8>)>, flip: Option<u64>) -> Fake {
    Fake {
        regions,
        data,
        reads: AtomicUsize::new(0),
        big_reads: AtomicUsize::new(0),
        flip,
    }
}

fn parts(e: &AddrExpr) -> (String, Option<String>, u64) {
    match &e.0 {
        Expr::Add(l, r) => match (&**l, &**r) {
            (Expr::Aob { pattern, module }, Expr::Num(n)) => (pattern.clone(), module.clone(), *n),
            other => panic!("unexpected shape {other:?}"),
        },
        Expr::Aob { pattern, module } => (pattern.clone(), module.clone(), 0),
        other => panic!("unexpected shape {other:?}"),
    }
}

#[test]
fn unique_window_found() {
    let base = 0x10000;
    let src = fake(
        vec![region(base, 4096, "[heap]")],
        vec![(base, random_bytes(4096))],
        None,
    );
    let addr = base + 2000;
    let sig = make_signature(&src, addr, 4, 64).unwrap();
    assert_eq!(sig.resolve(&src).unwrap(), addr);
    assert!(src.reads.load(Ordering::SeqCst) >= 2);
    // Text form is parseable and uses uppercase hex.
    let text = sig.to_string();
    assert_eq!(AddrExpr::parse(&text).unwrap(), sig);
    let (pattern, _, _) = parts(&sig);
    assert_eq!(pattern, pattern.to_uppercase());
}

#[test]
fn offset_zero_is_bare_aob() {
    // Target at the very start of the only region: the window is clamped to
    // start there, so the offset is 0.
    let base = 0x10000;
    let src = fake(
        vec![region(base, 4096, "[heap]")],
        vec![(base, random_bytes(4096))],
        None,
    );
    let sig = make_signature(&src, base, 4, 64).unwrap();
    assert!(matches!(sig.0, Expr::Aob { .. }), "{sig}");
    assert_eq!(sig.resolve(&src).unwrap(), base);
}

#[test]
fn changing_bytes_become_wildcards() {
    let base = 0x10000;
    let addr = base + 2000;
    let src = fake(
        vec![region(base, 4096, "[heap]")],
        vec![(base, random_bytes(4096))],
        Some(addr - 2),
    );
    let sig = make_signature(&src, addr, 4, 64).unwrap();
    let (pattern, module, offset) = parts(&sig);
    assert_eq!(module, None);
    assert_eq!(offset, 4);
    let toks: Vec<&str> = pattern.split(' ').collect();
    assert_eq!(toks.len(), 8);
    assert_eq!(toks[2], "??");
    // The changing byte and the value's own 4 bytes.
    assert_eq!(toks.iter().filter(|t| **t == "??").count(), 5);
    assert!(src.reads.load(Ordering::SeqCst) >= 2);
}

#[test]
fn repetitive_memory_not_unique() {
    let a = 0x10000;
    let b = 0x40000;
    let src = fake(
        vec![region(a, 4096, "[heap]"), region(b, 4096, "[heap]")],
        vec![(a, vec![0; 4096]), (b, vec![0; 4096])],
        None,
    );
    let err = make_signature(&src, a + 2000, 4, 64).unwrap_err();
    assert_eq!(err, SignatureError::NotUnique);
    assert_eq!(err.to_string(), "no unique pattern within 64 bytes");
}

#[test]
fn unreadable_address() {
    let base = 0x10000;
    let src = fake(
        vec![region(base, 4096, "[heap]")],
        vec![(base, random_bytes(4096))],
        None,
    );
    let err = make_signature(&src, 0x900000, 4, 64).unwrap_err();
    assert_eq!(err, SignatureError::Unreadable(0x900000));
    assert_eq!(err.to_string(), "address 0x900000 unreadable");
}

#[test]
fn module_scope_is_used() {
    let base = 0x10000;
    let src = fake(
        vec![region(base, 4096, "/usr/lib/libgame.so")],
        vec![(base, random_bytes(4096))],
        None,
    );
    let addr = base + 1000;
    let sig = make_signature(&src, addr, 4, 64).unwrap();
    let (_, module, _) = parts(&sig);
    assert_eq!(module.as_deref(), Some("libgame.so"));
    assert_eq!(sig.resolve(&src).unwrap(), addr);
}

#[test]
fn bss_tail_is_searched_with_its_module() {
    let base = 0x10000;
    let tail = base + 4096;
    let mut regions = vec![
        region(base, 4096, "/usr/lib/libgame.so"),
        // Anonymous .bss tail right after the image's last file-backed page.
        region(tail, 4096, ""),
    ];
    for r in &mut regions {
        r.write = true;
    }
    let bytes = random_bytes(8192);
    let src = fake(
        regions,
        vec![
            (base, bytes[..4096].to_vec()),
            (tail, bytes[4096..].to_vec()),
        ],
        None,
    );
    let addr = tail + 1000;
    let sig = make_signature(&src, addr, 4, 64).expect("unique within the module");
    let (_, module, _) = parts(&sig);
    assert_eq!(module.as_deref(), Some("libgame.so"));
    assert_eq!(sig.resolve(&src).unwrap(), addr);
}

#[test]
fn unnameable_module_is_left_out_of_the_pattern() {
    let base = 0x10000;
    let src = fake(
        vec![region(base, 4096, "/usr/lib/lib\"game.so")],
        vec![(base, random_bytes(4096))],
        None,
    );
    let addr = base + 1000;
    let sig = make_signature(&src, addr, 4, 64).unwrap();
    assert_eq!(parts(&sig).1, None);
    assert!(sig.validate().is_ok());
    assert_eq!(sig.resolve(&src).unwrap(), addr);
}

/// The 8-byte window around the value occurs many times, the 24-byte one
/// once: memory is searched in one pass, the longer windows are only checked
/// where the short one matched.
#[test]
fn longer_windows_are_checked_without_new_passes() {
    let base = 0x10000;
    let mut bytes = random_bytes(8192);
    // Ten copies of the 8 bytes around the target, elsewhere in memory.
    let addr_off = 5000usize;
    let core: Vec<u8> = bytes[addr_off - 4..addr_off + 4].to_vec();
    for k in 0..10 {
        let at = 300 + k * 200;
        bytes[at..at + 8].copy_from_slice(&core);
    }
    let src = fake(
        vec![region(base, 8192, "[heap]")],
        vec![(base, bytes)],
        None,
    );
    let addr = base + addr_off as u64;
    let sig = make_signature(&src, addr, 4, 64).unwrap();
    assert_eq!(sig.resolve(&src).unwrap(), addr);
    let (pattern, _, _) = parts(&sig);
    assert_eq!(
        pattern.split(' ').count(),
        16,
        "first unique window: 16 bytes"
    );
    // One pass to sign; resolving it above is another.
    assert_eq!(
        src.big_reads.load(Ordering::SeqCst),
        2,
        "one pass, then the resolve"
    );
}

#[test]
fn signature_reports_progress() {
    let base = 0x10000;
    let src = fake(
        vec![region(base, 4096, "[heap]")],
        vec![(base, random_bytes(4096))],
        None,
    );
    let progress = ScanProgress::default();
    make_signature_with(&src, base + 2000, 4, 64, &progress).unwrap();
    assert_eq!(progress.fraction(), Some(1.0));
}

#[test]
fn cancelled_signature_stops() {
    let base = 0x10000;
    let src = fake(
        vec![region(base, 4096, "[heap]")],
        vec![(base, random_bytes(4096))],
        None,
    );
    let progress = ScanProgress::default();
    progress.cancel();
    assert_eq!(
        make_signature_with(&src, base + 2000, 4, 64, &progress),
        Err(SignatureError::Cancelled)
    );
}

/// The value's own bytes change with play (a rupee count): never part of
/// the pattern, so the address is found again whatever the value is.
#[test]
fn value_bytes_are_wildcards() {
    let base = 0x10000;
    let addr = base + 2000;
    let mut data = random_bytes(4096);
    let src = fake(
        vec![region(base, 4096, "[heap]")],
        vec![(base, data.clone())],
        None,
    );
    let sig = make_signature(&src, addr, 4, 64).unwrap();
    let (pattern, _, offset) = parts(&sig);
    let toks: Vec<&str> = pattern.split(' ').collect();
    let at = offset as usize;
    assert_eq!(&toks[at..at + 4], ["??"; 4], "{sig}");

    // The value changed: still found.
    data[2000..2004].copy_from_slice(&123i32.to_le_bytes());
    let changed = fake(vec![region(base, 4096, "[heap]")], vec![(base, data)], None);
    assert_eq!(sig.resolve(&changed).unwrap(), addr);
}

/// An address stored next to the value differs on every run: never part of
/// the pattern.
#[test]
fn pointers_are_wildcards() {
    let base = 0x10000;
    let addr = base + 2000;
    let mut data = random_bytes(4096);
    // An aligned 8-byte word just before the value pointing into the region.
    let ptr_at = 1992;
    data[ptr_at..ptr_at + 8].copy_from_slice(&(base + 0x123u64).to_le_bytes());
    let src = fake(vec![region(base, 4096, "[heap]")], vec![(base, data)], None);
    let sig = make_signature(&src, addr, 4, 64).unwrap();
    let (pattern, _, offset) = parts(&sig);
    let toks: Vec<&str> = pattern.split(' ').collect();
    let ptr = (offset as usize)
        .checked_sub(8)
        .expect("the pointer is in the window");
    assert_eq!(&toks[ptr..ptr + 8], ["??"; 8], "{sig}");
}

/// A value 8 bytes wide fills the smallest window: it is skipped, not
/// searched as all wildcards.
#[test]
fn wide_value_still_gets_a_signature() {
    let base = 0x10000;
    let src = fake(
        vec![region(base, 4096, "[heap]")],
        vec![(base, random_bytes(4096))],
        None,
    );
    let sig = make_signature(&src, base + 2000, 8, 64).unwrap();
    assert_eq!(sig.resolve(&src).unwrap(), base + 2000);
    assert!(parts(&sig).0.split(' ').any(|t| t != "??"));
}

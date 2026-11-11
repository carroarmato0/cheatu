//! Resolving address expressions. Module/pointer tests target the test
//! process itself; AoB tests use an in-memory fake source so match counts are
//! deterministic.

use std::io;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use cheatu_core::source::{LiveSource, MemSource};
use cheatu_core::{module_containing, AddrExpr, MemoryRegion, ResolveError};

fn live() -> LiveSource {
    LiveSource::open(std::process::id() as i32).expect("open own process")
}

fn parse(s: &str) -> AddrExpr {
    AddrExpr::parse(s).unwrap()
}

static VALUE: AtomicU64 = AtomicU64::new(0x1122_3344);

#[test]
fn module_offset_reaches_a_static() {
    let src = live();
    let addr = &VALUE as *const AtomicU64 as u64;
    let (name, base) = module_containing(src.regions(), addr).expect("static is in a module");
    let expr = parse(&format!("\"{name}\"+{}", addr - base));
    assert_eq!(expr.resolve(&src).unwrap(), addr);
}

struct Node {
    next: Box<u64>,
    pad: u64,
}

static ROOT: AtomicPtr<Node> = AtomicPtr::new(std::ptr::null_mut());

#[test]
fn pointer_chain_resolves() {
    let node = Box::new(Node {
        next: Box::new(0xABCD),
        pad: 7,
    });
    let node_ptr = Box::into_raw(node);
    ROOT.store(node_ptr, Ordering::SeqCst);
    // `Node` is repr(Rust): derive the offsets from the real layout.
    let o1 = unsafe { std::ptr::addr_of!((*node_ptr).next) as u64 - node_ptr as u64 };
    let value_addr = unsafe { &*(*node_ptr).next as *const u64 as u64 };
    let _ = unsafe { (*node_ptr).pad };

    let src = live();
    let root_addr = &ROOT as *const AtomicPtr<Node> as u64;
    let (name, base) = module_containing(src.regions(), root_addr).unwrap();
    let r = root_addr - base;
    let expr = parse(&format!("[[\"{name}\"+{r}]+{o1}]+0"));
    assert_eq!(expr.resolve(&src).unwrap(), value_addr);

    // Null intermediate pointer: error, no panic.
    ROOT.store(std::ptr::null_mut(), Ordering::SeqCst);
    let expr = parse(&format!("[[\"{name}\"+{r}]+{o1}]+0"));
    assert_eq!(expr.resolve(&src), Err(ResolveError::Unreadable(0)));
    unsafe { drop(Box::from_raw(node_ptr)) };
}

#[test]
fn deref_of_null_is_unreadable() {
    let src = live();
    assert_eq!(
        parse("[0x0]+0x10").resolve(&src),
        Err(ResolveError::Unreadable(0))
    );
}

#[test]
fn arithmetic_wraps() {
    let src = live();
    assert_eq!(parse("0 - 1").resolve(&src).unwrap(), u64::MAX);
    assert_eq!(parse("0xffffffffffffffff + 2").resolve(&src).unwrap(), 1);
}

#[test]
fn unknown_module_is_an_error() {
    let src = live();
    let err = parse("\"no_such_module\"+1").resolve(&src).unwrap_err();
    assert_eq!(err.to_string(), "module \"no_such_module\" not found");
}

/// Synthetic address space: regions plus the bytes behind them.
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

fn region(start: u64, len: u64, path: &str) -> MemoryRegion {
    MemoryRegion {
        start,
        end: start + len,
        read: true,
        write: false,
        exec: false,
        shared: false,
        path: path.into(),
    }
}

const MARKER: [u8; 16] = [
    0x9e, 0x37, 0x79, 0xb1, 0x85, 0xeb, 0xca, 0x87, 0x27, 0xd4, 0xeb, 0x2f, 0x16, 0x5c, 0x1b, 0x42,
];
const MARKER_HEX: &str = "9e 37 79 b1 85 eb ca 87 27 d4 eb 2f 16 5c 1b 42";

fn buffer_with(markers_at: &[usize], len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    for &o in markers_at {
        v[o..o + 16].copy_from_slice(&MARKER);
    }
    v
}

#[test]
fn aob_unique_match() {
    // Marker straddles the 4 MiB chunk boundary: must be found once.
    let boundary = 4 * 1024 * 1024 - 5;
    let len = 6 * 1024 * 1024;
    let src = Fake {
        regions: vec![
            region(0x10_0000, len as u64, "/x/mod"),
            region(0x9000_0000, 0x1000, ""),
        ],
        data: vec![
            (0x10_0000, buffer_with(&[boundary], len)),
            (0x9000_0000, vec![0; 0x1000]),
        ],
    };
    let expr = parse(&format!("aob(\"{MARKER_HEX}\")+0x2"));
    assert_eq!(expr.resolve(&src).unwrap(), 0x10_0000 + boundary as u64 + 2);
}

#[test]
fn aob_two_matches_reports_count() {
    let src = Fake {
        regions: vec![region(0x1000, 0x1000, "/x/mod")],
        data: vec![(0x1000, buffer_with(&[0x10, 0x100], 0x1000))],
    };
    let err = parse(&format!("aob(\"{MARKER_HEX}\")"))
        .resolve(&src)
        .unwrap_err();
    assert_eq!(
        err,
        ResolveError::AobCount {
            pattern: MARKER_HEX.into(),
            module: None,
            count: 2
        }
    );
    assert_eq!(
        err.to_string(),
        format!("aob(\"{MARKER_HEX}\") matched 2 times, must be unique")
    );
}

#[test]
fn aob_limited_to_module_and_skips_unreadable() {
    let src = Fake {
        regions: vec![
            region(0x1000, 0x1000, "/x/a"),
            region(0x3000, 0x1000, "/x/b"),
            region(0x5000, 0x1000, "/x/b"), // unreadable (no data)
        ],
        data: vec![
            (0x1000, buffer_with(&[0x10], 0x1000)),
            (0x3000, buffer_with(&[0x20], 0x1000)),
        ],
    };
    let e = parse(&format!("aob(\"{MARKER_HEX}\", \"b\")"));
    assert_eq!(e.resolve(&src).unwrap(), 0x3020);
    let none = parse("aob(\"01 02 03 04\", \"b\")")
        .resolve(&src)
        .unwrap_err();
    assert_eq!(
        none.to_string(),
        "aob(\"01 02 03 04\", \"b\") matched 0 times, must be unique"
    );
}

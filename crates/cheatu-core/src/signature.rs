//! Build a position-independent `aob(...)` expression for an address.

use std::fmt;
use std::time::Duration;

use crate::addr_expr::{find_matches_with, valid_module_name, AddrExpr, Expr};
use crate::maps::{module_containing, region_for};
use crate::scan::ScanProgress;
use crate::source::MemSource;

/// Bytes read on each side of the address.
const CONTEXT: u64 = 64;
/// Most places the smallest window may match and still be narrowed down by
/// checking them (see [`make_signature_with`]).
const MAX_CANDIDATES: usize = 100_000;
/// Pause between the two context reads, so a live target can change bytes.
const RESAMPLE_DELAY: Duration = Duration::from_millis(50);

/// Why no signature could be made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignatureError {
    /// No window up to the size limit matched exactly once.
    NotUnique,
    /// The address itself could not be read.
    Unreadable(u64),
    /// Stopped through its [`ScanProgress`].
    Cancelled,
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignatureError::NotUnique => f.write_str("no unique pattern within 64 bytes"),
            SignatureError::Unreadable(a) => write!(f, "address {a:#x} unreadable"),
            SignatureError::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for SignatureError {}

/// Make an expression resolving to `addr`: `aob("<pattern>"[, "<module>"])`
/// plus `+0x<offset>` when `addr` is not the start of the pattern. The memory
/// around `addr` is read twice, 50 ms apart; bytes that differ become `??`.
/// The window grows from 8 bytes in steps of 8, up to `max_len`, until it
/// matches exactly once.
pub fn make_signature(
    src: &dyn MemSource,
    addr: u64,
    max_len: usize,
) -> Result<AddrExpr, SignatureError> {
    make_signature_with(src, addr, max_len, &ScanProgress::default())
}

/// [`make_signature`], reporting progress in chunks of memory searched and
/// stopping with [`SignatureError::Cancelled`] on a cancel.
///
/// Memory is searched once, for the smallest window. Every larger window
/// contains it, so it can only match where the smallest one did: those
/// places are checked directly. When the smallest window matches more than
/// [`MAX_CANDIDATES`] times, each larger window gets its own search.
pub fn make_signature_with(
    src: &dyn MemSource,
    addr: u64,
    max_len: usize,
    progress: &ScanProgress,
) -> Result<AddrExpr, SignatureError> {
    let unreadable = SignatureError::Unreadable(addr);
    let region = region_for(src.regions(), addr)
        .filter(|r| r.read)
        .ok_or_else(|| unreadable.clone())?;
    let ctx_start = addr.saturating_sub(CONTEXT).max(region.start);
    let ctx_end = addr.saturating_add(CONTEXT).min(region.end);
    let len = (ctx_end - ctx_start) as usize;

    let mut first = vec![0u8; len];
    let n1 = src
        .read(ctx_start, &mut first)
        .map_err(|_| unreadable.clone())?;
    std::thread::sleep(RESAMPLE_DELAY);
    let mut second = vec![0u8; len];
    let n2 = src
        .read(ctx_start, &mut second)
        .map_err(|_| unreadable.clone())?;
    // Only bytes read both times count; the window must contain `addr`.
    let n = n1.min(n2);
    let addr_idx = (addr - ctx_start) as usize;
    if addr_idx >= n {
        return Err(unreadable);
    }
    let bytes: Vec<Option<u8>> = (0..n)
        .map(|i| (first[i] == second[i]).then_some(first[i]))
        .collect();

    // A module whose file name cannot be written in an expression is left
    // out: the pattern then has to be unique in all of memory.
    let module = module_containing(src.regions(), addr)
        .map(|(name, _)| name)
        .filter(|name| valid_module_name(name));
    // The windows tried, smallest first; each contains the one before.
    let mut windows: Vec<(usize, usize)> = Vec::new();
    for size in (8..=max_len).step_by(8) {
        let lo = addr_idx.saturating_sub(size / 2);
        let hi = (lo + size).min(n);
        if windows.last() != Some(&(lo, hi)) {
            windows.push((lo, hi));
        }
    }
    let Some(&(lo0, hi0)) = windows.first() else {
        return Err(SignatureError::NotUnique);
    };
    let found = find_matches_with(
        src,
        &bytes[lo0..hi0],
        module.as_deref(),
        MAX_CANDIDATES + 1,
        Some(progress),
    );
    if progress.take_cancel() {
        return Err(SignatureError::Cancelled);
    }
    // A matching error (e.g. module vanished) means no usable pattern.
    let Ok(mut candidates) = found else {
        return Err(SignatureError::NotUnique);
    };
    let narrow = candidates.len() <= MAX_CANDIDATES;
    for (k, &(lo, hi)) in windows.iter().enumerate() {
        let window = &bytes[lo..hi];
        let unique = if k == 0 {
            candidates.len() == 1
        } else if narrow {
            // Where window 0 starts at `c`, this one starts `lo0 - lo` earlier.
            let back = (lo0 - lo) as u64;
            let mut buf = vec![0u8; window.len()];
            candidates.retain(|&c| {
                c >= back
                    && matches!(src.read(c - back, &mut buf), Ok(r) if r == buf.len())
                    && window
                        .iter()
                        .zip(&buf)
                        .all(|(p, b)| p.is_none_or(|p| p == *b))
            });
            candidates.iter_mut().for_each(|c| *c -= back);
            candidates.len() == 1
        } else {
            let found = find_matches_with(src, window, module.as_deref(), 2, Some(progress));
            if progress.take_cancel() {
                return Err(SignatureError::Cancelled);
            }
            matches!(found, Ok(v) if v.len() == 1)
        };
        if unique {
            let pattern = window
                .iter()
                .map(|b| b.map_or("??".to_string(), |b| format!("{b:02X}")))
                .collect::<Vec<_>>()
                .join(" ");
            let aob = Expr::Aob { pattern, module };
            let offset = (addr_idx - lo) as u64;
            return Ok(AddrExpr(if offset == 0 {
                aob
            } else {
                Expr::Add(Box::new(aob), Box::new(Expr::Num(offset)))
            }));
        }
        // After window 0 the candidates move to each window's start; for the
        // next window they must be relative to window 0 again.
        if k > 0 && narrow {
            let back = (lo0 - lo) as u64;
            candidates.iter_mut().for_each(|c| *c += back);
        }
    }
    Err(SignatureError::NotUnique)
}

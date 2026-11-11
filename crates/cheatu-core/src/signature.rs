//! Build a position-independent `aob(...)` expression for an address.

use std::fmt;
use std::time::Duration;

use crate::addr_expr::{count_matches, valid_module_name, AddrExpr, Expr};
use crate::maps::{module_containing, region_for};
use crate::source::MemSource;

/// Bytes read on each side of the address.
const CONTEXT: u64 = 64;
/// Pause between the two context reads, so a live target can change bytes.
const RESAMPLE_DELAY: Duration = Duration::from_millis(50);

/// Why no signature could be made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignatureError {
    /// No window up to the size limit matched exactly once.
    NotUnique,
    /// The address itself could not be read.
    Unreadable(u64),
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignatureError::NotUnique => f.write_str("no unique pattern within 64 bytes"),
            SignatureError::Unreadable(a) => write!(f, "address {a:#x} unreadable"),
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
    let mut prev = (usize::MAX, usize::MAX);
    for size in (8..=max_len).step_by(8) {
        let lo = addr_idx.saturating_sub(size / 2);
        let hi = (lo + size).min(n);
        if (lo, hi) == prev {
            continue;
        }
        prev = (lo, hi);
        let window = &bytes[lo..hi];
        // A matching error (e.g. module vanished) means no usable pattern.
        if count_matches(src, window, module.as_deref(), 2) == Ok(1) {
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
    }
    Err(SignatureError::NotUnique)
}

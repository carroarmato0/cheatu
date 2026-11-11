//! Breadth-first search for pointer chains from module statics to a target.

use std::collections::HashSet;
use std::io;

use super::{PointerMap, PtrScanSettings};
use crate::addr_expr::{valid_module_name, AddrExpr, Expr, ResolveError};
use crate::maps::{module_base, module_containing};
use crate::scan::{cancelled_err, ScanProgress};
use crate::source::MemSource;

/// Most targets kept per search level. Past it the level stops growing and the
/// search is marked truncated.
const MAX_FRONTIER: usize = 1_000_000;

/// A path from a module's static data to an address:
/// `[…[["module"+base_offset]+offsets[0]]…]+offsets[n-1]`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PointerChain {
    pub module: String,
    pub base_offset: u64,
    /// Offsets added after each dereference, from the static side outwards.
    pub offsets: Vec<u64>,
}

impl PointerChain {
    /// The chain as an address expression, e.g. `[["m"+0x10]+0x18]+0x40`.
    pub fn to_expr(&self) -> AddrExpr {
        let mut e = Expr::Add(
            Box::new(Expr::Module(self.module.clone())),
            Box::new(Expr::Num(self.base_offset)),
        );
        for &o in &self.offsets {
            e = Expr::Add(Box::new(Expr::Deref(Box::new(e))), Box::new(Expr::Num(o)));
        }
        AddrExpr(e)
    }

    /// Follow the chain in `src`. Same result as resolving
    /// [`to_expr`](Self::to_expr); arithmetic wraps at 64 bits.
    pub fn resolve(&self, src: &dyn MemSource) -> Result<u64, ResolveError> {
        let mut addr = module_base(src.regions(), &self.module)?.wrapping_add(self.base_offset);
        for &o in &self.offsets {
            let mut b = [0u8; 8];
            match src.read(addr, &mut b) {
                Ok(8) => addr = u64::from_le_bytes(b).wrapping_add(o),
                _ => return Err(ResolveError::Unreadable(addr)),
            }
        }
        Ok(addr)
    }
}

/// What [`find_chains`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChainSearch {
    /// Shortest first, then by module, base offset and offsets.
    pub chains: Vec<PointerChain>,
    /// The result cap or the per-level target cap was hit, so chains may be
    /// missing.
    pub truncated: bool,
}

/// A search node: a pointer location (or the target itself, at the root).
struct Node {
    addr: u64,
    /// Offset from the value stored at `addr` to the parent's address.
    offset: u64,
    parent: Option<usize>,
}

/// Search `map` backwards from `target` for chains that start at a static
/// location. `src` supplies the module layout for naming static locations.
///
/// Each location is expanded at most once, at the shallowest level it is
/// reached; static locations complete a chain and are not expanded further.
/// Progress counts the targets of the current level; a cancel returns
/// [`io::ErrorKind::Interrupted`].
pub fn find_chains(
    map: &PointerMap,
    src: &dyn MemSource,
    target: u64,
    settings: &PtrScanSettings,
    progress: &ScanProgress,
) -> io::Result<ChainSearch> {
    let regions = src.regions();
    let mut nodes = vec![Node {
        addr: target,
        offset: 0,
        parent: None,
    }];
    let mut visited: HashSet<u64> = HashSet::from([target]);
    let mut level: Vec<usize> = vec![0];
    let mut out = ChainSearch::default();

    'search: for depth in 1..=settings.max_depth {
        if level.is_empty() {
            break;
        }
        progress.begin(level.len());
        let mut next = Vec::new();
        for &idx in &level {
            if progress.cancelled() {
                progress.take_cancel();
                return Err(cancelled_err());
            }
            let t = nodes[idx].addr;
            let lo = t.saturating_sub(settings.max_offset);
            for (value, location) in map.pointers_into(lo, t.saturating_add(1)) {
                let offset = t - value;
                if map.is_static(location) {
                    // A module whose file name cannot be written in an
                    // expression cannot name a chain (see `valid_module_name`).
                    let Some((module, base)) = module_containing(regions, location)
                        .filter(|(name, _)| valid_module_name(name))
                    else {
                        continue;
                    };
                    if out.chains.len() >= settings.max_results {
                        out.truncated = true;
                        break 'search;
                    }
                    let mut offsets = vec![offset];
                    let mut cur = Some(idx);
                    while let Some(i) = cur {
                        let n = &nodes[i];
                        if n.parent.is_some() {
                            offsets.push(n.offset);
                        }
                        cur = n.parent;
                    }
                    out.chains.push(PointerChain {
                        module,
                        base_offset: location - base,
                        offsets,
                    });
                } else if depth < settings.max_depth && !visited.contains(&location) {
                    if next.len() >= MAX_FRONTIER {
                        out.truncated = true;
                        continue;
                    }
                    visited.insert(location);
                    nodes.push(Node {
                        addr: location,
                        offset,
                        parent: Some(idx),
                    });
                    next.push(nodes.len() - 1);
                }
            }
            progress.advance(1);
        }
        level = next;
    }

    out.chains.sort_by(|a, b| {
        (a.offsets.len(), &a.module, a.base_offset, &a.offsets).cmp(&(
            b.offsets.len(),
            &b.module,
            b.base_offset,
            &b.offsets,
        ))
    });
    Ok(out)
}

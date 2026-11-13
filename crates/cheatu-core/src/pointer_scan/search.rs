//! Breadth-first search for pointer chains from module statics to a target.

use std::collections::HashMap;
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
    /// The result cap, the per-level target cap or the link cap was hit, so
    /// chains may be missing.
    pub truncated: bool,
}

/// Most links between locations kept. A link records one more way a known
/// location leads towards the target; past this cap new ones are dropped and
/// the search is marked truncated.
const MAX_LINKS: usize = 8_000_000;

/// A static location that points near a search node: `[static]+offset` is
/// `nodes[node]`.
struct Root {
    module: String,
    base_offset: u64,
    node: usize,
    offset: u64,
}

/// Search `map` backwards from `target` for chains that start at a static
/// location. `src` supplies the module layout for naming static locations.
///
/// Each location is expanded at most once, at the shallowest level it is
/// reached, but every way it leads towards the target is kept: a location
/// that is one step from the target *and* two steps from it (two heap objects
/// within `max_offset` of each other) yields both chains, and only the longer
/// one may survive the objects moving apart. Chains are then assembled
/// shortest first, never passing a location twice. Static locations complete
/// a chain and are not expanded further. Progress counts the targets of the
/// current level; a cancel returns [`io::ErrorKind::Interrupted`].
pub fn find_chains(
    map: &PointerMap,
    src: &dyn MemSource,
    target: u64,
    settings: &PtrScanSettings,
    progress: &ScanProgress,
) -> io::Result<ChainSearch> {
    let regions = src.regions();
    // Node 0 is the target. `links[n]` holds `(child, offset)`: the value at
    // `addrs[n]` plus `offset` is `addrs[child]`.
    let mut addrs = vec![target];
    let mut links: Vec<Vec<(usize, u64)>> = vec![Vec::new()];
    let mut index: HashMap<u64, usize> = HashMap::from([(target, 0)]);
    let mut n_links = 0usize;
    let mut roots: Vec<Root> = Vec::new();
    let mut level: Vec<usize> = vec![0];
    let mut truncated = false;

    for depth in 1..=settings.max_depth {
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
            let t = addrs[idx];
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
                    if roots.len() + n_links >= MAX_LINKS {
                        truncated = true;
                        continue;
                    }
                    roots.push(Root {
                        module,
                        base_offset: location - base,
                        node: idx,
                        offset,
                    });
                } else if depth < settings.max_depth {
                    let n = match index.get(&location) {
                        Some(&0) => continue,
                        Some(&n) => n,
                        None => {
                            if next.len() >= MAX_FRONTIER {
                                truncated = true;
                                continue;
                            }
                            addrs.push(location);
                            links.push(Vec::new());
                            let n = addrs.len() - 1;
                            index.insert(location, n);
                            next.push(n);
                            n
                        }
                    };
                    if roots.len() + n_links >= MAX_LINKS {
                        truncated = true;
                        continue;
                    }
                    links[n].push((idx, offset));
                    n_links += 1;
                }
            }
            progress.advance(1);
        }
        level = next;
    }

    roots.sort_by(|a, b| (&a.module, a.base_offset).cmp(&(&b.module, b.base_offset)));
    let mut out = ChainSearch {
        chains: Vec::new(),
        truncated,
    };
    // One pass per chain length, so the result cap keeps the shortest.
    'len: for len in 1..=settings.max_depth {
        let start = out.chains.len();
        for root in &roots {
            let mut path = vec![root.node];
            let mut offsets = vec![root.offset];
            if !walk(&links, &mut path, &mut offsets, len, &mut |offsets| {
                if out.chains.len() >= settings.max_results {
                    return false;
                }
                out.chains.push(PointerChain {
                    module: root.module.clone(),
                    base_offset: root.base_offset,
                    offsets: offsets.to_vec(),
                });
                true
            }) {
                out.truncated = true;
                out.chains[start..].sort_by(chain_order);
                break 'len;
            }
        }
        out.chains[start..].sort_by(chain_order);
    }
    Ok(out)
}

fn chain_order(a: &PointerChain, b: &PointerChain) -> std::cmp::Ordering {
    (&a.module, a.base_offset, &a.offsets).cmp(&(&b.module, b.base_offset, &b.offsets))
}

/// Extend `path` (whose last node is the current one) along `links` to the
/// target, node 0, emitting the offsets of every chain with exactly `len`
/// offsets. A path never repeats a node. Returns `false` once `emit` does.
fn walk(
    links: &[Vec<(usize, u64)>],
    path: &mut Vec<usize>,
    offsets: &mut Vec<u64>,
    len: usize,
    emit: &mut dyn FnMut(&[u64]) -> bool,
) -> bool {
    let node = *path.last().expect("path starts at a root");
    if node == 0 {
        return offsets.len() != len || emit(offsets);
    }
    if offsets.len() >= len {
        return true;
    }
    for &(child, offset) in &links[node] {
        if path.contains(&child) {
            continue;
        }
        path.push(child);
        offsets.push(offset);
        let go_on = walk(links, path, offsets, len, emit);
        path.pop();
        offsets.pop();
        if !go_on {
            return false;
        }
    }
    true
}

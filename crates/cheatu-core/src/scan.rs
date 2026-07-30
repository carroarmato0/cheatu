//! The scan engine: first scan builds a candidate set, next scans refine it.
//!
//! Each surviving [`Candidate`] carries its own [`ScanValue`], so its numeric
//! type travels with it. That lets a single result set mix types — which is how
//! the "Any" scan works: you enter a value you saw on screen without knowing
//! whether the game stores it as an i32, an f32, an i64, and so on.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::maps::{read_maps, MemoryRegion, RegionKind};
use crate::mem::Mem;
use crate::value::{ScanType, ScanValue};

/// How large a chunk to read from the target per syscall during a first scan.
const CHUNK: usize = 1 << 20; // 1 MiB

/// Granularity of parallel work items during a first scan (a big region is
/// sliced into pieces this size so threads share the load evenly). A multiple
/// of 8 and of `CHUNK`, so every slice boundary is value-aligned.
const WORK_ITEM: u64 = 8 << 20; // 8 MiB

/// Number of worker threads to use for scanning.
fn worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16)
}

/// The set of types tried by an "Any" scan.
///
/// Deliberately excludes 8- and 16-bit widths: a value like `100` would match
/// almost every byte/short in memory and bury the real hit in noise. These six
/// cover the overwhelming majority of real game values (ints, longs, floats,
/// doubles), and a type that can't represent the entered value (e.g. `3.5` as
/// an integer) is skipped automatically.
pub const ANY_TYPES: [ScanType; 6] = [
    ScanType::I32,
    ScanType::U32,
    ScanType::I64,
    ScanType::U64,
    ScanType::F32,
    ScanType::F64,
];

/// A surviving candidate address and the value last observed there.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub addr: u64,
    /// Value observed at the most recent scan. Its variant records the type.
    pub prev: ScanValue,
}

impl Candidate {
    /// The numeric type this candidate is being tracked as.
    pub fn ty(&self) -> ScanType {
        self.prev.ty()
    }
}

/// How likely a candidate is to be the authoritative game value rather than a
/// transient or display copy. A heuristic nudge, not proof.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Confidence {
    Likely,
    Neutral,
    Unlikely,
}

/// A per-address hint: a confidence level plus a short reason label.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AddrHint {
    pub confidence: Confidence,
    pub label: &'static str,
}

/// Rank a candidate by its memory region and type.
///
/// Real game state usually lives on the heap or in a module's writable data;
/// stack addresses are transient locals, and a `Bytes` match is the on-screen
/// text of a value rather than the value itself. This ranks by region + type
/// only — it cannot tell two same-region, same-type candidates apart.
pub fn address_hint(kind: RegionKind, ty: ScanType) -> AddrHint {
    // A byte/string match is the display text, whatever region it sits in.
    if matches!(ty, ScanType::Bytes(_)) {
        return AddrHint {
            confidence: Confidence::Unlikely,
            label: "text",
        };
    }
    match kind {
        RegionKind::Stack => AddrHint {
            confidence: Confidence::Unlikely,
            label: "stack",
        },
        RegionKind::Heap => AddrHint {
            confidence: Confidence::Likely,
            label: "heap",
        },
        RegionKind::ModuleData => AddrHint {
            confidence: Confidence::Likely,
            label: "module data",
        },
        RegionKind::Anonymous => AddrHint {
            confidence: Confidence::Neutral,
            label: "anon",
        },
        RegionKind::Other => AddrHint {
            confidence: Confidence::Neutral,
            label: "other",
        },
    }
}

/// The comparison for an initial (first) scan.
#[derive(Clone, Debug)]
pub enum FirstScan {
    /// Match `value` (entered as text) decoded as each of `types`.
    ///
    /// With a single type this is an ordinary typed search; with [`ANY_TYPES`]
    /// it is an "unknown type" search. Types that can't parse `value` are
    /// skipped, so results only contain plausible interpretations.
    Value { value: String, types: Vec<ScanType> },
    /// Keep every scannable address, decoded as one type (unknown value).
    Unknown(ScanType),
    /// Match a byte pattern — `None` entries are wildcards. Used for both
    /// Array-of-Bytes (via [`parse_aob`]) and String searches (plain UTF-8
    /// bytes, no wildcards).
    Pattern(Vec<Option<u8>>),
}

/// Parse a space-separated hex byte pattern such as `"48 65 ?? 6C 6F"` into
/// match bytes, where `?`/`??` tokens are wildcards.
///
/// Returns `None` if any non-wildcard token isn't a valid hex byte.
pub fn parse_aob(s: &str) -> Option<Vec<Option<u8>>> {
    let pattern: Option<Vec<Option<u8>>> = s
        .split_whitespace()
        .map(|tok| {
            if tok.chars().all(|c| c == '?') {
                Some(None)
            } else {
                u8::from_str_radix(tok, 16).ok().map(Some)
            }
        })
        .collect();
    pattern.filter(|p| !p.is_empty())
}

/// The comparison for a subsequent (next) scan.
///
/// Value-based comparisons take an `operand` string in [`Scanner::next_scan`],
/// which is parsed per candidate against that candidate's own type.
#[derive(Copy, Clone, Debug)]
pub enum NextScan {
    /// Equal to the operand.
    Eq,
    /// Not equal to the operand.
    Ne,
    /// Greater than the operand.
    Gt,
    /// Less than the operand.
    Lt,
    /// Larger than at the previous scan.
    Increased,
    /// Smaller than at the previous scan.
    Decreased,
    /// Different from the previous scan.
    Changed,
    /// Same as the previous scan.
    Unchanged,
}

impl NextScan {
    /// Whether this comparison reads the operand value.
    pub fn needs_operand(self) -> bool {
        matches!(
            self,
            NextScan::Eq | NextScan::Ne | NextScan::Gt | NextScan::Lt
        )
    }
}

/// How many prior candidate sets to keep for [`Scanner::undo`]. Bounded so a
/// long narrowing session on a huge candidate set can't grow memory without
/// limit. ponytail: bump if users want deeper undo and memory allows.
const UNDO_DEPTH: usize = 16;

/// Holds an open memory handle plus the current candidate set for one process.
pub struct Scanner {
    mem: Mem,
    results: Vec<Candidate>,
    scanned: bool,
    /// Snapshots of `results` taken before each narrowing/clear, newest last,
    /// so the most recent scans can be undone.
    history: Vec<Vec<Candidate>>,
}

impl Scanner {
    /// Attach to `pid`.
    pub fn new(pid: i32) -> io::Result<Scanner> {
        Ok(Scanner {
            mem: Mem::open(pid)?,
            results: Vec::new(),
            scanned: false,
            history: Vec::new(),
        })
    }

    /// Re-attach to `pid` with a previously saved candidate set.
    ///
    /// This is what makes a scan **session** possible across separate runs of
    /// the tool (or across an interactive pause while the target changes): save
    /// [`Scanner::results`], then reload them here and continue with
    /// [`Scanner::next_scan`].
    pub fn from_candidates(pid: i32, candidates: Vec<Candidate>) -> io::Result<Scanner> {
        Ok(Scanner {
            mem: Mem::open(pid)?,
            results: candidates,
            scanned: true,
            history: Vec::new(),
        })
    }

    pub fn pid(&self) -> i32 {
        self.mem.pid()
    }

    /// Whether a first scan has been performed yet.
    pub fn has_scanned(&self) -> bool {
        self.scanned
    }

    /// Number of surviving candidates.
    pub fn count(&self) -> usize {
        self.results.len()
    }

    pub fn results(&self) -> &[Candidate] {
        &self.results
    }

    /// Clear the candidate set and the "has scanned" flag. Undoable if there
    /// was anything to clear.
    pub fn reset(&mut self) {
        if !self.results.is_empty() {
            let prev = std::mem::take(&mut self.results);
            self.push_history(prev);
        }
        self.scanned = false;
    }

    /// Whether [`Scanner::undo`] has a snapshot to restore.
    pub fn can_undo(&self) -> bool {
        !self.history.is_empty()
    }

    /// Restore the candidate set as it was before the most recent narrowing or
    /// clear. Returns `false` if there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        match self.history.pop() {
            // History is only populated once a first scan has happened, so the
            // restored set always belongs to a scanned session.
            Some(prev) => {
                self.results = prev;
                self.scanned = true;
                true
            }
            None => false,
        }
    }

    /// Push a pre-change snapshot, dropping the oldest past [`UNDO_DEPTH`].
    fn push_history(&mut self, prev: Vec<Candidate>) {
        if self.history.len() == UNDO_DEPTH {
            self.history.remove(0);
        }
        self.history.push(prev);
    }

    /// Run a first scan across all scannable regions.
    ///
    /// The work is split into fixed-size slices and processed by a pool of
    /// threads sharing one memory handle (`pread` is thread-safe), which makes
    /// scanning a multi-gigabyte process markedly faster on multi-core CPUs.
    pub fn first_scan(&mut self, scan: FirstScan) -> io::Result<()> {
        let regions = read_maps(self.pid())?;
        let scannable: Vec<MemoryRegion> =
            regions.into_iter().filter(|r| r.is_scannable()).collect();
        let items = work_items(&scannable);

        let mut results = match scan {
            FirstScan::Unknown(ty) => self
                .parallel_scan(&items, move |mem, start, end, buf, out| {
                    scan_unknown_range(mem, start, end, ty, buf, out)
                }),
            FirstScan::Value { value, types } => {
                // Precompute (type, target) pairs for every type that can
                // represent the entered value.
                let targets: Vec<(ScanType, ScanValue)> = types
                    .iter()
                    .filter_map(|&t| t.parse(&value).map(|v| (t, v)))
                    .collect();
                if targets.is_empty() {
                    Vec::new()
                } else {
                    self.parallel_scan(&items, |mem, start, end, buf, out| {
                        scan_value_range(mem, start, end, &targets, buf, out)
                    })
                }
            }
            FirstScan::Pattern(pattern) => {
                if pattern.is_empty() {
                    Vec::new()
                } else {
                    self.parallel_scan(&items, |mem, start, end, buf, out| {
                        scan_pattern_range(mem, start, end, &pattern, buf, out)
                    })
                }
            }
        };

        // Threads finish out of address order; sort so display/list is stable.
        results.sort_unstable_by_key(|c| c.addr);
        self.results = results;
        self.scanned = true;
        // A first scan starts a fresh session; earlier snapshots no longer apply.
        self.history.clear();
        Ok(())
    }

    /// Distribute `items` across worker threads, each running `scan_one` on a
    /// slice into its own buffer and local result vector, then concatenate.
    fn parallel_scan<F>(&self, items: &[(u64, u64)], scan_one: F) -> Vec<Candidate>
    where
        F: Fn(&Mem, u64, u64, &mut [u8], &mut Vec<Candidate>) + Sync,
    {
        let next = AtomicUsize::new(0);
        let mem = &self.mem;
        let scan_one = &scan_one;
        let threads = worker_count().min(items.len().max(1));

        std::thread::scope(|s| {
            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    s.spawn(|| {
                        let mut buf = vec![0u8; CHUNK];
                        let mut local = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            if i >= items.len() {
                                break;
                            }
                            let (start, end) = items[i];
                            scan_one(mem, start, end, &mut buf, &mut local);
                        }
                        local
                    })
                })
                .collect();

            let mut all = Vec::new();
            for h in handles {
                all.extend(h.join().expect("scan worker panicked"));
            }
            all
        })
    }

    /// Refine the candidate set with a next scan, in parallel.
    ///
    /// Each candidate is re-read and compared using **its own type**. For
    /// value-based comparisons, `operand` is parsed per candidate type
    /// (`Gt`/`Lt` compare numerically as f64). Candidates whose memory can no
    /// longer be read are dropped. Candidate order is preserved.
    pub fn next_scan(&mut self, cmp: NextScan, operand: Option<&str>) -> io::Result<()> {
        if self.results.is_empty() {
            return Ok(());
        }
        let operand_f = operand.and_then(|s| s.trim().parse::<f64>().ok());
        let mem = &self.mem;
        let cands = &self.results;

        let threads = worker_count().min(cands.len());
        let chunk = cands.len().div_ceil(threads);

        let kept: Vec<Candidate> = std::thread::scope(|s| {
            let handles: Vec<_> = cands
                .chunks(chunk)
                .map(|group| {
                    s.spawn(move || {
                        let mut buf = [0u8; 8];
                        let mut local = Vec::new();
                        for cand in group {
                            let ty = cand.ty();
                            let size = ty.size();
                            let cur = match mem.read_at(cand.addr, &mut buf[..size]) {
                                Ok(n) if n == size => ScanValue::from_ne_bytes(ty, &buf[..size])
                                    .expect("slice is exactly one value wide"),
                                _ => continue, // unreadable now -> drop
                            };

                            let keep = match cmp {
                                NextScan::Eq => operand.is_some_and(|s| eq_operand(&cur, ty, s)),
                                NextScan::Ne => !operand.is_some_and(|s| eq_operand(&cur, ty, s)),
                                NextScan::Gt => operand_f.is_some_and(|t| cur.as_f64() > t),
                                NextScan::Lt => operand_f.is_some_and(|t| cur.as_f64() < t),
                                NextScan::Increased => cur.as_f64() > cand.prev.as_f64(),
                                NextScan::Decreased => cur.as_f64() < cand.prev.as_f64(),
                                NextScan::Changed => !cur.approx_eq(&cand.prev),
                                NextScan::Unchanged => cur.approx_eq(&cand.prev),
                            };

                            if keep {
                                local.push(Candidate {
                                    addr: cand.addr,
                                    prev: cur,
                                });
                            }
                        }
                        local
                    })
                })
                .collect();

            // Chunks are contiguous and joined in order, so global order holds.
            let mut all = Vec::new();
            for h in handles {
                all.extend(h.join().expect("scan worker panicked"));
            }
            all
        });

        // Move the pre-scan set into history (no clone) so this narrowing can
        // be undone, e.g. after an accidental "Decreased" that matched nothing.
        let prev = std::mem::replace(&mut self.results, kept);
        self.push_history(prev);
        Ok(())
    }

    /// Read the value at `addr` decoded as `ty` (for live display and editing).
    pub fn read_typed(&self, addr: u64, ty: ScanType) -> io::Result<ScanValue> {
        let size = ty.size();
        let mut buf = [0u8; 8];
        let n = self.mem.read_at(addr, &mut buf[..size])?;
        if n != size {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read"));
        }
        Ok(ScanValue::from_ne_bytes(ty, &buf[..size]).expect("slice is exactly one value wide"))
    }

    /// Write `value` at `addr`.
    pub fn write_value(&self, addr: u64, value: &ScanValue) -> io::Result<()> {
        self.mem.write_at(addr, &value.to_ne_bytes())
    }
}

/// Split scannable regions into value-aligned work items of at most
/// [`WORK_ITEM`] bytes, so worker threads share the load evenly even when one
/// region (e.g. a game's main heap) dwarfs the rest.
fn work_items(regions: &[MemoryRegion]) -> Vec<(u64, u64)> {
    let mut items = Vec::new();
    for region in regions {
        let mut start = region.start;
        while start < region.end {
            let end = (start + WORK_ITEM).min(region.end);
            items.push((start, end));
            start = end;
        }
    }
    items
}

/// Equality of a freshly-read value against a user-typed operand during a
/// next scan.
///
/// Normally exact-per-type via [`ScanValue::approx_eq`]. The exception: a
/// **float** candidate compared against a **whole number** is treated as the
/// value the game *displays* — HP shown as `199` is really a float somewhere in
/// `[198, 200)` once regen/buffs leave a fraction. Without this, narrowing an
/// "Any" scan by the on-screen integer culls the real fractional float while
/// its whole-number display-mirrors survive (which then freeze to no effect).
/// Type a decimal (`199.0`) to force an exact float match instead.
fn eq_operand(cur: &ScanValue, ty: ScanType, operand: &str) -> bool {
    let is_float = matches!(ty, ScanType::F32 | ScanType::F64);
    if is_float && !operand.contains('.') {
        if let Ok(n) = operand.trim().parse::<f64>() {
            // ponytail: ±1.0 window survives floor/round/ceil display; the
            // intersection across successive narrowing rounds tightens it.
            return (cur.as_f64() - n).abs() < 1.0;
        }
    }
    ty.parse(operand).is_some_and(|t| cur.approx_eq(&t))
}

/// Scan `[start, end)` for concrete `targets`, appending hits to `out`.
///
/// Reads in [`CHUNK`]-sized pieces and advances by whole 8-byte units so that
/// no value straddles a read boundary; on an unreadable piece it skips ahead.
fn scan_value_range(
    mem: &Mem,
    start: u64,
    end: u64,
    targets: &[(ScanType, ScanValue)],
    buf: &mut [u8],
    out: &mut Vec<Candidate>,
) {
    let mut addr = start;
    while addr < end {
        let want = ((end - addr) as usize).min(buf.len());
        let n = match mem.read_at(addr, &mut buf[..want]) {
            Ok(n) if n > 0 => n,
            _ => {
                addr += want as u64;
                continue;
            }
        };
        let usable = (n / 8) * 8;
        for (ty, target) in targets {
            let size = ty.size();
            let mut off = 0;
            while off + size <= usable {
                let v = ScanValue::from_ne_bytes(*ty, &buf[off..off + size])
                    .expect("slice is exactly one value wide");
                if v.approx_eq(target) {
                    out.push(Candidate {
                        addr: addr + off as u64,
                        prev: v,
                    });
                }
                off += size;
            }
        }
        addr += if usable > 0 { usable as u64 } else { n as u64 };
    }
}

/// Scan `[start, end)` for `pattern`, where `None` entries are wildcard bytes.
///
/// Consecutive chunk reads overlap by `pattern.len() - 1` bytes so a match
/// straddling a chunk boundary is never missed or double-counted.
///
/// Naive byte-by-byte comparison — fine for typical AoB/string pattern
/// lengths; swap in a Boyer-Moore-style skip if long patterns over huge
/// regions turn out to be a bottleneck.
fn scan_pattern_range(
    mem: &Mem,
    start: u64,
    end: u64,
    pattern: &[Option<u8>],
    buf: &mut [u8],
    out: &mut Vec<Candidate>,
) {
    let plen = pattern.len();
    if plen == 0 || (end - start) < plen as u64 {
        return;
    }
    let mut addr = start;
    while addr < end {
        let want = ((end - addr) as usize).min(buf.len());
        let n = match mem.read_at(addr, &mut buf[..want]) {
            Ok(n) if n > 0 => n,
            _ => {
                addr += want as u64;
                continue;
            }
        };
        if n >= plen {
            for (off, window) in buf[..n].windows(plen).enumerate() {
                if pattern_matches(pattern, window) {
                    out.push(Candidate {
                        addr: addr + off as u64,
                        prev: ScanValue::Bytes(window.to_vec()),
                    });
                }
            }
            addr += (n - (plen - 1)) as u64;
        } else {
            addr += n as u64;
        }
    }
}

fn pattern_matches(pattern: &[Option<u8>], data: &[u8]) -> bool {
    pattern
        .iter()
        .zip(data)
        .all(|(p, b)| p.is_none_or(|want| want == *b))
}

/// Store every aligned slot of one type in `[start, end)` (unknown value scan).
fn scan_unknown_range(
    mem: &Mem,
    start: u64,
    end: u64,
    ty: ScanType,
    buf: &mut [u8],
    out: &mut Vec<Candidate>,
) {
    let size = ty.size();
    let mut addr = start;
    while addr < end {
        let want = ((end - addr) as usize).min(buf.len());
        let n = match mem.read_at(addr, &mut buf[..want]) {
            Ok(n) if n > 0 => n,
            _ => {
                addr += want as u64;
                continue;
            }
        };
        let usable = (n / size) * size;
        let mut off = 0;
        while off + size <= usable {
            let v = ScanValue::from_ne_bytes(ty, &buf[off..off + size])
                .expect("slice is exactly one value wide");
            out.push(Candidate {
                addr: addr + off as u64,
                prev: v,
            });
            off += size;
        }
        addr += if usable > 0 { usable as u64 } else { n as u64 };
    }
}

//! The pointer map: every aligned value in writable memory that points into
//! readable memory, as `(value, location)` records sorted by value.
//!
//! A large target holds tens of millions of candidates, so the map lives on
//! disk: records are sorted in memory in runs of `chunk_entries`, each run is
//! written to a file, the runs are k-way merged into `map.bin`, and that file
//! is memory-mapped for binary search. Everything lives in a private (mode
//! 0700) temp directory under [`PointerMap::temp_root`] that is removed on drop
//! and on every error path; directories left by processes that died are swept
//! when the next map is built.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::Mmap;

use super::PtrScanSettings;
use crate::maps::{image_path, MemoryRegion};
use crate::scan::{cancelled_err, ScanProgress};
use crate::source::MemSource;

/// Bytes read from the target per call.
const READ_CHUNK: u64 = 4 * 1024 * 1024;
/// Bytes per record: `value` then `location`, both little-endian `u64`.
const RECORD: usize = 16;
/// How many records the merge writes between cancel checks.
const MERGE_CANCEL_EVERY: u64 = 1 << 20;
/// Most runs merged at once: bounds open files and read buffers. More runs
/// are merged in several passes.
const MAX_FAN_IN: usize = 64;
/// Read/write buffer per open run file.
const IO_BUF: usize = 64 * 1024;

/// A disk-backed, value-sorted map of the pointers in a target's writable
/// memory. See the [module docs](self).
pub struct PointerMap {
    // Declared before `_dir` so the mapping is gone before the directory is
    // removed.
    mmap: Option<Mmap>,
    len: u64,
    /// Sorted, merged address ranges of module images.
    modules: Vec<(u64, u64)>,
    failed_chunks: u64,
    _dir: TempDir,
}

impl PointerMap {
    /// Snapshot `src` into a pointer map.
    ///
    /// Progress is reported in bytes of read+write memory processed. A chunk
    /// that cannot be read is skipped and counted (see
    /// [`failed_chunks`](Self::failed_chunks)); the build fails only when at
    /// least half of the chunks fail, which means the target exited or access
    /// was lost. A cancel returns [`io::ErrorKind::Interrupted`].
    pub fn build(
        src: &dyn MemSource,
        settings: &PtrScanSettings,
        progress: &ScanProgress,
    ) -> io::Result<PointerMap> {
        if settings.alignment == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pointer alignment must be at least 1",
            ));
        }
        let regions = src.regions();
        let readable = merged_ranges(regions.iter().filter(|r| r.read));
        // Module images, including an anonymous `.bss` tail (see `image_path`).
        let modules = merged_ranges(
            (0..regions.len())
                .filter(|&i| image_path(regions, i).is_some())
                .map(|i| &regions[i]),
        );
        let scannable: Vec<&MemoryRegion> = regions.iter().filter(|r| r.is_scannable()).collect();
        progress.begin(scannable.iter().map(|r| r.size() as usize).sum());

        let dir = TempDir::create()?;
        let mut runs = RunWriter {
            dir: &dir,
            runs: Vec::new(),
            buf: Vec::new(),
            cap: settings.chunk_entries.max(1),
        };
        let mut attempted = 0u64;
        let mut failed = 0u64;
        let mut last_err: Option<io::Error> = None;
        // Room for a value starting just before the chunk end.
        let mut buf = vec![0u8; READ_CHUNK as usize + 7];

        for region in scannable {
            let mut start = region.start;
            while start < region.end {
                if progress.cancelled() {
                    progress.take_cancel();
                    return Err(cancelled_err());
                }
                let end = start.saturating_add(READ_CHUNK).min(region.end);
                let read_end = end.saturating_add(7).min(region.end);
                attempted += 1;
                match src.read(start, &mut buf[..(read_end - start) as usize]) {
                    Ok(n) if n > 0 => {
                        let got = &buf[..n];
                        let mut loc = align_up(start, settings.alignment);
                        while loc < end {
                            let off = (loc - start) as usize;
                            let Some(bytes) = got.get(off..off + 8) else {
                                break;
                            };
                            let value = u64::from_le_bytes(bytes.try_into().unwrap());
                            if in_ranges(&readable, value) {
                                runs.push(value, loc)?;
                            }
                            loc = match loc.checked_add(settings.alignment) {
                                Some(l) => l,
                                None => break,
                            };
                        }
                    }
                    Ok(_) => failed += 1,
                    Err(e) => {
                        failed += 1;
                        last_err = Some(e);
                    }
                }
                progress.advance((end - start) as usize);
                start = end;
            }
        }

        if attempted > 0 && failed * 2 >= attempted {
            let kind = match last_err.as_ref().map(io::Error::kind) {
                None => io::ErrorKind::UnexpectedEof,
                // Interrupted is reserved for a user cancel.
                Some(io::ErrorKind::Interrupted) => io::ErrorKind::Other,
                Some(k) => k,
            };
            let cause = last_err.map_or_else(|| "empty read".to_string(), |e| e.to_string());
            return Err(io::Error::new(
                kind,
                format!(
                    "{failed} of {attempted} memory chunks could not be read \
                     (target exited or access lost?): {cause}"
                ),
            ));
        }

        let run_paths = runs.finish()?;
        let map_path = dir.path().join("map.bin");
        let len = merge_runs(run_paths, &map_path, progress)?;
        let mmap = if len == 0 {
            // Mapping an empty file fails on some platforms.
            None
        } else {
            let file = File::open(&map_path)?;
            // SAFETY: the file lives in our private temp directory and is
            // never written again while mapped.
            Some(unsafe { Mmap::map(&file)? })
        };

        Ok(PointerMap {
            mmap,
            len,
            modules,
            failed_chunks: failed,
            _dir: dir,
        })
    }

    /// Number of `(value, location)` records.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the map holds no records.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Read chunks that failed and were skipped while building.
    pub fn failed_chunks(&self) -> u64 {
        self.failed_chunks
    }

    /// Records whose value lies in `[lo, hi)`, in (value, location) order.
    pub fn pointers_into(&self, lo: u64, hi: u64) -> impl Iterator<Item = (u64, u64)> + '_ {
        let bytes: &[u8] = self.mmap.as_deref().unwrap_or(&[]);
        let n = (bytes.len() / RECORD) as u64;
        let record = move |i: u64| {
            let r = &bytes[i as usize * RECORD..(i as usize + 1) * RECORD];
            (
                u64::from_le_bytes(r[..8].try_into().unwrap()),
                u64::from_le_bytes(r[8..].try_into().unwrap()),
            )
        };
        // Lower bound: first record with value >= lo.
        let (mut a, mut b) = (0u64, n);
        while a < b {
            let mid = a + (b - a) / 2;
            if record(mid).0 < lo {
                a = mid + 1;
            } else {
                b = mid;
            }
        }
        (a..n).map(record).take_while(move |&(v, _)| v < hi)
    }

    /// Directory the pointer map's temp files go in: `/var/tmp` on Linux when
    /// it is writable (it is disk-backed, while `/tmp` is often a RAM-backed
    /// tmpfs and the map can take gigabytes), otherwise the system temp dir.
    pub fn temp_root() -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            let writable = unsafe { libc::access(c"/var/tmp".as_ptr(), libc::W_OK | libc::X_OK) };
            if writable == 0 {
                return PathBuf::from("/var/tmp");
            }
        }
        std::env::temp_dir()
    }

    /// Whether `location` lies inside a module's image, so it can be named as
    /// `"module"+offset` across restarts.
    pub fn is_static(&self, location: u64) -> bool {
        in_ranges(&self.modules, location)
    }
}

fn align_up(addr: u64, align: u64) -> u64 {
    match addr % align {
        0 => addr,
        r => addr.saturating_add(align - r),
    }
}

/// Sorted, non-overlapping `[start, end)` ranges covering `regions`.
fn merged_ranges<'a>(regions: impl Iterator<Item = &'a MemoryRegion>) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = regions
        .filter(|r| r.end > r.start)
        .map(|r| (r.start, r.end))
        .collect();
    v.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(v.len());
    for (s, e) in v {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

fn in_ranges(ranges: &[(u64, u64)], addr: u64) -> bool {
    let i = ranges.partition_point(|&(s, _)| s <= addr);
    i > 0 && addr < ranges[i - 1].1
}

/// Buffers records and writes them as sorted run files.
struct RunWriter<'a> {
    dir: &'a TempDir,
    runs: Vec<PathBuf>,
    buf: Vec<(u64, u64)>,
    cap: usize,
}

impl RunWriter<'_> {
    fn push(&mut self, value: u64, location: u64) -> io::Result<()> {
        self.buf.push((value, location));
        if self.buf.len() >= self.cap {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        self.buf.sort_unstable();
        let path = self.dir.path().join(format!("run-{}.bin", self.runs.len()));
        let mut w = BufWriter::new(File::create(&path)?);
        for &(v, l) in &self.buf {
            w.write_all(&v.to_le_bytes())?;
            w.write_all(&l.to_le_bytes())?;
        }
        w.flush()?;
        self.runs.push(path);
        self.buf.clear();
        Ok(())
    }

    fn finish(mut self) -> io::Result<Vec<PathBuf>> {
        self.flush()?;
        Ok(self.runs)
    }
}

fn read_record(r: &mut impl Read) -> io::Result<Option<(u64, u64)>> {
    let mut rec = [0u8; RECORD];
    match r.read_exact(&mut rec) {
        Ok(()) => Ok(Some((
            u64::from_le_bytes(rec[..8].try_into().unwrap()),
            u64::from_le_bytes(rec[8..].try_into().unwrap()),
        ))),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

/// Merge the sorted `runs` into `out`, at most [`MAX_FAN_IN`] at a time,
/// deleting each run once merged. Returns the number of records written.
fn merge_runs(runs: Vec<PathBuf>, out: &Path, progress: &ScanProgress) -> io::Result<u64> {
    let dir = out.parent().unwrap_or(Path::new("."));
    let mut runs = runs;
    let mut pass = 0;
    while runs.len() > MAX_FAN_IN {
        let mut next = Vec::with_capacity(runs.len().div_ceil(MAX_FAN_IN));
        for (j, group) in runs.chunks(MAX_FAN_IN).enumerate() {
            let path = dir.join(format!("merge-{pass}-{j}.bin"));
            merge_group(group, &path, progress)?;
            next.push(path);
        }
        runs = next;
        pass += 1;
    }
    merge_group(&runs, out, progress)
}

/// K-way merge `runs` into `out` (an empty file when there are none),
/// deleting each run afterwards. Returns the number of records written.
fn merge_group(runs: &[PathBuf], out: &Path, progress: &ScanProgress) -> io::Result<u64> {
    if let [only] = runs {
        fs::rename(only, out)?;
        return Ok(fs::metadata(out)?.len() / RECORD as u64);
    }
    let mut readers = runs
        .iter()
        .map(|p| File::open(p).map(|f| BufReader::with_capacity(IO_BUF, f)))
        .collect::<io::Result<Vec<_>>>()?;
    let mut heap = BinaryHeap::with_capacity(readers.len());
    for (i, r) in readers.iter_mut().enumerate() {
        if let Some((v, l)) = read_record(r)? {
            heap.push(Reverse((v, l, i)));
        }
    }
    let mut w = BufWriter::with_capacity(IO_BUF, File::create(out)?);
    let mut written = 0u64;
    while let Some(Reverse((v, l, i))) = heap.pop() {
        w.write_all(&v.to_le_bytes())?;
        w.write_all(&l.to_le_bytes())?;
        written += 1;
        if written.is_multiple_of(MERGE_CANCEL_EVERY) && progress.cancelled() {
            progress.take_cancel();
            return Err(cancelled_err());
        }
        if let Some((v, l)) = read_record(&mut readers[i])? {
            heap.push(Reverse((v, l, i)));
        }
    }
    w.flush()?;
    drop(readers);
    for p in runs {
        fs::remove_file(p)?;
    }
    Ok(written)
}

/// `<temp root>/cheatu-ptrmap-<pid>-<n>/` (mode 0700), removed with everything
/// in it on drop — including on early `?` returns from [`PointerMap::build`].
struct TempDir(PathBuf);

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

const DIR_PREFIX: &str = "cheatu-ptrmap-";

impl TempDir {
    fn create() -> io::Result<TempDir> {
        TempDir::create_in(&PointerMap::temp_root())
    }

    fn create_in(root: &Path) -> io::Result<TempDir> {
        sweep_stale(root);
        let pid = std::process::id();
        loop {
            let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!("{DIR_PREFIX}{pid}-{n}"));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(TempDir(path)),
                // Left over by an earlier process with the same pid, or
                // planted: never reused.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The pid in a `cheatu-ptrmap-<pid>-<n>` directory name.
fn dir_pid(name: &str) -> Option<i32> {
    let (pid, n) = name.strip_prefix(DIR_PREFIX)?.split_once('-')?;
    n.parse::<u64>().ok()?;
    pid.parse().ok().filter(|&p| p > 0)
}

/// Whether no process with this pid exists.
fn pid_gone(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// Remove pointer map directories under `root` left by processes that no
/// longer exist (a crash or kill skips `Drop`). Only real directories owned by
/// this user are touched, never links. Best effort: errors are ignored.
fn sweep_stale(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let uid = unsafe { libc::geteuid() };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(dir_pid) else {
            continue;
        };
        if pid as u32 == std::process::id() || !pid_gone(pid) {
            continue;
        }
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_dir() && m.uid() == uid => {
                let _ = fs::remove_dir_all(&path);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cheatu-maptest-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dir_is_private() {
        let root = scratch("mode");
        let dir = TempDir::create_in(&root).unwrap();
        let mode = fs::metadata(dir.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        let path = dir.path().to_path_buf();
        drop(dir);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dirs_of_dead_processes_are_swept() {
        let root = scratch("sweep");
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let stale = root.join(format!("{DIR_PREFIX}{dead}-3"));
        fs::create_dir(&stale).unwrap();
        fs::write(stale.join("map.bin"), b"x").unwrap();
        // pid 1 always exists.
        let alive = root.join(format!("{DIR_PREFIX}1-3"));
        fs::create_dir(&alive).unwrap();
        let other = root.join(format!("{DIR_PREFIX}{dead}-x"));
        fs::create_dir(&other).unwrap();

        let dir = TempDir::create_in(&root).unwrap();
        assert!(!stale.exists(), "stale dir swept");
        assert!(alive.exists(), "live process's dir kept");
        assert!(other.exists(), "foreign name kept");
        drop(dir);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dir_names_parse() {
        assert_eq!(dir_pid("cheatu-ptrmap-123-4"), Some(123));
        assert_eq!(dir_pid("cheatu-ptrmap-123-x"), None);
        assert_eq!(dir_pid("cheatu-ptrmap-0-1"), None);
        assert_eq!(dir_pid("other-123-4"), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_prefers_var_tmp() {
        if Path::new("/var/tmp").is_dir() {
            assert_eq!(PointerMap::temp_root(), PathBuf::from("/var/tmp"));
        }
    }
}

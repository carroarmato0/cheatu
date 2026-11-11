//! File helpers for saving and loading user files. cheatu usually runs as
//! root and opens files other people share, so saves never follow a planted
//! link and loads never block on, or grow memory from, a non-regular file.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TMP: AtomicU64 = AtomicU64::new(0);

/// A temp file name next to `path` that is hard to guess:
/// `.<name>.tmp-<pid>-<nanos>-<n>`.
fn tmp_name(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = NEXT_TMP.fetch_add(1, Ordering::Relaxed);
    let tmp = format!(".{name}.tmp-{}-{nanos:x}-{n}", std::process::id());
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(tmp),
        _ => PathBuf::from(tmp),
    }
}

/// Replace `path` with `bytes` atomically: write a new temp file (mode 0600,
/// never an existing file or link) in the same directory, fsync it and rename
/// it over `path`. On any error the temp file is removed and `path` is left as
/// it was.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut attempts = 0;
    let (tmp, mut file) = loop {
        let tmp = tmp_name(path);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
        {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempts < 16 => attempts += 1,
            Err(e) => return Err(e),
        }
    };
    let result = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Open `path` for reading, refusing anything but a regular file. The open
/// does not block on a FIFO.
pub(crate) fn open_regular(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

/// The error for a file over `max` bytes.
pub(crate) fn too_large(max: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("file too large (max {max} bytes)"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_names_differ() {
        let p = Path::new("/x/t.toml");
        let (a, b) = (tmp_name(p), tmp_name(p));
        assert_ne!(a, b);
        assert_eq!(a.parent(), Some(Path::new("/x")));
        assert!(a
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with(".t.toml.tmp-"));
        assert_eq!(tmp_name(Path::new("t")).parent(), Some(Path::new("")));
    }
}

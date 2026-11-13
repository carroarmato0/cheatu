//! File helpers for saving and loading user files. cheatu usually runs as
//! root and opens files other people share, so saves never follow a planted
//! link and loads never block on, or grow memory from, a non-regular file.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
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

/// Replace `path` with `bytes` atomically: write a new temp file (never an
/// existing file or link) in the same directory, fsync it and rename it over
/// `path`. On any error the temp file is removed and `path` is left as it was.
///
/// A file that already exists keeps its permission bits and, when cheatu runs
/// as root, its owner. A new file gets `0644` minus the umask: cheatu often
/// runs as root, and the user who asked for the save must be able to open it.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // `symlink_metadata`: a link at `path` is replaced, not followed, so its
    // target's mode and owner do not matter.
    let existing = fs::symlink_metadata(path).ok().filter(|m| m.is_file());
    let mut attempts = 0;
    let (tmp, mut file) = loop {
        let tmp = tmp_name(path);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(if existing.is_some() { 0o600 } else { 0o644 })
            .open(&tmp)
        {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempts < 16 => attempts += 1,
            Err(e) => return Err(e),
        }
    };
    let keep = |file: &File| -> io::Result<()> {
        let Some(m) = &existing else { return Ok(()) };
        // Only root may give a file away; for anyone else the owner is
        // already right, or cannot be changed.
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            std::os::unix::fs::fchown(file, Some(m.uid()), Some(m.gid()))?;
        }
        file.set_permissions(fs::Permissions::from_mode(m.mode() & 0o777))
    };
    let result = keep(&file)
        .and_then(|()| file.write_all(bytes))
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

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cheatu-file-io-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let _ = fs::remove_file(&path);
        path
    }

    /// A table saved as root must stay readable by the user who asked for it.
    #[test]
    fn new_file_is_readable_by_others() {
        let path = scratch("new.toml");
        atomic_write(&path, b"x").unwrap();
        // SAFETY: umask is process-wide; it is read and restored at once.
        let umask = unsafe {
            let u = libc::umask(0o022);
            libc::umask(u);
            u as u32
        };
        assert_eq!(mode(&path), 0o644 & !umask);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn overwrite_keeps_existing_mode() {
        use std::os::unix::fs::PermissionsExt;
        let path = scratch("existing.toml");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(mode(&path), 0o640);
        assert_eq!(fs::read(&path).unwrap(), b"new");
        fs::remove_file(&path).unwrap();
    }

    /// Root overwriting a user's table must leave it the user's. Only runs
    /// as root (e.g. in the Linux test container).
    #[test]
    fn root_overwrite_keeps_owner() {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        let path = scratch("owned.toml");
        fs::write(&path, b"old").unwrap();
        std::os::unix::fs::chown(&path, Some(1000), Some(1000)).unwrap();
        atomic_write(&path, b"new").unwrap();
        let m = fs::metadata(&path).unwrap();
        assert_eq!((m.uid(), m.gid()), (1000, 1000));
        fs::remove_file(&path).unwrap();
    }
}

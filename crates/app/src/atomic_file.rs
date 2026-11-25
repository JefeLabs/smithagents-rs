//! Private, atomic file writes for the app's own files under the home
//! directory (`app-settings.json`, `app-layout.json`, `app-state.json`).

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Writes `bytes` to `dir/file_name` atomically: a fresh temp file in the
/// same directory (mode 0600), fsync, then rename over the target. A missing
/// `dir` is created with mode 0700 (an existing one is left as it is). On
/// failure the temp file is removed and the old file, if any, is untouched.
pub(crate) fn write_private(dir: &Path, file_name: &str, bytes: &[u8]) -> io::Result<()> {
    if !dir.is_dir() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let (tmp, file) = create_temp(dir, file_name)?;
    let written = write_and_rename(file, bytes, &tmp, &dir.join(file_name));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Creates a fresh, private (0600) temp file with `create_new`, so a stale
/// file or symlink at the name is never reused or followed.
fn create_temp(dir: &Path, file_name: &str) -> io::Result<(PathBuf, fs::File)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    for _ in 0..16 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(
            ".{file_name}.{}.{nanos:x}.{n}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "cannot find an unused temp file name",
    ))
}

fn write_and_rename(mut file: fs::File, bytes: &[u8], tmp: &Path, dest: &Path) -> io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(tmp, dest)
}

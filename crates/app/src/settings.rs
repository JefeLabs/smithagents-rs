//! Persistent app appearance settings: `<home>/app-settings.json`.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use smithagents_theme::state::AppearanceSettings;

const FILE_NAME: &str = "app-settings.json";

/// Loads and saves [`AppearanceSettings`] under a home directory.
#[derive(Debug, Clone)]
pub struct AppSettingsStore {
    home: PathBuf,
}

impl AppSettingsStore {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    fn path(&self) -> PathBuf {
        self.home.join(FILE_NAME)
    }

    /// Reads the settings. A missing, empty or corrupt file gives the
    /// defaults; this never fails.
    pub fn load(&self) -> AppearanceSettings {
        let path = self.path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                tracing::debug!("no app settings at {}: {e}", path.display());
                return AppearanceSettings::default();
            }
        };
        match serde_json::from_str(&text) {
            Ok(settings) => settings,
            Err(e) => {
                if text.trim().is_empty() {
                    tracing::debug!("empty app settings {}", path.display());
                } else {
                    tracing::warn!("ignoring corrupt app settings {}: {e}", path.display());
                }
                AppearanceSettings::default()
            }
        }
    }

    /// Writes the settings atomically (temp file in the same directory, then
    /// rename), mode 0600. Creates a missing home directory with mode 0700.
    pub fn save(&self, settings: &AppearanceSettings) -> io::Result<()> {
        if !self.home.is_dir() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.home)?;
        }
        let json = serde_json::to_vec_pretty(settings).map_err(io::Error::other)?;
        let (tmp, file) = create_temp(&self.home)?;
        let written = write_and_rename(file, &json, &tmp, &self.path());
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written
    }
}

/// Creates a fresh, private (0600) temp file with `create_new`, so a stale
/// file or symlink at the name is never reused or followed.
fn create_temp(home: &Path) -> io::Result<(PathBuf, fs::File)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    for _ in 0..16 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = home.join(format!(
            ".{FILE_NAME}.{}.{nanos:x}.{n}.tmp",
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

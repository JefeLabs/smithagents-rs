//! Persistent app appearance settings: `<home>/app-settings.json`.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

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
                tracing::debug!("ignoring unreadable app settings {}: {e}", path.display());
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
        let tmp = self
            .home
            .join(format!(".{FILE_NAME}.{}.tmp", std::process::id()));
        let written = write_private(&tmp, &json).and_then(|()| fs::rename(&tmp, self.path()));
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

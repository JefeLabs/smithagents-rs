//! Persistent app appearance settings: `<home>/app-settings.json`.

use std::fs;
use std::io;
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
        let json = serde_json::to_vec_pretty(settings).map_err(io::Error::other)?;
        crate::atomic_file::write_private(&self.home, FILE_NAME, &json)
    }
}

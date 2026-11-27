//! What the app remembers between runs that is not a setting:
//! `<home>/app-state.json`, for example
//! `{ "last_repo_id": "<uuid>", "collapsed_settings_sections": ["appearance"],
//! "right_pane": "intents" }`.
//!
//! It is kept apart from `app-settings.json`, whose shape belongs to the theme
//! crate's `AppearanceSettings`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Serialize, Serializer};
use serde_json::Value;
use smithagents_core::ids::RepoId;

use crate::view_models::SettingsSection;
use crate::views::rail::RailEntry;

const FILE_NAME: &str = "app-state.json";

/// The remembered values. Each field loads on its own: one that is missing or
/// unreadable is `None` without losing the others.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AppState {
    /// The repository the last agent was created in from the picker; the
    /// picker preselects it while it still exists.
    pub last_repo_id: Option<RepoId>,
    /// The Settings pane's sections the user collapsed (all others are
    /// expanded, so a new section starts expanded). Each name once.
    #[serde(
        rename = "collapsed_settings_sections",
        serialize_with = "section_names"
    )]
    pub collapsed_sections: Vec<SettingsSection>,
    /// The rail entry the right dock showed last (`None`: never chosen, so
    /// Settings). Spelled as [`RailEntry::as_str`].
    #[serde(serialize_with = "rail_entry_name")]
    pub right_pane: Option<RailEntry>,
}

fn section_names<S: Serializer>(sections: &[SettingsSection], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(sections.iter().map(|section| section.as_str()))
}

fn rail_entry_name<S: Serializer>(entry: &Option<RailEntry>, s: S) -> Result<S::Ok, S::Error> {
    match entry {
        Some(entry) => s.serialize_some(entry.as_str()),
        None => s.serialize_none(),
    }
}

/// Loads and saves [`AppState`] under a home directory.
#[derive(Debug, Clone)]
pub struct AppStateStore {
    home: PathBuf,
}

impl AppStateStore {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.home.join(FILE_NAME)
    }

    /// Reads the state leniently: a missing, empty or corrupt file, or a field
    /// of the wrong shape, reads as nothing remembered. Never fails; fields
    /// this version does not know are ignored.
    pub fn load(&self) -> AppState {
        let path = self.path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                tracing::debug!("no app state at {}: {e}", path.display());
                return AppState::default();
            }
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(e) => {
                if !text.trim().is_empty() {
                    tracing::warn!("ignoring corrupt app state {}: {e}", path.display());
                }
                return AppState::default();
            }
        };
        let last_repo_id = value
            .get("last_repo_id")
            .filter(|id| !id.is_null())
            .and_then(|id| match id.as_str().map(str::parse::<RepoId>) {
                Some(Ok(id)) => Some(id),
                _ => {
                    tracing::warn!("ignoring last_repo_id {id} in {}", path.display());
                    None
                }
            });
        let collapsed_sections = match value.get("collapsed_settings_sections") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(names)) => {
                let mut sections = Vec::new();
                for name in names {
                    match name.as_str().and_then(SettingsSection::parse) {
                        Some(section) if !sections.contains(&section) => sections.push(section),
                        Some(_) => {}
                        // A section of a newer app, or junk.
                        None => tracing::debug!(
                            "ignoring collapsed section {name} in {}",
                            path.display()
                        ),
                    }
                }
                sections
            }
            Some(other) => {
                tracing::warn!(
                    "ignoring collapsed_settings_sections {other} in {}",
                    path.display()
                );
                Vec::new()
            }
        };
        let right_pane = value
            .get("right_pane")
            .filter(|name| !name.is_null())
            .and_then(|name| {
                let entry = name.as_str().and_then(RailEntry::parse);
                if entry.is_none() {
                    // An entry of a newer app, or junk.
                    tracing::debug!("ignoring right_pane {name} in {}", path.display());
                }
                entry
            });
        AppState {
            last_repo_id,
            collapsed_sections,
            right_pane,
        }
    }

    /// Writes the state atomically, mode 0600 (see
    /// [`crate::atomic_file::write_private`]).
    pub fn save(&self, state: &AppState) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
        crate::atomic_file::write_private(&self.home, FILE_NAME, &json)
    }
}

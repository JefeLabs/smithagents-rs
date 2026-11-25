//! The persisted dock layout: `<home>/app-layout.json`, a gpui-component
//! [`DockAreaState`] as written by `DockArea::dump`.
//!
//! Loading is lenient like the settings file: a missing, empty, corrupt or
//! other-version file gives `None` (the default layout), never an error.
//! The shell's panel tree is fixed (the dock is locked), so what a saved
//! layout restores is each side dock's size and open state; see
//! [`saved_dock`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gpui_kit::component::dock::{DockAreaState, DockPlacement};
use gpui_kit::{px, Pixels};

pub const LAYOUT_FILE_NAME: &str = "app-layout.json";

/// The layout schema version written into the file. Bump it when the
/// shell's dock structure changes; files of another version are ignored.
pub const LAYOUT_VERSION: usize = 1;

/// Restored dock sizes are clamped into this range, so a corrupt or
/// hand-edited file cannot collapse a dock to nothing or squeeze the center
/// out of a normal window. The lower bound is the dock's own minimum.
pub const MIN_DOCK_SIZE: Pixels = px(100.);
pub const MAX_DOCK_SIZE: Pixels = px(1000.);

/// Loads and saves the dock layout under a home directory.
#[derive(Debug, Clone)]
pub struct LayoutStore {
    home: PathBuf,
}

impl LayoutStore {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.home.join(LAYOUT_FILE_NAME)
    }

    /// The saved layout, or `None` when there is none this build can use.
    pub fn load(&self) -> Option<DockAreaState> {
        let path = self.path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                tracing::debug!("no dock layout at {}: {e}", path.display());
                return None;
            }
        };
        let state: DockAreaState = match serde_json::from_str(&text) {
            Ok(state) => state,
            Err(e) => {
                if text.trim().is_empty() {
                    tracing::debug!("empty dock layout {}", path.display());
                } else {
                    tracing::warn!("ignoring corrupt dock layout {}: {e}", path.display());
                }
                return None;
            }
        };
        if state.version != Some(LAYOUT_VERSION) {
            tracing::info!(
                "ignoring dock layout {} of version {:?} (expected {LAYOUT_VERSION})",
                path.display(),
                state.version
            );
            return None;
        }
        Some(state)
    }

    /// Writes the layout atomically, mode 0600 (see `atomic_file`).
    pub fn save(&self, state: &DockAreaState) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
        crate::atomic_file::write_private(&self.home, LAYOUT_FILE_NAME, &json)
    }
}

/// What a saved layout restores for one side dock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SavedDock {
    pub size: Pixels,
    pub open: bool,
}

/// The saved size (clamped to [`MIN_DOCK_SIZE`]..=[`MAX_DOCK_SIZE`]) and open
/// state of the left or right dock. `None` for the center and bottom, for a
/// dock the file does not have, for a non-finite size, and for a dock filed
/// under the wrong side.
pub fn saved_dock(state: &DockAreaState, placement: DockPlacement) -> Option<SavedDock> {
    let dock = match placement {
        DockPlacement::Left => state.left_dock.as_ref()?,
        DockPlacement::Right => state.right_dock.as_ref()?,
        DockPlacement::Center | DockPlacement::Bottom => return None,
    };
    if dock.placement() != placement || !f32::from(dock.size()).is_finite() {
        return None;
    }
    Some(SavedDock {
        size: dock.size().clamp(MIN_DOCK_SIZE, MAX_DOCK_SIZE),
        open: dock.open(),
    })
}

//! The persisted window layout:
//!
//! - `<home>/app-layout.json`, a gpui-component [`DockAreaState`]: what
//!   `DockArea::dump` writes for the center and the right dock, plus a
//!   `left_dock` entry the shell writes for its left pane, which is not a
//!   dock ([`with_left_pane`], [`saved_left_pane`]). The panel tree is fixed,
//!   so what it restores is the right dock's size and open state
//!   ([`saved_dock`]), taken as the user's preferred width and fitted to the window
//!   ([`fitted_right_width`]), and the left
//!   pane's mode and expanded width.
//! - `<home>/app-window.json`, the main window's place and size
//!   ([`SavedWindow`]), reused only where it can be seen
//!   ([`restored_window_bounds`]).
//!
//! Loading is lenient like the settings file: a missing, empty, corrupt or
//! other-version file gives `None` (the default), never an error.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gpui_kit::component::dock::{DockAreaState, DockPlacement, DockState, PanelInfo, PanelState};
use gpui_kit::{px, size, Bounds, Pixels, Size, WindowBounds};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::view_models::LeftPaneMode;
use crate::views::rail::RAIL_WIDTH;

pub const LAYOUT_FILE_NAME: &str = "app-layout.json";
pub const WINDOW_FILE_NAME: &str = "app-window.json";

/// The layout schema version written into the file. Bump it when the
/// shell's dock structure changes; files of another version are ignored.
pub const LAYOUT_VERSION: usize = 1;

/// Restored dock sizes are clamped into this range, so a corrupt or
/// hand-edited file cannot collapse a dock to nothing or squeeze the center
/// out of a normal window. The lower bound is the dock's own minimum.
pub const MIN_DOCK_SIZE: Pixels = px(100.);
pub const MAX_DOCK_SIZE: Pixels = px(1000.);

/// The right dock's width when nothing is saved.
pub const DEFAULT_RIGHT_DOCK_SIZE: Pixels = px(300.);

/// The width the center keeps when the side docks are fitted to a window.
pub const MIN_CENTER_WIDTH: Pixels = px(320.);

/// The left pane's width while compact: a fixed strip, never resized.
pub const COMPACT_LEFT_PANE_WIDTH: Pixels = px(56.);
/// The narrowest an expanded left pane is drawn, saved or dragged to.
pub const MIN_LEFT_PANE_WIDTH: Pixels = px(100.);
/// The widest remembered expanded width (a guard against corrupt files).
pub const MAX_LEFT_PANE_WIDTH: Pixels = px(1000.);
/// The left pane's expanded width when nothing is saved.
pub const DEFAULT_LEFT_PANE_WIDTH: Pixels = px(240.);
/// Height of the status bar across the bottom of the window, under the left
/// pane, the dock area and the rail.
pub const STATUS_BAR_HEIGHT: Pixels = px(24.);
/// The persisted name of the left pane's panel. Never change it.
pub const LEFT_PANE_PANEL: &str = "Agents";
/// The persisted name gpui-base gives a tab group.
const TAB_GROUP_PANEL: &str = "TabPanel";

/// A saved window smaller than this is not reused.
const MIN_WINDOW_WIDTH: f32 = 640.;
const MIN_WINDOW_HEIGHT: f32 = 400.;
/// A saved window is reused only if this much of its top strip (where the
/// title bar is) lies on one display, so it can be seen and dragged.
const TITLE_STRIP_HEIGHT: f32 = 30.;
const MIN_VISIBLE_TITLE_WIDTH: f32 = 100.;
const MIN_VISIBLE_TITLE_HEIGHT: f32 = 15.;

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

    /// The saved dock layout, or `None` when there is none this build can
    /// use.
    pub fn load(&self) -> Option<DockAreaState> {
        let state: DockAreaState = read_json(&self.path(), "dock layout")?;
        if state.version != Some(LAYOUT_VERSION) {
            tracing::info!(
                "ignoring dock layout {} of version {:?} (expected {LAYOUT_VERSION})",
                self.path().display(),
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

    pub fn window_path(&self) -> PathBuf {
        self.home.join(WINDOW_FILE_NAME)
    }

    /// The saved window, or `None` when there is none this build can use.
    /// Whether it still fits the displays is [`restored_window_bounds`]'s
    /// question.
    pub fn load_window(&self) -> Option<SavedWindow> {
        let saved: SavedWindow = read_json(&self.window_path(), "window bounds")?;
        if saved.version != LAYOUT_VERSION {
            tracing::info!(
                "ignoring window bounds {} of version {} (expected {LAYOUT_VERSION})",
                self.window_path().display(),
                saved.version
            );
            return None;
        }
        Some(saved)
    }

    /// Writes the window bounds atomically, mode 0600.
    pub fn save_window(&self, saved: &SavedWindow) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(saved).map_err(io::Error::other)?;
        crate::atomic_file::write_private(&self.home, WINDOW_FILE_NAME, &json)
    }
}

/// Reads a JSON file leniently: `None` (logged) when missing, empty or
/// corrupt.
fn read_json<T: DeserializeOwned>(path: &Path, what: &str) -> Option<T> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            tracing::debug!("no {what} at {}: {e}", path.display());
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(e) => {
            if text.trim().is_empty() {
                tracing::debug!("empty {what} file {}", path.display());
            } else {
                tracing::warn!("ignoring corrupt {what} file {}: {e}", path.display());
            }
            None
        }
    }
}

/// The main window as saved, in GPUI's macOS terms: `bounds.origin` is the
/// window frame's top-left and `bounds.size` the **content** size, both
/// relative to the top-left of the display it was on (`display`, that
/// display's UUID). That is what `WindowOptions` takes back (with the
/// display's id): the platform reports the frame size (title bar included)
/// but opens windows from a content size, so saving the frame size would
/// grow the window by its title bar on every launch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub version: usize,
    pub display: Option<String>,
    pub bounds: Bounds<Pixels>,
    pub maximized: bool,
}

/// What to save for a window whose platform bounds are `bounds`, whose
/// content is `content` large, on the display with UUID `display`. `None`
/// for a fullscreen window: the last windowed save stays.
pub fn saved_window(
    bounds: WindowBounds,
    content: Size<Pixels>,
    display: Option<String>,
) -> Option<SavedWindow> {
    let (origin, maximized) = match bounds {
        WindowBounds::Windowed(frame) => (frame.origin, false),
        WindowBounds::Maximized(frame) => (frame.origin, true),
        WindowBounds::Fullscreen(_) => return None,
    };
    Some(SavedWindow {
        version: LAYOUT_VERSION,
        display,
        bounds: Bounds {
            origin,
            size: content,
        },
        maximized,
    })
}

/// How to reopen a saved window on its display, whose bounds are `display`
/// (`None` when that display is gone), or `None` to use the default
/// placement: when its numbers are not finite, it is smaller than 640x400,
/// or its top strip (where the title bar is) is not at least 100x15 px on
/// the display (a smaller display, a window dragged almost off screen).
pub fn restored_window_bounds(
    saved: &SavedWindow,
    display: Option<Bounds<Pixels>>,
) -> Option<WindowBounds> {
    let display = display?;
    let b = saved.bounds;
    let numbers = [b.origin.x, b.origin.y, b.size.width, b.size.height];
    if numbers.iter().any(|v| !f32::from(*v).is_finite())
        || f32::from(b.size.width) < MIN_WINDOW_WIDTH
        || f32::from(b.size.height) < MIN_WINDOW_HEIGHT
    {
        return None;
    }
    let title_strip = Bounds {
        origin: b.origin,
        size: size(b.size.width, px(TITLE_STRIP_HEIGHT)),
    };
    if !display.intersects(&title_strip) {
        return None;
    }
    let seen = display.intersect(&title_strip).size;
    if f32::from(seen.width) < MIN_VISIBLE_TITLE_WIDTH
        || f32::from(seen.height) < MIN_VISIBLE_TITLE_HEIGHT
    {
        return None;
    }
    Some(if saved.maximized {
        WindowBounds::Maximized(b)
    } else {
        WindowBounds::Windowed(b)
    })
}

/// The width the right dock is drawn at: the user's `preferred` width (clamped
/// to [`MIN_DOCK_SIZE`]..=[`MAX_DOCK_SIZE`]; [`DEFAULT_RIGHT_DOCK_SIZE`] when
/// not finite), narrowed if needed so the center keeps [`MIN_CENTER_WIDTH`]
/// in a window `window_width` wide (rail included) beside a left pane drawn
/// `left_drawn_width` wide, but never below [`MIN_DOCK_SIZE`]. When even a
/// minimum right dock leaves too little room, the left pane is narrowed where
/// it is drawn ([`left_pane_extent`]), never this. Pure: the preferred width
/// is an input and is never rewritten, so more room gives it back. An unknown
/// window or left width changes nothing.
pub fn fitted_right_width(
    preferred: Pixels,
    window_width: Pixels,
    left_drawn_width: Pixels,
) -> Pixels {
    let preferred = if f32::from(preferred).is_finite() {
        preferred.clamp(MIN_DOCK_SIZE, MAX_DOCK_SIZE)
    } else {
        DEFAULT_RIGHT_DOCK_SIZE
    };
    let room = f32::from(window_width)
        - RAIL_WIDTH
        - f32::from(left_drawn_width)
        - f32::from(MIN_CENTER_WIDTH);
    if !room.is_finite() || f32::from(preferred) <= room {
        return preferred;
    }
    px(room.floor().max(f32::from(MIN_DOCK_SIZE)))
}

/// The width the left pane is drawn at: [`COMPACT_LEFT_PANE_WIDTH`] when
/// compact; when expanded, its expanded width (the default when not finite,
/// at most [`MAX_LEFT_PANE_WIDTH`]), narrowed if needed so the center keeps
/// [`MIN_CENTER_WIDTH`] beside `right_extent` (the right dock's drawn width,
/// 0 when hidden), but never below [`MIN_LEFT_PANE_WIDTH`]. Never zero,
/// whatever the inputs.
pub fn left_pane_extent(left: LeftPaneLayout, right_extent: Pixels, available: Pixels) -> Pixels {
    match left.mode {
        LeftPaneMode::Compact => COMPACT_LEFT_PANE_WIDTH,
        LeftPaneMode::Expanded => {
            let mut width = f32::from(left.expanded_width);
            if !width.is_finite() {
                width = f32::from(DEFAULT_LEFT_PANE_WIDTH);
            }
            width = width.min(f32::from(MAX_LEFT_PANE_WIDTH));
            let room = f32::from(available) - f32::from(right_extent) - f32::from(MIN_CENTER_WIDTH);
            if room.is_finite() {
                width = width.min(room);
            }
            px(width.max(f32::from(MIN_LEFT_PANE_WIDTH)))
        }
    }
}

/// The left pane's layout: its mode and the width it has when expanded
/// (remembered while compact).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeftPaneLayout {
    pub mode: LeftPaneMode,
    pub expanded_width: Pixels,
}

impl Default for LeftPaneLayout {
    fn default() -> Self {
        Self {
            mode: LeftPaneMode::Expanded,
            expanded_width: DEFAULT_LEFT_PANE_WIDTH,
        }
    }
}

/// Writes the left pane into `state` as a `left_dock` entry shaped like the
/// one gpui-base dumps (a tab group holding the Agents panel), so the file
/// stays a plain [`DockAreaState`]:
/// - `size` is the expanded width;
/// - `open` is true when expanded;
/// - the Agents panel's info spells the mode out: `{"mode": "compact"}`.
///
/// Builds before the compact mode wrote a closed dock for a hidden pane;
/// [`saved_left_pane`] reads that as compact.
pub fn with_left_pane(mut state: DockAreaState, left: LeftPaneLayout) -> DockAreaState {
    let agents = PanelState {
        panel_name: LEFT_PANE_PANEL.into(),
        children: vec![],
        info: PanelInfo::panel(serde_json::json!({ "mode": left.mode.as_str() })),
    };
    let group = PanelState {
        panel_name: TAB_GROUP_PANEL.into(),
        children: vec![agents],
        info: PanelInfo::tabs(0),
    };
    state.left_dock = Some(DockState::new(
        group,
        DockPlacement::Left,
        left.expanded_width,
        left.mode == LeftPaneMode::Expanded,
    ));
    state
}

/// `state` with the right dock's saved size set to the user's `preferred`
/// width: the dock is dumped at the width it is drawn, which may be
/// temporarily fitted narrower, and the file keeps what the user chose.
pub fn with_right_preferred(mut state: DockAreaState, preferred: Pixels) -> DockAreaState {
    state.right_dock = state.right_dock.map(|dock| {
        DockState::new(
            dock.panel().clone(),
            dock.placement(),
            preferred,
            dock.open(),
        )
    });
    state
}

/// The saved left pane, or `None` (use the default) when the file has no
/// `left_dock` on the left side or its size is not finite. The expanded
/// width is clamped to [`MIN_LEFT_PANE_WIDTH`]..=[`MAX_LEFT_PANE_WIDTH`]. The mode is the Agents panel's `mode` when it is one this
/// build knows; otherwise an open dock is expanded and a closed one compact,
/// which is how a layout from before the compact mode, whose closed left dock
/// meant hidden, comes back.
pub fn saved_left_pane(state: &DockAreaState) -> Option<LeftPaneLayout> {
    let left = state.left_dock.as_ref()?;
    if left.placement() != DockPlacement::Left || !f32::from(left.size()).is_finite() {
        return None;
    }
    let written_mode =
        find_panel(left.panel(), LEFT_PANE_PANEL).and_then(|agents| match &agents.info {
            PanelInfo::Panel(info) => info.get("mode")?.as_str().and_then(LeftPaneMode::parse),
            _ => None,
        });
    let mode = written_mode.unwrap_or(if left.open() {
        LeftPaneMode::Expanded
    } else {
        LeftPaneMode::Compact
    });
    Some(LeftPaneLayout {
        mode,
        expanded_width: left.size().clamp(MIN_LEFT_PANE_WIDTH, MAX_LEFT_PANE_WIDTH),
    })
}

fn find_panel<'a>(state: &'a PanelState, name: &str) -> Option<&'a PanelState> {
    if state.panel_name == name {
        return Some(state);
    }
    state
        .children
        .iter()
        .find_map(|child| find_panel(child, name))
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

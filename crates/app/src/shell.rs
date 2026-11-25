//! The main window's root view.
//!
//! Layout: a gpui-component [`DockArea`] (left dock, center, right dock; no
//! bottom dock) followed by the icon rail at the far right edge. The dock
//! area lays its left dock out full height and puts the center between the
//! two docks; the center panel ([`CenterPanel`]) stacks the connection
//! banner, the tab strip and the content area the web engine will cover.
//!
//! State lives in a separate [`ShellState`] entity, which the center panel
//! observes, so no view holds a handle back to its owner.

use std::time::Duration;

use gpui_kit::component::dock::{
    panel_handle, DockArea, DockAreaState, DockEvent, DockLayout, DockPlacement, DockSkin,
    DockToggleEvent,
};
use gpui_kit::component::h_flex;
use gpui_kit::{
    div, px, App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, KeyBinding, ParentElement as _, Pixels, Render, Styled as _, Subscription, Task,
    Window,
};
use smithagents_theme::native::NativeTheme;

use crate::layout_store::{
    fit_side_docks, saved_dock, saved_window, LayoutStore, SavedWindow, LAYOUT_VERSION,
};
use crate::link::{DaemonLink, LinkConfig};
use crate::model::ShellModel;
use crate::shell_state::ShellState;
use crate::views::center::CenterPanel;
use crate::views::placeholder::PlaceholderPanel;
use crate::views::rail::{rail, RailEntry, RAIL_WIDTH};

gpui_kit::actions!(
    smithagents_shell,
    [
        /// Collapse or expand the left (agents) dock.
        ToggleAgentsPane,
        /// Collapse or expand the right dock with the Settings view.
        ToggleSettingsPane,
    ]
);

/// Key context of the shell, for its key bindings.
pub const KEY_CONTEXT: &str = "SmithAgentsShell";
/// Persisted panel names. Never change them once shipped.
pub const AGENTS_PANEL: &str = "Agents";
pub const SETTINGS_PANEL: &str = "Settings";

const DOCK_AREA_ID: &str = "smithagents-shell";
const DEFAULT_LEFT_DOCK: Pixels = px(240.);
const DEFAULT_RIGHT_DOCK: Pixels = px(300.);
/// Layout changes (a resize drag fires many) are written this long after the
/// last one.
const LAYOUT_SAVE_DELAY: Duration = Duration::from_millis(500);

/// Registers the shell's key bindings: `cmd-b` toggles the agents dock,
/// `cmd-alt-b` the settings dock. (v1 is macOS only. `KeyBinding::new` does
/// not resolve the portable `secondary-` modifier and panics on it.)
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-b", ToggleAgentsPane, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-b", ToggleSettingsPane, Some(KEY_CONTEXT)),
    ]);
}

/// How to build a [`Shell`].
pub struct ShellOptions {
    /// The starting model (empty in the app; tests pre-fill it).
    pub model: ShellModel,
    /// Starts a [`DaemonLink`] with this configuration; `None` runs offline.
    pub link: Option<LinkConfig>,
    /// Where the dock layout and window bounds persist; `None` keeps them
    /// in memory only.
    pub layout: Option<LayoutStore>,
    /// The current theme, for agent accents.
    pub native_theme: Option<NativeTheme>,
}

/// The root view of the main window.
pub struct Shell {
    state: Entity<ShellState>,
    dock: Entity<DockArea>,
    layout: Option<LayoutStore>,
    /// The window's place and size as last observed (not updated while
    /// fullscreen).
    window_place: Option<SavedWindow>,
    /// What this shell last wrote, so an unchanged layout is not rewritten.
    last_saved_layout: Option<DockAreaState>,
    last_saved_window: Option<SavedWindow>,
    pending_save: Option<Task<()>>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    pub fn new(options: ShellOptions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ShellOptions {
            model,
            link,
            layout,
            native_theme,
        } = options;
        let link = link.map(DaemonLink::start);
        let state = cx.new(|cx| ShellState::new(model, native_theme, link, cx));

        let (dock, skin) = DockSkin::dock_area(DOCK_AREA_ID, Some(LAYOUT_VERSION), window, cx);
        // The skin's own collapse buttons sit in the center's title bar, which
        // the center panel replaces with its tab strip; the strip and the rail
        // carry the toggles instead.
        skin.set_toggle_button_visible(false, cx);
        let center = cx.new(|cx| CenterPanel::new(state.clone(), &dock, cx));
        let agents = cx.new(|cx| {
            PlaceholderPanel::new(
                AGENTS_PANEL,
                "Agents",
                "Your agents will be listed here.",
                cx,
            )
        });
        let settings = cx.new(|cx| {
            PlaceholderPanel::new(
                SETTINGS_PANEL,
                "Settings",
                "Appearance and source repositories will be set here.",
                cx,
            )
        });
        let saved = layout.as_ref().and_then(LayoutStore::load);
        let available = window.viewport_size().width - px(RAIL_WIDTH);
        dock.update(cx, |area, cx| {
            let center = DockLayout::tabs().panel_view(panel_handle(center), cx);
            let left = DockLayout::tabs().panel_view(panel_handle(agents), cx);
            let right = DockLayout::tabs().panel_view(panel_handle(settings), cx);
            area.set_center(center, window, cx);
            area.set_dock(DockPlacement::Left, left, window, cx);
            area.set_dock(DockPlacement::Right, right, window, cx);
            area.set_dock_size(DockPlacement::Left, DEFAULT_LEFT_DOCK, window, cx);
            area.set_dock_size(DockPlacement::Right, DEFAULT_RIGHT_DOCK, window, cx);
            // The panels stay where the shell puts them: docks resize and
            // collapse, but tabs cannot be dragged between regions.
            area.set_locked(true, window, cx);
            if let Some(saved) = &saved {
                restore_docks(area, saved, window, cx);
            }
            fit_docks_to_window(area, available, window, cx);
        });

        let subscriptions = vec![
            cx.subscribe(&dock, |this, _, event: &DockEvent, cx| {
                if matches!(event, DockEvent::LayoutChanged) {
                    tracing::trace!("dock layout changed");
                    this.schedule_layout_save(cx);
                    // The rail's selection follows the right dock.
                    cx.notify();
                }
            }),
            // Toggles made by the dock skin itself (a resize handle dragged
            // past the edge); the shell's own entry points log theirs.
            cx.subscribe(&dock, |_, _, event: &DockToggleEvent, _| {
                tracing::debug!(
                    source = "dock",
                    placement = ?event.placement(),
                    open = event.is_open(),
                    "dock toggled"
                );
            }),
            cx.observe_window_bounds(window, |this, window, cx| {
                let place = window_place(window, cx);
                if place.is_some() && place != this.window_place {
                    this.window_place = place;
                    this.schedule_layout_save(cx);
                }
            }),
            // A collapsed dock is no longer drawn, so if it held focus the
            // window has none and keys dispatch from the Kit's `Root`, above
            // the shell's key context: the shortcuts would stop working.
            // Take focus back instead.
            cx.on_focus_lost(window, |this, window, cx| {
                tracing::debug!("focus lost; returning it to the shell");
                this.focus.focus(window, cx);
            }),
            cx.on_app_quit(|this, cx| {
                this.pending_save = None;
                this.save_layout(cx);
                async {}
            }),
            // Closing the window releases the shell before the app quits.
            cx.on_release(|this, cx| this.save_layout(cx)),
        ];

        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            state,
            dock,
            layout,
            window_place: window_place(window, cx),
            last_saved_layout: None,
            last_saved_window: None,
            pending_save: None,
            focus,
            _subscriptions: subscriptions,
        }
    }

    pub fn state(&self) -> &Entity<ShellState> {
        &self.state
    }

    pub fn dock(&self) -> &Entity<DockArea> {
        &self.dock
    }

    /// The rail entry whose view is on screen.
    pub fn rail_active(&self, cx: &App) -> Option<RailEntry> {
        RailEntry::ALL
            .iter()
            .copied()
            .find(|entry| self.dock.read(cx).is_dock_open(entry.placement()))
    }

    /// Selecting the entry already on screen closes its dock; selecting
    /// another opens it.
    pub fn select_rail_entry(&mut self, entry: RailEntry, window: &mut Window, cx: &mut App) {
        let placement = entry.placement();
        let showing = self.rail_active(cx) == Some(entry);
        self.dock.update(cx, |area, cx| {
            if area.is_dock_open(placement) == showing {
                area.toggle_dock(placement, window, cx);
            }
        });
    }

    fn on_toggle_agents(
        &mut self,
        _: &ToggleAgentsPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!(source = "shortcut", placement = ?DockPlacement::Left, "dock toggle");
        self.dock.update(cx, |area, cx| {
            area.toggle_dock(DockPlacement::Left, window, cx)
        });
    }

    fn on_toggle_settings(
        &mut self,
        _: &ToggleSettingsPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!(source = "shortcut", entry = ?RailEntry::Settings, "rail select");
        self.select_rail_entry(RailEntry::Settings, window, cx);
    }

    fn schedule_layout_save(&mut self, cx: &mut Context<Self>) {
        if self.layout.is_none() {
            return;
        }
        // Replacing the task cancels the previous wait: a debounce.
        self.pending_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LAYOUT_SAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| this.save_layout(cx));
        }));
    }

    /// Writes the dock layout and the window bounds, each unless it equals
    /// what was last written.
    fn save_layout(&mut self, cx: &App) {
        let Some(store) = &self.layout else {
            return;
        };
        let state = self.dock.read(cx).dump(cx);
        if self.last_saved_layout.as_ref() != Some(&state) {
            match store.save(&state) {
                Ok(()) => {
                    tracing::debug!("saved the dock layout to {}", store.path().display());
                    self.last_saved_layout = Some(state);
                }
                Err(e) => tracing::warn!(
                    "cannot save the dock layout to {}: {e}",
                    store.path().display()
                ),
            }
        }
        let Some(place) = &self.window_place else {
            return;
        };
        if self.last_saved_window.as_ref() != Some(place) {
            match store.save_window(place) {
                Ok(()) => {
                    tracing::debug!(
                        "saved the window bounds to {}",
                        store.window_path().display()
                    );
                    self.last_saved_window = Some(place.clone());
                }
                Err(e) => tracing::warn!(
                    "cannot save the window bounds to {}: {e}",
                    store.window_path().display()
                ),
            }
        }
    }
}

/// The window's place and content size on its display, as saved (`None`
/// while fullscreen).
fn window_place(window: &Window, cx: &App) -> Option<SavedWindow> {
    let display = window
        .display(cx)
        .and_then(|display| display.uuid().ok())
        .map(|uuid| uuid.to_string());
    saved_window(window.window_bounds(), window.viewport_size(), display)
}

/// Applies the saved size and open state of each side dock.
fn restore_docks(
    area: &mut DockArea,
    saved: &DockAreaState,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) {
    for placement in [DockPlacement::Left, DockPlacement::Right] {
        let Some(dock) = saved_dock(saved, placement) else {
            continue;
        };
        tracing::debug!(?placement, size = ?dock.size, open = dock.open, "restoring dock");
        area.set_dock_size(placement, dock.size, window, cx);
        if area.is_dock_open(placement) != dock.open {
            area.toggle_dock(placement, window, cx);
        }
    }
}

/// Shrinks the side docks if, both open, they would leave the center less
/// than its minimum of the dock area's `available` width.
fn fit_docks_to_window(
    area: &mut DockArea,
    available: Pixels,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) {
    let left = area
        .dock_size(DockPlacement::Left)
        .unwrap_or(DEFAULT_LEFT_DOCK);
    let right = area
        .dock_size(DockPlacement::Right)
        .unwrap_or(DEFAULT_RIGHT_DOCK);
    let (fit_left, fit_right) = fit_side_docks(left, right, available);
    if (fit_left, fit_right) != (left, right) {
        tracing::debug!(
            ?available,
            ?left,
            ?right,
            ?fit_left,
            ?fit_right,
            "fitting docks to the window"
        );
        area.set_dock_size(DockPlacement::Left, fit_left, window, cx);
        area.set_dock_size(DockPlacement::Right, fit_right, window, cx);
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = cx.entity().downgrade();
        let on_select = move |entry, window: &mut Window, cx: &mut App| {
            tracing::debug!(source = "rail", ?entry, "rail select");
            let _ = shell.update(cx, |this, cx| this.select_rail_entry(entry, window, cx));
        };
        h_flex()
            .id("shell")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_toggle_agents))
            .on_action(cx.listener(Self::on_toggle_settings))
            .size_full()
            .child(div().flex_1().min_w_0().h_full().child(self.dock.clone()))
            .child(rail(self.rail_active(cx), on_select, cx))
    }
}

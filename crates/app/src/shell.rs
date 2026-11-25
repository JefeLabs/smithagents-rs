//! The main window's root view.
//!
//! Layout, left to right, full height: the left pane, a gpui-component
//! [`DockArea`] (the center and the right dock; no left or bottom dock), and
//! the icon rail at the far right edge. The center panel ([`CenterPanel`])
//! stacks the connection banner, the tab strip and the content area the web
//! engine will cover.
//!
//! The left pane is never hidden. It is expanded (resizable from its trailing
//! edge) or compact (a fixed
//! [`COMPACT_LEFT_PANE_WIDTH`](crate::layout_store::COMPACT_LEFT_PANE_WIDTH) strip). It is the
//! shell's own flex child rather than a dock: gpui-base floors every dock at
//! 100 px (`Dock::set_size`), its skin always draws a resize handle, and a
//! dock can be closed, so a dock cannot express a fixed narrow strip that
//! never hides. The right pane is a dock and hides as before.
//!
//! State lives in a separate [`ShellState`] entity, which the panes observe,
//! so no view holds a handle back to its owner.

use std::time::Duration;

use gpui_kit::base::HandleEdge;
use gpui_kit::component::dock::{
    panel_handle, DockArea, DockAreaState, DockEvent, DockLayout, DockPlacement, DockSkin,
    DockToggleEvent,
};
use gpui_kit::component::{h_flex, resize_handle, ActiveTheme as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, App, AppContext as _, Axis, Context, DragMoveEvent, Empty, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, KeyBinding, MouseButton, ParentElement as _, Pixels,
    Render, Styled as _, Subscription, Task, TestSupportExt as _, Window,
};
use smithagents_theme::native::NativeTheme;

use crate::layout_store::{
    fit_panes, left_pane_extent, saved_dock, saved_left_pane, saved_window, with_left_pane,
    LayoutStore, LeftPaneLayout, SavedWindow, LAYOUT_VERSION,
};
use crate::link::{DaemonLink, LinkConfig};
use crate::model::ShellModel;
use crate::shell_state::{ShellEvent, ShellState};
use crate::view_models::LeftPaneMode;
use crate::views::center::CenterPanel;
use crate::views::left_pane::LeftPanePlaceholder;
use crate::views::placeholder::PlaceholderPanel;
use crate::views::rail::{rail, RailEntry, RAIL_WIDTH};

gpui_kit::actions!(
    smithagents_shell,
    [
        /// Switch the left (agents) pane between expanded and compact.
        ToggleAgentsPane,
        /// Collapse or expand the right dock with the Settings view.
        ToggleSettingsPane,
    ]
);

/// Key context of the shell, for its key bindings.
pub const KEY_CONTEXT: &str = "SmithAgentsShell";
/// The right dock's persisted panel name. Never change it once shipped.
pub const SETTINGS_PANEL: &str = "Settings";
/// Element id of the left pane's frame.
pub const LEFT_PANE_ID: &str = "left-pane";
/// Element id of the left pane's resize handle (expanded only).
pub const LEFT_PANE_RESIZE_ID: &str = "left-pane-resize";

const DOCK_AREA_ID: &str = "smithagents-shell";
const DEFAULT_RIGHT_DOCK: Pixels = px(300.);
/// Layout changes (a resize drag fires many) are written this long after the
/// last one.
const LAYOUT_SAVE_DELAY: Duration = Duration::from_millis(500);

/// Registers the shell's key bindings: `cmd-b` switches the agents pane
/// between expanded and compact, `cmd-alt-b` hides or shows the settings
/// dock. (v1 is macOS only. `KeyBinding::new` does
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

/// The drag payload of the left pane's resize handle; draws nothing.
#[derive(Clone)]
struct LeftPaneResize;

impl Render for LeftPaneResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// The root view of the main window.
pub struct Shell {
    state: Entity<ShellState>,
    dock: Entity<DockArea>,
    /// The left pane's content (Task 13 replaces the placeholder).
    left_pane: Entity<LeftPanePlaceholder>,
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

        // The saved layout. The right dock is fitted to this window so the
        // center keeps its minimum width; the left pane's remembered expanded
        // width is kept as saved and only narrowed where it is drawn.
        let saved = layout.as_ref().and_then(LayoutStore::load);
        let saved_right = saved
            .as_ref()
            .and_then(|saved| saved_dock(saved, DockPlacement::Right));
        let left = saved.as_ref().and_then(saved_left_pane).unwrap_or_default();
        let available = window.viewport_size().width - px(RAIL_WIDTH);
        let right_size = saved_right.map_or(DEFAULT_RIGHT_DOCK, |right| right.size);
        let right_size = fit_panes(left, right_size, available);
        if saved.is_some() {
            tracing::debug!(
                mode = ?left.mode,
                expanded_width = ?left.expanded_width,
                right = ?saved_right,
                ?right_size,
                "restoring the layout"
            );
        }
        let state = cx.new(|cx| ShellState::new(model, native_theme, link, left, cx));

        let (dock, skin) = DockSkin::dock_area(DOCK_AREA_ID, Some(LAYOUT_VERSION), window, cx);
        // The skin's own collapse buttons sit in the center's title bar, which
        // the center panel replaces with its tab strip; the strip and the rail
        // carry the toggles instead.
        skin.set_toggle_button_visible(false, cx);
        let center = cx.new(|cx| CenterPanel::new(state.clone(), cx));
        let settings = cx.new(|cx| {
            PlaceholderPanel::new(
                SETTINGS_PANEL,
                "Settings",
                "Appearance and source repositories will be set here.",
                cx,
            )
        });
        let left_pane = cx.new(|cx| LeftPanePlaceholder::new(state.clone(), cx));
        dock.update(cx, |area, cx| {
            let center = DockLayout::tabs().panel_view(panel_handle(center), cx);
            let right = DockLayout::tabs().panel_view(panel_handle(settings), cx);
            area.set_center(center, window, cx);
            area.set_dock(DockPlacement::Right, right, window, cx);
            area.set_dock_size(DockPlacement::Right, right_size, window, cx);
            // The panels stay where the shell puts them: the right dock
            // resizes and hides, but tabs cannot be dragged between regions.
            area.set_locked(true, window, cx);
            if saved_right.is_some_and(|right| !right.open) {
                area.toggle_dock(DockPlacement::Right, window, cx);
            }
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
            cx.subscribe_in(&state, window, |this, _, event: &ShellEvent, window, cx| {
                match event {
                    ShellEvent::LeftPaneChanged => {
                        // An expanded pane needs room: the right dock gives way.
                        this.refit_right_dock(window, cx);
                        this.schedule_layout_save(cx);
                        cx.notify();
                    }
                }
            }),
            // Toggles made by the dock skin itself; the shell's own entry
            // points log theirs.
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
            // A hidden right dock is no longer drawn (and a compact left pane
            // may drop a focused child), so if it held focus the window has
            // none and keys dispatch from the Kit's `Root`, above the shell's
            // key context: the shortcuts would stop working. Take focus back.
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
            left_pane,
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
        if !showing {
            self.refit_right_dock(window, cx);
        }
    }

    /// Shrinks the right dock if, beside the left pane, it would leave the
    /// center less than its minimum width (see `layout_store::fit_panes`).
    fn refit_right_dock(&mut self, window: &mut Window, cx: &mut App) {
        let left = self.state.read(cx).left_pane_layout();
        let available = window.viewport_size().width - px(RAIL_WIDTH);
        self.dock.update(cx, |area, cx| {
            let Some(right) = area.dock_size(DockPlacement::Right) else {
                return;
            };
            let fitted = fit_panes(left, right, available);
            if fitted != right {
                tracing::debug!(
                    ?right,
                    ?fitted,
                    "fitting the right dock beside the left pane"
                );
                area.set_dock_size(DockPlacement::Right, fitted, window, cx);
            }
        });
    }

    fn on_toggle_agents(&mut self, _: &ToggleAgentsPane, _: &mut Window, cx: &mut Context<Self>) {
        tracing::debug!(source = "shortcut", "left pane toggle");
        self.state
            .update(cx, |state, cx| state.toggle_left_pane(cx));
    }

    /// The width `left` is drawn at in `window`: fitted beside the right
    /// dock so the center keeps its minimum, never zero.
    fn fitted_left_width(&self, left: LeftPaneLayout, window: &Window, cx: &App) -> Pixels {
        let dock = self.dock.read(cx);
        let right_extent = if dock.is_dock_open(DockPlacement::Right) {
            dock.dock_size(DockPlacement::Right).unwrap_or(px(0.))
        } else {
            px(0.)
        };
        let available = window.viewport_size().width - px(RAIL_WIDTH);
        left_pane_extent(left, right_extent, available)
    }

    /// Follows a drag of the left pane's edge (expanded only). A move without
    /// the left button held means the mouse-up was missed: the drag ends
    /// there, so the pane never keeps following the pointer.
    fn on_left_pane_drag(
        &mut self,
        event: &DragMoveEvent<LeftPaneResize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.event.pressed_button != Some(MouseButton::Left) {
            tracing::debug!("left pane resize ended without a mouse-up");
            cx.stop_active_drag(window);
            return;
        }
        let mut layout = self.state.read(cx).left_pane_layout();
        if layout.mode != LeftPaneMode::Expanded {
            return;
        }
        layout.expanded_width = event.event.position.x - event.bounds.left();
        // The same fit as drawing: the center keeps its minimum width.
        let width = self.fitted_left_width(layout, window, cx);
        self.state
            .update(cx, |state, cx| state.set_left_expanded_width(width, cx));
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
        let state = with_left_pane(
            self.dock.read(cx).dump(cx),
            self.state.read(cx).left_pane_layout(),
        );
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

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = cx.entity().downgrade();
        let on_select = move |entry, window: &mut Window, cx: &mut App| {
            tracing::debug!(source = "rail", ?entry, "rail select");
            let _ = shell.update(cx, |this, cx| this.select_rail_entry(entry, window, cx));
        };
        let expanded = self.state.read(cx).left_pane_mode() == LeftPaneMode::Expanded;
        let left_width = self.fitted_left_width(self.state.read(cx).left_pane_layout(), window, cx);
        let theme = cx.theme();
        let left_pane = div()
            .id(LEFT_PANE_ID)
            .test_support()
            .relative()
            .flex_none()
            .h_full()
            .w(left_width)
            .overflow_hidden()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(self.left_pane.clone())
            // Only an expanded pane resizes; a compact one is a fixed strip.
            .when(expanded, |this| {
                this.child(
                    resize_handle(LEFT_PANE_RESIZE_ID, Axis::Horizontal)
                        .inside(HandleEdge::Trailing)
                        .on_drag(LeftPaneResize, |drag, _, _, cx| {
                            cx.stop_propagation();
                            cx.new(|_| drag.clone())
                        }),
                )
            });
        h_flex()
            .id("shell")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_toggle_agents))
            .on_action(cx.listener(Self::on_toggle_settings))
            .on_drag_move(cx.listener(Self::on_left_pane_drag))
            .size_full()
            .child(left_pane)
            .child(div().flex_1().min_w_0().h_full().child(self.dock.clone()))
            .child(rail(self.rail_active(cx), on_select, cx))
    }
}

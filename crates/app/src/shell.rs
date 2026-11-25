//! The main window's root view.
//!
//! Layout: a gpui-component [`DockArea`] (left dock, center, right dock; no
//! bottom dock) followed by the icon rail at the far right edge. The dock
//! area lays its left dock out full height and puts the center between the
//! two docks; the center panel ([`CenterPanel`]) stacks the connection
//! banner, the tab strip and the content area the web engine will cover.
//!
//! State lives in a separate [`ShellState`] entity (the [`ShellModel`], the
//! [`DaemonLink`], the queue of [`Effect`]s), which the center panel observes,
//! so no view holds a handle back to its owner.

use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::dock::{
    panel_handle, DockArea, DockAreaState, DockEvent, DockLayout, DockPlacement, DockSkin,
};
use gpui_kit::component::h_flex;
use gpui_kit::{
    div, px, App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, KeyBinding, ParentElement as _, Pixels, Render, Styled as _, Subscription, Task,
    WeakEntity, Window,
};
use smithagents_center_host::PixelRect;
use smithagents_theme::native::NativeTheme;
use tokio::sync::mpsc;

use crate::layout_store::{saved_dock, LayoutStore, LAYOUT_VERSION};
use crate::link::{DaemonLink, LinkConfig};
use crate::model::{Action, Effect, ShellModel};
use crate::view_models::banner_text;
use crate::views::center::CenterPanel;
use crate::views::placeholder::PlaceholderPanel;
use crate::views::rail::{rail, RailEntry};

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
/// How often the link's last error is re-read while it retries; the link
/// changes it without sending an action.
const LINK_ERROR_POLL: Duration = Duration::from_secs(1);

/// Registers the shell's key bindings: `cmd-b` toggles the agents dock,
/// `cmd-alt-b` the settings dock. (v1 is macOS only. `KeyBinding::new` does
/// not resolve the portable `secondary-` modifier and panics on it.)
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-b", ToggleAgentsPane, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-b", ToggleSettingsPane, Some(KEY_CONTEXT)),
    ]);
}

/// Receives the center content area's rectangle whenever it changes. Called
/// outside any `ShellState` update, so it may update other entities.
pub type CenterBoundsListener = Rc<dyn Fn(PixelRect, &mut App)>;

/// The shell's state: the model, the link, and the effects for the tab
/// lifecycle (Task 15) to carry out.
pub struct ShellState {
    model: ShellModel,
    /// Effects not yet consumed, oldest first. Task 15 drains them with
    /// [`ShellState::take_effects`]; until then they are only logged.
    effects: VecDeque<Effect>,
    link: Option<DaemonLink>,
    /// The link's last connect error as last read, for the banner.
    link_error: Option<String>,
    native_theme: Option<NativeTheme>,
    center_rect: Option<PixelRect>,
    center_listener: Option<CenterBoundsListener>,
    _tasks: Vec<Task<()>>,
}

impl ShellState {
    fn new(
        model: ShellModel,
        native_theme: Option<NativeTheme>,
        link: Option<(DaemonLink, mpsc::Receiver<Action>)>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut tasks = Vec::new();
        let link = link.map(|(link, actions)| {
            tasks.push(pump_actions(actions, cx));
            tasks.push(watch_link_error(cx));
            link
        });
        let mut state = Self {
            model,
            effects: VecDeque::new(),
            link,
            link_error: None,
            native_theme,
            center_rect: None,
            center_listener: None,
            _tasks: tasks,
        };
        state.link_error = state.current_link_error();
        state
    }

    pub fn model(&self) -> &ShellModel {
        &self.model
    }

    /// The daemon link, when the shell runs one.
    pub fn link(&self) -> Option<&DaemonLink> {
        self.link.as_ref()
    }

    /// Applies one action (from the user or the link), queues its effects and
    /// redraws.
    pub fn dispatch(&mut self, action: Action, cx: &mut Context<Self>) {
        self.apply(action);
        self.link_error = self.current_link_error();
        cx.notify();
    }

    /// Applies a burst of actions with one redraw.
    fn dispatch_all(&mut self, actions: Vec<Action>, cx: &mut Context<Self>) {
        for action in actions {
            self.apply(action);
        }
        self.link_error = self.current_link_error();
        cx.notify();
    }

    fn apply(&mut self, action: Action) {
        if let Action::ConnectionChanged(connection) = &action {
            tracing::debug!(?connection, "daemon connection changed");
        }
        let effects = self.model.apply(action);
        for effect in &effects {
            tracing::debug!(?effect, "shell effect queued");
        }
        self.effects.extend(effects);
    }

    /// Effects not yet taken, oldest first.
    pub fn pending_effects(&self) -> &VecDeque<Effect> {
        &self.effects
    }

    /// Takes every pending effect, oldest first.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        self.effects.drain(..).collect()
    }

    /// The banner line(s), or `None` while connected.
    pub fn banner(&self) -> Option<String> {
        banner_text(&self.model.connection(), self.link_error.as_deref())
    }

    /// The theme agent accents are drawn in.
    pub fn native_theme(&self) -> Option<&NativeTheme> {
        self.native_theme.as_ref()
    }

    pub fn set_native_theme(&mut self, theme: NativeTheme, cx: &mut Context<Self>) {
        self.native_theme = Some(theme);
        cx.notify();
    }

    /// The center content area's last reported rectangle.
    pub fn center_rect(&self) -> Option<PixelRect> {
        self.center_rect
    }

    /// Sets the one center-bounds listener (replacing any earlier one). If the
    /// rectangle is already known, the listener receives it right after this
    /// update ends.
    pub fn set_center_bounds_listener(
        &mut self,
        listener: impl Fn(PixelRect, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        let listener: CenterBoundsListener = Rc::new(listener);
        if let Some(rect) = self.center_rect {
            let first = listener.clone();
            cx.defer(move |cx| first(rect, cx));
        }
        self.center_listener = Some(listener);
    }

    /// Records `rect`; returns the listener to call when it changed.
    fn record_center_rect(&mut self, rect: PixelRect) -> Option<CenterBoundsListener> {
        if self.center_rect == Some(rect) {
            return None;
        }
        tracing::trace!(?rect, "center bounds changed");
        self.center_rect = Some(rect);
        self.center_listener.clone()
    }

    fn current_link_error(&self) -> Option<String> {
        self.link.as_ref().and_then(DaemonLink::last_error)
    }

    fn refresh_link_error(&mut self, cx: &mut Context<Self>) {
        let latest = self.current_link_error();
        if latest != self.link_error {
            self.link_error = latest;
            cx.notify();
        }
    }
}

/// Feeds the link's actions into the state on GPUI's foreground executor.
/// tokio's receiver needs no tokio runtime to be awaited. Each wake-up
/// drains what is already queued and applies it as one batch. Ends when the
/// link closes the channel or the state is gone; dropping the task drops the
/// receiver, which stops the link.
fn pump_actions(mut actions: mpsc::Receiver<Action>, cx: &mut Context<ShellState>) -> Task<()> {
    cx.spawn(async move |state, cx| {
        while let Some(first) = actions.recv().await {
            let mut batch = vec![first];
            while let Ok(next) = actions.try_recv() {
                batch.push(next);
            }
            if state
                .update(cx, |state, cx| state.dispatch_all(batch, cx))
                .is_err()
            {
                break;
            }
        }
        tracing::debug!("daemon link channel closed");
    })
}

fn watch_link_error(cx: &mut Context<ShellState>) -> Task<()> {
    cx.spawn(async move |state, cx| loop {
        cx.background_executor().timer(LINK_ERROR_POLL).await;
        if state
            .update(cx, |state, cx| state.refresh_link_error(cx))
            .is_err()
        {
            break;
        }
    })
}

/// Called from the center content area's prepaint with its rectangle.
pub(crate) fn report_center_bounds(state: &WeakEntity<ShellState>, rect: PixelRect, cx: &mut App) {
    let listener = state
        .update(cx, |state, _| state.record_center_rect(rect))
        .ok()
        .flatten();
    if let Some(listener) = listener {
        listener(rect, cx);
    }
}

/// How to build a [`Shell`].
pub struct ShellOptions {
    /// The starting model (empty in the app; tests pre-fill it).
    pub model: ShellModel,
    /// Starts a [`DaemonLink`] with this configuration; `None` runs offline.
    pub link: Option<LinkConfig>,
    /// Where the dock layout persists; `None` keeps it in memory only.
    pub layout: Option<LayoutStore>,
    /// The current theme, for agent accents.
    pub native_theme: Option<NativeTheme>,
}

/// The root view of the main window.
pub struct Shell {
    state: Entity<ShellState>,
    dock: Entity<DockArea>,
    layout: Option<LayoutStore>,
    /// What this shell last wrote, so an unchanged layout is not rewritten.
    last_saved_layout: Option<DockAreaState>,
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
        });

        let subscriptions = vec![
            cx.subscribe(&dock, |this, _, event: &DockEvent, cx| {
                if matches!(event, DockEvent::LayoutChanged) {
                    this.schedule_layout_save(cx);
                    // The rail's selection follows the right dock.
                    cx.notify();
                }
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
            last_saved_layout: None,
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

    fn toggle_dock(&mut self, placement: DockPlacement, window: &mut Window, cx: &mut App) {
        self.dock
            .update(cx, |area, cx| area.toggle_dock(placement, window, cx));
    }

    fn on_toggle_agents(
        &mut self,
        _: &ToggleAgentsPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_dock(DockPlacement::Left, window, cx);
    }

    fn on_toggle_settings(
        &mut self,
        _: &ToggleSettingsPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    /// Writes the dock layout unless it equals what was last written.
    fn save_layout(&mut self, cx: &App) {
        let Some(store) = &self.layout else {
            return;
        };
        let state = self.dock.read(cx).dump(cx);
        if self.last_saved_layout.as_ref() == Some(&state) {
            return;
        }
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

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = cx.entity().downgrade();
        let on_select = move |entry, window: &mut Window, cx: &mut App| {
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

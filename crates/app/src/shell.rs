//! The main window's root view.
//!
//! Layout, top to bottom: the title bar, the main row, and the status bar
//! ([`status_bar`](crate::views::status_bar)) across the full width. The main
//! row, left to right: the left pane, a gpui-component
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
//!
//! The center tabs' web engine is a [`CenterTabHost`] ([`ShellOptions::tab_host`],
//! [`PlaceholderHost`] by default). The shell attaches it to the window once;
//! the state feeds it the center's rectangle and every theme change itself;
//! the shell shows a notification when a tab does not open
//! ([`ShellEvent::TabOpenFailed`]).

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::HandleEdge;
use gpui_kit::component::dock::{
    panel_handle, DockArea, DockAreaState, DockEvent, DockLayout, DockPlacement, DockSkin,
    DockToggleEvent,
};
use gpui_kit::component::{h_flex, resize_handle, v_flex, ActiveTheme as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, App, AppContext as _, Axis, Context, DragMoveEvent, Empty, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, KeyBinding, Menu, MenuItem, MouseButton,
    ParentElement as _, Pixels, Render, Styled as _, Subscription, Task, TestSupportExt as _,
    Window,
};
use smithagents_center_host::CenterTabHost;
use smithagents_theme::tokens::ThemeSet;

use crate::app_state::AppStateStore;
use crate::appearance::AppearanceRuntime;
use crate::layout_store::{
    fitted_right_width, left_pane_extent, saved_dock, saved_left_pane, saved_window,
    with_left_pane, with_right_preferred, LayoutStore, LeftPaneLayout, SavedWindow,
    DEFAULT_RIGHT_DOCK_SIZE, LAYOUT_VERSION, MIN_DOCK_SIZE,
};
use crate::link::{AgentCommands, DaemonLink, LinkConfig};
use crate::model::{Action, ShellModel};
use crate::settings::AppSettingsStore;
use crate::shell_state::{ConnectionInfoSource, LinkFailureSource, ShellEvent, ShellState};
use crate::tabs::{PlaceholderHost, DEFAULT_PAGE_ORIGIN};
use crate::theme_adapter::{region_paint, OsAppearance, Region, SystemAppearanceSource};
use crate::update::{NoUpdater, Updater};
use crate::view_models::{update_button, LeftPaneMode};
use crate::views::agents::AgentsPane;
use crate::views::center::{notify_tab_failure, CenterPanel};
use crate::views::rail::{rail, RailEntry, RAIL_WIDTH};
use crate::views::settings::SettingsPane;
use crate::views::status_bar::status_bar;
use crate::views::title_bar::title_bar;

gpui_kit::actions!(
    smithagents_shell,
    [
        /// Switch the left (agents) pane between expanded and compact.
        ToggleAgentsPane,
        /// Collapse or expand the right dock with the Settings view.
        ToggleSettingsPane,
        /// Close the active center tab (nothing without one; the window
        /// stays open).
        CloseActiveTab,
        /// Activate the center tab after the active one (the first after
        /// the last).
        NextTab,
        /// Activate the center tab before the active one (the last before
        /// the first).
        PreviousTab,
        /// Quit the app the way closing its window does: the layout is
        /// saved and the tab host shut down; the daemon keeps running.
        Quit,
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
/// Layout changes (a resize drag fires many) are written this long after the
/// last one.
const LAYOUT_SAVE_DELAY: Duration = Duration::from_millis(500);

/// Registers the shell's key bindings and the app-wide [`Quit`]:
/// - `cmd-q` quits (anywhere, also with no window focused);
/// - `cmd-b` switches the agents pane between expanded and compact,
///   `cmd-alt-b` hides or shows the settings dock;
/// - `cmd-w` closes the active tab, `ctrl-tab` and `ctrl-shift-tab` cycle
///   the tabs;
///
/// plus the agents pane's and the settings view's own
/// ([`crate::views::agents::init`], [`crate::views::settings::init`]). (v1 is
/// macOS only. `KeyBinding::new` does not resolve the portable `secondary-`
/// modifier and panics on it.) The menu bar ([`app_menus`]) shows these
/// keys as its items' key equivalents.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-b", ToggleAgentsPane, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-b", ToggleSettingsPane, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-w", CloseActiveTab, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-tab", NextTab, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some(KEY_CONTEXT)),
    ]);
    cx.on_action(quit);
    crate::views::agents::init(cx);
    crate::views::settings::init(cx);
}

/// The macOS menu bar: the app menu (Quit), View (the two pane toggles)
/// and Window (Close Tab, enabled only while a tab is active: the shell
/// handles [`CloseActiveTab`] only then). Set once with `App::set_menus`;
/// each item shows its key binding.
pub fn app_menus() -> Vec<Menu> {
    vec![
        Menu::new(crate::WINDOW_TITLE).items([MenuItem::action(
            format!("Quit {}", crate::WINDOW_TITLE),
            Quit,
        )]),
        Menu::new("View").items([
            MenuItem::action("Toggle Agents Pane", ToggleAgentsPane),
            MenuItem::action("Toggle Settings", ToggleSettingsPane),
        ]),
        Menu::new("Window").items([MenuItem::action("Close Tab", CloseActiveTab)]),
    ]
}

/// [`Quit`]: closes every window first, after the action that asked (which
/// may run inside a window), so each shell takes the same path as a closed
/// window (saves its layout, shuts its tab host down), then quits. The
/// daemon link is dropped with the shell, which never stops the daemon.
fn quit(_: &Quit, cx: &mut App) {
    tracing::info!("quit requested");
    quit_app(cx);
}

/// What [`Quit`] does; the termination signals take the same path.
pub(crate) fn quit_app(cx: &mut App) {
    cx.defer(|cx| {
        for window in cx.windows() {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
        cx.quit();
    });
}

/// Builds the center tabs' web engine. It runs once, in [`Shell::new`],
/// after the GPUI app exists (an engine such as CEF must be created then).
pub type TabHostFactory = Box<dyn FnOnce(&mut App) -> Box<dyn CenterTabHost>>;

/// How to build a [`Shell`]. The default is an empty, offline shell.
#[derive(Default)]
pub struct ShellOptions {
    /// The starting model (empty in the app; tests pre-fill it).
    pub model: ShellModel,
    /// Starts a [`DaemonLink`] with this configuration; `None` runs offline.
    pub link: Option<LinkConfig>,
    /// Where the dock layout and window bounds persist; `None` keeps them
    /// in memory only.
    pub layout: Option<LayoutStore>,
    /// Where the appearance settings persist (`app-settings.json`); `None`
    /// starts from the defaults and keeps changes in memory only.
    pub settings: Option<AppSettingsStore>,
    /// Where the OS light/dark appearance comes from; `None` is the real one
    /// ([`OsAppearance`]). Headless tests inject a fake they can flip.
    pub system_appearance: Option<Rc<dyn SystemAppearanceSource>>,
    /// The design tokens the themes come from; `None` is the embedded set
    /// ([`ThemeSet::embedded`]). Tests pass a set with distinct tokens.
    pub theme_set: Option<&'static ThemeSet>,
    /// Replaces the daemon link for the views' commands (create and close an
    /// agent, add a repository), so headless tests need no daemon; `None`
    /// sends them through the link.
    pub commands: Option<Rc<dyn AgentCommands>>,
    /// Where the app remembers small state such as the last repository
    /// chosen (`app-state.json`); `None` keeps it in memory only.
    pub app_state: Option<AppStateStore>,
    /// How long a command may go unanswered; `None` is
    /// [`DEFAULT_COMMAND_TIMEOUT`](crate::shell_state::DEFAULT_COMMAND_TIMEOUT).
    pub command_timeout: Option<Duration>,
    /// Detects and installs updates; `None` is [`NoUpdater`](crate::update::NoUpdater),
    /// which never finds one.
    pub updater: Option<Rc<dyn Updater>>,
    /// How long an `Info` status message stays; `None` is
    /// [`DEFAULT_INFO_STATUS_DURATION`](crate::shell_state::DEFAULT_INFO_STATUS_DURATION).
    pub info_status_duration: Option<Duration>,
    /// Builds the web engine that draws the center tabs (once, while the
    /// shell is built); `None` is [`PlaceholderHost`], which draws nothing
    /// and logs. Tests inject a recording host (through
    /// [`SharedHost`](crate::tabs::SharedHost) to keep a handle on it).
    pub tab_host: Option<TabHostFactory>,
    /// Where chat pages load from; `None` is [`DEFAULT_PAGE_ORIGIN`]. The app
    /// passes `SMITHAGENTS_WEB_ORIGIN` when set, in debug builds only (see
    /// [`page_origin_from`](crate::tabs::page_origin_from)). An invalid one
    /// is an Error status at startup and fails every open.
    pub page_origin: Option<String>,
    /// Answers the daemon's port and token for new tabs; `None` asks the
    /// daemon link (and, offline, there is none). Headless tests inject a
    /// fake.
    pub connection_info: Option<ConnectionInfoSource>,
    /// Answers the link's last connect error (the banner's and the status
    /// bar's wording); `None` asks the daemon link. Headless tests inject a
    /// fake.
    pub link_failure: Option<LinkFailureSource>,
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
    /// The left pane's content: the agents pane, in either mode.
    left_pane: Entity<AgentsPane>,
    layout: Option<LayoutStore>,
    /// The right dock's width the user chose (a drag of its handle, or the
    /// saved layout). It is what the layout file keeps; the width drawn is
    /// this, fitted beside the left pane and the window
    /// ([`fitted_right_width`]) and so never above it.
    preferred_right: Pixels,
    /// The width this shell last put the right dock at, or saw it settle at.
    /// A dock size different from this is a drag of the user's.
    applied_right: Pixels,
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
            settings,
            system_appearance,
            theme_set,
            commands,
            app_state,
            command_timeout,
            updater,
            info_status_duration,
            tab_host,
            page_origin,
            connection_info,
            link_failure,
        } = options;
        let link = link.map(DaemonLink::start);
        let tab_host = match tab_host {
            Some(factory) => factory(cx),
            None => Box::new(PlaceholderHost),
        };

        // The saved layout. The right dock is fitted to this window so the
        // center keeps its minimum width; the left pane's remembered expanded
        // width is kept as saved and only narrowed where it is drawn.
        let saved = layout.as_ref().and_then(LayoutStore::load);
        let saved_right = saved
            .as_ref()
            .and_then(|saved| saved_dock(saved, DockPlacement::Right));
        let left = saved.as_ref().and_then(saved_left_pane).unwrap_or_default();
        let preferred_right = saved_right.map_or(DEFAULT_RIGHT_DOCK_SIZE, |right| right.size);
        let right_size = fit_right(left, preferred_right, window);
        if saved.is_some() {
            tracing::debug!(
                mode = ?left.mode,
                expanded_width = ?left.expanded_width,
                right = ?saved_right,
                ?right_size,
                "restoring the layout"
            );
        }
        // The theme: the saved settings resolved against the OS appearance.
        // The state applies it before the first frame and follows the OS
        // from here on.
        let system_appearance = system_appearance.unwrap_or_else(|| Rc::new(OsAppearance));
        let appearance = AppearanceRuntime::new(
            theme_set.unwrap_or_else(ThemeSet::embedded),
            settings,
            system_appearance.current(cx),
        );
        let state = cx.new(|cx| {
            let mut state = ShellState::new(model, appearance, link, left, commands, app_state, cx);
            if let Some(timeout) = command_timeout {
                state.set_command_timeout(timeout);
            }
            // The startup check, once: the app's updater finds nothing.
            state.set_updater(updater.unwrap_or_else(|| Rc::new(NoUpdater)), cx);
            if let Some(duration) = info_status_duration {
                state.set_info_status_duration(duration);
            }
            if let Some(source) = connection_info {
                state.set_connection_info(source);
            }
            if let Some(source) = link_failure {
                state.set_link_failure_source(source, cx);
            }
            state.set_tab_host(
                tab_host,
                page_origin.unwrap_or_else(|| DEFAULT_PAGE_ORIGIN.to_owned()),
                cx,
            );
            state
        });
        // The web engine gets the window once; the state hands it the
        // center's rectangle after every layout that changes it.
        state.update(cx, |state, cx| state.attach_tab_host(&*window, cx));

        let (dock, skin) = DockSkin::dock_area(DOCK_AREA_ID, Some(LAYOUT_VERSION), window, cx);
        // The skin's own collapse buttons sit in the center's title bar, which
        // the center panel replaces with its tab strip; the strip and the rail
        // carry the toggles instead.
        skin.set_toggle_button_visible(false, cx);
        let center = cx.new(|cx| CenterPanel::new(state.clone(), cx));
        let settings = cx.new(|cx| SettingsPane::new(state.clone(), window, cx));
        let left_pane = cx.new(|cx| AgentsPane::new(state.clone(), window, cx));
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

        let weak_state = state.downgrade();
        let follow_os = system_appearance.observe(
            window,
            Box::new(move |appearance, cx| {
                let _ =
                    weak_state.update(cx, |state, cx| state.set_system_appearance(appearance, cx));
            }),
        );
        let subscriptions = vec![
            // "System" follows the OS appearance live.
            follow_os,
            // A theme change reaches the views through the state.
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe(&dock, |this, _, event: &DockEvent, cx| {
                if matches!(event, DockEvent::LayoutChanged) {
                    tracing::trace!("dock layout changed");
                    this.schedule_layout_save(cx);
                    // The rail's selection follows the right dock.
                    cx.notify();
                }
            }),
            // A drag of the right dock's handle changes its size without a
            // layout event; this is how the user's width is noticed.
            cx.observe_in(&dock, window, |this, _, window, cx| {
                this.on_dock_notified(window, cx);
            }),
            cx.subscribe_in(&state, window, |this, _, event: &ShellEvent, window, cx| {
                match event {
                    ShellEvent::LeftPaneChanged => {
                        // An expanded pane needs room: the right dock gives way.
                        this.refit_right_dock(window, cx);
                        this.schedule_layout_save(cx);
                        cx.notify();
                    }
                    ShellEvent::RevealSettings => {
                        if this.rail_active(cx) != Some(RailEntry::Settings) {
                            tracing::debug!(source = "agents", "showing the settings dock");
                            this.select_rail_entry(RailEntry::Settings, window, cx);
                            cx.notify();
                        }
                    }
                    // The views repaint from the state's notify; the state
                    // already pushed the theme to the web tabs.
                    ShellEvent::ThemeChanged => {}
                    ShellEvent::TabOpenFailed(text) => {
                        notify_tab_failure(text.clone(), window, cx);
                    }
                    // The settings view handles its own focus.
                    ShellEvent::FocusSettings(_) => {}
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
                // The dock's room changed with the window.
                this.refit_right_dock(window, cx);
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
            // The app quits (a signal, or the system's own Quit) with the
            // window still open.
            cx.on_app_quit(|this, cx| {
                this.pending_save = None;
                this.save_layout(cx);
                this.shut_down_tabs(cx);
                async {}
            }),
            // Closing the window (and Quit, which closes it first) releases
            // the shell before the app quits.
            cx.on_release(|this, cx| {
                this.save_layout(cx);
                this.shut_down_tabs(cx);
            }),
        ];

        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            state,
            dock,
            left_pane,
            layout,
            preferred_right,
            applied_right: right_size,
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

    /// Shuts the tab host down (once, whichever of quit and close comes
    /// first).
    fn shut_down_tabs(&self, cx: &mut App) {
        self.state.update(cx, |state, _| state.shut_down_tabs());
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

    /// Draws the right dock at its preferred width fitted to the window and
    /// the left pane (see [`fitted_right_width`]). Called when an input
    /// changed: the left pane's mode or width, the window's size, the dock
    /// opening. It rewrites only the drawn width, never the preference, and
    /// does so open or closed, so a dock opened later is already fitted.
    fn refit_right_dock(&mut self, window: &mut Window, cx: &mut App) {
        let left = self.state.read(cx).left_pane_layout();
        let fitted = fit_right(left, self.preferred_right, window);
        self.applied_right = fitted;
        self.dock.update(cx, |area, cx| {
            let Some(right) = area.dock_size(DockPlacement::Right) else {
                return;
            };
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

    /// The dock changed. An open right dock whose size is not the one this
    /// shell applied was resized by the user: that width becomes the
    /// preference (as far as it fits).
    fn on_dock_notified(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dock = self.dock.read(cx);
        if !dock.is_dock_open(DockPlacement::Right) {
            return;
        }
        let Some(size) = dock.dock_size(DockPlacement::Right) else {
            return;
        };
        if size == self.applied_right {
            return;
        }
        let left = self.state.read(cx).left_pane_layout();
        let fitted = fit_right(left, size, window);
        tracing::debug!(?size, ?fitted, "the right dock was resized by the user");
        self.preferred_right = fitted;
        self.applied_right = fitted;
        if fitted != size {
            self.dock.update(cx, |area, cx| {
                area.set_dock_size(DockPlacement::Right, fitted, window, cx)
            });
        }
        self.schedule_layout_save(cx);
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

    fn on_close_active_tab(&mut self, _: &CloseActiveTab, _: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.state.read(cx).model().active_tab() else {
            return;
        };
        tracing::debug!(source = "shortcut", agent = %active, "close tab");
        self.state
            .update(cx, |state, cx| state.dispatch(Action::CloseTab(active), cx));
    }

    fn on_next_tab(&mut self, _: &NextTab, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(true, cx);
    }

    fn on_previous_tab(&mut self, _: &PreviousTab, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(false, cx);
    }

    /// Activates the tab after (or before) the active one, wrapping around;
    /// nothing without a tab.
    fn cycle_tab(&mut self, forward: bool, cx: &mut Context<Self>) {
        let next = {
            let model = self.state.read(cx).model();
            let tabs = model.tabs();
            let at = model
                .active_tab()
                .and_then(|active| tabs.iter().position(|tab| *tab == active));
            match (tabs.len(), at) {
                (0, _) => None,
                (len, Some(at)) if forward => Some(tabs[(at + 1) % len]),
                (len, Some(at)) => Some(tabs[(at + len - 1) % len]),
                (len, None) => Some(tabs[if forward { 0 } else { len - 1 }]),
            }
        };
        if let Some(next) = next {
            tracing::debug!(source = "shortcut", agent = %next, "switch tab");
            self.state
                .update(cx, |state, cx| state.dispatch(Action::OpenTab(next), cx));
        }
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
            with_right_preferred(self.dock.read(cx).dump(cx), self.preferred_right),
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

/// The right dock's width for `preferred` beside the left pane `left` in
/// `window`. The left pane counts as drawn with the right dock at its minimum,
/// which is its remembered width unless the window is too small for both.
fn fit_right(left: LeftPaneLayout, preferred: Pixels, window: &Window) -> Pixels {
    let width = window.viewport_size().width;
    let left_drawn = left_pane_extent(left, MIN_DOCK_SIZE, width - px(RAIL_WIDTH));
    fitted_right_width(preferred, width, left_drawn)
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
        let state = self.state.downgrade();
        let on_update = move |_: &mut Window, cx: &mut App| {
            tracing::debug!(source = "title bar", "update chip");
            let _ = state.update(cx, |state, cx| state.install_update(cx));
        };
        let expanded = self.state.read(cx).left_pane_mode() == LeftPaneMode::Expanded;
        let has_active_tab = self.state.read(cx).model().active_tab().is_some();
        let left_width = self.fitted_left_width(self.state.read(cx).left_pane_layout(), window, cx);
        let native = self.state.read(cx).shared_native_theme();
        let theme = cx.theme();
        let (left_surface, _) = region_paint(native.as_deref(), Region::LeftPane, cx);
        let left_pane = div()
            .id(LEFT_PANE_ID)
            .test_support()
            .relative()
            .flex_none()
            .h_full()
            .w(left_width)
            .overflow_hidden()
            .bg(left_surface)
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
        let body = h_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(left_pane)
            .child(div().flex_1().min_w_0().h_full().child(self.dock.clone()))
            .child(rail(self.rail_active(cx), on_select, native.as_deref(), cx));
        v_flex()
            .id("shell")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_toggle_agents))
            .on_action(cx.listener(Self::on_toggle_settings))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_previous_tab))
            // Only with a tab to close, so the menu's Close Tab is disabled
            // otherwise (macOS asks whether the action is available).
            .when(has_active_tab, |this| {
                this.on_action(cx.listener(Self::on_close_active_tab))
            })
            .on_drag_move(cx.listener(Self::on_left_pane_drag))
            .size_full()
            .child(title_bar(
                native.as_deref(),
                update_button(self.state.read(cx).update_state()),
                on_update,
                cx,
            ))
            .child(body)
            .child(status_bar(
                self.state.read(cx).status_message(),
                native.as_deref(),
                cx,
            ))
    }
}

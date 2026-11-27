//! The shell's state entity: the [`ShellModel`], the [`DaemonLink`], the
//! center tabs' [`TabManager`] (which carries the model's [`Effect`]s out on
//! the web engine's host right after each dispatch), the left pane's
//! [`LeftPaneLayout`], the remembered [`AppState`] and the appearance (the
//! settings, the `ThemeController` that resolves them, and the theme in
//! effect), plus the tasks that feed the link's actions in.
//!
//! The model and the host stay consistent: a tab the host could not open is
//! closed again in the model (an `Action::RevertOpen`, which also gives the
//! focus back to the tab that had it), so the strip never shows a tab without
//! a page, and the failure is reported ([`ShellEvent::TabOpenFailed`] and an
//! Error status).
//!
//! The tabs follow the state directly: the center's rectangle and the theme
//! in effect reach the [`TabManager`] from inside the state (the center
//! panel's bounds report and [`ShellState::set_appearance`]), so no outside
//! listener or subscription is needed for them.

use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, FixedOffset};
use gpui_kit::{App, Context, EventEmitter, Pixels, Task, WeakEntity};
use raw_window_handle::HasWindowHandle;
use smithagents_center_host::{validated_origin, CenterTabHost, PixelRect};
use smithagents_core::daemon::DaemonToken;
use smithagents_core::ids::{AgentId, IntentId, RepoId};
use smithagents_theme::native::NativeTheme;
use smithagents_theme::state::{AppearanceSettings, ThemeState};
use smithagents_theme::tokens::{Appearance, ThemeSet};
use tokio::sync::mpsc;

use crate::app_state::{AppState, AppStateStore};
use crate::appearance::AppearanceRuntime;
use crate::layout_store::{LeftPaneLayout, MAX_LEFT_PANE_WIDTH, MIN_LEFT_PANE_WIDTH};
use crate::link::{AgentCommands, DaemonLink, FeedbackCommands, IntentCommands, LinkFailure};
use crate::model::{Action, Connection, Effect, ShellModel};
use crate::tabs::{PlaceholderHost, TabError, TabManager, DEFAULT_PAGE_ORIGIN};
use crate::theme_adapter::apply_framework_theme;
use crate::update::{NoUpdater, UpdateState, Updater};
use crate::view_models::{
    connection_banner, repository_added_text, status_to_show, tab_error_text, BannerVm,
    LeftPaneMode, SettingsSection, StatusKind, StatusMessage, OPEN_CHAT_FAILED,
};
use crate::views::rail::RailEntry;

/// How often the link's last error is re-read while the link is not
/// connected: it changes the error between retries without sending an action.
const LINK_ERROR_POLL: Duration = Duration::from_secs(1);

/// How long the views wait for the daemon to answer a command (create or
/// close an agent) before they give up on it and say so.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// How long an `Info` status message stays in the status bar.
pub const DEFAULT_INFO_STATUS_DURATION: Duration = Duration::from_secs(5);

/// Receives the center content area's rectangle.
///
/// The contract:
/// - It is called with each new rectangle after the frame that laid it out,
///   through `App::defer`, so never during a draw and never inside an entity
///   update. It may update any entity, `ShellState` included, and a
///   `cx.notify()` in it schedules the next frame normally.
/// - It is called only when the rectangle changed, in order, so the last call
///   carries the current rectangle.
/// - Setting a listener calls it once with the current rectangle, if one is
///   known yet (also deferred).
///
/// It is an extra observer only: the center tabs follow the rectangle through
/// the state itself ([`TabManager::on_bounds`], in the same deferred call,
/// before the listener), so replacing or never setting a listener never
/// detaches them.
pub type CenterBoundsListener = Rc<dyn Fn(PixelRect, &mut App)>;

/// Answers the daemon's port and token for new web tabs, or `None` while not
/// connected. The shell asks the [`DaemonLink`]; headless tests inject a fake.
/// Keep the token out of logs.
pub type ConnectionInfoSource = Rc<dyn Fn() -> Option<(u16, DaemonToken)>>;

/// Answers the link's last connect error, or `None`. The shell asks the
/// [`DaemonLink`]; headless tests inject a fake.
pub type LinkFailureSource = Rc<dyn Fn() -> Option<LinkFailure>>;

/// Answers the current time, with the offset the views show times in
/// ([`ShellState::now`]). The app reads the local clock; headless tests
/// inject a fixed one.
pub type Clock = Rc<dyn Fn() -> DateTime<FixedOffset>>;

/// An `OpenTab` being carried out: what to put back if its tab fails.
#[derive(Debug, Clone, Copy)]
struct PendingOpen {
    id: AgentId,
    active: Option<AgentId>,
    selected: Option<AgentId>,
}

/// What `ShellState` tells the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    /// The left pane's mode or expanded width changed (save the layout).
    LeftPaneChanged,
    /// Show this rail entry's view: open its dock on it, or switch the open
    /// dock to it (never closes it). See [`ShellState::reveal`].
    Reveal(RailEntry),
    /// Bring a Settings section into view (it is already expanded): the
    /// Settings view scrolls toward it, as far as the content below it
    /// allows, and, for Source repositories, focuses the path field (the
    /// only section that asks for this). Emitted after
    /// `ShellEvent::Reveal(RailEntry::Settings)`.
    FocusSettings(SettingsSection),
    /// The theme in effect changed: [`ShellState::theme_state`] and
    /// [`ShellState::native_theme`] hold the new one. Emitted once per change,
    /// after both the native panes and gpui-component's theme have it, for
    /// whatever must follow it: the shell pushes it to the web tabs.
    ThemeChanged,
    /// A chat tab could not be opened (or re-opened after a reconnect) and was
    /// closed again: show a notification with this sentence. The status bar
    /// already has the error.
    TabOpenFailed(String),
    /// Show the Intents view on this intent's detail. See
    /// [`ShellState::open_intent`].
    OpenIntent(IntentId),
}

pub struct ShellState {
    model: ShellModel,
    /// The left pane is never hidden: expanded or compact.
    left_pane: LeftPaneLayout,
    /// Effects not yet carried out, oldest first; drained through `tabs`
    /// right after each action.
    effects: VecDeque<Effect>,
    /// The center tabs on the web engine's host.
    tabs: TabManager<Box<dyn CenterTabHost>>,
    link: Option<DaemonLink>,
    /// Replaces the link as the source of the daemon's port and token
    /// (headless tests).
    connection_info: Option<ConnectionInfoSource>,
    /// Replaces the link for agent commands (headless tests).
    commands: Option<Rc<dyn AgentCommands>>,
    /// Replaces the link for intent commands (headless tests).
    intent_commands: Option<Rc<dyn IntentCommands>>,
    /// Replaces the link for feedback commands (headless tests).
    feedback_commands: Option<Rc<dyn FeedbackCommands>>,
    /// Replaces the local clock (headless tests).
    clock: Option<Clock>,
    command_timeout: Duration,
    /// Where [`AppState`] persists; `None` keeps it in memory.
    app_state_store: Option<AppStateStore>,
    app_state: AppState,
    /// The link's last connect error as last read, for the banner.
    link_error: Option<LinkFailure>,
    /// Replaces the link as the source of its last error (headless tests).
    link_failure: Option<LinkFailureSource>,
    /// The saved appearance settings and their resolution.
    appearance: AppearanceRuntime,
    /// The theme in effect (last applied).
    theme_state: ThemeState,
    /// Shared with the views, which hold it across a frame without copying.
    native_theme: Option<Rc<NativeTheme>>,
    center_rect: Option<PixelRect>,
    center_listener: Option<CenterBoundsListener>,
    /// The `OpenTab` whose effects are running, so a failed open can be
    /// reverted ([`Action::RevertOpen`]).
    opening: Option<PendingOpen>,
    /// Feeds the link's actions in; `None` without a link.
    _pump: Option<Task<()>>,
    /// Polls the link's error; runs only while a link is not connected.
    error_watch: Option<Task<()>>,
    /// The transient status message of the status bar.
    status: Option<StatusMessage>,
    /// Clears an `Info` message; replaced (so cancelled) with the message.
    status_timer: Option<Task<()>>,
    /// The status was set while the connection was down.
    status_set_offline: bool,
    info_status_duration: Duration,
    updater: Rc<dyn Updater>,
    update: UpdateState,
    /// The running update check or install (one at a time).
    update_task: Option<Task<()>>,
}

impl ShellState {
    /// Builds the state and applies the theme `appearance` resolves to, to
    /// gpui-component's theme and the native panes, before the first frame.
    pub(crate) fn new(
        model: ShellModel,
        appearance: AppearanceRuntime,
        link: Option<(DaemonLink, mpsc::Receiver<Action>)>,
        left_pane: LeftPaneLayout,
        commands: Option<Rc<dyn AgentCommands>>,
        app_state_store: Option<AppStateStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (link, pump) = match link {
            Some((link, actions)) => (Some(link), Some(pump_actions(actions, cx))),
            None => (None, None),
        };
        let app_state = app_state_store
            .as_ref()
            .map(AppStateStore::load)
            .unwrap_or_default();
        let theme_state = appearance.state();
        let native_theme = apply_framework_theme(appearance.set(), &theme_state, cx).map(Rc::new);
        tracing::debug!(?theme_state, "starting theme");
        let tabs = TabManager::new(
            Box::new(PlaceholderHost) as Box<dyn CenterTabHost>,
            DEFAULT_PAGE_ORIGIN.to_owned(),
            theme_state.clone(),
        );
        let mut state = Self {
            model,
            left_pane,
            effects: VecDeque::new(),
            tabs,
            link,
            connection_info: None,
            commands,
            intent_commands: None,
            feedback_commands: None,
            clock: None,
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
            app_state_store,
            app_state,
            link_error: None,
            link_failure: None,
            appearance,
            theme_state,
            native_theme,
            center_rect: None,
            center_listener: None,
            opening: None,
            _pump: pump,
            error_watch: None,
            status: None,
            status_timer: None,
            status_set_offline: false,
            info_status_duration: DEFAULT_INFO_STATUS_DURATION,
            updater: Rc::new(NoUpdater),
            update: UpdateState::UpToDate,
            update_task: None,
        };
        state.link_error = state.current_link_error();
        state.sync_error_watch(cx);
        state
    }

    pub fn model(&self) -> &ShellModel {
        &self.model
    }

    /// The left pane's mode and remembered expanded width.
    pub fn left_pane_layout(&self) -> LeftPaneLayout {
        self.left_pane
    }

    pub fn left_pane_mode(&self) -> LeftPaneMode {
        self.left_pane.mode
    }

    /// Switches the left pane between expanded and compact.
    pub fn toggle_left_pane(&mut self, cx: &mut Context<Self>) {
        self.set_left_pane_mode(self.left_pane.mode.toggled(), cx);
    }

    pub fn set_left_pane_mode(&mut self, mode: LeftPaneMode, cx: &mut Context<Self>) {
        if self.left_pane.mode == mode {
            return;
        }
        tracing::debug!(?mode, "left pane mode");
        self.left_pane.mode = mode;
        cx.emit(ShellEvent::LeftPaneChanged);
        cx.notify();
    }

    /// Sets the width the left pane has when expanded, clamped to
    /// `MIN_LEFT_PANE_WIDTH..=MAX_LEFT_PANE_WIDTH` (a non-finite width is
    /// ignored). It is kept while compact.
    pub fn set_left_expanded_width(&mut self, width: Pixels, cx: &mut Context<Self>) {
        if !f32::from(width).is_finite() {
            return;
        }
        let width = width.clamp(MIN_LEFT_PANE_WIDTH, MAX_LEFT_PANE_WIDTH);
        if self.left_pane.expanded_width == width {
            return;
        }
        tracing::trace!(?width, "left pane expanded width");
        self.left_pane.expanded_width = width;
        cx.emit(ShellEvent::LeftPaneChanged);
        cx.notify();
    }

    /// The daemon link, when the shell runs one.
    pub fn link(&self) -> Option<&DaemonLink> {
        self.link.as_ref()
    }

    /// Where agent commands go: the injected commands if any, else the link;
    /// `None` when the shell runs offline.
    pub fn agent_commands(&self) -> Option<&dyn AgentCommands> {
        match &self.commands {
            Some(commands) => Some(commands.as_ref()),
            None => self.link.as_ref().map(|link| link as &dyn AgentCommands),
        }
    }

    /// Where intent commands go: the injected commands if any, else the
    /// link; `None` when the shell runs offline.
    pub fn intent_commands(&self) -> Option<&dyn IntentCommands> {
        match &self.intent_commands {
            Some(commands) => Some(commands.as_ref()),
            None => self.link.as_ref().map(|link| link as &dyn IntentCommands),
        }
    }

    /// Where feedback commands go: the injected commands if any, else the
    /// link; `None` when the shell runs offline.
    pub fn feedback_commands(&self) -> Option<&dyn FeedbackCommands> {
        match &self.feedback_commands {
            Some(commands) => Some(commands.as_ref()),
            None => self.link.as_ref().map(|link| link as &dyn FeedbackCommands),
        }
    }

    /// Replaces the link for feedback commands.
    pub(crate) fn set_feedback_commands(&mut self, commands: Rc<dyn FeedbackCommands>) {
        self.feedback_commands = Some(commands);
    }

    /// Replaces the link for intent commands.
    pub(crate) fn set_intent_commands(&mut self, commands: Rc<dyn IntentCommands>) {
        self.intent_commands = Some(commands);
    }

    /// The current time as the views show it: the injected clock's, else the
    /// local clock's, in the local offset.
    pub fn now(&self) -> DateTime<FixedOffset> {
        match &self.clock {
            Some(clock) => clock(),
            None => chrono::Local::now().fixed_offset(),
        }
    }

    /// Replaces the local clock.
    pub(crate) fn set_clock(&mut self, clock: Clock) {
        self.clock = Some(clock);
    }

    /// How long a command may go unanswered before the views give up on it
    /// ([`DEFAULT_COMMAND_TIMEOUT`] unless set).
    pub fn command_timeout(&self) -> Duration {
        self.command_timeout
    }

    pub fn set_command_timeout(&mut self, timeout: Duration) {
        self.command_timeout = timeout;
    }

    /// The repository the picker preselects while it still exists.
    pub fn last_repo_id(&self) -> Option<RepoId> {
        self.app_state.last_repo_id
    }

    /// Remembers `id` as the last repository chosen and saves it (a failed
    /// write is logged; the value is still used for this run).
    pub fn set_last_repo_id(&mut self, id: RepoId) {
        if self.app_state.last_repo_id == Some(id) {
            return;
        }
        self.app_state.last_repo_id = Some(id);
        self.save_app_state();
    }

    fn save_app_state(&self) {
        if let Some(store) = &self.app_state_store {
            if let Err(e) = store.save(&self.app_state) {
                tracing::warn!("cannot save {}: {e}", store.path().display());
            }
        }
    }

    /// Whether a Settings section is expanded (all are, until collapsed).
    pub fn settings_section_expanded(&self, section: SettingsSection) -> bool {
        !self.app_state.collapsed_sections.contains(&section)
    }

    /// Expands or collapses a Settings section and remembers it in
    /// `app-state.json` (a failed write is logged; it still holds this run).
    pub fn set_settings_section_expanded(
        &mut self,
        section: SettingsSection,
        expanded: bool,
        cx: &mut Context<Self>,
    ) {
        if self.settings_section_expanded(section) == expanded {
            return;
        }
        tracing::debug!(?section, expanded, "settings section");
        let collapsed = &mut self.app_state.collapsed_sections;
        if expanded {
            collapsed.retain(|s| *s != section);
        } else {
            collapsed.push(section);
        }
        self.save_app_state();
        cx.notify();
    }

    /// Shows the Settings view with `section` expanded, scrolled into view
    /// and its first field focused (the New agent button, when there is no
    /// repository, asks for Source repositories).
    pub fn request_settings_focus(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        tracing::debug!(?section, "settings focus requested");
        self.set_settings_section_expanded(section, true, cx);
        self.reveal(RailEntry::Settings, cx);
        cx.emit(ShellEvent::FocusSettings(section));
    }

    /// The rail entry whose view the right dock shows (while it is open):
    /// the one last chosen, remembered in `app-state.json`; Settings until
    /// one is.
    pub fn right_entry(&self) -> RailEntry {
        self.app_state.right_pane.unwrap_or(RailEntry::Settings)
    }

    /// Makes `entry` the right dock's view and saves it (a failed write is
    /// logged; it still holds this run). Only the shell calls this, as it
    /// switches the dock ([`Shell::show_rail_entry`](crate::shell::Shell::show_rail_entry));
    /// a view asks for a pane with [`ShellState::reveal`].
    pub fn set_right_entry(&mut self, entry: RailEntry, cx: &mut Context<Self>) {
        if self.app_state.right_pane == Some(entry) {
            return;
        }
        tracing::debug!(?entry, "right pane");
        self.app_state.right_pane = Some(entry);
        self.save_app_state();
        cx.notify();
    }

    /// Asks the shell to show `entry`'s view ([`ShellEvent::Reveal`]).
    pub fn reveal(&mut self, entry: RailEntry, cx: &mut Context<Self>) {
        tracing::debug!(?entry, "reveal requested");
        cx.emit(ShellEvent::Reveal(entry));
    }

    /// Asks the shell to show the Intents view on `id`'s detail
    /// ([`ShellEvent::OpenIntent`]): what Submit does with the intent it
    /// created.
    pub fn open_intent(&mut self, id: IntentId, cx: &mut Context<Self>) {
        tracing::debug!(intent = %id, "open intent requested");
        cx.emit(ShellEvent::OpenIntent(id));
    }

    /// Applies one action (from the user or the link), queues its effects and
    /// redraws.
    pub fn dispatch(&mut self, action: Action, cx: &mut Context<Self>) {
        self.dispatch_all(vec![action], cx);
    }

    /// Applies a burst of actions, with one redraw if anything the shell
    /// draws changed (a streamed chunk changes nothing here: the web tab
    /// shows it).
    fn dispatch_all(&mut self, actions: Vec<Action>, cx: &mut Context<Self>) {
        let was_connected = self.model.connection() == Connection::Connected;
        let revision = self.model.revision();
        for action in actions {
            let added = match &action {
                Action::RepoAdded(repo) => Some(repository_added_text(&repo.path)),
                _ => None,
            };
            self.apply(action, cx);
            if let Some(text) = added {
                self.set_status(StatusMessage::info(text), cx);
            }
        }
        // An error that happened while the connection was down is stale once
        // it is back.
        if !was_connected
            && self.model.connection() == Connection::Connected
            && self.status_set_offline
            && self
                .status
                .as_ref()
                .is_some_and(|s| s.kind == StatusKind::Error)
        {
            tracing::debug!("clearing a status error set while disconnected");
            self.clear_status(cx);
        }
        let link_error = self.current_link_error();
        let link_changed = link_error != self.link_error;
        self.link_error = link_error;
        self.sync_error_watch(cx);
        if link_changed || self.model.revision() != revision {
            cx.notify();
        }
    }

    /// Applies one action to the model and carries its effects out on the
    /// tabs, in order. A connection that came up may be to a new daemon:
    /// the tabs then re-open with its port and token.
    fn apply(&mut self, action: Action, cx: &mut Context<Self>) {
        let connected = action == Action::ConnectionChanged(Connection::Connected);
        if let Action::ConnectionChanged(connection) = &action {
            tracing::debug!(?connection, "daemon connection changed");
        }
        self.opening = match action {
            Action::OpenTab(id) if !self.model.tabs().contains(&id) => Some(PendingOpen {
                id,
                active: self.model.active_tab(),
                selected: self.model.selected(),
            }),
            _ => None,
        };
        let effects = self.model.apply(action);
        self.effects.extend(effects);
        self.run_effects(cx);
        self.opening = None;
        if connected {
            self.refresh_tab_connections(cx);
        }
    }

    /// Carries out the queued effects, oldest first. A tab that does not open
    /// is closed in the model again, whose effects run here too.
    fn run_effects(&mut self, cx: &mut Context<Self>) {
        while let Some(effect) = self.effects.pop_front() {
            tracing::debug!(?effect, "shell effect");
            let opening = match effect {
                Effect::OpenCenterTab(id) => Some(id),
                _ => None,
            };
            let conn = opening.and_then(|_| self.connection_info());
            let agent = opening.and_then(|id| self.model.agent(id));
            let conn = conn.as_ref().map(|(port, token)| (*port, token));
            if let Err(error) = self.tabs.handle(effect, agent, conn) {
                // Only an open fails.
                if let Some(id) = opening {
                    self.tab_failed(id, error, cx);
                }
            }
        }
    }

    /// Re-opens the tabs whose pages were booted for another daemon port or
    /// token (see [`TabManager::on_connection`]).
    fn refresh_tab_connections(&mut self, cx: &mut Context<Self>) {
        let conn = self.connection_info();
        let model = &self.model;
        let failed = self
            .tabs
            .on_connection(conn.as_ref().map(|(port, token)| (*port, token)), &|id| {
                model.agent(id).cloned()
            });
        for (id, error) in failed {
            self.tab_failed(id, error, cx);
        }
        self.run_effects(cx);
    }

    /// `id`'s tab did not open: says so (an Error status and a notification
    /// through [`ShellEvent::TabOpenFailed`]) and closes it in the model, so
    /// the strip matches the host. A tab the user was opening is reverted:
    /// the tab and the row that had the focus get it back. The resulting
    /// effects are queued.
    fn tab_failed(&mut self, id: AgentId, error: TabError, cx: &mut Context<Self>) {
        tracing::warn!(agent = %id, %error, "a chat tab could not be opened");
        self.set_status(
            StatusMessage::error(format!("{OPEN_CHAT_FAILED}: {error}")),
            cx,
        );
        if matches!(error, TabError::NotConnected) {
            // The link dropped before the model saw it: the error belongs to
            // the disconnection and clears when the connection comes back.
            self.status_set_offline = true;
        }
        cx.emit(ShellEvent::TabOpenFailed(tab_error_text(&error)));
        let undo = match self.opening.take() {
            Some(open) if open.id == id => Action::RevertOpen {
                id,
                active: open.active,
                selected: open.selected,
            },
            _ => Action::CloseTab(id),
        };
        let effects = self.model.apply(undo);
        self.effects.extend(effects);
    }

    /// The daemon's port and token: from the injected source if any, else
    /// the link; `None` while not connected.
    fn connection_info(&self) -> Option<(u16, DaemonToken)> {
        match &self.connection_info {
            Some(source) => source(),
            None => self.link.as_ref().and_then(DaemonLink::connection_info),
        }
    }

    /// Replaces the link as the source of the daemon's port and token.
    pub(crate) fn set_connection_info(&mut self, source: ConnectionInfoSource) {
        self.connection_info = Some(source);
    }

    /// Replaces the link as the source of its last connect error, and reads
    /// it now.
    pub(crate) fn set_link_failure_source(
        &mut self,
        source: LinkFailureSource,
        cx: &mut Context<Self>,
    ) {
        self.link_failure = Some(source);
        self.refresh_link_error(cx);
        self.sync_error_watch(cx);
    }

    /// Puts the center tabs on `host`, loading pages from `page_origin`.
    /// Called once while the shell is built, before any tab opens. An
    /// invalid origin is reported as an Error status now (it is a setting,
    /// so a connection coming up does not clear it), and every open will
    /// fail with it.
    pub(crate) fn set_tab_host(
        &mut self,
        host: Box<dyn CenterTabHost>,
        page_origin: String,
        cx: &mut Context<Self>,
    ) {
        debug_assert!(
            self.tabs.open_agents().is_empty(),
            "the tab host is chosen before any tab opens"
        );
        tracing::debug!(%page_origin, "chat page origin");
        if let Err(e) = validated_origin(&page_origin) {
            tracing::error!(%page_origin, "invalid chat page origin: {e}");
            self.set_status(
                StatusMessage::error(format!(
                    "Chats cannot open: invalid page origin {page_origin:?} ({e})"
                )),
                cx,
            );
            self.status_set_offline = false;
        }
        self.tabs = TabManager::new(host, page_origin, self.theme_state.clone());
        if let Some(rect) = self.center_rect {
            self.tabs.on_bounds(rect);
        }
    }

    /// Hands the window to the tab host (once, when the window exists). A
    /// failure is an Error status.
    pub(crate) fn attach_tab_host(&mut self, window: &dyn HasWindowHandle, cx: &mut Context<Self>) {
        if let Err(e) = self.tabs.attach(window) {
            tracing::error!("the chat view cannot attach to the window: {e}");
            self.set_status(StatusMessage::error(format!("Chats cannot show: {e}")), cx);
            self.status_set_offline = false;
        }
    }

    /// Shuts the tab host down; only the first call reaches it.
    pub(crate) fn shut_down_tabs(&mut self) {
        self.tabs.shutdown();
    }

    /// Positions the web tabs at the center's new rectangle.
    fn set_tab_bounds(&mut self, rect: PixelRect) {
        self.tabs.on_bounds(rect);
    }

    /// The connection banner, or `None` while connected.
    pub fn banner(&self) -> Option<BannerVm> {
        connection_banner(&self.model.connection(), self.link_error.as_ref())
    }

    /// What the status bar's center shows: the connection's status while not
    /// connected, else the transient message.
    pub fn status_message(&self) -> Option<StatusMessage> {
        status_to_show(
            &self.model.connection(),
            self.link_error.as_ref(),
            self.status.as_ref(),
        )
    }

    /// Shows `message` in the status bar, replacing the current one. An
    /// `Info` message clears itself after the info duration; an `Error`
    /// stays until replaced or [`ShellState::clear_status`].
    pub fn set_status(&mut self, message: StatusMessage, cx: &mut Context<Self>) {
        tracing::debug!(?message, "status message");
        // Replacing the task cancels the previous message's timer.
        self.status_timer = (message.kind == StatusKind::Info).then(|| {
            let duration = self.info_status_duration;
            cx.spawn(async move |state, cx| {
                cx.background_executor().timer(duration).await;
                let _ = state.update(cx, |state, cx| {
                    state.status = None;
                    state.status_timer = None;
                    cx.notify();
                });
            })
        });
        self.status_set_offline = self.model.connection() != Connection::Connected;
        self.status = Some(message);
        cx.notify();
    }

    pub fn clear_status(&mut self, cx: &mut Context<Self>) {
        self.status_timer = None;
        if self.status.take().is_some() {
            cx.notify();
        }
    }

    /// Sets how long an `Info` message stays ([`DEFAULT_INFO_STATUS_DURATION`]
    /// unless set).
    pub fn set_info_status_duration(&mut self, duration: Duration) {
        self.info_status_duration = duration;
    }

    /// Replaces the updater and asks it once for a newer version (the
    /// startup check). Without it the shell runs [`NoUpdater`]. The update
    /// state starts over and a check or install of the old updater is
    /// dropped.
    pub fn set_updater(&mut self, updater: Rc<dyn Updater>, cx: &mut Context<Self>) {
        self.updater = updater;
        self.update_task = None;
        self.update = UpdateState::UpToDate;
        self.check_for_updates(cx);
    }

    pub fn update_state(&self) -> &UpdateState {
        &self.update
    }

    /// Asks the updater for a newer version and stores its answer. Called once
    /// at startup; call it again to re-check (nothing does yet). Ignored while
    /// an install runs, and an install that starts meanwhile wins.
    pub fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        if self.update_task.is_some() || matches!(self.update, UpdateState::RestartRequired { .. })
        {
            return;
        }
        let check = self.updater.check();
        self.update_task = Some(cx.spawn(async move |state, cx| {
            let found = check.await;
            let _ = state.update(cx, |state, cx| {
                state.update_task = None;
                if state.update != found {
                    tracing::debug!(?found, "update state");
                    state.update = found;
                    cx.notify();
                }
            });
        }));
    }

    /// Installs the available version (or retries a failed install). Does
    /// nothing unless an update is `Available` or `Failed`, so a second click
    /// while installing starts no second install. On success the state is
    /// `RestartRequired` and the status says so; on failure it is
    /// `Failed` and the status carries the error.
    pub fn install_update(&mut self, cx: &mut Context<Self>) {
        let version = match &self.update {
            UpdateState::Available { version } | UpdateState::Failed { version, .. } => {
                version.clone()
            }
            UpdateState::UpToDate
            | UpdateState::Installing { .. }
            | UpdateState::RestartRequired { .. } => return,
        };
        tracing::debug!(%version, "installing an update");
        self.update = UpdateState::Installing {
            version: version.clone(),
        };
        let install = self.updater.install(version.clone());
        self.update_task = Some(cx.spawn(async move |state, cx| {
            let result = install.await;
            let _ = state.update(cx, |state, cx| {
                state.update_task = None;
                match result {
                    Ok(()) => {
                        state.update = UpdateState::RestartRequired {
                            version: version.clone(),
                        };
                        state.set_status(
                            StatusMessage::info(format!(
                                "Update v{version} installed. Restart SmithAgents to finish."
                            )),
                            cx,
                        );
                    }
                    Err(message) => {
                        tracing::warn!(%version, %message, "the update failed");
                        state.update = UpdateState::Failed {
                            version,
                            message: message.clone(),
                        };
                        state.set_status(
                            StatusMessage::error(format!("Update failed: {message}")),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The theme agent accents are drawn in.
    pub fn native_theme(&self) -> Option<&NativeTheme> {
        self.native_theme.as_deref()
    }

    /// [`ShellState::native_theme`] as a shared handle, for a view that holds
    /// it while it draws (no copy of the theme per frame).
    pub fn shared_native_theme(&self) -> Option<Rc<NativeTheme>> {
        self.native_theme.clone()
    }

    /// The theme in effect: what the native panes and gpui-component paint
    /// with, and what the web tabs boot with and are pushed.
    /// [`ShellEvent::ThemeChanged`] says when it changes.
    pub fn theme_state(&self) -> &ThemeState {
        &self.theme_state
    }

    /// The design tokens the themes come from.
    pub fn theme_set(&self) -> &'static ThemeSet {
        self.appearance.set()
    }

    /// The user's appearance choices as saved (a stale theme id stays as it
    /// is; [`ShellState::theme_state`] has what it resolved to).
    pub fn appearance_settings(&self) -> &AppearanceSettings {
        self.appearance.settings()
    }

    /// Takes the user's new appearance settings: resolves them with the
    /// `ThemeController`, saves changed settings to `app-settings.json`, and
    /// applies a changed theme live. A failed save shows an error status; the
    /// change still applies for this run.
    pub fn set_appearance(&mut self, settings: AppearanceSettings, cx: &mut Context<Self>) {
        tracing::debug!(?settings, "appearance settings");
        let change = self.appearance.update_settings(settings);
        if let Some(e) = change.save_error {
            tracing::warn!("cannot save the app settings: {e}");
            self.set_status(
                StatusMessage::error(format!("Could not save settings: {e}")),
                cx,
            );
        }
        if let Some(state) = change.state {
            self.apply_theme_state(state, cx);
        }
        // The choice shows even when the theme in effect did not change.
        cx.notify();
    }

    /// The OS appearance changed. With "System" chosen, the theme follows it
    /// live; an explicit theme stays.
    pub fn set_system_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        tracing::debug!(?appearance, "system appearance");
        if let Some(state) = self.appearance.set_system_appearance(appearance) {
            self.apply_theme_state(state, cx);
        }
    }

    /// Applies a resolved theme to gpui-component's theme and the native
    /// panes, and records it.
    fn apply_theme_state(&mut self, state: ThemeState, cx: &mut Context<Self>) {
        let native = apply_framework_theme(self.appearance.set(), &state, cx);
        self.record_theme(state, native, cx);
    }

    /// Records a theme just applied to gpui-component's theme as the one in
    /// effect, hands it to the web tabs (one push for all of them, and the
    /// theme a tab opened later in this same update boots with), and says so.
    fn record_theme(
        &mut self,
        state: ThemeState,
        native: Option<NativeTheme>,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!(?state, "theme changed");
        if let Some(native) = native {
            self.native_theme = Some(Rc::new(native));
        }
        self.tabs.set_theme(state.clone());
        self.theme_state = state;
        cx.emit(ShellEvent::ThemeChanged);
        cx.notify();
    }

    /// The center content area's last reported rectangle.
    pub fn center_rect(&self) -> Option<PixelRect> {
        self.center_rect
    }

    /// Sets the extra center-bounds observer, replacing any earlier one (the
    /// tabs follow the center whatever is set here). See
    /// [`CenterBoundsListener`] for when it is called.
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

    /// Whether the link's error is being polled (only while not connected).
    pub fn is_polling_link_error(&self) -> bool {
        self.error_watch.is_some()
    }

    /// Records `rect`: `None` when it is unchanged, else the extra listener
    /// to call after the tabs, if one is set.
    fn record_center_rect(&mut self, rect: PixelRect) -> Option<Option<CenterBoundsListener>> {
        if self.center_rect == Some(rect) {
            return None;
        }
        tracing::trace!(?rect, "center bounds changed");
        self.center_rect = Some(rect);
        Some(self.center_listener.clone())
    }

    fn current_link_error(&self) -> Option<LinkFailure> {
        match &self.link_failure {
            Some(source) => source(),
            None => self.link.as_ref().and_then(DaemonLink::last_failure),
        }
    }

    fn refresh_link_error(&mut self, cx: &mut Context<Self>) {
        let latest = self.current_link_error();
        if latest != self.link_error {
            self.link_error = latest;
            cx.notify();
        }
    }

    /// Polls the link's error only while a link exists and is not connected;
    /// a connected shell never wakes for it.
    fn sync_error_watch(&mut self, cx: &mut Context<Self>) {
        let has_source = self.link.is_some() || self.link_failure.is_some();
        let wanted = has_source && self.model.connection() != Connection::Connected;
        match (wanted, self.error_watch.is_some()) {
            (true, false) => {
                tracing::debug!("polling the daemon link's error while disconnected");
                self.error_watch = Some(watch_link_error(cx));
            }
            (false, true) => {
                tracing::debug!("stopped polling the daemon link's error");
                // Dropping the task cancels it.
                self.error_watch = None;
            }
            _ => {}
        }
    }
}

impl EventEmitter<ShellEvent> for ShellState {}

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
/// Records it and, when it changed, positions the tabs and then calls the
/// extra listener, after the current update (see [`CenterBoundsListener`]).
/// The deferred call holds the state weakly: it may run after it is gone.
pub(crate) fn report_center_bounds(state: &WeakEntity<ShellState>, rect: PixelRect, cx: &mut App) {
    let Ok(Some(listener)) = state.update(cx, |state, _| state.record_center_rect(rect)) else {
        return;
    };
    let state = state.clone();
    cx.defer(move |cx| {
        let _ = state.update(cx, |state, _| state.set_tab_bounds(rect));
        if let Some(listener) = listener {
            listener(rect, cx);
        }
    });
}

//! The shell's state entity: the [`ShellModel`], the [`DaemonLink`], the
//! [`Effect`]s for the tab lifecycle (Task 15) to carry out, the left pane's
//! [`LeftPaneLayout`] and the remembered [`AppState`], plus the tasks that
//! feed the link's actions in.

use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{App, Context, EventEmitter, Pixels, Task, WeakEntity};
use smithagents_center_host::PixelRect;
use smithagents_core::ids::RepoId;
use smithagents_theme::native::NativeTheme;
use tokio::sync::mpsc;

use crate::app_state::{AppState, AppStateStore};
use crate::layout_store::{LeftPaneLayout, MAX_LEFT_PANE_WIDTH, MIN_LEFT_PANE_WIDTH};
use crate::link::{AgentCommands, DaemonLink};
use crate::model::{Action, Connection, Effect, ShellModel};
use crate::update::{NoUpdater, UpdateState, Updater};
use crate::view_models::{
    banner_text, repository_added_text, status_to_show, LeftPaneMode, StatusKind, StatusMessage,
};

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
pub type CenterBoundsListener = Rc<dyn Fn(PixelRect, &mut App)>;

/// What `ShellState` tells the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellEvent {
    /// The left pane's mode or expanded width changed (save the layout).
    LeftPaneChanged,
    /// Show the Settings view in the right dock (the New agent button found
    /// no repository; Task 14 scrolls it to Source repositories).
    RevealSettings,
}

pub struct ShellState {
    model: ShellModel,
    /// The left pane is never hidden: expanded or compact.
    left_pane: LeftPaneLayout,
    /// Effects not yet consumed, oldest first. Task 15 drains them with
    /// [`ShellState::take_effects`]; until then they are only logged.
    effects: VecDeque<Effect>,
    link: Option<DaemonLink>,
    /// Replaces the link for agent commands (headless tests).
    commands: Option<Rc<dyn AgentCommands>>,
    command_timeout: Duration,
    /// Where [`AppState`] persists; `None` keeps it in memory.
    app_state_store: Option<AppStateStore>,
    app_state: AppState,
    /// The link's last connect error as last read, for the banner.
    link_error: Option<String>,
    native_theme: Option<NativeTheme>,
    center_rect: Option<PixelRect>,
    center_listener: Option<CenterBoundsListener>,
    /// Feeds the link's actions in; `None` without a link.
    _pump: Option<Task<()>>,
    /// Polls the link's error; runs only while a link is not connected.
    error_watch: Option<Task<()>>,
    /// The transient status message of the status bar.
    status: Option<StatusMessage>,
    /// Clears an `Info` message; replaced (so cancelled) with the message.
    status_timer: Option<Task<()>>,
    info_status_duration: Duration,
    updater: Rc<dyn Updater>,
    update: UpdateState,
    /// The running update check or install (one at a time).
    update_task: Option<Task<()>>,
}

impl ShellState {
    pub(crate) fn new(
        model: ShellModel,
        native_theme: Option<NativeTheme>,
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
        let mut state = Self {
            model,
            left_pane,
            effects: VecDeque::new(),
            link,
            commands,
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
            app_state_store,
            app_state,
            link_error: None,
            native_theme,
            center_rect: None,
            center_listener: None,
            _pump: pump,
            error_watch: None,
            status: None,
            status_timer: None,
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
        if let Some(store) = &self.app_state_store {
            if let Err(e) = store.save(&self.app_state) {
                tracing::warn!("cannot save {}: {e}", store.path().display());
            }
        }
    }

    /// Asks the shell to show the Settings view.
    pub fn reveal_settings(&mut self, cx: &mut Context<Self>) {
        tracing::debug!("reveal settings");
        cx.emit(ShellEvent::RevealSettings);
    }

    /// Applies one action (from the user or the link), queues its effects and
    /// redraws.
    pub fn dispatch(&mut self, action: Action, cx: &mut Context<Self>) {
        self.dispatch_all(vec![action], cx);
    }

    /// Applies a burst of actions with one redraw.
    fn dispatch_all(&mut self, actions: Vec<Action>, cx: &mut Context<Self>) {
        for action in actions {
            let added = match &action {
                Action::RepoAdded(repo) => Some(repository_added_text(&repo.path)),
                _ => None,
            };
            self.apply(action);
            if let Some(text) = added {
                self.set_status(StatusMessage::info(text), cx);
            }
        }
        self.link_error = self.current_link_error();
        self.sync_error_watch(cx);
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

    /// What the status bar's center shows: the connection's status while not
    /// connected, else the transient message.
    pub fn status_message(&self) -> Option<StatusMessage> {
        status_to_show(
            &self.model.connection(),
            self.link_error.as_deref(),
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
    /// startup check). Without it the shell runs [`NoUpdater`].
    pub fn set_updater(&mut self, updater: Rc<dyn Updater>, cx: &mut Context<Self>) {
        self.updater = updater;
        self.update_task = None;
        self.check_for_updates(cx);
    }

    pub fn update_state(&self) -> &UpdateState {
        &self.update
    }

    /// Asks the updater for a newer version and stores its answer. Called once
    /// at startup; call it again to re-check (nothing does yet). Ignored while
    /// an install runs, and an install that starts meanwhile wins.
    pub fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        if self.update_task.is_some() {
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
    /// `UpToDate` and the status asks for a restart; on failure it is
    /// `Failed` and the status carries the error.
    pub fn install_update(&mut self, cx: &mut Context<Self>) {
        let version = match &self.update {
            UpdateState::Available { version } | UpdateState::Failed { version, .. } => {
                version.clone()
            }
            UpdateState::UpToDate | UpdateState::Installing { .. } => return,
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
                        state.update = UpdateState::UpToDate;
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
                        state.set_status(StatusMessage::error(message), cx);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
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

    /// Sets the one center-bounds listener, replacing any earlier one. See
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

    /// Polls the link's error only while a link exists and is not connected;
    /// a connected shell never wakes for it.
    fn sync_error_watch(&mut self, cx: &mut Context<Self>) {
        let wanted = self.link.is_some() && self.model.connection() != Connection::Connected;
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
/// Records it and, when it changed, calls the listener after the current
/// update (see [`CenterBoundsListener`]).
pub(crate) fn report_center_bounds(state: &WeakEntity<ShellState>, rect: PixelRect, cx: &mut App) {
    let listener = state
        .update(cx, |state, _| state.record_center_rect(rect))
        .ok()
        .flatten();
    if let Some(listener) = listener {
        cx.defer(move |cx| listener(rect, cx));
    }
}

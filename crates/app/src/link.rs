//! The app's connection to the daemon: find or start it, take a snapshot,
//! forward its events, reconnect, and run commands.
//!
//! GPUI owns the main thread, so the link runs its own tokio runtime on a
//! dedicated OS thread and talks to the UI through a channel of [`Action`]s.
//! The app is only a client: dropping the link stops its thread but never the
//! daemon, and a healthy daemon is never restarted.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use futures::future::LocalBoxFuture;
use futures::{FutureExt as _, StreamExt};
use smithagents_client::{
    ensure_daemon_with, Client, ClientError, EnsureError, FeedbackFile, SpawnOptions,
};
use smithagents_core::agent::{AddSourceControl, Agent, Repo};
use smithagents_core::daemon::{read_lockfile, DaemonToken};
use smithagents_core::feedback::{
    FeedbackOutcome, FeedbackRequest, FeedbackSettings, FeedbackTargets, SubmissionId,
};
use smithagents_core::ids::{AgentId, IntentId, ItemId, QuestionId, RepoId};
use smithagents_core::intent::{
    AgentDefaults, AnswerInput, CreateIntentRequest, Intent, IntentFilter, Patch, ReviewAction,
};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, Notify};

use crate::model::{Action, Connection};

/// Actions buffered between the link and the UI.
const CHANNEL_CAPACITY: usize = 256;

/// How to find or start the daemon.
#[derive(Debug, Clone)]
pub struct LinkConfig {
    pub home: PathBuf,
    pub daemon_bin: PathBuf,
    /// The daemon version this app expects.
    pub version: String,
    /// Extra arguments and environment for a daemon the link starts.
    pub spawn: SpawnOptions,
    /// First reconnect delay; doubles per failed attempt.
    pub backoff_min: Duration,
    /// Longest reconnect delay.
    pub backoff_max: Duration,
    /// How long health, the lists and the subscription may take together
    /// before the daemon counts as unreachable.
    pub snapshot_timeout: Duration,
}

impl LinkConfig {
    /// Defaults: backoff from 0.5 s to 10 s, no extra daemon arguments.
    pub fn new(home: PathBuf, daemon_bin: PathBuf, version: String) -> Self {
        Self {
            home,
            daemon_bin,
            version,
            spawn: SpawnOptions::default(),
            backoff_min: Duration::from_millis(500),
            backoff_max: Duration::from_secs(10),
            snapshot_timeout: Duration::from_secs(10),
        }
    }
}

/// Why a command did not run or failed.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("not connected to the daemon")]
    NotConnected,
    #[error("the daemon link has stopped")]
    Stopped,
    #[error(transparent)]
    Client(#[from] ClientError),
}

/// What kind of failure `last_error` describes, so the UI can react (for
/// example offer a restart on a version mismatch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkErrorKind {
    /// A daemon of another version is running; it was left alone.
    VersionMismatch,
    /// The daemon could not be started.
    StartFailed,
    /// The daemon did not answer, or answered with an error.
    Unreachable,
    Other,
}

/// The link's last connect error, as the UI shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkFailure {
    /// A daemon of another version runs; it was left alone (the app has no
    /// way to stop it yet). `running` is its version, `expected` the one this
    /// app needs. For a daemon of the same version that speaks another wire
    /// protocol, both also name the protocol (see
    /// [`LinkFailure::protocol_mismatch`]).
    VersionMismatch { running: String, expected: String },
    /// Any other failure: its text and kind.
    Other {
        message: String,
        kind: LinkErrorKind,
    },
}

impl LinkFailure {
    /// A daemon of this app's `version` runs, but speaks wire protocol
    /// `running` where this app needs `expected`. To the user it is a daemon
    /// of another version: both are named with their protocol.
    pub fn protocol_mismatch(version: &str, running: u32, expected: u32) -> Self {
        LinkFailure::VersionMismatch {
            running: format!("{version}, protocol {running}"),
            expected: format!("{version}, protocol {expected}"),
        }
    }

    /// The failure as one line of text (also what [`DaemonLink::last_error`]
    /// returns).
    pub fn message(&self) -> String {
        match self {
            LinkFailure::VersionMismatch { running, expected } => {
                format!("smithagentsd {running} is running, but {expected} is expected")
            }
            LinkFailure::Other { message, .. } => message.clone(),
        }
    }

    pub fn kind(&self) -> LinkErrorKind {
        match self {
            LinkFailure::VersionMismatch { .. } => LinkErrorKind::VersionMismatch,
            LinkFailure::Other { kind, .. } => *kind,
        }
    }
}

/// Next reconnect delay: doubled, never above `max`.
fn next_delay(current: Duration, max: Duration) -> Duration {
    current.saturating_mul(2).min(max)
}

/// The backoff bounds made sane: at least 1 ms, `min <= max`.
fn clamp_backoff(min: Duration, max: Duration) -> (Duration, Duration) {
    let max = max.max(Duration::from_millis(1));
    (min.clamp(Duration::from_millis(1), max), max)
}

/// The port and token of the connected daemon.
#[derive(Clone, Debug)]
struct Endpoint {
    port: u16,
    token: DaemonToken,
}

#[derive(Default)]
struct Shared {
    /// Set while connected (snapshot taken, events subscribed).
    connected: Mutex<Option<(Client, Endpoint)>>,
    last_error: Mutex<Option<LinkFailure>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Handle to the link thread. Dropping it stops and joins the thread; the
/// daemon keeps running.
pub struct DaemonLink {
    shared: Arc<Shared>,
    cmd: Commander,
    stop: Arc<Notify>,
    thread: Option<JoinHandle<()>>,
}

impl DaemonLink {
    /// Starts the link thread. The receiver yields what the UI must apply:
    /// `Snapshot`, `IntentsLoaded`, `AgentDefaultsLoaded`, `FeedbackSettingsLoaded`,
    /// `ConnectionChanged(Connected)`, then daemon events; after a loss
    /// `ConnectionChanged(Reconnecting)` and the same loads again.
    pub fn start(config: LinkConfig) -> (DaemonLink, mpsc::Receiver<Action>) {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime builds");
        let handle = runtime.handle().clone();
        let shared = Arc::new(Shared::default());
        let stop = Arc::new(Notify::new());
        let thread = {
            let (shared, stop, tx) = (shared.clone(), stop.clone(), tx.clone());
            std::thread::Builder::new()
                .name("smithagents-link".into())
                .spawn(move || {
                    runtime.block_on(async {
                        tokio::select! {
                            () = stop.notified() => {}
                            // Nobody listens any more: stop retrying.
                            () = tx.closed() => {}
                            () = run(config, shared, tx.clone()) => {}
                        }
                    });
                    // Dropping the runtime cancels command tasks still running.
                })
                .expect("link thread spawns")
        };
        let link = DaemonLink {
            cmd: Commander {
                shared: shared.clone(),
                handle,
                tx,
            },
            shared,
            stop,
            thread: Some(thread),
        };
        (link, rx)
    }

    /// Stops the link thread and waits for it. The daemon is left running.
    pub fn shutdown(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        // `notify_one` stores a permit, so this works even before the loop polls.
        self.stop.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// The latest connect error, cleared once connected again.
    pub fn last_failure(&self) -> Option<LinkFailure> {
        lock(&self.shared.last_error).clone()
    }

    /// The text of [`DaemonLink::last_failure`].
    pub fn last_error(&self) -> Option<String> {
        lock(&self.shared.last_error)
            .as_ref()
            .map(LinkFailure::message)
    }

    /// The kind of [`DaemonLink::last_failure`].
    pub fn last_error_kind(&self) -> Option<LinkErrorKind> {
        lock(&self.shared.last_error)
            .as_ref()
            .map(LinkFailure::kind)
    }

    /// Whether the link thread has ended (shut down, or nobody listens).
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Port and token of the connected daemon, for the web tabs.
    pub fn connection_info(&self) -> Option<(u16, DaemonToken)> {
        lock(&self.shared.connected)
            .as_ref()
            .map(|(_, e)| (e.port, e.token.clone()))
    }

    /// The commands return `'static` futures, so a UI executor can hold them
    /// without borrowing the link.
    pub fn create_agent(
        &self,
        repo_id: RepoId,
    ) -> impl Future<Output = Result<Agent, LinkError>> + Send + 'static {
        self.cmd
            .clone()
            .run(move |c, _| async move { Ok(c.create_agent(repo_id).await?) })
    }

    /// Adds the repo, with the source control `choice` says, and tells the
    /// UI with `Action::RepoAdded` (repos have no event). The action is sent
    /// from the link's runtime, so it is not lost when the caller drops the
    /// future.
    pub fn add_repo(
        &self,
        path: &Path,
        choice: AddSourceControl,
    ) -> impl Future<Output = Result<Repo, LinkError>> + Send + 'static {
        let path = path.to_owned();
        self.cmd.clone().run(move |c, tx| async move {
            let repo = c.add_repo(&path, choice).await?;
            let _ = tx.send(Action::RepoAdded(repo.clone())).await;
            Ok(repo)
        })
    }

    /// Turns repository `id`'s source control on (git) or off, and tells the
    /// UI with `Action::RepoUpdated`, sent from the link's runtime like
    /// [`DaemonLink::add_repo`]'s. Then it reads the feedback settings again
    /// and sends them as `Action::FeedbackSettingsLoaded`: turning it off for
    /// the meta repository clears that choice, and neither the answer nor an
    /// event says so. The switch stands when that read fails.
    pub fn set_source_control(
        &self,
        id: RepoId,
        on: bool,
    ) -> impl Future<Output = Result<Repo, LinkError>> + Send + 'static {
        self.cmd.clone().run(move |c, tx| async move {
            let repo = c.set_source_control(id, on).await?;
            let _ = tx.send(Action::RepoUpdated(repo.clone())).await;
            match c.feedback_settings().await {
                Ok(settings) => {
                    let _ = tx.send(Action::FeedbackSettingsLoaded(settings)).await;
                }
                Err(e) => tracing::warn!(
                    repo = %id,
                    "cannot read the feedback settings after a source control switch: {e}"
                ),
            }
            Ok(repo)
        })
    }

    pub fn close_agent(
        &self,
        id: AgentId,
    ) -> impl Future<Output = Result<(), LinkError>> + Send + 'static {
        self.cmd
            .clone()
            .run(move |c, _| async move { Ok(c.close_agent(id).await?) })
    }

    pub fn create_intent(
        &self,
        req: CreateIntentRequest,
    ) -> impl Future<Output = Result<Intent, LinkError>> + Send + 'static {
        self.intent_command(move |c| async move { c.create_intent(req).await })
    }

    pub fn update_intent(
        &self,
        id: IntentId,
        patch: Patch,
    ) -> impl Future<Output = Result<Intent, LinkError>> + Send + 'static {
        self.intent_command(move |c| async move { c.update_intent(id, patch).await })
    }

    pub fn answer_question(
        &self,
        id: IntentId,
        question: QuestionId,
        answer: AnswerInput,
    ) -> impl Future<Output = Result<Intent, LinkError>> + Send + 'static {
        self.intent_command(move |c| async move { c.answer_question(id, question, answer).await })
    }

    pub fn retry_polish(
        &self,
        id: IntentId,
    ) -> impl Future<Output = Result<Intent, LinkError>> + Send + 'static {
        self.intent_command(move |c| async move { c.retry_polish(id).await })
    }

    /// Accepts, edits (which also accepts) or drops one item.
    pub fn review_item(
        &self,
        id: IntentId,
        item: ItemId,
        action: ReviewAction,
    ) -> impl Future<Output = Result<Intent, LinkError>> + Send + 'static {
        self.intent_command(move |c| async move {
            match action {
                ReviewAction::Accept => c.accept_item(id, item).await,
                ReviewAction::Edit(text) => c.edit_item(id, item, &text).await,
                ReviewAction::Drop => c.drop_item(id, item).await,
            }
        })
    }

    pub fn send_intent(
        &self,
        id: IntentId,
    ) -> impl Future<Output = Result<Intent, LinkError>> + Send + 'static {
        self.intent_command(move |c| async move { c.send_intent(id).await })
    }

    pub fn resolve_intent(
        &self,
        id: IntentId,
    ) -> impl Future<Output = Result<Intent, LinkError>> + Send + 'static {
        self.intent_command(move |c| async move { c.resolve_intent(id).await })
    }

    /// Stores the defaults and tells the UI with `Action::AgentDefaultsLoaded`
    /// (settings have no event), sent from the link's runtime like
    /// [`DaemonLink::add_repo`]'s.
    pub fn set_agent_defaults(
        &self,
        defaults: AgentDefaults,
    ) -> impl Future<Output = Result<AgentDefaults, LinkError>> + Send + 'static {
        self.cmd.clone().run(move |c, tx| async move {
            let stored = c.set_agent_defaults(defaults).await?;
            let _ = tx.send(Action::AgentDefaultsLoaded(stored.clone())).await;
            Ok(stored)
        })
    }

    /// Stores the feedback settings and tells the UI with
    /// `Action::FeedbackSettingsLoaded`, like [`DaemonLink::set_agent_defaults`].
    pub fn set_feedback_settings(
        &self,
        settings: FeedbackSettings,
    ) -> impl Future<Output = Result<FeedbackSettings, LinkError>> + Send + 'static {
        self.cmd.clone().run(move |c, tx| async move {
            let stored = c.set_feedback_settings(&settings).await?;
            let _ = tx
                .send(Action::FeedbackSettingsLoaded(stored.clone()))
                .await;
            Ok(stored)
        })
    }

    /// The repositories Submit can choose from and the tools' readiness.
    pub fn feedback_targets(
        &self,
    ) -> impl Future<Output = Result<FeedbackTargets, LinkError>> + Send + 'static {
        self.cmd
            .clone()
            .run(move |c, _| async move { Ok(c.feedback_targets().await?) })
    }

    /// Sends a submission; an intent it created reaches the UI as
    /// `Action::IntentUpserted` before the answer does, from the link's
    /// runtime like [`DaemonLink::add_repo`]'s.
    pub fn submit_feedback(
        &self,
        request: FeedbackRequest,
        files: Vec<FeedbackFile>,
    ) -> impl Future<Output = Result<FeedbackOutcome, LinkError>> + Send + 'static {
        self.feedback_command(move |c| async move { c.submit_feedback(&request, &files).await })
    }

    /// Runs the steps of a recorded submission that did not succeed; answered
    /// as [`DaemonLink::submit_feedback`] is.
    pub fn retry_feedback(
        &self,
        id: SubmissionId,
    ) -> impl Future<Output = Result<FeedbackOutcome, LinkError>> + Send + 'static {
        self.feedback_command(move |c| async move { c.retry_feedback(id).await })
    }

    /// An idea intent's "Open pull request" (`public_acknowledged`: the user
    /// confirmed that a public repository shows the screenshots to anyone);
    /// the answer reaches the UI as `Action::IntentUpserted` too, as for
    /// every intent command.
    pub fn open_pull_request(
        &self,
        id: IntentId,
        public_acknowledged: bool,
    ) -> impl Future<Output = Result<Intent, LinkError>> + Send + 'static {
        self.intent_command(
            move |c| async move { c.open_pull_request(id, public_acknowledged).await },
        )
    }

    /// An idea intent's "Create implementation intent" (`public_acknowledged`
    /// as for [`Self::open_pull_request`]); the new intent reaches the UI as
    /// `Action::IntentUpserted` too.
    pub fn create_implementation_intent(
        &self,
        id: IntentId,
        public_acknowledged: bool,
    ) -> impl Future<Output = Result<Intent, LinkError>> + Send + 'static {
        self.intent_command(move |c| async move {
            c.create_implementation_intent(id, public_acknowledged)
                .await
        })
    }

    /// Runs one submission or retry; an intent in its outcome reaches the UI
    /// as `Action::IntentUpserted` first.
    fn feedback_command<F, Fut>(
        &self,
        command: F,
    ) -> impl Future<Output = Result<FeedbackOutcome, LinkError>> + Send + 'static
    where
        F: FnOnce(Client) -> Fut + Send + 'static,
        Fut: Future<Output = Result<FeedbackOutcome, ClientError>> + Send + 'static,
    {
        self.cmd.clone().run(move |c, tx| async move {
            let outcome = command(c).await?;
            if let Some(intent) = &outcome.intent {
                let _ = tx.send(Action::IntentUpserted(intent.clone())).await;
            }
            Ok(outcome)
        })
    }

    /// Runs one intent command and hands its answer to the UI as
    /// `Action::IntentUpserted` too, from the link's runtime, so the model
    /// has it even when the caller drops the future (the event of the same
    /// change may come before or after it; the model keeps the later copy).
    fn intent_command<F, Fut>(
        &self,
        command: F,
    ) -> impl Future<Output = Result<Intent, LinkError>> + Send + 'static
    where
        F: FnOnce(Client) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Intent, ClientError>> + Send + 'static,
    {
        self.cmd.clone().run(move |c, tx| async move {
            let intent = command(c).await?;
            let _ = tx.send(Action::IntentUpserted(intent.clone())).await;
            Ok(intent)
        })
    }
}

/// The commands the shell's views send to the daemon: the agents pane's
/// create and close, and the Settings pane's add repository and source
/// control switch. [`DaemonLink`]
/// is the production implementation; headless tests inject a fake through
/// `ShellOptions::commands` so they need no daemon.
///
/// The futures are `'static` (a UI executor holds them without borrowing the
/// implementation) and need not be `Send`: the shell awaits them on GPUI's
/// foreground executor.
pub trait AgentCommands {
    fn create_agent(&self, repo_id: RepoId) -> LocalBoxFuture<'static, Result<Agent, LinkError>>;
    fn close_agent(&self, id: AgentId) -> LocalBoxFuture<'static, Result<(), LinkError>>;
    /// Adds the local folder at `path`, with the source control `choice`
    /// says. The model learns of it through `Action::RepoAdded`, which the
    /// link sends itself (a fake leaves that to the test).
    fn add_repo(
        &self,
        path: &Path,
        choice: AddSourceControl,
    ) -> LocalBoxFuture<'static, Result<Repo, LinkError>>;
    /// Turns repository `id`'s source control on (git) or off. The model
    /// learns of the answer through `Action::RepoUpdated`, and of the
    /// feedback settings through `Action::FeedbackSettingsLoaded`, which the
    /// link sends itself (a fake leaves that to the test).
    fn set_source_control(
        &self,
        id: RepoId,
        on: bool,
    ) -> LocalBoxFuture<'static, Result<Repo, LinkError>>;
}

impl AgentCommands for DaemonLink {
    fn create_agent(&self, repo_id: RepoId) -> LocalBoxFuture<'static, Result<Agent, LinkError>> {
        DaemonLink::create_agent(self, repo_id).boxed_local()
    }

    fn close_agent(&self, id: AgentId) -> LocalBoxFuture<'static, Result<(), LinkError>> {
        DaemonLink::close_agent(self, id).boxed_local()
    }

    fn add_repo(
        &self,
        path: &Path,
        choice: AddSourceControl,
    ) -> LocalBoxFuture<'static, Result<Repo, LinkError>> {
        DaemonLink::add_repo(self, path, choice).boxed_local()
    }

    fn set_source_control(
        &self,
        id: RepoId,
        on: bool,
    ) -> LocalBoxFuture<'static, Result<Repo, LinkError>> {
        DaemonLink::set_source_control(self, id, on).boxed_local()
    }
}

/// The daemon's answer to an intent command: the intent as it now is.
pub type IntentAnswer = LocalBoxFuture<'static, Result<Intent, LinkError>>;

/// The commands the Intents pane and Settings → Agent defaults send to the
/// daemon. [`DaemonLink`] is the production implementation; headless tests
/// inject a fake through `ShellOptions::intent_commands`.
///
/// The model learns of each answer through `Action::IntentUpserted` (or
/// `Action::AgentDefaultsLoaded`), which the link sends itself; a fake leaves
/// that to the test. The futures are `'static` and need not be `Send`, as for
/// [`AgentCommands`].
pub trait IntentCommands {
    fn create_intent(&self, req: CreateIntentRequest) -> IntentAnswer;
    fn update_intent(&self, id: IntentId, patch: Patch) -> IntentAnswer;
    fn answer_question(
        &self,
        id: IntentId,
        question: QuestionId,
        answer: AnswerInput,
    ) -> IntentAnswer;
    fn retry_polish(&self, id: IntentId) -> IntentAnswer;
    /// Accepts, edits (which also accepts) or drops one item.
    fn review_item(&self, id: IntentId, item: ItemId, action: ReviewAction) -> IntentAnswer;
    fn send_intent(&self, id: IntentId) -> IntentAnswer;
    fn resolve_intent(&self, id: IntentId) -> IntentAnswer;
    fn set_agent_defaults(
        &self,
        defaults: AgentDefaults,
    ) -> LocalBoxFuture<'static, Result<AgentDefaults, LinkError>>;
}

impl IntentCommands for DaemonLink {
    fn create_intent(&self, req: CreateIntentRequest) -> IntentAnswer {
        DaemonLink::create_intent(self, req).boxed_local()
    }

    fn update_intent(&self, id: IntentId, patch: Patch) -> IntentAnswer {
        DaemonLink::update_intent(self, id, patch).boxed_local()
    }

    fn answer_question(
        &self,
        id: IntentId,
        question: QuestionId,
        answer: AnswerInput,
    ) -> IntentAnswer {
        DaemonLink::answer_question(self, id, question, answer).boxed_local()
    }

    fn retry_polish(&self, id: IntentId) -> IntentAnswer {
        DaemonLink::retry_polish(self, id).boxed_local()
    }

    fn review_item(&self, id: IntentId, item: ItemId, action: ReviewAction) -> IntentAnswer {
        DaemonLink::review_item(self, id, item, action).boxed_local()
    }

    fn send_intent(&self, id: IntentId) -> IntentAnswer {
        DaemonLink::send_intent(self, id).boxed_local()
    }

    fn resolve_intent(&self, id: IntentId) -> IntentAnswer {
        DaemonLink::resolve_intent(self, id).boxed_local()
    }

    fn set_agent_defaults(
        &self,
        defaults: AgentDefaults,
    ) -> LocalBoxFuture<'static, Result<AgentDefaults, LinkError>> {
        DaemonLink::set_agent_defaults(self, defaults).boxed_local()
    }
}

/// The commands the Feedback tool, Settings → Source repositories and an idea
/// intent's detail send to the daemon. [`DaemonLink`] is the production
/// implementation; headless tests inject a fake through
/// `ShellOptions::feedback_commands`. As for [`IntentCommands`], the link
/// tells the model of each answer itself: the settings, the intent an
/// outcome carries (`Action::IntentUpserted`, before the outcome is
/// answered), and the intent an idea action answers with.
pub trait FeedbackCommands {
    fn set_feedback_settings(
        &self,
        settings: FeedbackSettings,
    ) -> LocalBoxFuture<'static, Result<FeedbackSettings, LinkError>>;
    fn feedback_targets(&self) -> LocalBoxFuture<'static, Result<FeedbackTargets, LinkError>>;
    /// Sends `request` and uploads `files` (each local file as its part).
    fn submit_feedback(
        &self,
        request: FeedbackRequest,
        files: Vec<FeedbackFile>,
    ) -> LocalBoxFuture<'static, Result<FeedbackOutcome, LinkError>>;
    fn retry_feedback(
        &self,
        id: SubmissionId,
    ) -> LocalBoxFuture<'static, Result<FeedbackOutcome, LinkError>>;
    /// Pushes the idea intent's work as its idea branch and opens a draft
    /// pull request; `public_acknowledged` once the user confirmed that a
    /// public repository shows the screenshots to anyone. The link upserts
    /// the answer.
    fn open_pull_request(&self, id: IntentId, public_acknowledged: bool) -> IntentAnswer;
    /// Creates the idea intent's implementation intent (once);
    /// `public_acknowledged` once the user confirmed that its agent's pull
    /// request may show the idea's files to anyone. The link upserts the
    /// answer.
    fn create_implementation_intent(&self, id: IntentId, public_acknowledged: bool)
        -> IntentAnswer;
}

impl FeedbackCommands for DaemonLink {
    fn set_feedback_settings(
        &self,
        settings: FeedbackSettings,
    ) -> LocalBoxFuture<'static, Result<FeedbackSettings, LinkError>> {
        DaemonLink::set_feedback_settings(self, settings).boxed_local()
    }

    fn feedback_targets(&self) -> LocalBoxFuture<'static, Result<FeedbackTargets, LinkError>> {
        DaemonLink::feedback_targets(self).boxed_local()
    }

    fn submit_feedback(
        &self,
        request: FeedbackRequest,
        files: Vec<FeedbackFile>,
    ) -> LocalBoxFuture<'static, Result<FeedbackOutcome, LinkError>> {
        DaemonLink::submit_feedback(self, request, files).boxed_local()
    }

    fn retry_feedback(
        &self,
        id: SubmissionId,
    ) -> LocalBoxFuture<'static, Result<FeedbackOutcome, LinkError>> {
        DaemonLink::retry_feedback(self, id).boxed_local()
    }

    fn open_pull_request(&self, id: IntentId, public_acknowledged: bool) -> IntentAnswer {
        DaemonLink::open_pull_request(self, id, public_acknowledged).boxed_local()
    }

    fn create_implementation_intent(
        &self,
        id: IntentId,
        public_acknowledged: bool,
    ) -> IntentAnswer {
        DaemonLink::create_implementation_intent(self, id, public_acknowledged).boxed_local()
    }
}

/// What commands need, cloneable into `'static` futures.
#[derive(Clone)]
struct Commander {
    shared: Arc<Shared>,
    handle: Handle,
    tx: mpsc::Sender<Action>,
}

impl Commander {
    /// Runs `f` on the link's runtime, so it works from any executor.
    async fn run<T, F, Fut>(self, f: F) -> Result<T, LinkError>
    where
        T: Send + 'static,
        F: FnOnce(Client, mpsc::Sender<Action>) -> Fut,
        Fut: Future<Output = Result<T, LinkError>> + Send + 'static,
    {
        let client = lock(&self.shared.connected)
            .as_ref()
            .map(|(c, _)| c.clone())
            .ok_or(LinkError::NotConnected)?;
        let task = self.handle.spawn(f(client, self.tx));
        task.await.map_err(|_| LinkError::Stopped)?
    }
}

impl Drop for DaemonLink {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

/// Why a session ended.
enum Exit {
    /// The UI dropped its receiver: nothing left to serve.
    ReceiverGone,
    /// The stream ended after being connected.
    StreamEnded,
    Failed(LinkFailure),
}

fn unreachable(e: impl std::fmt::Display) -> Exit {
    Exit::Failed(LinkFailure::Other {
        message: format!("could not reach the daemon: {e}"),
        kind: LinkErrorKind::Unreachable,
    })
}

async fn run(config: LinkConfig, shared: Arc<Shared>, tx: mpsc::Sender<Action>) {
    let (backoff_min, backoff_max) = clamp_backoff(config.backoff_min, config.backoff_max);
    let mut delay = backoff_min;
    let mut announced = false;
    loop {
        let mut reached = false;
        let exit = session(&config, &shared, &tx, &mut reached).await;
        *lock(&shared.connected) = None;
        if reached {
            announced = false;
            delay = backoff_min;
        }
        match exit {
            Exit::ReceiverGone => return,
            Exit::StreamEnded => tracing::debug!("event stream ended; reconnecting"),
            Exit::Failed(failure) => {
                tracing::debug!("daemon link failed: {}", failure.message());
                *lock(&shared.last_error) = Some(failure);
            }
        }
        if !announced {
            announced = true;
            if tx
                .send(Action::ConnectionChanged(Connection::Reconnecting))
                .await
                .is_err()
            {
                return;
            }
        }
        tokio::time::sleep(delay).await;
        delay = next_delay(delay, backoff_max);
    }
}

/// One connection: snapshot, subscribe, forward events until it breaks.
/// Sets `reached` once `Connected` was sent.
async fn session(
    config: &LinkConfig,
    shared: &Shared,
    tx: &mpsc::Sender<Action>,
    reached: &mut bool,
) -> Exit {
    match connect_and_forward(config, shared, tx, reached).await {
        Ok(()) => Exit::StreamEnded,
        Err(exit) => exit,
    }
}

async fn connect_and_forward(
    config: &LinkConfig,
    shared: &Shared,
    tx: &mpsc::Sender<Action>,
    reached: &mut bool,
) -> Result<(), Exit> {
    let client = ensure_daemon_with(
        &config.home,
        &config.daemon_bin,
        &config.version,
        &config.spawn,
    )
    .await
    .map_err(|e| match e {
        EnsureError::VersionMismatch { running, expected } => {
            Exit::Failed(LinkFailure::VersionMismatch { running, expected })
        }
        EnsureError::ProtocolMismatch {
            version,
            running,
            expected,
        } => Exit::Failed(LinkFailure::protocol_mismatch(&version, running, expected)),
        // The error names what failed: the daemon binary, or a file or
        // folder of the home that the start needs.
        e => Exit::Failed(LinkFailure::Other {
            message: e.to_string(),
            kind: LinkErrorKind::StartFailed,
        }),
    })?;
    let info = read_lockfile(&config.home)
        .map_err(unreachable)?
        .ok_or_else(|| unreachable("the daemon's lockfile is gone"))?;
    // The seq is read before listing, so an event between the lists and the
    // subscription is replayed rather than missed; the model drops duplicates.
    // A 410 on subscribing (events too old, or a restarted daemon) is a
    // failure like any other: the next round takes a fresh snapshot.
    let fetch = async {
        let latest_seq = client.health().await?.latest_seq;
        let repos = client.list_repos().await?;
        let agents = client.list_agents(false).await?;
        let intents = client.list_intents(IntentFilter::default()).await?;
        let defaults = client.agent_defaults().await?;
        let feedback = client.feedback_settings().await?;
        let events = client.events(Some(latest_seq)).await?;
        Ok::<_, ClientError>((
            latest_seq, repos, agents, intents, defaults, feedback, events,
        ))
    };
    let (latest_seq, repos, agents, intents, defaults, feedback, mut events) =
        match tokio::time::timeout(config.snapshot_timeout, fetch).await {
            Ok(fetched) => fetched.map_err(unreachable)?,
            Err(_) => return Err(unreachable("the daemon did not answer in time")),
        };

    send(
        tx,
        Action::Snapshot {
            agents,
            repos,
            latest_seq,
        },
    )
    .await?;
    send(tx, Action::IntentsLoaded(intents)).await?;
    send(tx, Action::AgentDefaultsLoaded(defaults)).await?;
    send(tx, Action::FeedbackSettingsLoaded(feedback)).await?;
    // Commands work from here on; the UI sees `Connected` after this.
    *lock(&shared.connected) = Some((
        client,
        Endpoint {
            port: info.port,
            token: info.token,
        },
    ));
    *lock(&shared.last_error) = None;
    send(tx, Action::ConnectionChanged(Connection::Connected)).await?;
    *reached = true;

    while let Some(event) = events.next().await {
        send(tx, Action::Event(event)).await?;
    }
    Ok(())
}

async fn send(tx: &mpsc::Sender<Action>, action: Action) -> Result<(), Exit> {
    tx.send(action).await.map_err(|_| Exit::ReceiverGone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_doubles_up_to_the_cap_and_never_overflows() {
        let max = Duration::from_secs(10);
        assert_eq!(
            next_delay(Duration::from_millis(500), max),
            Duration::from_secs(1)
        );
        assert_eq!(next_delay(Duration::from_secs(6), max), max);
        assert_eq!(next_delay(Duration::MAX, max), max);
    }

    #[test]
    fn backoff_bounds_are_clamped() {
        let ms = Duration::from_millis;
        assert_eq!(clamp_backoff(Duration::ZERO, ms(200)), (ms(1), ms(200)));
        assert_eq!(clamp_backoff(ms(500), ms(200)), (ms(200), ms(200)));
        assert_eq!(clamp_backoff(ms(5), Duration::ZERO), (ms(1), ms(1)));
    }
}

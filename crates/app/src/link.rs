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

use futures::StreamExt;
use smithagents_client::{ensure_daemon_with, Client, ClientError, EnsureError, SpawnOptions};
use smithagents_core::agent::{Agent, Repo};
use smithagents_core::daemon::read_lockfile;
use smithagents_core::ids::{AgentId, RepoId};
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

/// Next reconnect delay: doubled, never above `max`.
fn next_delay(current: Duration, max: Duration) -> Duration {
    current.saturating_mul(2).min(max)
}

/// The backoff bounds made sane: at least 1 ms, `min <= max`.
fn clamp_backoff(min: Duration, max: Duration) -> (Duration, Duration) {
    let max = max.max(Duration::from_millis(1));
    (min.clamp(Duration::from_millis(1), max), max)
}

/// The port and token of the connected daemon. Never printed.
#[derive(Clone)]
struct Endpoint {
    port: u16,
    token: String,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("port", &self.port)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Default)]
struct Shared {
    /// Set while connected (snapshot taken, events subscribed).
    connected: Mutex<Option<(Client, Endpoint)>>,
    last_error: Mutex<Option<(String, LinkErrorKind)>>,
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
    /// `Snapshot`, `ConnectionChanged(Connected)`, then daemon events; after
    /// a loss `ConnectionChanged(Reconnecting)` and a fresh snapshot.
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
    pub fn last_error(&self) -> Option<String> {
        lock(&self.shared.last_error)
            .as_ref()
            .map(|(m, _)| m.clone())
    }

    /// The kind of [`DaemonLink::last_error`].
    pub fn last_error_kind(&self) -> Option<LinkErrorKind> {
        lock(&self.shared.last_error).as_ref().map(|(_, k)| *k)
    }

    /// Whether the link thread has ended (shut down, or nobody listens).
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Port and token of the connected daemon, for the web tabs. Keep the
    /// token out of logs.
    pub fn connection_info(&self) -> Option<(u16, String)> {
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

    /// Adds the repo and tells the UI with `Action::RepoAdded` (repos have no
    /// event). The action is sent from the link's runtime, so it is not lost
    /// when the caller drops the future.
    pub fn add_repo(
        &self,
        path: &Path,
    ) -> impl Future<Output = Result<Repo, LinkError>> + Send + 'static {
        let path = path.to_owned();
        self.cmd.clone().run(move |c, tx| async move {
            let repo = c.add_repo(&path).await?;
            let _ = tx.send(Action::RepoAdded(repo.clone())).await;
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
    Failed(String, LinkErrorKind),
}

fn unreachable(e: impl std::fmt::Display) -> Exit {
    Exit::Failed(
        format!("could not reach the daemon: {e}"),
        LinkErrorKind::Unreachable,
    )
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
            Exit::Failed(msg, kind) => {
                tracing::debug!("daemon link failed: {msg}");
                *lock(&shared.last_error) = Some((msg, kind));
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
        EnsureError::VersionMismatch { .. } => {
            Exit::Failed(e.to_string(), LinkErrorKind::VersionMismatch)
        }
        e => Exit::Failed(
            format!("could not start {}: {e}", config.daemon_bin.display()),
            LinkErrorKind::StartFailed,
        ),
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
        let events = client.events(Some(latest_seq)).await?;
        Ok::<_, ClientError>((latest_seq, repos, agents, events))
    };
    let (latest_seq, repos, agents, mut events) =
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

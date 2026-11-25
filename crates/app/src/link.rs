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
use smithagents_client::{ensure_daemon_with, Client, ClientError, SpawnOptions};
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
    last_error: Mutex<Option<String>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Handle to the link thread. Dropping it stops and joins the thread; the
/// daemon keeps running.
pub struct DaemonLink {
    shared: Arc<Shared>,
    handle: Handle,
    tx: mpsc::Sender<Action>,
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
                            () = run(config, shared, tx) => {}
                        }
                    });
                    // Dropping the runtime cancels command tasks still running.
                })
                .expect("link thread spawns")
        };
        let link = DaemonLink {
            shared,
            handle,
            tx,
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
        lock(&self.shared.last_error).clone()
    }

    /// Port and token of the connected daemon, for the web tabs. Keep the
    /// token out of logs.
    pub fn connection_info(&self) -> Option<(u16, String)> {
        lock(&self.shared.connected)
            .as_ref()
            .map(|(_, e)| (e.port, e.token.clone()))
    }

    pub async fn create_agent(&self, repo_id: RepoId) -> Result<Agent, LinkError> {
        self.command(move |c| async move { c.create_agent(repo_id).await })
            .await
    }

    /// Adds the repo and tells the UI with `Action::RepoAdded` (repos have no
    /// event).
    pub async fn add_repo(&self, path: &Path) -> Result<Repo, LinkError> {
        let path = path.to_owned();
        let repo: Repo = self
            .command(move |c| async move { c.add_repo(&path).await })
            .await?;
        let _ = self.tx.send(Action::RepoAdded(repo.clone())).await;
        Ok(repo)
    }

    pub async fn close_agent(&self, id: AgentId) -> Result<(), LinkError> {
        self.command(move |c| async move { c.close_agent(id).await })
            .await
    }

    /// Runs `f` on the link's runtime, so it works from any executor.
    async fn command<T, F, Fut>(&self, f: F) -> Result<T, LinkError>
    where
        T: Send + 'static,
        F: FnOnce(Client) -> Fut,
        Fut: Future<Output = Result<T, ClientError>> + Send + 'static,
    {
        let client = lock(&self.shared.connected)
            .as_ref()
            .map(|(c, _)| c.clone())
            .ok_or(LinkError::NotConnected)?;
        let task = self.handle.spawn(f(client));
        Ok(task.await.map_err(|_| LinkError::Stopped)??)
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
    Failed(String),
}

impl<E: std::fmt::Display> From<E> for Exit {
    fn from(e: E) -> Self {
        Exit::Failed(e.to_string())
    }
}

async fn run(config: LinkConfig, shared: Arc<Shared>, tx: mpsc::Sender<Action>) {
    let mut delay = config.backoff_min;
    let mut announced = false;
    loop {
        let mut reached = false;
        let exit = session(&config, &shared, &tx, &mut reached).await;
        *lock(&shared.connected) = None;
        if reached {
            announced = false;
            delay = config.backoff_min;
        }
        match exit {
            Exit::ReceiverGone => return,
            Exit::StreamEnded => tracing::debug!("event stream ended; reconnecting"),
            Exit::Failed(msg) => {
                tracing::debug!("daemon link failed: {msg}");
                *lock(&shared.last_error) = Some(msg);
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
        delay = (delay * 2).min(config.backoff_max);
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
    .await?;
    let info = read_lockfile(&config.home)?.ok_or("the daemon's lockfile is gone")?;
    // The seq is read before listing, so an event between the lists and the
    // subscription is replayed rather than missed; the model drops duplicates.
    let latest_seq = client.health().await?.latest_seq;
    let repos = client.list_repos().await?;
    let agents = client.list_agents(false).await?;
    // A 410 here (events too old, or a restarted daemon) is a failure like any
    // other: the next round takes a fresh snapshot.
    let mut events = client.events(Some(latest_seq)).await?;

    *lock(&shared.connected) = Some((
        client,
        Endpoint {
            port: info.port,
            token: info.token,
        },
    ));
    *lock(&shared.last_error) = None;
    send(
        tx,
        Action::Snapshot {
            agents,
            repos,
            latest_seq,
        },
    )
    .await?;
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

//! `DaemonLink` against the real `smithagentsd`.

mod common;

use std::future::Future;
use std::time::Duration;

use common::{daemon_bin, fake_claude, init_repo, TestHome};
use smithagents_app::link::{DaemonLink, LinkConfig, LinkError};
use smithagents_app::model::{Action, Connection};
use smithagents_client::{Client, ClientError, SpawnOptions};
use smithagents_core::daemon::read_lockfile;
use smithagents_core::event::EventKind;
use tokio::sync::mpsc::Receiver;

const WAIT: Duration = Duration::from_secs(20);

fn config(home: &TestHome) -> LinkConfig {
    let mut config = LinkConfig::new(
        home.path().to_owned(),
        daemon_bin().to_owned(),
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    config.spawn = SpawnOptions {
        args: vec![
            "--claude-bin".into(),
            fake_claude().into(),
            "--idle-timeout-secs".into(),
            "120".into(),
        ],
        env: Vec::new(),
    };
    config.backoff_min = Duration::from_millis(50);
    config.backoff_max = Duration::from_millis(200);
    config
}

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

async fn next(rx: &mut Receiver<Action>, what: &str) -> Action {
    within(what, rx.recv())
        .await
        .unwrap_or_else(|| panic!("link closed while waiting for {what}"))
}

/// Receives until `pick` returns something.
async fn until<T>(
    rx: &mut Receiver<Action>,
    what: &str,
    mut pick: impl FnMut(&Action) -> Option<T>,
) -> T {
    within(what, async {
        loop {
            let action = rx.recv().await.expect("link closed");
            if let Some(found) = pick(&action) {
                return found;
            }
        }
    })
    .await
}

fn client_for(home: &TestHome) -> Client {
    Client::new(&read_lockfile(home.path()).unwrap().expect("lockfile"))
}

async fn connected(home: &TestHome) -> (DaemonLink, Receiver<Action>) {
    let (link, mut rx) = DaemonLink::start(config(home));
    assert!(matches!(
        next(&mut rx, "snapshot").await,
        Action::Snapshot { .. }
    ));
    assert_eq!(
        next(&mut rx, "connected").await,
        Action::ConnectionChanged(Connection::Connected)
    );
    (link, rx)
}

#[tokio::test]
async fn snapshot_then_connected() {
    let home = TestHome::new();
    let (link, mut rx) = DaemonLink::start(config(&home));
    match next(&mut rx, "snapshot").await {
        Action::Snapshot { agents, repos, .. } => {
            assert!(agents.is_empty() && repos.is_empty());
        }
        other => panic!("expected a snapshot, got {other:?}"),
    }
    assert_eq!(
        next(&mut rx, "connected").await,
        Action::ConnectionChanged(Connection::Connected)
    );
    let info = link.connection_info().expect("connection info");
    let lock = read_lockfile(home.path()).unwrap().unwrap();
    assert_eq!(info, (lock.port, lock.token));
    assert_eq!(link.last_error(), None);
}

#[tokio::test]
async fn agent_created_elsewhere_arrives_as_event() {
    let home = TestHome::new();
    let (_link, mut rx) = connected(&home).await;
    let repo_dir = home.scratch().join("repo");
    init_repo(&repo_dir);
    let other = client_for(&home);
    let repo = other.add_repo(&repo_dir).await.unwrap();
    let agent = other.create_agent(repo.id).await.unwrap();
    let seen = until(&mut rx, "AgentCreated", |a| match a {
        Action::Event(e) => match &e.kind {
            EventKind::AgentCreated(created) => Some(created.id),
            _ => None,
        },
        _ => None,
    })
    .await;
    assert_eq!(seen, agent.id);
}

#[tokio::test]
async fn killed_daemon_reconnects_with_a_fresh_snapshot() {
    use rustix::process::{kill_process, Pid, Signal};

    let home = TestHome::new();
    let repo_dir = home.scratch().join("repo");
    init_repo(&repo_dir);
    let (link, mut rx) = connected(&home).await;
    let repo = link.add_repo(&repo_dir).await.unwrap();
    let agent = link.create_agent(repo.id).await.unwrap();
    until(&mut rx, "AgentCreated", |a| match a {
        Action::Event(e) if matches!(e.kind, EventKind::AgentCreated(_)) => Some(()),
        _ => None,
    })
    .await;

    let old = read_lockfile(home.path()).unwrap().unwrap();
    kill_process(
        Pid::from_raw(i32::try_from(old.pid).unwrap()).unwrap(),
        Signal::KILL,
    )
    .unwrap();

    until(&mut rx, "Reconnecting", |a| {
        (*a == Action::ConnectionChanged(Connection::Reconnecting)).then_some(())
    })
    .await;
    let agents = until(&mut rx, "fresh snapshot", |a| match a {
        Action::Snapshot { agents, .. } => Some(agents.clone()),
        _ => None,
    })
    .await;
    assert!(agents.iter().any(|a| a.id == agent.id), "{agents:?}");
    assert_eq!(
        next(&mut rx, "connected").await,
        Action::ConnectionChanged(Connection::Connected)
    );
    let new = read_lockfile(home.path()).unwrap().unwrap();
    assert_ne!(new.pid, old.pid, "a new daemon was started");
    assert_eq!(link.connection_info(), Some((new.port, new.token)));
}

#[tokio::test]
async fn add_repo_with_a_missing_path_returns_the_typed_error() {
    let home = TestHome::new();
    let (link, mut rx) = connected(&home).await;
    let missing = home.scratch().join("nope");
    match link.add_repo(&missing).await {
        Err(LinkError::Client(ClientError::Api { code, .. })) => {
            assert_eq!(code, "path_not_found");
        }
        other => panic!("expected path_not_found, got {other:?}"),
    }
    // A good one succeeds and also reaches the model, since repos have no event.
    let repo_dir = home.scratch().join("repo");
    init_repo(&repo_dir);
    let repo = link.add_repo(&repo_dir).await.unwrap();
    let added = until(&mut rx, "RepoAdded", |a| match a {
        Action::RepoAdded(r) => Some(r.id),
        _ => None,
    })
    .await;
    assert_eq!(added, repo.id);
}

#[tokio::test]
async fn close_agent_emits_an_event() {
    let home = TestHome::new();
    let repo_dir = home.scratch().join("repo");
    init_repo(&repo_dir);
    let (link, mut rx) = connected(&home).await;
    let repo = link.add_repo(&repo_dir).await.unwrap();
    let agent = link.create_agent(repo.id).await.unwrap();
    link.close_agent(agent.id).await.unwrap();
    until(&mut rx, "close event", |a| match a {
        Action::Event(e) if matches!(e.kind, EventKind::AgentClosed { .. }) => Some(()),
        _ => None,
    })
    .await;
}

#[tokio::test]
async fn an_unstartable_daemon_stays_reconnecting_with_an_error_and_drop_is_prompt() {
    let home = TestHome::new();
    let mut cfg = config(&home);
    cfg.daemon_bin = home.scratch().join("no-such-daemon");
    let (link, mut rx) = DaemonLink::start(cfg);
    assert_eq!(
        next(&mut rx, "reconnecting").await,
        Action::ConnectionChanged(Connection::Reconnecting)
    );
    within("an error text", async {
        while link.last_error().is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(link.connection_info().is_none());
    assert!(matches!(
        link.create_agent(Default::default()).await,
        Err(LinkError::NotConnected)
    ));
    drop_promptly(link).await;
}

#[tokio::test]
async fn dropping_the_link_ends_its_thread_and_leaves_the_daemon_running() {
    let home = TestHome::new();
    let (link, _rx) = connected(&home).await;
    drop_promptly(link).await;
    let info = read_lockfile(home.path()).unwrap().unwrap();
    assert!(Client::new(&info).health().await.is_ok());
}

async fn drop_promptly(link: DaemonLink) {
    let (done, wait) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(link);
        let _ = done.send(());
    });
    tokio::task::spawn_blocking(move || wait.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .expect("dropping the link hung");
}

//! `DaemonLink` against the real `smithagentsd`.

mod common;

use std::future::Future;
use std::time::Duration;

use common::{daemon_bin, fake_claude, init_repo, TestHome};
use smithagents_app::link::{DaemonLink, LinkConfig, LinkError, LinkErrorKind};
use smithagents_app::model::{Action, Connection};
use smithagents_app::view_models::repo_error_text;
use smithagents_client::{Client, ClientError, SpawnOptions};
use smithagents_core::agent::AgentState;
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

/// Adds `path` expecting the daemon to refuse it with `code`; returns the
/// error and checks the daemon's raw message carries `prefix` (its internal
/// `ServiceError` prefix, which the Settings pane must never show).
async fn refused_add(
    link: &DaemonLink,
    path: &std::path::Path,
    code: &str,
    prefix: &str,
) -> LinkError {
    let err = link
        .add_repo(path)
        .await
        .expect_err("the daemon refuses the add");
    match &err {
        LinkError::Client(ClientError::Api {
            code: got, message, ..
        }) => {
            assert_eq!(got, code, "{message}");
            assert!(message.starts_with(prefix), "{message}");
        }
        other => panic!("expected {code}, got {other:?}"),
    }
    err
}

#[tokio::test]
async fn add_repo_with_a_missing_path_returns_the_typed_error() {
    let home = TestHome::new();
    let (link, mut rx) = connected(&home).await;
    let missing = home.scratch().join("nope");
    let err = refused_add(&link, &missing, "path_not_found", "git: no directory at ").await;
    assert_eq!(repo_error_text(&err), "No directory at that path.");
    // A file is not a directory either.
    let file = home.scratch().join("a file.txt");
    std::fs::write(&file, "not a repository").unwrap();
    let err = refused_add(&link, &file, "path_not_found", "git: no directory at ").await;
    assert_eq!(repo_error_text(&err), "No directory at that path.");
    // A plain directory is not a git repository.
    let plain = home.scratch().join("plain dir ünï");
    std::fs::create_dir(&plain).unwrap();
    let err = refused_add(&link, &plain, "not_a_git_repo", "git: not a git repository").await;
    assert_eq!(
        repo_error_text(&err),
        "That directory is not a git repository."
    );

    // A good one succeeds and also reaches the model, since repos have no event.
    let repo_dir = home.scratch().join("My Repo ünïcode");
    init_repo(&repo_dir);
    let repo = link.add_repo(&repo_dir).await.unwrap();
    let added = until(&mut rx, "RepoAdded", |a| match a {
        Action::RepoAdded(r) => Some(r.id),
        _ => None,
    })
    .await;
    assert_eq!(added, repo.id);
    // Adding it again is refused.
    let err = refused_add(
        &link,
        &repo_dir,
        "repo_exists",
        "store: a repo with this path",
    )
    .await;
    assert_eq!(repo_error_text(&err), "That repository is already added.");
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
    // The error is recorded before `Reconnecting` is sent.
    let error = link.last_error().expect("an error text");
    assert!(error.contains("could not start"), "{error}");
    assert_eq!(link.last_error_kind(), Some(LinkErrorKind::StartFailed));
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

/// Kills the process (and its group) whose pid `file` holds, on drop.
struct KillPidFile(std::path::PathBuf);

impl Drop for KillPidFile {
    fn drop(&mut self) {
        use rustix::process::{kill_process, kill_process_group, Pid, Signal};
        let Some(pid) = std::fs::read_to_string(&self.0)
            .ok()
            .and_then(|t| t.trim().parse::<i32>().ok())
            .and_then(Pid::from_raw)
        else {
            return;
        };
        // The script's `sleep` child would outlive it: kill children first.
        let _ = std::process::Command::new("pkill")
            .args(["-KILL", "-P", &pid.as_raw_nonzero().get().to_string()])
            .status();
        let _ = kill_process_group(pid, Signal::KILL);
        let _ = kill_process(pid, Signal::KILL);
    }
}

#[tokio::test]
async fn a_working_agent_is_dead_in_the_snapshot_after_the_daemon_is_killed() {
    use rustix::process::{kill_process, Pid, Signal};

    let home = TestHome::new();
    let repo_dir = home.scratch().join("repo");
    init_repo(&repo_dir);
    let pid_file = home.scratch().join("claude.pid");
    let _cleanup = KillPidFile(pid_file.clone());
    let mut cfg = config(&home);
    cfg.spawn.env = vec![
        ("FAKE_CLAUDE_SLEEP".into(), "30".into()),
        ("FAKE_CLAUDE_PID_FILE".into(), pid_file.clone().into()),
    ];
    let (_link, mut rx) = DaemonLink::start(cfg);
    until(&mut rx, "connected", |a| {
        (*a == Action::ConnectionChanged(Connection::Connected)).then_some(())
    })
    .await;

    let other = client_for(&home);
    let repo = other.add_repo(&repo_dir).await.unwrap();
    let agent = other.create_agent(repo.id).await.unwrap();
    other.send_message(agent.id, "hello").await.unwrap();
    until(&mut rx, "Working", |a| match a {
        Action::Event(e) => match &e.kind {
            EventKind::AgentUpdated(u) if u.id == agent.id && u.state == AgentState::Working => {
                Some(())
            }
            _ => None,
        },
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
    let after = agents
        .iter()
        .find(|a| a.id == agent.id)
        .expect("agent listed");
    assert_eq!(after.state, AgentState::Dead, "{after:?}");
    assert_eq!(
        next(&mut rx, "connected").await,
        Action::ConnectionChanged(Connection::Connected)
    );
}

#[tokio::test]
async fn a_version_mismatch_is_reported_and_no_second_daemon_starts() {
    let home = TestHome::new();
    let (link, _rx) = connected(&home).await;
    let pid = read_lockfile(home.path()).unwrap().unwrap().pid;
    drop(link);

    let mut cfg = config(&home);
    cfg.version = "0.0.0-other".into();
    let (link, mut rx2) = DaemonLink::start(cfg);
    assert_eq!(
        next(&mut rx2, "reconnecting").await,
        Action::ConnectionChanged(Connection::Reconnecting)
    );
    assert_eq!(link.last_error_kind(), Some(LinkErrorKind::VersionMismatch));
    assert!(link.last_error().unwrap().contains("0.0.0-other"));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(read_lockfile(home.path()).unwrap().unwrap().pid, pid);
}

#[tokio::test]
async fn dropping_the_receiver_stops_the_link() {
    let home = TestHome::new();
    let mut cfg = config(&home);
    cfg.daemon_bin = home.scratch().join("no-such-daemon");
    let (link, mut rx) = DaemonLink::start(cfg);
    next(&mut rx, "reconnecting").await;
    drop(rx);
    within("the link thread to end", async {
        while !link.is_finished() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

#[tokio::test]
async fn dropping_the_link_while_the_daemon_never_becomes_ready_is_prompt() {
    use std::os::unix::fs::PermissionsExt;

    let home = TestHome::new();
    let pid_file = home.scratch().join("sleeper.pid");
    let _cleanup = KillPidFile(pid_file.clone());
    let script = home.scratch().join("sleeper");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho $$ > {}\nexec sleep 60\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = config(&home);
    cfg.daemon_bin = script;
    let (link, _rx) = DaemonLink::start(cfg);
    within("the sleeper to start", async {
        while std::fs::read_to_string(&pid_file).map_or(true, |t| t.trim().is_empty()) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    drop_promptly(link).await;
}

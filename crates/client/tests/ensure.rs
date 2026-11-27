//! `ensure_daemon` and the typed client against the real `smithagentsd`, plus
//! error and event-stream handling against stub servers.

mod common;

use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use futures::{SinkExt, Stream, StreamExt};
use serde_json::json;
use smithagents_client::{
    ensure_daemon, ensure_daemon_with, Client, ClientError, EnsureError, SpawnOptions,
};
use smithagents_core::agent::{AgentState, ChatMessage, Role};
use smithagents_core::daemon::{lockfile_path, read_lockfile, DaemonInfo};
use smithagents_core::event::{Event, EventKind};
use smithagents_core::ids::{AgentId, MessageId};
use smithagents_core::{PROTOCOL, VERSION};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

use common::{claude_fixture, daemon_bin, fake_claude, init_repo, shell_script, TestHome};

/// Bounds each wait in these tests: a hang is a failure, not a stuck run.
const PATIENCE: Duration = Duration::from_secs(30);
/// Bounds each test as a whole.
const TEST_DEADLINE: Duration = Duration::from_secs(60);
/// No process can have this pid: it is above every platform's pid limit.
const DEAD_PID: u32 = i32::MAX as u32;

/// Bounds a whole test body, on top of the finer `within` bounds inside it.
/// The daemon binary is resolved first, so a cold build of it never counts
/// against (and is never reported as) a test running out of time.
async fn bounded<T>(test: impl Future<Output = T>) -> T {
    daemon_bin();
    tokio::time::timeout(TEST_DEADLINE, test)
        .await
        .unwrap_or_else(|_| panic!("the test did not finish within {TEST_DEADLINE:?}"))
}

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(PATIENCE, fut)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish within {PATIENCE:?}"))
}

/// `ensure_daemon` for this crate's version, panicking with the daemon log.
async fn ensure(home: &TestHome) -> Client {
    within(
        "ensure_daemon",
        ensure_daemon(home.path(), daemon_bin(), VERSION),
    )
    .await
    .unwrap_or_else(|e| panic!("ensure_daemon failed: {e}; daemon log:\n{}", home.log()))
}

fn write_lockfile(home: &TestHome, info: &DaemonInfo) {
    let path = lockfile_path(home.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(info).unwrap()).unwrap();
}

/// A loopback port with nothing listening on it.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

async fn next_event(stream: &mut (impl Stream<Item = Event> + Unpin)) -> Option<Event> {
    within("the next event", stream.next()).await
}

// ------------------------------------------------------------ ensure_daemon

#[tokio::test]
async fn ensure_starts_daemon_when_none_running() {
    let home = TestHome::new();
    bounded(async {
        assert_eq!(read_lockfile(home.path()).unwrap(), None);

        let client = ensure(&home).await;
        let health = client.health().await.unwrap();
        let info = read_lockfile(home.path())
            .unwrap()
            .expect("the started daemon wrote its lockfile");
        assert_eq!(health.pid, info.pid);
        assert_eq!(health.version, VERSION);
        assert_eq!(health.running_agents, 0);
        assert_eq!(health.latest_seq, 0);
        assert_eq!(health.instance_id.len(), 16, "{health:?}");
        assert!(health.instance_id.chars().all(|c| c.is_ascii_hexdigit()));

        // Detached into its own process group, so signals aimed at the caller's
        // group (ctrl-c in a terminal) do not reach it.
        let pid = rustix::process::Pid::from_raw(info.pid as i32).unwrap();
        assert_eq!(rustix::process::getpgid(Some(pid)).unwrap(), pid);
        // Its output is appended to run/daemon.log.
        assert!(
            home.log().contains("listening on 127.0.0.1"),
            "daemon log:\n{}",
            home.log()
        );
    })
    .await;
}

#[tokio::test]
async fn ensure_reuses_running_daemon() {
    let home = TestHome::new();
    bounded(async {
        let first = ensure(&home).await.health().await.unwrap();
        let info = read_lockfile(home.path()).unwrap();

        let second = ensure(&home).await.health().await.unwrap();
        assert_eq!(second.pid, first.pid);
        assert_eq!(
            read_lockfile(home.path()).unwrap(),
            info,
            "lockfile rewritten"
        );
    })
    .await;
}

#[tokio::test]
async fn ensure_replaces_stale_lockfile() {
    let home = TestHome::new();
    bounded(async {
        let stale = DaemonInfo {
            pid: DEAD_PID,
            port: closed_port(),
            token: "stale".into(),
            version: VERSION.into(),
        };
        write_lockfile(&home, &stale);

        let health = ensure(&home).await.health().await.unwrap();
        assert_ne!(health.pid, DEAD_PID);
        let info = read_lockfile(home.path()).unwrap().unwrap();
        assert_eq!(info.pid, health.pid);
        assert_ne!(info.token, stale.token);
    })
    .await;
}

#[tokio::test]
async fn ensure_starts_over_unreachable_lockfile_whose_pid_is_alive() {
    // The pid may have been reused by an unrelated process (here: this test),
    // so the lockfile is not deleted, but the new daemon takes over anyway.
    let home = TestHome::new();
    bounded(async {
        let unreachable = DaemonInfo {
            pid: std::process::id(),
            port: closed_port(),
            token: "unreachable".into(),
            version: VERSION.into(),
        };
        write_lockfile(&home, &unreachable);

        let health = ensure(&home).await.health().await.unwrap();
        assert_ne!(health.pid, std::process::id());
        assert_eq!(read_lockfile(home.path()).unwrap().unwrap().pid, health.pid);
    })
    .await;
}

#[tokio::test]
async fn ensure_reports_version_mismatch() {
    let home = TestHome::new();
    bounded(async {
        let client = ensure(&home).await;
        let before = client.health().await.unwrap();

        let err = within(
            "ensure_daemon",
            ensure_daemon(home.path(), daemon_bin(), "0.0.0-other"),
        )
        .await
        .expect_err("a different version must not be accepted");
        match err {
            EnsureError::VersionMismatch { running, expected } => {
                assert_eq!(running, VERSION);
                assert_eq!(expected, "0.0.0-other");
            }
            other => panic!("expected VersionMismatch, got {other:?}"),
        }
        // Nothing was restarted: the caller decides.
        assert_eq!(client.health().await.unwrap().pid, before.pid);
        assert_eq!(read_lockfile(home.path()).unwrap().unwrap().pid, before.pid);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn ensure_concurrent_calls_share_one_daemon() {
    let home = TestHome::new();
    bounded(async {
        let bin = daemon_bin();
        let calls: Vec<_> = (0..8)
            .map(|_| {
                let path = home.path().to_owned();
                tokio::spawn(async move { ensure_daemon(&path, bin, VERSION).await })
            })
            .collect();
        let results = within(
            "8 concurrent ensure_daemon calls",
            futures::future::join_all(calls),
        )
        .await;

        let mut pids = HashSet::new();
        for result in results {
            let client = result
                .unwrap()
                .unwrap_or_else(|e| panic!("ensure_daemon failed: {e}; log:\n{}", home.log()));
            pids.insert(client.health().await.unwrap().pid);
        }
        assert_eq!(pids.len(), 1, "callers reached different daemons: {pids:?}");
        let pid = pids.into_iter().next().unwrap();
        assert_eq!(read_lockfile(home.path()).unwrap().unwrap().pid, pid);
    })
    .await;
}

#[tokio::test]
async fn ensure_does_not_hang_on_an_unresponsive_port() {
    // The kernel completes connections to a bound socket nobody accepts on,
    // so a request to it waits forever without the health timeout.
    let hung = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let home = TestHome::new();
    bounded(async {
        write_lockfile(
            &home,
            &DaemonInfo {
                pid: std::process::id(),
                port: hung.local_addr().unwrap().port(),
                token: "hung".into(),
                version: VERSION.into(),
            },
        );

        let health = ensure(&home).await.health().await.unwrap();
        assert_eq!(read_lockfile(home.path()).unwrap().unwrap().pid, health.pid);
    })
    .await;
}

/// Number of lines `script_starts_line` appended to `file`.
fn starts(file: &std::path::Path) -> usize {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .count()
}

/// A script line recording each start of a fake daemon in `file`.
fn script_starts_line(file: &std::path::Path) -> String {
    format!("echo start >> '{}'", file.display())
}

#[tokio::test]
async fn ensure_deletes_only_a_stale_lockfile() {
    // A daemon binary that fails at once leaves the lockfile as ensure left it.
    let home = TestHome::new();
    bounded(async {
        let failing = shell_script(&home.scratch().join("failing-daemon"), "exit 1");
        let stale = DaemonInfo {
            pid: DEAD_PID,
            port: closed_port(),
            token: "stale".into(),
            version: VERSION.into(),
        };
        write_lockfile(&home, &stale);

        let err = within(
            "ensure_daemon",
            ensure_daemon(home.path(), &failing, VERSION),
        )
        .await
        .expect_err("the daemon binary fails");
        // A start that fails outright is reported at once, not as a timeout.
        assert!(
            matches!(&err, EnsureError::Io(e) if e.to_string().contains("exit status: 1")),
            "{err:?}"
        );
        assert_eq!(
            read_lockfile(home.path()).unwrap(),
            None,
            "stale lockfile kept"
        );

        // A live pid may be a daemon on its way out, or a reused pid: kept.
        let alive = DaemonInfo {
            pid: std::process::id(),
            ..stale
        };
        write_lockfile(&home, &alive);
        within(
            "ensure_daemon",
            ensure_daemon(home.path(), &failing, VERSION),
        )
        .await
        .expect_err("the daemon binary fails");
        assert_eq!(read_lockfile(home.path()).unwrap(), Some(alive));
        // The instance lock file belongs to the daemon alone.
        assert!(!home.path().join("run").join("daemon.lock").exists());
    })
    .await;
}

#[tokio::test]
async fn ensure_creates_the_home_owner_only() {
    // The home (transcripts, worktrees, the lockfile's run/) is created by
    // whoever comes first; here the client, since this daemon fails at once.
    use std::os::unix::fs::PermissionsExt;
    let home = TestHome::new();
    bounded(async {
        let failing = shell_script(&home.scratch().join("failing-daemon"), "exit 1");
        assert!(!home.path().exists());
        within(
            "ensure_daemon",
            ensure_daemon(home.path(), &failing, VERSION),
        )
        .await
        .expect_err("the daemon binary fails");
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(home.path()), 0o700);
        assert_eq!(mode(&home.path().join("run")), 0o700);
    })
    .await;
}

#[tokio::test]
async fn ensure_starts_over_when_the_lock_holder_has_no_lockfile() {
    // The first start loses the instance lock (exit 3) to a daemon that has
    // already removed its lockfile on the way out; a later start wins.
    let home = TestHome::new();
    bounded(async {
        let started = home.scratch().join("starts");
        let lost_once = home.scratch().join("lost-once");
        let script = shell_script(
            &home.scratch().join("daemon"),
            &format!(
                "{}\nif [ ! -e '{lost}' ]; then : > '{lost}'; exit 3; fi\nexec '{bin}' \"$@\"",
                script_starts_line(&started),
                lost = lost_once.display(),
                bin = daemon_bin().display(),
            ),
        );

        let client = within(
            "ensure_daemon",
            ensure_daemon(home.path(), &script, VERSION),
        )
        .await
        .unwrap_or_else(|e| panic!("ensure_daemon failed: {e}; log:\n{}", home.log()));
        let health = client.health().await.unwrap();
        assert_eq!(read_lockfile(home.path()).unwrap().unwrap().pid, health.pid);
        assert_eq!(starts(&started), 2);
    })
    .await;
}

#[tokio::test]
async fn ensure_gives_up_after_three_lost_starts_without_a_lockfile() {
    let home = TestHome::new();
    bounded(async {
        let started = home.scratch().join("starts");
        let script = shell_script(
            &home.scratch().join("daemon"),
            &format!("{}\nexit 3", script_starts_line(&started)),
        );

        let err = within(
            "ensure_daemon",
            ensure_daemon(home.path(), &script, VERSION),
        )
        .await
        .expect_err("no daemon ever publishes a lockfile");
        assert!(matches!(err, EnsureError::StartTimeout), "{err:?}");
        assert_eq!(starts(&started), 3);
    })
    .await;
}

#[tokio::test]
async fn ensure_keeps_waiting_while_the_lock_holder_has_a_lockfile() {
    // Losing the instance lock while a lockfile exists is the normal path for
    // concurrent callers: no new start, just wait for that daemon to answer
    // (this one never does).
    let home = TestHome::new();
    bounded(async {
        write_lockfile(
            &home,
            &DaemonInfo {
                pid: std::process::id(),
                port: closed_port(),
                token: "silent".into(),
                version: VERSION.into(),
            },
        );
        let started = home.scratch().join("starts");
        let script = shell_script(
            &home.scratch().join("daemon"),
            &format!("{}\nexit 3", script_starts_line(&started)),
        );

        let began = std::time::Instant::now();
        let err = within(
            "ensure_daemon",
            ensure_daemon(home.path(), &script, VERSION),
        )
        .await
        .expect_err("the lockfile's daemon never answers");
        assert!(matches!(err, EnsureError::StartTimeout), "{err:?}");
        assert_eq!(starts(&started), 1);
        assert!(
            began.elapsed() >= Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
    })
    .await;
}

/// An HTTP server, not a daemon, that answers every request with `health`
/// as JSON and records each request head it receives.
async fn impostor(health: serde_json::Value) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&heads);
    tokio::spawn(async move {
        let answer = response("200 OK", "application/json", &health.to_string());
        while let Ok((mut socket, _)) = listener.accept().await {
            let Some(head) = read_head(&mut socket).await else {
                continue;
            };
            seen.lock().unwrap().push(head);
            let _ = socket.write_all(answer.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    (port, heads)
}

#[tokio::test]
async fn ensure_never_sends_the_token_to_a_dead_daemons_port() {
    // The lockfile's daemon is dead and another process now listens on its
    // port, answering health the way the daemon would.
    let home = TestHome::new();
    bounded(async {
        let (port, heads) = impostor(json!({
            "version": VERSION, "pid": DEAD_PID, "running_agents": 0, "ws_clients": 0,
        }))
        .await;
        let token = "secret-daemon-token";
        write_lockfile(
            &home,
            &DaemonInfo {
                pid: DEAD_PID,
                port,
                token: token.into(),
                version: VERSION.into(),
            },
        );

        let health = ensure(&home).await.health().await.unwrap();
        assert_ne!(health.pid, DEAD_PID, "the impostor was accepted");
        assert_eq!(read_lockfile(home.path()).unwrap().unwrap().pid, health.pid);
        let heads = heads.lock().unwrap();
        assert!(
            heads.iter().all(|head| {
                !head.to_ascii_lowercase().contains("\r\nauthorization:") && !head.contains(token)
            }),
            "the token was sent to the impostor: {heads:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn ensure_rejects_a_daemon_reporting_another_pid() {
    // The lockfile's pid is alive (here: this test process), but what answers
    // on its port reports another pid: it is not the daemon that wrote it.
    let home = TestHome::new();
    bounded(async {
        let (port, heads) = impostor(json!({
            "version": VERSION, "pid": 1, "running_agents": 0, "ws_clients": 0,
        }))
        .await;
        write_lockfile(
            &home,
            &DaemonInfo {
                pid: std::process::id(),
                port,
                token: "token".into(),
                version: VERSION.into(),
            },
        );

        let health = ensure(&home).await.health().await.unwrap();
        assert_ne!(health.pid, 1, "the impostor was accepted");
        assert_ne!(health.pid, std::process::id());
        assert_eq!(read_lockfile(home.path()).unwrap().unwrap().pid, health.pid);
        // It was asked, so the pid check (not the liveness check) rejected it.
        assert!(!heads.lock().unwrap().is_empty());
    })
    .await;
}

#[tokio::test]
async fn a_daemon_of_the_same_version_speaking_another_protocol_is_a_mismatch() {
    // Every build reports the same version, so the version cannot tell a
    // daemon built from other code. One that answers without `protocol` is
    // from before the field: protocol 0.
    for (answered, running) in [(None, 0), (Some(PROTOCOL + 1), PROTOCOL + 1)] {
        let home = TestHome::new();
        bounded(async {
            let mut health = json!({
                "version": VERSION, "pid": std::process::id(), "running_agents": 0, "ws_clients": 0,
            });
            if let Some(protocol) = answered {
                health["protocol"] = json!(protocol);
            }
            let (port, _heads) = impostor(health).await;
            write_lockfile(
                &home,
                &DaemonInfo {
                    pid: std::process::id(),
                    port,
                    token: "token".into(),
                    version: VERSION.into(),
                },
            );

            let err = within(
                "ensure_daemon",
                ensure_daemon(home.path(), daemon_bin(), VERSION),
            )
            .await
            .expect_err("a daemon speaking another protocol must not be accepted");

            assert_eq!(
                err.to_string(),
                format!(
                    "smithagentsd {VERSION} is running with protocol {running}, \
                     but protocol {PROTOCOL} is expected"
                )
            );
            match err {
                EnsureError::ProtocolMismatch {
                    version,
                    running: found,
                    expected,
                } => {
                    assert_eq!(version, VERSION);
                    assert_eq!(found, running);
                    assert_eq!(expected, PROTOCOL);
                }
                other => panic!("expected ProtocolMismatch, got {other:?}"),
            }
            // Nothing was started in its place: the caller decides.
            assert_eq!(read_lockfile(home.path()).unwrap().unwrap().port, port);
        })
        .await;
    }
}

#[tokio::test]
async fn version_mismatch_is_reported_for_a_health_answer_without_newer_fields() {
    // A daemon of an older version answers without `instance_id` and
    // `latest_seq`; it must still be recognized, as a version mismatch.
    let home = TestHome::new();
    bounded(async {
        let (port, _heads) = impostor(json!({
            "version": "0.0.1-old", "pid": std::process::id(), "running_agents": 0, "ws_clients": 0,
        }))
        .await;
        write_lockfile(
            &home,
            &DaemonInfo {
                pid: std::process::id(),
                port,
                token: "token".into(),
                version: "0.0.1-old".into(),
            },
        );
        let err = within(
            "ensure_daemon",
            ensure_daemon(home.path(), daemon_bin(), VERSION),
        )
        .await
        .expect_err("another version is running");
        assert!(
            matches!(&err, EnsureError::VersionMismatch { running, .. } if running == "0.0.1-old"),
            "{err:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn restarted_daemon_rejects_a_seq_from_before_the_restart() {
    let home = TestHome::new();
    bounded(async {
        let repo_dir = home.scratch().join("repo");
        init_repo(&repo_dir);
        let client = ensure(&home).await;
        let repo = client.add_repo(&repo_dir).await.unwrap();
        client.create_agent(repo.id).await.unwrap();
        client.create_agent(repo.id).await.unwrap();
        let before = client.health().await.unwrap();
        assert_eq!(before.latest_seq, 2, "{before:?}");
        // The latest seq is a valid position on this daemon.
        drop(client.events(Some(before.latest_seq)).await.unwrap());
        drop(client);

        common::stop_daemon(home.path());
        let client = ensure(&home).await;
        let after = client.health().await.unwrap();
        assert_ne!(after.pid, before.pid, "the daemon was not restarted");
        assert_ne!(after.instance_id, before.instance_id);
        assert_eq!(after.latest_seq, 0);

        // Resuming from the old daemon's seq would silently skip events.
        let err = within("events", client.events(Some(before.latest_seq)))
            .await
            .err()
            .expect("a seq from before the restart must be refused");
        let (status, code, message) = api_error(err);
        assert_eq!((status, code.as_str()), (410, "events_too_old"));
        assert!(message.contains("restarted"), "{message}");
        // Subscribing without `since` works.
        let mut events = within("events", client.events(None)).await.unwrap();
        client.create_agent(repo.id).await.unwrap();
        let next = next_event(&mut events).await.expect("stream ended");
        assert_eq!(next.seq, 1);
    })
    .await;
}

// ------------------------------------------------------------------ client

#[tokio::test]
async fn events_stream_yields_agent_created() {
    let home = TestHome::new();
    bounded(async {
        let repo_dir = home.scratch().join("repo");
        init_repo(&repo_dir);
        let client = ensure(&home).await;
        let repo = client.add_repo(&repo_dir).await.unwrap();

        let mut live = client.events(None).await.unwrap();
        let agent = client.create_agent(repo.id).await.unwrap();
        let created = next_event(&mut live).await.expect("stream ended");
        assert_eq!(created.kind, EventKind::AgentCreated(agent.clone()));

        // A later subscriber replays the retained events after `since`.
        let mut replay = client.events(Some(0)).await.unwrap();
        let replayed = next_event(&mut replay).await.expect("stream ended");
        assert_eq!(replayed, created);
    })
    .await;
}

#[tokio::test]
async fn client_covers_the_api() {
    let home = TestHome::new();
    bounded(async {
        let repo_dir = home.scratch().join("repo");
        init_repo(&repo_dir);
        let options = SpawnOptions {
            args: vec!["--claude-bin".into(), fake_claude().into()],
            env: vec![("FAKE_CLAUDE_FIXTURE".into(), claude_fixture().into())],
        };
        let client = within(
            "ensure_daemon_with",
            ensure_daemon_with(home.path(), daemon_bin(), VERSION, &options),
        )
        .await
        .unwrap_or_else(|e| panic!("ensure_daemon_with failed: {e}; log:\n{}", home.log()));

        assert_eq!(client.list_repos().await.unwrap(), vec![]);
        let repo = client.add_repo(&repo_dir).await.unwrap();
        assert_eq!(client.list_repos().await.unwrap(), vec![repo.clone()]);
        let dup = client.add_repo(&repo_dir).await.unwrap_err();
        assert!(
            matches!(&dup, ClientError::Api { status: 409, code, .. } if code == "repo_exists"),
            "{dup:?}"
        );

        let agent = client.create_agent(repo.id).await.unwrap();
        assert_eq!(
            client.list_agents(false).await.unwrap(),
            vec![agent.clone()]
        );

        let mut events = client.events(None).await.unwrap();
        let sent = client.send_message(agent.id, "hello").await.unwrap();
        assert_eq!(
            (sent.agent_id, sent.role, sent.text.as_str()),
            (agent.id, Role::User, "hello")
        );
        // Wait for the fake turn to finish before reading the transcript.
        loop {
            let event = next_event(&mut events).await.expect("stream ended");
            if let EventKind::AgentUpdated(a) = &event.kind {
                if a.id == agent.id && a.state == AgentState::Done {
                    break;
                }
            }
        }
        let transcript = client.messages(agent.id, 0).await.unwrap();
        assert_eq!(transcript.first(), Some(&sent));
        assert!(
            transcript.iter().any(|m| m.role == Role::Assistant),
            "{transcript:?}"
        );
        let after = client.messages(agent.id, sent.seq).await.unwrap();
        assert_eq!(after, transcript[1..]);

        let missing = client.messages(AgentId::new(), 0).await.unwrap_err();
        assert!(
            matches!(&missing, ClientError::Api { status: 404, code, .. } if code == "agent_not_found"),
            "{missing:?}"
        );

        client.close_agent(agent.id).await.unwrap();
        assert_eq!(client.list_agents(false).await.unwrap(), vec![]);
        let all = client.list_agents(true).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, agent.id);
        assert!(all[0].closed_at.is_some());

        let mut wrong = read_lockfile(home.path()).unwrap().unwrap();
        wrong.token = "0".repeat(64).into();
        let denied = Client::new(&wrong).health().await.unwrap_err();
        assert!(
            matches!(&denied, ClientError::Api { status: 401, code, .. } if code == "unauthorized"),
            "{denied:?}"
        );
        // A refused WebSocket upgrade carries the daemon's JSON error too.
        let denied = within("events", Client::new(&wrong).events(None))
            .await
            .err()
            .expect("a wrong token must not subscribe");
        assert!(
            matches!(&denied, ClientError::Api { status: 401, code, .. } if code == "unauthorized"),
            "{denied:?}"
        );
    })
    .await;
}

// ------------------------------------------------------------ stub servers

fn stub_info(port: u16) -> DaemonInfo {
    DaemonInfo {
        pid: DEAD_PID,
        port,
        token: "stub-token".into(),
        version: VERSION.into(),
    }
}

/// Reads an HTTP request head; `None` if the peer goes away first.
async fn read_head(socket: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match socket.read(&mut buf).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    Some(String::from_utf8_lossy(&head).into_owned())
}

/// Serves one HTTP connection per canned response, in order; resolves to the
/// request heads it received.
async fn http_stub(responses: Vec<String>) -> (DaemonInfo, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let served = tokio::spawn(async move {
        let mut heads = Vec::new();
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let head = read_head(&mut socket)
                .await
                .expect("client closed before sending a request");
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
            heads.push(head);
        }
        heads
    });
    (stub_info(port), served)
}

fn response(status: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn api_error(err: ClientError) -> (u16, String, String) {
    match err {
        ClientError::Api {
            status,
            code,
            message,
        } => (status, code, message),
        other => panic!("expected ClientError::Api, got {other:?}"),
    }
}

#[tokio::test]
async fn api_errors_use_the_json_body_or_fall_back_to_the_status() {
    bounded(async {
        let json = r#"{"error":"turn_in_progress","message":"a turn is already running"}"#;
        let (info, served) =
            http_stub(vec![response("409 Conflict", "application/json", json)]).await;
        let err = within("health", Client::new(&info).health())
            .await
            .unwrap_err();
        assert_eq!(
            api_error(err),
            (
                409,
                "turn_in_progress".into(),
                "a turn is already running".into()
            )
        );
        let head = served.await.unwrap().remove(0);
        assert!(head.starts_with("GET /v1/health HTTP/1.1\r\n"), "{head}");
        assert!(
            head.to_ascii_lowercase()
                .contains("\r\nauthorization: bearer stub-token\r\n"),
            "{head}"
        );

        // axum answers unknown routes, wrong methods and oversized bodies in plain text.
        let (info, _served) = http_stub(vec![response(
            "405 Method Not Allowed",
            "text/plain",
            "nope",
        )])
        .await;
        let err = within("health", Client::new(&info).health())
            .await
            .unwrap_err();
        assert_eq!(api_error(err), (405, "http_405".into(), "nope".into()));

        // JSON that is not the error shape is passed through as text too.
        let (info, _served) = http_stub(vec![response(
            "500 Internal Server Error",
            "application/json",
            r#"{"oops":1}"#,
        )])
        .await;
        let err = within("health", Client::new(&info).health())
            .await
            .unwrap_err();
        assert_eq!(
            api_error(err),
            (500, "http_500".into(), r#"{"oops":1}"#.into())
        );
    })
    .await;
}

#[tokio::test]
async fn messages_fetches_every_page() {
    bounded(async {
        let agent = AgentId::new();
        let message = |seq: u64| ChatMessage {
            id: MessageId::new(),
            agent_id: agent,
            seq,
            role: Role::User,
            text: format!("m{seq}"),
            created_at: Utc::now(),
        };
        // 1000 messages is a full page, so another page is asked for.
        let first: Vec<_> = (6..1006).map(message).collect();
        let second = vec![message(1006)];
        let ok = |page: &[ChatMessage]| {
            response(
                "200 OK",
                "application/json",
                &serde_json::to_string(page).unwrap(),
            )
        };
        let (info, served) = http_stub(vec![ok(&first), ok(&second)]).await;

        let all = within("messages", Client::new(&info).messages(agent, 5))
            .await
            .unwrap();
        assert_eq!(all, [first, second].concat());
        let heads = served.await.unwrap();
        let path = format!("/v1/agents/{agent}/messages");
        assert!(
            heads[0].starts_with(&format!("GET {path}?after=5&limit=1000 HTTP/1.1\r\n")),
            "{}",
            heads[0]
        );
        assert!(
            heads[1].starts_with(&format!("GET {path}?after=1005&limit=1000 HTTP/1.1\r\n")),
            "{}",
            heads[1]
        );
    })
    .await;
}

#[tokio::test]
async fn events_too_old_is_an_api_error() {
    bounded(async {
        let json = r#"{"error":"events_too_old","message":"oldest retained seq is 9"}"#;
        let (info, served) = http_stub(vec![response("410 Gone", "application/json", json)]).await;
        let err = within("events", Client::new(&info).events(Some(7)))
            .await
            .err()
            .expect("subscribing must fail");
        assert_eq!(
            api_error(err),
            (
                410,
                "events_too_old".into(),
                "oldest retained seq is 9".into()
            )
        );
        // The token travels in the upgrade request's header, never in the URL.
        let head = served.await.unwrap().remove(0);
        assert!(
            head.starts_with("GET /v1/events?since=7 HTTP/1.1\r\n"),
            "{head}"
        );
        let lower = head.to_ascii_lowercase();
        assert!(
            lower.contains("\r\nauthorization: bearer stub-token\r\n"),
            "{head}"
        );
        assert!(lower.contains("\r\nupgrade: websocket\r\n"), "{head}");
    })
    .await;
}

fn sample_event(seq: u64) -> Event {
    Event {
        seq,
        at: Utc::now(),
        kind: EventKind::AgentClosed {
            agent_id: AgentId::new(),
        },
    }
}

/// Accepts one WebSocket connection and sends `script`, then waits for the
/// client to go away.
async fn ws_stub(script: Vec<Message>) -> DaemonInfo {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
        for message in script {
            if ws.send(message).await.is_err() {
                return;
            }
        }
        while let Some(Ok(_)) = ws.next().await {}
    });
    stub_info(port)
}

#[tokio::test]
async fn lagged_close_ends_the_event_stream() {
    bounded(async {
        let event = sample_event(5);
        let info = ws_stub(vec![
            Message::text(serde_json::to_string(&event).unwrap()),
            Message::Close(Some(CloseFrame {
                code: CloseCode::Again,
                reason: "lagged".into(),
            })),
        ])
        .await;
        let mut stream = within("events", Client::new(&info).events(None))
            .await
            .unwrap();
        assert_eq!(next_event(&mut stream).await, Some(event));
        assert_eq!(next_event(&mut stream).await, None);
    })
    .await;
}

#[tokio::test]
async fn malformed_event_ends_the_event_stream() {
    bounded(async {
        let info = ws_stub(vec![
            Message::text("not an event"),
            Message::text(serde_json::to_string(&sample_event(6)).unwrap()),
        ])
        .await;
        let mut stream = within("events", Client::new(&info).events(Some(5)))
            .await
            .unwrap();
        assert_eq!(next_event(&mut stream).await, None);
    })
    .await;
}

#[tokio::test]
async fn event_of_an_unknown_type_is_skipped() {
    // A newer daemon may send kinds this client does not know yet.
    bounded(async {
        let later = sample_event(6);
        let info = ws_stub(vec![
            Message::text(
                r#"{"seq":5,"at":"2026-11-24T10:00:00Z","kind":{"type":"agent_renamed","data":{"agent_id":"x","name":"Pluto"}}}"#,
            ),
            Message::text(serde_json::to_string(&later).unwrap()),
        ])
        .await;
        let mut stream = within("events", Client::new(&info).events(None))
            .await
            .unwrap();
        assert_eq!(next_event(&mut stream).await, Some(later));
    })
    .await;
}

#[tokio::test]
async fn event_of_a_known_type_with_bad_data_ends_the_event_stream() {
    bounded(async {
        let info = ws_stub(vec![
            Message::text(
                r#"{"seq":5,"at":"2026-11-24T10:00:00Z","kind":{"type":"agent_closed","data":{"oops":1}}}"#,
            ),
            Message::text(serde_json::to_string(&sample_event(6)).unwrap()),
        ])
        .await;
        let mut stream = within("events", Client::new(&info).events(None))
            .await
            .unwrap();
        assert_eq!(next_event(&mut stream).await, None);
    })
    .await;
}

//! Runs the real `smithagentsd` binary against a temporary home.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::Value;
use smithagents_core::agent::{Agent, AgentState, Repo};
use smithagents_core::daemon::{read_lockfile, DaemonInfo};
use smithagents_core::ids::{AgentId, RepoId};
use smithagents_core::palette::palette;
use smithagents_server::paths::Paths;
use smithagents_server::store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const DAEMON: &str = env!("CARGO_BIN_EXE_smithagentsd");
const FAKE_CLAUDE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/bin/fake-claude");
/// Upper bound for anything the daemon should do promptly (start, answer, exit).
const PATIENCE: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(25);

/// A daemon process, killed and reaped on drop so a failing test never leaves
/// one running. Its stdout and stderr go to a log file, shown when a check fails.
struct Daemon {
    child: Child,
    log: PathBuf,
}

impl Daemon {
    /// Starts `smithagentsd --home <home> --claude-bin <fake-claude> <extra>`,
    /// logging to `<dir>/<name>.log`. `SMITHAGENTS_HOME` is set to `env_home`
    /// when given and removed otherwise.
    fn spawn(dir: &Path, name: &str, home: &Path, extra: &[&str], env_home: Option<&Path>) -> Self {
        let log = dir.join(format!("{name}.log"));
        let out = File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let mut cmd = Command::new(DAEMON);
        cmd.arg("--home")
            .arg(home)
            .args(["--claude-bin", FAKE_CLAUDE])
            .args(extra)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        match env_home {
            Some(dir) => cmd.env("SMITHAGENTS_HOME", dir),
            None => cmd.env_remove("SMITHAGENTS_HOME"),
        };
        Daemon {
            child: cmd.spawn().unwrap(),
            log,
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().unwrap()
    }

    async fn wait_exit(&mut self, within: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.exited() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Waits until the lockfile names this daemon, failing fast if it exits.
    async fn wait_for_lockfile(&mut self, home: &Path) -> DaemonInfo {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(info) = read_lockfile(home).unwrap() {
                if info.pid == self.pid() {
                    return info;
                }
            }
            if let Some(status) = self.exited() {
                panic!(
                    "daemon exited with {status} before writing its lockfile; log:\n{}",
                    self.log_text()
                );
            }
            assert!(
                Instant::now() < deadline,
                "no lockfile after {PATIENCE:?}; log:\n{}",
                self.log_text()
            );
            tokio::time::sleep(POLL).await;
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Minimal HTTP/1.1 GET against the daemon; returns status and body.
async fn http_get(port: u16, path: &str, token: Option<&str>) -> (u16, String) {
    let exchange = async {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut req =
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
        if let Some(t) = token {
            req.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).await.unwrap();
        raw
    };
    let raw = tokio::time::timeout(PATIENCE, exchange)
        .await
        .expect("daemon did not answer");
    let (head, body) = raw.split_once("\r\n\r\n").expect("malformed response");
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("malformed status line");
    (status, body.to_owned())
}

#[tokio::test]
async fn daemon_writes_lockfile_and_serves_health() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let env_home = tmp.path().join("env-home");
    let mut daemon = Daemon::spawn(tmp.path(), "daemon", &home, &[], Some(&env_home));
    let info = daemon.wait_for_lockfile(&home).await;

    assert_eq!(info.version, smithagents_core::VERSION);
    assert_ne!(info.port, 0);
    assert_eq!(info.token.len(), 64);
    assert!(info.token.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(!env_home.exists(), "--home must win over SMITHAGENTS_HOME");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&home.join("run").join("daemon.json")), 0o600);
        assert_eq!(mode(&home.join("run")), 0o700);
        // Chat transcripts live in the database: owner only, like the home.
        assert_eq!(mode(&home), 0o700);
        for name in ["smithagents.db", "smithagents.db-wal", "smithagents.db-shm"] {
            let file = home.join(name);
            assert!(file.exists(), "no {name}");
            assert_eq!(mode(&file), 0o600, "{name}");
        }
    }

    assert_eq!(http_get(info.port, "/v1/health", None).await.0, 401);
    let wrong = "0".repeat(64);
    assert_eq!(http_get(info.port, "/v1/health", Some(&wrong)).await.0, 401);
    let (status, body) = http_get(info.port, "/v1/health", Some(&info.token)).await;
    assert_eq!(status, 200, "{body}");
    let health: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(health["pid"], daemon.pid());
    assert_eq!(health["version"], smithagents_core::VERSION);
    assert_eq!(health["running_agents"], 0);
    assert_eq!(health["latest_seq"], 0);
    let instance_id = health["instance_id"].as_str().unwrap();
    assert_eq!(instance_id.len(), 16, "{health}");
    assert!(instance_id.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(!info.token.contains(instance_id), "derived from the token");

    assert!(daemon.exited().is_none(), "daemon stopped serving");
    assert!(
        !daemon.log_text().contains(&info.token),
        "the token must never be logged"
    );
}

#[tokio::test]
async fn concurrent_start_single_instance() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mut a = Daemon::spawn(tmp.path(), "a", &home, &[], None);
    let mut b = Daemon::spawn(tmp.path(), "b", &home, &[], None);

    let deadline = Instant::now() + PATIENCE;
    let (status, loser, mut winner) = loop {
        if let Some(status) = a.exited() {
            break (status, a, b);
        }
        if let Some(status) = b.exited() {
            break (status, b, a);
        }
        assert!(
            Instant::now() < deadline,
            "both daemons still running; logs:\n{}\n{}",
            a.log_text(),
            b.log_text()
        );
        tokio::time::sleep(POLL).await;
    };
    assert_eq!(status.code(), Some(3), "loser log:\n{}", loser.log_text());
    assert!(
        loser.log_text().contains("already running"),
        "loser log:\n{}",
        loser.log_text()
    );

    let info = winner.wait_for_lockfile(&home).await;
    let (code, _) = http_get(info.port, "/v1/health", Some(&info.token)).await;
    assert_eq!(code, 200);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        winner.exited().is_none(),
        "the winner must keep running; log:\n{}",
        winner.log_text()
    );
    assert_eq!(
        read_lockfile(&home).unwrap().map(|i| i.pid),
        Some(winner.pid())
    );
}

#[tokio::test]
async fn daemon_exits_and_removes_lockfile_when_idle() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let started = Instant::now();
    let mut daemon = Daemon::spawn(
        tmp.path(),
        "daemon",
        &home,
        &["--idle-timeout-secs", "1"],
        None,
    );
    daemon.wait_for_lockfile(&home).await;

    let status = daemon
        .wait_exit(PATIENCE)
        .await
        .unwrap_or_else(|| panic!("idle daemon kept running; log:\n{}", daemon.log_text()));
    assert_eq!(status.code(), Some(0), "log:\n{}", daemon.log_text());
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(read_lockfile(&home).unwrap(), None);
}

#[tokio::test]
async fn connected_websocket_client_keeps_idle_daemon_alive() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mut daemon = Daemon::spawn(
        tmp.path(),
        "daemon",
        &home,
        &["--idle-timeout-secs", "2"],
        None,
    );
    let info = daemon.wait_for_lockfile(&home).await;
    let url = format!(
        "ws://127.0.0.1:{}/v1/events?token={}",
        info.port, info.token
    );
    let (ws, _) = tokio::time::timeout(PATIENCE, tokio_tungstenite::connect_async(url))
        .await
        .expect("websocket connect timed out")
        .unwrap();

    // Well past the idle timeout.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        daemon.exited().is_none(),
        "daemon exited with a client connected; log:\n{}",
        daemon.log_text()
    );
    let (_, body) = http_get(info.port, "/v1/health", Some(&info.token)).await;
    let health: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(health["ws_clients"], 1);

    drop(ws);
    let status = daemon.wait_exit(PATIENCE).await.unwrap_or_else(|| {
        panic!(
            "daemon kept running after its only client left; log:\n{}",
            daemon.log_text()
        )
    });
    assert_eq!(status.code(), Some(0), "log:\n{}", daemon.log_text());
    assert_eq!(read_lockfile(&home).unwrap(), None);
}

#[tokio::test]
async fn daemon_exits_and_removes_lockfile_on_sigterm() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mut daemon = Daemon::spawn(tmp.path(), "daemon", &home, &[], None);
    daemon.wait_for_lockfile(&home).await;

    let sent = Command::new("kill")
        .args(["-TERM", &daemon.pid().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
    let status = daemon
        .wait_exit(PATIENCE)
        .await
        .unwrap_or_else(|| panic!("daemon ignored SIGTERM; log:\n{}", daemon.log_text()));
    assert_eq!(status.code(), Some(0), "log:\n{}", daemon.log_text());
    assert_eq!(read_lockfile(&home).unwrap(), None);
}

#[tokio::test]
async fn unknown_flag_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mut daemon = Daemon::spawn(tmp.path(), "daemon", &home, &["--bogus"], None);
    let status = daemon
        .wait_exit(PATIENCE)
        .await
        .expect("daemon started despite an unknown flag");
    assert_eq!(status.code(), Some(2), "log:\n{}", daemon.log_text());
    let log = daemon.log_text();
    assert!(
        log.contains("--bogus") && log.contains("usage:"),
        "log:\n{log}"
    );
    assert!(!home.exists(), "nothing is created on a usage error");
}

#[tokio::test]
async fn agents_left_running_by_a_previous_daemon_start_dead() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let paths = Paths { home: home.clone() };
    paths.ensure_dirs().unwrap();
    let repo = Repo {
        id: RepoId::new(),
        path: tmp.path().join("repo"),
        default_branch: "main".into(),
        created_at: Utc::now(),
    };
    let agent = Agent {
        id: AgentId::new(),
        name: "Mercury".into(),
        color: palette()[0],
        repo_id: repo.id,
        worktree_path: tmp.path().join("wt"),
        branch: "agent/mercury-00000000".into(),
        harness_session_id: Some("s1".into()),
        state: AgentState::Working,
        created_at: Utc::now(),
        closed_at: None,
    };
    {
        let store = Store::open(&paths.db_path()).await.unwrap();
        store.insert_repo(&repo).await.unwrap();
        store.insert_agent(&agent).await.unwrap();
    }

    let mut daemon = Daemon::spawn(tmp.path(), "daemon", &home, &[], None);
    let info = daemon.wait_for_lockfile(&home).await;
    let (status, body) = http_get(info.port, "/v1/agents", Some(&info.token)).await;
    assert_eq!(status, 200, "{body}");
    let agents: Vec<Agent> = serde_json::from_str(&body).unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, agent.id);
    assert_eq!(agents[0].state, AgentState::Dead);
    assert_eq!(agents[0].harness_session_id.as_deref(), Some("s1"));
}

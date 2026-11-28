//! Runs the real `smithagentsd` binary against a temporary home.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::Value;
use smithagents_core::agent::{Agent, AgentState, Repo, SourceControl};
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
    assert_eq!(info.token.expose().len(), 64);
    assert!(info.token.expose().chars().all(|c| c.is_ascii_hexdigit()));
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
    let (status, body) = http_get(info.port, "/v1/health", Some(info.token.expose())).await;
    assert_eq!(status, 200, "{body}");
    let health: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(health["pid"], daemon.pid());
    assert_eq!(health["version"], smithagents_core::VERSION);
    assert_eq!(health["protocol"], smithagents_core::PROTOCOL);
    assert_eq!(health["running_agents"], 0);
    assert_eq!(health["latest_seq"], 0);
    let instance_id = health["instance_id"].as_str().unwrap();
    assert_eq!(instance_id.len(), 16, "{health}");
    assert!(instance_id.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(
        !info.token.expose().contains(instance_id),
        "derived from the token"
    );

    assert!(daemon.exited().is_none(), "daemon stopped serving");
    assert!(
        !daemon.log_text().contains(info.token.expose()),
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
    let (code, _) = http_get(info.port, "/v1/health", Some(info.token.expose())).await;
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
        info.port,
        info.token.expose()
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
    let (_, body) = http_get(info.port, "/v1/health", Some(info.token.expose())).await;
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
async fn feedback_leftovers_are_removed_when_the_daemon_starts() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let paths = Paths { home: home.clone() };
    let upload = paths.feedback_tmp_dir().join("x").join("upload");
    std::fs::create_dir_all(&upload).unwrap();
    std::fs::write(upload.join("file-1"), b"left by a crashed run").unwrap();

    let mut daemon = Daemon::spawn(tmp.path(), "daemon", &home, &[], None);
    daemon.wait_for_lockfile(&home).await;

    let left: Vec<_> = std::fs::read_dir(paths.feedback_tmp_dir())
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}; log:\n{}", daemon.log_text());
}

/// A `git` for the daemon's `PATH` that, on `git commit`, touches `marker`
/// and waits `pause` before running the real git.
fn slow_commit_git(bin: &Path, marker: &Path, pause: Duration) {
    let real = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_string();
    assert!(real.starts_with('/'), "no git found: {real:?}");
    std::fs::create_dir_all(bin).unwrap();
    let script = format!(
        "#!/bin/sh\nfor a in \"$@\"; do\n  if [ \"$a\" = commit ]; then : > '{}'; sleep {}; break; fi\ndone\nexec '{real}' \"$@\"\n",
        marker.display(),
        pause.as_secs()
    );
    let path = bin.join("git");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// The daemon exits for idleness only once a submission whose client went
/// away has finished: its branch, its record and its intent all exist.
#[tokio::test]
async fn a_running_submission_keeps_an_idle_daemon_up() {
    use smithagents_core::feedback::{FeedbackKind, FeedbackRequest, SendAs, SubmissionId};

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let repo_dir = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.name", "Test User"],
        &["config", "user.email", "test@example.invalid"],
        &["commit", "-q", "--allow-empty", "-m", "init"],
    ] {
        let ok = Command::new("git")
            .args(args)
            .current_dir(&repo_dir)
            .status()
            .unwrap();
        assert!(ok.success(), "git {args:?}");
    }
    let bin = tmp.path().join("bin");
    let marker = tmp.path().join("committing");
    slow_commit_git(&bin, &marker, Duration::from_secs(3));
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let log = tmp.path().join("daemon.log");
    let out = File::create(&log).unwrap();
    let mut daemon = Daemon {
        child: Command::new(DAEMON)
            .arg("--home")
            .arg(&home)
            .args(["--claude-bin", FAKE_CLAUDE, "--idle-timeout-secs", "1"])
            .env("PATH", path)
            .env_remove("SMITHAGENTS_HOME")
            .stdin(Stdio::null())
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .unwrap(),
        log,
    };
    let info = daemon.wait_for_lockfile(&home).await;
    let client = smithagents_client::Client::new(&info);
    // A connected client keeps the daemon up while the test sets up.
    let events = client.events(None).await.unwrap();
    let repo = client.add_repo(&repo_dir).await.unwrap();
    let request = FeedbackRequest {
        submission_id: SubmissionId::new(),
        main_idea: "Fix the zoom".into(),
        kind: FeedbackKind::Bug,
        repo_id: repo.id,
        send_as: SendAs::Intent,
        idea: None,
        app_version: "0.1.0".into(),
        platform: "macos".into(),
        attachments: vec![],
        public_acknowledged: false,
    };
    let submit = {
        let (client, request) = (client.clone(), request.clone());
        tokio::spawn(async move { client.submit_feedback(&request, &[]).await })
    };
    let deadline = Instant::now() + PATIENCE;
    while !marker.exists() {
        assert!(
            Instant::now() < deadline,
            "the commit never began; log:\n{}",
            daemon.log_text()
        );
        tokio::time::sleep(POLL).await;
    }
    // Every client goes away while the commit waits.
    submit.abort();
    let _ = submit.await;
    drop(events);

    let status = daemon
        .wait_exit(PATIENCE)
        .await
        .unwrap_or_else(|| panic!("the daemon never went idle; log:\n{}", daemon.log_text()));
    assert_eq!(status.code(), Some(0), "log:\n{}", daemon.log_text());
    let store = Store::open(&Paths { home }.db_path()).await.unwrap();
    let row = store
        .get_submission(request.submission_id)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("no record; log:\n{}", daemon.log_text()));
    assert!(
        row.intent_id.is_some(),
        "no intent; log:\n{}",
        daemon.log_text()
    );
    let tip = Command::new("git")
        .args(["rev-parse", &format!("refs/heads/{}", row.branch)])
        .current_dir(&repo_dir)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&tip.stdout).trim(), row.commit);
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

#[cfg(unix)]
#[tokio::test]
async fn a_home_that_is_not_utf8_is_refused_at_startup() {
    // Worktree paths under it could not be written as JSON, in events or
    // answers, so the daemon does not start there at all.
    use std::os::unix::ffi::OsStringExt;

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp
        .path()
        .join(std::ffi::OsString::from_vec(b"home-\xff".to_vec()));
    let mut daemon = Daemon::spawn(tmp.path(), "daemon", &home, &[], None);
    let status = daemon
        .wait_exit(PATIENCE)
        .await
        .expect("the daemon started in a home whose path is not UTF-8");
    assert_eq!(status.code(), Some(1), "log:\n{}", daemon.log_text());
    let log = daemon.log_text();
    assert!(
        log.contains("the home directory must be a UTF-8 path"),
        "log:\n{log}"
    );
    assert!(!home.exists(), "nothing is created");
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
        source_control: Some(SourceControl::Git {
            default_branch: "main".into(),
        }),
        created_at: Utc::now(),
    };
    let agent = Agent {
        id: AgentId::new(),
        name: "Mercury".into(),
        color: palette()[0],
        repo_id: repo.id,
        worktree_path: tmp.path().join("wt"),
        branch: Some("agent/mercury-00000000".into()),
        harness_session_id: Some("s1".into()),
        state: AgentState::Working,
        created_at: Utc::now(),
        closed_at: None,
        intent_id: None,
    };
    {
        let store = Store::open(&paths.db_path()).await.unwrap();
        store.insert_repo(&repo).await.unwrap();
        store.insert_agent(&agent).await.unwrap();
    }

    let mut daemon = Daemon::spawn(tmp.path(), "daemon", &home, &[], None);
    let info = daemon.wait_for_lockfile(&home).await;
    let (status, body) = http_get(info.port, "/v1/agents", Some(info.token.expose())).await;
    assert_eq!(status, 200, "{body}");
    let agents: Vec<Agent> = serde_json::from_str(&body).unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, agent.id);
    assert_eq!(agents[0].state, AgentState::Dead);
    assert_eq!(agents[0].harness_session_id.as_deref(), Some("s1"));
}

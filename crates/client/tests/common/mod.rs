//! Helpers shared by the client integration tests: the daemon binary, a
//! temporary home that stops its daemon on drop, and git fixtures.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use smithagents_core::daemon::read_lockfile;

/// How long a stopped daemon gets to remove its lockfile before SIGKILL. Covers
/// the daemon's 5 s shutdown grace.
const STOP_PATIENCE: Duration = Duration::from_secs(10);

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/client sits two levels below the workspace root")
}

/// Builds `smithagentsd` once per test process and returns its path.
///
/// `CARGO_BIN_EXE_*` only exists for binaries of the package under test, and
/// the daemon lives in `smithagents-server`, so the tests build it themselves.
pub fn daemon_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let root = workspace_root();
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| env!("CARGO").into());
        let out = Command::new(cargo)
            .args([
                "build",
                "--quiet",
                "-p",
                "smithagents-server",
                "--bin",
                "smithagentsd",
            ])
            .current_dir(root)
            .output()
            .expect("could not run cargo");
        assert!(
            out.status.success(),
            "building smithagentsd failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // A relative CARGO_TARGET_DIR is relative to where cargo ran: the root.
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(|dir| root.join(dir))
            .unwrap_or_else(|| root.join("target"));
        let bin = target
            .join("debug")
            .join(format!("smithagentsd{}", std::env::consts::EXE_SUFFIX));
        assert!(bin.is_file(), "no daemon binary at {}", bin.display());
        bin
    })
}

/// `crates/server/tests/bin/fake-claude`, the test double for the claude CLI.
pub fn fake_claude() -> PathBuf {
    workspace_root().join("crates/server/tests/bin/fake-claude")
}

/// A claude stream-json transcript that fake-claude replays.
pub fn claude_fixture() -> PathBuf {
    workspace_root().join("crates/server/tests/fixtures/claude_stream_hello.jsonl")
}

/// A hand-written transcript of a turn that calls a tool between text blocks.
pub fn claude_tool_fixture() -> PathBuf {
    workspace_root().join("crates/server/tests/fixtures/claude_stream_tool_use.jsonl")
}

/// Writes an executable `sh` script, `body` after the shebang line.
pub fn shell_script(path: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_owned()
}

/// A unique temporary SmithAgents home. Dropping it stops the daemon its
/// lockfile names before the directory is deleted, so a failing test never
/// leaves a daemon running.
pub struct TestHome {
    home: PathBuf,
    tmp: tempfile::TempDir,
}

impl TestHome {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        Self { home, tmp }
    }

    /// The home directory (not created until a daemon starts).
    pub fn path(&self) -> &Path {
        &self.home
    }

    /// Scratch space next to the home, for fixtures such as git repos.
    pub fn scratch(&self) -> &Path {
        self.tmp.path()
    }

    /// The daemon's stdout and stderr, as `ensure_daemon` redirects them.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.home.join("run").join("daemon.log")).unwrap_or_default()
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        stop_daemon(&self.home);
    }
}

/// Stops the daemon named by `home`'s lockfile: SIGTERM, wait for it to remove
/// its lockfile, SIGKILL if it does not.
pub fn stop_daemon(home: &Path) {
    use rustix::process::{kill_process, Pid, Signal};

    let Ok(Some(info)) = read_lockfile(home) else {
        return;
    };
    // A test may plant a lockfile naming this very process.
    if info.pid == std::process::id() {
        return;
    }
    let Some(pid) = i32::try_from(info.pid).ok().and_then(Pid::from_raw) else {
        return;
    };
    if kill_process(pid, Signal::TERM).is_err() {
        // Already gone (or a planted pid that never existed).
        return;
    }
    let deadline = Instant::now() + STOP_PATIENCE;
    while Instant::now() < deadline {
        let current = read_lockfile(home).ok().flatten().map(|i| i.pid);
        if current != Some(info.pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = kill_process(pid, Signal::KILL);
}

/// Whether a process with this pid exists, via `kill(pid, 0)` as the client
/// crate checks it. A child of this process that exited but was not reaped yet
/// (tokio reaps the daemons `ensure_daemon` started while the runtime runs)
/// still counts as alive.
pub fn pid_alive(pid: u32) -> bool {
    use rustix::io::Errno;
    use rustix::process::{test_kill_process, Pid};

    let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return false;
    };
    // EPERM means it exists but belongs to someone else.
    !matches!(test_kill_process(pid), Err(Errno::SRCH))
}

/// Runs git with a fixed identity and C locale, panicking on failure; returns
/// its trimmed stdout.
pub fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(cwd)
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Creates a git repo on `main` with one empty commit.
pub fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["commit", "--allow-empty", "-m", "init"]);
}

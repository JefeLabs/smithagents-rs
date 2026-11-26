//! Helpers for the integration tests: the daemon binary, a temporary home
//! that stops its daemon on drop, and a git fixture (the link tests), plus
//! [`ui`] for the headless shell tests and [`intents`] for the intent tests.
#![allow(dead_code)]

pub mod intents;
pub mod ui;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use smithagents_core::daemon::read_lockfile;

/// How long a stopped daemon gets to remove its lockfile before SIGKILL.
const STOP_PATIENCE: Duration = Duration::from_secs(10);

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/app sits two levels below the workspace root")
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

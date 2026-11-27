//! Find-or-start: connect to the daemon named by the lockfile, or start one.
//!
//! The daemon's instance lock (`run/daemon.lock`, which only the daemon ever
//! touches) decides which process runs: starting a daemon is always safe,
//! because a second one for the same home exits with code 3 before doing
//! anything. So `ensure_daemon` starts one whenever the lockfile does not lead
//! to a healthy daemon, and then waits for whichever daemon won to publish a
//! healthy lockfile.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use smithagents_core::daemon::{lockfile_path, read_lockfile, DaemonInfo};
use tokio::process::{Child, Command};
use tokio::time::Instant;

use crate::api::Client;

/// Lockfile polling interval while a daemon starts.
const POLL: Duration = Duration::from_millis(50);
/// How long one find-or-start waits for a healthy daemon, however many
/// starts it takes (see [`Started::Retry`]).
const START_DEADLINE: Duration = Duration::from_secs(5);
/// A health check slower than this counts as no answer.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(1);
/// After losing the instance lock, how long to wait for the winner's lockfile
/// before starting over.
const LOSER_SETTLE: Duration = Duration::from_secs(1);
/// `smithagentsd` exit code when another daemon holds the instance lock.
const EXIT_ALREADY_RUNNING: i32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum EnsureError {
    /// A daemon of another version is running. It is left alone: the caller
    /// asks the user before restarting it.
    #[error("smithagentsd {running} is running, but {expected} is expected")]
    VersionMismatch { running: String, expected: String },
    /// A daemon of the expected version is running, built from code that
    /// speaks another wire protocol (`smithagents_core::PROTOCOL`). It is
    /// left alone, like one of another version.
    #[error(
        "smithagentsd {version} is running with protocol {running}, but protocol {expected} is expected"
    )]
    ProtocolMismatch {
        version: String,
        running: u32,
        expected: u32,
    },
    #[error("smithagentsd did not become ready in time; see run/daemon.log in the home directory")]
    StartTimeout,
    /// A file, a folder or the daemon binary could not be used. `what` is
    /// what was tried, as the message starts ("could not read"), and `path`
    /// is what it was tried on: the lockfile, the home or its `run/` folder,
    /// the daemon's log, or the daemon binary.
    #[error("{what} {}: {source}", path.display())]
    Io {
        what: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// The started daemon exited before it was ready, and not because another
    /// daemon holds the instance lock. Its output is in `log`.
    #[error(
        "{} exited with {status} before it was ready; see {}",
        bin.display(),
        log.display()
    )]
    Exited {
        bin: PathBuf,
        status: ExitStatus,
        log: PathBuf,
    },
}

/// For `map_err`: the [`EnsureError::Io`] of `what` failing on `path`.
fn io_error<'a>(what: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> EnsureError + 'a {
    move |source| EnsureError::Io {
        what,
        path: path.to_owned(),
        source,
    }
}

/// Extra settings for a daemon that [`ensure_daemon_with`] starts. They do not
/// apply to a daemon that is already running.
#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    /// Arguments passed after `--home <home>`.
    pub args: Vec<OsString>,
    /// Environment variables added to the daemon's environment.
    pub env: Vec<(OsString, OsString)>,
}

/// Returns a client for the daemon of `home`, starting `daemon_bin` if none is
/// running. See [`ensure_daemon_with`].
pub async fn ensure_daemon(
    home: &Path,
    daemon_bin: &Path,
    expected_version: &str,
) -> Result<Client, EnsureError> {
    ensure_daemon_with(home, daemon_bin, expected_version, &SpawnOptions::default()).await
}

/// Returns a client for the daemon of `home`, starting one if none is running.
///
/// - A lockfile whose daemon answers with the lockfile's pid: its client, or
///   [`EnsureError::VersionMismatch`] when it runs another version, or
///   [`EnsureError::ProtocolMismatch`] when it is of this version but speaks
///   another wire protocol.
/// - A lockfile whose pid is dead is stale: it is deleted and its port is
///   never sent the token. One whose pid is alive but whose port does not
///   answer, or answers with another pid, is kept but not trusted (the pid may
///   be a daemon that is shutting down, or a reused pid); the started daemon
///   replaces it if it wins.
/// - Start: `daemon_bin --home <home> <options.args>` in its own process group,
///   stdin null, stdout and stderr appended to `<home>/run/daemon.log`. The
///   lockfile is then polled every 50 ms until it names a daemon that
///   answers. Concurrent callers all end up on the daemon that won the
///   instance lock: the others exit with code 3 and their callers keep
///   polling. When a loser exits and no lockfile appears within a second (the
///   winner is shutting down, or still starting), the find-or-start is done
///   again.
/// - One deadline covers the whole call: 5 s after it began, whatever it is
///   then waiting for, it gives up with [`EnsureError::StartTimeout`]. A
///   daemon it started is left to finish starting, so a later call finds it.
///
/// The started daemon is never waited on after this returns and is not killed
/// when the returned client or this future is dropped.
pub async fn ensure_daemon_with(
    home: &Path,
    daemon_bin: &Path,
    expected_version: &str,
    options: &SpawnOptions,
) -> Result<Client, EnsureError> {
    let deadline = Instant::now() + START_DEADLINE;
    let log = log_path(home);
    loop {
        if let Some(client) = connect_existing(home, expected_version).await? {
            return Ok(client);
        }
        let child = spawn_daemon(home, daemon_bin, options, &log)?;
        match wait_until_ready(home, expected_version, child, daemon_bin, &log, deadline).await? {
            Started::Ready(client) => return Ok(client),
            Started::Retry if Instant::now() < deadline => {}
            Started::Retry => return Err(EnsureError::StartTimeout),
        }
    }
}

/// The daemon the lockfile names, if it answers. A lockfile whose pid is dead
/// is stale: it is deleted, and its port is never contacted.
async fn connect_existing(home: &Path, expected: &str) -> Result<Option<Client>, EnsureError> {
    let Some(info) = lockfile(home)? else {
        return Ok(None);
    };
    if !pid_alive(info.pid) {
        remove_stale_lockfile(home, &info);
        return Ok(None);
    }
    connect(&info, expected).await
}

/// The lockfile, or `None` when it is absent or unreadable as JSON (the daemon
/// writes it atomically, so a corrupt one is garbage a new daemon overwrites).
fn lockfile(home: &Path) -> Result<Option<DaemonInfo>, EnsureError> {
    match read_lockfile(home) {
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            tracing::debug!("ignoring an unparseable lockfile: {e}");
            Ok(None)
        }
        other => other.map_err(io_error("could not read", &lockfile_path(home))),
    }
}

/// A client for `info` if its daemon answers in time; an error if it answers
/// with another version or another wire protocol.
///
/// The health request carries the token, so it is only sent while the
/// lockfile's pid is alive: once that daemon is gone, any local process may
/// hold its port. An answer from another pid is not the daemon that wrote the
/// lockfile and is not trusted. (This narrows, but does not close, the window
/// in which an impostor on a live daemon's port sees the token; a challenge
/// on the health check would close it.)
async fn connect(info: &DaemonInfo, expected: &str) -> Result<Option<Client>, EnsureError> {
    if !pid_alive(info.pid) {
        return Ok(None);
    }
    let client = Client::new(info);
    match client.health_within(HEALTH_TIMEOUT).await {
        Ok(health) if health.pid != info.pid => {
            // Debug, not warn: it repeats on every poll until a new daemon
            // replaces the lockfile.
            tracing::debug!(
                lockfile_pid = info.pid,
                answering_pid = health.pid,
                port = info.port,
                "the lockfile's port is answered by another process; not trusting it"
            );
            Ok(None)
        }
        Ok(health) if health.version != expected => Err(EnsureError::VersionMismatch {
            running: health.version,
            expected: expected.to_owned(),
        }),
        Ok(health) if health.protocol != smithagents_core::PROTOCOL => {
            Err(EnsureError::ProtocolMismatch {
                version: health.version,
                running: health.protocol,
                expected: smithagents_core::PROTOCOL,
            })
        }
        Ok(_) => Ok(Some(client)),
        Err(e) => {
            tracing::debug!(
                pid = info.pid,
                port = info.port,
                "daemon did not answer: {e}"
            );
            Ok(None)
        }
    }
}

/// Deletes the lockfile if it still holds `stale`, so a lockfile written
/// meanwhile by a newly started daemon is kept. (The gap between this check
/// and the unlink is the one window in which a fresh lockfile could be lost;
/// its daemon then stays unreachable and the next attempts report
/// `StartTimeout` rather than start a second daemon.) Only `run/daemon.json`
/// is ever deleted; the instance lock file is the daemon's alone.
fn remove_stale_lockfile(home: &Path, stale: &DaemonInfo) {
    match lockfile(home) {
        Ok(Some(current)) if current == *stale => {}
        _ => return,
    }
    match fs::remove_file(lockfile_path(home)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            // Not fatal: the started daemon overwrites it.
            tracing::warn!("could not remove the stale lockfile: {e}");
        }
        _ => tracing::debug!(pid = stale.pid, "removed a stale lockfile"),
    }
}

/// Whether a process with this pid exists, via `kill(pid, 0)`. Only a
/// definite "no such process" counts as dead, so a lockfile is never deleted
/// on a guess.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    use rustix::io::Errno;
    use rustix::process::{test_kill_process, Pid};

    // 0 or beyond pid_t is not a pid any daemon could have had.
    let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return false;
    };
    // EPERM means it exists but belongs to someone else.
    !matches!(test_kill_process(pid), Err(Errno::SRCH))
}

/// Without a way to ask, assume the process exists: never delete the lockfile.
#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

fn log_path(home: &Path) -> PathBuf {
    lockfile_path(home).with_file_name("daemon.log")
}

/// Starts the daemon detached: its own process group (a terminal's ctrl-c to
/// the caller's group does not reach it), stdin null, output to the log.
///
/// The returned `Child` does not kill the daemon when dropped, and tokio reaps
/// it in the background once it exits, so no zombie is left behind.
fn spawn_daemon(
    home: &Path,
    daemon_bin: &Path,
    options: &SpawnOptions,
    log: &Path,
) -> Result<Child, EnsureError> {
    let out = open_log(log)?;
    let err = out.try_clone().map_err(io_error("could not open", log))?;
    let mut cmd = Command::new(daemon_bin);
    cmd.arg("--home")
        .arg(home)
        .args(&options.args)
        .envs(options.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .kill_on_drop(false);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.spawn().map_err(io_error("could not start", daemon_bin))
}

/// Opens the log for appending (0600), creating the home and `run/` (both
/// 0700) if needed. The daemon tightens existing ones itself.
fn open_log(log: &Path) -> Result<File, EnsureError> {
    const CREATE: &str = "could not create";
    let run = log.parent().expect("the log lives in run/");
    if let Some(home) = run.parent() {
        if let Some(parent) = home.parent() {
            fs::create_dir_all(parent).map_err(io_error(CREATE, parent))?;
        }
        create_private_dir(home).map_err(io_error(CREATE, home))?;
    }
    create_private_dir(run).map_err(io_error(CREATE, run))?;
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(log).map_err(io_error("could not open", log))
}

/// Creates `dir` with mode 0700 unless it exists; its parent must exist.
fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
        _ => Ok(()),
    }
}

enum Started {
    Ready(Client),
    /// The started daemon lost the instance lock to a daemon that has no
    /// lockfile and published none since: it is shutting down, or still
    /// starting. Starting over finds the lock free once it is gone, or the
    /// lockfile once it is written; the caller does so until its deadline.
    Retry,
}

/// Polls the lockfile until it names a daemon that answers, until `child`
/// fails, or until `deadline` (that of the whole find-or-start).
async fn wait_until_ready(
    home: &Path,
    expected: &str,
    mut child: Child,
    daemon_bin: &Path,
    log: &Path,
    deadline: Instant,
) -> Result<Started, EnsureError> {
    let mut lost = false;
    let mut absent_since: Option<Instant> = None;
    loop {
        let current = lockfile(home)?;
        if let Some(info) = &current {
            if let Some(client) = connect(info, expected).await? {
                return Ok(Started::Ready(client));
            }
        }
        if !lost {
            let exited = child
                .try_wait()
                .map_err(io_error("could not wait for", daemon_bin))?;
            if let Some(status) = exited {
                if status.code() != Some(EXIT_ALREADY_RUNNING) {
                    return Err(EnsureError::Exited {
                        bin: daemon_bin.to_owned(),
                        status,
                        log: log.to_owned(),
                    });
                }
                // Another daemon holds the instance lock: wait for its lockfile.
                lost = true;
            }
        }
        let now = Instant::now();
        if lost && current.is_none() {
            let since = *absent_since.get_or_insert(now);
            if now - since >= LOSER_SETTLE || now >= deadline {
                return Ok(Started::Retry);
            }
        } else {
            absent_since = None;
            if now >= deadline {
                return Err(EnsureError::StartTimeout);
            }
        }
        tokio::time::sleep(POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_that_cannot_be_created_is_named() {
        // A file where the home's parent belongs, so nothing below it can be
        // made, whoever runs the test.
        let tmp = tempfile::tempdir().unwrap();
        let blocked = tmp.path().join("a-file");
        fs::write(&blocked, b"").unwrap();
        let log = log_path(&blocked.join("home"));

        let err = open_log(&log).expect_err("the home cannot be created");

        assert!(
            matches!(&err, EnsureError::Io { path, .. } if *path == blocked),
            "{err:?}"
        );
        let text = err.to_string();
        assert!(
            text.starts_with(&format!("could not create {}: ", blocked.display())),
            "{text}"
        );
    }
}

//! Daemon process lifecycle: the single-instance lock, the lockfile clients
//! read to find the daemon, and idle exit.
//!
//! Only the process holding the instance lock writes or removes the lockfile,
//! so two daemons never race on it.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rand::rngs::SysRng;
use rand::TryRng;
use smithagents_core::daemon::{lockfile_path, read_lockfile, DaemonInfo};
use tokio::time::Instant;

use crate::paths::{ensure_private_dir, Paths};

const INSTANCE_LOCK_FILE: &str = "daemon.lock";
const TOKEN_BYTES: usize = 32;
const INSTANCE_ID_BYTES: usize = 8;
const MIN_IDLE_SAMPLE: Duration = Duration::from_millis(10);
const MAX_IDLE_SAMPLE: Duration = Duration::from_secs(1);

/// Writes `<home>/run/daemon.json` atomically: the JSON goes to a fresh
/// owner-only (0600) temp file that is then renamed over the lockfile, so a
/// reader sees the old file or the new one, never a partial write. Creates
/// `run/` with mode 0700 if needed.
pub fn write_lockfile(paths: &Paths, info: &DaemonInfo) -> io::Result<()> {
    ensure_run_dir(paths)?;
    let path = lockfile_path(&paths.home);
    let tmp = path.with_extension("json.tmp");
    // A temp file left by a crash may have any mode; always start from a new one.
    match fs::remove_file(&tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut file = opts.open(&tmp)?;
    // The umask can only clear bits, but make the mode independent of it.
    #[cfg(unix)]
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    file.write_all(&serde_json::to_vec(info)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, &path)
}

/// Removes the lockfile if it still names this process; a lockfile written by
/// another process is left alone. Failures are logged, not returned.
pub fn remove_lockfile(paths: &Paths) {
    match read_lockfile(&paths.home) {
        Ok(Some(info)) if info.pid == std::process::id() => {
            match fs::remove_file(lockfile_path(&paths.home)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => {
                    tracing::warn!("could not remove the lockfile: {e}");
                }
                _ => {}
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("could not read the lockfile to remove it: {e}"),
    }
}

/// A fresh API token: 32 bytes from the operating system's CSPRNG, as 64
/// lowercase hex digits.
pub fn generate_token() -> io::Result<String> {
    random_hex(TOKEN_BYTES)
}

/// A fresh id for this daemon process (8 random bytes as 16 hex digits),
/// reported by `/v1/health`. Event sequence numbers restart with every
/// daemon; a changed id tells a client that its last seen seq is void.
pub fn generate_instance_id() -> io::Result<String> {
    random_hex(INSTANCE_ID_BYTES)
}

/// `len` bytes from the operating system's CSPRNG as lowercase hex.
fn random_hex(len: usize) -> io::Result<String> {
    let mut bytes = vec![0u8; len];
    SysRng
        .try_fill_bytes(&mut bytes)
        .map_err(io::Error::other)?;
    Ok(bytes
        .iter()
        .fold(String::with_capacity(len * 2), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        }))
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another smithagentsd is already running for this home")]
    AlreadyRunning,
    #[error("could not take the instance lock: {0}")]
    Io(#[from] io::Error),
}

/// Proof that this process is the only daemon for its home. The lock is held
/// until this value is dropped (or the process exits, which releases it even
/// after a crash).
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

/// Takes an exclusive, non-blocking lock on `<home>/run/daemon.lock`, creating
/// `run/` with mode 0700 if needed.
pub fn acquire_instance_lock(paths: &Paths) -> Result<InstanceLock, LockError> {
    let dir = ensure_run_dir(paths)?;
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let file = opts.open(dir.join(INSTANCE_LOCK_FILE))?;
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(InstanceLock { _file: file }),
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Err(LockError::AlreadyRunning)
        }
        Err(e) => Err(e.into()),
    }
}

/// Creates the home and `run/` in it (mode 0700 both), tightening existing
/// ones to 0700.
fn ensure_run_dir(paths: &Paths) -> io::Result<PathBuf> {
    paths.ensure_home()?;
    let dir = paths.run_dir();
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// Decides when an idle daemon should exit: once it has had no WebSocket
/// clients and no running agents for a whole `timeout`. Any activity restarts
/// the countdown.
///
/// Activity is sampled every tenth of the timeout (clamped to 10 ms..=1 s), so
/// a burst shorter than one sample interval may go unseen.
pub struct IdleMonitor<F> {
    timeout: Duration,
    ws_clients: Arc<AtomicUsize>,
    running: F,
}

impl<F: Fn() -> usize> IdleMonitor<F> {
    /// `running` reports the number of agents with a turn in flight.
    pub fn new(timeout: Duration, ws_clients: Arc<AtomicUsize>, running: F) -> Self {
        Self {
            timeout,
            ws_clients,
            running,
        }
    }

    fn active(&self) -> bool {
        self.ws_clients.load(Ordering::SeqCst) > 0 || (self.running)() > 0
    }

    /// Resolves once the daemon has been idle for the whole timeout.
    pub async fn wait_idle_exit(self) {
        let sample = (self.timeout / 10).clamp(MIN_IDLE_SAMPLE, MAX_IDLE_SAMPLE);
        let mut idle_since: Option<Instant> = None;
        loop {
            let now = Instant::now();
            if self.active() {
                idle_since = None;
            } else if now - *idle_since.get_or_insert(now) >= self.timeout {
                return;
            }
            tokio::time::sleep(sample).await;
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::path::Path;
    use std::sync::atomic::Ordering;

    use smithagents_core::daemon::{lockfile_path, read_lockfile};
    use tokio::time::{timeout, Instant};

    use super::*;

    const IDLE: Duration = Duration::from_millis(200);
    /// Far longer than any expected wait; only guards against a hang.
    const HANG_GUARD: Duration = Duration::from_secs(60);

    fn temp_paths() -> (tempfile::TempDir, Paths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths {
            home: tmp.path().join("home"),
        };
        (tmp, paths)
    }

    fn info(pid: u32, token: &str) -> DaemonInfo {
        DaemonInfo {
            pid,
            port: 4242,
            token: token.into(),
            version: smithagents_core::VERSION.into(),
        }
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn lockfile_is_0600_and_dir_is_0700() {
        let (_tmp, paths) = temp_paths();
        write_lockfile(&paths, &info(1, "secret")).unwrap();
        assert_eq!(mode(&lockfile_path(&paths.home)), 0o600);
        assert_eq!(mode(&paths.run_dir()), 0o700);

        // The instance lock, taken first at startup, creates the run dir the
        // same way, and the home it lives in too.
        let (_tmp, paths) = temp_paths();
        let _lock = acquire_instance_lock(&paths).unwrap();
        assert_eq!(mode(&paths.run_dir()), 0o700);
        assert_eq!(mode(&paths.home), 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn lockfile_tightens_existing_run_dir_and_stale_temp_file() {
        let (_tmp, paths) = temp_paths();
        std::fs::create_dir_all(paths.run_dir()).unwrap();
        std::fs::set_permissions(paths.run_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
        // A temp file left by a crash, readable by everyone.
        let stale = paths.run_dir().join("daemon.json.tmp");
        std::fs::write(&stale, "junk").unwrap();
        std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_lockfile(&paths, &info(1, "secret")).unwrap();
        assert_eq!(mode(&paths.run_dir()), 0o700);
        assert_eq!(mode(&lockfile_path(&paths.home)), 0o600);
        assert!(!stale.exists());
    }

    #[test]
    fn lockfile_write_is_atomic_round_trip() {
        let (_tmp, paths) = temp_paths();
        let first = info(1, "one");
        write_lockfile(&paths, &first).unwrap();
        assert_eq!(read_lockfile(&paths.home).unwrap(), Some(first));

        let second = info(2, "two");
        write_lockfile(&paths, &second).unwrap();
        assert_eq!(read_lockfile(&paths.home).unwrap(), Some(second));
        // Only the final file remains: the temp file was renamed into place.
        let names: Vec<_> = std::fs::read_dir(paths.run_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["daemon.json"]);
    }

    #[test]
    fn remove_lockfile_only_removes_this_processes_lockfile() {
        let (_tmp, paths) = temp_paths();
        let other = info(std::process::id().wrapping_add(1), "other");
        write_lockfile(&paths, &other).unwrap();
        remove_lockfile(&paths);
        assert_eq!(read_lockfile(&paths.home).unwrap(), Some(other));

        write_lockfile(&paths, &info(std::process::id(), "mine")).unwrap();
        remove_lockfile(&paths);
        assert_eq!(read_lockfile(&paths.home).unwrap(), None);
        // Removing again is harmless.
        remove_lockfile(&paths);
    }

    #[test]
    fn second_instance_lock_fails_already_running() {
        let (_tmp, paths) = temp_paths();
        let first = acquire_instance_lock(&paths).unwrap();
        let err = acquire_instance_lock(&paths).unwrap_err();
        assert!(matches!(err, LockError::AlreadyRunning), "{err:?}");

        drop(first);
        acquire_instance_lock(&paths).expect("lock is free once the holder drops it");
    }

    #[test]
    fn token_is_64_hex_chars_and_unique() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(a, b);
    }

    #[test]
    fn instance_id_is_16_hex_chars_and_unique() {
        let a = generate_instance_id().unwrap();
        let b = generate_instance_id().unwrap();
        assert_eq!(a.len(), 16);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(a, b);
    }

    struct Activity {
        ws_clients: Arc<AtomicUsize>,
        running: Arc<AtomicUsize>,
    }

    fn monitor() -> (Activity, IdleMonitor<impl Fn() -> usize>) {
        let ws_clients = Arc::new(AtomicUsize::new(0));
        let running = Arc::new(AtomicUsize::new(0));
        let r = Arc::clone(&running);
        let monitor = IdleMonitor::new(IDLE, Arc::clone(&ws_clients), move || {
            r.load(Ordering::SeqCst)
        });
        (
            Activity {
                ws_clients,
                running,
            },
            monitor,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn idle_monitor_exits_after_timeout_with_no_clients_or_agents() {
        let (_activity, monitor) = monitor();
        let start = Instant::now();
        timeout(HANG_GUARD, monitor.wait_idle_exit())
            .await
            .expect("idle monitor never fired");
        let waited = start.elapsed();
        assert!(waited >= IDLE, "fired after {waited:?}, before the timeout");
        assert!(waited < IDLE * 2, "fired late, after {waited:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn idle_monitor_waits_while_ws_client_connected() {
        let (activity, monitor) = monitor();
        activity.ws_clients.store(1, Ordering::SeqCst);
        let idle = monitor.wait_idle_exit();
        tokio::pin!(idle);
        assert!(
            timeout(IDLE * 5, &mut idle).await.is_err(),
            "exited with a client connected"
        );

        activity.ws_clients.store(0, Ordering::SeqCst);
        let left = Instant::now();
        timeout(HANG_GUARD, &mut idle)
            .await
            .expect("idle monitor never fired after the client left");
        assert!(left.elapsed() >= IDLE, "fired {:?} after", left.elapsed());
    }

    #[tokio::test(start_paused = true)]
    async fn idle_monitor_waits_while_agent_running() {
        let (activity, monitor) = monitor();
        activity.running.store(2, Ordering::SeqCst);
        let idle = monitor.wait_idle_exit();
        tokio::pin!(idle);
        assert!(
            timeout(IDLE * 5, &mut idle).await.is_err(),
            "exited with agents running"
        );

        activity.running.store(0, Ordering::SeqCst);
        let finished = Instant::now();
        timeout(HANG_GUARD, &mut idle)
            .await
            .expect("idle monitor never fired after the agents finished");
        assert!(
            finished.elapsed() >= IDLE,
            "fired {:?} after",
            finished.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn idle_monitor_resets_after_activity() {
        let (activity, monitor) = monitor();
        let idle = monitor.wait_idle_exit();
        tokio::pin!(idle);
        // Idle for most of the timeout, then a short burst of activity.
        assert!(timeout(IDLE * 3 / 4, &mut idle).await.is_err());
        activity.running.store(1, Ordering::SeqCst);
        assert!(timeout(IDLE / 4, &mut idle).await.is_err());
        activity.running.store(0, Ordering::SeqCst);

        // The full timeout starts over from the end of the activity.
        let quiet = Instant::now();
        timeout(HANG_GUARD, &mut idle)
            .await
            .expect("idle monitor never fired after the activity ended");
        assert!(quiet.elapsed() >= IDLE, "fired {:?} after", quiet.elapsed());
    }
}

//! Command-line tools a built-in plugin depends on: finding them, checking
//! their version, installing one through mise when the user asks, and
//! running one for the daemon's own code.
//!
//! The daemon runs these programs; a plugin never does. Each is found by its
//! bare name in a fixed list of absolute directories (never through a shell,
//! never relative to the working directory) and run with [`run_command`]:
//! stdin closed, stderr discarded, a timeout, and only the exit status in
//! errors.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use smithagents_plugin_protocol::ToolDecl;

use crate::secrets::{run_command, RunError, Stdout, COMMAND_TIMEOUT};

/// Where mise's own installation is explained. The daemon never installs
/// mise itself.
pub const MISE_INSTALL_URL: &str = "https://mise.jdx.dev";
/// The longest `mise use` may run.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolState {
    /// Found at `path`. `version` is read only when the declaration sets a
    /// `min_version`, and is then at least that.
    Ready {
        path: PathBuf,
        version: Option<semver::Version>,
    },
    Missing {
        reason: MissingReason,
    },
    /// Found, but older than the declaration's `min_version`.
    TooOld {
        found: semver::Version,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingReason {
    /// No executable of that name in the search path.
    NotFound,
    /// Found, but a `min_version` is set and `<name> --version` printed no
    /// version (or failed), so it cannot be said to be new enough.
    UnreadableVersion,
}

/// Why [`ToolFinder::install`] did not install the tool. Carries nothing
/// mise printed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    /// The declaration has no valid `install.mise` spec.
    #[error("the tool has no mise package to install")]
    NoMiseSpec,
    #[error("mise is not installed (see {install_url})")]
    MiseMissing { install_url: String },
    /// `status` is mise's exit code; `None` when it could not be started or
    /// was ended by a signal.
    #[error("mise failed ({})", exit_text(.status))]
    Failed { status: Option<i32> },
    #[error("mise did not finish within 10 minutes")]
    Timeout,
}

fn exit_text(status: &Option<i32>) -> String {
    match status {
        Some(code) => format!("exit status {code}"),
        None => "no exit status".to_string(),
    }
}

/// One run of a tool by [`ToolFinder::run`]: `<tool> <args>` in `cwd`, with
/// `env` added to the daemon's environment (any `SMITHAGENTS_*` variable,
/// the daemon's or one given here, is left out), stopped after `timeout` on
/// tokio's clock.
#[derive(Debug, Clone, Copy)]
pub struct ToolRun<'a> {
    pub args: &'a [String],
    pub cwd: &'a Path,
    pub env: &'a [(&'a str, &'a str)],
    pub timeout: Duration,
}

/// Why [`ToolFinder::run`] has no answer. Carries nothing the tool printed:
/// its messages are fixed sentences with an exit status at most.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolRunError {
    /// No usable executable of that name in the search path.
    #[error("the tool is not installed")]
    NotFound,
    /// It could not be started (or its output read, or its end awaited).
    #[error("the tool could not be started")]
    NotStarted,
    #[error("the tool did not finish in time")]
    Timeout,
    /// It printed more than 64 KiB.
    #[error("the tool printed more than 64 KiB")]
    TooMuchOutput,
    /// `status` is its exit code; `None` when it was ended by a signal.
    #[error("the tool failed ({})", exit_text(.status))]
    Failed { status: Option<i32> },
}

/// Finds tools in a fixed list of directories. The daemon uses
/// [`ToolFinder::default`]; tests give their own directories.
#[derive(Debug, Clone)]
pub struct ToolFinder {
    dirs: Vec<PathBuf>,
}

impl Default for ToolFinder {
    /// `PATH`, then mise's shims directory, `/opt/homebrew/bin`,
    /// `/usr/local/bin` and `~/.local/bin`: the places a tool the user
    /// installed is found even when the daemon was started without the
    /// shell's `PATH`.
    fn default() -> ToolFinder {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        ToolFinder::new(default_dirs(
            var("PATH"),
            var("HOME").map(PathBuf::from),
            var("MISE_DATA_DIR").map(PathBuf::from),
            var("XDG_DATA_HOME").map(PathBuf::from),
        ))
    }
}

/// The default search path, in order. mise keeps its shims in
/// `<data dir>/shims`, the data directory being `MISE_DATA_DIR`, else
/// `$XDG_DATA_HOME/mise`, else `~/.local/share/mise` (mise's documentation,
/// "Directories" and "Shims").
fn default_dirs(
    path: Option<OsString>,
    home: Option<PathBuf>,
    mise_data_dir: Option<PathBuf>,
    xdg_data_home: Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let mise_data = mise_data_dir
        .or_else(|| xdg_data_home.map(|d| d.join("mise")))
        .or_else(|| home.as_ref().map(|h| h.join(".local/share/mise")));
    dirs.extend(mise_data.map(|d| d.join("shims")));
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.extend(home.map(|h| h.join(".local/bin")));
    dirs
}

impl ToolFinder {
    /// Searches `dirs` in order. Relative directories are dropped: they
    /// would depend on the daemon's working directory.
    pub fn new(dirs: Vec<PathBuf>) -> ToolFinder {
        let mut kept: Vec<PathBuf> = Vec::with_capacity(dirs.len());
        for dir in dirs {
            if dir.is_absolute() && !kept.contains(&dir) {
                kept.push(dir);
            }
        }
        ToolFinder { dirs: kept }
    }

    /// The directories searched, in order.
    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    /// The first `<dir>/<name>` that is a regular executable file not
    /// writable by others. `name` must be a bare tool name as the manifest
    /// allows (so never a path). The path is returned as found, not with its
    /// links resolved: a shim decides what to run from the name it was
    /// called by.
    pub fn find(&self, name: &str) -> Option<PathBuf> {
        if !ToolDecl::is_valid_name(name) {
            return None;
        }
        self.dirs
            .iter()
            .filter(|dir| is_trusted_dir(dir))
            .map(|dir| dir.join(name))
            .find(|path| is_trusted_executable(path))
    }

    /// Whether the tool is there and, when the declaration sets a
    /// `min_version`, new enough: compared with the first version in the
    /// first line `<name> --version` prints. Without a `min_version` the tool
    /// is not run.
    pub async fn check(&self, decl: &ToolDecl) -> ToolState {
        let Some(path) = self.find(&decl.name) else {
            return ToolState::Missing {
                reason: MissingReason::NotFound,
            };
        };
        let Some(min) = &decl.min_version else {
            return ToolState::Ready {
                path,
                version: None,
            };
        };
        let args = ["--version".to_string()];
        let line = run_command(
            tool_command(&path, &args),
            COMMAND_TIMEOUT,
            Stdout::FirstLine,
        )
        .await
        .ok()
        .flatten();
        match line.as_deref().and_then(first_version) {
            None => ToolState::Missing {
                reason: MissingReason::UnreadableVersion,
            },
            Some(found) if found < *min => ToolState::TooOld { found },
            Some(found) => ToolState::Ready {
                path,
                version: Some(found),
            },
        }
    }

    /// Runs `mise use --global <spec>` for the declaration's `install.mise`
    /// spec (installing the tool and adding it to the user's global mise
    /// configuration, so it is on the user's `PATH` too), then checks the
    /// tool again. mise's output is discarded; it may run for 10 minutes.
    /// Run only when the user asks for it.
    pub async fn install(&self, decl: &ToolDecl) -> Result<ToolState, InstallError> {
        let spec = decl
            .install
            .get("mise")
            .filter(|spec| ToolDecl::is_valid_spec(spec))
            .ok_or(InstallError::NoMiseSpec)?;
        let mise = self.find("mise").ok_or_else(|| InstallError::MiseMissing {
            install_url: MISE_INSTALL_URL.to_string(),
        })?;
        let args = ["use".to_string(), "--global".to_string(), spec.clone()];
        match run_command(tool_command(&mise, &args), INSTALL_TIMEOUT, Stdout::Discard).await {
            Ok(_) => Ok(self.check(decl).await),
            Err(RunError::Status(status)) => Err(InstallError::Failed {
                status: status.code(),
            }),
            Err(RunError::Spawn(e)) => {
                // The kind only: the system's message may name paths.
                tracing::warn!(kind = ?e.kind(), "mise could not be started");
                Err(InstallError::Failed { status: None })
            }
            Err(RunError::Timeout) => Err(InstallError::Timeout),
            // Its output is not read.
            Err(RunError::TooMuchOutput) => Err(InstallError::Failed { status: None }),
        }
    }

    /// Runs `<name> <args>` in `run.cwd` as [`run_command`] runs every
    /// command, with `run.env` added; the answer is the first line it
    /// printed, trimmed (`None` when that is empty). `name` is found as
    /// [`ToolFinder::find`] finds it. The caller decides which tools may run:
    /// the daemon allows only those a built-in plugin declares.
    pub async fn run(&self, name: &str, run: ToolRun<'_>) -> Result<Option<String>, ToolRunError> {
        let path = self.find(name).ok_or(ToolRunError::NotFound)?;
        let mut cmd = tokio::process::Command::new(&path);
        cmd.args(run.args).current_dir(run.cwd);
        for (key, value) in run.env {
            // `run_command` removes the daemon's own; this keeps a caller
            // from adding one.
            if !key.starts_with("SMITHAGENTS_") {
                cmd.env(key, value);
            }
        }
        match run_command(cmd, run.timeout, Stdout::FirstLine).await {
            Ok(line) => Ok(line),
            Err(RunError::Status(status)) => Err(ToolRunError::Failed {
                status: status.code(),
            }),
            Err(RunError::Spawn(e)) => {
                // The kind only: the system's message may name paths.
                tracing::warn!(tool = %name, kind = ?e.kind(), "a tool could not be started");
                Err(ToolRunError::NotStarted)
            }
            Err(RunError::Timeout) => Err(ToolRunError::Timeout),
            Err(RunError::TooMuchOutput) => Err(ToolRunError::TooMuchOutput),
        }
    }
}

/// Whether the tool is installed; see [`ToolFinder::find`].
pub fn find_tool(name: &str) -> Option<PathBuf> {
    ToolFinder::default().find(name)
}

/// See [`ToolFinder::check`].
pub async fn check_tool(decl: &ToolDecl) -> ToolState {
    ToolFinder::default().check(decl).await
}

/// See [`ToolFinder::install`].
pub async fn install_tool(decl: &ToolDecl) -> Result<ToolState, InstallError> {
    ToolFinder::default().install(decl).await
}

/// `path` with `args`, run from the file system root: a tool (mise above
/// all, which reads the configuration files of the directory it runs in)
/// must not depend on whichever directory the daemon was started from.
pub(crate) fn tool_command(path: &Path, args: &[String]) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(path);
    cmd.args(args);
    #[cfg(unix)]
    cmd.current_dir("/");
    cmd
}

// What the daemon runs must not be something another user could have put
// there: a search directory others can write (or a group can, unless the
// current user or root owns it) is skipped, and an executable must belong to
// the current user or root and be writable by neither its group nor others.
// Homebrew's `/opt/homebrew/bin` (the user's) and `/usr/local/bin` (root's)
// pass. Unix only: other platforms have no such permission bits here.

/// A search directory to use: not world-writable, and group-writable only
/// when root or the current user owns it.
#[cfg(unix)]
fn dir_mode_ok(mode: u32, owner: u32, me: u32) -> bool {
    mode & 0o002 == 0 && (mode & 0o020 == 0 || owner == 0 || owner == me)
}

/// An executable to run: owned by root or the current user, executable, and
/// writable by neither its group nor others.
#[cfg(unix)]
fn exe_mode_ok(mode: u32, owner: u32, me: u32) -> bool {
    (owner == 0 || owner == me) && mode & 0o111 != 0 && mode & 0o022 == 0
}

#[cfg(unix)]
fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[cfg(unix)]
fn is_trusted_dir(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(dir)
        .is_ok_and(|meta| meta.is_dir() && dir_mode_ok(meta.mode(), meta.uid(), effective_uid()))
}

#[cfg(unix)]
fn is_trusted_executable(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    // Follows links: brew's and mise's commands are links; their targets are
    // what runs.
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && exe_mode_ok(meta.mode(), meta.uid(), effective_uid()))
}

#[cfg(not(unix))]
fn is_trusted_dir(dir: &Path) -> bool {
    dir.is_dir()
}

#[cfg(not(unix))]
fn is_trusted_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

/// The first word of `line` that is a semantic version, with an optional
/// leading `v` and surrounding punctuation ignored: `gh version 2.41.0
/// (2024-01-09)` gives 2.41.0.
fn first_version(line: &str) -> Option<semver::Version> {
    line.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')'))
        .map(|word| word.strip_prefix('v').unwrap_or(word))
        .find_map(|word| semver::Version::parse(word).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_search_path_is_path_then_the_usual_install_places() {
        let path = std::env::join_paths(["/usr/bin", "/bin"]).unwrap();
        let dirs = default_dirs(Some(path), Some("/home/u".into()), None, None);
        let want: Vec<PathBuf> = [
            "/usr/bin",
            "/bin",
            "/home/u/.local/share/mise/shims",
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/home/u/.local/bin",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        assert_eq!(dirs, want);
    }

    #[test]
    fn mise_data_dir_then_xdg_data_home_move_the_shims() {
        let shims = |mise: Option<&str>, xdg: Option<&str>| {
            default_dirs(
                None,
                Some("/home/u".into()),
                mise.map(Into::into),
                xdg.map(Into::into),
            )[0]
            .clone()
        };
        assert_eq!(
            shims(Some("/data/mise"), Some("/xdg")),
            PathBuf::from("/data/mise/shims")
        );
        assert_eq!(shims(None, Some("/xdg")), PathBuf::from("/xdg/mise/shims"));
        assert_eq!(
            shims(None, None),
            PathBuf::from("/home/u/.local/share/mise/shims")
        );
    }

    #[test]
    fn relative_entries_of_the_default_path_are_dropped() {
        let path = std::env::join_paths(["", ".", "bin", "/usr/bin"]).unwrap();
        let finder = ToolFinder::new(default_dirs(Some(path), None, Some("rel".into()), None));
        assert_eq!(
            finder.dirs(),
            ["/usr/bin", "/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_search_directory_others_could_fill_is_skipped() {
        let (me, other, root) = (501, 502, 0);
        // Not writable by group or others: anyone's.
        for owner in [me, other, root] {
            assert!(dir_mode_ok(0o755, owner, me));
        }
        // Group-writable: only root's or the current user's.
        assert!(dir_mode_ok(0o775, me, me));
        assert!(dir_mode_ok(0o775, root, me));
        assert!(!dir_mode_ok(0o775, other, me));
        // World-writable, sticky or not: nobody's.
        for owner in [me, other, root] {
            assert!(!dir_mode_ok(0o777, owner, me));
            assert!(!dir_mode_ok(0o1777, owner, me));
            assert!(!dir_mode_ok(0o757, owner, me));
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_executable_must_be_root_s_or_the_user_s_and_written_by_no_one_else() {
        let (me, other, root) = (501, 502, 0);
        assert!(exe_mode_ok(0o755, me, me));
        assert!(exe_mode_ok(0o755, root, me));
        assert!(exe_mode_ok(0o700, me, me));
        assert!(exe_mode_ok(0o4755, root, me));
        assert!(!exe_mode_ok(0o755, other, me), "another user's");
        assert!(!exe_mode_ok(0o775, me, me), "group-writable");
        assert!(!exe_mode_ok(0o757, root, me), "world-writable");
        assert!(!exe_mode_ok(0o644, me, me), "not executable");
    }

    #[test]
    fn the_first_version_of_a_line_is_read() {
        let v = |s: &str| first_version(s).map(|v| v.to_string());
        assert_eq!(
            v("gh version 2.41.0 (2024-01-09)").as_deref(),
            Some("2.41.0")
        );
        assert_eq!(
            v("2024.12.0 macos-arm64 (2024-12-01)").as_deref(),
            Some("2024.12.0")
        );
        assert_eq!(v("node v20.1.0").as_deref(), Some("20.1.0"));
        assert_eq!(
            v("tool 1.2.3-rc.1, then 4.5.6").as_deref(),
            Some("1.2.3-rc.1")
        );
        assert_eq!(v("git version 2.39"), None);
        assert_eq!(v(""), None);
    }
}

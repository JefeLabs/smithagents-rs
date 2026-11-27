//! Repository validation and worktree management via the `git` CLI.
//!
//! All commands are spawned directly (never through a shell) with arguments
//! passed separately, so paths containing spaces or unicode need no escaping.

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Output, Stdio};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::child_env::remove_plugin_secrets;

/// A validated repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub root: PathBuf,
    pub default_branch: String,
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// Nothing exists at the path, or it is not a directory.
    #[error("no directory at {}", .0.display())]
    PathNotFound(PathBuf),
    #[error("not a git repository")]
    NotAGitRepo,
    #[error("bare repositories are not supported")]
    BareRepo,
    #[error("could not determine the default branch")]
    NoDefaultBranch,
    #[error("branch already exists: {0}")]
    BranchExists(String),
    #[error("git command failed: {stderr}")]
    Command { stderr: String },
    /// git exited unsuccessfully. What it printed is not kept.
    #[error("git failed ({})", exit_status_text(.status))]
    Failed { status: Option<i32> },
    /// A name or revision starting with `-`, refused before git runs so it is
    /// never read as an option.
    #[error("a name passed to git must not start with -")]
    UnsafeArgument,
    /// git did not finish in its time and was killed with its process group.
    #[error("git did not finish in time")]
    TimedOut,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// `exit status <n>`, or `no exit status` for a process killed by a signal.
pub(crate) fn exit_status_text(status: &Option<i32>) -> String {
    match status {
        Some(n) => format!("exit status {n}"),
        None => "no exit status".to_string(),
    }
}

/// Why [`commit_paths`] failed. Nothing git printed is kept.
#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    /// git knows no user name or email for the repository.
    #[error("git has no identity for this repository")]
    IdentityMissing,
    #[error("git could not commit ({})", exit_status_text(.status))]
    Failed { status: Option<i32> },
    #[error(transparent)]
    Io(std::io::Error),
}

/// Why [`push_commit`] failed, classified from git's C-locale messages, which
/// are not kept.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PushError {
    #[error("the repository has no remote named origin")]
    NoRemote,
    #[error("authentication failed")]
    Auth,
    #[error("the remote rejected the branch")]
    Rejected,
    #[error("the push did not finish in time")]
    Timeout,
    #[error("git could not push ({})", exit_status_text(.status))]
    Failed { status: Option<i32> },
}

/// Builds a git command with a sanitized environment. `LC_ALL`/`LANGUAGE` are
/// pinned to `C` so the stderr patterns we match are never localized, and no
/// plugin secret is passed on.
fn git_command(cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .kill_on_drop(true);
    remove_plugin_secrets(&mut cmd);
    cmd
}

async fn git_raw(cwd: &Path, args: &[&str]) -> Result<Output, GitError> {
    Ok(git_command(cwd, args).output().await?)
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).trim().to_string()
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Runs git and returns trimmed stdout, mapping failures to typed errors.
async fn git(cwd: &Path, args: &[&str]) -> Result<String, GitError> {
    let out = git_raw(cwd, args).await?;
    if out.status.success() {
        return Ok(stdout_of(&out));
    }
    let stderr = stderr_of(&out);
    if stderr.contains("not a git repository") {
        Err(GitError::NotAGitRepo)
    } else {
        Err(GitError::Command { stderr })
    }
}

/// Runs git and reports only whether it exited successfully.
async fn git_ok(cwd: &Path, args: &[&str]) -> Result<bool, GitError> {
    Ok(git_raw(cwd, args).await?.status.success())
}

/// Resolves `path` to its repository root and default branch. A path that is
/// not an existing directory is [`GitError::PathNotFound`].
///
/// The default branch is the one named by `origin/HEAD`, else the checked-out
/// branch, else (detached HEAD) a local `main` or `master`.
pub async fn validate_repo(path: &Path) -> Result<RepoInfo, GitError> {
    // git runs in `path`, so without this a missing directory would surface
    // as an I/O error from starting git rather than as a bad path.
    match tokio::fs::metadata(path).await {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(GitError::PathNotFound(path.to_owned())),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Err(GitError::PathNotFound(path.to_owned()));
        }
        Err(e) => return Err(e.into()),
    }
    if git(path, &["rev-parse", "--is-bare-repository"]).await? == "true" {
        return Err(GitError::BareRepo);
    }
    let root = PathBuf::from(git(path, &["rev-parse", "--show-toplevel"]).await?);
    let default_branch = default_branch(&root).await?;
    Ok(RepoInfo {
        root,
        default_branch,
    })
}

async fn default_branch(root: &Path) -> Result<String, GitError> {
    let origin = git_raw(
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .await?;
    if origin.status.success() {
        let name = stdout_of(&origin);
        let name = name.strip_prefix("origin/").unwrap_or(&name);
        if !name.is_empty() {
            return Ok(name.to_string());
        }
    }
    let head = git_raw(root, &["symbolic-ref", "--short", "HEAD"]).await?;
    if head.status.success() {
        let name = stdout_of(&head);
        if !name.is_empty() {
            return Ok(name);
        }
    }
    for candidate in ["main", "master"] {
        let reference = format!("refs/heads/{candidate}");
        if git_ok(root, &["show-ref", "--verify", "--quiet", &reference]).await? {
            return Ok(candidate.to_string());
        }
    }
    Err(GitError::NoDefaultBranch)
}

/// Branch an intent's agent works on.
pub fn branch_name_for_intent(slug: &str) -> String {
    format!("intent/{slug}")
}

/// Whether `name` is usable as the name of a local branch. Refused before
/// git runs: an empty name, `@`, a leading `-`, whitespace or control
/// characters, `..`, `@{`, and a trailing `/` or `.lock`. Anything else must
/// pass `git check-ref-format --branch`, of which only the exit status is read.
pub async fn is_valid_branch_name(repo_root: &Path, name: &str) -> Result<bool, GitError> {
    let refused = name.is_empty()
        || name == "@"
        || name.starts_with('-')
        || name.chars().any(|c| c.is_whitespace() || c.is_control())
        || name.contains("..")
        || name.contains("@{")
        || name.ends_with('/')
        || name.ends_with(".lock");
    if refused {
        return Ok(false);
    }
    git_ok(repo_root, &["check-ref-format", "--branch", name]).await
}

/// Whether the local branch `refs/heads/<branch>` exists. Nothing is fetched,
/// and tags and remote-tracking branches of the same name do not count.
pub async fn branch_exists(repo_root: &Path, branch: &str) -> Result<bool, GitError> {
    let reference = format!("refs/heads/{branch}");
    git_ok(repo_root, &["show-ref", "--verify", "--quiet", &reference]).await
}

/// Creates `branch` from `base` in a new worktree under `worktrees_dir`.
///
/// The directory is `worktrees_dir/<branch with '/' replaced by '-'>`.
pub async fn create_worktree(
    repo_root: &Path,
    worktrees_dir: &Path,
    branch: &str,
    base: &str,
) -> Result<PathBuf, GitError> {
    let reference = format!("refs/heads/{branch}");
    if git_ok(repo_root, &["show-ref", "--verify", "--quiet", &reference]).await? {
        return Err(GitError::BranchExists(branch.to_string()));
    }
    tokio::fs::create_dir_all(worktrees_dir).await?;
    let dir = worktrees_dir.join(branch.replace('/', "-"));
    let dir_arg = dir.to_string_lossy().into_owned();
    let out = git_raw(
        repo_root,
        &["worktree", "add", "-b", branch, "--", &dir_arg, base],
    )
    .await?;
    if out.status.success() {
        return Ok(dir);
    }
    let stderr = stderr_of(&out);
    if stderr.contains("not a git repository") {
        Err(GitError::NotAGitRepo)
    } else if stderr.contains("already exists") && stderr.contains("branch") {
        Err(GitError::BranchExists(branch.to_string()))
    } else {
        Err(GitError::Command { stderr })
    }
}

/// Removes a worktree (and its directory), discarding uncommitted changes.
pub async fn remove_worktree(repo_root: &Path, worktree: &Path) -> Result<(), GitError> {
    let wt = worktree.to_string_lossy().into_owned();
    git(repo_root, &["worktree", "remove", "--force", "--", &wt]).await?;
    Ok(())
}

// What follows is used by the feedback tool in a user's repository.
//
// Every command is isolated (see [`isolated_command`]) and runs in a process
// group of its own for a bounded time (see [`run`]): the whole group is
// killed when the time is up or the future is dropped. None returns or logs
// what git printed. A value from the user is passed after `--` or where git
// cannot read it as an option, and one starting with `-` is refused before
// git runs. Over SSH, ssh itself may still prompt on a terminal the daemon
// has (an unknown host key, a passphrase); a push then waits until its
// timeout.

/// The longest a local git command of the feedback tool may take.
pub const LOCAL_TIMEOUT: Duration = Duration::from_secs(60);
/// The most of stdout a command keeps; the rest is read and dropped.
const OUTPUT_MAX: u64 = 16 * 1024 * 1024;
/// The most of stderr a command keeps, only ever to classify a failure.
const ERROR_OUTPUT_MAX: u64 = 64 * 1024;

tokio::task_local! {
    /// Another [`LOCAL_TIMEOUT`] for one task (see [`with_git_timeout`]).
    static TIMEOUT_OVERRIDE: Duration;
}

/// The time a local command may take in this task.
pub(crate) fn local_timeout() -> Duration {
    TIMEOUT_OVERRIDE
        .try_with(|timeout| *timeout)
        .unwrap_or(LOCAL_TIMEOUT)
}

/// Runs `f` with every local git command of the feedback tool limited to
/// `timeout` instead of [`LOCAL_TIMEOUT`].
#[cfg(feature = "test-util")]
pub async fn with_git_timeout<F: std::future::Future>(timeout: Duration, f: F) -> F::Output {
    TIMEOUT_OVERRIDE.scope(timeout, f).await
}

/// `f` (a git call not made through [`run`]) limited to [`local_timeout`];
/// git is killed when the time is up.
pub(crate) async fn bounded<T>(
    f: impl std::future::Future<Output = Result<T, GitError>>,
) -> Result<T, GitError> {
    tokio::time::timeout(local_timeout(), f)
        .await
        .unwrap_or(Err(GitError::TimedOut))
}

/// Configuration given ahead of the repository's own: no hook, file-system
/// monitor or signing program runs, no background maintenance starts, and a
/// sparse checkout of the user's does not narrow what is committed.
const ISOLATED_CONFIG: [&str; 14] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "commit.gpgSign=false",
    "-c",
    "gc.auto=0",
    "-c",
    "maintenance.auto=false",
    "-c",
    "core.sparseCheckout=false",
    "-c",
    "index.sparse=false",
];

/// Variables the daemon may have inherited that would point git at other
/// objects or refs, put another identity or date on a commit, or add or
/// replace configuration (the numbered `GIT_CONFIG_KEY_<n>` and
/// `GIT_CONFIG_VALUE_<n>` are removed as well, see
/// [`INHERITED_CONFIG_PREFIXES`]).
const UNWANTED_ENV: [&str; 18] = [
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    "SSH_ASKPASS",
];

/// The numbered configuration variables (`GIT_CONFIG_KEY_0`, …), found by
/// looking through the daemon's environment.
const INHERITED_CONFIG_PREFIXES: [&[u8]; 2] = [b"GIT_CONFIG_KEY_", b"GIT_CONFIG_VALUE_"];

/// [`git_command`] with [`ISOLATED_CONFIG`] and without [`UNWANTED_ENV`]; no
/// editor (`GIT_EDITOR=:` is git's "start none"); paths are never patterns
/// (`GIT_LITERAL_PATHSPECS`); no replacement or graft the user made
/// (`git replace`) changes what a commit's history is
/// (`GIT_NO_REPLACE_OBJECTS`: a push always sends the real one); and no
/// program asks a person for a credential: an empty `GIT_ASKPASS` stops git
/// looking for one, and Git Credential Manager is told not to open a window.
fn isolated_command(cwd: &Path, args: &[&str]) -> Command {
    let all: Vec<&str> = ISOLATED_CONFIG.iter().chain(args).copied().collect();
    let mut cmd = git_command(cwd, &all);
    cmd.env("GIT_EDITOR", ":")
        .env("GIT_ASKPASS", "")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1");
    for name in UNWANTED_ENV {
        cmd.env_remove(name);
    }
    for (name, _) in std::env::vars_os() {
        let bytes = name.as_encoded_bytes();
        if INHERITED_CONFIG_PREFIXES
            .iter()
            .any(|prefix| bytes.starts_with(prefix))
        {
            cmd.env_remove(&name);
        }
    }
    cmd
}

/// What a command printed (the start of it) and how it ended.
struct Ran {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Ran {
    fn ok(&self) -> Result<(), GitError> {
        if self.status.success() {
            Ok(())
        } else {
            Err(GitError::Failed {
                status: self.status.code(),
            })
        }
    }

    fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }
}

/// Runs `cmd` in a process group of its own for at most `timeout`, with
/// `input` (or nothing) on stdin. The whole group is killed when the time is
/// up (`TimedOut`) or the future is dropped.
async fn run(
    mut cmd: Command,
    timeout: Duration,
    input: Option<&[u8]>,
    stdout_max: u64,
) -> Result<Ran, GitError> {
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn()?;
    // Declared after `child`, so it is dropped first: the group is killed
    // while its leader is not yet reaped and its id cannot be reused.
    let mut group = GroupKiller(child.id());
    let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let write = async move {
        if let (Some(mut pipe), Some(bytes)) = (stdin, input) {
            // git may stop reading early; its exit status tells.
            let _ = pipe.write_all(bytes).await;
        }
    };
    let finish = async {
        let (_, stdout, stderr, status) = tokio::join!(
            write,
            read_capped(stdout, stdout_max),
            read_capped(stderr, ERROR_OUTPUT_MAX),
            child.wait()
        );
        status.map(|status| Ran {
            status,
            stdout,
            stderr,
        })
    };
    match tokio::time::timeout(timeout, finish).await {
        Ok(Ok(ran)) => {
            group.0 = None;
            Ok(ran)
        }
        Ok(Err(e)) => Err(GitError::Io(e)),
        Err(_) => Err(GitError::TimedOut),
    }
}

async fn read_capped<R: AsyncRead + Unpin>(pipe: Option<R>, max: u64) -> Vec<u8> {
    let Some(mut pipe) = pipe else {
        return Vec::new();
    };
    let mut kept = Vec::new();
    let _ = (&mut pipe).take(max).read_to_end(&mut kept).await;
    let _ = tokio::io::copy(&mut pipe, &mut tokio::io::sink()).await;
    kept
}

/// Kills the process group led by the process it names when dropped, unless
/// cleared first.
struct GroupKiller(Option<u32>);

impl Drop for GroupKiller {
    fn drop(&mut self) {
        kill_group(self.0);
    }
}

fn kill_group(leader: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = leader
        .and_then(|id| i32::try_from(id).ok())
        .and_then(rustix::process::Pid::from_raw)
    {
        // The group may be gone already.
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
}

/// [`run`] for a `Drop`: blocking, output discarded, at most `timeout`; only
/// whether git succeeded.
fn run_blocking(cmd: Command, timeout: Duration) -> bool {
    let mut cmd = cmd.into_std();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                kill_group(Some(child.id()));
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Runs an isolated command for at most [`local_timeout`].
async fn isolated(cwd: &Path, args: &[&str]) -> Result<Ran, GitError> {
    run(
        isolated_command(cwd, args),
        local_timeout(),
        None,
        OUTPUT_MAX,
    )
    .await
}

/// Runs an isolated command and keeps only whether it succeeded.
async fn isolated_ok(cwd: &Path, args: &[&str]) -> Result<(), GitError> {
    isolated(cwd, args).await?.ok()
}

fn refuse_option_like(value: &str) -> Result<(), GitError> {
    if value.starts_with('-') {
        Err(GitError::UnsafeArgument)
    } else {
        Ok(())
    }
}

/// `rev` as a commit id (40 or 64 hex digits) or a local branch: any other
/// name is read as `refs/heads/<name>`, so a tag of the same name never wins.
fn branch_or_commit(rev: &str) -> Result<String, GitError> {
    refuse_option_like(rev)?;
    Ok(if is_commit_id(rev) || rev.starts_with("refs/heads/") {
        rev.to_string()
    } else {
        format!("refs/heads/{rev}")
    })
}

/// Whether `origin` has a branch `branch` as far as this repository knows:
/// the remote-tracking branch `refs/remotes/origin/<branch>` exists. Nothing
/// is fetched.
pub async fn tracking_branch_exists(repo_root: &Path, branch: &str) -> Result<bool, GitError> {
    refuse_option_like(branch)?;
    let reference = format!("refs/remotes/origin/{branch}");
    let ran = isolated(repo_root, &["show-ref", "--verify", "--quiet", &reference]).await?;
    match ran.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        status => Err(GitError::Failed { status }),
    }
}

/// What a Git LFS pointer file starts with.
const LFS_POINTER_START: &[u8] = b"version https://git-lfs.github.com/spec/v1";
/// The largest file Git LFS writes as a pointer.
const LFS_POINTER_MAX: u64 = 1024;

/// Whether any file under `folder` at `commit` (a full commit id) is a Git
/// LFS pointer: a blob of at most 1024 bytes that starts with `version
/// https://git-lfs.github.com/spec/v1`. The sizes come from `git ls-tree -r
/// -l -z`; only the small blobs are read, each at most 1024 bytes, never
/// through a filter.
pub async fn has_lfs_pointer(
    repo_root: &Path,
    commit: &str,
    folder: &str,
) -> Result<bool, GitError> {
    if !is_commit_id(commit) {
        return Err(GitError::UnsafeArgument);
    }
    let ran = isolated(
        repo_root,
        &[
            "ls-tree",
            "-r",
            "-l",
            "-z",
            "--full-tree",
            commit,
            "--",
            folder,
        ],
    )
    .await?;
    ran.ok()?;
    // Each entry: `<mode> SP <type> SP <id> SP+ <size> TAB <path>`, ended by
    // NUL; a submodule's size is `-`.
    let small: Vec<String> = ran
        .stdout
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let tab = entry.iter().position(|b| *b == b'\t')?;
            let head = std::str::from_utf8(&entry[..tab]).ok()?;
            let mut fields = head.split_ascii_whitespace();
            let (_mode, kind, id, size) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            let size: u64 = size.parse().ok()?;
            (kind == "blob" && size <= LFS_POINTER_MAX && is_commit_id(id)).then(|| id.to_owned())
        })
        .collect();
    for id in small {
        let read = run(
            isolated_command(repo_root, &["cat-file", "blob", &id]),
            local_timeout(),
            None,
            LFS_POINTER_MAX,
        )
        .await?;
        read.ok()?;
        if read.stdout.starts_with(LFS_POINTER_START) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// What `path` is at `rev` (a local branch or a commit id, see
/// [`show_file`]): `blob` (a file or a link), `tree` (a folder) or `commit`
/// (a submodule, mode 160000); `None` when nothing is there or `rev` names
/// nothing. Read from the tree entry (`git ls-tree -z --full-tree <rev> --
/// <path>`), not from the object, which a submodule's commit is not in this
/// repository to have.
pub async fn path_kind(
    repo_root: &Path,
    rev: &str,
    path: &str,
) -> Result<Option<String>, GitError> {
    let rev = branch_or_commit(rev)?;
    let ran = isolated(
        repo_root,
        &["ls-tree", "-z", "--full-tree", &rev, "--", path],
    )
    .await?;
    if !ran.status.success() {
        return Ok(None);
    }
    // Each entry: `<mode> SP <type> SP <id> TAB <path>`, ended by NUL.
    Ok(ran.stdout.split(|b| *b == 0).find_map(|entry| {
        let tab = entry.iter().position(|b| *b == b'\t')?;
        if entry[tab + 1..] != *path.as_bytes() {
            return None;
        }
        let kind = entry[..tab].split(|b| *b == b' ').nth(1)?;
        Some(String::from_utf8_lossy(kind).into_owned())
    }))
}

/// The URL pushes to `origin` go to, as configured: `remote.origin.pushurl`
/// when set, else `remote.origin.url`, with any user name and password
/// removed (`https://user:token@host/…` is `https://host/…`). It is read
/// with `git config`, so a `url.<base>.insteadOf` rewrite is not applied.
/// `None` when neither is set.
pub async fn origin_url(repo_root: &Path) -> Result<Option<String>, GitError> {
    for key in ["remote.origin.pushurl", "remote.origin.url"] {
        let ran = isolated(repo_root, &["config", "--get-all", key]).await?;
        match ran.status.code() {
            Some(0) => {
                let text = String::from_utf8_lossy(&ran.stdout);
                if let Some(url) = text.lines().map(str::trim).find(|l| !l.is_empty()) {
                    return Ok(Some(without_userinfo(url)));
                }
            }
            // The key is not set.
            Some(1) => {}
            status => return Err(GitError::Failed { status }),
        }
    }
    Ok(None)
}

/// `url` without the user name and password of its authority:
/// `scheme://user:pass@host/path` and the scp-like `user@host:path`.
pub(crate) fn without_userinfo(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        let end = rest.find('/').unwrap_or(rest.len());
        let (authority, path) = rest.split_at(end);
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        return format!("{scheme}://{host}{path}");
    }
    match url.split_once(':') {
        Some((before, after)) if !before.contains('/') => match before.rsplit_once('@') {
            Some((_, host)) => format!("{host}:{after}"),
            None => url.to_string(),
        },
        _ => url.to_string(),
    }
}

/// The id of the commit `rev` names; `None` when it names no commit. `rev`
/// is a full commit id or a local branch (`refs/heads/<name>` or `<name>`,
/// never a tag of that name), as in [`show_file`].
pub async fn rev_parse(repo_root: &Path, rev: &str) -> Result<Option<String>, GitError> {
    let commit = format!("{}^{{commit}}", branch_or_commit(rev)?);
    let ran = isolated(repo_root, &["rev-parse", "--verify", "--quiet", &commit]).await?;
    match ran.status.code() {
        Some(0) => Ok(Some(ran.stdout_text()).filter(|id| !id.is_empty())),
        Some(1) => Ok(None),
        status => Err(GitError::Failed { status }),
    }
}

/// Whether the commit `commit` (a full commit id) is the tip of the local
/// branch `branch` or in its history: `git merge-base --is-ancestor <commit>
/// <tip>`. `false` when the branch is gone, or the commit is not in the
/// repository.
pub async fn is_on_branch(repo_root: &Path, commit: &str, branch: &str) -> Result<bool, GitError> {
    if !is_commit_id(commit) {
        return Err(GitError::UnsafeArgument);
    }
    let Some(tip) = rev_parse(repo_root, &format!("refs/heads/{branch}")).await? else {
        return Ok(false);
    };
    if rev_parse(repo_root, commit).await?.as_deref() != Some(commit) {
        return Ok(false);
    }
    let ran = isolated(repo_root, &["merge-base", "--is-ancestor", commit, &tip]).await?;
    match ran.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        status => Err(GitError::Failed { status }),
    }
}

/// The repository's common git directory, absolute (where `refs/` is).
pub async fn common_dir(repo_root: &Path) -> Result<PathBuf, GitError> {
    let ran = isolated(
        repo_root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    ran.ok()?;
    Ok(PathBuf::from(ran.stdout_text()))
}

/// Deletes the local branch `branch` only while it points at `commit` (a
/// full commit id): `git update-ref -d refs/heads/<branch> <commit>`. A
/// branch that moved on, or is gone, is not touched (`Failed`).
pub async fn delete_branch_at(
    repo_root: &Path,
    branch: &str,
    commit: &str,
) -> Result<(), GitError> {
    refuse_option_like(branch)?;
    let is_id = matches!(commit.len(), 40 | 64) && commit.bytes().all(|b| b.is_ascii_hexdigit());
    if !is_id {
        return Err(GitError::UnsafeArgument);
    }
    let reference = format!("refs/heads/{branch}");
    isolated_ok(repo_root, &["update-ref", "-d", &reference, commit]).await
}

/// A local branch's tip and the values of one trailer of its message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailerTip {
    /// Without `refs/heads/`.
    pub branch: String,
    pub commit: String,
    pub values: Vec<String>,
}

/// The tips of the local branches under `refs/heads/<namespace>/`, each with
/// the values of the trailer `key` in its message, as git reads trailers
/// (the last paragraph, never the subject; folded lines unfolded). One
/// `git for-each-ref`. `key` and each namespace are letters, digits and `-`.
pub async fn branch_tips_with_trailer(
    repo_root: &Path,
    namespaces: &[&str],
    key: &str,
) -> Result<Vec<TrailerTip>, GitError> {
    let plain =
        |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !plain(key) || !namespaces.iter().all(|n| plain(n)) {
        return Err(GitError::UnsafeArgument);
    }
    let format = format!(
        "--format=%(objectname)%00%(refname)%00%(trailers:key={key},valueonly,unfold,separator=%x00)"
    );
    let patterns: Vec<String> = namespaces
        .iter()
        .map(|n| format!("refs/heads/{n}"))
        .collect();
    let mut args = vec!["for-each-ref", format.as_str()];
    args.extend(patterns.iter().map(String::as_str));
    let ran = isolated(repo_root, &args).await?;
    ran.ok()?;
    let mut tips = Vec::new();
    for record in ran.stdout.split(|b| *b == b'\n').filter(|r| !r.is_empty()) {
        let mut fields = record.split(|b| *b == 0);
        let (Some(commit), Some(refname)) = (fields.next(), fields.next()) else {
            continue;
        };
        let refname = String::from_utf8_lossy(refname);
        let Some(branch) = refname.strip_prefix("refs/heads/") else {
            continue;
        };
        // `refs/heads/feedback` itself matches the pattern too.
        if !namespaces.iter().any(|n| {
            branch
                .strip_prefix(n)
                .is_some_and(|rest| rest.starts_with('/'))
        }) {
            continue;
        }
        tips.push(TrailerTip {
            branch: branch.to_string(),
            commit: String::from_utf8_lossy(commit).into_owned(),
            values: fields
                .filter(|v| !v.is_empty())
                .map(|v| String::from_utf8_lossy(v).into_owned())
                .collect(),
        });
    }
    Ok(tips)
}

/// The paths `commit` (a full commit id) changes against its parent: `git
/// diff-tree -r --name-only`. Nothing is read through a filter.
pub async fn commit_changed_paths(repo_root: &Path, commit: &str) -> Result<Vec<String>, GitError> {
    let is_id = matches!(commit.len(), 40 | 64) && commit.bytes().all(|b| b.is_ascii_hexdigit());
    if !is_id {
        return Err(GitError::UnsafeArgument);
    }
    let ran = isolated(
        repo_root,
        &[
            "diff-tree",
            "--no-commit-id",
            "-r",
            "--name-only",
            "--no-renames",
            "-z",
            commit,
        ],
    )
    .await?;
    ran.ok()?;
    Ok(ran
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect())
}

/// What this repository knows `origin` has of some commits: see
/// [`publication`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publication {
    /// All of them and their history are on remote-tracking branches of
    /// `origin`.
    Published,
    /// At least one commit is on none of them.
    Unpushed,
    /// There is no `refs/remotes/origin/*` at all, so nothing is known.
    NeverFetched,
}

/// Whether every commit `revs` stands for, with its history, is on a
/// remote-tracking branch of `origin`: `git rev-list --max-count=1 <revs>
/// --not --remotes=origin` prints nothing. Only local refs are read; nothing
/// is fetched. Each rev is a full commit id, a full commit id's parents
/// (`<id>^@`) or a local branch (`refs/heads/<name>`).
pub async fn publication(repo_root: &Path, revs: &[&str]) -> Result<Publication, GitError> {
    let valid = |rev: &str| {
        let id = rev.strip_suffix("^@").unwrap_or(rev);
        is_commit_id(id)
            || rev
                .strip_prefix("refs/heads/")
                .is_some_and(|name| !name.is_empty() && !name.starts_with('-'))
    };
    if revs.is_empty() || !revs.iter().all(|rev| valid(rev)) {
        return Err(GitError::UnsafeArgument);
    }
    let tracking = isolated(
        repo_root,
        &[
            "for-each-ref",
            "--count=1",
            "--format=%(refname)",
            "refs/remotes/origin",
        ],
    )
    .await?;
    tracking.ok()?;
    if tracking.stdout_text().is_empty() {
        return Ok(Publication::NeverFetched);
    }
    let mut args = vec!["rev-list", "--max-count=1"];
    args.extend_from_slice(revs);
    args.extend(["--not", "--remotes=origin", "--"]);
    let listed = isolated(repo_root, &args).await?;
    listed.ok()?;
    Ok(if listed.stdout_text().is_empty() {
        Publication::Published
    } else {
        Publication::Unpushed
    })
}

/// What the commits on top of a base change: see [`changes_since`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Changes {
    /// The paths, as many times as commits change them, in no useful order.
    Paths(Vec<String>),
    /// The head does not have the base in its history.
    NotOnBase,
    /// More commits than were to be looked at.
    TooMany,
}

/// Every path a commit after `base` up to `head` (both full commit ids)
/// changes: each commit of `git rev-list <base>..<head>` against each of its
/// parents (`git diff-tree --stdin -r -m --root --name-only --no-renames
/// -z`), so a file changed and changed back, or one a merge brings in, is
/// there too. `NotOnBase` unless `git merge-base --is-ancestor <base>
/// <head>` holds; `TooMany` past `max_commits` commits, or when the paths do
/// not fit the output git may print here. Nothing is read through a filter.
pub async fn changes_since(
    repo_root: &Path,
    base: &str,
    head: &str,
    max_commits: usize,
) -> Result<Changes, GitError> {
    if !is_commit_id(base) || !is_commit_id(head) {
        return Err(GitError::UnsafeArgument);
    }
    let ancestor = isolated(repo_root, &["merge-base", "--is-ancestor", base, head]).await?;
    match ancestor.status.code() {
        Some(0) => {}
        Some(1) => return Ok(Changes::NotOnBase),
        status => return Err(GitError::Failed { status }),
    }
    let max = format!("--max-count={}", max_commits.saturating_add(1));
    let range = format!("{base}..{head}");
    let listed = isolated(repo_root, &["rev-list", &max, &range, "--"]).await?;
    listed.ok()?;
    let commits = String::from_utf8_lossy(&listed.stdout).into_owned();
    let commits: Vec<&str> = commits.lines().filter(|l| !l.is_empty()).collect();
    if commits.len() > max_commits {
        return Ok(Changes::TooMany);
    }
    if commits.is_empty() {
        return Ok(Changes::Paths(Vec::new()));
    }
    let input = format!("{}\n", commits.join("\n"));
    let ran = run(
        isolated_command(
            repo_root,
            &[
                "diff-tree",
                "--stdin",
                "-r",
                "-m",
                "--root",
                "--no-commit-id",
                "--name-only",
                "--no-renames",
                "-z",
            ],
        ),
        local_timeout(),
        Some(input.as_bytes()),
        OUTPUT_MAX + 1,
    )
    .await?;
    ran.ok()?;
    if ran.stdout.len() as u64 > OUTPUT_MAX {
        return Ok(Changes::TooMany);
    }
    Ok(Changes::Paths(
        ran.stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect(),
    ))
}

/// Creates the local branch `branch` at `commit`, without tracking anything.
/// Fails when the branch exists.
pub async fn create_branch(repo_root: &Path, branch: &str, commit: &str) -> Result<(), GitError> {
    refuse_option_like(branch)?;
    refuse_option_like(commit)?;
    isolated_ok(repo_root, &["branch", "--no-track", "--", branch, commit]).await
}

/// Adds a worktree at `dir` (which must not exist), locked with
/// `lock_reason`, with `HEAD` detached at `commit` and the index read from it
/// but no file checked out. A commit made there keeps every other file of
/// `commit`, and nothing the repository holds (a checkout hook, a smudge
/// filter, a symbolic link) runs or is followed. The lock keeps a prune by
/// anyone else from removing the worktree while it is used, and names it.
pub async fn add_detached_worktree(
    repo_root: &Path,
    dir: &Path,
    commit: &str,
    lock_reason: &str,
) -> Result<(), GitError> {
    refuse_option_like(commit)?;
    refuse_option_like(lock_reason)?;
    let dir_arg = dir.to_string_lossy().into_owned();
    isolated_ok(
        repo_root,
        &[
            "worktree",
            "add",
            "--detach",
            "--no-checkout",
            "--lock",
            "--reason",
            lock_reason,
            "--",
            &dir_arg,
            commit,
        ],
    )
    .await?;
    isolated_ok(dir, &["read-tree", "HEAD"]).await
}

fn remove_worktree_command(repo_root: &Path, dir: &Path) -> Command {
    let dir_arg = dir.to_string_lossy().into_owned();
    isolated_command(
        repo_root,
        &["worktree", "remove", "--force", "--force", "--", &dir_arg],
    )
}

/// Removes the one worktree registered at `dir`, locked or not, with its
/// directory if there is one: `git worktree remove --force --force`. Nothing
/// else is pruned: a worktree of the user's whose folder is missing (moved,
/// on a volume not mounted) keeps its data.
pub async fn remove_worktree_at(repo_root: &Path, dir: &Path) -> Result<(), GitError> {
    run(
        remove_worktree_command(repo_root, dir),
        local_timeout(),
        None,
        OUTPUT_MAX,
    )
    .await?
    .ok()
}

/// [`remove_worktree_at`] for a `Drop`: blocking, at most `timeout`.
pub(crate) fn remove_worktree_at_blocking(repo_root: &Path, dir: &Path, timeout: Duration) -> bool {
    run_blocking(remove_worktree_command(repo_root, dir), timeout)
}

/// A worktree the repository knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    /// The lock's reason (empty when it has none); `None` when not locked.
    pub locked: Option<String>,
}

/// The repository's worktrees, the main one first: `git worktree list
/// --porcelain -z`.
pub async fn list_worktrees(repo_root: &Path) -> Result<Vec<WorktreeEntry>, GitError> {
    let ran = isolated(repo_root, &["worktree", "list", "--porcelain", "-z"]).await?;
    ran.ok()?;
    let mut entries: Vec<WorktreeEntry> = Vec::new();
    for field in ran.stdout.split(|b| *b == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            entries.push(WorktreeEntry {
                path: PathBuf::from(std::ffi::OsStr::from_bytes(path)),
                locked: None,
            });
        } else if let Some(entry) = entries.last_mut() {
            if field == b"locked" {
                entry.locked = Some(String::new());
            } else if let Some(reason) = field.strip_prefix(b"locked ") {
                entry.locked = Some(String::from_utf8_lossy(reason).into_owned());
            }
        }
    }
    Ok(entries)
}

/// What git prints, in the C locale, when it knows no identity to commit as.
const IDENTITY_MISSING: &str = "Please tell me who you are";

/// Configuration for storing and committing feedback files: Git LFS is named
/// and turned off, the global attributes file is not read, and line endings
/// are never converted. (The files themselves are stored without any
/// attribute; this covers the commit's own reading of the index.)
const NO_ATTRIBUTES_CONFIG: [&str; 14] = [
    "-c",
    "filter.lfs.clean=",
    "-c",
    "filter.lfs.smudge=",
    "-c",
    "filter.lfs.process=",
    "-c",
    "filter.lfs.required=false",
    "-c",
    "core.attributesFile=/dev/null",
    "-c",
    "core.autocrlf=false",
    "-c",
    "core.safecrlf=false",
];

/// Stores `paths` (files in `worktree`) and commits them with `message`,
/// then returns the new commit's id.
///
/// The files are committed byte for byte as plain blobs, whatever the
/// repository's attributes say (in-tree `.gitattributes`, `info/attributes`,
/// global or system): each is stored with `git hash-object -w --no-filters`
/// and put in the index with `git update-index --add --cacheinfo 100644,…`,
/// so no filter (Git LFS, git-crypt, …), `ident`, `working-tree-encoding` or
/// line-ending conversion runs or changes it, and ignore rules do not apply.
/// Its working copy is then removed, so `git commit --no-verify` never reads
/// it again through a filter.
///
/// The author is the identity configured for the repository, never one git
/// guesses from the user and host names (`user.useConfigOnly`). The commit is
/// never signed: a signing program could wait for a passphrase no one is
/// there to type. The message goes on stdin, and `--cleanup=whitespace` keeps
/// a configured comment character from eating it.
pub async fn commit_paths(
    worktree: &Path,
    paths: &[&str],
    message: &str,
) -> Result<String, CommitError> {
    if paths.is_empty() || paths.iter().any(|p| p.is_empty() || p.starts_with('-')) {
        return Err(CommitError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a path to commit is empty or starts with -",
        )));
    }
    let command = |args: &[&str]| {
        let mut all: Vec<&str> = NO_ATTRIBUTES_CONFIG.to_vec();
        all.extend_from_slice(args);
        isolated_command(worktree, &all)
    };
    for path in paths {
        let hash = ["hash-object", "-w", "--no-filters", "--", path];
        let ran = run(command(&hash), local_timeout(), None, OUTPUT_MAX)
            .await
            .map_err(commit_io)?;
        ran.ok().map_err(commit_io)?;
        let entry = format!("100644,{},{path}", ran.stdout_text());
        let update = ["update-index", "--add", "--cacheinfo", &entry];
        run(command(&update), local_timeout(), None, OUTPUT_MAX)
            .await
            .and_then(|ran| ran.ok())
            .map_err(commit_io)?;
        tokio::fs::remove_file(worktree.join(path))
            .await
            .map_err(CommitError::Io)?;
    }

    let commit = [
        "-c",
        "user.useConfigOnly=true",
        "commit",
        "--quiet",
        "--no-verify",
        "--no-gpg-sign",
        "--cleanup=whitespace",
        "-F",
        "-",
    ];
    let ran = run(
        command(&commit),
        local_timeout(),
        Some(message.as_bytes()),
        OUTPUT_MAX,
    )
    .await
    .map_err(commit_io)?;
    if !ran.status.success() {
        return Err(
            if String::from_utf8_lossy(&ran.stderr).contains(IDENTITY_MISSING) {
                CommitError::IdentityMissing
            } else {
                CommitError::Failed {
                    status: ran.status.code(),
                }
            },
        );
    }
    let ran = isolated(worktree, &["rev-parse", "--verify", "HEAD^{commit}"])
        .await
        .map_err(commit_io)?;
    let id = ran.stdout_text();
    if !ran.status.success() || id.is_empty() {
        return Err(CommitError::Failed {
            status: ran.status.code(),
        });
    }
    Ok(id)
}

fn commit_io(e: GitError) -> CommitError {
    match e {
        GitError::Io(e) => CommitError::Io(e),
        GitError::Failed { status } => CommitError::Failed { status },
        _ => CommitError::Failed { status: None },
    }
}

/// A branch was refused: not a fast-forward, or declined by the remote.
const PUSH_REJECTED: [&str; 2] = ["[rejected]", "[remote rejected]"];
/// No credential, or a wrong one, over HTTPS or SSH.
const PUSH_AUTH: [&str; 8] = [
    "Authentication failed",
    "could not read Username",
    "could not read Password",
    "terminal prompts disabled",
    "Invalid username or password",
    "Permission denied (publickey",
    "The requested URL returned error: 401",
    "The requested URL returned error: 403",
];
/// git takes a remote name it does not know for a path.
const PUSH_NO_REMOTE: &str = "'origin' does not appear to be a git repository";

/// `git push origin <commit>:refs/heads/<remote_branch>`, at most `timeout`
/// long: that commit, whatever any local branch points at now. Only for
/// `feedback::repo::push_commit`, which first checks what the push would
/// publish.
///
/// Never forced, and the repository's settings add nothing: no hook,
/// signature, tag or submodule. A credential that is not stored fails at once
/// (`GIT_TERMINAL_PROMPT=0` and no askpass program) instead of waiting for a
/// person. `origin` must have a URL to push to as git sees it (`git remote
/// get-url --push`: a `pushurl` alone will do), else `NoRemote`. `commit`
/// must be a full commit id (40 or 64 hex digits) and `remote_branch` a
/// valid branch name that starts with neither `-` nor `+`, else `Failed`
/// with no exit status before git runs.
pub(crate) async fn push_commit(
    repo_root: &Path,
    commit: &str,
    remote_branch: &str,
    timeout: Duration,
) -> Result<(), PushError> {
    if !is_commit_id(commit) {
        return Err(PushError::Failed { status: None });
    }
    require_push_name(repo_root, remote_branch).await?;
    push_refspec(
        repo_root,
        &format!("{commit}:refs/heads/{remote_branch}"),
        timeout,
    )
    .await
}

/// A full commit id: 40 (SHA-1) or 64 (SHA-256) hex digits.
pub fn is_commit_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `Failed` with no exit status unless `name` is a branch name git takes
/// that starts with neither `-` nor `+`.
async fn require_push_name(repo_root: &Path, name: &str) -> Result<(), PushError> {
    let valid = !name.starts_with(['-', '+'])
        && bounded(is_valid_branch_name(repo_root, name))
            .await
            .unwrap_or(false);
    if valid {
        Ok(())
    } else {
        Err(PushError::Failed { status: None })
    }
}

/// `git push origin <refspec>` once `origin` has a URL to push to.
async fn push_refspec(repo_root: &Path, refspec: &str, timeout: Duration) -> Result<(), PushError> {
    match isolated(repo_root, &["remote", "get-url", "--push", "origin"]).await {
        Ok(ran) if ran.status.success() => {}
        // "No such remote".
        Ok(ran) if ran.status.code() == Some(2) => return Err(PushError::NoRemote),
        Ok(ran) => {
            return Err(PushError::Failed {
                status: ran.status.code(),
            })
        }
        Err(GitError::TimedOut) => return Err(PushError::Timeout),
        Err(_) => return Err(PushError::Failed { status: None }),
    }
    match run(
        push_command(repo_root, refspec),
        timeout,
        None,
        ERROR_OUTPUT_MAX,
    )
    .await
    {
        Ok(ran) if ran.status.success() => Ok(()),
        Ok(ran) => {
            let mut output = String::from_utf8_lossy(&ran.stdout).into_owned();
            output.push_str(&String::from_utf8_lossy(&ran.stderr));
            Err(classify_push_failure(&output, ran.status.code()))
        }
        Err(GitError::TimedOut) => Err(PushError::Timeout),
        Err(_) => Err(PushError::Failed { status: None }),
    }
}

fn push_command(repo_root: &Path, refspec: &str) -> Command {
    isolated_command(
        repo_root,
        &[
            "push",
            "--no-verify",
            "--no-signed",
            "--no-follow-tags",
            "--recurse-submodules=no",
            "--",
            "origin",
            refspec,
        ],
    )
}

fn classify_push_failure(output: &str, status: Option<i32>) -> PushError {
    let has = |markers: &[&str]| markers.iter().any(|m| output.contains(m));
    if has(&PUSH_REJECTED) {
        PushError::Rejected
    } else if has(&PUSH_AUTH) {
        PushError::Auth
    } else if output.contains(PUSH_NO_REMOTE) {
        PushError::NoRemote
    } else {
        PushError::Failed { status }
    }
}

/// The type of the object `object` (`<rev>:<path>`) names, `None` when it
/// names nothing.
async fn object_type(repo_root: &Path, object: &str) -> Result<Option<String>, GitError> {
    let ran = isolated(repo_root, &["cat-file", "-t", object]).await?;
    Ok(ran.status.success().then(|| ran.stdout_text()))
}

/// The file `path` at `rev` as text (`git cat-file blob <rev>:<path>`): at
/// most `max_bytes` of it, cut at a character boundary. `rev` is a commit id
/// or a local branch (`refs/heads/<name>` or `<name>`, never a tag of that
/// name). `None` when `rev` names nothing or holds no file at `path`.
pub async fn show_file(
    repo_root: &Path,
    rev: &str,
    path: &str,
    max_bytes: usize,
) -> Result<Option<String>, GitError> {
    let object = format!("{}:{path}", branch_or_commit(rev)?);
    if object_type(repo_root, &object).await?.as_deref() != Some("blob") {
        return Ok(None);
    }
    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let ran = run(
        isolated_command(repo_root, &["cat-file", "blob", &object]),
        local_timeout(),
        None,
        limit,
    )
    .await?;
    ran.ok()?;
    let mut bytes = ran.stdout;
    if bytes.len() > max_bytes {
        bytes.truncate(max_bytes);
        // Drop a character the cut split.
        if let Err(e) = std::str::from_utf8(&bytes) {
            if e.error_len().is_none() {
                bytes.truncate(e.valid_up_to());
            }
        }
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// The names of the files directly in `folder` at `rev` (as in
/// [`show_file`]), in git's order: no subfolder, no submodule. Empty when
/// `rev` names nothing or holds no folder at `folder`.
pub async fn list_folder(
    repo_root: &Path,
    rev: &str,
    folder: &str,
) -> Result<Vec<String>, GitError> {
    let object = format!("{}:{folder}", branch_or_commit(rev)?);
    if object_type(repo_root, &object).await?.as_deref() != Some("tree") {
        return Ok(Vec::new());
    }
    let ran = isolated(repo_root, &["ls-tree", "-z", &object]).await?;
    ran.ok()?;
    // Each entry: `<mode> SP <type> SP <id> TAB <name>`, ended by NUL.
    Ok(ran
        .stdout
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let tab = entry.iter().position(|b| *b == b'\t')?;
            let kind = entry[..tab].split(|b| *b == b' ').nth(1)?;
            (kind == b"blob").then(|| String::from_utf8_lossy(&entry[tab + 1..]).into_owned())
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Synchronous git helper for building fixtures, independent of user config.
    fn g(cwd: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
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
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn init_repo(dir: &Path, branch: &str) {
        std::fs::create_dir_all(dir).unwrap();
        g(dir, &["init", "-b", branch]);
        g(dir, &["commit", "--allow-empty", "-m", "init"]);
    }

    #[tokio::test]
    async fn changes_since_lists_what_a_merge_commit_adds_of_its_own() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("r");
        init_repo(&dir, "main");
        let write = |path: &str| {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, path).unwrap();
            g(&dir, &["add", "-A"]);
        };
        write("f/a");
        g(&dir, &["commit", "-q", "-m", "base"]);
        let base = g(&dir, &["rev-parse", "HEAD"]);
        g(&dir, &["checkout", "-q", "-b", "side"]);
        write("f/s");
        g(&dir, &["commit", "-q", "-m", "side"]);
        g(&dir, &["checkout", "-q", "main"]);
        write("f/m");
        g(&dir, &["commit", "-q", "-m", "main"]);
        // The merge commit itself brings a file no parent has.
        g(&dir, &["merge", "-q", "--no-ff", "--no-commit", "side"]);
        write("evil");
        g(&dir, &["commit", "-q", "-m", "merge"]);
        let head = g(&dir, &["rev-parse", "HEAD"]);

        let Changes::Paths(paths) = changes_since(&dir, &base, &head, 10).await.unwrap() else {
            panic!("paths expected");
        };
        assert!(paths.iter().any(|p| p == "evil"), "{paths:?}");
        assert!(
            paths.iter().all(|p| p == "evil" || p.starts_with("f/")),
            "{paths:?}"
        );
    }

    #[tokio::test]
    async fn changes_since_lists_every_commits_paths_against_each_parent() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("r");
        init_repo(&dir, "main");
        let commit = |path: &str, rm: bool| {
            if rm {
                g(&dir, &["rm", "-q", path]);
            } else {
                let file = dir.join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(file, path).unwrap();
                g(&dir, &["add", "-A"]);
            }
            g(&dir, &["commit", "-q", "-m", path]);
            g(&dir, &["rev-parse", "HEAD"])
        };
        let base = commit("f/a", false);
        // Changed and changed back: still there.
        commit("b", false);
        commit("b", true);
        g(&dir, &["checkout", "-q", "-b", "side", &base]);
        commit("s", false);
        g(&dir, &["checkout", "-q", "main"]);
        g(&dir, &["merge", "-q", "--no-edit", "side"]);
        let head = g(&dir, &["rev-parse", "HEAD"]);

        let Changes::Paths(mut paths) = changes_since(&dir, &base, &head, 10).await.unwrap() else {
            panic!("paths expected");
        };
        paths.sort();
        paths.dedup();
        assert_eq!(paths, ["b", "s"], "the base's own change is not listed");
        assert_eq!(
            changes_since(&dir, &head, &head, 10).await.unwrap(),
            Changes::Paths(Vec::new())
        );
        assert_eq!(
            changes_since(&dir, &base, &head, 3).await.unwrap(),
            Changes::TooMany
        );
        // A head without the base in its history.
        g(&dir, &["checkout", "-q", "--orphan", "other"]);
        g(&dir, &["commit", "-q", "--allow-empty", "-m", "other"]);
        let other = g(&dir, &["rev-parse", "HEAD"]);
        assert_eq!(
            changes_since(&dir, &base, &other, 10).await.unwrap(),
            Changes::NotOnBase
        );
        assert!(matches!(
            changes_since(&dir, "main", &head, 10).await,
            Err(GitError::UnsafeArgument)
        ));
    }

    fn canon(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap()
    }

    #[test]
    fn git_command_pins_locale_and_clears_git_env() {
        let cmd = git_command(Path::new("."), &["status"]);
        let envs: std::collections::HashMap<_, _> = cmd.as_std().get_envs().collect();
        let val = |k: &str| envs.get(std::ffi::OsStr::new(k)).copied();
        assert_eq!(val("LC_ALL"), Some(Some(std::ffi::OsStr::new("C"))));
        assert_eq!(val("LANGUAGE"), Some(Some(std::ffi::OsStr::new("C"))));
        assert_eq!(val("GIT_DIR"), Some(None));
        assert_eq!(val("GIT_WORK_TREE"), Some(None));
    }

    #[test]
    fn git_command_does_not_inherit_plugin_secrets() {
        // Set once and never removed: a name no other test uses.
        std::env::set_var("SMITHAGENTS_SECRET_GIT_TEST__TOKEN", "not-for-git");
        let cmd = git_command(Path::new("."), &["status"]);
        let envs: std::collections::HashMap<_, _> = cmd.as_std().get_envs().collect();
        let name = std::ffi::OsStr::new("SMITHAGENTS_SECRET_GIT_TEST__TOKEN");
        assert_eq!(envs.get(name).copied(), Some(None));
    }

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn isolated_commands_run_no_hook_monitor_signer_editor_or_askpass() {
        let cmd = isolated_command(Path::new("."), &["commit"]);
        let args = args_of(&cmd);
        for setting in [
            "core.hooksPath=/dev/null",
            "core.fsmonitor=false",
            "commit.gpgSign=false",
            "gc.auto=0",
            "maintenance.auto=false",
            "core.sparseCheckout=false",
            "index.sparse=false",
        ] {
            let at = args.iter().position(|a| a == setting).expect(setting);
            assert_eq!(args[at - 1], "-c");
        }
        assert_eq!(args.last().map(String::as_str), Some("commit"));
        let envs: std::collections::HashMap<_, _> = cmd.as_std().get_envs().collect();
        let val = |k: &str| envs.get(std::ffi::OsStr::new(k)).copied();
        let set = |v: &'static str| Some(Some(std::ffi::OsStr::new(v)));
        assert_eq!(val("GIT_EDITOR"), set(":"));
        assert_eq!(val("GIT_ASKPASS"), set(""));
        assert_eq!(val("GCM_INTERACTIVE"), set("never"));
        assert_eq!(val("GIT_TERMINAL_PROMPT"), set("0"));
        assert_eq!(val("GIT_LITERAL_PATHSPECS"), set("1"));
        assert_eq!(val("GIT_NO_REPLACE_OBJECTS"), set("1"));
        assert_eq!(val("LC_ALL"), set("C"));
        for name in [
            "SSH_ASKPASS",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
            "GIT_CEILING_DIRECTORIES",
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_AUTHOR_DATE",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
            "GIT_COMMITTER_DATE",
        ] {
            assert_eq!(val(name), Some(None), "{name} is removed");
        }
    }

    #[test]
    fn isolated_commands_inherit_no_configuration_from_the_environment() {
        // Set once and never removed: names no other test uses, and without
        // `GIT_CONFIG_COUNT` git ignores them.
        std::env::set_var("GIT_CONFIG_KEY_93", "user.name");
        std::env::set_var("GIT_CONFIG_VALUE_93", "Injected");
        let cmd = isolated_command(Path::new("."), &["commit"]);
        let envs: std::collections::HashMap<_, _> = cmd.as_std().get_envs().collect();
        for name in [
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_93",
            "GIT_CONFIG_VALUE_93",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_NOSYSTEM",
        ] {
            assert_eq!(
                envs.get(std::ffi::OsStr::new(name)).copied(),
                Some(None),
                "{name} is removed"
            );
        }
    }

    #[test]
    fn credentials_are_removed_from_a_url() {
        for (url, expected) in [
            (
                "https://user:LEAK@github.com/o/r.git",
                "https://github.com/o/r.git",
            ),
            (
                "https://LEAK@github.com/o/r.git",
                "https://github.com/o/r.git",
            ),
            ("ssh://git@github.com/o/r.git", "ssh://github.com/o/r.git"),
            ("https://github.com/o/r.git", "https://github.com/o/r.git"),
            ("https://u:p@host", "https://host"),
            ("git@github.com:o/r.git", "github.com:o/r.git"),
            ("github.com:o/r.git", "github.com:o/r.git"),
            ("/srv/repos/a@b/r.git", "/srv/repos/a@b/r.git"),
            ("./a@b:c", "./a@b:c"),
        ] {
            assert_eq!(without_userinfo(url), expected, "{url}");
        }
    }

    #[test]
    fn a_rev_is_a_commit_id_or_a_local_branch() {
        let id = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(branch_or_commit(id).unwrap(), id);
        assert_eq!(branch_or_commit("main").unwrap(), "refs/heads/main");
        assert_eq!(
            branch_or_commit("feedback/x").unwrap(),
            "refs/heads/feedback/x"
        );
        assert_eq!(branch_or_commit("refs/heads/x").unwrap(), "refs/heads/x");
        assert_eq!(
            branch_or_commit("refs/tags/v1").unwrap(),
            "refs/heads/refs/tags/v1"
        );
        assert!(matches!(
            branch_or_commit("-x"),
            Err(GitError::UnsafeArgument)
        ));
    }

    #[tokio::test]
    async fn files_are_committed_byte_for_byte_whatever_the_attributes() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo, "main");
        let ran = tmp.path().join("filter-ran");
        let filter = tmp.path().join("filter");
        std::fs::write(
            &filter,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", ran.display()),
        )
        .unwrap();
        std::fs::set_permissions(&filter, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        std::fs::write(
            repo.join(".gitattributes"),
            "*.png filter=lfs\n*.md filter=custom ident working-tree-encoding=UTF-16\n",
        )
        .unwrap();
        g(&repo, &["add", ".gitattributes"]);
        g(&repo, &["commit", "-q", "-m", "attributes"]);
        std::fs::write(repo.join(".git/info/attributes"), "* filter=info ident\n").unwrap();
        for driver in ["lfs", "custom", "info"] {
            for key in ["clean", "smudge", "process"] {
                g(
                    &repo,
                    &[
                        "config",
                        &format!("filter.{driver}.{key}"),
                        filter.to_str().unwrap(),
                    ],
                );
            }
            g(
                &repo,
                &["config", &format!("filter.{driver}.required"), "true"],
            );
        }
        g(&repo, &["config", "user.name", "Test User"]);
        g(&repo, &["config", "user.email", "test@example.invalid"]);
        let dir = tmp.path().join("wt");
        let tip = g(&repo, &["rev-parse", "main"]);
        add_detached_worktree(&repo, &dir, &tip, "a test")
            .await
            .unwrap();
        std::fs::create_dir_all(dir.join("d")).unwrap();
        let text = "a $Id: 0 $\r\nb\n";
        std::fs::write(dir.join("d/i.md"), text).unwrap();
        std::fs::write(dir.join("d/s.png"), b"\x89PNG\r\n").unwrap();
        let commit = commit_paths(&dir, &["d/i.md", "d/s.png"], "m")
            .await
            .unwrap();
        assert_eq!(
            g(&repo, &["cat-file", "-p", &format!("{commit}:d/i.md")]),
            text.trim()
        );
        let out = std::process::Command::new("git")
            .args(["cat-file", "blob", &format!("{commit}:d/s.png")])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert_eq!(out.stdout, b"\x89PNG\r\n");
        assert_eq!(
            g(&repo, &["show", "--name-only", "--format=", &commit]),
            "d/i.md\nd/s.png"
        );
        assert!(!ran.exists(), "a filter of the repository ran");
    }

    /// A repository on `main` whose `origin` is a new bare repository; both.
    fn repo_with_bare_origin(tmp: &Path) -> (PathBuf, PathBuf) {
        let repo = tmp.join("repo");
        init_repo(&repo, "main");
        let bare = tmp.join("origin.git");
        g(
            tmp,
            &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
        );
        g(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        (repo, bare)
    }

    #[tokio::test]
    async fn a_push_refuses_option_like_names_before_git_runs() {
        let tmp = TempDir::new().unwrap();
        let (repo, bare) = repo_with_bare_origin(tmp.path());
        let tip = g(&repo, &["rev-parse", "main"]);
        for (commit, remote) in [
            (tip.as_str(), "--force"),
            (tip.as_str(), "+main"),
            (tip.as_str(), "-f"),
            ("-f", "main"),
            ("main", "main"),
        ] {
            let err = push_commit(&repo, commit, remote, Duration::from_secs(30))
                .await
                .unwrap_err();
            assert_eq!(err, PushError::Failed { status: None }, "{commit} {remote}");
        }
        assert_eq!(g(&bare, &["for-each-ref"]), "", "nothing was pushed");
    }

    #[tokio::test]
    async fn a_push_that_does_not_finish_is_stopped() {
        let tmp = TempDir::new().unwrap();
        let (repo, bare) = repo_with_bare_origin(tmp.path());
        // The remote accepts the branch once its hook has slept for 2 seconds.
        let hook = bare.join("hooks/pre-receive");
        std::fs::write(&hook, "#!/bin/sh\nsleep 2\n").unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let tip = g(&repo, &["rev-parse", "main"]);
        let started = std::time::Instant::now();
        let err = push_commit(&repo, &tip, "feedback/x", Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(err, PushError::Timeout);
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "{:?}",
            started.elapsed()
        );
        // The whole push was killed, the remote side included: the branch
        // never arrives, even after the hook would have finished.
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(g(&bare, &["for-each-ref"]), "");
    }

    #[tokio::test]
    async fn an_lfs_pointer_is_found_only_under_the_folder() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("r");
        init_repo(&dir, "main");
        let pointer = "version https://git-lfs.github.com/spec/v1\noid sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\nsize 12345\n";
        let write = |path: &str, text: &[u8]| {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        };
        write("docs/issues/x/issue.md", b"# x\n");
        write("docs/issues/x/sub/big.bin", &vec![b'v'; 4096]);
        write("elsewhere/demo.mp4", pointer.as_bytes());
        g(&dir, &["add", "-A"]);
        g(&dir, &["commit", "-q", "-m", "files"]);
        let clean = g(&dir, &["rev-parse", "HEAD"]);
        assert!(!has_lfs_pointer(&dir, &clean, "docs/issues/x")
            .await
            .unwrap());
        assert!(has_lfs_pointer(&dir, &clean, "elsewhere").await.unwrap());

        // In a subfolder too, and whatever the file is called.
        write("docs/issues/x/sub/demo.mp4", pointer.as_bytes());
        g(&dir, &["add", "-A"]);
        g(&dir, &["commit", "-q", "-m", "pointer"]);
        let with = g(&dir, &["rev-parse", "HEAD"]);
        assert!(has_lfs_pointer(&dir, &with, "docs/issues/x").await.unwrap());
        assert!(matches!(
            has_lfs_pointer(&dir, "main", "docs/issues/x").await,
            Err(GitError::UnsafeArgument)
        ));
    }

    #[test]
    fn a_push_is_never_forced_and_never_prompts() {
        let cmd = push_command(Path::new("."), "refs/heads/a:refs/heads/b");
        let args = args_of(&cmd);
        let push = args.iter().position(|a| a == "push").unwrap();
        assert_eq!(
            args[push..],
            [
                "push",
                "--no-verify",
                "--no-signed",
                "--no-follow-tags",
                "--recurse-submodules=no",
                "--",
                "origin",
                "refs/heads/a:refs/heads/b",
            ]
        );
        assert!(args.contains(&"core.hooksPath=/dev/null".to_string()));
        assert!(!args
            .iter()
            .any(|a| a.contains("force") || a.starts_with('+')));
        let envs: std::collections::HashMap<_, _> = cmd.as_std().get_envs().collect();
        assert_eq!(
            envs.get(std::ffi::OsStr::new("GIT_TERMINAL_PROMPT"))
                .copied(),
            Some(Some(std::ffi::OsStr::new("0")))
        );
    }

    #[test]
    fn push_failures_are_classified_from_gits_messages() {
        let cases = [
            (" ! [rejected]        main -> x (non-fast-forward)", PushError::Rejected),
            (
                " ! [remote rejected] main -> x (pre-receive hook declined)",
                PushError::Rejected,
            ),
            (
                "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
                PushError::Auth,
            ),
            ("remote: Invalid username or password.", PushError::Auth),
            ("fatal: Authentication failed for 'https://github.com/o/r.git/'", PushError::Auth),
            ("git@github.com: Permission denied (publickey).", PushError::Auth),
            (
                "fatal: unable to access 'https://x/': The requested URL returned error: 403",
                PushError::Auth,
            ),
            (
                "fatal: 'origin' does not appear to be a git repository",
                PushError::NoRemote,
            ),
            (
                "fatal: '/x/LEAK' does not appear to be a git repository",
                PushError::Failed { status: Some(128) },
            ),
        ];
        for (output, expected) in cases {
            assert_eq!(
                classify_push_failure(output, Some(128)),
                expected,
                "{output}"
            );
        }
    }

    #[test]
    fn git_errors_of_the_feedback_tool_hold_no_output() {
        assert_eq!(
            GitError::Failed { status: Some(128) }.to_string(),
            "git failed (exit status 128)"
        );
        assert_eq!(
            CommitError::Failed { status: None }.to_string(),
            "git could not commit (no exit status)"
        );
    }

    #[tokio::test]
    async fn validate_repo_returns_root_and_default_branch() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path(), "main");
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let info = validate_repo(&sub).await.unwrap();
        assert_eq!(canon(&info.root), canon(tmp.path()));
        assert_eq!(info.default_branch, "main");
    }

    #[tokio::test]
    async fn validate_repo_uses_origin_head_when_present() {
        let tmp = TempDir::new().unwrap();
        let upstream = tmp.path().join("up");
        init_repo(&upstream, "trunk");
        let clone = tmp.path().join("clone");
        g(
            tmp.path(),
            &["clone", upstream.to_str().unwrap(), clone.to_str().unwrap()],
        );
        g(&clone, &["checkout", "-b", "feature"]);
        let info = validate_repo(&clone).await.unwrap();
        assert_eq!(info.default_branch, "trunk");
    }

    #[tokio::test]
    async fn validate_repo_handles_spaces_and_unicode_paths() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("répo é 1");
        init_repo(&repo, "main");
        let info = validate_repo(&repo).await.unwrap();
        assert_eq!(canon(&info.root), canon(&repo));
        assert_eq!(info.default_branch, "main");
    }

    #[tokio::test]
    async fn validate_repo_rejects_non_git_dir() {
        let tmp = TempDir::new().unwrap();
        let err = validate_repo(tmp.path()).await.unwrap_err();
        assert!(matches!(err, GitError::NotAGitRepo), "{err:?}");
    }

    #[tokio::test]
    async fn validate_repo_rejects_a_path_that_is_not_a_directory() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("nope");
        let err = validate_repo(&missing).await.unwrap_err();
        assert!(
            matches!(&err, GitError::PathNotFound(p) if *p == missing),
            "{err:?}"
        );
        let file = tmp.path().join("file.txt");
        std::fs::write(&file, "x").unwrap();
        let err = validate_repo(&file).await.unwrap_err();
        assert!(
            matches!(&err, GitError::PathNotFound(p) if *p == file),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn validate_repo_rejects_bare_repo() {
        let tmp = TempDir::new().unwrap();
        g(tmp.path(), &["init", "--bare", "-b", "main"]);
        let err = validate_repo(tmp.path()).await.unwrap_err();
        assert!(matches!(err, GitError::BareRepo), "{err:?}");
    }

    #[tokio::test]
    async fn validate_repo_detached_head_without_main_errors() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path(), "trunk");
        g(tmp.path(), &["checkout", "--detach"]);
        let err = validate_repo(tmp.path()).await.unwrap_err();
        assert!(matches!(err, GitError::NoDefaultBranch), "{err:?}");
    }

    #[tokio::test]
    async fn validate_repo_detached_head_with_local_main_returns_main() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path(), "main");
        g(tmp.path(), &["checkout", "--detach"]);
        let info = validate_repo(tmp.path()).await.unwrap();
        assert_eq!(info.default_branch, "main");
    }

    #[tokio::test]
    async fn create_worktree_checks_out_new_branch_from_base() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo, "main");
        let wts = tmp.path().join("wts").join("nested");
        let dir = create_worktree(&repo, &wts, "agent/x", "main")
            .await
            .unwrap();
        assert_eq!(dir, wts.join("agent-x"));
        assert!(dir.is_dir());
        assert_eq!(g(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), "agent/x");
        // The main checkout is untouched.
        assert_eq!(g(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
    }

    #[tokio::test]
    async fn create_worktree_existing_branch_errors() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo, "main");
        let wts = tmp.path().join("wts");
        create_worktree(&repo, &wts, "agent/x", "main")
            .await
            .unwrap();
        let err = create_worktree(&repo, &wts, "agent/x", "main")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, GitError::BranchExists(b) if b == "agent/x"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn remove_worktree_deletes_directory() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo, "main");
        let dir = create_worktree(&repo, &tmp.path().join("wts"), "agent/y", "main")
            .await
            .unwrap();
        std::fs::write(dir.join("dirty.txt"), "uncommitted").unwrap();
        remove_worktree(&repo, &dir).await.unwrap();
        assert!(!dir.exists());
    }
}

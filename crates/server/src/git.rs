//! Repository validation and worktree management via the `git` CLI.
//!
//! All commands are spawned directly (never through a shell) with arguments
//! passed separately, so paths containing spaces or unicode need no escaping.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Output, Stdio};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
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

/// Why [`push_branch`] failed, classified from git's C-locale messages, which
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

// What follows is used by the feedback tool in a user's repository. Every
// command is isolated (see [`isolated_command`]); none returns or logs what
// git printed; a value from the user is passed after `--` or where git cannot
// read it as an option, and one starting with `-` is refused before git runs.

/// Configuration given ahead of the repository's own: no hook, file-system
/// monitor or signing program runs, and no background maintenance starts.
const ISOLATED_CONFIG: [&str; 10] = [
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
];

/// [`git_command`] with [`ISOLATED_CONFIG`], no editor (`GIT_EDITOR=:` is
/// git's "start none") and no program that asks a person for a credential:
/// an empty `GIT_ASKPASS` stops git looking for one, `SSH_ASKPASS` is
/// removed, and Git Credential Manager is told not to open a window.
fn isolated_command(cwd: &Path, args: &[&str]) -> Command {
    let all: Vec<&str> = ISOLATED_CONFIG.iter().chain(args).copied().collect();
    let mut cmd = git_command(cwd, &all);
    cmd.env("GIT_EDITOR", ":")
        .env("GIT_ASKPASS", "")
        .env("GCM_INTERACTIVE", "never")
        .env_remove("SSH_ASKPASS");
    cmd
}

async fn isolated(cwd: &Path, args: &[&str]) -> Result<Output, GitError> {
    Ok(isolated_command(cwd, args).output().await?)
}

/// Runs an isolated command and keeps only whether it succeeded.
async fn isolated_ok(cwd: &Path, args: &[&str]) -> Result<(), GitError> {
    let out = isolated(cwd, args).await?;
    if out.status.success() {
        Ok(())
    } else {
        Err(GitError::Failed {
            status: out.status.code(),
        })
    }
}

fn refuse_option_like(value: &str) -> Result<(), GitError> {
    if value.starts_with('-') {
        Err(GitError::UnsafeArgument)
    } else {
        Ok(())
    }
}

/// Whether `path` (a file or a folder) exists on the local branch `branch`:
/// `git cat-file -e refs/heads/<branch>:<path>`.
pub async fn path_in_branch(repo_root: &Path, branch: &str, path: &str) -> Result<bool, GitError> {
    refuse_option_like(branch)?;
    let object = format!("refs/heads/{branch}:{path}");
    Ok(isolated(repo_root, &["cat-file", "-e", &object])
        .await?
        .status
        .success())
}

/// `remote.origin.url` as configured (`git config --get`), not rewritten by
/// `url.<base>.insteadOf` as `git remote get-url` would. `None` when unset.
pub async fn origin_url(repo_root: &Path) -> Result<Option<String>, GitError> {
    let out = isolated(repo_root, &["config", "--get", "remote.origin.url"]).await?;
    match out.status.code() {
        Some(0) => Ok(Some(stdout_of(&out)).filter(|url| !url.is_empty())),
        // The key is not set.
        Some(1) => Ok(None),
        status => Err(GitError::Failed { status }),
    }
}

/// The id of the commit `rev` names; `None` when it names no commit.
pub async fn rev_parse(repo_root: &Path, rev: &str) -> Result<Option<String>, GitError> {
    refuse_option_like(rev)?;
    let commit = format!("{rev}^{{commit}}");
    let out = isolated(repo_root, &["rev-parse", "--verify", "--quiet", &commit]).await?;
    match out.status.code() {
        Some(0) => Ok(Some(stdout_of(&out)).filter(|id| !id.is_empty())),
        Some(1) => Ok(None),
        status => Err(GitError::Failed { status }),
    }
}

/// Forgets the worktrees whose directory is gone: `git worktree prune`.
pub async fn prune_worktrees(repo_root: &Path) -> Result<(), GitError> {
    isolated_ok(repo_root, &["worktree", "prune"]).await
}

/// [`prune_worktrees`] for a `Drop`: blocking, and only whether it worked.
pub(crate) fn prune_worktrees_blocking(repo_root: &Path) -> bool {
    isolated_command(repo_root, &["worktree", "prune"])
        .into_std()
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Deletes the local branch `branch`, merged or not: `git branch -D`.
pub async fn delete_branch(repo_root: &Path, branch: &str) -> Result<(), GitError> {
    refuse_option_like(branch)?;
    isolated_ok(repo_root, &["branch", "-D", "--", branch]).await
}

/// Creates the local branch `branch` at `commit`, without tracking anything.
/// Fails when the branch exists.
pub async fn create_branch(repo_root: &Path, branch: &str, commit: &str) -> Result<(), GitError> {
    refuse_option_like(branch)?;
    refuse_option_like(commit)?;
    isolated_ok(repo_root, &["branch", "--no-track", "--", branch, commit]).await
}

/// Adds a worktree at `dir` (which must not exist) with `HEAD` detached at
/// `commit` and the index read from it, but no file checked out: a commit
/// made there keeps every other file of `commit`, and nothing the repository
/// holds (a checkout hook, a smudge filter, a symbolic link) runs or is
/// followed while the worktree is made or written to.
pub async fn add_detached_worktree(
    repo_root: &Path,
    dir: &Path,
    commit: &str,
) -> Result<(), GitError> {
    refuse_option_like(commit)?;
    let dir_arg = dir.to_string_lossy().into_owned();
    isolated_ok(
        repo_root,
        &[
            "worktree",
            "add",
            "--detach",
            "--no-checkout",
            "--",
            &dir_arg,
            commit,
        ],
    )
    .await?;
    isolated_ok(dir, &["read-tree", "HEAD"]).await
}

/// What git prints, in the C locale, when it knows no identity to commit as.
const IDENTITY_MISSING: &str = "Please tell me who you are";

/// `git add -- <paths>` and `git commit --no-verify -m <message>` in
/// `worktree`, then the new commit's id.
///
/// Ignore rules do not drop a path (`--force`). The author is the identity
/// configured for the repository, never one git guesses from the user and
/// host names (`user.useConfigOnly`). The commit is never signed: a signing
/// program could wait for a passphrase no one is there to type.
/// `--cleanup=whitespace` keeps a configured comment character from eating
/// the message.
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
    let mut add = vec!["add", "--force", "--"];
    add.extend_from_slice(paths);
    let out = isolated(worktree, &add).await.map_err(commit_io)?;
    if !out.status.success() {
        return Err(CommitError::Failed {
            status: out.status.code(),
        });
    }
    let commit = [
        "-c",
        "user.useConfigOnly=true",
        "commit",
        "--quiet",
        "--no-verify",
        "--no-gpg-sign",
        "--cleanup=whitespace",
        "-m",
        message,
    ];
    let out = isolated(worktree, &commit).await.map_err(commit_io)?;
    if !out.status.success() {
        return Err(
            if String::from_utf8_lossy(&out.stderr).contains(IDENTITY_MISSING) {
                CommitError::IdentityMissing
            } else {
                CommitError::Failed {
                    status: out.status.code(),
                }
            },
        );
    }
    let out = isolated(worktree, &["rev-parse", "--verify", "HEAD^{commit}"])
        .await
        .map_err(commit_io)?;
    let id = stdout_of(&out);
    if !out.status.success() || id.is_empty() {
        return Err(CommitError::Failed {
            status: out.status.code(),
        });
    }
    Ok(id)
}

fn commit_io(e: GitError) -> CommitError {
    match e {
        GitError::Io(e) => CommitError::Io(e),
        _ => CommitError::Failed { status: None },
    }
}

/// The most of each of git's output streams a push keeps to classify a
/// failure; the rest is read and dropped.
const PUSH_OUTPUT_MAX: u64 = 64 * 1024;
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

/// `git push origin refs/heads/<local>:refs/heads/<remote_branch>`, at most
/// `timeout` long.
///
/// Never forced, and the repository's settings add nothing: no hook,
/// signature, tag or submodule. A credential that is not stored fails at once
/// (`GIT_TERMINAL_PROMPT=0` and no askpass program) instead of waiting for a
/// person. git runs in a process group of its own, killed whole when the time
/// is up or the future is dropped. Both names must be valid branch names that
/// start with neither `-` nor `+`, else `Failed` with no exit status before
/// git runs.
pub async fn push_branch(
    repo_root: &Path,
    local: &str,
    remote_branch: &str,
    timeout: Duration,
) -> Result<(), PushError> {
    for name in [local, remote_branch] {
        let valid = !name.starts_with(['-', '+'])
            && is_valid_branch_name(repo_root, name).await.unwrap_or(false);
        if !valid {
            return Err(PushError::Failed { status: None });
        }
    }
    match origin_url(repo_root).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(PushError::NoRemote),
        Err(GitError::Failed { status }) => return Err(PushError::Failed { status }),
        Err(_) => return Err(PushError::Failed { status: None }),
    }
    let refspec = format!("refs/heads/{local}:refs/heads/{remote_branch}");
    match run_with_timeout(push_command(repo_root, &refspec), timeout).await {
        Ok(Some((status, _))) if status.success() => Ok(()),
        Ok(Some((status, output))) => Err(classify_push_failure(&output, status.code())),
        Ok(None) => Err(PushError::Timeout),
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

/// Runs `cmd` with stdin closed, in a process group of its own, for at most
/// `timeout`. `None` when the time ran out (the group is killed); else the
/// exit status and the start of stdout and stderr.
async fn run_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> std::io::Result<Option<(ExitStatus, String)>> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn()?;
    // Declared after `child`, so it is dropped first: the group is killed
    // while its leader is not yet reaped and its id cannot be reused.
    let mut group = GroupKiller(child.id());
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let run = async {
        let (out, err, status) =
            tokio::join!(read_capped(stdout), read_capped(stderr), child.wait());
        status.map(|status| (status, out + &err))
    };
    match tokio::time::timeout(timeout, run).await {
        Ok(Ok(done)) => {
            group.0 = None;
            Ok(Some(done))
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Ok(None),
    }
}

async fn read_capped<R: AsyncRead + Unpin>(pipe: Option<R>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut kept = Vec::new();
    let _ = (&mut pipe)
        .take(PUSH_OUTPUT_MAX)
        .read_to_end(&mut kept)
        .await;
    let _ = tokio::io::copy(&mut pipe, &mut tokio::io::sink()).await;
    String::from_utf8_lossy(&kept).into_owned()
}

/// Kills the process group led by the process it names when dropped, unless
/// cleared first.
struct GroupKiller(Option<u32>);

impl Drop for GroupKiller {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self
            .0
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            // The group may be gone already.
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}

/// The type of the object `object` (`<rev>:<path>`) names, `None` when it
/// names nothing.
async fn object_type(repo_root: &Path, object: &str) -> Result<Option<String>, GitError> {
    let out = isolated(repo_root, &["cat-file", "-t", object]).await?;
    Ok(out.status.success().then(|| stdout_of(&out)))
}

/// The file `path` at `rev` as text (`git cat-file blob <rev>:<path>`): at
/// most `max_bytes` of it, cut at a character boundary. `None` when `rev`
/// names nothing or holds no file at `path`.
pub async fn show_file(
    repo_root: &Path,
    rev: &str,
    path: &str,
    max_bytes: usize,
) -> Result<Option<String>, GitError> {
    refuse_option_like(rev)?;
    let object = format!("{rev}:{path}");
    if object_type(repo_root, &object).await?.as_deref() != Some("blob") {
        return Ok(None);
    }
    let mut child = isolated_command(repo_root, &["cat-file", "blob", &object])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut bytes = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let limit = u64::try_from(max_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        stdout.take(limit).read_to_end(&mut bytes).await?;
    }
    let cut = bytes.len() > max_bytes;
    if cut {
        bytes.truncate(max_bytes);
        // The rest is not wanted.
        let _ = child.start_kill();
        let _ = child.wait().await;
    } else {
        let status = child.wait().await?;
        if !status.success() {
            return Err(GitError::Failed {
                status: status.code(),
            });
        }
    }
    if cut {
        // Drop a character the cut split.
        if let Err(e) = std::str::from_utf8(&bytes) {
            if e.error_len().is_none() {
                bytes.truncate(e.valid_up_to());
            }
        }
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// The names of the files directly in `folder` at `rev`, in git's order:
/// no subfolder, no submodule. Empty when `rev` names nothing or holds no
/// folder at `folder`.
pub async fn list_folder(
    repo_root: &Path,
    rev: &str,
    folder: &str,
) -> Result<Vec<String>, GitError> {
    refuse_option_like(rev)?;
    let object = format!("{rev}:{folder}");
    if object_type(repo_root, &object).await?.as_deref() != Some("tree") {
        return Ok(Vec::new());
    }
    let out = isolated(repo_root, &["ls-tree", "-z", &object]).await?;
    if !out.status.success() {
        return Err(GitError::Failed {
            status: out.status.code(),
        });
    }
    // Each entry: `<mode> SP <type> SP <id> TAB <name>`, ended by NUL.
    Ok(out
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
        assert_eq!(val("SSH_ASKPASS"), Some(None));
        assert_eq!(val("GCM_INTERACTIVE"), set("never"));
        assert_eq!(val("GIT_TERMINAL_PROMPT"), set("0"));
        assert_eq!(val("LC_ALL"), set("C"));
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

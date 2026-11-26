//! The git side of a feedback submission: where the issue folder goes, the
//! commit made in a temporary worktree, and the push.
//!
//! The commit is made in a worktree of its own inside the scratch directory
//! the caller gives (a directory of its own directly under
//! [`Paths::feedback_tmp_dir`]), detached at the default branch's tip, locked
//! with this process's id, and with no file checked out. The user's
//! checkout, index and current branch are never touched; nothing the
//! repository holds or configures (hooks, attributes and filters, a symbolic
//! link, signing) runs, is followed or changes the files; and git is never
//! told to prune, so no worktree of the user's loses its data. The branch is
//! created only once its commit exists, so no failure leaves an empty branch
//! behind. Every git command is bounded in time (see `git`). Nothing git
//! prints is kept: a failure is one of [`FeedbackError`]'s fixed sentences,
//! or a [`FeedbackFailure`] with a code.
//!
//! [`Paths::feedback_tmp_dir`]: crate::paths::Paths::feedback_tmp_dir

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::NaiveDate;
use smithagents_core::agent::Repo;
use smithagents_core::feedback::{FeedbackFailure, FeedbackStep};

use super::issue::{branch_name, folder_path, issue_slug, with_suffix};
use super::FeedbackError;
use crate::git::{self, exit_status_text, CommitError, GitError, PushError};
use crate::paths::ensure_private_real_dir;

/// The longest a push may take.
pub const PUSH_TIMEOUT: Duration = Duration::from_secs(120);
/// The highest clash suffix tried.
const MAX_SUFFIX: u32 = 99;
const WORKTREE_STEP: &str = "create the worktree";
const COMMIT_STEP: &str = "commit the files";
/// The temporary worktree's directory inside the scratch directory.
const WORKTREE_DIR: &str = "wt";
const FILE_MODE: u32 = 0o644;
/// The start of the lock reason of this tool's worktrees; the process id
/// follows.
const LOCK_REASON: &str = "smithagents feedback pid";

/// Where a submission's files go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub slug: String,
    /// `docs/issues/<date>-<slug>`.
    pub folder: String,
    /// `feedback/<date>-<slug>`, or `idea/<slug>` for an idea.
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSource {
    Text(String),
    /// A file on disk (an upload), moved into the folder: it is removed once
    /// the commit is made, and kept when it is not.
    Staged(PathBuf),
}

/// One file of the issue folder; `name` is a plain file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderFile {
    pub name: String,
    pub source: FileSource,
}

/// The first suffix 1..=99 whose folder is absent from the default branch
/// and whose branch does not exist. Nothing is created. A file where the
/// folders go (`docs`, `docs/issues`) is `FolderBlocked`; a branch named like
/// the start of the branch (`feedback`, `idea`) is `BranchBlocked`.
pub async fn place(
    repo: &Repo,
    title: &str,
    date: NaiveDate,
    idea: bool,
) -> Result<Placement, FeedbackError> {
    require_default_branch(repo).await?;
    let root = repo.path.as_path();
    let default = format!("refs/heads/{}", repo.default_branch);
    let slug = issue_slug(title);
    for n in 1..=MAX_SUFFIX {
        let slug = with_suffix(&slug, n);
        let placement = Placement {
            folder: folder_path(date, &slug),
            branch: branch_name(date, &slug, idea),
            slug,
        };
        check_names(&placement, &[])?;
        if n == 1 {
            // The same for every suffix.
            check_parents(root, &default, &placement).await?;
        }
        if !name_taken(root, &default, &placement).await? {
            check_branch(root, &placement.branch).await?;
            return Ok(placement);
        }
    }
    Err(FeedbackError::NoFreeName)
}

/// [`place`] then [`commit_folder`]; when the name was taken in between (the
/// same title submitted twice at once), places again and retries once, which
/// gives the next suffix.
pub async fn place_and_commit(
    repo: &Repo,
    title: &str,
    date: NaiveDate,
    idea: bool,
    files: &[FolderFile],
    scratch: &Path,
) -> Result<(Placement, String), FeedbackError> {
    let mut retried = false;
    loop {
        let placement = place(repo, title, date, idea).await?;
        match commit_folder(repo, &placement, files, title, scratch).await {
            Ok(commit) => return Ok((placement, commit)),
            Err(FeedbackError::NameTaken) if !retried => retried = true,
            Err(e) => return Err(e),
        }
    }
}

/// Creates `placement.branch` from the default branch in `<scratch>/wt`,
/// writes the folder, commits `docs(issues): <title>`, removes the worktree,
/// and returns the commit's id.
///
/// `NameTaken` when the branch or the folder appeared since `place` chose
/// them (see [`place_and_commit`]). On every failure the worktree is removed
/// and no branch is created; `Staged` files stay where they were. When the
/// future is dropped, the git command running is killed with its process
/// group and `Drop` removes the worktree's directory, its registration and a
/// branch lock file the killed git left; a commit object may stay behind,
/// unreferenced, for git's garbage collection.
///
/// `scratch` must be a directory of its own directly under
/// [`Paths::feedback_tmp_dir`]: it is made owner-only (a symbolic link or a
/// directory of another user is refused), and a worktree this tool left
/// under that directory in a run that crashed is removed. Every file of the
/// folder gets mode 0644 and is committed byte for byte.
///
/// [`Paths::feedback_tmp_dir`]: crate::paths::Paths::feedback_tmp_dir
pub async fn commit_folder(
    repo: &Repo,
    placement: &Placement,
    files: &[FolderFile],
    title: &str,
    scratch: &Path,
) -> Result<String, FeedbackError> {
    let root = repo.path.as_path();
    check_names(placement, files)?;
    check_branch(root, &placement.branch).await?;
    require_default_branch(repo).await?;
    ensure_private_real_dir(scratch)?;
    let scratch = std::fs::canonicalize(scratch)?;

    // Armed before anything is created: a panic or a dropped future removes
    // the worktree in `Drop`; every other way out goes through `remove`.
    let worktree = TempWorktree::new(root, scratch.join(WORKTREE_DIR), git::local_timeout());
    let committed = commit_in_worktree(repo, &worktree, placement, files, title).await;
    worktree.remove().await;
    if committed.is_ok() {
        for file in files {
            if let FileSource::Staged(from) = &file.source {
                // The upload is in the commit now.
                let _ = tokio::fs::remove_file(from).await;
            }
        }
    }
    committed
}

/// The part of [`commit_folder`] that makes and uses the worktree.
async fn commit_in_worktree(
    repo: &Repo,
    worktree: &TempWorktree,
    placement: &Placement,
    files: &[FolderFile],
    title: &str,
) -> Result<String, FeedbackError> {
    let root = repo.path.as_path();
    let dir = worktree.dir.as_path();
    remove_leftovers(root, dir).await?;
    let base = git::rev_parse(root, &format!("refs/heads/{}", repo.default_branch))
        .await
        .map_err(git_failed(WORKTREE_STEP))?
        .ok_or_else(|| FeedbackError::DefaultBranchMissing(repo.default_branch.clone()))?;
    check_parents(root, &base, placement).await?;
    if name_taken(root, &base, placement).await? {
        return Err(FeedbackError::NameTaken);
    }
    worktree.registered.store(true, Ordering::SeqCst);
    git::add_detached_worktree(root, dir, &base, &lock_reason())
        .await
        .map_err(git_failed(WORKTREE_STEP))?;

    let paths = write_folder(worktree, &placement.folder, files).await?;
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    let message = format!("docs(issues): {}", one_line(title));
    let commit = git::commit_paths(dir, &paths, &message)
        .await
        .map_err(|e| match e {
            CommitError::IdentityMissing => FeedbackError::GitIdentityMissing,
            CommitError::Failed { status } => FeedbackError::Git {
                step: COMMIT_STEP,
                status,
            },
            CommitError::Io(e) => FeedbackError::Io(e),
        })?;
    create_branch(root, worktree, &placement.branch, &commit).await?;
    Ok(commit)
}

/// Creates the branch at `commit` (only now that its commit exists). A lock
/// file a killed git left is removed; a branch that appeared meanwhile is
/// `NameTaken`.
async fn create_branch(
    root: &Path,
    worktree: &TempWorktree,
    branch: &str,
    commit: &str,
) -> Result<(), FeedbackError> {
    let refs = git::common_dir(root)
        .await
        .map_err(git_failed(COMMIT_STEP))?
        .join("refs")
        .join("heads");
    let lock = refs.join(format!("{branch}.lock"));
    worktree.hold_branch_lock(BranchLock {
        existed: lock.exists(),
        path: lock,
        commit: commit.to_string(),
    });
    let created = git::create_branch(root, branch, commit).await;
    let lock = worktree.take_branch_lock();
    let failure = match created {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    let killed = matches!(
        failure,
        GitError::TimedOut | GitError::Io(_) | GitError::Failed { status: None }
    );
    if killed {
        if let Some(lock) = lock {
            lock.release();
        }
    }
    let now = git::rev_parse(root, &format!("refs/heads/{branch}"))
        .await
        .map_err(git_failed(COMMIT_STEP))?;
    match now {
        // git was stopped after it had made the branch. (Only then: two
        // submissions of the same files in the same second make the same
        // commit, and the other one's branch is not this one's.)
        Some(id) if killed && id == commit => Ok(()),
        Some(_) => Err(FeedbackError::NameTaken),
        None => Err(git_failed(COMMIT_STEP)(failure)),
    }
}

/// Removes this tool's worktrees left by runs that crashed: the one at
/// `own` (this scratch directory's), and those under the feedback directory
/// (`own`'s grandparent) locked by another process of this tool. A worktree
/// of anyone else is never touched, and nothing is pruned.
async fn remove_leftovers(root: &Path, own: &Path) -> Result<(), FeedbackError> {
    let feedback_dir = own.parent().and_then(Path::parent).unwrap_or(own);
    for entry in git::list_worktrees(root)
        .await
        .map_err(git_failed(WORKTREE_STEP))?
    {
        let crashed = entry.path.starts_with(feedback_dir)
            && entry.locked.as_deref().is_some_and(locked_by_another_run);
        if entry.path == own || crashed {
            if let Err(e) = git::remove_worktree_at(root, &entry.path).await {
                tracing::warn!("could not remove a feedback worktree a crashed run left: {e}");
            }
        }
    }
    remove_path(own).await?;
    Ok(())
}

/// The lock reason of a worktree this process makes.
fn lock_reason() -> String {
    format!("{LOCK_REASON} {}", std::process::id())
}

/// Whether `reason` locks a worktree this tool made in another process: one
/// that crashed (this process's own worktrees may be in use).
fn locked_by_another_run(reason: &str) -> bool {
    reason
        .strip_prefix(LOCK_REASON)
        .and_then(|pid| pid.trim().parse::<u32>().ok())
        .is_some_and(|pid| pid != std::process::id())
}

/// Pushes the local branch `local` to `origin` as `remote_branch`, never
/// forced, for at most [`PUSH_TIMEOUT`]. A failure is step `Push` with a
/// fixed sentence; the branch and its commit stay for a retry.
pub async fn push(repo: &Repo, local: &str, remote_branch: &str) -> Result<(), FeedbackFailure> {
    git::push_branch(&repo.path, local, remote_branch, PUSH_TIMEOUT)
        .await
        .map_err(push_failure)
}

fn push_failure(error: PushError) -> FeedbackFailure {
    let (code, message) = match error {
        PushError::NoRemote => (
            "push_no_remote",
            "git could not push: the repository has no remote named origin".to_string(),
        ),
        PushError::Auth => (
            "push_auth",
            "git could not push to origin: authentication failed".to_string(),
        ),
        PushError::Rejected => (
            "push_rejected",
            "git could not push to origin: the remote rejected the branch".to_string(),
        ),
        PushError::Timeout => (
            "push_timeout",
            format!(
                "git did not finish pushing within {} seconds",
                PUSH_TIMEOUT.as_secs()
            ),
        ),
        PushError::Failed { status } => (
            "push_failed",
            format!(
                "git could not push to origin ({})",
                exit_status_text(&status)
            ),
        ),
    };
    FeedbackFailure {
        step: FeedbackStep::Push,
        code: code.to_string(),
        message,
    }
}

/// `DefaultBranchMissing` unless the repository's default branch exists as a
/// local branch (an unborn repository has none).
async fn require_default_branch(repo: &Repo) -> Result<(), FeedbackError> {
    let name = &repo.default_branch;
    let exists = !name.starts_with('-')
        && git::bounded(git::branch_exists(&repo.path, name))
            .await
            .map_err(git_failed(WORKTREE_STEP))?;
    if exists {
        Ok(())
    } else {
        Err(FeedbackError::DefaultBranchMissing(name.clone()))
    }
}

/// Whether the folder exists at `rev` or the branch exists.
async fn name_taken(root: &Path, rev: &str, placement: &Placement) -> Result<bool, FeedbackError> {
    let folder = git::path_kind(root, rev, &placement.folder)
        .await
        .map_err(git_failed(WORKTREE_STEP))?;
    Ok(folder.is_some()
        || git::bounded(git::branch_exists(root, &placement.branch))
            .await
            .map_err(git_failed(WORKTREE_STEP))?)
}

/// Refuses a folder whose parent is not a folder at `rev` (a file named
/// `docs` or `docs/issues` would be replaced by the commit), and a branch
/// whose parent is a branch (`feedback` blocks `feedback/<x>`).
async fn check_parents(root: &Path, rev: &str, placement: &Placement) -> Result<(), FeedbackError> {
    for parent in parents(&placement.folder) {
        let kind = git::path_kind(root, rev, parent)
            .await
            .map_err(git_failed(WORKTREE_STEP))?;
        if kind.is_some_and(|kind| kind != "tree") {
            return Err(FeedbackError::FolderBlocked(parent.to_string()));
        }
    }
    for parent in parents(&placement.branch) {
        if git::bounded(git::branch_exists(root, parent))
            .await
            .map_err(git_failed(WORKTREE_STEP))?
        {
            return Err(FeedbackError::BranchBlocked(parent.to_string()));
        }
    }
    Ok(())
}

/// `a`, `a/b` for `a/b/c`.
fn parents(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(move |(at, _)| &path[..at])
}

/// Maps a git failure to `Git { step, .. }`; git's words are never kept.
fn git_failed(step: &'static str) -> impl Fn(GitError) -> FeedbackError {
    move |e| match e {
        GitError::Io(e) => FeedbackError::Io(e),
        GitError::UnsafeArgument => unsafe_name(),
        GitError::Failed { status } => FeedbackError::Git { step, status },
        _ => FeedbackError::Git { step, status: None },
    }
}

fn unsafe_name() -> FeedbackError {
    FeedbackError::Io(io::Error::new(
        io::ErrorKind::InvalidInput,
        "a folder, branch or file name is not safe to use",
    ))
}

/// A plain name: a letter or digit, then letters, digits, `.`, `_` and `-`.
/// Never empty, `.`, `..`, hidden, an option or a path.
fn is_plain_name(name: &str) -> bool {
    name.len() <= 255
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Refuses, before git runs, a folder that is not a relative path of plain
/// names, a branch that starts with `-`, and a file name that is not plain
/// or comes twice.
fn check_names(placement: &Placement, files: &[FolderFile]) -> Result<(), FeedbackError> {
    let folder_ok = placement.folder.split('/').all(is_plain_name);
    let files_ok = files.iter().enumerate().all(|(i, file)| {
        is_plain_name(&file.name) && files[..i].iter().all(|other| other.name != file.name)
    });
    if folder_ok && files_ok && !placement.branch.starts_with('-') {
        Ok(())
    } else {
        Err(unsafe_name())
    }
}

/// Refuses a branch name git would not take (`git::is_valid_branch_name`).
async fn check_branch(repo_root: &Path, branch: &str) -> Result<(), FeedbackError> {
    let valid = git::bounded(git::is_valid_branch_name(repo_root, branch))
        .await
        .map_err(git_failed(WORKTREE_STEP))?;
    if valid {
        Ok(())
    } else {
        Err(unsafe_name())
    }
}

/// The title on one line: controls as spaces, format characters (Unicode
/// category Cf: bidirectional overrides, zero-width characters, …) removed,
/// runs of white space as one.
fn one_line(title: &str) -> String {
    let spaced: String = title
        .chars()
        .filter(|c| !is_format_char(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `c` is in Unicode's general category Cf (format).
fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{600}'..='\u{605}'
            | '\u{61c}'
            | '\u{6dd}'
            | '\u{70f}'
            | '\u{890}'..='\u{891}'
            | '\u{8e2}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

/// Writes the folder in the worktree, on the blocking pool and under the
/// worktree's gate; returns the paths to commit. The worktree has no file
/// checked out, so nothing on the way is a link the repository put there.
async fn write_folder(
    worktree: &TempWorktree,
    folder: &str,
    files: &[FolderFile],
) -> Result<Vec<String>, FeedbackError> {
    let gate = Arc::clone(&worktree.gate);
    let dir = worktree.dir.join(folder);
    let folder = folder.to_string();
    let files = files.to_vec();
    let written = tokio::task::spawn_blocking(move || {
        let stopped = gate.lock().unwrap_or_else(PoisonError::into_inner);
        if *stopped {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "the commit was stopped",
            ));
        }
        std::fs::create_dir_all(&dir)?;
        let mut paths = Vec::with_capacity(files.len());
        for file in &files {
            let to = dir.join(&file.name);
            match &file.source {
                FileSource::Text(text) => std::fs::write(&to, text)?,
                FileSource::Staged(from) => link_staged(from, &to)?,
            }
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(FILE_MODE))?;
            paths.push(format!("{folder}/{}", file.name));
        }
        Ok(paths)
    })
    .await
    .map_err(|_| io::Error::other("writing the issue folder stopped"))?;
    Ok(written?)
}

/// Puts the staged file `from` at `to` (a hard link, else a copy) and keeps
/// it, so a retry can use it again. Only a regular file is taken, never what
/// a link points to.
fn link_staged(from: &Path, to: &Path) -> io::Result<()> {
    if !std::fs::symlink_metadata(from)?.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a staged file is not a regular file",
        ));
    }
    if std::fs::hard_link(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map(drop)
}

/// Removes `path` (a directory, a file or a link, never followed); absent is
/// fine.
async fn remove_path(path: &Path) -> io::Result<()> {
    let removed = match tokio::fs::symlink_metadata(path).await {
        Ok(meta) if meta.is_dir() => tokio::fs::remove_dir_all(path).await,
        Ok(_) => tokio::fs::remove_file(path).await,
        Err(e) => Err(e),
    };
    match removed {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The lock file of the branch being created, and whether it is this run's.
struct BranchLock {
    path: PathBuf,
    /// It was there before this run's git started: someone else's.
    existed: bool,
    commit: String,
}

impl BranchLock {
    /// Removes the lock file when this run's git left it: it was not there
    /// before, and holds nothing yet or this run's commit id.
    fn release(&self) {
        if self.existed {
            return;
        }
        if let Ok(content) = std::fs::read(&self.path) {
            let content = content.trim_ascii();
            if content.is_empty() || content == self.commit.as_bytes() {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

/// The temporary worktree. [`TempWorktree::remove`] removes its registration
/// (`git worktree remove --force --force`) and its directory; when it is not
/// reached (a panic, the future being dropped) `Drop` does the same,
/// blocking, and also removes a branch lock file this run's killed git left.
struct TempWorktree {
    repo_root: PathBuf,
    /// Canonical, as git lists it.
    dir: PathBuf,
    removed: bool,
    /// Set before git is asked to add the worktree.
    registered: AtomicBool,
    /// Held while the branch is created.
    branch_lock: Mutex<Option<BranchLock>>,
    /// Held while the folder is written on the blocking pool, where a write
    /// goes on after its future is dropped; `true` once `Drop` has run, so a
    /// write that had not started does nothing. `Drop` takes it before
    /// removing anything, so nothing is written again after the removal.
    gate: Arc<Mutex<bool>>,
    /// The time each git command of the cleanup may take.
    timeout: Duration,
}

impl TempWorktree {
    fn new(repo_root: &Path, dir: PathBuf, timeout: Duration) -> Self {
        TempWorktree {
            repo_root: repo_root.to_path_buf(),
            dir,
            removed: false,
            registered: AtomicBool::new(false),
            branch_lock: Mutex::new(None),
            gate: Arc::default(),
            timeout,
        }
    }

    fn hold_branch_lock(&self, lock: BranchLock) {
        *self
            .branch_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(lock);
    }

    fn take_branch_lock(&self) -> Option<BranchLock> {
        self.branch_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    async fn remove(mut self) {
        if self.registered.load(Ordering::SeqCst)
            && git::remove_worktree_at(&self.repo_root, &self.dir)
                .await
                .is_err()
            && self.still_registered().await
        {
            tracing::warn!("could not remove a temporary feedback worktree from its repository");
        }
        if let Err(e) = remove_path(&self.dir).await {
            tracing::warn!("could not remove a temporary feedback worktree: {e}");
        }
        if let Some(lock) = self.take_branch_lock() {
            lock.release();
        }
        self.removed = true;
    }

    async fn still_registered(&self) -> bool {
        git::list_worktrees(&self.repo_root)
            .await
            .map_or(true, |entries| entries.iter().any(|e| e.path == self.dir))
    }
}

impl Drop for TempWorktree {
    fn drop(&mut self) {
        if self.removed {
            return;
        }
        let mut stopped = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        *stopped = true;
        if self.registered.load(Ordering::SeqCst)
            && !git::remove_worktree_at_blocking(&self.repo_root, &self.dir, self.timeout)
            && self.dir.exists()
        {
            tracing::warn!("could not remove a temporary feedback worktree from its repository");
        }
        match std::fs::remove_dir_all(&self.dir) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                tracing::warn!("could not remove a temporary feedback worktree: {e}");
            }
            _ => {}
        }
        if let Some(lock) = self.take_branch_lock() {
            lock.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn git(cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=Test User",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(cwd)
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A repository and a registered, locked worktree at `<tmp>/scratch/wt`.
    fn repo_with_worktree(tmp: &Path) -> TempWorktree {
        let root = tmp.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let dir = tmp.join("scratch").join(WORKTREE_DIR);
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                "--lock",
                dir.to_str().unwrap(),
            ],
        );
        assert_eq!(git(&root, &["worktree", "list"]).lines().count(), 2);
        let dir = std::fs::canonicalize(dir).unwrap();
        let worktree = TempWorktree::new(&root, dir, git::LOCAL_TIMEOUT);
        worktree.registered.store(true, Ordering::SeqCst);
        worktree
    }

    #[test]
    fn a_dropped_worktree_is_removed_with_its_registration() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = repo_with_worktree(tmp.path());
        let (root, dir) = (worktree.repo_root.clone(), worktree.dir.clone());
        drop(worktree);
        assert!(!dir.exists());
        assert_eq!(git(&root, &["worktree", "list"]).lines().count(), 1);
    }

    #[tokio::test]
    async fn a_panic_removes_the_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = repo_with_worktree(tmp.path());
        let (root, dir) = (worktree.repo_root.clone(), worktree.dir.clone());
        let task = tokio::spawn(async move {
            let _worktree = worktree;
            tokio::task::yield_now().await;
            panic!("a bug while the worktree exists");
        });
        assert!(task.await.unwrap_err().is_panic());
        assert!(!dir.exists());
        assert_eq!(git(&root, &["worktree", "list"]).lines().count(), 1);
    }

    #[tokio::test]
    async fn remove_removes_the_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = repo_with_worktree(tmp.path());
        let (root, dir) = (worktree.repo_root.clone(), worktree.dir.clone());
        worktree.remove().await;
        assert!(!dir.exists());
        assert_eq!(git(&root, &["worktree", "list"]).lines().count(), 1);
    }

    #[test]
    fn a_branch_lock_is_removed_only_when_this_run_left_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("x.lock");
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let lock = |existed| BranchLock {
            path: path.clone(),
            existed,
            commit: commit.into(),
        };
        // This run's git made it: empty, or holding this run's commit.
        for content in ["", "0123456789abcdef0123456789abcdef01234567\n"] {
            std::fs::write(&path, content).unwrap();
            lock(false).release();
            assert!(!path.exists(), "{content:?}");
        }
        // Someone else's: there before, or holding another commit.
        std::fs::write(&path, "").unwrap();
        lock(true).release();
        assert!(path.exists());
        std::fs::write(&path, "fedcba9876543210fedcba9876543210fedcba98\n").unwrap();
        lock(false).release();
        assert!(path.exists());
    }

    #[test]
    fn only_another_processes_lock_marks_a_crashed_run() {
        assert!(locked_by_another_run("smithagents feedback pid 1"));
        assert!(!locked_by_another_run(&lock_reason()));
        assert!(!locked_by_another_run("initializing"));
        assert!(!locked_by_another_run(""));
        assert!(!locked_by_another_run("smithagents feedback pid x"));
    }

    #[test]
    fn parents_of_a_path() {
        assert_eq!(
            parents("docs/issues/2026-11-26-x").collect::<Vec<_>>(),
            ["docs", "docs/issues"]
        );
        assert_eq!(parents("feedback/x").collect::<Vec<_>>(), ["feedback"]);
        assert_eq!(parents("x").count(), 0);
    }

    #[test]
    fn push_failures_are_fixed_sentences() {
        let cases = [
            (
                PushError::NoRemote,
                "push_no_remote",
                "git could not push: the repository has no remote named origin",
            ),
            (
                PushError::Auth,
                "push_auth",
                "git could not push to origin: authentication failed",
            ),
            (
                PushError::Rejected,
                "push_rejected",
                "git could not push to origin: the remote rejected the branch",
            ),
            (
                PushError::Timeout,
                "push_timeout",
                "git did not finish pushing within 120 seconds",
            ),
            (
                PushError::Failed { status: Some(128) },
                "push_failed",
                "git could not push to origin (exit status 128)",
            ),
            (
                PushError::Failed { status: None },
                "push_failed",
                "git could not push to origin (no exit status)",
            ),
        ];
        for (error, code, message) in cases {
            let failure = push_failure(error);
            assert_eq!(failure.step, FeedbackStep::Push);
            assert_eq!(
                (failure.code.as_str(), failure.message.as_str()),
                (code, message)
            );
        }
    }

    #[test]
    fn git_failures_are_fixed_sentences() {
        let err = git_failed(COMMIT_STEP)(GitError::Failed { status: Some(1) });
        assert_eq!(
            err.to_string(),
            "git could not commit the files (exit status 1)"
        );
        assert_eq!((err.code(), err.status().as_u16()), ("git_failed", 500));
        let err = git_failed(WORKTREE_STEP)(GitError::Command {
            stderr: "fatal: LEAK".into(),
        });
        assert_eq!(
            err.to_string(),
            "git could not create the worktree (no exit status)"
        );
        let err = git_failed(COMMIT_STEP)(GitError::TimedOut);
        assert_eq!(
            err.to_string(),
            "git could not commit the files (no exit status)"
        );
    }

    #[test]
    fn names_must_be_plain() {
        for name in [
            "issue.md",
            "screenshot-1.annotated.png",
            "clip-2.mp4",
            "a_b",
        ] {
            assert!(is_plain_name(name), "{name}");
        }
        for name in ["", ".", "..", ".git", "-x", "a/b", "a b", "é.png", "a\nb"] {
            assert!(!is_plain_name(name), "{name:?}");
        }
    }

    #[test]
    fn the_subject_is_one_line_without_format_characters() {
        assert_eq!(one_line("Fix\nthe \t zoom\u{7}"), "Fix the zoom");
        assert_eq!(
            one_line("Fix \u{202e}the\u{200b} zoom\u{2066}\u{feff}\u{e0041}"),
            "Fix the zoom"
        );
        assert_eq!(one_line("a\u{2028}b"), "a b");
    }
}

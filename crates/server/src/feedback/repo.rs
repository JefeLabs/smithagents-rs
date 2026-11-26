//! The git side of a feedback submission: where the issue folder goes, the
//! commit made in a temporary worktree, and the push.
//!
//! The commit is made in a worktree of its own inside the scratch directory
//! the caller gives (under [`Paths::feedback_tmp_dir`]), detached at the
//! default branch's tip and with no file checked out. The user's checkout,
//! index and current branch are never touched, and nothing the repository
//! holds or configures (hooks, a checkout filter, a symbolic link, signing)
//! runs or is followed. The branch is created only once its commit exists, so
//! no failure or crash leaves an empty branch behind. Nothing git prints is
//! kept: a failure is one of [`FeedbackError`]'s fixed sentences, or a
//! [`FeedbackFailure`] with a code.
//!
//! [`Paths::feedback_tmp_dir`]: crate::paths::Paths::feedback_tmp_dir

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
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
    /// A file on disk (an upload), moved into the folder.
    Staged(PathBuf),
}

/// One file of the issue folder; `name` is a plain file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderFile {
    pub name: String,
    pub source: FileSource,
}

/// The first suffix 1..=99 whose folder is absent from the default branch
/// and whose branch does not exist. Nothing is created.
pub async fn place(
    repo: &Repo,
    title: &str,
    date: NaiveDate,
    idea: bool,
) -> Result<Placement, FeedbackError> {
    require_default_branch(repo).await?;
    let root = repo.path.as_path();
    let slug = issue_slug(title);
    for n in 1..=MAX_SUFFIX {
        let slug = with_suffix(&slug, n);
        let placement = Placement {
            folder: folder_path(date, &slug),
            branch: branch_name(date, &slug, idea),
            slug,
        };
        check_names(&placement, &[])?;
        let taken = git::path_in_branch(root, &repo.default_branch, &placement.folder)
            .await
            .map_err(git_failed(WORKTREE_STEP))?
            || git::branch_exists(root, &placement.branch)
                .await
                .map_err(git_failed(WORKTREE_STEP))?;
        if !taken {
            check_branch(root, &placement.branch).await?;
            return Ok(placement);
        }
    }
    Err(FeedbackError::NoFreeName)
}

/// Creates `placement.branch` from the default branch in `<scratch>/wt`,
/// writes the folder, commits `docs(issues): <title>`, removes the worktree,
/// and returns the commit's id. On any failure (and when the future is
/// dropped) the worktree is removed and no branch is created.
///
/// `scratch` should be a directory of its own under
/// [`Paths::feedback_tmp_dir`]; it is made owner-only (a symbolic link or a
/// directory of another user is refused). `Staged` files are moved into the
/// folder (renamed, else copied); every file gets mode 0644.
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

    // Armed before anything is created: a panic or a dropped future removes
    // the worktree in `Drop`; every other way out goes through `remove`.
    let worktree = TempWorktree::new(root, scratch.join(WORKTREE_DIR));
    let committed = commit_in_worktree(repo, &worktree, placement, files, title).await;
    worktree.remove().await;
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
    // A worktree an earlier run left here when it crashed; its registration
    // goes with the prune below, as do those of other crashed runs whose
    // directories the daemon removed when it started.
    remove_path(dir).await?;
    git::prune_worktrees(root)
        .await
        .map_err(git_failed(WORKTREE_STEP))?;
    let base = git::rev_parse(root, &format!("refs/heads/{}", repo.default_branch))
        .await
        .map_err(git_failed(WORKTREE_STEP))?
        .ok_or_else(|| FeedbackError::DefaultBranchMissing(repo.default_branch.clone()))?;
    git::add_detached_worktree(root, dir, &base)
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
    // Only now that its commit exists: a failure before leaves no branch.
    git::create_branch(root, &placement.branch, &commit)
        .await
        .map_err(git_failed(COMMIT_STEP))?;
    Ok(commit)
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
        && git::branch_exists(&repo.path, name)
            .await
            .map_err(git_failed(WORKTREE_STEP))?;
    if exists {
        Ok(())
    } else {
        Err(FeedbackError::DefaultBranchMissing(name.clone()))
    }
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
    let valid = git::is_valid_branch_name(repo_root, branch)
        .await
        .map_err(git_failed(WORKTREE_STEP))?;
    if valid {
        Ok(())
    } else {
        Err(unsafe_name())
    }
}

/// The title on one line: controls as spaces, runs of white space as one.
fn one_line(title: &str) -> String {
    let spaced: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
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
                FileSource::Staged(from) => move_staged(from, &to)?,
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

/// Renames `from` to `to`, else copies it and removes `from`. Only a regular
/// file is taken, never what a link points to.
fn move_staged(from: &Path, to: &Path) -> io::Result<()> {
    if !std::fs::symlink_metadata(from)?.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a staged file is not a regular file",
        ));
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    // The staging directory is removed by its owner in any case.
    let _ = std::fs::remove_file(from);
    Ok(())
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

/// The temporary worktree. [`TempWorktree::remove`] deletes its directory
/// and prunes its registration; when it is not reached (a panic, the future
/// being dropped) `Drop` does the same, blocking.
struct TempWorktree {
    repo_root: PathBuf,
    dir: PathBuf,
    removed: bool,
    /// Held while the folder is written on the blocking pool, where a write
    /// goes on after its future is dropped; `true` once `Drop` has run, so a
    /// write that had not started does nothing. `Drop` takes it before
    /// removing anything, so nothing is written again after the removal.
    gate: Arc<Mutex<bool>>,
}

impl TempWorktree {
    fn new(repo_root: &Path, dir: PathBuf) -> Self {
        TempWorktree {
            repo_root: repo_root.to_path_buf(),
            dir,
            removed: false,
            gate: Arc::default(),
        }
    }

    async fn remove(mut self) {
        if let Err(e) = remove_path(&self.dir).await {
            tracing::warn!("could not remove a temporary feedback worktree: {e}");
        }
        if let Err(e) = git::prune_worktrees(&self.repo_root).await {
            tracing::warn!("could not prune the worktrees of a repository: {e}");
        }
        self.removed = true;
    }
}

impl Drop for TempWorktree {
    fn drop(&mut self) {
        if self.removed {
            return;
        }
        let mut stopped = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        *stopped = true;
        match std::fs::remove_dir_all(&self.dir) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                tracing::warn!("could not remove a temporary feedback worktree: {e}");
            }
            _ => {}
        }
        if !git::prune_worktrees_blocking(&self.repo_root) {
            tracing::warn!("could not prune the worktrees of a repository");
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

    /// A repository and a registered worktree at `<tmp>/scratch/wt`.
    fn repo_with_worktree(tmp: &Path) -> TempWorktree {
        let root = tmp.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let dir = tmp.join("scratch").join(WORKTREE_DIR);
        git(
            &root,
            &["worktree", "add", "-q", "--detach", dir.to_str().unwrap()],
        );
        assert_eq!(git(&root, &["worktree", "list"]).lines().count(), 2);
        TempWorktree::new(&root, dir)
    }

    #[test]
    fn a_dropped_worktree_is_removed_and_pruned() {
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
    fn the_subject_is_one_line() {
        assert_eq!(one_line("Fix\nthe \t zoom\u{7}"), "Fix the zoom");
    }
}

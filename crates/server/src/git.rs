//! Repository validation and worktree management via the `git` CLI.
//!
//! All commands are spawned directly (never through a shell) with arguments
//! passed separately, so paths containing spaces or unicode need no escaping.

use std::path::{Path, PathBuf};
use std::process::Output;

use tokio::process::Command;

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
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Builds a git command with a sanitized environment. `LC_ALL`/`LANGUAGE` are
/// pinned to `C` so the stderr patterns we match are never localized.
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

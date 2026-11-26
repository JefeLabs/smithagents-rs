//! The git side of a feedback submission: where the issue folder goes, the
//! commit made in a temporary worktree, and the push.
//!
//! Every repository is a temp directory with its own neutral identity, and
//! `origin` is a bare repository the test makes: nothing reaches the network.
//! Every git this binary starts (the daemon's included) reads no global or
//! system configuration, so a test repository's identity is its own.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use chrono::{NaiveDate, Utc};
use smithagents_core::agent::Repo;
use smithagents_core::feedback::{FeedbackStep, SubmissionId};
use smithagents_core::ids::RepoId;
use smithagents_server::feedback::repo::{
    commit_folder, find_committed, place, place_and_commit, push, Committed, FileSource,
    FolderFile, Placement,
};
use smithagents_server::feedback::FeedbackError;
use smithagents_server::git::{self, CommitError, GitError, PushError};
use smithagents_server::paths::Paths;
use tempfile::TempDir;

const GITHUB_URL: &str = "https://github.com/smithagents-test-invalid/widgets.git";
const SHOT: &[u8] = b"\x89PNG\r\n\x1a\nnot really a picture\x00\xff";
const ISSUE_MD: &str = "# Fix the zoom\n\nThe page is too big.\n";
const BRANCH: &str = "feedback/2026-11-26-fix-the-zoom";
const FOLDER: &str = "docs/issues/2026-11-26-fix-the-zoom";

/// A new submission id for a commit.
fn id() -> SubmissionId {
    SubmissionId::new()
}

fn date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 26).unwrap()
}

/// An identity a daemon may inherit from the shell that started it and must
/// never put on a commit.
const INHERITED_IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "LEAK Inherited Author"),
    ("GIT_AUTHOR_EMAIL", "leak-author@example.invalid"),
    ("GIT_AUTHOR_DATE", "2001-02-03T04:05:06Z"),
    ("GIT_COMMITTER_NAME", "LEAK Inherited Committer"),
    ("GIT_COMMITTER_EMAIL", "leak-committer@example.invalid"),
    ("GIT_COMMITTER_DATE", "2001-02-03T04:05:06Z"),
];

/// Configuration a daemon may inherit through the environment (it overrides
/// the repository's own) and must not pass on to the git it runs.
const INHERITED_CONFIG: [(&str, &str); 3] = [
    ("GIT_CONFIG_COUNT", "1"),
    ("GIT_CONFIG_KEY_0", "user.name"),
    ("GIT_CONFIG_VALUE_0", "Injected"),
];

/// Points `HOME` and `XDG_CONFIG_HOME` at an empty directory, turns the
/// system configuration off and sets [`INHERITED_IDENTITY`] and
/// [`INHERITED_CONFIG`], once, before any test of this binary runs git.
fn isolate_git_config() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join("feedback_repo-empty-home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", &home);
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        for (name, value) in INHERITED_IDENTITY.iter().chain(&INHERITED_CONFIG) {
            std::env::set_var(name, value);
        }
    });
}

/// git as the test runs it: hooks and signing off, no inherited identity,
/// and no optional locks, so the test's own `git status` never rewrites the
/// index it compares.
fn git_cmd(cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "commit.gpgsign=false",
    ])
    .args(args)
    .current_dir(cwd)
    .env("LC_ALL", "C")
    .env("LANGUAGE", "C")
    .env("GIT_OPTIONAL_LOCKS", "0")
    .env_remove("GIT_DIR")
    .env_remove("GIT_WORK_TREE")
    .env_remove("GIT_INDEX_FILE");
    for (name, _) in INHERITED_IDENTITY.iter().chain(&INHERITED_CONFIG) {
        cmd.env_remove(name);
    }
    cmd
}

/// What a crashed run of another daemon process leaves: a worktree under the
/// feedback directory, locked the way `commit_folder` locks its own (pid 1
/// is never the test's), whose directory the daemon removed at its start.
fn crashed_leftover(fx: &Fixture, scratch_name: &str) -> PathBuf {
    let dir = fx.scratch(scratch_name).join("wt");
    git(
        fx.root(),
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            "--lock",
            "--reason",
            "smithagents feedback pid 1",
            dir.to_str().unwrap(),
            "main",
        ],
    );
    std::fs::remove_dir_all(fx.scratch(scratch_name)).unwrap();
    std::fs::canonicalize(fx.paths.feedback_tmp_dir())
        .unwrap()
        .join(scratch_name)
        .join("wt")
}

/// The paths of the repository's registered worktrees.
fn worktree_paths(root: &Path) -> Vec<String> {
    git(root, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(str::to_string)
        .collect()
}

/// A worktree of the user's own whose folder was moved away: git still
/// knows it (`.git/worktrees/<name>`) and a bare prune would delete that.
fn moved_user_worktree(fx: &Fixture) -> PathBuf {
    let dir = fx.tmp.path().join("user-wt");
    git(
        fx.root(),
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            dir.to_str().unwrap(),
            "main",
        ],
    );
    std::fs::rename(&dir, fx.tmp.path().join("user-wt-moved")).unwrap();
    let admin = fx.root().join(".git/worktrees/user-wt");
    assert!(admin.join("HEAD").exists());
    admin
}

/// Whether a process whose command line contains `pattern` is running.
fn process_running(pattern: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .unwrap()
        .status
        .success()
}

/// Runs git, panicking on failure; stdout without its trailing newline.
fn git(cwd: &Path, args: &[&str]) -> String {
    let out = git_cmd(cwd, args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn git_bytes(cwd: &Path, args: &[&str]) -> Vec<u8> {
    let out = git_cmd(cwd, args).output().unwrap();
    assert!(out.status.success(), "git {args:?} failed");
    out.stdout
}

fn git_succeeds(cwd: &Path, args: &[&str]) -> bool {
    git_cmd(cwd, args).output().unwrap().status.success()
}

fn executable(path: &Path, script: &str) {
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn invalid_input(err: &FeedbackError) -> bool {
    matches!(err, FeedbackError::Io(e) if e.kind() == std::io::ErrorKind::InvalidInput)
}

struct Fixture {
    tmp: TempDir,
    repo: Repo,
    paths: Paths,
}

impl Fixture {
    /// A repository on `main` with two commits and the identity
    /// `Test User <test@example.invalid>`, and a daemon home beside it.
    fn new() -> Self {
        isolate_git_config();
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        common::init_repo(&root);
        git(&root, &["config", "user.name", "Test User"]);
        git(&root, &["config", "user.email", "test@example.invalid"]);
        std::fs::write(root.join("README.md"), "widgets\n").unwrap();
        git(&root, &["add", "README.md"]);
        git(&root, &["commit", "-q", "-m", "readme"]);
        let repo = Repo {
            id: RepoId::new(),
            path: std::fs::canonicalize(&root).unwrap(),
            default_branch: "main".into(),
            created_at: Utc::now(),
        };
        let paths = Paths {
            home: tmp.path().join("home"),
        };
        Fixture { tmp, repo, paths }
    }

    fn root(&self) -> &Path {
        &self.repo.path
    }

    fn scratch(&self, name: &str) -> PathBuf {
        self.paths.feedback_tmp_dir().join(name)
    }

    /// `issue.md` as text and a screenshot staged in `scratch`, the way an
    /// upload leaves it.
    fn files(&self, scratch: &Path) -> Vec<FolderFile> {
        let upload = scratch.join("upload");
        std::fs::create_dir_all(&upload).unwrap();
        let staged = upload.join("file-1");
        std::fs::write(&staged, SHOT).unwrap();
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)).unwrap();
        vec![
            FolderFile {
                name: "issue.md".into(),
                source: FileSource::Text(ISSUE_MD.into()),
            },
            FolderFile {
                name: "screenshot-1.png".into(),
                source: FileSource::Staged(staged),
            },
        ]
    }

    /// `place` and `commit_folder` for "Fix the zoom" in the scratch
    /// directory `scratch_name`.
    async fn submit(&self, scratch_name: &str) -> (Placement, String) {
        let placement = place(&self.repo, "Fix the zoom", date(), false)
            .await
            .unwrap();
        let scratch = self.scratch(scratch_name);
        let files = self.files(&scratch);
        let commit = commit_folder(
            &self.repo,
            &placement,
            &files,
            "Fix the zoom",
            id(),
            &scratch,
        )
        .await
        .unwrap();
        (placement, commit)
    }

    fn commit_on_main(&self, path: &str, text: &str) -> String {
        let file = self.root().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, text).unwrap();
        git(self.root(), &["add", "--", path]);
        git(self.root(), &["commit", "-q", "-m", "more"]);
        git(self.root(), &["rev-parse", "HEAD"])
    }

    fn worktree_lines(&self) -> usize {
        git(self.root(), &["worktree", "list"]).lines().count()
    }

    fn feedback_branches(&self) -> String {
        git(self.root(), &["branch", "--list", "feedback/*", "idea/*"])
    }

    /// A bare repository, made `origin`.
    fn origin(&self) -> PathBuf {
        let bare = self.tmp.path().join("origin.git");
        git(
            self.tmp.path(),
            &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
        );
        git(
            self.root(),
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        bare
    }
}

#[tokio::test]
async fn the_folder_is_committed_on_a_new_branch_from_the_default_branch() {
    let fx = Fixture::new();
    let main_tip = git(fx.root(), &["rev-parse", "main"]);

    let (placement, commit) = fx.submit("s1").await;

    assert_eq!(
        placement,
        Placement {
            slug: "fix-the-zoom".into(),
            folder: FOLDER.into(),
            branch: BRANCH.into(),
        }
    );
    assert_eq!(commit, git(fx.root(), &["rev-parse", BRANCH]));
    assert_eq!(
        git(fx.root(), &["rev-parse", &format!("{commit}^")]),
        main_tip
    );
    assert_eq!(
        git(fx.root(), &["show", "--name-only", "--format=", &commit]),
        format!("{FOLDER}/issue.md\n{FOLDER}/screenshot-1.png")
    );
    assert_eq!(
        git(fx.root(), &["log", "-1", "--format=%s", &commit]),
        "docs(issues): Fix the zoom"
    );
    // The files hold what was given, are not executable, and the commit
    // keeps every other file of `main`.
    assert_eq!(
        git_bytes(
            fx.root(),
            &["cat-file", "blob", &format!("{commit}:{FOLDER}/issue.md")]
        ),
        ISSUE_MD.as_bytes()
    );
    assert_eq!(
        git_bytes(
            fx.root(),
            &[
                "cat-file",
                "blob",
                &format!("{commit}:{FOLDER}/screenshot-1.png")
            ]
        ),
        SHOT
    );
    let tree = git(fx.root(), &["ls-tree", "-r", &commit]);
    assert_eq!(
        tree.lines().filter(|l| l.starts_with("100644 ")).count(),
        3,
        "{tree}"
    );
    assert!(tree.contains("\tREADME.md"), "{tree}");
    // Made with the repository's own identity, not the one the daemon's
    // environment holds (INHERITED_IDENTITY), and dated now.
    assert_eq!(
        git(
            fx.root(),
            &["log", "-1", "--format=%an <%ae>|%cn <%ce>", &commit]
        ),
        "Test User <test@example.invalid>|Test User <test@example.invalid>"
    );
    assert_eq!(
        git(
            fx.root(),
            &["log", "-1", "--date=format:%Y", "--format=%ad %cd", &commit]
        ),
        format!("{0} {0}", Utc::now().format("%Y"))
    );
    // The user's checkout is as it was; nothing is left behind.
    assert_eq!(git(fx.root(), &["status", "--porcelain"]), "");
    assert_eq!(git(fx.root(), &["branch", "--show-current"]), "main");
    assert_eq!(fx.worktree_lines(), 1);
    assert!(!fx.scratch("s1").join("wt").exists());
    assert!(
        !fx.scratch("s1").join("upload/file-1").exists(),
        "the upload is moved"
    );
}

#[tokio::test]
async fn a_clash_gets_the_next_suffix() {
    let fx = Fixture::new();
    fx.commit_on_main(&format!("{FOLDER}/issue.md"), "earlier\n");
    let p = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    assert_eq!(
        p,
        Placement {
            slug: "fix-the-zoom-2".into(),
            folder: "docs/issues/2026-11-26-fix-the-zoom-2".into(),
            branch: "feedback/2026-11-26-fix-the-zoom-2".into(),
        }
    );

    git(fx.root(), &["branch", "feedback/2026-11-26-fix-the-zoom-2"]);
    let p = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    assert_eq!(p.slug, "fix-the-zoom-3");
    assert_eq!(p.folder, "docs/issues/2026-11-26-fix-the-zoom-3");
    assert_eq!(p.branch, "feedback/2026-11-26-fix-the-zoom-3");

    // An idea's branch clashes on its own name.
    let fresh = Fixture::new();
    git(fresh.root(), &["branch", "idea/fix-the-zoom"]);
    let p = place(&fresh.repo, "Fix the zoom", date(), true)
        .await
        .unwrap();
    assert_eq!(
        p,
        Placement {
            slug: "fix-the-zoom-2".into(),
            folder: "docs/issues/2026-11-26-fix-the-zoom-2".into(),
            branch: "idea/fix-the-zoom-2".into(),
        }
    );
    // Placing creates nothing.
    assert_eq!(fresh.feedback_branches(), "  idea/fix-the-zoom");
}

#[tokio::test]
async fn ninety_nine_clashes_are_no_free_name() {
    let fx = Fixture::new();
    let tip = git(fx.root(), &["rev-parse", "main"]);
    let mut stdin = String::new();
    for n in 1..=99 {
        let suffix = if n == 1 {
            String::new()
        } else {
            format!("-{n}")
        };
        stdin.push_str(&format!("create refs/heads/{BRANCH}{suffix} {tip}\n"));
    }
    let mut child = git_cmd(fx.root(), &["update-ref", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), stdin.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());

    let err = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap_err();
    assert!(matches!(err, FeedbackError::NoFreeName), "{err:?}");
    assert_eq!(
        err.to_string(),
        "99 issue folders with this name already exist"
    );
    assert_eq!(err.code(), "no_free_name");
    assert_eq!(err.status().as_u16(), 409);
}

#[tokio::test]
async fn the_branch_starts_from_the_default_branch_as_it_is_now() {
    let fx = Fixture::new();
    let placement = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    // `main` moves on after the repository was added and the name was chosen.
    let new_tip = fx.commit_on_main("later.txt", "later\n");
    let scratch = fx.scratch("s");
    let files = fx.files(&scratch);
    let commit = commit_folder(&fx.repo, &placement, &files, "Fix the zoom", id(), &scratch)
        .await
        .unwrap();
    assert_eq!(
        git(fx.root(), &["rev-parse", &format!("{commit}^")]),
        new_tip
    );
    assert!(git(fx.root(), &["ls-tree", "-r", "--name-only", &commit]).contains("later.txt"));
}

#[tokio::test]
async fn a_missing_default_branch_is_refused_and_nothing_is_created() {
    let fx = Fixture::new();
    let placement = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    git(fx.root(), &["branch", "-m", "main", "trunk"]);

    let err = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, FeedbackError::DefaultBranchMissing(b) if b == "main"),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "the repository's default branch main does not exist locally"
    );
    assert_eq!(err.code(), "default_branch_missing");
    assert_eq!(err.status().as_u16(), 409);

    let scratch = fx.scratch("s");
    let files = fx.files(&scratch);
    let err = commit_folder(&fx.repo, &placement, &files, "Fix the zoom", id(), &scratch)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, FeedbackError::DefaultBranchMissing(b) if b == "main"),
        "{err:?}"
    );
    assert_eq!(git(fx.root(), &["branch", "--list", "feedback/*"]), "");
    assert_eq!(fx.worktree_lines(), 1);
    assert!(!scratch.join("wt").exists());
}

#[tokio::test]
async fn an_unborn_repository_is_refused_with_the_same_sentence() {
    isolate_git_config();
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    let repo = Repo {
        id: RepoId::new(),
        path: root,
        default_branch: "main".into(),
        created_at: Utc::now(),
    };
    let err = place(&repo, "Fix the zoom", date(), false)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, FeedbackError::DefaultBranchMissing(b) if b == "main"),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "the repository's default branch main does not exist locally"
    );
}

#[tokio::test]
async fn a_failed_commit_leaves_no_branch_and_no_worktree() {
    let fx = Fixture::new();
    git(fx.root(), &["config", "--unset", "user.name"]);
    git(fx.root(), &["config", "--unset", "user.email"]);
    git(fx.root(), &["config", "user.useConfigOnly", "true"]);
    let placement = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    let scratch = fx.scratch("s1");
    let files = fx.files(&scratch);

    let err = commit_folder(&fx.repo, &placement, &files, "Fix the zoom", id(), &scratch)
        .await
        .unwrap_err();
    assert!(matches!(err, FeedbackError::GitIdentityMissing), "{err:?}");
    assert_eq!(
        err.to_string(),
        "git has no user.name and user.email set for this repository"
    );
    assert_eq!(err.code(), "git_identity_missing");
    assert_eq!(err.status().as_u16(), 409);
    assert!(!git::branch_exists(fx.root(), BRANCH).await.unwrap());
    assert_eq!(fx.worktree_lines(), 1);
    assert!(!scratch.join("wt").exists());

    // Once an identity is set, the same name is free and the commit works.
    git(fx.root(), &["config", "user.name", "Test User"]);
    git(fx.root(), &["config", "user.email", "test@example.invalid"]);
    let again = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    assert_eq!(again, placement);
    let scratch = fx.scratch("s2");
    let files = fx.files(&scratch);
    let commit = commit_folder(&fx.repo, &again, &files, "Fix the zoom", id(), &scratch)
        .await
        .unwrap();
    assert_eq!(commit, git(fx.root(), &["rev-parse", BRANCH]));
}

#[tokio::test]
async fn an_identity_is_never_guessed() {
    // Without `user.useConfigOnly` git would make one up from the user name
    // and the host name, and push it to a public repository.
    let fx = Fixture::new();
    git(fx.root(), &["config", "--unset", "user.name"]);
    git(fx.root(), &["config", "--unset", "user.email"]);
    let placement = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    let scratch = fx.scratch("s");
    let files = fx.files(&scratch);
    let err = commit_folder(&fx.repo, &placement, &files, "Fix the zoom", id(), &scratch)
        .await
        .unwrap_err();
    assert!(matches!(err, FeedbackError::GitIdentityMissing), "{err:?}");
    assert_eq!(fx.feedback_branches(), "");
}

#[tokio::test]
async fn a_pre_commit_hook_is_not_run() {
    let fx = Fixture::new();
    executable(
        &fx.root().join(".git/hooks/pre-commit"),
        "#!/bin/sh\necho refused >&2\nexit 1\n",
    );
    let (_, commit) = fx.submit("s").await;
    assert_eq!(commit, git(fx.root(), &["rev-parse", BRANCH]));
}

#[tokio::test]
async fn no_hook_editor_monitor_or_signing_program_runs() {
    let fx = Fixture::new();
    let ran = fx.tmp.path().join("ran");
    std::fs::create_dir_all(&ran).unwrap();
    let hooks = fx.tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let marker = |name: &str| format!("#!/bin/sh\ntouch '{}'\nexit 1\n", ran.join(name).display());
    for hook in [
        "pre-commit",
        "prepare-commit-msg",
        "commit-msg",
        "post-commit",
        "post-checkout",
        "post-index-change",
        "reference-transaction",
        "pre-push",
        "push-to-checkout",
    ] {
        executable(&hooks.join(hook), &marker(hook));
    }
    let program = fx.tmp.path().join("program");
    executable(&program, &marker("program"));
    let program = program.to_str().unwrap();
    for (key, value) in [
        ("core.hooksPath", hooks.to_str().unwrap()),
        ("core.editor", program),
        ("sequence.editor", program),
        ("core.fsmonitor", program),
        ("commit.gpgSign", "true"),
        ("gpg.program", program),
        ("push.gpgSign", "true"),
        ("push.followTags", "true"),
    ] {
        git(fx.root(), &["config", key, value]);
    }
    let bare = fx.origin();
    git(fx.root(), &["tag", "-a", "-m", "tag", "v1"]);

    let (placement, commit) = fx.submit("s").await;
    push(&fx.repo, &placement.branch, &placement.branch)
        .await
        .unwrap();

    let ran: Vec<_> = std::fs::read_dir(&ran)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(ran.is_empty(), "ran: {ran:?}");
    let raw = git(fx.root(), &["cat-file", "commit", &commit]);
    assert!(!raw.contains("gpgsig"), "the commit is unsigned: {raw}");
    // The repository's push settings add nothing: the branch alone arrives.
    let refs = git(&bare, &["for-each-ref", "--format=%(refname)"]);
    assert_eq!(refs, format!("refs/heads/{BRANCH}"));
}

#[tokio::test]
async fn a_signing_repository_neither_hangs_nor_fails() {
    let fx = Fixture::new();
    let program = fx.tmp.path().join("slow-gpg");
    executable(&program, "#!/bin/sh\nsleep 30\nexit 1\n");
    git(fx.root(), &["config", "commit.gpgSign", "true"]);
    git(
        fx.root(),
        &["config", "gpg.program", program.to_str().unwrap()],
    );
    let started = Instant::now();
    let (_, commit) = tokio::time::timeout(Duration::from_secs(20), fx.submit("s"))
        .await
        .expect("the commit does not wait for a signing program");
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(!git(fx.root(), &["cat-file", "commit", &commit]).contains("gpgsig"));
}

/// What a person sees of their checkout: `HEAD`, the index file's bytes, the
/// status (branch, changes, untracked files) and a changed file's content.
fn checkout_state(root: &Path) -> (String, Vec<u8>, String, String) {
    (
        std::fs::read_to_string(root.join(".git/HEAD")).unwrap(),
        std::fs::read(root.join(".git/index")).unwrap(),
        git(
            root,
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "--untracked-files=all",
            ],
        ),
        std::fs::read_to_string(root.join("README.md")).unwrap(),
    )
}

#[tokio::test]
async fn the_users_checkout_is_untouched() {
    for checkout in ["main", "another branch", "detached"] {
        let fx = Fixture::new();
        if checkout == "another branch" {
            git(fx.root(), &["checkout", "-q", "-b", "work"]);
        } else if checkout == "detached" {
            git(fx.root(), &["checkout", "-q", "--detach"]);
        }
        std::fs::write(fx.root().join("README.md"), "changed, not staged\n").unwrap();
        std::fs::write(fx.root().join("staged.txt"), "staged\n").unwrap();
        git(fx.root(), &["add", "staged.txt"]);
        std::fs::write(fx.root().join("untracked.txt"), "untracked\n").unwrap();
        fx.origin();
        let before = checkout_state(fx.root());
        assert!(before.2.contains("untracked.txt") && before.2.contains("staged.txt"));

        let (placement, _) = fx.submit("s").await;
        push(&fx.repo, &placement.branch, &placement.branch)
            .await
            .unwrap();

        assert_eq!(checkout_state(fx.root()), before, "checked out: {checkout}");
        assert_eq!(fx.worktree_lines(), 1, "checked out: {checkout}");
    }
}

#[tokio::test]
async fn a_name_starting_with_a_dash_is_refused_before_git_runs() {
    let fx = Fixture::new();
    let good = Placement {
        slug: "fix-the-zoom".into(),
        folder: FOLDER.into(),
        branch: BRANCH.into(),
    };
    // git cannot even start in this directory: a refusal that comes first
    // is `InvalidInput`, anything git did would be another error.
    let nowhere = Repo {
        path: fx.tmp.path().join("no-such-repository"),
        ..fx.repo.clone()
    };
    let scratch = fx.scratch("s");
    let files = fx.files(&scratch);
    let placements = [
        Placement {
            branch: "-b".into(),
            ..good.clone()
        },
        Placement {
            branch: "--force".into(),
            ..good.clone()
        },
        Placement {
            folder: "-docs/x".into(),
            ..good.clone()
        },
        Placement {
            folder: "docs/--x".into(),
            ..good.clone()
        },
        Placement {
            folder: "docs/../x".into(),
            ..good.clone()
        },
        Placement {
            folder: "/tmp/x".into(),
            ..good.clone()
        },
        Placement {
            folder: ".git/hooks".into(),
            ..good.clone()
        },
        Placement {
            folder: String::new(),
            ..good.clone()
        },
    ];
    for placement in &placements {
        let err = commit_folder(&nowhere, placement, &files, "t", id(), &scratch)
            .await
            .unwrap_err();
        assert!(invalid_input(&err), "{placement:?}: {err:?}");
    }
    for name in ["-x", "--all", "../x", "a/b", ".hidden", ""] {
        let mut bad = files.clone();
        bad[0].name = name.into();
        let err = commit_folder(&nowhere, &good, &bad, "t", id(), &scratch)
            .await
            .unwrap_err();
        assert!(invalid_input(&err), "{name:?}: {err:?}");
    }
    let mut twice = files.clone();
    twice[1].name = "issue.md".into();
    let err = commit_folder(&nowhere, &good, &twice, "t", id(), &scratch)
        .await
        .unwrap_err();
    assert!(invalid_input(&err), "{err:?}");

    // A branch name git itself refuses is checked with git before anything
    // is created.
    for branch in ["feedback/a..b", "feedback/x.lock", "feedback/a b"] {
        let placement = Placement {
            branch: branch.into(),
            ..good.clone()
        };
        let err = commit_folder(&fx.repo, &placement, &files, "t", id(), &scratch)
            .await
            .unwrap_err();
        assert!(invalid_input(&err), "{branch}: {err:?}");
    }
    assert_eq!(git(fx.root(), &["branch", "--list"]), "* main");
    assert!(!scratch.join("wt").exists());
    assert_eq!(fx.worktree_lines(), 1);
}

#[tokio::test]
async fn git_refuses_option_like_values() {
    let fx = Fixture::new();
    let nowhere = fx.tmp.path().join("no-such-repository");
    let unsafe_arg = |r: Result<(), GitError>| matches!(r, Err(GitError::UnsafeArgument));
    assert!(unsafe_arg(git::rev_parse(&nowhere, "-h").await.map(drop)));
    assert!(unsafe_arg(git::delete_branch(&nowhere, "-D").await));
    assert!(unsafe_arg(
        git::path_in_branch(&nowhere, "-x", "p").await.map(drop)
    ));
    assert!(unsafe_arg(
        git::show_file(&nowhere, "--output=x", "p", 10)
            .await
            .map(drop)
    ));
    assert!(unsafe_arg(
        git::list_folder(&nowhere, "-x", "docs").await.map(drop)
    ));
    let err = git::commit_paths(&nowhere, &["-x"], "m").await.unwrap_err();
    assert!(
        matches!(&err, CommitError::Io(e) if e.kind() == std::io::ErrorKind::InvalidInput),
        "{err:?}"
    );
    let bare = fx.origin();
    for (local, remote) in [("-f", "main"), ("main", "--force"), ("main", "+main")] {
        let err = git::push_branch(fx.root(), local, remote, Duration::from_secs(30))
            .await
            .unwrap_err();
        assert_eq!(err, PushError::Failed { status: None }, "{local} {remote}");
    }
    assert_eq!(git(&bare, &["for-each-ref"]), "", "nothing was pushed");
}

#[tokio::test]
async fn the_scratch_directory_is_private_and_holds_the_only_worktree() {
    let fx = Fixture::new();
    // An existing scratch directory anyone may write to is made owner-only.
    let scratch = fx.scratch("open");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o777)).unwrap();
    let placement = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    let files = fx.files(&scratch);
    commit_folder(&fx.repo, &placement, &files, "Fix the zoom", id(), &scratch)
        .await
        .unwrap();
    assert_eq!(mode(&scratch), 0o700);
    // Nothing was made inside the repository.
    assert_eq!(git(fx.root(), &["status", "--porcelain", "--ignored"]), "");

    // A scratch directory that is a symbolic link is refused.
    let elsewhere = fx.tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let link = fx.scratch("link");
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    let placement = place(&fx.repo, "Other", date(), false).await.unwrap();
    let staged = fx.tmp.path().join("other.png");
    std::fs::write(&staged, SHOT).unwrap();
    let files = [FolderFile {
        name: "screenshot-1.png".into(),
        source: FileSource::Staged(staged),
    }];
    let err = commit_folder(&fx.repo, &placement, &files, "Other", id(), &link)
        .await
        .unwrap_err();
    assert!(matches!(err, FeedbackError::Io(_)), "{err:?}");
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    assert!(!git::branch_exists(fx.root(), &placement.branch)
        .await
        .unwrap());
}

#[tokio::test]
async fn a_dropped_commit_leaves_no_worktree() {
    let fx = Fixture::new();
    let placement = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    let scratch = fx.scratch("s");
    let files = fx.files(&scratch);
    let writing = scratch.join("wt").join("docs");
    {
        let mut commit = Box::pin(commit_folder(
            &fx.repo,
            &placement,
            &files,
            "Fix the zoom",
            id(),
            &scratch,
        ));
        // Poll until the folder is being written in the worktree under the
        // daemon's home, then drop the future there.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            assert!(
                futures::poll!(commit.as_mut()).is_pending(),
                "finished before it could be dropped"
            );
            if writing.exists() {
                break;
            }
            assert!(Instant::now() < deadline, "the worktree never appeared");
            tokio::task::yield_now().await;
        }
    }
    assert!(!scratch.join("wt").exists());
    assert_eq!(fx.worktree_lines(), 1);
    assert_eq!(fx.feedback_branches(), "");
    assert_eq!(git(fx.root(), &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn a_crashed_runs_worktree_is_cleaned_up_on_the_next_call() {
    let fx = Fixture::new();
    // A run of another daemon process crashed; the daemon removed
    // `tmp/feedback` when it started again, which leaves a stale, locked
    // registration in the repository.
    crashed_leftover(&fx, "crashed");
    // A run that crashed in the very scratch directory it is given again.
    let scratch = fx.scratch("again");
    let leftover = scratch.join("wt");
    git(
        fx.root(),
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            leftover.to_str().unwrap(),
            "main",
        ],
    );
    std::fs::write(leftover.join("junk.txt"), "junk").unwrap();
    assert_eq!(fx.worktree_lines(), 3);

    let placement = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    let files = fx.files(&scratch);
    let commit = commit_folder(&fx.repo, &placement, &files, "Fix the zoom", id(), &scratch)
        .await
        .unwrap();

    assert_eq!(fx.worktree_lines(), 1);
    assert!(!leftover.exists());
    assert_eq!(
        git(fx.root(), &["show", "--name-only", "--format=", &commit]),
        format!("{FOLDER}/issue.md\n{FOLDER}/screenshot-1.png")
    );
}

#[tokio::test]
async fn push_sends_the_branch_to_origin() {
    let fx = Fixture::new();
    let bare = fx.origin();
    let (placement, commit) = fx.submit("s").await;
    push(&fx.repo, &placement.branch, &placement.branch)
        .await
        .unwrap();
    assert_eq!(git(&bare, &["rev-parse", BRANCH]), commit);
    push(&fx.repo, &placement.branch, &placement.branch)
        .await
        .unwrap();
    // Under another name on the remote.
    push(&fx.repo, &placement.branch, "idea/fix-the-zoom")
        .await
        .unwrap();
    assert_eq!(git(&bare, &["rev-parse", "idea/fix-the-zoom"]), commit);
}

#[tokio::test]
async fn push_failures_are_named_without_gits_words() {
    let fx = Fixture::new();
    let (placement, _) = fx.submit("s").await;
    let branch = placement.branch.as_str();

    let failure = push(&fx.repo, branch, branch).await.unwrap_err();
    assert_eq!(failure.step, FeedbackStep::Push);
    assert_eq!(failure.code, "push_no_remote");
    assert_eq!(
        failure.message,
        "git could not push: the repository has no remote named origin"
    );

    git(
        fx.root(),
        &["remote", "add", "origin", "/nonexistent/LEAK-git-path"],
    );
    let failure = push(&fx.repo, branch, branch).await.unwrap_err();
    assert!(
        ["push_failed", "push_no_remote"].contains(&failure.code.as_str()),
        "{failure:?}"
    );
    for word in ["LEAK", "fatal", "nonexistent"] {
        assert!(!failure.message.contains(word), "{failure:?}");
    }
    if failure.code == "push_failed" {
        assert_eq!(
            failure.message,
            "git could not push to origin (exit status 128)"
        );
    }

    // A remote whose hook refuses every push.
    let bare = fx.tmp.path().join("refusing.git");
    git(
        fx.tmp.path(),
        &["init", "-q", "--bare", bare.to_str().unwrap()],
    );
    executable(
        &bare.join("hooks/pre-receive"),
        "#!/bin/sh\necho 'LEAK: no pushes today' >&2\nexit 1\n",
    );
    git(
        fx.root(),
        &["remote", "set-url", "origin", bare.to_str().unwrap()],
    );
    let failure = push(&fx.repo, branch, branch).await.unwrap_err();
    assert_eq!(failure.code, "push_rejected");
    assert_eq!(
        failure.message,
        "git could not push to origin: the remote rejected the branch"
    );

    // A branch of that name on the remote that is not an ancestor: never forced.
    std::fs::remove_file(bare.join("hooks/pre-receive")).unwrap();
    let theirs = git(fx.root(), &["commit-tree", "main^{tree}", "-m", "theirs"]);
    git(
        fx.root(),
        &[
            "push",
            "-q",
            "origin",
            &format!("{theirs}:refs/heads/{branch}"),
        ],
    );
    let failure = push(&fx.repo, branch, branch).await.unwrap_err();
    assert_eq!(failure.code, "push_rejected");
    assert_eq!(git(&bare, &["rev-parse", branch]), theirs);
}

#[tokio::test]
async fn a_failed_push_keeps_the_commit_and_a_retry_does_not_commit_again() {
    let fx = Fixture::new();
    let (placement, commit) = fx.submit("s").await;
    let failure = push(&fx.repo, &placement.branch, &placement.branch)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "push_no_remote");
    assert_eq!(git(fx.root(), &["rev-parse", BRANCH]), commit);

    let bare = fx.origin();
    push(&fx.repo, &placement.branch, &placement.branch)
        .await
        .unwrap();
    assert_eq!(git(&bare, &["rev-parse", BRANCH]), commit);
    assert_eq!(
        git(
            fx.root(),
            &["rev-list", "--count", &format!("main..{BRANCH}")]
        ),
        "1"
    );
    assert_eq!(git(&bare, &["rev-list", "--count", BRANCH]), "3");
}

#[tokio::test]
async fn a_push_that_does_not_finish_is_stopped() {
    let fx = Fixture::new();
    let bare = fx.origin();
    // The remote accepts the branch once its hook has slept for 2 seconds.
    executable(&bare.join("hooks/pre-receive"), "#!/bin/sh\nsleep 2\n");
    let (placement, _) = fx.submit("s").await;
    let started = Instant::now();
    let err = git::push_branch(
        fx.root(),
        &placement.branch,
        &placement.branch,
        Duration::from_secs(1),
    )
    .await
    .unwrap_err();
    assert_eq!(err, PushError::Timeout);
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
    // The whole push was killed, the remote side included: the branch never
    // arrives, even after the hook would have finished.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!git_succeeds(
        &bare,
        &["rev-parse", "--verify", "--quiet", BRANCH]
    ));
}

#[tokio::test]
async fn origin_url_reads_the_configured_url_not_the_rewritten_one() {
    let fx = Fixture::new();
    assert_eq!(git::origin_url(fx.root()).await.unwrap(), None);

    let bare = fx.tmp.path().join("widgets.git");
    git(
        fx.tmp.path(),
        &["init", "-q", "--bare", bare.to_str().unwrap()],
    );
    git(fx.root(), &["remote", "add", "origin", GITHUB_URL]);
    git(
        fx.root(),
        &[
            "config",
            &format!("url.{}.insteadOf", bare.display()),
            GITHUB_URL,
        ],
    );
    assert_eq!(
        git::origin_url(fx.root()).await.unwrap().as_deref(),
        Some(GITHUB_URL)
    );

    let (placement, commit) = fx.submit("s").await;
    push(&fx.repo, &placement.branch, &placement.branch)
        .await
        .unwrap();
    assert_eq!(git(&bare, &["rev-parse", BRANCH]), commit);
}

#[tokio::test]
async fn show_file_reads_a_file_from_a_branch_and_is_none_when_absent() {
    let fx = Fixture::new();
    let (placement, commit) = fx.submit("s").await;
    let issue = format!("{FOLDER}/issue.md");
    let root = fx.root();
    assert_eq!(
        git::show_file(root, &placement.branch, &issue, 1 << 20)
            .await
            .unwrap()
            .as_deref(),
        Some(ISSUE_MD)
    );
    assert_eq!(
        git::show_file(root, &commit, &issue, 1 << 20)
            .await
            .unwrap()
            .as_deref(),
        Some(ISSUE_MD)
    );
    // At most `max_bytes` are read.
    assert_eq!(
        git::show_file(root, &commit, &issue, 6)
            .await
            .unwrap()
            .as_deref(),
        Some("# Fix ")
    );
    assert_eq!(
        git::show_file(root, "main", &issue, 1 << 20).await.unwrap(),
        None
    );
    assert_eq!(
        git::show_file(root, &commit, "nope.md", 1 << 20)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        git::show_file(root, &commit, FOLDER, 1 << 20)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        git::show_file(root, "no-such-branch", &issue, 1 << 20)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn list_folder_names_the_files_of_one_folder() {
    let fx = Fixture::new();
    let (placement, commit) = fx.submit("s").await;
    let root = fx.root();
    let names = vec!["issue.md".to_string(), "screenshot-1.png".to_string()];
    assert_eq!(
        git::list_folder(root, &commit, FOLDER).await.unwrap(),
        names
    );
    assert_eq!(
        git::list_folder(root, &placement.branch, FOLDER)
            .await
            .unwrap(),
        names
    );
    assert!(git::list_folder(root, "main", FOLDER)
        .await
        .unwrap()
        .is_empty());
    assert!(git::list_folder(root, &commit, "docs/nope")
        .await
        .unwrap()
        .is_empty());
    assert!(git::list_folder(root, &commit, "README.md")
        .await
        .unwrap()
        .is_empty());

    // Subfolders are not files of the folder.
    fx.commit_on_main("docs/x/a.md", "a");
    fx.commit_on_main("docs/x/b c.png", "b");
    fx.commit_on_main("docs/x/sub/c.md", "c");
    assert_eq!(
        git::list_folder(root, "main", "docs/x").await.unwrap(),
        vec!["a.md".to_string(), "b c.png".to_string()]
    );
}

#[tokio::test]
async fn rev_parse_path_in_branch_and_delete_branch() {
    let fx = Fixture::new();
    let root = fx.root();
    let tip = git(root, &["rev-parse", "main"]);
    assert_eq!(
        git::rev_parse(root, "main").await.unwrap(),
        Some(tip.clone())
    );
    assert_eq!(
        git::rev_parse(root, "refs/heads/main").await.unwrap(),
        Some(tip.clone())
    );
    assert_eq!(git::rev_parse(root, &tip).await.unwrap(), Some(tip.clone()));
    assert_eq!(git::rev_parse(root, "no-such-branch").await.unwrap(), None);
    // A tree names no commit.
    assert_eq!(git::rev_parse(root, "main^{tree}").await.unwrap(), None);

    assert!(git::path_in_branch(root, "main", "README.md")
        .await
        .unwrap());
    assert!(!git::path_in_branch(root, "main", "docs").await.unwrap());
    assert!(!git::path_in_branch(root, "no-such-branch", "README.md")
        .await
        .unwrap());

    git(root, &["branch", "feedback/x"]);
    git::delete_branch(root, "feedback/x").await.unwrap();
    assert!(!git::branch_exists(root, "feedback/x").await.unwrap());
    let err = git::delete_branch(root, "feedback/x").await.unwrap_err();
    assert!(
        matches!(err, GitError::Failed { status: Some(_) }),
        "{err:?}"
    );

    let locked = fx.tmp.path().join("locked");
    git(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            "--lock",
            "--reason",
            "a reason",
            locked.to_str().unwrap(),
        ],
    );
    let entries = git::list_worktrees(root).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].path, root);
    assert_eq!(entries[0].locked, None);
    assert_eq!(entries[1].path, std::fs::canonicalize(&locked).unwrap());
    assert_eq!(entries[1].locked.as_deref(), Some("a reason"));
    git::remove_worktree_at(root, &entries[1].path)
        .await
        .unwrap();
    assert_eq!(git::list_worktrees(root).await.unwrap().len(), 1);
}

/// A log writer that keeps everything written to it.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn nothing_git_prints_reaches_an_error_or_a_log() {
    isolate_git_config();
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let _logging = tracing::subscriber::set_default(subscriber);

    let fx = Fixture::new();
    let mut texts = Vec::new();
    // The identity git asks for.
    git(fx.root(), &["config", "--unset", "user.name"]);
    let placement = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    let scratch = fx.scratch("s1");
    let files = fx.files(&scratch);
    let err = commit_folder(&fx.repo, &placement, &files, "Fix the zoom", id(), &scratch)
        .await
        .unwrap_err();
    texts.push(err.to_string());
    git(fx.root(), &["config", "user.name", "Test User"]);
    // Pushes git and the remote explain at length.
    let (placement, _) = fx.submit("s2").await;
    for url in [
        "/nonexistent/LEAK-git-path",
        "file:///nonexistent/LEAK-token",
    ] {
        let _ = git_cmd(fx.root(), &["remote", "remove", "origin"]).output();
        git(fx.root(), &["remote", "add", "origin", url]);
        let failure = push(&fx.repo, &placement.branch, &placement.branch)
            .await
            .unwrap_err();
        texts.push(failure.message);
    }
    // The default branch git cannot find.
    git(fx.root(), &["branch", "-m", "main", "trunk"]);
    texts.push(
        place(&fx.repo, "Fix the zoom", date(), false)
            .await
            .unwrap_err()
            .to_string(),
    );

    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    for text in texts.iter().chain([&logs]) {
        for word in [
            "LEAK",
            "fatal",
            "error:",
            "hint:",
            "Please tell me",
            "nonexistent",
        ] {
            assert!(!text.contains(word), "{word:?} in {text:?}");
        }
    }
}

// ---- Fix round 1 ----

/// F1: git is never told to prune; only this tool's own registrations go.
#[tokio::test]
async fn a_users_worktree_whose_folder_moved_survives_every_path() {
    let fx = Fixture::new();
    let admin = moved_user_worktree(&fx);

    // Success.
    fx.submit("s1").await;
    assert!(admin.join("HEAD").exists(), "the success path pruned it");

    // Failure.
    git(fx.root(), &["config", "--unset", "user.name"]);
    let placement = place(&fx.repo, "Other", date(), false).await.unwrap();
    let scratch = fx.scratch("s2");
    let files = fx.files(&scratch);
    let err = commit_folder(&fx.repo, &placement, &files, "Other", id(), &scratch)
        .await
        .unwrap_err();
    assert!(matches!(err, FeedbackError::GitIdentityMissing), "{err:?}");
    assert!(admin.join("HEAD").exists(), "the failure path pruned it");
    git(fx.root(), &["config", "user.name", "Test User"]);

    // Leftovers of a crashed run go; a worktree under the feedback directory
    // that this tool did not lock is left alone, as is the user's.
    let crashed = crashed_leftover(&fx, "crashed");
    let foreign = fx.scratch("foreign");
    git(
        fx.root(),
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            foreign.to_str().unwrap(),
            "main",
        ],
    );
    std::fs::remove_dir_all(&foreign).unwrap();
    let placement = place(&fx.repo, "Third", date(), false).await.unwrap();
    let scratch = fx.scratch("s3");
    let files = fx.files(&scratch);
    commit_folder(&fx.repo, &placement, &files, "Third", id(), &scratch)
        .await
        .unwrap();
    let paths = worktree_paths(fx.root());
    assert!(!paths.contains(&crashed.display().to_string()), "{paths:?}");
    assert_eq!(
        paths.len(),
        3,
        "the checkout, the user's, the foreign: {paths:?}"
    );
    assert!(admin.join("HEAD").exists(), "the leftover path pruned it");
}

/// F2: feedback files are committed byte for byte, whatever the
/// repository's attributes and filters say, and no filter runs.
#[tokio::test]
async fn repository_attributes_and_filters_do_not_shape_the_commit() {
    let fx = Fixture::new();
    let ran = fx.tmp.path().join("filter-ran");
    let filter = fx.tmp.path().join("filter");
    executable(
        &filter,
        &format!("#!/bin/sh\ntouch '{}'\nexit 1\n", ran.display()),
    );
    std::fs::write(
        fx.root().join(".gitattributes"),
        "*.jpg filter=lfs diff=lfs merge=lfs -text\n\
         *.png filter=custom\n\
         *.md working-tree-encoding=UTF-16 ident text eol=crlf\n",
    )
    .unwrap();
    git(fx.root(), &["add", ".gitattributes"]);
    git(fx.root(), &["commit", "-q", "-m", "attributes"]);
    let filter = filter.to_str().unwrap();
    for driver in ["lfs", "custom"] {
        for (key, value) in [
            ("clean", filter),
            ("smudge", filter),
            ("process", filter),
            ("required", "true"),
        ] {
            git(
                fx.root(),
                &["config", &format!("filter.{driver}.{key}"), value],
            );
        }
    }
    git(fx.root(), &["config", "core.autocrlf", "true"]);
    // `info/attributes` outranks every other attribute source.
    std::fs::write(
        fx.root().join(".git/info/attributes"),
        "* filter=infofilter ident\n",
    )
    .unwrap();
    for (key, value) in [("clean", filter), ("required", "true")] {
        git(
            fx.root(),
            &["config", &format!("filter.infofilter.{key}"), value],
        );
    }

    let issue = "# Fix\r\nid: $Id: 0123 $\nna\u{ef}ve\n";
    let jpeg: &[u8] = b"\xff\xd8\xff\xe0 a jpeg \r\n";
    let scratch = fx.scratch("s");
    let mut files = fx.files(&scratch);
    files[0].source = FileSource::Text(issue.into());
    let staged = scratch.join("upload/file-2");
    std::fs::write(&staged, jpeg).unwrap();
    files.push(FolderFile {
        name: "image-1.jpg".into(),
        source: FileSource::Staged(staged),
    });
    let placement = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    let commit = commit_folder(&fx.repo, &placement, &files, "Fix the zoom", id(), &scratch)
        .await
        .unwrap();

    let blob = |name: &str| {
        git_bytes(
            fx.root(),
            &["cat-file", "blob", &format!("{commit}:{FOLDER}/{name}")],
        )
    };
    assert_eq!(blob("issue.md"), issue.as_bytes());
    assert_eq!(blob("screenshot-1.png"), SHOT);
    assert_eq!(blob("image-1.jpg"), jpeg);
    assert!(!ran.exists(), "a filter of the repository ran");
}

/// F3: a repository with a sparse checkout can take feedback, and its
/// sparse checkout stays as it was.
#[tokio::test]
async fn a_sparse_checkout_repository_can_submit() {
    let fx = Fixture::new();
    fx.commit_on_main("src/a.txt", "a\n");
    fx.commit_on_main("other/b.txt", "b\n");
    git(fx.root(), &["sparse-checkout", "set", "src"]);
    let sparse = git(fx.root(), &["sparse-checkout", "list"]);
    let status = git(fx.root(), &["status", "--porcelain=v2", "--branch"]);

    let (_, commit) = fx.submit("s").await;

    assert_eq!(
        git(fx.root(), &["show", "--name-only", "--format=", &commit]),
        format!("{FOLDER}/issue.md\n{FOLDER}/screenshot-1.png")
    );
    let tree = git(fx.root(), &["ls-tree", "-r", "--name-only", &commit]);
    assert!(
        tree.contains("other/b.txt") && tree.contains("src/a.txt"),
        "{tree}"
    );
    assert_eq!(git(fx.root(), &["sparse-checkout", "list"]), sparse);
    assert_eq!(
        git(fx.root(), &["status", "--porcelain=v2", "--branch"]),
        status
    );
}

/// F4: a name that was taken after `place` chose it is its own outcome, and
/// nothing is created.
#[tokio::test]
async fn a_name_taken_since_placing_is_reported_and_nothing_is_created() {
    let fx = Fixture::new();
    let placement = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    git(fx.root(), &["branch", BRANCH]);
    let scratch = fx.scratch("s1");
    let files = fx.files(&scratch);
    let err = commit_folder(&fx.repo, &placement, &files, "Fix the zoom", id(), &scratch)
        .await
        .unwrap_err();
    assert!(matches!(err, FeedbackError::NameTaken), "{err:?}");
    assert_eq!(err.code(), "name_taken");
    assert_eq!(err.status().as_u16(), 409);
    assert_eq!(
        err.to_string(),
        "an issue folder or branch of this name was created meanwhile; submit again"
    );
    assert_eq!(
        git(fx.root(), &["rev-parse", BRANCH]),
        git(fx.root(), &["rev-parse", "main"])
    );
    assert!(!scratch.join("wt").exists());
    assert!(
        scratch.join("upload/file-1").exists(),
        "the upload stays for a retry"
    );

    // The folder appeared on the default branch.
    let fresh = Fixture::new();
    let placement = place(&fresh.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    fresh.commit_on_main(&format!("{FOLDER}/issue.md"), "theirs\n");
    let scratch = fresh.scratch("s");
    let files = fresh.files(&scratch);
    let err = commit_folder(
        &fresh.repo,
        &placement,
        &files,
        "Fix the zoom",
        id(),
        &scratch,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, FeedbackError::NameTaken), "{err:?}");
    assert_eq!(fresh.feedback_branches(), "");
}

/// F4: the same title submitted twice at once gets the next suffix.
#[tokio::test]
async fn the_same_title_submitted_twice_at_once_gets_two_names() {
    let fx = Fixture::new();
    let (scratch_a, scratch_b) = (fx.scratch("a"), fx.scratch("b"));
    let (files_a, files_b) = (fx.files(&scratch_a), fx.files(&scratch_b));
    let (a, b) = tokio::join!(
        place_and_commit(
            &fx.repo,
            "Fix the zoom",
            date(),
            false,
            &files_a,
            id(),
            &scratch_a
        ),
        place_and_commit(
            &fx.repo,
            "Fix the zoom",
            date(),
            false,
            &files_b,
            id(),
            &scratch_b
        ),
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    let mut slugs = vec![a.0.slug.clone(), b.0.slug.clone()];
    slugs.sort();
    assert_eq!(slugs, ["fix-the-zoom", "fix-the-zoom-2"]);
    for (placement, commit) in [&a, &b] {
        assert_eq!(git(fx.root(), &["rev-parse", &placement.branch]), *commit);
        assert_eq!(
            git(fx.root(), &["show", "--name-only", "--format=", commit]),
            format!("{0}/issue.md\n{0}/screenshot-1.png", placement.folder)
        );
    }
    assert_eq!(fx.feedback_branches().lines().count(), 2);
    assert_eq!(fx.worktree_lines(), 1);
}

/// Holds the lock of `branch` the way a stuck git process would, and makes
/// git wait for such a lock for ever.
fn hold_branch_lock(fx: &Fixture, branch: &str) -> PathBuf {
    git(fx.root(), &["config", "core.filesRefLockTimeout", "-1"]);
    let lock = fx
        .root()
        .join(".git/refs/heads")
        .join(format!("{branch}.lock"));
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, "").unwrap();
    lock
}

/// F5: a git that never returns is stopped; what this tool made is removed
/// and the lock it did not make is left.
#[tokio::test]
async fn a_git_that_never_returns_is_stopped_and_cleaned_up() {
    let fx = Fixture::new();
    let placement = place(&fx.repo, "Hang one", date(), false).await.unwrap();
    let lock = hold_branch_lock(&fx, &placement.branch);
    let scratch = fx.scratch("s");
    let files = fx.files(&scratch);
    let started = Instant::now();
    let err = git::with_git_timeout(
        Duration::from_secs(2),
        commit_folder(&fx.repo, &placement, &files, "Hang one", id(), &scratch),
    )
    .await
    .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    assert!(
        matches!(
            err,
            FeedbackError::Git {
                step: "commit the files",
                status: None
            }
        ),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "git could not commit the files (no exit status)"
    );
    assert!(lock.exists(), "a lock this tool did not make is left");
    assert!(!git::branch_exists(fx.root(), &placement.branch)
        .await
        .unwrap());
    assert!(!scratch.join("wt").exists());
    assert_eq!(fx.worktree_lines(), 1);
    assert!(!process_running(&placement.branch), "git still runs");
}

/// F5: dropping the future while git hangs kills git and cleans up.
#[tokio::test]
async fn dropping_a_hung_commit_kills_git_and_cleans_up() {
    let fx = Fixture::new();
    let placement = place(&fx.repo, "Hang two", date(), false).await.unwrap();
    let lock = hold_branch_lock(&fx, &placement.branch);
    let scratch = fx.scratch("s");
    let files = fx.files(&scratch);
    let dropped = tokio::time::timeout(
        Duration::from_secs(3),
        commit_folder(&fx.repo, &placement, &files, "Hang two", id(), &scratch),
    )
    .await;
    assert!(dropped.is_err(), "the commit should have hung: {dropped:?}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!process_running(&placement.branch), "git still runs");
    assert!(lock.exists());
    assert!(!git::branch_exists(fx.root(), &placement.branch)
        .await
        .unwrap());
    assert!(!scratch.join("wt").exists());
    assert_eq!(fx.worktree_lines(), 1);
}

/// F7: the URL is the one pushes go to, with any credential in it removed.
#[tokio::test]
async fn origin_url_prefers_the_push_url_and_hides_credentials() {
    let fx = Fixture::new();
    git(
        fx.root(),
        &[
            "remote",
            "add",
            "origin",
            "https://someone:LEAK-token@github.com/smithagents-test-invalid/widgets.git",
        ],
    );
    assert_eq!(
        git::origin_url(fx.root()).await.unwrap().as_deref(),
        Some(GITHUB_URL)
    );
    git(
        fx.root(),
        &[
            "config",
            "remote.origin.pushurl",
            "https://x-access-token:LEAK@github.com/smithagents-test-invalid/fork.git",
        ],
    );
    assert_eq!(
        git::origin_url(fx.root()).await.unwrap().as_deref(),
        Some("https://github.com/smithagents-test-invalid/fork.git")
    );
}

/// F7: a remote with only a push URL is a remote.
#[tokio::test]
async fn a_push_url_alone_is_a_remote() {
    let fx = Fixture::new();
    let bare = fx.tmp.path().join("push-only.git");
    git(
        fx.tmp.path(),
        &["init", "-q", "--bare", bare.to_str().unwrap()],
    );
    git(
        fx.root(),
        &["config", "remote.origin.pushurl", bare.to_str().unwrap()],
    );
    assert_eq!(
        git::origin_url(fx.root()).await.unwrap(),
        Some(bare.display().to_string())
    );
    let (placement, commit) = fx.submit("s").await;
    push(&fx.repo, &placement.branch, &placement.branch)
        .await
        .unwrap();
    assert_eq!(git(&bare, &["rev-parse", BRANCH]), commit);
}

/// F8: a branch name is read as a branch, even when a tag has its name.
#[tokio::test]
async fn a_tag_named_like_a_branch_does_not_win() {
    let fx = Fixture::new();
    fx.commit_on_main("docs/x/a.md", "tag\n");
    git(fx.root(), &["tag", "fix"]);
    git(fx.root(), &["checkout", "-q", "-b", "fix"]);
    fx.commit_on_main("docs/x/a.md", "branch\n");
    fx.commit_on_main("docs/x/b.md", "b\n");
    git(fx.root(), &["checkout", "-q", "main"]);
    let root = fx.root();
    assert_eq!(
        git::rev_parse(root, "fix").await.unwrap(),
        Some(git(root, &["rev-parse", "refs/heads/fix"]))
    );
    assert_ne!(
        git(root, &["rev-parse", "refs/tags/fix"]),
        git(root, &["rev-parse", "refs/heads/fix"])
    );
    assert_eq!(
        git::show_file(root, "fix", "docs/x/a.md", 100)
            .await
            .unwrap()
            .as_deref(),
        Some("branch\n")
    );
    assert_eq!(
        git::show_file(root, "refs/heads/fix", "docs/x/a.md", 100)
            .await
            .unwrap()
            .as_deref(),
        Some("branch\n")
    );
    assert_eq!(
        git::list_folder(root, "fix", "docs/x").await.unwrap(),
        vec!["a.md".to_string(), "b.md".to_string()]
    );
}

/// F8: a path is a path, never a pattern.
#[tokio::test]
async fn pathspec_magic_in_a_path_is_literal() {
    let fx = Fixture::new();
    let dir = fx.tmp.path().join("wt");
    let tip = git(fx.root(), &["rev-parse", "main"]);
    git::add_detached_worktree(fx.root(), &dir, &tip, "a test of pathspecs")
        .await
        .unwrap();
    std::fs::write(dir.join("x.md"), "x").unwrap();
    std::fs::write(dir.join("[x].md"), "pattern").unwrap();
    std::fs::write(dir.join(":(glob)y.md"), "magic").unwrap();
    std::fs::write(dir.join("y.md"), "y").unwrap();
    let commit = git::commit_paths(&dir, &["[x].md", ":(glob)y.md"], "m")
        .await
        .unwrap();
    assert_eq!(
        git(fx.root(), &["show", "--name-only", "--format=", &commit]),
        ":(glob)y.md\n[x].md"
    );
    git(
        fx.root(),
        &["worktree", "remove", "-f", "-f", dir.to_str().unwrap()],
    );
}

/// F8: the message goes to git on stdin, as one clean line.
#[tokio::test]
async fn a_long_title_and_a_bidi_override_make_one_clean_subject() {
    let fx = Fixture::new();
    let long = "a".repeat(300 * 1024);
    let placement = place(&fx.repo, "Long", date(), false).await.unwrap();
    let scratch = fx.scratch("s1");
    let files = fx.files(&scratch);
    let commit = commit_folder(&fx.repo, &placement, &files, &long, id(), &scratch)
        .await
        .unwrap();
    assert_eq!(
        git(fx.root(), &["log", "-1", "--format=%s", &commit]),
        format!("docs(issues): {long}")
    );

    let placement = place(&fx.repo, "Bidi", date(), false).await.unwrap();
    let scratch = fx.scratch("s2");
    let files = fx.files(&scratch);
    let title = "Fix \u{202e}the\u{200b} zoom\u{2066}\u{feff}";
    let submission = id();
    let commit = commit_folder(&fx.repo, &placement, &files, title, submission, &scratch)
        .await
        .unwrap();
    assert_eq!(
        git(fx.root(), &["log", "-1", "--format=%B", &commit]),
        format!("docs(issues): Fix the zoom\n\nFeedback-Id: {submission}")
    );
}

/// F9: a file where the issue folders go is never replaced.
#[tokio::test]
async fn a_file_where_the_folders_go_is_refused() {
    for path in ["docs/issues", "docs"] {
        let fx = Fixture::new();
        let placement = Placement {
            slug: "fix-the-zoom".into(),
            folder: FOLDER.into(),
            branch: BRANCH.into(),
        };
        fx.commit_on_main(path, "the user's file\n");
        let err = place(&fx.repo, "Fix the zoom", date(), false)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, FeedbackError::FolderBlocked(p) if p == path),
            "{err:?}"
        );
        assert_eq!(
            err.to_string(),
            format!("{path} is a file in this repository, so feedback cannot be stored there.")
        );
        assert_eq!(err.code(), "folder_blocked");
        assert_eq!(err.status().as_u16(), 409);

        let scratch = fx.scratch("s");
        let files = fx.files(&scratch);
        let err = commit_folder(&fx.repo, &placement, &files, "Fix the zoom", id(), &scratch)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, FeedbackError::FolderBlocked(p) if p == path),
            "{err:?}"
        );
        assert_eq!(fx.feedback_branches(), "");
        assert_eq!(fx.worktree_lines(), 1);
    }
}

/// F9: a branch named `feedback` or `idea` blocks every branch under it.
#[tokio::test]
async fn a_branch_named_like_the_prefix_is_refused() {
    let fx = Fixture::new();
    git(fx.root(), &["branch", "feedback"]);
    let err = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, FeedbackError::BranchBlocked(b) if b == "feedback"),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "a branch named feedback exists in this repository, so no branch can be created under feedback/"
    );
    assert_eq!(err.code(), "branch_blocked");
    assert_eq!(err.status().as_u16(), 409);
    let scratch = fx.scratch("s");
    let files = fx.files(&scratch);
    let placement = Placement {
        slug: "fix-the-zoom".into(),
        folder: FOLDER.into(),
        branch: BRANCH.into(),
    };
    let err = commit_folder(&fx.repo, &placement, &files, "Fix the zoom", id(), &scratch)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, FeedbackError::BranchBlocked(b) if b == "feedback"),
        "{err:?}"
    );

    // An idea is not blocked by `feedback`, but is by `idea`.
    assert_eq!(
        place(&fx.repo, "Fix the zoom", date(), true)
            .await
            .unwrap()
            .branch,
        "idea/fix-the-zoom"
    );
    git(fx.root(), &["branch", "idea"]);
    let err = place(&fx.repo, "Fix the zoom", date(), true)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, FeedbackError::BranchBlocked(b) if b == "idea"),
        "{err:?}"
    );
}

// ---- Fix round 2 ----

/// A commit id that is not in the test repositories: a submodule's commit
/// lives in the submodule, so `git cat-file` cannot read it.
const ABSENT_COMMIT: &str = "1234567890123456789012345678901234567890";

/// Commits a submodule entry (a gitlink, mode 160000) at `path` on `main`.
fn commit_submodule(fx: &Fixture, path: &str) {
    git(
        fx.root(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{ABSENT_COMMIT},{path}"),
        ],
    );
    git(fx.root(), &["commit", "-q", "-m", "a submodule"]);
}

/// Item 2: a submodule where the folders go gets the fixed sentence.
#[tokio::test]
async fn a_submodule_where_the_folders_go_is_refused() {
    for path in ["docs", "docs/issues"] {
        let fx = Fixture::new();
        commit_submodule(&fx, path);
        let err = place(&fx.repo, "Fix the zoom", date(), false)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, FeedbackError::FolderBlocked(p) if p == path),
            "{path}: {err:?}"
        );
        let placement = Placement {
            slug: "fix-the-zoom".into(),
            folder: FOLDER.into(),
            branch: BRANCH.into(),
        };
        let scratch = fx.scratch("s");
        let files = fx.files(&scratch);
        let err = commit_folder(&fx.repo, &placement, &files, "Fix the zoom", id(), &scratch)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, FeedbackError::FolderBlocked(p) if p == path),
            "{path}: {err:?}"
        );
        assert_eq!(fx.feedback_branches(), "");
        assert_eq!(fx.worktree_lines(), 1);
    }
}

/// Item 2: a submodule at the folder itself takes the name.
#[tokio::test]
async fn a_submodule_at_the_folder_itself_takes_the_name() {
    let fx = Fixture::new();
    let stale = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    commit_submodule(&fx, FOLDER);
    let placement = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    assert_eq!(placement.slug, "fix-the-zoom-2");
    let scratch = fx.scratch("s");
    let files = fx.files(&scratch);
    let err = commit_folder(&fx.repo, &stale, &files, "Fix the zoom", id(), &scratch)
        .await
        .unwrap_err();
    assert!(matches!(err, FeedbackError::NameTaken), "{err:?}");
    assert_eq!(
        git::path_kind(fx.root(), "main", FOLDER)
            .await
            .unwrap()
            .as_deref(),
        Some("commit")
    );
    assert_eq!(fx.feedback_branches(), "");
}

// ------------------------------------------------ the commit names its submission

#[tokio::test]
async fn the_commit_names_its_submission_in_a_trailer_and_is_found_by_it() {
    let fx = Fixture::new();
    let placement = place(&fx.repo, "Fix the zoom", date(), false)
        .await
        .unwrap();
    let scratch = fx.scratch("s");
    let files = fx.files(&scratch);
    let submission = id();
    let commit = commit_folder(
        &fx.repo,
        &placement,
        &files,
        "Fix the zoom",
        submission,
        &scratch,
    )
    .await
    .unwrap();

    let trailer = git(
        fx.root(),
        &[
            "log",
            "-1",
            "--format=%(trailers:key=Feedback-Id,valueonly)",
            &commit,
        ],
    );
    assert_eq!(trailer, submission.to_string());
    let found = find_committed(&fx.repo, submission).await.unwrap();
    assert_eq!(
        found,
        Some(Committed {
            branch: BRANCH.into(),
            folder: FOLDER.into(),
            commit: commit.clone(),
        })
    );
    // Another submission's id finds nothing.
    assert_eq!(find_committed(&fx.repo, id()).await.unwrap(), None);
}

#[tokio::test]
async fn an_idea_branch_is_found_by_its_trailer_too() {
    let fx = Fixture::new();
    let scratch = fx.scratch("s");
    let files = fx.files(&scratch);
    let submission = id();
    let (placement, commit) = place_and_commit(
        &fx.repo,
        "Fix the zoom",
        date(),
        true,
        &files,
        submission,
        &scratch,
    )
    .await
    .unwrap();
    assert_eq!(placement.branch, "idea/fix-the-zoom");

    let found = find_committed(&fx.repo, submission).await.unwrap().unwrap();
    assert_eq!(
        (found.branch.as_str(), found.folder.as_str(), found.commit),
        ("idea/fix-the-zoom", FOLDER, commit)
    );
}

#[tokio::test]
async fn a_branch_is_deleted_only_at_the_commit_it_was_made_at() {
    let fx = Fixture::new();
    let (placement, commit) = fx.submit("s").await;
    let main = git(fx.root(), &["rev-parse", "refs/heads/main"]);

    // Moved on since: kept.
    let moved = git::delete_branch_at(fx.root(), &placement.branch, &main).await;
    assert!(moved.is_err(), "{moved:?}");
    assert_eq!(
        git(fx.root(), &["rev-parse", &format!("refs/heads/{BRANCH}")]),
        commit
    );
    // Still at its commit: deleted, and nothing else.
    git::delete_branch_at(fx.root(), &placement.branch, &commit)
        .await
        .unwrap();
    assert_eq!(git(fx.root(), &["branch", "--list", "feedback/*"]), "");
    assert_eq!(git(fx.root(), &["rev-parse", "refs/heads/main"]), main);
    // A name or commit that could be an option is refused before git runs.
    for (branch, at) in [("-d", commit.as_str()), (BRANCH, "-x"), (BRANCH, "main")] {
        let refused = git::delete_branch_at(fx.root(), branch, at).await;
        assert!(
            matches!(refused, Err(GitError::UnsafeArgument)),
            "{branch} {at}: {refused:?}"
        );
    }
}

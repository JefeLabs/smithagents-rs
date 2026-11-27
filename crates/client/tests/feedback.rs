//! Feedback through the client, against the real `smithagentsd`: a
//! submission becomes an intent whose agent has the files, or a GitHub issue
//! whose failed push is retried, or an idea whose draft pull request and
//! implementation intent follow; work on the default branch that `origin`
//! has not got is never published.
//!
//! Nothing leaves the machine and nothing real runs:
//! - `gh` and `agent-browser` are the fakes of `crates/server/tests/bin`,
//!   copied into a scratch `bin` that leads the daemon's `PATH`. The daemon
//!   also looks in the usual install places, where a real, signed-in `gh`
//!   may be: before the daemon starts, each test checks that the fakes are
//!   there, executable and trusted, and that their directory comes first;
//!   after a `gh` step, the fake's own log shows that it is what ran.
//! - `claude` is `fake-claude`: polish and the agents' turns cost nothing.
//! - The repository's `origin` is [`GITHUB_REMOTE`], which the repository's
//!   own `url.<bare>.insteadOf` sends to a bare repository in the scratch
//!   directory; before the daemon starts, each test checks that git, as the
//!   daemon runs it, fetches from and pushes to that bare repository. The
//!   daemon's `HOME` is an empty directory, so no configuration of this
//!   machine's user (an `insteadOf` or `pushInsteadOf`) can send it
//!   elsewhere.
//!
//! Not covered here: the daemon stopping between a commit and its record.
//! The daemon binary has no point to stop it there (`gh` runs before the
//! commit or after the record, and git runs no hook), and the case is not
//! faked by editing the store; `crates/server/tests/feedback_api.rs` covers
//! it in process.
#![cfg(unix)]

mod common;

use std::ffi::OsString;
use std::fs;
use std::future::Future;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use futures::{Stream, StreamExt};
use serde_json::{json, Value};
use smithagents_client::{ensure_daemon_with, Client, ClientError, FeedbackFile, SpawnOptions};
use smithagents_core::agent::{Agent, AgentState};
use smithagents_core::daemon::read_lockfile;
use smithagents_core::event::{Event, EventKind};
use smithagents_core::feedback::{
    AttachmentKind, FeedbackAttachment, FeedbackBox, FeedbackKind, FeedbackRequest, FeedbackStep,
    IdeaOptions, SendAs, SubmissionId,
};
use smithagents_core::ids::{IntentId, RepoId};
use smithagents_core::intent::{AnswerInput, Intent, IntentKind, IntentState, ItemStatus, Phase};
use smithagents_core::plugin::PluginState;
use smithagents_core::VERSION;

use common::{
    claude_fixture, daemon_bin, fake_claude, fake_tools, git, github_repo_with_local_origin,
    TestHome, GITHUB_REMOTE,
};

const PATIENCE: Duration = Duration::from_secs(60);
/// The feedback plugin's secret as the daemon gets it: `gh` must never see it.
const SECRET_VAR: &str = "SMITHAGENTS_SECRET_FEEDBACK__TOKEN";
const SECRET: &str = "LEAK-secret";
/// The comment of the uploaded screenshot's box.
const COMMENT: &str = "The toolbar covers the zoom buttons";
const MAIN_IDEA: &str = "Fix the zoom\n\nThe page is drawn too big.";
/// The answers of the fake `gh`.
const ISSUE_URL_END: &str = "/issues/7";
const PR_URL_END: &str = "/pull/8";
const UNPUSHED: &str = "The default branch has commits that are not on origin. Push them first, so they are not published with this feedback.";
const PUBLIC: &str = "This repository is public. The screenshots will be visible to anyone. Confirm that before sending.";
/// An idea package's fixed criteria for an explainer and a demo video.
const CRITERION_EXPLAINER: &str = "`explainer.md` exists in the issue folder and covers what, why, how it could work and open questions.";
const CRITERION_DEMO: &str = "`demo.mp4` (at most 50 MB) shows the bug or the idea in a browser, and `demo.md` lists the steps taken.";
const CRITERION_SCOPE: &str =
    "Only files inside the issue folder are changed, and nothing is implemented.";

/// A 4×4 RGB PNG.
const PNG_4X4: [u8; 73] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x04, 0x08, 0x02, 0x00, 0x00, 0x00, 0x26, 0x93, 0x09,
    0x29, 0x00, 0x00, 0x00, 0x10, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x30, 0x4e, 0x9b, 0x09,
    0x47, 0x0c, 0xc4, 0x71, 0x00, 0xeb, 0x23, 0x13, 0x21, 0x2a, 0x62, 0x60, 0x73, 0x00, 0x00, 0x00,
    0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
/// A small file that starts like a JPEG (the daemon tells a file's type by
/// its first bytes).
const JPEG: &[u8] = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00\xff\xd9";

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(PATIENCE, fut)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish within {PATIENCE:?}"))
}

/// A first polish pass with one question.
fn pass_with_a_question() -> Value {
    json!({
        "title": "Fix the zoom",
        "kind": "bug",
        "current_behavior": "The page is drawn too big.",
        "questions": [
            {"text": "Which screens?", "options": [{"label": "All"}, {"label": "The editor"}]}
        ]
    })
}

/// A second polish pass that adds a precondition and a criterion.
fn second_pass() -> Value {
    json!({
        "preconditions": ["The zoom buttons exist"],
        "acceptance_criteria": ["The toolbar leaves the zoom buttons visible"],
        "stories": [],
        "depends_on": []
    })
}

/// Both passes at once, asking nothing and adding nothing: an intent goes
/// straight to Complete when every item it has is accepted.
fn passes_that_add_nothing() -> Value {
    json!({
        "title": "Fix the zoom",
        "kind": "bug",
        "current_behavior": "",
        "questions": [],
        "preconditions": [],
        "acceptance_criteria": [],
        "stories": [],
        "depends_on": []
    })
}

struct Fx {
    home: TestHome,
    client: Client,
    /// The fake `gh`'s control directory (`FAKE_GH_DIR`): its logs.
    gh_dir: PathBuf,
    /// fake-claude's control directory.
    claude_dir: PathBuf,
    repo_dir: PathBuf,
    bare: PathBuf,
}

/// Starts the daemon of a new home: the fakes first on its `PATH` (checked
/// first), the fake `gh`'s control files `gh_controls` written, fake-claude
/// answering the first polish pass with `pass1`, and a GitHub repository
/// whose `origin` is a local bare repository (checked first).
async fn start(pass1: &Value, gh_controls: &[(&str, &str)]) -> Fx {
    let home = TestHome::new();
    let scratch = home.scratch().to_owned();
    let (bin, gh_dir) = fake_tools(&scratch);
    for (file, text) in gh_controls {
        fs::write(gh_dir.join(file), text).unwrap();
    }
    let (repo_dir, bare) = github_repo_with_local_origin(&scratch);
    let user_home = scratch.join("user-home");
    fs::create_dir_all(&user_home).unwrap();
    let claude_dir = scratch.join("claude");
    fs::create_dir_all(&claude_dir).unwrap();
    let pass1_file = scratch.join("pass1.json");
    fs::write(&pass1_file, pass1.to_string()).unwrap();
    fs::write(claude_dir.join("structured"), pass1_file.to_str().unwrap()).unwrap();

    let path = lead_path_with(&bin);
    assert_the_fakes_come_first(&bin, &path);
    assert_origin_is_the_bare_repository(&repo_dir, &bare, &user_home);

    let options = SpawnOptions {
        args: vec!["--claude-bin".into(), fake_claude().into()],
        env: vec![
            ("PATH".into(), path),
            ("FAKE_GH_DIR".into(), gh_dir.clone().into()),
            (SECRET_VAR.into(), SECRET.into()),
            ("HOME".into(), user_home.clone().into()),
            ("XDG_CONFIG_HOME".into(), user_home.into()),
            ("FAKE_CLAUDE_FIXTURE".into(), claude_fixture().into()),
            ("FAKE_CLAUDE_STRUCTURED_FILE".into(), pass1_file.into()),
            ("FAKE_CLAUDE_CONTROL_DIR".into(), claude_dir.clone().into()),
        ],
    };
    let client = within(
        "ensure_daemon_with",
        ensure_daemon_with(home.path(), daemon_bin(), VERSION, &options),
    )
    .await
    .unwrap_or_else(|e| panic!("ensure failed: {e}; log:\n{}", home.log()));
    Fx {
        home,
        client,
        gh_dir,
        claude_dir,
        repo_dir,
        bare,
    }
}

/// `bin`, then this test's own `PATH`.
fn lead_path_with(bin: &Path) -> OsString {
    let own = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![bin.to_owned()];
    dirs.extend(std::env::split_paths(&own));
    std::env::join_paths(dirs).unwrap()
}

/// The daemon finds the fakes before any real tool: `bin` is the first
/// directory of `path`, and holds the fake `gh` and `agent-browser` as files
/// the daemon trusts (this user's, mode 0755, in a directory only this user
/// can write) and runs.
fn assert_the_fakes_come_first(bin: &Path, path: &OsString) {
    let first = std::env::split_paths(path).next();
    assert_eq!(first.as_deref(), Some(bin), "the fakes lead the PATH");
    let me = rustix::process::geteuid().as_raw();
    let dir = fs::metadata(bin).unwrap();
    assert_eq!(dir.uid(), me, "the fakes' directory is this user's");
    assert_eq!(
        dir.mode() & 0o022,
        0,
        "only this user can write the fakes' directory"
    );
    for (name, says) in [
        ("gh", "Test double for the GitHub CLI"),
        ("agent-browser", "Test double for `agent-browser`"),
    ] {
        let fake = bin.join(name);
        let meta = fs::metadata(&fake).unwrap_or_else(|e| panic!("no fake {name}: {e}"));
        assert!(meta.is_file(), "the fake {name} is a file");
        assert_eq!(meta.uid(), me, "the fake {name} is this user's");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o755,
            "the fake {name} runs"
        );
        let text = fs::read_to_string(&fake).unwrap();
        assert!(text.contains(says), "{} is the fake {name}", fake.display());
    }
}

/// git, as the daemon runs it (this `HOME`, the system configuration),
/// fetches from and pushes to `bare` for `origin`: nothing goes to GitHub.
fn assert_origin_is_the_bare_repository(repo: &Path, bare: &Path, user_home: &Path) {
    for args in [
        &["remote", "get-url", "origin"][..],
        &["remote", "get-url", "--push", "origin"][..],
    ] {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("HOME", user_home)
            .env("XDG_CONFIG_HOME", user_home)
            .env_remove("GIT_DIR")
            .env_remove("GIT_CONFIG_GLOBAL")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_CONFIG_COUNT")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            bare.to_str().unwrap(),
            "git {args:?} names the bare repository"
        );
    }
}

impl Fx {
    /// fake-claude answers the next polish passes with `answer`.
    fn polish_next_with(&self, answer: &Value) {
        let file = self.home.scratch().join("next-pass.json");
        fs::write(&file, answer.to_string()).unwrap();
        fs::write(self.claude_dir.join("structured"), file.to_str().unwrap()).unwrap();
    }

    /// The runs of the fake `gh` that start with `prefix`.
    fn gh_calls(&self, prefix: &str) -> Vec<String> {
        fs::read_to_string(self.gh_dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with(prefix))
            .map(str::to_owned)
            .collect()
    }

    /// The fake `gh` is what ran: its own log has a run that starts with
    /// `call`, and every run was in the daemon's home.
    fn assert_the_fake_ran(&self, call: &str) {
        assert!(
            !self.gh_calls(call).is_empty(),
            "the fake gh never ran `{call}`: {:?}",
            self.gh_calls("")
        );
        let home = fs::canonicalize(self.home.path()).unwrap();
        let cwd = fs::read_to_string(self.gh_dir.join("cwd.log")).unwrap();
        for line in cwd.lines() {
            assert_eq!(Path::new(line), home, "gh runs in the daemon's home");
        }
    }

    /// A screenshot (a 4×4 PNG and its annotated copy) with one box, and a
    /// JPEG image, written to the scratch directory.
    fn upload(&self) -> Upload {
        let dir = self.home.scratch().join("upload");
        fs::create_dir_all(&dir).unwrap();
        let files = [
            ("file-1", "shot.png", &PNG_4X4[..]),
            ("annotated-1", "shot.annotated.png", &PNG_4X4[..]),
            ("file-2", "photo.jpg", JPEG),
        ]
        .map(|(part, name, bytes)| {
            let path = dir.join(name);
            fs::write(&path, bytes).unwrap();
            FeedbackFile {
                part: part.into(),
                path,
            }
        });
        Upload {
            attachments: vec![
                FeedbackAttachment {
                    kind: AttachmentKind::Screenshot,
                    original_name: "shot.png".into(),
                    boxes: vec![FeedbackBox {
                        x: 0.25,
                        y: 0.25,
                        w: 0.5,
                        h: 0.25,
                        comment: COMMENT.into(),
                    }],
                },
                FeedbackAttachment {
                    kind: AttachmentKind::Image,
                    original_name: "photo.jpg".into(),
                    boxes: vec![],
                },
            ],
            files: files.to_vec(),
        }
    }
}

struct Upload {
    attachments: Vec<FeedbackAttachment>,
    files: Vec<FeedbackFile>,
}

impl Upload {
    /// The local file sent as `part`.
    fn file(&self, part: &str) -> &Path {
        &self.files.iter().find(|f| f.part == part).unwrap().path
    }

    /// A request of kind bug for `repo` with these attachments, under a new
    /// id.
    fn request(&self, repo: RepoId, send_as: SendAs, idea: Option<IdeaOptions>) -> FeedbackRequest {
        FeedbackRequest {
            submission_id: SubmissionId::new(),
            main_idea: MAIN_IDEA.into(),
            kind: FeedbackKind::Bug,
            repo_id: repo,
            send_as,
            idea,
            app_version: "0.1.0".into(),
            platform: "macos".into(),
            attachments: self.attachments.clone(),
            public_acknowledged: false,
        }
    }
}

fn api_error(e: ClientError) -> (u16, String, String) {
    match e {
        ClientError::Api {
            status,
            code,
            message,
        } => (status, code, message),
        other => panic!("expected an API error, got {other:?}"),
    }
}

/// The bare repository's branches, short names.
fn bare_branches(bare: &Path) -> Vec<String> {
    git(
        bare,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

/// The names of the files of `folder` at `commit`.
fn committed_names(repo: &Path, commit: &str, folder: &str) -> Vec<String> {
    git(
        repo,
        &["ls-tree", "--name-only", &format!("{commit}:{folder}")],
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

/// Whether the repository at `dir` has the object `id`.
fn has_object(dir: &Path, id: &str) -> bool {
    Command::new("git")
        .args(["cat-file", "-e", id])
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .status()
        .unwrap()
        .success()
}

/// Polls the intent until `pred` holds.
async fn wait_intent(client: &Client, id: IntentId, pred: impl Fn(&Intent) -> bool) -> Intent {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let intent = client.get_intent(id).await.unwrap();
        if pred(&intent) {
            return intent;
        }
        assert!(
            !matches!(intent.phase, Phase::Failed { .. }),
            "polish failed: {:?}",
            intent.phase
        );
        assert!(
            Instant::now() < deadline,
            "timed out; the intent is {intent:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Consumes `events` until the agent of `intent` is updated to Done: it is
/// published once its turn is over, so its branch holds what it committed.
async fn agent_done(events: &mut (impl Stream<Item = Event> + Unpin), intent: IntentId) -> Agent {
    within("the idea agent's turn", async {
        while let Some(event) = events.next().await {
            if let EventKind::AgentUpdated(agent) = event.kind {
                if agent.intent_id == Some(intent) {
                    assert_ne!(agent.state, AgentState::Dead, "the agent died");
                    if agent.state == AgentState::Done {
                        return agent;
                    }
                }
            }
        }
        panic!("the event stream ended first");
    })
    .await
}

/// Review Focus 3, at the end of every test: nothing secret, remote or
/// written by the user is in the daemon's log, `gh` got no `SMITHAGENTS_`
/// variable, nothing is left in the staging directory, and once the home is
/// dropped no daemon of it runs.
async fn nothing_leaks_and_nothing_is_left_behind(fx: Fx) {
    let Fx {
        home,
        client,
        gh_dir,
        ..
    } = fx;
    let token = read_lockfile(home.path())
        .unwrap()
        .expect("the daemon's lockfile")
        .token;
    let log = home.log();
    assert!(
        log.contains("smithagents_server::feedback"),
        "the daemon's log has the feedback lines:\n{log}"
    );
    for secret in ["LEAK", "smithagents-test-invalid", token.as_str(), COMMENT] {
        assert!(
            !log.contains(secret),
            "the daemon's log holds {secret:?}:\n{log}"
        );
    }
    let env = fs::read_to_string(gh_dir.join("env.log")).expect("the fake gh ran");
    let leaked: Vec<&str> = env
        .lines()
        .filter(|line| line.starts_with("SMITHAGENTS_") || line.contains(SECRET))
        .collect();
    assert!(leaked.is_empty(), "gh got {leaked:?}");
    let staging = home.path().join("tmp").join("feedback");
    let left: Vec<_> = fs::read_dir(&staging)
        .expect("the staging directory")
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "left in the staging directory: {left:?}");

    let home_path = home.path().to_str().unwrap().to_owned();
    drop(client);
    drop(home);
    let deadline = Instant::now() + PATIENCE;
    loop {
        let out = Command::new("pgrep")
            .args(["-fl", "smithagentsd"])
            .output()
            .unwrap();
        let listed = String::from_utf8_lossy(&out.stdout).into_owned();
        let ours: Vec<&str> = listed
            .lines()
            .filter(|line| line.contains(&home_path))
            .collect();
        if ours.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a daemon of this home still runs: {ours:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn feedback_becomes_an_intent_whose_agent_has_the_files() {
    let fx = start(&pass_with_a_question(), &[]).await;
    let client = &fx.client;

    let plugin = client.plugin("feedback").await.unwrap();
    assert!(plugin.builtin && plugin.enabled, "{plugin:?}");
    assert_eq!(plugin.host, "in-process");
    assert_eq!(plugin.state, PluginState::Ready, "{plugin:?}");
    assert!(plugin.resources.is_empty());

    let repo = client.add_repo(&fx.repo_dir).await.unwrap();
    let targets = client.feedback_targets().await.unwrap();
    let target = targets
        .repos
        .iter()
        .find(|t| t.repo_id == repo.id)
        .expect("the repository is a target");
    assert!(target.has_github_remote, "{targets:?}");
    assert!(
        targets.gh.ready && targets.agent_browser.ready,
        "{targets:?}"
    );
    fx.assert_the_fake_ran("auth status --hostname=github.com");

    let main_before = git(&fx.repo_dir, &["rev-parse", "main"]);
    let upload = fx.upload();
    let request = upload.request(repo.id, SendAs::Intent, None);
    let outcome = within(
        "the submission",
        client.submit_feedback(&request, &upload.files),
    )
    .await
    .unwrap();
    assert!(outcome.branch.starts_with("feedback/"), "{outcome:?}");
    assert_eq!(outcome.failed, None, "{outcome:?}");
    assert!(
        !outcome.pushed && outcome.issue_url.is_none(),
        "{outcome:?}"
    );
    let intent = outcome.intent.clone().expect("the outcome has the intent");
    assert_eq!(intent.state, IntentState::Draft);
    assert_eq!(intent.content.kind, IntentKind::Bug);
    assert_eq!(intent.base_branch.as_deref(), Some(outcome.branch.as_str()));

    // The branch holds the folder, each file as it was sent.
    let branch = format!("refs/heads/{}", outcome.branch);
    assert_eq!(git(&fx.repo_dir, &["rev-parse", &branch]), outcome.commit);
    assert_eq!(
        committed_names(&fx.repo_dir, &outcome.commit, &outcome.folder),
        [
            "image-1.jpg",
            "issue.md",
            "screenshot-1.annotated.png",
            "screenshot-1.png"
        ]
    );
    for (name, part) in [
        ("screenshot-1.png", "file-1"),
        ("screenshot-1.annotated.png", "annotated-1"),
        ("image-1.jpg", "file-2"),
    ] {
        let local = upload.file(part).to_str().unwrap();
        assert_eq!(
            git(
                &fx.repo_dir,
                &[
                    "rev-parse",
                    &format!("{}:{}/{name}", outcome.commit, outcome.folder)
                ]
            ),
            git(&fx.repo_dir, &["hash-object", "--no-filters", local]),
            "{name} is {part} as sent"
        );
    }
    let issue_md = git(
        &fx.repo_dir,
        &[
            "show",
            &format!("{}:{}/issue.md", outcome.commit, outcome.folder),
        ],
    );
    assert!(issue_md.contains(COMMENT), "{issue_md}");

    // The checkout is untouched, and nothing was pushed.
    assert_eq!(
        git(&fx.repo_dir, &["symbolic-ref", "--short", "HEAD"]),
        "main"
    );
    assert_eq!(git(&fx.repo_dir, &["status", "--porcelain"]), "");
    assert_eq!(git(&fx.repo_dir, &["rev-parse", "main"]), main_before);
    assert_eq!(bare_branches(&fx.bare), ["main"]);

    // Polish, review and send it as `full_loop_through_the_client` does.
    let at_questions = wait_intent(client, intent.id, |i| i.phase == Phase::Questions).await;
    fx.polish_next_with(&second_pass());
    let question = &at_questions.content.questions[0];
    client
        .answer_question(
            intent.id,
            question.id,
            AnswerInput::Option(question.options[0].id.clone()),
        )
        .await
        .unwrap();
    let review = wait_intent(client, intent.id, |i| i.phase == Phase::Review).await;
    let c = &review.content;
    for item in c.preconditions.iter().chain(&c.criteria).chain(&c.stories) {
        client.accept_item(intent.id, item.id).await.unwrap();
    }
    wait_intent(client, intent.id, |i| i.state == IntentState::Complete).await;
    client.send_intent(intent.id).await.unwrap();
    let sent = wait_intent(client, intent.id, |i| {
        i.state == IntentState::Sent && i.agent_id.is_some()
    })
    .await;
    let agent_id = sent.agent_id.unwrap();
    let agent = client
        .list_agents(false)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.id == agent_id)
        .expect("the intent's agent is listed");
    let in_checkout = agent.worktree_path.join(&outcome.folder).join("issue.md");
    assert!(
        in_checkout.is_file(),
        "the agent's checkout has {}",
        in_checkout.display()
    );
    git(
        &fx.repo_dir,
        &[
            "merge-base",
            "--is-ancestor",
            &outcome.commit,
            &agent.branch,
        ],
    );
    assert_eq!(
        bare_branches(&fx.bare),
        ["main"],
        "an intent pushes nothing"
    );

    nothing_leaks_and_nothing_is_left_behind(fx).await;
}

#[tokio::test]
async fn feedback_becomes_a_github_issue_and_a_failed_push_is_retried() {
    let fx = start(&passes_that_add_nothing(), &[]).await;
    let client = &fx.client;
    let repo = client.add_repo(&fx.repo_dir).await.unwrap();
    // The remote refuses every push, saying something that must not reach
    // the daemon's log.
    let hook = fx.bare.join("hooks").join("pre-receive");
    common::shell_script(&hook, "echo 'LEAK: no pushes today' >&2\nexit 1");

    let upload = fx.upload();
    let request = upload.request(repo.id, SendAs::GithubIssue, None);
    let first = within(
        "the submission",
        client.submit_feedback(&request, &upload.files),
    )
    .await
    .unwrap();
    assert!(first.branch.starts_with("feedback/"), "{first:?}");
    assert!(!first.pushed && first.issue_url.is_none(), "{first:?}");
    let failed = first.failed.clone().expect("the push failed");
    assert_eq!(failed.step, FeedbackStep::Push);
    assert_eq!(failed.code, "push_rejected");
    assert_eq!(
        failed.message,
        "git could not push to origin: the remote rejected the branch"
    );
    assert_eq!(bare_branches(&fx.bare), ["main"]);
    assert!(fx.gh_calls("issue create").is_empty());

    fs::remove_file(&hook).unwrap();
    let retried = within("the retry", client.retry_feedback(request.submission_id))
        .await
        .unwrap();
    assert_eq!(retried.failed, None, "{retried:?}");
    assert!(retried.pushed);
    assert!(
        retried
            .issue_url
            .as_deref()
            .is_some_and(|url| url.ends_with(ISSUE_URL_END)),
        "{retried:?}"
    );
    assert_eq!(
        (&retried.branch, &retried.commit),
        (&first.branch, &first.commit)
    );
    let pushed = format!("refs/heads/{}", first.branch);
    assert_eq!(git(&fx.bare, &["rev-parse", &pushed]), first.commit);
    assert_eq!(fx.gh_calls("issue create").len(), 1);
    fx.assert_the_fake_ran("issue create");
    let body = fs::read_to_string(fx.gh_dir.join("issue-body.md")).unwrap();
    let link = format!(
        "{}/blob/{}/{}/screenshot-1.annotated.png?raw=true",
        GITHUB_REMOTE.trim_end_matches(".git"),
        first.commit,
        first.folder
    );
    assert!(body.contains(&link), "the issue links {link}:\n{body}");

    // The same request again answers as the retry did, and creates nothing.
    let again = within(
        "the same submission",
        client.submit_feedback(&request, &upload.files),
    )
    .await
    .unwrap();
    assert_eq!(again, retried);
    assert_eq!(fx.gh_calls("issue create").len(), 1, "no second issue");

    nothing_leaks_and_nothing_is_left_behind(fx).await;
}

#[tokio::test]
async fn an_idea_goes_from_feedback_to_a_draft_pull_request() {
    // A public repository: the pull request needs the acknowledgement.
    let fx = start(&passes_that_add_nothing(), &[("visibility", "PUBLIC")]).await;
    let client = &fx.client;
    let repo = client.add_repo(&fx.repo_dir).await.unwrap();
    let mut events = client.events(None).await.unwrap();

    let upload = fx.upload();
    let idea = IdeaOptions {
        explainer: true,
        diagrams: false,
        demo_video: true,
    };
    let request = upload.request(repo.id, SendAs::Intent, Some(idea));
    let outcome = within(
        "the submission",
        client.submit_feedback(&request, &upload.files),
    )
    .await
    .unwrap();
    assert_eq!(outcome.failed, None, "{outcome:?}");
    let slug = outcome
        .branch
        .strip_prefix("idea/")
        .unwrap_or_else(|| panic!("{} is an idea branch", outcome.branch));
    assert!(outcome.folder.ends_with(&format!("-{slug}")), "{outcome:?}");
    let intent = outcome.intent.clone().expect("the outcome has the intent");
    let fixed = |i: &Intent| -> Vec<(String, bool, ItemStatus)> {
        i.content
            .criteria
            .iter()
            .take(3)
            .map(|c| (c.text.clone(), c.fixed, c.status))
            .collect()
    };
    let expected: Vec<(String, bool, ItemStatus)> =
        [CRITERION_EXPLAINER, CRITERION_DEMO, CRITERION_SCOPE]
            .map(|text| (text.to_owned(), true, ItemStatus::Accepted))
            .to_vec();
    assert_eq!(fixed(&intent), expected);

    // Polish adds nothing, so it is Complete; its agent prepares the package.
    let complete = wait_intent(client, intent.id, |i| i.state == IntentState::Complete).await;
    assert_eq!(fixed(&complete), expected, "polish kept the fixed criteria");
    client.send_intent(intent.id).await.unwrap();
    let agent = agent_done(&mut events, intent.id).await;
    let head = git(
        &fx.repo_dir,
        &["rev-parse", &format!("refs/heads/{}", agent.branch)],
    );

    // Without the acknowledgement nothing is pushed or opened.
    let refused = within(
        "the refused pull request",
        client.open_pull_request(intent.id, false),
    )
    .await
    .unwrap_err();
    let (status, code, message) = api_error(refused);
    assert_eq!((status, code.as_str()), (409, "public_not_acknowledged"));
    assert_eq!(message, PUBLIC);
    assert_eq!(bare_branches(&fx.bare), ["main"]);
    assert!(fx.gh_calls("pr ").is_empty(), "{:?}", fx.gh_calls("pr "));
    fx.assert_the_fake_ran("repo view");

    let opened = within(
        "the pull request",
        client.open_pull_request(intent.id, true),
    )
    .await
    .unwrap();
    let package = opened
        .origin
        .as_ref()
        .and_then(|origin| origin.idea.as_ref())
        .expect("an idea package");
    assert!(
        package
            .pr_url
            .as_deref()
            .is_some_and(|url| url.ends_with(PR_URL_END)),
        "{package:?}"
    );
    let idea_branch = format!("refs/heads/{}", outcome.branch);
    assert_eq!(git(&fx.bare, &["rev-parse", &idea_branch]), head);
    let creates = fx.gh_calls("pr create");
    assert_eq!(creates.len(), 1, "{creates:?}");
    assert!(
        creates[0].contains("--draft")
            && creates[0].contains(&format!("--head={}", outcome.branch)),
        "{creates:?}"
    );

    let implementation = within(
        "the implementation intent",
        client.create_implementation_intent(intent.id, true),
    )
    .await
    .unwrap();
    assert_ne!(implementation.id, intent.id);
    assert_eq!(
        implementation.base_branch.as_deref(),
        Some(agent.branch.as_str())
    );
    assert!(
        implementation
            .content
            .depends_on
            .iter()
            .any(|d| d.fixed && d.intent_ref == Some(intent.id)),
        "{:?}",
        implementation.content.depends_on
    );
    let again = client
        .create_implementation_intent(intent.id, true)
        .await
        .unwrap();
    assert_eq!(again.id, implementation.id, "made once");
    assert_eq!(fx.gh_calls("pr create").len(), 1);

    nothing_leaks_and_nothing_is_left_behind(fx).await;
}

#[tokio::test]
async fn unpushed_work_on_the_default_branch_is_never_published() {
    let fx = start(&passes_that_add_nothing(), &[]).await;
    let client = &fx.client;
    let repo = client.add_repo(&fx.repo_dir).await.unwrap();
    // Work of the user's that origin has not got.
    fs::write(fx.repo_dir.join("plan.txt"), "not for anyone yet\n").unwrap();
    git(&fx.repo_dir, &["add", "plan.txt"]);
    git(&fx.repo_dir, &["commit", "-q", "-m", "local work"]);
    let unpushed = git(&fx.repo_dir, &["rev-parse", "main"]);

    let upload = fx.upload();
    let request = upload.request(repo.id, SendAs::GithubIssue, None);
    let refused = within(
        "the submission",
        client.submit_feedback(&request, &upload.files),
    )
    .await
    .unwrap_err();
    let (status, code, message) = api_error(refused);
    assert_eq!((status, code.as_str()), (409, "unpushed_commits"));
    assert_eq!(message, UNPUSHED);

    // Nothing was committed or recorded.
    assert_eq!(
        git(
            &fx.repo_dir,
            &["for-each-ref", "refs/heads/feedback", "refs/heads/idea"]
        ),
        ""
    );
    let worktrees = git(&fx.repo_dir, &["worktree", "list", "--porcelain"]);
    assert_eq!(worktrees.matches("worktree ").count(), 1, "{worktrees}");
    assert_eq!(git(&fx.repo_dir, &["rev-parse", "main"]), unpushed);
    let err = client
        .retry_feedback(request.submission_id)
        .await
        .unwrap_err();
    assert_eq!(api_error(err).1, "submission_not_found");

    // The bare repository never received the user's work.
    assert_eq!(bare_branches(&fx.bare), ["main"]);
    assert_ne!(git(&fx.bare, &["rev-parse", "main"]), unpushed);
    assert!(!has_object(&fx.bare, &unpushed));
    assert!(fx.gh_calls("issue ").is_empty(), "{:?}", fx.gh_calls(""));
    fx.assert_the_fake_ran("auth status --hostname=github.com");

    nothing_leaks_and_nothing_is_left_behind(fx).await;
}

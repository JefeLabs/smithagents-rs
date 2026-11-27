//! The GitHub issue path of a feedback submission: readiness, the push, `gh
//! issue create`, labels, partial failures and Retry, and what the targets
//! say about GitHub.
//!
//! `origin` is `https://github.com/smithagents-test-invalid/widgets.git`;
//! `url.<bare>.insteadOf` sends git to a bare repository in the temp
//! directory, so a push goes there and nowhere else. `gh` is the fake of
//! `tests/bin`, found only on the fixture's own finder path: no real `gh` is
//! ever run. Polish never runs.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use common::feedback::{multipart, png, request_json, FakeTools};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use smithagents_core::agent::Repo;
use smithagents_core::feedback::{AttachmentKind, FeedbackRequest, SubmissionId};
use smithagents_core::ids::IntentId;
use smithagents_plugin_host::StaticSecrets;
use smithagents_server::api::{router, AppState};
use smithagents_server::events::EventBus;
use smithagents_server::feedback::issue::{file_names, render_issue, FileKind, IssueInput};
use smithagents_server::feedback::repo::{place_and_commit, FileSource, FolderFile};
use smithagents_server::feedback::FeedbackService;
use smithagents_server::harness::fake::FakeHarness;
use smithagents_server::intents::{IntentError, IntentService, PolishRunner};
use smithagents_server::paths::Paths;
use smithagents_server::plugins::{embedded_plugins, ManagerConfig, PluginManager};
use smithagents_server::queue::{Scheduler, SchedulerHandle};
use smithagents_server::service::AgentService;
use smithagents_server::store::{Store, Submission};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "test-token-123";
const GITHUB_URL: &str = "https://github.com/smithagents-test-invalid/widgets.git";
const ISSUE_URL: &str = "https://github.com/smithagents-test-invalid/widgets/issues/7";
const REPO_ARG: &str = "--repo=github.com/smithagents-test-invalid/widgets";
/// The sign-in check, for github.com only.
const AUTH: &str = "auth status --hostname=github.com";
const BRANCH: &str = "feedback/2026-11-26-fix-the-zoom";
/// The refusals of a GitHub issue to a repository that is, or may be,
/// public, sent without the user's acknowledgement.
const PUBLIC: &str =
    "This repository is public. The screenshots will be visible to anyone. Confirm that before sending.";
const MAYBE_PUBLIC: &str = "This repository may be public. The screenshots will be visible to anyone. Confirm that before sending.";
/// The refusals while the default branch is not known to be on origin.
const UNPUSHED: &str = "The default branch has commits that are not on origin. Push them first, so they are not published with this feedback.";
const NEVER_FETCHED: &str = "This repository has never been fetched from origin, so it is not known what is already published. Run git fetch first.";
const FOLDER: &str = "docs/issues/2026-11-26-fix-the-zoom";
/// A plugin secret in the test process's environment, which `gh` must never
/// see.
const SECRET: (&str, &str) = ("SMITHAGENTS_SECRET_FEEDBACK__X", "LEAK-secret");
/// Waits in these tests end long before this unless something is wrong.
const PATIENCE: Duration = Duration::from_secs(20);

/// Once per binary, before any git or fake runs: `HOME` and
/// `XDG_CONFIG_HOME` name an empty directory, so no configuration of the
/// machine's user changes what git does, and the process holds [`SECRET`]
/// and [`SHELL_COLOUR`], as a daemon started from a shell may.
fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join("feedback_github-empty-home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", &home);
        std::env::set_var(SECRET.0, SECRET.1);
        for (name, value) in SHELL_COLOUR {
            std::env::set_var(name, value);
        }
    });
}

/// What a user's shell may hand the daemon that would make `gh` write
/// coloured or indented JSON: `gh` must get them overridden.
const SHELL_COLOUR: [(&str, &str); 2] = [("CLICOLOR_FORCE", "1"), ("GH_FORCE_TTY", "1")];

/// The service clock starts at 2026-11-26T14:15:00Z.
fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 26, 14, 15, 0).unwrap()
}

/// A polish runner that never polishes.
struct NoPolish;

impl PolishRunner for NoPolish {
    fn start(&self, _: IntentId, _: u8) {}
}

struct Opts {
    gh: bool,
    gh_timeout: Option<Duration>,
    /// Replaces the 5 s limit of `gh auth status` and `gh repo view`.
    check_timeout: Option<Duration>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            gh: true,
            gh_timeout: None,
            check_timeout: None,
        }
    }
}

struct Fx {
    tmp: TempDir,
    state: AppState,
    paths: Paths,
    fake: FakeTools,
    /// Minutes the service clock is past [`start`].
    minutes: Arc<AtomicI64>,
    _sched: SchedulerHandle,
}

async fn fixture() -> Fx {
    fixture_with(Opts::default()).await
}

async fn fixture_with(opts: Opts) -> Fx {
    setup();
    let tmp = TempDir::new().unwrap();
    let store = Store::open_in_memory().await.unwrap();
    let bus = EventBus::new(1024);
    let paths = Paths {
        home: tmp.path().join("home"),
    };
    paths.ensure_dirs().unwrap();
    let harness = FakeHarness::scripted(vec![]);
    let service = AgentService::new(
        store.clone(),
        bus.clone(),
        Arc::new(harness.clone()),
        paths.clone(),
        Duration::from_secs(3600),
    );
    let intents = IntentService::new(
        store.clone(),
        bus.clone(),
        Arc::new(harness),
        service.clone(),
        paths.clone(),
    )
    .with_polish_runner(Arc::new(NoPolish));
    let sched = Scheduler::start(intents.clone(), service.clone(), store.clone(), bus.clone());
    let fake = FakeTools::new(&tmp.path().join("fakes"), opts.gh, false);
    let plugins = PluginManager::start(
        store.clone(),
        bus.clone(),
        Arc::new(StaticSecrets(Default::default())),
        embedded_plugins(),
        ManagerConfig {
            tools: fake.finder(),
            ..ManagerConfig::default()
        },
    )
    .await
    .unwrap();
    let minutes = Arc::new(AtomicI64::new(0));
    let clock = {
        let minutes = Arc::clone(&minutes);
        Arc::new(move || start() + chrono::Duration::minutes(minutes.load(Ordering::SeqCst)))
    };
    let mut feedback = FeedbackService::new(
        store.clone(),
        intents.clone(),
        service.clone(),
        plugins.clone(),
        paths.clone(),
    )
    .with_clock(clock);
    if let Some(timeout) = opts.gh_timeout {
        feedback = feedback.with_gh_timeout_for_test(timeout);
    }
    if let Some(timeout) = opts.check_timeout {
        feedback = feedback.with_check_timeout_for_test(timeout);
    }
    let state = AppState {
        service,
        intents,
        feedback,
        bus,
        store,
        plugins,
        token: Arc::from(TOKEN),
        instance_id: Arc::from("0123456789abcdef"),
        ws_clients: Arc::new(AtomicUsize::new(0)),
        started_at: Instant::now(),
    };
    Fx {
        tmp,
        state,
        paths,
        fake,
        minutes,
        _sched: sched,
    }
}

impl Fx {
    /// The answer's status and its body as text.
    async fn send(&self, req: Request<Body>) -> (StatusCode, String) {
        let resp = router(self.state.clone()).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn call_text(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, String) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(match body {
                Some(v) => Body::from(v.to_string()),
                None => Body::empty(),
            })
            .unwrap();
        self.send(req).await
    }

    async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let (status, text) = self.call_text(method, uri, body).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// `POST /v1/feedback` with `request` and then `files`.
    async fn submit_text(&self, request: &Value, files: &[(&str, &[u8])]) -> (StatusCode, String) {
        let json = request.to_string();
        let mut parts: Vec<(&str, &[u8])> = vec![("request", json.as_bytes())];
        parts.extend_from_slice(files);
        let (content_type, body) = multipart(&parts);
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/feedback")
            .header(header::CONTENT_TYPE, content_type)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::from(body))
            .unwrap();
        self.send(req).await
    }

    async fn submit(&self, request: &Value, files: &[(&str, &[u8])]) -> (StatusCode, Value) {
        let (status, text) = self.submit_text(request, files).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// Submits `request` with the files of its one screenshot.
    async fn submit_shot(&self, request: &Value) -> (StatusCode, Value) {
        let (status, text) = self.submit_shot_text(request).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn submit_shot_text(&self, request: &Value) -> (StatusCode, String) {
        let (shot, annotated) = (png(300), png(400));
        self.submit_text(request, &[("file-1", &shot), ("annotated-1", &annotated)])
            .await
    }

    async fn retry_text(&self, id: &Value) -> (StatusCode, String) {
        let id = id.as_str().unwrap();
        self.call_text(Method::POST, &format!("/v1/feedback/{id}/retry"), None)
            .await
    }

    /// `POST /v1/feedback/{id}/retry`.
    async fn retry(&self, id: &Value) -> (StatusCode, Value) {
        let (status, text) = self.retry_text(id).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// A repository on `main` with one commit and its own identity, added
    /// to the daemon; no remote.
    async fn add_repo(&self, name: &str) -> Repo {
        let dir = self.tmp.path().join(name);
        common::init_repo(&dir);
        common::git(&dir, &["config", "user.name", "Test User"]);
        common::git(&dir, &["config", "user.email", "test@example.invalid"]);
        let (status, body) = self
            .call(Method::POST, "/v1/repos", Some(json!({ "path": dir })))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        serde_json::from_value(body).unwrap()
    }

    /// [`Fx::add_repo`] whose `origin` is `url`, with `main` known to be
    /// on it: `refs/remotes/origin/main` at `main`, as a fetch would leave
    /// it (nothing is fetched or pushed).
    async fn add_repo_with_origin(&self, name: &str, url: &str) -> Repo {
        let repo = self.add_repo(name).await;
        common::git(&repo.path, &["remote", "add", "origin", url]);
        common::git(
            &repo.path,
            &["update-ref", "refs/remotes/origin/main", "main"],
        );
        repo
    }

    /// [`Fx::add_repo`] whose `origin` is [`GITHUB_URL`], which git sends to
    /// the returned bare repository (it logs every update of its refs).
    async fn github_repo(&self, name: &str) -> (Repo, PathBuf) {
        self.github_repo_at(name, GITHUB_URL).await
    }

    /// [`Fx::github_repo`] whose `origin` is the GitHub repository `url`.
    async fn github_repo_at(&self, name: &str, url: &str) -> (Repo, PathBuf) {
        let repo = self.add_repo_with_origin(name, url).await;
        let bare = self.tmp.path().join(format!("{name}-origin.git"));
        common::git(
            self.tmp.path(),
            &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
        );
        common::git(&bare, &["config", "core.logAllRefUpdates", "always"]);
        redirect_url(&repo, &bare, url);
        (repo, bare)
    }

    async fn row(&self, id: &Value) -> Option<Submission> {
        let id: SubmissionId = serde_json::from_value(id.clone()).unwrap();
        self.state.store.get_submission(id).await.unwrap()
    }

    fn feedback_tmp_is_empty(&self) -> bool {
        match std::fs::read_dir(self.paths.feedback_tmp_dir()) {
            Ok(mut entries) => entries.next().is_none(),
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        }
    }

    /// The calls of the fake `gh` that start with `prefix`.
    fn calls_starting(&self, prefix: &str) -> Vec<String> {
        self.fake
            .calls()
            .into_iter()
            .filter(|c| c.starts_with(prefix))
            .collect()
    }

    fn clear(&self, file: &str) {
        let _ = std::fs::remove_file(self.fake.dir.join(file));
    }

    async fn intent_count(&self) -> usize {
        self.state
            .intents
            .list(smithagents_core::intent::IntentFilter {
                state: None,
                repo_id: None,
            })
            .await
            .unwrap()
            .len()
    }

    /// Submits and expects a refusal with `status`, `code` and `message`:
    /// no branch, no row, nothing pushed, nothing left in the temp directory.
    async fn refused(&self, repo: &Repo, bare: Option<&Path>, expected: (StatusCode, &str, &str)) {
        let req = issue_request(repo);
        let (status, body) = self.submit_shot(&req).await;
        assert_eq!(
            (status, body["error"].as_str(), body["message"].as_str()),
            (expected.0, Some(expected.1), Some(expected.2)),
            "{body}"
        );
        assert_eq!(
            git_out(&repo.path, &["branch", "--list", "feedback/*"]),
            "",
            "{}",
            expected.2
        );
        assert_eq!(self.row(&req["submission_id"]).await, None);
        if let Some(bare) = bare {
            assert_eq!(git_out(bare, &["for-each-ref"]), "", "{}", expected.2);
        }
        assert!(self.feedback_tmp_is_empty(), "{}", expected.2);
        assert!(
            self.calls_starting("issue").is_empty(),
            "{:?}",
            self.fake.calls()
        );
    }
}

/// Points `repo`'s `origin` at `target` for git (the configured URL stays).
fn redirect(repo: &Repo, target: &Path) {
    redirect_url(repo, target, GITHUB_URL);
}

/// Sends git from `url` to `target` in `repo`.
fn redirect_url(repo: &Repo, target: &Path, url: &str) {
    common::git(
        &repo.path,
        &[
            "config",
            &format!("url.{}.insteadOf", target.display()),
            url,
        ],
    );
}

/// A `github_issue` request for `repo`: "Fix the zoom", kind `new_feature`,
/// one screenshot with one comment, sent once the user confirmed that the
/// screenshots may be public (`public_acknowledged`).
fn issue_request(repo: &Repo) -> Value {
    let mut req = request_json(repo.id, "github_issue");
    req["kind"] = json!("new_feature");
    req["attachments"] = json!([screenshot("Too big")]);
    req["public_acknowledged"] = json!(true);
    req
}

/// [`issue_request`] as an older app sends it: no `public_acknowledged`.
fn unacknowledged_issue_request(repo: &Repo) -> Value {
    let mut req = issue_request(repo);
    req.as_object_mut().unwrap().remove("public_acknowledged");
    req
}

fn screenshot(comment: &str) -> Value {
    json!({
        "kind": "screenshot",
        "original_name": "Screenshot 1.png",
        "boxes": [{"x": 0.1, "y": 0.2, "w": 0.3, "h": 0.4, "comment": comment}],
    })
}

fn git_cmd(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("LC_ALL", "C")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap()
}

/// git's standard output, trimmed; panics when git fails.
fn git_out(cwd: &Path, args: &[&str]) -> String {
    let out = git_cmd(cwd, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commits_above_main(dir: &Path, branch: &str) -> String {
    git_out(dir, &["rev-list", "--count", &format!("main..{branch}")])
}

/// The number of entries in the bare repository's log of `branch`.
fn reflog_size(bare: &Path, branch: &str) -> usize {
    git_out(
        bare,
        &[
            "reflog",
            "show",
            "--format=%H",
            &format!("refs/heads/{branch}"),
        ],
    )
    .lines()
    .count()
}

/// The `<!-- smithagents-feedback:<id> -->` marker of a request.
fn marker_of(req: &Value) -> String {
    format!(
        "<!-- smithagents-feedback:{} -->",
        req["submission_id"].as_str().unwrap()
    )
}

/// Polls `done` every few milliseconds until it holds, for at most PATIENCE.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Whether the process `pid` runs (a zombie does not).
fn process_alive(pid: &str) -> bool {
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// Everything this test binary logs at info and above, from every test.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

fn logs() -> &'static Logs {
    static LOGS: OnceLock<Logs> = OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Logs::default();
        let sink = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || sink.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("the only global subscriber");
        logs
    })
}

// ------------------------------------------------------------ the happy path

#[tokio::test]
async fn an_issue_submission_pushes_and_creates_the_issue() {
    let fx = fixture().await;
    let (repo, bare) = fx.github_repo("r1").await;
    let req = issue_request(&repo);

    let (status, out) = fx.submit_shot(&req).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    let commit = git_out(&repo.path, &["rev-parse", &format!("refs/heads/{BRANCH}")]);
    assert_eq!(out["submission_id"], req["submission_id"]);
    assert_eq!(out["branch"], BRANCH);
    assert_eq!(out["folder"], FOLDER);
    assert_eq!(out["commit"], commit);
    assert_eq!(out["pushed"], true);
    assert_eq!(out["issue_url"], ISSUE_URL);
    assert_eq!(out["intent"], Value::Null);
    assert_eq!(out["failed"], Value::Null);
    // Pushed under the same name, the commit as made.
    assert_eq!(git_out(&bare, &["rev-parse", BRANCH]), commit);

    let calls = fx.fake.calls();
    assert_eq!(calls.len(), 4, "{calls:?}");
    assert_eq!(calls[0], AUTH);
    assert!(calls[1].starts_with("issue list"), "{calls:?}");
    assert!(
        calls[2].starts_with("label list --search=enhancement"),
        "{calls:?}"
    );
    assert!(
        calls[3].starts_with("issue create --title=Fix the zoom --body-file="),
        "{calls:?}"
    );
    assert!(calls[3].ends_with("--label=enhancement"), "{calls:?}");
    // The search looks for the submission's id among the issues the
    // signed-in user opened; every call on the repository names it, joined
    // to its option.
    let id = req["submission_id"].as_str().unwrap();
    assert_eq!(
        calls[1],
        format!(
            r#"issue list {REPO_ARG} --author=@me --state=all --search="{id}" in:body --json=url --jq=.[0].url // """#
        )
    );
    for call in &calls[1..] {
        assert!(call.contains(REPO_ARG), "{call}");
    }

    let body = fx.fake.read("issue-body.md").unwrap();
    assert!(!body.starts_with("---"), "{body}");
    assert!(body.starts_with("# Fix the zoom\n"), "{body}");
    let permalink = format!(
        "https://github.com/smithagents-test-invalid/widgets/blob/{commit}/{FOLDER}/screenshot-1.annotated.png?raw=true"
    );
    assert!(body.contains(&permalink), "{body}");
    assert!(body.ends_with(&marker_of(&req)), "{body}");

    // Nothing else was made, and the record has it all.
    assert_eq!(fx.intent_count().await, 0);
    let row = fx.row(&req["submission_id"]).await.unwrap();
    assert!(row.pushed);
    assert_eq!(row.issue_url.as_deref(), Some(ISSUE_URL));
    // The body file is gone and the user's checkout is untouched.
    assert!(fx.feedback_tmp_is_empty());
    assert_eq!(git_out(&repo.path, &["status", "--porcelain"]), "");
    assert_eq!(git_out(&repo.path, &["branch", "--show-current"]), "main");
}

#[tokio::test]
async fn gh_runs_from_the_daemon_home_with_its_own_environment() {
    let fx = fixture().await;
    let (repo, _bare) = fx.github_repo("r1").await;

    let (status, out) = fx.submit_shot(&issue_request(&repo)).await;
    assert_eq!(status, StatusCode::OK, "{out}");

    // Never from inside the user's repository.
    let home = std::fs::canonicalize(&fx.paths.home).unwrap();
    let cwds = fx.fake.read("cwd.log").unwrap();
    assert_eq!(cwds.lines().count(), 4, "{cwds}");
    for cwd in cwds.lines() {
        assert_eq!(Path::new(cwd), home, "{cwds}");
    }
    let env = fx.fake.read("env.log").unwrap();
    for var in [
        "GH_PROMPT_DISABLED=1",
        "GH_NO_UPDATE_NOTIFIER=1",
        "NO_COLOR=1",
        // What the shell handed the daemon is overridden: colour off,
        // no terminal-style output (`gh` reads an empty value as unset).
        "CLICOLOR_FORCE=0",
        "GH_FORCE_TTY=",
    ] {
        assert_eq!(
            env.lines().filter(|l| *l == var).count(),
            4,
            "{var} in every run"
        );
    }
    for (name, value) in SHELL_COLOUR {
        let inherited = format!("{name}={value}");
        assert!(!env.lines().any(|l| l == inherited), "{inherited}: {env}");
    }
    assert_eq!(std::env::var(SECRET.0).as_deref(), Ok(SECRET.1));
    assert!(!env.lines().any(|l| l.starts_with("SMITHAGENTS_")), "{env}");
}

#[tokio::test]
async fn the_same_issue_submission_twice_creates_one_issue() {
    let fx = fixture().await;
    let (repo, _bare) = fx.github_repo("r1").await;
    let req = issue_request(&repo);
    let (shot, annotated) = (png(300), png(400));
    let files: [(&str, &[u8]); 2] = [("file-1", &shot), ("annotated-1", &annotated)];

    let (a, b) = tokio::join!(fx.submit(&req, &files), fx.submit(&req, &files));
    let (c_status, c) = fx.submit(&req, &files).await;

    assert_eq!(a.0, StatusCode::OK, "{}", a.1);
    assert_eq!(a, b);
    assert_eq!((c_status, &c), (StatusCode::OK, &a.1));
    assert_eq!(a.1["issue_url"], ISSUE_URL);
    assert_eq!(fx.calls_starting("issue create").len(), 1);
    assert_eq!(
        git_out(&repo.path, &["branch", "--list", "feedback/*"]),
        BRANCH
    );
    assert_eq!(commits_above_main(&repo.path, BRANCH), "1");
}

// -------------------------------------------------------- labels and titles

#[tokio::test]
async fn labels_follow_the_kind_and_only_when_they_exist() {
    let fx = fixture().await;
    let (repo, _bare) = fx.github_repo("r1").await;
    let last_create = |fx: &Fx| fx.calls_starting("issue create").pop().unwrap();

    let mut bug = issue_request(&repo);
    bug["kind"] = json!("bug");
    bug["main_idea"] = json!("A bug");
    let (status, out) = fx.submit_shot(&bug).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert!(
        last_create(&fx).ends_with("--label=bug"),
        "{:?}",
        fx.fake.calls()
    );
    let check = fx.calls_starting("label list").pop().unwrap();
    assert!(
        check.starts_with(r#"label list --search=bug --json=name --jq=any(.[]; .name == "bug")"#),
        "{check}"
    );

    let mut enhancement = issue_request(&repo);
    enhancement["kind"] = json!("enhancement");
    enhancement["main_idea"] = json!("An enhancement");
    let (status, out) = fx.submit_shot(&enhancement).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert!(
        last_create(&fx).ends_with("--label=enhancement"),
        "{:?}",
        fx.fake.calls()
    );

    // The repository has no such label: the issue is made without one.
    fx.fake.set("label", "false");
    let mut unlabelled = issue_request(&repo);
    unlabelled["main_idea"] = json!("No label here");
    let (status, out) = fx.submit_shot(&unlabelled).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["issue_url"], ISSUE_URL);
    assert!(
        !last_create(&fx).contains("--label"),
        "{:?}",
        fx.fake.calls()
    );

    // A label check that fails is no label either.
    fx.clear("label");
    fx.fake.set("fail", "label list");
    let mut failing = issue_request(&repo);
    failing["main_idea"] = json!("The check fails");
    let (status, out) = fx.submit_shot(&failing).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["failed"], Value::Null, "{out}");
    assert!(
        !last_create(&fx).contains("--label"),
        "{:?}",
        fx.fake.calls()
    );
}

#[tokio::test]
async fn a_title_that_looks_like_a_flag_stays_a_title() {
    let fx = fixture().await;
    let (repo, _bare) = fx.github_repo("r1").await;
    let mut req = issue_request(&repo);
    req["main_idea"] = json!("--web");

    let (status, out) = fx.submit_shot(&req).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["issue_url"], ISSUE_URL);
    let create = fx.calls_starting("issue create").pop().unwrap();
    assert!(
        create.starts_with("issue create --title=--web --body-file="),
        "{create}"
    );
}

/// What only GitHub gets: `@name` with a zero-width space after the `@`.
const ZWSP: char = '\u{200b}';

#[tokio::test]
async fn mentions_are_neutralised_only_in_what_github_gets() {
    let fx = fixture().await;
    let (repo, _bare) = fx.github_repo("r1").await;
    let mut req = issue_request(&repo);
    req["main_idea"] = json!(
        "@octocat the zoom of https://a.b/@x\n\n@hubot, see (@reviewer) and @org/team.\nMail a@b.com or pkg/@scope.\nTypes: https://www.npmjs.com/package/@types/node, then @after."
    );
    req["attachments"] = json!([
        screenshot("cc @someone and me@example.com"),
        {"kind": "image", "original_name": "photo@2x.png"},
    ]);
    let (shot, annotated, image) = (png(300), png(400), png(200));
    let files: [(&str, &[u8]); 3] = [
        ("file-1", &shot),
        ("annotated-1", &annotated),
        ("file-2", &image),
    ];

    let (status, out) = fx.submit(&req, &files).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    // The committed issue.md is the user's text as written.
    let request: FeedbackRequest = serde_json::from_value(req.clone()).unwrap();
    let names = file_names(&[
        (AttachmentKind::Screenshot, FileKind::Png),
        (AttachmentKind::Image, FileKind::Png),
    ]);
    let raw = render_issue(&IssueInput {
        title: "@octocat the zoom of https://a.b/@x",
        main_idea: &request.main_idea,
        kind: request.kind,
        created: start(),
        app_version: "0.1.0",
        platform: "macos",
        attachments: &request.attachments,
        names: &names,
    });
    let branch = out["branch"].as_str().unwrap();
    let folder = out["folder"].as_str().unwrap();
    let committed = git_out(
        &repo.path,
        &["show", &format!("{branch}:{folder}/issue.md")],
    );
    assert_eq!(committed, raw.trim_end());
    assert!(!committed.contains(ZWSP), "{committed}");

    // The title and the whole body GitHub gets mention no one. W2-5: a web
    // address in the body (rendered as Markdown, so it is a link) keeps its
    // `@`; the title is not rendered, so it is strict throughout.
    let create = fx.calls_starting("issue create").pop().unwrap();
    assert!(
        create.starts_with(&format!(
            "issue create --title=@{ZWSP}octocat the zoom of https://a.b/@{ZWSP}x --body-file="
        )),
        "{create}"
    );
    let body = fx.fake.read("issue-body.md").unwrap();
    for neutral in [
        format!("# @{ZWSP}octocat the zoom of https://a.b/@x\n"),
        format!("Types: https://www.npmjs.com/package/@types/node, then @{ZWSP}after."),
        format!("@{ZWSP}hubot, see (@{ZWSP}reviewer) and @{ZWSP}org/team."),
        format!("cc @{ZWSP}someone and me@example.com"),
        // D-M3: after anything but a letter, digit or `_`, as GitHub may read it.
        format!("Mail a@b.com or pkg/@{ZWSP}scope."),
    ] {
        assert!(body.contains(&neutral), "{neutral:?} in {body}");
    }
    for raw in [
        "@octocat",
        "@hubot",
        "@reviewer",
        "@org",
        "@someone",
        "@scope",
        "@after",
    ] {
        assert!(!body.contains(raw), "{raw} in {body}");
    }
    // An `@` after a letter or digit stays as typed.
    for kept in ["Mail a@b.com or", "## photo@2x.png", "me@example.com"] {
        assert!(body.contains(kept), "{kept} in {body}");
    }

    // The intent path seeds from the same text as its issue.md.
    let other = fx.add_repo("r2").await;
    let mut intent = req.clone();
    intent["submission_id"] = json!(SubmissionId::new());
    intent["repo_id"] = json!(other.id);
    intent["send_as"] = json!("intent");
    let (status, out) = fx.submit(&intent, &files).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let seed = out["intent"]["seed"]["text"].as_str().unwrap();
    let issue_md = git_out(
        &other.path,
        &[
            "show",
            &format!(
                "{}:{}/issue.md",
                out["branch"].as_str().unwrap(),
                out["folder"].as_str().unwrap()
            ),
        ],
    );
    assert!(seed.starts_with(request.main_idea.as_str()), "{seed}");
    assert!(seed.contains("cc @someone and me@example.com"), "{seed}");
    assert!(
        issue_md.contains("1. cc @someone and me@example.com"),
        "{issue_md}"
    );
    assert!(!seed.contains(ZWSP) && !issue_md.contains(ZWSP));
}

// --------------------------------------------------------------- refusals

#[tokio::test]
async fn the_issue_path_is_refused_before_anything_is_written() {
    let fx = fixture().await;
    let conflict = StatusCode::CONFLICT;
    let not_github = "the repository's origin is not a GitHub repository";

    let none = fx.add_repo("none").await;
    fx.refused(&none, None, (conflict, "no_github_remote", not_github))
        .await;
    let gitlab = fx
        .add_repo_with_origin("gitlab", "https://gitlab.com/o/r.git")
        .await;
    fx.refused(&gitlab, None, (conflict, "no_github_remote", not_github))
        .await;
    assert!(fx.fake.calls().is_empty(), "{:?}", fx.fake.calls());

    let (repo, bare) = fx.github_repo("signed-out").await;
    fx.fake.set("signed-out", "");
    fx.refused(
        &repo,
        Some(&bare),
        (
            conflict,
            "gh_not_ready",
            "GitHub CLI (gh) is not signed in; run: gh auth login",
        ),
    )
    .await;
    assert_eq!(fx.fake.calls(), [AUTH]);

    let fx = fixture_with(Opts {
        gh: false,
        ..Opts::default()
    })
    .await;
    let (repo, bare) = fx.github_repo("no-gh").await;
    fx.refused(
        &repo,
        Some(&bare),
        (conflict, "gh_not_ready", "GitHub CLI (gh) is not installed"),
    )
    .await;
}

// ------------------------------------------------- partial failure and Retry

#[tokio::test]
async fn a_failed_push_keeps_the_commit_and_retry_pushes_and_creates() {
    let fx = fixture().await;
    let (repo, bare) = fx.github_repo("r1").await;
    let hook = bare.join("hooks/pre-receive");
    std::fs::write(
        &hook,
        "#!/bin/sh\necho 'LEAK: no pushes today' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let req = issue_request(&repo);

    let (status, out) = fx.submit_shot(&req).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    let commit = git_out(&repo.path, &["rev-parse", &format!("refs/heads/{BRANCH}")]);
    assert_eq!(out["branch"], BRANCH);
    assert_eq!(out["commit"], commit);
    assert_eq!(out["pushed"], false);
    assert_eq!(out["issue_url"], Value::Null);
    assert_eq!(
        out["failed"],
        json!({
            "step": "push",
            "code": "push_rejected",
            "message": "git could not push to origin: the remote rejected the branch",
        })
    );
    assert!(
        fx.calls_starting("issue create").is_empty(),
        "{:?}",
        fx.fake.calls()
    );
    assert!(!fx.row(&req["submission_id"]).await.unwrap().pushed);

    std::fs::remove_file(&hook).unwrap();
    let (status, again) = fx.retry(&req["submission_id"]).await;

    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["pushed"], true);
    assert_eq!(again["issue_url"], ISSUE_URL);
    assert_eq!(again["failed"], Value::Null);
    assert_eq!(again["commit"], commit);
    assert_eq!(git_out(&bare, &["rev-parse", BRANCH]), commit);
    assert_eq!(commits_above_main(&repo.path, BRANCH), "1");
    assert_eq!(
        git_out(&repo.path, &["branch", "--list", "feedback/*"]),
        BRANCH
    );
    assert_eq!(fx.calls_starting("issue create").len(), 1);
    assert!(fx.feedback_tmp_is_empty());
}

#[tokio::test]
async fn a_recorded_submission_whose_repository_git_cannot_read_says_so_in_its_step() {
    // W2-3: the commit and its record exist, so this is a failed step, not
    // a refusal that would say nothing was made.
    let fx = fixture().await;
    let (repo, bare) = fx.github_repo("r1").await;
    let hook = refuse_pushes(&bare);
    let req = issue_request(&repo);
    let (status, out) = fx.submit_shot(&req).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["failed"]["code"], "push_rejected");
    std::fs::remove_file(&hook).unwrap();
    let aside = fx.tmp.path().join("aside");
    std::fs::rename(repo.path.join(".git"), &aside).unwrap();

    let (status, again) = fx.retry(&req["submission_id"]).await;

    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        again["failed"],
        json!({
            "step": "push",
            "code": "origin_unreadable",
            "message": "git could not read this repository's settings. Check that its folder is still a git repository.",
        })
    );
    assert_eq!(again["pushed"], false);
    std::fs::rename(&aside, repo.path.join(".git")).unwrap();
    assert_eq!(git_out(&bare, &["for-each-ref"]), "", "nothing pushed");
    let (status, again) = fx.retry(&req["submission_id"]).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["issue_url"], ISSUE_URL);
}

/// A `pre-receive` hook in `bare` that refuses every push; its path.
fn refuse_pushes(bare: &Path) -> PathBuf {
    let hook = bare.join("hooks/pre-receive");
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    hook
}

/// Whether `bare` holds the object `id`.
fn has_object(bare: &Path, id: &str) -> bool {
    git_cmd(bare, &["cat-file", "-e", &format!("{id}^{{commit}}")])
        .status
        .success()
}

#[tokio::test]
async fn a_retry_pushes_the_recorded_commit_not_the_branch_tip() {
    let fx = fixture().await;

    // The local branch moved on before the retry: only the recorded commit
    // goes to origin.
    let (repo, bare) = fx.github_repo("moved").await;
    let hook = refuse_pushes(&bare);
    let req = issue_request(&repo);
    let (status, out) = fx.submit_shot(&req).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["failed"]["code"], "push_rejected", "{out}");
    let commit = out["commit"].as_str().unwrap().to_string();
    let extra = git_out(
        &repo.path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit-tree",
            &format!("{commit}^{{tree}}"),
            "-p",
            &commit,
            "-m",
            "not part of the submission",
        ],
    );
    git_out(
        &repo.path,
        &["update-ref", &format!("refs/heads/{BRANCH}"), &extra],
    );
    std::fs::remove_file(&hook).unwrap();
    let (status, again) = fx.retry(&req["submission_id"]).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["pushed"], true);
    assert_eq!(again["failed"], Value::Null);
    assert_eq!(git_out(&bare, &["rev-parse", BRANCH]), commit);
    assert!(
        !has_object(&bare, &extra),
        "the extra commit reached origin"
    );

    // The local branch was deleted: the recorded commit is still pushed.
    let (repo, bare) = fx.github_repo("deleted").await;
    let hook = refuse_pushes(&bare);
    let req = issue_request(&repo);
    let (_, out) = fx.submit_shot(&req).await;
    assert_eq!(out["failed"]["code"], "push_rejected", "{out}");
    let commit = out["commit"].as_str().unwrap().to_string();
    common::git(&repo.path, &["branch", "-D", BRANCH]);
    std::fs::remove_file(&hook).unwrap();
    let (status, again) = fx.retry(&req["submission_id"]).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["pushed"], true);
    assert_eq!(again["issue_url"], ISSUE_URL);
    assert_eq!(git_out(&bare, &["rev-parse", BRANCH]), commit);

    // The commit is gone from the repository: nothing is pushed.
    let (repo, bare) = fx.github_repo("gone").await;
    let hook = refuse_pushes(&bare);
    let req = issue_request(&repo);
    let (_, out) = fx.submit_shot(&req).await;
    assert_eq!(out["failed"]["code"], "push_rejected", "{out}");
    let commit = out["commit"].as_str().unwrap().to_string();
    common::git(&repo.path, &["branch", "-D", BRANCH]);
    common::git(&repo.path, &["reflog", "expire", "--expire=now", "--all"]);
    common::git(&repo.path, &["gc", "-q", "--prune=now"]);
    assert!(!has_object(&repo.path, &commit));
    std::fs::remove_file(&hook).unwrap();
    let creates = fx.calls_starting("issue create").len();
    let (status, again) = fx.retry(&req["submission_id"]).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["pushed"], false);
    assert_eq!(
        again["failed"],
        json!({
            "step": "push",
            "code": "commit_missing",
            "message": "The feedback commit is no longer in this repository, so it cannot be pushed.",
        })
    );
    assert_eq!(git_out(&bare, &["for-each-ref"]), "");
    assert_eq!(fx.calls_starting("issue create").len(), creates);
}

#[tokio::test]
async fn a_failed_gh_keeps_the_push_and_retry_only_creates_the_issue() {
    let fx = fixture().await;
    let (repo, bare) = fx.github_repo("r1").await;
    fx.fake.set("fail", "issue create");
    let req = issue_request(&repo);

    let (status, out) = fx.submit_shot(&req).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["pushed"], true);
    assert_eq!(out["issue_url"], Value::Null);
    assert_eq!(
        out["failed"],
        json!({
            "step": "create_issue",
            "code": "gh_failed",
            "message": "gh could not create the issue (exit status 1)",
        })
    );
    let commit = out["commit"].as_str().unwrap().to_string();
    assert_eq!(git_out(&bare, &["rev-parse", BRANCH]), commit);
    let pushes = reflog_size(&bare, BRANCH);
    assert_eq!(pushes, 1);
    assert_eq!(fx.calls_starting("issue create").len(), 1);

    // A push now would fail: the retry must not try one.
    let moved = fx.tmp.path().join("moved-origin.git");
    std::fs::rename(&bare, &moved).unwrap();
    fx.clear("fail");
    let (status, again) = fx.retry(&req["submission_id"]).await;

    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["issue_url"], ISSUE_URL);
    assert_eq!(again["pushed"], true);
    assert_eq!(again["failed"], Value::Null);
    assert_eq!(fx.calls_starting("issue create").len(), 2);
    assert_eq!(reflog_size(&moved, BRANCH), pushes);
    assert_eq!(commits_above_main(&repo.path, BRANCH), "1");

    // Complete: answered as it is.
    let (status, third) = fx.retry(&req["submission_id"]).await;
    assert_eq!((status, &third), (StatusCode::OK, &again));
    assert_eq!(fx.calls_starting("issue create").len(), 2);
}

#[tokio::test]
async fn a_hanging_gh_times_out() {
    let fx = fixture_with(Opts {
        gh_timeout: Some(Duration::from_millis(300)),
        ..Opts::default()
    })
    .await;
    let (repo, _bare) = fx.github_repo("r1").await;
    fx.fake.set("hang", "issue create");

    let (status, out) = fx.submit_shot(&issue_request(&repo)).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["pushed"], true);
    assert_eq!(out["failed"]["step"], "create_issue");
    assert_eq!(out["failed"]["code"], "gh_timeout");
    assert_eq!(
        out["failed"]["message"],
        "gh did not finish within 60 seconds"
    );
    // Stopped before it got to hang, it is gone already.
    if let Some(pid) = fx.fake.read("hang.pid") {
        let pid = pid.trim().to_string();
        until("the hanging gh is still running", || !process_alive(&pid)).await;
    }
}

#[tokio::test]
async fn an_unconfirmed_issue_is_not_created_again_for_a_minute() {
    let fx = fixture_with(Opts {
        gh_timeout: Some(Duration::from_millis(300)),
        ..Opts::default()
    })
    .await;
    let (repo, _bare) = fx.github_repo("r1").await;
    let unconfirmed = json!({
        "step": "create_issue",
        "code": "issue_unconfirmed",
        "message": "GitHub has not confirmed whether the issue was created. Try again in a minute.",
    });

    // `gh issue create` timed out: it may have made the issue.
    fx.fake.set("hang", "issue create");
    let req = issue_request(&repo);
    let (_, out) = fx.submit_shot(&req).await;
    assert_eq!(out["failed"]["code"], "gh_timeout", "{out}");
    fx.clear("hang");
    // Retried at once: the search finds nothing yet, and nothing is created.
    let (status, again) = fx.retry(&req["submission_id"]).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["failed"], unconfirmed);
    assert_eq!(fx.calls_starting("issue create").len(), 1);
    let searches = fx.calls_starting("issue list").len();
    assert_eq!(searches, 2, "{:?}", fx.fake.calls());
    // A minute later the search finds it: it is that issue.
    fx.minutes.store(2, Ordering::SeqCst);
    fx.fake.set("existing", ISSUE_URL);
    let (_, found) = fx.retry(&req["submission_id"]).await;
    assert_eq!(found["issue_url"], ISSUE_URL, "{found}");
    assert_eq!(found["failed"], Value::Null);
    assert_eq!(fx.calls_starting("issue create").len(), 1);
    fx.clear("existing");

    // gh answered something that is not an issue's address: the same.
    fx.fake.set("issue-url", "LEAK-not-a-url");
    let mut req = issue_request(&repo);
    req["main_idea"] = json!("A strange answer");
    let (_, out) = fx.submit_shot(&req).await;
    assert_eq!(out["failed"]["code"], "gh_bad_answer", "{out}");
    fx.clear("issue-url");
    let (_, again) = fx.retry(&req["submission_id"]).await;
    assert_eq!(again["failed"], unconfirmed);
    assert_eq!(fx.calls_starting("issue create").len(), 2);
    // A minute later the search still finds nothing: it is created once.
    fx.minutes.store(4, Ordering::SeqCst);
    let (_, made) = fx.retry(&req["submission_id"]).await;
    assert_eq!(made["issue_url"], ISSUE_URL, "{made}");
    assert_eq!(made["failed"], Value::Null);
    assert_eq!(fx.calls_starting("issue create").len(), 3);

    // gh said it failed (an exit status): a retry may create at once.
    fx.fake.set("fail", "issue create");
    let mut req = issue_request(&repo);
    req["main_idea"] = json!("A plain failure");
    let (_, out) = fx.submit_shot(&req).await;
    assert_eq!(out["failed"]["code"], "gh_failed", "{out}");
    fx.clear("fail");
    let (_, again) = fx.retry(&req["submission_id"]).await;
    assert_eq!(again["issue_url"], ISSUE_URL, "{again}");
}

#[tokio::test]
async fn an_issue_that_already_exists_is_not_created_again() {
    let fx = fixture().await;
    let (repo, _bare) = fx.github_repo("r1").await;
    fx.fake.set("fail", "issue create");
    let req = issue_request(&repo);
    let (status, out) = fx.submit_shot(&req).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["failed"]["code"], "gh_failed");

    // gh made it after all (its answer was lost): the search finds it.
    fx.clear("fail");
    fx.fake.set("existing", ISSUE_URL);
    let (status, again) = fx.retry(&req["submission_id"]).await;

    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["issue_url"], ISSUE_URL);
    assert_eq!(again["failed"], Value::Null);
    assert_eq!(fx.calls_starting("issue create").len(), 1);
    let row = fx.row(&req["submission_id"]).await.unwrap();
    assert_eq!(row.issue_url.as_deref(), Some(ISSUE_URL));
}

#[tokio::test]
async fn an_issue_whose_record_cannot_be_saved_is_not_created_again() {
    let fx = fixture().await;
    let (repo, bare) = fx.github_repo("r1").await;
    let req = issue_request(&repo);
    // The save after the push, then both saves after the issue.
    fx.state.feedback.fail_next_saves_for_test(3);

    let (status, out) = fx.submit_shot(&req).await;

    // The issue exists: it is the answer, not an error that invites another.
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["issue_url"], ISSUE_URL);
    assert_eq!(out["failed"], Value::Null);
    let row = fx.row(&req["submission_id"]).await.unwrap();
    assert_eq!(row.issue_url, None);

    // Search finds nothing yet (GitHub's index lags): the daemon remembers.
    let (status, again) = fx.retry(&req["submission_id"]).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["issue_url"], ISSUE_URL);
    assert_eq!(again["pushed"], true);
    assert_eq!(fx.calls_starting("issue create").len(), 1);
    let row = fx.row(&req["submission_id"]).await.unwrap();
    assert_eq!(row.issue_url.as_deref(), Some(ISSUE_URL));
    assert!(row.pushed);
    assert_eq!(reflog_size(&bare, BRANCH), 1);
}

#[tokio::test]
async fn a_wrong_answer_from_gh_is_refused() {
    let fx = fixture().await;
    let (repo, _bare) = fx.github_repo("r1").await;
    let bad_message = "gh did not print the issue's address";
    for (i, answer) in [
        "https://example.com/x",
        "http://github.com/smithagents-test-invalid/widgets/issues/7",
        "https://github.com.evil.example/smithagents-test-invalid/widgets/issues/7",
        "https://github.com/smithagents-test-invalid/widgets/pull/7",
        "https://github.com/smithagents-test-invalid/widgets/issues/7x",
        "https://github.com/smithagents-test-invalid/widgets/issues/",
        "https://github.com/smithagents-test-invalid/widgets/issues/7/LEAK",
        "https://github.com/../widgets/issues/7",
        "https://github.com/LEAK owner/widgets/issues/7",
        "LEAK-not-a-url",
    ]
    .into_iter()
    .enumerate()
    {
        fx.fake.set("issue-url", answer);
        let mut req = issue_request(&repo);
        req["main_idea"] = json!(format!("Answer {i}"));
        let (status, out) = fx.submit_shot(&req).await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(
            out["failed"],
            json!({"step": "create_issue", "code": "gh_bad_answer", "message": bad_message}),
            "{answer}"
        );
        assert_eq!(out["issue_url"], Value::Null, "{answer}");
        assert_eq!(fx.row(&req["submission_id"]).await.unwrap().issue_url, None);
    }

    // What `issue create` prints for a renamed or moved repository (its
    // name now) is an issue all the same, and is kept.
    for (i, answer) in [
        "https://github.com/Smithagents-Test-Invalid/Widgets/issues/12",
        "https://github.com/someone-else/widgets/issues/13",
        "https://github.com/smithagents-test-invalid/gadgets.rs/issues/14",
    ]
    .into_iter()
    .enumerate()
    {
        fx.fake.set("issue-url", answer);
        let mut req = issue_request(&repo);
        req["main_idea"] = json!(format!("Moved {i}"));
        let (_, out) = fx.submit_shot(&req).await;
        assert_eq!(out["issue_url"], answer, "{out}");
        assert_eq!(out["failed"], Value::Null, "{out}");
        let row = fx.row(&req["submission_id"]).await.unwrap();
        assert_eq!(row.issue_url.as_deref(), Some(answer));
    }

    // A search must answer an issue of this repository; anything else
    // creates nothing.
    fx.clear("issue-url");
    for (i, answer) in [
        "https://example.com/x",
        "https://github.com/someone-else/widgets/issues/7",
    ]
    .into_iter()
    .enumerate()
    {
        fx.fake.set("existing", answer);
        let creates = fx.calls_starting("issue create").len();
        let mut req = issue_request(&repo);
        req["main_idea"] = json!(format!("A strange search {i}"));
        let (_, out) = fx.submit_shot(&req).await;
        assert_eq!(out["failed"]["code"], "gh_bad_answer", "{out}");
        assert_eq!(fx.calls_starting("issue create").len(), creates);
    }
}

#[tokio::test]
async fn retry_of_an_unknown_submission_is_404() {
    let fx = fixture().await;
    let (status, body) = fx.retry(&json!(SubmissionId::new())).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(
        body,
        json!({"error": "submission_not_found", "message": "no such submission"})
    );
    let (status, body) = fx
        .call(Method::POST, "/v1/feedback/not-an-id/retry", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn retry_finishes_a_failed_intent_step() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    let req = request_json(repo.id, "intent");
    fx.state
        .feedback
        .fail_next_intent_for_test(IntentError::Invalid("boom".into()));
    let (status, out) = fx.submit(&req, &[]).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["failed"]["step"], "create_intent");
    assert_eq!(fx.intent_count().await, 0);

    let (status, again) = fx.retry(&req["submission_id"]).await;

    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["failed"], Value::Null);
    assert_eq!(again["intent"]["base_branch"], BRANCH);
    assert_eq!(again["commit"], out["commit"]);
    assert_eq!(fx.intent_count().await, 1);
    let (status, third) = fx.retry(&req["submission_id"]).await;
    assert_eq!((status, &third), (StatusCode::OK, &again));
    assert_eq!(fx.intent_count().await, 1);
    assert!(fx.fake.calls().is_empty(), "{:?}", fx.fake.calls());
}

// ---------------------------------------------------- the lock and detaching

#[tokio::test]
async fn a_long_push_holds_up_neither_other_submissions_nor_its_own_client() {
    let fx = Arc::new(fixture().await);
    let (repo, bare) = fx.github_repo("r1").await;
    let (started, gate) = (fx.tmp.path().join("started"), fx.tmp.path().join("gate"));
    // The remote takes the branch only once the test opens the gate.
    let hook = bare.join("hooks/pre-receive");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\ntouch '{}'\ni=0\nwhile [ ! -f '{}' ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done\nexit 0\n",
            started.display(),
            gate.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let req = issue_request(&repo);
    let call = {
        let (fx, req) = (Arc::clone(&fx), req.clone());
        tokio::spawn(async move { fx.submit_shot(&req).await })
    };
    until("the push never began", || started.exists()).await;
    assert!(fx.state.feedback.running_submissions() >= 1);

    // Another submission is made meanwhile.
    let other = fx.add_repo("r2").await;
    let (status, out) =
        tokio::time::timeout(PATIENCE, fx.submit(&request_json(other.id, "intent"), &[]))
            .await
            .expect("another submission waited for the push");
    assert_eq!(status, StatusCode::OK, "{out}");
    assert!(out["intent"]["id"].is_string(), "{out}");
    assert!(!gate.exists());

    // The client goes away; a retry of the same submission waits for it.
    call.abort();
    let _ = call.await;
    let retry = {
        let (fx, id) = (Arc::clone(&fx), req["submission_id"].clone());
        tokio::spawn(async move { fx.retry(&id).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!retry.is_finished());
    assert!(fx.state.feedback.running_submissions() >= 2);
    std::fs::write(&gate, "").unwrap();

    let (status, again) = tokio::time::timeout(PATIENCE, retry)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["issue_url"], ISSUE_URL);
    assert_eq!(again["pushed"], true);
    assert_eq!(fx.calls_starting("issue create").len(), 1);
    assert_eq!(reflog_size(&bare, BRANCH), 1);
    let row = fx.row(&req["submission_id"]).await.unwrap();
    assert_eq!(row.issue_url.as_deref(), Some(ISSUE_URL));
    until("a submission still counts as running", || {
        fx.state.feedback.running_submissions() == 0
    })
    .await;
}

#[tokio::test]
async fn a_slow_sign_in_check_holds_up_no_other_submission() {
    // The check may take a minute here, so only the gate decides when it ends.
    let fx = Arc::new(
        fixture_with(Opts {
            check_timeout: Some(Duration::from_secs(60)),
            ..Opts::default()
        })
        .await,
    );
    let (repo, _bare) = fx.github_repo("r1").await;
    // `gh auth status` waits until the test opens the gate.
    fx.fake.set("wait", "auth status");
    let req = issue_request(&repo);
    let call = {
        let (fx, req) = (Arc::clone(&fx), req.clone());
        tokio::spawn(async move { fx.submit_shot(&req).await })
    };
    until("the check never began", || {
        fx.fake.read("wait.pid").is_some()
    })
    .await;

    // Another submission is made while the check waits.
    let other = fx.add_repo("r2").await;
    let (status, out) =
        tokio::time::timeout(PATIENCE, fx.submit(&request_json(other.id, "intent"), &[]))
            .await
            .expect("another submission waited for the sign-in check");
    assert_eq!(status, StatusCode::OK, "{out}");
    assert!(!call.is_finished(), "the check ended without the gate");

    fx.fake.set("go", "");
    let (status, body) = tokio::time::timeout(PATIENCE, call).await.unwrap().unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["issue_url"], ISSUE_URL);

    // A check that does not answer in its time is "not signed in", and
    // nothing is written (however far the fake got before it was stopped).
    let fx = fixture_with(Opts {
        check_timeout: Some(Duration::from_secs(1)),
        ..Opts::default()
    })
    .await;
    let (repo, bare) = fx.github_repo("r1").await;
    fx.fake.set("hang", "auth status");
    let req = issue_request(&repo);
    let (status, body) = fx.submit_shot(&req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "gh_not_ready");
    assert_eq!(
        body["message"],
        "GitHub CLI (gh) is not signed in; run: gh auth login"
    );
    assert_eq!(fx.row(&req["submission_id"]).await, None);
    assert_eq!(git_out(&bare, &["for-each-ref"]), "");
    // Stopped before it got to hang, it is gone already.
    if let Some(pid) = fx.fake.read("hang.pid") {
        let pid = pid.trim().to_string();
        until("the hanging gh is still running", || !process_alive(&pid)).await;
    }
}

// ------------------------------------------------------------------- leaking

#[tokio::test]
async fn nothing_gh_or_git_printed_reaches_a_response_or_an_event() {
    let logs = logs();
    let fx = fixture().await;
    let mut bodies = Vec::new();

    // gh fails, printing LEAK on both streams; then its retry.
    let (repo, _bare) = fx.github_repo("r1").await;
    fx.fake.set("fail", "issue create");
    let req = issue_request(&repo);
    let (status, text) = fx.submit_shot_text(&req).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("gh_failed"), "{text}");
    bodies.push(text);
    bodies.push(fx.retry_text(&req["submission_id"]).await.1);

    // git cannot reach origin, whose rewritten address holds LEAK.
    let other = fx.add_repo_with_origin("r2", GITHUB_URL).await;
    redirect(&other, Path::new("/nonexistent/LEAK-git-path"));
    let req = issue_request(&other);
    let (status, text) = fx.submit_shot_text(&req).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains(r#""step":"push""#), "{text}");
    bodies.push(text);
    bodies.push(fx.retry_text(&req["submission_id"]).await.1);

    // The targets, with gh failing to answer.
    fx.fake.set("fail", "repo view");
    bodies.push(
        fx.call_text(Method::GET, "/v1/feedback/targets", None)
            .await
            .1,
    );
    fx.fake.set("fail", "auth status");
    bodies.push(
        fx.call_text(Method::GET, "/v1/feedback/targets", None)
            .await
            .1,
    );

    let events: Vec<String> = fx
        .state
        .bus
        .subscribe(Some(0))
        .unwrap()
        .backlog
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    let logged = logs.text();
    // The log is captured and names the steps that failed.
    assert!(logged.contains("step=\"push\""), "{logged}");
    assert!(logged.contains("code=\"gh_failed\""), "{logged}");
    for text in events.iter().chain(&bodies).chain([&logged]) {
        assert!(!text.contains("LEAK"), "{text}");
    }
    let env = fx.fake.read("env.log").unwrap();
    assert_eq!(std::env::var(SECRET.0).as_deref(), Ok(SECRET.1));
    assert!(!env.lines().any(|l| l.starts_with("SMITHAGENTS_")), "{env}");
}

// --------------------------------------------------------------- the targets

#[tokio::test]
async fn targets_report_visibility_and_sign_in() {
    let fx = fixture().await;
    fx.fake.set("visibility", "PUBLIC");
    let public = fx.add_repo_with_origin("public", GITHUB_URL).await;
    let local = fx.add_repo("local").await;
    let views = |fx: &Fx| fx.calls_starting("repo view");

    let (status, body) = fx.call(Method::GET, "/v1/feedback/targets", None).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["repos"],
        json!([
            {"repo_id": public.id, "has_github_remote": true, "is_public": true},
            {"repo_id": local.id, "has_github_remote": false, "is_public": null},
        ])
    );
    assert_eq!(body["gh"], json!({"ready": true, "reason": null}));
    assert_eq!(
        views(&fx),
        ["repo view --json=visibility --jq=.visibility -- github.com/smithagents-test-invalid/widgets"]
    );
    // Asked again within 10 minutes: the answer is remembered.
    fx.minutes.store(9, Ordering::SeqCst);
    let (_, again) = fx.call(Method::GET, "/v1/feedback/targets", None).await;
    assert_eq!(again, body);
    assert_eq!(views(&fx).len(), 1);
    // Later it is asked again.
    fx.fake.set("visibility", "INTERNAL");
    fx.minutes.store(11, Ordering::SeqCst);
    let (_, later) = fx.call(Method::GET, "/v1/feedback/targets", None).await;
    assert_eq!(later["repos"][0]["is_public"], false);
    assert_eq!(views(&fx).len(), 2);

    // PRIVATE, for each GitHub repository (an scp-like remote too).
    let fx = fixture().await;
    fx.add_repo_with_origin("widgets", GITHUB_URL).await;
    fx.add_repo_with_origin(
        "gadgets",
        "git@github.com:smithagents-test-invalid/gadgets.git",
    )
    .await;
    fx.fake.set("visibility", "PRIVATE");
    let (_, body) = fx.call(Method::GET, "/v1/feedback/targets", None).await;
    assert_eq!(body["repos"][0]["is_public"], false, "{body}");
    assert_eq!(body["repos"][1]["is_public"], false, "{body}");
    assert_eq!(views(&fx).len(), 2);

    // A failure is not remembered: the answer comes once gh works.
    let fx = fixture().await;
    fx.add_repo_with_origin("failing", GITHUB_URL).await;
    fx.fake.set("fail", "repo view");
    let (_, body) = fx.call(Method::GET, "/v1/feedback/targets", None).await;
    assert_eq!(body["repos"][0]["is_public"], Value::Null, "{body}");
    fx.clear("fail");
    fx.fake.set("visibility", "PUBLIC");
    let (_, body) = fx.call(Method::GET, "/v1/feedback/targets", None).await;
    assert_eq!(body["repos"][0]["is_public"], true, "{body}");

    // Signed out: gh is not ready and nothing is asked.
    let fx = fixture().await;
    fx.add_repo_with_origin("signed-out", GITHUB_URL).await;
    fx.fake.set("signed-out", "");
    let (status, body) = fx.call(Method::GET, "/v1/feedback/targets", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["gh"],
        json!({"ready": false, "reason": "not signed in"})
    );
    assert_eq!(body["repos"][0]["is_public"], Value::Null);
    assert_eq!(fx.fake.calls(), [AUTH]);
}

// ------------------------------------- what origin has not got stays at home

#[tokio::test]
async fn unpushed_default_branch_commits_are_never_published() {
    let fx = fixture().await;
    let (repo, bare) = fx.github_repo("r1").await;
    // origin has M0; the local main has M1 on top, never pushed.
    common::git(&repo.path, &["push", "-q", bare.to_str().unwrap(), "main"]);
    common::git(&repo.path, &["fetch", "-q", "origin"]);
    let m0 = git_out(&repo.path, &["rev-parse", "main"]);
    common::git(
        &repo.path,
        &["commit", "-q", "--allow-empty", "-m", "M1, not for anyone"],
    );
    let m1 = git_out(&repo.path, &["rev-parse", "main"]);
    let req = issue_request(&repo);

    let (status, body) = fx.submit_shot(&req).await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        (body["error"].as_str(), body["message"].as_str()),
        (Some("unpushed_commits"), Some(UNPUSHED))
    );
    // Nothing was made, and origin never saw M1.
    assert_eq!(git_out(&repo.path, &["branch", "--list", "feedback/*"]), "");
    assert_eq!(fx.row(&req["submission_id"]).await, None);
    assert!(fx.feedback_tmp_is_empty());
    assert_eq!(git_out(&bare, &["rev-parse", "main"]), m0);
    assert!(!has_object(&bare, &m1));
    assert!(
        fx.calls_starting("issue").is_empty(),
        "{:?}",
        fx.fake.calls()
    );

    // Once M1 is on origin, the same submission goes through.
    common::git(&repo.path, &["push", "-q", bare.to_str().unwrap(), "main"]);
    common::git(&repo.path, &["fetch", "-q", "origin"]);
    let (status, out) = fx.submit_shot(&req).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["issue_url"], ISSUE_URL, "{out}");
    assert_eq!(out["failed"], Value::Null);
}

#[tokio::test]
async fn a_repository_never_fetched_is_refused_before_anything_is_written() {
    let fx = fixture().await;
    let (repo, bare) = fx.github_repo("r1").await;
    common::git(
        &repo.path,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );

    fx.refused(
        &repo,
        Some(&bare),
        (StatusCode::CONFLICT, "unpushed_commits", NEVER_FETCHED),
    )
    .await;

    // An intent is not pushed, so it is made all the same.
    let mut req = issue_request(&repo);
    req["send_as"] = json!("intent");
    let (status, out) = fx.submit_shot(&req).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["failed"], Value::Null);
    assert_eq!(git_out(&bare, &["for-each-ref"]), "");
}

#[tokio::test]
async fn every_push_checks_what_origin_has_first() {
    // The check before the push, after the commit: a failed step, retried.
    let fx = fixture().await;
    let (repo, bare) = fx.github_repo("r1").await;
    let hook = refuse_pushes(&bare);
    let req = issue_request(&repo);
    let (status, out) = fx.submit_shot(&req).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["failed"]["code"], "push_rejected", "{out}");
    std::fs::remove_file(&hook).unwrap();
    common::git(
        &repo.path,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );

    let (status, again) = fx.retry(&req["submission_id"]).await;

    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["pushed"], false);
    assert_eq!(
        again["failed"],
        json!({"step": "push", "code": "unpushed_commits", "message": NEVER_FETCHED})
    );
    assert_eq!(git_out(&bare, &["for-each-ref"]), "", "nothing pushed");
    assert!(fx.calls_starting("issue create").is_empty());

    common::git(
        &repo.path,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    let (status, again) = fx.retry(&req["submission_id"]).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["pushed"], true);
    assert_eq!(again["issue_url"], ISSUE_URL);
}

// --------------------------------------------- the public-repository question

#[tokio::test]
async fn a_public_repository_needs_the_users_acknowledgement() {
    let fx = fixture().await;
    fx.fake.set("visibility", "PUBLIC");
    let (repo, bare) = fx.github_repo("r1").await;
    // An older app's request has no acknowledgement: refused, nothing made.
    let req = unacknowledged_issue_request(&repo);
    let (status, body) = fx.submit_shot(&req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        (body["error"].as_str(), body["message"].as_str()),
        (Some("public_not_acknowledged"), Some(PUBLIC))
    );
    assert_eq!(git_out(&repo.path, &["branch", "--list", "feedback/*"]), "");
    assert_eq!(fx.row(&req["submission_id"]).await, None);
    assert!(fx.feedback_tmp_is_empty());
    assert_eq!(git_out(&bare, &["for-each-ref"]), "");
    assert!(
        fx.calls_starting("issue").is_empty(),
        "{:?}",
        fx.fake.calls()
    );
    // So is `false`.
    let mut refused = req.clone();
    refused["public_acknowledged"] = json!(false);
    let (status, body) = fx.submit_shot(&refused).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("public_not_acknowledged"))
    );

    // Acknowledged, under the same id: made.
    let mut acknowledged = req.clone();
    acknowledged["public_acknowledged"] = json!(true);
    let (status, out) = fx.submit_shot(&acknowledged).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["issue_url"], ISSUE_URL);
}

#[tokio::test]
async fn a_private_repository_is_not_asked_and_an_unknown_one_is() {
    let fx = fixture().await;
    let (repo, _bare) = fx.github_repo("r1").await;
    // Private (the fake's default): no acknowledgement needed.
    let (status, out) = fx.submit_shot(&unacknowledged_issue_request(&repo)).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["issue_url"], ISSUE_URL);
    assert_eq!(
        fx.calls_starting("repo view").len(),
        1,
        "{:?}",
        fx.fake.calls()
    );

    // gh cannot tell: it may be public. (The same GitHub repository: past
    // the 10 minutes its answer is used again.)
    let (other, bare) = fx.github_repo("r2").await;
    fx.fake.set("fail", "repo view");
    fx.minutes.store(11, Ordering::SeqCst);
    let req = unacknowledged_issue_request(&other);
    let (status, body) = fx.submit_shot(&req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        (body["error"].as_str(), body["message"].as_str()),
        (Some("public_not_acknowledged"), Some(MAYBE_PUBLIC))
    );
    assert_eq!(fx.row(&req["submission_id"]).await, None);
    assert_eq!(git_out(&bare, &["for-each-ref"]), "");
    // A plain intent is asked as well (its agent's pull request carries the
    // folder); an idea package is not.
    let mut intent = req.clone();
    intent["send_as"] = json!("intent");
    let (status, body) = fx.submit_shot(&intent).await;
    assert_eq!(
        (status, body["error"].as_str(), body["message"].as_str()),
        (
            StatusCode::CONFLICT,
            Some("public_not_acknowledged"),
            Some(MAYBE_PUBLIC)
        ),
        "{body}"
    );
    intent["idea"] = json!({"explainer": true, "diagrams": false, "demo_video": false});
    let (status, out) = fx.submit_shot(&intent).await;
    assert_eq!(status, StatusCode::OK, "{out}");
}

#[tokio::test]
async fn a_recorded_submission_is_not_asked_again() {
    let fx = fixture().await;
    fx.fake.set("visibility", "PUBLIC");
    let (repo, bare) = fx.github_repo("r1").await;
    let hook = refuse_pushes(&bare);
    let req = issue_request(&repo);
    let (status, out) = fx.submit_shot(&req).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["failed"]["code"], "push_rejected", "{out}");
    std::fs::remove_file(&hook).unwrap();

    // The retry route has no acknowledgement, and needs none.
    let (status, again) = fx.retry(&req["submission_id"]).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["issue_url"], ISSUE_URL);
    // Nor does the same request sent again without it.
    let (status, same) = fx
        .submit_shot(&unacknowledged_issue_request_like(&req))
        .await;
    assert_eq!(status, StatusCode::OK, "{same}");
    assert_eq!(same["issue_url"], ISSUE_URL);
    assert_eq!(fx.calls_starting("issue create").len(), 1);
}

/// `req` without its `public_acknowledged`.
fn unacknowledged_issue_request_like(req: &Value) -> Value {
    let mut req = req.clone();
    req.as_object_mut().unwrap().remove("public_acknowledged");
    req
}

// ------------------------------- a plain intent publishes through its agent

/// [`unacknowledged_issue_request`] sent as a plain intent.
fn unacknowledged_intent_request(repo: &Repo) -> Value {
    let mut req = unacknowledged_issue_request(repo);
    req["send_as"] = json!("intent");
    req
}

#[tokio::test]
async fn a_plain_intent_to_a_public_repository_needs_the_acknowledgement() {
    // D-C3.
    let fx = fixture().await;
    fx.fake.set("visibility", "PUBLIC");
    let (repo, bare) = fx.github_repo("r1").await;
    let mut req = unacknowledged_intent_request(&repo);

    let (status, body) = fx.submit_shot(&req).await;

    assert_eq!(
        (status, body["error"].as_str(), body["message"].as_str()),
        (
            StatusCode::CONFLICT,
            Some("public_not_acknowledged"),
            Some(PUBLIC)
        ),
        "{body}"
    );
    assert_eq!(git_out(&repo.path, &["branch", "--list", "feedback/*"]), "");
    assert_eq!(fx.row(&req["submission_id"]).await, None);
    assert_eq!(fx.intent_count().await, 0);
    assert!(fx.feedback_tmp_is_empty());

    // Acknowledged, under the same id: the intent is made (and the daemon
    // pushes nothing itself).
    req["public_acknowledged"] = json!(true);
    let (status, out) = fx.submit_shot(&req).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert!(out["intent"]["id"].is_string(), "{out}");
    assert_eq!(fx.intent_count().await, 1);
    assert_eq!(git_out(&bare, &["for-each-ref"]), "");
}

#[tokio::test]
async fn a_plain_intent_is_asked_only_for_a_github_repository_that_may_be_public() {
    // D-C3.
    let fx = fixture().await;
    // Private (the fake's default): not asked.
    let (repo, _bare) = fx.github_repo("r1").await;
    let (status, out) = fx.submit_shot(&unacknowledged_intent_request(&repo)).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(fx.calls_starting("repo view").len(), 1);
    // No GitHub remote: not asked, and gh is not run for it.
    let local = fx.add_repo("local").await;
    let (status, out) = fx.submit_shot(&unacknowledged_intent_request(&local)).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let elsewhere = fx
        .add_repo_with_origin(
            "elsewhere",
            "https://gitlab.com/smithagents-test-invalid/widgets.git",
        )
        .await;
    let (status, out) = fx
        .submit_shot(&unacknowledged_intent_request(&elsewhere))
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(fx.calls_starting("repo view").len(), 1);
    // An idea package to a public one: not asked (its two actions are).
    fx.fake.set("visibility", "PUBLIC");
    fx.minutes.store(11, Ordering::SeqCst);
    let (public, _bare) = fx.github_repo("r2").await;
    let mut idea = unacknowledged_intent_request(&public);
    idea["idea"] = json!({"explainer": true, "diagrams": false, "demo_video": false});
    let (status, out) = fx.submit_shot(&idea).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(fx.calls_starting("repo view").len(), 1);
}

#[tokio::test]
async fn without_gh_an_acknowledged_intent_is_made_and_gh_is_not_needed() {
    // D-C3: no gh, or one not signed in, cannot tell: it may be public.
    let fx = fixture_with(Opts {
        gh: false,
        ..Opts::default()
    })
    .await;
    let (repo, _bare) = fx.github_repo("r1").await;
    let mut req = unacknowledged_intent_request(&repo);
    let (status, body) = fx.submit_shot(&req).await;
    assert_eq!(
        (status, body["error"].as_str(), body["message"].as_str()),
        (
            StatusCode::CONFLICT,
            Some("public_not_acknowledged"),
            Some(MAYBE_PUBLIC)
        ),
        "{body}"
    );
    req["public_acknowledged"] = json!(true);
    let (status, out) = fx.submit_shot(&req).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert!(out["intent"]["id"].is_string(), "{out}");
    assert!(fx.fake.calls().is_empty(), "{:?}", fx.fake.calls());

    // Signed out: the same.
    let fx = fixture().await;
    fx.fake.set("signed-out", "");
    fx.fake.set("fail", "repo view");
    let (repo, _bare) = fx.github_repo("r1").await;
    let mut req = unacknowledged_intent_request(&repo);
    let (status, body) = fx.submit_shot(&req).await;
    assert_eq!(body["error"], "public_not_acknowledged", "{status} {body}");
    req["public_acknowledged"] = json!(true);
    let calls = fx.fake.calls().len();
    let (status, out) = fx.submit_shot(&req).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(fx.fake.calls().len(), calls, "no further gh call");
}

/// What a daemon stopped between its commit and its record leaves: the
/// submission's commit on its branch, with no row; the branch and commit.
async fn commit_without_record(fx: &Fx, repo: &Repo, id: &Value) -> (String, String) {
    let id: SubmissionId = serde_json::from_value(id.clone()).unwrap();
    let files = [FolderFile {
        name: "issue.md".into(),
        source: FileSource::Text("# Fix the zoom\n".into()),
    }];
    let scratch = fx.paths.feedback_tmp_dir().join("stopped-run");
    let date = start().date_naive();
    let (placement, commit) =
        place_and_commit(repo, "Fix the zoom", date, false, &files, id, &scratch)
            .await
            .unwrap();
    std::fs::remove_dir_all(&scratch).unwrap();
    assert_eq!(fx.state.store.get_submission(id).await.unwrap(), None);
    (placement.branch, commit)
}

#[tokio::test]
async fn a_commit_taken_up_after_a_stop_meets_the_same_checks() {
    // D-C2: a request re-sent after a stop between the commit and the
    // record is checked as a new one before its commit is recorded.
    let fx = fixture().await;
    fx.fake.set("visibility", "PUBLIC");
    let (repo, bare) = fx.github_repo("r1").await;
    let mut req = unacknowledged_issue_request(&repo);
    let (branch, commit) = commit_without_record(&fx, &repo, &req["submission_id"]).await;

    let (status, body) = fx.submit_shot(&req).await;

    assert_eq!(
        (status, body["error"].as_str(), body["message"].as_str()),
        (
            StatusCode::CONFLICT,
            Some("public_not_acknowledged"),
            Some(PUBLIC)
        ),
        "{body}"
    );
    assert_eq!(fx.row(&req["submission_id"]).await, None, "not recorded");
    assert_eq!(git_out(&bare, &["for-each-ref"]), "", "nothing pushed");
    assert!(fx.calls_starting("issue create").is_empty());
    // The commit is left as it was.
    assert_eq!(git_out(&repo.path, &["rev-parse", &branch]), commit);

    // Acknowledged: that commit is taken up, not made again.
    req["public_acknowledged"] = json!(true);
    let (status, out) = fx.submit_shot(&req).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["commit"], commit);
    assert_eq!(out["issue_url"], ISSUE_URL);
    assert_eq!(
        git_out(&repo.path, &["branch", "--list", "feedback/*"]),
        branch
    );
    assert_eq!(commits_above_main(&repo.path, &branch), "1");

    // The push guard too: main has a commit origin lacks.
    let mut req = issue_request(&repo);
    req["main_idea"] = json!("Another idea");
    let (branch, commit) = commit_without_record(&fx, &repo, &req["submission_id"]).await;
    common::git(
        &repo.path,
        &["commit", "-q", "--allow-empty", "-m", "M1, not for anyone"],
    );
    let (status, body) = fx.submit_shot(&req).await;
    assert_eq!(
        (status, body["error"].as_str(), body["message"].as_str()),
        (
            StatusCode::CONFLICT,
            Some("unpushed_commits"),
            Some(UNPUSHED)
        ),
        "{body}"
    );
    assert_eq!(fx.row(&req["submission_id"]).await, None);
    assert_eq!(git_out(&repo.path, &["rev-parse", &branch]), commit);
    assert_eq!(fx.calls_starting("issue create").len(), 1);
}

#[tokio::test]
async fn each_github_repository_gets_its_own_answer() {
    // D-I3 (124f): what gh said of one repository is never another's.
    for public_first in [false, true] {
        let fx = fixture().await;
        fx.fake.set("visibility-widgets", "PUBLIC");
        fx.fake.set("visibility-gadgets", "PRIVATE");
        let (public, public_bare) = fx.github_repo("r1").await;
        let (private, _bare) = fx
            .github_repo_at(
                "r2",
                "https://github.com/smithagents-test-invalid/gadgets.git",
            )
            .await;
        let (to_private, to_public) = (
            unacknowledged_issue_request(&private),
            unacknowledged_issue_request(&public),
        );
        let (private_answer, public_answer) = if public_first {
            let p = fx.submit_shot_text(&to_public).await;
            (fx.submit_shot_text(&to_private).await, p)
        } else {
            let p = fx.submit_shot_text(&to_private).await;
            (p, fx.submit_shot_text(&to_public).await)
        };
        assert_eq!(private_answer.0, StatusCode::OK, "{}", private_answer.1);
        assert_eq!(public_answer.0, StatusCode::CONFLICT, "{}", public_answer.1);
        let body: Value = serde_json::from_str(&public_answer.1).unwrap();
        assert_eq!(
            (body["error"].as_str(), body["message"].as_str()),
            (Some("public_not_acknowledged"), Some(PUBLIC))
        );
        assert_eq!(git_out(&public_bare, &["for-each-ref"]), "");
        assert_eq!(
            fx.calls_starting("repo view").len(),
            2,
            "public first: {public_first}"
        );
    }
}

#[tokio::test]
async fn what_gh_left_unconfirmed_is_forgotten_after_the_wait() {
    // D-M10: the memory of an unanswered `gh issue create` never grows.
    let fx = fixture_with(Opts {
        gh_timeout: Some(Duration::from_millis(300)),
        ..Opts::default()
    })
    .await;
    let (repo, _bare) = fx.github_repo("r1").await;
    fx.fake.set("hang", "issue create");
    let (_, out) = fx.submit_shot(&issue_request(&repo)).await;
    assert_eq!(out["failed"]["code"], "gh_timeout", "{out}");
    assert_eq!(fx.state.feedback.unconfirmed_for_test(), 1);
    fx.clear("hang");

    // Another submission, past the wait: the old entry is dropped.
    fx.minutes.store(2, Ordering::SeqCst);
    let mut other = issue_request(&repo);
    other["main_idea"] = json!("Another idea");
    let (status, made) = fx.submit_shot(&other).await;
    assert_eq!(status, StatusCode::OK, "{made}");
    assert_eq!(made["issue_url"], ISSUE_URL);
    assert_eq!(fx.state.feedback.unconfirmed_for_test(), 0);
}

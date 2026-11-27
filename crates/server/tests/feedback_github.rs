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
use smithagents_core::feedback::SubmissionId;
use smithagents_core::ids::IntentId;
use smithagents_plugin_host::StaticSecrets;
use smithagents_server::api::{router, AppState};
use smithagents_server::events::EventBus;
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
const BRANCH: &str = "feedback/2026-11-26-fix-the-zoom";
const FOLDER: &str = "docs/issues/2026-11-26-fix-the-zoom";
/// A plugin secret in the test process's environment, which `gh` must never
/// see.
const SECRET: (&str, &str) = ("SMITHAGENTS_SECRET_FEEDBACK__X", "LEAK-secret");
/// Waits in these tests end long before this unless something is wrong.
const PATIENCE: Duration = Duration::from_secs(20);

/// Once per binary, before any git or fake runs: `HOME` and
/// `XDG_CONFIG_HOME` name an empty directory, so no configuration of the
/// machine's user changes what git does, and the process holds [`SECRET`].
fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join("feedback_github-empty-home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", &home);
        std::env::set_var(SECRET.0, SECRET.1);
    });
}

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
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            gh: true,
            gh_timeout: None,
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

    /// [`Fx::add_repo`] whose `origin` is `url`.
    async fn add_repo_with_origin(&self, name: &str, url: &str) -> Repo {
        let repo = self.add_repo(name).await;
        common::git(&repo.path, &["remote", "add", "origin", url]);
        repo
    }

    /// [`Fx::add_repo`] whose `origin` is [`GITHUB_URL`], which git sends to
    /// the returned bare repository (it logs every update of its refs).
    async fn github_repo(&self, name: &str) -> (Repo, PathBuf) {
        let repo = self.add_repo_with_origin(name, GITHUB_URL).await;
        let bare = self.tmp.path().join(format!("{name}-origin.git"));
        common::git(
            self.tmp.path(),
            &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
        );
        common::git(&bare, &["config", "core.logAllRefUpdates", "always"]);
        redirect(&repo, &bare);
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
    common::git(
        &repo.path,
        &[
            "config",
            &format!("url.{}.insteadOf", target.display()),
            GITHUB_URL,
        ],
    );
}

/// A `github_issue` request for `repo`: "Fix the zoom", kind `new_feature`,
/// one screenshot with one comment.
fn issue_request(repo: &Repo) -> Value {
    let mut req = request_json(repo.id, "github_issue");
    req["kind"] = json!("new_feature");
    req["attachments"] = json!([screenshot("Too big")]);
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
    assert_eq!(calls[0], "auth status");
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
    // The search looks for the submission's id; every call on the
    // repository names it, joined to its option.
    let id = req["submission_id"].as_str().unwrap();
    assert!(
        calls[1].contains(&format!("--search={id} in:body")),
        "{calls:?}"
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
    ] {
        assert_eq!(
            env.lines().filter(|l| *l == var).count(),
            4,
            "{var} in every run"
        );
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

#[tokio::test]
async fn mentions_in_the_title_and_comments_notify_no_one() {
    let fx = fixture().await;
    let (repo, _bare) = fx.github_repo("r1").await;
    let mut req = issue_request(&repo);
    req["main_idea"] = json!("@octocat the zoom\n\nThe owner's own words: ping @hubot.");
    req["attachments"] = json!([
        screenshot("cc @someone"),
        {"kind": "image", "original_name": "@team.png"},
    ]);
    let (shot, annotated, image) = (png(300), png(400), png(200));

    let (status, out) = fx
        .submit(
            &req,
            &[
                ("file-1", &shot),
                ("annotated-1", &annotated),
                ("file-2", &image),
            ],
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{out}");
    let create = fx.calls_starting("issue create").pop().unwrap();
    assert!(
        create.starts_with("issue create --title=@\u{200b}octocat the zoom --body-file="),
        "{create}"
    );
    let body = fx.fake.read("issue-body.md").unwrap();
    assert!(body.starts_with("# @\u{200b}octocat the zoom\n"), "{body}");
    assert!(body.contains("cc @\u{200b}someone"), "{body}");
    assert!(body.contains("## @\u{200b}team.png"), "{body}");
    for raw in ["@octocat", "@someone", "@team"] {
        assert!(!body.contains(raw), "{raw} in {body}");
    }
    // The main idea's own text is the owner's and stays as written.
    assert!(
        body.contains("The owner's own words: ping @hubot."),
        "{body}"
    );
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
    assert_eq!(fx.fake.calls(), ["auth status"]);

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
    let pid = fx.fake.read("hang.pid").unwrap();
    let pid = pid.trim().to_string();
    until("the hanging gh is still running", || !process_alive(&pid)).await;
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
        "https://github.com/someone-else/widgets/issues/7",
        "https://github.com/smithagents-test-invalid/gadgets/issues/7",
        "https://github.com/smithagents-test-invalid/widgets/pull/7",
        "https://github.com/smithagents-test-invalid/widgets/issues/7x",
        "https://github.com/smithagents-test-invalid/widgets/issues/",
        "https://github.com/smithagents-test-invalid/widgets/issues/7/LEAK",
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

    // The same name in another case is the same repository.
    fx.fake.set(
        "issue-url",
        "https://github.com/Smithagents-Test-Invalid/Widgets/issues/12",
    );
    let mut req = issue_request(&repo);
    req["main_idea"] = json!("Another case");
    let (_, out) = fx.submit_shot(&req).await;
    assert_eq!(
        out["issue_url"],
        "https://github.com/Smithagents-Test-Invalid/Widgets/issues/12"
    );

    // A search that answers something else creates nothing.
    fx.clear("issue-url");
    fx.fake.set("existing", "https://example.com/x");
    let creates = fx.calls_starting("issue create").len();
    let mut req = issue_request(&repo);
    req["main_idea"] = json!("A strange search");
    let (_, out) = fx.submit_shot(&req).await;
    assert_eq!(out["failed"]["code"], "gh_bad_answer", "{out}");
    assert_eq!(fx.calls_starting("issue create").len(), creates);
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
    let fx = Arc::new(fixture().await);
    let (repo, bare) = fx.github_repo("r1").await;
    // `gh auth status` never answers: the check gives up after 5 seconds.
    fx.fake.set("hang", "auth status");
    let req = issue_request(&repo);
    let call = {
        let (fx, req) = (Arc::clone(&fx), req.clone());
        tokio::spawn(async move { fx.submit_shot(&req).await })
    };
    until("the check never began", || {
        fx.fake.read("hang.pid").is_some()
    })
    .await;

    let other = fx.add_repo("r2").await;
    let (status, out) = fx.submit(&request_json(other.id, "intent"), &[]).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert!(
        !call.is_finished(),
        "the check ended before the other submission"
    );

    let (status, body) = tokio::time::timeout(PATIENCE, call).await.unwrap().unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "gh_not_ready");
    assert_eq!(
        body["message"],
        "GitHub CLI (gh) is not signed in; run: gh auth login"
    );
    assert_eq!(fx.row(&req["submission_id"]).await, None);
    assert_eq!(git_out(&bare, &["for-each-ref"]), "");
    let pid = fx.fake.read("hang.pid").unwrap();
    let pid = pid.trim().to_string();
    until("the hanging gh is still running", || !process_alive(&pid)).await;
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
    assert_eq!(fx.fake.calls(), ["auth status"]);
}

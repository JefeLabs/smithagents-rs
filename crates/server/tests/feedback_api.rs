//! The feedback routes: the meta repository setting, the targets Submit can
//! choose from, and `POST /v1/feedback` (a multipart upload committed as an
//! issue folder on a new branch, then an intent).
//!
//! Repositories are temp directories with their own neutral identity;
//! `origin`, where there is one, is only read, never contacted. `gh` and
//! `agent-browser` are the fakes of `tests/bin`. Polish never runs: the
//! intent service's polish runner does nothing.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::http::{header, Method, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use common::feedback::{jpeg, mp4, multipart, multipart_named, png, request_json, FakeTools};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use smithagents_core::agent::Repo;
use smithagents_core::feedback::{AttachmentKind, FeedbackRequest, SubmissionId};
use smithagents_core::ids::{IntentId, RepoId};
use smithagents_core::intent::{IntentFilter, Phase};
use smithagents_plugin_host::StaticSecrets;
use smithagents_server::api::{router, AppState};
use smithagents_server::events::EventBus;
use smithagents_server::feedback::issue::{file_names, render_issue, FileKind, IssueInput};
use smithagents_server::feedback::repo::{place_and_commit, FileSource, FolderFile};
use smithagents_server::feedback::upload::UploadLimits;
use smithagents_server::feedback::FeedbackService;
use smithagents_server::harness::fake::FakeHarness;
use smithagents_server::intents::{IntentError, IntentService, PolishRunner};
use smithagents_server::paths::Paths;
use smithagents_server::plugins::{embedded_plugins, ManagerConfig, PluginManager};
use smithagents_server::queue::{Scheduler, SchedulerHandle};
use smithagents_server::service::AgentService;
use smithagents_server::store::Store;
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "test-token-123";
const GITHUB_URL: &str = "https://github.com/smithagents-test-invalid/widgets.git";
const BRANCH: &str = "feedback/2026-11-26-fix-the-zoom";
const FOLDER: &str = "docs/issues/2026-11-26-fix-the-zoom";
/// Waits in these tests end long before this unless something is wrong.
const PATIENCE: Duration = Duration::from_secs(20);

/// The service clock: 2026-11-26T14:15:00Z.
fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 26, 14, 15, 0).unwrap()
}

/// A polish runner that never polishes: an intent stays as it was created.
struct NoPolish;

impl PolishRunner for NoPolish {
    fn start(&self, _: IntentId, _: u8) {}
}

struct Opts {
    gh: bool,
    agent_browser: bool,
    limits: UploadLimits,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            gh: true,
            agent_browser: true,
            limits: UploadLimits::default(),
        }
    }
}

struct Fx {
    tmp: TempDir,
    state: AppState,
    paths: Paths,
    fake: FakeTools,
    _sched: SchedulerHandle,
}

async fn fixture() -> Fx {
    fixture_with(Opts::default()).await
}

async fn fixture_with(opts: Opts) -> Fx {
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
    let fake = FakeTools::new(&tmp.path().join("fakes"), opts.gh, opts.agent_browser);
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
    let feedback = FeedbackService::new(
        store.clone(),
        intents.clone(),
        service.clone(),
        plugins.clone(),
        paths.clone(),
    )
    .with_clock(Arc::new(now))
    .with_limits(opts.limits);
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
        _sched: sched,
    }
}

impl Fx {
    async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.call_with(method, uri, body, Some(TOKEN)).await
    }

    async fn call_with(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let req = req
            .body(match body {
                Some(v) => Body::from(v.to_string()),
                None => Body::empty(),
            })
            .unwrap();
        let (status, text) = self.send(req).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// The answer's status and its body as text.
    async fn send(&self, req: Request<Body>) -> (StatusCode, String) {
        let resp = router(self.state.clone()).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// `POST /v1/feedback` with this content type and body.
    async fn upload(&self, content_type: &str, body: impl Into<Body>) -> (StatusCode, Value) {
        let (status, text) = self.upload_text(content_type, body).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn upload_text(&self, content_type: &str, body: impl Into<Body>) -> (StatusCode, String) {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/feedback")
            .header(header::CONTENT_TYPE, content_type)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(body.into())
            .unwrap();
        self.send(req).await
    }

    /// Submits `request` followed by `files`.
    async fn submit(&self, request: &Value, files: &[(&str, &[u8])]) -> (StatusCode, Value) {
        let json = request.to_string();
        let mut parts: Vec<(&str, &[u8])> = vec![("request", json.as_bytes())];
        parts.extend_from_slice(files);
        let (content_type, body) = multipart(&parts);
        self.upload(&content_type, body).await
    }

    /// A repository on `main` with one commit and its own identity, added
    /// to the daemon.
    async fn add_repo(&self, name: &str) -> Repo {
        let dir = self.tmp.path().join(name);
        common::init_repo(&dir);
        common::git(&dir, &["config", "user.name", "Test User"]);
        common::git(&dir, &["config", "user.email", "test@example.invalid"]);
        let (status, body) = self
            .call(
                Method::POST,
                "/v1/repos",
                Some(json!({ "path": dir, "source_control": "git" })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        serde_json::from_value(body).unwrap()
    }

    fn feedback_tmp_is_empty(&self) -> bool {
        match std::fs::read_dir(self.paths.feedback_tmp_dir()) {
            Ok(mut entries) => entries.next().is_none(),
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        }
    }

    async fn intents(&self) -> Vec<smithagents_core::intent::Intent> {
        self.state
            .intents
            .list(IntentFilter {
                state: None,
                repo_id: None,
            })
            .await
            .unwrap()
    }

    /// No `feedback/*` branch in `dir`, no row for `id`, nothing under the
    /// feedback temp directory and no intent.
    async fn assert_nothing_created(&self, dir: &Path, id: &Value, what: &str) {
        assert_eq!(
            git_out(dir, &["branch", "--list", "feedback/*"]),
            "",
            "{what}"
        );
        let id: SubmissionId = serde_json::from_value(id.clone()).unwrap();
        assert_eq!(
            self.state.store.get_submission(id).await.unwrap(),
            None,
            "{what}"
        );
        assert!(self.feedback_tmp_is_empty(), "{what}");
        assert!(self.intents().await.is_empty(), "{what}");
    }

    /// Submits and expects a refusal with `status`, `code` and `message`,
    /// with nothing created.
    async fn refused(
        &self,
        dir: &Path,
        request: &Value,
        files: &[(&str, &[u8])],
        expected: (StatusCode, &str, &str),
    ) {
        let (status, body) = self.submit(request, files).await;
        let (code, message) = (body["error"].as_str(), body["message"].as_str());
        assert_eq!(
            (status, code, message),
            (expected.0, Some(expected.1), Some(expected.2)),
            "{body}"
        );
        self.assert_nothing_created(dir, &request["submission_id"], expected.2)
            .await;
    }
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

/// git's standard output, as bytes; panics when git fails.
fn git_bytes(cwd: &Path, args: &[&str]) -> Vec<u8> {
    let out = git_cmd(cwd, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// git's standard output, trimmed; panics when git fails.
fn git_out(cwd: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git_bytes(cwd, args))
        .trim()
        .to_string()
}

fn screenshot(boxes: Value) -> Value {
    json!({"kind": "screenshot", "original_name": "Screenshot 1.png", "boxes": boxes})
}

fn image(name: &str) -> Value {
    json!({"kind": "image", "original_name": name})
}

fn video(name: &str) -> Value {
    json!({"kind": "video", "original_name": name})
}

fn a_box(comment: &str) -> Value {
    json!({"x": 0.1, "y": 0.2, "w": 0.3, "h": 0.4, "comment": comment})
}

/// Every file under `root` named `name`.
fn find_named(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_name() == name {
                found.push(path.clone());
            }
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                todo.push(path);
            }
        }
    }
    found
}

/// Everything this test binary logs at info and above, from every test.
/// Global on purpose: a subscriber set for one thread misses events whose
/// call site another test's thread reached first.
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

// ------------------------------------------------------- the meta repository

#[tokio::test]
async fn the_meta_repository_can_be_read_set_and_cleared() {
    let fx = fixture().await;
    let repo = fx.add_repo("one").await;

    let (status, body) = fx.call(Method::GET, "/v1/settings/feedback", None).await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, json!({"meta_repo_id": null}))
    );

    let put = json!({ "meta_repo_id": repo.id });
    let (status, body) = fx
        .call(Method::PUT, "/v1/settings/feedback", Some(put.clone()))
        .await;
    assert_eq!((status, &body), (StatusCode::OK, &put));
    let (_, body) = fx.call(Method::GET, "/v1/settings/feedback", None).await;
    assert_eq!(body, put);

    let (status, body) = fx
        .call(
            Method::PUT,
            "/v1/settings/feedback",
            Some(json!({"meta_repo_id": null})),
        )
        .await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, json!({"meta_repo_id": null}))
    );
    let (_, body) = fx.call(Method::GET, "/v1/settings/feedback", None).await;
    assert_eq!(body, json!({"meta_repo_id": null}));
}

#[tokio::test]
async fn an_unknown_meta_repository_is_refused() {
    let fx = fixture().await;
    let repo = fx.add_repo("one").await;
    let put = json!({ "meta_repo_id": repo.id });
    fx.call(Method::PUT, "/v1/settings/feedback", Some(put.clone()))
        .await;

    let (status, body) = fx
        .call(
            Method::PUT,
            "/v1/settings/feedback",
            Some(json!({ "meta_repo_id": RepoId::new() })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "repo_not_found");
    let (_, body) = fx.call(Method::GET, "/v1/settings/feedback", None).await;
    assert_eq!(body, put);
}

#[tokio::test]
async fn a_stored_repository_that_is_gone_reads_as_none() {
    let fx = fixture().await;
    fx.state
        .store
        .set_setting(
            "feedback",
            &smithagents_core::feedback::FeedbackSettings {
                meta_repo_id: Some(RepoId::new()),
            },
        )
        .await
        .unwrap();
    let (status, body) = fx.call(Method::GET, "/v1/settings/feedback", None).await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, json!({"meta_repo_id": null}))
    );
}

#[tokio::test]
async fn the_route_needs_the_token() {
    let fx = fixture().await;
    for method in [Method::GET, Method::PUT] {
        let (status, _) = fx
            .call_with(method, "/v1/settings/feedback", Some(json!({})), None)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

// ------------------------------------------------------------ the body limit

#[tokio::test]
async fn the_feedback_route_has_its_own_body_limit() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    let mut req = request_json(repo.id, "intent");
    req["attachments"] = json!([image("a.png"), image("b.png")]);
    let big = png(2 * 1024 * 1024);

    // 2 MiB is read to its end: the refusal is about the missing part.
    let (status, body) = fx.submit(&req, &[("file-1", &big)]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "invalid", "{body}");
    assert_eq!(body["message"], "attachment 2 was not sent", "{body}");

    // Every other route keeps the daemon's 1 MiB.
    let path = "x".repeat(2 * 1024 * 1024);
    let (status, _) = fx
        .call(Method::POST, "/v1/repos", Some(json!({ "path": path })))
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

// --------------------------------------------------------------- the targets

#[tokio::test]
async fn targets_list_repositories_tools_and_the_meta_repository() {
    let fx = fixture().await;
    let a = fx.add_repo("a").await;
    common::git(&a.path, &["remote", "add", "origin", GITHUB_URL]);
    let b = fx.add_repo("b").await;

    let (status, body) = fx.call(Method::GET, "/v1/feedback/targets", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The fake `gh` says every repository is private.
    assert_eq!(
        body,
        json!({
            "repos": [
                {"repo_id": a.id, "has_github_remote": true, "is_public": false},
                {"repo_id": b.id, "has_github_remote": false, "is_public": null},
            ],
            "meta_repo_id": null,
            "gh": {"ready": true, "reason": null},
            "agent_browser": {"ready": true, "reason": null},
        })
    );

    let put = json!({ "meta_repo_id": b.id });
    let (status, _) = fx
        .call(Method::PUT, "/v1/settings/feedback", Some(put))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = fx.call(Method::GET, "/v1/feedback/targets", None).await;
    assert_eq!(body["meta_repo_id"], json!(b.id));
    // gh was asked whether it is signed in each time, and once whether the
    // GitHub repository is public; agent-browser was only looked up.
    let view = "repo view --json=visibility --jq=.visibility -- github.com/smithagents-test-invalid/widgets";
    let auth = "auth status --hostname=github.com";
    assert_eq!(fx.fake.calls(), [auth, view, auth]);

    // A plugin that is turned off has no tool ready, and none runs.
    let (status, _) = fx
        .call(Method::POST, "/v1/plugins/feedback/disable", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = fx.call(Method::GET, "/v1/feedback/targets", None).await;
    let off = json!({"ready": false, "reason": "turned off or not running"});
    assert_eq!(body["gh"], off);
    assert_eq!(body["agent_browser"], off);
    assert_eq!(body["repos"][0]["is_public"], Value::Null);
    assert_eq!(fx.fake.calls().len(), 3);
}

#[tokio::test]
async fn targets_without_agent_browser_and_with_one_repository() {
    let fx = fixture_with(Opts {
        agent_browser: false,
        ..Opts::default()
    })
    .await;
    let only = fx.add_repo("only").await;

    let (status, body) = fx.call(Method::GET, "/v1/feedback/targets", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["gh"], json!({"ready": true, "reason": null}));
    assert_eq!(
        body["agent_browser"],
        json!({"ready": false, "reason": "not installed"})
    );
    // The only repository is the meta repository.
    assert_eq!(body["meta_repo_id"], json!(only.id));
}

#[tokio::test]
async fn targets_never_show_a_remote_url() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    common::git(
        &repo.path,
        &[
            "remote",
            "add",
            "origin",
            "https://user:LEAK-token@github.com/o/r.git",
        ],
    );
    let req = Request::builder()
        .uri("/v1/feedback/targets")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let (status, text) = fx.send(req).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains(r#""has_github_remote":true"#), "{text}");
    assert!(!text.contains("LEAK") && !text.contains("user:"), "{text}");
}

// ---------------------------------------------------------------- submitting

#[tokio::test]
async fn an_intent_submission_commits_the_folder_and_creates_the_intent() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    let dir = repo.path.clone();
    let mut req = request_json(repo.id, "intent");
    req["attachments"] = json!([
        screenshot(json!([a_box("Too big"), a_box("Also here")])),
        image("photo.jpg"),
    ]);
    let (shot, annotated, photo) = (png(300), png(400), jpeg());

    let (status, out) = fx
        .submit(
            &req,
            &[
                ("file-1", &shot),
                ("annotated-1", &annotated),
                ("file-2", &photo),
            ],
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{out}");
    let commit = git_out(&dir, &["rev-parse", &format!("refs/heads/{BRANCH}")]);
    assert_eq!(out["submission_id"], req["submission_id"]);
    assert_eq!(out["repo_id"], json!(repo.id));
    assert_eq!(out["branch"], BRANCH);
    assert_eq!(out["folder"], FOLDER);
    assert_eq!(out["commit"], commit);
    assert_eq!(out["pushed"], false);
    assert_eq!(out["issue_url"], Value::Null);
    assert_eq!(out["failed"], Value::Null);

    // issue.md is the writer's text for the same input.
    let request: FeedbackRequest = serde_json::from_value(req.clone()).unwrap();
    let names = file_names(&[
        (AttachmentKind::Screenshot, FileKind::Png),
        (AttachmentKind::Image, FileKind::Jpeg),
    ]);
    let expected = render_issue(&IssueInput {
        title: "Fix the zoom",
        main_idea: &request.main_idea,
        kind: request.kind,
        created: now(),
        app_version: "0.1.0",
        platform: "macos",
        attachments: &request.attachments,
        names: &names,
    });
    let issue = git_bytes(&dir, &["show", &format!("{BRANCH}:{FOLDER}/issue.md")]);
    assert_eq!(String::from_utf8(issue).unwrap(), expected);
    let tree = git_out(
        &dir,
        &["ls-tree", "--name-only", &format!("{BRANCH}:{FOLDER}")],
    );
    assert_eq!(
        tree.lines().collect::<Vec<_>>(),
        [
            "image-1.jpg",
            "issue.md",
            "screenshot-1.annotated.png",
            "screenshot-1.png"
        ]
    );
    for (name, bytes) in [
        ("screenshot-1.png", &shot),
        ("screenshot-1.annotated.png", &annotated),
        ("image-1.jpg", &photo),
    ] {
        let stored = git_bytes(&dir, &["show", &format!("{BRANCH}:{FOLDER}/{name}")]);
        assert_eq!(&stored, bytes, "{name}");
    }
    assert_eq!(
        git_out(&dir, &["rev-list", "--count", &format!("main..{BRANCH}")]),
        "1"
    );

    // The intent starts from the branch and knows where its evidence is.
    let intent = &out["intent"];
    assert_eq!(intent["repo_id"], json!(repo.id));
    assert_eq!(intent["content"]["kind"], "bug");
    assert_eq!(intent["seed"]["kind"], "bug");
    assert_eq!(intent["base_branch"], BRANCH);
    assert_eq!(
        intent["origin"],
        json!({"folder": FOLDER, "branch": BRANCH, "commit": commit})
    );
    let seed = intent["seed"]["text"].as_str().unwrap();
    assert!(
        seed.starts_with("Fix the zoom\n\nThe page is too big."),
        "{seed}"
    );
    assert!(
        seed.ends_with(&format!(
            "Feedback files: `{FOLDER}/` on branch `{BRANCH}` (commit {commit})."
        )),
        "{seed}"
    );
    let id = intent["id"].as_str().unwrap();
    let (status, got) = fx
        .call(Method::GET, &format!("/v1/intents/{id}"), None)
        .await;
    assert_eq!((status, &got), (StatusCode::OK, intent));

    // The submission is recorded with its intent.
    let sid: SubmissionId = serde_json::from_value(req["submission_id"].clone()).unwrap();
    let row = fx.state.store.get_submission(sid).await.unwrap().unwrap();
    assert_eq!(row.commit, commit);
    assert_eq!(row.branch, BRANCH);
    assert_eq!(row.intent_id.map(|i| i.to_string()).as_deref(), Some(id));

    // The user's checkout is untouched, nothing is left behind and gh never ran.
    assert_eq!(git_out(&dir, &["status", "--porcelain"]), "");
    assert_eq!(git_out(&dir, &["branch", "--show-current"]), "main");
    assert!(fx.feedback_tmp_is_empty());
    assert!(fx.fake.calls().is_empty(), "{:?}", fx.fake.calls());
}

#[tokio::test]
async fn a_submission_without_attachments_is_allowed() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    let req = request_json(repo.id, "intent");

    let (status, out) = fx.submit(&req, &[]).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["branch"], BRANCH);
    let tree = git_out(
        &repo.path,
        &["ls-tree", "--name-only", &format!("{BRANCH}:{FOLDER}")],
    );
    assert_eq!(tree, "issue.md");
    assert_eq!(out["intent"]["base_branch"], BRANCH);
    assert!(fx.feedback_tmp_is_empty());
}

#[tokio::test]
async fn hostile_and_oversized_uploads_are_refused_and_leave_nothing() {
    let fx = fixture_with(Opts {
        limits: UploadLimits {
            image_bytes: 1024,
            video_bytes: 4096,
            total_bytes: 6000,
            ..UploadLimits::default()
        },
        ..Opts::default()
    })
    .await;
    let repo = fx.add_repo("r1").await;
    let dir = repo.path.clone();
    let request = |attachments: Value| {
        let mut r = request_json(repo.id, "intent");
        r["attachments"] = attachments;
        r
    };
    let text: &[u8] = b"just some words, not a picture at all";
    let shot = png(500);
    let bad = StatusCode::BAD_REQUEST;
    let large = StatusCode::PAYLOAD_TOO_LARGE;

    // A part that is not what the request says.
    let one_shot = request(json!([screenshot(json!([]))]));
    fx.refused(
        &dir,
        &one_shot,
        &[("file-1", text), ("annotated-1", &shot)],
        (bad, "unsupported_file", "screenshot 1 is not a png file"),
    )
    .await;
    let one_shot = request(json!([screenshot(json!([]))]));
    fx.refused(
        &dir,
        &one_shot,
        &[("file-1", &shot), ("annotated-1", &jpeg())],
        (bad, "unsupported_file", "screenshot 1 is not a png file"),
    )
    .await;
    fx.refused(
        &dir,
        &request(json!([image("a.png")])),
        &[("file-1", text)],
        (
            bad,
            "unsupported_file",
            "attachment 1 is not a png, jpg, gif, webp or mp4 file",
        ),
    )
    .await;
    fx.refused(
        &dir,
        &request(json!([video("a.mp4")])),
        &[("file-1", &shot)],
        (
            bad,
            "unsupported_file",
            "attachment 1 is not a png, jpg, gif, webp or mp4 file",
        ),
    )
    .await;

    // Parts that do not belong, or come twice, or not at all.
    fx.refused(
        &dir,
        &request(json!([image("a.png")])),
        &[("../../x", &shot)],
        (bad, "invalid", "unexpected part ../../x"),
    )
    .await;
    fx.refused(
        &dir,
        &request(json!([image("a.png")])),
        &[("file-2", &shot)],
        (bad, "invalid", "unexpected part file-2"),
    )
    .await;
    // Cleaned (a tab is a space) and cut to 40 characters.
    let long_name = format!("x\t{}", "y".repeat(100));
    let shown = format!("unexpected part x {}", "y".repeat(38));
    fx.refused(
        &dir,
        &request(json!([])),
        &[(long_name.as_str(), &shot)],
        (bad, "invalid", &shown),
    )
    .await;
    fx.refused(
        &dir,
        &request(json!([image("a.png")])),
        &[("file-1", &shot), ("file-1", &shot)],
        (bad, "invalid", "part file-1 was sent twice"),
    )
    .await;
    fx.refused(
        &dir,
        &request(json!([screenshot(json!([]))])),
        &[("file-1", &shot)],
        (bad, "invalid", "attachment 1 was not sent"),
    )
    .await;
    let first = request(json!([image("a.png")]));
    let json = first.to_string();
    let (content_type, body) = multipart(&[("file-1", &shot), ("request", json.as_bytes())]);
    let (status, body) = fx.upload(&content_type, body).await;
    assert_eq!(
        (status, &body["error"], &body["message"]),
        (
            bad,
            &json!("invalid"),
            &json!("the request part must come first")
        ),
        "{body}"
    );
    fx.assert_nothing_created(&dir, &first["submission_id"], "request not first")
        .await;
    let (content_type, body) = multipart(&[]);
    let (status, body) = fx.upload(&content_type, body).await;
    assert_eq!(
        (status, &body["message"]),
        (bad, &json!("the request part must come first")),
        "{body}"
    );

    // The request itself.
    let eleven: Vec<Value> = (0..11).map(|i| image(&format!("{i}.png"))).collect();
    fx.refused(
        &dir,
        &request(json!(eleven)),
        &[],
        (bad, "invalid", "at most 10 attachments can be sent"),
    )
    .await;
    let mut outside = a_box("x");
    outside["x"] = json!(2.0);
    fx.refused(
        &dir,
        &request(json!([screenshot(json!([outside]))])),
        &[],
        (bad, "invalid", "attachment 1: a box is outside the image"),
    )
    .await;
    let mut past_the_edge = a_box("x");
    past_the_edge["x"] = json!(0.8);
    past_the_edge["w"] = json!(0.3);
    fx.refused(
        &dir,
        &request(json!([image("a.png"), screenshot(json!([past_the_edge]))])),
        &[],
        (bad, "invalid", "attachment 2: a box is outside the image"),
    )
    .await;
    let boxes: Vec<Value> = (0..21).map(|i| a_box(&i.to_string())).collect();
    fx.refused(
        &dir,
        &request(json!([screenshot(json!(boxes))])),
        &[],
        (bad, "invalid", "attachment 1 has more than 20 boxes"),
    )
    .await;
    fx.refused(
        &dir,
        &request(json!([screenshot(json!([a_box(&"é".repeat(501))]))])),
        &[],
        (
            bad,
            "invalid",
            "attachment 1: a comment is longer than 500 characters",
        ),
    )
    .await;
    let mut boxed_image = image("a.png");
    boxed_image["boxes"] = json!([a_box("x")]);
    fx.refused(
        &dir,
        &request(json!([boxed_image])),
        &[],
        (
            bad,
            "invalid",
            "attachment 1: only a screenshot can have boxes",
        ),
    )
    .await;
    let mut blank = request(json!([]));
    blank["main_idea"] = json!("  \n\t\n ");
    fx.refused(
        &dir,
        &blank,
        &[],
        (bad, "invalid", "the main idea is empty"),
    )
    .await;
    let mut long = request(json!([]));
    long["main_idea"] = json!(format!("Fix it\n{}", "a".repeat(20_000)));
    fx.refused(
        &dir,
        &long,
        &[],
        (
            bad,
            "invalid",
            "the main idea must be at most 20000 characters",
        ),
    )
    .await;
    let mut platform = request(json!([]));
    platform["platform"] = json!("mac os; rm");
    fx.refused(
        &dir,
        &platform,
        &[],
        (bad, "invalid", "the platform is not valid"),
    )
    .await;
    let mut version = request(json!([]));
    version["app_version"] = json!("");
    fx.refused(
        &dir,
        &version,
        &[],
        (bad, "invalid", "the app version is not valid"),
    )
    .await;
    let mut no_part = request(json!([]));
    no_part["idea"] = json!({"explainer": false, "diagrams": false, "demo_video": false});
    fx.refused(
        &dir,
        &no_part,
        &[],
        (
            bad,
            "invalid",
            "choose at least one part of the idea package",
        ),
    )
    .await;
    let mut idea_issue = request(json!([]));
    idea_issue["send_as"] = json!("github_issue");
    idea_issue["idea"] = json!({"explainer": true, "diagrams": false, "demo_video": false});
    fx.refused(
        &dir,
        &idea_issue,
        &[],
        (
            bad,
            "invalid",
            "an idea package needs an intent, not a GitHub issue",
        ),
    )
    .await;
    let mut not_a_request = request(json!([]));
    not_a_request["kind"] = json!("LEAK-kind");
    let (status, text) = {
        let json = not_a_request.to_string();
        let (content_type, body) = multipart(&[("request", json.as_bytes())]);
        fx.upload_text(&content_type, body).await
    };
    assert_eq!(status, bad, "{text}");
    assert!(
        text.contains("the request part is not a valid feedback request"),
        "{text}"
    );
    assert!(!text.contains("LEAK"), "{text}");
    let mut huge = request(json!([]));
    huge["platform"] = json!("x".repeat(1024 * 1024));
    fx.refused(
        &dir,
        &huge,
        &[],
        (
            large,
            "payload_too_large",
            "the request part is larger than 1 MiB",
        ),
    )
    .await;

    // Sizes, counted while the parts stream.
    fx.refused(
        &dir,
        &request(json!([image("a.png")])),
        &[("file-1", &png(1025))],
        (
            large,
            "payload_too_large",
            "attachment 1 is larger than 1024 bytes",
        ),
    )
    .await;
    fx.refused(
        &dir,
        &request(json!([screenshot(json!([]))])),
        &[("file-1", &png(1000)), ("annotated-1", &png(1025))],
        (
            large,
            "payload_too_large",
            "attachment 1 is larger than 1024 bytes",
        ),
    )
    .await;
    fx.refused(
        &dir,
        &request(json!([video("a.mp4")])),
        &[("file-1", &mp4(4097))],
        (
            large,
            "payload_too_large",
            "attachment 1 is larger than 4096 bytes",
        ),
    )
    .await;
    fx.refused(
        &dir,
        &request(json!([video("a.mp4"), video("b.mp4"), image("c.png")])),
        &[
            ("file-1", &mp4(4000)),
            ("file-2", &mp4(1500)),
            ("file-3", &png(1000)),
        ],
        (
            large,
            "payload_too_large",
            "the submission is larger than 6000 bytes",
        ),
    )
    .await;

    // Not a multipart body; no such repository.
    let (status, body) = fx.upload("application/json", "{}").await;
    assert_eq!(
        (status, &body["error"], &body["message"]),
        (
            bad,
            &json!("invalid"),
            &json!("the upload is not valid multipart form data")
        ),
        "{body}"
    );
    let (content_type, _) = multipart(&[]);
    let (status, body) = fx.upload(&content_type, "not multipart at all").await;
    assert_eq!(
        (status, &body["message"]),
        (bad, &json!("the upload is not valid multipart form data")),
        "{body}"
    );
    let mut unknown = request(json!([]));
    unknown["repo_id"] = json!(RepoId::new());
    let (status, body) = fx.submit(&unknown, &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "repo_not_found");
    fx.assert_nothing_created(&dir, &unknown["submission_id"], "unknown repository")
        .await;
}

#[tokio::test]
async fn a_name_from_the_client_is_never_a_path() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    let mut req = request_json(repo.id, "intent");
    req["attachments"] = json!([image("../../../outside.png\nsecond line")]);
    let json = req.to_string();
    let shot = png(200);
    let (content_type, body) = multipart_named(&[
        ("request", None, json.as_bytes()),
        ("file-1", Some("/etc/passwd"), &shot),
    ]);

    let (status, out) = fx.upload(&content_type, body).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    let tree = git_out(
        &repo.path,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            &format!("{BRANCH}:{FOLDER}"),
        ],
    );
    assert_eq!(
        tree.lines().collect::<Vec<_>>(),
        ["image-1.png", "issue.md"]
    );
    for name in ["outside.png", "outside.png\nsecond line", "passwd"] {
        assert_eq!(find_named(fx.tmp.path(), name), Vec::<PathBuf>::new());
    }
    assert!(fx.feedback_tmp_is_empty());
}

#[tokio::test]
async fn the_same_submission_twice_makes_one_branch_and_one_intent() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    let mut req = request_json(repo.id, "intent");
    req["attachments"] = json!([image("a.png")]);
    let shot = png(300);

    let (s1, first) = fx.submit(&req, &[("file-1", &shot)]).await;
    let (s2, second) = fx.submit(&req, &[("file-1", &shot)]).await;

    assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK), "{second}");
    assert_eq!(first, second);
    assert!(first["intent"]["id"].is_string(), "{first}");
    assert_eq!(
        git_out(&repo.path, &["branch", "--list", "feedback/*"])
            .lines()
            .count(),
        1
    );
    assert_eq!(fx.intents().await.len(), 1);

    // The same new submission twice at once.
    let mut again = request_json(repo.id, "intent");
    again["attachments"] = json!([image("a.png")]);
    let files: [(&str, &[u8]); 1] = [("file-1", &shot)];
    let (a, b) = tokio::join!(fx.submit(&again, &files), fx.submit(&again, &files));
    assert_eq!(a.0, StatusCode::OK, "{}", a.1);
    assert_eq!(a, b);
    assert_eq!(a.1["branch"], "feedback/2026-11-26-fix-the-zoom-2");
    assert_eq!(
        git_out(&repo.path, &["branch", "--list", "feedback/*"])
            .lines()
            .count(),
        2
    );
    assert_eq!(fx.intents().await.len(), 2);
    assert!(fx.feedback_tmp_is_empty());
}

#[tokio::test]
async fn a_failed_intent_step_keeps_the_commit_and_says_so() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    let mut req = request_json(repo.id, "intent");
    req["attachments"] = json!([image("a.png")]);
    let shot = png(300);
    fx.state
        .feedback
        .fail_next_intent_for_test(IntentError::Invalid("boom".into()));

    let (status, out) = fx.submit(&req, &[("file-1", &shot)]).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["branch"], BRANCH);
    let commit = git_out(&repo.path, &["rev-parse", &format!("refs/heads/{BRANCH}")]);
    assert_eq!(out["commit"], commit);
    assert_eq!(out["intent"], Value::Null);
    assert_eq!(
        out["failed"],
        json!({"step": "create_intent", "code": "invalid", "message": "boom"})
    );
    assert!(fx.intents().await.is_empty());

    // Submitted again (the hook is spent): the same commit, now with its intent.
    let (status, again) = fx.submit(&req, &[("file-1", &shot)]).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["branch"], BRANCH);
    assert_eq!(again["commit"], commit);
    assert_eq!(again["failed"], Value::Null);
    assert_eq!(again["intent"]["base_branch"], BRANCH);
    assert_eq!(
        git_out(
            &repo.path,
            &["rev-list", "--count", &format!("main..{BRANCH}")]
        ),
        "1"
    );
    assert_eq!(fx.intents().await.len(), 1);
}

#[tokio::test]
async fn a_renamed_default_branch_is_refused() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    common::git(&repo.path, &["branch", "-m", "main", "trunk"]);
    let req = request_json(repo.id, "intent");
    fx.refused(
        &repo.path,
        &req,
        &[],
        (
            StatusCode::CONFLICT,
            "default_branch_missing",
            "the repository's default branch main does not exist locally",
        ),
    )
    .await;
}

#[tokio::test]
async fn a_repository_without_a_remote_takes_the_intent_path() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    assert_eq!(git_out(&repo.path, &["remote"]), "");
    let req = request_json(repo.id, "intent");

    let (status, out) = fx.submit(&req, &[]).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["pushed"], false);
    assert!(out["intent"]["id"].is_string(), "{out}");
}

#[tokio::test]
async fn a_repository_git_cannot_read_is_refused_before_anything_is_made() {
    // W2-3: its folder is gone, or is no longer a git repository. Whatever
    // it is sent as, the answer is a 409 of its own, and nothing is made.
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    common::git(&repo.path, &["remote", "add", "origin", GITHUB_URL]);
    let aside = fx.tmp.path().join("aside");
    let unreadable = "git could not read this repository's settings. Check that its folder is still a git repository.";
    for (what, moved) in [
        ("its folder is gone", repo.path.clone()),
        ("not a git repository", repo.path.join(".git")),
    ] {
        std::fs::rename(&moved, &aside).unwrap();
        for send_as in ["intent", "idea", "github_issue"] {
            let mut req = request_json(repo.id, send_as);
            if send_as == "idea" {
                req["send_as"] = json!("intent");
                req["idea"] = json!({"explainer": true, "diagrams": false, "demo_video": false});
            }
            req["public_acknowledged"] = json!(true);

            let (status, body) = fx.submit(&req, &[]).await;

            assert_eq!(
                (status, body["error"].as_str(), body["message"].as_str()),
                (
                    StatusCode::CONFLICT,
                    Some("origin_unreadable"),
                    Some(unreadable)
                ),
                "{what}, {send_as}: {body}"
            );
            let id: SubmissionId = serde_json::from_value(req["submission_id"].clone()).unwrap();
            assert_eq!(
                fx.state.store.get_submission(id).await.unwrap(),
                None,
                "{what}, {send_as}: nothing recorded"
            );
        }
        std::fs::rename(&aside, &moved).unwrap();
        // Nothing reached the repository.
        assert_eq!(
            git_out(
                &repo.path,
                &["for-each-ref", "refs/heads/feedback", "refs/heads/idea"]
            ),
            "",
            "{what}"
        );
        assert!(fx.feedback_tmp_is_empty(), "{what}");
        assert!(fx.intents().await.is_empty(), "{what}");
        assert!(fx.fake.calls().is_empty(), "{what}: {:?}", fx.fake.calls());
    }
}

#[tokio::test]
async fn blocked_folders_and_branches_are_refused_with_their_sentences() {
    let fx = fixture().await;
    let a = fx.add_repo("a").await;
    std::fs::write(a.path.join("docs"), "a file, not a folder\n").unwrap();
    common::git(&a.path, &["add", "docs"]);
    common::git(&a.path, &["commit", "-m", "docs file"]);
    fx.refused(
        &a.path,
        &request_json(a.id, "intent"),
        &[],
        (
            StatusCode::CONFLICT,
            "folder_blocked",
            "docs is a file in this repository, so feedback cannot be stored there.",
        ),
    )
    .await;

    let b = fx.add_repo("b").await;
    common::git(&b.path, &["branch", "feedback"]);
    let req = request_json(b.id, "intent");
    let (status, body) = fx.submit(&req, &[]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "branch_blocked");
    assert_eq!(
        body["message"],
        "a branch named feedback exists in this repository, so no branch can be created under feedback/"
    );
    assert_eq!(git_out(&b.path, &["branch", "--list", "feedback/*"]), "");
    assert!(fx.feedback_tmp_is_empty());
    assert!(fx.intents().await.is_empty());
}

#[tokio::test]
async fn a_demo_video_needs_agent_browser() {
    let fx = fixture_with(Opts {
        agent_browser: false,
        ..Opts::default()
    })
    .await;
    let repo = fx.add_repo("r1").await;
    let mut req = request_json(repo.id, "intent");
    req["idea"] = json!({"explainer": false, "diagrams": false, "demo_video": true});
    fx.refused(
        &repo.path,
        &req,
        &[],
        (
            StatusCode::CONFLICT,
            "tool_missing",
            "agent-browser is not installed",
        ),
    )
    .await;

    // Without a demo video agent-browser is not needed.
    let mut req = request_json(repo.id, "intent");
    req["idea"] = json!({"explainer": true, "diagrams": false, "demo_video": false});
    let (status, out) = fx.submit(&req, &[]).await;
    assert_eq!(status, StatusCode::OK, "{out}");
}

#[tokio::test]
async fn leftovers_are_removed_at_start() {
    let fx = fixture().await;
    let upload = fx.paths.feedback_tmp_dir().join("x").join("upload");
    std::fs::create_dir_all(&upload).unwrap();
    std::fs::write(upload.join("file-1"), b"left by a crashed run").unwrap();
    std::fs::write(fx.paths.feedback_tmp_dir().join("stray"), b"x").unwrap();

    fx.state.feedback.remove_leftovers().await;

    assert!(fx.paths.feedback_tmp_dir().is_dir());
    assert!(fx.feedback_tmp_is_empty());
}

#[tokio::test]
async fn the_routes_need_the_token() {
    let fx = fixture().await;
    let (status, _) = fx
        .call_with(Method::GET, "/v1/feedback/targets", None, None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (content_type, body) = multipart(&[]);
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/feedback")
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .unwrap();
    let (status, _) = fx.send(req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ------------------------------------------- streaming, drops and cancellation

/// A body that yields `chunks` and then stays open, never ending, until the
/// returned sender is dropped.
fn open_body(
    chunks: Vec<Vec<u8>>,
) -> (
    Body,
    tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(chunks.len() + 1);
    for chunk in chunks {
        tx.try_send(Ok(Bytes::from(chunk))).unwrap();
    }
    let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    (body, tx)
}

/// The start of a multipart body: the request part, then the head of a
/// part named `part`.
fn upload_head(request: &Value, part: &str) -> (String, Vec<u8>) {
    let json = request.to_string();
    let (content_type, mut body) = multipart(&[("request", json.as_bytes())]);
    let closing = format!("--{}--\r\n", common::feedback::BOUNDARY);
    body.truncate(body.len() - closing.len());
    body.extend_from_slice(
        format!(
            "--{}\r\nContent-Disposition: form-data; name=\"{part}\"; filename=\"{part}\"\r\n\r\n",
            common::feedback::BOUNDARY
        )
        .as_bytes(),
    );
    (content_type, body)
}

#[tokio::test]
async fn an_oversized_part_is_refused_without_reading_the_rest() {
    let fx = fixture_with(Opts {
        limits: UploadLimits {
            image_bytes: 1024,
            ..UploadLimits::default()
        },
        ..Opts::default()
    })
    .await;
    let repo = fx.add_repo("r1").await;
    let mut req = request_json(repo.id, "intent");
    req["attachments"] = json!([image("a.png")]);
    let (content_type, head) = upload_head(&req, "file-1");
    // Past the limit, and then the body never ends.
    let (body, _open) = open_body(vec![head, png(2048)]);

    let answer = tokio::time::timeout(PATIENCE, fx.upload(&content_type, body))
        .await
        .expect("the daemon waited for the rest of the body");

    let (status, body) = answer;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(body["message"], "attachment 1 is larger than 1024 bytes");
    fx.assert_nothing_created(&repo.path, &req["submission_id"], "oversized")
        .await;
}

/// Polls `done` every few milliseconds until it holds, for at most PATIENCE.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn a_request_dropped_during_the_upload_leaves_nothing() {
    let fx = Arc::new(fixture().await);
    let repo = fx.add_repo("r1").await;
    let mut req = request_json(repo.id, "intent");
    req["attachments"] = json!([image("a.png")]);
    let (content_type, head) = upload_head(&req, "file-1");
    let (body, open) = open_body(vec![head, png(500)]);
    let call = {
        let fx = Arc::clone(&fx);
        tokio::spawn(async move { fx.upload(&content_type, body).await })
    };

    // The part is being written when the client goes away.
    let tmp = fx.paths.feedback_tmp_dir();
    until("the upload never reached the disk", || {
        find_named(&tmp, "file-1")
            .iter()
            .any(|f| f.metadata().is_ok_and(|m| m.len() > 0))
    })
    .await;
    call.abort();
    let _ = call.await;
    drop(open);

    until("the staged files stayed", || fx.feedback_tmp_is_empty()).await;
    fx.assert_nothing_created(&repo.path, &req["submission_id"], "dropped upload")
        .await;
}

#[tokio::test]
async fn a_request_dropped_once_the_commit_began_finishes_whole() {
    let fx = Arc::new(fixture().await);
    let repo = fx.add_repo("r1").await;
    let mut req = request_json(repo.id, "intent");
    req["attachments"] = json!([image("a.png")]);
    let shot = png(500);
    let json = req.to_string();
    let (content_type, body) = multipart(&[("request", json.as_bytes()), ("file-1", &shot)]);
    let (body, open) = open_body(vec![body]);
    drop(open);
    let call = {
        let fx = Arc::clone(&fx);
        tokio::spawn(async move { fx.upload(&content_type, body).await })
    };

    // Gone once the commit has begun (or, at the latest, once it is made).
    let tmp = fx.paths.feedback_tmp_dir();
    let dir = repo.path.clone();
    until("the commit never began", || {
        !find_named(&tmp, "wt").is_empty()
            || !git_out(&dir, &["branch", "--list", "feedback/*"]).is_empty()
    })
    .await;
    call.abort();
    let _ = call.await;

    // The submission runs on: a branch, its record and its intent, together.
    let sid: SubmissionId = serde_json::from_value(req["submission_id"].clone()).unwrap();
    let deadline = Instant::now() + PATIENCE;
    let row = loop {
        let row = fx.state.store.get_submission(sid).await.unwrap();
        if row.as_ref().is_some_and(|r| r.intent_id.is_some()) {
            break row.unwrap();
        }
        assert!(Instant::now() < deadline, "the submission did not finish");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(
        git_out(&dir, &["rev-parse", &format!("refs/heads/{}", row.branch)]),
        row.commit
    );
    assert_eq!(
        git_out(&dir, &["branch", "--list", "feedback/*"])
            .lines()
            .count(),
        1
    );
    assert_eq!(fx.intents().await.len(), 1);
    until("the staged files stayed", || fx.feedback_tmp_is_empty()).await;
}

// ------------------------------------------------------- intents with evidence

#[tokio::test]
async fn the_repository_of_an_intent_with_evidence_cannot_change() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    let other = fx.add_repo("r2").await;
    let (status, out) = fx.submit(&request_json(repo.id, "intent"), &[]).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let id: IntentId = serde_json::from_value(out["intent"]["id"].clone()).unwrap();
    // Polish has finished (the fixture never polishes).
    let mut intent = fx.state.store.get_intent(id).await.unwrap().unwrap();
    intent.phase = Phase::Review;
    fx.state.store.save_intent(&intent).await.unwrap();

    let (status, body) = fx
        .call(
            Method::PATCH,
            &format!("/v1/intents/{id}"),
            Some(json!({"repo_id": other.id, "title": "Moved"})),
        )
        .await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "evidence_in_repo");
    assert_eq!(
        body["message"],
        "This intent's evidence is in its repository, so the repository cannot be changed."
    );
    let now = fx.state.store.get_intent(id).await.unwrap().unwrap();
    assert_eq!(now, intent);

    // Its own repository, or no repository at all, is not a change.
    let (status, body) = fx
        .call(
            Method::PATCH,
            &format!("/v1/intents/{id}"),
            Some(json!({"repo_id": repo.id, "title": "Renamed"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["content"]["title"], "Renamed");
    assert_eq!(body["repo_id"], json!(repo.id));
}

// ------------------------------------------------------------------- leaking

#[tokio::test]
async fn nothing_uploaded_or_typed_reaches_a_log_and_no_file_reaches_an_event() {
    let logs = logs();
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    let mut req = request_json(repo.id, "intent");
    req["main_idea"] = json!("USERTEXT-title\n\nUSERTEXT-body");
    req["attachments"] = json!([
        screenshot(json!([a_box("USERTEXT-comment")])),
        image("USERTEXT-name.png"),
    ]);
    let mut shot = png(200);
    shot.extend_from_slice(b"LEAK-bytes");
    let mut photo = jpeg();
    photo.extend_from_slice(b"LEAK-bytes");
    let files: [(&str, &[u8]); 3] = [
        ("file-1", &shot),
        ("annotated-1", &shot),
        ("file-2", &photo),
    ];
    let mut bodies = Vec::new();

    // A submission whose intent step fails, then its retry.
    fx.state
        .feedback
        .fail_next_intent_for_test(IntentError::Invalid("could not".into()));
    for _ in 0..2 {
        let (status, out) = fx.submit(&req, &files).await;
        assert_eq!(status, StatusCode::OK, "{out}");
        bodies.push(out.to_string());
    }
    // Refusals: a wrong part type, too large, a git failure.
    let mut wrong = req.clone();
    wrong["submission_id"] = json!(SubmissionId::new());
    let (_, out) = fx
        .submit(&wrong, &[("file-1", &photo), ("annotated-1", &shot)])
        .await;
    bodies.push(out.to_string());
    common::git(&repo.path, &["branch", "-m", "main", "trunk"]);
    let mut gone = req.clone();
    gone["submission_id"] = json!(SubmissionId::new());
    let (_, out) = fx.submit(&gone, &files).await;
    assert_eq!(out["error"], "default_branch_missing", "{out}");
    bodies.push(out.to_string());

    let events: Vec<String> = fx
        .state
        .bus
        .subscribe(Some(0))
        .unwrap()
        .backlog
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    assert!(!events.is_empty());
    let logged = logs.text();
    // The log is captured: it names the submission and its steps.
    let id = req["submission_id"].as_str().unwrap();
    assert!(
        logged.contains(&format!("submission_id={id} step=\"commit\"")),
        "{logged}"
    );
    assert!(logged.contains("code=\"invalid\""), "{logged}");
    for text in events.iter().chain(&bodies).chain([&logged]) {
        assert!(!text.contains("LEAK"), "{text}");
    }
    assert!(!logged.contains("USERTEXT"), "{logged}");
}

// ------------------------------------------- fix round 1: never half-made

/// `n` commits on `branch` above `main`.
fn commits_above_main(dir: &Path, branch: &str) -> String {
    git_out(dir, &["rev-list", "--count", &format!("main..{branch}")])
}

#[tokio::test]
async fn a_record_that_cannot_be_stored_takes_its_branch_back() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    let mut req = request_json(repo.id, "intent");
    req["attachments"] = json!([image("a.png")]);
    let shot = png(300);
    fx.state.feedback.fail_next_insert_for_test();

    let (status, body) = fx.submit(&req, &[("file-1", &shot)]).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "store_error", "{body}");
    fx.assert_nothing_created(&repo.path, &req["submission_id"], "record failed")
        .await;

    // Sent again: the same name is free, and there is one commit.
    let (status, out) = fx.submit(&req, &[("file-1", &shot)]).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["branch"], BRANCH);
    assert_eq!(commits_above_main(&repo.path, BRANCH), "1");
    assert_eq!(fx.intents().await.len(), 1);
}

#[tokio::test]
async fn an_intent_whose_record_cannot_be_saved_is_still_the_answer() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;

    // One failed save is retried.
    let req = request_json(repo.id, "intent");
    fx.state.feedback.fail_next_saves_for_test(1);
    let (status, out) = fx.submit(&req, &[]).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let id: IntentId = serde_json::from_value(out["intent"]["id"].clone()).unwrap();
    let sid: SubmissionId = serde_json::from_value(req["submission_id"].clone()).unwrap();
    let row = fx.state.store.get_submission(sid).await.unwrap().unwrap();
    assert_eq!(row.intent_id, Some(id));

    // Two failed saves: the intent that was made is the answer, not a 500.
    let mut req = request_json(repo.id, "intent");
    req["main_idea"] = json!("Another idea");
    fx.state.feedback.fail_next_saves_for_test(2);
    let (status, out) = fx.submit(&req, &[]).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["failed"], Value::Null);
    let id: IntentId = serde_json::from_value(out["intent"]["id"].clone()).unwrap();
    let stored = fx.state.store.get_intent(id).await.unwrap().unwrap();
    assert_eq!(serde_json::to_value(&stored).unwrap(), out["intent"]);
    assert_eq!(fx.intents().await.len(), 2);
}

/// What a daemon stopped between its commit and its record leaves: the
/// submission's commit on its branch, with no row.
async fn commit_without_record(fx: &Fx, repo: &Repo, id: &Value) -> String {
    let id: SubmissionId = serde_json::from_value(id.clone()).unwrap();
    let files = [FolderFile {
        name: "issue.md".into(),
        source: FileSource::Text("# Fix the zoom\n".into()),
    }];
    let scratch = fx.paths.feedback_tmp_dir().join("stopped-run");
    let date = now().date_naive();
    let (placement, commit) =
        place_and_commit(repo, "Fix the zoom", date, false, &files, id, &scratch)
            .await
            .unwrap();
    assert_eq!(placement.branch, BRANCH);
    std::fs::remove_dir_all(&scratch).unwrap();
    assert_eq!(fx.state.store.get_submission(id).await.unwrap(), None);
    commit
}

#[tokio::test]
async fn a_commit_left_by_a_stopped_daemon_is_adopted() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    let mut req = request_json(repo.id, "intent");
    req["attachments"] = json!([image("a.png")]);
    let commit = commit_without_record(&fx, &repo, &req["submission_id"]).await;

    let (status, out) = fx.submit(&req, &[("file-1", &png(300))]).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["branch"], BRANCH);
    assert_eq!(out["folder"], FOLDER);
    assert_eq!(out["commit"], commit);
    assert_eq!(out["failed"], Value::Null);
    assert_eq!(out["intent"]["origin"]["commit"], commit);
    // No second branch and no second commit: the folder is as first committed.
    assert_eq!(
        git_out(&repo.path, &["branch", "--list", "feedback/*"]),
        BRANCH
    );
    assert_eq!(commits_above_main(&repo.path, BRANCH), "1");
    let tree = git_out(
        &repo.path,
        &["ls-tree", "--name-only", &format!("{BRANCH}:{FOLDER}")],
    );
    assert_eq!(tree, "issue.md");
    assert_eq!(fx.intents().await.len(), 1);
    let sid: SubmissionId = serde_json::from_value(req["submission_id"].clone()).unwrap();
    let row = fx.state.store.get_submission(sid).await.unwrap().unwrap();
    assert_eq!(
        (row.branch.as_str(), row.commit.as_str()),
        (BRANCH, commit.as_str())
    );
    assert!(fx.feedback_tmp_is_empty());

    // And the next time it is the recorded submission.
    let (status, again) = fx.submit(&req, &[("file-1", &png(300))]).await;
    assert_eq!((status, &again), (StatusCode::OK, &out));
}

/// A branch `branch` from `main` with one commit whose message is
/// `message`, adding `docs/issues/<name>/issue.md` as Submit would.
fn decoy(dir: &Path, branch: &str, name: &str, message: &str) -> String {
    common::git(dir, &["switch", "-q", "-c", branch, "main"]);
    let folder = dir.join("docs/issues").join(name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("issue.md"), "# A decoy\n").unwrap();
    common::git(dir, &["add", "docs"]);
    common::git(
        dir,
        &["commit", "-q", "--cleanup=whitespace", "-m", message],
    );
    common::git(dir, &["switch", "-q", "main"]);
    git_out(dir, &["rev-parse", &format!("refs/heads/{branch}")])
}

#[tokio::test]
async fn only_this_submissions_trailer_is_adopted() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    let dir = repo.path.clone();
    let req = request_json(repo.id, "intent");
    let id = req["submission_id"].as_str().unwrap().to_string();
    let other = SubmissionId::new().to_string();
    let decoys = [
        // The id in the title, and in the body but not as the trailer.
        (
            "feedback/2026-11-26-title",
            format!("docs(issues): Feedback-Id: {id}\n\nmentions {id}\nFeedback-Id: {id} too"),
        ),
        // A trailer whose value only starts with the id.
        (
            "feedback/2026-11-26-longer",
            format!("docs(issues): x\n\nFeedback-Id: {id}-2"),
        ),
        // Another submission's trailer.
        (
            "idea/other",
            format!("docs(issues): x\n\nFeedback-Id: {other}"),
        ),
        // The right trailer, on a branch Submit never makes.
        ("work/x", format!("docs(issues): x\n\nFeedback-Id: {id}")),
    ];
    let mut tips = Vec::new();
    for (i, (branch, message)) in decoys.iter().enumerate() {
        tips.push(decoy(&dir, branch, &format!("decoy-{i}"), message));
    }

    let (status, out) = fx.submit(&req, &[]).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["branch"], BRANCH, "{out}");
    assert!(!tips.contains(&out["commit"].as_str().unwrap().to_string()));
    assert_eq!(commits_above_main(&dir, BRANCH), "1");
    let message = git_out(&dir, &["log", "-1", "--format=%B", BRANCH]);
    assert_eq!(
        message,
        format!("docs(issues): Fix the zoom\n\nFeedback-Id: {id}")
    );
    // The decoys are left as they were.
    for ((branch, _), tip) in decoys.iter().zip(&tips) {
        assert_eq!(
            &git_out(&dir, &["rev-parse", &format!("refs/heads/{branch}")]),
            tip
        );
    }
}

#[tokio::test]
async fn a_maximal_request_in_a_three_byte_script_is_accepted() {
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    // 漢 is three bytes in UTF-8: 20,000 of them, and 20 comments of 500 on
    // each of ten screenshots, with quotes to escape.
    let comment = format!("\"{}\"", "漢".repeat(498));
    let boxes: Vec<Value> = (0..20).map(|_| a_box(&comment)).collect();
    let mut req = request_json(repo.id, "intent");
    req["main_idea"] = json!(format!("漢{}", "\"漢".repeat(9_999)));
    req["attachments"] = json!((0..10)
        .map(|_| screenshot(json!(boxes)))
        .collect::<Vec<_>>());
    let json = req.to_string();
    assert!(json.len() > 256 * 1024, "{}", json.len());
    assert_eq!(req["main_idea"].as_str().unwrap().chars().count(), 19_999);
    let shot = png(64);
    let names: Vec<String> = (1..=10)
        .flat_map(|i| [format!("file-{i}"), format!("annotated-{i}")])
        .collect();
    let files: Vec<(&str, &[u8])> = names
        .iter()
        .map(|n| (n.as_str(), shot.as_slice()))
        .collect();

    let (status, out) = fx.submit(&req, &files).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    assert!(out["intent"]["id"].is_string(), "{out}");
}

#[tokio::test]
async fn an_intent_made_but_not_recorded_is_taken_up_not_made_again() {
    // D-M6: both saves of the intent's id failed; the retry finds it by its
    // origin.
    let fx = fixture().await;
    let repo = fx.add_repo("r1").await;
    let req = request_json(repo.id, "intent");
    fx.state.feedback.fail_next_saves_for_test(2);
    let (status, out) = fx.submit(&req, &[]).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let made = out["intent"]["id"].clone();
    let sid: SubmissionId = serde_json::from_value(req["submission_id"].clone()).unwrap();
    let row = fx.state.store.get_submission(sid).await.unwrap().unwrap();
    assert_eq!(row.intent_id, None, "not recorded");

    let (status, again) = fx
        .call(Method::POST, &format!("/v1/feedback/{sid}/retry"), None)
        .await;

    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["intent"]["id"], made, "the same intent");
    assert_eq!(fx.intents().await.len(), 1);
    let row = fx.state.store.get_submission(sid).await.unwrap().unwrap();
    assert_eq!(json!(row.intent_id), made, "recorded now");
}

//! The idea package: a submission with `idea` set is committed on an
//! `idea/<slug>` branch and becomes an idea intent whose criteria the daemon
//! fixes, which polish and review keep.
//!
//! Repositories are temp directories with their own neutral identity and no
//! remote. `gh` and `agent-browser` are the fakes of `tests/bin` on the
//! injected finder only. Polish runs against the fake harness (scripted
//! answers, nothing is spent) or not at all.
#![cfg(unix)]

mod common;

use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use common::feedback::{multipart, png, request_json, FakeTools};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use smithagents_core::agent::Repo;
use smithagents_core::feedback::{IdeaOptions, IdeaPackage, SubmissionId};
use smithagents_core::ids::IntentId;
use smithagents_core::intent::{Intent, IntentFilter, ItemStatus, Phase};
use smithagents_plugin_host::StaticSecrets;
use smithagents_server::api::{router, AppState};
use smithagents_server::events::EventBus;
use smithagents_server::feedback::idea::{
    fixed_criteria, CRITERION_DEMO, CRITERION_DIAGRAMS, CRITERION_EXPLAINER, CRITERION_SCOPE,
};
use smithagents_server::feedback::FeedbackService;
use smithagents_server::harness::fake::FakeHarness;
use smithagents_server::harness::HarnessError;
use smithagents_server::intents::{IntentError, IntentService, PolishRunner};
use smithagents_server::paths::Paths;
use smithagents_server::plugins::{embedded_plugins, ManagerConfig, PluginManager};
use smithagents_server::service::AgentService;
use smithagents_server::store::Store;
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "test-token-123";
const BRANCH: &str = "idea/fix-the-zoom";
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

struct Fx {
    tmp: TempDir,
    state: AppState,
    harness: FakeHarness,
    fake: FakeTools,
}

/// The daemon's services over an in-memory store. `polish`: the intent
/// service runs its passes against the fake harness; else they never run.
async fn fixture(polish: bool) -> Fx {
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
    let mut intents = IntentService::new(
        store.clone(),
        bus.clone(),
        Arc::new(harness.clone()),
        service.clone(),
        paths.clone(),
    );
    if !polish {
        intents = intents.with_polish_runner(Arc::new(NoPolish));
    }
    let fake = FakeTools::new(&tmp.path().join("fakes"), true, true);
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
        paths,
    )
    .with_clock(Arc::new(now));
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
        harness,
        fake,
    }
}

impl Fx {
    async fn send(&self, req: Request<Body>) -> (StatusCode, Value) {
        let resp = router(self.state.clone()).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
            .unwrap();
        self.send(req).await
    }

    /// `POST /v1/feedback` with `request` and one screenshot-free image.
    async fn submit(&self, request: &Value) -> (StatusCode, Value) {
        let json = request.to_string();
        let shot = png(300);
        let (content_type, body) =
            multipart(&[("request", json.as_bytes()), ("file-1", shot.as_slice())]);
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/feedback")
            .header(header::CONTENT_TYPE, content_type)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::from(body))
            .unwrap();
        self.send(req).await
    }

    /// A repository on `main` with one commit and its own identity, added
    /// to the daemon.
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

    async fn intents(&self) -> Vec<Intent> {
        self.state
            .intents
            .list(IntentFilter::default())
            .await
            .unwrap()
    }

    /// Polls the store until `pred` holds for the intent.
    async fn wait(&self, id: IntentId, pred: impl Fn(&Intent) -> bool) -> Intent {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let i = self.state.intents.get(id).await.unwrap();
            if pred(&i) {
                return i;
            }
            assert!(Instant::now() < deadline, "timed out; the intent is {i:?}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

/// An intent request with an idea package of these parts, for an image.
fn idea_request(repo: &Repo, explainer: bool, diagrams: bool, demo_video: bool) -> Value {
    let mut req = request_json(repo.id, "intent");
    req["attachments"] = json!([{"kind": "image", "original_name": "a.png"}]);
    req["idea"] = json!({"explainer": explainer, "diagrams": diagrams, "demo_video": demo_video});
    req
}

fn git_out(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("LC_ALL", "C")
        .env("GIT_OPTIONAL_LOCKS", "0")
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

fn branches(dir: &Path, pattern: &str) -> Vec<String> {
    git_out(
        dir,
        &["branch", "--list", "--format=%(refname:short)", pattern],
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

/// The intent's criteria as (text, status, fixed).
fn criteria(intent: &Intent) -> Vec<(String, ItemStatus, bool)> {
    intent
        .content
        .criteria
        .iter()
        .map(|i| (i.text.clone(), i.status, i.fixed))
        .collect()
}

fn fixed_accepted(texts: &[&str]) -> Vec<(String, ItemStatus, bool)> {
    texts
        .iter()
        .map(|t| ((*t).to_owned(), ItemStatus::Accepted, true))
        .collect()
}

/// Everything this test binary logs at info and above.
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

// ------------------------------------------------------------------ tests

#[test]
fn fixed_criteria_follow_the_ticked_parts() {
    let all = IdeaOptions {
        explainer: true,
        diagrams: true,
        demo_video: true,
    };
    assert_eq!(
        fixed_criteria(all),
        [
            CRITERION_EXPLAINER,
            CRITERION_DIAGRAMS,
            CRITERION_DEMO,
            CRITERION_SCOPE
        ]
    );
    let demo = IdeaOptions {
        explainer: false,
        diagrams: false,
        demo_video: true,
    };
    assert_eq!(fixed_criteria(demo), [CRITERION_DEMO, CRITERION_SCOPE]);
    let explainer = IdeaOptions {
        explainer: true,
        diagrams: false,
        demo_video: false,
    };
    assert_eq!(
        fixed_criteria(explainer),
        [CRITERION_EXPLAINER, CRITERION_SCOPE]
    );

    // The sentences, verbatim.
    assert_eq!(
        CRITERION_EXPLAINER,
        "`explainer.md` exists in the issue folder and covers what, why, how it could work and open questions."
    );
    assert_eq!(
        CRITERION_DIAGRAMS,
        "`explainer.md` has at least one Mermaid diagram of the idea."
    );
    assert_eq!(
        CRITERION_DEMO,
        "`demo.mp4` (at most 50 MB) shows the bug or the idea in a browser, and `demo.md` lists the steps taken."
    );
    assert_eq!(
        CRITERION_SCOPE,
        "Only files inside the issue folder are changed, and nothing is implemented."
    );
}

#[tokio::test]
async fn an_idea_submission_makes_an_idea_intent() {
    let fx = fixture(true).await;
    let repo = fx.add_repo("r1").await;
    fx.harness.script_structured(vec![
        Ok(json!({
            "title": "Fix the zoom",
            "kind": "bug",
            "current_behavior": "The page is drawn at 200%.",
            "questions": [{"text": "Which pages?", "options": [{"label": "All"}, {"label": "Settings"}]}],
        })),
        Ok(json!({
            "preconditions": [],
            // One repeats a fixed criterion: it is not stored twice.
            "acceptance_criteria": ["The page fits the window", CRITERION_SCOPE, "The zoom is remembered"],
            "stories": [],
            "depends_on": [],
        })),
    ]);

    let (status, out) = fx.submit(&idea_request(&repo, true, true, false)).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["branch"], BRANCH);
    assert_eq!(out["folder"], FOLDER);
    assert_eq!(out["failed"], Value::Null);
    assert_eq!(
        out["commit"],
        git_out(&repo.path, &["rev-parse", &format!("refs/heads/{BRANCH}")])
    );
    assert_eq!(branches(&repo.path, "feedback/*"), Vec::<String>::new());
    assert_eq!(branches(&repo.path, "idea/*"), [BRANCH]);
    assert_eq!(
        git_out(
            &repo.path,
            &["rev-list", "--count", &format!("main..{BRANCH}")]
        ),
        "1"
    );
    let created: Intent = serde_json::from_value(out["intent"].clone()).unwrap();
    assert_eq!(created.base_branch.as_deref(), Some(BRANCH));
    let origin = created.origin.clone().expect("an origin");
    assert_eq!(
        (origin.folder.as_str(), origin.branch.as_str()),
        (FOLDER, BRANCH)
    );
    assert_eq!(
        origin.idea,
        Some(IdeaPackage {
            explainer: true,
            diagrams: true,
            demo_video: false,
            pr_url: None,
            not_done: None,
            implementation_intent_id: None,
        })
    );
    let fixed = [CRITERION_EXPLAINER, CRITERION_DIAGRAMS, CRITERION_SCOPE];
    assert_eq!(criteria(&created), fixed_accepted(&fixed));
    assert!(created.content.criteria.iter().all(|i| !i.edited));
    // The idea path runs no gh.
    assert!(fx.fake.calls().is_empty(), "{:?}", fx.fake.calls());

    // Pass 1 asks its question and leaves the criteria alone.
    let asked = fx.wait(created.id, |i| i.phase == Phase::Questions).await;
    assert_eq!(asked.content.criteria, created.content.criteria);
    assert_eq!(asked.content.questions.len(), 1);
    let question = asked.content.questions[0].id;
    let (status, body) = fx
        .call(
            Method::POST,
            &format!("/v1/intents/{}/questions/{question}/answer", created.id),
            Some(json!({"option_id": "o1"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Pass 2 adds its criteria after the fixed ones.
    let reviewed = fx.wait(created.id, |i| i.phase == Phase::Review).await;
    let mut expected = fixed_accepted(&fixed);
    expected.push((
        "The page fits the window".into(),
        ItemStatus::Pending,
        false,
    ));
    expected.push(("The zoom is remembered".into(), ItemStatus::Pending, false));
    assert_eq!(criteria(&reviewed), expected);
    let ids = |i: &Intent| i.content.criteria.iter().map(|c| c.id).collect::<Vec<_>>();
    assert_eq!(ids(&reviewed)[..3], ids(&created)[..]);
    assert_eq!(reviewed.content.questions.len(), 1);
    assert_eq!(reviewed.content.questions[0].text, "Which pages?");
    assert_eq!(reviewed.origin, created.origin);

    // The user's text went to the model only inside its fence.
    let calls = fx.harness.structured_calls();
    assert_eq!(calls.len(), 2);
    for call in &calls {
        let parts: Vec<&str> = call.prompt.split("=====").collect();
        let inside = parts
            .iter()
            .position(|p| p.contains("BEGIN UNTRUSTED USER CONTENT"))
            .expect("a fence");
        for (at, part) in parts.iter().enumerate() {
            if at != inside + 1 {
                assert!(!part.contains("The page is too big."), "{}", call.prompt);
            }
        }
    }
}

#[tokio::test]
async fn an_idea_needs_a_part_and_an_intent() {
    let fx = fixture(false).await;
    let repo = fx.add_repo("r1").await;
    for (request, message) in [
        (
            idea_request(&repo, false, false, false),
            "choose at least one part of the idea package",
        ),
        (
            {
                let mut req = idea_request(&repo, true, false, false);
                req["send_as"] = json!("github_issue");
                req
            },
            "an idea package needs an intent, not a GitHub issue",
        ),
    ] {
        let (status, body) = fx.submit(&request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "invalid", "{body}");
        assert_eq!(body["message"], message, "{body}");
        assert_eq!(branches(&repo.path, "idea/*"), Vec::<String>::new());
        assert_eq!(branches(&repo.path, "feedback/*"), Vec::<String>::new());
        let id: SubmissionId = serde_json::from_value(request["submission_id"].clone()).unwrap();
        assert_eq!(fx.state.store.get_submission(id).await.unwrap(), None);
        assert!(fx.intents().await.is_empty());
    }
}

#[tokio::test]
async fn the_same_idea_twice_makes_one_branch() {
    // Review Focus 5.
    let fx = fixture(false).await;
    let repo = fx.add_repo("r1").await;
    let req = idea_request(&repo, true, false, false);

    let (s1, first) = fx.submit(&req).await;
    let (s2, second) = fx.submit(&req).await;

    assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK), "{second}");
    assert_eq!(first, second);
    assert_eq!(first["branch"], BRANCH);
    assert_eq!(branches(&repo.path, "idea/*"), [BRANCH]);
    assert_eq!(fx.intents().await.len(), 1);

    // The same new idea twice at once.
    let again = idea_request(&repo, true, false, false);
    let (a, b) = tokio::join!(fx.submit(&again), fx.submit(&again));
    assert_eq!(a.0, StatusCode::OK, "{}", a.1);
    assert_eq!(a, b);
    assert_eq!(a.1["branch"], "idea/fix-the-zoom-2");
    assert_eq!(
        branches(&repo.path, "idea/*"),
        [BRANCH, "idea/fix-the-zoom-2"]
    );
    assert_eq!(fx.intents().await.len(), 2);
}

#[tokio::test]
async fn a_failed_idea_intent_step_can_be_retried() {
    // Review Focus 2.
    let fx = fixture(false).await;
    let repo = fx.add_repo("r1").await;
    let req = idea_request(&repo, false, true, false);
    fx.state
        .feedback
        .fail_next_intent_for_test(IntentError::Invalid("boom".into()));

    let (status, out) = fx.submit(&req).await;

    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["branch"], BRANCH);
    assert_eq!(out["intent"], Value::Null);
    assert_eq!(
        out["failed"],
        json!({"step": "create_intent", "code": "invalid", "message": "boom"})
    );
    assert!(fx.intents().await.is_empty());

    let id = req["submission_id"].as_str().unwrap();
    let (status, again) = fx
        .call(Method::POST, &format!("/v1/feedback/{id}/retry"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["failed"], Value::Null);
    assert_eq!(again["commit"], out["commit"]);
    let intent: Intent = serde_json::from_value(again["intent"].clone()).unwrap();
    assert_eq!(
        criteria(&intent),
        fixed_accepted(&[CRITERION_DIAGRAMS, CRITERION_SCOPE])
    );
    assert!(intent
        .origin
        .and_then(|o| o.idea)
        .is_some_and(|i| i.diagrams));
    assert_eq!(intent.base_branch.as_deref(), Some(BRANCH));
    assert_eq!(
        git_out(
            &repo.path,
            &["rev-list", "--count", &format!("main..{BRANCH}")]
        ),
        "1"
    );
    assert_eq!(branches(&repo.path, "idea/*"), [BRANCH]);
    assert_eq!(fx.intents().await.len(), 1);
}

#[tokio::test]
async fn a_failed_pass_two_retried_keeps_the_fixed_criteria() {
    let fx = fixture(true).await;
    let repo = fx.add_repo("r1").await;
    fx.harness.script_structured(vec![
        Ok(json!({"title": "Fix the zoom", "kind": "bug", "current_behavior": "", "questions": []})),
        Err(HarnessError::Timeout),
        Ok(json!({
            // The model drops one fixed criterion, rewords another and
            // repeats the third.
            "acceptance_criteria": [CRITERION_SCOPE, "`explainer.md` exists.", CRITERION_SCOPE],
            "preconditions": [], "stories": [], "depends_on": [],
        })),
    ]);
    let (status, out) = fx.submit(&idea_request(&repo, true, true, false)).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let id: IntentId = serde_json::from_value(out["intent"]["id"].clone()).unwrap();
    let fixed = [CRITERION_EXPLAINER, CRITERION_DIAGRAMS, CRITERION_SCOPE];

    let failed = fx
        .wait(id, |i| matches!(i.phase, Phase::Failed { pass: 2, .. }))
        .await;
    assert_eq!(criteria(&failed), fixed_accepted(&fixed));

    let (status, body) = fx
        .call(
            Method::POST,
            &format!("/v1/intents/{id}/polish/retry"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let reviewed = fx.wait(id, |i| i.phase == Phase::Review).await;
    let mut expected = fixed_accepted(&fixed);
    expected.push(("`explainer.md` exists.".into(), ItemStatus::Pending, false));
    assert_eq!(criteria(&reviewed), expected);
    assert_eq!(
        reviewed.content.criteria[..3],
        failed.content.criteria[..],
        "same ids"
    );
}

#[tokio::test]
async fn the_review_routes_cannot_change_a_fixed_criterion() {
    let fx = fixture(true).await;
    let repo = fx.add_repo("r1").await;
    fx.harness.script_structured(vec![
        Ok(json!({"title": "Fix the zoom", "kind": "bug", "current_behavior": "", "questions": []})),
        Ok(json!({
            "acceptance_criteria": ["The page fits the window"],
            "preconditions": [], "stories": [], "depends_on": [],
        })),
    ]);
    let (status, out) = fx.submit(&idea_request(&repo, true, false, false)).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let id: IntentId = serde_json::from_value(out["intent"]["id"].clone()).unwrap();
    let reviewed = fx.wait(id, |i| i.phase == Phase::Review).await;
    let explainer = reviewed.content.criteria[0].clone();
    assert!(explainer.fixed);

    for (route, body) in [
        ("drop", None),
        ("edit", Some(json!({"text": "Something else."}))),
    ] {
        let (status, answer) = fx
            .call(
                Method::POST,
                &format!("/v1/intents/{id}/items/{}/{route}", explainer.id),
                body,
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{route}: {answer}");
        assert_eq!(answer["error"], "invalid", "{route}: {answer}");
        assert_eq!(
            answer["message"], "this item is fixed: it cannot be edited or dropped",
            "{route}"
        );
        assert_eq!(fx.state.intents.get(id).await.unwrap(), reviewed, "{route}");
    }
    // Accepting it again changes nothing.
    let (status, answer) = fx
        .call(
            Method::POST,
            &format!("/v1/intents/{id}/items/{}/accept", explainer.id),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(
        fx.state.intents.get(id).await.unwrap().content,
        reviewed.content
    );

    // The model's own criterion is reviewed as usual, and a PATCH of the
    // other fields leaves the fixed ones.
    let other = reviewed.content.criteria[2].id;
    let (status, _) = fx
        .call(
            Method::POST,
            &format!("/v1/intents/{id}/items/{other}/drop"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, patched) = fx
        .call(
            Method::PATCH,
            &format!("/v1/intents/{id}"),
            Some(json!({"title": "Zoom", "kind": "enhancement", "current_behavior": "Big."})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    let patched: Intent = serde_json::from_value(patched).unwrap();
    assert_eq!(
        patched.content.criteria[..2],
        reviewed.content.criteria[..2]
    );
    assert_eq!(patched.content.criteria[2].status, ItemStatus::Dropped);
}

#[tokio::test]
async fn nothing_typed_reaches_a_log() {
    let logs = logs();
    let fx = fixture(true).await;
    let repo = fx.add_repo("r1").await;
    fx.harness.script_structured(vec![
        Ok(json!({"title": "LEAK-title", "kind": "bug", "current_behavior": "LEAK-behavior", "questions": []})),
        Ok(json!({
            "acceptance_criteria": ["LEAK-criterion"],
            "preconditions": [], "stories": [], "depends_on": [],
        })),
    ]);
    let mut req = idea_request(&repo, true, true, false);
    req["main_idea"] = json!("Fix the zoom LEAK-idea\n\nLEAK-body text.");
    req["attachments"][0]["original_name"] = json!("LEAK-name.png");

    let (status, out) = fx.submit(&req).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let id: IntentId = serde_json::from_value(out["intent"]["id"].clone()).unwrap();
    fx.wait(id, |i| i.phase == Phase::Review).await;

    let text = logs.text();
    assert!(!text.contains("LEAK"), "{text}");
    // A positive control: the submission's steps are logged.
    let submission = req["submission_id"].as_str().unwrap();
    assert!(
        text.contains(&format!("submission_id={submission}")),
        "{text}"
    );
}

impl Logs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

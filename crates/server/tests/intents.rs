mod common;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::{json, Value};
use smithagents_core::agent::{AddSourceControl, Agent, Repo, Role};
use smithagents_core::event::EventKind;
use smithagents_core::feedback::FeedbackOrigin;
use smithagents_core::ids::{IntentId, ItemId, QuestionId, RepoId};
use smithagents_core::intent::{
    Answer, AnswerInput, CreateIntentRequest, Intent, IntentFilter, IntentKind, IntentState, Item,
    ItemStatus, Patch, Phase, Question, QuestionOption, ReviewAction, SeedInput, SeedSource,
    VersionAuthor,
};
use smithagents_server::events::{EventBus, Subscription};
use smithagents_server::harness::fake::FakeHarness;
use smithagents_server::harness::{
    Harness, HarnessError, HarnessEvent, StructuredRequest, StructuredResult, TurnRequest,
};
use smithagents_server::intents::{CreateExtras, IntentError, IntentService, PolishRunner};
use smithagents_server::paths::Paths;
use smithagents_server::polish::MAX_SEED_BYTES;
use smithagents_server::service::AgentService;
use smithagents_server::store::Store;
use tempfile::TempDir;
use tokio::sync::Semaphore;

#[derive(Default)]
struct Recorder(Mutex<Vec<(IntentId, u8)>>);

impl PolishRunner for Recorder {
    fn start(&self, id: IntentId, pass: u8) {
        self.0.lock().unwrap().push((id, pass));
    }
}

/// `FakeHarness` plus two test controls for structured runs: a gate that
/// holds each run until released, and panics injected inside `run_structured`.
struct TestHarness {
    fake: FakeHarness,
    /// Structured runs started, counted on entry (before the gate), so a
    /// duplicate pass cannot hide behind a held gate.
    entered: AtomicUsize,
    /// Structured runs that returned (not counting panics).
    finished: AtomicUsize,
    panics: AtomicUsize,
    gated: AtomicBool,
    gate: Semaphore,
}

impl TestHarness {
    fn new() -> Self {
        TestHarness {
            fake: FakeHarness::scripted(vec![]),
            entered: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
            panics: AtomicUsize::new(0),
            gated: AtomicBool::new(false),
            gate: Semaphore::new(0),
        }
    }

    fn script(&self, results: Vec<Result<Value, HarnessError>>) {
        self.fake.script_structured(results);
    }

    /// Requests of the runs that got past the gate (and did not panic).
    fn calls(&self) -> Vec<StructuredRequest> {
        self.fake.structured_calls()
    }

    fn entered(&self) -> usize {
        self.entered.load(Ordering::SeqCst)
    }

    fn finished(&self) -> usize {
        self.finished.load(Ordering::SeqCst)
    }

    /// Waits until `n` structured runs have entered the harness.
    async fn wait_entered(&self, n: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.entered() < n {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{} of {n} structured runs entered", self.entered()));
    }

    async fn wait_finished(&self, n: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.finished() < n {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{} of {n} structured runs finished", self.finished()));
    }

    /// The next structured run panics instead of answering.
    fn panic_next(&self) {
        self.panics.fetch_add(1, Ordering::SeqCst);
    }

    /// From now on every structured run waits for a [`Self::release`].
    fn hold(&self) {
        self.gated.store(true, Ordering::SeqCst);
    }

    fn release(&self, runs: usize) {
        self.gate.add_permits(runs);
    }
}

#[async_trait]
impl Harness for TestHarness {
    async fn run_turn(
        &self,
        req: TurnRequest,
    ) -> Result<BoxStream<'static, HarnessEvent>, HarnessError> {
        self.fake.run_turn(req).await
    }

    async fn run_structured(
        &self,
        req: StructuredRequest,
    ) -> Result<StructuredResult, HarnessError> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        if self.gated.load(Ordering::SeqCst) {
            self.gate.acquire().await.unwrap().forget();
        }
        if self
            .panics
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            panic!("test harness panic inside run_structured");
        }
        let result = self.fake.run_structured(req).await;
        self.finished.fetch_add(1, Ordering::SeqCst);
        result
    }
}

struct Fx {
    tmp: TempDir,
    store: Store,
    bus: EventBus,
    agents: AgentService,
    paths: Paths,
    svc: IntentService,
    runner: Arc<Recorder>,
    h: Arc<TestHarness>,
}

/// Service whose polish runner only records starts (nothing polishes).
async fn fx() -> Fx {
    build(false, false).await
}

/// Service with its real background polish runner over [`TestHarness`].
async fn real_fx() -> Fx {
    build(true, false).await
}

/// `fx`/`real_fx` over a database file, as the daemon runs. Needed where a
/// caller is dropped mid-query: sqlx then closes that connection, which
/// would erase the single-connection in-memory database.
async fn file_fx(real_runner: bool) -> Fx {
    build(real_runner, true).await
}

async fn build(real_runner: bool, file_db: bool) -> Fx {
    let tmp = TempDir::new().unwrap();
    let store = if file_db {
        Store::open(&tmp.path().join("smithagents.db"))
            .await
            .unwrap()
    } else {
        Store::open_in_memory().await.unwrap()
    };
    let bus = EventBus::new(1024);
    let paths = Paths {
        home: tmp.path().join("home"),
    };
    paths.ensure_dirs().unwrap();
    let h = Arc::new(TestHarness::new());
    let agents = AgentService::new(
        store.clone(),
        bus.clone(),
        h.clone(),
        paths.clone(),
        Duration::from_secs(3600),
    );
    let runner = Arc::new(Recorder::default());
    let mut svc = IntentService::new(
        store.clone(),
        bus.clone(),
        h.clone(),
        agents.clone(),
        paths.clone(),
    );
    if !real_runner {
        svc = svc.with_polish_runner(runner.clone());
    }
    Fx {
        tmp,
        store,
        bus,
        agents,
        paths,
        svc,
        runner,
        h,
    }
}

const RESTART_MESSAGE: &str = "polish was interrupted by a daemon restart; retry";

fn pass1(title: &str, questions: usize) -> Result<Value, HarnessError> {
    let questions: Vec<Value> = (1..=questions)
        .map(|n| {
            json!({
                "text": format!("Question {n}?"),
                "options": [{"label": format!("Yes {n}")}, {"label": format!("No {n}")}]
            })
        })
        .collect();
    Ok(json!({
        "title": title,
        "kind": "enhancement",
        "current_behavior": "Builds take ten minutes.",
        "questions": questions,
    }))
}

fn pass2(deps: &[IntentId]) -> Result<Value, HarnessError> {
    let deps: Vec<Value> = deps
        .iter()
        .map(|d| json!({"intent_id": d.to_string(), "reason": "it must land first"}))
        .collect();
    Ok(json!({
        "preconditions": ["CI is green"],
        "acceptance_criteria": ["Builds finish in under a minute"],
        "stories": ["As a developer I get feedback quickly"],
        "depends_on": deps,
    }))
}

fn empty_pass2() -> Result<Value, HarnessError> {
    Ok(json!({"preconditions": [], "acceptance_criteria": [], "stories": [], "depends_on": []}))
}

fn is_failed(i: &Intent) -> bool {
    matches!(i.phase, Phase::Failed { .. })
}

/// The `Failed` pass and message; panics on any other phase.
fn failure(i: &Intent) -> (u8, String) {
    match &i.phase {
        Phase::Failed { pass, message } => (*pass, message.clone()),
        other => panic!("expected a failed phase, got {other:?}"),
    }
}

/// A failure message is short prose with no prompt text or seed in it.
fn assert_user_readable(message: &str) {
    assert!(!message.trim().is_empty());
    assert!(message.len() < 200, "{message}");
    for leak in [
        "make it faster",
        "UNTRUSTED",
        "You are helping",
        "pass 1 of 2",
        "pass 2 of 2",
        "sk-ant",
    ] {
        assert!(!message.contains(leak), "{message:?} contains {leak:?}");
    }
}

fn authors(versions: &[smithagents_core::intent::IntentVersion]) -> Vec<VersionAuthor> {
    versions.iter().map(|v| v.author).collect()
}

/// `IntentUpdated` payloads for `id` until one satisfies `last` (inclusive).
async fn updates_until(
    sub: &mut Subscription,
    id: IntentId,
    last: impl Fn(&Intent) -> bool,
) -> Vec<Intent> {
    let mut seen: Vec<Intent> = sub
        .backlog
        .drain(..)
        .filter_map(|e| match e.kind {
            EventKind::IntentUpdated(i) if i.id == id => Some(i),
            _ => None,
        })
        .collect();
    if let Some(end) = seen.iter().position(&last) {
        seen.truncate(end + 1);
        return seen;
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let e = sub.live.recv().await.unwrap();
            if let EventKind::IntentUpdated(i) = e.kind {
                if i.id == id {
                    let done = last(&i);
                    seen.push(i);
                    if done {
                        return;
                    }
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out; updates so far: {seen:?}"));
    seen
}

impl Fx {
    /// Polls the store until `pred` holds for the intent.
    async fn wait(&self, id: IntentId, pred: impl Fn(&Intent) -> bool) -> Intent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let i = self.store.get_intent(id).await.unwrap().unwrap();
            if pred(&i) {
                return i;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting; intent is {:?} in {:?}",
                i.state,
                i.phase
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn wait_phase(&self, id: IntentId, phase: Phase) -> Intent {
        self.wait(id, |i| i.phase == phase).await
    }

    async fn versions_of(&self, id: IntentId) -> Vec<VersionAuthor> {
        authors(&self.svc.versions(id).await.unwrap())
    }

    /// A second service over the same store whose runner never polishes, for
    /// seeding other intents without consuming scripted results.
    fn seeder(&self) -> IntentService {
        IntentService::new(
            self.store.clone(),
            self.bus.clone(),
            self.h.clone(),
            self.agents.clone(),
            self.paths.clone(),
        )
        .with_polish_runner(Arc::new(Recorder::default()))
    }

    async fn repo(&self, name: &str) -> Repo {
        let dir = self.tmp.path().join(name);
        common::init_repo(&dir);
        self.agents
            .add_repo(&dir, AddSourceControl::Git)
            .await
            .unwrap()
    }

    async fn agent(&self, repo: &Repo) -> Agent {
        self.agents.create_agent(repo.id).await.unwrap()
    }

    async fn text_intent(&self, repo: &Repo) -> smithagents_core::intent::Intent {
        self.svc
            .create(CreateIntentRequest {
                kind: None,
                repo_id: Some(repo.id),
                seed: SeedInput::Text("make it faster".into()),
            })
            .await
            .unwrap()
    }

    /// Moves an intent out of the polishing phase, as the runner would.
    async fn finish_polish(&self, id: IntentId, state: IntentState) {
        let mut i = self.store.get_intent(id).await.unwrap().unwrap();
        i.phase = Phase::Review;
        i.state = state;
        self.store.save_intent(&i).await.unwrap();
    }
}

#[tokio::test]
async fn create_from_chat_tags_source_and_defaults_repo() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let agent = f.agent(&repo).await;
    f.store
        .set_harness_session(agent.id, "sess-1")
        .await
        .unwrap();
    f.store
        .append_message(agent.id, Role::User, "add a cache")
        .await
        .unwrap();
    f.store
        .append_message(agent.id, Role::Assistant, "sure")
        .await
        .unwrap();
    f.store
        .append_message(agent.id, Role::Tool, "ls output")
        .await
        .unwrap();

    let i = f
        .svc
        .create(CreateIntentRequest {
            kind: None,
            repo_id: None,
            seed: SeedInput::Chat {
                agent_id: agent.id,
                from_seq: None,
                to_seq: None,
            },
        })
        .await
        .unwrap();

    assert_eq!(i.repo_id, repo.id);
    assert_eq!(i.state, IntentState::Draft);
    assert_eq!(i.phase, Phase::Polishing { pass: 1 });
    assert_eq!(i.content.title, "Untitled intent");
    assert_eq!(i.seed.text, "User: add a cache\n\nAssistant: sure");
    assert_eq!(
        i.seed.source,
        SeedSource::Chat {
            agent_id: agent.id,
            session_id: Some("sess-1".into()),
            from_seq: None,
            to_seq: None,
        }
    );
    let hex = i.id.to_string().replace('-', "");
    assert_eq!(i.slug, format!("intent-{}", &hex[..8]));
    assert_eq!(f.store.get_intent(i.id).await.unwrap().unwrap(), i);
    assert_eq!(*f.runner.0.lock().unwrap(), vec![(i.id, 1)]);
}

#[tokio::test]
async fn create_publishes_intent_created() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let i = f.text_intent(&repo).await;
    let sub = f.bus.subscribe(Some(0)).unwrap();
    let backlog = sub.backlog;
    assert!(backlog
        .iter()
        .any(|e| matches!(&e.kind, EventKind::IntentCreated(x) if x.id == i.id)));
}

#[tokio::test]
async fn create_from_unknown_agent_is_agent_not_found() {
    let f = fx().await;
    let err = f
        .svc
        .create(CreateIntentRequest {
            kind: None,
            repo_id: None,
            seed: SeedInput::Chat {
                agent_id: smithagents_core::ids::AgentId::new(),
                from_seq: None,
                to_seq: None,
            },
        })
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::AgentNotFound), "{err:?}");
    assert_eq!(err.code(), "agent_not_found");
}

#[tokio::test]
async fn tool_only_chat_is_invalid() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let agent = f.agent(&repo).await;
    f.store
        .append_message(agent.id, Role::Tool, "out")
        .await
        .unwrap();
    let err = f
        .svc
        .create(CreateIntentRequest {
            kind: None,
            repo_id: None,
            seed: SeedInput::Chat {
                agent_id: agent.id,
                from_seq: None,
                to_seq: None,
            },
        })
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
    assert!(f.runner.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn bad_range_is_invalid() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let agent = f.agent(&repo).await;
    f.store
        .append_message(agent.id, Role::User, "x")
        .await
        .unwrap();
    let err = f
        .svc
        .create(CreateIntentRequest {
            kind: None,
            repo_id: None,
            seed: SeedInput::Chat {
                agent_id: agent.id,
                from_seq: Some(5),
                to_seq: Some(2),
            },
        })
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
}

#[tokio::test]
async fn text_seed_requires_repository() {
    let f = fx().await;
    let err = f
        .svc
        .create(CreateIntentRequest {
            kind: None,
            repo_id: None,
            seed: SeedInput::Text("hello".into()),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
    assert_eq!(err.code(), "invalid");
}

#[tokio::test]
async fn blank_text_seed_is_invalid_and_unknown_repo_not_found() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let err = f
        .svc
        .create(CreateIntentRequest {
            kind: None,
            repo_id: Some(repo.id),
            seed: SeedInput::Text("  \n ".into()),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
    let err = f
        .svc
        .create(CreateIntentRequest {
            kind: None,
            repo_id: Some(RepoId::new()),
            seed: SeedInput::Text("hi".into()),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::RepoNotFound), "{err:?}");
}

#[tokio::test]
async fn text_seed_over_the_seed_limit_is_invalid() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let text_seed = |text: String| CreateIntentRequest {
        kind: None,
        repo_id: Some(repo.id),
        seed: SeedInput::Text(text),
    };
    let err = f
        .svc
        .create(text_seed("x".repeat(MAX_SEED_BYTES + 1)))
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
    assert!(f
        .svc
        .list(IntentFilter::default())
        .await
        .unwrap()
        .is_empty());
    assert!(f.runner.0.lock().unwrap().is_empty());

    let ok = f
        .svc
        .create(text_seed("x".repeat(MAX_SEED_BYTES)))
        .await
        .unwrap();
    assert_eq!(ok.seed.text.len(), MAX_SEED_BYTES);
}

#[tokio::test]
async fn explicit_repo_overrides_agent_repo() {
    let f = fx().await;
    let r1 = f.repo("r1").await;
    let r2 = f.repo("r2").await;
    let agent = f.agent(&r1).await;
    f.store
        .append_message(agent.id, Role::User, "hi")
        .await
        .unwrap();
    let i = f
        .svc
        .create(CreateIntentRequest {
            kind: None,
            repo_id: Some(r2.id),
            seed: SeedInput::Chat {
                agent_id: agent.id,
                from_seq: Some(1),
                to_seq: Some(1),
            },
        })
        .await
        .unwrap();
    assert_eq!(i.repo_id, r2.id);
}

#[tokio::test]
async fn versions_has_only_seed_version() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let i = f.text_intent(&repo).await;
    let v = f.svc.versions(i.id).await.unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].version, 1);
    assert_eq!(v[0].author, VersionAuthor::Seed);
    assert!(matches!(
        f.svc.versions(IntentId::new()).await.unwrap_err(),
        IntentError::NotFound
    ));
}

#[tokio::test]
async fn update_while_polishing_is_polishing() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let i = f.text_intent(&repo).await;
    let err = f
        .svc
        .update(
            i.id,
            Patch {
                title: Some("x".into()),
                ..Patch::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::Polishing), "{err:?}");
    assert_eq!(err.code(), "polishing");
}

#[tokio::test]
async fn update_title_and_kind_publishes_event() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let i = f.text_intent(&repo).await;
    f.finish_polish(i.id, IntentState::Draft).await;
    let mut sub = f.bus.subscribe(None).unwrap();
    let u = f
        .svc
        .update(
            i.id,
            Patch {
                title: Some("  Faster builds ".into()),
                kind: Some(IntentKind::Bug),
                current_behavior: Some("slow".into()),
                repo_id: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(u.content.title, "Faster builds");
    assert_eq!(u.content.kind, IntentKind::Bug);
    assert_eq!(u.content.current_behavior, "slow");
    assert!(u.updated_at >= i.updated_at);
    assert_eq!(f.svc.get(i.id).await.unwrap(), u);
    let ev = tokio::time::timeout(Duration::from_secs(1), sub.live.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ev.kind, EventKind::IntentUpdated(x) if x.id == i.id));
}

#[tokio::test]
async fn update_allowed_when_complete_rejected_otherwise() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let i = f.text_intent(&repo).await;
    f.finish_polish(i.id, IntentState::Complete).await;
    let patch = Patch {
        title: Some("ok".into()),
        ..Patch::default()
    };
    f.svc.update(i.id, patch.clone()).await.unwrap();
    f.finish_polish(i.id, IntentState::Queued).await;
    let err = f.svc.update(i.id, patch).await.unwrap_err();
    assert!(
        matches!(
            err,
            IntentError::InvalidState {
                actual: IntentState::Queued,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(err.code(), "invalid_state");
}

#[tokio::test]
async fn blank_or_long_title_is_invalid() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let i = f.text_intent(&repo).await;
    f.finish_polish(i.id, IntentState::Draft).await;
    for t in ["   ".to_string(), "x".repeat(121)] {
        let err = f
            .svc
            .update(
                i.id,
                Patch {
                    title: Some(t),
                    ..Patch::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
    }
    let ok = f
        .svc
        .update(
            i.id,
            Patch {
                title: Some("x".repeat(120)),
                ..Patch::default()
            },
        )
        .await;
    assert!(ok.is_ok());
}

#[tokio::test]
async fn current_behavior_over_20000_chars_is_invalid() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let i = f.text_intent(&repo).await;
    f.finish_polish(i.id, IntentState::Draft).await;
    let before = f.svc.get(i.id).await.unwrap();
    let patch = |text: String| Patch {
        current_behavior: Some(text),
        ..Patch::default()
    };
    // Counted in characters: "é" is two bytes.
    let err = f
        .svc
        .update(i.id, patch("é".repeat(20_001)))
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
    assert_eq!(f.svc.get(i.id).await.unwrap(), before);

    let ok = f.svc.update(i.id, patch("é".repeat(20_000))).await.unwrap();
    assert_eq!(ok.content.current_behavior.chars().count(), 20_000);
}

#[tokio::test]
async fn changing_repo_clears_depends_on() {
    let f = fx().await;
    let r1 = f.repo("r1").await;
    let r2 = f.repo("r2").await;
    let i = f.text_intent(&r1).await;
    let mut stored = f.store.get_intent(i.id).await.unwrap().unwrap();
    stored.phase = Phase::Review;
    stored.content.depends_on.push(Item {
        id: ItemId::new(),
        text: "dep".into(),
        status: ItemStatus::Accepted,
        edited: false,
        intent_ref: Some(IntentId::new()),
        fixed: false,
    });
    f.store.save_intent(&stored).await.unwrap();

    // Same repo keeps them.
    let same = f
        .svc
        .update(
            i.id,
            Patch {
                repo_id: Some(r1.id),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(same.content.depends_on.len(), 1);

    let moved = f
        .svc
        .update(
            i.id,
            Patch {
                repo_id: Some(r2.id),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(moved.repo_id, r2.id);
    assert!(moved.content.depends_on.is_empty());

    let err = f
        .svc
        .update(
            i.id,
            Patch {
                repo_id: Some(RepoId::new()),
                ..Patch::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::RepoNotFound), "{err:?}");
}

#[tokio::test]
async fn unknown_intent_is_not_found() {
    let f = fx().await;
    assert!(matches!(
        f.svc.get(IntentId::new()).await.unwrap_err(),
        IntentError::NotFound
    ));
    let err = f
        .svc
        .update(IntentId::new(), Patch::default())
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::NotFound), "{err:?}");
    assert_eq!(err.code(), "intent_not_found");
}

#[tokio::test]
async fn list_filters_by_state_and_repo() {
    let f = fx().await;
    let r1 = f.repo("r1").await;
    let r2 = f.repo("r2").await;
    let a = f.text_intent(&r1).await;
    let b = f.text_intent(&r2).await;
    f.finish_polish(b.id, IntentState::Complete).await;
    let all = f.svc.list(IntentFilter::default()).await.unwrap();
    assert_eq!(all.len(), 2);
    let drafts = f
        .svc
        .list(IntentFilter {
            state: Some(IntentState::Draft),
            repo_id: None,
        })
        .await
        .unwrap();
    assert_eq!(drafts.iter().map(|i| i.id).collect::<Vec<_>>(), vec![a.id]);
    let in_r2 = f
        .svc
        .list(IntentFilter {
            state: None,
            repo_id: Some(r2.id),
        })
        .await
        .unwrap();
    assert_eq!(in_r2.iter().map(|i| i.id).collect::<Vec<_>>(), vec![b.id]);
}

#[tokio::test]
async fn reservations_serialize_per_intent() {
    let f = fx().await;
    let id = IntentId::new();
    let first = f.svc.reserve(id).await;
    let svc = f.svc.clone();
    let waiter = tokio::spawn(async move {
        let _r = svc.reserve(id).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiter.is_finished());
    // A different intent is not blocked.
    let _other = f.svc.reserve(IntentId::new()).await;
    drop(first);
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap();
}

// ------------------------------------------------------------ polish (Task 6)

#[tokio::test]
async fn polish_asks_questions_then_pass_two_reaches_review() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.script(vec![pass1("Add a build cache", 3), pass2(&[])]);
    let mut sub = f.bus.subscribe(None).unwrap();
    let i = f.text_intent(&repo).await;

    let q = f.wait_phase(i.id, Phase::Questions).await;
    assert_eq!(q.state, IntentState::Draft);
    assert_eq!(q.content.title, "Add a build cache");
    assert_eq!(q.content.kind, IntentKind::Enhancement);
    assert_eq!(q.content.current_behavior, "Builds take ten minutes.");
    assert_eq!(q.slug, "add-a-build-cache");
    assert_eq!(q.content.questions.len(), 3);
    assert_eq!(
        f.versions_of(i.id).await,
        [VersionAuthor::Seed, VersionAuthor::Polish1]
    );
    let calls = f.h.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].prompt.contains("pass 1 of 2"));
    assert!(calls[0].prompt.contains("make it faster"));
    assert_eq!(calls[0].cwd, repo.path);
    assert_eq!(calls[0].model, None);

    let qs = q.content.questions.clone();
    let a = f
        .svc
        .answer(i.id, qs[0].id, AnswerInput::Option("o1".into()))
        .await
        .unwrap();
    assert_eq!(a.phase, Phase::Questions);
    assert_eq!(
        a.content.questions[0].answer,
        Answer::Option {
            option_id: "o1".into()
        }
    );
    let b = f
        .svc
        .answer(i.id, qs[1].id, AnswerInput::Skip)
        .await
        .unwrap();
    assert_eq!(b.phase, Phase::Questions);
    assert_eq!(b.content.questions[1].answer, Answer::Skipped);
    assert_eq!(b.content.questions_open(), 1);
    let c = f
        .svc
        .answer(i.id, qs[2].id, AnswerInput::Option("o2".into()))
        .await
        .unwrap();
    assert_eq!(c.phase, Phase::Polishing { pass: 2 });
    assert_eq!(f.svc.get(i.id).await.unwrap(), c);

    let r = f.wait_phase(i.id, Phase::Review).await;
    assert_eq!(r.state, IntentState::Draft);
    assert_eq!(r.content.pending_count(), 3);
    assert_eq!(
        r.content.criteria[0].text,
        "Builds finish in under a minute"
    );
    assert_eq!(r.content.criteria[0].status, ItemStatus::Pending);
    assert_eq!(r.content.questions, c.content.questions);
    assert_eq!(
        f.versions_of(i.id).await,
        [
            VersionAuthor::Seed,
            VersionAuthor::Polish1,
            VersionAuthor::Polish2
        ]
    );
    let second = &f.h.calls()[1];
    assert!(second.prompt.contains("pass 2 of 2"));
    assert!(second.prompt.contains("A: Yes 1"));
    assert!(second.prompt.contains("A: No 3"));

    // Every step published an update, in order, with increasing updated_at.
    let updates = updates_until(&mut sub, i.id, |x| x.phase == Phase::Review).await;
    let phases: Vec<Phase> = updates.iter().map(|x| x.phase.clone()).collect();
    assert_eq!(
        phases,
        [
            Phase::Questions,
            Phase::Questions,
            Phase::Questions,
            Phase::Polishing { pass: 2 },
            Phase::Review
        ]
    );
    let mut last = i.updated_at;
    for u in &updates {
        assert!(u.updated_at > last, "{} !> {last}", u.updated_at);
        last = u.updated_at;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(f.h.entered(), 2, "pass 2 ran more than once");
}

/// Makes every save of an intent into the phase `kind` fail, through a
/// trigger on the database file of a [`file_fx`]. Keep the pool alive.
async fn refuse_saves_into_phase(f: &Fx, kind: &str) -> sqlx::SqlitePool {
    let raw = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(f.tmp.path().join("smithagents.db")),
    )
    .await
    .unwrap();
    // `kind` is one of this file's own phase names, never outside input.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TRIGGER refuse_{kind} BEFORE UPDATE ON intents \
         WHEN json_extract(NEW.phase_json, '$.kind') = '{kind}' \
         BEGIN SELECT RAISE(ABORT, 'injected failure'); END"
    )))
    .execute(&raw)
    .await
    .unwrap();
    raw
}

#[tokio::test]
async fn a_failed_pass_one_save_leaves_no_polish_version() {
    let f = file_fx(true).await;
    let repo = f.repo("r1").await;
    let _raw = refuse_saves_into_phase(&f, "questions").await;
    f.h.script(vec![pass1("Add a build cache", 2)]);
    let i = f.text_intent(&repo).await;

    let failed = f.wait(i.id, is_failed).await;
    assert_eq!(failure(&failed).0, 1);
    assert_eq!(failed.content.title, "Untitled intent");
    assert_eq!(f.versions_of(i.id).await, [VersionAuthor::Seed]);
}

#[tokio::test]
async fn a_failed_pass_two_save_leaves_no_polish_version() {
    let f = file_fx(true).await;
    let repo = f.repo("r1").await;
    let _raw = refuse_saves_into_phase(&f, "review").await;
    f.h.script(vec![pass1("Add a build cache", 0), pass2(&[])]);
    let i = f.text_intent(&repo).await;

    let failed = f.wait(i.id, is_failed).await;
    assert_eq!(failure(&failed).0, 2);
    assert!(failed.content.criteria.is_empty());
    assert_eq!(
        f.versions_of(i.id).await,
        [VersionAuthor::Seed, VersionAuthor::Polish1]
    );
}

#[tokio::test]
async fn zero_questions_goes_straight_to_pass_two() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.script(vec![pass1("Fix the login bug", 0), pass2(&[])]);
    let i = f.text_intent(&repo).await;
    let r = f.wait_phase(i.id, Phase::Review).await;
    assert_eq!(r.state, IntentState::Draft);
    assert!(r.content.questions.is_empty());
    assert_eq!(r.content.pending_count(), 3);
    assert_eq!(
        f.versions_of(i.id).await,
        [
            VersionAuthor::Seed,
            VersionAuthor::Polish1,
            VersionAuthor::Polish2
        ]
    );
    let calls = f.h.calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].prompt.contains("(no questions were asked)"));
}

#[tokio::test]
async fn nothing_to_review_completes_at_once() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.script(vec![pass1("Tidy imports", 0), empty_pass2()]);
    let i = f.text_intent(&repo).await;
    let r = f.wait(i.id, |x| x.state == IntentState::Complete).await;
    assert_eq!(r.phase, Phase::Review);
    assert!(r.is_complete_content());
}

#[tokio::test]
async fn slug_follows_title_and_collisions_get_suffixes() {
    let f = real_fx().await;
    let r1 = f.repo("r1").await;
    let r2 = f.repo("r2").await;
    let mut slugs = Vec::new();
    for repo in [&r1, &r1, &r1, &r2] {
        f.h.script(vec![pass1("Add a build cache!", 1)]);
        let i = f.text_intent(repo).await;
        slugs.push(f.wait_phase(i.id, Phase::Questions).await.slug);
    }
    assert_eq!(
        slugs,
        [
            "add-a-build-cache",
            "add-a-build-cache-2",
            "add-a-build-cache-3",
            "add-a-build-cache"
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_same_titles_get_unique_slugs() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.hold();
    f.h.script((0..4).map(|_| pass1("Same title", 1)).collect());
    let mut ids = Vec::new();
    for _ in 0..4 {
        ids.push(f.text_intent(&repo).await.id);
    }
    // All four results land at once, so the slug checks and saves interleave.
    f.h.release(4);
    let mut slugs = Vec::new();
    for id in &ids {
        slugs.push(f.wait_phase(*id, Phase::Questions).await.slug);
    }
    slugs.sort();
    assert_eq!(
        slugs,
        ["same-title", "same-title-2", "same-title-3", "same-title-4"]
    );
    // A save retried after a slug conflict left no extra version behind.
    for id in &ids {
        assert_eq!(
            f.versions_of(*id).await,
            [VersionAuthor::Seed, VersionAuthor::Polish1]
        );
    }
}

#[tokio::test]
async fn long_colliding_title_keeps_the_slug_within_48_chars() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    let title = "word ".repeat(30);
    let mut slugs = Vec::new();
    for _ in 0..2 {
        f.h.script(vec![pass1(&title, 1)]);
        let i = f.text_intent(&repo).await;
        slugs.push(f.wait_phase(i.id, Phase::Questions).await.slug);
    }
    assert_ne!(slugs[0], slugs[1]);
    assert!(slugs[1].ends_with("-2"), "{}", slugs[1]);
    for s in &slugs {
        assert!(s.len() <= 48 && !s.contains("--"), "{s}");
    }
}

#[tokio::test]
async fn slug_is_kept_once_sent() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.script(vec![Err(HarnessError::Timeout)]);
    let i = f.text_intent(&repo).await;
    let mut failed = f.wait(i.id, is_failed).await;
    failed.slug = "kept-slug".into();
    failed.sent_at = Some(chrono::Utc::now());
    f.store.save_intent(&failed).await.unwrap();
    f.h.script(vec![pass1("A brand new title", 1)]);
    f.svc.retry_polish(i.id).await.unwrap();
    let q = f.wait_phase(i.id, Phase::Questions).await;
    assert_eq!(q.content.title, "A brand new title");
    assert_eq!(q.slug, "kept-slug");
}

#[tokio::test]
async fn polish_model_is_passed_to_the_harness() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    let svc = IntentService::new(
        f.store.clone(),
        f.bus.clone(),
        f.h.clone(),
        f.agents.clone(),
        f.paths.clone(),
    );
    // A clone taken before the model is set sees it too (shared state).
    let early = svc.clone();
    let _svc = svc.with_polish_model(Some("haiku".into()));
    f.h.script(vec![pass1("T", 0), pass2(&[])]);
    let i = early
        .create(CreateIntentRequest {
            kind: None,
            repo_id: Some(repo.id),
            seed: SeedInput::Text("x".into()),
        })
        .await
        .unwrap();
    f.wait_phase(i.id, Phase::Review).await;
    let models: Vec<_> = f.h.calls().into_iter().map(|c| c.model).collect();
    assert_eq!(
        models,
        [Some("haiku".to_string()), Some("haiku".to_string())]
    );
}

/// Pass 1 fails with whatever is scripted (or panics); retry then succeeds.
async fn fail_pass_one_then_retry(f: &Fx, repo: &Repo) {
    let mut sub = f.bus.subscribe(None).unwrap();
    let i = f.text_intent(repo).await;
    let failed = f.wait(i.id, is_failed).await;
    let (pass, message) = failure(&failed);
    assert_eq!(pass, 1);
    assert_user_readable(&message);
    assert_eq!(failed.state, IntentState::Draft);
    assert_eq!(failed.slug, i.slug, "a failed pass keeps the slug");
    assert_eq!(failed.content, i.content, "a failed pass keeps the content");
    assert_eq!(f.versions_of(i.id).await, [VersionAuthor::Seed]);
    let ev = updates_until(&mut sub, i.id, is_failed).await;
    assert!(ev.last().unwrap().updated_at > i.updated_at);

    f.h.script(vec![pass1("Retried", 0), pass2(&[])]);
    let retried = f.svc.retry_polish(i.id).await.unwrap();
    assert_eq!(retried.phase, Phase::Polishing { pass: 1 });
    assert!(retried.updated_at > failed.updated_at);
    let ev = updates_until(&mut sub, i.id, |x| x.phase == Phase::Polishing { pass: 1 }).await;
    assert_eq!(ev.last().unwrap(), &retried);
    let r = f.wait_phase(i.id, Phase::Review).await;
    assert_eq!(r.content.title, "Retried");
    assert_eq!(r.slug, "retried");
}

#[tokio::test]
async fn harness_timeout_fails_and_retry_succeeds() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.script(vec![Err(HarnessError::Timeout)]);
    fail_pass_one_then_retry(&f, &repo).await;
}

#[tokio::test]
async fn invalid_output_fails_and_retry_succeeds() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.script(vec![Err(HarnessError::InvalidOutput(
        "no JSON object in claude output".into(),
    ))]);
    fail_pass_one_then_retry(&f, &repo).await;
}

#[tokio::test]
async fn unusable_output_fails_and_retry_succeeds() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    // Valid JSON that `apply_pass1` rejects (empty title).
    f.h.script(vec![Ok(
        json!({"title": "  ", "kind": "bug", "current_behavior": "", "questions": []}),
    )]);
    fail_pass_one_then_retry(&f, &repo).await;
}

#[tokio::test]
async fn harness_failure_text_does_not_reach_the_message() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.script(vec![Err(HarnessError::Failed(
        "You are helping turn a rough request: make it faster; key sk-ant-123".into(),
    ))]);
    fail_pass_one_then_retry(&f, &repo).await;
}

#[tokio::test]
async fn a_pass_that_could_not_start_says_what_is_missing() {
    // claude, or the folder to run it in: the person fixes different things.
    let cases = [
        (
            HarnessError::Spawn(std::io::ErrorKind::NotFound.into()),
            "polish pass 1 could not start the claude CLI; check that it is installed, then retry",
        ),
        (
            HarnessError::MissingCwd("/moved/repo".into()),
            "polish pass 1 could not find the repository's folder; check that it is still there, then retry",
        ),
    ];
    for (error, sentence) in cases {
        let f = real_fx().await;
        let repo = f.repo("r1").await;
        f.h.script(vec![Err(error)]);
        let i = f.text_intent(&repo).await;
        let failed = f.wait(i.id, is_failed).await;
        assert_eq!(failure(&failed), (1, sentence.to_owned()));
        assert_user_readable(sentence);
    }
}

#[tokio::test]
async fn panic_inside_the_harness_fails_and_retry_succeeds() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.panic_next();
    fail_pass_one_then_retry(&f, &repo).await;
}

#[tokio::test]
async fn pass_two_failure_retries_pass_two_only() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.script(vec![pass1("T", 0), Err(HarnessError::Timeout)]);
    let i = f.text_intent(&repo).await;
    let failed = f.wait(i.id, is_failed).await;
    let (pass, message) = failure(&failed);
    assert_eq!(pass, 2);
    assert_user_readable(&message);
    assert_eq!(failed.content.title, "T", "pass 1 result is kept");

    f.h.script(vec![pass2(&[])]);
    let retried = f.svc.retry_polish(i.id).await.unwrap();
    assert_eq!(retried.phase, Phase::Polishing { pass: 2 });
    f.wait_phase(i.id, Phase::Review).await;
    let calls = f.h.calls();
    assert_eq!(calls.len(), 3);
    assert!(calls[2].prompt.contains("pass 2 of 2"));
    assert_eq!(
        f.versions_of(i.id).await,
        [
            VersionAuthor::Seed,
            VersionAuthor::Polish1,
            VersionAuthor::Polish2
        ]
    );
}

#[tokio::test]
async fn answer_validation_and_already_answered() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.script(vec![pass1("T", 2)]);
    let i = f.text_intent(&repo).await;
    let q = f.wait_phase(i.id, Phase::Questions).await;
    let q0 = q.content.questions[0].id;

    let err = f
        .svc
        .answer(i.id, QuestionId::new(), AnswerInput::Skip)
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
    let err = f
        .svc
        .answer(i.id, q0, AnswerInput::Option("o9".into()))
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
    assert_eq!(err.code(), "invalid");
    let err = f
        .svc
        .answer(IntentId::new(), q0, AnswerInput::Skip)
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::NotFound), "{err:?}");

    f.svc
        .answer(i.id, q0, AnswerInput::Option("o2".into()))
        .await
        .unwrap();
    for again in [AnswerInput::Option("o1".into()), AnswerInput::Skip] {
        let err = f.svc.answer(i.id, q0, again).await.unwrap_err();
        assert!(matches!(err, IntentError::AlreadyAnswered), "{err:?}");
        assert_eq!(err.code(), "already_answered");
        assert_eq!(err.to_string(), "question already answered");
    }
    let stored = f.svc.get(i.id).await.unwrap();
    assert_eq!(
        stored.content.questions[0].answer,
        Answer::Option {
            option_id: "o2".into()
        }
    );
    assert_eq!(stored.phase, Phase::Questions);
    assert_eq!(f.h.entered(), 1, "no pass 2 while a question is open");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_answers_to_the_last_two_questions_run_pass_two_once() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.script(vec![pass1("T", 3), pass2(&[])]);
    let i = f.text_intent(&repo).await;
    let q = f.wait_phase(i.id, Phase::Questions).await;
    let qs: Vec<QuestionId> = q.content.questions.iter().map(|q| q.id).collect();
    f.svc.answer(i.id, qs[0], AnswerInput::Skip).await.unwrap();

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let tasks: Vec<_> = [
        (qs[1], AnswerInput::Option("o1".into())),
        (qs[2], AnswerInput::Skip),
    ]
    .into_iter()
    .map(|(qid, a)| {
        let svc = f.svc.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            svc.answer(i.id, qid, a).await
        })
    })
    .collect();
    let mut phases = Vec::new();
    for t in tasks {
        phases.push(t.await.unwrap().unwrap().phase);
    }
    assert_eq!(
        phases
            .iter()
            .filter(|p| **p == Phase::Polishing { pass: 2 })
            .count(),
        1,
        "{phases:?}"
    );
    assert!(phases.contains(&Phase::Questions), "{phases:?}");

    let r = f.wait_phase(i.id, Phase::Review).await;
    assert_eq!(r.content.questions_open(), 0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(f.h.entered(), 2, "pass 2 must run exactly once");
    let versions = f.versions_of(i.id).await;
    assert_eq!(
        versions
            .iter()
            .filter(|a| **a == VersionAuthor::Polish2)
            .count(),
        1
    );
    assert_eq!(f.svc.get(i.id).await.unwrap().phase, Phase::Review);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_answers_to_the_same_last_question_one_wins() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.hold();
    f.h.script(vec![pass1("T", 1), pass2(&[])]);
    let i = f.text_intent(&repo).await;
    f.h.release(1);
    let q = f.wait_phase(i.id, Phase::Questions).await;
    let qid = q.content.questions[0].id;

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let tasks: Vec<_> = ["o1", "o2"]
        .into_iter()
        .map(|o| {
            let svc = f.svc.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                svc.answer(i.id, qid, AnswerInput::Option(o.into())).await
            })
        })
        .collect();
    let mut results = Vec::new();
    for t in tasks {
        results.push(t.await.unwrap());
    }
    let wins: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(wins.len(), 1, "{results:?}");
    assert_eq!(wins[0].phase, Phase::Polishing { pass: 2 });
    let err = results.iter().find_map(|r| r.as_ref().err()).unwrap();
    assert!(
        matches!(err, IntentError::InvalidState { .. }),
        "conflict expected, got {err:?}"
    );
    // Pass 2 is held at the gate; a second pass-2 job would be counted on
    // entry while still held.
    f.h.wait_entered(2).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(f.h.entered(), 2, "pass 2 started more than once");
    f.h.release(1);
    f.wait_phase(i.id, Phase::Review).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(f.h.entered(), 2);
}

#[tokio::test]
async fn answer_and_retry_while_polishing_are_conflicts() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.hold();
    f.h.script(vec![pass1("T", 1), pass2(&[])]);
    let i = f.text_intent(&repo).await;

    // Pass 1 is held in the harness.
    let err = f.svc.retry_polish(i.id).await.unwrap_err();
    assert!(matches!(err, IntentError::Polishing), "{err:?}");
    assert_eq!(err.code(), "polishing");
    let err = f
        .svc
        .answer(i.id, QuestionId::new(), AnswerInput::Skip)
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::InvalidState { .. }), "{err:?}");

    f.h.release(1);
    let q = f.wait_phase(i.id, Phase::Questions).await;
    let qid = q.content.questions[0].id;
    let err = f.svc.retry_polish(i.id).await.unwrap_err();
    assert!(matches!(err, IntentError::InvalidState { .. }), "{err:?}");
    let started = f.svc.answer(i.id, qid, AnswerInput::Skip).await.unwrap();
    assert_eq!(started.phase, Phase::Polishing { pass: 2 });

    // Pass 2 is held in the harness: answering again and retrying conflict.
    let err = f
        .svc
        .answer(i.id, qid, AnswerInput::Option("o1".into()))
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::InvalidState { .. }), "{err:?}");
    assert_eq!(err.code(), "invalid_state");
    assert_eq!(
        err.to_string(),
        "intent is in the polishing phase (pass 2), expected the questions phase"
    );
    let err = f.svc.retry_polish(i.id).await.unwrap_err();
    assert!(matches!(err, IntentError::Polishing), "{err:?}");

    f.h.release(1);
    f.wait_phase(i.id, Phase::Review).await;
    let err = f.svc.retry_polish(i.id).await.unwrap_err();
    assert!(matches!(err, IntentError::InvalidState { .. }), "{err:?}");
    let err = f
        .svc
        .answer(i.id, qid, AnswerInput::Skip)
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::InvalidState { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        "intent is in the review phase, expected the questions phase"
    );
    assert_eq!(f.h.entered(), 2);
    let err = f.svc.retry_polish(IntentId::new()).await.unwrap_err();
    assert!(matches!(err, IntentError::NotFound), "{err:?}");
}

#[tokio::test]
async fn recover_fails_interrupted_polishes() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let a = f.text_intent(&repo).await;
    let b = f.text_intent(&repo).await;
    let mut stored = f.store.get_intent(b.id).await.unwrap().unwrap();
    stored.phase = Phase::Polishing { pass: 2 };
    f.store.save_intent(&stored).await.unwrap();
    let c = f.text_intent(&repo).await;
    f.finish_polish(c.id, IntentState::Draft).await;
    let c_before = f.store.get_intent(c.id).await.unwrap().unwrap();
    let mut sub = f.bus.subscribe(None).unwrap();

    assert_eq!(f.svc.recover().await.unwrap(), 2);

    let a2 = f.svc.get(a.id).await.unwrap();
    assert_eq!(
        a2.phase,
        Phase::Failed {
            pass: 1,
            message: RESTART_MESSAGE.into()
        }
    );
    assert!(a2.updated_at > a.updated_at);
    assert_eq!(
        f.svc.get(b.id).await.unwrap().phase,
        Phase::Failed {
            pass: 2,
            message: RESTART_MESSAGE.into()
        }
    );
    assert_eq!(f.svc.get(c.id).await.unwrap(), c_before);
    assert_eq!(updates_until(&mut sub, a.id, is_failed).await.len(), 1);
    assert_eq!(updates_until(&mut sub, b.id, is_failed).await.len(), 1);
    assert_eq!(f.svc.recover().await.unwrap(), 0);

    f.svc.retry_polish(a.id).await.unwrap();
    f.svc.retry_polish(b.id).await.unwrap();
    assert_eq!(
        *f.runner.0.lock().unwrap(),
        vec![(a.id, 1), (b.id, 1), (c.id, 1), (a.id, 1), (b.id, 2)]
    );
}

#[tokio::test]
async fn pass_two_sees_same_repo_candidates_and_skip_marker() {
    let f = real_fx().await;
    let r1 = f.repo("r1").await;
    let r2 = f.repo("r2").await;
    let seeder = f.seeder();
    let other = |repo: RepoId, title: &'static str, state: IntentState| {
        let seeder = seeder.clone();
        let store = f.store.clone();
        async move {
            let i = seeder
                .create(CreateIntentRequest {
                    kind: None,
                    repo_id: Some(repo),
                    seed: SeedInput::Text("other".into()),
                })
                .await
                .unwrap();
            let mut i = store.get_intent(i.id).await.unwrap().unwrap();
            i.phase = Phase::Review;
            i.state = state;
            i.content.title = title.into();
            store.save_intent(&i).await.unwrap();
            i
        }
    };
    let alpha = other(r1.id, "Alpha", IntentState::Complete).await;
    let beta = other(r2.id, "Beta", IntentState::Draft).await;
    let gamma = other(r1.id, "Gamma", IntentState::Resolved).await;

    f.h.script(vec![pass1("Target", 1)]);
    let t = f.text_intent(&r1).await;
    let q = f.wait_phase(t.id, Phase::Questions).await;

    // Alpha already depends on the target (confirmed), so the target may not
    // depend on Alpha.
    let mut a = f.store.get_intent(alpha.id).await.unwrap().unwrap();
    a.content.depends_on.push(Item {
        id: ItemId::new(),
        text: "needs target".into(),
        status: ItemStatus::Accepted,
        edited: false,
        intent_ref: Some(t.id),
        fixed: false,
    });
    f.store.save_intent(&a).await.unwrap();

    f.h.script(vec![pass2(&[alpha.id, gamma.id, beta.id, t.id])]);
    f.svc
        .answer(t.id, q.content.questions[0].id, AnswerInput::Skip)
        .await
        .unwrap();
    let r = f.wait_phase(t.id, Phase::Review).await;

    let prompt = f.h.calls()[1].prompt.clone();
    assert!(prompt.contains("pass 2 of 2"));
    assert!(prompt.contains("A: no answer; assume reasonable defaults"));
    assert!(prompt.contains(&alpha.id.to_string()));
    assert!(prompt.contains(&gamma.id.to_string()));
    assert!(prompt.contains("title: Gamma | state: resolved"));
    assert!(
        !prompt.contains(&beta.id.to_string()),
        "cross-repo candidate"
    );
    assert!(
        !prompt.contains(&t.id.to_string()),
        "self is not a candidate"
    );

    let refs: Vec<_> = r.content.depends_on.iter().map(|d| d.intent_ref).collect();
    assert_eq!(refs, [Some(gamma.id)]);
    assert_eq!(r.content.depends_on[0].status, ItemStatus::Pending);
}

#[tokio::test]
async fn recover_during_pass_one_discards_its_late_result() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.hold();
    f.h.script(vec![pass1("Late result", 1)]);
    let i = f.text_intent(&repo).await;
    f.h.wait_entered(1).await;

    assert_eq!(f.svc.recover().await.unwrap(), 1);
    f.h.release(1);
    f.h.wait_finished(1).await;
    // Give the job time to apply its result, which it must not do.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let after = f.svc.get(i.id).await.unwrap();
    assert_eq!(
        after.phase,
        Phase::Failed {
            pass: 1,
            message: RESTART_MESSAGE.into()
        }
    );
    assert_eq!(after.content, i.content);
    assert_eq!(after.slug, i.slug);
    assert_eq!(f.versions_of(i.id).await, [VersionAuthor::Seed]);
}

// ------------------------------------------------- callers that stop waiting

/// Polls `fut` without pause until it finishes or `budget` of wall-clock
/// time has passed, then drops it unfinished, as a server drops the handler
/// of a client that went away. Sweeping `budget` lands the drop at every
/// stage of the call, including while a store write is in flight.
async fn run_then_drop<F: Future>(fut: F, budget: Duration) {
    let mut fut = std::pin::pin!(fut);
    let start = std::time::Instant::now();
    while futures::poll!(fut.as_mut()).is_pending() {
        if start.elapsed() >= budget {
            return;
        }
        tokio::task::yield_now().await;
    }
}

/// `runs` budgets spread evenly over 0 to 1.5 times `full` (the duration of
/// a whole call), so the drops cover the call from start to finish.
fn budgets(full: Duration, runs: u32) -> impl Iterator<Item = Duration> {
    (1..=runs).map(move |k| full * 3 * k / (2 * runs))
}

/// Puts a seeded intent into the questions phase with one open question.
async fn with_one_question(f: &Fx, id: IntentId) -> QuestionId {
    let mut i = f.store.get_intent(id).await.unwrap().unwrap();
    let qid = QuestionId::new();
    i.phase = Phase::Questions;
    i.content.questions = vec![Question {
        id: qid,
        text: "Which one?".into(),
        options: vec![
            QuestionOption {
                id: "o1".into(),
                label: "This".into(),
            },
            QuestionOption {
                id: "o2".into(),
                label: "That".into(),
            },
        ],
        answer: Answer::Unanswered,
    }];
    f.store.save_intent(&i).await.unwrap();
    qid
}

/// Stopping at every point of the call: whether or not the answer was
/// written, the intent never stays in `Polishing` without a job, and every
/// written last answer runs pass 2 exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropped_answer_calls_never_strand_pass_two() {
    const RUNS: u32 = 40;
    let f = file_fx(true).await;
    let repo = f.repo("r1").await;
    let seeder = f.seeder();
    let seeded = || async {
        let i = seeder
            .create(CreateIntentRequest {
                kind: None,
                repo_id: Some(repo.id),
                seed: SeedInput::Text("x".into()),
            })
            .await
            .unwrap();
        let qid = with_one_question(&f, i.id).await;
        (i.id, qid)
    };
    f.h.script((0..=RUNS).map(|_| pass2(&[])).collect());
    // One whole call, to size the budgets.
    let (id, qid) = seeded().await;
    let started = std::time::Instant::now();
    f.svc.answer(id, qid, AnswerInput::Skip).await.unwrap();
    let full = started.elapsed();
    f.wait_phase(id, Phase::Review).await;

    let mut reviewed = 1;
    for budget in budgets(full, RUNS) {
        let (id, qid) = seeded().await;
        run_then_drop(f.svc.answer(id, qid, AnswerInput::Skip), budget).await;
        // Let a write that was already on its way land.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let end = f
            .wait(id, |x| x.phase != Phase::Polishing { pass: 2 })
            .await;
        match end.phase {
            Phase::Review => reviewed += 1,
            Phase::Questions => {}
            other => panic!("dropped after {budget:?}: unexpected {other:?}"),
        }
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        f.h.entered(),
        reviewed,
        "one pass 2 per written last answer"
    );
}

/// Same for `create` and `retry_polish`, which write `Polishing{pass}` and
/// then start it: a written `Polishing` always comes with exactly one start.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropped_create_and_retry_calls_never_strand_polishing() {
    const RUNS: u32 = 80;
    let f = file_fx(false).await;
    let repo = f.repo("r1").await;
    let create = || {
        f.svc.create(CreateIntentRequest {
            kind: None,
            repo_id: Some(repo.id),
            seed: SeedInput::Text("x".into()),
        })
    };
    let starts = |id: IntentId, pass: u8| {
        f.runner
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|s| **s == (id, pass))
            .count()
    };
    let fail = |i: &Intent| {
        let mut failed = i.clone();
        failed.phase = Phase::Failed {
            pass: 2,
            message: "x".into(),
        };
        let store = f.store.clone();
        async move { store.save_intent(&failed).await.unwrap() }
    };

    let started = std::time::Instant::now();
    let first = create().await.unwrap();
    let full_create = started.elapsed();
    fail(&first).await;
    let started = std::time::Instant::now();
    f.svc.retry_polish(first.id).await.unwrap();
    let full_retry = started.elapsed();

    for budget in budgets(full_create, RUNS) {
        run_then_drop(create(), budget).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let created: Vec<Intent> = f
        .svc
        .list(IntentFilter::default())
        .await
        .unwrap()
        .into_iter()
        .filter(|i| i.id != first.id)
        .collect();
    for i in &created {
        assert_eq!(i.phase, Phase::Polishing { pass: 1 });
        assert_eq!(starts(i.id, 1), 1, "created {} without a start", i.id);
    }

    for (i, budget) in created.iter().zip(budgets(full_retry, RUNS)) {
        fail(i).await;
        run_then_drop(f.svc.retry_polish(i.id), budget).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    for i in &created {
        let now = f.svc.get(i.id).await.unwrap();
        let expected = match now.phase {
            Phase::Polishing { pass: 2 } => 1,
            Phase::Failed { .. } => 0,
            ref other => panic!("unexpected {other:?}"),
        };
        assert_eq!(starts(i.id, 2), expected, "{} is {:?}", i.id, now.phase);
    }
}

// ------------------------------------------------------------------ review

fn item(text: &str, status: ItemStatus) -> Item {
    Item {
        id: ItemId::new(),
        text: text.into(),
        status,
        edited: false,
        intent_ref: None,
        fixed: false,
    }
}

impl Fx {
    /// A text intent in the review phase with three pending items
    /// (precondition, criterion, story); returns their ids.
    async fn review_intent(&self, repo: &Repo) -> (Intent, [ItemId; 3]) {
        let i = self.text_intent(repo).await;
        let mut s = self.store.get_intent(i.id).await.unwrap().unwrap();
        s.phase = Phase::Review;
        s.state = IntentState::Draft;
        let items = [
            item("pre", ItemStatus::Pending),
            item("crit", ItemStatus::Pending),
            item("story", ItemStatus::Pending),
        ];
        s.content.preconditions = vec![items[0].clone()];
        s.content.criteria = vec![items[1].clone()];
        s.content.stories = vec![items[2].clone()];
        self.store.save_intent(&s).await.unwrap();
        (s, [items[0].id, items[1].id, items[2].id])
    }

    async fn add_dep(&self, id: IntentId, target: IntentId, status: ItemStatus) -> ItemId {
        let mut s = self.store.get_intent(id).await.unwrap().unwrap();
        let mut d = item("dep", status);
        d.intent_ref = Some(target);
        let did = d.id;
        s.content.depends_on.push(d);
        self.store.save_intent(&s).await.unwrap();
        did
    }
}

#[tokio::test]
async fn review_accept_edit_drop_and_counters() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let (i, [a, b, c]) = f.review_intent(&repo).await;
    let mut sub = f.bus.subscribe(None).unwrap();
    let r = f.svc.review(i.id, a, ReviewAction::Accept).await.unwrap();
    assert_eq!(r.content.preconditions[0].status, ItemStatus::Accepted);
    assert!(!r.content.preconditions[0].edited);
    assert_eq!(r.content.pending_count(), 2);
    assert_eq!(r.state, IntentState::Draft);
    let ev = tokio::time::timeout(Duration::from_secs(1), sub.live.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ev.kind, EventKind::IntentUpdated(x) if x.id == i.id));

    let r = f
        .svc
        .review(i.id, b, ReviewAction::Edit("  better ".into()))
        .await
        .unwrap();
    let crit = &r.content.criteria[0];
    assert_eq!(
        (crit.text.as_str(), crit.edited, crit.status),
        ("better", true, ItemStatus::Accepted)
    );
    let r = f.svc.review(i.id, c, ReviewAction::Drop).await.unwrap();
    assert_eq!(r.content.stories[0].status, ItemStatus::Dropped);
    assert_eq!(f.svc.get(i.id).await.unwrap(), r);
    // No new version.
    assert_eq!(f.versions_of(i.id).await, [VersionAuthor::Seed]);
}

#[tokio::test]
async fn counters_read_one_of_three_accepted() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let (i, [a, ..]) = f.review_intent(&repo).await;
    let r = f.svc.review(i.id, a, ReviewAction::Accept).await.unwrap();
    let all = r
        .content
        .preconditions
        .iter()
        .chain(&r.content.criteria)
        .chain(&r.content.stories)
        .chain(&r.content.depends_on);
    let (accepted, total) = all.fold((0, 0), |(a, t), x| {
        (a + (x.status == ItemStatus::Accepted) as u32, t + 1)
    });
    assert_eq!((accepted, total), (1, 3));
}

#[tokio::test]
async fn last_decision_completes_and_reopening_recomputes() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let (i, [a, b, c]) = f.review_intent(&repo).await;
    f.svc.review(i.id, a, ReviewAction::Accept).await.unwrap();
    f.svc.review(i.id, b, ReviewAction::Drop).await.unwrap();
    let r = f.svc.review(i.id, c, ReviewAction::Accept).await.unwrap();
    assert_eq!(r.state, IntentState::Complete);
    // Changing a decision keeps it complete.
    let r = f.svc.review(i.id, a, ReviewAction::Drop).await.unwrap();
    assert_eq!(r.state, IntentState::Complete);
    assert_eq!(r.content.preconditions[0].status, ItemStatus::Dropped);
}

#[tokio::test]
async fn dropping_every_item_completes() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let (i, ids) = f.review_intent(&repo).await;
    let mut last = None;
    for id in ids {
        last = Some(f.svc.review(i.id, id, ReviewAction::Drop).await.unwrap());
    }
    assert_eq!(last.unwrap().state, IntentState::Complete);
}

#[tokio::test]
async fn pending_open_question_keeps_it_draft_and_edit_validation() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let (i, [a, ..]) = f.review_intent(&repo).await;
    for bad in ["   ".to_string(), "x".repeat(2001)] {
        let err = f
            .svc
            .review(i.id, a, ReviewAction::Edit(bad))
            .await
            .unwrap_err();
        assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
    }
    let err = f
        .svc
        .review(i.id, ItemId::new(), ReviewAction::Accept)
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
    let ok = f
        .svc
        .review(i.id, a, ReviewAction::Edit("x".repeat(2000)))
        .await;
    assert!(ok.is_ok());
    let unchanged = f.svc.get(i.id).await.unwrap();
    assert_eq!(unchanged.content.criteria[0].status, ItemStatus::Pending);
}

#[tokio::test]
async fn review_in_wrong_phase_or_state_is_invalid_state() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let (i, [a, ..]) = f.review_intent(&repo).await;
    for phase in [
        Phase::Polishing { pass: 1 },
        Phase::Polishing { pass: 2 },
        Phase::Questions,
        Phase::Failed {
            pass: 1,
            message: "m".into(),
        },
    ] {
        let mut s = f.store.get_intent(i.id).await.unwrap().unwrap();
        s.phase = phase;
        f.store.save_intent(&s).await.unwrap();
        let err = f
            .svc
            .review(i.id, a, ReviewAction::Accept)
            .await
            .unwrap_err();
        assert!(matches!(err, IntentError::InvalidState { .. }), "{err:?}");
    }
    for state in [
        IntentState::Queued,
        IntentState::Sent,
        IntentState::Resolved,
    ] {
        let mut s = f.store.get_intent(i.id).await.unwrap().unwrap();
        s.phase = Phase::Review;
        s.state = state;
        f.store.save_intent(&s).await.unwrap();
        let err = f
            .svc
            .review(i.id, a, ReviewAction::Accept)
            .await
            .unwrap_err();
        assert!(
            matches!(err, IntentError::InvalidState { actual, .. } if actual == state),
            "{err:?}"
        );
    }
    let err = f
        .svc
        .review(IntentId::new(), a, ReviewAction::Accept)
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::NotFound), "{err:?}");
}

#[tokio::test]
async fn accepting_a_dependency_that_closes_a_cycle_fails() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let (a, _) = f.review_intent(&repo).await;
    let (b, _) = f.review_intent(&repo).await;
    // A depends on B (confirmed); now B proposes A.
    f.add_dep(a.id, b.id, ItemStatus::Accepted).await;
    let dep = f.add_dep(b.id, a.id, ItemStatus::Pending).await;
    let err = f
        .svc
        .review(b.id, dep, ReviewAction::Accept)
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::DependencyCycle), "{err:?}");
    let s = f.svc.get(b.id).await.unwrap();
    assert_eq!(s.content.depends_on[0].status, ItemStatus::Pending);
    // Dropping is still fine.
    f.svc.review(b.id, dep, ReviewAction::Drop).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_accepts_cannot_close_a_cycle() {
    let f = file_fx(false).await;
    let repo = f.repo("r1").await;
    for round in 0..40 {
        let (a, _) = f.review_intent(&repo).await;
        let (b, _) = f.review_intent(&repo).await;
        let a_on_b = f.add_dep(a.id, b.id, ItemStatus::Pending).await;
        let b_on_a = f.add_dep(b.id, a.id, ItemStatus::Pending).await;

        let (ra, rb) = tokio::join!(
            f.svc.review(a.id, a_on_b, ReviewAction::Accept),
            f.svc.review(b.id, b_on_a, ReviewAction::Accept)
        );
        let (ok, err) = match (ra, rb) {
            (Ok(ok), Err(err)) | (Err(err), Ok(ok)) => (ok, err),
            (ra, rb) => panic!(
                "round {round}: expected one success, got {:?} and {:?}",
                ra.map(|i| i.id),
                rb.map(|i| i.id)
            ),
        };
        assert!(
            matches!(err, IntentError::DependencyCycle),
            "round {round}: {err:?}"
        );
        assert_eq!(ok.content.depends_on[0].status, ItemStatus::Accepted);
        // The refused one stays pending in the store.
        let refused = if ok.id == a.id { b.id } else { a.id };
        let stored = f.svc.get(refused).await.unwrap();
        assert_eq!(stored.content.depends_on[0].status, ItemStatus::Pending);
    }
}

#[tokio::test]
async fn accepting_a_dependency_checks_existence_and_repository() {
    let f = fx().await;
    let r1 = f.repo("r1").await;
    let r2 = f.repo("r2").await;
    let (a, _) = f.review_intent(&r1).await;
    let other = f.review_intent(&r2).await.0;
    let stale = f.add_dep(a.id, other.id, ItemStatus::Pending).await;
    let err = f
        .svc
        .review(a.id, stale, ReviewAction::Accept)
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::DependencyCrossRepo), "{err:?}");
    assert_eq!(err.code(), "dependency_cross_repo");
    let missing = f.add_dep(a.id, IntentId::new(), ItemStatus::Pending).await;
    let err = f
        .svc
        .review(a.id, missing, ReviewAction::Accept)
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
    assert_eq!(
        f.svc.get(a.id).await.unwrap().content.depends_on[0].status,
        ItemStatus::Pending
    );

    // A valid one is accepted; editing keeps the reference.
    let (b, _) = f.review_intent(&r1).await;
    let ok = f.add_dep(a.id, b.id, ItemStatus::Pending).await;
    let r = f
        .svc
        .review(a.id, ok, ReviewAction::Edit("needs b first".into()))
        .await
        .unwrap();
    let d = r.content.depends_on.iter().find(|d| d.id == ok).unwrap();
    assert_eq!(d.intent_ref, Some(b.id));
    assert_eq!(d.text, "needs b first");
    assert_eq!(d.status, ItemStatus::Accepted);
}

#[tokio::test]
async fn changing_repo_of_a_complete_intent_recomputes_state() {
    let f = fx().await;
    let r1 = f.repo("r1").await;
    let r2 = f.repo("r2").await;
    let (i, [a, b, c]) = f.review_intent(&r1).await;
    f.svc.review(i.id, a, ReviewAction::Accept).await.unwrap();
    f.svc.review(i.id, b, ReviewAction::Accept).await.unwrap();
    f.add_dep(i.id, IntentId::new(), ItemStatus::Accepted).await;
    let r = f.svc.review(i.id, c, ReviewAction::Accept).await.unwrap();
    assert_eq!(r.state, IntentState::Complete);
    let moved = f
        .svc
        .update(
            i.id,
            Patch {
                repo_id: Some(r2.id),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    assert!(moved.content.depends_on.is_empty());
    assert_eq!(moved.state, IntentState::Complete);

    // A pending item reopens a complete intent when it is not removed.
    let (j, _) = f.review_intent(&r1).await;
    let mut s = f.store.get_intent(j.id).await.unwrap().unwrap();
    s.state = IntentState::Complete;
    f.store.save_intent(&s).await.unwrap();
    let u = f
        .svc
        .update(
            j.id,
            Patch {
                title: Some("t".into()),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(u.state, IntentState::Draft);
}

#[tokio::test]
async fn repo_change_keeps_a_free_slug_and_suffixes_a_taken_one() {
    let f = fx().await;
    let r1 = f.repo("r1").await;
    let r2 = f.repo("r2").await;
    let (i, _) = f.review_intent(&r1).await;
    let before = f.svc.get(i.id).await.unwrap().slug;
    let moved = f
        .svc
        .update(
            i.id,
            Patch {
                repo_id: Some(r2.id),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(moved.slug, before);

    // Another intent in r1 owns that slug; moving there needs a suffix.
    let (j, _) = f.review_intent(&r1).await;
    let mut s = f.store.get_intent(j.id).await.unwrap().unwrap();
    s.slug = before.clone();
    f.store.save_intent(&s).await.unwrap();
    let back = f
        .svc
        .update(
            i.id,
            Patch {
                repo_id: Some(r1.id),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(back.slug, format!("{before}-2"));
}

#[tokio::test]
async fn empty_patch_changes_nothing_and_publishes_nothing() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let (i, _) = f.review_intent(&repo).await;
    let mut sub = f.bus.subscribe(None).unwrap();
    let r = f.svc.update(i.id, Patch::default()).await.unwrap();
    assert_eq!(r.updated_at, i.updated_at);
    let none = tokio::time::timeout(Duration::from_millis(100), sub.live.recv()).await;
    assert!(none.is_err(), "unexpected event");
}

#[tokio::test]
async fn create_with_a_kind_survives_pass_one() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.script(vec![pass1("Tidy the build", 1)]);
    let i = f
        .svc
        .create(CreateIntentRequest {
            kind: Some(IntentKind::Maintenance),
            repo_id: Some(repo.id),
            seed: SeedInput::Text("tidy the build".into()),
        })
        .await
        .unwrap();
    assert_eq!(i.content.kind, IntentKind::Maintenance);
    assert_eq!(i.seed.kind, Some(IntentKind::Maintenance));
    let q = f.wait_phase(i.id, Phase::Questions).await;
    assert_eq!(q.content.kind, IntentKind::Maintenance);
    assert_eq!(q.content.title, "Tidy the build");

    // A failed pass 1 and its retry keep the kind too.
    f.h.script(vec![Err(HarnessError::Timeout)]);
    let j = f
        .svc
        .create(CreateIntentRequest {
            kind: Some(IntentKind::Maintenance),
            repo_id: Some(repo.id),
            seed: SeedInput::Text("tidy more".into()),
        })
        .await
        .unwrap();
    let failed = f.wait(j.id, is_failed).await;
    assert_eq!(failed.content.kind, IntentKind::Maintenance);
    f.h.script(vec![pass1("Tidy more", 1)]);
    f.svc.retry_polish(j.id).await.unwrap();
    let q = f.wait_phase(j.id, Phase::Questions).await;
    assert_eq!(q.content.kind, IntentKind::Maintenance);
    assert_eq!(q.content.title, "Tidy more");
}

#[tokio::test]
async fn a_kind_patched_after_a_failed_pass_one_survives_the_retry() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.script(vec![Err(HarnessError::Timeout)]);
    let i = f
        .svc
        .create(CreateIntentRequest {
            kind: Some(IntentKind::Bug),
            repo_id: Some(repo.id),
            seed: SeedInput::Text("the build breaks".into()),
        })
        .await
        .unwrap();
    f.wait(i.id, is_failed).await;
    let patched = f
        .svc
        .update(
            i.id,
            Patch {
                kind: Some(IntentKind::Refactor),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(patched.content.kind, IntentKind::Refactor);
    assert_eq!(patched.seed.kind, Some(IntentKind::Refactor));

    f.h.script(vec![pass1("Split the build", 1)]);
    f.svc.retry_polish(i.id).await.unwrap();
    let q = f.wait_phase(i.id, Phase::Questions).await;
    assert_eq!(q.content.kind, IntentKind::Refactor);
    let prompt = f.h.calls().last().unwrap().prompt.clone();
    assert!(prompt.contains("the user chose refactor"), "{prompt}");
}

#[tokio::test]
async fn create_without_a_kind_takes_the_polished_kind() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    f.h.script(vec![pass1("Faster builds", 1)]);
    let i = f.text_intent(&repo).await;
    assert_eq!(i.content.kind, IntentKind::Task);
    assert_eq!(i.seed.kind, None);
    let q = f.wait_phase(i.id, Phase::Questions).await;
    assert_eq!(q.content.kind, IntentKind::Enhancement);
}

#[tokio::test]
async fn a_stored_task_intent_still_loads() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let i = f.text_intent(&repo).await;
    assert_eq!(i.content.kind, IntentKind::Task);
    f.store.save_intent(&i).await.unwrap();
    let got = f.store.get_intent(i.id).await.unwrap().unwrap();
    assert_eq!(got.content.kind, IntentKind::Task);
    assert_eq!(got.seed.kind, None);
    let seed_json = serde_json::to_string(&got.seed).unwrap();
    assert!(!seed_json.contains("kind"), "{seed_json}");
    assert_eq!(
        f.svc.get(i.id).await.unwrap().content.kind,
        IntentKind::Task
    );
}

// -------------------------------------------------------------- base branch

/// Why a start fails when the stored base branch is not a usable branch name.
const BASE_INVALID: &str = "the branch this intent starts from no longer exists";
const FEEDBACK_X_MISSING: &str =
    "the base branch feedback/x no longer exists, so the intent cannot start from it";

/// Runs git with a neutral identity and the C locale: its trimmed stdout,
/// or `None` when it fails.
fn git_stdout(cwd: &Path, args: &[&str]) -> Option<String> {
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
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_out(cwd: &Path, args: &[&str]) -> String {
    git_stdout(cwd, args).unwrap_or_else(|| panic!("git {args:?} failed"))
}

impl Fx {
    /// A repository whose branch `feedback/x` adds `docs/issues/x/issue.md`
    /// on top of `main`, as Submit leaves it; `main` stays checked out.
    async fn feedback_repo(&self) -> (Repo, PathBuf) {
        let dir = self.tmp.path().join("r1");
        common::init_repo(&dir);
        common::git(&dir, &["switch", "-c", "feedback/x"]);
        std::fs::create_dir_all(dir.join("docs/issues/x")).unwrap();
        std::fs::write(dir.join("docs/issues/x/issue.md"), "# Fix the zoom\n").unwrap();
        common::git(&dir, &["add", "--", "docs/issues/x/issue.md"]);
        common::git(&dir, &["commit", "-m", "docs(issues): Fix the zoom"]);
        common::git(&dir, &["switch", "main"]);
        (
            self.agents
                .add_repo(&dir, AddSourceControl::Git)
                .await
                .unwrap(),
            dir,
        )
    }

    /// A text intent created with `base_branch` and `origin` as Submit
    /// creates it, polished by the fake harness and reviewed to `Complete`.
    async fn complete_intent(
        &self,
        repo: &Repo,
        base_branch: Option<&str>,
        origin: Option<FeedbackOrigin>,
    ) -> Intent {
        self.h.script(vec![pass1("Fix the zoom", 0), pass2(&[])]);
        let extras = CreateExtras {
            base_branch: base_branch.map(Into::into),
            origin: origin.clone(),
            ..CreateExtras::default()
        };
        let i = self
            .svc
            .create_with(
                CreateIntentRequest {
                    kind: None,
                    repo_id: Some(repo.id),
                    seed: SeedInput::Text("make it faster".into()),
                },
                extras,
            )
            .await
            .unwrap();
        assert_eq!(i.base_branch.as_deref(), base_branch);
        assert_eq!(i.origin, origin);
        let r = self.wait_phase(i.id, Phase::Review).await;
        let c = &r.content;
        let items: Vec<ItemId> = c
            .preconditions
            .iter()
            .chain(&c.criteria)
            .chain(&c.stories)
            .map(|x| x.id)
            .collect();
        for item in items {
            self.svc
                .review(i.id, item, ReviewAction::Accept)
                .await
                .unwrap();
        }
        let done = self.svc.get(i.id).await.unwrap();
        assert_eq!(done.state, IntentState::Complete);
        // Polish and review kept them.
        assert_eq!(done.base_branch.as_deref(), base_branch);
        assert_eq!(done.origin, origin);
        done
    }

    async fn first_message(&self, agent: &Agent) -> String {
        let messages = self.store.list_messages(agent.id, 0, 10).await.unwrap();
        assert_eq!(messages[0].role, Role::User);
        messages[0].text.clone()
    }

    /// No agent, no `intent/*` branch and no worktree exist.
    async fn assert_nothing_started(&self, dir: &Path) {
        assert!(self.store.list_agents(true).await.unwrap().is_empty());
        assert_eq!(git_out(dir, &["branch", "--list", "intent/*"]), "");
        let worktrees = git_out(dir, &["worktree", "list", "--porcelain"]);
        assert_eq!(worktrees.matches("worktree ").count(), 1, "{worktrees}");
        // No repository has a worktree folder with anything in it.
        for repo_dir in std::fs::read_dir(self.paths.worktrees_dir()).unwrap() {
            let repo_dir = repo_dir.unwrap().path();
            let left: Vec<_> = std::fs::read_dir(&repo_dir).unwrap().collect();
            assert!(left.is_empty(), "{left:?} in {}", repo_dir.display());
        }
    }
}

#[tokio::test]
async fn send_starts_the_agent_from_the_base_branch() {
    let f = real_fx().await;
    let (repo, dir) = f.feedback_repo().await;
    let feedback_tip = git_out(&dir, &["rev-parse", "refs/heads/feedback/x"]);
    let origin = FeedbackOrigin {
        folder: "docs/issues/x".into(),
        branch: "feedback/x".into(),
        commit: feedback_tip.clone(),
        idea: None,
    };
    let x = f
        .complete_intent(&repo, Some("feedback/x"), Some(origin))
        .await;

    let sent = f.svc.send(x.id).await.unwrap();

    assert_eq!(sent.state, IntentState::Sent);
    assert_eq!(sent.last_start_error, None);
    let agent = f
        .store
        .get_agent(sent.agent_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(agent.branch, Some(format!("intent/{}", x.slug)));
    assert_eq!(agent.branch.as_deref(), Some("intent/fix-the-zoom"));
    assert!(agent.worktree_path.join("docs/issues/x/issue.md").is_file());
    let ancestor = [
        "merge-base",
        "--is-ancestor",
        "refs/heads/feedback/x",
        agent.branch.as_deref().unwrap(),
    ];
    assert!(git_stdout(&dir, &ancestor).is_some());
    // The feedback branch and the user's checkout are left as they were.
    assert_eq!(
        git_out(&dir, &["rev-parse", "refs/heads/feedback/x"]),
        feedback_tip
    );
    assert_eq!(git_out(&dir, &["branch", "--show-current"]), "main");
    // The brief tells the agent where the evidence is.
    let first = f.first_message(&agent).await;
    assert!(
        first.contains("## Evidence\n\nThe feedback files are in `docs/issues/x/` on this branch."),
        "{first}"
    );
}

#[tokio::test]
async fn a_missing_base_branch_is_a_start_error() {
    let f = real_fx().await;
    let (repo, dir) = f.feedback_repo().await;
    let x = f.complete_intent(&repo, Some("feedback/x"), None).await;
    let tip = git_out(&dir, &["rev-parse", "refs/heads/feedback/x"]);
    common::git(&dir, &["branch", "-D", "feedback/x"]);

    let err = f.svc.send(x.id).await.unwrap_err();

    assert_eq!(err.code(), "invalid");
    assert_eq!(err.to_string(), FEEDBACK_X_MISSING);
    let now = f.svc.get(x.id).await.unwrap();
    assert_eq!(now.state, IntentState::Complete);
    assert_eq!(now.agent_id, None);
    assert_eq!(
        now.last_start_error.as_deref(),
        Some("could not start the agent: the base branch feedback/x no longer exists, so the intent cannot start from it")
    );
    assert!(!f.versions_of(x.id).await.contains(&VersionAuthor::Send));
    // No silent start from the default branch.
    f.assert_nothing_started(&dir).await;

    // With the branch back, the same intent starts from it.
    common::git(&dir, &["branch", "feedback/x", &tip]);
    let sent = f.svc.send(x.id).await.unwrap();
    assert_eq!(sent.state, IntentState::Sent);
    assert_eq!(sent.last_start_error, None);
    let agent = f
        .store
        .get_agent(sent.agent_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(agent.worktree_path.join("docs/issues/x/issue.md").is_file());
}

#[tokio::test]
async fn an_intent_without_a_base_branch_starts_from_the_default_branch() {
    let f = real_fx().await;
    let (repo, dir) = f.feedback_repo().await;
    let x = f.complete_intent(&repo, None, None).await;

    let sent = f.svc.send(x.id).await.unwrap();

    assert_eq!(sent.state, IntentState::Sent);
    assert_eq!(sent.last_start_error, None);
    let agent = f
        .store
        .get_agent(sent.agent_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(agent.branch.as_deref(), Some("intent/fix-the-zoom"));
    assert!(!agent.worktree_path.join("docs").exists());
    assert_eq!(
        git_out(&dir, &["rev-parse", agent.branch.as_deref().unwrap()]),
        git_out(&dir, &["rev-parse", "refs/heads/main"])
    );
    assert!(!f.first_message(&agent).await.contains("## Evidence"));
}

/// Writes each name straight to the store on purpose: it stands for a stored
/// base branch that went bad after the intent was made (the creation path
/// stores the branch Submit has just committed), so what is tested is the
/// check at start, which must hold whatever the row says.
#[tokio::test]
async fn an_unusable_base_branch_name_is_refused_and_nothing_starts() {
    let f = real_fx().await;
    let (repo, dir) = f.feedback_repo().await;
    let x = f.complete_intent(&repo, None, None).await;
    let refused = [
        // Refused before git sees the name.
        "",
        "-b",
        "--orphan=x",
        "feedback x",
        "feedback/x\n",
        "feedback\tx",
        "feedback/x\u{7f}",
        "\u{1b}[31mfeedback/x",
        "feedback/x..main",
        "../feedback/x",
        "feedback/x@{1}",
        "@{-1}",
        "@",
        "feedback/",
        "feedback/x.lock",
        // Refused by `git check-ref-format --branch`. Several of these name
        // a commit when git reads them as a revision.
        "feedback/x~1",
        "feedback/x^",
        "feedback/x:docs",
        "feedback/x?",
        "feedback/*",
        "feedback/[x",
        "feedback//x",
        "/feedback/x",
        "feedback/.x",
        "feedback/x.",
        "feedback\\x",
    ];
    let recorded = format!("could not start the agent: {BASE_INVALID}");
    for name in refused {
        let mut i = f.svc.get(x.id).await.unwrap();
        i.base_branch = Some(name.into());
        f.store.save_intent(&i).await.unwrap();

        let err = f.svc.send(x.id).await.unwrap_err();

        assert_eq!(
            (err.code(), err.to_string().as_str()),
            ("invalid", BASE_INVALID),
            "{name:?}"
        );
        let now = f.svc.get(x.id).await.unwrap();
        assert_eq!(now.state, IntentState::Complete, "{name:?}");
        assert_eq!(
            now.last_start_error.as_deref(),
            Some(recorded.as_str()),
            "{name:?}"
        );
    }
    f.assert_nothing_started(&dir).await;
}

#[tokio::test]
async fn a_base_branch_is_resolved_locally_and_nothing_is_fetched() {
    let f = real_fx().await;
    let (repo, dir) = f.feedback_repo().await;
    let x = f.complete_intent(&repo, Some("feedback/x"), None).await;
    let tip = git_out(&dir, &["rev-parse", "refs/heads/feedback/x"]);
    // `origin` (a bare repository made here) has the branch; locally only a
    // tag and a remote-tracking ref of that name are left.
    let bare = f.tmp.path().join("origin.git");
    let bare_arg = bare.to_str().unwrap();
    let dir_arg = dir.to_str().unwrap();
    common::git(f.tmp.path(), &["clone", "--bare", dir_arg, bare_arg]);
    common::git(&dir, &["remote", "add", "origin", bare_arg]);
    common::git(
        &dir,
        &["update-ref", "refs/remotes/origin/feedback/x", &tip],
    );
    common::git(&dir, &["tag", "feedback/x", &tip]);
    common::git(&dir, &["branch", "-D", "feedback/x"]);

    let err = f.svc.send(x.id).await.unwrap_err();
    assert_eq!(err.to_string(), FEEDBACK_X_MISSING);
    f.assert_nothing_started(&dir).await;

    // Without the remote-tracking ref nothing is fetched to replace it.
    common::git(
        &dir,
        &["update-ref", "-d", "refs/remotes/origin/feedback/x"],
    );
    let err = f.svc.send(x.id).await.unwrap_err();
    assert_eq!(err.to_string(), FEEDBACK_X_MISSING);
    assert_eq!(git_out(&dir, &["for-each-ref", "refs/remotes"]), "");
    assert!(!dir.join(".git/FETCH_HEAD").exists());
    assert_eq!(f.svc.get(x.id).await.unwrap().state, IntentState::Complete);
    f.assert_nothing_started(&dir).await;
}

// ------------------------------------------------------------ fixed items

/// What [`IntentService::create_with`] is given for an intent with fixed
/// items (an idea package's criteria, an implementation intent's
/// dependency).
fn fixed_extras(criteria: &[&str], dependency: Option<IntentId>) -> CreateExtras {
    CreateExtras {
        fixed_criteria: criteria.iter().map(|c| (*c).to_owned()).collect(),
        fixed_dependency: dependency,
        ..CreateExtras::default()
    }
}

fn text_request(repo: &Repo) -> CreateIntentRequest {
    CreateIntentRequest {
        kind: None,
        repo_id: Some(repo.id),
        seed: SeedInput::Text("make it faster".into()),
    }
}

#[tokio::test]
async fn fixed_items_are_accepted_from_the_start_and_kept_by_polish() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    let seeder = f.seeder();
    let target = seeder.create(text_request(&repo)).await.unwrap();
    let mut t = f.store.get_intent(target.id).await.unwrap().unwrap();
    t.content.title = "Prepare the zoom idea".into();
    f.store.save_intent(&t).await.unwrap();
    let other = seeder.create(text_request(&repo)).await.unwrap();
    f.h.script(vec![
        pass1("Implement the zoom idea", 0),
        pass2(&[target.id, other.id]),
    ]);

    let i = f
        .svc
        .create_with(
            text_request(&repo),
            fixed_extras(&["Fixed one.", "Fixed two."], Some(target.id)),
        )
        .await
        .unwrap();

    let fixed = |text: &str, intent_ref: Option<IntentId>, item: &Item| {
        assert_eq!(item.text, text);
        assert_eq!(item.status, ItemStatus::Accepted);
        assert!(item.fixed && !item.edited);
        assert_eq!(item.intent_ref, intent_ref);
    };
    assert_eq!(i.content.criteria.len(), 2);
    fixed("Fixed one.", None, &i.content.criteria[0]);
    fixed("Fixed two.", None, &i.content.criteria[1]);
    assert_eq!(i.content.depends_on.len(), 1);
    fixed(
        "Prepare the zoom idea",
        Some(target.id),
        &i.content.depends_on[0],
    );
    // The first version holds them too.
    let versions = f.svc.versions(i.id).await.unwrap();
    assert_eq!(versions[0].content.criteria, i.content.criteria);

    let r = f.wait_phase(i.id, Phase::Review).await;
    assert_eq!(r.content.criteria[..2], i.content.criteria[..]);
    assert_eq!(r.content.criteria.len(), 3);
    assert_eq!(r.content.criteria[2].status, ItemStatus::Pending);
    // The model proposed the target again: it is there once, fixed.
    assert_eq!(r.content.depends_on.len(), 2, "{:?}", r.content.depends_on);
    assert_eq!(r.content.depends_on[0], i.content.depends_on[0]);
    assert_eq!(r.content.depends_on[1].intent_ref, Some(other.id));
    assert_eq!(r.content.depends_on[1].status, ItemStatus::Pending);
}

#[tokio::test]
async fn a_fixed_item_cannot_be_edited_or_dropped() {
    let f = real_fx().await;
    let repo = f.repo("r1").await;
    let target = f.seeder().create(text_request(&repo)).await.unwrap();
    f.h.script(vec![pass1("Implement it", 0), pass2(&[])]);
    let i = f
        .svc
        .create_with(
            text_request(&repo),
            fixed_extras(&["Fixed one."], Some(target.id)),
        )
        .await
        .unwrap();
    let r = f.wait_phase(i.id, Phase::Review).await;
    let fixed_items = [r.content.criteria[0].id, r.content.depends_on[0].id];

    for item in fixed_items {
        for action in [ReviewAction::Drop, ReviewAction::Edit("Other text.".into())] {
            let err = f.svc.review(i.id, item, action.clone()).await.unwrap_err();
            assert_eq!(
                (err.code(), err.to_string().as_str()),
                (
                    "invalid",
                    "this item is fixed: it cannot be edited or dropped"
                ),
                "{action:?}"
            );
            assert_eq!(f.svc.get(i.id).await.unwrap(), r, "{action:?}");
        }
        // Accepting it again changes nothing.
        let again = f
            .svc
            .review(i.id, item, ReviewAction::Accept)
            .await
            .unwrap();
        assert_eq!(again.content, r.content);
    }

    // The rest is reviewed as usual and completes the intent.
    let c = &r.content;
    let rest: Vec<ItemId> = c
        .preconditions
        .iter()
        .chain(&c.criteria[1..])
        .chain(&c.stories)
        .map(|x| x.id)
        .collect();
    for item in rest {
        f.svc
            .review(i.id, item, ReviewAction::Accept)
            .await
            .unwrap();
    }
    let done = f.svc.get(i.id).await.unwrap();
    assert_eq!(done.state, IntentState::Complete);
    assert_eq!(done.content.criteria[0], r.content.criteria[0]);
    assert_eq!(done.content.depends_on, r.content.depends_on);

    // A PATCH of the other fields leaves them.
    let patched = f
        .svc
        .update(
            i.id,
            Patch {
                title: Some("Renamed".into()),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(patched.content.criteria, done.content.criteria);
    assert_eq!(patched.content.depends_on, done.content.depends_on);
}

#[tokio::test]
async fn a_fixed_dependency_must_be_an_intent_of_the_same_repository() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let elsewhere = f.repo("r2").await;
    let there = f.seeder().create(text_request(&elsewhere)).await.unwrap();

    let err = f
        .svc
        .create_with(
            text_request(&repo),
            fixed_extras(&[], Some(IntentId::new())),
        )
        .await
        .unwrap_err();
    assert_eq!(
        (err.code(), err.to_string().as_str()),
        ("invalid", "the intent this one depends on does not exist")
    );
    let err = f
        .svc
        .create_with(text_request(&repo), fixed_extras(&[], Some(there.id)))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "dependency_cross_repo");
    let mine = f
        .svc
        .list(IntentFilter {
            state: None,
            repo_id: Some(repo.id),
        })
        .await
        .unwrap();
    assert!(mine.is_empty(), "nothing was created");
    assert!(f.runner.0.lock().unwrap().is_empty(), "no polish started");
}

#[tokio::test]
async fn a_fixed_dependency_keeps_the_intent_in_its_repository() {
    let f = fx().await;
    let repo = f.repo("r1").await;
    let other = f.repo("r2").await;
    let target = f.seeder().create(text_request(&repo)).await.unwrap();
    let i = f
        .svc
        .create_with(text_request(&repo), fixed_extras(&[], Some(target.id)))
        .await
        .unwrap();
    f.finish_polish(i.id, IntentState::Draft).await;
    let before = f.svc.get(i.id).await.unwrap();
    assert!(before.origin.is_none(), "refused without an origin too");
    let move_it = || Patch {
        repo_id: Some(other.id),
        ..Patch::default()
    };

    let err = f.svc.update(i.id, move_it()).await.unwrap_err();

    assert_eq!(
        (err.status_and_code(), err.to_string().as_str()),
        (
            (axum::http::StatusCode::CONFLICT, "fixed_dependency"),
            "This intent depends on another intent in its repository, so the repository cannot be changed."
        )
    );
    assert_eq!(f.svc.get(i.id).await.unwrap(), before, "nothing changed");

    // With an origin as well, the fixed dependency is the reason given.
    let mut with_origin = before.clone();
    with_origin.origin = Some(FeedbackOrigin {
        folder: "docs/issues/x".into(),
        branch: "feedback/x".into(),
        commit: "0".repeat(40),
        idea: None,
    });
    f.store.save_intent(&with_origin).await.unwrap();
    let err = f.svc.update(i.id, move_it()).await.unwrap_err();
    assert_eq!(err.code(), "fixed_dependency");

    // Its own repository is no change, and the other fields still change.
    let renamed = f
        .svc
        .update(
            i.id,
            Patch {
                repo_id: Some(repo.id),
                title: Some("Renamed".into()),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(renamed.content.title, "Renamed");
    assert_eq!(renamed.content.depends_on, before.content.depends_on);
}

#[tokio::test]
async fn a_base_branch_that_cannot_be_checked_out_is_a_fixed_sentence() {
    // A-M7: git's words (here a path) never reach the start error. Both
    // worktree directories the start tries are in the way.
    let f = real_fx().await;
    let (repo, _dir) = f.feedback_repo().await;
    let x = f.complete_intent(&repo, Some("feedback/x"), None).await;
    let hex = x.id.0.simple().to_string();
    for name in [
        "intent-fix-the-zoom".to_owned(),
        format!("intent-fix-the-zoom-{}", &hex[..6]),
    ] {
        let taken = f.paths.repo_worktrees_dir(&repo).join(name);
        std::fs::create_dir_all(&taken).unwrap();
        std::fs::write(taken.join("LEAK.txt"), "in the way").unwrap();
    }

    let err = f.svc.send(x.id).await.unwrap_err();

    assert_eq!(err.code(), "base_branch_failed");
    assert_eq!(
        err.to_string(),
        "The intent's base branch could not be checked out."
    );
    let now = f.svc.get(x.id).await.unwrap();
    assert_eq!(now.state, IntentState::Complete);
    assert_eq!(
        now.last_start_error.as_deref(),
        Some("could not start the agent: The intent's base branch could not be checked out.")
    );
    assert!(f.store.list_agents(true).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_agent_branch_tracks_nothing_whatever_the_repository_says() {
    // D-M4: with `branch.autoSetupMerge=always` the agent's bare `git push`
    // would otherwise go to the local base branch.
    let f = real_fx().await;
    let (repo, dir) = f.feedback_repo().await;
    common::git(&dir, &["config", "branch.autoSetupMerge", "always"]);
    let x = f.complete_intent(&repo, Some("feedback/x"), None).await;

    let sent = f.svc.send(x.id).await.unwrap();

    let agent = f
        .store
        .get_agent(sent.agent_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    for key in ["remote", "merge"] {
        let name = format!("branch.{}.{key}", agent.branch.as_deref().unwrap());
        assert_eq!(
            git_stdout(&dir, &["config", "--get", &name]),
            None,
            "{name}"
        );
    }
}

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::{json, Value};
use smithagents_core::agent::{Agent, Repo, Role};
use smithagents_core::event::EventKind;
use smithagents_core::ids::{IntentId, ItemId, QuestionId, RepoId};
use smithagents_core::intent::{
    Answer, AnswerInput, CreateIntentRequest, Intent, IntentFilter, IntentKind, IntentState, Item,
    ItemStatus, Patch, Phase, SeedInput, SeedSource, VersionAuthor,
};
use smithagents_server::events::{EventBus, Subscription};
use smithagents_server::harness::fake::FakeHarness;
use smithagents_server::harness::{
    Harness, HarnessError, HarnessEvent, StructuredRequest, StructuredResult, TurnRequest,
};
use smithagents_server::intents::{IntentError, IntentService, PolishRunner};
use smithagents_server::paths::Paths;
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
    panics: AtomicUsize,
    gated: AtomicBool,
    gate: Semaphore,
}

impl TestHarness {
    fn new() -> Self {
        TestHarness {
            fake: FakeHarness::scripted(vec![]),
            panics: AtomicUsize::new(0),
            gated: AtomicBool::new(false),
            gate: Semaphore::new(0),
        }
    }

    fn script(&self, results: Vec<Result<Value, HarnessError>>) {
        self.fake.script_structured(results);
    }

    fn calls(&self) -> Vec<StructuredRequest> {
        self.fake.structured_calls()
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
        if self.gated.load(Ordering::SeqCst) {
            self.gate.acquire().await.unwrap().forget();
        }
        if self
            .panics
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            panic!("test harness panic inside run_structured");
        }
        self.fake.run_structured(req).await
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
    build(false).await
}

/// Service with its real background polish runner over [`TestHarness`].
async fn real_fx() -> Fx {
    build(true).await
}

async fn build(real_runner: bool) -> Fx {
    let tmp = TempDir::new().unwrap();
    let store = Store::open_in_memory().await.unwrap();
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
        self.agents.add_repo(&dir).await.unwrap()
    }

    async fn agent(&self, repo: &Repo) -> Agent {
        self.agents.create_agent(repo.id).await.unwrap()
    }

    async fn text_intent(&self, repo: &Repo) -> smithagents_core::intent::Intent {
        self.svc
            .create(CreateIntentRequest {
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
            repo_id: Some(repo.id),
            seed: SeedInput::Text("  \n ".into()),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
    let err = f
        .svc
        .create(CreateIntentRequest {
            repo_id: Some(RepoId::new()),
            seed: SeedInput::Text("hi".into()),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::RepoNotFound), "{err:?}");
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
    assert_eq!(f.h.calls().len(), 2, "pass 2 ran more than once");
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
    for id in ids {
        slugs.push(f.wait_phase(id, Phase::Questions).await.slug);
    }
    slugs.sort();
    assert_eq!(
        slugs,
        ["same-title", "same-title-2", "same-title-3", "same-title-4"]
    );
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
    )
    .with_polish_model(Some("haiku".into()));
    f.h.script(vec![pass1("T", 0), pass2(&[])]);
    let i = svc
        .create(CreateIntentRequest {
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
    assert!(f.h.calls().len() == 1, "no pass 2 while a question is open");
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
    assert_eq!(f.h.calls().len(), 2, "pass 2 must run exactly once");
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
    f.h.release(1);
    f.wait_phase(i.id, Phase::Review).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(f.h.calls().len(), 2);
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
    assert_eq!(f.h.calls().len(), 2);
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

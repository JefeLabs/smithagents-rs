//! Send, the dependency-aware queue scheduler, Resolve and the queue limit,
//! over real git worktrees with a harness whose turns the tests control.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{self, BoxStream, StreamExt};
use smithagents_core::agent::{AddSourceControl, Agent, AgentState, ChatMessage, Repo, Role};
use smithagents_core::event::EventKind;
use smithagents_core::ids::{AgentId, IntentId, ItemId};
use smithagents_core::intent::{
    AgentDefaults, Content, Intent, IntentKind, IntentState, IntentVersion, Item, ItemStatus,
    Phase, Seed, SeedSource, VersionAuthor,
};
use smithagents_server::brief::{compose, DepSummary};
use smithagents_server::events::EventBus;
use smithagents_server::git::GitError;
use smithagents_server::harness::{
    Harness, HarnessError, HarnessEvent, StructuredRequest, StructuredResult, TurnRequest,
};
use smithagents_server::intents::{IntentError, IntentService, PolishRunner};
use smithagents_server::paths::Paths;
use smithagents_server::queue::{Scheduler, SchedulerHandle};
use smithagents_server::service::{AgentService, CreateAgentOptions, ServiceError};
use smithagents_server::store::Store;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::sync::Semaphore;

/// Upper bound for anything the tests wait on; only guards against a hang.
const WAIT: Duration = Duration::from_secs(10);
/// A `quiet_after` no test reaches unless it means to.
const NEVER_QUIET: Duration = Duration::from_secs(3600);

/// Turn harness under test control: every turn is one event, `TurnCompleted`
/// (or `Failed` after [`Self::die_next`]). While [`Self::hold`] is on, a
/// turn started from then waits for a [`Self::release`] before that event,
/// so its agent stays `Working` (or `Quiet`).
struct GateHarness {
    held: AtomicBool,
    gate: Arc<Semaphore>,
    die: Arc<AtomicUsize>,
    /// Turns the harness was asked to run.
    turns: AtomicUsize,
}

impl GateHarness {
    fn new() -> Self {
        GateHarness {
            held: AtomicBool::new(false),
            gate: Arc::new(Semaphore::new(0)),
            die: Arc::new(AtomicUsize::new(0)),
            turns: AtomicUsize::new(0),
        }
    }

    /// Waits until the harness has been asked to run `n` turns, so a hold
    /// or [`Self::unhold`] after it no longer changes theirs.
    async fn wait_turns(&self, n: usize) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while self.turns.load(Ordering::SeqCst) < n {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out; {} turns asked for, expected {n}",
                self.turns.load(Ordering::SeqCst)
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn hold(&self) {
        self.held.store(true, Ordering::SeqCst);
    }

    fn release(&self, turns: usize) {
        self.gate.add_permits(turns);
    }

    /// Turns started from now on end at once again.
    fn unhold(&self) {
        self.held.store(false, Ordering::SeqCst);
    }

    /// The next turn to pass the gate ends with `Failed` (its agent `Dead`).
    fn die_next(&self) {
        self.die.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl Harness for GateHarness {
    async fn run_turn(
        &self,
        _req: TurnRequest,
    ) -> Result<BoxStream<'static, HarnessEvent>, HarnessError> {
        let gate = self.held.load(Ordering::SeqCst).then(|| self.gate.clone());
        self.turns.fetch_add(1, Ordering::SeqCst);
        let die = self.die.clone();
        Ok(stream::once(async move {
            if let Some(gate) = gate {
                gate.acquire().await.unwrap().forget();
            }
            if die
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                HarnessEvent::Failed {
                    message: "the agent died".into(),
                }
            } else {
                HarnessEvent::TurnCompleted {
                    session_id: "s1".into(),
                    is_error: false,
                    result_text: Some("done".into()),
                }
            }
        })
        .boxed())
    }

    async fn run_structured(
        &self,
        _req: StructuredRequest,
    ) -> Result<StructuredResult, HarnessError> {
        Err(HarnessError::Failed("not used by these tests".into()))
    }
}

struct NoPolish;

impl PolishRunner for NoPolish {
    fn start(&self, _id: IntentId, _pass: u8) {}
}

struct Fx {
    tmp: TempDir,
    db: PathBuf,
    store: Store,
    bus: EventBus,
    paths: Paths,
    h: Arc<GateHarness>,
    agents: AgentService,
    svc: IntentService,
    sched: Option<SchedulerHandle>,
    repo: Repo,
    repo_dir: PathBuf,
}

async fn fx() -> Fx {
    fx_with(NEVER_QUIET).await
}

/// A file database (as the daemon uses), one registered repository, an
/// intent service and a running scheduler.
async fn fx_with(quiet_after: Duration) -> Fx {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("smithagents.db");
    let store = Store::open(&db).await.unwrap();
    let bus = EventBus::new(4096);
    let paths = Paths {
        home: tmp.path().join("home"),
    };
    paths.ensure_dirs().unwrap();
    let h = Arc::new(GateHarness::new());
    let agents = AgentService::new(
        store.clone(),
        bus.clone(),
        h.clone(),
        paths.clone(),
        quiet_after,
    );
    let repo_dir = tmp.path().join("repo");
    common::init_repo(&repo_dir);
    let repo = agents
        .add_repo(&repo_dir, AddSourceControl::Git)
        .await
        .unwrap();
    let svc = service(&store, &bus, &h, &agents, &paths);
    let sched = Scheduler::start(svc.clone(), agents.clone(), store.clone(), bus.clone());
    Fx {
        tmp,
        db,
        store,
        bus,
        paths,
        h,
        agents,
        svc,
        sched: Some(sched),
        repo,
        repo_dir,
    }
}

fn service(
    store: &Store,
    bus: &EventBus,
    h: &Arc<GateHarness>,
    agents: &AgentService,
    paths: &Paths,
) -> IntentService {
    IntentService::new(
        store.clone(),
        bus.clone(),
        h.clone(),
        agents.clone(),
        paths.clone(),
    )
    .with_polish_runner(Arc::new(NoPolish))
}

fn accepted(text: &str, target: Option<IntentId>) -> Item {
    Item {
        id: ItemId::new(),
        text: text.into(),
        status: ItemStatus::Accepted,
        edited: false,
        intent_ref: target,
        fixed: false,
    }
}

fn hex6(id: IntentId) -> String {
    id.0.simple().to_string()[..6].to_string()
}

async fn raw_db(path: &Path) -> SqlitePool {
    SqlitePool::connect_with(SqliteConnectOptions::new().filename(path))
        .await
        .unwrap()
}

impl Fx {
    fn sched(&self) -> &SchedulerHandle {
        self.sched.as_ref().expect("scheduler was shut down")
    }

    /// Inserts a `Complete` intent with one accepted criterion and an
    /// accepted depends-on item per entry of `deps`.
    async fn intent(&self, slug: &str, deps: &[IntentId]) -> Intent {
        self.intent_on(&self.repo, slug, deps).await
    }

    /// [`Self::intent`], on `repo`.
    async fn intent_on(&self, repo: &Repo, slug: &str, deps: &[IntentId]) -> Intent {
        let content = Content {
            title: format!("Title of {slug}"),
            kind: IntentKind::Task,
            current_behavior: format!("{slug} is slow today"),
            questions: vec![],
            preconditions: vec![],
            criteria: vec![accepted(&format!("{slug} is fast"), None)],
            stories: vec![],
            depends_on: deps
                .iter()
                .map(|d| accepted("must land first", Some(*d)))
                .collect(),
        };
        self.insert_on(repo, slug, IntentState::Complete, content)
            .await
    }

    async fn insert(&self, slug: &str, state: IntentState, content: Content) -> Intent {
        self.insert_on(&self.repo, slug, state, content).await
    }

    async fn insert_on(
        &self,
        repo: &Repo,
        slug: &str,
        state: IntentState,
        content: Content,
    ) -> Intent {
        let now = Utc::now();
        let intent = Intent {
            id: IntentId::new(),
            repo_id: repo.id,
            slug: slug.into(),
            state,
            phase: Phase::Review,
            seed: Seed {
                kind: None,
                source: SeedSource::Text,
                text: "seed".into(),
            },
            content: content.clone(),
            agent_id: None,
            created_at: now,
            updated_at: now,
            queued_at: None,
            sent_at: None,
            resolved_at: None,
            last_start_error: None,
            base_branch: None,
            origin: None,
        };
        let first = IntentVersion {
            intent_id: intent.id,
            version: 1,
            author: VersionAuthor::Seed,
            content,
            at: now,
        };
        self.store.insert_intent(&intent, &first).await.unwrap();
        intent
    }

    async fn get(&self, id: IntentId) -> Intent {
        self.store.get_intent(id).await.unwrap().unwrap()
    }

    /// An open agent of `intent` on its branch, as a start leaves it when the
    /// daemon stops between creating the agent and saving the intent `Sent`.
    async fn stray_agent(&self, intent: &Intent) -> Agent {
        self.agents
            .create_agent_with(
                self.repo.id,
                CreateAgentOptions {
                    branch: Some(format!("intent/{}", intent.slug)),
                    intent_id: Some(intent.id),
                    system_prompt_append: None,
                    base: None,
                },
            )
            .await
            .unwrap()
    }

    async fn state(&self, id: IntentId) -> IntentState {
        self.get(id).await.state
    }

    async fn agent(&self, id: AgentId) -> Agent {
        self.store.get_agent(id).await.unwrap().unwrap()
    }

    async fn all_agents(&self) -> Vec<Agent> {
        self.store.list_agents(true).await.unwrap()
    }

    async fn agents_for(&self, id: IntentId) -> Vec<Agent> {
        self.all_agents()
            .await
            .into_iter()
            .filter(|a| a.intent_id == Some(id))
            .collect()
    }

    async fn messages(&self, agent: AgentId) -> Vec<ChatMessage> {
        self.store.list_messages(agent, 0, 100).await.unwrap()
    }

    async fn authors(&self, id: IntentId) -> Vec<VersionAuthor> {
        self.svc
            .versions(id)
            .await
            .unwrap()
            .into_iter()
            .map(|v| v.author)
            .collect()
    }

    /// Marks an intent `Resolved` directly in the store, without any call
    /// that would wake the scheduler.
    async fn mark_resolved(&self, id: IntentId) {
        let mut i = self.get(id).await;
        i.state = IntentState::Resolved;
        i.resolved_at = Some(Utc::now());
        self.store.save_intent(&i).await.unwrap();
    }

    /// Polls the store until `pred` holds for the intent.
    async fn wait_intent(&self, id: IntentId, pred: impl Fn(&Intent) -> bool) -> Intent {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let i = self.get(id).await;
            if pred(&i) {
                return i;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out; intent {} is {:?}, last_start_error {:?}",
                i.slug,
                i.state,
                i.last_start_error
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn wait_sent(&self, id: IntentId) -> Intent {
        self.wait_intent(id, |i| i.state == IntentState::Sent).await
    }

    async fn wait_agent(&self, id: AgentId, pred: impl Fn(&Agent) -> bool) -> Agent {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let a = self.agent(id).await;
            if pred(&a) {
                return a;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out; agent {} is {:?}",
                a.name,
                a.state
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// `IntentUpdated` payloads for `id` published after `seq`, in order.
    fn updates_since(&self, seq: u64, id: IntentId) -> Vec<Intent> {
        self.bus
            .subscribe(Some(seq))
            .unwrap()
            .backlog
            .into_iter()
            .filter_map(|e| match e.kind {
                EventKind::IntentUpdated(i) if i.id == id => Some(i),
                _ => None,
            })
            .collect()
    }

    /// Every agent worktree, whichever repository it belongs to.
    fn worktree_dirs(&self) -> Vec<PathBuf> {
        let Ok(repos) = std::fs::read_dir(self.paths.worktrees_dir()) else {
            return vec![];
        };
        repos
            .flat_map(|repo| std::fs::read_dir(repo.unwrap().path()).unwrap())
            .map(|entry| entry.unwrap().path())
            .collect()
    }
}

fn is_invalid_state(err: &IntentError, actual: IntentState) -> bool {
    matches!(err, IntentError::InvalidState { actual: a, .. } if *a == actual)
}

// --------------------------------------------------------------------- send

#[tokio::test]
async fn send_starts_an_agent_with_the_brief() {
    let f = fx().await;
    let x = f.intent("fix-login", &[]).await;
    let seq = f.bus.latest_seq();

    let sent = f.svc.send(x.id).await.unwrap();

    assert_eq!(sent.state, IntentState::Sent);
    assert!(sent.sent_at.is_some());
    assert_eq!(sent.last_start_error, None);
    assert_eq!(f.get(x.id).await, sent);
    let agent = f.agent(sent.agent_id.unwrap()).await;
    assert_eq!(agent.name, "Mercury");
    assert_eq!(agent.branch.as_deref(), Some("intent/fix-login"));
    assert_eq!(agent.intent_id, Some(x.id));
    assert!(agent.worktree_path.is_dir());
    let brief = compose(&x, &[], false);
    let first = &f.messages(agent.id).await[0];
    assert_eq!(
        (first.role, first.text.as_str()),
        (Role::User, brief.first_message.as_str())
    );
    assert_eq!(
        f.store
            .agent_system_prompt_append(agent.id)
            .await
            .unwrap()
            .as_deref(),
        Some(brief.system_append.as_str())
    );
    assert_eq!(
        f.authors(x.id).await,
        [VersionAuthor::Seed, VersionAuthor::Send]
    );
    let updates = f.updates_since(seq, x.id);
    assert_eq!(updates.last(), Some(&sent));
}

#[tokio::test]
async fn send_requires_a_complete_intent() {
    let f = fx().await;
    let mut content = f.intent("pending", &[]).await.content;
    content.criteria[0].status = ItemStatus::Pending;
    let draft = f.insert("draft", IntentState::Draft, content).await;

    let err = f.svc.send(draft.id).await.unwrap_err();
    assert!(is_invalid_state(&err, IntentState::Draft), "{err:?}");
    assert_eq!(err.code(), "invalid_state");
    assert_eq!(f.state(draft.id).await, IntentState::Draft);
    assert_eq!(f.authors(draft.id).await, [VersionAuthor::Seed]);
    assert!(f.all_agents().await.is_empty());
}

#[tokio::test]
async fn second_send_is_invalid_state_and_makes_no_second_agent() {
    let f = fx().await;
    let x = f.intent("once", &[]).await;
    f.svc.send(x.id).await.unwrap();

    let err = f.svc.send(x.id).await.unwrap_err();
    assert!(is_invalid_state(&err, IntentState::Sent), "{err:?}");
    assert_eq!(f.all_agents().await.len(), 1);
    assert_eq!(
        f.authors(x.id).await,
        [VersionAuthor::Seed, VersionAuthor::Send]
    );
}

#[tokio::test]
async fn unresolved_dependency_queues_without_an_agent() {
    let f = fx().await;
    let base = f.intent("base", &[]).await;
    let x = f.intent("dependent", &[base.id]).await;
    let seq = f.bus.latest_seq();

    let queued = f.svc.send(x.id).await.unwrap();

    assert_eq!(queued.state, IntentState::Queued);
    assert!(queued.queued_at.is_some());
    assert_eq!(queued.agent_id, None);
    assert_eq!(f.get(x.id).await, queued);
    assert_eq!(
        f.authors(x.id).await,
        [VersionAuthor::Seed, VersionAuthor::Send]
    );
    assert_eq!(f.updates_since(seq, x.id).last(), Some(&queued));
    f.sched().run_pass().await;
    assert_eq!(f.state(x.id).await, IntentState::Queued);
    assert!(f.all_agents().await.is_empty());
}

#[tokio::test]
async fn dropped_dependencies_do_not_hold_an_intent() {
    let f = fx().await;
    let base = f.intent("base", &[]).await;
    let mut x = f.intent("dependent", &[base.id]).await;
    x.content.depends_on[0].status = ItemStatus::Dropped;
    f.store.save_intent(&x).await.unwrap();

    let sent = f.svc.send(x.id).await.unwrap();
    assert_eq!(sent.state, IntentState::Sent);
    // A dropped dependency is not summarized in the brief either.
    let first = &f.messages(sent.agent_id.unwrap()).await[0];
    assert_eq!(first.text, compose(&x, &[], false).first_message);
}

#[tokio::test]
async fn resolving_the_dependency_starts_the_dependent() {
    let f = fx().await;
    let base = f.intent("base", &[]).await;
    let x = f.intent("dependent", &[base.id]).await;
    let base_sent = f.svc.send(base.id).await.unwrap();
    f.wait_agent(base_sent.agent_id.unwrap(), |a| a.state == AgentState::Done)
        .await;
    assert_eq!(f.svc.send(x.id).await.unwrap().state, IntentState::Queued);
    let seq = f.bus.latest_seq();

    let resolved = f.svc.resolve(base.id).await.unwrap();
    assert_eq!(resolved.state, IntentState::Resolved);
    assert!(resolved.resolved_at.is_some());
    assert_eq!(f.updates_since(seq, base.id).last(), Some(&resolved));

    // No other call: the scheduler starts the dependent on its own.
    let sent = f.wait_sent(x.id).await;
    let agent = f.agent(sent.agent_id.unwrap()).await;
    assert_eq!(agent.name, "Venus");
    assert_eq!(agent.intent_id, Some(x.id));
    let dep = DepSummary {
        slug: base.slug.clone(),
        title: base.content.title.clone(),
        current_behavior: base.content.current_behavior.clone(),
    };
    let first = &f.messages(agent.id).await[0];
    assert_eq!(first.text, compose(&x, &[dep], false).first_message);
    assert!(
        first.text.contains("Title of base (base)"),
        "{}",
        first.text
    );
    // The Send version was written once, at Send, not again at the start.
    assert_eq!(
        f.authors(x.id).await,
        [VersionAuthor::Seed, VersionAuthor::Send]
    );
}

// -------------------------------------------------------------------- limit

#[tokio::test]
async fn limit_one_starts_queued_intents_one_at_a_time_in_queued_order() {
    let f = fx_with(Duration::from_millis(50)).await;
    let base = f.intent("base", &[]).await;
    let base_sent = f.svc.send(base.id).await.unwrap();
    f.wait_agent(base_sent.agent_id.unwrap(), |a| a.state == AgentState::Done)
        .await;
    let a = f.intent("a", &[base.id]).await;
    let b = f.intent("b", &[base.id]).await;
    let c = f.intent("c", &[base.id]).await;
    // Queued in the order b, a, c (not the order they were created).
    for x in [&b, &a, &c] {
        assert_eq!(f.svc.send(x.id).await.unwrap().state, IntentState::Queued);
    }
    f.svc
        .set_agent_defaults(AgentDefaults {
            queue_limit: Some(1),
        })
        .await
        .unwrap();
    f.h.hold();

    f.svc.resolve(base.id).await.unwrap();

    let b_sent = f.wait_sent(b.id).await;
    let b_agent = b_sent.agent_id.unwrap();
    // Working, then Quiet: both hold the only slot.
    f.wait_agent(b_agent, |x| x.state == AgentState::Quiet)
        .await;
    f.sched().run_pass().await;
    assert_eq!(f.state(a.id).await, IntentState::Queued);
    assert_eq!(f.state(c.id).await, IntentState::Queued);

    // b's agent finishes (Done): the slot frees and a starts, not c.
    f.h.release(1);
    let a_sent = f.wait_sent(a.id).await;
    assert_eq!(f.agent(b_agent).await.state, AgentState::Done);
    f.sched().run_pass().await;
    assert_eq!(f.state(c.id).await, IntentState::Queued);

    // a's agent dies (Dead): that frees the slot too.
    f.h.die_next();
    f.h.release(1);
    let c_sent = f.wait_sent(c.id).await;
    assert_eq!(
        f.agent(a_sent.agent_id.unwrap()).await.state,
        AgentState::Dead
    );

    assert!(b_sent.sent_at < a_sent.sent_at && a_sent.sent_at < c_sent.sent_at);
    for (sent, name) in [(&b_sent, "Venus"), (&a_sent, "Earth"), (&c_sent, "Mars")] {
        assert_eq!(f.agent(sent.agent_id.unwrap()).await.name, name);
    }
}

/// Polls until exactly `n` agents have a turn in flight.
async fn wait_running(agents: &AgentService, n: usize) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while agents.running_agent_count() != n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out; {} agents running, expected {n}",
            agents.running_agent_count()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn manual_agents_count_but_never_wait() {
    let f = fx().await;
    f.svc
        .set_agent_defaults(AgentDefaults {
            queue_limit: Some(1),
        })
        .await
        .unwrap();
    f.h.hold();
    let manual = f.agents.create_agent(f.repo.id).await.unwrap();
    f.agents
        .send_message(manual.id, "hello".into())
        .await
        .unwrap();
    f.wait_agent(manual.id, |a| a.state == AgentState::Working)
        .await;

    // A replying manual agent takes the only slot: a sent intent queues.
    let x = f.intent("x", &[]).await;
    assert_eq!(f.svc.send(x.id).await.unwrap().state, IntentState::Queued);
    f.sched().run_pass().await;
    assert_eq!(f.state(x.id).await, IntentState::Queued);
    assert!(f.agents_for(x.id).await.is_empty());

    // The manual turn ends: x starts with no other call.
    f.h.release(1);
    let x_sent = f.wait_sent(x.id).await;
    f.wait_agent(x_sent.agent_id.unwrap(), |a| a.state == AgentState::Working)
        .await;

    // With the slot taken by x, the manual agent's next message is still
    // accepted at once (over the limit)...
    f.agents
        .send_message(manual.id, "again".into())
        .await
        .unwrap();
    f.wait_agent(manual.id, |a| a.state == AgentState::Working)
        .await;
    // ...and counts: another intent waits while either turn runs.
    let y = f.intent("y", &[]).await;
    assert_eq!(f.svc.send(y.id).await.unwrap().state, IntentState::Queued);
    f.h.release(1);
    wait_running(&f.agents, 1).await;
    f.sched().run_pass().await;
    assert_eq!(f.state(y.id).await, IntentState::Queued);

    // The last turn ends, whichever it is: y starts.
    f.h.release(1);
    f.wait_sent(y.id).await;
}

#[tokio::test]
async fn a_resolved_intent_whose_agent_still_works_counts() {
    let f = fx().await;
    f.svc
        .set_agent_defaults(AgentDefaults {
            queue_limit: Some(1),
        })
        .await
        .unwrap();
    f.h.hold();
    let x = f.intent("x", &[]).await;
    let agent = f.svc.send(x.id).await.unwrap().agent_id.unwrap();
    f.wait_agent(agent, |a| a.state == AgentState::Working)
        .await;
    f.svc.resolve(x.id).await.unwrap();

    let y = f.intent("y", &[]).await;
    assert_eq!(f.svc.send(y.id).await.unwrap().state, IntentState::Queued);
    f.sched().run_pass().await;
    assert_eq!(f.state(y.id).await, IntentState::Queued);

    f.h.release(1);
    f.wait_sent(y.id).await;
}

#[tokio::test]
async fn raising_the_limit_starts_waiting_intents() {
    let f = fx().await;
    f.svc
        .set_agent_defaults(AgentDefaults {
            queue_limit: Some(1),
        })
        .await
        .unwrap();
    f.h.hold();
    let x = f.intent("x", &[]).await;
    let y = f.intent("y", &[]).await;
    assert_eq!(f.svc.send(x.id).await.unwrap().state, IntentState::Sent);
    assert_eq!(f.svc.send(y.id).await.unwrap().state, IntentState::Queued);

    f.svc
        .set_agent_defaults(AgentDefaults { queue_limit: None })
        .await
        .unwrap();
    f.wait_sent(y.id).await;
}

#[tokio::test]
async fn a_started_agent_counts_before_its_state_is_recorded() {
    let f = fx().await;
    f.svc
        .set_agent_defaults(AgentDefaults {
            queue_limit: Some(1),
        })
        .await
        .unwrap();
    f.h.hold();
    let x = f.intent("x", &[]).await;
    let agent = f.svc.send(x.id).await.unwrap().agent_id.unwrap();
    f.wait_agent(agent, |a| a.state == AgentState::Working)
        .await;
    // Stand in for the moment between `send_message` accepting the brief and
    // the turn's task recording `Working`: the store still says Idle.
    f.store
        .update_agent_state(agent, AgentState::Idle)
        .await
        .unwrap();

    let y = f.intent("y", &[]).await;
    assert_eq!(f.svc.send(y.id).await.unwrap().state, IntentState::Queued);
    f.sched().run_pass().await;
    assert_eq!(f.state(y.id).await, IntentState::Queued);
    f.h.release(1);
    f.wait_sent(y.id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_sends_for_the_last_slot_start_one() {
    let f = fx().await;
    f.svc
        .set_agent_defaults(AgentDefaults {
            queue_limit: Some(1),
        })
        .await
        .unwrap();
    f.h.hold();
    let x = f.intent("x", &[]).await;
    let y = f.intent("y", &[]).await;

    let (rx, ry) = tokio::join!(f.svc.send(x.id), f.svc.send(y.id));
    let mut states = [rx.unwrap().state, ry.unwrap().state];
    states.sort_by_key(|s| format!("{s:?}"));
    assert_eq!(states, [IntentState::Queued, IntentState::Sent]);
    f.sched().run_pass().await;
    assert_eq!(f.all_agents().await.len(), 1);

    f.h.release(1);
    let waiting = if f.state(x.id).await == IntentState::Queued {
        x.id
    } else {
        y.id
    };
    f.wait_sent(waiting).await;
    assert_eq!(f.all_agents().await.len(), 2);
}

// ----------------------------------------------------------- double starts

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_kicks_start_each_intent_once() {
    let f = fx().await;
    let base = f.intent("base", &[]).await;
    let mut queued = Vec::new();
    for slug in ["a", "b", "c"] {
        let x = f.intent(slug, &[base.id]).await;
        assert_eq!(f.svc.send(x.id).await.unwrap().state, IntentState::Queued);
        queued.push(x.id);
    }
    f.mark_resolved(base.id).await;

    let (k1, k2) = (f.sched().kicker(), f.sched().kicker());
    let t1 = tokio::spawn(async move { k1.kick() });
    let t2 = tokio::spawn(async move { k2.kick() });
    t1.await.unwrap();
    t2.await.unwrap();

    for id in &queued {
        f.wait_sent(*id).await;
    }
    f.sched().run_pass().await;
    for id in &queued {
        assert_eq!(f.agents_for(*id).await.len(), 1);
    }
    assert_eq!(f.all_agents().await.len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_starts_make_one_agent() {
    let f = fx().await;
    let base = f.intent("base", &[]).await;
    let x = f.intent("x", &[base.id]).await;
    f.svc.send(x.id).await.unwrap();

    let (r1, r2) = tokio::join!(f.svc.start(x.id), f.svc.start(x.id));
    let (r1, r2) = (r1.unwrap(), r2.unwrap());
    assert_eq!(r1.state, IntentState::Sent);
    assert_eq!(r1.agent_id, r2.agent_id);
    assert_eq!(f.agents_for(x.id).await.len(), 1);

    // A start of a Sent intent returns it unchanged.
    let before = f.get(x.id).await;
    assert_eq!(f.svc.start(x.id).await.unwrap(), before);
    assert_eq!(f.get(x.id).await, before);
    assert_eq!(f.agents_for(x.id).await.len(), 1);

    // Only Complete and Queued intents can start.
    let mut content = base.content.clone();
    content.criteria[0].status = ItemStatus::Pending;
    let draft = f.insert("draft", IntentState::Draft, content).await;
    let err = f.svc.start(draft.id).await.unwrap_err();
    assert!(is_invalid_state(&err, IntentState::Draft), "{err:?}");
    f.mark_resolved(base.id).await;
    let err = f.svc.start(base.id).await.unwrap_err();
    assert!(is_invalid_state(&err, IntentState::Resolved), "{err:?}");
}

fn attached_message(agent: &Agent) -> String {
    format!(
        "agent {} is already attached to this intent; close it to start again",
        agent.name
    )
}

#[tokio::test]
async fn an_open_agent_left_by_a_crashed_start_blocks_a_second_one() {
    let f = fx().await;
    let x = f.intent("x", &[]).await;
    let stray = f.stray_agent(&x).await;

    let err = f.svc.send(x.id).await.unwrap_err();
    assert!(
        matches!(&err, IntentError::Invalid(m) if *m == attached_message(&stray)),
        "{err:?}"
    );
    let now = f.get(x.id).await;
    assert_eq!(now.state, IntentState::Complete);
    assert_eq!(now.agent_id, None);
    assert_eq!(now.last_start_error, Some(attached_message(&stray)));
    assert_eq!(f.authors(x.id).await, [VersionAuthor::Seed]);
    assert_eq!(f.agents_for(x.id).await.len(), 1);

    // Once that agent is closed, Send starts a new one.
    f.agents.close_agent(stray.id).await.unwrap();
    let sent = f.svc.send(x.id).await.unwrap();
    assert_eq!(sent.state, IntentState::Sent);
    assert_eq!(sent.last_start_error, None);
    assert_ne!(sent.agent_id, Some(stray.id));
    assert_eq!(f.agents_for(x.id).await.len(), 2);
}

#[tokio::test]
async fn a_queued_intent_with_a_stray_agent_starts_once_that_agent_closes() {
    let f = fx().await;
    let base = f.intent("base", &[]).await;
    let q = f.intent("q", &[base.id]).await;
    assert_eq!(f.svc.send(q.id).await.unwrap().state, IntentState::Queued);
    let stray = f.stray_agent(&q).await;
    f.mark_resolved(base.id).await;

    f.sched().run_pass().await;
    let now = f.get(q.id).await;
    assert_eq!(now.state, IntentState::Queued);
    assert_eq!(now.last_start_error, Some(attached_message(&stray)));
    assert_eq!(f.agents_for(q.id).await.len(), 1);

    // Closing the stray agent is the only trigger needed.
    f.agents.close_agent(stray.id).await.unwrap();
    let sent = f.wait_sent(q.id).await;
    assert_ne!(sent.agent_id, Some(stray.id));
    assert_eq!(sent.last_start_error, None);
    assert_eq!(f.agents_for(q.id).await.len(), 2);
}

#[tokio::test]
async fn a_panicking_start_does_not_stop_the_scheduler() {
    let f = fx().await;
    let base = f.intent("base", &[]).await;
    let q = f.intent("q", &[base.id]).await;
    assert_eq!(f.svc.send(q.id).await.unwrap().state, IntentState::Queued);
    // Let the pass that Send asked for run first.
    f.sched().run_pass().await;
    let panics = Arc::new(AtomicUsize::new(1));
    let left = panics.clone();
    f.svc.set_start_hook(Some(Arc::new(move |_id| {
        if left
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            panic!("test start panic");
        }
    })));
    f.mark_resolved(base.id).await;

    // This pass's start panics.
    f.sched().run_pass().await;
    assert_eq!(panics.load(Ordering::SeqCst), 0);
    assert_eq!(f.state(q.id).await, IntentState::Queued);
    assert!(f.all_agents().await.is_empty());

    // The scheduler still runs (and the locks the pass held are free): a
    // later kick starts the intent.
    f.sched().kick();
    f.wait_sent(q.id).await;
    assert_eq!(f.agents_for(q.id).await.len(), 1);
}

// ------------------------------------------------------------ plain folders

impl Fx {
    /// A folder added with no source control, beside the git repository.
    async fn plain_repo(&self) -> Repo {
        let dir = self.tmp.path().join("folder");
        std::fs::create_dir_all(&dir).unwrap();
        self.agents
            .add_repo(&dir, AddSourceControl::None)
            .await
            .unwrap()
    }
}

/// With a queue limit of 2, sends a and b on a plain folder and c on the git
/// repository, in that order, and returns once a's turn has ended: a and c
/// are `Sent`, and b has waited, with no start error, while a's turn ran and
/// after it ended, though a slot was free.
async fn two_intents_on_one_folder(f: &Fx) -> (Repo, Intent, Intent) {
    f.svc
        .set_agent_defaults(AgentDefaults {
            queue_limit: Some(2),
        })
        .await
        .unwrap();
    let folder = f.plain_repo().await;
    let a = f.intent_on(&folder, "a", &[]).await;
    let b = f.intent_on(&folder, "b", &[]).await;
    let c = f.intent("c", &[]).await;
    f.h.hold();
    let a = f.svc.send(a.id).await.unwrap();
    assert_eq!(a.state, IntentState::Sent);
    let queued = f.svc.send(b.id).await.unwrap();
    assert_eq!(
        (queued.state, queued.last_start_error),
        (IntentState::Queued, None)
    );
    // c's turn ends at once, so only a's runs.
    f.h.wait_turns(1).await;
    f.h.unhold();
    let c = f.svc.send(c.id).await.unwrap();
    assert_eq!(c.state, IntentState::Sent);
    wait_running(&f.agents, 1).await;
    assert!(f.agents.turn_in_flight(a.agent_id.unwrap()));

    f.sched().run_pass().await;
    let waiting = f.get(b.id).await;
    assert_eq!(
        (waiting.state, waiting.last_start_error),
        (IntentState::Queued, None)
    );

    f.h.release(1);
    f.wait_agent(a.agent_id.unwrap(), |x| x.state == AgentState::Done)
        .await;
    wait_running(&f.agents, 0).await;
    f.sched().run_pass().await;
    let waiting = f.get(b.id).await;
    assert_eq!(
        (waiting.state, waiting.last_start_error),
        (IntentState::Queued, None)
    );
    assert!(f.agents_for(b.id).await.is_empty());
    assert_eq!(f.state(a.id).await, IntentState::Sent);
    (folder, a, b)
}

#[tokio::test]
async fn intents_on_one_plain_folder_run_one_after_the_other() {
    let f = fx().await;
    let (folder, a, b) = two_intents_on_one_folder(&f).await;

    f.svc.resolve(a.id).await.unwrap();

    // No other call: the scheduler starts b, in the folder itself.
    let sent = f.wait_sent(b.id).await;
    let agent = f.agent(sent.agent_id.unwrap()).await;
    assert_eq!((agent.branch, agent.worktree_path), (None, folder.path));
}

#[tokio::test]
async fn closing_the_agent_frees_the_folder_for_the_next_intent() {
    let f = fx().await;
    let (_, a, b) = two_intents_on_one_folder(&f).await;

    f.agents.close_agent(a.agent_id.unwrap()).await.unwrap();

    f.wait_sent(b.id).await;
    assert_eq!(f.state(a.id).await, IntentState::Sent);
}

// ----------------------------------------------------------------- failures

#[tokio::test]
async fn branch_collision_retries_once_with_a_suffix() {
    let f = fx().await;
    let x = f.intent("taken", &[]).await;
    common::git(&f.repo_dir, &["branch", "intent/taken"]);

    let sent = f.svc.send(x.id).await.unwrap();
    let agent = f.agent(sent.agent_id.unwrap()).await;
    assert_eq!(agent.branch, Some(format!("intent/taken-{}", hex6(x.id))));
    assert_eq!(sent.last_start_error, None);

    // Both names taken: one retry only, then a typed error and no agent.
    let y = f.intent("busy", &[]).await;
    common::git(&f.repo_dir, &["branch", "intent/busy"]);
    common::git(
        &f.repo_dir,
        &["branch", &format!("intent/busy-{}", hex6(y.id))],
    );
    let err = f.svc.send(y.id).await.unwrap_err();
    assert!(
        matches!(
            err,
            IntentError::Service(ServiceError::Git(GitError::BranchExists(_)))
        ),
        "{err:?}"
    );
    let now = f.get(y.id).await;
    assert_eq!(now.state, IntentState::Complete);
    assert!(now.last_start_error.is_some());
    assert!(f.agents_for(y.id).await.is_empty());
}

#[tokio::test]
async fn moved_repository_keeps_the_intent_and_records_the_error() {
    let f = fx().await;
    let base = f.intent("base", &[]).await;
    let q = f.intent("queued", &[base.id]).await;
    assert_eq!(f.svc.send(q.id).await.unwrap().state, IntentState::Queued);
    let x = f.intent("now", &[]).await;
    let moved = f.tmp.path().join("moved");
    std::fs::rename(&f.repo_dir, &moved).unwrap();

    // Send: a typed error, the intent stays Complete with the error noted.
    let seq = f.bus.latest_seq();
    let err = f.svc.send(x.id).await.unwrap_err();
    assert!(
        matches!(err, IntentError::Service(ServiceError::Git(_))),
        "{err:?}"
    );
    let now = f.get(x.id).await;
    assert_eq!(now.state, IntentState::Complete);
    assert_eq!(now.agent_id, None);
    assert!(now.last_start_error.is_some());
    assert_eq!(f.authors(x.id).await, [VersionAuthor::Seed]);
    assert_eq!(f.updates_since(seq, x.id).last(), Some(&now));

    // The queue: the start fails, the intent stays Queued with the error.
    f.mark_resolved(base.id).await;
    f.sched().run_pass().await;
    let now = f.get(q.id).await;
    assert_eq!(now.state, IntentState::Queued);
    assert!(now.last_start_error.is_some());
    assert!(f.all_agents().await.is_empty());
    assert!(f.worktree_dirs().is_empty(), "{:?}", f.worktree_dirs());

    // With the repository back, both start and the errors clear.
    std::fs::rename(&moved, &f.repo_dir).unwrap();
    f.sched().run_pass().await;
    assert_eq!(f.get(q.id).await.last_start_error, None);
    assert_eq!(f.state(q.id).await, IntentState::Sent);
    let sent = f.svc.send(x.id).await.unwrap();
    assert_eq!(sent.state, IntentState::Sent);
    assert_eq!(sent.last_start_error, None);
}

#[tokio::test]
async fn failed_save_after_the_start_closes_the_agent() {
    let f = fx().await;
    let x = f.intent("x", &[]).await;
    let raw = raw_db(&f.db).await;
    sqlx::query(
        "CREATE TRIGGER refuse_sent BEFORE UPDATE ON intents WHEN NEW.state = 'sent' \
         BEGIN SELECT RAISE(ABORT, 'injected failure'); END",
    )
    .execute(&raw)
    .await
    .unwrap();
    f.h.hold();

    let err = f.svc.send(x.id).await.unwrap_err();
    assert!(matches!(err, IntentError::Store(_)), "{err:?}");
    let now = f.get(x.id).await;
    assert_eq!(now.state, IntentState::Complete);
    assert_eq!(now.agent_id, None);
    assert!(now.last_start_error.is_some());
    assert_eq!(f.authors(x.id).await, [VersionAuthor::Seed]);

    // The agent's turn is running, so it is closed once that turn ends.
    let orphans = f.agents_for(x.id).await;
    assert_eq!(orphans.len(), 1);
    assert!(orphans[0].closed_at.is_none());
    f.h.release(1);
    f.wait_agent(orphans[0].id, |a| a.closed_at.is_some()).await;
    assert!(orphans[0].worktree_path.is_dir());

    // With the store working again, Send succeeds (on the suffixed branch).
    sqlx::query("DROP TRIGGER refuse_sent")
        .execute(&raw)
        .await
        .unwrap();
    let sent = f.svc.send(x.id).await.unwrap();
    assert_eq!(sent.state, IntentState::Sent);
    assert_eq!(sent.last_start_error, None);
    let agent = f.agent(sent.agent_id.unwrap()).await;
    assert_eq!(agent.branch, Some(format!("intent/x-{}", hex6(x.id))));
}

// ------------------------------------------------------------------ resolve

#[tokio::test]
async fn resolve_requires_a_sent_intent() {
    let f = fx().await;
    let base = f.intent("base", &[]).await;
    let x = f.intent("x", &[base.id]).await;
    f.svc.send(x.id).await.unwrap();

    let err = f.svc.resolve(base.id).await.unwrap_err();
    assert!(is_invalid_state(&err, IntentState::Complete), "{err:?}");
    let err = f.svc.resolve(x.id).await.unwrap_err();
    assert!(is_invalid_state(&err, IntentState::Queued), "{err:?}");

    // Any agent state will do: here the agent is still working.
    f.h.hold();
    let sent = f.svc.send(base.id).await.unwrap();
    f.wait_agent(sent.agent_id.unwrap(), |a| a.state == AgentState::Working)
        .await;
    let resolved = f.svc.resolve(base.id).await.unwrap();
    assert_eq!(resolved.state, IntentState::Resolved);
    assert_eq!(f.get(base.id).await, resolved);
    let err = f.svc.resolve(base.id).await.unwrap_err();
    assert!(is_invalid_state(&err, IntentState::Resolved), "{err:?}");
    f.h.release(2);
}

// ------------------------------------------------------------------ restart

#[tokio::test]
async fn restart_starts_an_intent_whose_dependency_resolved_while_down() {
    let mut f = fx().await;
    let base = f.intent("base", &[]).await;
    let x = f.intent("dependent", &[base.id]).await;
    let base_sent = f.svc.send(base.id).await.unwrap();
    f.wait_agent(base_sent.agent_id.unwrap(), |a| a.state == AgentState::Done)
        .await;
    assert_eq!(f.svc.send(x.id).await.unwrap().state, IntentState::Queued);

    // The daemon goes down; meanwhile the dependency is marked Resolved.
    f.sched.take().unwrap().shutdown().await;
    f.mark_resolved(base.id).await;

    // A fresh daemon: new services and scheduler over the same store.
    let agents = AgentService::new(
        f.store.clone(),
        f.bus.clone(),
        f.h.clone(),
        f.paths.clone(),
        NEVER_QUIET,
    );
    let svc = service(&f.store, &f.bus, &f.h, &agents, &f.paths);
    let _sched = Scheduler::start(svc, agents, f.store.clone(), f.bus.clone());
    let sent = f.wait_sent(x.id).await;
    let agent = f.agent(sent.agent_id.unwrap()).await;
    assert_eq!(agent.intent_id, Some(x.id));
    assert_eq!(agent.name, "Venus");
}

// --------------------------------------------------------------- settings

#[tokio::test]
async fn agent_defaults_validate_and_normalize_the_limit() {
    let f = fx().await;
    assert_eq!(f.svc.agent_defaults().await.unwrap().queue_limit, None);

    let err = f
        .svc
        .set_agent_defaults(AgentDefaults {
            queue_limit: Some(100),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
    assert_eq!(err.code(), "invalid");
    let err = f
        .svc
        .set_agent_defaults(AgentDefaults {
            queue_limit: Some(65),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, IntentError::Invalid(_)), "{err:?}");
    assert_eq!(f.svc.agent_defaults().await.unwrap().queue_limit, None);

    let set = f
        .svc
        .set_agent_defaults(AgentDefaults {
            queue_limit: Some(64),
        })
        .await
        .unwrap();
    assert_eq!(set.queue_limit, Some(64));
    assert_eq!(f.svc.agent_defaults().await.unwrap(), set);

    // 0 means no limit.
    let set = f
        .svc
        .set_agent_defaults(AgentDefaults {
            queue_limit: Some(0),
        })
        .await
        .unwrap();
    assert_eq!(set.queue_limit, None);
    assert_eq!(f.svc.agent_defaults().await.unwrap().queue_limit, None);
}

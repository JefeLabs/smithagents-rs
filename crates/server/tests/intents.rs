mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use smithagents_core::agent::{Agent, Repo, Role};
use smithagents_core::event::EventKind;
use smithagents_core::ids::{IntentId, ItemId, RepoId};
use smithagents_core::intent::{
    CreateIntentRequest, IntentFilter, IntentKind, IntentState, Item, ItemStatus, Patch, Phase,
    SeedInput, SeedSource, VersionAuthor,
};
use smithagents_server::events::EventBus;
use smithagents_server::harness::fake::FakeHarness;
use smithagents_server::intents::{IntentError, IntentService, PolishRunner};
use smithagents_server::paths::Paths;
use smithagents_server::service::AgentService;
use smithagents_server::store::Store;
use tempfile::TempDir;

#[derive(Default)]
struct Recorder(Mutex<Vec<(IntentId, u8)>>);

impl PolishRunner for Recorder {
    fn start(&self, id: IntentId, pass: u8) {
        self.0.lock().unwrap().push((id, pass));
    }
}

struct Fx {
    tmp: TempDir,
    store: Store,
    bus: EventBus,
    agents: AgentService,
    svc: IntentService,
    runner: Arc<Recorder>,
}

async fn fx() -> Fx {
    let tmp = TempDir::new().unwrap();
    let store = Store::open_in_memory().await.unwrap();
    let bus = EventBus::new(1024);
    let paths = Paths {
        home: tmp.path().join("home"),
    };
    paths.ensure_dirs().unwrap();
    let harness = Arc::new(FakeHarness::scripted(vec![]));
    let agents = AgentService::new(
        store.clone(),
        bus.clone(),
        harness.clone(),
        paths.clone(),
        Duration::from_secs(3600),
    );
    let runner = Arc::new(Recorder::default());
    let svc = IntentService::new(store.clone(), bus.clone(), harness, agents.clone(), paths)
        .with_polish_runner(runner.clone());
    Fx {
        tmp,
        store,
        bus,
        agents,
        svc,
        runner,
    }
}

impl Fx {
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

//! The intent and settings API through the real `smithagentsd`, with
//! `crates/server/tests/bin/fake-claude` standing in for `claude`.
//!
//! Pass 1 and pass 2 of the polish return different structured results from
//! the same daemon process: the `structured` file of fake-claude's control
//! directory names the JSON file the next run prints, and the test rewrites
//! it between the passes.

mod common;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::{Stream, StreamExt};
use serde_json::json;
use smithagents_client::{ensure_daemon_with, Client, ClientError, SpawnOptions};
use smithagents_core::agent::{AgentState, Role};
use smithagents_core::event::{Event, EventKind};
use smithagents_core::intent::{
    Answer, AnswerInput, CreateIntentRequest, Intent, IntentFilter, IntentKind, IntentState, Patch,
    Phase, SeedInput,
};
use smithagents_core::VERSION;

use common::{claude_fixture, daemon_bin, fake_claude, init_repo, pid_alive, TestHome};

const PATIENCE: Duration = Duration::from_secs(30);

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(PATIENCE, fut)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish within {PATIENCE:?}"))
}

struct Fixture {
    home: TestHome,
    client: Client,
    control_dir: PathBuf,
    pass2: PathBuf,
}

fn write_json(path: &Path, value: &serde_json::Value) {
    std::fs::write(path, value.to_string()).unwrap();
}

async fn start() -> Fixture {
    start_with(&[]).await
}

async fn start_with(extra_env: &[(&str, &str)]) -> Fixture {
    let bin = daemon_bin();
    let home = TestHome::new();
    let scratch = home.scratch().to_owned();
    let control_dir = scratch.join("control");
    std::fs::create_dir(&control_dir).unwrap();
    let pass1 = scratch.join("pass1.json");
    let pass2 = scratch.join("pass2.json");
    write_json(
        &pass1,
        &json!({
            "title": "Add dark mode",
            "kind": "enhancement",
            "current_behavior": "Only a light theme exists.",
            "questions": [
                {"text": "Follow the system theme?", "options": [{"label": "Yes"}, {"label": "No"}]},
                {"text": "Persist the choice?", "options": [{"label": "Yes"}, {"label": "No"}]}
            ]
        }),
    );
    write_json(
        &pass2,
        &json!({
            "preconditions": ["A theme token file exists"],
            "acceptance_criteria": ["Dark colors apply everywhere"],
            "stories": ["As a user I can switch to dark mode"],
            "depends_on": []
        }),
    );
    std::fs::write(control_dir.join("structured"), pass1.to_str().unwrap()).unwrap();
    let mut options = SpawnOptions {
        args: vec!["--claude-bin".into(), fake_claude().into()],
        env: vec![
            ("FAKE_CLAUDE_FIXTURE".into(), claude_fixture().into()),
            ("FAKE_CLAUDE_STRUCTURED_FILE".into(), pass1.into()),
            ("FAKE_CLAUDE_CONTROL_DIR".into(), control_dir.clone().into()),
        ],
    };
    options
        .env
        .extend(extra_env.iter().map(|&(k, v)| (k.into(), v.into())));
    let client = within(
        "ensure_daemon_with",
        ensure_daemon_with(home.path(), bin, VERSION, &options),
    )
    .await
    .unwrap_or_else(|e| panic!("ensure failed: {e}; log:\n{}", home.log()));
    Fixture {
        home,
        client,
        control_dir,
        pass2,
    }
}

/// Consumes `events` until an intent update `wanted` accepts, and returns it.
async fn next_intent(
    events: &mut (impl Stream<Item = Event> + Unpin),
    wanted: impl Fn(&Intent) -> bool,
) -> Intent {
    within("an intent event", async {
        while let Some(event) = events.next().await {
            match event.kind {
                EventKind::IntentCreated(i) | EventKind::IntentUpdated(i) if wanted(&i) => {
                    return i
                }
                _ => {}
            }
        }
        panic!("the event stream ended first");
    })
    .await
}

fn api_code(e: ClientError) -> (u16, String) {
    match e {
        ClientError::Api { status, code, .. } => (status, code),
        other => panic!("expected an API error, got {other:?}"),
    }
}

#[tokio::test]
async fn full_loop_through_the_client() {
    let fx = start().await;
    let client = &fx.client;
    let repo_dir = fx.home.scratch().join("repo");
    init_repo(&repo_dir);
    let repo = client.add_repo(&repo_dir).await.unwrap();
    let agent = client.create_agent(repo.id).await.unwrap();
    let mut events = client.events(None).await.unwrap();

    // A chat turn, then an intent seeded from it.
    client
        .send_message(agent.id, "please add dark mode")
        .await
        .unwrap();
    within("the chat turn", async {
        while let Some(e) = events.next().await {
            if matches!(&e.kind, EventKind::AgentUpdated(a)
                if a.id == agent.id && matches!(a.state, AgentState::Done | AgentState::Dead))
            {
                return;
            }
        }
        panic!("stream ended");
    })
    .await;

    let created = client
        .create_intent(CreateIntentRequest {
            kind: Some(IntentKind::Refactor),
            repo_id: None,
            seed: SeedInput::Chat {
                agent_id: agent.id,
                from_seq: None,
                to_seq: None,
            },
        })
        .await
        .unwrap();
    assert_eq!(created.state, IntentState::Draft);
    assert_eq!(created.repo_id, repo.id);

    let at_questions = next_intent(&mut events, |i| {
        i.id == created.id && i.phase == Phase::Questions
    })
    .await;
    assert_eq!(at_questions.content.title, "Add dark mode");
    assert_eq!(at_questions.content.kind, IntentKind::Refactor);
    assert_eq!(at_questions.content.questions.len(), 2);

    // Switch the daemon's fake claude to the pass-2 result.
    std::fs::write(
        fx.control_dir.join("structured"),
        fx.pass2.to_str().unwrap(),
    )
    .unwrap();

    let (q1, q2) = (
        &at_questions.content.questions[0],
        &at_questions.content.questions[1],
    );
    let answered = client
        .answer_question(
            created.id,
            q1.id,
            AnswerInput::Option(q1.options[0].id.clone()),
        )
        .await
        .unwrap();
    assert!(matches!(
        answered.content.questions[0].answer,
        Answer::Option { .. }
    ));
    let err = client
        .answer_question(created.id, q1.id, AnswerInput::Skip)
        .await
        .unwrap_err();
    assert_eq!(api_code(err), (409, "already_answered".to_owned()));
    client
        .answer_question(created.id, q2.id, AnswerInput::Skip)
        .await
        .unwrap();

    let review = next_intent(&mut events, |i| {
        i.id == created.id && i.phase == Phase::Review
    })
    .await;
    assert_eq!(review.content.criteria.len(), 1);

    // Not complete yet: sending is refused.
    let err = client.send_intent(created.id).await.unwrap_err();
    assert_eq!(api_code(err).1, "invalid_state");

    let patched = client
        .update_intent(
            created.id,
            Patch {
                kind: Some(IntentKind::Task),
                ..Patch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(patched.content.kind, IntentKind::Task);

    let c = &review.content;
    client
        .accept_item(created.id, c.preconditions[0].id)
        .await
        .unwrap();
    client
        .edit_item(
            created.id,
            c.criteria[0].id,
            "Dark colors apply on every screen",
        )
        .await
        .unwrap();
    let after = client
        .accept_item(created.id, c.criteria[0].id)
        .await
        .unwrap();
    assert_eq!(
        after.content.criteria[0].text,
        "Dark colors apply on every screen"
    );
    client.drop_item(created.id, c.stories[0].id).await.unwrap();
    let done = next_intent(&mut events, |i| {
        i.id == created.id && i.state == IntentState::Complete
    })
    .await;
    assert!(done.is_complete_content());

    let versions = client.intent_versions(created.id).await.unwrap();
    assert!(versions.len() >= 3, "{versions:?}");
    assert_eq!(client.get_intent(created.id).await.unwrap().id, created.id);
    let listed = client
        .list_intents(IntentFilter {
            state: Some(IntentState::Complete),
            repo_id: Some(repo.id),
        })
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    let none = client
        .list_intents(IntentFilter {
            state: Some(IntentState::Resolved),
            repo_id: None,
        })
        .await
        .unwrap();
    assert!(none.is_empty());
    assert_eq!(
        client
            .list_intents(IntentFilter::default())
            .await
            .unwrap()
            .len(),
        1
    );

    // Send: the intent reaches the agent as a brief.
    client.send_intent(created.id).await.unwrap();
    let sent = next_intent(&mut events, |i| {
        i.id == created.id && i.state == IntentState::Sent
    })
    .await;
    let brief_agent = sent.agent_id.expect("a sent intent has an agent");
    // Poll the agent's messages: the AgentUpdated event may already have been
    // consumed by `next_intent`, so waiting on the stream could miss it.
    let deadline = std::time::Instant::now() + PATIENCE;
    let messages = loop {
        let messages = client.messages(brief_agent, 0).await.unwrap();
        if messages.iter().any(|m| m.role == Role::User) {
            break messages;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the brief was not recorded within {PATIENCE:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert!(
        messages
            .iter()
            .any(|m| m.role == Role::User && m.text.contains("Add dark mode")),
        "{messages:?}"
    );

    let resolved = client.resolve_intent(created.id).await.unwrap();
    assert_eq!(resolved.state, IntentState::Resolved);
}

#[tokio::test]
async fn polish_model_env_reaches_the_cli() {
    let args_dir = tempfile::tempdir().unwrap();
    let args_path = args_dir.path().join("args");
    let fx = start_with(&[
        ("SMITHAGENTS_POLISH_MODEL", "haiku"),
        ("FAKE_CLAUDE_ARGS_FILE", args_path.to_str().unwrap()),
    ])
    .await;
    let repo_dir = fx.home.scratch().join("repo");
    init_repo(&repo_dir);
    let repo = fx.client.add_repo(&repo_dir).await.unwrap();
    let mut events = fx.client.events(None).await.unwrap();
    let created = fx
        .client
        .create_intent(CreateIntentRequest {
            kind: None,
            repo_id: Some(repo.id),
            seed: SeedInput::Text("add dark mode".into()),
        })
        .await
        .unwrap();
    next_intent(&mut events, |i| {
        i.id == created.id && i.phase == Phase::Questions
    })
    .await;
    let args: Vec<String> = std::fs::read_to_string(&args_path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let at = args.iter().position(|a| a == "--model").expect("--model");
    assert_eq!(args[at + 1], "haiku", "{args:?}");
}

/// Crashes the daemon (SIGKILL, so nothing shuts down cleanly).
fn kill_daemon(home: &Path) {
    use rustix::process::{kill_process, Pid, Signal};
    let info = smithagents_core::daemon::read_lockfile(home)
        .unwrap()
        .expect("a lockfile");
    kill_process(Pid::from_raw(info.pid as i32).unwrap(), Signal::KILL).unwrap();
    let deadline = std::time::Instant::now() + PATIENCE;
    while pid_alive(info.pid) {
        assert!(std::time::Instant::now() < deadline, "the daemon survived");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Acceptance: a daemon that dies during pass 1 leaves a Failed intent after
/// the restart, and Retry runs pass 1 again to completion.
#[tokio::test]
async fn daemon_crash_during_pass_one_fails_the_intent_and_retry_works() {
    let fx = start().await;
    let repo_dir = fx.home.scratch().join("repo");
    init_repo(&repo_dir);
    let repo = fx.client.add_repo(&repo_dir).await.unwrap();
    // Pass 1 hangs, so the daemon dies in the middle of it.
    std::fs::write(fx.control_dir.join("sleep"), "30").unwrap();
    let created = fx
        .client
        .create_intent(CreateIntentRequest {
            kind: None,
            repo_id: Some(repo.id),
            seed: SeedInput::Text("add dark mode".into()),
        })
        .await
        .unwrap();
    assert_eq!(created.phase, Phase::Polishing { pass: 1 });
    kill_daemon(fx.home.path());
    std::fs::remove_file(fx.control_dir.join("sleep")).unwrap();

    let options = SpawnOptions {
        args: vec!["--claude-bin".into(), fake_claude().into()],
        env: vec![
            ("FAKE_CLAUDE_FIXTURE".into(), claude_fixture().into()),
            (
                "FAKE_CLAUDE_STRUCTURED_FILE".into(),
                fx.home.scratch().join("pass1.json").into(),
            ),
            (
                "FAKE_CLAUDE_CONTROL_DIR".into(),
                fx.control_dir.clone().into(),
            ),
        ],
    };
    let client = within(
        "restart",
        ensure_daemon_with(fx.home.path(), daemon_bin(), VERSION, &options),
    )
    .await
    .unwrap_or_else(|e| panic!("restart failed: {e}; log:\n{}", fx.home.log()));
    let failed = client.get_intent(created.id).await.unwrap();
    assert!(
        matches!(&failed.phase, Phase::Failed { pass: 1, .. }),
        "{:?}",
        failed.phase
    );
    client.retry_polish(created.id).await.unwrap();
    let mut events = client.events(None).await.unwrap();
    let again = within("the retried pass", async {
        loop {
            let i = client.get_intent(created.id).await.unwrap();
            if i.phase == Phase::Questions {
                return i;
            }
            assert!(
                !matches!(i.phase, Phase::Failed { .. }),
                "retry failed: {:?}",
                i.phase
            );
            let _ = tokio::time::timeout(Duration::from_millis(100), events.next()).await;
        }
    })
    .await;
    assert_eq!(again.content.title, "Add dark mode");
}

/// Acceptance: a seed from a chat of several turns carries every message.
#[tokio::test]
async fn chat_seed_carries_every_message() {
    let fx = start().await;
    let client = &fx.client;
    let repo_dir = fx.home.scratch().join("repo");
    init_repo(&repo_dir);
    let repo = client.add_repo(&repo_dir).await.unwrap();
    let agent = client.create_agent(repo.id).await.unwrap();
    let mut events = client.events(None).await.unwrap();
    for text in ["first request", "second request"] {
        client.send_message(agent.id, text).await.unwrap();
        within("the chat turn", async {
            while let Some(e) = events.next().await {
                if matches!(&e.kind, EventKind::AgentUpdated(a)
                    if a.id == agent.id && matches!(a.state, AgentState::Done | AgentState::Dead))
                {
                    return;
                }
            }
            panic!("stream ended");
        })
        .await;
    }
    let intent = client
        .create_intent(CreateIntentRequest {
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
    for text in ["first request", "second request"] {
        assert!(intent.seed.text.contains(text), "{}", intent.seed.text);
    }
    assert!(
        intent.seed.text.matches("Assistant:").count() >= 2,
        "{}",
        intent.seed.text
    );
}

#[tokio::test]
async fn agent_defaults_round_trip() {
    let fx = start().await;
    let first = fx.client.agent_defaults().await.unwrap();
    let set = smithagents_core::intent::AgentDefaults {
        queue_limit: Some(first.queue_limit.map_or(3, |n| n + 1)),
    };
    let echoed = fx.client.set_agent_defaults(set.clone()).await.unwrap();
    assert_eq!(echoed, set);
    assert_eq!(fx.client.agent_defaults().await.unwrap(), set);
}

#[tokio::test]
async fn missing_intent_is_a_typed_error() {
    let fx = start().await;
    let err = fx
        .client
        .get_intent(smithagents_core::ids::IntentId::new())
        .await
        .unwrap_err();
    assert_eq!(api_code(err).0, 404);
}

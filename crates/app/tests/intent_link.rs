//! The intent flow against the real `smithagentsd`, with
//! `crates/server/tests/bin/fake-claude` standing in for `claude`: create,
//! questions, review, Complete, Send, Sent, Resolved. Every action the link
//! sends is applied to a `ShellModel`, and the assertions read what the
//! Intents pane would draw through the view-model functions. No window is
//! opened.
//!
//! Pass 1 and pass 2 of the polish print different structured results from
//! the same daemon: the `structured` file of fake-claude's control directory
//! names the JSON file the next run prints, and the test rewrites it between
//! the passes (as `crates/client/tests/intents.rs` does).

mod common;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use common::{daemon_bin, fake_claude, init_repo, TestHome};
use serde_json::json;
use smithagents_app::intent_vm::{
    answer_text, body_preview, intent_cards, intent_state_label, kind_label, section_vm,
    BodyPreview, IntentFilterChoice, SectionKind, SectionVm,
};
use smithagents_app::link::{DaemonLink, LinkConfig};
use smithagents_app::model::{Action, Connection, ShellModel};
use smithagents_client::SpawnOptions;
use smithagents_core::ids::IntentId;
use smithagents_core::intent::{
    AnswerInput, CreateIntentRequest, Intent, IntentKind, ReviewAction, SeedInput,
};
use tokio::sync::mpsc::Receiver;

const WAIT: Duration = Duration::from_secs(30);

/// The transcript fake-claude prints for a chat turn: the agent's brief.
const HELLO_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../server/tests/fixtures/claude_stream_hello.jsonl"
);

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// fake-claude's controls: the control directory whose `structured` file
/// names the JSON the next polish pass prints, and the two passes' JSON.
struct Polish {
    control_dir: PathBuf,
    pass1: PathBuf,
    pass2: PathBuf,
}

impl Polish {
    fn new(scratch: &Path) -> Self {
        let control_dir = scratch.join("control");
        std::fs::create_dir(&control_dir).unwrap();
        let pass1 = scratch.join("pass1.json");
        let pass2 = scratch.join("pass2.json");
        let pass1_json = json!({
            "title": "Add dark mode",
            "kind": "enhancement",
            "current_behavior": "## Today\nOnly a light theme exists.",
            "questions": [
                {"text": "Follow the system theme?", "options": [{"label": "Yes"}, {"label": "No"}]}
            ]
        });
        let pass2_json = json!({
            "preconditions": ["A theme token file exists"],
            "acceptance_criteria": [
                "The theme follows the system setting",
                "Dark colors apply everywhere"
            ],
            "stories": ["As a user I can switch to dark mode"],
            "depends_on": []
        });
        std::fs::write(&pass1, pass1_json.to_string()).unwrap();
        std::fs::write(&pass2, pass2_json.to_string()).unwrap();
        let polish = Self {
            control_dir,
            pass1,
            pass2,
        };
        polish.next_pass_prints(&polish.pass1);
        polish
    }

    /// Makes the next polish run print `pass`.
    fn next_pass_prints(&self, pass: &Path) {
        std::fs::write(
            self.control_dir.join("structured"),
            pass.to_str().expect("a UTF-8 temp path"),
        )
        .unwrap();
    }
}

fn config(home: &TestHome, polish: &Polish) -> LinkConfig {
    let mut config = LinkConfig::new(
        home.path().to_owned(),
        daemon_bin().to_owned(),
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    config.spawn = SpawnOptions {
        args: vec![
            "--claude-bin".into(),
            fake_claude().into(),
            "--idle-timeout-secs".into(),
            "120".into(),
        ],
        env: vec![
            ("FAKE_CLAUDE_FIXTURE".into(), HELLO_FIXTURE.into()),
            (
                "FAKE_CLAUDE_STRUCTURED_FILE".into(),
                polish.pass1.clone().into(),
            ),
            (
                "FAKE_CLAUDE_CONTROL_DIR".into(),
                polish.control_dir.clone().into(),
            ),
        ],
    };
    config.backoff_min = Duration::from_millis(50);
    config.backoff_max = Duration::from_millis(200);
    config
}

/// The receiving end of the link and the model it drives, as in the shell.
struct Shell {
    rx: Receiver<Action>,
    model: ShellModel,
}

impl Shell {
    /// Receives the next action and applies it.
    async fn next(&mut self, what: &str) -> Action {
        let action = within(what, self.rx.recv())
            .await
            .unwrap_or_else(|| panic!("link closed while waiting for {what}"));
        self.model.apply(action.clone());
        action
    }

    /// Applies received actions until `done` holds for the model.
    async fn apply_until(&mut self, what: &str, done: impl Fn(&ShellModel) -> bool) {
        within(what, async {
            while !done(&self.model) {
                let action = self.rx.recv().await.expect("link closed");
                self.model.apply(action);
            }
        })
        .await;
    }

    /// Applies received actions until intent `id` shows `label`; returns the
    /// model's copy of it.
    async fn until_label(&mut self, id: IntentId, label: &str) -> Intent {
        let shows = |model: &ShellModel| {
            let intent = model.intent(id);
            if let Some(failed) = intent.filter(|i| intent_state_label(i) == "Polish failed") {
                panic!("polish failed: {:?}", failed.phase);
            }
            intent.is_some_and(|i| intent_state_label(i) == label)
        };
        self.apply_until(&format!("the intent to show {label}"), shows)
            .await;
        self.model.intent(id).unwrap().clone()
    }
}

fn counter_and_status(section: &SectionVm) -> (&str, &str) {
    (&section.counter, &section.status)
}

#[tokio::test]
async fn create_review_send_through_the_link() {
    // `home` outlives the link: its drop stops the daemon, on every exit.
    let home = TestHome::new();
    let polish = Polish::new(home.scratch());
    let repo_dir = home.scratch().join("repo");
    init_repo(&repo_dir);

    let (link, rx) = DaemonLink::start(config(&home, &polish));
    let mut shell = Shell {
        rx,
        model: ShellModel::new(),
    };
    assert!(matches!(
        shell.next("snapshot").await,
        Action::Snapshot { .. }
    ));
    assert_eq!(
        shell.next("intents").await,
        Action::IntentsLoaded(Vec::new())
    );
    assert!(matches!(
        shell.next("agent defaults").await,
        Action::AgentDefaultsLoaded(_)
    ));
    assert_eq!(
        shell.next("connected").await,
        Action::ConnectionChanged(Connection::Connected)
    );
    let repo = link.add_repo(&repo_dir).await.unwrap();

    // Create: pass 1 proposes the title, the body and one question; the
    // kind chosen at create is kept.
    let created = link
        .create_intent(CreateIntentRequest {
            repo_id: Some(repo.id),
            kind: Some(IntentKind::Refactor),
            seed: SeedInput::Text("dark mode please".into()),
        })
        .await
        .unwrap();
    let id = created.id;
    let intent = shell.until_label(id, "Questions").await;
    assert_eq!(kind_label(intent.content.kind), "Refactor");
    assert_eq!(intent.content.title, "Add dark mode");
    assert_eq!(
        body_preview(&intent.content.current_behavior),
        BodyPreview {
            heading: Some("Today".into()),
            line: Some("Only a light theme exists.".into()),
        }
    );
    assert_eq!(
        counter_and_status(&section_vm(&intent.content, SectionKind::Questions)),
        ("0 of 1 answered", "1 to answer")
    );

    // Answer: pass 2 proposes a precondition, two criteria and a story.
    polish.next_pass_prints(&polish.pass2);
    let question = &intent.content.questions[0];
    link.answer_question(
        id,
        question.id,
        AnswerInput::Option(question.options[0].id.clone()),
    )
    .await
    .unwrap();
    let intent = shell.until_label(id, "Review").await;
    assert_eq!(
        answer_text(&intent.content.questions[0]).as_deref(),
        Some("→ Yes")
    );
    assert_eq!(
        counter_and_status(&section_vm(&intent.content, SectionKind::Criteria)),
        ("0 of 2 accepted", "2 to review")
    );

    // Review every item: the intent becomes Complete.
    let content = &intent.content;
    let reviews = [
        (content.preconditions[0].id, ReviewAction::Accept),
        (content.criteria[0].id, ReviewAction::Accept),
        (
            content.criteria[1].id,
            ReviewAction::Edit("Dark colors apply in every pane".into()),
        ),
        (content.stories[0].id, ReviewAction::Drop),
    ];
    for (item, action) in reviews {
        link.review_item(id, item, action).await.unwrap();
    }
    let intent = shell.until_label(id, "Complete").await;
    assert_eq!(
        counter_and_status(&section_vm(&intent.content, SectionKind::Criteria)),
        ("2 of 2 accepted", "✓ done")
    );
    let stories = section_vm(&intent.content, SectionKind::Stories);
    assert_eq!(counter_and_status(&stories), ("0 of 0 accepted", "✓ done"));
    assert_eq!(stories.skipped, 1);
    assert_eq!(
        intent.content.criteria[1].text,
        "Dark colors apply in every pane"
    );

    // Send: a new agent starts for the intent and colors its card.
    link.send_intent(id).await.unwrap();
    let intent = shell.until_label(id, "Sent").await;
    let agent_id = intent.agent_id.expect("a sent intent names its agent");
    // The agent arrives as an event, which may trail the command's answer.
    shell
        .apply_until("the intent's agent", |model| {
            model.agent(agent_id).is_some()
        })
        .await;
    let agent = shell.model.agent(agent_id).unwrap();
    assert_eq!(agent.intent_id, Some(id));
    let cards = intent_cards(
        &shell.model,
        IntentFilterChoice::Sent,
        Utc::now().fixed_offset(),
    );
    assert_eq!(cards.len(), 1, "{cards:?}");
    assert_eq!(cards[0].id, id);
    assert_eq!(cards[0].accent_hue, Some(agent.color.h));
    assert!(cards[0].meta.contains(" | Sent | "), "{}", cards[0].meta);

    link.resolve_intent(id).await.unwrap();
    shell.until_label(id, "Resolved").await;

    drop(link);
    // `home` drops last and stops the daemon.
}

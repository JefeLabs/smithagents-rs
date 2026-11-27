//! Helpers for the intent tests: [`FakeIntents`], which records each intent
//! command and answers it when the test says so; builders for intents, items
//! and questions; a fixed clock; and a connected model holding intents.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use chrono::{DateTime, Utc};
use futures::channel::oneshot;
use futures::future::LocalBoxFuture;
use futures::FutureExt as _;
use smithagents_app::link::{IntentAnswer, IntentCommands, LinkError};
use smithagents_app::model::{Action, Connection, ShellModel};
use smithagents_app::shell_state::Clock;
use smithagents_client::ClientError;
use smithagents_core::agent::{Agent, Repo};
use smithagents_core::ids::{IntentId, ItemId, QuestionId, RepoId};
use smithagents_core::intent::{
    AgentDefaults, Answer, AnswerInput, Content, CreateIntentRequest, Intent, IntentKind,
    IntentState, Item, ItemStatus, Patch, Phase, Question, QuestionOption, ReviewAction, Seed,
    SeedSource,
};

/// When every intent [`intent`] builds was created and last changed.
pub const INTENT_TIME: &str = "2026-11-26T14:15:00Z";

/// The repository every intent [`intent`] builds belongs to.
const INTENT_REPO: &str = "5d2c9a4e-1b7f-4e3a-9c6d-8f0a2b4c6e18";

/// One intent command, as a view sent it.
#[derive(Debug, Clone, PartialEq)]
pub enum IntentCall {
    Create(CreateIntentRequest),
    Update(IntentId, Patch),
    Answer(IntentId, QuestionId, AnswerInput),
    Retry(IntentId),
    Review(IntentId, ItemId, ReviewAction),
    Send(IntentId),
    Resolve(IntentId),
    SetDefaults(AgentDefaults),
}

type Reply = Result<Intent, LinkError>;

/// Intent commands that record each call in `calls`, then answer it with
/// the next result [`FakeIntents::queue`]d, or else wait until the test
/// [`FakeIntents::answer`]s it (or forever). `set_agent_defaults` takes its
/// answer the same way: an error is its error, and any `Ok` answers the
/// defaults it was given.
#[derive(Default)]
pub struct FakeIntents {
    pub calls: RefCell<Vec<IntentCall>>,
    results: RefCell<VecDeque<Reply>>,
    pending: RefCell<VecDeque<oneshot::Sender<Reply>>>,
}

impl FakeIntents {
    /// The answer of the next call (calls made before take no queued answer).
    pub fn queue(&self, result: Result<Intent, LinkError>) {
        self.results.borrow_mut().push_back(result);
    }

    /// Answers the oldest call still waiting.
    pub fn answer(&self, result: Result<Intent, LinkError>) {
        let sender = self
            .pending
            .borrow_mut()
            .pop_front()
            .expect("an intent command waits for an answer");
        let _ = sender.send(result);
    }

    /// The daemon's refusal with this status, code and message.
    pub fn api_error(status: u16, code: &str, message: &str) -> LinkError {
        LinkError::Client(ClientError::Api {
            status,
            code: code.to_owned(),
            message: message.to_owned(),
        })
    }

    fn reply(&self, call: IntentCall) -> LocalBoxFuture<'static, Reply> {
        self.calls.borrow_mut().push(call);
        if let Some(result) = self.results.borrow_mut().pop_front() {
            return async move { result }.boxed_local();
        }
        let (tx, rx) = oneshot::channel();
        self.pending.borrow_mut().push_back(tx);
        async move { rx.await.unwrap_or(Err(LinkError::Stopped)) }.boxed_local()
    }
}

impl IntentCommands for FakeIntents {
    fn create_intent(&self, req: CreateIntentRequest) -> IntentAnswer {
        self.reply(IntentCall::Create(req))
    }

    fn update_intent(&self, id: IntentId, patch: Patch) -> IntentAnswer {
        self.reply(IntentCall::Update(id, patch))
    }

    fn answer_question(
        &self,
        id: IntentId,
        question: QuestionId,
        answer: AnswerInput,
    ) -> IntentAnswer {
        self.reply(IntentCall::Answer(id, question, answer))
    }

    fn retry_polish(&self, id: IntentId) -> IntentAnswer {
        self.reply(IntentCall::Retry(id))
    }

    fn review_item(&self, id: IntentId, item: ItemId, action: ReviewAction) -> IntentAnswer {
        self.reply(IntentCall::Review(id, item, action))
    }

    fn send_intent(&self, id: IntentId) -> IntentAnswer {
        self.reply(IntentCall::Send(id))
    }

    fn resolve_intent(&self, id: IntentId) -> IntentAnswer {
        self.reply(IntentCall::Resolve(id))
    }

    fn set_agent_defaults(
        &self,
        defaults: AgentDefaults,
    ) -> LocalBoxFuture<'static, Result<AgentDefaults, LinkError>> {
        let reply = self.reply(IntentCall::SetDefaults(defaults.clone()));
        async move { reply.await.map(|_| defaults) }.boxed_local()
    }
}

/// An intent seeded from text, with no questions or items, in `state` and
/// `phase`. Its title is the slug as words ("fix-zoom" is "Fix zoom"), its
/// kind Enhancement, its repository the same for every intent built here,
/// and every time it has is [`INTENT_TIME`] (`queued_at`, `sent_at` and
/// `resolved_at` only as far as `state` implies them).
pub fn intent(slug: &str, state: IntentState, phase: Phase) -> Intent {
    let at: DateTime<Utc> = INTENT_TIME.parse().unwrap();
    let words = slug.replace('-', " ");
    let mut chars = words.chars();
    let title = chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    Intent {
        id: IntentId::new(),
        repo_id: INTENT_REPO.parse::<RepoId>().unwrap(),
        slug: slug.to_owned(),
        state,
        phase,
        seed: Seed {
            source: SeedSource::Text,
            text: format!("Seed of {slug}"),
            kind: None,
        },
        content: Content {
            title,
            kind: IntentKind::Enhancement,
            current_behavior: String::new(),
            questions: Vec::new(),
            preconditions: Vec::new(),
            criteria: Vec::new(),
            stories: Vec::new(),
            depends_on: Vec::new(),
        },
        agent_id: None,
        created_at: at,
        updated_at: at,
        queued_at: (state == IntentState::Queued).then_some(at),
        sent_at: matches!(state, IntentState::Sent | IntentState::Resolved).then_some(at),
        resolved_at: (state == IntentState::Resolved).then_some(at),
        last_start_error: None,
    }
}

/// An item (not a dependency) with this text and status, never edited.
pub fn item(text: &str, status: ItemStatus) -> Item {
    Item {
        id: ItemId::new(),
        text: text.to_owned(),
        status,
        edited: false,
        intent_ref: None,
    }
}

/// A question offering `options` in order, with the ids "o1", "o2", ..
pub fn question(text: &str, options: &[&str], answer: Answer) -> Question {
    Question {
        id: QuestionId::new(),
        text: text.to_owned(),
        options: options
            .iter()
            .enumerate()
            .map(|(i, label)| QuestionOption {
                id: format!("o{}", i + 1),
                label: (*label).to_owned(),
            })
            .collect(),
        answer,
    }
}

/// A clock that always reads `rfc3339` (for `ShellOptions::clock`).
pub fn fixed_clock(rfc3339: &str) -> Clock {
    let now = DateTime::parse_from_rfc3339(rfc3339).expect("an RFC 3339 time");
    Rc::new(move || now)
}

/// A connected model holding these agents and repositories, with its intent
/// list loaded as `intents`. No agent defaults are loaded.
pub fn model_with(intents: Vec<Intent>, agents: Vec<Agent>, repos: Vec<Repo>) -> ShellModel {
    let mut model = ShellModel::new();
    model.apply(Action::Snapshot {
        agents,
        repos,
        latest_seq: 1,
    });
    model.apply(Action::IntentsLoaded(intents));
    model.apply(Action::ConnectionChanged(Connection::Connected));
    model
}

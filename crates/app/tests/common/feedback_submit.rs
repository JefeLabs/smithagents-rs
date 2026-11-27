//! [`FakeFeedback`]: feedback commands that record each call, then answer it
//! with the next result queued for its kind, or else wait until the test
//! answers it (or forever), like `FakeIntents`. As there, the link's `Action`
//! for an answer is left to the test. Plus builders for the targets and the
//! outcomes the Submit tests answer with, and for an idea intent.

use std::cell::RefCell;
use std::collections::VecDeque;

use futures::channel::oneshot;
use futures::future::LocalBoxFuture;
use futures::FutureExt as _;
use smithagents_app::link::{FeedbackCommands, IntentAnswer, LinkError};
use smithagents_client::FeedbackFile;
use smithagents_core::feedback::{
    FeedbackOrigin, FeedbackOutcome, FeedbackRequest, FeedbackSettings, FeedbackTarget,
    FeedbackTargets, IdeaPackage, SubmissionId, ToolReadiness,
};
use smithagents_core::ids::{IntentId, RepoId};
use smithagents_core::intent::{Intent, IntentState, Phase};

/// The branch every outcome built here names.
pub const BRANCH: &str = "feedback/2026-11-26-fix-the-zoom";
/// The commit every outcome built here names (its first 7 characters are
/// what the sentences show).
pub const COMMIT: &str = "0123abc4567";
pub const FOLDER: &str = "docs/issues/2026-11-26-fix-the-zoom";

/// One feedback command, as a view sent it.
#[derive(Debug, Clone, PartialEq)]
pub enum FeedbackCall {
    SetSettings(FeedbackSettings),
    Targets,
    Submit(FeedbackRequest, Vec<FeedbackFile>),
    Retry(SubmissionId),
    /// The idea intent, and whether the user acknowledged a public
    /// repository.
    OpenPullRequest(IntentId, bool),
    /// The idea intent, and whether the user acknowledged that the new
    /// intent's pull request may be public.
    Implement(IntentId, bool),
}

type Reply = Result<FeedbackSettings, LinkError>;
type TargetsReply = Result<FeedbackTargets, LinkError>;
type OutcomeReply = Result<FeedbackOutcome, LinkError>;
type IntentReply = Result<Intent, LinkError>;

/// Answers of one kind: queued ones first, else the calls wait.
struct Answers<T> {
    queued: VecDeque<T>,
    waiting: VecDeque<oneshot::Sender<T>>,
}

impl<T> Default for Answers<T> {
    fn default() -> Self {
        Self {
            queued: VecDeque::new(),
            waiting: VecDeque::new(),
        }
    }
}

impl<T: 'static> Answers<T> {
    fn reply(&mut self, gone: impl FnOnce() -> T + 'static) -> LocalBoxFuture<'static, T> {
        if let Some(result) = self.queued.pop_front() {
            return async move { result }.boxed_local();
        }
        let (tx, rx) = oneshot::channel();
        self.waiting.push_back(tx);
        async move { rx.await.unwrap_or_else(|_| gone()) }.boxed_local()
    }

    fn answer(&mut self, result: T, what: &str) {
        let sender = self
            .waiting
            .pop_front()
            .unwrap_or_else(|| panic!("a {what} command waits for an answer"));
        let _ = sender.send(result);
    }
}

#[derive(Default)]
pub struct FakeFeedback {
    pub calls: RefCell<Vec<FeedbackCall>>,
    settings: RefCell<Answers<Reply>>,
    targets: RefCell<Answers<TargetsReply>>,
    outcomes: RefCell<Answers<OutcomeReply>>,
    intents: RefCell<Answers<IntentReply>>,
}

impl FakeFeedback {
    /// The answer of the next settings call.
    pub fn queue(&self, result: Reply) {
        self.settings.borrow_mut().queued.push_back(result);
    }

    /// Answers the oldest settings call still waiting.
    pub fn answer(&self, result: Reply) {
        self.settings.borrow_mut().answer(result, "settings");
    }

    /// The answer of the next targets call.
    pub fn queue_targets(&self, result: TargetsReply) {
        self.targets.borrow_mut().queued.push_back(result);
    }

    /// Answers the oldest targets call still waiting.
    pub fn answer_targets(&self, result: TargetsReply) {
        self.targets.borrow_mut().answer(result, "targets");
    }

    /// The answer of the next submit or retry.
    pub fn queue_outcome(&self, result: OutcomeReply) {
        self.outcomes.borrow_mut().queued.push_back(result);
    }

    /// Answers the oldest submit or retry still waiting.
    pub fn answer_outcome(&self, result: OutcomeReply) {
        self.outcomes.borrow_mut().answer(result, "submit or retry");
    }

    /// How many submits and retries wait for an answer.
    pub fn outcomes_waiting(&self) -> usize {
        self.outcomes.borrow().waiting.len()
    }

    /// The answer of the next idea action (Open pull request, Create
    /// implementation intent).
    pub fn queue_intent(&self, result: IntentReply) {
        self.intents.borrow_mut().queued.push_back(result);
    }

    /// Answers the oldest idea action still waiting.
    pub fn answer_intent(&self, result: IntentReply) {
        self.intents.borrow_mut().answer(result, "idea action");
    }

    /// The submits sent, in order.
    pub fn submits(&self) -> Vec<(FeedbackRequest, Vec<FeedbackFile>)> {
        self.calls
            .borrow()
            .iter()
            .filter_map(|call| match call {
                FeedbackCall::Submit(request, files) => Some((request.clone(), files.clone())),
                _ => None,
            })
            .collect()
    }

    /// The retries sent, in order.
    pub fn retries(&self) -> Vec<SubmissionId> {
        self.calls
            .borrow()
            .iter()
            .filter_map(|call| match call {
                FeedbackCall::Retry(id) => Some(*id),
                _ => None,
            })
            .collect()
    }

    fn record(&self, call: FeedbackCall) {
        self.calls.borrow_mut().push(call);
    }
}

impl FeedbackCommands for FakeFeedback {
    fn set_feedback_settings(
        &self,
        settings: FeedbackSettings,
    ) -> LocalBoxFuture<'static, Result<FeedbackSettings, LinkError>> {
        self.record(FeedbackCall::SetSettings(settings));
        self.settings.borrow_mut().reply(|| Err(LinkError::Stopped))
    }

    fn feedback_targets(&self) -> LocalBoxFuture<'static, Result<FeedbackTargets, LinkError>> {
        self.record(FeedbackCall::Targets);
        self.targets.borrow_mut().reply(|| Err(LinkError::Stopped))
    }

    fn submit_feedback(
        &self,
        request: FeedbackRequest,
        files: Vec<FeedbackFile>,
    ) -> LocalBoxFuture<'static, Result<FeedbackOutcome, LinkError>> {
        self.record(FeedbackCall::Submit(request, files));
        self.outcomes.borrow_mut().reply(|| Err(LinkError::Stopped))
    }

    fn retry_feedback(
        &self,
        id: SubmissionId,
    ) -> LocalBoxFuture<'static, Result<FeedbackOutcome, LinkError>> {
        self.record(FeedbackCall::Retry(id));
        self.outcomes.borrow_mut().reply(|| Err(LinkError::Stopped))
    }

    fn open_pull_request(&self, id: IntentId, public_acknowledged: bool) -> IntentAnswer {
        self.record(FeedbackCall::OpenPullRequest(id, public_acknowledged));
        self.intents.borrow_mut().reply(|| Err(LinkError::Stopped))
    }

    fn create_implementation_intent(
        &self,
        id: IntentId,
        public_acknowledged: bool,
    ) -> IntentAnswer {
        self.record(FeedbackCall::Implement(id, public_acknowledged));
        self.intents.borrow_mut().reply(|| Err(LinkError::Stopped))
    }
}

/// An idea intent ("fix-zoom", as `common::intents::intent` builds it) in
/// `state`, whose pull request is `pr_url`: its origin is an idea package
/// with an explainer and diagrams, nothing done yet.
pub fn idea_intent(state: IntentState, pr_url: Option<&str>) -> Intent {
    let mut idea = super::intents::intent("fix-zoom", state, Phase::Review);
    idea.base_branch = Some("idea/fix-zoom".to_owned());
    idea.origin = Some(FeedbackOrigin {
        folder: FOLDER.to_owned(),
        branch: "idea/fix-zoom".to_owned(),
        commit: COMMIT.to_owned(),
        idea: Some(IdeaPackage {
            explainer: true,
            diagrams: true,
            demo_video: false,
            pr_url: pr_url.map(str::to_owned),
            not_done: None,
            implementation_intent_id: None,
            pr_commit: None,
            pr_pushed: false,
        }),
    });
    idea
}

/// Targets for these repositories, each `(id, has a GitHub remote, is
/// public)`, with `gh` and `agent-browser` ready or not installed. No meta
/// repository is stored.
pub fn targets(
    repos: &[(RepoId, bool, Option<bool>)],
    gh: bool,
    agent_browser: bool,
) -> FeedbackTargets {
    let readiness = |ready: bool| ToolReadiness {
        ready,
        reason: (!ready).then(|| "not installed".to_owned()),
    };
    FeedbackTargets {
        repos: repos
            .iter()
            .map(|&(repo_id, has_github_remote, is_public)| FeedbackTarget {
                repo_id,
                has_github_remote,
                is_public,
            })
            .collect(),
        meta_repo_id: None,
        gh: readiness(gh),
        agent_browser: readiness(agent_browser),
    }
}

/// A complete outcome of submission `id` that created `intent`, in the
/// intent's repository, on [`BRANCH`] at [`COMMIT`].
pub fn outcome_with_intent(id: SubmissionId, intent: Intent) -> FeedbackOutcome {
    FeedbackOutcome {
        submission_id: id,
        repo_id: intent.repo_id,
        branch: BRANCH.to_owned(),
        commit: COMMIT.to_owned(),
        folder: FOLDER.to_owned(),
        pushed: false,
        intent: Some(intent),
        issue_url: None,
        failed: None,
    }
}

/// A complete outcome of submission `id` that opened the GitHub issue at
/// `url` in `repo`.
pub fn outcome_with_issue(id: SubmissionId, repo: RepoId, url: &str) -> FeedbackOutcome {
    FeedbackOutcome {
        submission_id: id,
        repo_id: repo,
        branch: BRANCH.to_owned(),
        commit: COMMIT.to_owned(),
        folder: FOLDER.to_owned(),
        pushed: true,
        intent: None,
        issue_url: Some(url.to_owned()),
        failed: None,
    }
}

//! The idea package: a submission with `idea` set becomes an idea intent,
//! committed on `idea/<slug>`, whose agent prepares an explainer, diagrams
//! and a demo video in the issue folder instead of implementing anything.
//! The daemon fixes the intent's acceptance criteria from the ticked parts;
//! polish and review keep them (see [`crate::polish::apply_pass2`] and
//! [`crate::intents::IntentService::review`]).
//!
//! Once its agent is done, the idea intent offers two actions:
//!
//! - [`FeedbackService::open_pull_request`] pushes the agent's work to
//!   `origin` as the idea branch and opens a draft pull request with `gh`.
//!   What is pushed is one commit, chosen at the first press and recorded
//!   before anything leaves the machine (a retry pushes the same one), and
//!   only when every commit on top of the idea's own changes nothing outside
//!   the issue folder: the agent was told to commit only there, and an idea
//!   pull request must not publish code nobody reviewed as part of the idea.
//!   Each finished step (the push, the pull request) is recorded, so a retry
//!   after a failure runs only what is left. The pull request is looked for
//!   (by the signed-in user, for that branch) before one is opened, and none
//!   is opened for a minute after `gh` may have opened one without saying
//!   so, as for issues.
//! - [`FeedbackService::create_implementation_intent`] creates, once, the
//!   intent that implements the idea: it starts from the idea agent's
//!   branch, has the idea's folder as its origin, and depends (fixed) on the
//!   idea intent.
//!
//! One action of an idea intent runs at a time; each counts as running work
//! for the idle monitor. Nothing git, `gh` or the user wrote reaches a log,
//! an error or an event: failures are fixed sentences with a code.

use smithagents_core::agent::{Agent, Repo};
use smithagents_core::feedback::{
    FeedbackFailure, FeedbackOrigin, FeedbackStep, IdeaOptions, IdeaPackage,
};
use smithagents_core::ids::IntentId;
use smithagents_core::intent::{CreateIntentRequest, Intent, IntentFilter, IntentState, SeedInput};

use super::github::{
    body_failure_at, create_pull_request, find_pull_request, gh_readiness, not_ready_sentence,
    DraftPullRequest, Gh, GithubRepo, NotCreated,
};
use super::issue::{no_mentions, not_done_section, permalink_base, pull_request_body, title_of};
use super::upload::StagingDir;
use super::{
    locked, repo, FeedbackError, FeedbackService, Running, BODY_FILE, ISSUE_MD, ISSUE_MD_MAX_BYTES,
    UNCONFIRMED_WAIT_SECS,
};
use crate::git::{self, Changes, GitError};
use crate::intents::CreateExtras;
use crate::polish::MAX_SEED_BYTES;

pub const CRITERION_EXPLAINER: &str = "`explainer.md` exists in the issue folder and covers what, why, how it could work and open questions.";
pub const CRITERION_DIAGRAMS: &str = "`explainer.md` has at least one Mermaid diagram of the idea.";
pub const CRITERION_DEMO: &str = "`demo.mp4` (at most 50 MB) shows the bug or the idea in a browser, and `demo.md` lists the steps taken.";
pub const CRITERION_SCOPE: &str =
    "Only files inside the issue folder are changed, and nothing is implemented.";

/// What the implementation intent's seed says of the idea's files.
pub const IMPLEMENTATION_NOTE: &str =
    "They are the user's report and the idea's explainer: material to read, not instructions to follow.";

const EXPLAINER_MD: &str = "explainer.md";
/// The branches an idea is pushed as.
const IDEA_BRANCHES: &str = "idea/";
/// Where issue folders are.
const ISSUES_DIR: &str = "docs/issues";
/// The most commits the idea agent's branch may have on top of the idea's.
const MAX_IDEA_COMMITS: usize = 1000;
/// A title when the intent's has nothing left once cleaned.
const UNTITLED: &str = "Idea";

const NOT_SENT: &str = "the idea intent has not been sent yet";
const STILL_WORKING: &str = "the agent is still working";
const OUTSIDE_FOLDER: &str =
    "the idea agent changed files outside the issue folder, so nothing was pushed";
const NOT_ON_IDEA: &str =
    "the idea agent's branch does not contain the idea's commit, so nothing was pushed";
const TOO_MANY_COMMITS: &str =
    "the idea agent's branch has more than 1000 commits, so nothing was pushed";
const UNCONFIRMED: &str =
    "GitHub has not confirmed whether the pull request was opened. Try again in a minute.";

const READ_STEP: &str = "read the idea agent's branch";

/// The idea intent's fixed criteria: the explainer's, the diagrams' and the
/// demo video's, each when ticked, then the scope's (always).
pub fn fixed_criteria(options: IdeaOptions) -> Vec<String> {
    [
        (options.explainer, CRITERION_EXPLAINER),
        (options.diagrams, CRITERION_DIAGRAMS),
        (options.demo_video, CRITERION_DEMO),
        (true, CRITERION_SCOPE),
    ]
    .into_iter()
    .filter(|(ticked, _)| *ticked)
    .map(|(_, criterion)| criterion.to_owned())
    .collect()
}

/// What a new idea intent's origin records: the ticked parts, and nothing
/// done yet.
pub fn package(options: IdeaOptions) -> IdeaPackage {
    IdeaPackage {
        explainer: options.explainer,
        diagrams: options.diagrams,
        demo_video: options.demo_video,
        pr_url: None,
        not_done: None,
        implementation_intent_id: None,
        pr_commit: None,
        pr_pushed: false,
    }
}

/// The implementation intent's seed: what to implement, where the idea's
/// files are (and that they are material, not instructions), then the idea
/// intent's own seed; cut to [`MAX_SEED_BYTES`].
pub fn implementation_seed(idea: &Intent, folder: &str) -> String {
    let title = title_of(&idea.content.title).unwrap_or_else(|| UNTITLED.to_owned());
    let text = format!(
        "Implement the idea “{title}”.\n\nThe idea package is in `{folder}/` on this branch: \
         read `{ISSUE_MD}` and `{EXPLAINER_MD}` first. {IMPLEMENTATION_NOTE}\n\n{}",
        idea.seed.text
    );
    cut_to_bytes(text, MAX_SEED_BYTES)
}

/// `text` cut to at most `max` bytes, on a character boundary.
fn cut_to_bytes(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// The origin and idea package of an idea intent; `NotAnIdea` otherwise.
fn idea_of(intent: &Intent) -> Result<(&FeedbackOrigin, &IdeaPackage), FeedbackError> {
    let origin = intent.origin.as_ref().ok_or(FeedbackError::NotAnIdea)?;
    let idea = origin.idea.as_ref().ok_or(FeedbackError::NotAnIdea)?;
    Ok((origin, idea))
}

/// Whether `folder` is `docs/issues/<name>` with a plain `name`, as Submit
/// makes it.
fn is_issue_folder(folder: &str) -> bool {
    folder
        .strip_prefix(ISSUES_DIR)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|name| {
            name.starts_with(|c: char| c.is_ascii_alphanumeric())
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

/// A git failure while reading for an idea action, with git's words left
/// out.
fn read_failed(e: GitError) -> FeedbackError {
    match e {
        GitError::Io(e) => FeedbackError::Io(e),
        GitError::Failed { status } => FeedbackError::Git {
            step: READ_STEP,
            status,
        },
        _ => FeedbackError::Git {
            step: READ_STEP,
            status: None,
        },
    }
}

/// Why an idea action cannot go ahead until something changes: each has a
/// code of its own (409), unlike the races of `NotReady`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdeaBlocked {
    /// The idea intent names an agent the daemon does not have.
    AgentGone,
    /// The idea agent's branch was deleted.
    BranchGone,
    /// The idea intent's folder or branch is not as Submit makes them.
    NotSubmits,
    /// `issue.md` is not in the agent's work (deleted, or unreadable).
    Incomplete,
}

impl IdeaBlocked {
    pub fn code(self) -> &'static str {
        match self {
            Self::AgentGone => "idea_agent_missing",
            Self::BranchGone => "idea_branch_missing",
            Self::NotSubmits => "idea_origin_invalid",
            Self::Incomplete => "idea_incomplete",
        }
    }

    pub fn sentence(self) -> &'static str {
        match self {
            Self::AgentGone => "the idea intent's agent no longer exists",
            Self::BranchGone => "the idea agent's branch no longer exists",
            Self::NotSubmits => "the idea intent's folder or branch is not one Submit made",
            Self::Incomplete => "The idea's issue.md is missing from the agent's branch, so a pull request cannot be opened.",
        }
    }
}

/// The texts of the idea's pull request, read at the commit to push.
struct PullRequestText {
    body: String,
    /// The explainer's "Not done" section.
    not_done: Option<String>,
}

impl FeedbackService {
    /// "Open pull request" of the idea intent `id`: its agent's work pushed
    /// to `origin` as the idea branch (`idea/<slug>`, never forced), then a
    /// draft pull request into the default branch, whose address and the
    /// explainer's "Not done" section are saved on the intent and published.
    ///
    /// An intent with a pull request answers as it is. Refused, with
    /// nothing pushed: an intent that is not an idea (`NotAnIdea`), not sent
    /// or resolved, or whose agent still works (`NotReady`); one whose agent,
    /// branch, folder or `issue.md` is not there (`IdeaBlocked`); a
    /// repository whose `origin` is not on github.com (`NoGithubRemote`);
    /// `gh` not ready (`GhNotReady`); an agent's branch that changes more
    /// than the issue folder (`OutOfScope`); a default branch with commits
    /// `origin` has not got, or a repository never fetched (`Unpublished`).
    /// A repository that is, or may be, public needs `public_acknowledged`
    /// (`PublicNotAcknowledged`), asked on every press that would publish.
    /// A failed push or `gh` call is `Step`, and what succeeded before it
    /// stays done.
    pub async fn open_pull_request(
        &self,
        id: IntentId,
        public_acknowledged: bool,
    ) -> Result<Intent, FeedbackError> {
        let _running = Running::start(&self.parts.running);
        let _own = self.parts.ideas.lock(id).await;
        let intent = self.intent(id).await?;
        let (origin, idea) = idea_of(&intent)?;
        if idea.pr_url.is_some() {
            return Ok(intent);
        }
        let remembered = locked(&self.parts.pull_requests).get(&id).cloned();
        if let Some((url, not_done)) = remembered {
            return Ok(self.record_pull_request(&intent, url, not_done).await);
        }
        let repo = self.repo_of(&intent).await?;
        if !is_issue_folder(&origin.folder) || !origin.branch.starts_with(IDEA_BRANCHES) {
            return Err(FeedbackError::IdeaBlocked(IdeaBlocked::NotSubmits));
        }
        let agent = self.idle_agent(&intent).await?;
        let github = self
            .github_of(&repo)
            .await?
            .ok_or(FeedbackError::NoGithubRemote)?;
        let gh = self.gh();
        if let Some(reason) = gh_readiness(&gh).await.reason {
            return Err(FeedbackError::GhNotReady(not_ready_sentence(&reason)));
        }
        // Asked on every press, before a commit is chosen, recorded or
        // pushed.
        self.acknowledgement(&github, public_acknowledged)
            .await
            .inspect_err(|e| {
                tracing::warn!(intent_id = %id, step = "acknowledge", code = e.code(), "the idea branch was not pushed");
            })?;
        for branch in [&repo.default_branch, &origin.branch] {
            let valid = !branch.starts_with('-')
                && git::bounded(git::is_valid_branch_name(&repo.path, branch))
                    .await
                    .map_err(read_failed)?;
            if !valid {
                return Err(FeedbackError::IdeaBlocked(IdeaBlocked::NotSubmits));
            }
        }
        // The commit to push: the one chosen at an earlier press, else the
        // agent's branch as it is now ([`Self::settled_head`]). One chosen
        // before that was never pushed and is gone from the repository (the
        // branch was reset and the commit pruned) cannot be pushed, so one
        // is chosen again.
        let chosen = match &idea.pr_commit {
            Some(commit) if !idea.pr_pushed => {
                let present = git::is_commit_id(commit)
                    && git::rev_parse(&repo.path, commit)
                        .await
                        .map_err(read_failed)?
                        .is_some_and(|found| found == *commit);
                present.then(|| commit.clone())
            }
            chosen => chosen.clone(),
        };
        let (head, pushed, new) = match chosen {
            Some(commit) => (commit, idea.pr_pushed, false),
            None => (self.settled_head(&repo, &agent).await?, false, true),
        };
        if new {
            check_scope(&repo, origin, &head).await.inspect_err(|e| {
                tracing::warn!(intent_id = %id, step = "check_scope", code = e.code(), "the idea branch was not pushed");
            })?;
        }
        // Before anything is recorded or pushed: an idea without its
        // issue.md opens no pull request.
        let text = pull_request_text(&repo, &github, &origin.folder, &head)
            .await
            .inspect_err(|e| {
                tracing::warn!(intent_id = %id, step = "read_idea", code = e.code(), "the idea branch was not pushed");
            })?;
        let title =
            no_mentions(&title_of(&intent.content.title).unwrap_or_else(|| UNTITLED.to_owned()));
        if new {
            self.save_idea(id, |p| p.pr_commit = Some(head.clone()))
                .await?;
        }
        if !pushed {
            // Nothing the idea's commit is built on may publish the user's
            // unpushed work.
            repo::require_published_parents(&repo, &origin.commit)
                .await
                .inspect_err(|e| {
                    tracing::warn!(intent_id = %id, step = "push", code = e.code(), "the idea branch was not pushed");
                })?;
            repo::push_commit(&repo, &head, &origin.branch)
                .await
                .map_err(|failure| {
                    tracing::warn!(intent_id = %id, step = "push", code = failure.code.as_str(), "the idea branch was not pushed");
                    FeedbackError::Step(failure)
                })?;
            tracing::info!(intent_id = %id, step = "push", "idea branch pushed");
            // Not saved, the next press pushes the same commit again, which
            // changes nothing.
            let _ = self.save_idea(id, |p| p.pr_pushed = true).await;
        }

        let opened = self
            .make_pull_request(
                id,
                &gh,
                &github,
                (&repo.default_branch, &origin.branch),
                &title,
                &text.body,
            )
            .await;
        let url = opened.map_err(|failure| {
            tracing::warn!(intent_id = %id, step = "open_pull_request", code = failure.code.as_str(), "the idea pull request was not opened");
            FeedbackError::Step(failure)
        })?;
        tracing::info!(intent_id = %id, step = "open_pull_request", "idea pull request opened or found");
        locked(&self.parts.pull_requests).insert(id, (url.clone(), text.not_done.clone()));
        Ok(self.record_pull_request(&intent, url, text.not_done).await)
    }

    /// Saves the pull request's address and "Not done" on the idea intent
    /// (tried twice) and answers with the intent. The pull request exists
    /// either way, so a failed save is not an error: the address stays
    /// remembered for the next press, and the answer carries it all the
    /// same.
    async fn record_pull_request(
        &self,
        intent: &Intent,
        url: String,
        not_done: Option<String>,
    ) -> Intent {
        let id = intent.id;
        let change = |p: &mut IdeaPackage| {
            p.pr_url = Some(url.clone());
            p.not_done = not_done.clone();
        };
        let saved = match self.save_idea(id, change).await {
            Ok(saved) => Ok(saved),
            Err(_) => self.save_idea(id, change).await,
        };
        match saved {
            Ok(saved) => {
                locked(&self.parts.pull_requests).remove(&id);
                saved
            }
            Err(e) => {
                tracing::error!(intent_id = %id, step = "record_pull_request", code = e.code(), "the idea pull request was opened but not recorded");
                let mut answer = intent.clone();
                if let Some(p) = answer.origin.as_mut().and_then(|o| o.idea.as_mut()) {
                    change(p);
                }
                answer
            }
        }
    }

    /// The pull request of the pushed idea branch `branches.1` into
    /// `branches.0`: one the signed-in user opened before
    /// ([`find_pull_request`]); else, unless `gh pr create` ran without a
    /// usable answer less than a minute ago (`pr_unconfirmed`), a new draft
    /// ([`create_pull_request`]) from a body written to a scratch directory
    /// of its own (removed afterwards).
    async fn make_pull_request(
        &self,
        id: IntentId,
        gh: &Gh<'_>,
        github: &GithubRepo,
        (base, head): (&str, &str),
        title: &str,
        body: &str,
    ) -> Result<String, FeedbackFailure> {
        if let Some(url) = find_pull_request(gh, github, head).await? {
            locked(&self.parts.unconfirmed_prs).remove(&id);
            return Ok(url);
        }
        let last = locked(&self.parts.unconfirmed_prs).get(&id).copied();
        if let Some(at) = last {
            // A clock that went back counts as no time passed.
            if (self.clock)() - at < chrono::TimeDelta::seconds(UNCONFIRMED_WAIT_SECS) {
                return Err(FeedbackFailure {
                    step: FeedbackStep::OpenPullRequest,
                    code: "pr_unconfirmed".to_owned(),
                    message: UNCONFIRMED.to_owned(),
                });
            }
        }
        let write_failed = |kind: std::io::ErrorKind| {
            tracing::warn!(intent_id = %id, step = "open_pull_request", ?kind, "could not write the pull request's body");
            body_failure_at(FeedbackStep::OpenPullRequest)
        };
        let scratch =
            StagingDir::scratch(&self.staging_root(), id).map_err(|e| write_failed(e.kind()))?;
        let body_file = scratch.path().join(BODY_FILE);
        tokio::fs::write(&body_file, body)
            .await
            .map_err(|e| write_failed(e.kind()))?;
        let pr = DraftPullRequest {
            base,
            head,
            title,
            body_file: &body_file,
        };
        match create_pull_request(gh, github, &pr).await {
            Ok(url) => {
                locked(&self.parts.unconfirmed_prs).remove(&id);
                Ok(url)
            }
            Err(NotCreated {
                failure,
                unconfirmed,
            }) => {
                if unconfirmed {
                    locked(&self.parts.unconfirmed_prs).insert(id, (self.clock)());
                }
                Err(failure)
            }
        }
    }

    /// "Create implementation intent" of the idea intent `id`: an intent of
    /// the idea's kind and repository that starts from the idea agent's
    /// branch, has the idea's folder as its origin, depends (fixed) on the
    /// idea intent, and is seeded with [`implementation_seed`]. Made once:
    /// its id is saved on the idea intent, and an intent that already
    /// depends that way on the idea is taken (its id was not saved) instead
    /// of making another. Refused as [`Self::open_pull_request`] is for an
    /// intent that is not an idea or whose agent is not done; no `gh` runs.
    pub async fn create_implementation_intent(
        &self,
        id: IntentId,
    ) -> Result<Intent, FeedbackError> {
        let _running = Running::start(&self.parts.running);
        let _own = self.parts.ideas.lock(id).await;
        let intent = self.intent(id).await?;
        let (origin, idea) = idea_of(&intent)?;
        if let Some(existing) = self.implementation_of(&intent, idea).await? {
            return Ok(existing);
        }
        let repo = self.repo_of(&intent).await?;
        if !is_issue_folder(&origin.folder) {
            return Err(FeedbackError::IdeaBlocked(IdeaBlocked::NotSubmits));
        }
        let agent = self.idle_agent(&intent).await?;
        let head = self.settled_head(&repo, &agent).await?;
        let req = CreateIntentRequest {
            repo_id: Some(intent.repo_id),
            kind: Some(intent.content.kind),
            seed: SeedInput::Text(implementation_seed(&intent, &origin.folder)),
        };
        let extras = CreateExtras {
            base_branch: Some(agent.branch.clone()),
            origin: Some(FeedbackOrigin {
                folder: origin.folder.clone(),
                branch: agent.branch.clone(),
                commit: head,
                idea: None,
            }),
            fixed_criteria: Vec::new(),
            fixed_dependency: Some(id),
        };
        let created = self
            .parts
            .intents
            .create_with(req, extras)
            .await
            .map_err(|e| {
                tracing::warn!(intent_id = %id, step = "create_implementation_intent", code = e.status_and_code().1, "the implementation intent was not created");
                FeedbackError::Intent(e)
            })?;
        tracing::info!(intent_id = %id, implementation_intent_id = %created.id, step = "create_implementation_intent", "implementation intent created");
        self.record_implementation(id, created.id).await;
        Ok(created)
    }

    /// The idea's implementation intent made before: the one whose id is
    /// saved, else one of the repository whose fixed dependency is the idea
    /// intent (only this action makes such a dependency), whose id is then
    /// saved.
    async fn implementation_of(
        &self,
        idea_intent: &Intent,
        idea: &IdeaPackage,
    ) -> Result<Option<Intent>, FeedbackError> {
        let id = idea_intent.id;
        if let Some(saved) = idea.implementation_intent_id {
            if let Some(found) = self.store.get_intent(saved).await? {
                return Ok(Some(found));
            }
        }
        let filter = IntentFilter {
            state: None,
            repo_id: Some(idea_intent.repo_id),
        };
        let found = self
            .store
            .list_intents(filter)
            .await?
            .into_iter()
            .find(|i| {
                i.content
                    .depends_on
                    .iter()
                    .any(|d| d.fixed && d.intent_ref == Some(id))
            });
        if let Some(found) = &found {
            tracing::info!(intent_id = %id, implementation_intent_id = %found.id, step = "create_implementation_intent", "taking up an implementation intent made before");
            self.record_implementation(id, found.id).await;
        }
        Ok(found)
    }

    /// Saves the implementation intent's id on the idea intent, tried twice;
    /// when it is not saved, [`Self::implementation_of`] finds the intent by
    /// its dependency.
    async fn record_implementation(&self, id: IntentId, implementation: IntentId) {
        let change = |p: &mut IdeaPackage| p.implementation_intent_id = Some(implementation);
        if self.save_idea(id, change).await.is_err() && self.save_idea(id, change).await.is_err() {
            tracing::error!(intent_id = %id, step = "record_implementation", code = "store_error", "the implementation intent was made but not recorded");
        }
    }

    async fn intent(&self, id: IntentId) -> Result<Intent, FeedbackError> {
        self.parts
            .intents
            .get(id)
            .await
            .map_err(FeedbackError::Intent)
    }

    async fn repo_of(&self, intent: &Intent) -> Result<Repo, FeedbackError> {
        self.store
            .get_repo(intent.repo_id)
            .await?
            .ok_or(FeedbackError::RepoNotFound)
    }

    /// The idea intent's agent, once the intent was sent (or resolved) and
    /// the agent has no turn in flight.
    async fn idle_agent(&self, intent: &Intent) -> Result<Agent, FeedbackError> {
        let agent = match (intent.state, intent.agent_id) {
            (IntentState::Sent | IntentState::Resolved, Some(agent)) => agent,
            _ => return Err(FeedbackError::NotReady(NOT_SENT.to_owned())),
        };
        if self.parts.agents.turn_in_flight(agent) {
            return Err(FeedbackError::NotReady(STILL_WORKING.to_owned()));
        }
        self.store
            .get_agent(agent)
            .await?
            .ok_or(FeedbackError::IdeaBlocked(IdeaBlocked::AgentGone))
    }

    /// The commit the agent's branch (`refs/heads/<branch>`: never a tag of
    /// that name) is at, read right after a check that no turn is in
    /// flight; `NotReady` when one is, or when one started while the branch
    /// was read, since it may then hold a commit made in the middle of it.
    async fn settled_head(&self, repo: &Repo, agent: &Agent) -> Result<String, FeedbackError> {
        if self.parts.agents.turn_in_flight(agent.id) {
            return Err(FeedbackError::NotReady(STILL_WORKING.to_owned()));
        }
        let head = git::rev_parse(&repo.path, &format!("refs/heads/{}", agent.branch))
            .await
            .map_err(read_failed)?
            .ok_or(FeedbackError::IdeaBlocked(IdeaBlocked::BranchGone))?;
        #[cfg(feature = "test-util")]
        self.after_reading_the_head().await;
        if self.parts.agents.turn_in_flight(agent.id) {
            return Err(FeedbackError::NotReady(STILL_WORKING.to_owned()));
        }
        Ok(head)
    }

    /// [`crate::intents::IntentService::update_idea`] as a feedback error.
    async fn save_idea(
        &self,
        id: IntentId,
        change: impl FnOnce(&mut IdeaPackage),
    ) -> Result<Intent, FeedbackError> {
        #[cfg(feature = "test-util")]
        if self.idea_save_fails() {
            return Err(
                crate::store::StoreError::Conflict("a test failed this save".into()).into(),
            );
        }
        self.parts
            .intents
            .update_idea(id, change)
            .await
            .map_err(FeedbackError::Intent)
    }
}

/// `Ok` when every commit on the agent's branch after the idea's own commit
/// (`origin.commit`) up to `head` changes only files inside the issue
/// folder, and `head` has that commit in its history; else `OutOfScope`.
async fn check_scope(
    repo: &Repo,
    origin: &FeedbackOrigin,
    head: &str,
) -> Result<(), FeedbackError> {
    let base = &origin.commit;
    let present = git::is_commit_id(base)
        && git::rev_parse(&repo.path, base)
            .await
            .map_err(read_failed)?
            .is_some_and(|found| found == *base);
    if !present {
        return Err(FeedbackError::OutOfScope(NOT_ON_IDEA));
    }
    let inside = format!("{}/", origin.folder);
    match git::changes_since(&repo.path, base, head, MAX_IDEA_COMMITS)
        .await
        .map_err(read_failed)?
    {
        Changes::Paths(paths) if paths.iter().all(|p| p.starts_with(&inside)) => Ok(()),
        Changes::Paths(_) => Err(FeedbackError::OutOfScope(OUTSIDE_FOLDER)),
        Changes::NotOnBase => Err(FeedbackError::OutOfScope(NOT_ON_IDEA)),
        Changes::TooMany => Err(FeedbackError::OutOfScope(TOO_MANY_COMMITS)),
    }
}

/// The pull request's body ([`pull_request_body`] of `issue.md` and
/// `explainer.md` as committed at `head`, mentioning no one: [`no_mentions`])
/// and the explainer's "Not done" section. `IdeaBlocked(Incomplete)` when
/// `issue.md` is not there or cannot be read.
async fn pull_request_text(
    repo: &Repo,
    github: &GithubRepo,
    folder: &str,
    head: &str,
) -> Result<PullRequestText, FeedbackError> {
    let read = |name: &str| {
        let path = format!("{folder}/{name}");
        async move { git::show_file(&repo.path, head, &path, ISSUE_MD_MAX_BYTES).await }
    };
    let Ok(Some(issue_md)) = read(ISSUE_MD).await else {
        return Err(FeedbackError::IdeaBlocked(IdeaBlocked::Incomplete));
    };
    let explainer = read(EXPLAINER_MD).await.map_err(read_failed)?;
    let files = git::list_folder(&repo.path, head, folder)
        .await
        .map_err(read_failed)?;
    let base = permalink_base(&github.owner, &github.name, head, folder);
    let explained = explainer.as_deref().map(no_mentions);
    Ok(PullRequestText {
        body: pull_request_body(&no_mentions(&issue_md), explained.as_deref(), &files, &base),
        not_done: explainer.as_deref().and_then(not_done_section),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_folder_submit_makes_is_an_issue_folder() {
        for good in ["docs/issues/2026-11-26-fix-the-zoom", "docs/issues/a_b.c"] {
            assert!(is_issue_folder(good), "{good}");
        }
        for bad in [
            "",
            "docs",
            "docs/issues",
            "docs/issues/",
            "docs/issues/.x",
            "docs/issues/a/b",
            "docs/issues/a b",
            "docs/other/x",
            "/docs/issues/x",
            "docs/issues/-x",
        ] {
            assert!(!is_issue_folder(bad), "{bad:?}");
        }
    }

    #[test]
    fn lasting_refusals_have_codes_and_sentences_of_their_own() {
        let all = [
            IdeaBlocked::AgentGone,
            IdeaBlocked::BranchGone,
            IdeaBlocked::NotSubmits,
            IdeaBlocked::Incomplete,
        ];
        let codes: Vec<_> = all.iter().map(|b| b.code()).collect();
        assert_eq!(
            codes,
            [
                "idea_agent_missing",
                "idea_branch_missing",
                "idea_origin_invalid",
                "idea_incomplete"
            ]
        );
        for blocked in all {
            let e = FeedbackError::IdeaBlocked(blocked);
            assert_eq!(e.code(), blocked.code());
            assert_eq!(e.to_string(), blocked.sentence());
            assert_eq!(e.status(), axum::http::StatusCode::CONFLICT);
        }
        assert_eq!(
            IdeaBlocked::AgentGone.sentence(),
            "the idea intent's agent no longer exists"
        );
    }

    #[test]
    fn a_seed_is_cut_on_a_character_boundary() {
        let text = "aé".repeat(10);
        assert_eq!(cut_to_bytes(text.clone(), 4), "aéa");
        assert_eq!(cut_to_bytes(text.clone(), 1000), text);
    }
}

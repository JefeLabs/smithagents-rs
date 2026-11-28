//! Agent service: repos, agent lifecycle and turn orchestration.
//!
//! Each agent has at most one operation in flight that excludes others (a turn
//! or a close), tracked in a reservation map behind a `std::sync::Mutex` that is
//! never held across an `await`. A turn runs in its own task; its reservation is
//! released by a drop guard, so neither a panic nor an early return can leave
//! the agent unable to take another message. A turn of an agent that works in
//! place in a plain folder claims that folder in the same map, and the same
//! guard frees it: one such turn runs in a folder at a time.

use std::any::Any;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::http::StatusCode;
use chrono::Utc;
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use smithagents_core::agent::{
    next_agent_name, next_color, AddSourceControl, Agent, AgentState, ChatMessage, Repo, Role,
    SourceControl, PLANETS,
};
use smithagents_core::event::EventKind;
use smithagents_core::ids::{AgentId, IntentId, RepoId};
use smithagents_core::intent::{IntentFilter, IntentState};
use smithagents_core::palette::AgentColor;

use crate::events::EventBus;
use crate::git::{self, GitError};
use crate::harness::{Harness, HarnessEvent, TurnRequest};
use crate::paths::Paths;
use crate::store::{Store, StoreError};

/// Prefix of the assistant message recorded when a turn fails.
const FAILURE_PREFIX: &str = "Agent process failed: ";
const STREAM_ENDED: &str = "stream ended before the turn completed";
/// After `TurnCompleted`, how long to let the harness process exit on its own
/// (so it can finish writing its session) before dropping, and so killing, it.
const EXIT_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("repo not found")]
    RepoNotFound,
    #[error("agent not found")]
    AgentNotFound,
    #[error("agent is closed")]
    AgentClosed,
    #[error("a turn is already in progress for this agent")]
    TurnInProgress,
    #[error("this repository has no source control")]
    NeedsSourceControl,
    /// A turn of another agent that works in the same plain folder is in
    /// flight: one agent works in a folder at a time.
    #[error("Another agent is working in this folder.")]
    FolderBusy,
    /// Source control cannot be switched while an agent of the repository
    /// is open.
    #[error("this repository has an agent that is not closed")]
    RepoInUse,
    /// A plain folder would contain, or lie inside, another repository's
    /// folder or the daemon's home.
    #[error("the folder overlaps another repository of this Workspace, or the daemon's home")]
    RepoOverlaps,
    /// Source control cannot be turned off while an intent Feedback made for
    /// the repository is not Resolved: it starts from a branch.
    #[error("this repository has feedback intents that are not resolved")]
    FeedbackIntentsOpen,
    #[error("git: {0}")]
    Git(#[from] GitError),
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

impl ServiceError {
    /// The HTTP status and the stable code this error is answered with.
    pub fn status_and_code(&self) -> (StatusCode, &'static str) {
        use StatusCode as S;
        match self {
            Self::RepoNotFound => (S::NOT_FOUND, "repo_not_found"),
            Self::AgentNotFound => (S::NOT_FOUND, "agent_not_found"),
            Self::AgentClosed => (S::CONFLICT, "agent_closed"),
            Self::TurnInProgress => (S::CONFLICT, "turn_in_progress"),
            Self::NeedsSourceControl => (S::CONFLICT, "needs_source_control"),
            Self::FolderBusy => (S::CONFLICT, "folder_busy"),
            Self::RepoInUse => (S::CONFLICT, "repo_in_use"),
            Self::RepoOverlaps => (S::CONFLICT, "repo_overlaps"),
            Self::FeedbackIntentsOpen => (S::CONFLICT, "repo_has_feedback_intents"),
            Self::Git(GitError::NotInstalled) => (S::BAD_REQUEST, "git_not_installed"),
            Self::Git(GitError::IdentityMissing) => (S::BAD_REQUEST, "git_identity_missing"),
            Self::Git(GitError::FolderNotEmpty(_)) => (S::CONFLICT, "folder_not_empty"),
            Self::Git(GitError::NotRepoRoot) => (S::BAD_REQUEST, "not_repo_root"),
            Self::Git(GitError::PathNotFound(_)) => (S::BAD_REQUEST, "path_not_found"),
            Self::Git(GitError::NotAGitRepo) => (S::BAD_REQUEST, "not_a_git_repo"),
            Self::Git(GitError::BareRepo) => (S::BAD_REQUEST, "bare_repo"),
            Self::Git(GitError::NoDefaultBranch) => (S::BAD_REQUEST, "no_default_branch"),
            Self::Git(GitError::BranchExists(_)) => (S::CONFLICT, "branch_exists"),
            Self::Git(GitError::WorktreeExists(_)) => (S::CONFLICT, "worktree_exists"),
            Self::Git(_) => (S::INTERNAL_SERVER_ERROR, "git_error"),
            Self::Store(StoreError::DuplicateRepoPath) => (S::CONFLICT, "repo_exists"),
            Self::Store(_) => (S::INTERNAL_SERVER_ERROR, "store_error"),
        }
    }
}

/// Takes back what creating `agent` made in the user's repository, for an
/// agent whose row could not be saved: its worktree, then its branch, while
/// that holds no commit `base` does not. Left alone, the branch would take
/// the name the next start wants. Best effort: what cannot be removed stays,
/// and is logged.
async fn discard_unsaved(repo_root: &Path, agent: &Agent, base: &str) {
    let id = agent.id;
    // An agent that works in place made nothing: its folder is the repository's.
    let Some(branch) = agent.branch.as_deref() else {
        return;
    };
    if let Err(e) = git::remove_worktree(repo_root, &agent.worktree_path).await {
        // The branch is still checked out there, so it stays too.
        tracing::warn!(agent_id = %id, "could not remove worktree of unsaved agent: {e}");
        return;
    }
    match git::delete_branch_made_from(repo_root, branch, base).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(agent_id = %id, "the branch of an unsaved agent holds a commit of its own, and was kept");
        }
        Err(e) => {
            tracing::warn!(agent_id = %id, "could not delete the branch of an unsaved agent: {e}");
        }
    }
}

/// Optional settings for `create_agent_with`.
#[derive(Debug, Default, Clone)]
pub struct CreateAgentOptions {
    pub branch: Option<String>,
    pub intent_id: Option<IntentId>,
    pub system_prompt_append: Option<String>,
    /// The local branch to start from (read as `refs/heads/<base>`, never
    /// fetched); `None`: the repository's default branch. The caller checks
    /// the name first.
    pub base: Option<String>,
}

/// What an agent is reserved for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Turn,
    Closing,
}

struct Inner {
    store: Store,
    bus: EventBus,
    harness: Arc<dyn Harness>,
    paths: Paths,
    quiet_after: Duration,
    /// Serializes `create_agent` so concurrent creates never pick the same name or color.
    create_lock: tokio::sync::Mutex<()>,
    /// Serializes adding repositories and switching their source control,
    /// so two cannot both pass the overlap check.
    repos_lock: tokio::sync::Mutex<()>,
    reserved: Mutex<Reserved>,
}

impl Inner {
    fn reserved(&self) -> MutexGuard<'_, Reserved> {
        // Map and set operations cannot leave it inconsistent, so a poisoned lock is still usable.
        self.reserved.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What is claimed now: agents, and the plain folders their turns work in.
#[derive(Default)]
struct Reserved {
    /// What each agent is reserved for.
    agents: HashMap<AgentId, Slot>,
    /// The plain repositories with a turn of one of their in-place agents in
    /// flight: one agent works in a folder at a time.
    folders: HashSet<RepoId>,
}

impl Reserved {
    fn release(&mut self, id: AgentId, folder: Option<RepoId>) {
        self.agents.remove(&id);
        if let Some(repo) = folder {
            self.folders.remove(&repo);
        }
    }
}

/// Exclusive claim on one agent, and for a turn of an agent that works in
/// place on its folder too; both are released when it is dropped.
struct Reservation {
    inner: Arc<Inner>,
    id: AgentId,
    /// The plain repository whose folder this claim holds.
    folder: Option<RepoId>,
    armed: bool,
}

impl Reservation {
    /// Claims the folder of plain repository `repo` as well; `FolderBusy`
    /// while another agent's turn holds it.
    fn claim_folder(&mut self, repo: RepoId) -> Result<(), ServiceError> {
        if !self.inner.reserved().folders.insert(repo) {
            return Err(ServiceError::FolderBusy);
        }
        self.folder = Some(repo);
        Ok(())
    }

    /// Releases the reservation and publishes `kind` under the same lock, so a
    /// new reservation of this agent or its folder (and everything it
    /// publishes) comes after `kind`.
    fn release_with(mut self, kind: EventKind) {
        self.armed = false;
        let mut reserved = self.inner.reserved();
        reserved.release(self.id, self.folder);
        self.inner.bus.publish(kind);
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.armed {
            self.inner.reserved().release(self.id, self.folder);
        }
    }
}

/// How a turn ended.
enum TurnEnd {
    Done,
    Dead,
}

/// Assistant text of the running turn that is not saved yet.
///
/// Text is saved as its own message whenever a tool call follows it, so the
/// transcript keeps the order in which things happened. Separate text blocks
/// within one saved message are joined by a blank line.
#[derive(Default)]
struct Reply {
    pending: String,
    /// A text block started after the pending text.
    new_block: bool,
    /// Some assistant text of this turn has been taken for saving.
    taken_any: bool,
}

impl Reply {
    fn start_block(&mut self) {
        self.new_block = !self.pending.is_empty();
    }

    fn push(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if std::mem::take(&mut self.new_block) {
            self.pending.push_str("\n\n");
        }
        self.pending.push_str(text);
    }

    /// The pending text, if there is any worth saving; empties the buffer.
    fn take(&mut self) -> Option<String> {
        self.new_block = false;
        let text = std::mem::take(&mut self.pending);
        if text.trim().is_empty() {
            return None;
        }
        self.taken_any = true;
        Some(text)
    }
}

#[derive(Clone)]
pub struct AgentService {
    inner: Arc<Inner>,
}

impl AgentService {
    pub fn new(
        store: Store,
        bus: EventBus,
        harness: Arc<dyn Harness>,
        paths: Paths,
        quiet_after: Duration,
    ) -> Self {
        AgentService {
            inner: Arc::new(Inner {
                store,
                bus,
                harness,
                paths,
                quiet_after,
                create_lock: tokio::sync::Mutex::new(()),
                repos_lock: tokio::sync::Mutex::new(()),
                reserved: Mutex::default(),
            }),
        }
    }

    /// Registers the folder at `path` as a repository, as `choice` says:
    ///
    /// - `None`: the folder as it is, at its [`canonical`] path (no
    ///   repository root is looked for above it), with no source control.
    ///   No git runs.
    /// - `Git`: the git repository the folder is in, at its root, with its
    ///   default branch.
    /// - `NewGit`: a new git repository made at the canonical path
    ///   ([`git::init_repo`]).
    ///
    /// Every check and what is made use that one canonical path. A folder
    /// that is already a repository is `repo_exists`, however it is spelled.
    /// A plain folder may not contain or lie inside another repository's
    /// folder, nor the daemon's home (`RepoOverlaps`); two git repositories
    /// may nest, as before. The git choices ask first whether git can be run
    /// at all.
    pub async fn add_repo(
        &self,
        path: &Path,
        choice: AddSourceControl,
    ) -> Result<Repo, ServiceError> {
        let _serialized = self.inner.repos_lock.lock().await;
        let path = canonical(path).await;
        let (path, source_control) = match choice {
            AddSourceControl::None => {
                require_folder(&path).await?;
                self.refuse_overlap(&path, true, None).await?;
                (path, None)
            }
            AddSourceControl::Git => {
                require_git().await?;
                let info = git::validate_repo(&path).await?;
                self.refuse_overlap(&info.root, false, None).await?;
                (info.root, Some(git_of(info.default_branch)))
            }
            AddSourceControl::NewGit => {
                require_git().await?;
                // Before anything is made.
                self.refuse_overlap(&path, false, None).await?;
                let info = git::init_repo(&path).await?;
                (info.root, Some(git_of(info.default_branch)))
            }
        };
        let repo = Repo {
            id: RepoId::new(),
            path,
            source_control,
            created_at: Utc::now(),
        };
        self.inner.store.insert_repo(&repo).await?;
        Ok(repo)
    }

    /// Turns source control on (`on`) or off for repository `id`, and
    /// returns the repository as it then is; asking for what it already has
    /// changes nothing.
    ///
    /// Refused while an agent of the repository is not closed (`RepoInUse`).
    /// Turning on needs the folder to be the root of a git repository
    /// (`NotRepoRoot` for one inside it). Turning off is refused when the
    /// folder would then overlap another repository or the daemon's home, as
    /// a plain folder may not, and while an intent Feedback made for it is
    /// not Resolved (`FeedbackIntentsOpen`): such an intent starts from a
    /// branch. Turning it off for the META repository is allowed, and nothing
    /// else changes: a stored META choice that names a plain repository
    /// counts as none. The worktrees and branches of closed agents stay.
    pub async fn set_source_control(&self, id: RepoId, on: bool) -> Result<Repo, ServiceError> {
        let _serialized = self.inner.repos_lock.lock().await;
        // No agent can be made between the check below and the change.
        let _no_new_agent = self.inner.create_lock.lock().await;
        let store = &self.inner.store;
        let repo = store
            .get_repo(id)
            .await?
            .ok_or(ServiceError::RepoNotFound)?;
        let has_source_control = !repo.is_plain();
        if has_source_control == on {
            return Ok(repo);
        }
        let open = store.list_agents(false).await?;
        if open.iter().any(|agent| agent.repo_id == id) {
            return Err(ServiceError::RepoInUse);
        }
        let source_control = if on {
            require_git().await?;
            let info = git::validate_repo(&repo.path).await?;
            if canonical(&info.root).await != canonical(&repo.path).await {
                return Err(GitError::NotRepoRoot.into());
            }
            Some(git_of(info.default_branch))
        } else {
            self.refuse_overlap(&repo.path, true, Some(id)).await?;
            let intents = store
                .list_intents(IntentFilter {
                    state: None,
                    repo_id: Some(id),
                })
                .await?;
            let feedback_open = intents
                .iter()
                .any(|intent| intent.origin.is_some() && intent.state != IntentState::Resolved);
            if feedback_open {
                return Err(ServiceError::FeedbackIntentsOpen);
            }
            None
        };
        store
            .set_repo_source_control(id, source_control.as_ref())
            .await?;
        Ok(Repo {
            source_control,
            ..repo
        })
    }

    /// Refuses a repository at `path` (`plain`: with no source control) that
    /// would overlap another one, `except` aside (the repository itself, when
    /// it is switched). The same folder is `repo_exists`, as it always was.
    /// A folder inside or around another is `RepoOverlaps` when either of
    /// the two is plain, and a plain folder inside or around the daemon's
    /// home is too. Paths are compared as [`canonical`] spells them.
    async fn refuse_overlap(
        &self,
        path: &Path,
        plain: bool,
        except: Option<RepoId>,
    ) -> Result<(), ServiceError> {
        let path = canonical(path).await;
        for other in self.inner.store.list_repos().await? {
            if Some(other.id) == except {
                continue;
            }
            let at = canonical(&other.path).await;
            if at == path {
                return Err(StoreError::DuplicateRepoPath.into());
            }
            if (plain || other.is_plain()) && overlaps(&path, &at) {
                return Err(ServiceError::RepoOverlaps);
            }
        }
        if plain && overlaps(&path, &canonical(&self.inner.paths.home).await) {
            return Err(ServiceError::RepoOverlaps);
        }
        Ok(())
    }

    /// Creates an idle agent with the first free planet name and palette color,
    /// in a new worktree on its own branch off the repo's default branch. On a
    /// repository with no source control the agent works in the folder
    /// itself, with no branch, and no git runs.
    pub async fn create_agent(&self, repo_id: RepoId) -> Result<Agent, ServiceError> {
        self.create_agent_with(repo_id, CreateAgentOptions::default())
            .await
    }

    /// Like `create_agent`, with an explicit branch (which must not exist yet),
    /// a branch to start from, an owning intent and a system prompt append
    /// saved with the agent row. A branch or a base on a repository with no
    /// source control is `NeedsSourceControl`; a plain folder that is gone is
    /// `PathNotFound`.
    pub async fn create_agent_with(
        &self,
        repo_id: RepoId,
        opts: CreateAgentOptions,
    ) -> Result<Agent, ServiceError> {
        // Held from name/color selection through the insert.
        let _serialized = self.inner.create_lock.lock().await;
        let store = &self.inner.store;
        let repo = store
            .get_repo(repo_id)
            .await?
            .ok_or(ServiceError::RepoNotFound)?;
        let open = store.list_agents(false).await?;
        let names: HashSet<String> = open.iter().map(|a| a.name.clone()).collect();
        let colors: Vec<AgentColor> = open.iter().map(|a| a.color).collect();
        let name = next_agent_name(&PLANETS, &names);
        let color = next_color(&colors);
        let id = AgentId::new();
        // Where the agent works, its branch, and the branch that one was made
        // from.
        let (worktree_path, branch, base) = match repo.default_branch() {
            // A plain repository: the agent works in the folder itself, and
            // no git runs.
            None => {
                if opts.branch.is_some() || opts.base.is_some() {
                    return Err(ServiceError::NeedsSourceControl);
                }
                require_folder(&repo.path).await?;
                (repo.path.clone(), None, None)
            }
            Some(default_branch) => {
                let branch = opts.branch.unwrap_or_else(|| agent_branch(&name, id));
                let worktrees = self.inner.paths.repo_worktrees_dir(&repo);
                // Fully qualified, so a tag or remote-tracking ref of the same
                // name is never taken instead.
                let base = match &opts.base {
                    Some(base) => format!("refs/heads/{base}"),
                    None => default_branch.to_owned(),
                };
                let worktree_path =
                    git::create_worktree(&repo.path, &worktrees, &branch, &base).await?;
                (worktree_path, Some(branch), Some(base))
            }
        };
        let agent = Agent {
            id,
            name,
            color,
            repo_id,
            worktree_path,
            branch,
            harness_session_id: None,
            state: AgentState::Idle,
            created_at: Utc::now(),
            closed_at: None,
            intent_id: opts.intent_id,
        };
        let inserted = store
            .insert_agent_with_append(&agent, opts.system_prompt_append.as_deref())
            .await;
        if let Err(e) = inserted {
            if let Some(base) = &base {
                discard_unsaved(&repo.path, &agent, base).await;
            }
            return Err(e.into());
        }
        self.inner
            .bus
            .publish(EventKind::AgentCreated(agent.clone()));
        Ok(agent)
    }

    /// Marks the agent closed, freeing its name and color; the worktree is kept.
    /// Closing while a turn runs is not supported and returns `TurnInProgress`.
    pub async fn close_agent(&self, id: AgentId) -> Result<(), ServiceError> {
        let _reservation = self.reserve(id, Slot::Closing)?;
        self.open_agent(id).await?;
        self.inner.store.close_agent(id, Utc::now()).await?;
        self.inner
            .bus
            .publish(EventKind::AgentClosed { agent_id: id });
        Ok(())
    }

    /// Records the user's message and starts a turn, returning the message.
    /// Agents in any open state may be messaged (a `Dead` agent resumes its
    /// session); only one turn per agent runs at a time, and only one among
    /// the agents that work in place in one plain folder: while another's
    /// runs, the message is refused with `FolderBusy` and not kept.
    pub async fn send_message(
        &self,
        id: AgentId,
        text: String,
    ) -> Result<ChatMessage, ServiceError> {
        let mut reservation = self.reserve(id, Slot::Turn)?;
        let agent = self.open_agent(id).await?;
        // An agent made while its repository had source control keeps its
        // own worktree, so only one that works in place takes the folder.
        if agent.works_in_place() {
            reservation.claim_folder(agent.repo_id)?;
        }
        let message = self.append(id, Role::User, &text).await?;
        // Nothing below awaits before the spawn, so a caller that stops waiting
        // cannot leave a recorded message without a turn. The task sets `Working`.
        let svc = self.clone();
        tokio::spawn(async move { svc.run_turn(id, text, reservation).await });
        Ok(message)
    }

    /// Number of agents with a turn in flight (state `Working` or `Quiet`).
    pub fn running_agent_count(&self) -> usize {
        self.inner
            .reserved()
            .agents
            .values()
            .filter(|slot| **slot == Slot::Turn)
            .count()
    }

    /// Whether a turn is in flight for agent `id`: from the moment
    /// `send_message` accepts the message until the turn's final state is
    /// recorded. That is state `Working` or `Quiet`, plus the instant before
    /// the turn's task records `Working`.
    pub fn turn_in_flight(&self, id: AgentId) -> bool {
        self.inner.reserved().agents.get(&id) == Some(&Slot::Turn)
    }

    /// A turn is in flight among the in-place agents of `repo`: from the
    /// moment `send_message` accepts its message until its final state is
    /// recorded, as [`Self::turn_in_flight`] says of the agent.
    pub fn folder_busy(&self, repo: RepoId) -> bool {
        self.inner.reserved().folders.contains(&repo)
    }

    /// Atomically claims `id` for `slot`, failing if it is already claimed.
    fn reserve(&self, id: AgentId, slot: Slot) -> Result<Reservation, ServiceError> {
        match self.inner.reserved().agents.entry(id) {
            Entry::Occupied(held) => Err(match held.get() {
                Slot::Turn => ServiceError::TurnInProgress,
                Slot::Closing => ServiceError::AgentClosed,
            }),
            Entry::Vacant(free) => {
                free.insert(slot);
                Ok(Reservation {
                    inner: Arc::clone(&self.inner),
                    id,
                    folder: None,
                    armed: true,
                })
            }
        }
    }

    /// Loads an agent that exists and is not closed.
    async fn open_agent(&self, id: AgentId) -> Result<Agent, ServiceError> {
        let agent = self
            .inner
            .store
            .get_agent(id)
            .await?
            .ok_or(ServiceError::AgentNotFound)?;
        if agent.closed_at.is_some() {
            return Err(ServiceError::AgentClosed);
        }
        Ok(agent)
    }

    async fn append(
        &self,
        id: AgentId,
        role: Role,
        text: &str,
    ) -> Result<ChatMessage, ServiceError> {
        let message = self.inner.store.append_message(id, role, text).await?;
        self.inner
            .bus
            .publish(EventKind::MessageAppended(message.clone()));
        Ok(message)
    }

    /// Persists `state` and returns the updated agent, without publishing.
    async fn persist_state(&self, id: AgentId, state: AgentState) -> Result<Agent, ServiceError> {
        self.inner.store.update_agent_state(id, state).await?;
        self.inner
            .store
            .get_agent(id)
            .await?
            .ok_or(ServiceError::AgentNotFound)
    }

    async fn set_state(&self, id: AgentId, state: AgentState) -> Result<(), ServiceError> {
        let agent = self.persist_state(id, state).await?;
        self.inner.bus.publish(EventKind::AgentUpdated(agent));
        Ok(())
    }

    /// Owns the turn from start to final state. Store errors and panics inside
    /// the turn are recorded as a failure, so the agent always ends `Done` or `Dead`.
    async fn run_turn(self, id: AgentId, prompt: String, reservation: Reservation) {
        let end = match AssertUnwindSafe(self.drive_turn(id, prompt))
            .catch_unwind()
            .await
        {
            Ok(Ok(end)) => end,
            Ok(Err(e)) => self.abort_turn(id, &format!("turn aborted: {e}")).await,
            Err(panic) => {
                let reason = format!("turn aborted: {}", panic_message(&*panic));
                self.abort_turn(id, &reason).await
            }
        };
        let state = match end {
            TurnEnd::Done => AgentState::Done,
            TurnEnd::Dead => AgentState::Dead,
        };
        match self.persist_state(id, state).await {
            Ok(agent) => reservation.release_with(EventKind::AgentUpdated(agent)),
            // The reservation is still released when it drops here.
            Err(e) => tracing::error!(agent_id = %id, "could not record the end of a turn: {e}"),
        }
    }

    async fn drive_turn(&self, id: AgentId, prompt: String) -> Result<TurnEnd, ServiceError> {
        self.set_state(id, AgentState::Working).await?;
        // Read fresh: the previous turn may have recorded a new session id.
        let agent = self
            .inner
            .store
            .get_agent(id)
            .await?
            .ok_or(ServiceError::AgentNotFound)?;
        let mut session = agent.harness_session_id.clone();
        let request = TurnRequest {
            prompt,
            cwd: agent.worktree_path,
            resume_session_id: agent.harness_session_id,
            model: None,
            system_prompt_append: self.inner.store.agent_system_prompt_append(id).await?,
        };
        let mut stream = match self.inner.harness.run_turn(request).await {
            Ok(stream) => stream,
            Err(e) => return self.record_failure(id, None, &e.to_string()).await,
        };
        let mut reply = Reply::default();
        let mut quiet = false;
        loop {
            let next = if quiet {
                stream.next().await
            } else {
                match tokio::time::timeout(self.inner.quiet_after, stream.next()).await {
                    Ok(next) => next,
                    Err(_silent) => {
                        quiet = true;
                        self.set_state(id, AgentState::Quiet).await?;
                        continue;
                    }
                }
            };
            let Some(event) = next else {
                return self.record_failure(id, reply.take(), STREAM_ENDED).await;
            };
            if quiet {
                quiet = false;
                self.set_state(id, AgentState::Working).await?;
            }
            match event {
                HarnessEvent::SessionStarted { session_id } => {
                    self.save_session(id, &mut session, session_id).await?;
                }
                HarnessEvent::TextBlockStarted => reply.start_block(),
                HarnessEvent::AssistantText { text } => {
                    reply.push(&text);
                    self.inner
                        .bus
                        .publish(EventKind::TurnDelta { agent_id: id, text });
                }
                HarnessEvent::ToolUse { name, summary } => {
                    // The text so far came before this call.
                    self.save_reply(id, &mut reply).await?;
                    let text = if summary.is_empty() {
                        name
                    } else {
                        format!("{name}: {summary}")
                    };
                    self.append(id, Role::Tool, &text).await?;
                }
                HarnessEvent::TurnCompleted {
                    session_id,
                    is_error,
                    result_text,
                } => {
                    self.save_session(id, &mut session, session_id).await?;
                    if is_error {
                        let reason = result_text
                            .filter(|t| !t.is_empty())
                            .unwrap_or_else(|| "the turn ended with an error".into());
                        let end = self.record_failure(id, reply.take(), &reason).await?;
                        wait_for_exit(id, stream).await;
                        return Ok(end);
                    }
                    self.save_reply(id, &mut reply).await?;
                    // The result repeats the final text, which normally
                    // streamed already; it is only used when nothing did.
                    if !reply.taken_any {
                        if let Some(text) = result_text.filter(|t| !t.trim().is_empty()) {
                            self.append(id, Role::Assistant, &text).await?;
                        }
                    }
                    // The reply is complete, but a process that then fails still failed.
                    return match wait_for_exit(id, stream).await {
                        Some(failure) => self.record_failure(id, None, &failure).await,
                        None => Ok(TurnEnd::Done),
                    };
                }
                HarnessEvent::Failed { message } => {
                    return self.record_failure(id, reply.take(), &message).await;
                }
            }
        }
    }

    async fn save_session(
        &self,
        id: AgentId,
        current: &mut Option<String>,
        session_id: String,
    ) -> Result<(), ServiceError> {
        if session_id.is_empty() || current.as_deref() == Some(session_id.as_str()) {
            return Ok(());
        }
        self.inner
            .store
            .set_harness_session(id, &session_id)
            .await?;
        *current = Some(session_id);
        Ok(())
    }

    /// Saves the pending assistant text, if any, as its own message.
    async fn save_reply(&self, id: AgentId, reply: &mut Reply) -> Result<(), ServiceError> {
        if let Some(text) = reply.take() {
            self.append(id, Role::Assistant, &text).await?;
        }
        Ok(())
    }

    /// Keeps the text streamed but not yet saved (`partial`), then records the
    /// failure. The session id is kept.
    async fn record_failure(
        &self,
        id: AgentId,
        partial: Option<String>,
        reason: &str,
    ) -> Result<TurnEnd, ServiceError> {
        if let Some(partial) = partial {
            self.append(id, Role::Assistant, &partial).await?;
        }
        self.append(id, Role::Assistant, &format!("{FAILURE_PREFIX}{reason}"))
            .await?;
        Ok(TurnEnd::Dead)
    }

    /// Best-effort failure record for a turn that could not finish normally.
    async fn abort_turn(&self, id: AgentId, reason: &str) -> TurnEnd {
        tracing::error!(agent_id = %id, "{reason}");
        if let Err(e) = self.record_failure(id, None, reason).await {
            tracing::error!(agent_id = %id, "could not record turn failure: {e}");
        }
        TurnEnd::Dead
    }
}

/// Drains the rest of a completed turn's stream, bounded by [`EXIT_GRACE`], and
/// returns the first failure reported (such as a non-zero exit).
async fn wait_for_exit(
    id: AgentId,
    mut stream: BoxStream<'static, HarnessEvent>,
) -> Option<String> {
    let drained = tokio::time::timeout(EXIT_GRACE, async {
        let mut failure = None;
        while let Some(event) = stream.next().await {
            if let HarnessEvent::Failed { message } = event {
                failure.get_or_insert(message);
            }
        }
        failure
    })
    .await;
    drained.unwrap_or_else(|_| {
        tracing::warn!(agent_id = %id, "harness still running {EXIT_GRACE:?} after the turn; stopping it");
        None
    })
}

fn git_of(default_branch: String) -> SourceControl {
    SourceControl::Git { default_branch }
}

/// Refuses a git choice before anything is made when git cannot be run.
/// This is the only place git is looked for: asked each time, so git
/// installed meanwhile is noticed, and the answer is what `GET /v1/health`
/// reports from then on.
async fn require_git() -> Result<(), ServiceError> {
    if git::is_installed().await {
        Ok(())
    } else {
        Err(GitError::NotInstalled.into())
    }
}

/// `PathNotFound` unless `path` is a folder, for a repository with no source
/// control.
async fn require_folder(path: &Path) -> Result<(), ServiceError> {
    match tokio::fs::metadata(path).await {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(GitError::PathNotFound(path.to_owned()).into()),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Err(GitError::PathNotFound(path.to_owned()).into())
        }
        Err(e) => Err(GitError::Io(e).into()),
    }
}

/// `path` made absolute with links, `.` and `..` resolved (on macOS also in
/// the letter case the disk has). The longest leading part the system can
/// resolve, it resolves. The rest is resolved as written, a `..` there
/// dropping the part before it: what does not exist cannot be a link. Two
/// spellings of one folder come out the same, also where some is missing,
/// and what is checked is what gets made.
async fn canonical(path: &Path) -> PathBuf {
    let Ok(path) = std::path::absolute(path) else {
        return path.to_owned();
    };
    let parts: Vec<Component<'_>> = path.components().collect();
    let (mut at, mut rest) = (PathBuf::new(), &parts[..]);
    for end in (1..=parts.len()).rev() {
        let leading: PathBuf = parts[..end].iter().collect();
        if let Ok(real) = tokio::fs::canonicalize(&leading).await {
            (at, rest) = (real, &parts[end..]);
            break;
        }
    }
    for part in rest {
        match part {
            Component::ParentDir => {
                at.pop();
            }
            Component::CurDir => {}
            part => at.push(part),
        }
    }
    at
}

/// Whether one of two folders is, or lies inside, the other.
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// `agent/<lowercase name, spaces as dashes>-<first 8 hex digits of the id>`.
fn agent_branch(name: &str, id: AgentId) -> String {
    let slug = name.to_lowercase().replace(' ', "-");
    let hex = id.0.simple().to_string();
    format!("agent/{slug}-{}", &hex[..8])
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        format!("panic: {s}")
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("panic: {s}")
    } else {
        "panic".into()
    }
}

#[cfg(all(test, feature = "test-util"))]
mod tests {
    use std::path::PathBuf;

    use async_trait::async_trait;
    use futures::stream::{self, BoxStream, StreamExt};
    use smithagents_core::agent::{AddSourceControl, AgentState, Role};
    use smithagents_core::event::{Event, EventKind};
    use smithagents_core::feedback::FeedbackOrigin;
    use smithagents_core::intent::{
        Content, Intent, IntentKind, IntentState, IntentVersion, Phase, Seed, SeedSource,
        VersionAuthor,
    };
    use smithagents_core::palette::palette;
    use tempfile::TempDir;
    use tokio::sync::broadcast;

    use super::*;
    use crate::harness::fake::FakeHarness;
    use crate::harness::{
        HarnessError, HarnessEvent, StructuredRequest, StructuredResult, TurnRequest,
    };

    /// A `quiet_after` long enough that no test without a delay ever goes quiet.
    const NEVER_QUIET: Duration = Duration::from_secs(60);
    const TURN_TIMEOUT: Duration = Duration::from_secs(10);

    struct Fx {
        svc: AgentService,
        store: Store,
        bus: EventBus,
        harness: FakeHarness,
        paths: Paths,
        repo: Repo,
        repo_dir: PathBuf,
        tmp: TempDir,
    }

    impl Fx {
        async fn messages(&self, id: AgentId) -> Vec<(Role, String)> {
            self.store
                .list_messages(id, 0, 1000)
                .await
                .unwrap()
                .into_iter()
                .map(|m| (m.role, m.text))
                .collect()
        }

        async fn agent(&self, id: AgentId) -> Agent {
            self.store.get_agent(id).await.unwrap().unwrap()
        }

        fn live(&self) -> broadcast::Receiver<Event> {
            self.bus.subscribe(None).unwrap().live
        }
    }

    /// Runs git with a fixed identity and locale, independent of user config.
    fn git(cwd: &Path, args: &[&str]) -> String {
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
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    async fn fixture_with(harness: FakeHarness, quiet_after: Duration) -> Fx {
        let tmp = TempDir::new().unwrap();
        let repo_dir = tmp.path().join("my-repo");
        std::fs::create_dir(&repo_dir).unwrap();
        git(&repo_dir, &["init", "-b", "main"]);
        git(&repo_dir, &["commit", "--allow-empty", "-m", "init"]);
        let store = Store::open_in_memory().await.unwrap();
        let bus = EventBus::new(1024);
        let paths = Paths {
            home: tmp.path().join("home"),
        };
        let svc = AgentService::new(
            store.clone(),
            bus.clone(),
            Arc::new(harness.clone()),
            paths.clone(),
            quiet_after,
        );
        let repo = svc
            .add_repo(&repo_dir, AddSourceControl::Git)
            .await
            .unwrap();
        Fx {
            svc,
            store,
            bus,
            harness,
            paths,
            repo,
            repo_dir,
            tmp,
        }
    }

    async fn fixture(turns: Vec<Vec<HarnessEvent>>) -> Fx {
        fixture_with(FakeHarness::scripted(turns), NEVER_QUIET).await
    }

    fn started(session_id: &str) -> HarnessEvent {
        HarnessEvent::SessionStarted {
            session_id: session_id.into(),
        }
    }

    fn text(t: &str) -> HarnessEvent {
        HarnessEvent::AssistantText { text: t.into() }
    }

    fn completed(session_id: &str, result: Option<&str>) -> HarnessEvent {
        HarnessEvent::TurnCompleted {
            session_id: session_id.into(),
            is_error: false,
            result_text: result.map(Into::into),
        }
    }

    fn failed(message: &str) -> HarnessEvent {
        HarnessEvent::Failed {
            message: message.into(),
        }
    }

    /// One readable line per event, so whole sequences can be compared.
    fn describe(kind: &EventKind) -> String {
        match kind {
            EventKind::AgentCreated(a) => format!("created {}", a.name),
            EventKind::AgentUpdated(a) => format!("state {:?}", a.state),
            EventKind::AgentClosed { .. } => "closed".into(),
            EventKind::MessageAppended(m) => format!("{:?}: {}", m.role, m.text),
            EventKind::TurnDelta { text, .. } => format!("delta {text}"),
            other => format!("{other:?}"),
        }
    }

    fn describe_all(kinds: &[EventKind]) -> Vec<String> {
        kinds.iter().map(describe).collect()
    }

    /// Receives events until `pred` matches one; returns everything seen, in order.
    async fn recv_until(
        rx: &mut broadcast::Receiver<Event>,
        mut pred: impl FnMut(&EventKind) -> bool,
    ) -> Vec<EventKind> {
        tokio::time::timeout(TURN_TIMEOUT, async {
            let mut seen = Vec::new();
            loop {
                let kind = rx.recv().await.expect("bus closed or lagged").kind;
                let done = pred(&kind);
                seen.push(kind);
                if done {
                    return seen;
                }
            }
        })
        .await
        .expect("expected event did not arrive in time")
    }

    /// Receives events until agent `id` reaches `Done` or `Dead`.
    async fn until_turn_ends(rx: &mut broadcast::Receiver<Event>, id: AgentId) -> Vec<EventKind> {
        recv_until(rx, |k| {
            matches!(k, EventKind::AgentUpdated(a)
                if a.id == id && matches!(a.state, AgentState::Done | AgentState::Dead))
        })
        .await
    }

    /// Receives events until every agent of `ids` has reached `Done` or
    /// `Dead`, in whatever order.
    async fn until_turns_end(rx: &mut broadcast::Receiver<Event>, ids: &[AgentId]) {
        let mut running: HashSet<AgentId> = ids.iter().copied().collect();
        recv_until(rx, |k| {
            if let EventKind::AgentUpdated(a) = k {
                if matches!(a.state, AgentState::Done | AgentState::Dead) {
                    running.remove(&a.id);
                }
            }
            running.is_empty()
        })
        .await;
    }

    fn is_state(kind: &EventKind, state: AgentState) -> bool {
        matches!(kind, EventKind::AgentUpdated(a) if a.state == state)
    }

    #[tokio::test]
    async fn create_agent_assigns_planet_names_in_order() {
        let fx = fixture(vec![]).await;
        let mut names = Vec::new();
        for _ in 0..3 {
            names.push(fx.svc.create_agent(fx.repo.id).await.unwrap().name);
        }
        assert_eq!(names, ["Mercury", "Venus", "Earth"]);
    }

    #[tokio::test]
    async fn create_agent_assigns_distinct_palette_colors() {
        let fx = fixture(vec![]).await;
        let mut colors = Vec::new();
        for _ in 0..3 {
            colors.push(fx.svc.create_agent(fx.repo.id).await.unwrap().color);
        }
        assert_eq!(colors, palette()[0..3]);
    }

    #[tokio::test]
    async fn closing_agent_frees_name_and_color() {
        let fx = fixture(vec![]).await;
        let mercury = fx.svc.create_agent(fx.repo.id).await.unwrap();
        fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();
        fx.svc.close_agent(mercury.id).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::AgentClosed {
                agent_id: mercury.id
            }
        );
        assert!(fx.agent(mercury.id).await.closed_at.is_some());
        // The worktree is kept.
        assert!(mercury.worktree_path.is_dir());

        let next = fx.svc.create_agent(fx.repo.id).await.unwrap();
        assert_eq!(next.name, "Mercury");
        assert_eq!(next.color, palette()[0]);
        assert_ne!(next.id, mercury.id);
        assert_ne!(next.branch, mercury.branch);
    }

    #[tokio::test]
    async fn create_agent_makes_worktree_on_agent_branch() {
        let fx = fixture(vec![]).await;
        let mut rx = fx.live();
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();

        let branch = agent.branch.as_deref().unwrap();
        let suffix = branch.strip_prefix("agent/mercury-").unwrap();
        assert_eq!(suffix, &agent.id.0.simple().to_string()[..8]);
        assert!(agent.worktree_path.is_dir());
        assert!(agent
            .worktree_path
            .starts_with(fx.paths.repo_worktrees_dir(&fx.repo)));
        assert_eq!(
            git(&agent.worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"]),
            branch
        );
        // The user's checkout is untouched.
        assert_eq!(
            git(&fx.repo_dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "main"
        );
        assert_eq!(agent.state, AgentState::Idle);
        assert_eq!(agent.harness_session_id, None);
        assert_eq!(fx.agent(agent.id).await, agent);
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::AgentCreated(agent)
        );
    }

    #[tokio::test]
    async fn create_agent_with_saves_branch_intent_and_append() {
        let fx = fixture(vec![]).await;
        let intent_id = IntentId::new();
        let agent = fx
            .svc
            .create_agent_with(
                fx.repo.id,
                CreateAgentOptions {
                    branch: Some(git::branch_name_for_intent("fix-it")),
                    intent_id: Some(intent_id),
                    system_prompt_append: Some("do the thing".into()),
                    base: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(agent.branch.as_deref(), Some("intent/fix-it"));
        assert_eq!(agent.intent_id, Some(intent_id));
        assert_eq!(fx.agent(agent.id).await, agent);
        assert_eq!(
            git(&agent.worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "intent/fix-it"
        );
        assert_eq!(
            fx.store
                .agent_system_prompt_append(agent.id)
                .await
                .unwrap()
                .as_deref(),
            Some("do the thing")
        );
    }

    #[tokio::test]
    async fn every_turn_passes_the_saved_system_prompt_append() {
        let fx = fixture(vec![
            vec![started("s1"), completed("s1", Some("one"))],
            vec![completed("s1", Some("two"))],
        ])
        .await;
        let agent = fx
            .svc
            .create_agent_with(
                fx.repo.id,
                CreateAgentOptions {
                    system_prompt_append: Some("rules".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut rx = fx.live();
        fx.svc.send_message(agent.id, "a".into()).await.unwrap();
        until_turn_ends(&mut rx, agent.id).await;
        fx.svc.send_message(agent.id, "b".into()).await.unwrap();
        until_turn_ends(&mut rx, agent.id).await;
        let reqs = fx.harness.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].system_prompt_append.as_deref(), Some("rules"));
        assert_eq!(reqs[1].system_prompt_append.as_deref(), Some("rules"));
        assert_eq!(reqs[1].resume_session_id.as_deref(), Some("s1"));
    }

    #[tokio::test]
    async fn existing_branch_is_branch_exists_and_leaves_nothing_behind() {
        let fx = fixture(vec![]).await;
        git(&fx.repo_dir, &["branch", "intent/taken"]);
        let err = fx
            .svc
            .create_agent_with(
                fx.repo.id,
                CreateAgentOptions {
                    branch: Some("intent/taken".into()),
                    intent_id: Some(IntentId::new()),
                    system_prompt_append: Some("x".into()),
                    base: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::Git(GitError::BranchExists(b)) if b == "intent/taken"));
        assert!(fx.store.list_agents(true).await.unwrap().is_empty());
        assert!(!fx
            .paths
            .repo_worktrees_dir(&fx.repo)
            .join("intent-taken")
            .exists());
    }

    #[tokio::test]
    async fn a_plain_folder_is_added_as_it_is() {
        let fx = fixture(vec![]).await;
        // `notes/` is not a repository.
        let notes_dir = fx.tmp.path().join("notes");
        std::fs::create_dir(&notes_dir).unwrap();
        let notes = fx
            .svc
            .add_repo(&notes_dir, AddSourceControl::None)
            .await
            .unwrap();
        assert_eq!(notes.source_control, None);
        assert_eq!(notes.path, std::fs::canonicalize(&notes_dir).unwrap());
        assert_eq!(fx.store.get_repo(notes.id).await.unwrap(), Some(notes));

        // A folder inside a git repository that is not one of the
        // Workspace's keeps its own path: no root is looked for above it.
        let other = fx.tmp.path().join("other-repo");
        std::fs::create_dir(&other).unwrap();
        git(&other, &["init", "-b", "main"]);
        let docs = other.join("docs");
        std::fs::create_dir(&docs).unwrap();
        let inside = fx
            .svc
            .add_repo(&docs, AddSourceControl::None)
            .await
            .unwrap();
        assert_eq!(
            (inside.path, inside.source_control),
            (std::fs::canonicalize(&docs).unwrap(), None)
        );

        let gone = fx
            .svc
            .add_repo(&fx.tmp.path().join("nowhere"), AddSourceControl::None)
            .await
            .unwrap_err();
        assert!(
            matches!(gone, ServiceError::Git(GitError::PathNotFound(_))),
            "{gone:?}"
        );
        let file = fx.tmp.path().join("a-file");
        std::fs::write(&file, "not a folder").unwrap();
        let err = fx
            .svc
            .add_repo(&file, AddSourceControl::None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ServiceError::Git(GitError::PathNotFound(_))),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_new_git_repository_needs_an_empty_folder() {
        let fx = fixture(vec![]).await;
        let full = fx.tmp.path().join("full");
        std::fs::create_dir(&full).unwrap();
        std::fs::write(full.join("notes.txt"), "keep me").unwrap();
        let err = fx
            .svc
            .add_repo(&full, AddSourceControl::NewGit)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ServiceError::Git(GitError::FolderNotEmpty(_))),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read_to_string(full.join("notes.txt")).unwrap(),
            "keep me"
        );
        assert!(!full.join(".git").exists());

        // The same folder reached through `..` after a folder that does not
        // exist: nothing is made, not even the missing folder.
        let missing = fx.tmp.path().join("missing");
        let err = fx
            .svc
            .add_repo(&missing.join("..").join("full"), AddSourceControl::NewGit)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ServiceError::Git(GitError::FolderNotEmpty(_))),
            "{err:?}"
        );
        assert!(!full.join(".git").exists());
        assert!(!missing.exists());
        assert_eq!(fx.store.list_repos().await.unwrap(), vec![fx.repo.clone()]);
    }

    #[tokio::test]
    async fn a_plain_folder_may_not_overlap_another_repository() {
        let fx = fixture(vec![]).await;
        // x/ is a plain repository; every folder below exists.
        let x = fx.tmp.path().join("x");
        std::fs::create_dir_all(x.join("sub")).unwrap();
        fx.svc.add_repo(&x, AddSourceControl::None).await.unwrap();
        std::fs::create_dir_all(fx.paths.home.join("worktrees")).unwrap();
        std::fs::create_dir_all(fx.repo_dir.join("docs")).unwrap();
        let with_slash = PathBuf::from(format!("{}/", x.display()));

        #[allow(unused_mut)]
        let missing = fx.tmp.path().join("missing");
        let mut refused = vec![
            x.join("sub"),
            x.parent().unwrap().to_owned(),
            x.join("sub").join(".."),
            missing.join("..").join("x"),
            with_slash,
            fx.paths.home.clone(),
            fx.paths.home.join("worktrees"),
            fx.repo_dir.join("docs"),
        ];
        #[cfg(unix)]
        {
            let link_to_x = fx.tmp.path().join("link-to-x");
            std::os::unix::fs::symlink(&x, &link_to_x).unwrap();
            refused.push(link_to_x);
        }
        for path in refused {
            let err = fx
                .svc
                .add_repo(&path, AddSourceControl::None)
                .await
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    ServiceError::RepoOverlaps | ServiceError::Store(StoreError::DuplicateRepoPath)
                ),
                "{path:?}: {err:?}"
            );
        }
        // Where the disk ignores letter case, the upper-cased path of x/
        // names the same folder, and that spelling is refused too.
        let upper = fx.tmp.path().join("X");
        if upper.is_dir() {
            let err = fx
                .svc
                .add_repo(&upper, AddSourceControl::None)
                .await
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    ServiceError::RepoOverlaps | ServiceError::Store(StoreError::DuplicateRepoPath)
                ),
                "{err:?}"
            );
        }

        // A git repository may not lie inside the plain folder either, and a
        // new one there is refused before anything is made.
        let inner = x.join("inner");
        std::fs::create_dir(&inner).unwrap();
        git(&inner, &["init", "-b", "main"]);
        git(&inner, &["commit", "--allow-empty", "-m", "init"]);
        let err = fx
            .svc
            .add_repo(&inner, AddSourceControl::Git)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::RepoOverlaps), "{err:?}");
        let fresh = x.join("fresh");
        let err = fx
            .svc
            .add_repo(&fresh, AddSourceControl::NewGit)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::RepoOverlaps), "{err:?}");
        assert!(!fresh.exists());
        // Nor through `..` after a folder that does not exist: git never
        // runs in the plain folder.
        let err = fx
            .svc
            .add_repo(&missing.join("..").join("x"), AddSourceControl::NewGit)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                ServiceError::RepoOverlaps | ServiceError::Store(StoreError::DuplicateRepoPath)
            ),
            "{err:?}"
        );
        assert!(!x.join(".git").exists());
        assert!(!missing.exists());

        // A git repository inside another git repository is still added, as
        // before; neither may then become a plain folder.
        let nested = fx.repo_dir.join("nested");
        std::fs::create_dir(&nested).unwrap();
        git(&nested, &["init", "-b", "main"]);
        git(&nested, &["commit", "--allow-empty", "-m", "init"]);
        let nested = fx
            .svc
            .add_repo(&nested, AddSourceControl::Git)
            .await
            .unwrap();
        assert_eq!(nested.default_branch(), Some("main"));
        for id in [nested.id, fx.repo.id] {
            let err = fx.svc.set_source_control(id, false).await.unwrap_err();
            assert!(matches!(err, ServiceError::RepoOverlaps), "{err:?}");
            let kept = fx.store.get_repo(id).await.unwrap().unwrap();
            assert_eq!(kept.default_branch(), Some("main"));
        }
    }

    #[tokio::test]
    async fn source_control_is_switched_only_with_no_open_agent() {
        let fx = fixture(vec![]).await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        assert!(matches!(
            fx.svc.set_source_control(fx.repo.id, false).await,
            Err(ServiceError::RepoInUse)
        ));
        fx.svc.close_agent(agent.id).await.unwrap();
        let off = fx.svc.set_source_control(fx.repo.id, false).await.unwrap();
        assert_eq!(off.source_control, None);
        assert_eq!(fx.store.get_repo(fx.repo.id).await.unwrap(), Some(off));

        // An open agent holds the plain repository too.
        let in_place = fx.svc.create_agent(fx.repo.id).await.unwrap();
        assert!(in_place.works_in_place());
        assert!(matches!(
            fx.svc.set_source_control(fx.repo.id, true).await,
            Err(ServiceError::RepoInUse)
        ));
        fx.svc.close_agent(in_place.id).await.unwrap();
        let on = fx.svc.set_source_control(fx.repo.id, true).await.unwrap();
        assert_eq!(on.default_branch(), Some("main"));
        assert_eq!(fx.store.get_repo(fx.repo.id).await.unwrap(), Some(on));

        // The closed agent's worktree folder and branch are still there.
        assert!(agent.worktree_path.is_dir());
        assert!(branches(&fx).contains(agent.branch.as_ref().unwrap()));

        let err = fx
            .svc
            .set_source_control(RepoId::new(), true)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::RepoNotFound), "{err:?}");
    }

    #[tokio::test]
    async fn a_folder_inside_a_git_repository_cannot_be_turned_into_one() {
        let fx = fixture(vec![]).await;
        // docs/ inside the fixture's repository, added as plain, is refused
        // by the overlap rule; so: a plain repository at other-repo/docs,
        // other-repo not added.
        let other = fx.tmp.path().join("other-repo");
        std::fs::create_dir(&other).unwrap();
        git(&other, &["init", "-b", "main"]);
        git(&other, &["commit", "--allow-empty", "-m", "init"]);
        let docs = other.join("docs");
        std::fs::create_dir(&docs).unwrap();
        let plain = fx
            .svc
            .add_repo(&docs, AddSourceControl::None)
            .await
            .unwrap();
        let err = fx.svc.set_source_control(plain.id, true).await.unwrap_err();
        assert!(
            matches!(err, ServiceError::Git(GitError::NotRepoRoot)),
            "{err:?}"
        );
        assert_eq!(fx.store.get_repo(plain.id).await.unwrap(), Some(plain));

        // A folder in no git repository at all is not one either.
        let loose = fx.tmp.path().join("loose");
        std::fs::create_dir(&loose).unwrap();
        let loose = fx
            .svc
            .add_repo(&loose, AddSourceControl::None)
            .await
            .unwrap();
        let err = fx.svc.set_source_control(loose.id, true).await.unwrap_err();
        assert!(
            matches!(err, ServiceError::Git(GitError::NotAGitRepo)),
            "{err:?}"
        );
    }

    /// An intent of `repo_id` in `state`, made by Feedback when `feedback`.
    fn intent_in(repo_id: RepoId, slug: &str, state: IntentState, feedback: bool) -> Intent {
        let now = Utc::now();
        Intent {
            id: IntentId::new(),
            repo_id,
            slug: slug.into(),
            state,
            phase: Phase::Review,
            seed: Seed {
                source: SeedSource::Text,
                text: "seed".into(),
                kind: None,
            },
            content: Content {
                title: slug.into(),
                kind: IntentKind::Bug,
                current_behavior: String::new(),
                questions: vec![],
                preconditions: vec![],
                criteria: vec![],
                stories: vec![],
                depends_on: vec![],
            },
            agent_id: None,
            created_at: now,
            updated_at: now,
            queued_at: None,
            sent_at: None,
            resolved_at: None,
            last_start_error: None,
            base_branch: feedback.then(|| format!("feedback/{slug}")),
            origin: feedback.then(|| FeedbackOrigin {
                folder: format!("docs/issues/{slug}"),
                branch: format!("feedback/{slug}"),
                commit: "0".repeat(40),
                idea: None,
            }),
        }
    }

    async fn insert_intent(store: &Store, intent: &Intent) {
        let first = IntentVersion {
            intent_id: intent.id,
            version: 1,
            author: VersionAuthor::Seed,
            content: intent.content.clone(),
            at: intent.created_at,
        };
        store.insert_intent(intent, &first).await.unwrap();
    }

    #[tokio::test]
    async fn source_control_stays_on_while_a_feedback_intent_is_unresolved() {
        let fx = fixture(vec![]).await;
        // An intent the user wrote does not hold it; one Feedback made does.
        let own = intent_in(fx.repo.id, "own", IntentState::Queued, false);
        insert_intent(&fx.store, &own).await;
        let mut fed = intent_in(fx.repo.id, "fed", IntentState::Complete, true);
        insert_intent(&fx.store, &fed).await;
        assert!(matches!(
            fx.svc.set_source_control(fx.repo.id, false).await,
            Err(ServiceError::FeedbackIntentsOpen)
        ));
        assert!(!fx
            .store
            .get_repo(fx.repo.id)
            .await
            .unwrap()
            .unwrap()
            .is_plain());

        fed.state = IntentState::Resolved;
        fed.resolved_at = Some(Utc::now());
        fx.store.save_intent(&fed).await.unwrap();
        let off = fx.svc.set_source_control(fx.repo.id, false).await.unwrap();
        assert_eq!(off.source_control, None);
    }

    #[test]
    fn the_refusals_of_adding_and_switching_have_codes_of_their_own() {
        use StatusCode as S;
        let code = |e: ServiceError| e.status_and_code();
        let git = |e: GitError| ServiceError::Git(e).status_and_code();
        assert_eq!(code(ServiceError::RepoInUse), (S::CONFLICT, "repo_in_use"));
        assert_eq!(
            code(ServiceError::RepoOverlaps),
            (S::CONFLICT, "repo_overlaps")
        );
        assert_eq!(
            code(ServiceError::FeedbackIntentsOpen),
            (S::CONFLICT, "repo_has_feedback_intents")
        );
        assert_eq!(
            git(GitError::NotInstalled),
            (S::BAD_REQUEST, "git_not_installed")
        );
        assert_eq!(
            git(GitError::IdentityMissing),
            (S::BAD_REQUEST, "git_identity_missing")
        );
        assert_eq!(
            git(GitError::FolderNotEmpty("/src/full".into())),
            (S::CONFLICT, "folder_not_empty")
        );
        assert_eq!(
            git(GitError::NotRepoRoot),
            (S::BAD_REQUEST, "not_repo_root")
        );
    }

    /// The local branches of the fixture's repository, sorted.
    fn branches(fx: &Fx) -> Vec<String> {
        git(
            &fx.repo_dir,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        )
        .lines()
        .map(str::to_owned)
        .collect()
    }

    #[tokio::test]
    async fn an_agent_that_cannot_be_saved_leaves_no_worktree_and_no_branch() {
        let fx = fixture(vec![]).await;
        let opts = || CreateAgentOptions {
            branch: Some("intent/unsaved".into()),
            intent_id: Some(IntentId::new()),
            system_prompt_append: Some("x".into()),
            base: None,
        };
        fx.store.refuse_new_agents(true).await;

        // A branch that was asked for, and one the daemon names itself.
        let err = fx
            .svc
            .create_agent_with(fx.repo.id, opts())
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::Store(_)), "{err:?}");
        let err = fx.svc.create_agent(fx.repo.id).await.unwrap_err();
        assert!(matches!(err, ServiceError::Store(_)), "{err:?}");

        assert_eq!(
            branches(&fx),
            ["main"],
            "only the user's own branch is left"
        );
        let worktrees = git(&fx.repo_dir, &["worktree", "list", "--porcelain"]);
        assert_eq!(worktrees.matches("worktree ").count(), 1, "{worktrees}");
        let folder = fx.paths.repo_worktrees_dir(&fx.repo);
        assert!(!folder.join("intent-unsaved").exists());
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
        assert!(fx.store.list_agents(true).await.unwrap().is_empty());

        // So the same branch is free for the next try.
        fx.store.refuse_new_agents(false).await;
        let agent = fx.svc.create_agent_with(fx.repo.id, opts()).await.unwrap();
        assert_eq!(agent.branch.as_deref(), Some("intent/unsaved"));
        assert_eq!(branches(&fx), ["intent/unsaved", "main"]);
    }

    #[tokio::test]
    async fn an_unsaved_agents_branch_is_kept_once_it_holds_work() {
        // Only a branch with nothing of its own is deleted: one that moved
        // on has commits nothing else may hold.
        let fx = fixture(vec![]).await;
        git(&fx.repo_dir, &["branch", "moved-on"]);
        git(&fx.repo_dir, &["checkout", "-q", "moved-on"]);
        git(&fx.repo_dir, &["commit", "--allow-empty", "-m", "work"]);
        git(&fx.repo_dir, &["checkout", "-q", "main"]);

        let made_from = |branch: &'static str, base: &'static str| {
            let repo = fx.repo_dir.clone();
            async move {
                git::delete_branch_made_from(&repo, branch, base)
                    .await
                    .unwrap()
            }
        };

        assert!(!made_from("moved-on", "main").await, "it has a commit");
        assert!(!made_from("moved-on", "no-such-base").await, "no base");
        assert!(!made_from("no-such-branch", "main").await, "no branch");
        assert_eq!(branches(&fx), ["main", "moved-on"]);

        git(&fx.repo_dir, &["branch", "untouched"]);
        assert!(made_from("untouched", "refs/heads/main").await);
        assert_eq!(branches(&fx), ["main", "moved-on"]);

        // The base gaining a commit since does not make the branch's own:
        // everything on it is still on the base.
        git(&fx.repo_dir, &["branch", "left-behind"]);
        git(&fx.repo_dir, &["commit", "--allow-empty", "-m", "later"]);
        assert!(made_from("left-behind", "main").await);
        assert_eq!(branches(&fx), ["main", "moved-on"]);
    }

    #[test]
    fn a_taken_worktree_folder_is_a_conflict_like_a_taken_branch() {
        let taken = |e: GitError| ServiceError::Git(e).status_and_code();
        assert_eq!(
            taken(GitError::WorktreeExists("/home/worktrees/app/a-b".into())),
            (StatusCode::CONFLICT, "worktree_exists")
        );
        assert_eq!(
            taken(GitError::BranchExists("a/b".into())),
            (StatusCode::CONFLICT, "branch_exists")
        );
    }

    #[tokio::test]
    async fn two_repositories_with_the_same_folder_name_keep_their_worktrees_apart() {
        let fx = fixture(vec![]).await;
        // A second repository whose folder is also called `my-repo`.
        let other_dir = fx.tmp.path().join("elsewhere").join("my-repo");
        std::fs::create_dir_all(&other_dir).unwrap();
        git(&other_dir, &["init", "-b", "main"]);
        git(&other_dir, &["commit", "--allow-empty", "-m", "init"]);
        let other = fx
            .svc
            .add_repo(&other_dir, AddSourceControl::Git)
            .await
            .unwrap();
        let on = |branch: &str| CreateAgentOptions {
            branch: Some(branch.into()),
            ..CreateAgentOptions::default()
        };

        let first = fx
            .svc
            .create_agent_with(fx.repo.id, on("intent/same"))
            .await
            .unwrap();
        let second = fx
            .svc
            .create_agent_with(other.id, on("intent/same"))
            .await
            .unwrap();

        assert_eq!(first.branch.as_deref(), Some("intent/same"));
        assert_eq!(second.branch.as_deref(), Some("intent/same"));
        assert_ne!(first.worktree_path, second.worktree_path);
        for (agent, repo_dir) in [(&first, &fx.repo_dir), (&second, &other_dir)] {
            assert_eq!(
                git(&agent.worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"]),
                "intent/same"
            );
            // The worktree belongs to its own repository.
            let listed = git(repo_dir, &["worktree", "list", "--porcelain"]);
            assert_eq!(listed.matches("worktree ").count(), 2, "{listed}");
        }
    }

    #[tokio::test]
    async fn an_agent_can_start_from_another_branch() {
        let fx = fixture(vec![]).await;
        let main_tip = git(&fx.repo_dir, &["rev-parse", "main"]);
        git(&fx.repo_dir, &["switch", "-c", "feedback/x"]);
        let folder = fx.repo_dir.join("docs/issues/x");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("issue.md"), "# Fix the zoom\n").unwrap();
        git(&fx.repo_dir, &["add", "--", "docs/issues/x/issue.md"]);
        git(
            &fx.repo_dir,
            &["commit", "-m", "docs(issues): Fix the zoom"],
        );
        git(&fx.repo_dir, &["switch", "main"]);
        // A tag of the same name on `main`: the base is read as a local branch.
        git(&fx.repo_dir, &["tag", "feedback/x", "main"]);

        let agent = fx
            .svc
            .create_agent_with(
                fx.repo.id,
                CreateAgentOptions {
                    base: Some("feedback/x".into()),
                    branch: Some("intent/y".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(agent.branch.as_deref(), Some("intent/y"));
        assert!(agent.worktree_path.join("docs/issues/x/issue.md").is_file());
        // Succeeds only when the base is an ancestor of the agent's branch.
        git(
            &fx.repo_dir,
            &[
                "merge-base",
                "--is-ancestor",
                "refs/heads/feedback/x",
                "intent/y",
            ],
        );
        assert_eq!(
            git(&fx.repo_dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "main"
        );

        // Without a base the agent starts from the default branch.
        let plain = fx.svc.create_agent(fx.repo.id).await.unwrap();
        assert!(!plain.worktree_path.join("docs/issues/x").exists());
        assert_eq!(
            git(
                &fx.repo_dir,
                &["rev-parse", plain.branch.as_deref().unwrap()]
            ),
            main_tip
        );
    }

    #[tokio::test]
    async fn send_message_persists_and_streams_reply() {
        let fx = fixture(vec![vec![
            started("s1"),
            text("Hello"),
            text(", world"),
            completed("s1", Some("Hello, world")),
        ]])
        .await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        let user = fx.svc.send_message(agent.id, "hi".into()).await.unwrap();
        assert_eq!(
            (user.role, user.text.as_str(), user.seq),
            (Role::User, "hi", 1)
        );
        assert_eq!(user.agent_id, agent.id);

        let events = until_turn_ends(&mut rx, agent.id).await;
        assert_eq!(
            describe_all(&events),
            [
                "User: hi",
                "state Working",
                "delta Hello",
                "delta , world",
                "Assistant: Hello, world",
                "state Done",
            ]
        );
        assert!(matches!(&events[0], EventKind::MessageAppended(m) if *m == user));
        assert_eq!(
            fx.messages(agent.id).await,
            [
                (Role::User, "hi".to_string()),
                (Role::Assistant, "Hello, world".to_string())
            ]
        );
        let stored = fx.agent(agent.id).await;
        assert_eq!(stored.state, AgentState::Done);
        assert_eq!(stored.harness_session_id.as_deref(), Some("s1"));
        let reqs = fx.harness.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].prompt, "hi");
        assert_eq!(reqs[0].cwd, agent.worktree_path);
        assert_eq!(reqs[0].resume_session_id, None);
        assert_eq!(fx.svc.running_agent_count(), 0);
    }

    #[tokio::test]
    async fn second_send_while_running_is_turn_in_progress() {
        let harness = FakeHarness::scripted(vec![vec![
            started("s1"),
            text("slow"),
            completed("s1", None),
        ]])
        .with_delay(Duration::from_millis(100));
        let fx = fixture_with(harness, NEVER_QUIET).await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        assert!(!fx.svc.turn_in_flight(agent.id));
        fx.svc.send_message(agent.id, "first".into()).await.unwrap();
        assert_eq!(fx.svc.running_agent_count(), 1);
        // In flight from the moment the message is accepted.
        assert!(fx.svc.turn_in_flight(agent.id));
        let err = fx
            .svc
            .send_message(agent.id, "second".into())
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::TurnInProgress), "{err:?}");

        until_turn_ends(&mut rx, agent.id).await;
        assert_eq!(fx.harness.requests().len(), 1);
        assert_eq!(
            fx.messages(agent.id).await,
            [
                (Role::User, "first".to_string()),
                (Role::Assistant, "slow".to_string())
            ]
        );
        assert_eq!(fx.svc.running_agent_count(), 0);
        assert!(!fx.svc.turn_in_flight(agent.id));
    }

    #[tokio::test]
    async fn session_id_is_saved_and_reused_on_next_turn() {
        let fx = fixture(vec![
            vec![started("s1"), text("one"), completed("s1", Some("one"))],
            // No deltas: the reply falls back to the result text.
            vec![completed("s1", Some("two"))],
        ])
        .await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(agent.id, "a".into()).await.unwrap();
        until_turn_ends(&mut rx, agent.id).await;
        assert_eq!(
            fx.agent(agent.id).await.harness_session_id.as_deref(),
            Some("s1")
        );
        fx.svc.send_message(agent.id, "b".into()).await.unwrap();
        until_turn_ends(&mut rx, agent.id).await;

        let reqs = fx.harness.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].resume_session_id, None);
        assert_eq!(reqs[1].resume_session_id.as_deref(), Some("s1"));
        assert_eq!(
            fx.messages(agent.id).await.last().unwrap(),
            &(Role::Assistant, "two".to_string())
        );
    }

    #[tokio::test]
    async fn harness_failure_marks_agent_dead_and_keeps_session_id() {
        let fx = fixture(vec![vec![started("s1"), text("partial"), failed("boom")]]).await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(agent.id, "go".into()).await.unwrap();
        let events = until_turn_ends(&mut rx, agent.id).await;
        assert_eq!(
            describe_all(&events),
            [
                "User: go",
                "state Working",
                "delta partial",
                "Assistant: partial",
                "Assistant: Agent process failed: boom",
                "state Dead",
            ]
        );
        let stored = fx.agent(agent.id).await;
        assert_eq!(stored.state, AgentState::Dead);
        assert_eq!(stored.harness_session_id.as_deref(), Some("s1"));
        assert_eq!(fx.messages(agent.id).await.len(), 3);
        assert_eq!(fx.svc.running_agent_count(), 0);
    }

    #[tokio::test]
    async fn stream_end_without_completion_marks_agent_dead() {
        let fx = fixture(vec![vec![started("s1"), text("half")]]).await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(agent.id, "go".into()).await.unwrap();
        until_turn_ends(&mut rx, agent.id).await;
        let stored = fx.agent(agent.id).await;
        assert_eq!(stored.state, AgentState::Dead);
        assert_eq!(stored.harness_session_id.as_deref(), Some("s1"));
        assert_eq!(
            fx.messages(agent.id).await,
            [
                (Role::User, "go".to_string()),
                (Role::Assistant, "half".to_string()),
                (
                    Role::Assistant,
                    "Agent process failed: stream ended before the turn completed".to_string()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn error_result_marks_agent_dead() {
        let fx = fixture(vec![vec![
            started("s1"),
            HarnessEvent::TurnCompleted {
                session_id: "s1".into(),
                is_error: true,
                result_text: Some("Credit balance is too low".into()),
            },
        ]])
        .await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(agent.id, "go".into()).await.unwrap();
        until_turn_ends(&mut rx, agent.id).await;
        let stored = fx.agent(agent.id).await;
        assert_eq!(stored.state, AgentState::Dead);
        assert_eq!(stored.harness_session_id.as_deref(), Some("s1"));
        assert_eq!(
            fx.messages(agent.id).await.last().unwrap(),
            &(
                Role::Assistant,
                "Agent process failed: Credit balance is too low".to_string()
            )
        );
    }

    #[tokio::test]
    async fn failure_after_completed_reply_marks_agent_dead() {
        // What the Claude harness emits when the CLI prints a result, then exits non-zero.
        let fx = fixture(vec![vec![
            started("s1"),
            text("hi"),
            completed("s1", Some("hi")),
            failed("claude exited with exit status: 3"),
        ]])
        .await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(agent.id, "go".into()).await.unwrap();
        let events = until_turn_ends(&mut rx, agent.id).await;
        assert_eq!(
            describe_all(&events),
            [
                "User: go",
                "state Working",
                "delta hi",
                "Assistant: hi",
                "Assistant: Agent process failed: claude exited with exit status: 3",
                "state Dead",
            ]
        );
        assert_eq!(
            fx.agent(agent.id).await.harness_session_id.as_deref(),
            Some("s1")
        );
    }

    #[tokio::test]
    async fn dead_agent_can_be_messaged_again_and_resumes() {
        let fx = fixture(vec![
            vec![started("s1"), failed("crashed")],
            vec![started("s1"), text("back"), completed("s1", Some("back"))],
        ])
        .await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(agent.id, "one".into()).await.unwrap();
        until_turn_ends(&mut rx, agent.id).await;
        assert_eq!(fx.agent(agent.id).await.state, AgentState::Dead);

        fx.svc.send_message(agent.id, "two".into()).await.unwrap();
        let events = until_turn_ends(&mut rx, agent.id).await;
        assert_eq!(
            describe_all(&events),
            [
                "User: two",
                "state Working",
                "delta back",
                "Assistant: back",
                "state Done"
            ]
        );
        assert_eq!(fx.agent(agent.id).await.state, AgentState::Done);
        assert_eq!(
            fx.harness.requests()[1].resume_session_id.as_deref(),
            Some("s1")
        );
    }

    #[tokio::test]
    async fn silent_turn_becomes_quiet_then_working_on_next_event() {
        // Every event arrives 400ms after the previous one; quiet after 50ms.
        let harness = FakeHarness::scripted(vec![vec![text("hello"), completed("s1", None)]])
            .with_delay(Duration::from_millis(400));
        let fx = fixture_with(harness, Duration::from_millis(50)).await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(agent.id, "hi".into()).await.unwrap();
        let mut events = recv_until(&mut rx, |k| is_state(k, AgentState::Quiet)).await;
        assert_eq!(fx.agent(agent.id).await.state, AgentState::Quiet);
        assert_eq!(fx.svc.running_agent_count(), 1);

        events.extend(until_turn_ends(&mut rx, agent.id).await);
        assert_eq!(
            describe_all(&events),
            [
                "User: hi",
                "state Working",
                "state Quiet",
                "state Working",
                "delta hello",
                "state Quiet",
                "state Working",
                "Assistant: hello",
                "state Done",
            ]
        );
        assert_eq!(fx.svc.running_agent_count(), 0);
    }

    #[tokio::test]
    async fn closed_agent_rejects_messages() {
        let fx = fixture(vec![vec![completed("s1", Some("never"))]]).await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        fx.svc.close_agent(agent.id).await.unwrap();

        let err = fx
            .svc
            .send_message(agent.id, "hello?".into())
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::AgentClosed), "{err:?}");
        let err = fx.svc.close_agent(agent.id).await.unwrap_err();
        assert!(matches!(err, ServiceError::AgentClosed), "{err:?}");
        assert!(fx.messages(agent.id).await.is_empty());
        assert!(fx.harness.requests().is_empty());
        assert_eq!(fx.svc.running_agent_count(), 0);
    }

    #[tokio::test]
    async fn close_while_turn_running_is_turn_in_progress() {
        let harness = FakeHarness::scripted(vec![vec![started("s1"), completed("s1", Some("ok"))]])
            .with_delay(Duration::from_millis(100));
        let fx = fixture_with(harness, NEVER_QUIET).await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(agent.id, "go".into()).await.unwrap();
        let err = fx.svc.close_agent(agent.id).await.unwrap_err();
        assert!(matches!(err, ServiceError::TurnInProgress), "{err:?}");
        assert!(fx.agent(agent.id).await.closed_at.is_none());

        until_turn_ends(&mut rx, agent.id).await;
        fx.svc.close_agent(agent.id).await.unwrap();
        assert!(fx.agent(agent.id).await.closed_at.is_some());
    }

    #[tokio::test]
    async fn concurrent_creates_get_distinct_names_and_colors() {
        let fx = fixture(vec![]).await;
        let (a, b, c) = tokio::join!(
            fx.svc.create_agent(fx.repo.id),
            fx.svc.create_agent(fx.repo.id),
            fx.svc.create_agent(fx.repo.id),
        );
        let agents = [a.unwrap(), b.unwrap(), c.unwrap()];
        let mut names: Vec<_> = agents.iter().map(|a| a.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["Earth", "Mercury", "Venus"]);
        for (i, x) in agents.iter().enumerate() {
            for y in &agents[i + 1..] {
                assert_ne!(x.color, y.color);
            }
        }
    }

    #[tokio::test]
    async fn tool_use_appends_tool_message() {
        let fx = fixture(vec![vec![
            started("s1"),
            HarnessEvent::ToolUse {
                name: "Bash".into(),
                summary: "ls -la".into(),
            },
            text("listed"),
            completed("s1", Some("listed")),
        ]])
        .await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(agent.id, "ls".into()).await.unwrap();
        let events = until_turn_ends(&mut rx, agent.id).await;
        assert!(describe_all(&events).contains(&"Tool: Bash: ls -la".to_string()));
        assert_eq!(
            fx.messages(agent.id).await,
            [
                (Role::User, "ls".to_string()),
                (Role::Tool, "Bash: ls -la".to_string()),
                (Role::Assistant, "listed".to_string()),
            ]
        );
    }

    fn block() -> HarnessEvent {
        HarnessEvent::TextBlockStarted
    }

    fn tool(name: &str, summary: &str) -> HarnessEvent {
        HarnessEvent::ToolUse {
            name: name.into(),
            summary: summary.into(),
        }
    }

    #[tokio::test]
    async fn text_around_a_tool_call_is_saved_in_order() {
        let fx = fixture(vec![vec![
            started("s1"),
            block(),
            text("I'll check."),
            tool("Bash", "ls"),
            block(),
            text("Done."),
            completed("s1", Some("Done.")),
        ]])
        .await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(agent.id, "go".into()).await.unwrap();
        let events = until_turn_ends(&mut rx, agent.id).await;
        assert_eq!(
            describe_all(&events),
            [
                "User: go",
                "state Working",
                "delta I'll check.",
                "Assistant: I'll check.",
                "Tool: Bash: ls",
                "delta Done.",
                "Assistant: Done.",
                "state Done",
            ]
        );
        assert_eq!(
            fx.messages(agent.id).await,
            [
                (Role::User, "go".to_string()),
                (Role::Assistant, "I'll check.".to_string()),
                (Role::Tool, "Bash: ls".to_string()),
                (Role::Assistant, "Done.".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn separate_text_blocks_are_saved_as_paragraphs() {
        let fx = fixture(vec![vec![
            started("s1"),
            block(),
            text("One"),
            text(" more."),
            block(),
            text("Two."),
            completed("s1", Some("Two.")),
        ]])
        .await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(agent.id, "go".into()).await.unwrap();
        let events = until_turn_ends(&mut rx, agent.id).await;
        // Live deltas are passed on as they came.
        assert_eq!(
            describe_all(&events),
            [
                "User: go",
                "state Working",
                "delta One",
                "delta  more.",
                "delta Two.",
                "Assistant: One more.\n\nTwo.",
                "state Done",
            ]
        );
    }

    #[tokio::test]
    async fn result_text_is_not_saved_again_after_streamed_text() {
        // The result repeats the last text; it was already saved before the tool.
        let fx = fixture(vec![vec![
            started("s1"),
            text("Checking."),
            tool("Read", "a.txt"),
            completed("s1", Some("Checking.")),
        ]])
        .await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(agent.id, "go".into()).await.unwrap();
        until_turn_ends(&mut rx, agent.id).await;
        assert_eq!(
            fx.messages(agent.id).await,
            [
                (Role::User, "go".to_string()),
                (Role::Assistant, "Checking.".to_string()),
                (Role::Tool, "Read: a.txt".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn turn_without_any_text_saves_no_assistant_message() {
        let fx = fixture(vec![vec![started("s1"), block(), completed("s1", None)]]).await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(agent.id, "go".into()).await.unwrap();
        let events = until_turn_ends(&mut rx, agent.id).await;
        assert_eq!(
            describe_all(&events),
            ["User: go", "state Working", "state Done"]
        );
        assert_eq!(
            fx.messages(agent.id).await,
            [(Role::User, "go".to_string())]
        );
        assert_eq!(fx.agent(agent.id).await.state, AgentState::Done);
    }

    #[tokio::test]
    async fn failure_after_a_tool_keeps_text_in_order() {
        let fx = fixture(vec![vec![
            started("s1"),
            text("I'll check."),
            tool("Bash", "ls"),
            text("half"),
            failed("boom"),
        ]])
        .await;
        let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(agent.id, "go".into()).await.unwrap();
        until_turn_ends(&mut rx, agent.id).await;
        assert_eq!(
            fx.messages(agent.id).await,
            [
                (Role::User, "go".to_string()),
                (Role::Assistant, "I'll check.".to_string()),
                (Role::Tool, "Bash: ls".to_string()),
                (Role::Assistant, "half".to_string()),
                (Role::Assistant, "Agent process failed: boom".to_string()),
            ]
        );
        assert_eq!(fx.agent(agent.id).await.state, AgentState::Dead);
    }

    /// A harness whose stream panics, standing in for a bug in turn handling.
    struct PanickingHarness;

    #[async_trait]
    impl Harness for PanickingHarness {
        async fn run_turn(
            &self,
            _req: TurnRequest,
        ) -> Result<BoxStream<'static, HarnessEvent>, HarnessError> {
            Ok(stream::once(async { panic!("harness stream exploded") }).boxed())
        }

        async fn run_structured(
            &self,
            _req: StructuredRequest,
        ) -> Result<StructuredResult, HarnessError> {
            Err(HarnessError::Failed("not used".into()))
        }
    }

    #[tokio::test]
    async fn panicking_turn_marks_agent_dead_and_frees_the_agent() {
        let fx = fixture(vec![]).await;
        let svc = AgentService::new(
            fx.store.clone(),
            fx.bus.clone(),
            Arc::new(PanickingHarness),
            fx.paths.clone(),
            NEVER_QUIET,
        );
        let agent = svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        svc.send_message(agent.id, "go".into()).await.unwrap();
        until_turn_ends(&mut rx, agent.id).await;
        assert_eq!(fx.agent(agent.id).await.state, AgentState::Dead);
        let (role, last) = fx.messages(agent.id).await.pop().unwrap();
        assert_eq!(role, Role::Assistant);
        assert!(
            last.starts_with("Agent process failed:") && last.contains("harness stream exploded"),
            "{last}"
        );
        assert_eq!(svc.running_agent_count(), 0);
        // The agent is not stuck: it accepts the next message.
        svc.send_message(agent.id, "again".into()).await.unwrap();
        until_turn_ends(&mut rx, agent.id).await;
    }

    #[tokio::test]
    async fn unknown_ids_are_not_found() {
        let fx = fixture(vec![]).await;
        let err = fx.svc.create_agent(RepoId::new()).await.unwrap_err();
        assert!(matches!(err, ServiceError::RepoNotFound), "{err:?}");
        let err = fx
            .svc
            .send_message(AgentId::new(), "x".into())
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::AgentNotFound), "{err:?}");
        let err = fx.svc.close_agent(AgentId::new()).await.unwrap_err();
        assert!(matches!(err, ServiceError::AgentNotFound), "{err:?}");
        assert_eq!(fx.svc.running_agent_count(), 0);
        assert!(fx.harness.requests().is_empty());
    }

    #[tokio::test]
    async fn add_repo_validates_and_records_root() {
        let fx = fixture(vec![]).await;
        assert_eq!(fx.repo.default_branch(), Some("main"));
        assert_eq!(
            std::fs::canonicalize(&fx.repo.path).unwrap(),
            std::fs::canonicalize(&fx.repo_dir).unwrap()
        );
        assert_eq!(fx.store.list_repos().await.unwrap(), vec![fx.repo.clone()]);

        let sub = fx.repo_dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let err = fx
            .svc
            .add_repo(&sub, AddSourceControl::Git)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ServiceError::Store(StoreError::DuplicateRepoPath)),
            "{err:?}"
        );
        let plain = fx.tmp.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        let err = fx
            .svc
            .add_repo(&plain, AddSourceControl::Git)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ServiceError::Git(GitError::NotAGitRepo)),
            "{err:?}"
        );
    }

    /// A plain repository at `<tmp>/<name>`, a folder added with no source
    /// control.
    async fn add_plain(fx: &Fx, name: &str) -> Repo {
        let dir = fx.tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        fx.svc.add_repo(&dir, AddSourceControl::None).await.unwrap()
    }

    /// One turn that completes, for a harness with a delay.
    fn a_turn() -> Vec<HarnessEvent> {
        vec![started("s1"), completed("s1", Some("ok"))]
    }

    #[tokio::test]
    async fn an_agent_on_a_plain_repository_works_in_the_folder_itself() {
        let fx = fixture(vec![a_turn()]).await;
        // The folder is a git repository of its own, added as a plain
        // folder: no worktree and no branch may be made in it.
        let dir = fx.tmp.path().join("notes");
        std::fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-b", "main"]);
        git(&dir, &["commit", "--allow-empty", "-m", "init"]);
        let plain = fx.svc.add_repo(&dir, AddSourceControl::None).await.unwrap();
        let untouched = |dir: &Path| {
            let heads = git(
                dir,
                &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
            );
            assert_eq!(heads, "main");
            let listed = git(dir, &["worktree", "list", "--porcelain"]);
            assert_eq!(listed.matches("worktree ").count(), 1, "{listed}");
        };
        let mut rx = fx.live();

        let agent = fx.svc.create_agent(plain.id).await.unwrap();
        assert_eq!(agent.worktree_path, plain.path);
        assert_eq!(agent.branch, None);
        assert!(agent.works_in_place());
        assert_eq!(agent.state, AgentState::Idle);
        assert_eq!(fx.agent(agent.id).await, agent);
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::AgentCreated(agent.clone())
        );
        assert!(!fx.paths.repo_worktrees_dir(&plain).exists());
        untouched(&dir);

        // One turn: the harness runs in the folder itself.
        fx.svc.send_message(agent.id, "go".into()).await.unwrap();
        until_turn_ends(&mut rx, agent.id).await;
        assert_eq!(fx.agent(agent.id).await.state, AgentState::Done);
        let reqs = fx.harness.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].cwd, plain.path);

        // Options that name a branch or a base need source control.
        for opts in [
            CreateAgentOptions {
                branch: Some("intent/x".into()),
                ..Default::default()
            },
            CreateAgentOptions {
                base: Some("main".into()),
                ..Default::default()
            },
        ] {
            let err = fx.svc.create_agent_with(plain.id, opts).await.unwrap_err();
            assert!(matches!(err, ServiceError::NeedsSourceControl), "{err:?}");
        }
        // An agent that cannot be saved takes nothing away: the folder is
        // the user's own.
        fx.store.refuse_new_agents(true).await;
        let err = fx.svc.create_agent(plain.id).await.unwrap_err();
        assert!(matches!(err, ServiceError::Store(_)), "{err:?}");
        fx.store.refuse_new_agents(false).await;
        assert!(dir.join(".git").is_dir());
        assert_eq!(
            fx.store.list_agents(true).await.unwrap(),
            vec![fx.agent(agent.id).await]
        );
        assert!(!fx.paths.repo_worktrees_dir(&plain).exists());
        untouched(&dir);

        // A folder that is gone has no directory to work in.
        let gone = add_plain(&fx, "gone").await;
        std::fs::remove_dir(&gone.path).unwrap();
        let err = fx.svc.create_agent(gone.id).await.unwrap_err();
        assert!(
            matches!(&err, ServiceError::Git(GitError::PathNotFound(p)) if *p == gone.path),
            "{err:?}"
        );
        assert_eq!(fx.store.list_agents(true).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn one_agent_works_in_a_plain_folder_at_a_time() {
        let harness = FakeHarness::scripted(vec![a_turn(), a_turn(), a_turn()])
            .with_delay(Duration::from_millis(500));
        let fx = fixture_with(harness, NEVER_QUIET).await;
        let plain = add_plain(&fx, "notes").await;
        let a = fx.svc.create_agent(plain.id).await.unwrap();
        let b = fx.svc.create_agent(plain.id).await.unwrap();
        let c = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        assert!(!fx.svc.folder_busy(plain.id));
        fx.svc.send_message(a.id, "go".into()).await.unwrap();
        assert!(fx.svc.folder_busy(plain.id));
        // The agent's own turn is answered first.
        let err = fx.svc.send_message(a.id, "again".into()).await.unwrap_err();
        assert!(matches!(err, ServiceError::TurnInProgress), "{err:?}");

        let err = fx.svc.send_message(b.id, "go".into()).await.unwrap_err();
        assert!(matches!(err, ServiceError::FolderBusy), "{err:?}");
        assert_eq!(err.status_and_code(), (StatusCode::CONFLICT, "folder_busy"));
        assert_eq!(err.to_string(), "Another agent is working in this folder.");
        assert!(
            fx.messages(b.id).await.is_empty(),
            "the refused message is not kept"
        );
        assert!(!fx.svc.turn_in_flight(b.id));
        assert_eq!(fx.agent(b.id).await.state, AgentState::Idle);

        // Another repository is free.
        fx.svc.send_message(c.id, "go".into()).await.unwrap();
        assert!(!fx.svc.folder_busy(fx.repo.id));
        assert_eq!(fx.svc.running_agent_count(), 2);
        // A turn always ends before its folder is free.
        let err = fx.svc.close_agent(a.id).await.unwrap_err();
        assert!(matches!(err, ServiceError::TurnInProgress), "{err:?}");
        assert!(fx.svc.folder_busy(plain.id));

        until_turns_end(&mut rx, &[a.id, c.id]).await;
        assert!(!fx.svc.folder_busy(plain.id));
        fx.svc.send_message(b.id, "go".into()).await.unwrap();
        assert!(fx.svc.folder_busy(plain.id));
        until_turn_ends(&mut rx, b.id).await;
        assert!(!fx.svc.folder_busy(plain.id));
        assert_eq!(fx.harness.requests().len(), 3);
        for id in [a.id, b.id, c.id] {
            assert_eq!(fx.agent(id).await.state, AgentState::Done);
        }
        assert_eq!(
            fx.messages(b.id).await,
            [
                (Role::User, "go".to_string()),
                (Role::Assistant, "ok".to_string())
            ]
        );
    }

    /// A harness that cannot start a turn at all.
    struct UnstartableHarness;

    #[async_trait]
    impl Harness for UnstartableHarness {
        async fn run_turn(
            &self,
            _req: TurnRequest,
        ) -> Result<BoxStream<'static, HarnessEvent>, HarnessError> {
            Err(HarnessError::Failed("could not start".into()))
        }

        async fn run_structured(
            &self,
            _req: StructuredRequest,
        ) -> Result<StructuredResult, HarnessError> {
            Err(HarnessError::Failed("not used".into()))
        }
    }

    #[tokio::test]
    async fn a_turn_that_fails_frees_its_folder() {
        let fx = fixture(vec![vec![failed("boom")], a_turn()]).await;
        let plain = add_plain(&fx, "notes").await;
        let a = fx.svc.create_agent(plain.id).await.unwrap().id;
        let b = fx.svc.create_agent(plain.id).await.unwrap().id;
        // A turn that reports a failure, one whose harness cannot start, and
        // one that panics.
        let failing: [Arc<dyn Harness>; 3] = [
            Arc::new(fx.harness.clone()),
            Arc::new(UnstartableHarness),
            Arc::new(PanickingHarness),
        ];
        for harness in failing {
            let svc = AgentService::new(
                fx.store.clone(),
                fx.bus.clone(),
                harness,
                fx.paths.clone(),
                NEVER_QUIET,
            );
            let mut rx = fx.live();
            svc.send_message(a, "go".into()).await.unwrap();
            assert!(svc.folder_busy(plain.id));
            until_turn_ends(&mut rx, a).await;
            assert_eq!(fx.agent(a).await.state, AgentState::Dead);
            assert!(!svc.folder_busy(plain.id));
            svc.send_message(b, "go".into()).await.unwrap();
            until_turn_ends(&mut rx, b).await;
            assert!(!svc.folder_busy(plain.id));
        }
    }

    #[tokio::test]
    async fn two_agents_on_a_git_repository_still_work_at_once() {
        let harness =
            FakeHarness::scripted(vec![a_turn(), a_turn()]).with_delay(Duration::from_millis(200));
        let fx = fixture_with(harness, NEVER_QUIET).await;
        let a = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let b = fx.svc.create_agent(fx.repo.id).await.unwrap();
        let mut rx = fx.live();

        fx.svc.send_message(a.id, "go".into()).await.unwrap();
        fx.svc.send_message(b.id, "go".into()).await.unwrap();
        assert!(fx.svc.turn_in_flight(a.id) && fx.svc.turn_in_flight(b.id));
        assert_eq!(fx.svc.running_agent_count(), 2);
        assert!(!fx.svc.folder_busy(fx.repo.id));
        until_turns_end(&mut rx, &[a.id, b.id]).await;
        for id in [a.id, b.id] {
            assert_eq!(fx.agent(id).await.state, AgentState::Done);
        }
        assert!(!fx.svc.folder_busy(fx.repo.id));
    }
}

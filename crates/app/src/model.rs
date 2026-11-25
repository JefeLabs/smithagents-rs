//! GPUI-free shell state: the agent list, open tabs, selection and the
//! connection, driven by [`Action`]s. Applying one returns the [`Effect`]s the
//! view layer must carry out. Chat content is not kept here; it belongs to the
//! web tab.

use smithagents_core::agent::{Agent, Repo};
use smithagents_core::event::{Event, EventKind};
use smithagents_core::ids::AgentId;

/// State of the link to the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Connection {
    #[default]
    Connecting,
    Connected,
    Reconnecting,
}

/// Something that happened: from the daemon, the link, or the user.
// Actions are short-lived values moved straight into `apply`; boxing the
// event would only complicate the public `Action::Event(Event)` shape.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Snapshot {
        agents: Vec<Agent>,
        repos: Vec<Repo>,
        latest_seq: u64,
    },
    Event(Event),
    ConnectionChanged(Connection),
    SelectAgent(AgentId),
    OpenTab(AgentId),
    CloseTab(AgentId),
    RepoAdded(Repo),
}

/// Work for the view layer after an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    OpenCenterTab(AgentId),
    CloseCenterTab(AgentId),
    FocusCenterTab(AgentId),
    UpdateTabAccent(AgentId),
}

/// The shell's state. Holds open agents only.
#[derive(Debug, Default)]
pub struct ShellModel {
    agents: Vec<Agent>,
    repos: Vec<Repo>,
    selected: Option<AgentId>,
    tabs: Vec<AgentId>,
    active_tab: Option<AgentId>,
    connection: Connection,
    last_seq: Option<u64>,
}

impl ShellModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open agents, oldest first (ties broken by id).
    pub fn agents(&self) -> Vec<&Agent> {
        let mut agents: Vec<&Agent> = self.agents.iter().collect();
        agents.sort_by_key(|a| (a.created_at, a.id.0));
        agents
    }

    /// The open agent `id`, if the model has it.
    pub fn agent(&self, id: AgentId) -> Option<&Agent> {
        self.agents.iter().find(|a| a.id == id)
    }

    pub fn repos(&self) -> &[Repo] {
        &self.repos
    }

    pub fn selected(&self) -> Option<AgentId> {
        self.selected
    }

    pub fn tabs(&self) -> &[AgentId] {
        &self.tabs
    }

    pub fn active_tab(&self) -> Option<AgentId> {
        self.active_tab
    }

    pub fn connection(&self) -> Connection {
        self.connection
    }

    pub fn last_seq(&self) -> Option<u64> {
        self.last_seq
    }

    pub fn apply(&mut self, action: Action) -> Vec<Effect> {
        let mut effects = Vec::new();
        match action {
            Action::Snapshot {
                agents,
                repos,
                latest_seq,
            } => {
                self.agents = agents
                    .into_iter()
                    .filter(|a| a.closed_at.is_none())
                    .collect();
                self.repos = repos;
                self.last_seq = Some(latest_seq);
                self.drop_missing_tabs(&mut effects);
                self.fix_selection();
            }
            Action::Event(event) => self.apply_event(event, &mut effects),
            Action::ConnectionChanged(connection) => self.connection = connection,
            Action::SelectAgent(id) => {
                if self.has_agent(id) {
                    self.selected = Some(id);
                }
            }
            Action::OpenTab(id) => {
                if self.has_agent(id) {
                    self.selected = Some(id);
                    if self.tabs.contains(&id) {
                        effects.push(Effect::FocusCenterTab(id));
                    } else {
                        self.tabs.push(id);
                        effects.push(Effect::OpenCenterTab(id));
                    }
                    self.active_tab = Some(id);
                }
            }
            Action::CloseTab(id) => self.close_tab(id, &mut effects),
            Action::RepoAdded(repo) => match self.repos.iter_mut().find(|r| r.id == repo.id) {
                Some(existing) => *existing = repo,
                None => self.repos.push(repo),
            },
        }
        effects
    }

    fn apply_event(&mut self, event: Event, effects: &mut Vec<Effect>) {
        if self.last_seq.is_some_and(|last| event.seq <= last) {
            return;
        }
        self.last_seq = Some(event.seq);
        match event.kind {
            EventKind::AgentCreated(agent) | EventKind::AgentUpdated(agent) => {
                if agent.closed_at.is_some() {
                    self.remove_agent(agent.id, effects);
                    return;
                }
                let id = agent.id;
                match self.agents.iter_mut().find(|a| a.id == id) {
                    Some(existing) => {
                        let recolored = existing.color != agent.color;
                        *existing = agent;
                        if recolored && self.tabs.contains(&id) {
                            effects.push(Effect::UpdateTabAccent(id));
                        }
                    }
                    None => self.agents.push(agent),
                }
            }
            EventKind::AgentClosed { agent_id } => self.remove_agent(agent_id, effects),
            EventKind::MessageAppended(_)
            | EventKind::TurnDelta { .. }
            | EventKind::IntentCreated(_)
            | EventKind::IntentUpdated(_) => {}
        }
    }

    /// Closes every tab whose agent is gone (one `CloseCenterTab` each, in tab
    /// order), then, if the active tab was among them, focuses at most one
    /// surviving tab: the nearest before it, else the nearest after.
    fn drop_missing_tabs(&mut self, effects: &mut Vec<Effect>) {
        let old = std::mem::take(&mut self.tabs);
        let old_active = self
            .active_tab
            .and_then(|a| old.iter().position(|t| *t == a));
        let (kept, gone): (Vec<AgentId>, Vec<AgentId>) =
            old.iter().copied().partition(|id| self.has_agent(*id));
        effects.extend(gone.iter().map(|id| Effect::CloseCenterTab(*id)));
        if let Some(at) = old_active {
            if !kept.contains(&old[at]) {
                let survives = |id: &&AgentId| kept.contains(id);
                let next = old[..at]
                    .iter()
                    .rev()
                    .find(survives)
                    .or_else(|| old[at..].iter().find(survives))
                    .copied();
                self.active_tab = next;
                effects.extend(next.map(Effect::FocusCenterTab));
            }
        }
        self.tabs = kept;
    }

    fn has_agent(&self, id: AgentId) -> bool {
        self.agents.iter().any(|a| a.id == id)
    }

    fn remove_agent(&mut self, id: AgentId, effects: &mut Vec<Effect>) {
        self.agents.retain(|a| a.id != id);
        self.close_tab(id, effects);
        self.fix_selection();
    }

    fn fix_selection(&mut self) {
        if self.selected.is_some_and(|id| !self.has_agent(id)) {
            self.selected = None;
        }
    }

    /// Closes `id`'s tab if open. When it was active, the tab before it (else
    /// the one after) becomes active.
    fn close_tab(&mut self, id: AgentId, effects: &mut Vec<Effect>) {
        let Some(index) = self.tabs.iter().position(|t| *t == id) else {
            return;
        };
        self.tabs.remove(index);
        effects.push(Effect::CloseCenterTab(id));
        if self.active_tab == Some(id) {
            let next = index
                .checked_sub(1)
                .and_then(|i| self.tabs.get(i))
                .or_else(|| self.tabs.get(index))
                .copied();
            self.active_tab = next;
            if let Some(next) = next {
                effects.push(Effect::FocusCenterTab(next));
            }
        }
    }
}

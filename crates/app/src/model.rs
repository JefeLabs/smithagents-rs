//! GPUI-free shell state: the agent list, open tabs, selection, the
//! connection, the intents and the agent defaults, driven by [`Action`]s.
//! Applying one returns the [`Effect`]s the view layer must carry out. Chat
//! content is not kept here; it belongs to the web tab.

use std::cmp::Reverse;

use smithagents_core::agent::{Agent, Repo};
use smithagents_core::event::{Event, EventKind};
use smithagents_core::ids::{AgentId, IntentId};
use smithagents_core::intent::{AgentDefaults, Intent};

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
    /// The view layer could not carry out `OpenTab(id)`: closes `id`'s tab
    /// again and puts the active tab and the selection back to what they were
    /// before it (`active`, `selected`), as far as they still exist.
    RevertOpen {
        id: AgentId,
        active: Option<AgentId>,
        selected: Option<AgentId>,
    },
    RepoAdded(Repo),
    /// Every intent, as the link lists them at connect: replaces the list.
    IntentsLoaded(Vec<Intent>),
    /// An intent as a command's answer has it (an event carries it too).
    /// Kept unless the model has a later copy; see [`ShellModel::intents`].
    IntentUpserted(Intent),
    /// The agent defaults, as loaded at connect or as a change answers them.
    AgentDefaultsLoaded(AgentDefaults),
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
    intents: Vec<Intent>,
    /// An `IntentsLoaded` was applied.
    intents_loaded: bool,
    agent_defaults: Option<AgentDefaults>,
    /// Counts the changes to what the shell draws from the model.
    revision: u64,
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

    /// Every intent the model has, the most recently changed first (ties
    /// broken by id). Of two copies of an intent the model keeps the one
    /// with the later `updated_at`, whatever order they arrive in.
    pub fn intents(&self) -> Vec<&Intent> {
        let mut intents: Vec<&Intent> = self.intents.iter().collect();
        intents.sort_by_key(|i| (Reverse(i.updated_at), i.id.0));
        intents
    }

    /// The intent `id`, if the model has it.
    pub fn intent(&self, id: IntentId) -> Option<&Intent> {
        self.intents.iter().find(|i| i.id == id)
    }

    /// Whether the intent list was loaded (an `IntentsLoaded` was applied
    /// since the app started); before that an empty list means nothing.
    pub fn intents_loaded(&self) -> bool {
        self.intents_loaded
    }

    /// The agent defaults, once loaded.
    pub fn agent_defaults(&self) -> Option<&AgentDefaults> {
        self.agent_defaults.as_ref()
    }

    /// Counts the changes to what the shell draws from the model: the agents,
    /// the repositories, the selection, the tabs, the connection, the intents
    /// and the agent defaults. An action that leaves it unchanged (a streamed
    /// chunk, a replayed event, an older copy of an intent, chat content the
    /// web tab shows) needs no redraw.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn apply(&mut self, action: Action) -> Vec<Effect> {
        let mut effects = Vec::new();
        let changed = match action {
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
                true
            }
            Action::Event(event) => self.apply_event(event, &mut effects),
            Action::ConnectionChanged(connection) => {
                std::mem::replace(&mut self.connection, connection) != connection
            }
            Action::SelectAgent(id) => {
                let changed = self.has_agent(id) && self.selected != Some(id);
                if changed {
                    self.selected = Some(id);
                }
                changed
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
                self.has_agent(id)
            }
            Action::CloseTab(id) => self.close_tab(id, &mut effects),
            Action::RevertOpen {
                id,
                active,
                selected,
            } => {
                self.revert_open(id, active, selected, &mut effects);
                true
            }
            Action::RepoAdded(repo) => {
                match self.repos.iter_mut().find(|r| r.id == repo.id) {
                    Some(existing) => *existing = repo,
                    None => self.repos.push(repo),
                }
                true
            }
            Action::IntentsLoaded(intents) => {
                let changed = !self.intents_loaded || self.intents != intents;
                self.intents = intents;
                self.intents_loaded = true;
                changed
            }
            Action::IntentUpserted(intent) => self.upsert_intent(intent),
            Action::AgentDefaultsLoaded(defaults) => {
                let changed = self.agent_defaults.as_ref() != Some(&defaults);
                self.agent_defaults = Some(defaults);
                changed
            }
        };
        if changed {
            self.revision += 1;
        }
        effects
    }

    /// Applies one daemon event; returns whether anything the shell draws
    /// changed.
    fn apply_event(&mut self, event: Event, effects: &mut Vec<Effect>) -> bool {
        if self.last_seq.is_some_and(|last| event.seq <= last) {
            return false;
        }
        self.last_seq = Some(event.seq);
        match event.kind {
            EventKind::AgentCreated(agent) | EventKind::AgentUpdated(agent) => {
                if agent.closed_at.is_some() {
                    return self.remove_agent(agent.id, effects);
                }
                let id = agent.id;
                match self.agents.iter_mut().find(|a| a.id == id) {
                    Some(existing) if *existing == agent => false,
                    Some(existing) => {
                        let recolored = existing.color != agent.color;
                        *existing = agent;
                        if recolored && self.tabs.contains(&id) {
                            effects.push(Effect::UpdateTabAccent(id));
                        }
                        true
                    }
                    None => {
                        self.agents.push(agent);
                        true
                    }
                }
            }
            EventKind::AgentClosed { agent_id } => self.remove_agent(agent_id, effects),
            EventKind::IntentCreated(intent) | EventKind::IntentUpdated(intent) => {
                self.upsert_intent(intent)
            }
            // Chat content belongs to the web tab.
            EventKind::MessageAppended(_)
            | EventKind::TurnDelta { .. }
            | EventKind::PluginUpdated(_)
            | EventKind::ResourceSynced { .. } => false,
        }
    }

    /// Stores `intent`, or replaces the stored copy unless that one changed
    /// later (a command's answer can land after the event of a later change,
    /// and events replay after a reconnect). Returns whether the stored copy
    /// changed: an identical copy changes nothing.
    fn upsert_intent(&mut self, intent: Intent) -> bool {
        match self.intents.iter_mut().find(|i| i.id == intent.id) {
            Some(stored) if stored.updated_at > intent.updated_at || *stored == intent => false,
            Some(stored) => {
                *stored = intent;
                true
            }
            None => {
                self.intents.push(intent);
                true
            }
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

    /// Removes agent `id` and its tab; returns whether the model had it.
    fn remove_agent(&mut self, id: AgentId, effects: &mut Vec<Effect>) -> bool {
        let had = self.has_agent(id);
        self.agents.retain(|a| a.id != id);
        self.close_tab(id, effects);
        self.fix_selection();
        had
    }

    fn fix_selection(&mut self) {
        if self.selected.is_some_and(|id| !self.has_agent(id)) {
            self.selected = None;
        }
    }

    /// Closes `id`'s tab if open; returns whether it was. When it was
    /// active, the tab before it (else the one after) becomes active.
    fn close_tab(&mut self, id: AgentId, effects: &mut Vec<Effect>) -> bool {
        let Some(index) = self.tabs.iter().position(|t| *t == id) else {
            return false;
        };
        self.tabs.remove(index);
        effects.push(Effect::CloseCenterTab(id));
        if self.active_tab == Some(id) {
            let next = self.neighbour(index);
            self.active_tab = next;
            if let Some(next) = next {
                effects.push(Effect::FocusCenterTab(next));
            }
        }
        true
    }

    /// The tab that takes over from a removed one that sat at `index`: the
    /// one before it, else the one after.
    fn neighbour(&self, index: usize) -> Option<AgentId> {
        index
            .checked_sub(1)
            .and_then(|i| self.tabs.get(i))
            .or_else(|| self.tabs.get(index))
            .copied()
    }

    /// See [`Action::RevertOpen`]. The tab that was active is focused again
    /// (one `FocusCenterTab`), else the neighbour as for a close.
    fn revert_open(
        &mut self,
        id: AgentId,
        active: Option<AgentId>,
        selected: Option<AgentId>,
        effects: &mut Vec<Effect>,
    ) {
        if let Some(index) = self.tabs.iter().position(|t| *t == id) {
            self.tabs.remove(index);
            effects.push(Effect::CloseCenterTab(id));
            if self.active_tab == Some(id) {
                let back = active.filter(|tab| self.tabs.contains(tab));
                self.active_tab = back.or_else(|| self.neighbour(index));
                effects.extend(self.active_tab.map(Effect::FocusCenterTab));
            }
        }
        self.selected = selected.filter(|agent| self.has_agent(*agent));
    }
}

use chrono::{DateTime, Utc};
use smithagents_app::model::{Action, Connection, Effect, ShellModel};
use smithagents_core::agent::{Agent, AgentState, Repo};
use smithagents_core::event::{Event, EventKind};
use smithagents_core::ids::{AgentId, RepoId};
use smithagents_core::palette::AgentColor;

fn t(min: u32) -> DateTime<Utc> {
    format!("2026-11-24T09:{min:02}:00Z").parse().unwrap()
}

fn agent(name: &str, min: u32) -> Agent {
    Agent {
        id: AgentId::new(),
        name: name.into(),
        color: AgentColor {
            l: 0.5,
            c: 0.2,
            h: 100.0,
        },
        repo_id: RepoId::new(),
        worktree_path: "/tmp/wt".into(),
        branch: format!("agent/{name}"),
        harness_session_id: None,
        state: AgentState::Idle,
        created_at: t(min),
        closed_at: None,
        intent_id: None,
    }
}

fn repo() -> Repo {
    Repo {
        id: RepoId::new(),
        path: "/tmp/r".into(),
        default_branch: "main".into(),
        created_at: t(0),
    }
}

fn ev(seq: u64, kind: EventKind) -> Action {
    Action::Event(Event {
        seq,
        at: t(30),
        kind,
    })
}

fn snapshot(agents: Vec<Agent>, seq: u64) -> Action {
    Action::Snapshot {
        agents,
        repos: vec![],
        latest_seq: seq,
    }
}

fn with(agents: &[&Agent]) -> ShellModel {
    let mut m = ShellModel::new();
    m.apply(snapshot(agents.iter().map(|a| (*a).clone()).collect(), 1));
    m
}

fn names(m: &ShellModel) -> Vec<String> {
    m.agents().iter().map(|a| a.name.clone()).collect()
}

#[test]
fn starts_empty_and_connecting() {
    let m = ShellModel::new();
    assert!(m.agents().is_empty() && m.repos().is_empty() && m.tabs().is_empty());
    assert_eq!(
        (m.selected(), m.active_tab(), m.last_seq()),
        (None, None, None)
    );
    assert_eq!(m.connection(), Connection::Connecting);
}

#[test]
fn snapshot_replaces_agents_repos_and_sets_seq() {
    let (a, b) = (agent("B", 2), agent("A", 1));
    let r = repo();
    let mut m = ShellModel::new();
    let fx = m.apply(Action::Snapshot {
        agents: vec![a, b],
        repos: vec![r.clone()],
        latest_seq: 9,
    });
    assert!(fx.is_empty());
    assert_eq!(names(&m), ["A", "B"]);
    assert_eq!(m.repos(), [r]);
    assert_eq!(m.last_seq(), Some(9));
    m.apply(snapshot(vec![agent("C", 3)], 12));
    assert_eq!(names(&m), ["C"]);
    assert!(m.repos().is_empty());
    assert_eq!(m.last_seq(), Some(12));
}

#[test]
fn snapshot_excludes_closed_agents() {
    let mut closed = agent("Old", 1);
    closed.closed_at = Some(t(5));
    let m = with(&[&closed, &agent("New", 2)]);
    assert_eq!(names(&m), ["New"]);
}

#[test]
fn snapshot_leaves_connection_alone() {
    let mut m = ShellModel::new();
    m.apply(Action::ConnectionChanged(Connection::Connected));
    m.apply(snapshot(vec![], 1));
    assert_eq!(m.connection(), Connection::Connected);
}

#[test]
fn snapshot_after_restart_drops_vanished_agents_and_tabs() {
    let (a, b) = (agent("A", 1), agent("B", 2));
    let mut m = with(&[&a, &b]);
    m.apply(Action::OpenTab(a.id));
    m.apply(Action::OpenTab(b.id));
    let fx = m.apply(snapshot(vec![a.clone()], 5));
    assert_eq!(names(&m), ["A"]);
    assert_eq!(m.tabs(), [a.id]);
    assert_eq!(m.active_tab(), Some(a.id));
    assert_eq!(m.selected(), None);
    assert_eq!(
        fx,
        [Effect::CloseCenterTab(b.id), Effect::FocusCenterTab(a.id)]
    );
}

#[test]
fn snapshot_closing_every_tab_clears_active() {
    let a = agent("A", 1);
    let mut m = with(&[&a]);
    m.apply(Action::OpenTab(a.id));
    let fx = m.apply(snapshot(vec![], 5));
    assert_eq!(fx, [Effect::CloseCenterTab(a.id)]);
    assert_eq!((m.active_tab(), m.selected()), (None, None));
}

#[test]
fn snapshot_with_now_closed_agent_closes_its_tab() {
    let a = agent("A", 1);
    let mut m = with(&[&a]);
    m.apply(Action::OpenTab(a.id));
    let mut closed = a.clone();
    closed.closed_at = Some(t(9));
    let fx = m.apply(snapshot(vec![closed], 5));
    assert_eq!(fx, [Effect::CloseCenterTab(a.id)]);
    assert!(m.agents().is_empty());
}

#[test]
fn agents_sorted_by_created_at_then_id() {
    let mut x = agent("X", 1);
    let mut y = agent("Y", 1);
    if x.id.0 > y.id.0 {
        std::mem::swap(&mut x, &mut y);
    }
    let late = agent("Late", 5);
    let m = with(&[&late, &y, &x]);
    assert_eq!(names(&m), [x.name.clone(), y.name.clone(), "Late".into()]);
}

#[test]
fn duplicate_or_older_event_is_ignored() {
    let a = agent("A", 1);
    let mut m = ShellModel::new();
    m.apply(snapshot(vec![], 5));
    for seq in [5, 3] {
        assert!(m
            .apply(ev(seq, EventKind::AgentCreated(a.clone())))
            .is_empty());
        assert!(m.agents().is_empty());
        assert_eq!(m.last_seq(), Some(5));
    }
    m.apply(ev(6, EventKind::AgentCreated(a)));
    assert_eq!(m.agents().len(), 1);
    assert_eq!(m.last_seq(), Some(6));
}

#[test]
fn first_event_without_snapshot_is_applied() {
    let mut m = ShellModel::new();
    m.apply(ev(0, EventKind::AgentCreated(agent("A", 1))));
    assert_eq!(m.agents().len(), 1);
    assert_eq!(m.last_seq(), Some(0));
}

#[test]
fn created_and_updated_upsert() {
    let a = agent("A", 1);
    let mut m = with(&[]);
    m.apply(ev(2, EventKind::AgentCreated(a.clone())));
    m.apply(ev(3, EventKind::AgentCreated(a.clone())));
    assert_eq!(m.agents().len(), 1);
    let mut renamed = a.clone();
    renamed.name = "Renamed".into();
    m.apply(ev(4, EventKind::AgentUpdated(renamed)));
    assert_eq!(names(&m), ["Renamed"]);
    // Update for an agent not yet known inserts it.
    m.apply(ev(5, EventKind::AgentUpdated(agent("Z", 2))));
    assert_eq!(m.agents().len(), 2);
}

#[test]
fn dead_agent_stays_listed_with_state_dead() {
    let a = agent("A", 1);
    let mut m = with(&[&a]);
    let mut dead = a.clone();
    dead.state = AgentState::Dead;
    m.apply(ev(2, EventKind::AgentUpdated(dead)));
    let listed = m.agents();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].state, AgentState::Dead);
}

#[test]
fn closed_agent_removed_and_its_tab_closes_next_activates() {
    let (a, b) = (agent("A", 1), agent("B", 2));
    let mut m = with(&[&a, &b]);
    m.apply(Action::OpenTab(a.id));
    m.apply(Action::OpenTab(b.id));
    m.apply(Action::SelectAgent(b.id));
    let fx = m.apply(ev(2, EventKind::AgentClosed { agent_id: b.id }));
    assert_eq!(
        fx,
        [Effect::CloseCenterTab(b.id), Effect::FocusCenterTab(a.id)]
    );
    assert_eq!(names(&m), ["A"]);
    assert_eq!(m.tabs(), [a.id]);
    assert_eq!(m.active_tab(), Some(a.id));
    assert_eq!(m.selected(), None);
}

#[test]
fn closing_the_first_active_tab_activates_the_next() {
    let (a, b) = (agent("A", 1), agent("B", 2));
    let mut m = with(&[&a, &b]);
    m.apply(Action::OpenTab(a.id));
    m.apply(Action::OpenTab(b.id));
    m.apply(Action::OpenTab(a.id));
    let fx = m.apply(ev(2, EventKind::AgentClosed { agent_id: a.id }));
    assert_eq!(
        fx,
        [Effect::CloseCenterTab(a.id), Effect::FocusCenterTab(b.id)]
    );
    assert_eq!(m.active_tab(), Some(b.id));
}

#[test]
fn closed_unknown_agent_is_harmless_but_advances_seq() {
    let mut m = with(&[]);
    let fx = m.apply(ev(
        7,
        EventKind::AgentClosed {
            agent_id: AgentId::new(),
        },
    ));
    assert!(fx.is_empty());
    assert_eq!(m.last_seq(), Some(7));
}

#[test]
fn updated_with_closed_at_acts_like_closed() {
    let a = agent("A", 1);
    let mut m = with(&[&a]);
    m.apply(Action::OpenTab(a.id));
    let mut closed = a.clone();
    closed.closed_at = Some(t(9));
    let fx = m.apply(ev(2, EventKind::AgentUpdated(closed)));
    assert_eq!(fx, [Effect::CloseCenterTab(a.id)]);
    assert!(m.agents().is_empty() && m.tabs().is_empty());
}

#[test]
fn ignored_kinds_advance_seq_only() {
    use smithagents_core::agent::{ChatMessage, Role};
    let a = agent("A", 1);
    let mut m = with(&[&a]);
    let msg = ChatMessage {
        id: smithagents_core::ids::MessageId::new(),
        agent_id: a.id,
        seq: 1,
        role: Role::User,
        text: "hi".into(),
        created_at: t(3),
    };
    assert!(m.apply(ev(2, EventKind::MessageAppended(msg))).is_empty());
    assert_eq!(m.last_seq(), Some(2));
    let delta = EventKind::TurnDelta {
        agent_id: a.id,
        text: "x".into(),
    };
    assert!(m.apply(ev(3, delta)).is_empty());
    assert_eq!(m.last_seq(), Some(3));
    assert_eq!(names(&m), ["A"]);
}

#[test]
fn accent_effect_only_when_color_changed_and_tab_open() {
    let a = agent("A", 1);
    let mut m = with(&[&a]);
    let mut recolored = a.clone();
    recolored.color.h = 200.0;
    // No tab open: no effect.
    assert!(m
        .apply(ev(2, EventKind::AgentUpdated(recolored.clone())))
        .is_empty());
    m.apply(Action::OpenTab(a.id));
    // Same color: no effect.
    assert!(m
        .apply(ev(3, EventKind::AgentUpdated(recolored.clone())))
        .is_empty());
    let mut again = recolored.clone();
    again.color.h = 300.0;
    let fx = m.apply(ev(4, EventKind::AgentUpdated(again)));
    assert_eq!(fx, [Effect::UpdateTabAccent(a.id)]);
}

#[test]
fn select_selects_without_opening() {
    let a = agent("A", 1);
    let mut m = with(&[&a]);
    assert!(m.apply(Action::SelectAgent(a.id)).is_empty());
    assert_eq!(m.selected(), Some(a.id));
    assert!(m.tabs().is_empty());
    assert_eq!(m.active_tab(), None);
}

#[test]
fn select_unknown_agent_is_a_noop() {
    let mut m = with(&[]);
    assert!(m.apply(Action::SelectAgent(AgentId::new())).is_empty());
    assert_eq!(m.selected(), None);
}

#[test]
fn open_tab_opens_selects_and_activates() {
    let (a, b) = (agent("A", 1), agent("B", 2));
    let mut m = with(&[&a, &b]);
    assert_eq!(
        m.apply(Action::OpenTab(a.id)),
        [Effect::OpenCenterTab(a.id)]
    );
    assert_eq!(
        m.apply(Action::OpenTab(b.id)),
        [Effect::OpenCenterTab(b.id)]
    );
    assert_eq!(m.tabs(), [a.id, b.id]);
    assert_eq!((m.selected(), m.active_tab()), (Some(b.id), Some(b.id)));
}

#[test]
fn opening_same_agent_twice_yields_one_tab_and_focus() {
    let (a, b) = (agent("A", 1), agent("B", 2));
    let mut m = with(&[&a, &b]);
    m.apply(Action::OpenTab(a.id));
    m.apply(Action::OpenTab(b.id));
    let fx = m.apply(Action::OpenTab(a.id));
    assert_eq!(fx, [Effect::FocusCenterTab(a.id)]);
    assert_eq!(m.tabs(), [a.id, b.id]);
    assert_eq!((m.selected(), m.active_tab()), (Some(a.id), Some(a.id)));
}

#[test]
fn open_tab_for_unknown_or_closed_agent_is_a_noop() {
    let mut m = with(&[]);
    assert!(m.apply(Action::OpenTab(AgentId::new())).is_empty());
    let a = agent("A", 1);
    m.apply(ev(2, EventKind::AgentCreated(a.clone())));
    m.apply(ev(3, EventKind::AgentClosed { agent_id: a.id }));
    assert!(m.apply(Action::OpenTab(a.id)).is_empty());
    assert!(m.tabs().is_empty() && m.selected().is_none());
}

#[test]
fn closing_active_tab_activates_previous() {
    let (a, b, c) = (agent("A", 1), agent("B", 2), agent("C", 3));
    let mut m = with(&[&a, &b, &c]);
    for x in [&a, &b, &c] {
        m.apply(Action::OpenTab(x.id));
    }
    m.apply(Action::OpenTab(b.id));
    let fx = m.apply(Action::CloseTab(b.id));
    assert_eq!(
        fx,
        [Effect::CloseCenterTab(b.id), Effect::FocusCenterTab(a.id)]
    );
    assert_eq!(
        (m.tabs(), m.active_tab()),
        ([a.id, c.id].as_slice(), Some(a.id))
    );
}

#[test]
fn closing_inactive_tab_keeps_active() {
    let (a, b) = (agent("A", 1), agent("B", 2));
    let mut m = with(&[&a, &b]);
    m.apply(Action::OpenTab(a.id));
    m.apply(Action::OpenTab(b.id));
    let fx = m.apply(Action::CloseTab(a.id));
    assert_eq!(fx, [Effect::CloseCenterTab(a.id)]);
    assert_eq!(m.active_tab(), Some(b.id));
}

#[test]
fn closing_last_tab_leaves_no_active_and_keeps_agent() {
    let a = agent("A", 1);
    let mut m = with(&[&a]);
    m.apply(Action::OpenTab(a.id));
    let fx = m.apply(Action::CloseTab(a.id));
    assert_eq!(fx, [Effect::CloseCenterTab(a.id)]);
    assert_eq!(m.active_tab(), None);
    assert_eq!(m.agents().len(), 1);
}

#[test]
fn closing_a_tab_that_is_not_open_is_a_noop() {
    let a = agent("A", 1);
    let mut m = with(&[&a]);
    assert!(m.apply(Action::CloseTab(a.id)).is_empty());
}

#[test]
fn connection_changes_are_recorded() {
    let mut m = ShellModel::new();
    assert!(m
        .apply(Action::ConnectionChanged(Connection::Connected))
        .is_empty());
    assert_eq!(m.connection(), Connection::Connected);
    m.apply(Action::ConnectionChanged(Connection::Reconnecting));
    assert_eq!(m.connection(), Connection::Reconnecting);
}

#[test]
fn repo_added_upserts_by_id() {
    let mut m = ShellModel::new();
    let r = repo();
    m.apply(Action::RepoAdded(r.clone()));
    m.apply(Action::RepoAdded(r.clone()));
    assert_eq!(m.repos().len(), 1);
    let mut changed = r.clone();
    changed.default_branch = "trunk".into();
    m.apply(Action::RepoAdded(changed));
    assert_eq!(m.repos().len(), 1);
    assert_eq!(m.repos()[0].default_branch, "trunk");
    m.apply(Action::RepoAdded(repo()));
    assert_eq!(m.repos().len(), 2);
}

//! The start queue for sent intents: a `Queued` intent starts once every
//! dependency it accepted is `Resolved` and a slot under the queue limit is
//! free, oldest `queued_at` first.
//!
//! One background task ([`Scheduler`]) makes those decisions in passes. A pass
//! runs at startup, on [`SchedulerHandle::kick`] (or a [`Kicker`] clone), and
//! after `AgentUpdated` bus events, since an agent leaving `Working`/`Quiet`
//! frees a slot, and `AgentClosed` events, since closing an agent left
//! attached to an intent lets that intent start. Triggers that arrive while a
//! pass runs coalesce into one more pass after it, so none is lost and passes
//! never overlap. Each pass also holds the intent service's start lock, which
//! `IntentService::send` takes too, so the limit is checked and used
//! atomically by both. A pass that panics is logged and the scheduler keeps
//! running.
//!
//! The slot rule (spec: "Agents working at once"): `running` counts every
//! agent with a turn in flight (`Working` or `Quiet`), whatever started it:
//! an intent (`Sent`, or already `Resolved` while its agent still works) or
//! the user directly. Only intent starts wait for a slot; an agent the user
//! messages directly is never held back, but takes a slot while it replies.

use std::collections::HashSet;
use std::panic::AssertUnwindSafe;

use futures::FutureExt;
use smithagents_core::event::{Event, EventKind};
use smithagents_core::ids::IntentId;
use smithagents_core::intent::{Intent, IntentFilter, IntentState, ItemStatus};
use tokio::sync::broadcast::{self, error::RecvError, error::TryRecvError};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::events::EventBus;
use crate::intents::{panic_text, IntentError, IntentService};
use crate::service::AgentService;
use crate::store::{Store, StoreError};

/// The ids of the `Queued` intents to start now, oldest `queued_at` first: those
/// whose accepted dependencies are all in `resolved`, at most `limit - running`
/// of them. `limit` `None` (or `Some(0)`) means no limit. Intents in other
/// states are ignored.
pub fn next_startable(
    queued: &[Intent],
    resolved: &HashSet<IntentId>,
    running: usize,
    limit: Option<u32>,
) -> Vec<IntentId> {
    let free = match limit.filter(|&n| n > 0) {
        Some(n) => usize::try_from(n)
            .unwrap_or(usize::MAX)
            .saturating_sub(running),
        None => usize::MAX,
    };
    let mut ready: Vec<&Intent> = queued
        .iter()
        .filter(|i| i.state == IntentState::Queued && dependencies_resolved(i, resolved))
        .collect();
    ready.sort_by_key(|i| (i.queued_at, i.id.0));
    ready.into_iter().take(free).map(|i| i.id).collect()
}

/// Every accepted depends-on item points at a resolved intent. Dropped items
/// are ignored (pending ones cannot exist once an intent is complete), and so
/// is an item without a target, which review never lets anyone accept.
pub fn dependencies_resolved(intent: &Intent, resolved: &HashSet<IntentId>) -> bool {
    intent
        .content
        .depends_on
        .iter()
        .filter(|d| d.status == ItemStatus::Accepted)
        .filter_map(|d| d.intent_ref)
        .all(|target| resolved.contains(&target))
}

/// Ids of every `Resolved` intent.
pub(crate) async fn resolved_ids(store: &Store) -> Result<HashSet<IntentId>, StoreError> {
    let resolved = store
        .list_intents(IntentFilter {
            state: Some(IntentState::Resolved),
            repo_id: None,
        })
        .await?;
    Ok(resolved.into_iter().map(|i| i.id).collect())
}

/// Agents with a turn in flight, intent-started or not. An agent counts from
/// the moment its message is accepted (before the turn's task records
/// `Working`) until the turn's final state is recorded, so a start is counted
/// at once and the slot frees exactly when the turn ends.
pub(crate) fn running_count(agents: &AgentService) -> usize {
    agents.running_agent_count()
}

enum Msg {
    Kick,
    /// Run a pass and report when it has finished.
    Pass(oneshot::Sender<()>),
    Shutdown,
}

/// Wakes the scheduler for a pass; cheap to clone, never blocks. Kicks that
/// arrive while a pass runs coalesce into one more pass.
#[derive(Clone)]
pub struct Kicker {
    tx: mpsc::UnboundedSender<Msg>,
}

impl Kicker {
    pub fn kick(&self) {
        // An error only means the scheduler has stopped.
        let _ = self.tx.send(Msg::Kick);
    }
}

/// Owns the scheduler task: dropping it stops the task, as does
/// [`Self::shutdown`], which also waits for a pass in progress to finish.
pub struct SchedulerHandle {
    kicker: Kicker,
    task: Option<JoinHandle<()>>,
}

impl SchedulerHandle {
    pub fn kick(&self) {
        self.kicker.kick();
    }

    pub fn kicker(&self) -> Kicker {
        self.kicker.clone()
    }

    /// Runs a pass after any in progress, and returns once it has finished.
    pub async fn run_pass(&self) {
        let (tx, rx) = oneshot::channel();
        if self.kicker.tx.send(Msg::Pass(tx)).is_ok() {
            // An error only means the scheduler stopped meanwhile.
            let _ = rx.await;
        }
    }

    /// Stops the task once any pass in progress has finished.
    pub async fn shutdown(mut self) {
        let _ = self.kicker.tx.send(Msg::Shutdown);
        if let Some(task) = self.task.take() {
            if let Err(e) = task.await {
                if e.is_panic() {
                    tracing::error!("queue scheduler panicked: {e}");
                }
            }
        }
    }
}

impl Drop for SchedulerHandle {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            // A start in progress runs on its own task and is not cut short.
            task.abort();
        }
    }
}

/// What woke the scheduler.
enum Wake {
    Msg(Option<Msg>),
    AgentChanged,
    BusClosed,
}

pub struct Scheduler {
    intents: IntentService,
    agents: AgentService,
    store: Store,
}

impl Scheduler {
    /// Spawns the scheduler task, which runs a first pass at once, and
    /// connects `intents` to it so that sending, resolving and changing the
    /// queue limit wake it.
    pub fn start(
        intents: IntentService,
        agents: AgentService,
        store: Store,
        bus: EventBus,
    ) -> SchedulerHandle {
        let (tx, rx) = mpsc::unbounded_channel();
        let kicker = Kicker { tx };
        intents.set_scheduler(kicker.clone());
        // Subscribed before the first pass, so no agent update after it is missed.
        let events = match bus.subscribe(None) {
            Ok(sub) => Some(sub.live),
            Err(e) => {
                tracing::error!("queue scheduler cannot follow agent updates: {e}");
                None
            }
        };
        let scheduler = Scheduler {
            intents,
            agents,
            store,
        };
        let task = tokio::spawn(scheduler.run(rx, events));
        SchedulerHandle {
            kicker,
            task: Some(task),
        }
    }

    async fn run(
        self,
        mut rx: mpsc::UnboundedReceiver<Msg>,
        mut events: Option<broadcast::Receiver<Event>>,
    ) {
        self.pass().await;
        let mut waiters: Vec<oneshot::Sender<()>> = Vec::new();
        loop {
            let wake = tokio::select! {
                msg = rx.recv() => Wake::Msg(msg),
                wake = next_agent_update(events.as_mut()) => wake,
            };
            match wake {
                // Every sender gone, or asked to stop.
                Wake::Msg(None | Some(Msg::Shutdown)) => return,
                Wake::Msg(Some(Msg::Pass(waiter))) => waiters.push(waiter),
                Wake::Msg(Some(Msg::Kick)) | Wake::AgentChanged => {}
                Wake::BusClosed => {
                    events = None;
                    continue;
                }
            }
            // Fold everything already waiting into this one pass.
            loop {
                match rx.try_recv() {
                    Ok(Msg::Kick) => {}
                    Ok(Msg::Pass(waiter)) => waiters.push(waiter),
                    Ok(Msg::Shutdown) => return,
                    Err(_) => break,
                }
            }
            if let Some(live) = events.as_mut() {
                loop {
                    match live.try_recv() {
                        Ok(_) | Err(TryRecvError::Lagged(_)) => {}
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Closed) => {
                            events = None;
                            break;
                        }
                    }
                }
            }
            self.pass().await;
            for waiter in waiters.drain(..) {
                let _ = waiter.send(());
            }
        }
    }

    /// One pass; errors are logged, and the next trigger tries again. A panic
    /// (in a start, say) is caught and logged too, so it cannot end the
    /// scheduler task; the locks the pass held are released as it unwinds.
    async fn pass(&self) {
        let _starts = self.intents.lock_starts().await;
        match AssertUnwindSafe(self.start_ready()).catch_unwind().await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("queue pass failed: {e}"),
            Err(panic) => tracing::error!("queue pass panicked: {}", panic_text(&*panic)),
        }
    }

    async fn start_ready(&self) -> Result<(), IntentError> {
        let mut queued = self.store.queued_intents().await?;
        if queued.is_empty() {
            return Ok(());
        }
        let resolved = resolved_ids(&self.store).await?;
        let limit = self.intents.agent_defaults().await?.queue_limit;
        loop {
            // Counted afresh for each start: the agent just started holds its
            // slot already, and any turn that ended meanwhile has freed one.
            let running = running_count(&self.agents);
            let Some(id) = next_startable(&queued, &resolved, running, limit)
                .into_iter()
                .next()
            else {
                return Ok(());
            };
            queued.retain(|i| i.id != id);
            if let Err(e) = self.intents.start(id).await {
                // `start` kept it queued and recorded the error; the next
                // candidate may still take the slot.
                tracing::warn!(intent_id = %id, "could not start a queued intent: {e}");
            }
        }
    }
}

/// Waits for an `AgentUpdated` or `AgentClosed` event (or a lag, which may
/// have hidden one).
async fn next_agent_update(events: Option<&mut broadcast::Receiver<Event>>) -> Wake {
    let Some(live) = events else {
        return std::future::pending().await;
    };
    loop {
        match live.recv().await {
            Ok(Event {
                kind: EventKind::AgentUpdated(_) | EventKind::AgentClosed { .. },
                ..
            }) => return Wake::AgentChanged,
            Ok(_) => {}
            Err(RecvError::Lagged(_)) => return Wake::AgentChanged,
            Err(RecvError::Closed) => return Wake::BusClosed,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta, Utc};
    use smithagents_core::ids::{ItemId, RepoId};
    use smithagents_core::intent::{Content, IntentKind, Item, Phase, Seed, SeedSource};

    use super::*;

    fn t0() -> DateTime<Utc> {
        "2026-11-24T10:00:00Z".parse().unwrap()
    }

    fn dep(target: IntentId, status: ItemStatus) -> Item {
        Item {
            id: ItemId::new(),
            text: "dep".into(),
            status,
            edited: false,
            intent_ref: Some(target),
            fixed: false,
        }
    }

    /// A queued intent, queued `secs` after `t0`.
    fn queued(secs: i64, deps: Vec<Item>) -> Intent {
        Intent {
            id: IntentId::new(),
            repo_id: RepoId::new(),
            slug: format!("q{secs}"),
            state: IntentState::Queued,
            phase: Phase::Review,
            seed: Seed {
                kind: None,
                source: SeedSource::Text,
                text: "s".into(),
            },
            content: Content {
                title: "T".into(),
                kind: IntentKind::Task,
                current_behavior: String::new(),
                questions: vec![],
                preconditions: vec![],
                criteria: vec![],
                stories: vec![],
                depends_on: deps,
            },
            agent_id: None,
            created_at: t0(),
            updated_at: t0(),
            queued_at: Some(t0() + TimeDelta::seconds(secs)),
            sent_at: None,
            resolved_at: None,
            last_start_error: None,
            base_branch: None,
            origin: None,
        }
    }

    fn ids(intents: &[&Intent]) -> Vec<IntentId> {
        intents.iter().map(|i| i.id).collect()
    }

    #[test]
    fn oldest_ready_first_up_to_the_free_slots() {
        let (a, b, c) = (queued(3, vec![]), queued(1, vec![]), queued(2, vec![]));
        let all = [a.clone(), b.clone(), c.clone()];
        let none = HashSet::new();
        assert_eq!(next_startable(&all, &none, 0, None), ids(&[&b, &c, &a]));
        assert_eq!(next_startable(&all, &none, 0, Some(2)), ids(&[&b, &c]));
        assert_eq!(next_startable(&all, &none, 1, Some(2)), ids(&[&b]));
        assert!(next_startable(&all, &none, 2, Some(2)).is_empty());
        assert!(next_startable(&all, &none, 5, Some(2)).is_empty());
        // Some(0) is no limit, like None.
        assert_eq!(next_startable(&all, &none, 9, Some(0)).len(), 3);
        assert!(next_startable(&[], &none, 0, None).is_empty());
    }

    #[test]
    fn only_accepted_dependencies_must_be_resolved() {
        let (done, open) = (IntentId::new(), IntentId::new());
        let resolved: HashSet<IntentId> = [done].into();
        let waits = queued(
            1,
            vec![
                dep(done, ItemStatus::Accepted),
                dep(open, ItemStatus::Accepted),
            ],
        );
        let ready = queued(2, vec![dep(done, ItemStatus::Accepted)]);
        let dropped = queued(3, vec![dep(open, ItemStatus::Dropped)]);
        let all = [waits.clone(), ready.clone(), dropped.clone()];
        assert_eq!(
            next_startable(&all, &resolved, 0, None),
            ids(&[&ready, &dropped])
        );
        // A waiting intent never takes a slot from a ready one behind it.
        assert_eq!(next_startable(&all, &resolved, 0, Some(1)), ids(&[&ready]));
        let both: HashSet<IntentId> = [done, open].into();
        assert_eq!(next_startable(&all, &both, 0, None).len(), 3);
    }

    #[test]
    fn intents_not_queued_are_ignored() {
        let mut sent = queued(1, vec![]);
        sent.state = IntentState::Sent;
        let q = queued(2, vec![]);
        assert_eq!(
            next_startable(&[sent, q.clone()], &HashSet::new(), 0, None),
            ids(&[&q])
        );
    }

    #[test]
    fn equal_queue_times_keep_a_stable_order() {
        let a = queued(1, vec![]);
        let mut b = queued(1, vec![]);
        b.queued_at = a.queued_at;
        let first = next_startable(&[a.clone(), b.clone()], &HashSet::new(), 0, None);
        let second = next_startable(&[b, a], &HashSet::new(), 0, None);
        assert_eq!(first, second);
    }
}

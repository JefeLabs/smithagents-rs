//! Resumable in-process event bus: a bounded replay buffer plus live broadcast.
//!
//! `publish` and `subscribe` both run under one mutex, so a subscriber's backlog
//! snapshot and its live receiver are taken atomically with respect to publishing:
//! every event is in exactly one of the two.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::Utc;
use smithagents_core::event::{Event, EventKind};
use tokio::sync::broadcast;

/// Errors from [`EventBus::subscribe`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BusError {
    /// Events between the requested position and the oldest retained one were evicted.
    #[error("requested position is older than the oldest retained event ({oldest_retained})")]
    TooOld { oldest_retained: u64 },
}

/// A resumed view of the bus: replayed `backlog`, then the `live` stream.
///
/// `live` is a bounded broadcast receiver. A consumer that falls more than the
/// bus capacity behind gets `RecvError::Lagged` and MUST handle it, by dropping
/// the subscription and resubscribing with `since = last seen seq`. Slow
/// consumers never block the publisher.
pub struct Subscription {
    /// Retained events with `seq > since`, oldest first.
    pub backlog: Vec<Event>,
    /// Events published after the backlog snapshot; contiguous with `backlog`.
    pub live: broadcast::Receiver<Event>,
}

struct Inner {
    capacity: usize,
    last_seq: u64,
    ring: VecDeque<Event>,
    tx: broadcast::Sender<Event>,
}

/// Cheaply cloneable handle to a shared event bus.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Mutex<Inner>>,
}

impl EventBus {
    /// Creates a bus retaining the last `capacity` events (minimum 1). The live
    /// broadcast channel has the same capacity.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (tx, _) = broadcast::channel(capacity);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                capacity,
                last_seq: 0,
                ring: VecDeque::with_capacity(capacity),
                tx,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // No code under the lock can leave `Inner` inconsistent on panic.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Assigns the next sequence number (starting at 1), stamps the time, retains
    /// the event (evicting the oldest when full) and broadcasts it.
    pub fn publish(&self, kind: EventKind) -> Event {
        let mut inner = self.lock();
        inner.last_seq += 1;
        let event = Event {
            seq: inner.last_seq,
            at: Utc::now(),
            kind,
        };
        if inner.ring.len() == inner.capacity {
            inner.ring.pop_front();
        }
        inner.ring.push_back(event.clone());
        // Sending while locked keeps backlog/live atomic. An error only means
        // there are no receivers.
        let _ = inner.tx.send(event.clone());
        event
    }

    /// Subscribes, optionally resuming after `since`.
    ///
    /// `None` gives live events only. `Some(n)` gives retained events with
    /// `seq > n`; `n` at or beyond the latest seq (including `Some(0)` on an
    /// empty bus) yields an empty backlog. Fails with [`BusError::TooOld`] when
    /// an event in `(n, oldest_retained)` has been evicted.
    pub fn subscribe(&self, since: Option<u64>) -> Result<Subscription, BusError> {
        let inner = self.lock();
        let backlog = match since {
            None => Vec::new(),
            Some(n) => {
                if let Some(oldest) = inner.ring.front().map(|e| e.seq) {
                    if n.saturating_add(1) < oldest {
                        return Err(BusError::TooOld {
                            oldest_retained: oldest,
                        });
                    }
                }
                inner.ring.iter().filter(|e| e.seq > n).cloned().collect()
            }
        };
        Ok(Subscription {
            backlog,
            live: inner.tx.subscribe(),
        })
    }

    /// Sequence number of the latest published event, or 0 if none.
    pub fn latest_seq(&self) -> u64 {
        self.lock().last_seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithagents_core::ids::AgentId;
    use std::time::{Duration, Instant};

    fn kind(n: usize) -> EventKind {
        EventKind::TurnDelta {
            agent_id: AgentId::new(),
            text: n.to_string(),
        }
    }

    fn seqs(events: &[Event]) -> Vec<u64> {
        events.iter().map(|e| e.seq).collect()
    }

    fn bus_with(capacity: usize, n: usize) -> EventBus {
        let bus = EventBus::new(capacity);
        for i in 0..n {
            bus.publish(kind(i));
        }
        bus
    }

    #[tokio::test]
    async fn seq_is_monotonic_from_one() {
        let bus = EventBus::new(8);
        assert_eq!(bus.latest_seq(), 0);
        let got: Vec<u64> = (0..3).map(|i| bus.publish(kind(i)).seq).collect();
        assert_eq!(got, vec![1, 2, 3]);
        assert_eq!(bus.latest_seq(), 3);
    }

    #[tokio::test]
    async fn publish_stamps_time() {
        let bus = EventBus::new(8);
        let before = Utc::now();
        let ev = bus.publish(kind(0));
        assert!(ev.at >= before && ev.at <= Utc::now());
    }

    #[tokio::test]
    async fn subscribe_since_replays_backlog_then_live() {
        let bus = bus_with(8, 3);
        let mut sub = bus.subscribe(Some(1)).unwrap();
        assert_eq!(seqs(&sub.backlog), vec![2, 3]);
        let published = bus.publish(kind(3));
        assert_eq!(published.seq, 4);
        let live = sub.live.recv().await.unwrap();
        assert_eq!(live, published);
    }

    #[tokio::test]
    async fn subscribe_none_is_live_only() {
        let bus = bus_with(8, 3);
        let mut sub = bus.subscribe(None).unwrap();
        assert!(sub.backlog.is_empty());
        assert_eq!(bus.publish(kind(3)).seq, 4);
        assert_eq!(sub.live.recv().await.unwrap().seq, 4);
    }

    #[tokio::test]
    async fn subscribe_since_too_old_errors() {
        let bus = bus_with(2, 5);
        let err = bus.subscribe(Some(1)).err().unwrap();
        assert!(matches!(err, BusError::TooOld { oldest_retained: 4 }));
    }

    #[tokio::test]
    async fn since_just_before_oldest_retained_is_valid() {
        // Retained: 4, 5. since=3 means "everything after 3", which is fully retained.
        let bus = bus_with(2, 5);
        let sub = bus.subscribe(Some(3)).unwrap();
        assert_eq!(seqs(&sub.backlog), vec![4, 5]);
        // since=2 would need event 3, which was evicted.
        assert!(matches!(
            bus.subscribe(Some(2)).err().unwrap(),
            BusError::TooOld { oldest_retained: 4 }
        ));
    }

    #[tokio::test]
    async fn since_equal_to_latest_gives_empty_backlog() {
        let bus = bus_with(8, 3);
        let sub = bus.subscribe(Some(3)).unwrap();
        assert!(sub.backlog.is_empty());
    }

    #[tokio::test]
    async fn since_greater_than_latest_gives_empty_backlog() {
        let bus = bus_with(8, 3);
        let mut sub = bus.subscribe(Some(100)).unwrap();
        assert!(sub.backlog.is_empty());
        bus.publish(kind(3));
        assert_eq!(sub.live.recv().await.unwrap().seq, 4);
    }

    #[tokio::test]
    async fn since_zero_on_empty_bus_is_valid() {
        let bus = EventBus::new(4);
        let mut sub = bus.subscribe(Some(0)).unwrap();
        assert!(sub.backlog.is_empty());
        bus.publish(kind(0));
        assert_eq!(sub.live.recv().await.unwrap().seq, 1);
    }

    #[tokio::test]
    async fn since_zero_before_any_eviction_replays_everything() {
        let bus = bus_with(4, 3);
        let sub = bus.subscribe(Some(0)).unwrap();
        assert_eq!(seqs(&sub.backlog), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn capacity_exactly_full_evicts_nothing() {
        let bus = bus_with(3, 3);
        let sub = bus.subscribe(Some(0)).unwrap();
        assert_eq!(seqs(&sub.backlog), vec![1, 2, 3]);
        // One more publish evicts seq 1; since=0 is now too old.
        bus.publish(kind(3));
        assert!(matches!(
            bus.subscribe(Some(0)).err().unwrap(),
            BusError::TooOld { oldest_retained: 2 }
        ));
        assert_eq!(
            seqs(&bus.subscribe(Some(1)).unwrap().backlog),
            vec![2, 3, 4]
        );
    }

    #[tokio::test]
    async fn zero_capacity_is_clamped_to_one() {
        let bus = bus_with(0, 2);
        let sub = bus.subscribe(Some(1)).unwrap();
        assert_eq!(seqs(&sub.backlog), vec![2]);
    }

    #[tokio::test]
    async fn clones_share_state() {
        let bus = EventBus::new(4);
        let other = bus.clone();
        other.publish(kind(0));
        assert_eq!(bus.latest_seq(), 1);
    }

    #[tokio::test]
    async fn slow_subscriber_does_not_block_publisher() {
        let bus = EventBus::new(16);
        let mut sub = bus.subscribe(None).unwrap();
        let start = Instant::now();
        for i in 0..10_000 {
            bus.publish(kind(i));
        }
        assert!(start.elapsed() < Duration::from_secs(1));
        // The never-polled receiver has lagged and is told so.
        assert!(matches!(
            sub.live.recv().await,
            Err(broadcast::error::RecvError::Lagged(_))
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn no_gap_between_backlog_and_live_under_concurrent_publish() {
        const TOTAL: u64 = 5_000;
        // Ring and broadcast both hold everything, so nothing is evicted or lagged.
        let bus = EventBus::new(TOTAL as usize);
        let publisher = {
            let bus = bus.clone();
            tokio::spawn(async move {
                for i in 0..TOTAL as usize {
                    bus.publish(kind(i));
                    tokio::task::yield_now().await;
                }
            })
        };
        // Start subscribing once the stream is underway, then keep subscribing
        // while the publisher is still running.
        while bus.latest_seq() < 100 {
            tokio::task::yield_now().await;
        }
        let mut subscribers = Vec::new();
        for _ in 0..20 {
            subscribers.push(bus.subscribe(Some(0)).unwrap());
            tokio::task::yield_now().await;
        }
        publisher.await.unwrap();
        assert!(
            subscribers[0].backlog.len() >= 100 && subscribers[0].backlog.len() < TOTAL as usize,
            "first subscription did not overlap the publisher"
        );

        for mut sub in subscribers {
            let mut all = seqs(&sub.backlog);
            while let Ok(ev) = sub.live.try_recv() {
                all.push(ev.seq);
            }
            let expected: Vec<u64> = (1..=TOTAL).collect();
            assert_eq!(all, expected, "gap or duplicate between backlog and live");
        }
    }
}

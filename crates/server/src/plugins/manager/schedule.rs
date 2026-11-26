//! When runs start: a pass starts every run that is due, at most one per
//! plugin; the scheduler task runs a pass whenever it wakes, and sleeps until
//! the earliest due time or until something changes.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};

use tokio::sync::Notify;
use tokio::time::Instant;

use super::{run, Inner, Live, Managed, Problem, Running};
use crate::intents::panic_text;

/// Whether the plugin can start a run now: loaded and allowed to run,
/// enabled, not stopped, and not running one already.
fn can_start(plugin: &Managed, live: &Live) -> bool {
    plugin.can_run() && live.enabled && !live.is_failed() && live.running.is_none()
}

/// Held plugins run only what `sync_now` asked for.
fn held(live: &Live) -> bool {
    live.settings_error.is_some()
        || match &live.problem {
            None => false,
            Some(
                Problem::NeedsAuth { .. }
                | Problem::NeedsSettings { .. }
                | Problem::NeedsSetup { .. }
                | Problem::Failed { .. },
            ) => true,
        }
}

/// The resource to run next, if any is due: a requested one first, then the
/// most overdue, then manifest order.
fn next_resource(live: &Live, now: Instant) -> Option<usize> {
    let held = held(live);
    live.resources
        .iter()
        .enumerate()
        .filter(|(_, r)| r.forced || (!held && r.due <= now))
        .min_by_key(|(_, r)| (!r.forced, r.due))
        .map(|(i, _)| i)
}

/// Starts every run that is due now. Returns how many it started.
pub(super) fn pass(inner: &Arc<Inner>) -> usize {
    if inner.stopping.load(Ordering::SeqCst) {
        return 0;
    }
    let now = Instant::now();
    let mut started = 0;
    for plugin in inner.plugins.values() {
        let (kind, token) = {
            let mut live = plugin.live();
            if !can_start(plugin, &live) {
                continue;
            }
            let Some(index) = next_resource(&live, now) else {
                continue;
            };
            let token = inner.next_token.fetch_add(1, Ordering::Relaxed);
            let resource = &mut live.resources[index];
            resource.forced = false;
            let kind = resource.kind.clone();
            live.running = Some(Running {
                kind: kind.clone(),
                token,
            });
            (kind, token)
        };
        inner.active.send_modify(|n| *n += 1);
        tokio::spawn(run::run(Arc::clone(inner), plugin.id.clone(), kind, token));
        started += 1;
    }
    started
}

/// When the scheduler must look again: the earliest due time of a resource
/// that could start, or never.
fn next_wake(inner: &Inner) -> Option<Instant> {
    let now = Instant::now();
    inner
        .plugins
        .values()
        .filter_map(|plugin| {
            let live = plugin.live();
            if !can_start(plugin, &live) {
                return None;
            }
            let held = held(&live);
            live.resources
                .iter()
                .filter_map(|r| match (r.forced, held) {
                    (true, _) => Some(now),
                    (false, false) => Some(r.due),
                    (false, true) => None,
                })
                .min()
        })
        .min()
}

/// The scheduler task. It holds the manager weakly: once every handle is
/// gone (or on shutdown) it ends.
pub(super) async fn scheduler(inner: Weak<Inner>, wake: Arc<Notify>) {
    loop {
        let next = {
            let Some(inner) = inner.upgrade() else {
                return;
            };
            if inner.stopping.load(Ordering::SeqCst) {
                return;
            }
            // A panic here is a bug; the scheduler keeps going regardless.
            if let Err(panic) = catch_unwind(AssertUnwindSafe(|| pass(&inner))) {
                tracing::error!("a plugin scheduling pass panicked: {}", panic_text(&*panic));
            }
            next_wake(&inner)
        };
        let sleep = async {
            match next {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = sleep => {}
            () = wake.notified() => {}
        }
    }
}

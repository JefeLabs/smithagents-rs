//! One run of one resource: check the settings and secrets, fetch pages until
//! the plugin is done or the page budget is spent, then record the outcome.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::FutureExt;
use serde_json::Value;
use smithagents_core::event::EventKind;
use smithagents_plugin_host::{CallError, CallPurpose};
use smithagents_plugin_protocol::{HostMode, PluginError, SyncPage, SyncRequest};
use tokio::time::Instant;

use super::{clean_message, later, wall_after, Inner, Listing, Loaded, Managed, Problem};
use crate::intents::panic_text;
use crate::plugins::settings;

const NO_PROGRESS: &str = "the plugin did not advance its cursor";

/// Ends the run's bookkeeping however the run ends: frees the plugin, counts
/// the run out and wakes the scheduler.
struct RunGuard<'a> {
    inner: &'a Inner,
    plugin: Option<&'a Managed>,
    token: u64,
}

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        if let Some(plugin) = self.plugin {
            plugin.end_run(self.token);
        }
        self.inner.active.send_modify(|n| *n = n.saturating_sub(1));
        self.inner.wake.notify_one();
    }
}

/// What a run has done so far; kept outside the part that may panic.
struct Progress {
    /// The plugin's generation when the run began.
    generation: u64,
    /// Set when the first call is made.
    started_at: Option<DateTime<Utc>>,
    upserted: u32,
    deleted: u32,
}

/// How a run ended.
enum End {
    /// The plugin said `done`, or the page budget was spent.
    Finished,
    /// The plugin was disabled, given new settings or shut down between pages.
    Interrupted,
    Call(CallError),
    NoProgress,
    /// The store refused a page; the message is the host's own.
    Store(String),
    Panicked(String),
}

/// The run of `kind` that the scheduler marked with `token`.
pub(super) async fn run(inner: Arc<Inner>, id: String, kind: String, token: u64) {
    let plugin = inner.plugins.get(&id);
    let _guard = RunGuard {
        inner: &inner,
        plugin,
        token,
    };
    let Some(plugin) = plugin else {
        return;
    };
    let mut progress = Progress {
        generation: plugin.live().generation,
        started_at: None,
        upserted: 0,
        deleted: 0,
    };
    let body = AssertUnwindSafe(body(&inner, plugin, &kind, token, &mut progress));
    if let Err(panic) = body.catch_unwind().await {
        let end = End::Panicked(format!("the sync panicked: {}", panic_text(&*panic)));
        finish(&inner, plugin, &kind, token, &progress, end).await;
    }
}

async fn body(inner: &Inner, plugin: &Managed, kind: &str, token: u64, progress: &mut Progress) {
    // A slot first: at most `max_concurrent` calls run at once. The
    // semaphore is closed at shutdown.
    let Ok(_permit) = Arc::clone(&inner.permits).acquire_owned().await else {
        return;
    };
    let Some(call) = ready(inner, plugin, kind, progress.generation).await else {
        return;
    };
    progress.started_at = Some(inner.now());
    inner.publish_status(plugin).await;
    let end = fetch(inner, plugin, kind, &call, progress).await;
    finish(inner, plugin, kind, token, progress, end).await;
}

/// What a run calls the plugin with.
struct Call<'a> {
    loaded: &'a Loaded,
    settings: Value,
    cursor: Option<String>,
    listing: Listing,
    generation: u64,
}

/// Checks that the run may call the plugin. Settings that do not validate
/// and missing secrets stop it before any call (the plugin would only see a
/// missing secret as an HTTP error).
async fn ready<'a>(
    inner: &Inner,
    plugin: &'a Managed,
    kind: &str,
    generation: u64,
) -> Option<Call<'a>> {
    let loaded = plugin.loaded.as_ref().ok()?;
    let (stored, cursor, listing) = {
        let live = plugin.live();
        if inner.stopping.load(Ordering::SeqCst) || !live.enabled || live.is_failed() {
            return None;
        }
        let r = live.resource(kind)?;
        (
            live.settings.clone(),
            r.sync.cursor.clone(),
            r.listing.clone(),
        )
    };
    let Ok(settings) = settings::effective(&plugin.manifest, &stored) else {
        // The status already says NeedsSettings; a `sync_now` hears it here.
        inner.publish_status(plugin).await;
        return None;
    };
    let missing = missing_secrets(inner, plugin).await;
    if !missing.is_empty() {
        let message = match missing.as_slice() {
            [key] => format!("no value is configured for the secret {key}"),
            keys => format!("no value is configured for the secrets {}", keys.join(", ")),
        };
        tracing::warn!(plugin = %plugin.id, "{message}");
        {
            let mut live = plugin.live();
            if live.generation == generation {
                live.problem = Some(Problem::NeedsAuth { missing, message });
            }
        }
        inner.publish_status(plugin).await;
        return None;
    }
    Some(Call {
        loaded,
        settings,
        cursor,
        listing,
        generation,
    })
}

/// The declared secrets nothing provides. A source error counts as missing;
/// it is logged, never shown.
async fn missing_secrets(inner: &Inner, plugin: &Managed) -> Vec<String> {
    let mut missing = Vec::new();
    for decl in &plugin.manifest.secrets {
        match inner.secrets.get(&plugin.manifest.id, &decl.key).await {
            Ok(Some(_)) => {}
            Ok(None) => missing.push(decl.key.clone()),
            Err(e) => {
                tracing::warn!(plugin = %plugin.id, key = %decl.key, error = %e, "cannot resolve a plugin secret");
                missing.push(decl.key.clone());
            }
        }
    }
    missing
}

/// Whether the run must stop before its next call. Checked before the first
/// call too: resolving secrets can take a while.
fn interrupted(inner: &Inner, plugin: &Managed, generation: u64) -> bool {
    let live = plugin.live();
    inner.stopping.load(Ordering::SeqCst) || !live.enabled || live.generation != generation
}

/// Calls the plugin page after page. Each call is awaited to its end.
async fn fetch(
    inner: &Inner,
    plugin: &Managed,
    kind: &str,
    call: &Call<'_>,
    progress: &mut Progress,
) -> End {
    let mut cursor = call.cursor.clone();
    // Items carry the id of the listing that saw them last, so a snapshot
    // that took several runs deletes only what none of its pages had. A
    // listing whose start this manager did not see deletes nothing.
    let (listing_id, whole) = match (&call.listing, &cursor) {
        (_, None) | (Listing::AtStart, Some(_)) => (new_id(), true),
        (Listing::Continuing { id, whole }, Some(_)) => (id.clone(), *whole),
        (Listing::Unknown, Some(_)) => (new_id(), false),
    };
    for _ in 0..inner.pages_per_run {
        if interrupted(inner, plugin, call.generation) {
            return End::Interrupted;
        }
        let request = SyncRequest {
            resource: kind.to_string(),
            cursor: cursor.clone(),
            settings: call.settings.clone(),
        };
        let handle = call.loaded.services.begin_call(CallPurpose::Sync);
        let page = match call.loaded.runtime.sync(handle, request).await {
            Ok(page) => page,
            Err(e) => return End::Call(e),
        };
        if !page.done && page.next_cursor == cursor {
            return End::NoProgress;
        }
        if let Err(e) = store_page(inner, plugin, kind, &listing_id, whole, &page, progress).await {
            return End::Store(e);
        }
        if page.done {
            if page.snapshot_complete && whole {
                match inner
                    .store
                    .delete_unseen(&plugin.id, kind, &listing_id)
                    .await
                {
                    Ok(n) => progress.deleted = progress.deleted.saturating_add(n),
                    Err(e) => return End::Store(format!("cannot remove unseen items: {e}")),
                }
            }
            return End::Finished;
        }
        cursor = page.next_cursor;
    }
    End::Finished
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Stores the page's items and its cursor together, and moves the in-memory
/// state along under the same lock, so the two never disagree.
async fn store_page(
    inner: &Inner,
    plugin: &Managed,
    kind: &str,
    listing_id: &str,
    whole: bool,
    page: &SyncPage,
    progress: &mut Progress,
) -> Result<(), String> {
    let _writes = plugin.sync_writes.lock().await;
    let (upserted, deleted) = inner
        .store
        .apply_page(&plugin.id, kind, listing_id, page, inner.now())
        .await
        .map_err(|e| format!("cannot store the page: {e}"))?;
    progress.upserted = progress.upserted.saturating_add(upserted);
    progress.deleted = progress.deleted.saturating_add(deleted);
    let mut live = plugin.live();
    if let Some(r) = live.resource_mut(kind) {
        r.sync.cursor = page.next_cursor.clone();
        r.listing = if page.done {
            Listing::AtStart
        } else {
            Listing::Continuing {
                id: listing_id.to_string(),
                whole,
            }
        };
    }
    Ok(())
}

/// When the resource is next due after an ending.
enum Next {
    /// One interval from now.
    Interval,
    /// One interval or the plugin's retry-after, whichever is longer.
    RateLimited(u32),
    /// A failure: count it and back off.
    Backoff,
    /// Leave the due time as it is.
    Unchanged,
}

/// What an ending does to the resource and the plugin.
struct Effect {
    /// The run succeeded: failures and the last error are cleared, and so is
    /// an auth or settings problem.
    ok: bool,
    /// The error shown, already cleaned.
    error: Option<String>,
    next: Next,
    /// A problem that holds or stops the plugin.
    problem: Option<Problem>,
}

fn effect(end: End, mode: HostMode) -> Effect {
    let failed = |error: String, next: Next, problem: Option<Problem>| Effect {
        ok: false,
        error: Some(error),
        next,
        problem,
    };
    let stopped = |error: &str| {
        Some(Problem::Failed {
            message: clean_message(&format!(
                "{error}; it will not run again until the daemon restarts"
            )),
        })
    };
    match end {
        End::Finished => Effect {
            ok: true,
            error: None,
            next: Next::Interval,
            problem: None,
        },
        End::Interrupted => Effect {
            ok: false,
            error: None,
            next: Next::Unchanged,
            problem: None,
        },
        End::NoProgress => failed(NO_PROGRESS.into(), Next::Backoff, None),
        End::Store(message) | End::Panicked(message) => {
            failed(clean_message(&message), Next::Backoff, None)
        }
        End::Call(e) => {
            let error = clean_message(&e.to_string());
            match e {
                CallError::Plugin(PluginError::Auth { message }) => {
                    let problem = Problem::NeedsAuth {
                        missing: Vec::new(),
                        message: clean_message(&message),
                    };
                    failed(error, Next::Interval, Some(problem))
                }
                CallError::Plugin(PluginError::Settings { message }) => {
                    let problem = Problem::NeedsSettings {
                        message: clean_message(&message),
                    };
                    failed(error, Next::Interval, Some(problem))
                }
                CallError::Plugin(PluginError::RateLimited {
                    retry_after_secs, ..
                }) => failed(error, Next::RateLimited(retry_after_secs), None),
                CallError::Plugin(PluginError::Http { .. } | PluginError::Other { .. })
                | CallError::Limit { .. }
                | CallError::Contract { .. } => failed(error, Next::Backoff, None),
                // A native call that timed out still holds its thread; one
                // that crashed may have left the library in any state.
                CallError::Timeout => {
                    let problem = match mode {
                        HostMode::Dylib | HostMode::InProcess => stopped(&error),
                        HostMode::Wasm | HostMode::Process => None,
                    };
                    failed(error, Next::Backoff, problem)
                }
                CallError::Crashed { .. } => {
                    let problem = match mode {
                        HostMode::Dylib => stopped(&error),
                        HostMode::InProcess | HostMode::Wasm | HostMode::Process => None,
                    };
                    failed(error, Next::Backoff, problem)
                }
            }
        }
    }
}

/// Records how the run ended, saves the state and publishes the events.
async fn finish(
    inner: &Inner,
    plugin: &Managed,
    kind: &str,
    token: u64,
    progress: &Progress,
    end: End,
) {
    let mode = match &plugin.loaded {
        Ok(loaded) => loaded.runtime.mode(),
        Err(_) => return,
    };
    let effect = effect(end, mode);
    if let Some(error) = &effect.error {
        tracing::warn!(plugin = %plugin.id, resource = %kind, "plugin sync failed: {error}");
    }
    let (now, wall) = (Instant::now(), inner.now());
    {
        let mut live = plugin.live();
        // New settings, or enabled again, during the run: what the run met
        // no longer holds the plugin, and the resource is due again now.
        let renewed = live.generation != progress.generation;
        if let Some(r) = live.resource_mut(kind) {
            if progress.started_at.is_some() {
                r.sync.last_run_at = progress.started_at;
            }
            if effect.ok {
                r.sync.last_ok_at = Some(wall);
                r.sync.last_error = None;
                r.sync.failures = 0;
            }
            if let Some(error) = &effect.error {
                r.sync.last_error = Some(error.clone());
            }
            let delay = match effect.next {
                Next::Interval => Some(r.interval),
                Next::RateLimited(secs) => Some(Duration::from_secs(secs.into()).max(r.interval)),
                Next::Backoff => {
                    r.sync.failures = r.sync.failures.saturating_add(1);
                    Some(inner.backoff(r.sync.failures))
                }
                Next::Unchanged => None,
            };
            if let Some(d) = delay {
                r.due = later(now, d);
                r.sync.next_due_at = Some(wall_after(wall, d));
            }
            if renewed {
                r.sync.failures = 0;
                r.due = now;
                r.sync.next_due_at = Some(wall);
            }
        }
        match effect.problem {
            Some(stop @ Problem::Failed { .. }) => live.problem = Some(stop),
            Some(held @ (Problem::NeedsAuth { .. } | Problem::NeedsSettings { .. })) => {
                if !renewed {
                    live.problem = Some(held);
                }
            }
            None if effect.ok && !live.is_failed() => live.problem = None,
            None => {}
        }
        if live.running.as_ref().is_some_and(|r| r.token == token) {
            live.running = None;
        }
    }
    inner.persist(plugin, kind).await;
    if progress.upserted > 0 || progress.deleted > 0 {
        inner.bus.publish(EventKind::ResourceSynced {
            plugin_id: plugin.id.clone(),
            resource: kind.to_string(),
            upserted: progress.upserted,
            deleted: progress.deleted,
            at: wall,
        });
    }
    inner.publish_status(plugin).await;
}

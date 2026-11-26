//! The plugin manager: every plugin's state, when each of its resources
//! syncs, the runs themselves, and the events they publish.
//!
//! - [`schedule`]: one task wakes at the earliest due time and starts runs, at
//!   most one per plugin; a semaphore keeps `max_concurrent` calls at once.
//! - [`run`]: one run of one resource: secrets check, pages, outcome.
//! - [`status`]: the [`PluginStatus`] the API and events carry.
//!
//! Scheduling uses tokio's clock; the times it stores come from a wall clock,
//! which tests replace. A call into a plugin is never cancelled: a native
//! runtime marks itself stopped only when its own timeout fires, so a dropped
//! call would leave a stuck thread behind a plugin that looks healthy.
//! Disabling, new settings and shutdown take effect between pages.

mod run;
mod schedule;
mod status;

pub(crate) use status::clean_message;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::Value;
use smithagents_core::event::EventKind;
use smithagents_core::plugin::{ItemsPage, PluginStatus};
use smithagents_plugin_host::{
    HostServices, HttpPolicy, PluginRuntime, SecretSource, ServicesError,
};
use smithagents_plugin_protocol::Manifest;
use tokio::sync::{watch, Notify, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::{settings, Origin};
use crate::events::EventBus;
use crate::store::{PluginConfig, Store, StoreError, SyncState};

/// How long `shutdown` waits for runs in flight.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// The longest delay before a resource is due again (a rate limit can ask for
/// up to `u32::MAX` seconds); keeps stored times within four-digit years.
const MAX_DELAY: Duration = Duration::from_secs(u32::MAX as u64);

#[derive(Debug, Clone)]
pub struct ManagerConfig {
    /// Plugin calls running at once, across all plugins.
    pub max_concurrent: usize,
    /// Pages one run may fetch before it waits for the next interval.
    pub pages_per_run: u32,
    /// The first retry delay after a failure; it doubles with each failure.
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    pub http: HttpPolicy,
}

impl Default for ManagerConfig {
    fn default() -> ManagerConfig {
        ManagerConfig {
            max_concurrent: 4,
            pages_per_run: 20,
            backoff_min: Duration::from_secs(30),
            backoff_max: Duration::from_secs(30 * 60),
            http: HttpPolicy::default(),
        }
    }
}

/// A discovered plugin and its runtime, or why the runtime could not be built.
pub struct LoadedPlugin {
    pub manifest: Manifest,
    pub origin: Origin,
    pub runtime: Result<Arc<dyn PluginRuntime>, String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("no such plugin")]
    UnknownPlugin,
    #[error("the plugin has no such resource")]
    UnknownResource,
    /// Disabled, or unable to run: it failed to load or was stopped.
    #[error("the plugin is disabled or cannot run")]
    Disabled,
    #[error("this resource is syncing now")]
    Busy,
    #[error("invalid settings: {0}")]
    InvalidSettings(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Where the manager reads the time it stores and publishes.
pub type WallClock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Owns every plugin. Cheap to clone; clones share one manager.
#[derive(Clone)]
pub struct PluginManager {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    bus: EventBus,
    secrets: Arc<dyn SecretSource>,
    clock: WallClock,
    pages_per_run: u32,
    backoff_min: Duration,
    backoff_max: Duration,
    /// Fixed after `start`, sorted by id.
    plugins: BTreeMap<String, Managed>,
    /// One permit per call allowed at once; closed at shutdown.
    permits: Arc<Semaphore>,
    /// Wakes the scheduler: a run ended, or a plugin or resource changed.
    wake: Arc<Notify>,
    /// Runs started and not yet ended.
    active: watch::Sender<usize>,
    next_token: AtomicU64,
    stopping: AtomicBool,
    scheduler: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // The scheduler holds only a weak handle; let it notice and end.
        self.wake.notify_one();
    }
}

/// One plugin as the manager holds it.
struct Managed {
    id: String,
    manifest: Manifest,
    origin: Origin,
    loaded: Result<Loaded, String>,
    live: Mutex<Live>,
    /// Serialises writes of the plugin's configuration.
    config_writes: tokio::sync::Mutex<()>,
    /// Serialises writes of the plugin's sync rows (cursor and state), so the
    /// last write always carries the newest state.
    sync_writes: tokio::sync::Mutex<()>,
}

struct Loaded {
    runtime: Arc<dyn PluginRuntime>,
    services: Arc<HostServices>,
}

/// The parts of a plugin that change. Never held across an `.await`.
struct Live {
    enabled: bool,
    /// As stored: what the user gave, without defaults.
    settings: Value,
    /// Why the stored settings do not validate, if they do not.
    settings_error: Option<String>,
    problem: Option<Problem>,
    /// Bumped by new settings and by enabling: a run that started before
    /// stops after its current call, and its outcome does not hold the plugin.
    generation: u64,
    running: Option<Running>,
    /// In manifest order.
    resources: Vec<ResourceLive>,
}

/// What stops a plugin from syncing on schedule.
enum Problem {
    /// Until the settings change, it is enabled again, or `sync_now`.
    NeedsAuth {
        missing: Vec<String>,
        message: String,
    },
    /// Until the settings change, it is enabled again, or `sync_now`.
    NeedsSettings { message: String },
    /// Until the daemon restarts: a native call is stuck or crashed.
    Failed { message: String },
}

struct Running {
    kind: String,
    /// Tells this run's end from a later run's start.
    token: u64,
}

struct ResourceLive {
    kind: String,
    interval: Duration,
    /// Mirrors the stored row; written through [`Inner::persist`].
    sync: SyncState,
    due: Instant,
    /// Asked for by `sync_now`: runs next, even while the plugin needs auth.
    forced: bool,
}

impl Managed {
    fn live(&self) -> MutexGuard<'_, Live> {
        // No code holding the lock leaves `Live` inconsistent if it panics.
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn has_resource(&self, kind: &str) -> bool {
        self.manifest.resources.iter().any(|r| r.kind == kind)
    }

    /// Clears `running` if run `token` still holds it.
    fn end_run(&self, token: u64) {
        let mut live = self.live();
        if live.running.as_ref().is_some_and(|r| r.token == token) {
            live.running = None;
        }
    }
}

impl Live {
    fn resource(&self, kind: &str) -> Option<&ResourceLive> {
        self.resources.iter().find(|r| r.kind == kind)
    }

    fn resource_mut(&mut self, kind: &str) -> Option<&mut ResourceLive> {
        self.resources.iter_mut().find(|r| r.kind == kind)
    }

    /// Why the plugin needs settings, if it does: its stored settings do not
    /// validate, or a run was told so by the plugin.
    fn settings_problem(&self) -> Option<&str> {
        if let Some(message) = &self.settings_error {
            return Some(message);
        }
        match &self.problem {
            Some(Problem::NeedsSettings { message }) => Some(message),
            Some(Problem::NeedsAuth { .. } | Problem::Failed { .. }) | None => None,
        }
    }

    fn is_failed(&self) -> bool {
        match &self.problem {
            Some(Problem::Failed { .. }) => true,
            Some(Problem::NeedsAuth { .. } | Problem::NeedsSettings { .. }) | None => false,
        }
    }

    /// New settings or enabling: forget failures and auth or settings
    /// problems (a stopped plugin stays stopped), and make everything due now.
    fn reset(&mut self, now: Instant, wall: DateTime<Utc>) {
        self.generation += 1;
        if !self.is_failed() {
            self.problem = None;
        }
        for r in &mut self.resources {
            r.sync.failures = 0;
            r.sync.next_due_at = Some(wall);
            r.due = now;
        }
    }
}

/// `now + d`, with `d` capped at [`MAX_DELAY`].
fn later(now: Instant, d: Duration) -> Instant {
    let d = d.min(MAX_DELAY);
    now.checked_add(d).unwrap_or(now)
}

fn wall_after(wall: DateTime<Utc>, d: Duration) -> DateTime<Utc> {
    TimeDelta::from_std(d.min(MAX_DELAY))
        .ok()
        .and_then(|d| wall.checked_add_signed(d))
        .unwrap_or(wall)
}

/// The instant a stored due time stands for; due now if it is past or absent.
fn due_from(next_due_at: Option<DateTime<Utc>>, now: Instant, wall: DateTime<Utc>) -> Instant {
    match next_due_at.and_then(|at| (at - wall).to_std().ok()) {
        Some(wait) => later(now, wait),
        None => now,
    }
}

/// A fixed sentence: the error's own text would quote the manifest.
fn services_failure(e: &ServicesError) -> &'static str {
    match e {
        ServicesError::BadHost(_) => "a host the plugin declares cannot be used",
        ServicesError::Client(_) => "the plugin's HTTP client cannot be built",
    }
}

impl PluginManager {
    /// Loads every plugin's configuration and sync state, then starts the
    /// scheduler. A plugin whose runtime failed to load is kept, as
    /// `LoadFailed`.
    pub async fn start(
        store: Store,
        bus: EventBus,
        secrets: Arc<dyn SecretSource>,
        plugins: Vec<LoadedPlugin>,
        cfg: ManagerConfig,
    ) -> Result<PluginManager, ManagerError> {
        Self::launch(store, bus, secrets, plugins, cfg, Arc::new(Utc::now)).await
    }

    /// [`Self::start`] with a wall clock of the test's choosing.
    #[cfg(feature = "test-util")]
    pub async fn start_with_clock(
        store: Store,
        bus: EventBus,
        secrets: Arc<dyn SecretSource>,
        plugins: Vec<LoadedPlugin>,
        cfg: ManagerConfig,
        clock: WallClock,
    ) -> Result<PluginManager, ManagerError> {
        Self::launch(store, bus, secrets, plugins, cfg, clock).await
    }

    async fn launch(
        store: Store,
        bus: EventBus,
        secrets: Arc<dyn SecretSource>,
        plugins: Vec<LoadedPlugin>,
        cfg: ManagerConfig,
        clock: WallClock,
    ) -> Result<PluginManager, ManagerError> {
        let mut managed = BTreeMap::new();
        for plugin in plugins {
            let id = plugin.manifest.id.to_string();
            if managed.contains_key(&id) {
                tracing::warn!(plugin = %id, "a plugin id is listed twice; the first is kept");
                continue;
            }
            let plugin = prepare(&store, &secrets, &cfg.http, &clock, plugin).await?;
            managed.insert(id, plugin);
        }
        let (active, _) = watch::channel(0);
        let inner = Arc::new(Inner {
            store,
            bus,
            secrets,
            clock,
            pages_per_run: cfg.pages_per_run.max(1),
            backoff_min: cfg.backoff_min,
            backoff_max: cfg.backoff_max,
            plugins: managed,
            permits: Arc::new(Semaphore::new(cfg.max_concurrent.max(1))),
            wake: Arc::new(Notify::new()),
            active,
            next_token: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
            scheduler: Mutex::new(None),
        });
        let task = tokio::spawn(schedule::scheduler(
            Arc::downgrade(&inner),
            Arc::clone(&inner.wake),
        ));
        *inner
            .scheduler
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(task);
        Ok(PluginManager { inner })
    }

    /// Every plugin, sorted by id.
    pub async fn list(&self) -> Vec<PluginStatus> {
        let mut out = Vec::with_capacity(self.inner.plugins.len());
        for plugin in self.inner.plugins.values() {
            out.push(status::status(&self.inner, plugin).await);
        }
        out
    }

    pub async fn get(&self, id: &str) -> Option<PluginStatus> {
        let plugin = self.inner.plugins.get(id)?;
        Some(status::status(&self.inner, plugin).await)
    }

    /// Enabling also clears auth and settings problems and failures, and
    /// makes every resource due now. Disabling lets a call in flight finish
    /// and stops its run before the next page.
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<PluginStatus, ManagerError> {
        let inner = &self.inner;
        let plugin = inner.plugin(id)?;
        {
            let _config = plugin.config_writes.lock().await;
            let settings = plugin.live().settings.clone();
            inner
                .store
                .set_plugin_config(id, &PluginConfig { enabled, settings })
                .await?;
            let mut live = plugin.live();
            live.enabled = enabled;
            if enabled {
                live.reset(Instant::now(), inner.now());
            }
        }
        if enabled {
            inner.persist_all(plugin).await;
        }
        inner.wake.notify_one();
        Ok(inner.publish_status(plugin).await)
    }

    /// The settings as stored, without defaults.
    pub async fn settings(&self, id: &str) -> Result<Value, ManagerError> {
        Ok(self.inner.plugin(id)?.live().settings.clone())
    }

    /// Stores `value` if it validates against the manifest; then as enabling.
    /// A run in flight stops before its next page and runs again at once.
    pub async fn set_settings(&self, id: &str, value: Value) -> Result<PluginStatus, ManagerError> {
        let inner = &self.inner;
        let plugin = inner.plugin(id)?;
        settings::effective(&plugin.manifest, &value).map_err(ManagerError::InvalidSettings)?;
        {
            let _config = plugin.config_writes.lock().await;
            let enabled = plugin.live().enabled;
            let config = PluginConfig {
                enabled,
                settings: value.clone(),
            };
            inner.store.set_plugin_config(id, &config).await?;
            let mut live = plugin.live();
            live.settings = value;
            live.settings_error = None;
            live.reset(Instant::now(), inner.now());
        }
        inner.persist_all(plugin).await;
        inner.wake.notify_one();
        Ok(inner.publish_status(plugin).await)
    }

    /// Queues a run of `resource`, even while the plugin needs auth. `Busy`
    /// while that resource syncs; a run of another resource of the plugin
    /// finishes first. Refused while the plugin needs settings (with the
    /// reason), and when it is disabled, cannot run, or the manager is shut
    /// down.
    pub async fn sync_now(&self, id: &str, resource: &str) -> Result<(), ManagerError> {
        let plugin = self.inner.plugin(id)?;
        {
            let mut live = plugin.live();
            if live.resource(resource).is_none() {
                return Err(ManagerError::UnknownResource);
            }
            if plugin.loaded.is_err()
                || !live.enabled
                || live.is_failed()
                || self.inner.stopping.load(Ordering::SeqCst)
            {
                return Err(ManagerError::Disabled);
            }
            if let Some(message) = live.settings_problem() {
                return Err(ManagerError::InvalidSettings(message.to_string()));
            }
            if live.running.as_ref().is_some_and(|r| r.kind == resource) {
                return Err(ManagerError::Busy);
            }
            if let Some(r) = live.resource_mut(resource) {
                r.forced = true;
            }
        }
        self.inner.wake.notify_one();
        Ok(())
    }

    pub async fn items(
        &self,
        id: &str,
        resource: &str,
        limit: u32,
        after: Option<&str>,
        q: Option<&str>,
    ) -> Result<ItemsPage, ManagerError> {
        let plugin = self.inner.plugin(id)?;
        if !plugin.has_resource(resource) {
            return Err(ManagerError::UnknownResource);
        }
        Ok(self
            .inner
            .store
            .list_items(id, resource, limit, after, q)
            .await?)
    }

    /// Stops scheduling and waits up to 5 s for runs in flight. A call still
    /// running after that is left to finish or time out on its own; its run
    /// fetches no further page.
    pub async fn shutdown(&self) {
        let inner = &self.inner;
        inner.stopping.store(true, Ordering::SeqCst);
        // Runs waiting for a slot give up.
        inner.permits.close();
        inner.wake.notify_one();
        let task = inner
            .scheduler
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            if let Err(e) = task.await {
                tracing::error!("the plugin scheduler failed: {e}");
            }
        }
        let mut idle = inner.active.subscribe();
        if tokio::time::timeout(SHUTDOWN_GRACE, idle.wait_for(|n| *n == 0))
            .await
            .is_err()
        {
            tracing::warn!("plugin syncs still running after 5 s are left to finish on their own");
        }
    }

    /// One scheduling pass, awaited until every run it started (and any it
    /// had to wait for) has ended; repeats while a pass starts something.
    /// Returns with no run in flight, including runs the scheduler task
    /// started meanwhile.
    #[cfg(feature = "test-util")]
    pub async fn run_due(&self) {
        let mut idle = self.inner.active.subscribe();
        loop {
            // An error means the manager is gone, which `self` rules out.
            let _ = idle.wait_for(|n| *n == 0).await;
            // The scheduler task may have started a run since the wait saw
            // none: return only when nothing runs after an empty pass.
            if schedule::pass(&self.inner) == 0 && *self.inner.active.borrow() == 0 {
                return;
            }
        }
    }
}

impl Inner {
    fn plugin(&self, id: &str) -> Result<&Managed, ManagerError> {
        self.plugins.get(id).ok_or(ManagerError::UnknownPlugin)
    }

    fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }

    /// The first retry delay after `failures` failures in a row.
    fn backoff(&self, failures: u32) -> Duration {
        let factor = 2u32.saturating_pow(failures.saturating_sub(1));
        self.backoff_min
            .saturating_mul(factor)
            .min(self.backoff_max)
    }

    /// Writes the resource's sync state as it is now.
    async fn persist(&self, plugin: &Managed, kind: &str) {
        let _writes = plugin.sync_writes.lock().await;
        let Some(state) = plugin.live().resource(kind).map(|r| r.sync.clone()) else {
            return;
        };
        if let Err(e) = self.store.save_sync_state(&plugin.id, kind, &state).await {
            tracing::error!(plugin = %plugin.id, resource = %kind, "cannot save the sync state: {e}");
        }
    }

    async fn persist_all(&self, plugin: &Managed) {
        for decl in &plugin.manifest.resources {
            self.persist(plugin, &decl.kind).await;
        }
    }

    async fn publish_status(&self, plugin: &Managed) -> PluginStatus {
        let status = status::status(self, plugin).await;
        self.bus.publish(EventKind::PluginUpdated(status.clone()));
        status
    }
}

/// Builds one plugin's state: creates its configuration if absent, drops the
/// stored resources its manifest no longer declares, and reads the rest.
async fn prepare(
    store: &Store,
    secrets: &Arc<dyn SecretSource>,
    http: &HttpPolicy,
    clock: &WallClock,
    plugin: LoadedPlugin,
) -> Result<Managed, ManagerError> {
    let LoadedPlugin {
        manifest,
        origin,
        runtime,
    } = plugin;
    let id = manifest.id.to_string();
    let config = match store.plugin_config(&id).await? {
        Some(config) => config,
        None => {
            let config = PluginConfig {
                enabled: origin == Origin::Builtin,
                settings: settings::defaults(&manifest),
            };
            store.set_plugin_config(&id, &config).await?;
            config
        }
    };
    for kind in store.known_resources(&id).await? {
        if !manifest.resources.iter().any(|r| r.kind == kind) {
            let removed = store.delete_resource(&id, &kind).await?;
            tracing::info!(plugin = %id, resource = %kind, removed, "removed a resource the plugin no longer declares");
        }
    }
    let loaded = match runtime {
        Err(message) => Err(clean_message(&message)),
        Ok(runtime) => match HostServices::new(&manifest, Arc::clone(secrets), http.clone()) {
            Ok(services) => Ok(Loaded {
                runtime,
                services: Arc::new(services),
            }),
            Err(e) => Err(services_failure(&e).to_string()),
        },
    };
    let (now, wall) = (Instant::now(), clock());
    let mut resources = Vec::with_capacity(manifest.resources.len());
    for decl in &manifest.resources {
        let sync = store.sync_state(&id, &decl.kind).await?;
        resources.push(ResourceLive {
            kind: decl.kind.clone(),
            interval: Duration::from_secs(decl.interval_secs.into()),
            due: due_from(sync.next_due_at, now, wall),
            forced: false,
            sync,
        });
    }
    let settings_error = settings::effective(&manifest, &config.settings).err();
    Ok(Managed {
        id,
        origin,
        loaded,
        live: Mutex::new(Live {
            enabled: config.enabled,
            settings: config.settings,
            settings_error,
            problem: None,
            generation: 0,
            running: None,
            resources,
        }),
        config_writes: tokio::sync::Mutex::new(()),
        sync_writes: tokio::sync::Mutex::new(()),
        manifest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn stored_due_times_map_onto_the_scheduling_clock() {
        let now = Instant::now();
        let wall: DateTime<Utc> = "2026-11-26T10:00:00Z".parse().unwrap();
        assert_eq!(due_from(None, now, wall), now);
        assert_eq!(due_from(Some(wall - TimeDelta::hours(1)), now, wall), now);
        assert_eq!(
            due_from(Some(wall + TimeDelta::seconds(90)), now, wall),
            now + Duration::from_secs(90)
        );
    }

    #[test]
    fn delays_are_capped_so_stored_times_stay_readable() {
        let wall: DateTime<Utc> = "2026-11-26T10:00:00Z".parse().unwrap();
        let far = wall_after(wall, Duration::MAX);
        assert_eq!(far, wall_after(wall, MAX_DELAY));
        assert!(far > wall + TimeDelta::days(100 * 365));
        // The store writes RFC 3339, which needs a four-digit year.
        let text = far.to_rfc3339();
        assert_eq!(DateTime::parse_from_rfc3339(&text).unwrap(), far, "{text}");
    }
}

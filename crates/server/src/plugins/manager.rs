//! The plugin manager: every plugin's state, when each of its resources
//! syncs, the runs themselves, and the events they publish.
//!
//! - [`schedule`]: one task wakes at the earliest due time and starts runs, at
//!   most one per plugin; a semaphore keeps `max_concurrent` calls at once.
//! - [`run`]: one run of one resource: settings, tools and secrets checks,
//!   pages, outcome.
//! - [`status`]: the [`PluginStatus`] the API and events carry.
//! - [`tools`]: the command-line tools a built-in plugin depends on: checks
//!   before runs (and, for a plugin without resources, when its status is
//!   read), installs on request, secrets a tool prints, and runs for the
//!   daemon's own code.
//!
//! Scheduling uses tokio's clock; the times it stores come from a wall clock,
//! which tests replace. A call into a plugin is never cancelled: a native
//! runtime marks itself stopped only when its own timeout fires, so a dropped
//! call would leave a stuck thread behind a plugin that looks healthy.
//! Disabling, new settings and shutdown take effect between pages.

mod run;
mod schedule;
mod status;
mod tools;

pub(crate) use status::clean_message;

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::Value;
use smithagents_core::event::EventKind;
use smithagents_core::plugin::{ItemsPage, MissingTool, PluginStatus};
use smithagents_plugin_host::{
    HostServices, HttpPolicy, InstallError, PluginRuntime, SecretSource, ServicesError, ToolFinder,
    ToolRun, ToolRunError, ToolState,
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
/// The load failure of a plugin whose rows the store could not read at start.
pub(super) const UNREADABLE_STATE: &str = "this plugin's stored state could not be read";

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
    /// Where built-in plugins' command-line tools are looked for.
    pub tools: ToolFinder,
}

impl Default for ManagerConfig {
    fn default() -> ManagerConfig {
        ManagerConfig {
            max_concurrent: 4,
            pages_per_run: 20,
            backoff_min: Duration::from_secs(30),
            backoff_max: Duration::from_secs(30 * 60),
            http: HttpPolicy::default(),
            tools: ToolFinder::default(),
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
    /// Only a built-in plugin's tools are installed, checked on request or
    /// run.
    #[error("the daemon does not install, check or run an installed plugin's tools")]
    NotAllowed,
    #[error("the plugin declares no such tool")]
    UnknownTool,
    /// Another install runs: one at a time in the whole daemon.
    #[error("a command-line tool is being installed now")]
    InstallInProgress,
    #[error(transparent)]
    Install(InstallError),
    /// A declared tool ran and did not answer; see
    /// [`PluginManager::run_tool`]. Carries nothing the tool printed.
    #[error(transparent)]
    Tool(ToolRunError),
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
    tools: ToolFinder,
    /// An install runs; see [`tools::install`].
    installing: AtomicBool,
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
    /// Its stored configuration or sync state could not be read at start.
    /// While set it shows as `LoadFailed`, never runs, and nothing writes
    /// its rows; enabling it or giving it settings replaces them with fresh
    /// ones ([`Inner::repair`]) and clears this.
    unreadable_state: AtomicBool,
    /// An installed plugin that declares command-line tools: running a
    /// program a third-party manifest names needs an approval that does not
    /// exist yet, so it never runs.
    unapproved_tools: bool,
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
    /// The last check of the plugin's tools; see [`tools::missing_tools`].
    tool_checks: tools::ToolChecks,
}

/// What stops a plugin from syncing on schedule.
enum Problem {
    /// Until the settings change, it is enabled again, or `sync_now`.
    NeedsAuth {
        missing: Vec<String>,
        message: String,
    },
    /// Until the settings change or it is enabled again; `sync_now` is
    /// refused meanwhile.
    NeedsSettings { message: String },
    /// A tool is missing or too old. Until the settings change, it is
    /// enabled again, `sync_now`, or a tool is installed.
    NeedsSetup {
        tools: Vec<MissingTool>,
        message: String,
    },
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

    /// Loaded, and allowed to run.
    fn can_run(&self) -> bool {
        self.loaded.is_ok() && !self.unapproved_tools && !self.is_unreadable()
    }

    fn has_resource(&self, kind: &str) -> bool {
        self.manifest.resources.iter().any(|r| r.kind == kind)
    }

    /// Its stored state could not be read and has not been replaced yet.
    fn is_unreadable(&self) -> bool {
        self.unreadable_state.load(Ordering::SeqCst)
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
            Some(
                Problem::NeedsAuth { .. } | Problem::NeedsSetup { .. } | Problem::Failed { .. },
            )
            | None => None,
        }
    }

    fn is_failed(&self) -> bool {
        match &self.problem {
            Some(Problem::Failed { .. }) => true,
            Some(
                Problem::NeedsAuth { .. }
                | Problem::NeedsSettings { .. }
                | Problem::NeedsSetup { .. },
            )
            | None => false,
        }
    }

    /// New settings or enabling: forget failures, auth, settings and setup
    /// problems and the last tool check (a stopped plugin stays stopped),
    /// and make everything due now.
    fn reset(&mut self, now: Instant, wall: DateTime<Utc>) {
        self.generation += 1;
        if !self.is_failed() {
            self.problem = None;
        }
        self.tool_checks.forget();
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
    /// scheduler. A plugin whose runtime failed to load, or one of whose
    /// stored rows cannot be decoded, is kept, as `LoadFailed`, and the
    /// others start as usual. Any other store error fails the start.
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
        let mut ids = HashSet::new();
        let plugins: Vec<LoadedPlugin> = plugins
            .into_iter()
            .filter(|plugin| {
                let first = ids.insert(plugin.manifest.id.to_string());
                if !first {
                    tracing::warn!(plugin = %plugin.manifest.id, "a plugin id is listed twice; the first is kept");
                }
                first
            })
            .collect();
        let secrets = tools::with_tool_secrets(secrets, &plugins, &cfg.tools);
        let mut managed = BTreeMap::new();
        for plugin in plugins {
            let id = plugin.manifest.id.to_string();
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
            tools: cfg.tools,
            installing: AtomicBool::new(false),
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

    /// Enabling also clears auth and settings problems and failures, drops
    /// the plugin's cached secret values, and makes every resource due now.
    /// Disabling lets a call in flight finish and stops its run before the
    /// next page. For a plugin whose stored state could not be read,
    /// enabling replaces its rows with fresh ones (the manifest's default
    /// settings) and disabling changes nothing.
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<PluginStatus, ManagerError> {
        let inner = &self.inner;
        let plugin = inner.plugin(id)?;
        {
            let _config = plugin.config_writes.lock().await;
            if plugin.is_unreadable() {
                // Disabled already; nothing is written over the unread rows.
                if enabled {
                    let settings = settings::defaults(&plugin.manifest);
                    inner
                        .repair(plugin, PluginConfig { enabled, settings })
                        .await?;
                }
            } else {
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
        }
        if enabled {
            inner.secrets.forget(&plugin.manifest.id);
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
    /// For a plugin whose stored state could not be read, its rows are
    /// replaced with fresh ones holding `value`, enabled as a newly found
    /// plugin of its origin would be.
    pub async fn set_settings(&self, id: &str, value: Value) -> Result<PluginStatus, ManagerError> {
        let inner = &self.inner;
        let plugin = inner.plugin(id)?;
        settings::effective(&plugin.manifest, &value).map_err(ManagerError::InvalidSettings)?;
        {
            let _config = plugin.config_writes.lock().await;
            if plugin.is_unreadable() {
                let config = PluginConfig {
                    enabled: plugin.origin == Origin::Builtin,
                    settings: value,
                };
                inner.repair(plugin, config).await?;
            } else {
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
        }
        inner.secrets.forget(&plugin.manifest.id);
        inner.persist_all(plugin).await;
        inner.wake.notify_one();
        Ok(inner.publish_status(plugin).await)
    }

    /// Queues a run of `resource`, even while the plugin needs auth or
    /// setup; the run checks the plugin's tools again and resolves its
    /// secrets anew (a fresh sign-in counts at once). `Busy` while that
    /// resource syncs; a run of another resource of the plugin finishes
    /// first. Refused while the plugin needs settings (with the reason), and
    /// when it is disabled, cannot run, or the manager is shut down.
    pub async fn sync_now(&self, id: &str, resource: &str) -> Result<(), ManagerError> {
        let plugin = self.inner.plugin(id)?;
        {
            let mut live = plugin.live();
            if live.resource(resource).is_none() {
                return Err(ManagerError::UnknownResource);
            }
            if !plugin.can_run()
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
            live.tool_checks.forget();
        }
        self.inner.secrets.forget(&plugin.manifest.id);
        self.inner.wake.notify_one();
        Ok(())
    }

    /// Installs one of a built-in plugin's tools with `mise use --global`,
    /// then checks the plugin's tools again; when none is missing any more
    /// the plugin is renewed as by enabling. Asking is the approval: nothing
    /// is installed otherwise. One install at a time in the whole daemon
    /// (`mise use --global` edits one global file). Refused for an installed
    /// plugin.
    pub async fn install_tool(&self, id: &str, tool: &str) -> Result<PluginStatus, ManagerError> {
        let plugin = self.inner.plugin(id)?;
        tools::install(&self.inner, plugin, tool).await?;
        Ok(self.inner.publish_status(plugin).await)
    }

    /// A fresh check of the tool `tool` that the built-in plugin `id`
    /// declares (the status's cached check is neither read nor replaced).
    /// Refused for an installed plugin (`NotAllowed`) and a tool the plugin
    /// does not declare (`UnknownTool`).
    pub async fn tool_state(&self, id: &str, tool: &str) -> Result<ToolState, ManagerError> {
        let plugin = self.inner.plugin(id)?;
        let decl = tools::builtin_tool(plugin, tool)?;
        Ok(self.inner.tools.check(decl).await)
    }

    /// Runs the tool `tool` that the built-in plugin `id` declares, for the
    /// daemon's own code: `<tool> <args>` in `run.cwd`, found and run as
    /// every tool is (no shell, no `SMITHAGENTS_*` variable, a timeout, at
    /// most 64 KiB of output). The answer is the first line it printed. The
    /// same refusals as [`Self::tool_state`]; a run that fails is
    /// `ManagerError::Tool`, which carries nothing the tool printed.
    pub async fn run_tool(
        &self,
        id: &str,
        tool: &str,
        run: ToolRun<'_>,
    ) -> Result<Option<String>, ManagerError> {
        let plugin = self.inner.plugin(id)?;
        let decl = tools::builtin_tool(plugin, tool)?;
        self.inner
            .tools
            .run(&decl.name, run)
            .await
            .map_err(ManagerError::Tool)
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

    /// Replaces the rows of a plugin whose stored state could not be read:
    /// its configuration becomes `config` and its sync state is dropped, in
    /// one transaction, so a crash leaves either the old rows (still
    /// unreadable) or the new ones. Its items stay; the next complete
    /// listing reconciles them. The plugin then starts over as if newly
    /// found. The caller holds its `config_writes`.
    async fn repair(&self, plugin: &Managed, config: PluginConfig) -> Result<(), ManagerError> {
        let _writes = plugin.sync_writes.lock().await;
        self.store.reset_plugin_state(&plugin.id, &config).await?;
        tracing::warn!(plugin = %plugin.id, "replaced the plugin's unreadable stored state with a fresh one");
        let mut live = plugin.live();
        live.settings_error = settings::effective(&plugin.manifest, &config.settings).err();
        live.enabled = config.enabled;
        live.settings = config.settings;
        for r in &mut live.resources {
            r.sync = SyncState::default();
        }
        live.reset(Instant::now(), self.now());
        plugin.unreadable_state.store(false, Ordering::SeqCst);
        Ok(())
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

    /// Writes the resource's sync state as it is now; never over rows that
    /// could not be read.
    async fn persist(&self, plugin: &Managed, kind: &str) {
        if plugin.is_unreadable() {
            return;
        }
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

/// What the store holds for one plugin at start.
struct Stored {
    config: PluginConfig,
    /// In manifest order.
    syncs: Vec<SyncState>,
}

/// Creates the plugin's configuration if absent, drops the stored resources
/// its manifest no longer declares, and reads the rest.
async fn read_stored(
    store: &Store,
    id: &str,
    manifest: &Manifest,
    origin: Origin,
) -> Result<Stored, StoreError> {
    let config = match store.plugin_config(id).await? {
        Some(config) => config,
        None => {
            let config = PluginConfig {
                enabled: origin == Origin::Builtin,
                settings: settings::defaults(manifest),
            };
            store.set_plugin_config(id, &config).await?;
            config
        }
    };
    for kind in store.known_resources(id).await? {
        if !manifest.resources.iter().any(|r| r.kind == kind) {
            let removed = store.delete_resource(id, &kind).await?;
            tracing::info!(plugin = %id, resource = %kind, removed, "removed a resource the plugin no longer declares");
        }
    }
    let mut syncs = Vec::with_capacity(manifest.resources.len());
    for decl in &manifest.resources {
        syncs.push(store.sync_state(id, &decl.kind).await?);
    }
    Ok(Stored { config, syncs })
}

/// A value in one of the plugin's own rows that cannot be decoded, as
/// opposed to a database that cannot be used.
fn is_unreadable_row(e: &StoreError) -> bool {
    matches!(
        e,
        StoreError::Corrupt(_)
            | StoreError::Sqlx(sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_))
    )
}

/// Builds one plugin's state from its stored rows. A row that cannot be
/// decoded leaves the plugin `LoadFailed` (see [`Managed::unreadable_state`]);
/// any other store error fails the start.
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
    let (stored, unreadable_state) = match read_stored(store, &id, &manifest, origin).await {
        Ok(stored) => (stored, false),
        Err(e) if is_unreadable_row(&e) => {
            // The error may quote what the row holds: the id only.
            tracing::error!(plugin = %id, "{UNREADABLE_STATE}; the plugin is not loaded until it is enabled or given settings");
            let stored = Stored {
                config: PluginConfig {
                    enabled: false,
                    settings: settings::defaults(&manifest),
                },
                syncs: vec![SyncState::default(); manifest.resources.len()],
            };
            (stored, true)
        }
        Err(e) => return Err(e.into()),
    };
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
    let Stored { config, syncs } = stored;
    let (now, wall) = (Instant::now(), clock());
    let resources = manifest
        .resources
        .iter()
        .zip(syncs)
        .map(|(decl, sync)| ResourceLive {
            kind: decl.kind.clone(),
            interval: Duration::from_secs(decl.interval_secs.into()),
            due: due_from(sync.next_due_at, now, wall),
            forced: false,
            sync,
        })
        .collect();
    let settings_error = settings::effective(&manifest, &config.settings).err();
    let unapproved_tools = match origin {
        Origin::Builtin => false,
        Origin::Installed => !manifest.tools.is_empty(),
    };
    Ok(Managed {
        id,
        origin,
        loaded,
        unreadable_state: AtomicBool::new(unreadable_state),
        unapproved_tools,
        live: Mutex::new(Live {
            enabled: config.enabled,
            settings: config.settings,
            settings_error,
            problem: None,
            generation: 0,
            running: None,
            resources,
            tool_checks: tools::ToolChecks::default(),
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

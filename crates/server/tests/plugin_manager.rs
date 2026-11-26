//! The plugin manager: runs, errors and backoff, scheduling limits, shutdown
//! and restart.
//!
//! Time. Every test pauses tokio's clock and holds a blocking task open for
//! its whole length ([`Env`]'s `_frozen`). Tokio does not auto-advance a
//! paused clock while a blocking task runs, so the clock moves only when a
//! test calls `advance`. Without that, each store query would let the clock
//! jump to the next pending timer, because SQLite answers from its own thread
//! and the runtime looks idle meanwhile. The manager's wall clock follows
//! tokio's (`Env::clock`), so stored times are exact as well.

use std::future::Future;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::json;
use smithagents_core::event::{Event, EventKind};
use smithagents_core::plugin::{MissingTool, PluginState, PluginStatus, ResourceStatus};
use smithagents_plugin_fixture::Fixture;
use smithagents_plugin_host::{
    CallError, CallHandle, InstallError, LinkedRuntime, PluginRuntime, Secret, SecretError,
    SecretSource, StaticSecrets, ToolFinder,
};
use smithagents_plugin_protocol::{
    HostApi, HostMode, Item, Manifest, Plugin, PluginError, PluginId, SyncPage, SyncRequest,
};
use smithagents_server::events::EventBus;
use smithagents_server::plugins::{
    LoadedPlugin, ManagerConfig, ManagerError, Origin, PluginManager, WallClock,
};
use smithagents_server::store::{PluginConfig, Store, StoreError};
use tokio::sync::{broadcast, Semaphore};
use tokio::time::Instant;

const SECOND: Duration = Duration::from_secs(1);
/// Tokio's timers tick in whole milliseconds and round a deadline up, so a
/// test that waits for the scheduler task's own timer moves the clock this
/// much past the due time.
const TICK: Duration = Duration::from_millis(1);

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

// ---------------------------------------------------------------------------
// Environment

/// Keeps the paused clock still while held (see the module comment).
struct Frozen {
    _release: std::sync::mpsc::Sender<()>,
}

fn freeze() -> Frozen {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    tokio::task::spawn_blocking(move || {
        let _ = rx.recv();
    });
    Frozen { _release: tx }
}

struct Env {
    store: Store,
    bus: EventBus,
    events: broadcast::Receiver<Event>,
    base: DateTime<Utc>,
    t0: Instant,
    _frozen: Frozen,
}

async fn env() -> Env {
    // Opened before pausing: the pool's connect has a timer of its own.
    let store = Store::open_in_memory().await.unwrap();
    tokio::time::pause();
    let frozen = freeze();
    let bus = EventBus::new(4096);
    let events = bus.subscribe(None).unwrap().live;
    Env {
        store,
        bus,
        events,
        base: "2026-11-26T10:00:00Z".parse().unwrap(),
        t0: Instant::now(),
        _frozen: frozen,
    }
}

impl Env {
    fn clock(&self) -> WallClock {
        let (base, t0) = (self.base, self.t0);
        Arc::new(move || base + TimeDelta::from_std(t0.elapsed()).unwrap())
    }

    /// The wall time `n` seconds after the test began.
    fn at(&self, n: u64) -> DateTime<Utc> {
        self.base + TimeDelta::seconds(n as i64)
    }

    /// The wall time now.
    fn now(&self) -> DateTime<Utc> {
        (self.clock())()
    }

    async fn start(&self, plugins: Vec<LoadedPlugin>) -> PluginManager {
        self.start_with(plugins, no_secrets()).await
    }

    async fn start_with(
        &self,
        plugins: Vec<LoadedPlugin>,
        secrets: Arc<dyn SecretSource>,
    ) -> PluginManager {
        self.start_cfg(plugins, secrets, ManagerConfig::default())
            .await
    }

    async fn start_cfg(
        &self,
        plugins: Vec<LoadedPlugin>,
        secrets: Arc<dyn SecretSource>,
        cfg: ManagerConfig,
    ) -> PluginManager {
        PluginManager::start_with_clock(
            self.store.clone(),
            self.bus.clone(),
            secrets,
            plugins,
            cfg,
            self.clock(),
        )
        .await
        .unwrap()
    }

    /// Every event published since the last call.
    fn drain(&mut self) -> Vec<EventKind> {
        let mut out = vec![];
        while let Ok(e) = self.events.try_recv() {
            out.push(e.kind);
        }
        out
    }

    async fn advance(&self, d: Duration) {
        tokio::time::advance(d).await;
    }
}

/// Yields until `cond` holds; the clock stays still meanwhile.
async fn until(what: &str, cond: impl Fn() -> bool) {
    for _ in 0..1_000_000 {
        if cond() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("gave up waiting until {what}");
}

/// Polls the plugin's status until `pred` holds; the clock stays still meanwhile.
async fn until_status(
    m: &PluginManager,
    id: &str,
    what: &str,
    pred: impl Fn(&PluginStatus) -> bool,
) {
    for _ in 0..100_000 {
        if pred(&status(m, id).await) {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("gave up waiting until {what}");
}

fn no_secrets() -> Arc<dyn SecretSource> {
    Arc::new(StaticSecrets(Default::default()))
}

fn secrets(pairs: &[(&str, &str, &str)]) -> Arc<dyn SecretSource> {
    Arc::new(StaticSecrets(
        pairs
            .iter()
            .map(|(p, k, v)| ((p.to_string(), k.to_string()), v.to_string()))
            .collect(),
    ))
}

/// A source that cannot resolve anything.
struct BrokenSecrets;

#[async_trait]
impl SecretSource for BrokenSecrets {
    async fn get(&self, _: &PluginId, _: &str) -> Result<Option<Secret>, SecretError> {
        Err(SecretError::NotUnicode {
            name: "SOMETHING".into(),
        })
    }
}

/// A manifest with one host, the given resources and `extra` TOML tables.
fn manifest(id: &str, resources: &[(&str, u32)], extra: &str) -> Manifest {
    let mut toml = format!(
        "id = \"{id}\"\nname = \"Test {id}\"\nversion = \"1.0.0\"\ncontract = 1\n\
         host = \"in-process\"\nhosts = [\"api.example.com\"]\n{extra}\n"
    );
    for (kind, interval) in resources {
        toml.push_str(&format!(
            "[[resources]]\nkind = \"{kind}\"\nlabel = \"Label {kind}\"\ninterval_secs = {interval}\n"
        ));
    }
    Manifest::parse(&toml).unwrap()
}

fn builtin(manifest: Manifest, runtime: Arc<dyn PluginRuntime>) -> LoadedPlugin {
    LoadedPlugin {
        manifest,
        origin: Origin::Builtin,
        runtime: Ok(runtime),
    }
}

fn installed(manifest: Manifest, runtime: Arc<dyn PluginRuntime>) -> LoadedPlugin {
    LoadedPlugin {
        origin: Origin::Installed,
        ..builtin(manifest, runtime)
    }
}

async fn status(m: &PluginManager, id: &str) -> PluginStatus {
    m.get(id).await.expect("plugin listed")
}

fn resource<'a>(s: &'a PluginStatus, kind: &str) -> &'a ResourceStatus {
    s.resources.iter().find(|r| r.kind == kind).unwrap()
}

fn ev(plugin: &str, resource: &str, upserted: u32, deleted: u32) -> (String, String, u32, u32) {
    (plugin.into(), resource.into(), upserted, deleted)
}

/// Every `ResourceSynced` as `(plugin, resource, upserted, deleted)`.
fn synced(events: &[EventKind]) -> Vec<(String, String, u32, u32)> {
    events
        .iter()
        .filter_map(|e| match e {
            EventKind::ResourceSynced {
                plugin_id,
                resource,
                upserted,
                deleted,
                ..
            } => Some((plugin_id.clone(), resource.clone(), *upserted, *deleted)),
            _ => None,
        })
        .collect()
}

fn updates<'a>(events: &'a [EventKind], id: &str) -> Vec<&'a PluginStatus> {
    events
        .iter()
        .filter_map(|e| match e {
            EventKind::PluginUpdated(s) if s.id == id => Some(s),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The fixture plugin, linked, with a call counter

struct Counted {
    calls: AtomicUsize,
}

impl Plugin for Counted {
    fn sync(&self, host: &dyn HostApi, req: SyncRequest) -> Result<SyncPage, PluginError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Fixture.sync(host, req)
    }
}

impl Counted {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// The fixture over `LinkedRuntime`. Its calls run on blocking threads, which
/// keep the paused clock still too, so the call timeout cannot fire early.
fn fixture() -> (Arc<Counted>, Arc<dyn PluginRuntime>) {
    let counted = Arc::new(Counted {
        calls: AtomicUsize::new(0),
    });
    let runtime = LinkedRuntime::new(counted.clone(), secs(60));
    (counted, Arc::new(runtime))
}

// ---------------------------------------------------------------------------
// A scripted plugin runtime: async replies, no threads

type Reply = Pin<Box<dyn Future<Output = Result<SyncPage, CallError>> + Send>>;
type Script = Box<dyn Fn(SyncRequest) -> Reply + Send + Sync>;

struct Scripted {
    mode: HostMode,
    script: Script,
    calls: Mutex<Vec<SyncRequest>>,
}

impl Scripted {
    fn new(script: impl Fn(SyncRequest) -> Reply + Send + Sync + 'static) -> Arc<Scripted> {
        Self::with_mode(HostMode::InProcess, script)
    }

    fn with_mode(
        mode: HostMode,
        script: impl Fn(SyncRequest) -> Reply + Send + Sync + 'static,
    ) -> Arc<Scripted> {
        Arc::new(Scripted {
            mode,
            script: Box::new(script),
            calls: Mutex::new(vec![]),
        })
    }

    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn requests(&self) -> Vec<SyncRequest> {
        self.calls.lock().unwrap().clone()
    }

    fn cursors(&self) -> Vec<Option<String>> {
        self.requests().into_iter().map(|r| r.cursor).collect()
    }

    fn runtime(self: &Arc<Self>) -> Arc<dyn PluginRuntime> {
        self.clone()
    }
}

#[async_trait]
impl PluginRuntime for Scripted {
    fn mode(&self) -> HostMode {
        self.mode
    }

    async fn sync(&self, _call: CallHandle, req: SyncRequest) -> Result<SyncPage, CallError> {
        self.calls.lock().unwrap().push(req.clone());
        let page = (self.script)(req).await?;
        page.validate()?;
        Ok(page)
    }
}

fn items(range: Range<u32>) -> Vec<Item> {
    range
        .map(|n| Item {
            id: format!("item-{n}"),
            title: format!("Item {n}"),
            url: None,
            updated_at: None,
            version: None,
            fields: json!({}),
        })
        .collect()
}

fn page(upserts: Vec<Item>, next: Option<&str>, done: bool) -> SyncPage {
    SyncPage {
        upserts,
        deletes: vec![],
        next_cursor: next.map(Into::into),
        done,
        snapshot_complete: done,
    }
}

fn ok(p: SyncPage) -> Reply {
    Box::pin(async move { Ok(p) })
}

fn fail(e: CallError) -> Reply {
    Box::pin(async move { Err(e) })
}

fn other(message: &str) -> CallError {
    CallError::Plugin(PluginError::Other {
        message: message.into(),
    })
}

/// Three pages of two items: cursors "1" and "2", the last page done with no
/// cursor, so the next run starts over.
fn three_pages(cursor: Option<&str>) -> SyncPage {
    match cursor {
        None => page(items(1..3), Some("1"), false),
        Some("1") => page(items(3..5), Some("2"), false),
        _ => page(items(5..7), None, true),
    }
}

/// Set if a call's future is dropped before it finished: the manager must
/// never cancel a call.
#[derive(Clone, Default)]
struct DropWatch {
    dropped: Arc<AtomicBool>,
    finished: Arc<AtomicUsize>,
}

struct CallGuard {
    watch: DropWatch,
    done: bool,
}

impl DropWatch {
    fn guard(&self) -> CallGuard {
        CallGuard {
            watch: self.clone(),
            done: false,
        }
    }

    fn dropped(&self) -> bool {
        self.dropped.load(Ordering::SeqCst)
    }

    fn finished(&self) -> usize {
        self.finished.load(Ordering::SeqCst)
    }
}

impl CallGuard {
    fn done(mut self) {
        self.done = true;
        self.watch.finished.fetch_add(1, Ordering::SeqCst);
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        if !self.done {
            self.watch.dropped.store(true, Ordering::SeqCst);
        }
    }
}

/// A reply that waits for one permit of `gate` before answering `p`.
fn gated(gate: &Arc<Semaphore>, watch: &DropWatch, p: SyncPage) -> Reply {
    let (gate, guard) = (gate.clone(), watch.guard());
    Box::pin(async move {
        gate.acquire().await.unwrap().forget();
        guard.done();
        Ok(p)
    })
}

// ---------------------------------------------------------------------------
// Runs

#[tokio::test]
async fn a_first_run_stores_six_items_and_announces_them() {
    let mut env = env().await;
    let (fixture, runtime) = fixture();
    let m = env
        .start(vec![builtin(
            manifest("fixture", &[("items", 60)], ""),
            runtime,
        )])
        .await;
    m.run_due().await;

    assert_eq!(fixture.calls(), 3);
    assert_eq!(env.store.count_items("fixture", "items").await.unwrap(), 6);
    let events = env.drain();
    assert_eq!(synced(&events), [ev("fixture", "items", 6, 0)]);
    let last = *updates(&events, "fixture").last().expect("a PluginUpdated");
    assert_eq!(last.state, PluginState::Ready);
    assert_eq!(resource(last, "items").item_count, 6);
    assert!(!resource(last, "items").syncing);

    let s = status(&m, "fixture").await;
    let r = resource(&s, "items");
    assert_eq!(r.last_run_at, Some(env.at(0)));
    assert_eq!(r.last_ok_at, Some(env.at(0)));
    assert_eq!(r.next_due_at, Some(env.at(60)));
    assert_eq!(r.last_error, None);
    let state = env.store.sync_state("fixture", "items").await.unwrap();
    assert_eq!(state.cursor.as_deref(), Some("3"));
    assert_eq!(state.failures, 0);
}

#[tokio::test]
async fn a_run_that_changes_nothing_announces_nothing() {
    // A plugin that lists everything again from no cursor each time.
    let mut env = env().await;
    let rt = Scripted::new(|req| ok(three_pages(req.cursor.as_deref())));
    let m = env
        .start(vec![builtin(
            manifest("again", &[("items", 60)], ""),
            rt.runtime(),
        )])
        .await;
    m.run_due().await;
    assert_eq!(synced(&env.drain()), [ev("again", "items", 6, 0)]);

    env.advance(secs(59)).await;
    m.run_due().await;
    assert_eq!(rt.calls(), 3, "not due before its interval");

    env.advance(SECOND).await;
    m.run_due().await;
    assert_eq!(rt.calls(), 6);
    let events = env.drain();
    assert!(synced(&events).is_empty(), "{events:?}");
    assert!(!updates(&events, "again").is_empty());
    assert_eq!(env.store.count_items("again", "items").await.unwrap(), 6);
}

#[tokio::test]
async fn the_fixtures_second_run_keeps_its_six_items_and_announces_nothing() {
    let mut env = env().await;
    let (fixture, runtime) = fixture();
    let m = env
        .start(vec![builtin(
            manifest("fixture", &[("items", 60)], ""),
            runtime,
        )])
        .await;
    m.run_due().await;
    assert_eq!(synced(&env.drain()), [ev("fixture", "items", 6, 0)]);

    env.advance(secs(60)).await;
    m.run_due().await;
    assert_eq!(fixture.calls(), 4);
    let events = env.drain();
    assert!(synced(&events).is_empty(), "{events:?}");
    assert_eq!(env.store.count_items("fixture", "items").await.unwrap(), 6);
    let state = env.store.sync_state("fixture", "items").await.unwrap();
    assert_eq!(state.cursor.as_deref(), Some("3"));
    assert_eq!(state.last_error, None);
}

#[tokio::test]
async fn forever_stops_at_its_second_page_because_the_cursor_did_not_advance() {
    let env = env().await;
    let (fixture, runtime) = fixture();
    let m = env
        .start(vec![builtin(
            manifest("fixture", &[("forever", 60)], ""),
            runtime,
        )])
        .await;
    m.run_due().await;

    assert_eq!(fixture.calls(), 2);
    let s = status(&m, "fixture").await;
    let r = resource(&s, "forever");
    assert_eq!(
        r.last_error.as_deref(),
        Some("the plugin did not advance its cursor")
    );
    assert_eq!(r.next_due_at, Some(env.at(30)));
    let state = env.store.sync_state("fixture", "forever").await.unwrap();
    assert_eq!(state.cursor.as_deref(), Some("again"));
    assert_eq!(state.failures, 1);
}

#[tokio::test]
async fn a_run_stops_after_twenty_pages_and_keeps_its_cursor() {
    let env = env().await;
    let rt = Scripted::new(|req| {
        let n: u32 = req.cursor.as_deref().map_or(0, |c| c.parse().unwrap());
        let next = (n + 1).to_string();
        ok(page(items(n..n + 1), Some(&next), false))
    });
    let m = env
        .start(vec![builtin(
            manifest("endless", &[("items", 300)], ""),
            rt.runtime(),
        )])
        .await;
    m.run_due().await;

    assert_eq!(rt.calls(), 20);
    assert_eq!(env.store.count_items("endless", "items").await.unwrap(), 20);
    let state = env.store.sync_state("endless", "items").await.unwrap();
    assert_eq!(state.cursor.as_deref(), Some("20"));
    assert_eq!(state.failures, 0);
    assert_eq!(state.last_error, None);
    assert_eq!(state.next_due_at, Some(env.at(300)));

    env.advance(secs(299)).await;
    m.run_due().await;
    assert_eq!(rt.calls(), 20);
    env.advance(SECOND).await;
    m.run_due().await;
    assert_eq!(rt.calls(), 40);
    assert_eq!(rt.cursors()[20].as_deref(), Some("20"));
}

#[tokio::test]
async fn a_snapshot_that_spans_runs_deletes_only_what_the_whole_listing_missed() {
    let mut env = env().await;
    let without_first = Arc::new(AtomicBool::new(false));
    // 25 pages of one item, the last done with a complete snapshot.
    let rt = {
        let without_first = without_first.clone();
        Scripted::new(move |req| {
            let n: u32 = req.cursor.as_deref().map_or(0, |c| c.parse().unwrap());
            let upserts = if n == 0 && without_first.load(Ordering::SeqCst) {
                vec![]
            } else {
                items(n..n + 1)
            };
            let next = (n + 1).to_string();
            ok(match n {
                24 => page(upserts, None, true),
                _ => page(upserts, Some(&next), false),
            })
        })
    };
    let m = env
        .start(vec![builtin(
            manifest("big", &[("items", 60)], ""),
            rt.runtime(),
        )])
        .await;
    let store = env.store.clone();
    let count = || store.count_items("big", "items");
    m.run_due().await;
    assert_eq!(count().await.unwrap(), 20);
    env.advance(secs(60)).await;
    m.run_due().await;
    assert_eq!(rt.calls(), 25);
    assert_eq!(count().await.unwrap(), 25, "the first run's items are kept");

    without_first.store(true, Ordering::SeqCst);
    env.drain();
    for _ in 0..2 {
        env.advance(secs(60)).await;
        m.run_due().await;
    }
    assert_eq!(rt.calls(), 50);
    assert_eq!(count().await.unwrap(), 24);
    assert_eq!(synced(&env.drain()), [ev("big", "items", 0, 1)]);
    let ids: Vec<String> = m
        .items("big", "items", 200, None, None)
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|i| i.id)
        .collect();
    assert!(!ids.contains(&"item-0".to_string()), "{ids:?}");
}

#[tokio::test]
async fn an_oversized_page_stores_nothing_and_records_a_contract_error() {
    let mut env = env().await;
    let (_, runtime) = fixture();
    let m = env
        .start(vec![builtin(
            manifest("fixture", &[("huge", 60)], ""),
            runtime,
        )])
        .await;
    m.run_due().await;

    assert_eq!(env.store.count_items("fixture", "huge").await.unwrap(), 0);
    let s = status(&m, "fixture").await;
    let error = resource(&s, "huge").last_error.clone().unwrap();
    assert!(
        error.starts_with("the plugin broke the contract"),
        "{error}"
    );
    assert_eq!(s.state, PluginState::Ready);
    assert!(synced(&env.drain()).is_empty());
}

#[tokio::test]
async fn an_error_on_page_three_keeps_the_first_two_pages_and_their_cursor() {
    let mut env = env().await;
    let rt = Scripted::new(|req| match req.cursor.as_deref() {
        Some("2") => fail(CallError::Plugin(PluginError::Http {
            message: "502 from upstream".into(),
        })),
        cursor => ok(three_pages(cursor)),
    });
    let m = env
        .start(vec![builtin(
            manifest("flaky", &[("items", 60)], ""),
            rt.runtime(),
        )])
        .await;
    m.run_due().await;

    assert_eq!(rt.calls(), 3);
    assert_eq!(env.store.count_items("flaky", "items").await.unwrap(), 4);
    let state = env.store.sync_state("flaky", "items").await.unwrap();
    assert_eq!(state.cursor.as_deref(), Some("2"));
    assert_eq!(state.failures, 1);
    assert_eq!(state.last_error.as_deref(), Some("http: 502 from upstream"));
    assert_eq!(synced(&env.drain()), [ev("flaky", "items", 4, 0)]);

    // The retry starts from the stored cursor.
    env.advance(secs(30)).await;
    m.run_due().await;
    assert_eq!(rt.cursors()[3].as_deref(), Some("2"));
}

#[tokio::test]
async fn backoff_doubles_from_thirty_seconds_and_caps_at_thirty_minutes() {
    let env = env().await;
    let rt = Scripted::new(|_| fail(other("down")));
    let m = env
        .start(vec![builtin(
            manifest("down", &[("items", 60)], ""),
            rt.runtime(),
        )])
        .await;
    m.run_due().await;
    assert_eq!(rt.calls(), 1);

    let delays = [30, 60, 120, 240, 480, 960, 1800, 1800, 1800];
    for (i, delay) in delays.into_iter().enumerate() {
        let state = env.store.sync_state("down", "items").await.unwrap();
        assert_eq!(state.failures, i as u32 + 1);
        assert_eq!(
            state.next_due_at,
            Some(env.now() + TimeDelta::seconds(delay)),
            "failure {}",
            i + 1
        );
        assert_eq!(state.last_error.as_deref(), Some("down"));

        env.advance(secs(delay as u64) - SECOND).await;
        m.run_due().await;
        assert_eq!(rt.calls(), i + 1, "ran early after failure {}", i + 1);
        env.advance(SECOND).await;
        m.run_due().await;
        assert_eq!(rt.calls(), i + 2, "did not run after failure {}", i + 1);
    }
}

#[tokio::test]
async fn a_rate_limit_waits_for_the_longer_of_retry_after_and_the_interval() {
    let env = env().await;
    let (short, short_rt) = fixture();
    let (long, long_rt) = fixture();
    let m = env
        .start(vec![
            builtin(manifest("rate-short", &[("rate", 60)], ""), short_rt),
            builtin(manifest("rate-long", &[("rate", 300)], ""), long_rt),
        ])
        .await;
    m.run_due().await;
    assert_eq!((short.calls(), long.calls()), (1, 1));

    for (id, due) in [("rate-short", 120), ("rate-long", 300)] {
        let s = status(&m, id).await;
        assert_eq!(s.state, PluginState::Ready, "{id}");
        let r = resource(&s, "rate");
        assert_eq!(r.next_due_at, Some(env.at(due)), "{id}");
        let error = r.last_error.as_deref().unwrap();
        assert!(error.starts_with("rate limited"), "{error}");
        let state = env.store.sync_state(id, "rate").await.unwrap();
        assert_eq!(state.failures, 0, "{id}");
    }

    env.advance(secs(119)).await;
    m.run_due().await;
    assert_eq!((short.calls(), long.calls()), (1, 1));
    env.advance(SECOND).await;
    m.run_due().await;
    assert_eq!((short.calls(), long.calls()), (2, 1));
    env.advance(secs(179)).await;
    m.run_due().await;
    assert_eq!(long.calls(), 1);
    env.advance(SECOND).await;
    m.run_due().await;
    assert_eq!(long.calls(), 2);
}

#[tokio::test]
async fn an_auth_error_needs_auth_and_stops_runs_until_the_settings_change() {
    let env = env().await;
    let (fixture, runtime) = fixture();
    let m = env
        .start(vec![builtin(
            manifest("fixture", &[("auth", 60)], ""),
            runtime,
        )])
        .await;
    m.run_due().await;
    assert_eq!(fixture.calls(), 1);
    assert_eq!(
        status(&m, "fixture").await.state,
        PluginState::NeedsAuth {
            missing: vec![],
            message: "the token was rejected".into()
        }
    );

    env.advance(secs(3600)).await;
    m.run_due().await;
    assert_eq!(fixture.calls(), 1, "no retry while it needs auth");

    let s = m.set_settings("fixture", json!({})).await.unwrap();
    assert_eq!(s.state, PluginState::Ready);
    m.run_due().await;
    assert_eq!(fixture.calls(), 2, "settings changed: due at once");
    assert!(matches!(
        status(&m, "fixture").await.state,
        PluginState::NeedsAuth { .. }
    ));

    m.sync_now("fixture", "auth").await.unwrap();
    m.run_due().await;
    assert_eq!(fixture.calls(), 3, "sync_now retries");

    m.set_enabled("fixture", true).await.unwrap();
    m.run_due().await;
    assert_eq!(fixture.calls(), 4, "enabling retries");
}

#[tokio::test]
async fn a_settings_error_from_the_plugin_needs_settings_until_they_change() {
    let env = env().await;
    let rt = Scripted::new(|_| {
        fail(CallError::Plugin(PluginError::Settings {
            message: "repository acme/x not found".into(),
        }))
    });
    let m = env
        .start(vec![builtin(
            manifest("picky", &[("items", 60)], ""),
            rt.runtime(),
        )])
        .await;
    m.run_due().await;
    assert_eq!(
        status(&m, "picky").await.state,
        PluginState::NeedsSettings {
            message: "repository acme/x not found".into()
        }
    );
    env.advance(secs(600)).await;
    m.run_due().await;
    assert_eq!(rt.calls(), 1);
    match m.sync_now("picky", "items").await {
        Err(ManagerError::InvalidSettings(message)) => {
            assert_eq!(message, "repository acme/x not found")
        }
        other => panic!("expected InvalidSettings, got {other:?}"),
    }
    m.set_settings("picky", json!({})).await.unwrap();
    m.run_due().await;
    assert_eq!(rt.calls(), 2);
}

#[tokio::test]
async fn a_missing_secret_needs_auth_naming_it_and_the_plugin_is_not_called() {
    let mut env = env().await;
    let extra = "[[secrets]]\nkey = \"token\"\nlabel = \"Token\"\n\
                 [[secrets]]\nkey = \"other\"\nlabel = \"Other\"\n";
    let rt = Scripted::new(|req| ok(three_pages(req.cursor.as_deref())));
    let m = env
        .start_with(
            vec![builtin(
                manifest("signed", &[("items", 60)], extra),
                rt.runtime(),
            )],
            secrets(&[("signed", "other", "s3cr3t-value")]),
        )
        .await;
    m.run_due().await;

    assert_eq!(rt.calls(), 0);
    let s = status(&m, "signed").await;
    match &s.state {
        PluginState::NeedsAuth { missing, message } => {
            assert_eq!(missing, &["token".to_string()]);
            assert!(message.contains("token"), "{message}");
        }
        other => panic!("expected NeedsAuth, got {other:?}"),
    }
    assert_eq!(s.secrets, ["token", "other"]);
    // The last event says so too, and that nothing syncs any more.
    let events = env.drain();
    let last = *updates(&events, "signed").last().expect("a PluginUpdated");
    assert_eq!(last.state, s.state);
    assert!(!resource(last, "items").syncing, "{last:?}");

    // `sync_now` asks again, and ends the same way.
    m.sync_now("signed", "items").await.unwrap();
    m.run_due().await;
    assert_eq!(rt.calls(), 0);
    let events = env.drain();
    let last = *updates(&events, "signed").last().expect("a PluginUpdated");
    assert!(matches!(last.state, PluginState::NeedsAuth { .. }));
    assert!(!resource(last, "items").syncing, "{last:?}");
    m.shutdown().await;

    // A source that fails counts every key as missing; its error is not shown.
    let m = env
        .start_with(
            vec![builtin(
                manifest("signed", &[("items", 60)], extra),
                rt.runtime(),
            )],
            Arc::new(BrokenSecrets),
        )
        .await;
    m.run_due().await;
    assert_eq!(rt.calls(), 0);
    match status(&m, "signed").await.state {
        PluginState::NeedsAuth { missing, message } => {
            assert_eq!(missing, ["token", "other"]);
            assert!(!message.contains("SOMETHING"), "{message}");
        }
        other => panic!("expected NeedsAuth, got {other:?}"),
    }
    m.shutdown().await;

    // With every secret present the plugin runs.
    let m = env
        .start_with(
            vec![builtin(
                manifest("signed", &[("items", 60)], extra),
                rt.runtime(),
            )],
            secrets(&[
                ("signed", "token", "s3cr3t-value"),
                ("signed", "other", "s3cr3t-value"),
            ]),
        )
        .await;
    m.run_due().await;
    assert_eq!(rt.calls(), 3);
    assert_eq!(status(&m, "signed").await.state, PluginState::Ready);
}

#[tokio::test]
async fn the_missing_secrets_message_is_capped_too() {
    let mut env = env().await;
    let keys: Vec<String> = (0..30)
        .map(|n| format!("k{n:02}{}", "a".repeat(61)))
        .collect();
    let extra: String = keys
        .iter()
        .map(|k| format!("[[secrets]]\nkey = \"{k}\"\nlabel = \"Key\"\n"))
        .collect();
    let rt = Scripted::new(|_| ok(page(vec![], None, true)));
    let m = env
        .start(vec![builtin(
            manifest("many", &[("items", 60)], &extra),
            rt.runtime(),
        )])
        .await;
    m.run_due().await;
    assert_eq!(rt.calls(), 0);
    let events = env.drain();
    let last = *updates(&events, "many").last().expect("a PluginUpdated");
    for state in [&status(&m, "many").await.state, &last.state] {
        let PluginState::NeedsAuth { missing, message } = state else {
            panic!("expected NeedsAuth, got {state:?}");
        };
        assert_eq!(missing, &keys);
        assert!(message.len() <= 1024, "{} bytes", message.len());
        assert!(message.ends_with('…'), "{message}");
    }
}

#[tokio::test]
async fn no_secret_value_reaches_a_status_or_an_event() {
    let mut env = env().await;
    let extra = "[[secrets]]\nkey = \"token\"\nlabel = \"Token\"\n";
    let rt = Scripted::new(|req| match req.cursor.as_deref() {
        Some("2") => fail(CallError::Plugin(PluginError::Auth {
            message: "rejected".into(),
        })),
        cursor => ok(three_pages(cursor)),
    });
    let m = env
        .start_with(
            vec![builtin(
                manifest("signed", &[("items", 60)], extra),
                rt.runtime(),
            )],
            secrets(&[("signed", "token", "s3cr3t-value")]),
        )
        .await;
    m.run_due().await;
    m.set_settings("signed", json!({})).await.unwrap();
    m.run_due().await;
    let mut text = serde_json::to_string(&m.list().await).unwrap();
    for e in env.drain() {
        text.push_str(&serde_json::to_string(&e).unwrap());
    }
    assert!(text.contains("rejected"));
    assert!(!text.contains("s3cr3t"), "{text}");
}

// ---------------------------------------------------------------------------
// Messages

#[tokio::test]
async fn messages_are_capped_at_one_kib_and_carry_no_control_characters() {
    let env = env().await;
    let long = format!("bad\nline\u{1b}[31m\u{202e}{}", "é".repeat(3000));
    let message = long.clone();
    let rt = Scripted::new(move |_| {
        fail(CallError::Plugin(PluginError::Auth {
            message: message.clone(),
        }))
    });
    let m = env
        .start(vec![builtin(
            manifest("loud", &[("items", 60)], ""),
            rt.runtime(),
        )])
        .await;
    m.run_due().await;

    let s = status(&m, "loud").await;
    let PluginState::NeedsAuth { message, .. } = &s.state else {
        panic!("expected NeedsAuth, got {:?}", s.state);
    };
    let error = resource(&s, "items").last_error.clone().unwrap();
    for text in [message.as_str(), error.as_str()] {
        assert!(text.len() <= 1024, "{} bytes", text.len());
        assert!(text.ends_with('…'), "{text}");
        assert!(!text.chars().any(|c| c.is_control()), "{text:?}");
        assert!(!text.contains('\u{202e}'), "{text:?}");
    }
    assert!(message.starts_with("bad line [31m"), "{message}");
    let stored = env.store.sync_state("loud", "items").await.unwrap();
    assert_eq!(stored.last_error.as_deref(), Some(error.as_str()));
}

// ---------------------------------------------------------------------------
// Failures of the runtime

#[tokio::test]
async fn a_timeout_in_dylib_mode_fails_the_plugin_and_stops_its_schedule() {
    let env = env().await;
    let rt = Scripted::with_mode(HostMode::Dylib, |_| fail(CallError::Timeout));
    let m = env
        .start(vec![builtin(
            manifest("stuck", &[("items", 60)], ""),
            rt.runtime(),
        )])
        .await;
    m.run_due().await;
    let s = status(&m, "stuck").await;
    let PluginState::Failed { message } = &s.state else {
        panic!("expected Failed, got {:?}", s.state);
    };
    assert!(message.contains("timed out"), "{message}");
    assert_eq!(
        resource(&s, "items").last_error.as_deref(),
        Some("the plugin call timed out")
    );

    env.advance(secs(7200)).await;
    m.run_due().await;
    m.set_settings("stuck", json!({})).await.unwrap();
    m.set_enabled("stuck", true).await.unwrap();
    m.run_due().await;
    assert_eq!(rt.calls(), 1, "a stopped plugin is not called again");
    assert!(matches!(
        m.sync_now("stuck", "items").await,
        Err(ManagerError::Disabled)
    ));
    assert!(matches!(
        status(&m, "stuck").await.state,
        PluginState::Failed { .. }
    ));
}

#[tokio::test]
async fn a_linked_timeout_fails_the_plugin_but_a_wasm_timeout_only_backs_off() {
    let env = env().await;
    let linked = Scripted::with_mode(HostMode::InProcess, |_| fail(CallError::Timeout));
    let wasm = Scripted::with_mode(HostMode::Wasm, |_| fail(CallError::Timeout));
    let m = env
        .start(vec![
            builtin(manifest("linked", &[("items", 60)], ""), linked.runtime()),
            builtin(manifest("sandboxed", &[("items", 60)], ""), wasm.runtime()),
        ])
        .await;
    m.run_due().await;

    let PluginState::Failed { message } = status(&m, "linked").await.state else {
        panic!("expected Failed");
    };
    assert!(
        message.starts_with("the plugin call timed out"),
        "{message}"
    );
    let s = status(&m, "sandboxed").await;
    assert_eq!(s.state, PluginState::Ready);
    let r = resource(&s, "items");
    assert_eq!(r.last_error.as_deref(), Some("the plugin call timed out"));
    assert_eq!(r.next_due_at, Some(env.at(30)));

    env.advance(secs(30)).await;
    m.run_due().await;
    assert_eq!((linked.calls(), wasm.calls()), (1, 2));
}

#[tokio::test]
async fn a_crash_fails_a_dylib_plugin_but_only_backs_off_a_linked_one() {
    let env = env().await;
    let crash = || {
        fail(CallError::Crashed {
            message: "boom".into(),
        })
    };
    let native = Scripted::with_mode(HostMode::Dylib, move |_| crash());
    let linked = Scripted::new(move |_| crash());
    let m = env
        .start(vec![
            builtin(manifest("native", &[("items", 60)], ""), native.runtime()),
            builtin(manifest("linked", &[("items", 60)], ""), linked.runtime()),
        ])
        .await;
    m.run_due().await;

    let PluginState::Failed { message } = status(&m, "native").await.state else {
        panic!("expected Failed");
    };
    assert!(message.starts_with("the plugin crashed: boom"), "{message}");
    let s = status(&m, "linked").await;
    assert_eq!(s.state, PluginState::Ready);
    assert_eq!(
        resource(&s, "items").last_error.as_deref(),
        Some("the plugin crashed: boom")
    );
    env.advance(secs(30)).await;
    m.run_due().await;
    assert_eq!((native.calls(), linked.calls()), (1, 2));
}

#[tokio::test]
async fn a_plugin_that_failed_to_load_is_listed_and_never_runs() {
    let env = env().await;
    let m = env
        .start(vec![LoadedPlugin {
            manifest: manifest("broken", &[("items", 60)], ""),
            origin: Origin::Installed,
            runtime: Err("cannot load:\nbad module".into()),
        }])
        .await;
    m.set_enabled("broken", true).await.unwrap();
    m.run_due().await;
    let s = status(&m, "broken").await;
    assert_eq!(
        s.state,
        PluginState::LoadFailed {
            message: "cannot load: bad module".into()
        }
    );
    assert!(s.enabled);
    assert!(!s.builtin);
    assert!(matches!(
        m.sync_now("broken", "items").await,
        Err(ManagerError::Disabled)
    ));
}

async fn panics() -> Result<SyncPage, CallError> {
    panic!("scripted panic")
}

#[tokio::test]
async fn a_plugin_that_panics_does_not_stop_the_others_schedule() {
    let env = env().await;
    let panicky = Scripted::new(|_| Box::pin(panics()));
    let steady = Scripted::new(|_| ok(page(items(0..1), None, true)));
    let m = env
        .start(vec![
            builtin(manifest("panicky", &[("items", 60)], ""), panicky.runtime()),
            builtin(manifest("steady", &[("items", 60)], ""), steady.runtime()),
        ])
        .await;

    // Driven by the manager's own scheduler: each round's run is seen done
    // before `run_due` is called, which then only waits for the panicking
    // plugin's run to end, so the clock never moves under a store query.
    for round in 0..4u64 {
        let expected = Some(env.now());
        until_status(&m, "steady", "steady ran", |s| {
            let r = resource(s, "items");
            r.last_ok_at == expected && !r.syncing
        })
        .await;
        assert_eq!(steady.calls(), round as usize + 1);
        m.run_due().await;
        if round < 3 {
            env.advance(secs(60) + TICK).await;
        }
    }
    until_status(&m, "panicky", "the panic is recorded", |s| {
        let r = resource(s, "items");
        !r.syncing && r.last_error.as_deref() == Some("the sync panicked: scripted panic")
    })
    .await;
    assert_eq!(steady.calls(), 4);
    assert!(panicky.calls() >= 2, "the panicking plugin is retried");
    assert_eq!(status(&m, "panicky").await.state, PluginState::Ready);
    m.shutdown().await;
}

// ---------------------------------------------------------------------------
// Start: configuration, upgrades and restarts

#[tokio::test]
async fn start_creates_the_configuration_with_defaults_and_origin() {
    let env = env().await;
    let extra =
        "[[settings]]\nkey = \"org\"\nlabel = \"Org\"\nkind = \"text\"\ndefault = \"acme\"\n\
                 [[settings]]\nkey = \"repos\"\nlabel = \"Repos\"\nkind = \"list\"\n";
    let rt = Scripted::new(|_| ok(page(vec![], None, true)));
    let m = env
        .start(vec![
            builtin(manifest("shipped", &[("items", 60)], extra), rt.runtime()),
            installed(manifest("added", &[("items", 60)], extra), rt.runtime()),
        ])
        .await;
    assert_eq!(
        env.store.plugin_config("shipped").await.unwrap(),
        Some(PluginConfig {
            enabled: true,
            settings: json!({"org": "acme"})
        })
    );
    assert!(
        !env.store
            .plugin_config("added")
            .await
            .unwrap()
            .unwrap()
            .enabled
    );
    assert_eq!(m.settings("shipped").await.unwrap(), json!({"org": "acme"}));

    let list = m.list().await;
    assert_eq!(
        list.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["added", "shipped"]
    );
    let s = &list[1];
    assert!(s.builtin && s.enabled);
    assert_eq!(s.name, "Test shipped");
    assert_eq!(s.version, "1.0.0");
    assert_eq!(s.host, "in-process");
    assert_eq!(s.hosts, ["api.example.com"]);
    assert_eq!(
        s.settings
            .iter()
            .map(|f| (f.key.as_str(), f.kind.as_str(), f.required))
            .collect::<Vec<_>>(),
        [("org", "text", false), ("repos", "list", false)]
    );
    assert_eq!(resource(s, "items").label, "Label items");
    assert_eq!(list[0].state, PluginState::Disabled);
    assert!(m.get("nope").await.is_none());

    // An existing configuration is kept.
    m.set_settings("shipped", json!({"org": "other"}))
        .await
        .unwrap();
    m.set_enabled("shipped", false).await.unwrap();
    m.shutdown().await;
    let m = env
        .start(vec![builtin(
            manifest("shipped", &[("items", 60)], extra),
            rt.runtime(),
        )])
        .await;
    assert_eq!(
        m.settings("shipped").await.unwrap(),
        json!({"org": "other"})
    );
    assert_eq!(status(&m, "shipped").await.state, PluginState::Disabled);
}

#[tokio::test]
async fn a_resource_removed_from_the_manifest_loses_its_items_at_the_next_start() {
    let env = env().await;
    let rt = Scripted::new(|_| ok(page(items(0..2), None, true)));
    let m = env
        .start(vec![builtin(
            manifest("shrinks", &[("kept", 60), ("dropped", 60)], ""),
            rt.runtime(),
        )])
        .await;
    m.run_due().await;
    assert_eq!(
        env.store.count_items("shrinks", "dropped").await.unwrap(),
        2
    );
    m.shutdown().await;

    let m = env
        .start(vec![builtin(
            manifest("shrinks", &[("kept", 60)], ""),
            rt.runtime(),
        )])
        .await;
    assert_eq!(
        env.store.count_items("shrinks", "dropped").await.unwrap(),
        0
    );
    assert_eq!(env.store.count_items("shrinks", "kept").await.unwrap(), 2);
    assert_eq!(
        env.store.known_resources("shrinks").await.unwrap(),
        ["kept"]
    );
    assert_eq!(
        status(&m, "shrinks")
            .await
            .resources
            .iter()
            .map(|r| r.kind.as_str())
            .collect::<Vec<_>>(),
        ["kept"]
    );
    assert!(matches!(
        m.items("shrinks", "dropped", 10, None, None).await,
        Err(ManagerError::UnknownResource)
    ));
}

#[tokio::test]
async fn settings_that_no_longer_validate_need_settings_and_nothing_runs() {
    let env = env().await;
    let v1 = "[[settings]]\nkey = \"org\"\nlabel = \"Org\"\nkind = \"text\"\n";
    let v2 = "[[settings]]\nkey = \"repos\"\nlabel = \"Repos\"\nkind = \"list\"\nrequired = true\n";
    let rt = Scripted::new(|_| ok(page(vec![], None, true)));
    let m = env
        .start(vec![builtin(
            manifest("upgraded", &[("items", 60)], v1),
            rt.runtime(),
        )])
        .await;
    m.set_settings("upgraded", json!({"org": "acme"}))
        .await
        .unwrap();
    m.run_due().await;
    let ran = rt.calls();
    assert!(ran >= 1);
    m.shutdown().await;

    let m = env
        .start(vec![builtin(
            manifest("upgraded", &[("items", 60)], v2),
            rt.runtime(),
        )])
        .await;
    let PluginState::NeedsSettings { message } = status(&m, "upgraded").await.state else {
        panic!("expected NeedsSettings");
    };
    assert_eq!(message, "unknown setting \"org\"");
    assert_eq!(
        m.settings("upgraded").await.unwrap(),
        json!({"org": "acme"})
    );

    env.advance(secs(3600)).await;
    m.run_due().await;
    match m.sync_now("upgraded", "items").await {
        Err(ManagerError::InvalidSettings(message)) => {
            assert_eq!(message, "unknown setting \"org\"")
        }
        other => panic!("expected InvalidSettings, got {other:?}"),
    }
    m.run_due().await;
    assert_eq!(
        rt.calls(),
        ran,
        "never called with settings that do not validate"
    );

    let s = m
        .set_settings("upgraded", json!({"repos": ["acme/x"]}))
        .await
        .unwrap();
    assert_eq!(s.state, PluginState::Ready);
    m.run_due().await;
    assert_eq!(rt.calls(), ran + 1);
    assert_eq!(rt.requests()[ran].settings, json!({"repos": ["acme/x"]}));
}

#[tokio::test]
async fn invalid_settings_are_refused_and_change_nothing() {
    let env = env().await;
    let extra =
        "[[settings]]\nkey = \"repos\"\nlabel = \"Repos\"\nkind = \"list\"\nrequired = true\n";
    let rt = Scripted::new(|_| ok(page(vec![], None, true)));
    let m = env
        .start(vec![builtin(
            manifest("strict", &[("items", 60)], extra),
            rt.runtime(),
        )])
        .await;
    m.set_settings("strict", json!({"repos": ["a/b"]}))
        .await
        .unwrap();
    m.run_due().await;
    let before = status(&m, "strict").await;

    for (bad, why) in [
        (
            json!({"repos": "a/b"}),
            "setting \"repos\" must be of kind List",
        ),
        (json!([1]), "settings must be an object"),
        (json!({}), "missing required setting \"repos\""),
        (json!({"repos": [], "x": 1}), "unknown setting \"x\""),
    ] {
        match m.set_settings("strict", bad).await {
            Err(ManagerError::InvalidSettings(message)) => assert_eq!(message, why),
            other => panic!("expected InvalidSettings, got {other:?}"),
        }
    }
    assert_eq!(
        m.settings("strict").await.unwrap(),
        json!({"repos": ["a/b"]})
    );
    assert_eq!(
        env.store
            .plugin_config("strict")
            .await
            .unwrap()
            .unwrap()
            .settings,
        json!({"repos": ["a/b"]})
    );
    assert_eq!(status(&m, "strict").await, before);
    assert!(matches!(
        m.set_settings("nope", json!({})).await,
        Err(ManagerError::UnknownPlugin)
    ));
    assert!(matches!(
        m.settings("nope").await,
        Err(ManagerError::UnknownPlugin)
    ));
}

#[tokio::test]
async fn a_restarted_manager_resumes_from_the_stored_cursor() {
    let env = env().await;
    let gate = Arc::new(Semaphore::new(0));
    let watch = DropWatch::default();
    let first = {
        let (gate, watch) = (gate.clone(), watch.clone());
        Scripted::new(move |req| match req.cursor.as_deref() {
            None => ok(three_pages(None)),
            cursor => gated(&gate, &watch, three_pages(cursor)),
        })
    };
    let m = env
        .start(vec![builtin(
            manifest("resumes", &[("items", 60)], ""),
            first.runtime(),
        )])
        .await;
    until("page 2 is asked for", || first.calls() == 2).await;
    shut_down(&env, &m).await;
    assert!(!watch.dropped(), "shutdown cancelled a call");
    assert_eq!(env.store.count_items("resumes", "items").await.unwrap(), 2);

    let second = Scripted::new(|req| ok(three_pages(req.cursor.as_deref())));
    let m = env
        .start(vec![builtin(
            manifest("resumes", &[("items", 60)], ""),
            second.runtime(),
        )])
        .await;
    m.run_due().await;
    assert_eq!(second.cursors(), [Some("1".to_string()), Some("2".into())]);
    // The listing resumed with its stored id: it saw its start, and every
    // item, so its complete snapshot removes nothing.
    assert_eq!(env.store.count_items("resumes", "items").await.unwrap(), 6);
}

#[tokio::test]
async fn a_listing_resumed_after_a_restart_deletes_what_disappeared_upstream() {
    let mut env = env().await;
    let gone = Arc::new(AtomicBool::new(false));
    // Three pages; once `gone` is set, item-3 no longer exists upstream.
    let listing = {
        let gone = gone.clone();
        move |cursor: Option<&str>| {
            let mut p = three_pages(cursor);
            if gone.load(Ordering::SeqCst) {
                p.upserts.retain(|i| i.id != "item-3");
            }
            p
        }
    };
    let gate = Arc::new(Semaphore::new(0));
    let watch = DropWatch::default();
    let first = {
        let (gate, watch, gone, listing) =
            (gate.clone(), watch.clone(), gone.clone(), listing.clone());
        Scripted::new(move |req| {
            let p = listing(req.cursor.as_deref());
            // The second listing stops in page 2's call.
            if gone.load(Ordering::SeqCst) && req.cursor.as_deref() == Some("1") {
                gated(&gate, &watch, p)
            } else {
                ok(p)
            }
        })
    };
    let m = env
        .start(vec![builtin(
            manifest("resumes", &[("items", 60)], ""),
            first.runtime(),
        )])
        .await;
    m.run_due().await;
    assert_eq!(env.store.count_items("resumes", "items").await.unwrap(), 6);

    gone.store(true, Ordering::SeqCst);
    env.advance(secs(60) + TICK).await;
    until("page 2 of the second listing is asked for", || {
        first.calls() == 5
    })
    .await;
    shut_down(&env, &m).await;
    let state = env.store.sync_state("resumes", "items").await.unwrap();
    assert_eq!(state.cursor.as_deref(), Some("1"));
    assert!(state.listing_id.is_some() && state.listing_from_start);

    let second = Scripted::new(move |req| ok(listing(req.cursor.as_deref())));
    env.drain();
    let m = env
        .start(vec![builtin(
            manifest("resumes", &[("items", 60)], ""),
            second.runtime(),
        )])
        .await;
    m.run_due().await;
    assert_eq!(second.cursors(), [Some("1".to_string()), Some("2".into())]);
    let ids: Vec<String> = m
        .items("resumes", "items", 50, None, None)
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|i| i.id)
        .collect();
    assert_eq!(ids, ["item-1", "item-2", "item-4", "item-5", "item-6"]);
    assert_eq!(synced(&env.drain()), [ev("resumes", "items", 0, 1)]);
}

// ---------------------------------------------------------------------------
// Scheduling, enabling, sync_now and shutdown

#[tokio::test]
async fn sync_now_while_the_resource_syncs_is_busy_and_queues_another_resource() {
    let env = env().await;
    // One permit: the first run of `slow` passes, its second waits.
    let gate = Arc::new(Semaphore::new(1));
    let watch = DropWatch::default();
    let rt = {
        let (gate, watch) = (gate.clone(), watch.clone());
        Scripted::new(move |req| match req.resource.as_str() {
            "slow" => gated(&gate, &watch, page(items(0..1), None, true)),
            _ => ok(page(items(0..1), None, true)),
        })
    };
    let count = |kind: &str| rt.requests().iter().filter(|r| r.resource == kind).count();
    let m = env
        .start(vec![builtin(
            manifest("busy", &[("slow", 60), ("quick", 3600)], ""),
            rt.runtime(),
        )])
        .await;
    m.run_due().await;
    assert_eq!((count("slow"), count("quick")), (1, 1));

    env.advance(secs(60) + TICK).await;
    until("slow is called again", || count("slow") == 2).await;
    assert!(matches!(
        m.sync_now("busy", "slow").await,
        Err(ManagerError::Busy)
    ));
    assert!(resource(&status(&m, "busy").await, "slow").syncing);
    // Not due for an hour: only the request makes it run.
    m.sync_now("busy", "quick").await.unwrap();
    assert!(matches!(
        m.sync_now("busy", "nope").await,
        Err(ManagerError::UnknownResource)
    ));
    assert!(matches!(
        m.sync_now("nope", "slow").await,
        Err(ManagerError::UnknownPlugin)
    ));

    gate.add_permits(1);
    m.run_due().await;
    assert_eq!((count("slow"), count("quick")), (2, 2));
    assert!(!watch.dropped());
}

#[tokio::test]
async fn a_disabled_plugin_never_runs_and_sync_now_is_refused() {
    let mut env = env().await;
    let rt = Scripted::new(|_| ok(page(items(0..1), None, true)));
    let m = env
        .start(vec![installed(
            manifest("optin", &[("items", 60)], ""),
            rt.runtime(),
        )])
        .await;
    m.run_due().await;
    assert_eq!(rt.calls(), 0);
    assert!(matches!(
        m.sync_now("optin", "items").await,
        Err(ManagerError::Disabled)
    ));
    let s = status(&m, "optin").await;
    assert_eq!(s.state, PluginState::Disabled);
    assert!(!s.enabled);

    let s = m.set_enabled("optin", true).await.unwrap();
    assert_eq!(s.state, PluginState::Ready);
    assert!(s.enabled);
    assert!(
        env.store
            .plugin_config("optin")
            .await
            .unwrap()
            .unwrap()
            .enabled
    );
    assert!(!updates(&env.drain(), "optin").is_empty());
    m.run_due().await;
    assert_eq!(rt.calls(), 1);

    m.set_enabled("optin", false).await.unwrap();
    env.advance(secs(600)).await;
    m.run_due().await;
    assert_eq!(rt.calls(), 1);
    assert!(matches!(
        m.set_enabled("nope", true).await,
        Err(ManagerError::UnknownPlugin)
    ));
}

#[tokio::test]
async fn disabling_mid_run_lets_the_call_finish_and_skips_the_remaining_pages() {
    let env = env().await;
    let gate = Arc::new(Semaphore::new(0));
    let watch = DropWatch::default();
    let rt = {
        let (gate, watch) = (gate.clone(), watch.clone());
        Scripted::new(move |req| gated(&gate, &watch, three_pages(req.cursor.as_deref())))
    };
    let m = env
        .start(vec![builtin(
            manifest("paged", &[("items", 60)], ""),
            rt.runtime(),
        )])
        .await;
    until("page 1 is asked for", || rt.calls() == 1).await;
    m.set_enabled("paged", false).await.unwrap();
    gate.add_permits(10);
    m.run_due().await;

    assert!(!watch.dropped());
    assert_eq!(watch.finished(), 1);
    assert_eq!(rt.calls(), 1, "no page after the plugin was disabled");
    assert_eq!(env.store.count_items("paged", "items").await.unwrap(), 2);
    assert_eq!(
        env.store
            .sync_state("paged", "items")
            .await
            .unwrap()
            .cursor
            .as_deref(),
        Some("1")
    );
    assert!(!resource(&status(&m, "paged").await, "items").syncing);
}

#[tokio::test]
async fn new_settings_mid_run_stop_its_pages_and_run_again_with_them() {
    let env = env().await;
    let extra = "[[settings]]\nkey = \"org\"\nlabel = \"Org\"\nkind = \"text\"\n";
    let gate = Arc::new(Semaphore::new(0));
    let watch = DropWatch::default();
    let rt = {
        let (gate, watch) = (gate.clone(), watch.clone());
        Scripted::new(move |req| gated(&gate, &watch, three_pages(req.cursor.as_deref())))
    };
    let m = env
        .start(vec![builtin(
            manifest("tuned", &[("items", 60)], extra),
            rt.runtime(),
        )])
        .await;
    until("page 1 is asked for", || rt.calls() == 1).await;
    m.set_settings("tuned", json!({"org": "new"}))
        .await
        .unwrap();
    gate.add_permits(10);
    m.run_due().await;

    let requests = rt.requests();
    assert_eq!(requests[0].settings, json!({}));
    assert!(requests.len() > 1, "ran again at once");
    assert_eq!(requests[1].settings, json!({"org": "new"}));
    assert_eq!(requests[1].cursor.as_deref(), Some("1"));
    assert!(!watch.dropped());
}

#[tokio::test]
async fn a_queued_sync_now_starts_through_the_scheduler_when_the_running_resource_ends() {
    let env = env().await;
    // One permit: the first run of `slow` passes, its second waits.
    let gate = Arc::new(Semaphore::new(1));
    let watch = DropWatch::default();
    let rt = {
        let (gate, watch) = (gate.clone(), watch.clone());
        Scripted::new(move |req| match req.resource.as_str() {
            "slow" => gated(&gate, &watch, page(items(0..1), None, true)),
            _ => ok(page(items(0..1), None, true)),
        })
    };
    let count = |kind: &str| rt.requests().iter().filter(|r| r.resource == kind).count();
    let m = env
        .start(vec![builtin(
            manifest("queued", &[("slow", 60), ("quick", 3600)], ""),
            rt.runtime(),
        )])
        .await;
    m.run_due().await;
    assert_eq!((count("slow"), count("quick")), (1, 1));

    // From here on only the scheduler task starts runs.
    env.advance(secs(60) + TICK).await;
    until("slow is called again", || count("slow") == 2).await;
    m.sync_now("queued", "quick").await.unwrap();
    for _ in 0..1000 {
        tokio::task::yield_now().await;
    }
    assert_eq!(count("quick"), 1, "waits for the running resource");
    gate.add_permits(1);
    let now = Some(env.now());
    until_status(&m, "queued", "quick ran again", |s| {
        let r = resource(s, "quick");
        r.last_ok_at == now && !r.syncing
    })
    .await;
    assert_eq!((count("slow"), count("quick")), (2, 2));
    assert!(!watch.dropped());
}

#[tokio::test]
async fn sync_now_after_shutdown_is_refused() {
    let env = env().await;
    let rt = Scripted::new(|_| ok(page(vec![], None, true)));
    let m = env
        .start(vec![builtin(
            manifest("closing", &[("items", 60)], ""),
            rt.runtime(),
        )])
        .await;
    m.run_due().await;
    m.shutdown().await;
    assert!(matches!(
        m.sync_now("closing", "items").await,
        Err(ManagerError::Disabled)
    ));
}

#[tokio::test]
async fn a_run_given_up_while_queued_ends_its_syncing_state() {
    let mut env = env().await;
    let gate = Arc::new(Semaphore::new(0));
    let mut runtimes = vec![];
    let mut plugins = vec![];
    for n in 0..5 {
        let gate = gate.clone();
        let rt = Scripted::new(move |_| {
            let gate = gate.clone();
            Box::pin(async move {
                gate.acquire().await.unwrap().forget();
                Ok(page(vec![], None, true))
            })
        });
        plugins.push(builtin(
            manifest(&format!("queue-{n}"), &[("items", 60)], ""),
            rt.runtime(),
        ));
        runtimes.push(rt);
    }
    let m = env.start(plugins).await;
    until("four calls are in flight", || {
        runtimes.iter().filter(|rt| rt.calls() == 1).count() == 4
    })
    .await;
    let waiting = runtimes.iter().position(|rt| rt.calls() == 0).unwrap();
    let id = format!("queue-{waiting}");
    assert!(resource(&status(&m, &id).await, "items").syncing);

    m.set_enabled(&id, false).await.unwrap();
    gate.add_permits(5);
    m.run_due().await;
    assert_eq!(runtimes[waiting].calls(), 0);
    let events = env.drain();
    let last = *updates(&events, &id).last().expect("a PluginUpdated");
    assert_eq!(last.state, PluginState::Disabled);
    assert!(!resource(last, "items").syncing, "{last:?}");
}

#[tokio::test]
async fn at_most_four_runs_at_once() {
    let env = env().await;
    let gate = Arc::new(Semaphore::new(0));
    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(AtomicUsize::new(0));
    let mut plugins = vec![];
    for n in 0..5 {
        let (gate, current, peak, entered) =
            (gate.clone(), current.clone(), peak.clone(), entered.clone());
        let rt = Scripted::new(move |_| {
            let (gate, current, peak) = (gate.clone(), current.clone(), peak.clone());
            entered.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let now = current.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                gate.acquire().await.unwrap().forget();
                current.fetch_sub(1, Ordering::SeqCst);
                Ok(page(vec![], None, true))
            })
        });
        plugins.push(builtin(
            manifest(&format!("gate-{n}"), &[("items", 60)], ""),
            rt.runtime(),
        ));
    }
    let m = env.start(plugins).await;
    let pass = tokio::spawn({
        let m = m.clone();
        async move { m.run_due().await }
    });
    until("four calls are in flight", || {
        current.load(Ordering::SeqCst) == 4
    })
    .await;
    for _ in 0..1000 {
        tokio::task::yield_now().await;
    }
    assert_eq!(entered.load(Ordering::SeqCst), 4, "a fifth call started");

    gate.add_permits(5);
    pass.await.unwrap();
    assert_eq!(entered.load(Ordering::SeqCst), 5);
    assert_eq!(peak.load(Ordering::SeqCst), 4);
}

/// Calls `shutdown` and moves the clock until it returns; returns how long it took.
async fn shut_down(env: &Env, m: &PluginManager) -> Duration {
    let started = Instant::now();
    let task = tokio::spawn({
        let m = m.clone();
        async move { m.shutdown().await }
    });
    for _ in 0..100 {
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        if task.is_finished() {
            task.await.unwrap();
            return started.elapsed();
        }
        env.advance(SECOND / 2).await;
    }
    panic!("shutdown did not return");
}

#[tokio::test]
async fn shutdown_returns_while_a_run_sleeps_and_leaves_its_call_running() {
    let env = env().await;
    let watch = DropWatch::default();
    let rt = {
        let watch = watch.clone();
        Scripted::new(move |_| {
            let guard = watch.guard();
            Box::pin(async move {
                tokio::time::sleep(secs(3600)).await;
                guard.done();
                Ok(page(items(0..1), None, true))
            })
        })
    };
    let m = env
        .start(vec![builtin(
            manifest("sleepy", &[("items", 60)], ""),
            rt.runtime(),
        )])
        .await;
    until("the call starts", || rt.calls() == 1).await;

    let took = shut_down(&env, &m).await;
    assert!(took >= secs(5) && took <= secs(6), "{took:?}");
    assert!(!watch.dropped(), "shutdown cancelled the call");
    assert_eq!(watch.finished(), 0);

    // The detached run still finishes its call; nothing new is scheduled.
    env.advance(secs(3600)).await;
    until("the call finishes", || watch.finished() == 1).await;
    m.run_due().await;
    assert_eq!(rt.calls(), 1);
}

#[tokio::test]
async fn items_are_read_through_the_manager() {
    let env = env().await;
    let (_, runtime) = fixture();
    let m = env
        .start(vec![builtin(
            manifest("fixture", &[("items", 60)], ""),
            runtime,
        )])
        .await;
    m.run_due().await;
    let first = m.items("fixture", "items", 4, None, None).await.unwrap();
    assert_eq!(first.items.len(), 4);
    let rest = m
        .items("fixture", "items", 4, first.next.as_deref(), None)
        .await
        .unwrap();
    assert_eq!(rest.items.len(), 2);
    assert_eq!(rest.next, None);
    let found = m
        .items("fixture", "items", 10, None, Some("item-3"))
        .await
        .unwrap();
    assert_eq!(found.items.len(), 1);
    assert!(matches!(
        m.items("fixture", "items", 4, Some("zz"), None).await,
        Err(ManagerError::Store(StoreError::BadCursor))
    ));
    assert!(matches!(
        m.items("nope", "items", 4, None, None).await,
        Err(ManagerError::UnknownPlugin)
    ));
    assert!(matches!(
        m.items("fixture", "nope", 4, None, None).await,
        Err(ManagerError::UnknownResource)
    ));
}

// ---------------------------------------------------------------------------
// Command-line tools
//
// Every tool is a shell script the test writes into its own directory, which
// is the manager's only search path: nothing installed on the machine is
// found or run. Real processes run while the clock is frozen, so their 10 s
// timeout cannot fire early.

const GH_TOOL: &str = "[[tools]]\nname = \"gh\"\nlabel = \"GitHub CLI\"\n\
                       min_version = \"2.40.0\"\ninstall_url = \"https://cli.github.com\"\n\
                       install = { mise = \"gh\", brew = \"gh\" }\n";
const GH_TOKEN: &str = "[[secrets]]\nkey = \"token\"\nlabel = \"GitHub sign-in\"\n\
                        help = \"Run: gh auth login\"\n\
                        from_tool = { tool = \"gh\", args = [\"auth\", \"token\"] }\n";

fn tools_cfg(dir: &Path) -> ManagerConfig {
    ManagerConfig {
        tools: ToolFinder::new(vec![dir.to_path_buf()]),
        ..ManagerConfig::default()
    }
}

/// Writes an executable shell script `name` into `dir`.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A fake `gh` that logs each run's arguments to `<dir>/gh.log`, prints
/// `version_line` for `--version` and runs `auth` for `auth token`.
fn fake_gh(dir: &Path, version_line: &str, auth: &str) {
    let log = dir.join("gh.log");
    script(
        dir,
        "gh",
        &format!(
            "echo \"$*\" >> '{}'\ncase \"$1\" in\n--version) echo '{version_line}' ;;\nauth) {auth} ;;\nesac",
            log.display()
        ),
    );
}

const SIGNED_IN: &str = "echo tool-5ecret";

/// Every run of the fake `gh`, as its arguments.
fn gh_runs(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("gh.log"))
        .map(|s| s.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// A fake `mise` that logs its arguments, one per line, to `<dir>/mise.log`
/// and then runs `then`.
fn fake_mise(dir: &Path, then: &str) {
    let log = dir.join("mise.log");
    script(
        dir,
        "mise",
        &format!(
            "for a in \"$@\"; do echo \"$a\" >> '{}'; done\n{then}",
            log.display()
        ),
    );
}

/// What the fake `mise` runs to install a fake `gh` that is signed in.
fn installs_gh(dir: &Path) -> String {
    let gh = dir.join("gh");
    format!(
        "printf '#!/bin/sh\\ncase \"$1\" in --version) echo gh version 2.41.0 ;; *) echo tool-5ecret ;; esac\\n' > '{0}'; chmod 755 '{0}'",
        gh.display()
    )
}

fn gh_hints() -> std::collections::BTreeMap<String, String> {
    [("brew", "gh"), ("mise", "gh")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn missing_gh(reason: &str) -> MissingTool {
    MissingTool {
        name: "gh".into(),
        label: "GitHub CLI".into(),
        reason: reason.into(),
        install_url: Some("https://cli.github.com/".into()),
        install: gh_hints(),
    }
}

/// The tools of a `NeedsSetup` state, and its message.
fn needs_setup(state: &PluginState) -> (&[MissingTool], &str) {
    match state {
        PluginState::NeedsSetup { tools, message } => (tools, message),
        other => panic!("expected NeedsSetup, got {other:?}"),
    }
}

/// Everything statuses and events said, as text.
fn said(statuses: &[PluginStatus], events: &[EventKind]) -> String {
    let mut text = serde_json::to_string(statuses).unwrap();
    for e in events {
        text.push_str(&serde_json::to_string(e).unwrap());
    }
    text
}

#[tokio::test]
async fn a_builtin_plugin_with_a_missing_tool_needs_setup_and_is_not_called() {
    let mut env = env().await;
    let dir = tempfile::tempdir().unwrap();
    let rt = Scripted::new(|req| ok(three_pages(req.cursor.as_deref())));
    let m = env
        .start_cfg(
            vec![builtin(
                manifest("tooled", &[("items", 60)], GH_TOOL),
                rt.runtime(),
            )],
            no_secrets(),
            tools_cfg(dir.path()),
        )
        .await;
    m.run_due().await;

    assert_eq!(rt.calls(), 0);
    let s = status(&m, "tooled").await;
    let (tools, message) = needs_setup(&s.state);
    assert_eq!(tools, [missing_gh("not installed")]);
    assert!(message.contains("GitHub CLI (gh)"), "{message}");
    assert!(message.contains("not installed"), "{message}");
    // The last event says so, and that nothing syncs.
    let events = env.drain();
    let last = *updates(&events, "tooled").last().expect("a PluginUpdated");
    assert_eq!(last.state, s.state);
    assert!(!resource(last, "items").syncing, "{last:?}");

    // Held: time passing does not run it.
    env.advance(secs(3600)).await;
    m.run_due().await;
    assert_eq!(rt.calls(), 0);

    // Once the tool is there, `sync_now` checks again and the plugin runs.
    fake_gh(dir.path(), "gh version 2.41.0 (2024-01-09)", SIGNED_IN);
    m.sync_now("tooled", "items").await.unwrap();
    m.run_due().await;
    assert_eq!(rt.calls(), 3);
    assert_eq!(status(&m, "tooled").await.state, PluginState::Ready);
}

#[tokio::test]
async fn a_tool_too_old_or_without_a_readable_version_needs_setup_saying_why() {
    let env = env().await;
    let dir = tempfile::tempdir().unwrap();
    let rt = Scripted::new(|_| ok(page(vec![], None, true)));
    fake_gh(dir.path(), "gh version 2.30.1 (2023-05-01)", SIGNED_IN);
    let m = env
        .start_cfg(
            vec![builtin(
                manifest("aged", &[("items", 60)], GH_TOOL),
                rt.runtime(),
            )],
            no_secrets(),
            tools_cfg(dir.path()),
        )
        .await;
    m.run_due().await;
    let s = status(&m, "aged").await;
    let (tools, message) = needs_setup(&s.state);
    assert_eq!(
        tools,
        [missing_gh("older than the required version 2.40.0")]
    );
    // What the tool printed is never repeated, not even its version.
    assert!(!message.contains("2.30.1"), "{message}");

    fake_gh(dir.path(), "gh development build", SIGNED_IN);
    m.set_settings("aged", json!({})).await.unwrap();
    m.run_due().await;
    let s = status(&m, "aged").await;
    let (tools, message) = needs_setup(&s.state);
    assert_eq!(tools, [missing_gh("could not read its version")]);
    assert!(!message.contains("development"), "{message}");
    assert_eq!(rt.calls(), 0);
}

#[tokio::test]
async fn the_tool_check_is_kept_for_60_s_and_sync_now_settings_and_enabling_check_again() {
    let env = env().await;
    let dir = tempfile::tempdir().unwrap();
    fake_gh(dir.path(), "gh version 2.41.0", SIGNED_IN);
    let rt = Scripted::new(|_| ok(page(vec![], None, true)));
    let m = env
        .start_cfg(
            vec![builtin(
                manifest("cached", &[("a", 60), ("b", 60)], GH_TOOL),
                rt.runtime(),
            )],
            no_secrets(),
            tools_cfg(dir.path()),
        )
        .await;
    let checks = || gh_runs(dir.path()).len();

    // t=0: both resources run; the second uses the first's check.
    m.run_due().await;
    assert_eq!((rt.calls(), checks()), (2, 1));

    // t=30: sync_now checks again.
    env.advance(secs(30)).await;
    m.sync_now("cached", "a").await.unwrap();
    m.run_due().await;
    assert_eq!((rt.calls(), checks()), (3, 2));

    // t=61: `b` is due; the check from t=30 still holds.
    env.advance(secs(31)).await;
    m.run_due().await;
    assert_eq!((rt.calls(), checks()), (4, 2));

    // t=91: `a` is due; the check from t=30 is 61 s old.
    env.advance(secs(30)).await;
    m.run_due().await;
    assert_eq!((rt.calls(), checks()), (5, 3));

    // New settings and enabling check again, once for both runs.
    m.set_settings("cached", json!({})).await.unwrap();
    m.run_due().await;
    assert_eq!((rt.calls(), checks()), (7, 4));
    m.set_enabled("cached", true).await.unwrap();
    m.run_due().await;
    assert_eq!((rt.calls(), checks()), (9, 5));
    assert!(gh_runs(dir.path()).iter().all(|r| r == "--version"));
}

#[tokio::test]
async fn an_installed_plugin_declaring_a_tool_is_never_run() {
    let env = env().await;
    let dir = tempfile::tempdir().unwrap();
    // The tool is there, signed in, and new enough.
    fake_gh(dir.path(), "gh version 2.41.0", SIGNED_IN);
    let rt = Scripted::new(|req| ok(three_pages(req.cursor.as_deref())));
    let m = env
        .start_cfg(
            vec![installed(
                manifest("third", &[("items", 60)], &format!("{GH_TOOL}{GH_TOKEN}")),
                rt.runtime(),
            )],
            no_secrets(),
            tools_cfg(dir.path()),
        )
        .await;
    let want = PluginState::NeedsSetup {
        tools: vec![MissingTool {
            name: "gh".into(),
            label: "GitHub CLI".into(),
            reason: "needs approval".into(),
            install_url: None,
            install: Default::default(),
        }],
        message: "command-line tools need approval, which is not built yet".into(),
    };

    // Listed so even while disabled, and enabling does not change it.
    let s = status(&m, "third").await;
    assert!(!s.enabled);
    assert_eq!(s.state, want);
    let s = m.set_enabled("third", true).await.unwrap();
    assert_eq!(s.state, want);
    m.run_due().await;
    assert!(matches!(
        m.sync_now("third", "items").await,
        Err(ManagerError::Disabled)
    ));
    env.advance(secs(3600)).await;
    m.run_due().await;
    m.set_settings("third", json!({})).await.unwrap();
    m.run_due().await;

    assert_eq!(rt.calls(), 0);
    assert_eq!(status(&m, "third").await.state, want);
    assert!(matches!(
        m.install_tool("third", "gh").await,
        Err(ManagerError::NotAllowed)
    ));
    // Its tool was neither checked nor asked for a secret.
    assert_eq!(gh_runs(dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn a_tool_backed_secret_signs_a_builtin_plugin_in_after_the_configured_sources() {
    let mut env = env().await;
    let dir = tempfile::tempdir().unwrap();
    fake_gh(dir.path(), "gh version 2.41.0", SIGNED_IN);
    let rt = Scripted::new(|req| ok(three_pages(req.cursor.as_deref())));
    let extra = format!("{GH_TOOL}{GH_TOKEN}");
    let m = env
        .start_cfg(
            vec![builtin(
                manifest("signed", &[("items", 60)], &extra),
                rt.runtime(),
            )],
            no_secrets(),
            tools_cfg(dir.path()),
        )
        .await;
    m.run_due().await;
    assert_eq!(rt.calls(), 3);
    assert_eq!(status(&m, "signed").await.state, PluginState::Ready);
    assert_eq!(gh_runs(dir.path()), ["--version", "auth token"]);
    let text = said(&m.list().await, &env.drain());
    assert!(!text.contains("tool-5ecret"), "{text}");
    m.shutdown().await;

    // A configured value wins: the tool is not asked.
    std::fs::remove_file(dir.path().join("gh.log")).unwrap();
    let m = env
        .start_cfg(
            vec![builtin(
                manifest("signed", &[("items", 60)], &extra),
                rt.runtime(),
            )],
            secrets(&[("signed", "token", "s3cr3t-value")]),
            tools_cfg(dir.path()),
        )
        .await;
    m.set_settings("signed", json!({})).await.unwrap();
    m.run_due().await;
    assert_eq!(status(&m, "signed").await.state, PluginState::Ready);
    assert_eq!(gh_runs(dir.path()), ["--version"]);
}

/// Captured log output.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// Everything this test binary logs, from every test. Global on purpose: a
/// subscriber set for one thread misses events whose call site another
/// test's thread reached first (tracing caches the interest per call site).
fn logs() -> &'static Logs {
    static LOGS: std::sync::OnceLock<Logs> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Logs::default();
        let sink = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || sink.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("the only global subscriber");
        logs
    })
}

#[tokio::test]
async fn a_tool_that_is_not_signed_in_needs_auth_with_its_help_and_none_of_its_output() {
    let logs = logs();
    let mut env = env().await;
    let dir = tempfile::tempdir().unwrap();
    fake_gh(
        dir.path(),
        "gh version 2.41.0",
        "echo out-5ecret; echo err-5ecret >&2; exit 1",
    );
    let rt = Scripted::new(|_| ok(page(vec![], None, true)));
    let without_help = GH_TOKEN.replace("help = \"Run: gh auth login\"\n", "");
    let m = env
        .start_cfg(
            vec![
                builtin(
                    manifest("helped", &[("items", 60)], &format!("{GH_TOOL}{GH_TOKEN}")),
                    rt.runtime(),
                ),
                builtin(
                    manifest(
                        "unhelped",
                        &[("items", 60)],
                        &format!("{GH_TOOL}{without_help}"),
                    ),
                    rt.runtime(),
                ),
            ],
            no_secrets(),
            tools_cfg(dir.path()),
        )
        .await;
    m.run_due().await;

    assert_eq!(rt.calls(), 0);
    assert_eq!(
        status(&m, "helped").await.state,
        PluginState::NeedsAuth {
            missing: vec!["token".into()],
            message: "GitHub CLI is not signed in. Run: gh auth login".into()
        }
    );
    assert_eq!(
        status(&m, "unhelped").await.state,
        PluginState::NeedsAuth {
            missing: vec!["token".into()],
            message: "GitHub CLI is not signed in".into()
        }
    );
    let text = said(&m.list().await, &env.drain()) + &logs.text();
    assert!(
        text.contains(
            "cannot resolve a plugin secret plugin=helped key=token \
             error=secret command failed: exit status: 1"
        ),
        "{text}"
    );
    assert!(!text.contains("5ecret"), "{text}");
}

#[tokio::test]
async fn installing_a_tool_runs_mise_and_the_plugin_runs_at_once() {
    let mut env = env().await;
    let dir = tempfile::tempdir().unwrap();
    fake_mise(dir.path(), &installs_gh(dir.path()));
    let rt = Scripted::new(|req| ok(three_pages(req.cursor.as_deref())));
    let m = env
        .start_cfg(
            vec![builtin(
                manifest("tooled", &[("items", 60)], &format!("{GH_TOOL}{GH_TOKEN}")),
                rt.runtime(),
            )],
            no_secrets(),
            tools_cfg(dir.path()),
        )
        .await;
    m.run_due().await;
    needs_setup(&status(&m, "tooled").await.state);
    env.drain();

    let s = m.install_tool("tooled", "gh").await.unwrap();

    assert_eq!(s.state, PluginState::Ready);
    let log = std::fs::read_to_string(dir.path().join("mise.log")).unwrap();
    assert_eq!(log, "use\n--global\ngh\n");
    let events = env.drain();
    assert_eq!(
        updates(&events, "tooled").last().unwrap().state,
        PluginState::Ready
    );
    // Due now, without a sync_now.
    m.run_due().await;
    assert_eq!(rt.calls(), 3);
    assert_eq!(status(&m, "tooled").await.state, PluginState::Ready);
}

#[tokio::test]
async fn install_refusals() {
    let env = env().await;
    let dir = tempfile::tempdir().unwrap();
    let rt = Scripted::new(|_| ok(page(vec![], None, true)));
    let hinted = "[[tools]]\nname = \"jq\"\nlabel = \"jq\"\ninstall = { brew = \"jq\" }\n";
    let m = env
        .start_cfg(
            vec![
                builtin(manifest("tooled", &[("items", 60)], GH_TOOL), rt.runtime()),
                builtin(manifest("hinted", &[("items", 60)], hinted), rt.runtime()),
            ],
            no_secrets(),
            tools_cfg(dir.path()),
        )
        .await;
    assert!(matches!(
        m.install_tool("nope", "gh").await,
        Err(ManagerError::UnknownPlugin)
    ));
    assert!(matches!(
        m.install_tool("tooled", "jq").await,
        Err(ManagerError::UnknownTool)
    ));
    assert!(matches!(
        m.install_tool("hinted", "jq").await,
        Err(ManagerError::Install(InstallError::NoMiseSpec))
    ));
    // No mise in the search path.
    match m.install_tool("tooled", "gh").await {
        Err(ManagerError::Install(InstallError::MiseMissing { install_url })) => {
            assert_eq!(install_url, "https://mise.jdx.dev")
        }
        other => panic!("expected MiseMissing, got {other:?}"),
    }
    fake_mise(dir.path(), "echo out-5ecret; echo err-5ecret >&2; exit 3");
    let err = m.install_tool("tooled", "gh").await.unwrap_err();
    assert!(
        matches!(
            err,
            ManagerError::Install(InstallError::Failed { status: Some(3) })
        ),
        "{err:?}"
    );
    assert!(!format!("{err:?} {err}").contains("5ecret"), "{err:?}");
}

/// Waits, on a blocking thread so the clock stays still, until `path` exists.
async fn file_appears(path: PathBuf) {
    let found = tokio::task::spawn_blocking(move || {
        for _ in 0..2000 {
            if path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    })
    .await
    .unwrap();
    assert!(found, "gave up waiting for a file");
}

#[tokio::test]
async fn one_install_at_a_time_released_after_a_timeout_and_a_failure() {
    let env = env().await;
    let dir = tempfile::tempdir().unwrap();
    let started = dir.path().join("started");
    fake_mise(
        dir.path(),
        &format!("touch '{}'; exec sleep 600", started.display()),
    );
    let rt = Scripted::new(|_| ok(page(vec![], None, true)));
    let both = format!(
        "{GH_TOOL}[[tools]]\nname = \"jq\"\nlabel = \"jq\"\ninstall = {{ mise = \"jq\" }}\n"
    );
    let m = env
        .start_cfg(
            vec![
                builtin(manifest("tooled", &[("items", 60)], &both), rt.runtime()),
                builtin(manifest("also", &[("items", 60)], GH_TOOL), rt.runtime()),
            ],
            no_secrets(),
            tools_cfg(dir.path()),
        )
        .await;
    let first = {
        let m = m.clone();
        tokio::spawn(async move { m.install_tool("tooled", "gh").await })
    };
    file_appears(started.clone()).await;

    // Any install waits its turn: the same tool, for this or another
    // plugin, and another tool (mise edits one global file).
    for (id, tool) in [("tooled", "gh"), ("also", "gh"), ("tooled", "jq")] {
        assert!(
            matches!(
                m.install_tool(id, tool).await,
                Err(ManagerError::InstallInProgress)
            ),
            "{id} {tool}"
        );
    }

    // Ten minutes later the first install is stopped.
    env.advance(secs(600) + TICK).await;
    let result = first.await.unwrap();
    assert!(
        matches!(result, Err(ManagerError::Install(InstallError::Timeout))),
        "{result:?}"
    );

    // Released: the next install runs, fails, and releases too.
    fake_mise(dir.path(), "exit 3");
    for _ in 0..2 {
        assert!(matches!(
            m.install_tool("tooled", "gh").await,
            Err(ManagerError::Install(InstallError::Failed {
                status: Some(3)
            }))
        ));
    }
    fake_mise(dir.path(), &installs_gh(dir.path()));
    let s = m.install_tool("also", "gh").await.unwrap();
    assert_eq!(s.state, PluginState::Ready);
}

/// Replaces `<dir>/<name>` with a new script by renaming over it, so a shell
/// still running the old one keeps reading the old file.
fn replace_script(dir: &Path, name: &str, body: &str) {
    let fresh = script(dir, &format!("{name}.new"), body);
    std::fs::rename(fresh, dir.join(name)).unwrap();
}

#[tokio::test]
async fn a_check_that_ends_after_sync_now_does_not_overwrite_the_newer_one() {
    let env = env().await;
    let dir = tempfile::tempdir().unwrap();
    let (started, go) = (dir.path().join("started"), dir.path().join("go"));
    // An old gh whose version check waits for the test.
    script(
        dir.path(),
        "gh",
        &format!(
            "touch '{}'; while [ ! -f '{}' ]; do sleep 0.02; done; echo 'gh version 2.30.0'",
            started.display(),
            go.display()
        ),
    );
    let rt = Scripted::new(|_| ok(page(vec![], None, true)));
    let m = env
        .start_cfg(
            vec![builtin(
                manifest("racing", &[("a", 60), ("b", 60)], GH_TOOL),
                rt.runtime(),
            )],
            no_secrets(),
            tools_cfg(dir.path()),
        )
        .await;
    // The run of `a` starts its check and waits in the old gh.
    let runs = {
        let m = m.clone();
        tokio::spawn(async move { m.run_due().await })
    };
    file_appears(started).await;

    // Meanwhile a new gh is installed and `b` is asked for, which checks
    // again; then the old check ends, finding the old version.
    replace_script(dir.path(), "gh", "echo 'gh version 2.41.0'");
    m.sync_now("racing", "b").await.unwrap();
    std::fs::write(&go, "").unwrap();
    runs.await.unwrap();

    // `b` ran on the new check (the old result was not kept for it), and
    // then `a`, no longer held. Had the old result been kept, neither would.
    assert_eq!(rt.calls(), 2);
    assert_eq!(status(&m, "racing").await.state, PluginState::Ready);
}

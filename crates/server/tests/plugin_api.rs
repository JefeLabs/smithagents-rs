//! The plugin routes of the daemon API, over a manager running the linked
//! fixture plugin in real time: success shapes, every error code, the token
//! check, events, paging, and that no secret value reaches a response or an
//! event.

use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use smithagents_core::event::{Event, EventKind};
use smithagents_core::plugin::PluginState;
use smithagents_plugin_fixture::Fixture;
use smithagents_plugin_host::{LinkedRuntime, PluginRuntime, StaticSecrets};
use smithagents_plugin_protocol::{HostApi, Manifest, Plugin, PluginError, SyncPage, SyncRequest};
use smithagents_server::api::{router, AppState};
use smithagents_server::events::EventBus;
use smithagents_server::harness::fake::FakeHarness;
use smithagents_server::intents::IntentService;
use smithagents_server::paths::Paths;
use smithagents_server::plugins::{LoadedPlugin, ManagerConfig, Origin, PluginManager};
use smithagents_server::queue::{Scheduler, SchedulerHandle};
use smithagents_server::service::AgentService;
use smithagents_server::store::Store;
use tempfile::TempDir;
use tokio::sync::broadcast;
use tower::ServiceExt;

const TOKEN: &str = "test-token-123";
const SECRET: &str = "s3cr3t-value";
const PATIENCE: Duration = Duration::from_secs(20);

/// A plugin whose sync waits until the test lets it go.
struct Gate {
    started: Mutex<mpsc::Sender<()>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl Plugin for Gate {
    fn sync(&self, host: &dyn HostApi, req: SyncRequest) -> Result<SyncPage, PluginError> {
        let _ = self.started.lock().unwrap().send(());
        // A dropped sender also lets the call end.
        let _ = self.release.lock().unwrap().recv();
        Fixture.sync(host, req)
    }
}

fn manifest(id: &str, extra: &str) -> Manifest {
    Manifest::parse(&format!(
        "id = \"{id}\"\nname = \"Test {id}\"\nversion = \"1.0.0\"\ncontract = 1\n\
         host = \"in-process\"\nhosts = [\"api.example.com\"]\n{extra}\n\
         [[resources]]\nkind = \"items\"\nlabel = \"Items\"\ninterval_secs = 3600\n"
    ))
    .unwrap()
}

fn loaded(manifest: Manifest, plugin: Arc<dyn Plugin>) -> LoadedPlugin {
    let runtime: Arc<dyn PluginRuntime> =
        Arc::new(LinkedRuntime::new(plugin, Duration::from_secs(60)));
    LoadedPlugin {
        manifest,
        origin: Origin::Installed,
        runtime: Ok(runtime),
    }
}

struct Fx {
    _tmp: TempDir,
    state: AppState,
    events: Mutex<broadcast::Receiver<Event>>,
    /// Everything the routes answered and the events published, as text.
    seen: Mutex<Vec<String>>,
    gate_started: Mutex<mpsc::Receiver<()>>,
    gate_release: mpsc::Sender<()>,
    _sched: SchedulerHandle,
}

async fn fixture() -> Fx {
    let tmp = TempDir::new().unwrap();
    let store = Store::open_in_memory().await.unwrap();
    let bus = EventBus::new(1024);
    let events = bus.subscribe(None).unwrap().live;
    let paths = Paths {
        home: tmp.path().join("home"),
    };
    paths.ensure_dirs().unwrap();
    let harness = FakeHarness::scripted(vec![]);
    let service = AgentService::new(
        store.clone(),
        bus.clone(),
        Arc::new(harness.clone()),
        paths.clone(),
        Duration::from_secs(3600),
    );
    let intents = IntentService::new(
        store.clone(),
        bus.clone(),
        Arc::new(harness),
        service.clone(),
        paths,
    );
    let sched = Scheduler::start(intents.clone(), service.clone(), store.clone(), bus.clone());

    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let gate = Gate {
        started: Mutex::new(started_tx),
        release: Mutex::new(release_rx),
    };
    let secrets = StaticSecrets(
        [(
            ("fixture".to_owned(), "token".to_owned()),
            SECRET.to_owned(),
        )]
        .into_iter()
        .collect(),
    );
    let plugins = vec![
        loaded(
            manifest(
                "fixture",
                "[[secrets]]\nkey = \"token\"\nlabel = \"Token\"\nhosts = [\"api.example.com\"]\n\
                 [[settings]]\nkey = \"note\"\nlabel = \"Note\"\nkind = \"text\"\n",
            ),
            Arc::new(Fixture),
        ),
        loaded(
            manifest(
                "needy",
                "[[settings]]\nkey = \"repos\"\nlabel = \"Repos\"\nkind = \"list\"\nrequired = true\n",
            ),
            Arc::new(Fixture),
        ),
        loaded(manifest("gated", ""), Arc::new(gate)),
        LoadedPlugin {
            manifest: manifest("broken", ""),
            origin: Origin::Installed,
            runtime: Err("the entry file is not a component".into()),
        },
    ];
    let manager = PluginManager::start(
        store.clone(),
        bus.clone(),
        Arc::new(secrets),
        plugins,
        ManagerConfig::default(),
    )
    .await
    .unwrap();
    let state = AppState {
        service,
        intents,
        bus,
        store,
        plugins: manager,
        token: Arc::from(TOKEN),
        instance_id: Arc::from("0123456789abcdef"),
        ws_clients: Arc::new(AtomicUsize::new(0)),
        started_at: Instant::now(),
    };
    Fx {
        _tmp: tmp,
        state,
        events: Mutex::new(events),
        seen: Mutex::new(vec![]),
        gate_started: Mutex::new(started_rx),
        gate_release: release_tx,
        _sched: sched,
    }
}

impl Fx {
    async fn send(&self, req: Request<Body>) -> (StatusCode, Value) {
        let resp = router(self.state.clone()).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.seen.lock().unwrap().push(text.clone());
        let json = serde_json::from_str(&text).unwrap_or(Value::String(text));
        (status, json)
    }

    async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.call_raw(method, uri, body.map(|v| v.to_string()))
            .await
    }

    async fn call_raw(
        &self,
        method: Method,
        uri: &str,
        body: Option<String>,
    ) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.map_or_else(Body::empty, Body::from))
            .unwrap();
        self.send(req).await
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        self.call(Method::GET, uri, None).await
    }

    async fn post(&self, uri: &str) -> (StatusCode, Value) {
        self.call(Method::POST, uri, None).await
    }

    /// Events published so far, as they stand now.
    fn drain(&self) -> Vec<EventKind> {
        let mut out = vec![];
        let mut rx = self.events.lock().unwrap();
        while let Ok(e) = rx.try_recv() {
            self.seen
                .lock()
                .unwrap()
                .push(serde_json::to_string(&e).unwrap());
            out.push(e.kind);
        }
        out
    }

    /// Polls `GET /v1/plugins/{id}` until `pred` holds.
    async fn until(&self, id: &str, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let (status, body) = self.get(&format!("/v1/plugins/{id}")).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            if pred(&body) {
                return body;
            }
            assert!(
                Instant::now() < deadline,
                "gave up waiting until {what}: {body}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn assert_error(status: StatusCode, body: &Value, want: StatusCode, code: &str) {
    assert_eq!(status, want, "{body}");
    assert_eq!(body["error"], code, "{body}");
    assert!(body["message"].is_string(), "{body}");
}

fn item_count(plugin: &Value) -> u64 {
    plugin["resources"][0]["item_count"].as_u64().unwrap()
}

#[tokio::test]
async fn lists_and_gets_plugins_sorted_and_disabled() {
    let fx = fixture().await;
    let (status, body) = fx.get("/v1/plugins").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ids: Vec<_> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["broken", "fixture", "gated", "needy"]);

    let (status, body) = fx.get("/v1/plugins/fixture").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["state"], json!({"kind": "disabled"}));
    assert_eq!(body["resources"][0]["kind"], "items");
    assert_eq!(body["builtin"], false);

    let (status, body) = fx.get("/v1/plugins/nope").await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "plugin_not_found");
}

#[tokio::test]
async fn enable_syncs_and_publishes_events_then_disable() {
    let fx = fixture().await;
    let (status, body) = fx.post("/v1/plugins/fixture/enable").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], true);

    let done = fx
        .until("fixture", "six items", |p| item_count(p) == 6)
        .await;
    assert_eq!(done["state"], json!({"kind": "ready"}));

    let events = fx.drain();
    assert!(
        events.iter().any(|e| matches!(
            e,
            EventKind::ResourceSynced { plugin_id, resource, upserted: 6, deleted: 0, .. }
                if plugin_id == "fixture" && resource == "items"
        )),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, EventKind::PluginUpdated(s) if s.id == "fixture")),
        "{events:?}"
    );

    // A manual sync is accepted and runs again.
    let before = done["resources"][0]["last_run_at"].clone();
    let (status, body) = fx.post("/v1/plugins/fixture/resources/items/sync").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    fx.until("fixture", "another run", |p| {
        p["resources"][0]["last_run_at"] != before && p["resources"][0]["syncing"] == false
    })
    .await;

    let (status, body) = fx.post("/v1/plugins/fixture/disable").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["state"], json!({"kind": "disabled"}));
}

#[tokio::test]
async fn settings_round_trip_and_refusals() {
    let fx = fixture().await;
    let (status, body) = fx.get("/v1/plugins/fixture/settings").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({}));

    let (status, body) = fx
        .call(
            Method::PUT,
            "/v1/plugins/fixture/settings",
            Some(json!({"note": "hello"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], "fixture");
    let (status, body) = fx.get("/v1/plugins/fixture/settings").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"note": "hello"}));

    // A value the manifest refuses.
    let (status, body) = fx
        .call(Method::PUT, "/v1/plugins/needy/settings", Some(json!({})))
        .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_settings");
    assert!(
        body["message"].as_str().unwrap().contains("repos"),
        "{body}"
    );
    let (status, body) = fx
        .call(
            Method::PUT,
            "/v1/plugins/fixture/settings",
            Some(json!({"note": 5})),
        )
        .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_settings");
    // Nothing was stored by the refusals.
    let (_, body) = fx.get("/v1/plugins/needy/settings").await;
    assert_eq!(body, json!({}));

    // Not an object, not JSON, too large.
    let (status, body) = fx
        .call(
            Method::PUT,
            "/v1/plugins/fixture/settings",
            Some(json!([1])),
        )
        .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_settings");
    let (status, body) = fx
        .call_raw(
            Method::PUT,
            "/v1/plugins/fixture/settings",
            Some("{".into()),
        )
        .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "bad_request");
    let big = json!({"note": "x".repeat(64 * 1024)}).to_string();
    let (status, body) = fx
        .call_raw(Method::PUT, "/v1/plugins/fixture/settings", Some(big))
        .await;
    assert_error(
        status,
        &body,
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
    );

    let (status, body) = fx
        .call(Method::PUT, "/v1/plugins/nope/settings", Some(json!({})))
        .await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "plugin_not_found");
    let (status, body) = fx.get("/v1/plugins/nope/settings").await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "plugin_not_found");
}

#[tokio::test]
async fn sync_errors_say_why() {
    let fx = fixture().await;
    let sync = |id: &str, kind: &str| format!("/v1/plugins/{id}/resources/{kind}/sync");

    let (status, body) = fx.post(&sync("nope", "items")).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "plugin_not_found");
    let (status, body) = fx.post(&sync("fixture", "nope")).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "resource_not_found");

    // Disabled.
    let (status, body) = fx.post(&sync("fixture", "items")).await;
    assert_error(status, &body, StatusCode::CONFLICT, "plugin_disabled");

    // Enabled but needing settings.
    fx.post("/v1/plugins/needy/enable").await;
    let needy = fx
        .until("needy", "needs settings", |p| {
            p["state"]["kind"] == "needs_settings"
        })
        .await;
    let (status, body) = fx.post(&sync("needy", "items")).await;
    assert_error(status, &body, StatusCode::CONFLICT, "plugin_not_ready");
    assert!(
        body["message"].as_str().unwrap().contains("repos"),
        "{body} {needy}"
    );

    // Enabled but it never loaded.
    fx.post("/v1/plugins/broken/enable").await;
    let (status, body) = fx.post(&sync("broken", "items")).await;
    assert_error(status, &body, StatusCode::CONFLICT, "plugin_not_ready");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("not a component"),
        "{body}"
    );
    let (_, broken) = fx.get("/v1/plugins/broken").await;
    assert_eq!(broken["state"]["kind"], "load_failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_sync_of_a_running_resource_is_refused() {
    let fx = fixture().await;
    fx.post("/v1/plugins/gated/enable").await;
    // The first run starts at once and waits at the gate.
    tokio::task::block_in_place(|| {
        fx.gate_started
            .lock()
            .unwrap()
            .recv_timeout(PATIENCE)
            .expect("the gated plugin started")
    });
    let (status, body) = fx.post("/v1/plugins/gated/resources/items/sync").await;
    assert_error(status, &body, StatusCode::CONFLICT, "sync_in_progress");
    // One release per page: the fixture lists three.
    for _ in 0..3 {
        fx.gate_release.send(()).unwrap();
    }
    let done = fx
        .until("gated", "its run to end", |p| item_count(p) == 6)
        .await;
    assert!(matches!(
        serde_json::from_value::<PluginState>(done["state"].clone()).unwrap(),
        PluginState::Ready
    ));
}

#[tokio::test]
async fn items_page_with_after_and_filter() {
    let fx = fixture().await;
    fx.post("/v1/plugins/fixture/enable").await;
    fx.until("fixture", "six items", |p| item_count(p) == 6)
        .await;
    let base = "/v1/plugins/fixture/resources/items/items";

    let (status, first) = fx.get(&format!("{base}?limit=4")).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["items"].as_array().unwrap().len(), 4);
    let next = first["next"].as_str().expect("a next cursor").to_owned();

    let (status, second) = fx.get(&format!("{base}?limit=4&after={next}")).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["items"].as_array().unwrap().len(), 2);
    assert!(second["next"].is_null(), "{second}");
    let mut ids: Vec<_> = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["items"].as_array().unwrap())
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 6);

    // The default limit lists all six; zero is raised to one; a huge one is capped.
    let (_, all) = fx.get(base).await;
    assert_eq!(all["items"].as_array().unwrap().len(), 6);
    let (_, one) = fx.get(&format!("{base}?limit=0")).await;
    assert_eq!(one["items"].as_array().unwrap().len(), 1);
    let (status, _) = fx.get(&format!("{base}?limit=100000")).await;
    assert_eq!(status, StatusCode::OK);

    let (status, found) = fx.get(&format!("{base}?q=item-3")).await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["items"].as_array().unwrap().len(), 1, "{found}");

    let (status, body) = fx.get(&format!("{base}?after=zz")).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "bad_cursor");
    let long = "a".repeat(257);
    let (status, body) = fx.get(&format!("{base}?q={long}")).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid");
    let ok = "a".repeat(256);
    let (status, _) = fx.get(&format!("{base}?q={ok}")).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = fx.get(&format!("{base}?limit=abc")).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "bad_request");

    let (status, body) = fx.get("/v1/plugins/nope/resources/items/items").await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "plugin_not_found");
    let (status, body) = fx.get("/v1/plugins/fixture/resources/nope/items").await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "resource_not_found");
}

#[tokio::test]
async fn every_plugin_route_needs_the_token() {
    let fx = fixture().await;
    let routes = [
        (Method::GET, "/v1/plugins"),
        (Method::GET, "/v1/plugins/fixture"),
        (Method::POST, "/v1/plugins/fixture/enable"),
        (Method::POST, "/v1/plugins/fixture/disable"),
        (Method::GET, "/v1/plugins/fixture/settings"),
        (Method::PUT, "/v1/plugins/fixture/settings"),
        (Method::POST, "/v1/plugins/fixture/resources/items/sync"),
        (Method::GET, "/v1/plugins/fixture/resources/items/items"),
    ];
    for (method, uri) in routes {
        for auth in [None, Some("Bearer wrong")] {
            let mut req = Request::builder().method(method.clone()).uri(uri);
            if let Some(auth) = auth {
                req = req.header(header::AUTHORIZATION, auth);
            }
            let (status, body) = fx.send(req.body(Body::from("{}")).unwrap()).await;
            assert_error(status, &body, StatusCode::UNAUTHORIZED, "unauthorized");
        }
    }
    // None of those changed anything.
    let (_, body) = fx.get("/v1/plugins/fixture").await;
    assert_eq!(body["enabled"], false);
}

#[tokio::test]
async fn no_response_or_event_holds_a_secret_value() {
    let fx = fixture().await;
    fx.post("/v1/plugins/fixture/enable").await;
    fx.until("fixture", "six items", |p| item_count(p) == 6)
        .await;
    fx.get("/v1/plugins").await;
    fx.get("/v1/plugins/fixture").await;
    fx.get("/v1/plugins/fixture/settings").await;
    fx.get("/v1/plugins/fixture/resources/items/items").await;
    fx.call(
        Method::PUT,
        "/v1/plugins/fixture/settings",
        Some(json!({"note": "hi"})),
    )
    .await;
    fx.post("/v1/plugins/fixture/resources/items/sync").await;
    fx.post("/v1/plugins/needy/enable").await;
    fx.post("/v1/plugins/fixture/disable").await;
    fx.drain();
    let seen = fx.seen.lock().unwrap();
    assert!(seen.len() > 10, "{}", seen.len());
    for text in seen.iter() {
        assert!(!text.contains(SECRET), "{text}");
        assert!(!text.contains(TOKEN), "{text}");
    }
}

//! Plugins end to end.
//!
//! (a) Through the real `smithagentsd` binary: packed native plugins beside a
//! copy of the daemon are discovered as built-ins, one of them syncs and then
//! panics, and the daemon keeps answering.
//! (b) In process: the GitHub plugin against a fake server on loopback that
//! serves the plugin's own test fixtures, through the manager and the API.
//! (c) An `#[ignore]` smoke test against the real GitHub API.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use smithagents_client::{ensure_daemon_with, Client, SpawnOptions};
use smithagents_core::daemon::read_lockfile;
use smithagents_core::event::Event;
use smithagents_core::plugin::{PluginState, PluginStatus};
use smithagents_core::VERSION;
use smithagents_plugin_github::GitHub;
use smithagents_plugin_host::{
    HttpPolicy, LinkedRuntime, PluginRuntime, StaticSecrets, ToolFinder,
};
use smithagents_plugin_protocol::{
    HostApi, HttpError, HttpRequest, HttpResponse, LogLevel, Manifest, Plugin, PluginError,
    SyncPage, SyncRequest,
};
use smithagents_plugins_tool::pack_fixtures_once;
use smithagents_server::api::{router, AppState};
use smithagents_server::events::EventBus;
use smithagents_server::feedback::FeedbackService;
use smithagents_server::harness::fake::FakeHarness;
use smithagents_server::intents::IntentService;
use smithagents_server::paths::Paths;
use smithagents_server::plugins::{LoadedPlugin, ManagerConfig, Origin, PluginManager};
use smithagents_server::queue::{Scheduler, SchedulerHandle};
use smithagents_server::service::AgentService;
use smithagents_server::store::Store;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tower::ServiceExt;

const PATIENCE: Duration = Duration::from_secs(60);
const DAEMON: &str = env!("CARGO_BIN_EXE_smithagentsd");

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn fixture_file(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../plugins/github/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

// ---------------------------------------------------------------------------
// (a) The real daemon with packed native plugins

/// A daemon started from a copy of the binary with packed plugins beside it,
/// in a temporary directory. The daemon is stopped on drop.
struct Daemon {
    tmp: TempDir,
    client: Client,
}

impl Daemon {
    /// Copies the daemon binary to `<tmp>/smithagentsd` and the packed
    /// plugins `ids` to `<tmp>/plugins/<id>`, all with explicit modes (the
    /// built-in root and what is in it must be owned by this user and not
    /// writable by others), and starts it with `env` and a home of its own.
    async fn start(ids: &[&str], env: &[(&str, &str)]) -> Daemon {
        let tmp = tempfile::tempdir().unwrap();
        chmod(tmp.path(), 0o755);
        let bin = tmp.path().join("smithagentsd");
        fs::copy(DAEMON, &bin).unwrap();
        chmod(&bin, 0o755);
        let root = tmp.path().join("plugins");
        fs::create_dir(&root).unwrap();
        chmod(&root, 0o755);
        for id in ids {
            let src = pack_fixtures_once().dir.join(id);
            let dst = root.join(id);
            fs::create_dir(&dst).unwrap();
            chmod(&dst, 0o755);
            for entry in fs::read_dir(&src).unwrap() {
                let entry = entry.unwrap();
                let to = dst.join(entry.file_name());
                fs::copy(entry.path(), &to).unwrap();
                let mode = if entry.file_name() == "plugin.toml" {
                    0o644
                } else {
                    0o755
                };
                chmod(&to, mode);
            }
        }
        let home = tmp.path().join("home");
        let options = SpawnOptions {
            args: vec![],
            env: env
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
        };
        let started =
            tokio::time::timeout(PATIENCE, ensure_daemon_with(&home, &bin, VERSION, &options))
                .await;
        let client = match started {
            Ok(Ok(client)) => client,
            other => {
                let log =
                    fs::read_to_string(home.join("run").join("daemon.log")).unwrap_or_default();
                stop_daemon(&home);
                panic!(
                    "the daemon did not start: {:?}; daemon log:\n{log}",
                    other.map(|r| r.map(|_| ()))
                )
            }
        };
        Daemon { tmp, client }
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    fn log(&self) -> String {
        fs::read_to_string(self.home().join("run").join("daemon.log")).unwrap_or_default()
    }

    /// Polls the plugin until `done` holds.
    async fn until(
        &self,
        id: &str,
        what: &str,
        done: impl Fn(&PluginStatus) -> bool,
    ) -> PluginStatus {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let status = self.client.plugin(id).await.unwrap();
            if done(&status) {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "gave up waiting until {what}: {status:?}\n{}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        stop_daemon(&self.home());
    }
}

/// SIGTERM to the daemon the lockfile names, then SIGKILL if it does not
/// remove its lockfile in time.
fn stop_daemon(home: &Path) {
    use rustix::process::{kill_process, Pid, Signal};

    let Ok(Some(info)) = read_lockfile(home) else {
        return;
    };
    let Some(pid) = i32::try_from(info.pid).ok().and_then(Pid::from_raw) else {
        return;
    };
    if kill_process(pid, Signal::TERM).is_err() {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if read_lockfile(home).ok().flatten().map(|i| i.pid) != Some(info.pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = kill_process(pid, Signal::KILL);
}

#[tokio::test]
async fn the_real_daemon_runs_packed_native_plugins_and_survives_a_panic() {
    const SECRET: &str = "fixture-dylib-secret-for-e2e";
    let daemon = Daemon::start(
        &["fixture-dylib", "github"],
        &[("SMITHAGENTS_SECRET_FIXTURE_DYLIB__TOKEN", SECRET)],
    )
    .await;
    let client = &daemon.client;

    // Both are built-ins running as native libraries, beside the plugin
    // compiled into the daemon, which has no resources.
    let all = client.plugins().await.unwrap();
    let ids: Vec<_> = all.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["feedback", "fixture-dylib", "github"], "{all:?}");
    for plugin in &all {
        assert!(plugin.builtin && plugin.enabled, "{plugin:?}");
        let host = match plugin.id.as_str() {
            "feedback" => "in-process",
            _ => "dylib",
        };
        assert_eq!(plugin.host, host, "{plugin:?}");
    }
    assert_eq!(all[0].resources, [], "{:?}", all[0]);

    // GitHub has no repositories configured: it waits for settings and runs
    // nothing (no tool, no request).
    let github = client.plugin("github").await.unwrap();
    assert!(
        matches!(github.state, PluginState::NeedsSettings { .. }),
        "{github:?}"
    );
    assert_eq!(github.hosts, ["api.github.com"]);

    // The fixture's other resources need a `url` setting. It names a URL the
    // host refuses before any request is made (not https), so none is sent.
    client
        .set_plugin_settings(
            "fixture-dylib",
            &json!({ "url": "http://fixture.invalid/" }),
        )
        .await
        .unwrap();

    // The fixture's `items` resource stores six items. Other declared
    // resources run too, and some fail by design: look at `items` only.
    let deadline = Instant::now() + PATIENCE;
    loop {
        let page = client
            .plugin_items("fixture-dylib", "items", None, None, None)
            .await
            .unwrap();
        if page.items.len() == 6 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no six items yet: {page:?}\n{}",
            daemon.log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A panic inside the library is caught: that plugin fails, and the
    // daemon answers.
    client
        .sync_plugin_resource("fixture-dylib", "panic")
        .await
        .unwrap();
    let failed = daemon
        .until("fixture-dylib", "the plugin failed", |p: &PluginStatus| {
            matches!(p.state, PluginState::Failed { .. })
        })
        .await;
    let panic = failed.resources.iter().find(|r| r.kind == "panic").unwrap();
    assert!(
        panic.last_error.as_deref().is_some_and(|e| !e.is_empty()),
        "{panic:?}"
    );
    let health = client.health().await.unwrap();
    assert_eq!(health.version, VERSION);
    // The items stay.
    let page = client
        .plugin_items("fixture-dylib", "items", None, None, None)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 6);
    // GitHub is untouched by the other plugin's failure.
    let github = client.plugin("github").await.unwrap();
    assert!(
        matches!(github.state, PluginState::NeedsSettings { .. }),
        "{github:?}"
    );

    // Neither the daemon's token nor the secret reached its log.
    let log = daemon.log();
    let daemon_token = read_lockfile(&daemon.home()).unwrap().unwrap().token;
    assert!(
        !log.contains(daemon_token.expose()),
        "the daemon token is logged"
    );
    assert!(!log.contains(SECRET), "a secret is logged");
}

// ---------------------------------------------------------------------------
// (b) GitHub in process against a fake server

/// A server's `Date` header; the plugin trusts timestamps only against it.
const DATE: &str = "Thu, 26 Nov 2026 10:00:00 GMT";
const REPO: &str = "octo-org/widgets";
const TOKEN: &str = "e2e-token-must-never-leak";
const DAEMON_TOKEN: &str = "daemon-token-123";

const ISSUES: &str =
    "/repos/octo-org/widgets/issues?state=all&sort=updated&direction=desc&per_page=100";
const ISSUES_PAGE2: &str = "/repositories/1/issues?page=2";
const ISSUES_SINCE: &str = "/repos/octo-org/widgets/issues?state=all&sort=updated&direction=desc&per_page=100&since=2026-01-07T09:00:00Z";
const PULLS: &str =
    "/repos/octo-org/widgets/pulls?state=all&sort=updated&direction=desc&per_page=100";

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn ok(body: Vec<u8>) -> Reply {
        Reply {
            status: 200,
            headers: vec![],
            body,
        }
    }

    fn with(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.into(), value.into()));
        self
    }

    fn status(status: u16, body: &str) -> Reply {
        Reply {
            status,
            headers: vec![],
            body: body.as_bytes().to_vec(),
        }
    }
}

/// What the fake saw of one request.
#[derive(Debug, Clone)]
struct Seen {
    target: String,
    authorization: Option<String>,
}

#[derive(Default)]
struct Routes {
    by_target: HashMap<String, Reply>,
    /// For targets not in `by_target`; else a 404.
    fallback: Option<Reply>,
    seen: Vec<Seen>,
}

/// A minimal HTTP/1.1 server on 127.0.0.1: one request per connection, the
/// reply chosen by exact path and query.
struct FakeGitHub {
    routes: Arc<Mutex<Routes>>,
    base: String,
}

impl FakeGitHub {
    async fn start() -> FakeGitHub {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes = Arc::new(Mutex::new(Routes::default()));
        let shared = Arc::clone(&routes);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve(stream, Arc::clone(&shared)));
            }
        });
        FakeGitHub { routes, base }
    }

    fn route(&self, target: &str, reply: Reply) {
        self.routes
            .lock()
            .unwrap()
            .by_target
            .insert(target.into(), reply);
    }

    fn fallback(&self, reply: Reply) {
        self.routes.lock().unwrap().fallback = Some(reply);
    }

    fn seen(&self) -> Vec<Seen> {
        self.routes.lock().unwrap().seen.clone()
    }

    /// The GitHub routes the plugin's fixtures describe, for one repository.
    fn serve_fixtures(&self) {
        self.route(
            "/repos/octo-org/widgets",
            Reply::ok(fixture_file("repo_widgets.json")),
        );
        self.route(
            ISSUES,
            Reply::ok(fixture_file("issues_page1.json")).with(
                "Link",
                "<https://api.github.com/repositories/1/issues?page=2>; rel=\"next\"",
            ),
        );
        self.route(ISSUES_PAGE2, Reply::ok(fixture_file("issues_page2.json")));
        self.route(ISSUES_SINCE, Reply::ok(b"[]".to_vec()));
        self.route(PULLS, Reply::ok(fixture_file("pulls_widgets.json")));
    }
}

async fn serve(mut stream: tokio::net::TcpStream, routes: Arc<Mutex<Routes>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = head.lines();
    let target = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("")
        .to_string();
    let authorization = lines
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().to_string())
        })
        .filter(|v| !v.is_empty());
    let reply = {
        let mut routes = routes.lock().unwrap();
        routes.seen.push(Seen {
            target: target.clone(),
            authorization,
        });
        routes
            .by_target
            .get(&target)
            .or(routes.fallback.as_ref())
            .cloned()
            .unwrap_or_else(|| Reply::status(404, "{\"message\":\"Not Found\"}"))
    };
    let mut out = format!(
        "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    if !reply
        .headers
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case("date"))
    {
        out.push_str(&format!("Date: {DATE}\r\n"));
    }
    for (name, value) in &reply.headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    let _ = stream.write_all(out.as_bytes()).await;
    let _ = stream.write_all(&reply.body).await;
    let _ = stream.shutdown().await;
}

/// The GitHub plugin with the host's address for `https://api.github.com`
/// swapped for the fake server's: the plugin names the real API, the test
/// reaches the fake. Everything else about the request is unchanged.
struct AtFake {
    base: String,
}

impl Plugin for AtFake {
    fn sync(&self, host: &dyn HostApi, req: SyncRequest) -> Result<SyncPage, PluginError> {
        GitHub.sync(
            &Rewriting {
                inner: host,
                base: &self.base,
            },
            req,
        )
    }
}

struct Rewriting<'a> {
    inner: &'a dyn HostApi,
    base: &'a str,
}

impl HostApi for Rewriting<'_> {
    fn http(&self, mut req: HttpRequest) -> Result<HttpResponse, HttpError> {
        if let Some(rest) = req.url.strip_prefix("https://api.github.com") {
            req.url = format!("{}{rest}", self.base);
        }
        self.inner.http(req)
    }

    fn log(&self, level: LogLevel, message: &str) {
        self.inner.log(level, message);
    }
}

/// Everything logged through `tracing` in this process since the first call.
fn captured_log() -> Arc<Mutex<Vec<u8>>> {
    static LOG: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    Arc::clone(LOG.get_or_init(|| {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(move || Sink(Arc::clone(&sink)))
            .finish();
        // Set once for the whole test binary.
        tracing::subscriber::set_global_default(subscriber)
            .expect("no other tracing subscriber is installed in this test binary");
        log
    }))
}

struct Github {
    _tmp: TempDir,
    fake: FakeGitHub,
    state: AppState,
    events: Mutex<broadcast::Receiver<Event>>,
    /// Every API answer, as text.
    seen: Mutex<Vec<String>>,
    _sched: SchedulerHandle,
}

/// The manager over the GitHub plugin (its own manifest) and the fake. The
/// manifest keeps its declarations but not its `gh` tool: no command-line
/// tool is looked for or run, and the secret comes from `StaticSecrets`.
async fn github() -> Github {
    captured_log();
    let fake = FakeGitHub::start().await;
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
        paths.clone(),
    );
    let sched = Scheduler::start(intents.clone(), service.clone(), store.clone(), bus.clone());

    let mut manifest =
        Manifest::parse(include_str!("../../../plugins/github/plugin.toml")).unwrap();
    manifest.tools.clear();
    manifest.secrets[0].from_tool = None;
    let plugin: Arc<dyn Plugin> = Arc::new(AtFake {
        base: fake.base.clone(),
    });
    let runtime: Arc<dyn PluginRuntime> =
        Arc::new(LinkedRuntime::new(plugin, Duration::from_secs(60)));
    let secrets = StaticSecrets(
        [(("github".to_owned(), "token".to_owned()), TOKEN.to_owned())]
            .into_iter()
            .collect(),
    );
    let cfg = ManagerConfig {
        http: HttpPolicy {
            allow_insecure_loopback: true,
            ..HttpPolicy::default()
        },
        // Nothing is looked for anywhere.
        tools: ToolFinder::new(vec![tmp.path().join("no-tools")]),
        ..ManagerConfig::default()
    };
    let manager = PluginManager::start(
        store.clone(),
        bus.clone(),
        Arc::new(secrets),
        vec![LoadedPlugin {
            manifest,
            origin: Origin::Builtin,
            runtime: Ok(runtime),
        }],
        cfg,
    )
    .await
    .unwrap();
    let feedback = FeedbackService::new(
        store.clone(),
        intents.clone(),
        service.clone(),
        manager.clone(),
        paths,
    );
    let state = AppState {
        service,
        intents,
        feedback,
        bus,
        store,
        plugins: manager,
        token: Arc::from(DAEMON_TOKEN),
        instance_id: Arc::from("0123456789abcdef"),
        ws_clients: Arc::new(AtomicUsize::new(0)),
        started_at: Instant::now(),
    };
    Github {
        _tmp: tmp,
        fake,
        state,
        events: Mutex::new(events),
        seen: Mutex::new(vec![]),
        _sched: sched,
    }
}

impl Github {
    async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {DAEMON_TOKEN}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        let resp = router(self.state.clone()).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.seen.lock().unwrap().push(text.clone());
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn get(&self, uri: &str) -> Value {
        let (status, body) = self.call(Method::GET, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    /// Configures the repository, then enables the plugin.
    async fn start_syncing(&self) {
        let (status, body) = self
            .call(
                Method::PUT,
                "/v1/plugins/github/settings",
                Some(json!({ "repositories": [REPO] })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = self
            .call(Method::POST, "/v1/plugins/github/enable", None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    async fn until(&self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let body = self.get("/v1/plugins/github").await;
            if pred(&body) {
                return body;
            }
            assert!(
                Instant::now() < deadline,
                "gave up waiting until {what}: {body}\nfake saw: {:?}",
                self.fake.seen()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn item_ids(&self, kind: &str) -> Vec<String> {
        let page = self
            .get(&format!(
                "/v1/plugins/github/resources/{kind}/items?limit=100"
            ))
            .await;
        let mut ids: Vec<String> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    }

    /// The token is in no API answer, no event and no log line.
    fn assert_token_nowhere(&self) {
        let mut texts = self.seen.lock().unwrap().clone();
        let mut events = self.events.lock().unwrap();
        while let Ok(event) = events.try_recv() {
            texts.push(serde_json::to_string(&event).unwrap());
        }
        texts.push(String::from_utf8_lossy(&captured_log().lock().unwrap()).into_owned());
        for text in texts {
            assert!(!text.contains(TOKEN), "the token leaked into {text:?}");
        }
    }
}

fn item_counts(plugin: &Value) -> Vec<(String, u64)> {
    plugin["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["kind"].as_str().unwrap().to_string(),
                r["item_count"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn resource<'a>(plugin: &'a Value, kind: &str) -> &'a Value {
    plugin["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == kind)
        .unwrap()
}

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

#[tokio::test]
async fn github_syncs_repositories_issues_and_pull_requests_with_the_token_attached() {
    let gh = github().await;
    gh.fake.serve_fixtures();
    gh.start_syncing().await;

    let done = gh
        .until("all three resources synced", |p| {
            item_counts(p)
                == [
                    ("repository".to_string(), 1),
                    ("issue".to_string(), 2),
                    ("pull_request".to_string(), 3),
                ]
        })
        .await;
    assert_eq!(done["state"], json!({ "kind": "ready" }), "{done}");
    assert_eq!(done["builtin"], true);

    assert_eq!(gh.item_ids("repository").await, [REPO]);
    // The pull request that the issues endpoint also lists is skipped.
    assert_eq!(
        gh.item_ids("issue").await,
        ["octo-org/widgets#1", "octo-org/widgets#3"]
    );
    assert_eq!(
        gh.item_ids("pull_request").await,
        [
            "octo-org/widgets#7",
            "octo-org/widgets#8",
            "octo-org/widgets#9"
        ]
    );
    let issues = gh
        .get("/v1/plugins/github/resources/issue/items?q=spin")
        .await;
    assert_eq!(issues["items"][0]["title"], "Widget does not spin");
    assert_eq!(
        issues["items"][0]["url"],
        "https://github.com/octo-org/widgets/issues/1"
    );

    // The next run of the issue listing asks only for what changed since the
    // newest update it saw, and keeps what it has.
    let (status, body) = gh
        .call(
            Method::POST,
            "/v1/plugins/github/resources/issue/sync",
            None,
        )
        .await;
    assert!(status.is_success(), "{status}: {body}");
    let deadline = Instant::now() + PATIENCE;
    while !gh.fake.seen().iter().any(|s| s.target == ISSUES_SINCE) {
        assert!(
            Instant::now() < deadline,
            "the since request never came: {:?}",
            gh.fake.seen()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(gh.item_ids("issue").await.len(), 2);

    // Every request, whichever page, carried the token.
    let seen = gh.fake.seen();
    let targets: Vec<_> = seen.iter().map(|s| s.target.as_str()).collect();
    for wanted in [ISSUES, ISSUES_PAGE2, PULLS, "/repos/octo-org/widgets"] {
        assert!(
            targets.contains(&wanted),
            "{wanted} not requested: {targets:?}"
        );
    }
    for request in &seen {
        assert_eq!(
            request.authorization.as_deref(),
            Some(bearer().as_str()),
            "{}",
            request.target
        );
    }
    gh.assert_token_nowhere();
}

#[tokio::test]
async fn a_rejected_token_needs_auth() {
    let gh = github().await;
    gh.fake
        .fallback(Reply::status(401, "{\"message\":\"Bad credentials\"}"));
    gh.start_syncing().await;

    let plugin = gh
        .until("the plugin needs auth", |p| {
            p["state"]["kind"] == "needs_auth"
        })
        .await;
    assert!(
        !plugin["state"]["message"].as_str().unwrap().is_empty(),
        "{plugin}"
    );
    assert!(gh
        .fake
        .seen()
        .iter()
        .all(|s| s.authorization.as_deref() == Some(bearer().as_str())));
    gh.assert_token_nowhere();
}

#[tokio::test]
async fn a_rate_limit_delays_the_next_run_to_the_reset_time() {
    let gh = github().await;
    let date = DateTime::parse_from_rfc2822(DATE).unwrap().timestamp();
    // The limit resets ten minutes after the server's clock.
    gh.fake.fallback(
        Reply::status(403, "{\"message\":\"API rate limit exceeded\"}")
            .with("X-RateLimit-Remaining", "0")
            .with("X-RateLimit-Reset", &(date + 600).to_string()),
    );
    gh.start_syncing().await;

    // The listings run every five minutes, so the ten minutes decide.
    let plugin = gh
        .until("both listings are rate limited", |p| {
            ["issue", "pull_request"].iter().all(|kind| {
                let r = resource(p, kind);
                r["next_due_at"].is_string()
                    && r["last_error"]
                        .as_str()
                        .is_some_and(|e| e.starts_with("rate limited"))
            })
        })
        .await;
    assert_eq!(plugin["state"], json!({ "kind": "ready" }), "{plugin}");
    let now = Utc::now();
    for kind in ["issue", "pull_request"] {
        let due: DateTime<Utc> = resource(&plugin, kind)["next_due_at"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let wait = (due - now).num_seconds();
        assert!((540..=600).contains(&wait), "{kind}: due in {wait} s");
    }
    gh.assert_token_nowhere();
}

// ---------------------------------------------------------------------------
// (c) The real GitHub API

/// Read-only smoke test of the packed GitHub library against the real API:
/// syncs one repository's metadata and prints how many items came back.
///
/// Needs `GITHUB_TOKEN` and `SMITHAGENTS_TEST_REPO` (`owner/name`). Run it on
/// purpose with `--ignored`; it is never part of the gate. It prints neither
/// the token nor the repository.
#[tokio::test]
#[ignore = "calls the real GitHub API; needs GITHUB_TOKEN and SMITHAGENTS_TEST_REPO"]
async fn real_github_smoke() {
    let token = std::env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN");
    let repo = std::env::var("SMITHAGENTS_TEST_REPO").expect("SMITHAGENTS_TEST_REPO");
    let daemon = Daemon::start(&["github"], &[("SMITHAGENTS_SECRET_GITHUB__TOKEN", &token)]).await;
    let client = &daemon.client;
    client
        .set_plugin_settings("github", &json!({ "repositories": [repo] }))
        .await
        .unwrap();
    client
        .sync_plugin_resource("github", "repository")
        .await
        .unwrap();
    let status = daemon
        .until("github", "the repository synced", |p: &PluginStatus| {
            p.resources
                .iter()
                .any(|r| r.kind == "repository" && r.last_ok_at.is_some())
        })
        .await;
    let count = status
        .resources
        .iter()
        .find(|r| r.kind == "repository")
        .map_or(0, |r| r.item_count);
    println!("synced {count} repository item(s)");
    assert_eq!(count, 1);
}

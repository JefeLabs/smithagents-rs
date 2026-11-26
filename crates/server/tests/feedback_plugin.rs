//! The `feedback` built-in plugin: a manifest compiled into the daemon with
//! two command-line tools and no resources, whose state is advice from a tool
//! check made when its status is read; and the manager's runner for the
//! declared tools of a built-in plugin.
//!
//! Every tool is a fake from `tests/bin` on the manager's only search path
//! ([`FakeTools`]): no real `gh`, `mise` or `agent-browser` is found or run.
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use common::feedback::FakeTools;
use smithagents_core::plugin::{MissingTool, PluginState, PluginStatus};
use smithagents_plugin_host::{
    CallError, CallHandle, CallPurpose, HostServices, HttpPolicy, MissingReason, PluginRuntime,
    SecretSource, StaticSecrets, ToolRun, ToolRunError, ToolState,
};
use smithagents_plugin_protocol::{HostMode, Manifest, SyncPage, SyncRequest};
use smithagents_server::events::EventBus;
use smithagents_server::plugins::{
    embedded_plugins, LoadedPlugin, ManagerConfig, ManagerError, Origin, PluginManager,
    FEEDBACK_PLUGIN_ID,
};
use smithagents_server::store::Store;
use tempfile::TempDir;

/// The text the daemon embeds.
const MANIFEST: &str = include_str!("../src/plugins/embedded/feedback.toml");
const FAKE_GH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/bin/fake-gh");
const SECOND: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------
// Environment

/// Keeps a paused clock still while held: tokio does not move a paused clock
/// on its own while a blocking task runs (see `plugin_manager.rs`).
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

/// A store opened before the clock is paused (its pool's connect has a
/// timer of its own), then the clock paused and frozen.
async fn paused_store() -> (Store, Frozen) {
    let store = Store::open_in_memory().await.unwrap();
    tokio::time::pause();
    (store, freeze())
}

fn no_secrets() -> Arc<dyn SecretSource> {
    Arc::new(StaticSecrets(Default::default()))
}

async fn start(store: Store, plugins: Vec<LoadedPlugin>, fake: &FakeTools) -> PluginManager {
    PluginManager::start(
        store,
        EventBus::new(256),
        no_secrets(),
        plugins,
        ManagerConfig {
            tools: fake.finder(),
            ..ManagerConfig::default()
        },
    )
    .await
    .unwrap()
}

/// A manager holding the embedded plugins only.
async fn embedded_only(fake: &FakeTools) -> PluginManager {
    let store = Store::open_in_memory().await.unwrap();
    start(store, embedded_plugins(), fake).await
}

async fn status(m: &PluginManager, id: &str) -> PluginStatus {
    m.get(id).await.expect("plugin listed")
}

/// The names of the tools a `NeedsSetup` state lists; empty for any other.
fn missing(state: &PluginState) -> Vec<String> {
    match state {
        PluginState::NeedsSetup { tools, .. } => tools.iter().map(|t| t.name.clone()).collect(),
        _ => Vec::new(),
    }
}

/// Polls the plugin's status until `pred` holds; a paused clock stays still.
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

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| a.to_string()).collect()
}

/// `args` in `cwd`, with no extra variables and a 10 s timeout.
fn run<'a>(args: &'a [String], cwd: &'a Path) -> ToolRun<'a> {
    ToolRun {
        args,
        cwd,
        env: &[],
        timeout: Duration::from_secs(10),
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

fn hints(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn missing_gh() -> MissingTool {
    MissingTool {
        name: "gh".into(),
        label: "GitHub CLI".into(),
        reason: "not installed".into(),
        install_url: Some("https://cli.github.com/".into()),
        install: hints(&[("mise", "gh"), ("brew", "gh")]),
    }
}

fn missing_agent_browser() -> MissingTool {
    MissingTool {
        name: "agent-browser".into(),
        label: "agent-browser".into(),
        reason: "not installed".into(),
        install_url: None,
        install: hints(&[("mise", "npm:agent-browser")]),
    }
}

// ---------------------------------------------------------------------------
// Other plugins beside it

/// A runtime that answers every sync with an empty last page.
#[derive(Default)]
struct Quiet {
    calls: AtomicUsize,
}

impl Quiet {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl PluginRuntime for Quiet {
    fn mode(&self) -> HostMode {
        HostMode::InProcess
    }

    async fn sync(&self, _call: CallHandle, _req: SyncRequest) -> Result<SyncPage, CallError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(SyncPage {
            upserts: vec![],
            deletes: vec![],
            next_cursor: None,
            done: true,
            snapshot_complete: false,
        })
    }
}

const GH_TOOL: &str = "[[tools]]\nname = \"gh\"\nlabel = \"GitHub CLI\"\n";

/// A manifest with one host, one resource `items` synced hourly, and `extra`.
fn manifest(id: &str, extra: &str) -> Manifest {
    Manifest::parse(&format!(
        "id = \"{id}\"\nname = \"Test {id}\"\nversion = \"1.0.0\"\ncontract = 1\n\
         host = \"in-process\"\nhosts = [\"api.example.com\"]\n{extra}\n\
         [[resources]]\nkind = \"items\"\nlabel = \"Items\"\ninterval_secs = 3600\n"
    ))
    .unwrap()
}

fn plugin(manifest: Manifest, origin: Origin, runtime: Arc<Quiet>) -> LoadedPlugin {
    LoadedPlugin {
        manifest,
        origin,
        runtime: Ok(runtime),
    }
}

/// The embedded plugins, then an installed plugin `third-party` that
/// declares `gh` too.
fn with_third_party() -> Vec<LoadedPlugin> {
    let mut plugins = embedded_plugins();
    plugins.push(plugin(
        manifest("third-party", GH_TOOL),
        Origin::Installed,
        Arc::default(),
    ));
    plugins
}

// ---------------------------------------------------------------------------
// The plugin

#[test]
fn the_feedback_manifest_has_two_tools_and_no_resources() {
    let m = Manifest::parse(MANIFEST).unwrap();

    assert_eq!(m.id.as_ref(), FEEDBACK_PLUGIN_ID);
    assert_eq!(FEEDBACK_PLUGIN_ID, "feedback");
    assert_eq!(m.name, "Feedback");
    assert_eq!(m.host, HostMode::InProcess);
    assert_eq!(m.entry, None);
    let names: Vec<&str> = m.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["gh", "agent-browser"]);
    let (gh, browser) = (&m.tools[0], &m.tools[1]);
    assert_eq!(gh.label, "GitHub CLI");
    assert_eq!(gh.install_url.as_deref(), Some("https://cli.github.com/"));
    assert_eq!(gh.install, hints(&[("mise", "gh"), ("brew", "gh")]));
    assert_eq!(browser.label, "agent-browser");
    assert_eq!(browser.install_url, None);
    assert_eq!(browser.install["mise"], "npm:agent-browser");
    assert_eq!(browser.install.len(), 1);
    assert!(m.tools.iter().all(|t| t.min_version.is_none()));
    assert!(m.resources.is_empty());
    assert!(m.hosts.is_empty());
    assert!(m.secrets.is_empty());
    assert!(m.settings.is_empty());

    // The daemon registers exactly this, as a built-in plugin.
    let plugins = embedded_plugins();
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].manifest, m);
    assert_eq!(plugins[0].origin, Origin::Builtin);
}

#[tokio::test]
async fn its_runtime_has_nothing_to_sync() {
    let plugins = embedded_plugins();
    let runtime = plugins[0].runtime.as_ref().expect("a runtime");
    assert_eq!(runtime.mode(), HostMode::InProcess);

    let services = Arc::new(
        HostServices::new(&plugins[0].manifest, no_secrets(), HttpPolicy::default()).unwrap(),
    );
    let req = SyncRequest {
        resource: "anything".into(),
        cursor: None,
        settings: serde_json::json!({}),
    };
    assert_eq!(
        runtime
            .sync(services.begin_call(CallPurpose::Sync), req)
            .await,
        Err(CallError::Contract {
            message: "this plugin has nothing to sync".into()
        })
    );
}

#[tokio::test]
async fn it_is_listed_as_a_ready_built_in_plugin() {
    let root = TempDir::new().unwrap();
    let fake = FakeTools::new(root.path(), true, true);
    let m = embedded_only(&fake).await;

    let s = status(&m, FEEDBACK_PLUGIN_ID).await;

    assert_eq!(s.id, "feedback");
    assert_eq!(s.name, "Feedback");
    assert_eq!(s.version, "0.1.0");
    assert!(s.builtin);
    assert!(s.enabled);
    assert_eq!(s.host, "in-process");
    assert_eq!(s.resources, []);
    assert!(s.hosts.is_empty() && s.secrets.is_empty() && s.settings.is_empty());
    assert_eq!(s.state, PluginState::Ready);
    assert_eq!(m.list().await, [s]);
    // Reading the status only looks for the tools; none was run.
    assert_eq!(fake.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn missing_tools_are_advice() {
    let root = TempDir::new().unwrap();
    let m = embedded_only(&FakeTools::new(root.path(), false, false)).await;

    let s = status(&m, FEEDBACK_PLUGIN_ID).await;
    assert!(s.enabled);
    assert_eq!(
        s.state,
        PluginState::NeedsSetup {
            tools: vec![missing_gh(), missing_agent_browser()],
            message: "needs GitHub CLI (gh): not installed; \
                      needs agent-browser (agent-browser): not installed"
                .into(),
        }
    );
    // Disabled, it is only that.
    let s = m.set_enabled(FEEDBACK_PLUGIN_ID, false).await.unwrap();
    assert_eq!(s.state, PluginState::Disabled);

    // Only `gh`.
    let root = TempDir::new().unwrap();
    let fake = FakeTools::new(root.path(), true, false);
    let m = embedded_only(&fake).await;
    assert_eq!(
        status(&m, FEEDBACK_PLUGIN_ID).await.state,
        PluginState::NeedsSetup {
            tools: vec![missing_agent_browser()],
            message: "needs agent-browser (agent-browser): not installed".into(),
        }
    );
    // Advice only: the tool that is there runs.
    let work = TempDir::new().unwrap();
    m.run_tool(
        FEEDBACK_PLUGIN_ID,
        "gh",
        run(&args(&["auth", "status"]), work.path()),
    )
    .await
    .unwrap();
    assert_eq!(fake.calls(), ["auth status"]);
}

#[tokio::test]
async fn the_state_follows_the_tools_without_a_restart() {
    let (store, _frozen) = paused_store().await;
    let root = TempDir::new().unwrap();
    let m = start(
        store,
        embedded_plugins(),
        &FakeTools::new(root.path(), false, true),
    )
    .await;
    assert_eq!(missing(&status(&m, FEEDBACK_PLUGIN_ID).await.state), ["gh"]);

    // Installed by hand: the check is trusted for 60 s, then made again.
    FakeTools::new(root.path(), true, true);
    assert_eq!(missing(&status(&m, FEEDBACK_PLUGIN_ID).await.state), ["gh"]);
    tokio::time::advance(61 * SECOND).await;
    assert_eq!(
        status(&m, FEEDBACK_PLUGIN_ID).await.state,
        PluginState::Ready
    );

    // And removed again.
    std::fs::remove_file(root.path().join("bin/gh")).unwrap();
    tokio::time::advance(61 * SECOND).await;
    assert_eq!(missing(&status(&m, FEEDBACK_PLUGIN_ID).await.state), ["gh"]);
}

#[tokio::test]
async fn installing_a_tool_checks_again_at_once_and_leaves_no_problem_behind() {
    let (store, _frozen) = paused_store().await;
    let root = TempDir::new().unwrap();
    let fake = FakeTools::new(root.path(), false, true);
    let log = root.path().join("mise.log");
    let mise = |then: &str| {
        script(
            &fake.bin,
            "mise",
            &format!("printf '%s\\n' \"$@\" >> '{}'\n{then}", log.display()),
        )
    };
    let m = start(store, embedded_plugins(), &fake).await;
    assert_eq!(missing(&status(&m, FEEDBACK_PLUGIN_ID).await.state), ["gh"]);

    // mise succeeds but puts nothing on the path: still missing...
    mise("exit 0");
    let s = m.install_tool(FEEDBACK_PLUGIN_ID, "gh").await.unwrap();
    assert_eq!(missing(&s.state), ["gh"]);
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "use\n--global\ngh\n"
    );
    // ... but nothing holds that answer: once `gh` is there, the next check
    // finds it.
    FakeTools::new(root.path(), true, true);
    tokio::time::advance(61 * SECOND).await;
    assert_eq!(
        status(&m, FEEDBACK_PLUGIN_ID).await.state,
        PluginState::Ready
    );

    // An install that works shows at once, without waiting for the cache.
    std::fs::remove_file(root.path().join("bin/gh")).unwrap();
    tokio::time::advance(61 * SECOND).await;
    assert_eq!(missing(&status(&m, FEEDBACK_PLUGIN_ID).await.state), ["gh"]);
    let gh = fake.bin.join("gh");
    mise(&format!(
        "cp '{FAKE_GH}' '{0}'; chmod 755 '{0}'",
        gh.display()
    ));
    let s = m.install_tool(FEEDBACK_PLUGIN_ID, "gh").await.unwrap();
    assert_eq!(s.state, PluginState::Ready);
    assert_eq!(fake.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn an_embedded_id_wins_over_a_directory_plugin() {
    let root = TempDir::new().unwrap();
    let fake = FakeTools::new(root.path(), true, true);
    let impostor = Arc::new(Quiet::default());
    let mut plugins = embedded_plugins();
    plugins.push(plugin(
        manifest("feedback", ""),
        Origin::Installed,
        impostor.clone(),
    ));
    let store = Store::open_in_memory().await.unwrap();
    let m = start(store, plugins, &fake).await;

    let all = m.list().await;

    let ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["feedback"]);
    assert!(all[0].builtin && all[0].enabled);
    assert_eq!(all[0].name, "Feedback");
    assert_eq!(all[0].host, "in-process");
    assert_eq!(all[0].resources, []);
    assert_eq!(all[0].state, PluginState::Ready);
    assert_eq!(impostor.calls(), 0);
}

// ---------------------------------------------------------------------------
// Running a declared tool

#[tokio::test]
async fn run_tool_runs_only_declared_tools_of_built_in_plugins() {
    let root = TempDir::new().unwrap();
    let fake = FakeTools::new(root.path(), true, true);
    // On the same path, but declared by no built-in plugin.
    let ran = root.path().join("mise-ran");
    script(&fake.bin, "mise", &format!("touch '{}'", ran.display()));
    let store = Store::open_in_memory().await.unwrap();
    let m = start(store, with_third_party(), &fake).await;
    let work = TempDir::new().unwrap();

    let auth = args(&["auth", "status"]);
    let answer = m
        .run_tool(
            FEEDBACK_PLUGIN_ID,
            "gh",
            ToolRun {
                env: &[("GH_PROMPT_DISABLED", "1")],
                ..run(&auth, work.path())
            },
        )
        .await;
    assert!(matches!(answer, Ok(None)), "{answer:?}");
    assert_eq!(fake.calls(), ["auth status"]);
    let env = fake.read("env.log").unwrap();
    assert!(env.lines().any(|l| l == "GH_PROMPT_DISABLED=1"), "{env}");

    // The answer is the first line the tool printed.
    fake.set("visibility", "PUBLIC");
    let answer = m
        .run_tool(
            FEEDBACK_PLUGIN_ID,
            "gh",
            run(&args(&["repo", "view"]), work.path()),
        )
        .await;
    assert_eq!(answer.unwrap().as_deref(), Some("PUBLIC"));

    // Refused before anything runs.
    let refused = [
        (FEEDBACK_PLUGIN_ID, "mise", "UnknownTool"),
        (FEEDBACK_PLUGIN_ID, "../bin/gh", "UnknownTool"),
        ("nope", "gh", "UnknownPlugin"),
        ("third-party", "gh", "NotAllowed"),
    ];
    for (id, tool, want) in refused {
        let err = m
            .run_tool(id, tool, run(&auth, work.path()))
            .await
            .unwrap_err();
        let got = match err {
            ManagerError::UnknownTool => "UnknownTool",
            ManagerError::UnknownPlugin => "UnknownPlugin",
            ManagerError::NotAllowed => "NotAllowed",
            other => panic!("{id} {tool}: {other:?}"),
        };
        assert_eq!(got, want, "{id} {tool}");
    }
    assert!(!ran.exists(), "mise ran");
    assert_eq!(fake.calls(), ["auth status", "repo view"]);

    // A failure carries an exit status, never what the tool printed.
    fake.set("fail", "auth status");
    let err = m
        .run_tool(FEEDBACK_PLUGIN_ID, "gh", run(&auth, work.path()))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ManagerError::Tool(ToolRunError::Failed { status: Some(1) })
        ),
        "{err:?}"
    );
    assert!(!format!("{err:?} {err}").contains("LEAK"), "{err}");
}

#[tokio::test]
async fn tool_state_reports_a_missing_tool() {
    let root = TempDir::new().unwrap();
    let fake = FakeTools::new(root.path(), true, false);
    let store = Store::open_in_memory().await.unwrap();
    let m = start(store, with_third_party(), &fake).await;

    assert_eq!(
        m.tool_state(FEEDBACK_PLUGIN_ID, "gh").await.unwrap(),
        ToolState::Ready {
            path: fake.bin.join("gh"),
            version: None
        }
    );
    assert_eq!(
        m.tool_state(FEEDBACK_PLUGIN_ID, "agent-browser")
            .await
            .unwrap(),
        ToolState::Missing {
            reason: MissingReason::NotFound
        }
    );
    // A fresh check each time, not the status's cached one.
    let _ = status(&m, FEEDBACK_PLUGIN_ID).await;
    FakeTools::new(root.path(), true, true);
    assert_eq!(
        m.tool_state(FEEDBACK_PLUGIN_ID, "agent-browser")
            .await
            .unwrap(),
        ToolState::Ready {
            path: fake.bin.join("agent-browser"),
            version: None
        }
    );

    assert!(matches!(
        m.tool_state(FEEDBACK_PLUGIN_ID, "mise").await,
        Err(ManagerError::UnknownTool)
    ));
    assert!(matches!(
        m.tool_state("nope", "gh").await,
        Err(ManagerError::UnknownPlugin)
    ));
    assert!(matches!(
        m.tool_state("third-party", "gh").await,
        Err(ManagerError::NotAllowed)
    ));
    // Checking a tool without a minimum version runs nothing.
    assert_eq!(fake.calls(), Vec::<String>::new());
}

// ---------------------------------------------------------------------------
// Plugins with resources

/// A plugin that syncs keeps checking its tools before its runs only: its
/// status is not a check. (The `plugin_manager` and `plugin_api` suites run
/// unchanged.)
#[tokio::test]
async fn plugins_with_resources_are_unchanged() {
    let (store, _frozen) = paused_store().await;
    let root = TempDir::new().unwrap();
    let fake = FakeTools::new(root.path(), true, true);
    let synced = Arc::new(Quiet::default());
    let mut plugins = embedded_plugins();
    plugins.push(plugin(
        manifest("synced", GH_TOOL),
        Origin::Builtin,
        synced.clone(),
    ));
    let m = start(store, plugins, &fake).await;
    until_status(&m, "synced", "its first run ends", |s| {
        s.resources[0].last_ok_at.is_some() && !s.resources[0].syncing
    })
    .await;
    assert_eq!(synced.calls(), 1);
    assert_eq!(status(&m, "synced").await.state, PluginState::Ready);

    // `gh` goes away. The feedback plugin says so when read; the syncing
    // plugin says so after its next run looks.
    std::fs::remove_file(root.path().join("bin/gh")).unwrap();
    tokio::time::advance(61 * SECOND).await;
    assert_eq!(missing(&status(&m, FEEDBACK_PLUGIN_ID).await.state), ["gh"]);
    assert_eq!(status(&m, "synced").await.state, PluginState::Ready);

    m.sync_now("synced", "items").await.unwrap();
    until_status(&m, "synced", "its run finds gh missing", |s| {
        missing(&s.state) == ["gh"]
    })
    .await;
    assert_eq!(synced.calls(), 1);
}

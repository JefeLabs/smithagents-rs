//! The WebAssembly runtime: the conformance suite, the limits and the sandbox,
//! against the fixture components packed by `smithagents-plugins-tool`.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithagents_plugin_host::{
    CallError, CallPurpose, LoadError, PluginRuntime, WasmEngine, WasmLimits, WasmRuntime,
};
use smithagents_plugin_protocol::{HostMode, Manifest, SyncPage, SyncRequest};
use smithagents_plugins_tool::pack_fixtures_once;

use common::{fixture_services, TestServer};

/// The entry file of a packed fixture, as its manifest names it.
fn component(id: &str) -> PathBuf {
    let dir = pack_fixtures_once().dir.join(id);
    let manifest = std::fs::read_to_string(dir.join("plugin.toml")).unwrap();
    let manifest = Manifest::parse(&manifest).unwrap();
    dir.join(manifest.entry.unwrap())
}

async fn load(engine: &WasmEngine, limits: WasmLimits) -> WasmRuntime {
    WasmRuntime::load(engine, &component("fixture-wasm"), limits)
        .await
        .unwrap()
}

async fn call(
    runtime: &WasmRuntime,
    resource: &str,
    settings: Value,
) -> Result<SyncPage, CallError> {
    let req = SyncRequest {
        resource: resource.into(),
        cursor: None,
        settings,
    };
    runtime
        .sync(fixture_services().begin_call(CallPurpose::Sync), req)
        .await
}

fn limit(what: &str) -> CallError {
    CallError::Limit { what: what.into() }
}

#[tokio::test(flavor = "multi_thread")]
async fn conformance() {
    let server = TestServer::start().await;
    let engine = WasmEngine::new(None).unwrap();
    let limits = WasmLimits {
        call_timeout: Duration::from_millis(200),
        ..WasmLimits::default()
    };
    let runtime = load(&engine, limits).await;
    assert_eq!(runtime.mode(), HostMode::Wasm);
    common::conformance::run(Arc::new(runtime), fixture_services(), &server).await;
}

#[test]
fn the_default_limits_are_the_plan_decisions() {
    let limits = WasmLimits::default();
    assert_eq!(limits.memory_bytes, 64 * 1024 * 1024);
    assert_eq!(limits.cpu, Duration::from_secs(10));
    assert_eq!(limits.call_timeout, Duration::from_secs(60));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spinning_guest_hits_the_cpu_limit() {
    let engine = WasmEngine::new(None).unwrap();
    let limits = WasmLimits {
        cpu: Duration::from_millis(500),
        ..WasmLimits::default()
    };
    let runtime = load(&engine, limits).await;
    let started = Instant::now();
    assert_eq!(call(&runtime, "spin", json!({})).await, Err(limit("cpu")));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spinning_guest_is_stopped_at_the_call_timeout_before_its_cpu_budget() {
    let engine = WasmEngine::new(None).unwrap();
    let limits = WasmLimits {
        cpu: Duration::from_secs(10),
        call_timeout: Duration::from_millis(300),
        ..WasmLimits::default()
    };
    let runtime = load(&engine, limits).await;
    let started = Instant::now();
    assert_eq!(
        call(&runtime, "spin", json!({})).await,
        Err(CallError::Timeout)
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn waiting_on_the_host_does_not_use_the_cpu_budget() {
    // The guest sleeps through a host clock for longer than its CPU budget.
    let engine = WasmEngine::new(None).unwrap();
    let limits = WasmLimits {
        cpu: Duration::from_millis(300),
        call_timeout: Duration::from_secs(10),
        ..WasmLimits::default()
    };
    let runtime = load(&engine, limits).await;
    let page = call(&runtime, "sleep", json!({ "ms": "1000" }))
        .await
        .unwrap();
    assert!(page.done);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_growing_guest_hits_the_memory_limit_and_the_next_call_works() {
    let engine = WasmEngine::new(None).unwrap();
    let runtime = load(&engine, WasmLimits::default()).await;
    assert_eq!(
        call(&runtime, "bloat", json!({})).await,
        Err(limit("memory"))
    );
    let page = call(&runtime, "items", json!({})).await.unwrap();
    assert_eq!(page.upserts.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn forever_returns_ordinary_pages() {
    // Stopping a run that never ends is the scheduler's page budget.
    let engine = WasmEngine::new(None).unwrap();
    let runtime = load(&engine, WasmLimits::default()).await;
    for _ in 0..3 {
        let page = call(&runtime, "forever", json!({})).await.unwrap();
        assert!(!page.done);
        assert_eq!(page.next_cursor.as_deref(), Some("again"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_that_imports_the_filesystem_is_refused_at_load() {
    let engine = WasmEngine::new(None).unwrap();
    let path = component("fixture-wasm-fs");
    match WasmRuntime::load(&engine, &path, WasmLimits::default()).await {
        Err(LoadError::MissingImport { message }) => {
            assert!(message.contains("wasi:filesystem"), "{message}");
        }
        Err(other) => panic!("expected MissingImport, got {other:?}"),
        Ok(_) => panic!("a guest importing the filesystem was loaded"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_text_file_is_invalid() {
    let engine = WasmEngine::new(None).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugin.wasm");
    std::fs::write(&path, "(component)").unwrap();
    match WasmRuntime::load(&engine, &path, WasmLimits::default()).await {
        Err(LoadError::Invalid { .. }) => {}
        Err(other) => panic!("expected Invalid, got {other:?}"),
        Ok(_) => panic!("a text file was loaded"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_file_is_an_io_error() {
    let engine = WasmEngine::new(None).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("absent.wasm");
    match WasmRuntime::load(&engine, &path, WasmLimits::default()).await {
        Err(LoadError::Io(_)) => {}
        Err(other) => panic!("expected Io, got {other:?}"),
        Ok(_) => panic!("a missing file was loaded"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_component_over_64_mib_is_refused_before_it_is_read() {
    let engine = WasmEngine::new(None).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugin.wasm");
    // Sparse: no 64 MiB is written to disk.
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(64 * 1024 * 1024 + 1).unwrap();
    drop(file);
    match WasmRuntime::load(&engine, &path, WasmLimits::default()).await {
        Err(LoadError::Refused { message }) => {
            assert_eq!(message, "the component file is larger than 64 MiB");
        }
        Err(other) => panic!("expected Refused, got {other:?}"),
        Ok(_) => panic!("an oversized file was loaded"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ten_concurrent_calls_on_one_runtime_all_succeed() {
    let engine = WasmEngine::new(None).unwrap();
    let runtime = Arc::new(load(&engine, WasmLimits::default()).await);
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..10 {
        let runtime = Arc::clone(&runtime);
        calls.spawn(async move { call(&runtime, "items", json!({})).await });
    }
    let mut done = 0;
    while let Some(result) = calls.join_next().await {
        assert_eq!(result.unwrap().unwrap().upserts.len(), 2);
        done += 1;
    }
    assert_eq!(done, 10);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cached_component_loads_again() {
    let cache = tempfile::tempdir().unwrap();
    let engine = WasmEngine::new(Some(cache.path())).unwrap();
    load(&engine, WasmLimits::default()).await;
    let again = load(&engine, WasmLimits::default()).await;
    let page = call(&again, "items", json!({})).await.unwrap();
    assert_eq!(page.upserts.len(), 2);
}

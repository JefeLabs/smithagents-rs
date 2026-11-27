//! The dylib runtime against the `fixture-dylib` library packed by
//! `smithagents-plugins-tool`: the conformance suite, the stop after a
//! timeout, and host calls under a call number that is not running.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithagents_plugin_host::{CallError, CallPurpose, DylibRuntime, LoadError, PluginRuntime};
use smithagents_plugin_protocol::{HostMode, Manifest, SyncPage, SyncRequest};
use smithagents_plugins_tool::{pack_fixtures_once, BuiltinScratch};

use common::{fixture_services, TestServer};

/// The packed fixture library, as its manifest names it, copied into this
/// test executable's plugins folder: a native plugin is loaded only from
/// there. The copy goes when the returned folder is dropped.
fn library() -> (BuiltinScratch, PathBuf) {
    let dir = pack_fixtures_once().dir.join("fixture-dylib");
    let manifest = std::fs::read_to_string(dir.join("plugin.toml")).unwrap();
    let entry = Manifest::parse(&manifest).unwrap().entry.unwrap();
    let scratch = BuiltinScratch::new("plugin-host-dylib");
    let library = scratch.path().join(&entry);
    std::fs::copy(dir.join(&entry), &library).unwrap();
    (scratch, library)
}

async fn call(
    runtime: &dyn PluginRuntime,
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

#[tokio::test(flavor = "multi_thread")]
async fn conformance_then_every_call_is_refused() {
    let server = TestServer::start().await;
    let (_plugins, library) = library();
    let runtime = DylibRuntime::load(&library, Duration::from_millis(200)).unwrap();
    assert_eq!(runtime.mode(), HostMode::Dylib);
    let runtime: Arc<dyn PluginRuntime> = Arc::new(runtime);
    // The suite ends with a call that times out.
    common::conformance::run(Arc::clone(&runtime), fixture_services(), &server).await;

    let stopped = CallError::Crashed {
        message: "plugin stopped after a call timed out; restart the daemon".into(),
    };
    for resource in ["items", "sleep"] {
        let settings = json!({ "ms": "1" });
        assert_eq!(
            call(runtime.as_ref(), resource, settings).await,
            Err(stopped.clone())
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_call_under_a_finished_calls_number_is_a_bad_call() {
    let server = TestServer::start().await;
    let (_plugins, library) = library();
    let runtime = DylibRuntime::load(&library, Duration::from_secs(10)).unwrap();
    call(&runtime, "items", json!({})).await.unwrap();
    // `stale-call` sends an HTTP request under the number of the call above.
    let settings = json!({ "url": server.url("/ok") });
    let page = call(&runtime, "stale-call", settings).await.unwrap();
    let reply = &page.upserts[0].fields["reply"];
    assert_eq!(reply["type"], "bad_call", "{reply}");
    let message = reply["message"].as_str().unwrap();
    assert!(message.contains("no call"), "{message}");
    server.assert_untouched();
}

#[tokio::test(flavor = "multi_thread")]
async fn ten_concurrent_calls_on_one_runtime_all_succeed() {
    let (_plugins, library) = library();
    let runtime = DylibRuntime::load(&library, Duration::from_secs(10)).unwrap();
    let runtime = Arc::new(runtime);
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..10 {
        let runtime = Arc::clone(&runtime);
        calls.spawn(async move { call(runtime.as_ref(), "items", json!({})).await });
    }
    let mut done = 0;
    while let Some(result) = calls.join_next().await {
        assert_eq!(result.unwrap().unwrap().upserts.len(), 2);
        done += 1;
    }
    assert_eq!(done, 10);
}

#[test]
fn a_missing_library_is_a_dylib_load_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("libabsent.dylib");
    match DylibRuntime::load(&path, Duration::from_secs(1)) {
        Err(LoadError::Dylib { message }) => {
            assert!(message.contains("libabsent.dylib"), "{message}")
        }
        Err(other) => panic!("expected LoadError::Dylib, got {other:?}"),
        Ok(_) => panic!("a missing file was loaded"),
    }
}

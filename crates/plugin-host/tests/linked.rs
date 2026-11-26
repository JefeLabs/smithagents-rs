//! The linked runtime against the shared conformance suite.

mod common;

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use smithagents_plugin_fixture::Fixture;
use smithagents_plugin_host::{CallPurpose, LinkedRuntime, PluginRuntime};
use smithagents_plugin_protocol::{HostMode, SyncRequest};

use common::{fixture_services, TestServer};

fn runtime() -> Arc<dyn PluginRuntime> {
    Arc::new(LinkedRuntime::new(
        Arc::new(Fixture),
        Duration::from_millis(200),
    ))
}

#[tokio::test(flavor = "multi_thread")]
async fn conformance() {
    let server = TestServer::start().await;
    common::conformance::run(runtime(), fixture_services(), &server).await;
}

#[test]
fn the_linked_runtime_reports_in_process_mode() {
    assert_eq!(runtime().mode(), HostMode::InProcess);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_string_ms_setting_is_accepted() {
    let services = fixture_services();
    let req = SyncRequest {
        resource: "sleep".into(),
        cursor: None,
        settings: json!({ "ms": "1" }),
    };
    let page = runtime()
        .sync(services.begin_call(CallPurpose::Sync), req)
        .await
        .unwrap();
    assert!(page.done);
}

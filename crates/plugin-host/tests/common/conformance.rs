//! The runtime conformance suite. It is mode-neutral: the linked, WebAssembly
//! and dylib runtimes all run it against the same fixture plugin.
//!
//! The runtime must be built with a call timeout of at most a few hundred
//! milliseconds, and this suite never calls `spin`, `bloat` or `forever`. The
//! `sleep` case runs last: a runtime may refuse every call after a timeout.

use std::sync::Arc;

use serde_json::{json, Value};
use smithagents_plugin_host::{CallError, CallPurpose, HostServices, PluginRuntime};
use smithagents_plugin_protocol::{PluginError, SyncPage, SyncRequest};

use super::{TestServer, FIXTURE_TOKEN};

fn request(resource: &str, cursor: Option<&str>, settings: Value) -> SyncRequest {
    SyncRequest {
        resource: resource.into(),
        cursor: cursor.map(str::to_string),
        settings,
    }
}

async fn call(
    runtime: &Arc<dyn PluginRuntime>,
    services: &Arc<HostServices>,
    req: SyncRequest,
) -> Result<SyncPage, CallError> {
    runtime
        .sync(services.begin_call(CallPurpose::Sync), req)
        .await
}

async fn fail(
    runtime: &Arc<dyn PluginRuntime>,
    services: &Arc<HostServices>,
    resource: &str,
) -> CallError {
    match call(runtime, services, request(resource, None, json!({}))).await {
        Ok(page) => panic!("{resource}: expected an error, got {page:?}"),
        Err(err) => err,
    }
}

fn ids(page: &SyncPage) -> Vec<&str> {
    page.upserts.iter().map(|i| i.id.as_str()).collect()
}

pub async fn run(
    runtime: Arc<dyn PluginRuntime>,
    services: Arc<HostServices>,
    server: &TestServer,
) {
    // items: three pages of two, then nothing.
    let mut cursor: Option<String> = None;
    let mut seen = Vec::new();
    let mut cursors = Vec::new();
    for _ in 0..3 {
        let page = call(
            &runtime,
            &services,
            request("items", cursor.as_deref(), json!({})),
        )
        .await
        .unwrap();
        assert_eq!(page.upserts.len(), 2);
        seen.extend(ids(&page).into_iter().map(str::to_string));
        cursor = page.next_cursor.clone();
        cursors.push(cursor.clone().unwrap());
        assert_eq!(page.done, cursors.len() == 3);
        assert_eq!(page.snapshot_complete, page.done);
    }
    assert_eq!(
        seen,
        ["item-1", "item-2", "item-3", "item-4", "item-5", "item-6"]
    );
    assert_eq!(cursors, ["1", "2", "3"]);
    let tail = call(&runtime, &services, request("items", Some("3"), json!({})))
        .await
        .unwrap();
    assert!(tail.upserts.is_empty() && tail.done);

    // http: the body comes back and the server saw the injected bearer header.
    let settings = json!({ "url": server.url("/ok") });
    let page = call(&runtime, &services, request("http", None, settings.clone()))
        .await
        .unwrap();
    assert_eq!(page.upserts.len(), 1);
    assert_eq!(page.upserts[0].fields["status"], 200);
    assert_eq!(page.upserts[0].fields["body"], "hello");
    let expected = format!("Bearer {FIXTURE_TOKEN}");
    assert_eq!(server.last_headers()["authorization"], expected.as_str());
    assert!(!format!("{page:?}").contains(FIXTURE_TOKEN));

    // post: denied during a sync, and nothing reaches the server.
    let before = server.methods().len();
    match call(&runtime, &services, request("post", None, settings)).await {
        Err(CallError::Plugin(PluginError::Http { message })) => {
            assert!(message.contains("denied"), "{message}");
        }
        other => panic!("post: expected a denied HTTP error, got {other:?}"),
    }
    assert_eq!(server.methods().len(), before);
    assert!(!server.methods().iter().any(|m| m == "POST"));

    // auth and rate keep their payloads.
    assert_eq!(
        fail(&runtime, &services, "auth").await,
        CallError::Plugin(PluginError::Auth {
            message: "the token was rejected".into()
        })
    );
    assert_eq!(
        fail(&runtime, &services, "rate").await,
        CallError::Plugin(PluginError::RateLimited {
            retry_after_secs: 120,
            message: "slow down".into()
        })
    );

    // panic is a crash, and the runtime still serves the next call.
    assert!(matches!(
        fail(&runtime, &services, "panic").await,
        CallError::Crashed { .. }
    ));
    let page = call(&runtime, &services, request("items", None, json!({})))
        .await
        .unwrap();
    assert_eq!(ids(&page), ["item-1", "item-2"]);

    // Pages that break the contract.
    assert!(matches!(
        fail(&runtime, &services, "huge").await,
        CallError::Contract { .. }
    ));
    assert!(matches!(
        fail(&runtime, &services, "dupe").await,
        CallError::Contract { .. }
    ));

    // Last: a call that outlives the runtime's timeout.
    let slow = request("sleep", None, json!({ "ms": 3000 }));
    assert_eq!(
        call(&runtime, &services, slow).await.unwrap_err(),
        CallError::Timeout
    );
}

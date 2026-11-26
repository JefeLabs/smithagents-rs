//! The fixture plugin as a native library, plus one behaviour only this mode
//! has: `stale-call` sends an HTTP request under the number of the previous
//! call, which has finished, so the host must refuse it. The `Plugin` trait
//! hides call numbers, so `stale-call` lives below it, in a [`DylibHandler`].
//!
//! `spin` and `bloat` must never be called here: a native runtime cannot stop
//! a thread that does not return.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use smithagents_plugin_fixture::Fixture;
use smithagents_plugin_protocol::dylib::{DylibHandler, HostConnection, PluginHandler};
use smithagents_plugin_protocol::{
    HostCall, HttpMethod, HttpRequest, Item, PluginCall, PluginReply, SyncPage,
};

struct FixtureDylib {
    fixture: PluginHandler<Fixture>,
    /// The number of the call before the current one (0 before any).
    previous: AtomicU64,
}

impl DylibHandler for FixtureDylib {
    fn handle(&self, host: &HostConnection, call: PluginCall) -> PluginReply {
        let PluginCall::Sync {
            call: number,
            request,
        } = &call;
        let previous = self.previous.swap(*number, Ordering::SeqCst);
        if request.resource != "stale-call" {
            return self.fixture.handle(host, call);
        }
        let url = request
            .settings
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("https://fixture.invalid/")
            .to_string();
        let reply = host.call(&HostCall::Http {
            call: previous,
            request: HttpRequest {
                method: HttpMethod::Get,
                url,
                headers: Vec::new(),
                body: None,
                auth: None,
            },
        });
        // The host's answer, reported as the one item of the page.
        let item = Item {
            id: "stale-call".into(),
            title: "Stale call".into(),
            url: None,
            updated_at: None,
            version: None,
            fields: json!({ "call": previous, "reply": reply }),
        };
        PluginReply::Sync(Ok(SyncPage {
            upserts: vec![item],
            deletes: Vec::new(),
            next_cursor: None,
            done: true,
            snapshot_complete: true,
        }))
    }
}

smithagents_plugin_protocol::export_dylib_plugin!(handler: FixtureDylib {
    fixture: PluginHandler::new(Fixture),
    previous: AtomicU64::new(0),
});

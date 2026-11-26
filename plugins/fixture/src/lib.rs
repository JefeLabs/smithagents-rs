//! The fixture plugin: one resource per behaviour the host must handle, so the
//! same conformance suite can run against every runtime.
//!
//! `spin`, `bloat` and `forever` are for the WebAssembly tests only: a native
//! runtime cannot stop a thread that never returns.

use serde_json::{json, Value};
use smithagents_plugin_protocol::{
    Auth, HostApi, HttpMethod, HttpRequest, Item, Plugin, PluginError, SyncPage, SyncRequest,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct Fixture;

fn item(id: String) -> Item {
    Item {
        title: format!("Item {id}"),
        id,
        url: None,
        updated_at: None,
        version: None,
        fields: json!({}),
    }
}

fn page(upserts: Vec<Item>, next_cursor: Option<String>, done: bool) -> SyncPage {
    SyncPage {
        upserts,
        deletes: Vec::new(),
        next_cursor,
        done,
        snapshot_complete: done,
    }
}

fn empty_done() -> SyncPage {
    page(Vec::new(), None, true)
}

/// Three pages of two items; the cursors are "1" and "2", and the last page
/// carries "3". From "3" nothing is listed: an empty done page that keeps
/// "3" and claims no snapshot (a complete empty snapshot would tell the host
/// that every item is gone).
fn items(cursor: Option<&str>) -> SyncPage {
    let (first, next, done) = match cursor {
        None => (1, "1", false),
        Some("1") => (3, "2", false),
        Some("2") => (5, "3", true),
        Some("3") => {
            return SyncPage {
                snapshot_complete: false,
                ..page(Vec::new(), Some("3".into()), true)
            }
        }
        Some(_) => return empty_done(),
    };
    let upserts = (first..first + 2)
        .map(|n| item(format!("item-{n}")))
        .collect();
    page(upserts, Some(next.into()), done)
}

fn setting_url(settings: &Value) -> Result<String, PluginError> {
    settings
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| PluginError::Settings {
            message: "settings.url is required".into(),
        })
}

/// A number, or a string holding one: packed manifests declare settings as text.
fn setting_ms(settings: &Value) -> Result<u64, PluginError> {
    let ms = match settings.get("ms") {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    };
    ms.ok_or_else(|| PluginError::Settings {
        message: "settings.ms must be a number of milliseconds".into(),
    })
}

fn fetch(
    host: &dyn HostApi,
    method: HttpMethod,
    settings: &Value,
) -> Result<SyncPage, PluginError> {
    let resp = host
        .http(HttpRequest {
            method,
            url: setting_url(settings)?,
            headers: Vec::new(),
            body: None,
            auth: Some(Auth::Bearer {
                secret: "token".into(),
            }),
        })
        .map_err(|e| PluginError::Http {
            message: e.to_string(),
        })?;
    let mut it = item("http-1".into());
    it.fields = json!({
        "status": resp.status,
        "body": String::from_utf8_lossy(&resp.body),
    });
    Ok(page(vec![it], None, true))
}

impl Plugin for Fixture {
    fn sync(&self, host: &dyn HostApi, req: SyncRequest) -> Result<SyncPage, PluginError> {
        match req.resource.as_str() {
            "items" => Ok(items(req.cursor.as_deref())),
            "http" => fetch(host, HttpMethod::Get, &req.settings),
            "post" => fetch(host, HttpMethod::Post, &req.settings),
            "auth" => Err(PluginError::Auth {
                message: "the token was rejected".into(),
            }),
            "rate" => Err(PluginError::RateLimited {
                retry_after_secs: 120,
                message: "slow down".into(),
            }),
            "panic" => panic!("fixture panic"),
            "sleep" => {
                let ms = setting_ms(&req.settings)?;
                std::thread::sleep(std::time::Duration::from_millis(ms));
                Ok(empty_done())
            }
            "huge" => Ok(page(
                (0..501).map(|n| item(format!("huge-{n}"))).collect(),
                None,
                true,
            )),
            "dupe" => Ok(page(
                vec![item("same".into()), item("same".into())],
                None,
                true,
            )),
            "forever" => Ok(page(Vec::new(), Some("again".into()), false)),
            "spin" => loop {
                std::hint::spin_loop();
            },
            "bloat" => {
                let mut held: Vec<Vec<u8>> = Vec::new();
                loop {
                    let mut block: Vec<u8> = Vec::new();
                    if block.try_reserve_exact(1024 * 1024).is_err() {
                        return Err(PluginError::Other {
                            message: "allocation failed".into(),
                        });
                    }
                    block.resize(1024 * 1024, 1);
                    held.push(block);
                }
            }
            other => Err(PluginError::Other {
                message: format!("unknown resource {other:?}"),
            }),
        }
    }
}

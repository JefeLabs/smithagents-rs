#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};
use smithagents_plugin_github::GitHub;
use smithagents_plugin_protocol::{
    Auth, HostApi, HttpError, HttpMethod, HttpRequest, HttpResponse, LogLevel, Plugin, PluginError,
    SyncPage, SyncRequest,
};

pub const API: &str = "https://api.github.com";

pub fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// A host that answers exact URLs with canned responses, records every
/// request, and fails the test on a URL it was not told about.
#[derive(Default)]
pub struct Scripted {
    routes: Mutex<HashMap<String, HttpResponse>>,
    seen: Mutex<Vec<HttpRequest>>,
}

impl Scripted {
    pub fn new() -> Self {
        Self::default()
    }

    /// Like `on_raw`, but a `Date` header is added unless one is given: a
    /// server always sends one, and the plugin trusts timestamps only
    /// against it.
    pub fn on(self, url: &str, status: u16, headers: &[(&str, &str)], body: Vec<u8>) -> Self {
        let mut all = headers.to_vec();
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("date")) {
            all.push(("Date", "Thu, 26 Nov 2026 10:00:00 GMT"));
        }
        self.on_raw(url, status, &all, body)
    }

    /// Exactly the headers given, no more.
    pub fn on_raw(self, url: &str, status: u16, headers: &[(&str, &str)], body: Vec<u8>) -> Self {
        let resp = HttpResponse {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body,
        };
        self.routes.lock().unwrap().insert(url.to_string(), resp);
        self
    }

    pub fn ok(self, url: &str, body: Vec<u8>) -> Self {
        self.on(url, 200, &[], body)
    }

    pub fn ok_json(self, url: &str, body: &Value) -> Self {
        self.ok(url, serde_json::to_vec(body).unwrap())
    }

    pub fn requests(&self) -> Vec<HttpRequest> {
        self.seen.lock().unwrap().clone()
    }

    pub fn urls(&self) -> Vec<String> {
        self.requests().into_iter().map(|r| r.url).collect()
    }
}

impl HostApi for Scripted {
    fn http(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.seen.lock().unwrap().push(req.clone());
        match self.routes.lock().unwrap().get(&req.url) {
            Some(resp) => Ok(resp.clone()),
            None => panic!("unexpected request: {}", req.url),
        }
    }

    fn log(&self, _level: LogLevel, _message: &str) {}
}

/// A host whose transport always fails the same way.
pub struct Failing(pub HttpError);

impl HostApi for Failing {
    fn http(&self, _req: HttpRequest) -> Result<HttpResponse, HttpError> {
        Err(self.0.clone())
    }

    fn log(&self, _level: LogLevel, _message: &str) {}
}

pub fn settings(repos: &[&str]) -> Value {
    json!({ "repositories": repos })
}

pub fn request(resource: &str, cursor: Option<&str>, settings: Value) -> SyncRequest {
    SyncRequest {
        resource: resource.into(),
        cursor: cursor.map(str::to_string),
        settings,
    }
}

/// Run one sync call and check the page against the contract.
pub fn sync(
    host: &dyn HostApi,
    resource: &str,
    cursor: Option<&str>,
    settings: Value,
) -> Result<SyncPage, PluginError> {
    let page = GitHub.sync(host, request(resource, cursor, settings))?;
    page.validate().expect("page breaks the contract");
    if let Some(c) = &page.next_cursor {
        assert!(c.len() <= 4096, "cursor is {} bytes", c.len());
    }
    Ok(page)
}

/// Every request is a GET with the three headers and the bearer secret, and
/// none sets `Authorization` itself.
pub fn assert_standard(req: &HttpRequest) {
    assert_eq!(req.method, HttpMethod::Get);
    assert!(req.body.is_none());
    assert_eq!(
        req.auth,
        Some(Auth::Bearer {
            secret: "token".into()
        })
    );
    let header = |name: &str| {
        req.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(header("Accept"), Some("application/vnd.github+json"));
    assert_eq!(header("X-GitHub-Api-Version"), Some("2022-11-28"));
    assert_eq!(header("User-Agent"), Some("smithagents"));
    assert!(header("Authorization").is_none());
    assert!(header("Cookie").is_none());
}

pub fn assert_all_standard(host: &Scripted) {
    let reqs = host.requests();
    assert!(!reqs.is_empty());
    reqs.iter().for_each(assert_standard);
}

pub fn cursor_json(page: &SyncPage) -> Value {
    serde_json::from_str(page.next_cursor.as_deref().expect("a cursor")).unwrap()
}

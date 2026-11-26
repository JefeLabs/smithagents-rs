//! Helpers shared by the integration tests: a local HTTP server and the
//! runtime conformance suite.
#![allow(dead_code)]

pub mod conformance;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::serve::ListenerExt;
use axum::Router;

const MIB: usize = 1024 * 1024;

pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: HeaderMap,
}

#[derive(Default)]
pub struct Shared {
    pub seen: Mutex<Vec<Seen>>,
}

pub struct TestServer {
    pub port: u16,
    pub connections: Arc<AtomicUsize>,
    pub shared: Arc<Shared>,
}

impl TestServer {
    pub async fn start() -> TestServer {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicUsize::new(0));
        let counter = connections.clone();
        let listener = listener.tap_io(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let shared = Arc::new(Shared::default());
        let app = Router::new().fallback(handle).with_state(shared.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        TestServer {
            port,
            connections,
            shared,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub fn requests(&self) -> usize {
        self.shared.seen.lock().unwrap().len()
    }

    pub fn methods(&self) -> Vec<String> {
        let seen = self.shared.seen.lock().unwrap();
        seen.iter().map(|s| s.method.clone()).collect()
    }

    pub fn paths(&self) -> Vec<String> {
        let seen = self.shared.seen.lock().unwrap();
        seen.iter().map(|s| s.path.clone()).collect()
    }

    pub fn last_headers(&self) -> HeaderMap {
        let seen = self.shared.seen.lock().unwrap();
        seen.last()
            .expect("the server saw no request")
            .headers
            .clone()
    }

    pub fn assert_untouched(&self) {
        assert_eq!(self.requests(), 0, "the server saw a request");
        assert_eq!(
            self.connections.load(Ordering::SeqCst),
            0,
            "the server saw a connection"
        );
    }
}

async fn handle(State(shared): State<Arc<Shared>>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    shared.seen.lock().unwrap().push(Seen {
        method: req.method().to_string(),
        path: path.clone(),
        headers: req.headers().clone(),
    });
    match path.as_str() {
        "/ok" => ([("x-test", "yes")], "hello").into_response(),
        "/redirect" => (StatusCode::FOUND, [("location", "/target")]).into_response(),
        "/exact" => vec![b'x'; 5 * MIB].into_response(),
        "/big" => vec![b'x'; 5 * MIB + 1].into_response(),
        "/slow" => {
            tokio::time::sleep(Duration::from_secs(5)).await;
            "late".into_response()
        }
        _ => StatusCode::OK.into_response(),
    }
}

// ---------------------------------------------------------------- fixture plugin

pub const FIXTURE_TOKEN: &str = "fixture-t0ken-value";

const FIXTURE_MANIFEST: &str = r#"
id = "fixture"
name = "Fixture"
version = "0.1.0"
contract = 1
host = "in-process"
hosts = ["fixture.invalid"]

[[secrets]]
key = "token"
label = "Token"
hosts = ["fixture.invalid"]
"#;

/// Host services for the fixture plugin. Requests go to the local test server
/// through the policy's loopback exemption.
pub fn fixture_services() -> Arc<smithagents_plugin_host::HostServices> {
    use smithagents_plugin_host::{HostServices, HttpPolicy, StaticSecrets};

    let manifest = smithagents_plugin_protocol::Manifest::parse(FIXTURE_MANIFEST).unwrap();
    let secrets = StaticSecrets(
        [(
            ("fixture".to_string(), "token".to_string()),
            FIXTURE_TOKEN.to_string(),
        )]
        .into(),
    );
    let policy = HttpPolicy {
        allow_insecure_loopback: true,
        ..HttpPolicy::default()
    };
    Arc::new(HostServices::new(&manifest, Arc::new(secrets), policy).unwrap())
}

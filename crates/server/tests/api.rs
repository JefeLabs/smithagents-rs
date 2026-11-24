mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use futures::StreamExt;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use smithagents_core::agent::{Agent, ChatMessage, Repo, Role};
use smithagents_core::event::{Event, EventKind};
use smithagents_core::ids::AgentId;
use smithagents_core::palette::palette;
use smithagents_server::api::{router, AppState};
use smithagents_server::events::EventBus;
use smithagents_server::harness::fake::FakeHarness;
use smithagents_server::harness::HarnessEvent;
use smithagents_server::paths::Paths;
use smithagents_server::service::AgentService;
use smithagents_server::store::Store;
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

const TOKEN: &str = "test-token-123";
const INSTANCE_ID: &str = "0123456789abcdef";

struct Fx {
    tmp: TempDir,
    state: AppState,
}

async fn fixture(harness: FakeHarness, bus_capacity: usize) -> Fx {
    let tmp = TempDir::new().unwrap();
    let store = Store::open_in_memory().await.unwrap();
    let bus = EventBus::new(bus_capacity);
    let paths = Paths {
        home: tmp.path().join("home"),
    };
    paths.ensure_dirs().unwrap();
    let service = AgentService::new(
        store.clone(),
        bus.clone(),
        Arc::new(harness),
        paths,
        Duration::from_secs(3600),
    );
    let state = AppState {
        service,
        bus,
        store,
        token: Arc::from(TOKEN),
        instance_id: Arc::from(INSTANCE_ID),
        ws_clients: Arc::new(AtomicUsize::new(0)),
        started_at: Instant::now(),
    };
    Fx { tmp, state }
}

async fn default_fx() -> Fx {
    fixture(FakeHarness::scripted(vec![]), 1024).await
}

impl Fx {
    /// Sends a request with the valid token and returns status plus JSON body.
    async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(match body {
                Some(v) => Body::from(v.to_string()),
                None => Body::empty(),
            })
            .unwrap();
        raw(&self.state, req).await
    }

    fn repo_dir(&self) -> std::path::PathBuf {
        let dir = self.tmp.path().join("my-repo");
        if !dir.exists() {
            common::init_repo(&dir);
        }
        dir
    }

    async fn add_repo(&self) -> Repo {
        let (status, body) = self
            .call(
                Method::POST,
                "/v1/repos",
                Some(json!({ "path": self.repo_dir() })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        serde_json::from_value(body).unwrap()
    }

    async fn create_agent(&self, repo: &Repo) -> Agent {
        let (status, body) = self
            .call(
                Method::POST,
                "/v1/agents",
                Some(json!({ "repo_id": repo.id })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        serde_json::from_value(body).unwrap()
    }

    /// Serves the router on a real listener; returns its address.
    async fn serve(&self) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(self.state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    }
}

async fn raw(state: &AppState, req: Request<Body>) -> (StatusCode, Value) {
    let resp = router(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, json)
}

fn assert_error(body: &Value, code: &str) {
    assert_eq!(body["error"], code, "{body}");
    assert!(body["message"].is_string(), "{body}");
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(addr: std::net::SocketAddr, query: &str) -> Ws {
    let mut req = format!("ws://{addr}/v1/events{query}")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {TOKEN}").parse().unwrap(),
    );
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn next_event(ws: &mut Ws) -> Event {
    let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timed out waiting for frame")
        .expect("socket ended")
        .unwrap();
    match msg {
        Message::Text(t) => serde_json::from_str(t.as_str()).unwrap(),
        other => panic!("unexpected frame {other:?}"),
    }
}

async fn wait_for_clients(state: &AppState, want: usize) {
    for _ in 0..200 {
        if state.ws_clients.load(Ordering::SeqCst) == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "ws_clients never reached {want}, is {}",
        state.ws_clients.load(Ordering::SeqCst)
    );
}

fn closed_event(bus: &EventBus) -> Event {
    bus.publish(EventKind::AgentClosed(AgentId::new()))
}

#[tokio::test]
async fn missing_token_is_401() {
    let fx = default_fx().await;
    let req = Request::builder()
        .uri("/v1/health")
        .body(Body::empty())
        .unwrap();
    let (status, body) = raw(&fx.state, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_error(&body, "unauthorized");
}

#[tokio::test]
async fn wrong_token_is_401() {
    let fx = default_fx().await;
    for value in ["Bearer nope", "Bearer ", "Basic test-token-123", TOKEN] {
        let req = Request::builder()
            .uri("/v1/health")
            .header(header::AUTHORIZATION, value)
            .body(Body::empty())
            .unwrap();
        let (status, body) = raw(&fx.state, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
        assert_error(&body, "unauthorized");
        assert!(!body.to_string().contains(TOKEN));
    }
}

#[tokio::test]
async fn query_token_is_not_accepted_off_events() {
    let fx = default_fx().await;
    let req = Request::builder()
        .uri(format!("/v1/health?token={TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let (status, _) = raw(&fx.state, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn health_returns_version_and_counts() {
    let fx = default_fx().await;
    let (status, body) = fx.call(Method::GET, "/v1/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["version"], smithagents_core::VERSION);
    assert_eq!(body["pid"], std::process::id());
    assert_eq!(body["running_agents"], 0);
    assert_eq!(body["ws_clients"], 0);
    assert_eq!(body["instance_id"], INSTANCE_ID);
    assert_eq!(body["latest_seq"], 0);

    closed_event(&fx.state.bus);
    closed_event(&fx.state.bus);
    let (_, body) = fx.call(Method::GET, "/v1/health", None).await;
    assert_eq!(body["latest_seq"], 2);
}

#[tokio::test]
async fn add_repo_rejects_non_git_dir() {
    let fx = default_fx().await;
    let dir = fx.tmp.path().join("plain");
    std::fs::create_dir(&dir).unwrap();
    let (status, body) = fx
        .call(Method::POST, "/v1/repos", Some(json!({ "path": dir })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_error(&body, "not_a_git_repo");
}

#[tokio::test]
async fn add_repo_rejects_bare_repo() {
    let fx = default_fx().await;
    let dir = fx.tmp.path().join("bare.git");
    std::fs::create_dir(&dir).unwrap();
    common::git(&dir, &["init", "--bare", "-b", "main"]);
    let (status, body) = fx
        .call(Method::POST, "/v1/repos", Some(json!({ "path": dir })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_error(&body, "bare_repo");
}

#[tokio::test]
async fn add_repo_twice_is_409_repo_exists_and_lists() {
    let fx = default_fx().await;
    let repo = fx.add_repo().await;
    let (status, body) = fx
        .call(
            Method::POST,
            "/v1/repos",
            Some(json!({ "path": fx.repo_dir() })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_error(&body, "repo_exists");
    let (status, body) = fx.call(Method::GET, "/v1/repos", None).await;
    assert_eq!(status, StatusCode::OK);
    let repos: Vec<Repo> = serde_json::from_value(body).unwrap();
    assert_eq!(repos, vec![repo]);
}

#[tokio::test]
async fn malformed_json_and_invalid_id_are_400_bad_request() {
    let fx = default_fx().await;
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/repos")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::from("{not json"))
        .unwrap();
    let (status, body) = raw(&fx.state, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_error(&body, "bad_request");

    let (status, body) = fx
        .call(Method::POST, "/v1/agents/not-a-uuid/close", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_error(&body, "bad_request");
}

#[tokio::test]
async fn oversized_body_is_rejected() {
    let fx = default_fx().await;
    let big = "x".repeat(2 * 1024 * 1024);
    let (status, _) = fx
        .call(Method::POST, "/v1/repos", Some(json!({ "path": big })))
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn create_agent_unknown_repo_is_404() {
    let fx = default_fx().await;
    let (status, body) = fx
        .call(
            Method::POST,
            "/v1/agents",
            Some(json!({ "repo_id": smithagents_core::ids::RepoId::new() })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_error(&body, "repo_not_found");
}

#[tokio::test]
async fn create_agent_returns_name_and_color() {
    let fx = default_fx().await;
    let repo = fx.add_repo().await;
    let agent = fx.create_agent(&repo).await;
    assert_eq!(agent.name, "Mercury");
    assert_eq!(agent.color, palette()[0]);
    let (status, body) = fx.call(Method::GET, "/v1/agents", None).await;
    assert_eq!(status, StatusCode::OK);
    let agents: Vec<Agent> = serde_json::from_value(body).unwrap();
    assert_eq!(agents.len(), 1);
}

#[tokio::test]
async fn close_agent_is_204_and_hides_from_default_list() {
    let fx = default_fx().await;
    let repo = fx.add_repo().await;
    let agent = fx.create_agent(&repo).await;
    let (status, body) = fx
        .call(
            Method::POST,
            &format!("/v1/agents/{}/close", agent.id),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_null());
    let (_, body) = fx.call(Method::GET, "/v1/agents", None).await;
    assert_eq!(body.as_array().unwrap().len(), 0);
    let (_, body) = fx
        .call(Method::GET, "/v1/agents?include_closed=true", None)
        .await;
    assert_eq!(body.as_array().unwrap().len(), 1);

    let (status, body) = fx
        .call(
            Method::POST,
            &format!("/v1/agents/{}/messages", agent.id),
            Some(json!({ "text": "hi" })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_error(&body, "agent_closed");
}

#[tokio::test]
async fn unknown_agent_is_404() {
    let fx = default_fx().await;
    let id = AgentId::new();
    let (status, body) = fx
        .call(
            Method::POST,
            &format!("/v1/agents/{id}/messages"),
            Some(json!({ "text": "hi" })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_error(&body, "agent_not_found");
    let (status, body) = fx
        .call(Method::GET, &format!("/v1/agents/{id}/messages"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_error(&body, "agent_not_found");
}

#[tokio::test]
async fn send_message_second_call_is_409_turn_in_progress() {
    let harness = FakeHarness::scripted(vec![vec![HarnessEvent::TurnCompleted {
        session_id: "s".into(),
        is_error: false,
        result_text: None,
    }]])
    .with_delay(Duration::from_millis(500));
    let fx = fixture(harness, 1024).await;
    let repo = fx.add_repo().await;
    let agent = fx.create_agent(&repo).await;
    let uri = format!("/v1/agents/{}/messages", agent.id);

    let (status, body) = fx
        .call(Method::POST, &uri, Some(json!({ "text": "first" })))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let msg: ChatMessage = serde_json::from_value(body).unwrap();
    assert_eq!(msg.text, "first");
    assert_eq!(msg.role, Role::User);
    assert_eq!(msg.agent_id, agent.id);

    let (status, body) = fx
        .call(Method::POST, &uri, Some(json!({ "text": "second" })))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_error(&body, "turn_in_progress");
}

#[tokio::test]
async fn messages_after_seq_pages() {
    let fx = default_fx().await;
    let repo = fx.add_repo().await;
    let agent = fx.create_agent(&repo).await;
    for i in 1..=5 {
        fx.state
            .store
            .append_message(agent.id, Role::Assistant, &format!("m{i}"))
            .await
            .unwrap();
    }
    let uri = format!("/v1/agents/{}/messages", agent.id);
    let (status, body) = fx.call(Method::GET, &format!("{uri}?after=2"), None).await;
    assert_eq!(status, StatusCode::OK);
    let msgs: Vec<ChatMessage> = serde_json::from_value(body).unwrap();
    assert_eq!(msgs.iter().map(|m| m.seq).collect::<Vec<_>>(), [3, 4, 5]);

    let (_, body) = fx
        .call(Method::GET, &format!("{uri}?after=0&limit=2"), None)
        .await;
    let msgs: Vec<ChatMessage> = serde_json::from_value(body).unwrap();
    assert_eq!(msgs.iter().map(|m| m.seq).collect::<Vec<_>>(), [1, 2]);

    // limit=0 clamps up to 1.
    let (_, body) = fx.call(Method::GET, &format!("{uri}?limit=0"), None).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn events_ws_replays_since_then_streams_live() {
    let fx = default_fx().await;
    let e1 = closed_event(&fx.state.bus);
    let e2 = closed_event(&fx.state.bus);
    let e3 = closed_event(&fx.state.bus);
    let addr = fx.serve().await;
    let mut ws = ws_connect(addr, "?since=1").await;
    assert_eq!(next_event(&mut ws).await, e2);
    assert_eq!(next_event(&mut ws).await, e3);
    let live = closed_event(&fx.state.bus);
    assert_eq!(next_event(&mut ws).await, live);
    assert_eq!(live.seq, e1.seq + 3);
}

#[tokio::test]
async fn events_since_too_old_is_410() {
    let fx = fixture(FakeHarness::scripted(vec![]), 2).await;
    for _ in 0..5 {
        closed_event(&fx.state.bus);
    }
    let addr = fx.serve().await;
    let mut req = format!("ws://{addr}/v1/events?since=0")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {TOKEN}").parse().unwrap(),
    );
    // The oneshot service has no upgrade machinery, so use a real connection.
    let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
    let tokio_tungstenite::tungstenite::Error::Http(resp) = err else {
        panic!("expected an HTTP rejection, got {err:?}");
    };
    assert_eq!(resp.status(), StatusCode::GONE);
    let body: Value = serde_json::from_slice(resp.body().as_deref().unwrap()).unwrap();
    assert_error(&body, "events_too_old");
    assert!(body["message"].as_str().unwrap().contains('4'), "{body}");
}

#[tokio::test]
async fn events_since_beyond_latest_is_410_sequence_reset() {
    // A client resuming with a seq it saw on a previous daemon.
    let fx = default_fx().await;
    closed_event(&fx.state.bus);
    let addr = fx.serve().await;
    let mut req = format!("ws://{addr}/v1/events?since=7")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {TOKEN}").parse().unwrap(),
    );
    let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
    let tokio_tungstenite::tungstenite::Error::Http(resp) = err else {
        panic!("expected an HTTP rejection, got {err:?}");
    };
    assert_eq!(resp.status(), StatusCode::GONE);
    let body: Value = serde_json::from_slice(resp.body().as_deref().unwrap()).unwrap();
    assert_error(&body, "events_too_old");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("restarted"), "{body}");
    assert!(message.contains("without `since`"), "{body}");

    // The latest seq itself is still a valid position.
    let mut ws = ws_connect(addr, "?since=1").await;
    let live = closed_event(&fx.state.bus);
    assert_eq!(next_event(&mut ws).await, live);
}

#[tokio::test]
async fn events_without_token_is_401_and_query_token_works() {
    let fx = default_fx().await;
    let addr = fx.serve().await;

    let err = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/events"))
        .await
        .unwrap_err();
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED)
        }
        other => panic!("unexpected {other:?}"),
    }
    let err = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/events?token=wrong"))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        tokio_tungstenite::tungstenite::Error::Http(_)
    ));

    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/v1/events?token={TOKEN}"))
            .await
            .unwrap();
    wait_for_clients(&fx.state, 1).await;
    let live = closed_event(&fx.state.bus);
    let msg = ws.next().await.unwrap().unwrap();
    let got: Event = serde_json::from_str(msg.to_text().unwrap()).unwrap();
    assert_eq!(got, live);
}

#[tokio::test]
async fn ws_clients_count_tracks_connections() {
    let fx = default_fx().await;
    let addr = fx.serve().await;
    let mut ws = ws_connect(addr, "").await;
    wait_for_clients(&fx.state, 1).await;
    let (_, body) = fx.call(Method::GET, "/v1/health", None).await;
    assert_eq!(body["ws_clients"], 1);
    ws.close(None).await.unwrap();
    while ws.next().await.is_some() {}
    wait_for_clients(&fx.state, 0).await;
}

#[tokio::test]
async fn ws_clients_decrements_when_client_drops_without_close() {
    let fx = default_fx().await;
    let addr = fx.serve().await;
    let ws = ws_connect(addr, "").await;
    wait_for_clients(&fx.state, 1).await;
    drop(ws);
    wait_for_clients(&fx.state, 0).await;
}

#[tokio::test]
async fn lagging_client_is_closed_with_reason_lagged() {
    let fx = fixture(FakeHarness::scripted(vec![]), 2).await;
    let addr = fx.serve().await;
    let mut ws = ws_connect(addr, "").await;
    wait_for_clients(&fx.state, 1).await;
    // Publish far more than capacity in one burst, without reading.
    for _ in 0..200 {
        closed_event(&fx.state.bus);
    }
    let mut reason = None;
    while let Some(Ok(msg)) = ws.next().await {
        if let Message::Close(Some(frame)) = msg {
            reason = Some(frame.reason.to_string());
            break;
        }
    }
    assert_eq!(reason.as_deref(), Some("lagged"));
    wait_for_clients(&fx.state, 0).await;
}

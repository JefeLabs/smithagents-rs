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
use smithagents_server::intents::IntentService;
use smithagents_server::paths::Paths;
use smithagents_server::queue::{Scheduler, SchedulerHandle};
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
    harness: FakeHarness,
    /// Keeps the queue scheduler running for the fixture's lifetime.
    _sched: SchedulerHandle,
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
        Arc::new(harness.clone()),
        paths.clone(),
        Duration::from_secs(3600),
    );
    let intents = IntentService::new(
        store.clone(),
        bus.clone(),
        Arc::new(harness.clone()),
        service.clone(),
        paths,
    );
    let sched = Scheduler::start(intents.clone(), service.clone(), store.clone(), bus.clone());
    let state = AppState {
        service,
        intents,
        bus,
        store,
        token: Arc::from(TOKEN),
        instance_id: Arc::from(INSTANCE_ID),
        ws_clients: Arc::new(AtomicUsize::new(0)),
        started_at: Instant::now(),
    };
    Fx {
        tmp,
        state,
        harness,
        _sched: sched,
    }
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
    bus.publish(EventKind::AgentClosed {
        agent_id: AgentId::new(),
    })
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
async fn add_repo_rejects_missing_path() {
    let fx = default_fx().await;
    let dir = fx.tmp.path().join("does-not-exist");
    let (status, body) = fx
        .call(Method::POST, "/v1/repos", Some(json!({ "path": dir })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_error(&body, "path_not_found");
    assert!(
        body["message"].as_str().unwrap().contains("does-not-exist"),
        "{body}"
    );
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

// --------------------------------------------------------------- intents

fn pass1(
    title: &str,
    questions: usize,
) -> Result<Value, smithagents_server::harness::HarnessError> {
    let questions: Vec<Value> = (1..=questions)
        .map(|n| {
            json!({
                "text": format!("Question {n}?"),
                "options": [{"label": "Yes"}, {"label": "No"}]
            })
        })
        .collect();
    Ok(json!({
        "title": title,
        "kind": "enhancement",
        "current_behavior": "Builds are slow.",
        "questions": questions,
    }))
}

fn pass2(deps: &[Value]) -> Result<Value, smithagents_server::harness::HarnessError> {
    let deps: Vec<Value> = deps
        .iter()
        .map(|d| json!({"intent_id": d, "reason": "it must land first"}))
        .collect();
    Ok(json!({
        "preconditions": ["CI is green"],
        "acceptance_criteria": ["Builds finish quickly"],
        "stories": ["As a developer I get feedback quickly"],
        "depends_on": deps,
    }))
}

fn turn_done() -> Vec<HarnessEvent> {
    vec![HarnessEvent::TurnCompleted {
        session_id: "s".into(),
        is_error: false,
        result_text: None,
    }]
}

impl Fx {
    async fn get_intent(&self, id: &str) -> Value {
        let (status, body) = self
            .call(Method::GET, &format!("/v1/intents/{id}"), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn wait_intent(&self, id: &str, pred: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..400 {
            let body = self.get_intent(id).await;
            if pred(&body) {
                return body;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "intent never reached the wanted state: {}",
            self.get_intent(id).await
        );
    }

    async fn text_intent(&self, repo: &Repo, text: &str) -> Value {
        let (status, body) = self
            .call(
                Method::POST,
                "/v1/intents",
                Some(json!({ "repo_id": repo.id, "seed": { "text": text } })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body
    }

    /// A text intent taken through polish and review to `complete`.
    async fn complete_intent(&self, repo: &Repo, title: &str, deps: &[Value]) -> Value {
        self.harness
            .script_structured(vec![pass1(title, 1), pass2(deps)]);
        let created = self.text_intent(repo, title).await;
        let id = created["id"].as_str().unwrap().to_owned();
        let drafted = self
            .wait_intent(&id, |i| i["phase"]["kind"] == "questions")
            .await;
        let q = &drafted["content"]["questions"][0];
        let (status, body) = self
            .call(
                Method::POST,
                &format!(
                    "/v1/intents/{id}/questions/{}/answer",
                    q["id"].as_str().unwrap()
                ),
                Some(json!({ "option_id": q["options"][0]["id"] })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let reviewing = self
            .wait_intent(&id, |i| i["phase"]["kind"] == "review")
            .await;
        let mut last = reviewing.clone();
        for section in ["preconditions", "criteria", "stories", "depends_on"] {
            for item in reviewing["content"][section].as_array().unwrap() {
                let (status, body) = self
                    .call(
                        Method::POST,
                        &format!(
                            "/v1/intents/{id}/items/{}/accept",
                            item["id"].as_str().unwrap()
                        ),
                        None,
                    )
                    .await;
                assert_eq!(status, StatusCode::OK, "{body}");
                last = body;
            }
        }
        assert_eq!(last["state"], "complete", "{last}");
        last
    }
}

#[tokio::test]
async fn intent_routes_require_a_token() {
    let fx = default_fx().await;
    let id = smithagents_core::ids::IntentId::new();
    for (method, uri) in [
        (Method::POST, "/v1/intents".to_owned()),
        (Method::GET, "/v1/intents".to_owned()),
        (Method::GET, format!("/v1/intents/{id}")),
        (Method::POST, format!("/v1/intents/{id}/send")),
        (Method::GET, "/v1/settings/agent-defaults".to_owned()),
        (Method::PUT, "/v1/settings/agent-defaults".to_owned()),
    ] {
        let req = Request::builder()
            .method(method)
            .uri(&uri)
            .body(Body::empty())
            .unwrap();
        let (status, body) = raw(&fx.state, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_error(&body, "unauthorized");
    }
}

#[tokio::test]
async fn create_get_list_and_versions() {
    let fx = default_fx().await;
    let repo = fx.add_repo().await;
    fx.harness
        .script_structured(vec![pass1("Faster builds", 1)]);
    let created = fx.text_intent(&repo, "make builds faster").await;
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["state"], "draft");
    assert_eq!(created["repo_id"], json!(repo.id));

    let got = fx.get_intent(id).await;
    assert_eq!(got["id"], created["id"]);

    let (status, list) = fx.call(Method::GET, "/v1/intents", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (_, list) = fx.call(Method::GET, "/v1/intents?state=draft", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (_, list) = fx.call(Method::GET, "/v1/intents?state=sent", None).await;
    assert!(list.as_array().unwrap().is_empty());
    let (_, list) = fx
        .call(
            Method::GET,
            &format!("/v1/intents?repo_id={}&state=draft", repo.id),
            None,
        )
        .await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (_, list) = fx
        .call(
            Method::GET,
            &format!(
                "/v1/intents?repo_id={}",
                smithagents_core::ids::RepoId::new()
            ),
            None,
        )
        .await;
    assert!(list.as_array().unwrap().is_empty());

    fx.wait_intent(id, |i| i["phase"]["kind"] == "questions")
        .await;
    let (status, versions) = fx
        .call(Method::GET, &format!("/v1/intents/{id}/versions"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{versions}");
    assert!(versions.is_array());
}

#[tokio::test]
async fn patch_updates_content() {
    let fx = default_fx().await;
    let repo = fx.add_repo().await;
    fx.harness
        .script_structured(vec![pass1("Faster builds", 1)]);
    let created = fx.text_intent(&repo, "make builds faster").await;
    let id = created["id"].as_str().unwrap();
    fx.wait_intent(id, |i| i["phase"]["kind"] == "questions")
        .await;
    let (status, body) = fx
        .call(
            Method::PATCH,
            &format!("/v1/intents/{id}"),
            Some(json!({ "title": "Renamed", "kind": "bug" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["content"]["title"], "Renamed");
    assert_eq!(body["content"]["kind"], "bug");
}

#[tokio::test]
async fn unknown_ids_are_404_and_bad_uuids_400() {
    let fx = default_fx().await;
    let id = smithagents_core::ids::IntentId::new();
    let other = smithagents_core::ids::ItemId::new();
    for (method, uri, body) in [
        (Method::GET, format!("/v1/intents/{id}"), None),
        (Method::GET, format!("/v1/intents/{id}/versions"), None),
        (
            Method::PATCH,
            format!("/v1/intents/{id}"),
            Some(json!({"title": "x"})),
        ),
        (
            Method::POST,
            format!("/v1/intents/{id}/questions/{other}/answer"),
            Some(json!({"skip": true})),
        ),
        (Method::POST, format!("/v1/intents/{id}/polish/retry"), None),
        (
            Method::POST,
            format!("/v1/intents/{id}/items/{other}/accept"),
            None,
        ),
        (
            Method::POST,
            format!("/v1/intents/{id}/items/{other}/drop"),
            None,
        ),
        (
            Method::POST,
            format!("/v1/intents/{id}/items/{other}/edit"),
            Some(json!({"text": "t"})),
        ),
        (Method::POST, format!("/v1/intents/{id}/send"), None),
        (Method::POST, format!("/v1/intents/{id}/resolve"), None),
    ] {
        let (status, resp) = fx.call(method, &uri, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {resp}");
        assert_error(&resp, "intent_not_found");
    }
    for (method, uri, body) in [
        (Method::GET, "/v1/intents/nope".to_owned(), None),
        (Method::GET, "/v1/intents/nope/versions".to_owned(), None),
        (
            Method::PATCH,
            "/v1/intents/nope".to_owned(),
            Some(json!({})),
        ),
        (Method::POST, "/v1/intents/nope/send".to_owned(), None),
        (Method::POST, "/v1/intents/nope/resolve".to_owned(), None),
        (
            Method::POST,
            format!("/v1/intents/{id}/items/nope/accept"),
            None,
        ),
        (
            Method::POST,
            format!("/v1/intents/{id}/questions/nope/answer"),
            Some(json!({"skip": true})),
        ),
    ] {
        let (status, resp) = fx.call(method, &uri, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {resp}");
        assert!(resp["error"].is_string(), "{resp}");
    }
}

#[tokio::test]
async fn create_errors() {
    let fx = default_fx().await;
    let (status, body) = fx
        .call(
            Method::POST,
            "/v1/intents",
            Some(
                json!({ "repo_id": smithagents_core::ids::RepoId::new(), "seed": { "text": "x" } }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_error(&body, "repo_not_found");

    let (status, body) = fx
        .call(
            Method::POST,
            "/v1/intents",
            Some(json!({ "seed": { "chat": { "agent_id": AgentId::new() } } })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_error(&body, "agent_not_found");

    let (status, _) = fx
        .call(Method::POST, "/v1/intents", Some(json!({ "nonsense": 1 })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn bad_state_filter_is_invalid() {
    let fx = default_fx().await;
    let (status, body) = fx.call(Method::GET, "/v1/intents?state=bogus", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_error(&body, "invalid");
    let (status, body) = fx
        .call(Method::GET, "/v1/intents?repo_id=bogus", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_error(&body, "invalid");
}

#[tokio::test]
async fn conflict_codes() {
    let fx = default_fx().await;
    let repo = fx.add_repo().await;
    fx.harness
        .script_structured(vec![pass1("One", 1), pass2(&[])]);
    let created = fx.text_intent(&repo, "one").await;
    let id = created["id"].as_str().unwrap();

    // Still polishing, or in the questions phase: cannot be sent.
    let (status, body) = fx
        .call(Method::POST, &format!("/v1/intents/{id}/send"), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        ["invalid_state", "polishing"].contains(&body["error"].as_str().unwrap()),
        "{body}"
    );

    let drafted = fx
        .wait_intent(id, |i| i["phase"]["kind"] == "questions")
        .await;
    let (status, body) = fx
        .call(Method::POST, &format!("/v1/intents/{id}/send"), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_error(&body, "invalid_state");
    let (status, body) = fx
        .call(Method::POST, &format!("/v1/intents/{id}/resolve"), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_error(&body, "invalid_state");

    // An unknown option is a plain invalid answer.
    let q = drafted["content"]["questions"][0]["id"].as_str().unwrap();
    let (status, body) = fx
        .call(
            Method::POST,
            &format!("/v1/intents/{id}/questions/{q}/answer"),
            Some(json!({ "option_id": "nope" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_error(&body, "invalid");

    // Answering twice is 409 already_answered; the first answer closes the
    // last question, so the second may also find the draft polishing or in
    // review. Skip the first so the question stays recorded as answered.
    let (status, _) = fx
        .call(
            Method::POST,
            &format!("/v1/intents/{id}/questions/{q}/answer"),
            Some(json!({ "skip": true })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = fx
        .call(
            Method::POST,
            &format!("/v1/intents/{id}/questions/{q}/answer"),
            Some(json!({ "skip": true })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        ["already_answered", "polishing", "invalid_state"]
            .contains(&body["error"].as_str().unwrap()),
        "{body}"
    );
}

#[tokio::test]
async fn already_answered_is_409() {
    let fx = default_fx().await;
    let repo = fx.add_repo().await;
    fx.harness.script_structured(vec![pass1("Two", 2)]);
    let created = fx.text_intent(&repo, "two questions").await;
    let id = created["id"].as_str().unwrap();
    let drafted = fx
        .wait_intent(id, |i| i["phase"]["kind"] == "questions")
        .await;
    let q = drafted["content"]["questions"][0]["id"].as_str().unwrap();
    let uri = format!("/v1/intents/{id}/questions/{q}/answer");
    let (status, _) = fx
        .call(Method::POST, &uri, Some(json!({ "skip": true })))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = fx
        .call(Method::POST, &uri, Some(json!({ "skip": true })))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_error(&body, "already_answered");
}

#[tokio::test]
async fn polish_retry_after_failure() {
    let fx = default_fx().await;
    let repo = fx.add_repo().await;
    fx.harness.script_structured(vec![
        Err(smithagents_server::harness::HarnessError::Failed(
            "boom".into(),
        )),
        pass1("Recovered", 1),
    ]);
    let created = fx.text_intent(&repo, "retry me").await;
    let id = created["id"].as_str().unwrap();
    fx.wait_intent(id, |i| i["phase"]["kind"] == "failed").await;
    let (status, body) = fx
        .call(
            Method::POST,
            &format!("/v1/intents/{id}/polish/retry"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    fx.wait_intent(id, |i| i["phase"]["kind"] == "questions")
        .await;
}

#[tokio::test]
async fn edit_and_drop_items() {
    let fx = default_fx().await;
    let repo = fx.add_repo().await;
    fx.harness
        .script_structured(vec![pass1("Review me", 1), pass2(&[])]);
    let created = fx.text_intent(&repo, "review").await;
    let id = created["id"].as_str().unwrap();
    let drafted = fx
        .wait_intent(id, |i| i["phase"]["kind"] == "questions")
        .await;
    let q = &drafted["content"]["questions"][0];
    fx.call(
        Method::POST,
        &format!(
            "/v1/intents/{id}/questions/{}/answer",
            q["id"].as_str().unwrap()
        ),
        Some(json!({ "skip": true })),
    )
    .await;
    let reviewing = fx.wait_intent(id, |i| i["phase"]["kind"] == "review").await;
    let item = reviewing["content"]["preconditions"][0]["id"]
        .as_str()
        .unwrap();
    let (status, body) = fx
        .call(
            Method::POST,
            &format!("/v1/intents/{id}/items/{item}/edit"),
            Some(json!({ "text": "CI is very green" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["content"]["preconditions"][0]["text"],
        "CI is very green"
    );
    assert_eq!(body["content"]["preconditions"][0]["status"], "accepted");
    let (status, body) = fx
        .call(
            Method::POST,
            &format!("/v1/intents/{id}/items/{item}/drop"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["content"]["preconditions"][0]["status"], "dropped");
}

#[tokio::test]
async fn agent_defaults_round_trip() {
    let fx = default_fx().await;
    let (status, body) = fx
        .call(Method::GET, "/v1/settings/agent-defaults", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "queue_limit": null }));
    let (status, body) = fx
        .call(
            Method::PUT,
            "/v1/settings/agent-defaults",
            Some(json!({ "queue_limit": 2 })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "queue_limit": 2 }));
    let (_, body) = fx
        .call(Method::GET, "/v1/settings/agent-defaults", None)
        .await;
    assert_eq!(body, json!({ "queue_limit": 2 }));
    let (status, body) = fx
        .call(
            Method::PUT,
            "/v1/settings/agent-defaults",
            Some(json!({ "queue_limit": null })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "queue_limit": null }));
    let (status, body) = fx
        .call(
            Method::PUT,
            "/v1/settings/agent-defaults",
            Some(json!({ "queue_limit": 100000 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_error(&body, "invalid");
}

#[tokio::test]
async fn oversized_intent_body_is_rejected() {
    let fx = default_fx().await;
    let huge = "x".repeat(2 * 1024 * 1024);
    let (status, _) = fx
        .call(
            Method::POST,
            "/v1/intents",
            Some(json!({ "seed": { "text": huge } })),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn intent_events_arrive_over_the_websocket() {
    let fx = default_fx().await;
    let repo = fx.add_repo().await;
    let addr = fx.serve().await;
    let mut ws = ws_connect(addr, "").await;
    wait_for_clients(&fx.state, 1).await;

    fx.harness.script_structured(vec![pass1("Events", 1)]);
    let created = fx.text_intent(&repo, "watch me").await;
    let id = created["id"].as_str().unwrap().to_owned();

    let mut kinds = Vec::new();
    let mut saw_created = false;
    let mut saw_questions = false;
    while !saw_questions {
        let event = next_event(&mut ws).await;
        match &event.kind {
            EventKind::IntentCreated(i) if i.id.to_string() == id => {
                saw_created = true;
                kinds.push("intent_created");
            }
            EventKind::IntentUpdated(i) if i.id.to_string() == id => {
                kinds.push("intent_updated");
                saw_questions = matches!(i.phase, smithagents_core::intent::Phase::Questions);
            }
            _ => {}
        }
    }
    assert!(saw_created, "{kinds:?}");
    assert_eq!(kinds[0], "intent_created");
    assert!(kinds.contains(&"intent_updated"));
}

#[tokio::test]
async fn full_loop_over_http_ends_with_the_dependent_starting() {
    // Turns: the chat agent's reply, then the two intent agents.
    let harness = FakeHarness::scripted(vec![turn_done(), turn_done(), turn_done()]);
    let fx = fixture(harness, 4096).await;
    let repo = fx.add_repo().await;
    let chat = fx.create_agent(&repo).await;
    let (status, body) = fx
        .call(
            Method::POST,
            &format!("/v1/agents/{}/messages", chat.id),
            Some(json!({ "text": "we should make the build faster" })),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    // Intent A from the chat.
    fx.harness
        .script_structured(vec![pass1("Speed up builds", 1), pass2(&[])]);
    let (status, a) = fx
        .call(
            Method::POST,
            "/v1/intents",
            Some(json!({ "seed": { "chat": { "agent_id": chat.id } } })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{a}");
    let a_id = a["id"].as_str().unwrap().to_owned();
    let drafted = fx
        .wait_intent(&a_id, |i| i["phase"]["kind"] == "questions")
        .await;
    let q = &drafted["content"]["questions"][0];
    let (status, body) = fx
        .call(
            Method::POST,
            &format!(
                "/v1/intents/{a_id}/questions/{}/answer",
                q["id"].as_str().unwrap()
            ),
            Some(json!({ "option_id": q["options"][0]["id"] })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let reviewing = fx
        .wait_intent(&a_id, |i| i["phase"]["kind"] == "review")
        .await;
    let mut last = reviewing.clone();
    for section in ["preconditions", "criteria", "stories"] {
        for item in reviewing["content"][section].as_array().unwrap() {
            let (_, body) = fx
                .call(
                    Method::POST,
                    &format!(
                        "/v1/intents/{a_id}/items/{}/accept",
                        item["id"].as_str().unwrap()
                    ),
                    None,
                )
                .await;
            last = body;
        }
    }
    assert_eq!(last["state"], "complete", "{last}");

    // Intent B depends on A.
    let b = fx
        .complete_intent(&repo, "Cache the builds", &[json!(a_id)])
        .await;
    let b_id = b["id"].as_str().unwrap().to_owned();

    // B waits for A.
    let (status, sent_b) = fx
        .call(Method::POST, &format!("/v1/intents/{b_id}/send"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{sent_b}");
    assert_eq!(sent_b["state"], "queued");

    // A starts at once.
    let (status, sent_a) = fx
        .call(Method::POST, &format!("/v1/intents/{a_id}/send"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{sent_a}");
    let a_sent = fx.wait_intent(&a_id, |i| i["state"] == "sent").await;
    assert!(a_sent["agent_id"].is_string());
    assert_eq!(fx.get_intent(&b_id).await["state"], "queued");

    // Resolving A lets the scheduler start B.
    let (status, resolved) = fx
        .call(Method::POST, &format!("/v1/intents/{a_id}/resolve"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{resolved}");
    assert_eq!(resolved["state"], "resolved");
    let b_sent = fx.wait_intent(&b_id, |i| i["state"] == "sent").await;
    assert!(b_sent["agent_id"].is_string());
}

#[tokio::test]
async fn cors_preflight_needs_no_token() {
    let fx = default_fx().await;
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/agents")
        .header(header::ORIGIN, "http://localhost:5173")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            "authorization,content-type",
        )
        .header("access-control-request-private-network", "true")
        .body(Body::empty())
        .unwrap();
    let resp = router(fx.state.clone()).oneshot(req).await.unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let h = resp.headers();
    assert!(!h.contains_key("access-control-allow-private-network"));
    assert_eq!(h[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    let allow_headers = h[header::ACCESS_CONTROL_ALLOW_HEADERS]
        .to_str()
        .unwrap()
        .to_ascii_lowercase();
    assert!(allow_headers.contains("authorization"), "{allow_headers}");
    assert!(allow_headers.contains("content-type"), "{allow_headers}");
    let methods = h[header::ACCESS_CONTROL_ALLOW_METHODS].to_str().unwrap();
    for m in ["GET", "POST", "PUT", "PATCH", "OPTIONS"] {
        assert!(methods.contains(m), "{methods}");
    }
    assert!(h.contains_key(header::ACCESS_CONTROL_MAX_AGE));
}

#[tokio::test]
async fn cors_headers_on_401_and_on_success() {
    let fx = default_fx().await;
    let req = Request::builder()
        .uri("/v1/health")
        .header(header::ORIGIN, "http://localhost:5173")
        .body(Body::empty())
        .unwrap();
    let resp = router(fx.state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(resp.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");

    let req = Request::builder()
        .uri("/v1/health")
        .header(header::ORIGIN, "http://localhost:5173")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let resp = router(fx.state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
}

#[tokio::test]
async fn cors_does_not_weaken_ws_token_check() {
    let fx = default_fx().await;
    let addr = fx.serve().await;
    let mut req = format!("ws://{addr}/v1/events")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert(header::ORIGIN, "http://localhost:5173".parse().unwrap());
    req.headers_mut()
        .insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
    match tokio_tungstenite::connect_async(req).await.unwrap_err() {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED)
        }
        other => panic!("unexpected {other:?}"),
    }
}

// ------------------------------------------------------------------ chat

fn completed() -> HarnessEvent {
    HarnessEvent::TurnCompleted {
        session_id: "s".into(),
        is_error: false,
        result_text: None,
    }
}

fn text(t: &str) -> HarnessEvent {
    HarnessEvent::AssistantText { text: t.into() }
}

async fn post_chat(fx: &Fx, id: AgentId, body: Value) -> axum::response::Response {
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/v1/agents/{id}/chat"))
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    router(fx.state.clone()).oneshot(req).await.unwrap()
}

/// Parsed SSE events: JSON of each `data:` line, `[DONE]` as a string.
fn parse_sse(raw: &str) -> Vec<Value> {
    raw.split("\n\n")
        .filter_map(|block| {
            let data: Vec<&str> = block
                .lines()
                .filter(|l| !l.starts_with(':'))
                .filter_map(|l| l.strip_prefix("data: "))
                .collect();
            if data.is_empty() {
                return None;
            }
            let data = data.join("\n");
            Some(if data == "[DONE]" {
                Value::String(data)
            } else {
                serde_json::from_str(&data).unwrap()
            })
        })
        .collect()
}

async fn chat_body(resp: axum::response::Response) -> String {
    let bytes = tokio::time::timeout(Duration::from_secs(10), resp.into_body().collect())
        .await
        .expect("stream did not end")
        .unwrap()
        .to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn types(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            Value::String(s) => s.clone(),
            v => v["type"].as_str().unwrap().to_string(),
        })
        .collect()
}

async fn wait_agent_state(fx: &Fx, id: AgentId, want: &str) {
    for _ in 0..200 {
        let (_, body) = fx.call(Method::GET, "/v1/agents", None).await;
        if body
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == json!(id) && a["state"] == want)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("agent never reached {want}");
}

#[tokio::test]
async fn chat_streams_a_normal_turn() {
    let harness = FakeHarness::scripted(vec![vec![text("Hello"), text(", world"), completed()]]);
    let fx = fixture(harness, 1024).await;
    let agent = fx.create_agent(&fx.add_repo().await).await;
    let resp = post_chat(&fx, agent.id, json!({ "text": "hi" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "text/event-stream");
    assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-cache");
    assert_eq!(resp.headers()["x-vercel-ai-ui-message-stream"], "v1");
    let events = parse_sse(&chat_body(resp).await);
    assert_eq!(
        types(&events),
        [
            "start",
            "start-step",
            "text-start",
            "text-delta",
            "text-delta",
            "text-end",
            "finish-step",
            "finish",
            "[DONE]"
        ]
    );
    let joined: String = events
        .iter()
        .filter(|e| e["type"] == "text-delta")
        .map(|e| e["delta"].as_str().unwrap())
        .collect();
    assert_eq!(joined, "Hello, world");
    assert!(events[0]["messageId"].is_string());
    assert_eq!(events[2]["id"], "t1");
}

#[tokio::test]
async fn chat_while_a_turn_runs_is_409_json() {
    let harness = FakeHarness::scripted(vec![vec![text("a"), completed()]])
        .with_delay(Duration::from_millis(300));
    let fx = fixture(harness, 1024).await;
    let agent = fx.create_agent(&fx.add_repo().await).await;
    let first = post_chat(&fx, agent.id, json!({ "text": "one" })).await;
    assert_eq!(first.status(), StatusCode::OK);
    let second = post_chat(&fx, agent.id, json!({ "text": "two" })).await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert_ne!(second.headers()[header::CONTENT_TYPE], "text/event-stream");
    let body: Value = serde_json::from_str(&chat_body(second).await).unwrap();
    assert_error(&body, "turn_in_progress");
    chat_body(first).await;
}

#[tokio::test]
async fn chat_rejects_unknown_agent_empty_text_and_missing_token() {
    let fx = default_fx().await;
    let agent = fx.create_agent(&fx.add_repo().await).await;

    let resp = post_chat(&fx, AgentId::new(), json!({ "text": "hi" })).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = serde_json::from_str(&chat_body(resp).await).unwrap();
    assert_error(&body, "agent_not_found");

    for t in ["", "  \n\t "] {
        let resp = post_chat(&fx, agent.id, json!({ "text": t })).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: Value = serde_json::from_str(&chat_body(resp).await).unwrap();
        assert_error(&body, "bad_request");
    }

    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/v1/agents/{}/chat", agent.id))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "text": "hi" }).to_string()))
        .unwrap();
    let (status, body) = raw(&fx.state, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_error(&body, "unauthorized");
}

#[tokio::test]
async fn chat_failed_turn_streams_an_error_then_finishes_and_agent_is_dead() {
    let harness = FakeHarness::scripted(vec![vec![
        text("partial"),
        completed(),
        HarnessEvent::Failed {
            message: "exit status 1".into(),
        },
    ]]);
    let fx = fixture(harness, 1024).await;
    let agent = fx.create_agent(&fx.add_repo().await).await;
    let resp = post_chat(&fx, agent.id, json!({ "text": "hi" })).await;
    let events = parse_sse(&chat_body(resp).await);
    let t = types(&events);
    assert_eq!(
        t,
        [
            "start",
            "start-step",
            "text-start",
            "text-delta",
            "text-end",
            "error",
            "finish-step",
            "finish",
            "[DONE]"
        ]
    );
    assert_eq!(events[5]["errorText"], "The agent stopped unexpectedly.");
    wait_agent_state(&fx, agent.id, "dead").await;
}

#[tokio::test]
async fn chat_client_drop_does_not_cancel_the_turn() {
    let harness = FakeHarness::scripted(vec![
        vec![text("one"), text("two"), text("three"), completed()],
        vec![text("again"), completed()],
    ])
    .with_delay(Duration::from_millis(50));
    let fx = fixture(harness, 1024).await;
    let agent = fx.create_agent(&fx.add_repo().await).await;
    let resp = post_chat(&fx, agent.id, json!({ "text": "hi" })).await;
    let mut body = resp.into_body();
    // Read until the first text delta arrives, then drop the connection.
    let mut seen = String::new();
    while !seen.contains("text-delta") {
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        seen.push_str(&String::from_utf8_lossy(frame.data_ref().unwrap()));
    }
    drop(body);

    wait_agent_state(&fx, agent.id, "done").await;
    let (_, msgs) = fx
        .call(
            Method::GET,
            &format!("/v1/agents/{}/messages", agent.id),
            None,
        )
        .await;
    let msgs: Vec<ChatMessage> = serde_json::from_value(msgs).unwrap();
    let reply = msgs.iter().find(|m| m.role == Role::Assistant).unwrap();
    assert_eq!(reply.text, "onetwothree");

    let resp = post_chat(&fx, agent.id, json!({ "text": "next" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let events = parse_sse(&chat_body(resp).await);
    assert_eq!(types(&events).last().unwrap(), "[DONE]");
    assert_eq!(events[3]["delta"], "again");
}

#[tokio::test]
async fn chat_without_deltas_has_no_text_part_but_still_finishes() {
    let harness = FakeHarness::scripted(vec![vec![completed()]]);
    let fx = fixture(harness, 1024).await;
    let agent = fx.create_agent(&fx.add_repo().await).await;
    let resp = post_chat(&fx, agent.id, json!({ "text": "hi" })).await;
    let events = parse_sse(&chat_body(resp).await);
    assert_eq!(
        types(&events),
        ["start", "start-step", "finish-step", "finish", "[DONE]"]
    );
}

#[tokio::test]
async fn chat_to_a_closed_agent_is_409_agent_closed() {
    let fx = default_fx().await;
    let agent = fx.create_agent(&fx.add_repo().await).await;
    let (status, _) = fx
        .call(
            Method::POST,
            &format!("/v1/agents/{}/close", agent.id),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let resp = post_chat(&fx, agent.id, json!({ "text": "hi" })).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = serde_json::from_str(&chat_body(resp).await).unwrap();
    assert_error(&body, "agent_closed");
}

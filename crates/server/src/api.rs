//! HTTP and WebSocket API.
//!
//! Every route, including `/v1/health` and `/v1/events` before its upgrade, sits
//! behind one bearer-token middleware. Mutating service calls run in a spawned
//! task that is awaited: hyper cancels a handler future when the client
//! disconnects, which would otherwise abort the call midway and leave partial
//! state (a created worktree without an agent, a message without a turn).

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{header, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;
use smithagents_core::agent::{Agent, ChatMessage, Repo};
use smithagents_core::ids::{AgentId, RepoId};
use subtle::ConstantTimeEq;
use tokio::sync::broadcast::error::RecvError;

use crate::events::{BusError, EventBus, Subscription};
use crate::git::GitError;
use crate::service::{AgentService, ServiceError};
use crate::store::{Store, StoreError};

const MAX_BODY_BYTES: usize = 1024 * 1024;
const DEFAULT_MESSAGE_LIMIT: u32 = 200;
const MAX_MESSAGE_LIMIT: u32 = 1000;

#[derive(Clone)]
pub struct AppState {
    pub service: AgentService,
    pub bus: EventBus,
    pub store: Store,
    pub token: Arc<str>,
    /// Identifies this daemon process; see `lifecycle::generate_instance_id`.
    pub instance_id: Arc<str>,
    pub ws_clients: Arc<AtomicUsize>,
    pub started_at: Instant,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/repos", post(add_repo).get(list_repos))
        .route("/v1/agents", post(create_agent).get(list_agents))
        .route("/v1/agents/{id}/close", post(close_agent))
        .route(
            "/v1/agents/{id}/messages",
            post(send_message).get(list_messages),
        )
        .route("/v1/events", get(events))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state)
}

// ---------------------------------------------------------------- errors

/// JSON error `{ "error": <code>, "message": <text> }`.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid bearer token",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({ "error": self.code, "message": self.message });
        (self.status, Json(body)).into_response()
    }
}

impl From<ServiceError> for ApiError {
    fn from(e: ServiceError) -> Self {
        use StatusCode as S;
        let (status, code) = match &e {
            ServiceError::RepoNotFound => (S::NOT_FOUND, "repo_not_found"),
            ServiceError::AgentNotFound => (S::NOT_FOUND, "agent_not_found"),
            ServiceError::AgentClosed => (S::CONFLICT, "agent_closed"),
            ServiceError::TurnInProgress => (S::CONFLICT, "turn_in_progress"),
            ServiceError::Git(GitError::PathNotFound(_)) => (S::BAD_REQUEST, "path_not_found"),
            ServiceError::Git(GitError::NotAGitRepo) => (S::BAD_REQUEST, "not_a_git_repo"),
            ServiceError::Git(GitError::BareRepo) => (S::BAD_REQUEST, "bare_repo"),
            ServiceError::Git(GitError::NoDefaultBranch) => (S::BAD_REQUEST, "no_default_branch"),
            ServiceError::Git(GitError::BranchExists(_)) => (S::CONFLICT, "branch_exists"),
            ServiceError::Git(_) => (S::INTERNAL_SERVER_ERROR, "git_error"),
            ServiceError::Store(StoreError::DuplicateRepoPath) => (S::CONFLICT, "repo_exists"),
            ServiceError::Store(_) => (S::INTERNAL_SERVER_ERROR, "store_error"),
        };
        if status.is_server_error() {
            tracing::error!("request failed: {e}");
        }
        Self::new(status, code, e.to_string())
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        ServiceError::Store(e).into()
    }
}

/// Runs `fut` to completion in its own task so a dropped request cannot cancel it.
async fn detached<T, F>(fut: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: Future<Output = Result<T, ServiceError>> + Send + 'static,
{
    match tokio::spawn(fut).await {
        Ok(result) => result.map_err(ApiError::from),
        Err(e) => {
            tracing::error!("service task failed: {e}");
            Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            ))
        }
    }
}

// ------------------------------------------------------------------ auth

/// Query string `token` is honored only for this path: browsers cannot set
/// headers on WebSocket upgrades.
const EVENTS_PATH: &str = "/v1/events";

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// Compares in constant time over the bytes; a length mismatch is simply
/// unauthorized (token length is not secret).
fn token_matches(provided: &str, expected: &str) -> bool {
    provided.as_bytes().ct_eq(expected.as_bytes()).into()
}

async fn authenticate(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let from_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let provided = match from_header {
        Some(t) => Some(t.to_owned()),
        None if req.uri().path() == EVENTS_PATH => {
            query::<TokenQuery>(req.uri()).ok().and_then(|q| q.token)
        }
        None => None,
    };
    match provided {
        Some(t) if token_matches(&t, &state.token) => next.run(req).await,
        _ => ApiError::unauthorized().into_response(),
    }
}

// ------------------------------------------------------------- extractors

fn query<T: DeserializeOwned>(uri: &Uri) -> Result<T, ApiError> {
    axum::extract::Query::<T>::try_from_uri(uri)
        .map(|q| q.0)
        .map_err(|e| ApiError::bad_request(e.to_string()))
}

fn json_body<T: DeserializeOwned>(body: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))
}

fn agent_id(raw: &str) -> Result<AgentId, ApiError> {
    raw.parse()
        .map_err(|_| ApiError::bad_request("invalid agent id"))
}

// -------------------------------------------------------------- handlers

/// `latest_seq` and `instance_id` let a client tell whether its last seen
/// event seq still means anything: seqs restart with every daemon.
async fn health(State(s): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "version": smithagents_core::VERSION,
        "pid": std::process::id(),
        "instance_id": &*s.instance_id,
        "latest_seq": s.bus.latest_seq(),
        "running_agents": s.service.running_agent_count(),
        "ws_clients": s.ws_clients.load(Ordering::SeqCst),
    }))
}

#[derive(Deserialize)]
struct AddRepoBody {
    path: PathBuf,
}

async fn add_repo(State(s): State<AppState>, body: Bytes) -> Result<Json<Repo>, ApiError> {
    let AddRepoBody { path } = json_body(&body)?;
    let svc = s.service;
    detached(async move { svc.add_repo(&path).await })
        .await
        .map(Json)
}

async fn list_repos(State(s): State<AppState>) -> Result<Json<Vec<Repo>>, ApiError> {
    Ok(Json(s.store.list_repos().await?))
}

#[derive(Deserialize)]
struct CreateAgentBody {
    repo_id: RepoId,
}

async fn create_agent(State(s): State<AppState>, body: Bytes) -> Result<Json<Agent>, ApiError> {
    let CreateAgentBody { repo_id } = json_body(&body)?;
    let svc = s.service;
    detached(async move { svc.create_agent(repo_id).await })
        .await
        .map(Json)
}

#[derive(Deserialize, Default)]
struct ListAgentsQuery {
    #[serde(default)]
    include_closed: bool,
}

async fn list_agents(State(s): State<AppState>, uri: Uri) -> Result<Json<Vec<Agent>>, ApiError> {
    let q: ListAgentsQuery = query(&uri)?;
    Ok(Json(s.store.list_agents(q.include_closed).await?))
}

async fn close_agent(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = agent_id(&id)?;
    let svc = s.service;
    detached(async move { svc.close_agent(id).await }).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SendMessageBody {
    text: String,
}

async fn send_message(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<(StatusCode, Json<ChatMessage>), ApiError> {
    let id = agent_id(&id)?;
    let SendMessageBody { text } = json_body(&body)?;
    let svc = s.service;
    let msg = detached(async move { svc.send_message(id, text).await }).await?;
    Ok((StatusCode::ACCEPTED, Json(msg)))
}

#[derive(Deserialize)]
struct ListMessagesQuery {
    #[serde(default)]
    after: u64,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    DEFAULT_MESSAGE_LIMIT
}

async fn list_messages(
    State(s): State<AppState>,
    Path(id): Path<String>,
    uri: Uri,
) -> Result<Json<Vec<ChatMessage>>, ApiError> {
    let id = agent_id(&id)?;
    let q: ListMessagesQuery = query(&uri)?;
    s.store
        .get_agent(id)
        .await?
        .ok_or(ServiceError::AgentNotFound)?;
    let limit = q.limit.clamp(1, MAX_MESSAGE_LIMIT);
    Ok(Json(s.store.list_messages(id, q.after, limit).await?))
}

// ------------------------------------------------------------- websocket

#[derive(Deserialize)]
struct EventsQuery {
    since: Option<u64>,
}

async fn events(
    State(s): State<AppState>,
    uri: Uri,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let q: EventsQuery = query(&uri)?;
    // Subscribing before the upgrade lets TooOld be a plain HTTP 410 and leaves
    // no gap between the backlog and the live stream.
    // Both errors mean the same to a client: it cannot resume from `since`,
    // so it refetches the state it shows and subscribes again.
    let sub = s.bus.subscribe(q.since).map_err(|e| match e {
        BusError::TooOld { oldest_retained } => ApiError::new(
            StatusCode::GONE,
            "events_too_old",
            format!(
                "requested events are no longer retained; oldest retained seq is {oldest_retained}"
            ),
        ),
        BusError::FromTheFuture { latest } => ApiError::new(
            StatusCode::GONE,
            "events_too_old",
            format!(
                "requested seq is beyond the latest event ({latest}): the daemon restarted and \
                 its event sequence was reset; refetch the state and subscribe again without `since`"
            ),
        ),
    })?;
    let clients = s.ws_clients;
    Ok(ws.on_upgrade(move |socket| serve_socket(socket, sub, clients)))
}

/// Decrements the connected-client count on every exit path.
struct ClientGuard(Arc<AtomicUsize>);

impl ClientGuard {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn serve_socket(mut socket: WebSocket, sub: Subscription, clients: Arc<AtomicUsize>) {
    let _guard = ClientGuard::new(clients);
    let Subscription { backlog, mut live } = sub;
    for event in backlog {
        if send_event(&mut socket, &event).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            received = live.recv() => match received {
                Ok(event) => {
                    if send_event(&mut socket, &event).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    // The client resubscribes with its last seen seq.
                    let _ = socket
                        .send(Message::Close(Some(CloseFrame {
                            code: close_code::AGAIN,
                            reason: "lagged".into(),
                        })))
                        .await;
                    return;
                }
                Err(RecvError::Closed) => return,
            },
            incoming = socket.recv() => match incoming {
                // Pings are answered with pongs by the protocol layer.
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

async fn send_event(
    socket: &mut WebSocket,
    event: &smithagents_core::event::Event,
) -> Result<(), axum::Error> {
    // Event is plain data; serialization cannot fail.
    let text = serde_json::to_string(event).expect("event serializes");
    socket.send(Message::Text(text.into())).await
}

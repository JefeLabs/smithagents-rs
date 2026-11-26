//! HTTP and WebSocket API.
//!
//! Every route, including `/v1/health` and `/v1/events` before its upgrade, sits
//! behind one bearer-token middleware. Mutating service calls run in a spawned
//! task that is awaited: hyper cancels a handler future when the client
//! disconnects, which would otherwise abort the call midway and leave partial
//! state (a created worktree without an agent, a message without a turn).

use std::collections::VecDeque;
use std::convert::Infallible;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{header, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;
use smithagents_core::agent::{Agent, AgentState, ChatMessage, Repo};
use smithagents_core::event::{Event, EventKind};
use smithagents_core::ids::{AgentId, IntentId, ItemId, MessageId, QuestionId, RepoId};
use smithagents_core::intent::{
    AgentDefaults, AnswerInput, CreateIntentRequest, Intent, IntentFilter, IntentState,
    IntentVersion, Patch, ReviewAction,
};
use smithagents_core::plugin::{ItemsPage, PluginState, PluginStatus};
use smithagents_plugin_host::InstallError;
use subtle::ConstantTimeEq;
use tokio::sync::broadcast::{self, error::RecvError};
use tower_http::cors::{Any, CorsLayer};

use crate::events::{BusError, EventBus, Subscription};
use crate::git::GitError;
use crate::intents::{IntentError, IntentService};
use crate::plugins::{clean_message, ManagerError, PluginManager};
use crate::service::{AgentService, ServiceError};
use crate::store::{Store, StoreError};
use crate::ui_stream::{Chunk, DONE_SSE, STREAM_HEADER};

const MAX_BODY_BYTES: usize = 1024 * 1024;
const DEFAULT_MESSAGE_LIMIT: u32 = 200;
const MAX_MESSAGE_LIMIT: u32 = 1000;

#[derive(Clone)]
pub struct AppState {
    pub service: AgentService,
    pub intents: IntentService,
    pub bus: EventBus,
    pub store: Store,
    pub plugins: PluginManager,
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
        .route("/v1/agents/{id}/chat", post(chat))
        .route("/v1/intents", post(create_intent).get(list_intents))
        .route("/v1/intents/{id}", get(get_intent).patch(patch_intent))
        .route("/v1/intents/{id}/versions", get(intent_versions))
        .route(
            "/v1/intents/{id}/questions/{qid}/answer",
            post(answer_question),
        )
        .route("/v1/intents/{id}/polish/retry", post(retry_polish))
        .route("/v1/intents/{id}/items/{iid}/accept", post(accept_item))
        .route("/v1/intents/{id}/items/{iid}/drop", post(drop_item))
        .route("/v1/intents/{id}/items/{iid}/edit", post(edit_item))
        .route("/v1/intents/{id}/send", post(send_intent))
        .route("/v1/intents/{id}/resolve", post(resolve_intent))
        .route(
            "/v1/settings/agent-defaults",
            get(get_agent_defaults).put(put_agent_defaults),
        )
        .route("/v1/plugins", get(list_plugins))
        .route("/v1/plugins/{id}", get(get_plugin))
        .route("/v1/plugins/{id}/enable", post(enable_plugin))
        .route("/v1/plugins/{id}/disable", post(disable_plugin))
        .route(
            "/v1/plugins/{id}/settings",
            get(get_plugin_settings).put(put_plugin_settings),
        )
        .route(
            "/v1/plugins/{id}/resources/{kind}/sync",
            post(sync_plugin_resource),
        )
        .route("/v1/plugins/{id}/resources/{kind}/items", get(plugin_items))
        .route(
            "/v1/plugins/{id}/tools/{name}/install",
            post(install_plugin_tool),
        )
        .route("/v1/events", get(events))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        // CORS is added after (so outside) auth: preflights carry no token and
        // must be answered before `authenticate` runs, and 401s still get the
        // CORS headers so a browser can read them. Origin `*` is fine because
        // auth is a bearer token, not cookies (credentials stay disabled).
        // TODO(Plan 1 follow-up): add a Host-header allow-list against DNS
        // rebinding.
        .layer(cors_layer())
        .with_state(state)
}

fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::OPTIONS,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
        .max_age(Duration::from_secs(600))
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

impl From<IntentError> for ApiError {
    fn from(e: IntentError) -> Self {
        use StatusCode as S;
        if let IntentError::Service(inner) = e {
            return inner.into();
        }
        let status = match &e {
            IntentError::NotFound | IntentError::RepoNotFound | IntentError::AgentNotFound => {
                S::NOT_FOUND
            }
            IntentError::InvalidState { .. }
            | IntentError::Polishing
            | IntentError::AlreadyAnswered
            | IntentError::DependencyCycle
            | IntentError::DependencyCrossRepo => S::CONFLICT,
            IntentError::Invalid(_) => S::BAD_REQUEST,
            IntentError::Service(_) => unreachable!("handled above"),
            IntentError::Store(_) => S::INTERNAL_SERVER_ERROR,
        };
        if status.is_server_error() {
            tracing::error!("request failed: {e}");
        }
        Self::new(status, e.code(), e.to_string())
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        ServiceError::Store(e).into()
    }
}

/// Runs `fut` to completion in its own task so a dropped request cannot cancel it.
async fn detached<T, E, F>(fut: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    E: Into<ApiError> + Send + 'static,
    F: Future<Output = Result<T, E>> + Send + 'static,
{
    match tokio::spawn(fut).await {
        Ok(result) => result.map_err(Into::into),
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

impl From<ManagerError> for ApiError {
    /// For the routes where `Disabled` and `InvalidSettings` have no better
    /// reading than these; the sync route answers them itself.
    fn from(e: ManagerError) -> Self {
        use StatusCode as S;
        match e {
            ManagerError::UnknownPlugin => {
                Self::new(S::NOT_FOUND, "plugin_not_found", "no such plugin")
            }
            ManagerError::UnknownResource => Self::new(
                S::NOT_FOUND,
                "resource_not_found",
                "the plugin has no such resource",
            ),
            ManagerError::Disabled => Self::new(
                S::CONFLICT,
                "plugin_disabled",
                "the plugin is disabled or cannot run",
            ),
            ManagerError::Busy => Self::new(
                S::CONFLICT,
                "sync_in_progress",
                "this resource is syncing now",
            ),
            ManagerError::InvalidSettings(message) => {
                Self::new(S::BAD_REQUEST, "invalid_settings", message)
            }
            ManagerError::NotAllowed => Self::new(
                S::FORBIDDEN,
                "not_allowed",
                "only a built-in plugin's command-line tools can be installed",
            ),
            ManagerError::UnknownTool => Self::new(
                S::NOT_FOUND,
                "tool_not_found",
                "the plugin declares no such tool",
            ),
            ManagerError::InstallInProgress => Self::new(
                S::CONFLICT,
                "install_in_progress",
                "a command-line tool is being installed now; try again when it ends",
            ),
            ManagerError::Install(e) => install_error(e),
            ManagerError::Store(StoreError::BadCursor) => {
                Self::new(S::BAD_REQUEST, "bad_cursor", "malformed items cursor")
            }
            ManagerError::Store(e) => {
                tracing::error!("plugin request failed: {e}");
                Self::new(S::INTERNAL_SERVER_ERROR, "internal", "internal error")
            }
        }
    }
}

/// The answer to an install mise did not complete. Messages carry mise's
/// exit status at most, never its output.
fn install_error(e: InstallError) -> ApiError {
    use StatusCode as S;
    let (status, code, message) = match e {
        InstallError::NoMiseSpec => (
            S::BAD_REQUEST,
            "no_installer",
            "the tool has no mise package to install; see its install hints".to_owned(),
        ),
        InstallError::MiseMissing { install_url } => (
            S::CONFLICT,
            "mise_missing",
            format!("mise is not installed; install it from {install_url}, then try again"),
        ),
        InstallError::Failed { status: Some(code) } => (
            S::BAD_GATEWAY,
            "install_failed",
            format!("mise failed with exit status {code}"),
        ),
        InstallError::Failed { status: None } => (
            S::BAD_GATEWAY,
            "install_failed",
            "mise failed without an exit status".to_owned(),
        ),
        InstallError::Timeout => (
            S::BAD_GATEWAY,
            "install_failed",
            "mise did not finish within 10 minutes".to_owned(),
        ),
    };
    ApiError::new(status, code, clean_message(&message))
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

// --------------------------------------------------------------- plugins

/// The body of `PUT /v1/plugins/{id}/settings` is at most this long.
const MAX_SETTINGS_BYTES: usize = 64 * 1024;
const DEFAULT_ITEMS_LIMIT: u32 = 50;
const MAX_ITEMS_LIMIT: u32 = 200;
const MAX_QUERY_CHARS: usize = 256;

async fn list_plugins(State(s): State<AppState>) -> Json<Vec<PluginStatus>> {
    Json(s.plugins.list().await)
}

async fn get_plugin(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<PluginStatus>, ApiError> {
    s.plugins
        .get(&id)
        .await
        .map(Json)
        .ok_or_else(|| ManagerError::UnknownPlugin.into())
}

async fn enable_plugin(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<PluginStatus>, ApiError> {
    set_plugin_enabled(s.plugins, id, true).await
}

async fn disable_plugin(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<PluginStatus>, ApiError> {
    set_plugin_enabled(s.plugins, id, false).await
}

/// Detached: a client that disconnects must not leave the stored flag changed
/// while the live state, the scheduler wake and the event never follow.
async fn set_plugin_enabled(
    plugins: PluginManager,
    id: String,
    enabled: bool,
) -> Result<Json<PluginStatus>, ApiError> {
    detached(async move { plugins.set_enabled(&id, enabled).await })
        .await
        .map(Json)
}

async fn get_plugin_settings(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(s.plugins.settings(&id).await?))
}

async fn put_plugin_settings(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<PluginStatus>, ApiError> {
    if body.len() > MAX_SETTINGS_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            format!("settings are at most {MAX_SETTINGS_BYTES} bytes"),
        ));
    }
    let value: serde_json::Value = json_body(&body)?;
    if !value.is_object() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_settings",
            "settings must be a JSON object",
        ));
    }
    let plugins = s.plugins;
    detached(async move { plugins.set_settings(&id, value).await })
        .await
        .map(Json)
}

/// What the state says is wrong, for the answer to a refused sync.
fn not_ready_message(state: &PluginState) -> String {
    match state {
        PluginState::Disabled => "the plugin is disabled".to_owned(),
        PluginState::Ready => "the plugin cannot run now".to_owned(),
        PluginState::NeedsAuth { message, .. }
        | PluginState::NeedsSettings { message }
        | PluginState::NeedsSetup { message, .. }
        | PluginState::Failed { message }
        | PluginState::LoadFailed { message } => message.clone(),
    }
}

async fn sync_plugin_resource(
    State(s): State<AppState>,
    Path((id, kind)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let conflict =
        |code: &'static str, message: String| ApiError::new(StatusCode::CONFLICT, code, message);
    match s.plugins.sync_now(&id, &kind).await {
        Ok(()) => Ok(StatusCode::ACCEPTED),
        // `Disabled` also covers a plugin that failed, did not load, or was
        // stopped: say which.
        Err(ManagerError::Disabled) => {
            let status = s
                .plugins
                .get(&id)
                .await
                .ok_or(ManagerError::UnknownPlugin)?;
            if status.enabled {
                Err(conflict(
                    "plugin_not_ready",
                    not_ready_message(&status.state),
                ))
            } else {
                Err(conflict(
                    "plugin_disabled",
                    "the plugin is disabled".to_owned(),
                ))
            }
        }
        Err(ManagerError::InvalidSettings(message)) => Err(conflict("plugin_not_ready", message)),
        Err(
            e @ (ManagerError::UnknownPlugin
            | ManagerError::UnknownResource
            | ManagerError::Busy
            | ManagerError::NotAllowed
            | ManagerError::UnknownTool
            | ManagerError::InstallInProgress
            | ManagerError::Install(_)
            | ManagerError::Store(_)),
        ) => Err(e.into()),
    }
}

/// Installs a built-in plugin's tool with mise: the request is the user's
/// approval. Detached: an install that lost its client still finishes, and
/// its result still updates the plugin.
async fn install_plugin_tool(
    State(s): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<PluginStatus>, ApiError> {
    let plugins = s.plugins;
    detached(async move { plugins.install_tool(&id, &name).await })
        .await
        .map(Json)
}

#[derive(Deserialize, Default)]
struct ItemsQuery {
    limit: Option<u32>,
    after: Option<String>,
    q: Option<String>,
}

async fn plugin_items(
    State(s): State<AppState>,
    Path((id, kind)): Path<(String, String)>,
    uri: Uri,
) -> Result<Json<ItemsPage>, ApiError> {
    let q: ItemsQuery = query(&uri)?;
    if q.q
        .as_deref()
        .is_some_and(|t| t.chars().count() > MAX_QUERY_CHARS)
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid",
            format!("q is at most {MAX_QUERY_CHARS} characters"),
        ));
    }
    let limit = q
        .limit
        .unwrap_or(DEFAULT_ITEMS_LIMIT)
        .clamp(1, MAX_ITEMS_LIMIT);
    let text = q.q.as_deref().filter(|t| !t.is_empty());
    let page = s
        .plugins
        .items(&id, &kind, limit, q.after.as_deref(), text)
        .await?;
    Ok(Json(page))
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

fn path_id<T: std::str::FromStr>(raw: &str, what: &str) -> Result<T, ApiError> {
    raw.parse()
        .map_err(|_| ApiError::bad_request(format!("invalid {what} id")))
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

/// Sends a message and streams the reply as an AI SDK UI message stream.
///
/// The stream only observes the event bus; the turn runs in the service's own
/// task, so dropping the response never cancels it. Subscribing happens before
/// `send_message` so no delta is missed.
async fn chat(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let id = agent_id(&id)?;
    let SendMessageBody { text } = json_body(&body)?;
    if text.trim().is_empty() {
        return Err(ApiError::bad_request("text must not be empty"));
    }
    // `subscribe(None)` cannot fail (only resuming with `since` can).
    let sub = s
        .bus
        .subscribe(None)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    let svc = s.service.clone();
    let msg = detached(async move { svc.send_message(id, text).await }).await?;
    let svc = s.service;
    let stream = chat_stream(ChatStream::new(
        id,
        msg.id,
        sub.live,
        Box::new(move || svc.turn_in_flight(id)),
    ));
    Ok((
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
            (
                header::HeaderName::from_static(STREAM_HEADER.0),
                STREAM_HEADER.1,
            ),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

const KEEP_ALIVE: Duration = Duration::from_secs(15);
const KEEP_ALIVE_SSE: &str = ": keep-alive\n\n";

/// Observes the bus for one chat turn and yields SSE strings.
struct ChatStream {
    agent: AgentId,
    live: broadcast::Receiver<Event>,
    in_flight: Box<dyn Fn() -> bool + Send>,
    pending: VecDeque<String>,
    text_open: bool,
    /// Id of the user message that started this turn. Events of this agent
    /// published before it were published by an earlier turn (it publishes
    /// everything before releasing the agent, and our message is appended
    /// after our reservation), so they are ignored until it is seen.
    fence: MessageId,
    fenced: bool,
    finished: bool,
}

impl ChatStream {
    fn new(
        agent: AgentId,
        fence: MessageId,
        live: broadcast::Receiver<Event>,
        in_flight: Box<dyn Fn() -> bool + Send>,
    ) -> Self {
        let mut me = Self {
            agent,
            live,
            in_flight,
            pending: VecDeque::new(),
            text_open: false,
            fence,
            fenced: false,
            finished: false,
        };
        me.push(&Chunk::Start {
            message_id: uuid::Uuid::new_v4().to_string(),
        });
        me.push(&Chunk::StartStep);
        me
    }

    fn push(&mut self, chunk: &Chunk) {
        self.pending.push_back(chunk.to_sse());
    }

    fn finish(&mut self, error: Option<&str>) {
        if self.text_open {
            self.push(&Chunk::TextEnd { id: TEXT_ID.into() });
            self.text_open = false;
        }
        if let Some(error) = error {
            self.push(&Chunk::Error {
                error_text: error.into(),
            });
        }
        self.push(&Chunk::FinishStep);
        self.push(&Chunk::Finish);
        self.pending.push_back(DONE_SSE.to_string());
        self.finished = true;
    }

    fn on_event(&mut self, event: Event) {
        match event.kind {
            EventKind::MessageAppended(m) if m.agent_id == self.agent && m.id == self.fence => {
                self.fenced = true;
            }
            _ if !self.fenced => {}
            EventKind::TurnDelta { agent_id, text } if agent_id == self.agent => {
                if text.is_empty() {
                    return;
                }
                if !self.text_open {
                    self.push(&Chunk::TextStart { id: TEXT_ID.into() });
                    self.text_open = true;
                }
                self.push(&Chunk::TextDelta {
                    id: TEXT_ID.into(),
                    delta: text,
                });
            }
            EventKind::AgentUpdated(agent) if agent.id == self.agent => match agent.state {
                AgentState::Done => self.finish(None),
                AgentState::Dead => self.finish(Some(STOPPED)),
                _ => {}
            },
            _ => {}
        }
    }
}

const TEXT_ID: &str = "t1";
const LAGGED: &str = "The stream fell behind; the full reply will appear when the agent finishes.";
const STOPPED: &str = "The agent stopped unexpectedly.";

fn chat_stream(state: ChatStream) -> impl futures::Stream<Item = Result<Bytes, Infallible>> {
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + KEEP_ALIVE, KEEP_ALIVE);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    futures::stream::unfold((state, tick), |(mut st, mut tick)| async move {
        loop {
            if let Some(out) = st.pending.pop_front() {
                return Some((Ok(Bytes::from(out)), (st, tick)));
            }
            if st.finished {
                return None;
            }
            tokio::select! {
                received = st.live.recv() => match received {
                    Ok(event) => st.on_event(event),
                    Err(RecvError::Lagged(_)) => st.finish(Some(LAGGED)),
                    Err(RecvError::Closed) => st.finish(Some("The event stream closed.")),
                },
                _ = tick.tick() => {
                    // Safety net: a turn that ended without a terminal event
                    // must not leave the stream open forever. Check the turn
                    // first, then drain what is already queued, so a turn
                    // that finished is never mislabeled.
                    if (st.in_flight)() {
                        st.pending.push_back(KEEP_ALIVE_SSE.to_string());
                    } else {
                        while !st.finished {
                            match st.live.try_recv() {
                                Ok(event) => st.on_event(event),
                                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                                    st.finish(Some(LAGGED));
                                }
                                Err(_) => break,
                            }
                        }
                        if !st.finished {
                            st.finish(Some(STOPPED));
                        }
                    }
                }
            }
        }
    })
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

// --------------------------------------------------------------- intents

async fn create_intent(
    State(s): State<AppState>,
    body: Bytes,
) -> Result<(StatusCode, Json<Intent>), ApiError> {
    let req: CreateIntentRequest = json_body(&body)?;
    let svc = s.intents;
    let intent = detached(async move { svc.create(req).await }).await?;
    Ok((StatusCode::CREATED, Json(intent)))
}

#[derive(Deserialize, Default)]
struct ListIntentsQuery {
    state: Option<String>,
    repo_id: Option<String>,
}

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid", message)
}

async fn list_intents(State(s): State<AppState>, uri: Uri) -> Result<Json<Vec<Intent>>, ApiError> {
    let q: ListIntentsQuery = query(&uri)?;
    let state = q
        .state
        .map(|raw| {
            serde_json::from_value::<IntentState>(serde_json::Value::String(raw.clone()))
                .map_err(|_| invalid(format!("unknown intent state '{raw}'")))
        })
        .transpose()?;
    let repo_id = q
        .repo_id
        .map(|raw| {
            raw.parse::<RepoId>()
                .map_err(|_| invalid("invalid repo id"))
        })
        .transpose()?;
    Ok(Json(s.intents.list(IntentFilter { state, repo_id }).await?))
}

async fn get_intent(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Intent>, ApiError> {
    let id: IntentId = path_id(&id, "intent")?;
    Ok(Json(s.intents.get(id).await?))
}

async fn intent_versions(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<IntentVersion>>, ApiError> {
    let id: IntentId = path_id(&id, "intent")?;
    Ok(Json(s.intents.versions(id).await?))
}

async fn patch_intent(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<Intent>, ApiError> {
    let id: IntentId = path_id(&id, "intent")?;
    let patch: Patch = json_body(&body)?;
    let svc = s.intents;
    detached(async move { svc.update(id, patch).await })
        .await
        .map(Json)
}

async fn answer_question(
    State(s): State<AppState>,
    Path((id, qid)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<Intent>, ApiError> {
    let id: IntentId = path_id(&id, "intent")?;
    let qid: QuestionId = path_id(&qid, "question")?;
    let answer: AnswerInput = json_body(&body)?;
    let svc = s.intents;
    detached(async move { svc.answer(id, qid, answer).await })
        .await
        .map(Json)
}

async fn retry_polish(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Intent>, ApiError> {
    let id: IntentId = path_id(&id, "intent")?;
    let svc = s.intents;
    detached(async move { svc.retry_polish(id).await })
        .await
        .map(Json)
}

async fn review_item(
    s: AppState,
    id: &str,
    iid: &str,
    action: ReviewAction,
) -> Result<Json<Intent>, ApiError> {
    let id: IntentId = path_id(id, "intent")?;
    let iid: ItemId = path_id(iid, "item")?;
    let svc = s.intents;
    detached(async move { svc.review(id, iid, action).await })
        .await
        .map(Json)
}

async fn accept_item(
    State(s): State<AppState>,
    Path((id, iid)): Path<(String, String)>,
) -> Result<Json<Intent>, ApiError> {
    review_item(s, &id, &iid, ReviewAction::Accept).await
}

async fn drop_item(
    State(s): State<AppState>,
    Path((id, iid)): Path<(String, String)>,
) -> Result<Json<Intent>, ApiError> {
    review_item(s, &id, &iid, ReviewAction::Drop).await
}

#[derive(Deserialize)]
struct EditItemBody {
    text: String,
}

async fn edit_item(
    State(s): State<AppState>,
    Path((id, iid)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<Intent>, ApiError> {
    let EditItemBody { text } = json_body(&body)?;
    review_item(s, &id, &iid, ReviewAction::Edit(text)).await
}

async fn send_intent(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Intent>, ApiError> {
    let id: IntentId = path_id(&id, "intent")?;
    let svc = s.intents;
    detached(async move { svc.send(id).await }).await.map(Json)
}

async fn resolve_intent(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Intent>, ApiError> {
    let id: IntentId = path_id(&id, "intent")?;
    let svc = s.intents;
    detached(async move { svc.resolve(id).await })
        .await
        .map(Json)
}

async fn get_agent_defaults(State(s): State<AppState>) -> Result<Json<AgentDefaults>, ApiError> {
    Ok(Json(s.intents.agent_defaults().await?))
}

async fn put_agent_defaults(
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Json<AgentDefaults>, ApiError> {
    let defaults: AgentDefaults = json_body(&body)?;
    let svc = s.intents;
    detached(async move { svc.set_agent_defaults(defaults).await })
        .await
        .map(Json)
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

#[cfg(test)]
mod tests {
    use smithagents_core::intent::Phase;

    use super::*;

    /// The variant's name. Exhaustive on purpose: a new `IntentError`
    /// variant does not compile here until it is named, and then fails
    /// `every_intent_error_maps_to_a_fixed_status_and_code` until it has a case.
    fn variant(e: &IntentError) -> &'static str {
        match e {
            IntentError::NotFound => "NotFound",
            IntentError::RepoNotFound => "RepoNotFound",
            IntentError::AgentNotFound => "AgentNotFound",
            IntentError::InvalidState { .. } => "InvalidState",
            IntentError::Polishing => "Polishing",
            IntentError::AlreadyAnswered => "AlreadyAnswered",
            IntentError::Invalid(_) => "Invalid",
            IntentError::DependencyCycle => "DependencyCycle",
            IntentError::DependencyCrossRepo => "DependencyCrossRepo",
            IntentError::Service(_) => "Service",
            IntentError::Store(_) => "Store",
        }
    }

    const VARIANTS: [&str; 11] = [
        "NotFound",
        "RepoNotFound",
        "AgentNotFound",
        "InvalidState",
        "Polishing",
        "AlreadyAnswered",
        "Invalid",
        "DependencyCycle",
        "DependencyCrossRepo",
        "Service",
        "Store",
    ];

    #[test]
    fn every_intent_error_maps_to_a_fixed_status_and_code() {
        use StatusCode as S;
        let cases = [
            (IntentError::NotFound, S::NOT_FOUND, "intent_not_found"),
            (IntentError::RepoNotFound, S::NOT_FOUND, "repo_not_found"),
            (IntentError::AgentNotFound, S::NOT_FOUND, "agent_not_found"),
            (
                IntentError::InvalidState {
                    expected: "complete",
                    actual: IntentState::Draft,
                    phase: Some(Phase::Review),
                },
                S::CONFLICT,
                "invalid_state",
            ),
            (
                IntentError::InvalidState {
                    expected: "sent",
                    actual: IntentState::Queued,
                    phase: None,
                },
                S::CONFLICT,
                "invalid_state",
            ),
            (IntentError::Polishing, S::CONFLICT, "polishing"),
            (
                IntentError::AlreadyAnswered,
                S::CONFLICT,
                "already_answered",
            ),
            (
                IntentError::Invalid("bad".into()),
                S::BAD_REQUEST,
                "invalid",
            ),
            (
                IntentError::DependencyCycle,
                S::CONFLICT,
                "dependency_cycle",
            ),
            (
                IntentError::DependencyCrossRepo,
                S::CONFLICT,
                "dependency_cross_repo",
            ),
            // A wrapped agent-service error keeps that error's own mapping.
            (
                IntentError::Service(ServiceError::TurnInProgress),
                S::CONFLICT,
                "turn_in_progress",
            ),
            (
                IntentError::Service(ServiceError::AgentClosed),
                S::CONFLICT,
                "agent_closed",
            ),
            (
                IntentError::Service(ServiceError::Git(GitError::BranchExists("intent/x".into()))),
                S::CONFLICT,
                "branch_exists",
            ),
            (
                IntentError::Store(StoreError::Conflict("taken".into())),
                S::INTERNAL_SERVER_ERROR,
                "store_error",
            ),
        ];
        let covered: std::collections::BTreeSet<&str> =
            cases.iter().map(|(e, ..)| variant(e)).collect();
        assert_eq!(covered, VARIANTS.into_iter().collect());

        for (e, status, code) in cases {
            let message = e.to_string();
            let api = ApiError::from(e);
            assert_eq!((api.status, api.code), (status, code), "{message}");
            assert_eq!(api.message, message);
        }
    }

    fn updated(agent: AgentId, state: AgentState) -> Event {
        let a = Agent {
            id: agent,
            name: "Mercury".into(),
            color: smithagents_core::palette::AgentColor {
                l: 0.5,
                c: 0.25,
                h: 180.0,
            },
            repo_id: RepoId::new(),
            worktree_path: "/tmp/wt".into(),
            branch: "agent/x".into(),
            harness_session_id: None,
            state,
            created_at: chrono::Utc::now(),
            closed_at: None,
            intent_id: None,
        };
        Event {
            seq: 1,
            at: chrono::Utc::now(),
            kind: EventKind::AgentUpdated(a),
        }
    }

    fn appended(agent: AgentId, id: MessageId) -> Event {
        Event {
            seq: 1,
            at: chrono::Utc::now(),
            kind: EventKind::MessageAppended(ChatMessage {
                id,
                agent_id: agent,
                seq: 1,
                role: smithagents_core::agent::Role::User,
                text: "hi".into(),
                created_at: chrono::Utc::now(),
            }),
        }
    }

    fn delta(agent: AgentId, text: &str) -> Event {
        Event {
            seq: 1,
            at: chrono::Utc::now(),
            kind: EventKind::TurnDelta {
                agent_id: agent,
                text: text.into(),
            },
        }
    }

    async fn collect(stream: impl futures::Stream<Item = Result<Bytes, Infallible>>) -> String {
        use futures::StreamExt;
        let parts: Vec<_> = stream.collect().await;
        parts
            .into_iter()
            .map(|p| String::from_utf8(p.unwrap().to_vec()).unwrap())
            .collect()
    }

    fn stream_of(
        id: AgentId,
        fence: MessageId,
        rx: broadcast::Receiver<Event>,
        in_flight: bool,
    ) -> impl futures::Stream<Item = Result<Bytes, Infallible>> {
        chat_stream(ChatStream::new(id, fence, rx, Box::new(move || in_flight)))
    }

    #[tokio::test]
    async fn chat_stream_fences_on_its_own_message() {
        let id = AgentId::new();
        let fence = MessageId::new();
        let (tx, rx) = broadcast::channel(16);
        // The previous turn's tail, published before our message was appended.
        tx.send(delta(id, "stale")).unwrap();
        tx.send(updated(id, AgentState::Working)).unwrap();
        tx.send(updated(id, AgentState::Done)).unwrap();
        // Another message of this agent is not the fence.
        tx.send(appended(id, MessageId::new())).unwrap();
        tx.send(delta(id, "stale2")).unwrap();
        tx.send(appended(id, fence)).unwrap();
        tx.send(delta(id, "own")).unwrap();
        tx.send(updated(id, AgentState::Done)).unwrap();
        let out = collect(stream_of(id, fence, rx, true)).await;
        assert!(out.contains(r#""delta":"own""#), "{out}");
        assert!(!out.contains("stale"), "{out}");
        assert!(!out.contains("error"), "{out}");
        assert!(out.ends_with("data: [DONE]\n\n"), "{out}");
    }

    #[tokio::test]
    async fn chat_stream_dead_without_working_ends_immediately() {
        let id = AgentId::new();
        let fence = MessageId::new();
        let (tx, rx) = broadcast::channel(16);
        tx.send(appended(id, fence)).unwrap();
        tx.send(updated(id, AgentState::Dead)).unwrap();
        let out = collect(stream_of(id, fence, rx, true)).await;
        assert!(out.contains("stopped unexpectedly"), "{out}");
        assert!(out.ends_with("data: [DONE]\n\n"), "{out}");
    }

    #[tokio::test]
    async fn chat_stream_skips_other_agents_and_empty_deltas() {
        let id = AgentId::new();
        let fence = MessageId::new();
        let (tx, rx) = broadcast::channel(16);
        tx.send(appended(id, fence)).unwrap();
        tx.send(delta(AgentId::new(), "other")).unwrap();
        tx.send(delta(id, "")).unwrap();
        tx.send(updated(AgentId::new(), AgentState::Dead)).unwrap();
        tx.send(delta(id, "mine")).unwrap();
        tx.send(updated(id, AgentState::Done)).unwrap();
        let out = collect(stream_of(id, fence, rx, true)).await;
        assert!(out.contains(r#""delta":"mine""#), "{out}");
        assert!(!out.contains("other") && !out.contains("error"), "{out}");
        assert_eq!(out.matches("text-delta").count(), 1, "{out}");
    }

    #[tokio::test]
    async fn chat_stream_lag_ends_with_an_error_and_finish_sequence() {
        let id = AgentId::new();
        let fence = MessageId::new();
        let (tx, rx) = broadcast::channel(2);
        for i in 0..6 {
            tx.send(delta(id, &i.to_string())).unwrap();
        }
        let out = collect(stream_of(id, fence, rx, true)).await;
        assert!(out.contains(LAGGED), "{out}");
        assert!(
            out.contains("finish-step") && out.contains(r#""type":"finish""#),
            "{out}"
        );
        assert!(out.ends_with("data: [DONE]\n\n"), "{out}");
    }

    #[tokio::test(start_paused = true)]
    async fn chat_stream_emits_keep_alive_comments_while_idle() {
        use futures::StreamExt;
        let id = AgentId::new();
        let (tx, rx) = broadcast::channel(16);
        let mut s = Box::pin(stream_of(id, MessageId::new(), rx, true));
        let _start = s.next().await;
        let _step = s.next().await;
        let next = s.next().await.unwrap().unwrap();
        assert_eq!(&next[..], b": keep-alive\n\n");
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn chat_stream_ends_when_the_turn_vanished_without_a_terminal_event() {
        let id = AgentId::new();
        let (_tx, rx) = broadcast::channel(16);
        let out = collect(stream_of(id, MessageId::new(), rx, false)).await;
        assert!(out.contains("stopped unexpectedly"), "{out}");
        assert!(out.ends_with("data: [DONE]\n\n"), "{out}");
    }

    #[tokio::test(start_paused = true)]
    async fn chat_stream_streams_queued_events_even_when_the_turn_is_no_longer_in_flight() {
        let id = AgentId::new();
        let fence = MessageId::new();
        let (tx, rx) = broadcast::channel(16);
        let mut st = ChatStream::new(id, fence, rx, Box::new(|| false));
        // Queued but not yet consumed when the tick finds the turn finished.
        tx.send(appended(id, fence)).unwrap();
        tx.send(delta(id, "ok")).unwrap();
        tx.send(updated(id, AgentState::Done)).unwrap();
        st.pending.clear();
        let out = collect(chat_stream(st)).await;
        assert!(out.contains(r#""delta":"ok""#), "{out}");
        assert!(!out.contains("stopped unexpectedly"), "{out}");
    }
}

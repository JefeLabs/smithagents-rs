//! HTTP and WebSocket client for the daemon API.
//!
//! The daemon listens on 127.0.0.1 only. Every request carries the lockfile's
//! token as `Authorization: Bearer <token>`, including the events WebSocket
//! upgrade (the token never goes in a URL).

use std::fmt;
use std::future::ready;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::{Stream, StreamExt};
use reqwest::multipart::{Form, Part};
use reqwest::RequestBuilder;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use smithagents_core::agent::{Agent, ChatMessage, Repo};
use smithagents_core::daemon::{DaemonInfo, DaemonToken};
use smithagents_core::event::{Event, EventKind};
use smithagents_core::feedback::{
    FeedbackOutcome, FeedbackRequest, FeedbackSettings, FeedbackTargets, SubmissionId,
};
use smithagents_core::ids::{AgentId, IntentId, ItemId, QuestionId, RepoId};
use smithagents_core::intent::{
    AgentDefaults, AnswerInput, CreateIntentRequest, Intent, IntentFilter, IntentVersion, Patch,
};
use smithagents_core::plugin::{ItemsPage, PluginStatus};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{self, Message};

/// Bounds connecting, so a port nobody answers on cannot stall a call.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Bounds the events WebSocket handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Messages fetched per request: the daemon's maximum page size.
const MESSAGE_PAGE: usize = 1000;

/// `GET /v1/health`.
///
/// Fields added after the first version are defaulted when absent, so the
/// answer of a daemon of another version still decodes and is reported as a
/// version mismatch.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Health {
    pub version: String,
    /// The wire protocol the daemon speaks (`smithagents_core::PROTOCOL` of
    /// its build); 0 from a daemon built before it reported one.
    #[serde(default)]
    pub protocol: u32,
    pub pid: u32,
    /// Identifies this daemon process. It changes when the daemon restarts,
    /// and with it the event sequence: seqs seen before are then void.
    #[serde(default)]
    pub instance_id: String,
    /// Seq of the latest event this daemon published (0 before the first).
    #[serde(default)]
    pub latest_seq: u64,
    pub running_agents: usize,
    pub ws_clients: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The request could not be sent or the answer could not be read or decoded.
    #[error("request to the daemon failed: {0}")]
    Http(#[from] reqwest::Error),
    /// A non-2xx answer. `code` and `message` come from the daemon's
    /// `{"error", "message"}` body; when the body is not one (some errors are
    /// plain text), `code` is `http_<status>` and `message` is the raw body.
    #[error("daemon answered {status} {code}: {message}")]
    Api {
        status: u16,
        code: String,
        message: String,
    },
    /// The events WebSocket could not be opened.
    #[error("event stream failed: {0}")]
    Ws(#[from] tungstenite::Error),
    /// The local file `path` could not be opened for an upload. Nothing was
    /// sent.
    #[error("could not open {} to upload: {source}", path.display())]
    File { path: PathBuf, source: io::Error },
}

/// A file of a feedback upload: the local file at `path`, sent as the part
/// `part` (`file-<i>` or `annotated-<i>`, `i` the attachment's 1-based
/// position). The local file name is never sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackFile {
    pub part: String,
    pub path: PathBuf,
}

/// A client for one daemon, as described by its lockfile. Cheap to clone.
///
/// Connecting is bounded (5 s); whole requests are not, since some calls
/// (creating an agent's worktree in a large repo) can legitimately take long.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    port: u16,
    token: DaemonToken,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the token.
        f.debug_struct("Client")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(info: &DaemonInfo) -> Client {
        let http = reqwest::Client::builder()
            // Loopback only: an HTTP(S)_PROXY from the environment must not apply.
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            // Without TLS backends or a custom resolver there is nothing to fail.
            .expect("plain HTTP client builds");
        Client {
            http,
            port: info.port,
            token: info.token.clone(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub async fn health(&self) -> Result<Health, ClientError> {
        self.fetch(self.http.get(self.url("/v1/health"))).await
    }

    /// [`Client::health`], giving up after `timeout`.
    pub(crate) async fn health_within(&self, timeout: Duration) -> Result<Health, ClientError> {
        self.fetch(self.http.get(self.url("/v1/health")).timeout(timeout))
            .await
    }

    pub async fn add_repo(&self, path: &Path) -> Result<Repo, ClientError> {
        // Typed, not `json!`: a non-UTF-8 path is an error here, not a panic.
        #[derive(Serialize)]
        struct Body<'a> {
            path: &'a Path,
        }
        self.fetch(self.http.post(self.url("/v1/repos")).json(&Body { path }))
            .await
    }

    pub async fn list_repos(&self) -> Result<Vec<Repo>, ClientError> {
        self.fetch(self.http.get(self.url("/v1/repos"))).await
    }

    pub async fn create_agent(&self, repo_id: RepoId) -> Result<Agent, ClientError> {
        let body = json!({ "repo_id": repo_id });
        self.fetch(self.http.post(self.url("/v1/agents")).json(&body))
            .await
    }

    pub async fn list_agents(&self, include_closed: bool) -> Result<Vec<Agent>, ClientError> {
        let url = self.url(&format!("/v1/agents?include_closed={include_closed}"));
        self.fetch(self.http.get(url)).await
    }

    pub async fn close_agent(&self, id: AgentId) -> Result<(), ClientError> {
        let url = self.url(&format!("/v1/agents/{id}/close"));
        self.send(self.http.post(url)).await.map(drop)
    }

    /// Queues `text` for the agent and returns the stored user message; the
    /// turn runs in the daemon and reports through [`Client::events`].
    pub async fn send_message(&self, id: AgentId, text: &str) -> Result<ChatMessage, ClientError> {
        let url = self.url(&format!("/v1/agents/{id}/messages"));
        self.fetch(self.http.post(url).json(&json!({ "text": text })))
            .await
    }

    /// Every message of the agent with `seq > after_seq`, oldest first,
    /// fetched page by page.
    pub async fn messages(
        &self,
        id: AgentId,
        after_seq: u64,
    ) -> Result<Vec<ChatMessage>, ClientError> {
        let mut all = Vec::new();
        let mut after = after_seq;
        loop {
            let url = self.url(&format!(
                "/v1/agents/{id}/messages?after={after}&limit={MESSAGE_PAGE}"
            ));
            let page: Vec<ChatMessage> = self.fetch(self.http.get(url)).await?;
            let full = page.len() == MESSAGE_PAGE;
            let last_seq = page.last().map_or(after, |m| m.seq);
            all.extend(page);
            // A page that does not move forward ends the walk rather than loop.
            if !full || last_seq <= after {
                return Ok(all);
            }
            after = last_seq;
        }
    }

    // ------------------------------------------------------------ intents

    pub async fn create_intent(&self, req: CreateIntentRequest) -> Result<Intent, ClientError> {
        self.fetch(self.http.post(self.url("/v1/intents")).json(&req))
            .await
    }

    /// Intents matching `filter`; an absent field matches everything.
    pub async fn list_intents(&self, filter: IntentFilter) -> Result<Vec<Intent>, ClientError> {
        let mut query = Vec::new();
        if let Some(state) = filter.state {
            // The daemon expects the snake_case name serde gives the state.
            let name = serde_json::to_value(state)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            query.push(format!("state={name}"));
        }
        if let Some(repo_id) = filter.repo_id {
            query.push(format!("repo_id={repo_id}"));
        }
        // Snake_case names and uuids need no escaping.
        let mut url = self.url("/v1/intents");
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query.join("&"));
        }
        self.fetch(self.http.get(url)).await
    }

    pub async fn get_intent(&self, id: IntentId) -> Result<Intent, ClientError> {
        self.fetch(self.http.get(self.url(&format!("/v1/intents/{id}"))))
            .await
    }

    pub async fn intent_versions(&self, id: IntentId) -> Result<Vec<IntentVersion>, ClientError> {
        self.fetch(
            self.http
                .get(self.url(&format!("/v1/intents/{id}/versions"))),
        )
        .await
    }

    pub async fn update_intent(&self, id: IntentId, patch: Patch) -> Result<Intent, ClientError> {
        self.fetch(
            self.http
                .patch(self.url(&format!("/v1/intents/{id}")))
                .json(&patch),
        )
        .await
    }

    pub async fn answer_question(
        &self,
        id: IntentId,
        question: QuestionId,
        answer: AnswerInput,
    ) -> Result<Intent, ClientError> {
        let url = self.url(&format!("/v1/intents/{id}/questions/{question}/answer"));
        self.fetch(self.http.post(url).json(&answer)).await
    }

    pub async fn retry_polish(&self, id: IntentId) -> Result<Intent, ClientError> {
        let url = self.url(&format!("/v1/intents/{id}/polish/retry"));
        self.fetch(self.http.post(url)).await
    }

    pub async fn accept_item(&self, id: IntentId, item: ItemId) -> Result<Intent, ClientError> {
        let url = self.url(&format!("/v1/intents/{id}/items/{item}/accept"));
        self.fetch(self.http.post(url)).await
    }

    pub async fn drop_item(&self, id: IntentId, item: ItemId) -> Result<Intent, ClientError> {
        let url = self.url(&format!("/v1/intents/{id}/items/{item}/drop"));
        self.fetch(self.http.post(url)).await
    }

    /// Replaces the item's text with `text` and accepts the item (an edit is
    /// a decision, like accept); a depends-on item is checked as for accept.
    pub async fn edit_item(
        &self,
        id: IntentId,
        item: ItemId,
        text: &str,
    ) -> Result<Intent, ClientError> {
        let url = self.url(&format!("/v1/intents/{id}/items/{item}/edit"));
        self.fetch(self.http.post(url).json(&json!({ "text": text })))
            .await
    }

    pub async fn send_intent(&self, id: IntentId) -> Result<Intent, ClientError> {
        let url = self.url(&format!("/v1/intents/{id}/send"));
        self.fetch(self.http.post(url)).await
    }

    pub async fn resolve_intent(&self, id: IntentId) -> Result<Intent, ClientError> {
        let url = self.url(&format!("/v1/intents/{id}/resolve"));
        self.fetch(self.http.post(url)).await
    }

    pub async fn agent_defaults(&self) -> Result<AgentDefaults, ClientError> {
        self.fetch(self.http.get(self.url("/v1/settings/agent-defaults")))
            .await
    }

    pub async fn set_agent_defaults(
        &self,
        defaults: AgentDefaults,
    ) -> Result<AgentDefaults, ClientError> {
        self.fetch(
            self.http
                .put(self.url("/v1/settings/agent-defaults"))
                .json(&defaults),
        )
        .await
    }

    pub async fn feedback_settings(&self) -> Result<FeedbackSettings, ClientError> {
        self.fetch(self.http.get(self.url("/v1/settings/feedback")))
            .await
    }

    pub async fn set_feedback_settings(
        &self,
        settings: &FeedbackSettings,
    ) -> Result<FeedbackSettings, ClientError> {
        self.fetch(
            self.http
                .put(self.url("/v1/settings/feedback"))
                .json(settings),
        )
        .await
    }

    /// The repositories Submit can choose from, the meta repository and
    /// whether `gh` and `agent-browser` are ready.
    pub async fn feedback_targets(&self) -> Result<FeedbackTargets, ClientError> {
        self.fetch(self.http.get(self.url("/v1/feedback/targets")))
            .await
    }

    /// `POST /v1/feedback`: the part `request` (JSON) first, then each file,
    /// streamed from disk and named by its part (its own name stays here).
    /// No timeout: an upload of up to 100 MB and a commit can take long.
    /// Sending the same request again continues it (see the daemon).
    pub async fn submit_feedback(
        &self,
        request: &FeedbackRequest,
        files: &[FeedbackFile],
    ) -> Result<FeedbackOutcome, ClientError> {
        // Strings, numbers (a NaN is `null`), booleans and lists: nothing to fail.
        let json = serde_json::to_string(request).expect("a feedback request serializes");
        let mut form = Form::new().part("request", Part::text(json).mime_str("application/json")?);
        for file in files {
            let part = Part::file(&file.path)
                .await
                .map_err(|source| ClientError::File {
                    path: file.path.clone(),
                    source,
                })?
                .file_name(file.part.clone());
            form = form.part(file.part.clone(), part);
        }
        self.fetch(self.http.post(self.url("/v1/feedback")).multipart(form))
            .await
    }

    /// `POST /v1/feedback/{id}/retry`: runs the steps of a recorded
    /// submission that did not succeed (the intent, or the push and the
    /// issue) and answers as Submit does. No timeout: a push and `gh` can
    /// take minutes.
    pub async fn retry_feedback(&self, id: SubmissionId) -> Result<FeedbackOutcome, ClientError> {
        let url = self.url(&format!("/v1/feedback/{id}/retry"));
        self.fetch(self.http.post(url)).await
    }

    /// `POST /v1/intents/{id}/pull-request`: an idea intent's "Open pull
    /// request" (the push and `gh`), answered with the intent.
    /// `public_acknowledged`: the user confirmed that a public repository
    /// shows the screenshots to anyone (the daemon refuses a public or
    /// possibly public one without it). No timeout: a push and `gh` can take
    /// minutes.
    pub async fn open_pull_request(
        &self,
        id: IntentId,
        public_acknowledged: bool,
    ) -> Result<Intent, ClientError> {
        let url = self.url(&format!("/v1/intents/{id}/pull-request"));
        let body = json!({ "public_acknowledged": public_acknowledged });
        self.fetch(self.http.post(url).json(&body)).await
    }

    /// `POST /v1/intents/{id}/implementation`: an idea intent's "Create
    /// implementation intent", answered with the new (or the earlier) one.
    /// `public_acknowledged` as for [`Self::open_pull_request`]: its agent's
    /// pull request carries the idea's screenshots and explainer.
    pub async fn create_implementation_intent(
        &self,
        id: IntentId,
        public_acknowledged: bool,
    ) -> Result<Intent, ClientError> {
        let url = self.url(&format!("/v1/intents/{id}/implementation"));
        let body = json!({ "public_acknowledged": public_acknowledged });
        self.fetch(self.http.post(url).json(&body)).await
    }

    /// `/v1/plugins/<segments>?<params>`, each segment percent-encoded, so an
    /// id with `/`, `..`, `?` or `#` names no other route.
    ///
    /// A URL cannot carry an empty, `.` or `..` segment (they would be
    /// resolved away), so such a name is answered here as not found.
    fn plugin_url(
        &self,
        segments: &[&str],
        params: &[(&str, &str)],
    ) -> Result<reqwest::Url, ClientError> {
        if segments.iter().any(|s| matches!(*s, "" | "." | "..")) {
            return Err(ClientError::Api {
                status: 404,
                code: "not_found".into(),
                message: "no such plugin or resource".into(),
            });
        }
        // An address of the form `http://127.0.0.1:<port>/<path>` always parses.
        let mut url =
            reqwest::Url::parse(&self.url("/v1/plugins")).expect("the daemon's URL parses");
        if let Ok(mut path) = url.path_segments_mut() {
            path.extend(segments);
        }
        if !params.is_empty() {
            url.query_pairs_mut().extend_pairs(params);
        }
        Ok(url)
    }

    // ------------------------------------------------------------ plugins

    pub async fn plugins(&self) -> Result<Vec<PluginStatus>, ClientError> {
        self.fetch(self.http.get(self.url("/v1/plugins"))).await
    }

    pub async fn plugin(&self, id: &str) -> Result<PluginStatus, ClientError> {
        self.fetch(self.http.get(self.plugin_url(&[id], &[])?))
            .await
    }

    pub async fn set_plugin_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<PluginStatus, ClientError> {
        let action = if enabled { "enable" } else { "disable" };
        self.fetch(self.http.post(self.plugin_url(&[id, action], &[])?))
            .await
    }

    /// The settings as the user gave them, without declared defaults.
    pub async fn plugin_settings(&self, id: &str) -> Result<serde_json::Value, ClientError> {
        self.fetch(self.http.get(self.plugin_url(&[id, "settings"], &[])?))
            .await
    }

    /// Replaces the settings; `value` must be a JSON object. Fails with code
    /// `invalid_settings` when the plugin's manifest refuses it.
    pub async fn set_plugin_settings(
        &self,
        id: &str,
        value: &serde_json::Value,
    ) -> Result<PluginStatus, ClientError> {
        self.fetch(
            self.http
                .put(self.plugin_url(&[id, "settings"], &[])?)
                .json(value),
        )
        .await
    }

    /// Asks for a sync of one resource now; it runs in the daemon and reports
    /// through the events. Fails with code `sync_in_progress`,
    /// `plugin_disabled` or `plugin_not_ready` when it cannot be queued.
    pub async fn sync_plugin_resource(&self, id: &str, kind: &str) -> Result<(), ClientError> {
        let url = self.plugin_url(&[id, "resources", kind, "sync"], &[])?;
        self.send(self.http.post(url)).await.map(drop)
    }

    /// Installs one of a built-in plugin's command-line tools with mise and
    /// returns the plugin's status after its tools are checked again. It may
    /// take minutes. Fails with code `not_allowed` (an installed plugin),
    /// `tool_not_found`, `install_in_progress` (another install runs: one at
    /// a time), `no_installer` (the tool has no mise package), `mise_missing`
    /// (the message names mise's install page) or `install_failed`.
    pub async fn install_plugin_tool(
        &self,
        id: &str,
        name: &str,
    ) -> Result<PluginStatus, ClientError> {
        let url = self.plugin_url(&[id, "tools", name, "install"], &[])?;
        self.fetch(self.http.post(url)).await
    }

    /// One page of a resource's items, in the daemon's order. `after` is the
    /// `next` of the previous page; `q` filters by text.
    pub async fn plugin_items(
        &self,
        id: &str,
        kind: &str,
        limit: Option<u32>,
        after: Option<&str>,
        q: Option<&str>,
    ) -> Result<ItemsPage, ClientError> {
        let limit = limit.map(|n| n.to_string());
        let params = [("limit", limit.as_deref()), ("after", after), ("q", q)]
            .into_iter()
            .filter_map(|(key, value)| Some((key, value?)))
            .collect::<Vec<_>>();
        let url = self.plugin_url(&[id, "resources", kind, "items"], &params)?;
        self.fetch(self.http.get(url)).await
    }

    /// Subscribes to the daemon's events: with `since`, the retained events
    /// after that seq come first, then live ones.
    ///
    /// Fails with `ClientError::Api { status: 410, code: "events_too_old", .. }`
    /// when events after `since` are no longer retained, or when `since` is
    /// beyond the daemon's latest seq because the daemon restarted (seqs
    /// restart with it; see [`Health::instance_id`]). Either way the caller
    /// refetches the state it shows and subscribes again without `since`.
    ///
    /// The stream ends when the connection does. In particular the daemon
    /// closes it (reason `lagged`) when this subscriber falls too far behind;
    /// callers resubscribe with the last seq they saw. An event whose
    /// `kind.type` this version does not know (one a newer daemon added) is
    /// skipped, logged at debug level; any other event that cannot be decoded
    /// ends the stream, with a warning logged through `tracing`.
    pub async fn events(
        &self,
        since: Option<u64>,
    ) -> Result<impl Stream<Item = Event> + Send + Unpin + 'static, ClientError> {
        let mut url = format!("ws://127.0.0.1:{}/v1/events", self.port);
        if let Some(since) = since {
            url.push_str(&format!("?since={since}"));
        }
        let mut request = url.into_client_request()?;
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", self.token.expose()))
            .map_err(tungstenite::Error::from)?;
        auth.set_sensitive(true);
        request.headers_mut().insert(AUTHORIZATION, auth);

        let handshake = tokio_tungstenite::connect_async(request);
        let ws = match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await {
            Ok(Ok((ws, _response))) => ws,
            Ok(Err(tungstenite::Error::Http(response))) => {
                // The body is whatever arrived with the response head.
                let body = response.body().as_deref().unwrap_or_default();
                return Err(api_error(
                    response.status().as_u16(),
                    &String::from_utf8_lossy(body),
                ));
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                let timed_out = io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the daemon did not complete the WebSocket handshake",
                );
                return Err(tungstenite::Error::Io(timed_out).into());
            }
        };
        Ok(ws
            .map(read_step)
            .take_while(|step| ready(!matches!(step, Step::End)))
            .filter_map(|step| {
                ready(match step {
                    Step::Event(event) => Some(*event),
                    Step::Skip | Step::End => None,
                })
            }))
    }

    /// Sends `request` with the token and returns the decoded 2xx body.
    async fn fetch<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, ClientError> {
        Ok(self.send(request).await?.json().await?)
    }

    /// Sends `request` with the token; a non-2xx answer becomes `ClientError::Api`.
    async fn send(&self, request: RequestBuilder) -> Result<reqwest::Response, ClientError> {
        let response = request.bearer_auth(self.token.expose()).send().await?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response.text().await?;
        Err(api_error(status.as_u16(), &body))
    }
}

/// `ClientError::Api` from an error answer, falling back to `http_<status>`
/// and the raw text when the body is not the daemon's JSON error.
fn api_error(status: u16, body: &str) -> ClientError {
    #[derive(Deserialize)]
    struct ErrorBody {
        error: String,
        message: String,
    }
    match serde_json::from_str::<ErrorBody>(body) {
        Ok(e) => ClientError::Api {
            status,
            code: e.error,
            message: e.message,
        },
        Err(_) => ClientError::Api {
            status,
            code: format!("http_{status}"),
            message: body.to_owned(),
        },
    }
}

/// The `kind.type` of an undecodable event when it is one this version does
/// not know (a newer daemon's), so the event can be skipped; `None` when the
/// text is not JSON, has no such type, or has a known type (then it is
/// malformed).
fn unknown_kind(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let kind = value.get("kind")?.get("type")?.as_str()?;
    (!EventKind::TYPES.contains(&kind)).then(|| kind.to_owned())
}

/// What one WebSocket read means for the event stream.
enum Step {
    Event(Box<Event>),
    Skip,
    End,
}

fn read_step(read: Result<Message, tungstenite::Error>) -> Step {
    match read {
        Ok(Message::Text(text)) => match serde_json::from_str(&text) {
            Ok(event) => Step::Event(Box::new(event)),
            Err(e) => match unknown_kind(&text) {
                Some(kind) => {
                    tracing::debug!("skipping an event of unknown type {kind:?}");
                    Step::Skip
                }
                None => {
                    tracing::warn!("ending the event stream: undecodable event: {e}");
                    Step::End
                }
            },
        },
        Ok(Message::Close(frame)) => {
            match frame {
                Some(frame) if frame.reason == "lagged" => tracing::debug!(
                    "the daemon closed the event stream because this subscriber lagged; \
                     resubscribe with the last seen seq"
                ),
                frame => tracing::debug!("the daemon closed the event stream: {frame:?}"),
            }
            Step::End
        }
        Ok(Message::Binary(_)) => {
            tracing::warn!("ending the event stream: unexpected binary message");
            Step::End
        }
        Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => Step::Skip,
        Err(e) => {
            tracing::warn!("event stream failed: {e}");
            Step::End
        }
    }
}

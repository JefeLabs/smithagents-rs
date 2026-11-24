//! HTTP and WebSocket client for the daemon API.
//!
//! The daemon listens on 127.0.0.1 only. Every request carries the lockfile's
//! token as `Authorization: Bearer <token>`, including the events WebSocket
//! upgrade (the token never goes in a URL).

use std::fmt;
use std::future::ready;
use std::io;
use std::path::Path;
use std::time::Duration;

use futures::{Stream, StreamExt};
use reqwest::RequestBuilder;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use smithagents_core::agent::{Agent, ChatMessage, Repo};
use smithagents_core::daemon::DaemonInfo;
use smithagents_core::event::{Event, EventKind};
use smithagents_core::ids::{AgentId, RepoId};
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
}

/// A client for one daemon, as described by its lockfile. Cheap to clone.
///
/// Connecting is bounded (5 s); whole requests are not, since some calls
/// (creating an agent's worktree in a large repo) can legitimately take long.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    port: u16,
    token: String,
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
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", self.token))
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
        let response = request.bearer_auth(&self.token).send().await?;
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

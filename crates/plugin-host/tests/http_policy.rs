//! The HTTP policy, end to end through `CallHandle::http`.
//!
//! Nothing here leaves the machine. Allowed requests go to a local axum server
//! at `http://127.0.0.1:<port>` through `allow_insecure_loopback`; the only name
//! ever resolved is `localhost` (added to the manifest in code, see
//! [`manifest`]). Every denied request must leave the server untouched: no TCP
//! connection and no request.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::serve::ListenerExt;
use axum::Router;
use base64::{engine::general_purpose::STANDARD, Engine};
use smithagents_plugin_host::{CallHandle, CallPurpose, HostServices, HttpPolicy, StaticSecrets};
use smithagents_plugin_protocol::{
    Auth, HttpError, HttpMethod, HttpRequest, HttpResponse, LogLevel, Manifest,
};

const TOKEN: &str = "tok-5ecret-value-1";
const USER: &str = "usr-5ecret-value-2";
const PASSWORD: &str = "pw-5ecret-value-3";
const API_KEY: &str = "key-5ecret-value-4";
const SECRET_VALUES: [&str; 4] = [TOKEN, USER, PASSWORD, API_KEY];

const MIB: usize = 1024 * 1024;

const MANIFEST: &str = r#"
id = "test-plug"
name = "Test plugin"
version = "0.1.0"
contract = 1
host = "in-process"
hosts = ["fixture.invalid", "other.invalid"]

[[secrets]]
key = "token"
label = "Token"
hosts = ["fixture.invalid"]

[[secrets]]
key = "user"
label = "User"
hosts = ["fixture.invalid"]

[[secrets]]
key = "password"
label = "Password"
hosts = ["fixture.invalid"]

[[secrets]]
key = "api-key"
label = "API key"
hosts = ["fixture.invalid", "other.invalid"]

[[secrets]]
key = "unset"
label = "Declared but never configured"
hosts = ["fixture.invalid"]
"#;

// ---------------------------------------------------------------- test server

struct Seen {
    method: String,
    path: String,
    headers: HeaderMap,
}

#[derive(Default)]
struct Shared {
    seen: Mutex<Vec<Seen>>,
}

struct TestServer {
    port: u16,
    connections: Arc<AtomicUsize>,
    shared: Arc<Shared>,
}

impl TestServer {
    async fn start() -> TestServer {
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

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn requests(&self) -> usize {
        self.shared.seen.lock().unwrap().len()
    }

    fn paths(&self) -> Vec<String> {
        let seen = self.shared.seen.lock().unwrap();
        seen.iter().map(|s| s.path.clone()).collect()
    }

    fn last_headers(&self) -> HeaderMap {
        let seen = self.shared.seen.lock().unwrap();
        seen.last()
            .expect("the server saw no request")
            .headers
            .clone()
    }

    fn assert_untouched(&self) {
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

// ---------------------------------------------------------------- helpers

/// The test manifest plus `localhost`, the one name that resolves without the
/// network. `Manifest::parse` refuses it (it has no dot), so it is added here,
/// after parsing, to reach the policy's own checks and the resolver. Every
/// secret except `token` may also be sent there.
fn manifest() -> Manifest {
    let mut m = Manifest::parse(MANIFEST).unwrap();
    m.hosts.push("localhost".into());
    for secret in m.secrets.iter_mut().filter(|s| s.key != "token") {
        secret.hosts.push("localhost".into());
    }
    m
}

fn secrets_with(token: &str) -> Arc<StaticSecrets> {
    let mut map = HashMap::new();
    for (key, value) in [
        ("token", token),
        ("user", USER),
        ("password", PASSWORD),
        ("api-key", API_KEY),
    ] {
        map.insert(
            ("test-plug".to_string(), key.to_string()),
            value.to_string(),
        );
    }
    Arc::new(StaticSecrets(map))
}

fn services(policy: HttpPolicy) -> Arc<HostServices> {
    Arc::new(HostServices::new(&manifest(), secrets_with(TOKEN), policy).unwrap())
}

fn loopback() -> HttpPolicy {
    HttpPolicy {
        allow_insecure_loopback: true,
        ..HttpPolicy::default()
    }
}

fn sync_call(policy: HttpPolicy) -> CallHandle {
    services(policy).begin_call(CallPurpose::Sync)
}

fn request(method: HttpMethod, url: &str) -> HttpRequest {
    HttpRequest {
        method,
        url: url.to_string(),
        headers: Vec::new(),
        body: None,
        auth: None,
    }
}

fn get(url: &str) -> HttpRequest {
    request(HttpMethod::Get, url)
}

fn with_auth(url: &str, auth: Auth) -> HttpRequest {
    HttpRequest {
        auth: Some(auth),
        ..get(url)
    }
}

fn assert_no_secret(text: &str) {
    for value in SECRET_VALUES {
        assert!(!text.contains(value), "a secret value leaked into {text:?}");
    }
}

/// Checks that neither rendering of the error carries a secret value.
fn checked(err: HttpError) -> HttpError {
    assert_no_secret(&format!("{err:?}"));
    assert_no_secret(&err.to_string());
    err
}

fn ok(result: Result<HttpResponse, HttpError>) -> HttpResponse {
    match result {
        Ok(resp) => resp,
        Err(err) => panic!("expected a response, got {:?}", checked(err)),
    }
}

/// Asserts a `Denied` whose reason contains `needle`, and returns the reason.
fn denied(result: Result<HttpResponse, HttpError>, needle: &str) -> String {
    match result.map_err(checked) {
        Err(HttpError::Denied { reason }) => {
            assert!(
                reason.contains(needle),
                "reason {reason:?} does not mention {needle:?}"
            );
            reason
        }
        other => panic!("expected Denied mentioning {needle:?}, got {other:?}"),
    }
}

// ---------------------------------------------------------------- allowed requests

#[tokio::test]
async fn a_get_returns_status_headers_and_body() {
    let server = TestServer::start().await;
    let call = sync_call(loopback());

    let resp = ok(call.http(get(&server.url("/ok"))).await);

    assert_eq!(resp.status, 200);
    assert_eq!(resp.header("x-test"), Some("yes"));
    assert_eq!(resp.body, b"hello");
    assert_eq!(server.paths(), ["/ok"]);
    assert_eq!(server.shared.seen.lock().unwrap()[0].method, "GET");
}

#[tokio::test]
async fn a_head_is_allowed_during_a_sync() {
    let server = TestServer::start().await;
    let call = sync_call(loopback());

    let resp = ok(call
        .http(request(HttpMethod::Head, &server.url("/ok")))
        .await);

    assert_eq!(resp.status, 200);
    assert!(resp.body.is_empty());
    assert_eq!(server.shared.seen.lock().unwrap()[0].method, "HEAD");
}

#[tokio::test]
async fn bearer_auth_arrives_as_an_authorization_header() {
    let server = TestServer::start().await;
    let call = sync_call(loopback());

    let auth = Auth::Bearer {
        secret: "token".into(),
    };
    ok(call.http(with_auth(&server.url("/echo"), auth)).await);

    let expected = format!("Bearer {TOKEN}");
    assert_eq!(server.last_headers()["authorization"], expected.as_str());
}

#[tokio::test]
async fn basic_auth_arrives_as_an_authorization_header() {
    let server = TestServer::start().await;
    let call = sync_call(loopback());

    let auth = Auth::Basic {
        user_secret: "user".into(),
        password_secret: "password".into(),
    };
    ok(call.http(with_auth(&server.url("/echo"), auth)).await);

    let expected = format!("Basic {}", STANDARD.encode(format!("{USER}:{PASSWORD}")));
    assert_eq!(server.last_headers()["authorization"], expected.as_str());
}

#[tokio::test]
async fn header_auth_arrives_in_the_named_header_and_replaces_the_plugins_value() {
    let server = TestServer::start().await;
    let call = sync_call(loopback());

    let mut req = with_auth(
        &server.url("/echo"),
        Auth::Header {
            name: "X-Api-Key".into(),
            secret: "api-key".into(),
        },
    );
    req.headers
        .push(("x-api-key".into(), "plugin-chosen".into()));
    ok(call.http(req).await);

    let headers = server.last_headers();
    let values: Vec<_> = headers.get_all("x-api-key").iter().collect();
    assert_eq!(values, [API_KEY]);
    assert!(headers.get("authorization").is_none());
}

#[tokio::test]
async fn header_auth_may_fill_authorization() {
    let server = TestServer::start().await;
    let call = sync_call(loopback());

    let auth = Auth::Header {
        name: "Authorization".into(),
        secret: "api-key".into(),
    };
    ok(call.http(with_auth(&server.url("/echo"), auth)).await);

    assert_eq!(server.last_headers()["authorization"], API_KEY);
}

#[tokio::test]
async fn header_auth_may_not_name_routing_or_framing_headers() {
    let server = TestServer::start().await;
    let call = sync_call(loopback());

    for name in [
        "Host",
        "cookie",
        "Connection",
        "content-length",
        "Transfer-Encoding",
        "Upgrade",
        "TE",
        "trailer",
        "Proxy-Authorization",
        "proxy-anything",
        "X-HTTP-Method-Override",
        "x-http-method",
        "X-Method-Override",
    ] {
        let auth = Auth::Header {
            name: name.into(),
            secret: "api-key".into(),
        };
        denied(
            call.http(with_auth(&server.url("/echo"), auth)).await,
            "may not set",
        );
    }
    for name in ["bad name", "", "x:y"] {
        let auth = Auth::Header {
            name: name.into(),
            secret: "api-key".into(),
        };
        denied(
            call.http(with_auth(&server.url("/echo"), auth)).await,
            "header name",
        );
    }

    server.assert_untouched();
}

#[tokio::test]
async fn plugin_headers_are_sent() {
    let server = TestServer::start().await;
    let call = sync_call(loopback());

    let mut req = get(&server.url("/echo"));
    req.headers
        .push(("Accept".into(), "application/json".into()));
    req.headers.push(("X-Custom".into(), "one".into()));
    ok(call.http(req).await);

    let headers = server.last_headers();
    assert_eq!(headers["accept"], "application/json");
    assert_eq!(headers["x-custom"], "one");
}

#[tokio::test]
async fn a_redirect_is_returned_and_not_followed() {
    let server = TestServer::start().await;
    let call = sync_call(loopback());

    let auth = Auth::Bearer {
        secret: "token".into(),
    };
    let resp = ok(call.http(with_auth(&server.url("/redirect"), auth)).await);

    assert_eq!(resp.status, 302);
    assert_eq!(resp.header("location"), Some("/target"));
    assert_eq!(
        server.paths(),
        ["/redirect"],
        "the redirect target was requested"
    );
}

#[tokio::test]
async fn a_body_over_the_cap_is_too_large() {
    let server = TestServer::start().await;
    let call = sync_call(loopback());

    let exact = ok(call.http(get(&server.url("/exact"))).await);
    assert_eq!(exact.body.len(), 5 * MIB);

    let err = checked(call.http(get(&server.url("/big"))).await.unwrap_err());
    assert_eq!(err, HttpError::TooLarge);
}

#[tokio::test]
async fn a_slow_server_is_a_timeout() {
    let server = TestServer::start().await;
    let call = sync_call(HttpPolicy {
        timeout: Duration::from_millis(200),
        ..loopback()
    });

    let started = Instant::now();
    let err = checked(call.http(get(&server.url("/slow"))).await.unwrap_err());

    assert_eq!(err, HttpError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn the_51st_request_in_one_call_is_denied_and_a_new_call_starts_at_zero() {
    let server = TestServer::start().await;
    let services = services(loopback());
    let call = services.begin_call(CallPurpose::Sync);

    for _ in 0..50 {
        ok(call.http(get(&server.url("/ok"))).await);
    }
    denied(call.http(get(&server.url("/ok"))).await, "50");
    assert_eq!(server.requests(), 50);

    // A clone shares the call's budget.
    denied(call.clone().http(get(&server.url("/ok"))).await, "50");

    let next = services.begin_call(CallPurpose::Sync);
    ok(next.http(get(&server.url("/ok"))).await);
    assert_eq!(server.requests(), 51);
}

// ---------------------------------------------------------------- denied requests

#[tokio::test]
async fn a_secret_is_sent_only_to_its_own_hosts() {
    let server = TestServer::start().await;
    let call = sync_call(HttpPolicy::default());

    // `token` is bound to fixture.invalid; localhost is declared for the plugin
    // but not for that secret.
    let token = Auth::Bearer {
        secret: "token".into(),
    };
    let reason = denied(
        call.http(with_auth("https://localhost/", token)).await,
        "is not allowed for host",
    );
    assert!(
        reason.contains("\"token\"") && reason.contains("localhost"),
        "{reason}"
    );
    let basic = Auth::Basic {
        user_secret: "user".into(),
        password_secret: "token".into(),
    };
    denied(
        call.http(with_auth("https://localhost/", basic)).await,
        "is not allowed for host",
    );
    let header = Auth::Header {
        name: "X-Api-Key".into(),
        secret: "token".into(),
    };
    denied(
        call.http(with_auth("https://localhost/", header)).await,
        "is not allowed for host",
    );

    // A secret bound to localhost gets past the binding and is stopped only by
    // the address filter.
    let api_key = Auth::Bearer {
        secret: "api-key".into(),
    };
    denied(
        call.http(with_auth("https://localhost/", api_key)).await,
        "resolves only to",
    );

    server.assert_untouched();
}

#[tokio::test]
async fn under_the_loopback_exemption_any_declared_secret_may_be_sent() {
    let server = TestServer::start().await;
    let call = sync_call(loopback());

    let token = Auth::Bearer {
        secret: "token".into(),
    };
    ok(call.http(with_auth(&server.url("/echo"), token)).await);

    assert_eq!(
        server.last_headers()["authorization"],
        format!("Bearer {TOKEN}").as_str()
    );
}

#[tokio::test]
async fn url_rules_without_the_loopback_exemption() {
    let server = TestServer::start().await;
    let call = sync_call(HttpPolicy::default());

    // Plain http, even to the local server.
    denied(call.http(get(&server.url("/ok"))).await, "https");
    denied(call.http(get("http://localhost/")).await, "https");
    // Undeclared hosts, including a trailing dot.
    denied(
        call.http(get("https://undeclared.invalid/")).await,
        "not declared",
    );
    denied(call.http(get("https://localhost./")).await, "not declared");
    denied(
        call.http(get("https://sub.localhost/")).await,
        "not declared",
    );
    // IP literals in every spelling the URL parser accepts.
    for url in [
        format!("https://127.0.0.1:{}/", server.port),
        "https://127.0.0.1/".to_string(),
        "https://[::1]/".to_string(),
        "https://[::ffff:127.0.0.1]/".to_string(),
        "https://0x7f.0x0.0x0.0x1/".to_string(),
        "https://2130706433/".to_string(),
        "https://8.8.8.8/".to_string(),
    ] {
        denied(call.http(get(&url)).await, "IP address");
    }
    // Any port but 443.
    denied(call.http(get("https://localhost:8443/")).await, "port 8443");
    denied(call.http(get("https://localhost:80/")).await, "port 80");
    // User info.
    denied(
        call.http(get("https://user:pw@localhost/")).await,
        "user name or password",
    );
    denied(
        call.http(get("https://user@localhost/")).await,
        "user name or password",
    );
    // Not a URL at all.
    denied(call.http(get("not a url")).await, "invalid URL");

    server.assert_untouched();
}

#[tokio::test]
async fn a_host_differing_only_by_case_passes_the_host_check() {
    let call = sync_call(HttpPolicy::default());

    // `LOCALHOST` matches the declared `localhost`, so the request gets past the
    // URL rules and is stopped only by the address filter, inside the resolver
    // reqwest connects through: every address of localhost is loopback.
    denied(
        call.http(get("https://LOCALHOST/")).await,
        "resolves only to",
    );
}

#[tokio::test]
async fn a_declared_host_that_resolves_to_loopback_is_denied() {
    // The exemption covers http://127.0.0.1 only; names are always filtered.
    for policy in [HttpPolicy::default(), loopback()] {
        let call = sync_call(policy);
        denied(
            call.http(get("https://localhost/")).await,
            "resolves only to",
        );
    }
}

/// The rules that do not depend on the URL, against `base`.
async fn assert_request_rules(call: &CallHandle, base: &str) {
    for method in [
        HttpMethod::Post,
        HttpMethod::Put,
        HttpMethod::Patch,
        HttpMethod::Delete,
    ] {
        denied(call.http(request(method, base)).await, "GET and HEAD");
    }

    for (name, value) in [
        ("Authorization", "Bearer plugin-made"),
        ("cookie", "session=1"),
        ("HOST", "elsewhere.invalid"),
        ("Proxy-Authorization", "Basic x"),
        ("proxy-connection", "keep-alive"),
        ("Transfer-Encoding", "chunked"),
        ("content-length", "0"),
        // A sync only reads: no asking the server to treat this GET as a write.
        ("X-HTTP-Method-Override", "DELETE"),
        ("x-http-method", "PUT"),
        ("X-Method-Override", "POST"),
    ] {
        let mut req = get(base);
        req.headers.push(("Accept".into(), "*/*".into()));
        req.headers.push((name.into(), value.into()));
        denied(call.http(req).await, "may not set");
    }

    let mut req = get(base);
    req.headers.push(("bad header".into(), "x".into()));
    denied(call.http(req).await, "header name");
    let mut req = get(base);
    req.headers.push(("x-ok".into(), "line\nbreak".into()));
    denied(call.http(req).await, "header value");

    let undeclared = Auth::Bearer {
        secret: "nope".into(),
    };
    denied(call.http(with_auth(base, undeclared)).await, "not declared");
    let undeclared_password = Auth::Basic {
        user_secret: "user".into(),
        password_secret: "nope".into(),
    };
    denied(
        call.http(with_auth(base, undeclared_password)).await,
        "not declared",
    );
    let host_header = Auth::Header {
        name: "Host".into(),
        secret: "api-key".into(),
    };
    denied(call.http(with_auth(base, host_header)).await, "may not set");

    let unset = Auth::Bearer {
        secret: "unset".into(),
    };
    let err = checked(call.http(with_auth(base, unset)).await.unwrap_err());
    assert_eq!(
        err,
        HttpError::SecretMissing {
            key: "unset".into()
        }
    );

    let mut req = request(HttpMethod::Get, base);
    req.body = Some(vec![0; MIB + 1]);
    denied(call.http(req).await, "larger than");
    for method in [HttpMethod::Get, HttpMethod::Head] {
        let mut req = request(method, base);
        req.body = Some(b"x".to_vec());
        denied(call.http(req).await, "may not send a request body");
    }
}

#[tokio::test]
async fn request_rules_without_the_loopback_exemption() {
    let server = TestServer::start().await;
    let call = sync_call(HttpPolicy::default());

    assert_request_rules(&call, "https://localhost/").await;

    server.assert_untouched();
}

#[tokio::test]
async fn request_rules_still_apply_under_the_loopback_exemption() {
    let server = TestServer::start().await;
    let call = sync_call(loopback());

    assert_request_rules(&call, &server.url("/echo")).await;
    let with_user = format!("http://user:pw@127.0.0.1:{}/echo", server.port);
    denied(call.http(get(&with_user)).await, "user name or password");

    server.assert_untouched();
}

#[tokio::test]
async fn the_loopback_exemption_covers_only_plain_http_to_127_0_0_1() {
    let server = TestServer::start().await;
    let call = sync_call(loopback());
    let port = server.port;

    denied(
        call.http(get(&format!("https://127.0.0.1:{port}/"))).await,
        "IP address",
    );
    denied(
        call.http(get(&format!("http://localhost:{port}/"))).await,
        "https",
    );
    denied(
        call.http(get(&format!("http://[::1]:{port}/"))).await,
        "https",
    );
    denied(
        call.http(get(&format!("http://127.0.0.2:{port}/"))).await,
        "https",
    );

    server.assert_untouched();
}

#[tokio::test]
async fn a_secret_that_is_not_a_valid_header_value_is_denied_without_echoing_it() {
    let server = TestServer::start().await;
    let bad = format!("{TOKEN}\r\nX-Injected: 1");
    let services =
        Arc::new(HostServices::new(&manifest(), secrets_with(&bad), loopback()).unwrap());
    let call = services.begin_call(CallPurpose::Sync);

    let auth = Auth::Bearer {
        secret: "token".into(),
    };
    let reason = denied(
        call.http(with_auth(&server.url("/echo"), auth)).await,
        "not a valid header value",
    );
    assert!(reason.contains("\"token\""), "{reason}");

    server.assert_untouched();
}

// ---------------------------------------------------------------- construction

#[test]
fn a_manifest_host_the_url_parser_reads_as_an_ip_is_refused() {
    // `Manifest::parse` refuses this host; a manifest built in code can still
    // carry it, and a URL parser reads it as 127.0.0.1.
    let mut manifest = manifest();
    manifest.hosts = vec!["0x7f.0x0.0x0.0x1".into()];

    let err = HostServices::new(&manifest, secrets_with(TOKEN), HttpPolicy::default())
        .expect_err("an IP-like host must be refused");

    assert!(err.to_string().contains("0x7f.0x0.0x0.0x1"), "{err}");
    assert_no_secret(&err.to_string());
    assert_no_secret(&format!("{err:?}"));
}

#[test]
fn the_default_policy_is_the_plan_decisions() {
    let policy = HttpPolicy::default();
    assert!(!policy.allow_insecure_loopback);
    assert_eq!(policy.timeout, Duration::from_secs(30));
    assert_eq!(policy.max_response_bytes, 5 * MIB);
    assert_eq!(policy.max_request_bytes, MIB);
    assert_eq!(policy.max_requests_per_call, 50);
}

#[test]
fn a_call_handle_and_its_requests_can_cross_threads() {
    fn shareable<T: Send + Sync + Clone + 'static>(_: &T) {}
    fn sendable<F: std::future::Future + Send + 'static>(_: F) {}

    let call = sync_call(HttpPolicy::default());
    shareable(&call);
    let owned = call.clone();
    // Built, never polled: only the type is checked.
    sendable(async move { owned.http(get("https://localhost/")).await });
}

// ---------------------------------------------------------------- logging

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn log_names_the_plugin_and_truncates_the_message_to_2_kib() {
    let captured = Captured::default();
    let sink = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || sink.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    let call = sync_call(HttpPolicy::default());

    tracing::subscriber::with_default(subscriber, || {
        call.log(LogLevel::Warn, &"a".repeat(5000));
        call.log(
            LogLevel::Info,
            "fake\nline \u{1b}[31mred \u{202e}rev \u{2028}sep \u{2066}iso \u{2069}",
        );
        // Escaped first, then cut: 3000 newlines become 1024 `\n` pairs.
        call.log(LogLevel::Error, &"\n".repeat(3000));
    });

    let out = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert!(lines[0].contains("WARN"), "{}", lines[0]);
    assert!(lines[0].contains("test-plug"), "{}", lines[0]);
    assert!(lines[0].contains(&"a".repeat(2048)), "message cut short");
    assert!(
        !lines[0].contains(&"a".repeat(2049)),
        "message not truncated"
    );
    assert!(lines[1].contains("INFO") && lines[1].contains("test-plug"));
    for c in ['\u{1b}', '\u{202e}', '\u{2028}', '\u{2066}', '\u{2069}'] {
        assert!(!out.contains(c), "{c:?} reaches the log");
    }
    assert!(lines[1].contains("\\u{202e}rev"), "{}", lines[1]);
    assert!(lines[2].contains("ERROR"), "{}", lines[2]);
    assert!(lines[2].contains(&"\\n".repeat(1024)), "escapes cut short");
    assert!(
        !lines[2].contains(&"\\n".repeat(1025)),
        "escaped text not cut"
    );
}

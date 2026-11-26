//! The HTTP policy and the client every plugin request goes through.

use std::error::Error as StdError;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, Url};
use smithagents_plugin_protocol::{HttpError, HttpMethod, HttpResponse};

use crate::services::CallPurpose;

const USER_AGENT: &str = concat!("smithagents/", env!("CARGO_PKG_VERSION"));

/// Limits and switches for plugin HTTP. It is built in code only, never read
/// from a file or the environment.
#[derive(Debug, Clone)]
pub struct HttpPolicy {
    /// Tests only. Also allows `http://127.0.0.1:<any port>`, whatever the
    /// manifest's hosts, without the address filter. Every other rule still applies.
    pub allow_insecure_loopback: bool,
    /// For the whole exchange: connecting, sending and reading the body.
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub max_request_bytes: usize,
    pub max_requests_per_call: u32,
}

impl Default for HttpPolicy {
    fn default() -> HttpPolicy {
        HttpPolicy {
            allow_insecure_loopback: false,
            timeout: Duration::from_secs(30),
            max_response_bytes: 5 * 1024 * 1024,
            max_request_bytes: 1024 * 1024,
            max_requests_per_call: 50,
        }
    }
}

pub(crate) fn denied(reason: impl Into<String>) -> HttpError {
    HttpError::Denied {
        reason: reason.into(),
    }
}

/// One plugin's client: no redirects, no proxy, no cookie store, and every
/// name resolved through [`PublicOnly`].
pub(crate) fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .dns_resolver(PublicOnly)
        .user_agent(USER_AGENT)
        .build()
}

/// Whether a URL parser reads `host` as an IP address (or cannot read it as a
/// name at all), as it does for `0x7f.0x0.0x0.0x1` or `2130706433`.
pub(crate) fn is_ip_host(host: &str) -> bool {
    Url::parse(&format!("https://{host}/")).map_or(true, |url| url.domain().is_none())
}

/// Where a checked request goes.
pub(crate) struct Target {
    pub(crate) url: Url,
    /// The declared host, or `None` for `http://127.0.0.1` under the loopback exemption.
    pub(crate) host: Option<String>,
}

/// Checks where a request goes: `https` to a declared host on port 443, or,
/// under the loopback exemption, `http://127.0.0.1`.
pub(crate) fn check_url(
    raw: &str,
    hosts: &[String],
    policy: &HttpPolicy,
) -> Result<Target, HttpError> {
    let url = Url::parse(raw).map_err(|e| denied(format!("invalid URL: {e}")))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(denied("a URL may not carry a user name or password"));
    }
    if policy.allow_insecure_loopback
        && url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
    {
        return Ok(Target { url, host: None });
    }
    if url.scheme() != "https" {
        return Err(denied(format!(
            "only https URLs are allowed, not {}",
            url.scheme()
        )));
    }
    // `domain` is `None` for IP literals in every spelling the parser accepts.
    let host = match (url.domain(), url.host_str()) {
        (Some(domain), _) => domain.to_string(),
        (None, Some(_)) => return Err(denied("IP address hosts are not allowed")),
        (None, None) => return Err(denied("the URL has no host")),
    };
    if !hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
        return Err(denied(format!(
            "host {host:?} is not declared in the plugin manifest"
        )));
    }
    match url.port_or_known_default() {
        Some(443) => Ok(Target {
            url,
            host: Some(host),
        }),
        Some(port) => Err(denied(format!("port {port} is not allowed; only 443"))),
        None => Err(denied("the URL has no port")),
    }
}

pub(crate) fn check_method(purpose: CallPurpose, method: HttpMethod) -> Result<Method, HttpError> {
    let method = match method {
        HttpMethod::Get => Method::GET,
        HttpMethod::Head => Method::HEAD,
        HttpMethod::Post => Method::POST,
        HttpMethod::Put => Method::PUT,
        HttpMethod::Patch => Method::PATCH,
        HttpMethod::Delete => Method::DELETE,
    };
    match purpose {
        CallPurpose::Sync if method == Method::GET || method == Method::HEAD => Ok(method),
        CallPurpose::Sync => Err(denied(format!(
            "{method} is not allowed during a sync; only GET and HEAD"
        ))),
    }
}

/// `Cookie` and the headers that route or frame the request. The host and the
/// HTTP client set these; a plugin never does, not even through `Auth::Header`.
fn routing_or_framing(name: &HeaderName) -> bool {
    let name = name.as_str();
    name.starts_with("proxy-")
        || matches!(
            name,
            "host"
                | "cookie"
                | "connection"
                | "content-length"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
        )
}

/// Headers asking a server to treat the request as another method.
fn method_override(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "x-http-method-override" | "x-http-method" | "x-method-override"
    )
}

/// Headers a plugin may not set itself: credentials (`Authorization`,
/// `Cookie`), routing and framing, `Keep-Alive`, and during a sync the
/// method overrides.
fn plugin_may_not_set(name: &HeaderName, purpose: CallPurpose) -> bool {
    routing_or_framing(name)
        || matches!(name.as_str(), "authorization" | "keep-alive")
        || (purpose == CallPurpose::Sync && method_override(name))
}

pub(crate) fn plugin_headers(
    pairs: &[(String, String)],
    purpose: CallPurpose,
) -> Result<HeaderMap, HttpError> {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| denied(format!("invalid header name {name:?}")))?;
        if plugin_may_not_set(&name, purpose) {
            return Err(denied(format!("a plugin may not set the {name} header")));
        }
        let value = HeaderValue::from_str(value)
            .map_err(|_| denied(format!("invalid header value for {name}")))?;
        headers.append(name, value);
    }
    Ok(headers)
}

/// The header an `Auth::Header` fills: a valid header token that does not
/// route or frame the request. `Authorization` is allowed, for schemes such as
/// `Authorization: token <value>`. During a sync it may not be a method override.
pub(crate) fn auth_header_name(name: &str, purpose: CallPurpose) -> Result<HeaderName, HttpError> {
    let parsed = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| denied(format!("invalid auth header name {name:?}")))?;
    if routing_or_framing(&parsed) || (purpose == CallPurpose::Sync && method_override(&parsed)) {
        return Err(denied(format!("auth may not set the {parsed} header")));
    }
    Ok(parsed)
}

/// Sends a checked request and reads the response within the policy's
/// timeout and size cap. Redirects come back as they are.
pub(crate) async fn send(
    client: &reqwest::Client,
    policy: &HttpPolicy,
    method: Method,
    url: Url,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
) -> Result<HttpResponse, HttpError> {
    let mut request = client.request(method, url).headers(headers);
    if let Some(body) = body {
        request = request.body(body);
    }
    let exchange = async {
        let mut response = request.send().await.map_err(transport)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
                (name.as_str().to_string(), value)
            })
            .collect();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport)? {
            if body.len() + chunk.len() > policy.max_response_bytes {
                return Err(HttpError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    };
    tokio::time::timeout(policy.timeout, exchange)
        .await
        .unwrap_or(Err(HttpError::Timeout))
}

fn transport(err: reqwest::Error) -> HttpError {
    let mut cause: Option<&(dyn StdError + 'static)> = Some(&err);
    while let Some(e) = cause {
        if let Some(refused) = e.downcast_ref::<AddressRefused>() {
            return denied(refused.to_string());
        }
        cause = e.source();
    }
    if err.is_timeout() {
        return HttpError::Timeout;
    }
    // Error texts carry no header values, so no secret can appear here.
    let err = err.without_url();
    let mut message = err.to_string();
    let mut cause = err.source();
    while let Some(e) = cause {
        message.push_str(": ");
        message.push_str(&e.to_string());
        cause = e.source();
    }
    HttpError::Transport { message }
}

/// Every address of a name was filtered out.
#[derive(Debug)]
struct AddressRefused {
    host: String,
}

impl fmt::Display for AddressRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} resolves only to loopback, private, link-local or other non-public addresses",
            self.host
        )
    }
}

impl StdError for AddressRefused {}

/// The resolver reqwest connects through: it drops every non-public address,
/// so the addresses checked are the addresses used. IP literals never reach a
/// resolver; [`check_url`] refuses them first.
struct PublicOnly;

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(public_addresses(name.as_str().to_string()))
    }
}

async fn public_addresses(host: String) -> Result<Addrs, Box<dyn StdError + Send + Sync>> {
    let found: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
    if found.is_empty() {
        let message = format!("{host} has no addresses");
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, message).into());
    }
    let public: Vec<SocketAddr> = found.into_iter().filter(|a| is_public(a.ip())).collect();
    if public.is_empty() {
        return Err(Box::new(AddressRefused { host }));
    }
    Ok(Box::new(public.into_iter()))
}

/// The address filter: whether a resolved address may be connected to.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match embedded_v4(v6) {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(v6),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let refused = a == 0 // 0/8: this network, including unspecified
        || ip.is_loopback() // 127/8
        || ip.is_private() // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local() // 169.254/16
        || (a == 100 && (b & 0xc0) == 64) // 100.64/10: carrier-grade NAT
        || (a == 192 && b == 0 && c == 0) // 192.0.0/24: IETF protocol assignments
        || (a == 192 && b == 0 && c == 2) // 192.0.2/24: documentation
        || (a == 198 && (b & 0xfe) == 18) // 198.18/15: benchmarking
        || (a == 198 && b == 51 && c == 100) // 198.51.100/24: documentation
        || (a == 203 && b == 0 && c == 113) // 203.0.113/24: documentation
        || ip.is_multicast() // 224/4
        || a >= 240; // 240/4: reserved, and broadcast
    !refused
}

/// Global unicast (`2000::/3`) only, minus the tunnelling and documentation
/// blocks inside it. Everything else, such as `::1`, `fc00::/7`, `fe80::/10`,
/// `ff00::/8`, `100::/64` and `64:ff9b:1::/48`, lies outside `2000::/3`.
fn is_public_v6(ip: Ipv6Addr) -> bool {
    let [a, b, ..] = ip.segments();
    (a & 0xe000) == 0x2000
        && a != 0x2002 // 2002::/16: 6to4
        && !(a == 0x2001 && b == 0) // 2001::/32: Teredo
        && !(a == 0x2001 && b == 0x0db8) // 2001:db8::/32: documentation
}

/// The IPv4 address inside an IPv4-mapped (`::ffff:a.b.c.d`), IPv4-compatible
/// (`::a.b.c.d`) or NAT64 (`64:ff9b::a.b.c.d`) address.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = ip.to_ipv4() {
        return Some(v4);
    }
    let s = ip.segments();
    (s[..6] == [0x64, 0xff9b, 0, 0, 0, 0])
        .then(|| Ipv4Addr::from((u32::from(s[6]) << 16) | u32::from(s[7])))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(addr: &str) -> bool {
        is_public(addr.parse().unwrap())
    }

    #[test]
    fn the_address_filter_refuses_private_and_local_ranges() {
        for addr in [
            // IPv4: unspecified and this network, loopback, private, link-local, CGNAT
            "0.0.0.0",
            "0.1.2.3",
            "127.0.0.1",
            "127.255.255.254",
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.0.1",
            "192.168.255.255",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.255",
            // IPv4: special purpose, documentation, benchmarking
            "192.0.0.8",
            "192.0.2.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.1",
            "203.0.113.1",
            // IPv4: multicast, reserved, broadcast
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            // IPv6: unspecified, loopback, unique local, link-local, site-local, multicast
            "::",
            "::1",
            "fc00::1",
            "fdff:ffff::1",
            "fe80::1",
            "febf::1",
            "fec0::1",
            "ff02::1",
            // IPv6 outside 2000::/3, and the special blocks inside it
            "100::1",
            "64:ff9b:1::1",
            "1000::1",
            "4000::1",
            "2002::1",
            "2002:808:808::1",
            "2001::1",
            "2001:0:ffff::1",
            "2001:db8::1",
            "2001:db8:ffff::1",
            // IPv4 inside IPv6
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:198.18.0.1",
            "::127.0.0.1",
            "64:ff9b::7f00:1",
            "64:ff9b::a00:1",
        ] {
            assert!(!public(addr), "{addr} passed the filter");
        }
    }

    #[test]
    fn the_address_filter_keeps_public_addresses() {
        for addr in [
            "8.8.8.8",
            "140.82.112.6",
            "1.1.1.1",
            "100.63.255.255",
            "100.128.0.1",
            "172.15.255.255",
            "172.32.0.1",
            "192.169.0.1",
            "192.0.1.1",
            "192.0.3.1",
            "198.17.255.255",
            "198.20.0.1",
            "198.51.101.1",
            "203.0.114.1",
            "2606:4700::1",
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
            "2001:1::1",
            "2001:db9::1",
            "3ffe::1",
            "2a01:4f8::1",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
        ] {
            assert!(public(addr), "{addr} was refused");
        }
    }

    #[test]
    fn ip_spellings_are_ip_hosts() {
        for host in [
            "127.0.0.1",
            "0x7f.0x0.0x0.0x1",
            "2130706433",
            "0177.0.0.1",
            "[::1]",
        ] {
            assert!(is_ip_host(host), "{host}");
        }
        for host in ["api.github.com", "localhost", "0x7f.example"] {
            assert!(!is_ip_host(host), "{host}");
        }
    }
}

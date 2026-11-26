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

/// Checks where a request goes: `https` to a declared host on port 443, or,
/// under the loopback exemption, `http://127.0.0.1`.
pub(crate) fn check_url(
    raw: &str,
    hosts: &[String],
    policy: &HttpPolicy,
) -> Result<Url, HttpError> {
    let url = Url::parse(raw).map_err(|e| denied(format!("invalid URL: {e}")))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(denied("a URL may not carry a user name or password"));
    }
    if policy.allow_insecure_loopback
        && url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
    {
        return Ok(url);
    }
    if url.scheme() != "https" {
        return Err(denied(format!(
            "only https URLs are allowed, not {}",
            url.scheme()
        )));
    }
    // `domain` is `None` for IP literals in every spelling the parser accepts.
    let host = match (url.domain(), url.host_str()) {
        (Some(domain), _) => domain,
        (None, Some(_)) => return Err(denied("IP address hosts are not allowed")),
        (None, None) => return Err(denied("the URL has no host")),
    };
    if !hosts.iter().any(|h| h.eq_ignore_ascii_case(host)) {
        return Err(denied(format!(
            "host {host:?} is not declared in the plugin manifest"
        )));
    }
    match url.port_or_known_default() {
        Some(443) => Ok(url),
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

/// Headers the host owns: credentials and routing (`Authorization`, `Cookie`,
/// `Host`, `Proxy-*`) and message framing, which the HTTP client sets itself.
fn host_owned(name: &HeaderName) -> bool {
    let name = name.as_str();
    name.starts_with("proxy-")
        || matches!(
            name,
            "authorization"
                | "cookie"
                | "host"
                | "connection"
                | "content-length"
                | "keep-alive"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
        )
}

pub(crate) fn plugin_headers(pairs: &[(String, String)]) -> Result<HeaderMap, HttpError> {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| denied(format!("invalid header name {name:?}")))?;
        if host_owned(&name) {
            return Err(denied(format!("a plugin may not set the {name} header")));
        }
        let value = HeaderValue::from_str(value)
            .map_err(|_| denied(format!("invalid header value for {name}")))?;
        headers.append(name, value);
    }
    Ok(headers)
}

/// The header an `Auth::Header` fills. It may carry a credential
/// (`Authorization`, `Cookie`), but not route or frame the request.
pub(crate) fn auth_header_name(name: &str) -> Result<HeaderName, HttpError> {
    let parsed = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| denied(format!("invalid auth header name {name:?}")))?;
    if host_owned(&parsed) && !matches!(parsed.as_str(), "authorization" | "cookie") {
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
    let [a, b, ..] = ip.octets();
    !(a == 0                                // 0/8: this network, including unspecified
        || ip.is_loopback()                 // 127/8
        || ip.is_private()                  // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local()               // 169.254/16
        || (a == 100 && (b & 0xc0) == 64)   // 100.64/10: carrier-grade NAT
        || ip.is_multicast()                // 224/4
        || a >= 240) // 240/4: reserved, and broadcast
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !(ip.is_unspecified()
        || ip.is_loopback()
        || (first & 0xfe00) == 0xfc00   // fc00::/7: unique local
        || (first & 0xffc0) == 0xfe80   // fe80::/10: link-local
        || (first & 0xffc0) == 0xfec0   // fec0::/10: site-local (deprecated)
        || ip.is_multicast()) // ff00::/8
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
            // IPv4 inside IPv6
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
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
            "2606:4700:4700::1111",
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

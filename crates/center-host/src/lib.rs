//! Interface between the native shell and the web engine that renders center
//! tabs, plus the page boot data and the navigation policy. Pure: no GUI deps.

use raw_window_handle::HasWindowHandle;
use serde_json::json;
use smithagents_theme::state::ThemeState;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

#[derive(Debug, Clone, PartialEq)]
pub enum TabKind {
    Chat { agent_id: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabSpec {
    pub id: TabId,
    pub kind: TabKind,
    pub title: String,
}

/// Where the center content area is, for the engine's view.
///
/// `x`, `y`, `w`, `h` are **device pixels** relative to the top-left of the
/// window's content area: the shell's logical pixels times `scale`, the
/// window's scale factor (see `smithagents_app::view_models::center_rect`).
/// Divide by `scale` for points (macOS view frames).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub scale: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("daemon port must be in 1..=65535")]
    InvalidPort,
    #[error("serializing boot data failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("page origin must be smithagents://app or http://localhost|127.0.0.1:<port>")]
    InvalidOrigin,
    #[error("token and agent id must not be empty")]
    EmptyBootValue,
    #[error("web engine error: {0}")]
    Engine(String),
}

pub trait CenterTabHost {
    fn attach(&mut self, window: &dyn HasWindowHandle) -> Result<(), HostError>;
    fn open(&mut self, spec: TabSpec, boot: PageBoot) -> Result<(), HostError>;
    fn close(&mut self, id: TabId);
    /// Makes one tab visible and hides the others.
    fn show(&mut self, id: TabId);
    fn set_bounds(&mut self, rect: PixelRect);
    fn push_theme(&mut self, state: &ThemeState);
    /// Hands a filtered target to the system browser.
    fn open_external(&mut self, url: &ExternalUrl);
    /// The app is quitting or its window is closing: release the engine
    /// (for CEF, `CefShutdown`). Called once, after which no other call
    /// comes. The default does nothing.
    fn shutdown(&mut self) {}
}

/// A boxed host is a host, so a shell can hold `Box<dyn CenterTabHost>` and
/// pick the engine at startup.
impl<H: CenterTabHost + ?Sized> CenterTabHost for Box<H> {
    fn attach(&mut self, window: &dyn HasWindowHandle) -> Result<(), HostError> {
        (**self).attach(window)
    }
    fn open(&mut self, spec: TabSpec, boot: PageBoot) -> Result<(), HostError> {
        (**self).open(spec, boot)
    }
    fn close(&mut self, id: TabId) {
        (**self).close(id)
    }
    fn show(&mut self, id: TabId) {
        (**self).show(id)
    }
    fn set_bounds(&mut self, rect: PixelRect) {
        (**self).set_bounds(rect)
    }
    fn push_theme(&mut self, state: &ThemeState) {
        (**self).push_theme(state)
    }
    fn open_external(&mut self, url: &ExternalUrl) {
        (**self).open_external(url)
    }
    fn shutdown(&mut self) {
        (**self).shutdown()
    }
}

/// What the engine needs to boot a page. Only [`page_boot`] can build one.
///
/// The host script carries the token. The page's Content-Security-Policy blocks
/// inline scripts, so it must be run through an embedder API at document start
/// (never injected as an inline script). `Debug` and
/// [`PageBoot::redacted_host_script`] never show the token.
#[derive(Clone, PartialEq)]
pub struct PageBoot {
    page_url: String,
    host_script: String,
    redacted_script: String,
}

impl PageBoot {
    /// The page to load; never contains the token.
    pub fn page_url(&self) -> &str {
        &self.page_url
    }

    /// The script to run at document start. Contains the token: do not log it.
    pub fn host_script(&self) -> &str {
        &self.host_script
    }

    /// The same script with the token replaced by `REDACTED`, safe to log.
    pub fn redacted_host_script(&self) -> &str {
        &self.redacted_script
    }
}

impl std::fmt::Debug for PageBoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageBoot")
            .field("page_url", &self.page_url)
            .field("host_script", &self.redacted_script)
            .finish()
    }
}

/// Serializes to JSON that is safe inside any script context.
fn script_json<T: serde::Serialize>(v: &T) -> Result<String, HostError> {
    Ok(serde_json::to_string(v)?
        .replace('<', "\\u003c")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029"))
}

fn script(theme: &str, base_url: &str, token: &str, agent_id: &str) -> Result<String, HostError> {
    let host = script_json(&json!({"baseUrl": base_url, "token": token, "agentId": agent_id}))?;
    Ok(format!(
        "window.__SMITH_THEME__={theme};window.__SMITH_HOST__={host};"
    ))
}

/// Validates a page origin and returns `scheme://host[:port]`. Only the product
/// origin `smithagents://app` and the dev origins `http://localhost:<port>` and
/// `http://127.0.0.1:<port>` are accepted, with no credentials, query or fragment.
/// [`page_boot`] applies the same check; a shell calls this to report a bad
/// configured origin at startup rather than at the first open.
pub fn validated_origin(origin: &str) -> Result<String, HostError> {
    let u = Url::parse(origin).map_err(|_| HostError::InvalidOrigin)?;
    let clean = u.username().is_empty()
        && u.password().is_none()
        && u.query().is_none()
        && u.fragment().is_none()
        && matches!(u.path(), "" | "/");
    let ok = clean
        && match (u.scheme(), u.host_str(), u.port()) {
            ("smithagents", Some("app"), None) => true,
            ("http", Some("localhost" | "127.0.0.1"), Some(p)) => p != 0,
            _ => false,
        };
    if !ok {
        return Err(HostError::InvalidOrigin);
    }
    let host = u.host_str().unwrap_or_default();
    Ok(match u.port() {
        Some(p) => format!("{}://{host}:{p}", u.scheme()),
        None => format!("{}://{host}", u.scheme()),
    })
}

/// Builds the boot data for one tab. `page_origin` must be `smithagents://app`
/// (product) or `http://localhost:<port>` / `http://127.0.0.1:<port>` (dev), else
/// `InvalidOrigin`. Port 0 and an empty token or agent id are rejected. The
/// returned script must be run through an embedder API at document start, because
/// the page's CSP blocks inline scripts.
pub fn page_boot(
    spec: &TabSpec,
    daemon_port: u16,
    token: &str,
    theme: &ThemeState,
    page_origin: &str,
) -> Result<PageBoot, HostError> {
    if daemon_port == 0 {
        return Err(HostError::InvalidPort);
    }
    let origin = validated_origin(page_origin)?;
    let TabKind::Chat { agent_id } = &spec.kind;
    if token.is_empty() || agent_id.is_empty() {
        return Err(HostError::EmptyBootValue);
    }
    let theme_json = script_json(theme)?;
    let base_url = format!("http://127.0.0.1:{daemon_port}");
    Ok(PageBoot {
        page_url: format!("{origin}/index.html"),
        host_script: script(&theme_json, &base_url, token, agent_id)?,
        redacted_script: script(&theme_json, &base_url, "REDACTED", agent_id)?,
    })
}

/// Script for live theme pushes: updates `window.__SMITH_THEME__` and notifies the
/// page if it registered `window.__smithagentsApplyTheme`.
pub fn theme_update_script(state: &ThemeState) -> String {
    // ThemeState is plain strings and numbers; serialization cannot fail.
    let json = script_json(state).unwrap_or_else(|_| "null".into());
    format!(
        "window.__SMITH_THEME__={json};if(window.__smithagentsApplyTheme){{window.__smithagentsApplyTheme(window.__SMITH_THEME__);}}"
    )
}

/// True only for same-document navigation of the app page (query and fragment may
/// differ). For a denied target, the host should run it through [`external_target`]
/// and then `open_external`, but only for user-initiated navigations: a
/// script-initiated redirect must be dropped, since it could carry page data out.
pub fn navigation_allowed(page_url: &str, target: &str) -> bool {
    let (Ok(page), Ok(t)) = (Url::parse(page_url), Url::parse(target)) else {
        return false;
    };
    page.host_str().is_some()
        && t.username().is_empty()
        && t.password().is_none()
        && t.scheme() == page.scheme()
        && t.host_str() == page.host_str()
        && t.port_or_known_default() == page.port_or_known_default()
        && t.path() == page.path()
}

/// A URL that passed [`external_target`]: `http(s)` with a host, no credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalUrl(String);

impl ExternalUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

const MAX_EXTERNAL_LEN: usize = 8 * 1024;

/// The normalized URL for targets that may go to the system browser: `http(s)`
/// with a host, no embedded credentials, at most 8 KiB.
pub fn external_target(target: &str) -> Option<ExternalUrl> {
    if target.len() > MAX_EXTERNAL_LEN {
        return None;
    }
    let u = Url::parse(target).ok()?;
    let ok = matches!(u.scheme(), "http" | "https")
        && u.host_str().is_some_and(|h| !h.is_empty())
        && u.username().is_empty()
        && u.password().is_none();
    ok.then(|| ExternalUrl(u.to_string()))
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostCall {
    Attach,
    Open { spec: TabSpec, boot: PageBoot },
    Close(TabId),
    Show(TabId),
    SetBounds(PixelRect),
    PushTheme(ThemeState),
    OpenExternal(ExternalUrl),
    Shutdown,
}

/// Records every call, for tests of code that drives a [`CenterTabHost`].
/// Recorded `PageBoot`s print with the token redacted.
#[derive(Debug, Default)]
pub struct RecordingHost {
    calls: Vec<HostCall>,
}

impl RecordingHost {
    pub fn calls(&self) -> &[HostCall] {
        &self.calls
    }

    pub fn opened_ids(&self) -> Vec<TabId> {
        self.calls
            .iter()
            .filter_map(|c| match c {
                HostCall::Open { spec, .. } => Some(spec.id),
                _ => None,
            })
            .collect()
    }
}

impl CenterTabHost for RecordingHost {
    fn attach(&mut self, _window: &dyn HasWindowHandle) -> Result<(), HostError> {
        self.calls.push(HostCall::Attach);
        Ok(())
    }
    fn open(&mut self, spec: TabSpec, boot: PageBoot) -> Result<(), HostError> {
        self.calls.push(HostCall::Open { spec, boot });
        Ok(())
    }
    fn close(&mut self, id: TabId) {
        self.calls.push(HostCall::Close(id));
    }
    fn show(&mut self, id: TabId) {
        self.calls.push(HostCall::Show(id));
    }
    fn set_bounds(&mut self, rect: PixelRect) {
        self.calls.push(HostCall::SetBounds(rect));
    }
    fn push_theme(&mut self, state: &ThemeState) {
        self.calls.push(HostCall::PushTheme(state.clone()));
    }
    fn open_external(&mut self, url: &ExternalUrl) {
        self.calls.push(HostCall::OpenExternal(url.clone()));
    }
    fn shutdown(&mut self) {
        self.calls.push(HostCall::Shutdown);
    }
}

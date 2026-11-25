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
    /// Hands a target (already filtered by [`external_target`]) to the system browser.
    fn open_external(&mut self, url: &str);
}

/// What the engine needs to boot a page. `host_script` carries the token: it must
/// be run at document start through an embedder API, never as an inline script.
#[derive(Clone, PartialEq)]
pub struct PageBoot {
    pub page_url: String,
    pub host_script: String,
    redacted_script: String,
}

impl PageBoot {
    /// The same script with the token replaced by `<redacted>`, safe to log.
    pub fn redacted_host_script(&self) -> String {
        self.redacted_script.clone()
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

/// Builds the boot data for one tab. A port of 0 is rejected.
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
    let TabKind::Chat { agent_id } = &spec.kind;
    let theme_json = script_json(theme)?;
    let base_url = format!("http://127.0.0.1:{daemon_port}");
    Ok(PageBoot {
        page_url: format!("{}/index.html", page_origin.trim_end_matches('/')),
        host_script: script(&theme_json, &base_url, token, agent_id)?,
        redacted_script: script(&theme_json, &base_url, "<redacted>", agent_id)?,
    })
}

/// True only for same-document navigation of the app page (query and fragment may differ).
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

/// The normalized URL for targets that may go to the system browser: `http(s)`
/// with a host and no embedded credentials.
pub fn external_target(target: &str) -> Option<String> {
    let u = Url::parse(target).ok()?;
    let ok = matches!(u.scheme(), "http" | "https")
        && u.host_str().is_some_and(|h| !h.is_empty())
        && u.username().is_empty()
        && u.password().is_none();
    ok.then(|| u.to_string())
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostCall {
    Attach,
    Open { spec: TabSpec, boot: PageBoot },
    Close(TabId),
    Show(TabId),
    SetBounds(PixelRect),
    PushTheme(ThemeState),
    OpenExternal(String),
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
    fn open_external(&mut self, url: &str) {
        self.calls.push(HostCall::OpenExternal(url.to_string()));
    }
}

//! The center tabs' lifecycle on a [`CenterTabHost`]: the shell model's tab
//! [`Effect`]s become host calls, each new page boots with the theme in effect
//! and the live daemon connection, a theme change is pushed once, and a new
//! daemon identity re-opens the tabs. GPUI-free, so it is tested against a
//! [`RecordingHost`](smithagents_center_host::RecordingHost).
//!
//! The daemon token reaches the host only inside a [`PageBoot`]; nothing here
//! formats it (a `PageBoot` prints redacted).
//!
//! [`PlaceholderHost`] is the host until a real web engine lands: it draws
//! nothing and only logs. [`SharedHost`] lets a test keep a handle on the host
//! it injects.

use std::cell::RefCell;
use std::rc::Rc;

use raw_window_handle::HasWindowHandle;
use smithagents_center_host::{
    external_target, page_boot, CenterTabHost, ExternalUrl, HostError, PageBoot, PixelRect, TabId,
    TabKind, TabSpec,
};
use smithagents_core::agent::Agent;
use smithagents_core::ids::AgentId;
use smithagents_theme::state::ThemeState;

use crate::model::Effect;

/// The product's page origin: the custom scheme that serves the built web
/// page (`web/dist`).
pub const DEFAULT_PAGE_ORIGIN: &str = "smithagents://app";

/// Overrides the page origin in a debug build; in development the Vite dev
/// server, `http://localhost:5173`.
pub const PAGE_ORIGIN_ENV: &str = "SMITHAGENTS_WEB_ORIGIN";

/// The page origin for a value of [`PAGE_ORIGIN_ENV`].
///
/// - `dev_build` (the app passes `cfg!(debug_assertions)`): the trimmed
///   value, or [`DEFAULT_PAGE_ORIGIN`] when it is unset or blank. It is not
///   validated here: an invalid origin is reported by the shell and fails
///   every open.
/// - A release build always loads [`DEFAULT_PAGE_ORIGIN`]; a set value is
///   ignored with one warning. Honoring it would hand the token-bearing boot
///   script to whatever serves that origin.
pub fn page_origin_from(value: Option<&str>, dev_build: bool) -> String {
    let value = value.map(str::trim).filter(|origin| !origin.is_empty());
    match value {
        Some(origin) if dev_build => origin.to_owned(),
        Some(_) => {
            tracing::warn!(
                "{PAGE_ORIGIN_ENV} is ignored in a release build; chat pages load from \
                 {DEFAULT_PAGE_ORIGIN}"
            );
            DEFAULT_PAGE_ORIGIN.to_owned()
        }
        None => DEFAULT_PAGE_ORIGIN.to_owned(),
    }
}

/// Why a tab did not open.
#[derive(Debug, thiserror::Error)]
pub enum TabError {
    /// No daemon port and token: the page could not reach the daemon.
    #[error("not connected to the daemon")]
    NotConnected,
    /// The effect names an agent the model does not have.
    #[error("the agent is gone")]
    UnknownAgent,
    /// The boot data could not be built or the host refused the tab.
    #[error(transparent)]
    Host(#[from] HostError),
}

/// The daemon a tab's page was booted for. Compared, never printed.
#[derive(Clone, PartialEq, Eq)]
struct DaemonIdentity {
    port: u16,
    token: String,
}

impl DaemonIdentity {
    fn is(&self, port: u16, token: &str) -> bool {
        self.port == port && self.token == token
    }
}

impl std::fmt::Debug for DaemonIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonIdentity")
            .field("port", &self.port)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// One tab the host has open.
#[derive(Debug)]
struct OpenTab {
    agent: AgentId,
    id: TabId,
    daemon: DaemonIdentity,
}

/// Carries the shell model's tab effects out on a host. See the module docs.
///
/// Invariants: a tab is recorded as open only after the host accepted it;
/// [`TabId`]s are allocated from a counter and never reused in a session (a
/// closed or re-opened tab gets a new one, so a host that tears views down
/// asynchronously never sees an id come back); at most one tab is shown.
pub struct TabManager<H: CenterTabHost> {
    host: H,
    page_origin: String,
    /// The theme new pages boot with: the one in effect.
    theme: ThemeState,
    /// The center content area's last rectangle.
    bounds: Option<PixelRect>,
    /// The next [`TabId`].
    next_id: u64,
    /// In the order they were opened (a re-open keeps the place).
    open: Vec<OpenTab>,
    shown: Option<AgentId>,
    /// The host was shut down: it gets no further call.
    shut_down: bool,
}

impl<H: CenterTabHost> TabManager<H> {
    /// `page_origin` is where pages load from (see [`page_origin_from`]);
    /// `theme` is the theme in effect, which the first pages boot with.
    pub fn new(host: H, page_origin: String, theme: ThemeState) -> Self {
        Self {
            host,
            page_origin,
            theme,
            bounds: None,
            next_id: 1,
            open: Vec::new(),
            shown: None,
            shut_down: false,
        }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    /// The theme pages boot with.
    pub fn theme(&self) -> &ThemeState {
        &self.theme
    }

    pub fn page_origin(&self) -> &str {
        &self.page_origin
    }

    /// The host's id for `agent`'s tab, while it is open.
    pub fn tab_id(&self, agent: AgentId) -> Option<TabId> {
        self.find(agent).map(|tab| tab.id)
    }

    /// The agents whose tabs are open, in the order they were opened.
    pub fn open_agents(&self) -> Vec<AgentId> {
        self.open.iter().map(|tab| tab.agent).collect()
    }

    /// The agent whose tab the host shows.
    pub fn shown(&self) -> Option<AgentId> {
        self.shown
    }

    /// Shuts the host down (the app quits or the window closes), once: a
    /// second call does nothing. The open tabs are forgotten.
    pub fn shutdown(&mut self) {
        if std::mem::replace(&mut self.shut_down, true) {
            return;
        }
        tracing::debug!(tabs = self.open.len(), "shutting the tab host down");
        self.open.clear();
        self.shown = None;
        self.host.shutdown();
    }

    /// Hands the window to the host (once, when the window exists).
    pub fn attach(&mut self, window: &dyn HasWindowHandle) -> Result<(), TabError> {
        self.host.attach(window)?;
        Ok(())
    }

    /// Carries out one model effect.
    ///
    /// - `OpenCenterTab`: needs `agent` (the model's record of that agent)
    ///   and `conn` (the daemon's port and token), else `UnknownAgent` or
    ///   `NotConnected` with no host call. Boots the page with the theme in
    ///   effect, then opens, positions (when a rectangle is known) and shows
    ///   the tab. A tab already open is only shown. On an error nothing is
    ///   recorded: the caller must drop the tab from its model.
    /// - `FocusCenterTab`: shows the tab if it is open (else nothing).
    /// - `CloseCenterTab`: closes the tab if it is open and forgets it. If
    ///   another tab becomes active, the model's next `FocusCenterTab` shows
    ///   it.
    /// - `UpdateTabAccent`: nothing in v1. The page reads the agent's color
    ///   from the daemon itself.
    pub fn handle(
        &mut self,
        effect: Effect,
        agent: Option<&Agent>,
        conn: Option<(u16, &str)>,
    ) -> Result<(), TabError> {
        match effect {
            Effect::OpenCenterTab(id) => self.open_and_show(id, agent, conn),
            Effect::FocusCenterTab(id) => {
                self.show(id);
                Ok(())
            }
            Effect::CloseCenterTab(id) => {
                self.close(id);
                Ok(())
            }
            Effect::UpdateTabAccent(_) => Ok(()),
        }
    }

    /// Stores the theme in effect (later pages boot with it) and pushes it to
    /// the host once, which applies it to every open tab. With no tab open,
    /// or when `state` is the theme already in effect, the host is not
    /// called.
    pub fn set_theme(&mut self, state: ThemeState) {
        if state == self.theme {
            return;
        }
        if !self.open.is_empty() {
            tracing::debug!(theme = %state.theme, tabs = self.open.len(), "pushing the theme to the tabs");
            self.host.push_theme(&state);
        }
        self.theme = state;
    }

    /// The center content area moved or resized: positions the open tabs.
    /// The rectangle is kept for tabs opened later.
    pub fn on_bounds(&mut self, rect: PixelRect) {
        self.bounds = Some(rect);
        if !self.open.is_empty() {
            self.host.set_bounds(rect);
        }
    }

    /// The daemon connection changed. Every open tab whose page was booted
    /// for another daemon port or token is opened again with a fresh
    /// [`PageBoot`] (the old page holds a stale token), in place and with a
    /// new [`TabId`], and only then is its old view closed, so the old page
    /// stays on screen until the new one exists; the tab that was shown is
    /// shown again. `None` (not connected) changes nothing: a page shows its
    /// own lost-connection state.
    ///
    /// Returns the tabs that could not be re-opened (their agent is gone, or
    /// the host refused): they are closed on the host and forgotten, and the
    /// caller must drop them from its model.
    pub fn on_connection(
        &mut self,
        conn: Option<(u16, &str)>,
        agents: &dyn Fn(AgentId) -> Option<Agent>,
    ) -> Vec<(AgentId, TabError)> {
        let Some((port, token)) = conn else {
            return Vec::new();
        };
        if self.open.iter().all(|tab| tab.daemon.is(port, token)) {
            return Vec::new();
        }
        tracing::debug!(port, "the daemon changed; re-opening the chat tabs");
        let shown = self.shown.take();
        let mut failed = Vec::new();
        let mut kept = Vec::with_capacity(self.open.len());
        for tab in std::mem::take(&mut self.open) {
            if tab.daemon.is(port, token) {
                kept.push(tab);
                continue;
            }
            let reopened = match agents(tab.agent) {
                Some(agent) => self.open_tab(&agent, port, token),
                None => Err(TabError::UnknownAgent),
            };
            self.host.close(tab.id);
            match reopened {
                Ok(new) => kept.push(new),
                Err(error) => failed.push((tab.agent, error)),
            }
        }
        self.open = kept;
        if let Some(agent) = shown {
            self.show(agent);
        }
        failed
    }

    /// Hands `url` to the system browser through the host if it is a web
    /// target ([`external_target`]); returns whether it did.
    pub fn open_external(&mut self, url: &str) -> bool {
        match external_target(url) {
            Some(target) => {
                self.host.open_external(&target);
                true
            }
            None => {
                tracing::debug!("an external link was dropped: not an http(s) target");
                false
            }
        }
    }

    fn find(&self, agent: AgentId) -> Option<&OpenTab> {
        self.open.iter().find(|tab| tab.agent == agent)
    }

    fn open_and_show(
        &mut self,
        id: AgentId,
        agent: Option<&Agent>,
        conn: Option<(u16, &str)>,
    ) -> Result<(), TabError> {
        if self.find(id).is_some() {
            self.show(id);
            return Ok(());
        }
        let (port, token) = conn.ok_or(TabError::NotConnected)?;
        let agent = agent
            .filter(|agent| agent.id == id)
            .ok_or(TabError::UnknownAgent)?;
        let tab = self.open_tab(agent, port, token)?;
        self.open.push(tab);
        self.show(id);
        Ok(())
    }

    /// Boots and opens a page for `agent`, positioned when a rectangle is
    /// known; not shown and not recorded.
    fn open_tab(&mut self, agent: &Agent, port: u16, token: &str) -> Result<OpenTab, TabError> {
        if self.shut_down {
            return Err(HostError::Engine("the web engine has shut down".into()).into());
        }
        let id = TabId(self.next_id);
        self.next_id += 1;
        let spec = TabSpec {
            id,
            kind: TabKind::Chat {
                agent_id: agent.id.to_string(),
            },
            title: agent.name.clone(),
        };
        let boot = page_boot(&spec, port, token, &self.theme, &self.page_origin)?;
        tracing::debug!(tab = id.0, agent = %agent.id, page_url = boot.page_url(), "opening a chat tab");
        self.host.open(spec, boot)?;
        if let Some(rect) = self.bounds {
            self.host.set_bounds(rect);
        }
        Ok(OpenTab {
            agent: agent.id,
            id,
            daemon: DaemonIdentity {
                port,
                token: token.to_owned(),
            },
        })
    }

    fn show(&mut self, agent: AgentId) {
        if let Some(id) = self.tab_id(agent) {
            self.host.show(id);
            self.shown = Some(agent);
        }
    }

    fn close(&mut self, agent: AgentId) {
        let Some(at) = self.open.iter().position(|tab| tab.agent == agent) else {
            return;
        };
        let tab = self.open.remove(at);
        self.host.close(tab.id);
        if self.shown == Some(agent) {
            self.shown = None;
        }
    }
}

/// The host until a real web engine lands: it draws nothing and only logs
/// (at debug), never the token. `open_external` does not launch a browser.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlaceholderHost;

impl CenterTabHost for PlaceholderHost {
    fn attach(&mut self, window: &dyn HasWindowHandle) -> Result<(), HostError> {
        tracing::debug!(
            window_handle = window.window_handle().is_ok(),
            "placeholder host: attach (nothing to embed)"
        );
        Ok(())
    }

    fn open(&mut self, spec: TabSpec, boot: PageBoot) -> Result<(), HostError> {
        let TabKind::Chat { agent_id } = &spec.kind;
        tracing::debug!(
            tab = spec.id.0,
            agent = %agent_id,
            title = %spec.title,
            page_url = boot.page_url(),
            host_script = boot.redacted_host_script(),
            "placeholder host: open"
        );
        Ok(())
    }

    fn close(&mut self, id: TabId) {
        tracing::debug!(tab = id.0, "placeholder host: close");
    }

    fn show(&mut self, id: TabId) {
        tracing::debug!(tab = id.0, "placeholder host: show");
    }

    fn set_bounds(&mut self, rect: PixelRect) {
        tracing::debug!(?rect, "placeholder host: set_bounds");
    }

    fn push_theme(&mut self, state: &ThemeState) {
        tracing::debug!(?state, "placeholder host: push_theme");
    }

    fn open_external(&mut self, url: &ExternalUrl) {
        tracing::debug!(
            url = url.as_str(),
            "placeholder host: open_external (no browser is launched)"
        );
    }

    fn shutdown(&mut self) {
        tracing::debug!("placeholder host: shutdown");
    }
}

/// A host shared through an `Rc<RefCell<_>>`, so whoever injects it (a test,
/// say) keeps a handle to inspect it while the shell owns the adapter.
pub struct SharedHost<H>(Rc<RefCell<H>>);

impl<H> SharedHost<H> {
    /// Wraps `host`; [`SharedHost::handle`] gives the shared handle.
    pub fn new(host: H) -> Self {
        Self(Rc::new(RefCell::new(host)))
    }

    pub fn handle(&self) -> Rc<RefCell<H>> {
        self.0.clone()
    }
}

impl<H: CenterTabHost> CenterTabHost for SharedHost<H> {
    fn attach(&mut self, window: &dyn HasWindowHandle) -> Result<(), HostError> {
        self.0.borrow_mut().attach(window)
    }
    fn open(&mut self, spec: TabSpec, boot: PageBoot) -> Result<(), HostError> {
        self.0.borrow_mut().open(spec, boot)
    }
    fn close(&mut self, id: TabId) {
        self.0.borrow_mut().close(id)
    }
    fn show(&mut self, id: TabId) {
        self.0.borrow_mut().show(id)
    }
    fn set_bounds(&mut self, rect: PixelRect) {
        self.0.borrow_mut().set_bounds(rect)
    }
    fn push_theme(&mut self, state: &ThemeState) {
        self.0.borrow_mut().push_theme(state)
    }
    fn open_external(&mut self, url: &ExternalUrl) {
        self.0.borrow_mut().open_external(url)
    }
    fn shutdown(&mut self) {
        self.0.borrow_mut().shutdown()
    }
}

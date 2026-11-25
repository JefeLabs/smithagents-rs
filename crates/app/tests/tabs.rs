//! The tab lifecycle ([`TabManager`]) against a [`RecordingHost`]: the model's
//! effects become host calls, boots carry the current theme and the live
//! connection, a theme change is one push, and a new daemon identity re-opens
//! the tabs. No GPUI.

mod common;

use std::sync::{Arc, Mutex};

use common::ui::{agent, theme_state};
use raw_window_handle::{HandleError, HasWindowHandle, WindowHandle};
use smithagents_app::model::Effect;
use smithagents_app::tabs::{
    page_origin_from, PlaceholderHost, TabError, TabManager, DEFAULT_PAGE_ORIGIN,
};
use smithagents_app::view_models::tab_error_text;
use smithagents_center_host::{
    CenterTabHost, ExternalUrl, HostCall, HostError, PageBoot, PixelRect, RecordingHost, TabId,
    TabKind, TabSpec,
};
use smithagents_core::agent::Agent;
use smithagents_core::ids::AgentId;
use smithagents_theme::state::ThemeState;
use smithagents_theme::tokens::Appearance;

const PORT: u16 = 4123;
const TOKEN: &str = "tok-SECRET-first-4123";
const NEW_PORT: u16 = 4999;
const NEW_TOKEN: &str = "tok-SECRET-second-4999";
const ORIGIN: &str = "smithagents://app";

fn manager() -> TabManager<RecordingHost> {
    TabManager::new(
        RecordingHost::default(),
        ORIGIN.into(),
        theme_state(Appearance::Dark),
    )
}

fn conn() -> Option<(u16, &'static str)> {
    Some((PORT, TOKEN))
}

fn rect(w: f64) -> PixelRect {
    PixelRect {
        x: 10.0,
        y: 20.0,
        w,
        h: 300.0,
        scale: 2.0,
    }
}

fn open(m: &mut TabManager<RecordingHost>, agent: &Agent) {
    m.handle(Effect::OpenCenterTab(agent.id), Some(agent), conn())
        .expect("the tab opens");
}

fn calls(m: &TabManager<RecordingHost>) -> Vec<HostCall> {
    m.host().calls().to_vec()
}

/// The calls made since `from` calls were recorded.
fn calls_since(m: &TabManager<RecordingHost>, from: usize) -> Vec<HostCall> {
    m.host().calls()[from..].to_vec()
}

fn opened(call: &HostCall) -> (&TabSpec, &PageBoot) {
    match call {
        HostCall::Open { spec, boot } => (spec, boot),
        other => panic!("expected an open, got {other:?}"),
    }
}

fn json(state: &ThemeState) -> String {
    serde_json::to_string(state).unwrap()
}

fn lookup(agents: &[Agent]) -> impl Fn(AgentId) -> Option<Agent> + '_ {
    move |id| agents.iter().find(|a| a.id == id).cloned()
}

#[test]
fn opening_a_tab_opens_then_shows_it_with_the_current_theme_and_connection() {
    let mercury = agent("Mercury");
    let light = theme_state(Appearance::Light);
    let mut m = manager();
    m.set_theme(light.clone());
    assert!(calls(&m).is_empty(), "no tab yet: nothing to push");

    open(&mut m, &mercury);

    let calls = calls(&m);
    assert_eq!(calls.len(), 2, "{calls:?}");
    let (spec, boot) = opened(&calls[0]);
    let id = m.tab_id(mercury.id).expect("the tab is recorded as open");
    assert_eq!(spec.id, id);
    assert_eq!(
        spec.kind,
        TabKind::Chat {
            agent_id: mercury.id.to_string()
        }
    );
    assert_eq!(spec.title, "Mercury");
    assert_eq!(calls[1], HostCall::Show(id));
    assert_eq!(m.shown(), Some(mercury.id));

    assert_eq!(boot.page_url(), "smithagents://app/index.html");
    assert!(!boot.page_url().contains(TOKEN));
    assert!(
        boot.host_script().contains(&json(&light)),
        "the boot carries the light theme set before the open: {}",
        boot.redacted_host_script()
    );
    assert!(!boot
        .host_script()
        .contains(&json(&theme_state(Appearance::Dark))));
    assert!(boot.host_script().contains(TOKEN));
    assert!(boot.host_script().contains(&mercury.id.to_string()));
    assert!(boot
        .redacted_host_script()
        .contains(&format!("http://127.0.0.1:{PORT}")));
    assert!(!format!("{calls:?}").contains(TOKEN), "Debug is redacted");
}

#[test]
fn bounds_known_before_the_first_open_are_applied_right_after_it() {
    let mercury = agent("Mercury");
    let mut m = manager();
    m.on_bounds(rect(800.0));
    assert!(calls(&m).is_empty(), "no tab to position yet");

    open(&mut m, &mercury);
    let id = m.tab_id(mercury.id).unwrap();
    let calls = calls(&m);
    assert_eq!(calls.len(), 3, "{calls:?}");
    opened(&calls[0]);
    assert_eq!(calls[1], HostCall::SetBounds(rect(800.0)));
    assert_eq!(calls[2], HostCall::Show(id));

    // With a tab open, a new rectangle goes straight to the host.
    m.on_bounds(rect(900.0));
    assert_eq!(
        m.host().calls().last(),
        Some(&HostCall::SetBounds(rect(900.0)))
    );
}

#[test]
fn a_theme_change_is_one_push_however_many_tabs_are_open() {
    let agents = [agent("Mercury"), agent("Venus"), agent("Earth")];
    let mut m = manager();
    for a in &agents {
        open(&mut m, a);
    }
    let before = calls(&m).len();

    let light = theme_state(Appearance::Light);
    m.set_theme(light.clone());

    assert_eq!(calls_since(&m, before), vec![HostCall::PushTheme(light)]);
}

#[test]
fn a_theme_change_with_no_tab_is_kept_for_the_next_open() {
    let mercury = agent("Mercury");
    let light = theme_state(Appearance::Light);
    let mut m = manager();
    m.set_theme(light.clone());
    assert!(calls(&m).is_empty());

    open(&mut m, &mercury);
    let (_, boot) = opened(&m.host().calls()[0]);
    assert!(boot.host_script().contains(&json(&light)));
    assert_eq!(m.theme(), &light);
}

#[test]
fn opening_without_a_connection_fails_and_calls_no_host() {
    let mercury = agent("Mercury");
    let mut m = manager();
    let err = m
        .handle(Effect::OpenCenterTab(mercury.id), Some(&mercury), None)
        .unwrap_err();
    assert!(matches!(err, TabError::NotConnected), "{err:?}");
    assert_eq!(err.to_string(), "not connected to the daemon");
    assert!(calls(&m).is_empty());
    assert_eq!(m.tab_id(mercury.id), None);
    assert_eq!(m.shown(), None);
}

#[test]
fn opening_an_unknown_agent_fails_and_calls_no_host() {
    let (mercury, venus) = (agent("Mercury"), agent("Venus"));
    let mut m = manager();
    let err = m
        .handle(Effect::OpenCenterTab(mercury.id), None, conn())
        .unwrap_err();
    assert!(matches!(err, TabError::UnknownAgent), "{err:?}");
    // An agent that is not the one asked for is no agent at all.
    let err = m
        .handle(Effect::OpenCenterTab(mercury.id), Some(&venus), conn())
        .unwrap_err();
    assert!(matches!(err, TabError::UnknownAgent), "{err:?}");
    assert!(calls(&m).is_empty());
}

#[test]
fn an_empty_token_is_a_host_error_with_no_host_call() {
    let mercury = agent("Mercury");
    let mut m = manager();
    let err = m
        .handle(
            Effect::OpenCenterTab(mercury.id),
            Some(&mercury),
            Some((PORT, "")),
        )
        .unwrap_err();
    assert!(
        matches!(err, TabError::Host(HostError::EmptyBootValue)),
        "{err:?}"
    );
    assert!(calls(&m).is_empty());
}

#[test]
fn focus_shows_an_open_tab_and_ignores_one_that_is_not_open() {
    let (mercury, venus) = (agent("Mercury"), agent("Venus"));
    let mut m = manager();
    open(&mut m, &mercury);
    open(&mut m, &venus);
    let before = calls(&m).len();

    m.handle(Effect::FocusCenterTab(mercury.id), None, None)
        .unwrap();
    assert_eq!(
        calls_since(&m, before),
        vec![HostCall::Show(m.tab_id(mercury.id).unwrap())]
    );
    assert_eq!(m.shown(), Some(mercury.id));

    let earth = agent("Earth");
    m.handle(Effect::FocusCenterTab(earth.id), None, None)
        .unwrap();
    assert_eq!(calls(&m).len(), before + 1, "a tab never opened: no call");
    assert_eq!(m.shown(), Some(mercury.id));
}

#[test]
fn closing_forgets_the_tab_and_a_reopen_gets_a_new_id() {
    let mercury = agent("Mercury");
    let mut m = manager();
    open(&mut m, &mercury);
    let first = m.tab_id(mercury.id).unwrap();

    m.handle(Effect::CloseCenterTab(mercury.id), None, None)
        .unwrap();
    assert_eq!(m.host().calls().last(), Some(&HostCall::Close(first)));
    assert_eq!(m.tab_id(mercury.id), None);
    assert_eq!(m.shown(), None, "the last tab closed: none is visible");
    assert!(m.open_agents().is_empty());

    // Closing again, or a tab never opened, calls nothing.
    let before = calls(&m).len();
    m.handle(Effect::CloseCenterTab(mercury.id), None, None)
        .unwrap();
    assert_eq!(calls(&m).len(), before);

    open(&mut m, &mercury);
    let second = m.tab_id(mercury.id).unwrap();
    assert_ne!(second, first, "a TabId is never reused");
}

#[test]
fn ids_are_distinct_per_tab() {
    let agents = [agent("Mercury"), agent("Venus"), agent("Earth")];
    let mut m = manager();
    for a in &agents {
        open(&mut m, a);
    }
    let mut ids: Vec<TabId> = agents.iter().map(|a| m.tab_id(a.id).unwrap()).collect();
    ids.dedup();
    assert_eq!(ids.len(), 3);
    assert_eq!(m.host().opened_ids(), ids);
    assert_eq!(
        m.open_agents(),
        agents.iter().map(|a| a.id).collect::<Vec<_>>()
    );
}

#[test]
fn an_accent_update_calls_no_host() {
    let mercury = agent("Mercury");
    let mut m = manager();
    open(&mut m, &mercury);
    let before = calls(&m).len();
    m.handle(Effect::UpdateTabAccent(mercury.id), Some(&mercury), conn())
        .unwrap();
    assert_eq!(calls(&m).len(), before);
}

#[test]
fn an_already_open_tab_is_shown_not_opened_twice() {
    let mercury = agent("Mercury");
    let mut m = manager();
    open(&mut m, &mercury);
    let id = m.tab_id(mercury.id).unwrap();
    let before = calls(&m).len();
    open(&mut m, &mercury);
    assert_eq!(calls_since(&m, before), vec![HostCall::Show(id)]);
}

#[test]
fn a_new_token_reopens_every_tab_with_it_and_reshows_the_shown_one() {
    let agents = vec![agent("Mercury"), agent("Venus"), agent("Earth")];
    let mut m = manager();
    m.on_bounds(rect(800.0));
    for a in &agents {
        open(&mut m, a);
    }
    m.handle(Effect::FocusCenterTab(agents[1].id), None, None)
        .unwrap();
    let old: Vec<TabId> = agents.iter().map(|a| m.tab_id(a.id).unwrap()).collect();
    let before = calls(&m).len();

    let failed = m.on_connection(Some((NEW_PORT, NEW_TOKEN)), &lookup(&agents));
    assert!(failed.is_empty(), "{failed:?}");

    let new: Vec<TabId> = agents.iter().map(|a| m.tab_id(a.id).unwrap()).collect();
    let calls = calls_since(&m, before);
    assert_eq!(calls.len(), 10, "{calls:?}");
    // Per tab: the new view first (hidden, positioned), then the old one
    // closes, so the old page stays on screen until the new one exists.
    for (ix, a) in agents.iter().enumerate() {
        let (spec, boot) = opened(&calls[ix * 3]);
        assert_eq!(spec.id, new[ix]);
        assert_ne!(new[ix], old[ix], "a re-opened tab gets a fresh id");
        assert_eq!(spec.title, a.name);
        assert!(boot.host_script().contains(NEW_TOKEN));
        assert!(
            !boot.host_script().contains(TOKEN),
            "the stale token is gone"
        );
        assert!(boot
            .redacted_host_script()
            .contains(&format!("http://127.0.0.1:{NEW_PORT}")));
        assert_eq!(calls[ix * 3 + 1], HostCall::SetBounds(rect(800.0)));
        assert_eq!(calls[ix * 3 + 2], HostCall::Close(old[ix]));
    }
    assert_eq!(calls[9], HostCall::Show(new[1]), "Venus is shown again");
    assert_eq!(m.shown(), Some(agents[1].id));
    assert_eq!(
        m.open_agents(),
        agents.iter().map(|a| a.id).collect::<Vec<_>>(),
        "the order is kept"
    );
    assert!(!format!("{calls:?}").contains(NEW_TOKEN));

    // The same identity again: nothing to do.
    let after = m.host().calls().len();
    assert!(m
        .on_connection(Some((NEW_PORT, NEW_TOKEN)), &lookup(&agents))
        .is_empty());
    assert_eq!(m.host().calls().len(), after);
}

#[test]
fn a_new_port_alone_reopens_the_tabs() {
    let mercury = agent("Mercury");
    let agents = vec![mercury.clone()];
    let mut m = manager();
    open(&mut m, &mercury);
    let old = m.tab_id(mercury.id).unwrap();
    let before = calls(&m).len();

    assert!(m
        .on_connection(Some((NEW_PORT, TOKEN)), &lookup(&agents))
        .is_empty());
    let new = m.tab_id(mercury.id).unwrap();
    let calls = calls_since(&m, before);
    assert_eq!(calls.len(), 3, "{calls:?}");
    let (_, boot) = opened(&calls[0]);
    assert!(boot
        .redacted_host_script()
        .contains(&format!("http://127.0.0.1:{NEW_PORT}")));
    assert_eq!(calls[1], HostCall::Close(old), "after the new view");
    assert_eq!(calls[2], HostCall::Show(new));
}

#[test]
fn the_same_connection_or_none_changes_nothing() {
    let mercury = agent("Mercury");
    let agents = vec![mercury.clone()];
    let mut m = manager();
    open(&mut m, &mercury);
    let id = m.tab_id(mercury.id).unwrap();
    let before = calls(&m).len();

    assert!(m.on_connection(conn(), &lookup(&agents)).is_empty());
    assert!(m.on_connection(None, &lookup(&agents)).is_empty());
    assert_eq!(calls(&m).len(), before);
    assert_eq!(m.tab_id(mercury.id), Some(id));
}

#[test]
fn a_new_connection_with_no_tab_calls_nothing() {
    let mut m = manager();
    assert!(m
        .on_connection(Some((NEW_PORT, NEW_TOKEN)), &|_| None)
        .is_empty());
    assert!(calls(&m).is_empty());
}

#[test]
fn a_tab_whose_agent_is_gone_is_closed_and_reported_on_reconnect() {
    let (mercury, venus) = (agent("Mercury"), agent("Venus"));
    let mut m = manager();
    open(&mut m, &mercury);
    open(&mut m, &venus);
    let old_venus = m.tab_id(venus.id).unwrap();
    let before = calls(&m).len();

    // Only Mercury is still known.
    let known = vec![mercury.clone()];
    let failed = m.on_connection(Some((NEW_PORT, NEW_TOKEN)), &lookup(&known));
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].0, venus.id);
    assert!(matches!(failed[0].1, TabError::UnknownAgent));
    assert_eq!(m.tab_id(venus.id), None);
    assert_eq!(m.open_agents(), vec![mercury.id]);
    let calls = calls_since(&m, before);
    assert!(calls.contains(&HostCall::Close(old_venus)));
    assert_eq!(
        calls
            .iter()
            .filter(|c| matches!(c, HostCall::Open { .. }))
            .count(),
        1,
        "only Mercury is re-opened: {calls:?}"
    );
    // Venus was shown and is gone: nothing is re-shown in its place (the
    // shell's model picks the next tab).
    assert_eq!(m.shown(), None);
}

#[test]
fn open_external_forwards_only_web_targets() {
    let mut m = manager();
    assert!(m.open_external("https://example.com/docs?q=1"));
    assert!(!m.open_external("javascript:alert(1)"));
    assert!(!m.open_external("file:///etc/passwd"));
    assert!(!m.open_external("data:text/html,hi"));
    let calls = calls(&m);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(matches!(
        &calls[0],
        HostCall::OpenExternal(url) if url.as_str() == "https://example.com/docs?q=1"
    ));
}

/// A host whose `open` fails; everything else is recorded.
#[derive(Default)]
struct FailingHost(RecordingHost);

impl CenterTabHost for FailingHost {
    fn attach(&mut self, window: &dyn HasWindowHandle) -> Result<(), HostError> {
        self.0.attach(window)
    }
    fn open(&mut self, _: TabSpec, _: PageBoot) -> Result<(), HostError> {
        Err(HostError::Engine("the engine is down".into()))
    }
    fn close(&mut self, id: TabId) {
        self.0.close(id)
    }
    fn show(&mut self, id: TabId) {
        self.0.show(id)
    }
    fn set_bounds(&mut self, rect: PixelRect) {
        self.0.set_bounds(rect)
    }
    fn push_theme(&mut self, state: &ThemeState) {
        self.0.push_theme(state)
    }
    fn open_external(&mut self, url: &ExternalUrl) {
        self.0.open_external(url)
    }
}

#[test]
fn a_host_open_error_is_returned_and_the_tab_is_not_recorded() {
    let mercury = agent("Mercury");
    let mut m = TabManager::new(
        FailingHost::default(),
        ORIGIN.into(),
        theme_state(Appearance::Dark),
    );
    m.on_bounds(rect(800.0));
    let err = m
        .handle(Effect::OpenCenterTab(mercury.id), Some(&mercury), conn())
        .unwrap_err();
    assert!(
        matches!(&err, TabError::Host(HostError::Engine(text)) if text == "the engine is down"),
        "{err:?}"
    );
    assert_eq!(m.tab_id(mercury.id), None);
    assert_eq!(m.shown(), None);
    // No show, no bounds and no theme push for a tab that never opened.
    m.set_theme(theme_state(Appearance::Light));
    assert!(m.host().0.calls().is_empty(), "{:?}", m.host().0.calls());
}

#[test]
fn an_invalid_page_origin_fails_every_open() {
    let mercury = agent("Mercury");
    let mut m = TabManager::new(
        RecordingHost::default(),
        "file:///tmp/web".into(),
        theme_state(Appearance::Dark),
    );
    let err = m
        .handle(Effect::OpenCenterTab(mercury.id), Some(&mercury), conn())
        .unwrap_err();
    assert!(
        matches!(err, TabError::Host(HostError::InvalidOrigin)),
        "{err:?}"
    );
    assert!(calls(&m).is_empty());
}

#[test]
fn the_dev_origin_is_used_for_the_page_url() {
    let mercury = agent("Mercury");
    let mut m = TabManager::new(
        RecordingHost::default(),
        "http://localhost:5173".into(),
        theme_state(Appearance::Dark),
    );
    open(&mut m, &mercury);
    let (_, boot) = opened(&m.host().calls()[0]);
    assert_eq!(boot.page_url(), "http://localhost:5173/index.html");
}

#[test]
fn a_debug_build_takes_the_page_origin_from_the_environment_value() {
    assert_eq!(DEFAULT_PAGE_ORIGIN, "smithagents://app");
    let dev = |value| page_origin_from(value, true);
    assert_eq!(dev(None), DEFAULT_PAGE_ORIGIN);
    assert_eq!(dev(Some("")), DEFAULT_PAGE_ORIGIN);
    assert_eq!(dev(Some("  ")), DEFAULT_PAGE_ORIGIN);
    assert_eq!(
        dev(Some(" http://localhost:5173 ")),
        "http://localhost:5173"
    );
    // An invalid value is kept as given: opening reports it.
    assert_eq!(dev(Some("file:///x")), "file:///x");
}

#[test]
fn a_release_build_always_loads_the_product_origin_and_warns_once_when_set() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        assert_eq!(page_origin_from(None, false), DEFAULT_PAGE_ORIGIN);
        assert_eq!(page_origin_from(Some("  "), false), DEFAULT_PAGE_ORIGIN);
        assert_eq!(
            page_origin_from(Some("http://localhost:5173"), false),
            DEFAULT_PAGE_ORIGIN
        );
    });
    let log = captured.text();
    let warnings: Vec<&str> = log.lines().filter(|line| line.contains("WARN")).collect();
    assert_eq!(warnings.len(), 1, "{log}");
    assert!(warnings[0].contains("SMITHAGENTS_WEB_ORIGIN"), "{log}");
}

struct NoWindow;

impl HasWindowHandle for NoWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Err(HandleError::NotSupported)
    }
}

#[test]
fn attach_goes_to_the_host() {
    let mut m = manager();
    m.attach(&NoWindow).unwrap();
    assert_eq!(calls(&m), vec![HostCall::Attach]);
}

/// A host that refuses every page with an error naming what it was given
/// (the page and the redacted script, never the token).
struct RefusingHost;

impl CenterTabHost for RefusingHost {
    fn attach(&mut self, _: &dyn HasWindowHandle) -> Result<(), HostError> {
        Ok(())
    }
    fn open(&mut self, spec: TabSpec, boot: PageBoot) -> Result<(), HostError> {
        Err(HostError::Engine(format!(
            "cannot load {} for {:?}: {boot:?}",
            boot.page_url(),
            spec.title
        )))
    }
    fn close(&mut self, _: TabId) {}
    fn show(&mut self, _: TabId) {}
    fn set_bounds(&mut self, _: PixelRect) {}
    fn push_theme(&mut self, _: &ThemeState) {}
    fn open_external(&mut self, _: &ExternalUrl) {}
}

#[test]
fn errors_never_carry_the_token() {
    let mercury = agent("Mercury");
    let theme = theme_state(Appearance::Dark);
    let open = |m: &mut TabManager<RecordingHost>, agent: Option<&Agent>, conn| {
        m.handle(Effect::OpenCenterTab(mercury.id), agent, conn)
            .expect_err("the open fails")
    };
    let given = Some((PORT, TOKEN));
    let mut errors = vec![
        // The open was given the token each time.
        open(&mut manager(), None, given),
        open(&mut manager(), Some(&agent("Venus")), given),
        open(&mut manager(), Some(&mercury), Some((0, TOKEN))),
        open(&mut manager(), Some(&mercury), None),
    ];
    let mut invalid = TabManager::new(RecordingHost::default(), "file:///x".into(), theme.clone());
    errors.push(open(&mut invalid, Some(&mercury), given));
    let mut refusing = TabManager::new(RefusingHost, ORIGIN.into(), theme.clone());
    errors.push(
        refusing
            .handle(Effect::OpenCenterTab(mercury.id), Some(&mercury), given)
            .unwrap_err(),
    );
    // A re-open for a new daemon whose agent is gone.
    let mut reopened = manager();
    reopened
        .handle(Effect::OpenCenterTab(mercury.id), Some(&mercury), given)
        .unwrap();
    let failed = reopened.on_connection(Some((NEW_PORT, NEW_TOKEN)), &|_| None);
    errors.extend(failed.into_iter().map(|(_, error)| error));

    let kinds: Vec<String> = errors.iter().map(|e| format!("{e:?}")).collect();
    assert!(
        matches!(
            errors.as_slice(),
            [
                TabError::UnknownAgent,
                TabError::UnknownAgent,
                TabError::Host(HostError::InvalidPort),
                TabError::NotConnected,
                TabError::Host(HostError::InvalidOrigin),
                TabError::Host(HostError::Engine(_)),
                TabError::UnknownAgent,
            ]
        ),
        "{kinds:?}"
    );
    for err in &errors {
        for token in [TOKEN, NEW_TOKEN] {
            assert!(!err.to_string().contains(token), "{err}");
            assert!(!format!("{err:?}").contains(token), "{err:?}");
            assert!(!tab_error_text(err).contains(token));
        }
    }
}

/// Collects formatted `tracing` output.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

#[test]
fn the_placeholder_host_logs_the_page_and_the_redacted_script_never_the_token() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();

    let (mercury, venus) = (agent("Mercury"), agent("Venus"));
    let agents = vec![mercury.clone(), venus.clone()];
    tracing::subscriber::with_default(subscriber, || {
        let mut m = TabManager::new(
            PlaceholderHost,
            ORIGIN.into(),
            theme_state(Appearance::Dark),
        );
        m.attach(&NoWindow).unwrap();
        m.on_bounds(rect(800.0));
        for a in &agents {
            m.handle(Effect::OpenCenterTab(a.id), Some(a), conn())
                .unwrap();
        }
        m.handle(Effect::FocusCenterTab(mercury.id), None, None)
            .unwrap();
        m.set_theme(theme_state(Appearance::Light));
        assert!(m.open_external("https://example.com/"));
        assert!(m
            .on_connection(Some((NEW_PORT, NEW_TOKEN)), &lookup(&agents))
            .is_empty());
        m.handle(Effect::CloseCenterTab(venus.id), None, None)
            .unwrap();
        // Failures are logged by the shell with the error's text.
        let err = m
            .handle(Effect::OpenCenterTab(venus.id), Some(&venus), None)
            .unwrap_err();
        tracing::warn!(%err, ?err, "logged like the shell does");
    });

    let log = captured.text();
    assert!(
        log.contains("smithagents://app/index.html"),
        "the page url is logged:\n{log}"
    );
    assert!(log.contains("REDACTED"), "the redacted script is logged");
    assert!(log.contains(&format!("http://127.0.0.1:{PORT}")));
    assert!(log.contains(&format!("http://127.0.0.1:{NEW_PORT}")));
    assert!(log.contains("https://example.com/"));
    assert!(!log.contains(TOKEN), "the first token leaked:\n{log}");
    assert!(!log.contains(NEW_TOKEN), "the second token leaked:\n{log}");
}

#[test]
fn setting_the_theme_already_in_effect_pushes_nothing() {
    let mercury = agent("Mercury");
    let mut m = manager();
    open(&mut m, &mercury);
    let before = calls(&m).len();
    m.set_theme(theme_state(Appearance::Dark));
    assert_eq!(calls(&m).len(), before, "unchanged: no push");
    let light = theme_state(Appearance::Light);
    m.set_theme(light.clone());
    m.set_theme(light.clone());
    assert_eq!(calls_since(&m, before), vec![HostCall::PushTheme(light)]);
}

#[test]
fn shutdown_reaches_the_host_once_and_nothing_follows_it() {
    let mercury = agent("Mercury");
    let mut m = manager();
    open(&mut m, &mercury);
    m.shutdown();
    m.shutdown();
    let before = calls(&m).len();
    assert_eq!(calls(&m).last(), Some(&HostCall::Shutdown));
    assert_eq!(
        calls(&m)
            .iter()
            .filter(|call| **call == HostCall::Shutdown)
            .count(),
        1
    );
    m.on_bounds(rect(500.0));
    m.set_theme(theme_state(Appearance::Light));
    assert!(m
        .handle(Effect::OpenCenterTab(mercury.id), Some(&mercury), conn())
        .is_err());
    assert_eq!(calls(&m).len(), before, "no call after the shutdown");
}

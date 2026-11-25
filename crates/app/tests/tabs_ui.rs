//! The shell carrying its tabs out on an injected [`RecordingHost`]: opening,
//! switching and closing tabs from the views, theme pushes, bounds, a failed
//! open rolled back out of the strip, and a reconnect to a new daemon.
//! Headless (`#[gpui_kit::test]`), with a fake connection seam and no daemon.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use chrono::Utc;
use common::ui::{agent, open_shell_options, FakeAppearance};
use gpui_kit::base::Root;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestQueryExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Entity, TestAppContext, WindowHandle};
use raw_window_handle::HasWindowHandle;
use smithagents_app::model::{Action, Connection, ShellModel};
use smithagents_app::shell::ShellOptions;
use smithagents_app::shell_state::{ConnectionInfoSource, ShellState};
use smithagents_app::tabs::SharedHost;
use smithagents_app::view_models::{StatusKind, NOT_CONNECTED_TEXT, OPEN_CHAT_FAILED};
use smithagents_app::views::agents::agent_row_id;
use smithagents_app::views::center::CENTER_PANEL_ID;
use smithagents_app::views::center::{CHAT_NOTICE_ID, EMPTY_HINT_ID};
use smithagents_app::views::settings::THEME_OPTIONS_ID;
use smithagents_app::views::tabs::{
    close_button_id, tab_title_id, TAB_MAX_WIDTH, TOGGLE_AGENTS_ID,
};
use smithagents_center_host::{
    CenterTabHost, ExternalUrl, HostCall, HostError, PageBoot, PixelRect, RecordingHost, TabId,
    TabKind, TabSpec,
};
use smithagents_core::agent::Agent;
use smithagents_core::event::{Event, EventKind};
use smithagents_theme::state::{AppearanceSettings, ThemeChoice, ThemeState};
use smithagents_theme::tokens::Appearance;

const PORT: u16 = 4123;
const TOKEN: &str = "tok-SECRET-first-4123";
const NEW_PORT: u16 = 4999;
const NEW_TOKEN: &str = "tok-SECRET-second-4999";

type Conn = Rc<RefCell<Option<(u16, String)>>>;

struct Harness {
    window: WindowHandle<Root>,
    state: Entity<ShellState>,
    host: Rc<RefCell<RecordingHost>>,
    /// What the fake connection seam answers.
    conn: Conn,
}

impl Harness {
    fn calls(&self) -> Vec<HostCall> {
        self.host.borrow().calls().to_vec()
    }

    fn count(&self) -> usize {
        self.host.borrow().calls().len()
    }

    fn calls_since(&self, from: usize) -> Vec<HostCall> {
        self.host.borrow().calls()[from..].to_vec()
    }

    fn set_conn(&self, conn: Option<(u16, &str)>) {
        *self.conn.borrow_mut() = conn.map(|(port, token)| (port, token.to_owned()));
    }
}

fn connected(agents: Vec<Agent>) -> ShellModel {
    let mut model = ShellModel::new();
    model.apply(Action::Snapshot {
        agents,
        repos: vec![],
        latest_seq: 1,
    });
    model.apply(Action::ConnectionChanged(Connection::Connected));
    model
}

fn conn_source(conn: &Conn) -> ConnectionInfoSource {
    let conn = conn.clone();
    Rc::new(move || conn.borrow().clone())
}

/// Opens the shell on `model` with a recording host and a connection seam
/// answering `PORT`/`TOKEN`; `configure` may change the options first. Draws
/// a frame, so the center's rectangle is known.
fn open_with(
    cx: &mut TestAppContext,
    model: ShellModel,
    configure: impl FnOnce(&mut ShellOptions),
) -> Harness {
    let host = SharedHost::new(RecordingHost::default());
    let handle = host.handle();
    let conn: Conn = Rc::new(RefCell::new(Some((PORT, TOKEN.to_owned()))));
    let mut options = ShellOptions {
        model,
        tab_host: Some(Box::new(host)),
        connection_info: Some(conn_source(&conn)),
        ..Default::default()
    };
    configure(&mut options);
    let (window, shell) = open_shell_options(cx, options, (1200., 800.));
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    frame(cx, window);
    Harness {
        window,
        state,
        host: handle,
        conn,
    }
}

fn open(cx: &mut TestAppContext, model: ShellModel) -> Harness {
    open_with(cx, model, |_| {})
}

fn frame(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

fn dispatch(cx: &mut TestAppContext, h: &Harness, action: Action) {
    h.state.update(cx, |state, cx| state.dispatch(action, cx));
    frame(cx, h.window);
}

fn double_click_row(cx: &mut TestAppContext, h: &Harness, agent: &Agent) {
    cx.update_window(h.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.double_click(agent_row_id(agent.id), cx);
    })
    .unwrap();
    frame(cx, h.window);
}

/// `calls` without the `SetBounds` ones (the center moves when the strip or
/// the banner changes height; those are checked on their own).
fn without_bounds(calls: Vec<HostCall>) -> Vec<HostCall> {
    calls
        .into_iter()
        .filter(|call| !matches!(call, HostCall::SetBounds(_)))
        .collect()
}

/// The last rectangle the host was given.
fn last_bounds(h: &Harness) -> Option<PixelRect> {
    h.calls().iter().rev().find_map(|call| match call {
        HostCall::SetBounds(rect) => Some(*rect),
        _ => None,
    })
}

fn opened(call: &HostCall) -> (&TabSpec, &PageBoot) {
    match call {
        HostCall::Open { spec, boot } => (spec, boot),
        other => panic!("expected an open, got {other:?}"),
    }
}

/// The id of the last tab the host opened for `agent`.
fn last_opened_id(h: &Harness, agent: &Agent) -> TabId {
    h.calls()
        .iter()
        .rev()
        .find_map(|call| match call {
            HostCall::Open { spec, .. }
                if spec.kind
                    == TabKind::Chat {
                        agent_id: agent.id.to_string(),
                    } =>
            {
                Some(spec.id)
            }
            _ => None,
        })
        .expect("the host opened a tab for the agent")
}

fn theme(cx: &mut TestAppContext, h: &Harness) -> ThemeState {
    h.state
        .read_with(cx, |state, _| state.theme_state().clone())
}

fn center_rect(cx: &mut TestAppContext, h: &Harness) -> PixelRect {
    h.state
        .read_with(cx, |state, _| state.center_rect())
        .expect("a frame reported the center")
}

fn tabs(cx: &mut TestAppContext, h: &Harness) -> Vec<smithagents_core::ids::AgentId> {
    h.state
        .read_with(cx, |state, _| state.model().tabs().to_vec())
}

/// The text of the chat notification, if one is showing.
fn notice(cx: &mut TestAppContext, h: &Harness) -> Option<String> {
    let mut text = None;
    cx.update_window(h.window.into(), |_, window, cx| {
        window.render_frame(cx);
        text = window
            .try_find(CHAT_NOTICE_ID)
            .and_then(|notice| notice.value().map(str::to_owned));
    })
    .unwrap();
    text
}

fn notification_count(cx: &mut TestAppContext, h: &Harness) -> usize {
    let mut count = 0;
    cx.update_window(h.window.into(), |_, window, cx| {
        count = window.notifications(cx).len();
    })
    .unwrap();
    count
}

/// The tab labels the strip shows.
fn strip_shows(cx: &mut TestAppContext, h: &Harness, label: &str) -> bool {
    let mut found = false;
    cx.update_window(h.window.into(), |_, window, cx| {
        window.render_frame(cx);
        found = window
            .within("center-tabs")
            .try_find_by_label(label)
            .is_some();
    })
    .unwrap();
    found
}

#[gpui_kit::test]
fn the_host_is_attached_once_at_startup(cx: &mut TestAppContext) {
    let h = open(cx, connected(vec![agent("Mercury")]));
    assert_eq!(h.calls(), vec![HostCall::Attach]);
}

#[gpui_kit::test]
fn double_clicking_an_agent_opens_its_chat_on_the_host(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let h = open(cx, connected(vec![mercury.clone()]));
    let rect = center_rect(cx, &h);
    let before = h.count();

    double_click_row(cx, &h, &mercury);

    let calls = h.calls_since(before);
    assert!(calls.len() >= 3, "{calls:?}");
    let (spec, boot) = opened(&calls[0]);
    assert_eq!(spec.title, "Mercury");
    assert_eq!(
        spec.kind,
        TabKind::Chat {
            agent_id: mercury.id.to_string()
        }
    );
    assert_eq!(boot.page_url(), "smithagents://app/index.html");
    let current = theme(cx, &h);
    assert!(boot
        .host_script()
        .contains(&serde_json::to_string(&current).unwrap()));
    assert!(boot.host_script().contains(TOKEN));
    assert!(boot
        .redacted_host_script()
        .contains(&format!("http://127.0.0.1:{PORT}")));
    assert_eq!(calls[1], HostCall::SetBounds(rect), "positioned at once");
    assert_eq!(calls[2], HostCall::Show(spec.id));
    // The strip grew with its first tab, so the center moved down: the host
    // followed it.
    assert!(
        calls[3..]
            .iter()
            .all(|call| matches!(call, HostCall::SetBounds(_))),
        "{calls:?}"
    );
    assert_eq!(last_bounds(&h), Some(center_rect(cx, &h)));
    assert!(!format!("{calls:?}").contains(TOKEN));

    assert_eq!(tabs(cx, &h), [mercury.id]);
    assert!(strip_shows(cx, &h, "Mercury"));
    cx.update_window(h.window.into(), |_, window, _| {
        assert!(window.try_find(EMPTY_HINT_ID).is_none());
    })
    .unwrap();

    // Opening it again only shows it.
    let before = h.count();
    double_click_row(cx, &h, &mercury);
    assert_eq!(h.calls_since(before), vec![HostCall::Show(spec.id)]);
}

#[gpui_kit::test]
fn closing_a_tab_from_the_strip_closes_it_on_the_host(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let h = open(cx, connected(vec![mercury.clone()]));
    dispatch(cx, &h, Action::OpenTab(mercury.id));
    let id = last_opened_id(&h, &mercury);
    let before = h.count();

    cx.update_window(h.window.into(), |_, window, cx| {
        window.click(close_button_id(0), cx);
    })
    .unwrap();
    frame(cx, h.window);

    assert_eq!(h.calls_since(before), vec![HostCall::Close(id)]);
    assert!(tabs(cx, &h).is_empty());
    cx.update_window(h.window.into(), |_, window, _| {
        assert!(window.find(EMPTY_HINT_ID).visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn switching_tabs_shows_the_chosen_one_and_closing_the_active_shows_its_neighbour(
    cx: &mut TestAppContext,
) {
    let (mercury, venus) = (agent("Mercury"), agent("Venus"));
    let h = open(cx, connected(vec![mercury.clone(), venus.clone()]));
    dispatch(cx, &h, Action::OpenTab(mercury.id));
    dispatch(cx, &h, Action::OpenTab(venus.id));
    let (m_id, v_id) = (last_opened_id(&h, &mercury), last_opened_id(&h, &venus));
    let before = h.count();

    cx.update_window(h.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within("center-tabs").click(0usize, cx);
    })
    .unwrap();
    assert_eq!(h.calls_since(before), vec![HostCall::Show(m_id)]);

    // Closing the active Mercury: Venus, after it, becomes active and shown.
    let before = h.count();
    dispatch(cx, &h, Action::CloseTab(mercury.id));
    assert_eq!(
        h.calls_since(before),
        vec![HostCall::Close(m_id), HostCall::Show(v_id)]
    );
}

#[gpui_kit::test]
fn a_theme_change_in_settings_is_one_push_of_the_new_state(cx: &mut TestAppContext) {
    let (mercury, venus) = (agent("Mercury"), agent("Venus"));
    let h = open(cx, connected(vec![mercury.clone(), venus.clone()]));
    dispatch(cx, &h, Action::OpenTab(mercury.id));
    dispatch(cx, &h, Action::OpenTab(venus.id));
    let before = h.count();

    cx.update_window(h.window.into(), |_, window, cx| {
        let target = window.within(THEME_OPTIONS_ID).find_by_label("Smith Light");
        window
            .within(THEME_OPTIONS_ID)
            .click(target.path().last().unwrap().clone(), cx);
    })
    .unwrap();
    frame(cx, h.window);

    let light = theme(cx, &h);
    assert_eq!(light.theme, "smith-light");
    assert_eq!(
        h.calls_since(before),
        vec![HostCall::PushTheme(light.clone())],
        "one push for both tabs"
    );

    // Choosing the theme already in effect pushes nothing.
    let before = h.count();
    h.state.update(cx, |state, cx| {
        let settings = state.appearance_settings().clone();
        state.set_appearance(settings, cx)
    });
    frame(cx, h.window);
    assert_eq!(h.count(), before);

    // A tab opened now boots with the light theme.
    let earth = agent("Earth");
    dispatch(
        cx,
        &h,
        Action::Event(Event {
            seq: 5,
            at: Utc::now(),
            kind: EventKind::AgentCreated(earth.clone()),
        }),
    );
    dispatch(cx, &h, Action::OpenTab(earth.id));
    let calls = h.calls();
    let (_, boot) = opened(
        calls
            .iter()
            .rev()
            .find(|c| matches!(c, HostCall::Open { .. }))
            .unwrap(),
    );
    assert!(boot
        .host_script()
        .contains(&serde_json::to_string(&light).unwrap()));
}

#[gpui_kit::test]
fn an_os_flip_pushes_only_while_the_theme_follows_the_system(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let os = FakeAppearance::new(Appearance::Dark);
    let h = open_with(cx, connected(vec![mercury.clone()]), |options| {
        options.system_appearance = Some(os.clone());
    });
    dispatch(cx, &h, Action::OpenTab(mercury.id));

    let before = h.count();
    cx.update(|cx| os.set(Appearance::Light, cx));
    frame(cx, h.window);
    let light = theme(cx, &h);
    assert_eq!(light.appearance, Appearance::Light);
    assert_eq!(h.calls_since(before), vec![HostCall::PushTheme(light)]);

    // An explicit dark theme: one push for the choice itself...
    let before = h.count();
    h.state.update(cx, |state, cx| {
        let settings = AppearanceSettings {
            theme: ThemeChoice::Id("smith-dark".into()),
            ..state.appearance_settings().clone()
        };
        state.set_appearance(settings, cx)
    });
    frame(cx, h.window);
    let dark = theme(cx, &h);
    assert_eq!(dark.theme, "smith-dark");
    assert_eq!(h.calls_since(before), vec![HostCall::PushTheme(dark)]);

    // ...and none for OS flips from then on.
    let before = h.count();
    cx.update(|cx| os.set(Appearance::Dark, cx));
    cx.update(|cx| os.set(Appearance::Light, cx));
    frame(cx, h.window);
    assert_eq!(h.count(), before, "{:?}", h.calls_since(before));
}

#[gpui_kit::test]
fn compacting_the_left_pane_moves_the_tab_to_the_new_rect(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let h = open(cx, connected(vec![mercury.clone()]));
    dispatch(cx, &h, Action::OpenTab(mercury.id));
    let wide_left = center_rect(cx, &h);
    let before = h.count();

    cx.update_window(h.window.into(), |_, window, cx| {
        window.click(TOGGLE_AGENTS_ID, cx);
        window.render_frame(cx);
    })
    .unwrap();
    // The listener runs after the frame (deferred); one more frame settles.
    frame(cx, h.window);

    let compact = center_rect(cx, &h);
    assert!(compact.w > wide_left.w, "{compact:?} vs {wide_left:?}");
    assert_eq!(h.calls_since(before), vec![HostCall::SetBounds(compact)]);
}

#[gpui_kit::test]
fn opening_while_not_connected_reports_it_and_leaves_no_tab(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let h = open(cx, connected(vec![mercury.clone()]));
    h.set_conn(None);
    let before = h.count();

    double_click_row(cx, &h, &mercury);

    assert_eq!(
        h.count(),
        before,
        "no host call: {:?}",
        h.calls_since(before)
    );
    assert!(tabs(cx, &h).is_empty(), "the open was rolled back");
    assert_eq!(
        h.state.read_with(cx, |state, _| state.model().active_tab()),
        None
    );
    assert!(!strip_shows(cx, &h, "Mercury"));
    cx.update_window(h.window.into(), |_, window, _| {
        assert!(window.find(EMPTY_HINT_ID).visible());
    })
    .unwrap();
    assert_eq!(notice(cx, &h).as_deref(), Some(NOT_CONNECTED_TEXT));
    assert_eq!(notification_count(cx, &h), 1);
    let status = h
        .state
        .read_with(cx, |state, _| state.status_message())
        .expect("an error status");
    assert_eq!(status.kind, StatusKind::Error);
    assert_eq!(
        status.text,
        format!("{OPEN_CHAT_FAILED}: not connected to the daemon")
    );
    assert!(!status.text.contains(TOKEN));

    // A second failure replaces the notification rather than stacking.
    double_click_row(cx, &h, &mercury);
    assert_eq!(notification_count(cx, &h), 1);
}

#[gpui_kit::test]
fn a_failed_open_keeps_the_other_tab_active_and_shown(cx: &mut TestAppContext) {
    let (mercury, venus) = (agent("Mercury"), agent("Venus"));
    let h = open(cx, connected(vec![mercury.clone(), venus.clone()]));
    dispatch(cx, &h, Action::OpenTab(mercury.id));
    let m_id = last_opened_id(&h, &mercury);
    h.set_conn(None);
    let before = h.count();

    dispatch(cx, &h, Action::OpenTab(venus.id));

    assert_eq!(tabs(cx, &h), [mercury.id]);
    assert_eq!(
        h.state.read_with(cx, |state, _| state.model().active_tab()),
        Some(mercury.id)
    );
    // The model's rollback focuses Mercury again; the host still shows it.
    assert_eq!(h.calls_since(before), vec![HostCall::Show(m_id)]);
    assert!(strip_shows(cx, &h, "Mercury"));
    assert!(!strip_shows(cx, &h, "Venus"));
}

#[gpui_kit::test]
fn a_reconnect_to_a_new_daemon_reopens_the_tabs_with_its_token(cx: &mut TestAppContext) {
    let (mercury, venus) = (agent("Mercury"), agent("Venus"));
    let agents = vec![mercury.clone(), venus.clone()];
    let h = open(cx, connected(agents.clone()));
    dispatch(cx, &h, Action::OpenTab(mercury.id));
    dispatch(cx, &h, Action::OpenTab(venus.id));
    dispatch(cx, &h, Action::OpenTab(mercury.id));
    let old = [last_opened_id(&h, &mercury), last_opened_id(&h, &venus)];

    // The daemon restarted: the link loses it, then connects to the new one.
    h.set_conn(None);
    let before = h.count();
    dispatch(cx, &h, Action::ConnectionChanged(Connection::Reconnecting));
    // The banner moved the center (bounds follow); nothing else happens.
    assert!(
        without_bounds(h.calls_since(before)).is_empty(),
        "a lost connection keeps the tabs: {:?}",
        h.calls_since(before)
    );
    // The re-opened tabs are positioned where the center is now.
    let rect = center_rect(cx, &h);
    let before = h.count();
    h.set_conn(Some((NEW_PORT, NEW_TOKEN)));
    dispatch(
        cx,
        &h,
        Action::Snapshot {
            agents,
            repos: vec![],
            latest_seq: 1,
        },
    );
    dispatch(cx, &h, Action::ConnectionChanged(Connection::Connected));

    let calls = h.calls_since(before);
    assert!(calls.len() >= 7, "{calls:?}");
    let new = [last_opened_id(&h, &mercury), last_opened_id(&h, &venus)];
    for ix in 0..2 {
        assert_eq!(calls[ix * 3], HostCall::Close(old[ix]));
        let (spec, boot) = opened(&calls[ix * 3 + 1]);
        assert_eq!(spec.id, new[ix]);
        assert_ne!(new[ix], old[ix]);
        assert!(boot.host_script().contains(NEW_TOKEN));
        assert!(!boot.host_script().contains(TOKEN), "no stale token");
        assert!(boot
            .redacted_host_script()
            .contains(&format!("http://127.0.0.1:{NEW_PORT}")));
        assert_eq!(calls[ix * 3 + 2], HostCall::SetBounds(rect));
    }
    assert_eq!(calls[6], HostCall::Show(new[0]), "Mercury was shown");
    // The banner went away and the center moved back: bounds follow.
    assert!(without_bounds(calls[7..].to_vec()).is_empty(), "{calls:?}");
    assert_eq!(last_bounds(&h), Some(center_rect(cx, &h)));
    assert_eq!(
        tabs(cx, &h),
        [mercury.id, venus.id],
        "the strip is unchanged"
    );

    // Reconnecting to the same daemon re-opens nothing.
    let before = h.count();
    dispatch(cx, &h, Action::ConnectionChanged(Connection::Reconnecting));
    dispatch(cx, &h, Action::ConnectionChanged(Connection::Connected));
    assert!(
        without_bounds(h.calls_since(before)).is_empty(),
        "{:?}",
        h.calls_since(before)
    );
}

#[gpui_kit::test]
fn an_agent_closed_by_the_daemon_closes_its_tab_on_the_host(cx: &mut TestAppContext) {
    let (mercury, venus) = (agent("Mercury"), agent("Venus"));
    let h = open(cx, connected(vec![mercury.clone(), venus.clone()]));
    dispatch(cx, &h, Action::OpenTab(mercury.id));
    dispatch(cx, &h, Action::OpenTab(venus.id));
    let (m_id, v_id) = (last_opened_id(&h, &mercury), last_opened_id(&h, &venus));
    let before = h.count();

    dispatch(
        cx,
        &h,
        Action::Event(Event {
            seq: 2,
            at: Utc::now(),
            kind: EventKind::AgentClosed { agent_id: venus.id },
        }),
    );

    assert_eq!(
        h.calls_since(before),
        vec![HostCall::Close(v_id), HostCall::Show(m_id)]
    );
    assert_eq!(tabs(cx, &h), [mercury.id]);
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

#[gpui_kit::test]
fn a_host_that_refuses_the_tab_reports_its_message_and_leaves_no_tab(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let failing = SharedHost::new(FailingHost::default());
    let handle = failing.handle();
    let h = open_with(cx, connected(vec![mercury.clone()]), |options| {
        options.tab_host = Some(Box::new(failing));
    });

    double_click_row(cx, &h, &mercury);

    assert!(tabs(cx, &h).is_empty());
    assert_eq!(handle.borrow().0.calls(), [HostCall::Attach]);
    assert_eq!(
        notice(cx, &h).as_deref(),
        Some("Web engine error: the engine is down")
    );
    let status = h
        .state
        .read_with(cx, |state, _| state.status_message())
        .unwrap();
    assert_eq!(status.kind, StatusKind::Error);
    assert_eq!(
        status.text,
        format!("{OPEN_CHAT_FAILED}: web engine error: the engine is down")
    );
    assert!(!status.text.contains(TOKEN));
}

#[gpui_kit::test]
fn an_invalid_page_origin_is_reported_at_startup_and_blocks_opening(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let h = open_with(cx, connected(vec![mercury.clone()]), |options| {
        options.page_origin = Some("file:///tmp/web".into());
    });
    let status = h
        .state
        .read_with(cx, |state, _| state.status_message())
        .expect("the bad origin is reported");
    assert_eq!(status.kind, StatusKind::Error);
    assert!(status.text.contains("file:///tmp/web"), "{status:?}");

    double_click_row(cx, &h, &mercury);
    assert_eq!(h.calls(), vec![HostCall::Attach], "no open");
    assert!(tabs(cx, &h).is_empty());
    let text = notice(cx, &h).expect("a notification");
    assert!(text.starts_with("Page origin must be"), "{text}");
}

#[gpui_kit::test]
fn the_dev_origin_from_the_options_is_the_page_url(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let h = open_with(cx, connected(vec![mercury.clone()]), |options| {
        options.page_origin = Some("http://localhost:5173".into());
    });
    assert_eq!(
        h.state.read_with(cx, |state, _| state.status_message()),
        None,
        "a valid origin reports nothing"
    );
    dispatch(cx, &h, Action::OpenTab(mercury.id));
    let calls = h.calls();
    let (_, boot) = opened(&calls[1]);
    assert_eq!(boot.page_url(), "http://localhost:5173/index.html");
}

#[gpui_kit::test]
fn the_default_placeholder_host_opens_tabs(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let conn: Conn = Rc::new(RefCell::new(Some((PORT, TOKEN.to_owned()))));
    let (window, shell) = open_shell_options(
        cx,
        ShellOptions {
            model: connected(vec![mercury.clone()]),
            connection_info: Some(conn_source(&conn)),
            ..Default::default()
        },
        (1200., 800.),
    );
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    frame(cx, window);
    state.update(cx, |state, cx| {
        state.dispatch(Action::OpenTab(mercury.id), cx)
    });
    frame(cx, window);
    assert_eq!(
        state.read_with(cx, |state, _| state.model().tabs().to_vec()),
        [mercury.id]
    );
    assert_eq!(state.read_with(cx, |state, _| state.status_message()), None);
}

#[gpui_kit::test]
fn a_long_name_is_cut_short_and_never_pushes_the_other_tabs_off(cx: &mut TestAppContext) {
    let long = agent(&"Saturn".repeat(34)[..200]);
    assert_eq!(long.name.chars().count(), 200);
    let venus = agent("Venus");
    let h = open(cx, connected(vec![long.clone(), venus.clone()]));
    dispatch(cx, &h, Action::OpenTab(long.id));
    dispatch(cx, &h, Action::OpenTab(venus.id));
    cx.update_window(h.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let center = window.find(CENTER_PANEL_ID).bounds();
        let tab = |ix: usize, window: &mut gpui_kit::Window| {
            window.within("center-tabs").find(ix).bounds()
        };
        let (first, second) = (tab(0, window), tab(1, window));
        assert!(first.size.width <= TAB_MAX_WIDTH, "{first:?}");
        let title = window.find(tab_title_id(0)).bounds();
        assert!(title.right() <= first.right(), "{title:?} in {first:?}");
        assert!(second.left() >= first.right(), "{first:?} then {second:?}");
        assert!(second.right() <= center.right(), "{second:?} in {center:?}");
        let close = window.find(close_button_id(1));
        assert!(close.visible());
        assert!(close.bounds().right() <= center.right());
    })
    .unwrap();
}

#[gpui_kit::test]
fn each_close_button_names_its_tab(cx: &mut TestAppContext) {
    let (mercury, venus) = (agent("Mercury"), agent("Venus"));
    let h = open(cx, connected(vec![mercury.clone(), venus.clone()]));
    dispatch(cx, &h, Action::OpenTab(mercury.id));
    dispatch(cx, &h, Action::OpenTab(venus.id));
    cx.update_window(h.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(close_button_id(0)).label(),
            Some("Close Mercury")
        );
        assert_eq!(window.find(close_button_id(1)).label(), Some("Close Venus"));
        assert!(window.find_all_by_label("Close tab").is_empty());
    })
    .unwrap();
}

#[gpui_kit::test]
fn replacing_the_public_bounds_listener_keeps_the_tabs_positioned(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let h = open(cx, connected(vec![mercury.clone()]));
    dispatch(cx, &h, Action::OpenTab(mercury.id));
    let seen: Rc<RefCell<Vec<PixelRect>>> = Rc::default();
    h.state.update(cx, |state, cx| {
        let seen = seen.clone();
        state.set_center_bounds_listener(move |rect, _| seen.borrow_mut().push(rect), cx)
    });
    frame(cx, h.window);
    let before = h.count();

    cx.update_window(h.window.into(), |_, window, cx| {
        window.click(TOGGLE_AGENTS_ID, cx);
        window.render_frame(cx);
    })
    .unwrap();
    frame(cx, h.window);

    let compact = center_rect(cx, &h);
    assert_eq!(h.calls_since(before), vec![HostCall::SetBounds(compact)]);
    assert_eq!(
        seen.borrow().last(),
        Some(&compact),
        "the extra observer too"
    );
}

#[gpui_kit::test]
fn a_tab_opened_in_the_same_update_as_a_theme_change_boots_with_the_new_theme(
    cx: &mut TestAppContext,
) {
    let (mercury, venus) = (agent("Mercury"), agent("Venus"));
    let h = open(cx, connected(vec![mercury.clone(), venus.clone()]));
    dispatch(cx, &h, Action::OpenTab(mercury.id));
    let before = h.count();
    h.state.update(cx, |state, cx| {
        let settings = AppearanceSettings {
            theme: ThemeChoice::Id("smith-light".into()),
            ..state.appearance_settings().clone()
        };
        state.set_appearance(settings, cx);
        state.dispatch(Action::OpenTab(venus.id), cx);
    });
    frame(cx, h.window);
    cx.run_until_parked();

    let light = theme(cx, &h);
    assert_eq!(light.theme, "smith-light");
    let calls = without_bounds(h.calls_since(before));
    assert_eq!(
        calls[0],
        HostCall::PushTheme(light.clone()),
        "Mercury's page"
    );
    let (spec, boot) = opened(&calls[1]);
    assert_eq!(spec.title, "Venus");
    assert!(
        boot.host_script()
            .contains(&serde_json::to_string(&light).unwrap()),
        "Venus boots with the new theme"
    );
    let pushes = calls
        .iter()
        .filter(|call| matches!(call, HostCall::PushTheme(_)))
        .count();
    assert_eq!(pushes, 1, "one push per change: {calls:?}");
}

#[gpui_kit::test]
fn a_failed_open_gives_the_focus_back_to_the_tab_that_had_it(cx: &mut TestAppContext) {
    let [a, b, c, d] = ["Mercury", "Venus", "Earth", "Mars"].map(agent);
    let h = open(
        cx,
        connected(vec![a.clone(), b.clone(), c.clone(), d.clone()]),
    );
    for agent in [&a, &b, &c] {
        dispatch(cx, &h, Action::OpenTab(agent.id));
    }
    // A's tab is clicked again: active, and its row selected.
    dispatch(cx, &h, Action::OpenTab(a.id));
    let a_id = last_opened_id(&h, &a);
    h.set_conn(None);
    let before = h.count();

    dispatch(cx, &h, Action::OpenTab(d.id));

    assert_eq!(tabs(cx, &h), [a.id, b.id, c.id]);
    let (active, selected) = h.state.read_with(cx, |state, _| {
        (state.model().active_tab(), state.model().selected())
    });
    assert_eq!(active, Some(a.id), "the tab that was active");
    assert_eq!(selected, Some(a.id), "the row that was selected");
    assert_eq!(h.calls_since(before), vec![HostCall::Show(a_id)]);
}

#[gpui_kit::test]
fn a_not_connected_failure_clears_when_the_connection_comes_back(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let h = open(cx, connected(vec![mercury.clone()]));
    // The link dropped, but its Reconnecting has not reached the model yet.
    h.set_conn(None);
    dispatch(cx, &h, Action::OpenTab(mercury.id));
    let status = |cx: &mut TestAppContext| {
        h.state.read_with(cx, |state, _| {
            state.status_message().map(|m| (m.kind, m.text))
        })
    };
    assert_eq!(
        status(cx),
        Some((
            StatusKind::Error,
            format!("{OPEN_CHAT_FAILED}: not connected to the daemon")
        ))
    );

    dispatch(cx, &h, Action::ConnectionChanged(Connection::Reconnecting));
    h.set_conn(Some((PORT, TOKEN)));
    dispatch(cx, &h, Action::ConnectionChanged(Connection::Connected));
    assert_eq!(status(cx), None, "stale once the connection is back");
}

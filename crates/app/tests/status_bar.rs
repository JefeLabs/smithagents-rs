//! Headless checks of the status bar: its place across the window, its
//! three zones, the status messages and the update button.

mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use common::ui::{agent, dark_native, open_shell_options, painted_at, token_background};
use futures::channel::oneshot;
use futures::future::LocalBoxFuture;
use futures::FutureExt as _;
use gpui_kit::base::Root;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{point, px, AppContext as _, Entity, Role, TestAppContext, WindowHandle};
use smithagents_app::layout_store::STATUS_BAR_HEIGHT;
use smithagents_app::link::{AgentCommands, LinkError};
use smithagents_app::model::{Action, Connection, ShellModel};
use smithagents_app::shell::{Shell, ShellOptions, LEFT_PANE_ID};
use smithagents_app::shell_state::ShellState;
use smithagents_app::theme_adapter::{region_tokens, Region};
use smithagents_app::update::{UpdateState, Updater};
use smithagents_app::view_models::{StatusKind, StatusMessage};
use smithagents_app::views::rail::RAIL_ID;
use smithagents_app::views::status_bar::{
    STATUS_BAR_ID, STATUS_BAR_LABEL, STATUS_ERROR_DOT_ID, STATUS_MESSAGE_ID, STATUS_RIGHT_ID,
    STATUS_VERSION_ID, UPDATE_BUTTON_ID,
};
use smithagents_center_host::PixelRect;
use smithagents_core::agent::{Agent, Repo};
use smithagents_core::ids::{AgentId, RepoId};

const INFO_FOR: Duration = Duration::from_millis(300);

/// A fake updater: `check` answers `found`; each `install` waits for the
/// test to answer it.
struct FakeUpdater {
    found: UpdateState,
    installs: RefCell<Vec<String>>,
    pending: RefCell<Vec<oneshot::Sender<Result<(), String>>>>,
}

impl FakeUpdater {
    fn new(found: UpdateState) -> Rc<Self> {
        Rc::new(Self {
            found,
            installs: RefCell::default(),
            pending: RefCell::default(),
        })
    }

    fn available(version: &str) -> Rc<Self> {
        Self::new(UpdateState::Available {
            version: version.into(),
        })
    }

    fn answer(&self, result: Result<(), String>) {
        let sender = self.pending.borrow_mut().remove(0);
        let _ = sender.send(result);
    }
}

impl Updater for FakeUpdater {
    fn check(&self) -> LocalBoxFuture<'static, UpdateState> {
        let found = self.found.clone();
        async move { found }.boxed_local()
    }

    fn install(&self, version: String) -> LocalBoxFuture<'static, Result<(), String>> {
        self.installs.borrow_mut().push(version);
        let (tx, rx) = oneshot::channel();
        self.pending.borrow_mut().push(tx);
        async move { rx.await.unwrap_or(Err("dropped".into())) }.boxed_local()
    }
}

fn connected() -> ShellModel {
    let mut model = ShellModel::new();
    model.apply(Action::ConnectionChanged(Connection::Connected));
    model
}

fn open(
    cx: &mut TestAppContext,
    model: ShellModel,
    updater: Option<Rc<FakeUpdater>>,
) -> (WindowHandle<Root>, Entity<Shell>, Entity<ShellState>) {
    let options = ShellOptions {
        model,
        updater: updater.map(|u| u as Rc<dyn Updater>),
        info_status_duration: Some(INFO_FOR),
        ..Default::default()
    };
    let (window, shell) = open_shell_options(cx, options, (1200., 800.));
    cx.run_until_parked();
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    (window, shell, state)
}

/// The message's text as exposed to assistive technology, if one shows.
fn message(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Option<String> {
    let mut text = None;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        text = window
            .try_find(STATUS_MESSAGE_ID)
            .and_then(|m| m.label().map(str::to_owned));
    })
    .unwrap();
    text
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Root>, id: &'static str) -> bool {
    let mut found = false;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        found = window.try_find(id).is_some();
    })
    .unwrap();
    found
}

fn click(cx: &mut TestAppContext, window: WindowHandle<Root>, id: &'static str) {
    cx.update_window(window.into(), |_, window, cx| window.click(id, cx))
        .unwrap();
    cx.run_until_parked();
}

fn button_label(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
) -> (Option<String>, Option<bool>) {
    let mut out = (None, None);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let button = window.find(UPDATE_BUTTON_ID);
        out = (button.label().map(str::to_owned), button.disabled());
    })
    .unwrap();
    out
}

fn set_status(cx: &mut TestAppContext, state: &Entity<ShellState>, message: StatusMessage) {
    state.update(cx, |state, cx| state.set_status(message, cx));
    cx.run_until_parked();
}

#[gpui_kit::test]
fn the_bar_spans_the_window_bottom_and_the_regions_end_at_its_top(cx: &mut TestAppContext) {
    let (window, _shell, state) = open(cx, connected(), None);
    let seen: Rc<RefCell<Vec<PixelRect>>> = Rc::default();
    state.update(cx, |state, cx| {
        let seen = seen.clone();
        state.set_center_bounds_listener(move |rect, _| seen.borrow_mut().push(rect), cx);
    });
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let view = window.viewport_size();
        let bar = window.find(STATUS_BAR_ID);
        let bounds = bar.bounds();
        assert_eq!(bar.role(), Some(Role::Region));
        assert_eq!(bar.label(), Some(STATUS_BAR_LABEL));
        assert_eq!(bounds.origin.x, px(0.));
        assert_eq!(bounds.size.width, view.width);
        assert_eq!(bounds.bottom(), view.height);
        assert_eq!(bounds.size.height, STATUS_BAR_HEIGHT);
        assert_eq!(bounds.size.height, px(24.));
        assert_eq!(window.find(LEFT_PANE_ID).bounds().bottom(), bounds.top());
        assert_eq!(window.find(RAIL_ID).bounds().bottom(), bounds.top());

        // The bar paints card-2, the left pane card.
        let native = dark_native();
        let bar_color = painted_at(window, point(px(300.), bounds.top() + px(12.)));
        let left = window.find(LEFT_PANE_ID).bounds();
        let left_color = painted_at(window, point(px(20.), left.bottom() - px(20.)));
        assert_eq!(
            bar_color,
            token_background(&native, region_tokens(Region::BottomBar).0)
        );
        assert_ne!(bar_color, left_color);
    })
    .unwrap();
    let rect = *seen.borrow().last().expect("the center reported");
    let bottom = (rect.y + rect.h) / rect.scale;
    assert!(bottom <= (800. - 24.) + 0.01, "{rect:?}");
}

#[gpui_kit::test]
fn the_version_is_at_the_far_right_and_no_update_button_by_default(cx: &mut TestAppContext) {
    let (window, _shell, _state) = open(cx, connected(), None);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let view = window.viewport_size();
        let version = window.find(STATUS_VERSION_ID).bounds();
        assert!(version.right() <= view.width);
        assert!(version.right() >= view.width - px(16.), "{version:?}");
        assert!(window.try_find(UPDATE_BUTTON_ID).is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn an_available_update_shows_a_button_left_of_the_version_and_installs_once(
    cx: &mut TestAppContext,
) {
    let fake = FakeUpdater::available("9.9.9");
    let (window, _shell, _state) = open(cx, connected(), Some(fake.clone()));
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let button = window.find(UPDATE_BUTTON_ID);
        assert!(button.bounds().right() <= window.find(STATUS_VERSION_ID).bounds().left());
        assert_eq!(button.label(), Some("Update available: v9.9.9"));
    })
    .unwrap();

    click(cx, window, UPDATE_BUTTON_ID);
    click(cx, window, UPDATE_BUTTON_ID);
    assert_eq!(*fake.installs.borrow(), ["9.9.9"], "exactly one install");
    let (label, disabled) = button_label(cx, window);
    assert_eq!(disabled, Some(true), "disabled while installing");
    assert_eq!(label.as_deref(), Some("Installing…: v9.9.9"));
}

#[gpui_kit::test]
fn the_button_reads_installing_then_disappears_with_the_restart_message(cx: &mut TestAppContext) {
    let fake = FakeUpdater::available("9.9.9");
    let (window, _shell, state) = open(cx, connected(), Some(fake.clone()));
    click(cx, window, UPDATE_BUTTON_ID);
    assert_eq!(
        state.read_with(cx, |s, _| s.update_state().clone()),
        UpdateState::Installing {
            version: "9.9.9".into()
        }
    );
    fake.answer(Ok(()));
    cx.run_until_parked();
    assert!(!has(cx, window, UPDATE_BUTTON_ID));
    assert_eq!(
        message(cx, window).as_deref(),
        Some("Update v9.9.9 installed. Restart SmithAgents to finish.")
    );
}

#[gpui_kit::test]
fn a_failed_install_shows_the_error_and_retries(cx: &mut TestAppContext) {
    let fake = FakeUpdater::available("9.9.9");
    let (window, _shell, state) = open(cx, connected(), Some(fake.clone()));
    click(cx, window, UPDATE_BUTTON_ID);
    fake.answer(Err("boom".into()));
    cx.run_until_parked();
    assert_eq!(message(cx, window).as_deref(), Some("boom"));
    assert_eq!(
        state.read_with(cx, |s, _| s.status_message()),
        Some(StatusMessage::error("boom"))
    );
    assert!(has(cx, window, STATUS_ERROR_DOT_ID));
    let (label, disabled) = button_label(cx, window);
    assert_eq!(label.as_deref(), Some("Update failed, retry: v9.9.9"));
    assert_ne!(disabled, Some(true));

    click(cx, window, UPDATE_BUTTON_ID);
    assert_eq!(*fake.installs.borrow(), ["9.9.9", "9.9.9"], "retried");
    fake.answer(Ok(()));
    cx.run_until_parked();
    assert!(!has(cx, window, UPDATE_BUTTON_ID));
}

#[gpui_kit::test]
fn an_info_message_clears_itself_and_an_error_stays(cx: &mut TestAppContext) {
    let (window, _shell, state) = open(cx, connected(), None);
    set_status(cx, &state, StatusMessage::info("Hello"));
    assert_eq!(message(cx, window).as_deref(), Some("Hello"));
    assert!(!has(cx, window, STATUS_ERROR_DOT_ID), "no dot for info");
    cx.executor()
        .advance_clock(INFO_FOR + Duration::from_millis(10));
    cx.run_until_parked();
    assert_eq!(message(cx, window), None);

    set_status(cx, &state, StatusMessage::error("Bad"));
    cx.executor().advance_clock(INFO_FOR * 20);
    cx.run_until_parked();
    assert_eq!(message(cx, window).as_deref(), Some("Bad"));
    state.update(cx, |state, cx| state.clear_status(cx));
    assert_eq!(message(cx, window), None);
}

#[gpui_kit::test]
fn a_replaced_info_message_cancels_the_old_timer(cx: &mut TestAppContext) {
    let (window, _shell, state) = open(cx, connected(), None);
    set_status(cx, &state, StatusMessage::info("first"));
    cx.executor()
        .advance_clock(INFO_FOR - Duration::from_millis(100));
    set_status(cx, &state, StatusMessage::info("second"));
    // Past the first message's deadline, before the second's.
    cx.executor().advance_clock(Duration::from_millis(150));
    cx.run_until_parked();
    assert_eq!(message(cx, window).as_deref(), Some("second"));
    // An error replacing an info message is not cleared by the old timer.
    set_status(cx, &state, StatusMessage::error("err"));
    cx.executor().advance_clock(INFO_FOR * 2);
    cx.run_until_parked();
    assert_eq!(message(cx, window).as_deref(), Some("err"));
}

#[gpui_kit::test]
fn the_connection_text_wins_over_a_transient_message(cx: &mut TestAppContext) {
    let (window, _shell, state) = open(cx, connected(), None);
    set_status(cx, &state, StatusMessage::error("boom"));
    state.update(cx, |state, cx| {
        state.dispatch(Action::ConnectionChanged(Connection::Reconnecting), cx)
    });
    assert_eq!(
        message(cx, window).as_deref(),
        Some("Reconnecting to the local daemon…")
    );
    set_status(cx, &state, StatusMessage::info("other"));
    assert_eq!(
        message(cx, window).as_deref(),
        Some("Reconnecting to the local daemon…")
    );
    state.update(cx, |state, cx| {
        state.dispatch(Action::ConnectionChanged(Connection::Connected), cx)
    });
    assert_eq!(message(cx, window).as_deref(), Some("other"));
}

#[derive(Default)]
struct FakeCommands {
    created: RefCell<Option<Agent>>,
    closes: Cell<usize>,
}

impl AgentCommands for FakeCommands {
    fn create_agent(&self, _: RepoId) -> LocalBoxFuture<'static, Result<Agent, LinkError>> {
        let agent = self.created.borrow().clone().unwrap();
        async move { Ok(agent) }.boxed_local()
    }

    fn close_agent(&self, _: AgentId) -> LocalBoxFuture<'static, Result<(), LinkError>> {
        self.closes.set(self.closes.get() + 1);
        async { Err(LinkError::NotConnected) }.boxed_local()
    }
}

#[gpui_kit::test]
fn creating_an_agent_sets_the_created_status(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let repo = Repo {
        id: RepoId::new(),
        path: "/src/app".into(),
        default_branch: "main".into(),
        created_at: mercury.created_at,
    };
    let mut model = connected();
    model.apply(Action::RepoAdded(repo));
    let fake = Rc::new(FakeCommands::default());
    *fake.created.borrow_mut() = Some(mercury.clone());
    let options = ShellOptions {
        model,
        commands: Some(fake.clone()),
        info_status_duration: Some(INFO_FOR),
        ..Default::default()
    };
    let (window, shell) = open_shell_options(cx, options, (1200., 800.));
    cx.run_until_parked();
    let _ = shell;
    click(cx, window, smithagents_app::views::agents::NEW_AGENT_ID);
    assert_eq!(
        message(cx, window).as_deref(),
        Some("Agent Mercury created")
    );
}

#[gpui_kit::test]
fn a_repository_event_says_which_repository_was_added(cx: &mut TestAppContext) {
    let (window, _shell, state) = open(cx, connected(), None);
    let repo = Repo {
        id: RepoId::new(),
        path: "/src/my-app".into(),
        default_branch: "main".into(),
        created_at: "2026-11-24T09:00:00Z".parse().unwrap(),
    };
    state.update(cx, |state, cx| state.dispatch(Action::RepoAdded(repo), cx));
    assert_eq!(
        message(cx, window).as_deref(),
        Some("Repository added: my-app")
    );
}

#[gpui_kit::test]
fn a_very_long_message_is_contained_left_of_the_right_zone(cx: &mut TestAppContext) {
    let fake = FakeUpdater::available("9.9.9");
    let (window, _shell, state) = open(cx, connected(), Some(fake));
    set_status(cx, &state, StatusMessage::error("x".repeat(600)));
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let message = window.find(STATUS_MESSAGE_ID).bounds();
        let right = window.find(STATUS_RIGHT_ID).bounds();
        assert!(message.right() <= right.left(), "{message:?} vs {right:?}");
        assert!(
            window.find(UPDATE_BUTTON_ID).bounds().left() >= right.left(),
            "the right zone is not squeezed"
        );
        assert!(message.left() >= px(0.));
        assert!(message.size.height <= STATUS_BAR_HEIGHT);
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_short_message_is_centered_in_the_window(cx: &mut TestAppContext) {
    let fake = FakeUpdater::available("9.9.9");
    let (window, _shell, state) = open(cx, connected(), Some(fake));
    set_status(cx, &state, StatusMessage::info("Centered"));
    assert_eq!(
        state.read_with(cx, |s, _| s.status_message().map(|m| m.kind)),
        Some(StatusKind::Info)
    );
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let view = window.viewport_size();
        let message = window.find(STATUS_MESSAGE_ID).bounds();
        let middle = (message.left() + message.right()) / 2.;
        let off = (middle - view.width / 2.).abs();
        assert!(off <= px(8.), "message middle {middle:?} in {view:?}");
    })
    .unwrap();
}

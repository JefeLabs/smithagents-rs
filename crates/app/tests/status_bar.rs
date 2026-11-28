//! Headless checks of the status bar: its place across the window, its
//! three zones, the status messages and the update button.

mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use common::ui::{
    agent, dark_native, open_shell_options, painted_at, theme_state, token_background,
};
use futures::channel::oneshot;
use futures::future::LocalBoxFuture;
use futures::FutureExt as _;
use gpui_kit::base::{test_support, Root};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{point, px, AppContext as _, Entity, Role, TestAppContext, WindowHandle};
use smithagents_app::layout_store::{status_bar_height, MIN_WINDOW_SIZE};
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
    STATUS_VERSION_ID,
};
use smithagents_app::views::title_bar::{
    TITLE_BAR_ID, TITLE_TEXT_ID, UPDATE_CHIP_ID, UPDATE_CHIP_MARGIN, UPDATE_CHIP_TOKENS,
};
use smithagents_center_host::PixelRect;
use smithagents_core::agent::{AddSourceControl, Agent, Repo, SourceControl};
use smithagents_core::ids::{AgentId, RepoId};
use smithagents_theme::state::{AppearanceSettings, ThemeChoice};
use smithagents_theme::tokens::{Appearance, ThemeSet};

const INFO_FOR: Duration = Duration::from_millis(300);

/// A fake updater: `check` answers `found`; each `install` waits for the
/// test to answer it.
struct FakeUpdater {
    found: UpdateState,
    installs: RefCell<Vec<String>>,
    checks: Cell<usize>,
    pending: RefCell<Vec<oneshot::Sender<Result<(), String>>>>,
}

impl FakeUpdater {
    fn new(found: UpdateState) -> Rc<Self> {
        Rc::new(Self {
            found,
            installs: RefCell::default(),
            checks: Cell::new(0),
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
        self.checks.set(self.checks.get() + 1);
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
        let button = window.find(UPDATE_CHIP_ID);
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
        assert_eq!(bounds.size.height, status_bar_height(window.rem_size()));
        assert_eq!(bounds.size.height, px(24.), "at 100% text");
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
fn the_version_is_at_the_far_right_and_no_update_chip_by_default(cx: &mut TestAppContext) {
    let (window, _shell, _state) = open(cx, connected(), None);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let view = window.viewport_size();
        let version = window.find(STATUS_VERSION_ID).bounds();
        assert!(version.right() <= view.width);
        assert!(version.right() >= view.width - px(16.), "{version:?}");
        assert!(window.try_find(UPDATE_CHIP_ID).is_none());
    })
    .unwrap();
}

/// The chip's painted label text.
fn chip_text(window: &gpui_kit::Window) -> Vec<String> {
    test_support::texts(window)
        .into_iter()
        .filter(|text| {
            text.path()
                .contains(&gpui_kit::ElementId::from(UPDATE_CHIP_ID))
        })
        .map(|text| text.text().to_owned())
        .collect()
}

#[gpui_kit::test]
fn an_available_update_shows_a_chip_at_the_title_bars_top_right_and_installs_once(
    cx: &mut TestAppContext,
) {
    let fake = FakeUpdater::available("9.9.9");
    let (window, _shell, _state) = open(cx, connected(), Some(fake.clone()));
    assert_eq!(fake.checks.get(), 1, "one check at startup");
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let view = window.viewport_size();
        let bar = window.find(TITLE_BAR_ID).bounds();
        let chip = window.find(UPDATE_CHIP_ID);
        let b = chip.bounds();
        assert!(
            b.top() >= bar.top() && b.bottom() <= bar.bottom(),
            "{b:?} in {bar:?}"
        );
        assert!(b.size.height < bar.size.height, "a compact chip");
        // At the right edge, a small padding in.
        assert!(
            b.right() <= view.width - px(UPDATE_CHIP_MARGIN) + px(0.5),
            "{b:?}"
        );
        assert!(
            b.right() >= view.width - px(UPDATE_CHIP_MARGIN) - px(8.),
            "{b:?}"
        );
        // Clear of the traffic lights and the title text.
        let title = window.find(TITLE_TEXT_ID).bounds();
        assert!(b.left() > title.right(), "{b:?} vs {title:?}");
        assert!(b.left() > px(80.));
        assert_eq!(chip.label(), Some("Update available: v9.9.9"));
        assert_eq!(chip_text(window), ["Update available"]);
        // A pill on its own token, not the bar's.
        let native = dark_native();
        let color = painted_at(window, point(b.left() + px(6.), b.center().y));
        assert_eq!(color, token_background(&native, UPDATE_CHIP_TOKENS.0));
        assert_ne!(
            color,
            painted_at(window, point(px(400.), bar.center().y)),
            "differs from the bar"
        );
    })
    .unwrap();

    click(cx, window, UPDATE_CHIP_ID);
    click(cx, window, UPDATE_CHIP_ID);
    assert_eq!(*fake.installs.borrow(), ["9.9.9"], "exactly one install");
    let (label, disabled) = button_label(cx, window);
    assert_eq!(disabled, Some(true), "disabled while installing");
    assert_eq!(label.as_deref(), Some("Installing…: v9.9.9"));
    cx.update_window(window.into(), |_, window, _| {
        assert_eq!(chip_text(window), ["Installing…"]);
    })
    .unwrap();
    assert_eq!(fake.checks.get(), 1, "no further check");
}

#[gpui_kit::test]
fn the_status_bar_has_no_update_control(cx: &mut TestAppContext) {
    let fake = FakeUpdater::available("9.9.9");
    let (window, _shell, _state) = open(cx, connected(), Some(fake));
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let chip = window.find(UPDATE_CHIP_ID).bounds();
        let bar = window.find(STATUS_BAR_ID).bounds();
        assert!(chip.bottom() <= bar.top());
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_finished_install_leaves_a_restart_chip_and_a_transient_message(cx: &mut TestAppContext) {
    let fake = FakeUpdater::available("9.9.9");
    let (window, _shell, state) = open(cx, connected(), Some(fake.clone()));
    click(cx, window, UPDATE_CHIP_ID);
    assert_eq!(
        state.read_with(cx, |s, _| s.update_state().clone()),
        UpdateState::Installing {
            version: "9.9.9".into()
        }
    );
    fake.answer(Ok(()));
    cx.run_until_parked();
    assert_eq!(
        state.read_with(cx, |s, _| s.update_state().clone()),
        UpdateState::RestartRequired {
            version: "9.9.9".into()
        }
    );
    assert_eq!(
        message(cx, window).as_deref(),
        Some("Update v9.9.9 installed. Restart SmithAgents to finish.")
    );
    cx.update_window(window.into(), |_, window, _| {
        assert_eq!(chip_text(window), ["Restart to update"]);
    })
    .unwrap();
    // The message is transient; the chip stays and does nothing.
    cx.executor().advance_clock(INFO_FOR * 2);
    cx.run_until_parked();
    assert_eq!(message(cx, window), None);
    click(cx, window, UPDATE_CHIP_ID);
    assert_eq!(*fake.installs.borrow(), ["9.9.9"], "no second install");
    assert!(has(cx, window, UPDATE_CHIP_ID), "the chip stays");
    state.update(cx, |state, cx| state.check_for_updates(cx));
    cx.run_until_parked();
    assert_eq!(fake.checks.get(), 1, "no check after the install");
    assert!(matches!(
        state.read_with(cx, |s, _| s.update_state().clone()),
        UpdateState::RestartRequired { .. }
    ));
}

#[gpui_kit::test]
fn a_failed_install_shows_the_error_and_retries(cx: &mut TestAppContext) {
    let fake = FakeUpdater::available("9.9.9");
    let (window, _shell, state) = open(cx, connected(), Some(fake.clone()));
    click(cx, window, UPDATE_CHIP_ID);
    fake.answer(Err("boom".into()));
    cx.run_until_parked();
    assert_eq!(message(cx, window).as_deref(), Some("Update failed: boom"));
    assert_eq!(
        state.read_with(cx, |s, _| s.status_message()),
        Some(StatusMessage::error("Update failed: boom"))
    );
    assert!(has(cx, window, STATUS_ERROR_DOT_ID));
    assert_eq!(chip_text_of(cx, window), ["Update failed, retry"]);
    cx.update_window(window.into(), |_, window, _| {
        let dot = window.find(STATUS_ERROR_DOT_ID).bounds();
        assert_eq!(
            painted_at(window, dot.center()),
            token_background(&dark_native(), "status-error")
        );
    })
    .unwrap();
    let (label, disabled) = button_label(cx, window);
    assert_eq!(label.as_deref(), Some("Update failed, retry: v9.9.9"));
    assert_ne!(disabled, Some(true));

    click(cx, window, UPDATE_CHIP_ID);
    assert_eq!(*fake.installs.borrow(), ["9.9.9", "9.9.9"], "retried");
    fake.answer(Ok(()));
    cx.run_until_parked();
    assert_eq!(chip_text_of(cx, window), ["Restart to update"]);
}

fn chip_text_of(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Vec<String> {
    let mut text = vec![];
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        text = chip_text(window);
    })
    .unwrap();
    text
}

#[gpui_kit::test]
fn an_info_message_clears_itself_and_an_error_stays(cx: &mut TestAppContext) {
    let (window, _shell, state) = open(cx, connected(), None);
    set_status(cx, &state, StatusMessage::info("Hello"));
    assert_eq!(message(cx, window).as_deref(), Some("Hello"));
    cx.update_window(window.into(), |_, window, _| {
        assert_eq!(window.find(STATUS_MESSAGE_ID).role(), Some(Role::Status));
    })
    .unwrap();
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
}

impl AgentCommands for FakeCommands {
    fn create_agent(&self, _: RepoId) -> LocalBoxFuture<'static, Result<Agent, LinkError>> {
        let agent = self.created.borrow().clone().unwrap();
        async move { Ok(agent) }.boxed_local()
    }

    fn close_agent(&self, _: AgentId) -> LocalBoxFuture<'static, Result<(), LinkError>> {
        async { Err(LinkError::NotConnected) }.boxed_local()
    }

    fn add_repo(
        &self,
        _: &std::path::Path,
        _: AddSourceControl,
    ) -> LocalBoxFuture<'static, Result<Repo, LinkError>> {
        async { Err(LinkError::NotConnected) }.boxed_local()
    }

    fn set_source_control(
        &self,
        _: RepoId,
        _: bool,
    ) -> LocalBoxFuture<'static, Result<Repo, LinkError>> {
        async { Err(LinkError::NotConnected) }.boxed_local()
    }
}

#[gpui_kit::test]
fn creating_an_agent_sets_the_created_status(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let repo = Repo {
        id: RepoId::new(),
        path: "/src/app".into(),
        source_control: Some(SourceControl::Git {
            default_branch: "main".into(),
        }),
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
        source_control: Some(SourceControl::Git {
            default_branch: "main".into(),
        }),
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
        let version = window.find(STATUS_VERSION_ID).bounds();
        assert!(
            version.left() >= right.left(),
            "the version is not squeezed"
        );
        assert!(message.left() >= px(0.));
        assert!(message.size.height <= status_bar_height(window.rem_size()));
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

#[gpui_kit::test]
fn an_error_set_while_disconnected_is_gone_after_reconnect_but_an_info_stays(
    cx: &mut TestAppContext,
) {
    let (window, _shell, state) = open(cx, connected(), None);
    let connection = |cx: &mut TestAppContext, c: Connection| {
        state.update(cx, |state, cx| {
            state.dispatch(Action::ConnectionChanged(c), cx)
        });
    };
    connection(cx, Connection::Reconnecting);
    set_status(cx, &state, StatusMessage::error("stale"));
    connection(cx, Connection::Connected);
    assert_eq!(message(cx, window), None, "the stale error is gone");

    // An error set while connected survives a later drop and reconnect.
    set_status(cx, &state, StatusMessage::error("real"));
    connection(cx, Connection::Reconnecting);
    connection(cx, Connection::Connected);
    assert_eq!(message(cx, window).as_deref(), Some("real"));
    state.update(cx, |state, cx| state.clear_status(cx));

    // An info set while disconnected shows after the reconnect, until its timer.
    connection(cx, Connection::Reconnecting);
    set_status(cx, &state, StatusMessage::info("still here"));
    connection(cx, Connection::Connected);
    assert_eq!(message(cx, window).as_deref(), Some("still here"));
    cx.executor()
        .advance_clock(INFO_FOR + Duration::from_millis(10));
    cx.run_until_parked();
    assert_eq!(message(cx, window), None);
}

#[gpui_kit::test]
fn a_new_updater_starts_the_update_state_over(cx: &mut TestAppContext) {
    let first = FakeUpdater::available("1.0.0");
    let (window, _shell, state) = open(cx, connected(), Some(first.clone()));
    click(cx, window, UPDATE_CHIP_ID);
    assert!(matches!(
        state.read_with(cx, |s, _| s.update_state().clone()),
        UpdateState::Installing { .. }
    ));
    let second = FakeUpdater::new(UpdateState::UpToDate);
    state.update(cx, |state, cx| state.set_updater(second.clone(), cx));
    cx.run_until_parked();
    assert_eq!(
        state.read_with(cx, |s, _| s.update_state().clone()),
        UpdateState::UpToDate
    );
    assert_eq!(second.checks.get(), 1);
    assert!(!has(cx, window, UPDATE_CHIP_ID));
    // The old install's answer arrives late and changes nothing.
    first.answer(Ok(()));
    cx.run_until_parked();
    assert_eq!(
        state.read_with(cx, |s, _| s.update_state().clone()),
        UpdateState::UpToDate
    );
}

#[gpui_kit::test]
fn a_multi_line_error_shows_on_one_line(cx: &mut TestAppContext) {
    let (window, _shell, state) = open(cx, connected(), None);
    set_status(
        cx,
        &state,
        StatusMessage::error("line one\n\n  line two\r\n\tthree"),
    );
    assert_eq!(
        message(cx, window).as_deref(),
        Some("line one line two three")
    );
}

/// The painted fill of the status bar and of the update chip.
fn bar_and_chip_colors(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
) -> (gpui_kit::Background, gpui_kit::Background) {
    let mut colors = None;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let bar = window.find(STATUS_BAR_ID).bounds();
        let chip = window.find(UPDATE_CHIP_ID).bounds();
        colors = Some((
            painted_at(window, point(px(300.), bar.center().y)),
            painted_at(window, point(chip.left() + px(6.), chip.center().y)),
        ));
    })
    .unwrap();
    colors.unwrap()
}

#[gpui_kit::test]
fn the_status_bar_and_the_update_chip_paint_their_tokens_in_both_themes(cx: &mut TestAppContext) {
    let fake = FakeUpdater::available("9.9.9");
    let (window, _shell, state) = open(cx, connected(), Some(fake));
    let set = ThemeSet::embedded();
    for appearance in [Appearance::Dark, Appearance::Light] {
        let theme = theme_state(appearance).theme;
        state.update(cx, |state, cx| {
            state.set_appearance(
                AppearanceSettings {
                    theme: ThemeChoice::Id(theme.clone()),
                    ..AppearanceSettings::default()
                },
                cx,
            )
        });
        let native = set.native(&theme).unwrap();
        let (bar, chip) = bar_and_chip_colors(cx, window);
        assert_eq!(bar, token_background(&native, "card-2"), "{theme}: bar");
        // A primary pill, so it reads as a control on the card-2 bar.
        assert_eq!(chip, token_background(&native, "primary"), "{theme}: chip");
        assert_ne!(chip, bar, "{theme}");
    }
}

#[gpui_kit::test]
fn at_the_minimum_window_size_the_title_and_the_widest_chip_do_not_collide(
    cx: &mut TestAppContext,
) {
    // The main window cannot be made smaller than this.
    let min = smithagents_app::main_window_options(None, None)
        .window_min_size
        .expect("the main window has a minimum size");
    assert_eq!(min, MIN_WINDOW_SIZE);
    assert_eq!((min.width, min.height), (px(640.), px(400.)));

    // The longest chip label, at the largest text size.
    let fake = FakeUpdater::new(UpdateState::Failed {
        version: "10.20.30".into(),
        message: "no".into(),
    });
    let options = ShellOptions {
        model: connected(),
        updater: Some(fake as Rc<dyn Updater>),
        ..Default::default()
    };
    let (window, shell) = open_shell_options(cx, options, (640., 400.));
    cx.run_until_parked();
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    for scale in [1.0, 1.5] {
        state.update(cx, |state, cx| {
            let mut settings = state.appearance_settings().clone();
            settings.text_scale = scale;
            state.set_appearance(settings, cx)
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            let bar = window.find(TITLE_BAR_ID).bounds();
            let title = window.find(TITLE_TEXT_ID).bounds();
            let chip = window.find(UPDATE_CHIP_ID).bounds();
            assert_eq!(bar.size.width, px(640.));
            assert!(
                title.right() < chip.left(),
                "at {scale}: title {title:?} meets chip {chip:?}"
            );
            assert!(chip.right() <= bar.right(), "{chip:?} in {bar:?}");
            assert_eq!(chip_text(window), ["Update failed, retry"]);
        })
        .unwrap();
    }
}

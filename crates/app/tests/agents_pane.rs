//! Headless checks of the agents pane (the left pane's content) in both of
//! its modes, through gpui-kit's `test-support`. Agent commands go to a fake,
//! so no daemon runs.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use common::ui::{
    agent, dark_native, open_shell, open_shell_options, painted_at, quad_at, theme_state,
    token_background,
};
use futures::channel::oneshot;
use futures::future::LocalBoxFuture;
use futures::FutureExt as _;
use gpui_kit::base::{test_support, Root};
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::{TestEventExt as _, TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{
    point, px, AppContext as _, Background, Bounds, ElementId, Entity, MouseButton, Pixels, Quad,
    Role, ScrollDelta, TestAppContext, WindowHandle,
};
use smithagents_app::app_state::{AppState, AppStateStore};
use smithagents_app::layout_store::COMPACT_LEFT_PANE_WIDTH;
use smithagents_app::link::{AgentCommands, LinkError};
use smithagents_app::model::{Action, Connection, Effect, ShellModel};
use smithagents_app::shell::{ShellOptions, LEFT_PANE_ID};
use smithagents_app::shell_state::ShellState;
use smithagents_app::theme_adapter::{agent_accent, region_tokens, srgb_to_hsla, Region};
use smithagents_app::view_models::{
    LeftPaneMode, DAEMON_SILENT_TEXT, NEED_REPO_TEXT, NOT_CONNECTED_TEXT,
};
use smithagents_app::views::agents::{
    agent_marker_id, agent_row_id, repo_choice_id, ACCENT_BAR_ID, AGENTS_EMPTY_ID, AGENT_LIST_ID,
    CANCEL_CLOSE_ID, CLOSE_AGENT_LABEL, CLOSE_TITLE_ID, CONFIRM_CLOSE_ID, LEFT_PANE_CONTENT_ID,
    NEW_AGENT_ID, NOTICE_ID, NO_AGENTS_TEXT, STATUS_DOT_ID, SWATCH_ID,
};
use smithagents_app::views::rail::RailEntry;
use smithagents_client::ClientError;
use smithagents_core::agent::{Agent, AgentState, Repo};
use smithagents_core::event::{Event, EventKind};
use smithagents_core::ids::{AgentId, RepoId};
use smithagents_core::palette::AgentColor;
use smithagents_theme::tokens::{Appearance, ThemeSet};

/// Agent commands that record each call. A create answers with the next
/// queued result, or waits until the test answers it with
/// [`FakeCommands::answer_create`] (or forever); a close answers with the next
/// queued result, else `Ok`, unless `hold_closes` keeps it waiting forever.
#[derive(Default)]
struct FakeCommands {
    creates: RefCell<Vec<RepoId>>,
    closes: RefCell<Vec<AgentId>>,
    create_results: RefCell<VecDeque<Result<Agent, LinkError>>>,
    close_results: RefCell<VecDeque<Result<(), LinkError>>>,
    hold_closes: Cell<bool>,
    pending: RefCell<Vec<oneshot::Sender<Result<Agent, LinkError>>>>,
}

impl FakeCommands {
    fn answer_create(&self, result: Result<Agent, LinkError>) {
        let sender = self.pending.borrow_mut().remove(0);
        let _ = sender.send(result);
    }
}

impl AgentCommands for FakeCommands {
    fn create_agent(&self, repo_id: RepoId) -> LocalBoxFuture<'static, Result<Agent, LinkError>> {
        self.creates.borrow_mut().push(repo_id);
        if let Some(result) = self.create_results.borrow_mut().pop_front() {
            return async move { result }.boxed_local();
        }
        let (tx, rx) = oneshot::channel();
        self.pending.borrow_mut().push(tx);
        async move { rx.await.unwrap_or(Err(LinkError::Stopped)) }.boxed_local()
    }

    fn close_agent(&self, id: AgentId) -> LocalBoxFuture<'static, Result<(), LinkError>> {
        self.closes.borrow_mut().push(id);
        if self.hold_closes.get() {
            return futures::future::pending().boxed_local();
        }
        let result = self
            .close_results
            .borrow_mut()
            .pop_front()
            .unwrap_or(Ok(()));
        async move { result }.boxed_local()
    }
}

/// An agent created `minutes` after the fixture start, with `hue` and `state`.
fn agent_with(name: &str, minutes: i64, hue: f32, state: AgentState) -> Agent {
    Agent {
        created_at: agent(name).created_at + chrono::Duration::minutes(minutes),
        color: AgentColor {
            l: 0.5,
            c: 0.2,
            h: hue,
        },
        state,
        ..agent(name)
    }
}

fn repo(path: &str, minutes: i64) -> Repo {
    let at: DateTime<Utc> = "2026-11-20T09:00:00Z".parse().unwrap();
    Repo {
        id: RepoId::new(),
        path: path.into(),
        default_branch: "main".into(),
        created_at: at + chrono::Duration::minutes(minutes),
    }
}

/// A connected model with these agents and repositories.
fn connected(agents: Vec<Agent>, repos: Vec<Repo>) -> ShellModel {
    let mut model = ShellModel::new();
    model.apply(Action::Snapshot {
        agents,
        repos,
        latest_seq: 1,
    });
    model.apply(Action::ConnectionChanged(Connection::Connected));
    model
}

/// Opens the shell on `model` with `fake` for its agent commands.
fn open_with_commands(
    cx: &mut TestAppContext,
    model: ShellModel,
    fake: &Rc<FakeCommands>,
    app_state: Option<AppStateStore>,
) -> (WindowHandle<Root>, Entity<ShellState>) {
    open_pane(
        cx,
        ShellOptions {
            model,
            commands: Some(fake.clone()),
            app_state,
            ..Default::default()
        },
    )
}

/// Opens the shell from `options` in a 1200x800 window and draws a frame.
fn open_pane(
    cx: &mut TestAppContext,
    options: ShellOptions,
) -> (WindowHandle<Root>, Entity<ShellState>) {
    let (window, shell) = open_shell_options(cx, options, (1200., 800.));
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    (window, state)
}

fn selected(cx: &mut TestAppContext, state: &Entity<ShellState>) -> Option<AgentId> {
    state.read_with(cx, |state, _| state.model().selected())
}

/// The text painted in the last frame inside the element `id`.
fn texts_within(window: &gpui_kit::Window, id: &ElementId) -> Vec<(String, Bounds<Pixels>)> {
    test_support::texts(window)
        .into_iter()
        .filter(|text| text.path().contains(id))
        .map(|text| (text.text().to_owned(), text.bounds()))
        .collect()
}

/// Sends the daemon's "agent created" event for `agent`.
fn agent_created(cx: &mut TestAppContext, state: &Entity<ShellState>, agent: &Agent, seq: u64) {
    state.update(cx, |state, cx| {
        state.dispatch(
            Action::Event(Event {
                seq,
                at: agent.created_at,
                kind: EventKind::AgentCreated(agent.clone()),
            }),
            cx,
        )
    });
}

/// The pane's notification text, if one is showing.
fn notice(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Option<String> {
    let mut text = None;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        text = window
            .try_find(NOTICE_ID)
            .and_then(|notice| notice.value().map(str::to_owned));
    })
    .unwrap();
    text
}

#[gpui_kit::test]
fn the_expanded_pane_shows_a_row_per_agent_with_its_name(cx: &mut TestAppContext) {
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Idle);
    let venus = agent_with("Venus", 1, 260.0, AgentState::Working);
    let fake = Rc::new(FakeCommands::default());
    let (window, _) = open_with_commands(
        cx,
        connected(vec![venus.clone(), mercury.clone()], vec![]),
        &fake,
        None,
    );
    cx.update_window(window.into(), |_, window, _| {
        assert!(window.try_find(AGENTS_EMPTY_ID).is_none());
        let rows = window.find_all_by_role(Role::ListBoxOption);
        let labels: Vec<_> = rows.iter().map(|row| row.label().unwrap()).collect();
        assert_eq!(labels, ["Mercury, idle", "Venus, working"], "oldest first");
        for (agent, state) in [(&mercury, "idle"), (&venus, "working")] {
            let id = agent_row_id(agent.id);
            let row = window.find(id.clone());
            assert!(row.visible());
            assert_eq!(row.selected(), Some(false));
            let texts = texts_within(window, &id);
            let shown: Vec<&str> = texts.iter().map(|(text, _)| text.as_str()).collect();
            assert_eq!(shown, [agent.name.as_str(), state], "name and state words");
            for (text, bounds) in &texts {
                assert!(
                    row.bounds().contains(&bounds.origin),
                    "{text} is drawn in its row"
                );
            }
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn an_empty_pane_says_no_agents_yet(cx: &mut TestAppContext) {
    let fake = Rc::new(FakeCommands::default());
    let (window, _) = open_with_commands(cx, connected(vec![], vec![]), &fake, None);
    cx.update_window(window.into(), |_, window, _| {
        let hint = window.find(AGENTS_EMPTY_ID);
        assert!(hint.visible());
        assert_eq!(hint.role(), Some(Role::Label));
        assert_eq!(hint.value(), Some(NO_AGENTS_TEXT));
        assert!(window.find_all_by_role(Role::ListBoxOption).is_empty());
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_click_selects_a_row_and_a_double_click_opens_its_tab(cx: &mut TestAppContext) {
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Idle);
    let venus = agent_with("Venus", 1, 260.0, AgentState::Idle);
    let fake = Rc::new(FakeCommands::default());
    let (window, state) = open_with_commands(
        cx,
        connected(vec![mercury.clone(), venus.clone()], vec![]),
        &fake,
        None,
    );

    cx.update_window(window.into(), |_, window, cx| {
        window.click(agent_row_id(venus.id), cx)
    })
    .unwrap();
    assert_eq!(selected(cx, &state), Some(venus.id));
    state.read_with(cx, |state, _| {
        assert!(state.model().tabs().is_empty(), "a click only selects");
        assert!(state.pending_effects().is_empty());
    });
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(agent_row_id(venus.id)).selected(), Some(true));
        assert_eq!(
            window.find(agent_row_id(mercury.id)).selected(),
            Some(false)
        );
        window.double_click(agent_row_id(mercury.id), cx);
    })
    .unwrap();
    state.read_with(cx, |state, _| {
        assert_eq!(state.model().selected(), Some(mercury.id));
        assert_eq!(state.model().tabs(), [mercury.id]);
        assert_eq!(
            state.pending_effects().iter().cloned().collect::<Vec<_>>(),
            vec![Effect::OpenCenterTab(mercury.id)]
        );
    });
}

#[gpui_kit::test]
fn arrows_move_the_selection_and_enter_opens_the_selected_row(cx: &mut TestAppContext) {
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Idle);
    let venus = agent_with("Venus", 1, 260.0, AgentState::Idle);
    let earth = agent_with("Earth", 2, 300.0, AgentState::Idle);
    let fake = Rc::new(FakeCommands::default());
    let (window, state) = open_with_commands(
        cx,
        connected(vec![mercury.clone(), venus.clone(), earth.clone()], vec![]),
        &fake,
        None,
    );

    cx.update_window(window.into(), |_, window, cx| {
        window.click(agent_row_id(mercury.id), cx);
        window.press("down", cx);
        window.press("down", cx);
        window.press("down", cx);
    })
    .unwrap();
    assert_eq!(selected(cx, &state), Some(earth.id), "stops at the last");
    cx.update_window(window.into(), |_, window, cx| {
        window.press("up", cx);
        assert_eq!(
            window.find(agent_row_id(venus.id)).focused(),
            Some(true),
            "the focus follows the selection"
        );
        window.press("enter", cx);
    })
    .unwrap();
    state.read_with(cx, |state, _| {
        assert_eq!(state.model().selected(), Some(venus.id));
        assert_eq!(state.model().tabs(), [venus.id], "Enter opened Venus");
        assert_eq!(
            state.pending_effects().iter().cloned().collect::<Vec<_>>(),
            vec![Effect::OpenCenterTab(venus.id)]
        );
    });
}

#[gpui_kit::test]
fn a_row_paints_its_agent_color_and_its_status_token(cx: &mut TestAppContext) {
    let agents = [
        agent_with("Mercury", 0, 200.0, AgentState::Working),
        agent_with("Venus", 1, 260.0, AgentState::Done),
        agent_with("Earth", 2, 300.0, AgentState::Dead),
        agent_with("Mars", 3, 330.0, AgentState::Quiet),
    ];
    // Quiet: the working color, dimmed.
    let tokens = ["status-info", "status-ok", "status-error", "status-info"];
    let fake = Rc::new(FakeCommands::default());
    let (window, _) = open_with_commands(cx, connected(agents.to_vec(), vec![]), &fake, None);
    let native = dark_native();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        for (agent, token) in agents.iter().zip(tokens) {
            let id = agent_row_id(agent.id);
            let bar = window.within(id.clone()).find(ACCENT_BAR_ID).bounds();
            assert!(bar.size.width > px(0.) && bar.size.height > px(0.));
            assert_eq!(
                painted_at(window, bar.center()),
                Background::from(srgb_to_hsla(agent_accent(&native, agent.color.h))),
                "{}'s accent bar",
                agent.name
            );
            let dot = window.within(id).find(STATUS_DOT_ID).bounds();
            let color = srgb_to_hsla(native.colors[token]);
            let want = if agent.state == AgentState::Quiet {
                color.opacity(0.5)
            } else {
                color
            };
            assert_eq!(
                painted_at(window, dot.center()),
                Background::from(want),
                "{}'s status dot",
                agent.name
            );
        }
    })
    .unwrap();
}

/// Switches the left pane to compact and renders.
fn compact(cx: &mut TestAppContext, window: WindowHandle<Root>, state: &Entity<ShellState>) {
    state.update(cx, |state, cx| {
        state.set_left_pane_mode(LeftPaneMode::Compact, cx)
    });
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

#[gpui_kit::test]
fn the_compact_strip_shows_a_marker_per_agent_and_nothing_overflows_it(cx: &mut TestAppContext) {
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Working);
    let mars = agent_with("Mars", 1, 260.0, AgentState::Dead);
    let fake = Rc::new(FakeCommands::default());
    let mut model = connected(vec![mercury.clone(), mars.clone()], vec![]);
    model.apply(Action::SelectAgent(mars.id));
    let (window, state) = open_with_commands(cx, model, &fake, None);
    compact(cx, window, &state);
    let native = dark_native();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let strip = window.find(LEFT_PANE_ID).bounds();
        assert_eq!(strip.size.width, COMPACT_LEFT_PANE_WIDTH);
        assert!(window.try_find(agent_row_id(mercury.id)).is_none());

        // One marker each, told apart by the label though the initials match.
        let markers = window.find_all_by_role(Role::ListBoxOption);
        let labels: Vec<_> = markers.iter().map(|m| m.label().unwrap()).collect();
        assert_eq!(labels, ["Mercury, working", "Mars, stopped unexpectedly"]);
        for (agent, initial) in [(&mercury, "M"), (&mars, "M")] {
            let id = agent_marker_id(agent.id);
            let marker = window.find(id.clone());
            assert!(marker.visible());
            let shown: Vec<String> = texts_within(window, &id)
                .into_iter()
                .map(|(text, _)| text)
                .collect();
            assert_eq!(shown, [initial], "only the initial is drawn");
            let swatch = window.within(id.clone()).find(SWATCH_ID).bounds();
            assert_eq!(
                painted_at(window, swatch.origin + point(px(6.), px(6.))),
                Background::from(srgb_to_hsla(agent_accent(&native, agent.color.h))),
                "{}'s marker shows its color",
                agent.name
            );
        }
        let dot = window
            .within(agent_marker_id(mars.id))
            .find(STATUS_DOT_ID)
            .bounds();
        assert_eq!(
            painted_at(window, dot.center()),
            token_background(&native, "status-error")
        );
        assert_eq!(window.find(agent_marker_id(mars.id)).selected(), Some(true));
        assert_eq!(
            window.find(agent_marker_id(mercury.id)).selected(),
            Some(false)
        );

        // Nothing the pane draws reaches past the strip: every text, and
        // every quad clipped to the pane, lies inside it.
        let pane = window.find(LEFT_PANE_CONTENT_ID).bounds();
        let left = f32::from(strip.left());
        let right = f32::from(strip.right());
        for text in test_support::texts(window)
            .into_iter()
            .filter(|text| text.path().contains(&ElementId::from(LEFT_PANE_CONTENT_ID)))
        {
            let bounds = text.bounds();
            assert!(
                f32::from(bounds.left()) >= left && f32::from(bounds.right()) <= right,
                "text {:?} at {bounds:?} leaves the strip {strip:?}",
                text.text()
            );
        }
        let scale = window.scale_factor();
        let inside = |b: &Bounds<gpui_kit::ScaledPixels>| {
            b.origin.x.0 >= f32::from(pane.left()) * scale - 0.5
                && b.origin.x.0 + b.size.width.0 <= f32::from(pane.right()) * scale + 0.5
                && b.origin.y.0 >= f32::from(pane.top()) * scale - 0.5
                && b.origin.y.0 + b.size.height.0 <= f32::from(pane.bottom()) * scale + 0.5
        };
        let pane_quads: Vec<_> = window
            .painted_quads()
            .into_iter()
            .filter(|quad| inside(&quad.content_mask.bounds))
            .collect();
        assert!(
            pane_quads.len() >= 6,
            "markers, swatches and dots are painted"
        );
        for quad in pane_quads {
            let (x, w) = (
                quad.bounds.origin.x.0 / scale,
                quad.bounds.size.width.0 / scale,
            );
            assert!(
                x >= left - 0.5 && x + w <= right + 0.5,
                "a quad at x {x}..{} leaves the strip {left}..{right}",
                x + w
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_marker_selects_on_click_and_opens_on_double_click_or_enter(cx: &mut TestAppContext) {
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Idle);
    let mars = agent_with("Mars", 1, 260.0, AgentState::Idle);
    let fake = Rc::new(FakeCommands::default());
    let (window, state) = open_with_commands(
        cx,
        connected(vec![mercury.clone(), mars.clone()], vec![]),
        &fake,
        None,
    );
    compact(cx, window, &state);

    cx.update_window(window.into(), |_, window, cx| {
        window.click(agent_marker_id(mars.id), cx);
        assert_eq!(
            window.find(agent_marker_id(mars.id)).focused(),
            Some(true),
            "a marker takes the keyboard focus"
        );
    })
    .unwrap();
    assert_eq!(selected(cx, &state), Some(mars.id));
    assert!(state.read_with(cx, |state, _| state.model().tabs().is_empty()));

    cx.update_window(window.into(), |_, window, cx| {
        window.double_click(agent_marker_id(mercury.id), cx);
    })
    .unwrap();
    state.read_with(cx, |state, _| {
        assert_eq!(state.model().tabs(), [mercury.id]);
        assert_eq!(state.model().selected(), Some(mercury.id));
    });

    cx.update_window(window.into(), |_, window, cx| {
        window.press("down", cx);
        assert_eq!(window.find(agent_marker_id(mars.id)).focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    state.read_with(cx, |state, _| {
        assert_eq!(
            state.model().tabs(),
            [mercury.id, mars.id],
            "Enter opened Mars"
        );
        assert_eq!(state.model().active_tab(), Some(mars.id));
    });
}

#[gpui_kit::test]
fn hovering_a_marker_shows_its_name_and_state(cx: &mut TestAppContext) {
    // Same initial: the tooltips tell them apart.
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Working);
    let mars = agent_with("Mars", 1, 260.0, AgentState::Idle);
    let fake = Rc::new(FakeCommands::default());
    let (window, state) = open_with_commands(
        cx,
        connected(vec![mercury.clone(), mars.clone()], vec![]),
        &fake,
        None,
    );
    compact(cx, window, &state);
    let shown = |window: &gpui_kit::Window, tooltip: &str| {
        test_support::texts(window)
            .iter()
            .any(|text| text.text() == tooltip)
    };
    cx.update_window(window.into(), |_, window, _| {
        assert!(
            !shown(window, "Mercury, working"),
            "no tooltip before hovering"
        );
    })
    .unwrap();
    for (agent, tooltip) in [(&mercury, "Mercury, working"), (&mars, "Mars, idle")] {
        cx.update_window(window.into(), |_, window, cx| {
            window.hover(agent_marker_id(agent.id), cx)
        })
        .unwrap();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(shown(window, tooltip), "the tooltip says {tooltip:?}");
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn new_agent_without_a_repository_shows_the_settings_dock(cx: &mut TestAppContext) {
    let fake = Rc::new(FakeCommands::default());
    let (window, shell) = open_shell_options(
        cx,
        ShellOptions {
            model: connected(vec![], vec![]),
            commands: Some(fake.clone()),
            ..Default::default()
        },
        (1200., 800.),
    );
    let dock = shell.read_with(cx, |shell, _| shell.dock().clone());
    let open = |cx: &mut TestAppContext| {
        dock.read_with(cx, |dock, _| dock.is_dock_open(DockPlacement::Right))
    };
    // Hide the right dock first.
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(RailEntry::Settings.element_id(), cx);
    })
    .unwrap();
    assert!(!open(cx));

    cx.update_window(window.into(), |_, window, cx| {
        window.click(NEW_AGENT_ID, cx)
    })
    .unwrap();
    assert!(open(cx), "the Settings dock is shown");
    assert_eq!(notice(cx, window).as_deref(), Some(NEED_REPO_TEXT));
    assert!(fake.creates.borrow().is_empty());
}

#[gpui_kit::test]
fn one_repository_creates_once_even_when_clicked_twice_quickly(cx: &mut TestAppContext) {
    let only = repo("/src/app", 0);
    let fake = Rc::new(FakeCommands::default());
    let (window, state) =
        open_with_commands(cx, connected(vec![], vec![only.clone()]), &fake, None);

    cx.update_window(window.into(), |_, window, cx| {
        window.click(NEW_AGENT_ID, cx);
        window.click(NEW_AGENT_ID, cx);
        window.double_click(NEW_AGENT_ID, cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(*fake.creates.borrow(), [only.id], "exactly one create");
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(NEW_AGENT_ID).disabled(),
            Some(true),
            "disabled while the create runs"
        );
    })
    .unwrap();

    // The daemon answers; the agent itself arrives later as an event.
    let created = agent_with("Mercury", 5, 200.0, AgentState::Idle);
    fake.answer_create(Ok(created.clone()));
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_ne!(window.find(NEW_AGENT_ID).disabled(), Some(true));
        assert!(
            window.try_find(agent_row_id(created.id)).is_none(),
            "not inserted locally"
        );
    })
    .unwrap();
    assert_eq!(selected(cx, &state), None);

    agent_created(cx, &state, &created, 2);
    cx.run_until_parked();
    assert_eq!(
        selected(cx, &state),
        Some(created.id),
        "selected when it appears"
    );
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(agent_row_id(created.id)).selected(), Some(true));
    })
    .unwrap();

    // A second create works once the first is done.
    cx.update_window(window.into(), |_, window, cx| {
        window.click(NEW_AGENT_ID, cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(fake.creates.borrow().len(), 2);
}

#[gpui_kit::test]
fn a_created_agent_does_not_take_the_selection_from_the_users_choice(cx: &mut TestAppContext) {
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Idle);
    let only = repo("/src/app", 0);
    let fake = Rc::new(FakeCommands::default());
    let (window, state) = open_with_commands(
        cx,
        connected(vec![mercury.clone()], vec![only.clone()]),
        &fake,
        None,
    );
    let venus = agent_with("Venus", 5, 260.0, AgentState::Idle);
    fake.create_results
        .borrow_mut()
        .push_back(Ok(venus.clone()));
    cx.update_window(window.into(), |_, window, cx| {
        window.click(NEW_AGENT_ID, cx)
    })
    .unwrap();
    cx.run_until_parked();
    // The user picks Mercury before Venus shows up.
    cx.update_window(window.into(), |_, window, cx| {
        window.click(agent_row_id(mercury.id), cx)
    })
    .unwrap();
    agent_created(cx, &state, &venus, 2);
    cx.run_until_parked();
    assert_eq!(selected(cx, &state), Some(mercury.id));
}

#[gpui_kit::test]
fn a_failed_create_shows_the_daemons_message(cx: &mut TestAppContext) {
    let only = repo("/src/app", 0);
    let fake = Rc::new(FakeCommands::default());
    let (window, _) = open_with_commands(cx, connected(vec![], vec![only]), &fake, None);
    fake.create_results
        .borrow_mut()
        .push_back(Err(LinkError::Client(ClientError::Api {
            status: 409,
            code: "worktree_failed".into(),
            message: "cannot create the worktree: branch agent/mercury exists".into(),
        })));
    cx.update_window(window.into(), |_, window, cx| {
        window.click(NEW_AGENT_ID, cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        notice(cx, window).as_deref(),
        Some("cannot create the worktree: branch agent/mercury exists")
    );

    fake.create_results
        .borrow_mut()
        .push_back(Err(LinkError::NotConnected));
    cx.update_window(window.into(), |_, window, cx| {
        window.click(NEW_AGENT_ID, cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        notice(cx, window).as_deref(),
        Some("Not connected to the daemon")
    );
}

#[gpui_kit::test]
fn an_offline_shell_says_it_is_not_connected(cx: &mut TestAppContext) {
    // No link and no commands: nothing can create.
    let (window, _) = open_shell(cx, connected(vec![], vec![repo("/src/app", 0)]));
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(NEW_AGENT_ID, cx);
    })
    .unwrap();
    assert_eq!(notice(cx, window).as_deref(), Some(NOT_CONNECTED_TEXT));
}

#[gpui_kit::test]
fn several_repositories_open_a_picker_with_the_last_used_first(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let store = AppStateStore::new(home.path());
    let (older, newer) = (repo("/src/older", 0), repo("/src/newer", 10));
    store
        .save(&AppState {
            last_repo_id: Some(older.id),
        })
        .unwrap();
    let fake = Rc::new(FakeCommands::default());
    let (window, state) = open_with_commands(
        cx,
        connected(vec![], vec![newer.clone(), older.clone()]),
        &fake,
        Some(store.clone()),
    );

    cx.update_window(window.into(), |_, window, cx| {
        window.click(NEW_AGENT_ID, cx);
        window.render_frame(cx);
        let mut menu = window.within("popup-menu");
        let first = menu.find(repo_choice_id(0));
        assert_eq!(
            (first.role(), first.label()),
            (Some(Role::Group), Some("older, /src/older")),
            "the last used first, as a named node"
        );
        assert_eq!(
            menu.find(repo_choice_id(1)).label(),
            Some("newer, /src/newer")
        );
        menu.click(repo_choice_id(1), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(*fake.creates.borrow(), [newer.id]);
    assert_eq!(
        state.read_with(cx, |state, _| state.last_repo_id()),
        Some(newer.id)
    );
    assert_eq!(store.load().last_repo_id, Some(newer.id), "saved");
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none(), "the picker closed");
    })
    .unwrap();
}

/// Right-clicks the item `id`, chooses "Close agent", and lets the
/// confirmation open.
fn ask_to_close(cx: &mut TestAppContext, window: WindowHandle<Root>, id: ElementId) {
    cx.update_window(window.into(), |_, window, cx| window.right_click(id, cx))
        .unwrap();
    // The context menu is built after the event, deferred.
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let item = window.find_by_label(CLOSE_AGENT_LABEL).bounds().center();
        window.pointer_down(item, MouseButton::Left, cx);
        window.pointer_up(item, MouseButton::Left, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

fn dialog_open(cx: &mut TestAppContext, window: WindowHandle<Root>) -> bool {
    let mut open = false;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        open = window.has_active_dialog(cx);
    })
    .unwrap();
    open
}

#[gpui_kit::test]
fn the_close_confirmation_names_the_agent_and_enter_cancels(cx: &mut TestAppContext) {
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Idle);
    let fake = Rc::new(FakeCommands::default());
    let (window, state) =
        open_with_commands(cx, connected(vec![mercury.clone()], vec![]), &fake, None);

    ask_to_close(cx, window, agent_row_id(mercury.id));
    assert!(dialog_open(cx, window), "a confirmation is showing");
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(
            window.find(CLOSE_TITLE_ID).value(),
            Some("Close Mercury?"),
            "the title names the agent"
        );
        assert_eq!(
            window.find(CANCEL_CLOSE_ID).focused(),
            Some(true),
            "Cancel is the default"
        );
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(fake.closes.borrow().is_empty(), "Enter closes nothing");
    assert!(
        !dialog_open(cx, window),
        "Enter dismissed the dialog, like Cancel"
    );

    // Escape and Cancel dismiss too.
    ask_to_close(cx, window, agent_row_id(mercury.id));
    cx.update_window(window.into(), |_, window, cx| window.press("escape", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!dialog_open(cx, window));
    ask_to_close(cx, window, agent_row_id(mercury.id));
    cx.update_window(window.into(), |_, window, cx| {
        window.click(CANCEL_CLOSE_ID, cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!dialog_open(cx, window));
    assert!(fake.closes.borrow().is_empty(), "nothing closed");
    assert_eq!(
        state.read_with(cx, |state, _| state.model().agents().len()),
        1
    );
}

#[gpui_kit::test]
fn close_agent_closes_once_even_when_asked_again_while_in_flight(cx: &mut TestAppContext) {
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Idle);
    let fake = Rc::new(FakeCommands::default());
    // The daemon takes its time: the first close stays in flight.
    fake.hold_closes.set(true);
    let (window, state) =
        open_with_commands(cx, connected(vec![mercury.clone()], vec![]), &fake, None);

    ask_to_close(cx, window, agent_row_id(mercury.id));
    cx.update_window(window.into(), |_, window, cx| {
        window.double_click(CONFIRM_CLOSE_ID, cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(*fake.closes.borrow(), [mercury.id]);
    assert!(!dialog_open(cx, window));

    // Asked again, from the compact strip, before the daemon answered.
    compact(cx, window, &state);
    ask_to_close(cx, window, agent_marker_id(mercury.id));
    cx.update_window(window.into(), |_, window, cx| {
        window.click(CONFIRM_CLOSE_ID, cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(*fake.closes.borrow(), [mercury.id], "sent exactly once");
    // The agent leaves only when the daemon says so.
    assert_eq!(
        state.read_with(cx, |state, _| state.model().agents().len()),
        1
    );
}

#[gpui_kit::test]
fn a_failed_close_shows_the_daemons_message_and_keeps_the_row(cx: &mut TestAppContext) {
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Working);
    let fake = Rc::new(FakeCommands::default());
    fake.close_results
        .borrow_mut()
        .push_back(Err(LinkError::Client(ClientError::Api {
            status: 409,
            code: "turn_in_progress".into(),
            message: "a turn is already in progress for this agent".into(),
        })));
    let (window, _) = open_with_commands(cx, connected(vec![mercury.clone()], vec![]), &fake, None);
    ask_to_close(cx, window, agent_row_id(mercury.id));
    cx.update_window(window.into(), |_, window, cx| {
        window.click(CONFIRM_CLOSE_ID, cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(*fake.closes.borrow(), [mercury.id]);
    assert_eq!(
        notice(cx, window).as_deref(),
        Some("a turn is already in progress for this agent")
    );
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find(agent_row_id(mercury.id)).visible(),
            "the row stays"
        );
    })
    .unwrap();
}

/// The topmost quads painted at the center of the status dot inside `item`
/// and 1 px inside its left edge (on a hollow dot's ring).
fn dot_quads(window: &mut gpui_kit::Window, item: ElementId) -> (Quad, Quad) {
    let dot = window.within(item).find(STATUS_DOT_ID).bounds();
    let edge = point(dot.left() + px(1.), dot.center().y);
    (quad_at(window, dot.center()), quad_at(window, edge))
}

#[gpui_kit::test]
fn idle_is_a_hollow_ring_and_working_a_filled_dot(cx: &mut TestAppContext) {
    let idle = agent_with("Mercury", 0, 200.0, AgentState::Idle);
    let working = agent_with("Venus", 1, 260.0, AgentState::Working);
    let fake = Rc::new(FakeCommands::default());
    let (window, state) = open_with_commands(
        cx,
        connected(vec![idle.clone(), working.clone()], vec![]),
        &fake,
        None,
    );
    let native = dark_native();
    let muted = srgb_to_hsla(native.colors["muted-foreground"]);
    let info = token_background(&native, "status-info");
    // Neither row is selected and a marker's dot sits on a surface-colored
    // disc, so a hole shows the pane's surface.
    let surface = token_background(&native, region_tokens(Region::LeftPane).0);
    let check = |window: &mut gpui_kit::Window, idle_item: ElementId, working_item: ElementId| {
        let (center, edge) = dot_quads(window, idle_item);
        assert_eq!(
            center.background, surface,
            "idle: hollow, the surface shows"
        );
        assert_eq!(edge.border_color, muted, "idle: a muted ring");
        assert!(edge.border_widths.left.0 > 0.0, "idle: a ring");
        let (center, edge) = dot_quads(window, working_item);
        assert_eq!(center.background, info, "working: filled");
        assert_eq!(edge.background, info, "working: filled to the edge");
        assert_eq!(center.border_widths.left.0, 0.0, "working: no ring");
    };
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        check(window, agent_row_id(idle.id), agent_row_id(working.id));
    })
    .unwrap();
    compact(cx, window, &state);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        check(
            window,
            agent_marker_id(idle.id),
            agent_marker_id(working.id),
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn space_selects_the_focused_row_and_enter_opens_it(cx: &mut TestAppContext) {
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Idle);
    let venus = agent_with("Venus", 1, 260.0, AgentState::Idle);
    let fake = Rc::new(FakeCommands::default());
    let (window, state) = open_with_commands(
        cx,
        connected(vec![mercury.clone(), venus.clone()], vec![]),
        &fake,
        None,
    );
    cx.update_window(window.into(), |_, window, cx| {
        window.click(agent_row_id(mercury.id), cx);
        // Tab moves the focus without selecting.
        window.press("tab", cx);
        assert_eq!(window.find(agent_row_id(venus.id)).focused(), Some(true));
    })
    .unwrap();
    assert_eq!(selected(cx, &state), Some(mercury.id));

    cx.update_window(window.into(), |_, window, cx| window.press("space", cx))
        .unwrap();
    state.read_with(cx, |state, _| {
        assert_eq!(state.model().selected(), Some(venus.id), "Space selected");
        assert!(state.model().tabs().is_empty(), "Space opens nothing");
    });
    cx.update_window(window.into(), |_, window, cx| window.press("enter", cx))
        .unwrap();
    assert_eq!(
        state.read_with(cx, |state, _| state.model().tabs().to_vec()),
        [venus.id],
        "Enter opened it"
    );
}

/// `count` idle agents, a minute apart.
fn many_agents(count: usize) -> Vec<Agent> {
    (0..count)
        .map(|ix| {
            agent_with(
                &format!("Agent {ix:02}"),
                ix as i64,
                200.0,
                AgentState::Idle,
            )
        })
        .collect()
}

/// Whether `item` lies wholly inside the agent list's visible area.
fn shown_in_list(window: &gpui_kit::Window, item: ElementId) -> bool {
    let list = window.find(AGENT_LIST_ID).bounds();
    let item = window.find(item).bounds();
    item.top() >= list.top() && item.bottom() <= list.bottom()
}

#[gpui_kit::test]
fn arrow_keys_scroll_the_selection_into_view(cx: &mut TestAppContext) {
    let agents = many_agents(30);
    let (first, last) = (agents[0].id, agents[29].id);
    let fake = Rc::new(FakeCommands::default());
    let (window, state) = open_with_commands(cx, connected(agents, vec![]), &fake, None);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            !shown_in_list(window, agent_row_id(last)),
            "off screen at first"
        );
        window.click(agent_row_id(first), cx);
        for _ in 0..29 {
            window.press("down", cx);
        }
        window.render_frame(cx);
        assert!(
            shown_in_list(window, agent_row_id(last)),
            "the last row scrolled into view"
        );
        assert!(!shown_in_list(window, agent_row_id(first)));
    })
    .unwrap();
    assert_eq!(selected(cx, &state), Some(last));
}

#[gpui_kit::test]
fn a_long_compact_list_scrolls_and_new_agent_stays_reachable(cx: &mut TestAppContext) {
    let agents = many_agents(30);
    let last = agents[29].id;
    let fake = Rc::new(FakeCommands::default());
    let (window, state) = open_with_commands(cx, connected(agents, vec![]), &fake, None);
    compact(cx, window, &state);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(!shown_in_list(window, agent_marker_id(last)));
        window.scroll(
            AGENT_LIST_ID,
            ScrollDelta::Pixels(point(px(0.), px(-5000.))),
            cx,
        );
        window.render_frame(cx);
        assert!(
            shown_in_list(window, agent_marker_id(last)),
            "the wheel scrolls the strip"
        );
        let new_agent = window.find(NEW_AGENT_ID);
        assert!(new_agent.visible(), "New agent stays put above the list");
        assert!(new_agent.bounds().bottom() <= window.find(AGENT_LIST_ID).bounds().top());
        window.click(NEW_AGENT_ID, cx);
    })
    .unwrap();
    // No repository: the click still reaches the button.
    assert_eq!(notice(cx, window).as_deref(), Some(NEED_REPO_TEXT));
}

#[gpui_kit::test]
fn cmd_b_still_toggles_after_clicking_a_row_and_after_arrow_keys(cx: &mut TestAppContext) {
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Idle);
    let venus = agent_with("Venus", 1, 260.0, AgentState::Idle);
    let fake = Rc::new(FakeCommands::default());
    let (window, state) = open_with_commands(
        cx,
        connected(vec![mercury.clone(), venus.clone()], vec![]),
        &fake,
        None,
    );
    let mode = |cx: &mut TestAppContext| state.read_with(cx, |state, _| state.left_pane_mode());
    cx.update_window(window.into(), |_, window, cx| {
        window.click(agent_row_id(mercury.id), cx);
        window.press("cmd-b", cx);
    })
    .unwrap();
    assert_eq!(mode(cx), LeftPaneMode::Compact, "after a row click");
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.press("down", cx);
        assert_eq!(window.find(agent_marker_id(venus.id)).focused(), Some(true));
        window.press("cmd-b", cx);
    })
    .unwrap();
    assert_eq!(mode(cx), LeftPaneMode::Expanded, "after arrow keys");
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("up", cx);
        window.press("cmd-b", cx);
    })
    .unwrap();
    assert_eq!(mode(cx), LeftPaneMode::Compact);
    assert_eq!(selected(cx, &state), Some(mercury.id));
}

#[gpui_kit::test]
fn new_agent_without_a_repository_keeps_an_open_settings_dock_open(cx: &mut TestAppContext) {
    let fake = Rc::new(FakeCommands::default());
    let (window, shell) = open_shell_options(
        cx,
        ShellOptions {
            model: connected(vec![], vec![]),
            commands: Some(fake.clone()),
            ..Default::default()
        },
        (1200., 800.),
    );
    let dock = shell.read_with(cx, |shell, _| shell.dock().clone());
    let open = |cx: &mut TestAppContext| {
        dock.read_with(cx, |dock, _| dock.is_dock_open(DockPlacement::Right))
    };
    assert!(open(cx), "the Settings dock starts open");
    for click in 1..=2 {
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(NEW_AGENT_ID, cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(open(cx), "click {click}: still open, not toggled closed");
        let mut count = 0;
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            count = window.notifications(cx).len();
        })
        .unwrap();
        assert_eq!(count, 1, "click {click}: one hint, not a stack");
        assert_eq!(notice(cx, window).as_deref(), Some(NEED_REPO_TEXT));
    }
}

#[gpui_kit::test]
fn a_theme_switch_recolors_the_row_accents(cx: &mut TestAppContext) {
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Working);
    let fake = Rc::new(FakeCommands::default());
    let (window, state) =
        open_with_commands(cx, connected(vec![mercury.clone()], vec![]), &fake, None);
    let accent_and_dot = |cx: &mut TestAppContext| {
        let mut painted = None;
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            let row = agent_row_id(mercury.id);
            let bar = window.within(row.clone()).find(ACCENT_BAR_ID).bounds();
            let dot = window.within(row).find(STATUS_DOT_ID).bounds();
            painted = Some((
                painted_at(window, bar.center()),
                painted_at(window, dot.center()),
            ));
        })
        .unwrap();
        painted.unwrap()
    };
    let dark = accent_and_dot(cx);
    let set = ThemeSet::embedded();
    let light_state = theme_state(Appearance::Light);
    cx.update(|cx| {
        smithagents_app::theme_adapter::apply_theme(set, &light_state, Some(&state), cx);
    });
    let light = accent_and_dot(cx);
    let native = set.native(&light_state.theme).unwrap();
    assert_eq!(
        light.0,
        Background::from(srgb_to_hsla(agent_accent(&native, mercury.color.h)))
    );
    assert_eq!(light.1, token_background(&native, "status-info"));
    assert_ne!(dark.0, light.0, "the accent follows the theme");
}

#[gpui_kit::test]
fn a_create_the_daemon_never_answers_is_given_up_after_the_timeout(cx: &mut TestAppContext) {
    let only = repo("/src/app", 0);
    let fake = Rc::new(FakeCommands::default());
    let (window, _) = open_pane(
        cx,
        ShellOptions {
            model: connected(vec![], vec![only.clone()]),
            commands: Some(fake.clone()),
            command_timeout: Some(Duration::from_secs(5)),
            ..Default::default()
        },
    );
    let disabled = |cx: &mut TestAppContext| {
        let mut disabled = None;
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            disabled = window.find(NEW_AGENT_ID).disabled();
        })
        .unwrap();
        disabled == Some(true)
    };
    cx.update_window(window.into(), |_, window, cx| {
        window.click(NEW_AGENT_ID, cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert!(disabled(cx));
    cx.executor().advance_clock(Duration::from_secs(4));
    cx.run_until_parked();
    assert!(disabled(cx), "still waiting before the timeout");
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert!(!disabled(cx), "given up after the timeout");
    assert_eq!(notice(cx, window).as_deref(), Some(DAEMON_SILENT_TEXT));
    // The fake still holds the first create; a new one can start.
    cx.update_window(window.into(), |_, window, cx| {
        window.click(NEW_AGENT_ID, cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(fake.creates.borrow().len(), 2);
}

#[gpui_kit::test]
fn a_create_waiting_when_the_connection_drops_is_given_up(cx: &mut TestAppContext) {
    let only = repo("/src/app", 0);
    let fake = Rc::new(FakeCommands::default());
    let (window, state) =
        open_with_commands(cx, connected(vec![], vec![only.clone()]), &fake, None);
    cx.update_window(window.into(), |_, window, cx| {
        window.click(NEW_AGENT_ID, cx)
    })
    .unwrap();
    cx.run_until_parked();
    state.update(cx, |state, cx| {
        state.dispatch(Action::ConnectionChanged(Connection::Reconnecting), cx)
    });
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_ne!(window.find(NEW_AGENT_ID).disabled(), Some(true));
    })
    .unwrap();
    assert_eq!(notice(cx, window).as_deref(), Some(DAEMON_SILENT_TEXT));
}

#[gpui_kit::test]
fn enter_never_closes_even_on_the_focused_close_button_but_space_does(cx: &mut TestAppContext) {
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Idle);
    let fake = Rc::new(FakeCommands::default());
    let (window, _) = open_with_commands(cx, connected(vec![mercury.clone()], vec![]), &fake, None);
    ask_to_close(cx, window, agent_row_id(mercury.id));
    cx.update_window(window.into(), |_, window, cx| {
        window.press("tab", cx);
        assert_eq!(window.find(CONFIRM_CLOSE_ID).focused(), Some(true));
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(fake.closes.borrow().is_empty(), "Enter still dismisses");
    assert!(!dialog_open(cx, window));

    // The keyboard can still close: Space activates the focused button.
    ask_to_close(cx, window, agent_row_id(mercury.id));
    cx.update_window(window.into(), |_, window, cx| {
        window.press("tab", cx);
        window.press("space", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(*fake.closes.borrow(), [mercury.id]);
    assert!(!dialog_open(cx, window));
}

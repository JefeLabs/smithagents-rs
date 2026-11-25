//! Headless checks of the agents pane (the left pane's content) in both of
//! its modes, through gpui-kit's `test-support`. Agent commands go to a fake,
//! so no daemon runs.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use common::ui::{
    agent, dark_native, open_shell, open_shell_options, painted_at, token_background,
};
use futures::channel::oneshot;
use futures::future::LocalBoxFuture;
use futures::FutureExt as _;
use gpui_kit::base::{test_support, Root};
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::test::{TestEventExt as _, TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{
    point, px, AppContext as _, Background, Bounds, ElementId, Entity, MouseButton, Pixels, Role,
    TestAppContext, WindowHandle,
};
use smithagents_app::app_state::{AppState, AppStateStore};
use smithagents_app::layout_store::COMPACT_LEFT_PANE_WIDTH;
use smithagents_app::link::{AgentCommands, LinkError};
use smithagents_app::model::{Action, Connection, Effect, ShellModel};
use smithagents_app::shell::{ShellOptions, LEFT_PANE_ID};
use smithagents_app::shell_state::ShellState;
use smithagents_app::theme_adapter::{agent_accent, srgb_to_hsla};
use smithagents_app::view_models::{LeftPaneMode, NEED_REPO_TEXT, NOT_CONNECTED_TEXT};
use smithagents_app::views::agents::{
    agent_marker_id, agent_row_id, repo_choice_id, ACCENT_BAR_ID, AGENTS_EMPTY_ID,
    CLOSE_AGENT_LABEL, LEFT_PANE_CONTENT_ID, NEW_AGENT_ID, NOTICE_ID, NO_AGENTS_TEXT,
    STATUS_DOT_ID, SWATCH_ID,
};
use smithagents_app::views::rail::RailEntry;
use smithagents_client::ClientError;
use smithagents_core::agent::{Agent, AgentState, Repo};
use smithagents_core::event::{Event, EventKind};
use smithagents_core::ids::{AgentId, RepoId};
use smithagents_core::palette::AgentColor;

/// Agent commands that record each call. A create answers with the next
/// queued result, or waits until the test answers it with
/// [`FakeCommands::answer_create`]; a close answers `Ok`.
#[derive(Default)]
struct FakeCommands {
    creates: RefCell<Vec<RepoId>>,
    closes: RefCell<Vec<AgentId>>,
    create_results: RefCell<VecDeque<Result<Agent, LinkError>>>,
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
        async { Ok(()) }.boxed_local()
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
    let (window, shell) = open_shell_options(
        cx,
        ShellOptions {
            model,
            commands: Some(fake.clone()),
            app_state,
            ..Default::default()
        },
        (1200., 800.),
    );
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
    let tokens = [
        "status-info",
        "status-ok",
        "status-error",
        "muted-foreground",
    ];
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
            assert_eq!(
                painted_at(window, dot.center()),
                token_background(&native, token),
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
        assert_eq!(
            menu.find(repo_choice_id(0)).label(),
            Some("older, /src/older"),
            "the last used first"
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

#[gpui_kit::test]
fn close_agent_asks_first_and_closes_once(cx: &mut TestAppContext) {
    let mercury = agent_with("Mercury", 0, 200.0, AgentState::Idle);
    let fake = Rc::new(FakeCommands::default());
    let (window, state) =
        open_with_commands(cx, connected(vec![mercury.clone()], vec![]), &fake, None);

    ask_to_close(cx, window, agent_row_id(mercury.id));
    cx.update_window(window.into(), |_, window, cx| {
        assert!(window.find("ok").visible(), "a confirmation is showing");
        assert!(fake.closes.borrow().is_empty(), "nothing closes before OK");
        window.click("cancel", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(fake.closes.borrow().is_empty(), "Cancel closes nothing");

    // From the compact strip this time.
    compact(cx, window, &state);
    ask_to_close(cx, window, agent_marker_id(mercury.id));
    cx.update_window(window.into(), |_, window, cx| window.click("ok", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(*fake.closes.borrow(), [mercury.id], "closed exactly once");
    // The agent leaves only when the daemon says so.
    assert!(state.read_with(cx, |state, _| state.model().agents().len() == 1));
}

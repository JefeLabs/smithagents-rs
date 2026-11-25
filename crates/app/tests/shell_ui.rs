//! Headless checks of the shell window through gpui-kit's `test-support`
//! (`#[gpui_kit::test]`, real layout, hit-testing and clicks, no pixels).
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui_kit::base::Root;
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, size, AppContext as _, Bounds, Entity, Point, TestAppContext, WindowBounds, WindowHandle,
    WindowOptions,
};
use smithagents_app::layout_store::{saved_dock, LayoutStore};
use smithagents_app::model::{Action, Connection, Effect, ShellModel};
use smithagents_app::shell::{Shell, ShellOptions};
use smithagents_app::views::banner::BANNER_ID;
use smithagents_app::views::center::EMPTY_HINT_ID;
use smithagents_app::views::rail::RailEntry;
use smithagents_app::views::tabs::{close_button_id, TOGGLE_AGENTS_ID};
use smithagents_center_host::PixelRect;
use smithagents_core::agent::{Agent, AgentState};
use smithagents_core::ids::{AgentId, RepoId};
use smithagents_core::palette::AgentColor;
use smithagents_theme::tokens::ThemeSet;

fn agent(name: &str) -> Agent {
    let at: DateTime<Utc> = "2026-11-24T09:00:00Z".parse().unwrap();
    Agent {
        id: AgentId::new(),
        name: name.into(),
        color: AgentColor {
            l: 0.5,
            c: 0.2,
            h: 100.0,
        },
        repo_id: RepoId::new(),
        worktree_path: "/tmp/wt".into(),
        branch: format!("agent/{name}"),
        harness_session_id: None,
        state: AgentState::Idle,
        created_at: at,
        closed_at: None,
        intent_id: None,
    }
}

/// A connected model with `agent` open in a tab.
fn connected_with_tab(agent: &Agent) -> ShellModel {
    let mut model = ShellModel::new();
    model.apply(Action::Snapshot {
        agents: vec![agent.clone()],
        repos: vec![],
        latest_seq: 1,
    });
    model.apply(Action::ConnectionChanged(Connection::Connected));
    model.apply(Action::OpenTab(agent.id));
    model
}

/// Opens the shell, offline (no daemon link, no layout file), in a
/// 1200x800 window.
fn open_shell(cx: &mut TestAppContext, model: ShellModel) -> (WindowHandle<Root>, Entity<Shell>) {
    open_shell_with(cx, model, None)
}

fn open_shell_with(
    cx: &mut TestAppContext,
    model: ShellModel,
    layout: Option<LayoutStore>,
) -> (WindowHandle<Root>, Entity<Shell>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        smithagents_app::shell::init(cx);
        let set = ThemeSet::embedded();
        let native_theme = set.native(&set.themes[0].id);
        let bounds = Bounds {
            origin: Point::default(),
            size: size(px(1200.), px(800.)),
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        let (window, shell) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| {
                Shell::new(
                    ShellOptions {
                        model,
                        link: None,
                        layout,
                        native_theme,
                    },
                    window,
                    cx,
                )
            })
        })
        .expect("open the shell window");
        (window.downcast::<Root>().expect("Base Root"), shell)
    })
}

#[gpui_kit::test]
fn the_close_button_closes_the_tab_and_queues_the_effect(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let (window, shell) = open_shell(cx, connected_with_tab(&mercury));
    let state = shell.read_with(cx, |shell, _| shell.state().clone());

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(BANNER_ID).is_none(), "connected: no banner");
        assert!(window.try_find(EMPTY_HINT_ID).is_none(), "a tab is open");
        window.click(close_button_id(0), cx);
    })
    .unwrap();

    state.read_with(cx, |state, _| {
        assert!(state.model().tabs().is_empty());
        assert_eq!(state.model().active_tab(), None);
        assert_eq!(
            state.pending_effects().iter().cloned().collect::<Vec<_>>(),
            vec![Effect::CloseCenterTab(mercury.id)]
        );
    });
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find(EMPTY_HINT_ID).visible(),
            "the empty hint is back"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_banner_shows_until_the_link_is_connected(cx: &mut TestAppContext) {
    let (window, shell) = open_shell(cx, ShellModel::new());
    let state = shell.read_with(cx, |shell, _| shell.state().clone());

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(BANNER_ID).label(),
            Some("Connecting to the local daemon…")
        );
        assert!(window.find(EMPTY_HINT_ID).visible(), "no tab is open");
    })
    .unwrap();

    state.update(cx, |state, cx| {
        state.dispatch(Action::ConnectionChanged(Connection::Reconnecting), cx)
    });
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(BANNER_ID).label(),
            Some("Reconnecting to the local daemon…")
        );
    })
    .unwrap();

    state.update(cx, |state, cx| {
        state.dispatch(Action::ConnectionChanged(Connection::Connected), cx)
    });
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(BANNER_ID).is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn collapsing_either_dock_widens_the_reported_center(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let (window, shell) = open_shell(cx, connected_with_tab(&mercury));
    let (state, dock) =
        shell.read_with(cx, |shell, _| (shell.state().clone(), shell.dock().clone()));
    let seen: Rc<RefCell<Vec<PixelRect>>> = Rc::default();
    state.update(cx, |state, cx| {
        let seen = seen.clone();
        state.set_center_bounds_listener(move |rect, _| seen.borrow_mut().push(rect), cx);
    });

    let mut scale = 1.0;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        scale = f64::from(window.scale_factor());
    })
    .unwrap();
    let both_open = *seen
        .borrow()
        .last()
        .expect("the center reported its bounds");
    assert_eq!(both_open.scale, scale);
    assert!(both_open.w > 0.0 && both_open.h > 0.0, "{both_open:?}");
    assert_eq!(
        state.read_with(cx, |state, _| state.center_rect()),
        Some(both_open)
    );

    // The strip's toggle collapses the left dock.
    cx.update_window(window.into(), |_, window, cx| {
        window.click(TOGGLE_AGENTS_ID, cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert!(!dock.read_with(cx, |dock, _| dock.is_dock_open(DockPlacement::Left)));
    let left_closed = *seen.borrow().last().unwrap();
    assert!(
        left_closed.w > both_open.w,
        "{left_closed:?} vs {both_open:?}"
    );
    assert!(left_closed.x < both_open.x, "the center moved left");

    // The rail's Settings entry collapses the right dock.
    assert_eq!(
        shell.read_with(cx, |shell, cx| shell.rail_active(cx)),
        Some(RailEntry::Settings)
    );
    cx.update_window(window.into(), |_, window, cx| {
        window.click(RailEntry::Settings.element_id(), cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert!(!dock.read_with(cx, |dock, _| dock.is_dock_open(DockPlacement::Right)));
    assert_eq!(shell.read_with(cx, |shell, cx| shell.rail_active(cx)), None);
    let both_closed = *seen.borrow().last().unwrap();
    assert!(
        both_closed.w > left_closed.w,
        "{both_closed:?} vs {left_closed:?}"
    );

    // And back: the rail reopens it.
    cx.update_window(window.into(), |_, window, cx| {
        window.click(RailEntry::Settings.element_id(), cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert!(dock.read_with(cx, |dock, _| dock.is_dock_open(DockPlacement::Right)));
    assert_eq!(seen.borrow().last().unwrap().w, left_closed.w);
}

#[gpui_kit::test]
fn clicking_a_tab_activates_it(cx: &mut TestAppContext) {
    let (mercury, venus) = (agent("Mercury"), agent("Venus"));
    let mut model = connected_with_tab(&mercury);
    model.apply(Action::Snapshot {
        agents: vec![mercury.clone(), venus.clone()],
        repos: vec![],
        latest_seq: 2,
    });
    model.apply(Action::OpenTab(venus.id));
    let (window, shell) = open_shell(cx, model);
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    assert_eq!(
        state.read_with(cx, |state, _| state.model().active_tab()),
        Some(venus.id)
    );

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within("center-tabs").click(0usize, cx);
    })
    .unwrap();

    state.read_with(cx, |state, _| {
        assert_eq!(state.model().active_tab(), Some(mercury.id));
        assert_eq!(state.model().tabs(), [mercury.id, venus.id]);
        assert_eq!(
            state.pending_effects().iter().cloned().collect::<Vec<_>>(),
            vec![Effect::FocusCenterTab(mercury.id)]
        );
    });
}

#[gpui_kit::test]
fn cmd_b_toggles_the_agents_dock(cx: &mut TestAppContext) {
    let (window, shell) = open_shell(cx, ShellModel::new());
    let dock = shell.read_with(cx, |shell, _| shell.dock().clone());
    let left_open = |cx: &mut TestAppContext| {
        dock.read_with(cx, |dock, _| dock.is_dock_open(DockPlacement::Left))
    };
    assert!(left_open(cx));

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("cmd-b", cx);
    })
    .unwrap();
    assert!(!left_open(cx));

    cx.update_window(window.into(), |_, window, cx| window.press("cmd-b", cx))
        .unwrap();
    assert!(left_open(cx));
}

#[gpui_kit::test]
fn the_dock_layout_is_saved_after_a_change_and_restored_on_the_next_open(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(home.path());
    let (window, _) = open_shell_with(cx, ShellModel::new(), Some(store.clone()));
    assert_eq!(store.load(), None, "nothing is written before a change");

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(TOGGLE_AGENTS_ID, cx);
    })
    .unwrap();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();

    let saved = store.load().expect("the layout was written");
    assert_eq!(
        saved_dock(&saved, DockPlacement::Left).map(|d| d.open),
        Some(false)
    );
    assert_eq!(
        saved_dock(&saved, DockPlacement::Right).map(|d| d.open),
        Some(true)
    );

    // A second window, as on the next launch, starts from the saved layout.
    let (_, reopened) = open_shell_with(cx, ShellModel::new(), Some(store));
    let dock = reopened.read_with(cx, |shell, _| shell.dock().clone());
    dock.read_with(cx, |dock, _| {
        assert!(!dock.is_dock_open(DockPlacement::Left));
        assert!(dock.is_dock_open(DockPlacement::Right));
    });
}

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
use gpui_kit::component::dock::{DockAreaState, DockPlacement, DockState, PanelInfo, PanelState};
use gpui_kit::test::{TestEventExt as _, TestWindowExt as _};
use gpui_kit::{
    point, px, size, AppContext as _, Bounds, Entity, MouseButton, Pixels, Point, TestAppContext,
    WindowBounds, WindowHandle, WindowOptions,
};
use smithagents_app::layout_store::{
    saved_dock, saved_left_pane, with_left_pane, LayoutStore, LeftPaneLayout,
    COMPACT_LEFT_PANE_WIDTH, DEFAULT_LEFT_PANE_WIDTH, LAYOUT_VERSION, MIN_CENTER_WIDTH,
    MIN_LEFT_PANE_WIDTH,
};
use smithagents_app::link::LinkConfig;
use smithagents_app::model::{Action, Connection, Effect, ShellModel};
use smithagents_app::shell::{Shell, ShellOptions, LEFT_PANE_ID, LEFT_PANE_RESIZE_ID};
use smithagents_app::shell_state::ShellState;
use smithagents_app::view_models::LeftPaneMode;
use smithagents_app::views::banner::BANNER_ID;
use smithagents_app::views::center::EMPTY_HINT_ID;
use smithagents_app::views::left_pane::{
    COMPACT_MARKER_ID, EXPANDED_TITLE_ID, LEFT_PANE_CONTENT_ID,
};
use smithagents_app::views::rail::{RailEntry, RAIL_WIDTH};
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
    open_shell_full(cx, model, layout, None, (1200., 800.))
}

fn open_shell_full(
    cx: &mut TestAppContext,
    model: ShellModel,
    layout: Option<LayoutStore>,
    link: Option<LinkConfig>,
    (width, height): (f32, f32),
) -> (WindowHandle<Root>, Entity<Shell>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        smithagents_app::shell::init(cx);
        let set = ThemeSet::embedded();
        let native_theme = set.native(&set.themes[0].id);
        let bounds = Bounds {
            origin: Point::default(),
            size: size(px(width), px(height)),
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
                        link,
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

    // The strip's toggle compacts the left pane.
    cx.update_window(window.into(), |_, window, cx| {
        window.click(TOGGLE_AGENTS_ID, cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert_eq!(mode(cx, &state), LeftPaneMode::Compact);
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

fn mode(cx: &mut TestAppContext, state: &Entity<ShellState>) -> LeftPaneMode {
    state.read_with(cx, |state, _| state.left_pane_mode())
}

/// Renders a frame and returns the left pane's drawn width, asserting it is
/// there at all.
fn left_pane_width(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Pixels {
    let mut width = px(0.);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let pane = window.find(LEFT_PANE_ID);
        assert!(pane.visible(), "the left pane is drawn");
        width = pane.bounds().size.width;
    })
    .unwrap();
    assert!(width > px(0.), "the left pane has a width");
    width
}

#[gpui_kit::test]
fn cmd_b_switches_the_left_pane_between_expanded_and_compact(cx: &mut TestAppContext) {
    let (window, shell) = open_shell(cx, ShellModel::new());
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    let seen: Rc<RefCell<Vec<PixelRect>>> = Rc::default();
    state.update(cx, |state, cx| {
        let seen = seen.clone();
        state.set_center_bounds_listener(move |rect, _| seen.borrow_mut().push(rect), cx);
    });
    assert_eq!(mode(cx, &state), LeftPaneMode::Expanded);
    assert_eq!(left_pane_width(cx, window), DEFAULT_LEFT_PANE_WIDTH);
    let mut scale = 1.0;
    cx.update_window(window.into(), |_, window, _| {
        assert!(window.find(EXPANDED_TITLE_ID).visible());
        assert!(window.try_find(COMPACT_MARKER_ID).is_none());
        assert!(
            window.find(LEFT_PANE_RESIZE_ID).visible(),
            "expanded resizes"
        );
        scale = f64::from(window.scale_factor());
    })
    .unwrap();
    let expanded = *seen.borrow().last().unwrap();

    cx.update_window(window.into(), |_, window, cx| window.press("cmd-b", cx))
        .unwrap();
    assert_eq!(mode(cx, &state), LeftPaneMode::Compact);
    assert_eq!(left_pane_width(cx, window), COMPACT_LEFT_PANE_WIDTH);
    cx.update_window(window.into(), |_, window, _| {
        assert!(window.find(COMPACT_MARKER_ID).visible());
        assert!(window.try_find(EXPANDED_TITLE_ID).is_none());
        assert!(
            window.try_find(LEFT_PANE_RESIZE_ID).is_none(),
            "no resizing while compact"
        );
    })
    .unwrap();
    let compact = *seen.borrow().last().unwrap();
    let grew = f64::from(f32::from(DEFAULT_LEFT_PANE_WIDTH - COMPACT_LEFT_PANE_WIDTH)) * scale;
    assert!(
        (compact.w - expanded.w - grew).abs() < 0.5,
        "{compact:?} vs {expanded:?}"
    );
    assert!((expanded.x - compact.x - grew).abs() < 0.5);

    cx.update_window(window.into(), |_, window, cx| window.press("cmd-b", cx))
        .unwrap();
    assert_eq!(mode(cx, &state), LeftPaneMode::Expanded);
    assert_eq!(
        left_pane_width(cx, window),
        DEFAULT_LEFT_PANE_WIDTH,
        "back at the previous width"
    );
    assert_eq!(seen.borrow().last().unwrap().w, expanded.w);
}

#[gpui_kit::test]
fn dragging_the_edge_resizes_the_expanded_left_pane(cx: &mut TestAppContext) {
    let (window, shell) = open_shell(cx, ShellModel::new());
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    left_pane_width(cx, window);
    let mut to = point(px(0.), px(0.));
    cx.update_window(window.into(), |_, window, cx| {
        let from = window.find(LEFT_PANE_RESIZE_ID).bounds().center();
        to = point(from.x + px(60.), from.y);
        window.drag(from, to, cx);
    })
    .unwrap();
    // The pane's edge follows the pointer (the shell starts at x = 0).
    let width = state.read_with(cx, |state, _| state.left_pane_layout().expanded_width);
    assert!((width - to.x).abs() <= px(1.), "{width:?} vs {to:?}");
    assert!(width > DEFAULT_LEFT_PANE_WIDTH + px(50.));
    assert_eq!(left_pane_width(cx, window), width);
}

#[gpui_kit::test]
fn the_dock_layout_is_saved_after_a_change_and_restored_on_the_next_open(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(home.path());
    let (window, _) = open_shell_with(cx, ShellModel::new(), Some(store.clone()));
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert_eq!(store.load(), None, "nothing is written while idle");
    assert_eq!(store.load_window(), None, "nothing is written while idle");

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(TOGGLE_AGENTS_ID, cx);
    })
    .unwrap();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();

    let saved = store.load().expect("the layout was written");
    assert_eq!(
        saved_left_pane(&saved),
        Some(LeftPaneLayout {
            mode: LeftPaneMode::Compact,
            expanded_width: DEFAULT_LEFT_PANE_WIDTH
        })
    );
    assert_eq!(
        saved_dock(&saved, DockPlacement::Right).map(|d| d.open),
        Some(true)
    );

    // A second window, as on the next launch, starts from the saved layout,
    // remembering the expanded width for when the pane expands again.
    let (reopened_window, reopened) = open_shell_with(cx, ShellModel::new(), Some(store));
    let (state, dock) =
        reopened.read_with(cx, |shell, _| (shell.state().clone(), shell.dock().clone()));
    assert_eq!(mode(cx, &state), LeftPaneMode::Compact);
    assert_eq!(
        left_pane_width(cx, reopened_window),
        COMPACT_LEFT_PANE_WIDTH
    );
    assert!(dock.read_with(cx, |dock, _| dock.is_dock_open(DockPlacement::Right)));
    cx.update_window(reopened_window.into(), |_, window, cx| {
        window.press("cmd-b", cx)
    })
    .unwrap();
    assert_eq!(
        left_pane_width(cx, reopened_window),
        DEFAULT_LEFT_PANE_WIDTH
    );
}

#[gpui_kit::test]
fn every_saved_variant_restores_a_drawn_left_pane(cx: &mut TestAppContext) {
    let panel = |name: &str| PanelState {
        panel_name: name.into(),
        children: vec![],
        info: PanelInfo::panel(serde_json::Value::Null),
    };
    let area = |left: Option<DockState>| DockAreaState {
        version: Some(LAYOUT_VERSION),
        center: panel("Center"),
        left_dock: left,
        right_dock: Some(DockState::new(
            panel("Settings"),
            DockPlacement::Right,
            px(300.),
            true,
        )),
        bottom_dock: None,
    };
    let old = |size: f32, open| {
        area(Some(DockState::new(
            panel("Agents"),
            DockPlacement::Left,
            px(size),
            open,
        )))
    };
    let new = |mode, width: f32| {
        with_left_pane(
            area(None),
            LeftPaneLayout {
                mode,
                expanded_width: px(width),
            },
        )
    };
    // (file contents, expected mode, expected drawn width)
    // (file contents, expected mode, drawn width, drawn width after one cmd-b)
    let compact = COMPACT_LEFT_PANE_WIDTH;
    let cases: Vec<(Option<String>, LeftPaneMode, Pixels, Pixels)> = vec![
        (
            None,
            LeftPaneMode::Expanded,
            DEFAULT_LEFT_PANE_WIDTH,
            compact,
        ),
        (
            Some("{ corrupt".into()),
            LeftPaneMode::Expanded,
            DEFAULT_LEFT_PANE_WIDTH,
            compact,
        ),
        (
            Some(serde_json::to_string(&area(None)).unwrap()),
            LeftPaneMode::Expanded,
            DEFAULT_LEFT_PANE_WIDTH,
            compact,
        ),
        (
            Some(serde_json::to_string(&new(LeftPaneMode::Compact, 300.)).unwrap()),
            LeftPaneMode::Compact,
            compact,
            px(300.),
        ),
        (
            Some(serde_json::to_string(&new(LeftPaneMode::Expanded, 300.)).unwrap()),
            LeftPaneMode::Expanded,
            px(300.),
            compact,
        ),
        (
            Some(serde_json::to_string(&old(260., false)).unwrap()),
            LeftPaneMode::Compact,
            compact,
            px(260.),
        ),
        (
            Some(serde_json::to_string(&old(260., true)).unwrap()),
            LeftPaneMode::Expanded,
            px(260.),
            compact,
        ),
        (
            Some(serde_json::to_string(&old(1., true)).unwrap()),
            LeftPaneMode::Expanded,
            MIN_LEFT_PANE_WIDTH,
            compact,
        ),
    ];
    for (contents, want_mode, want_width, toggled_width) in cases {
        let home = tempfile::tempdir().unwrap();
        let store = LayoutStore::new(home.path());
        if let Some(contents) = &contents {
            std::fs::write(store.path(), contents).unwrap();
        }
        let (window, shell) = open_shell_with(cx, ShellModel::new(), Some(store));
        let state = shell.read_with(cx, |shell, _| shell.state().clone());
        assert_eq!(mode(cx, &state), want_mode, "{contents:?}");
        assert_eq!(left_pane_width(cx, window), want_width, "{contents:?}");
        // And it stays drawn through a round of toggles, at the remembered
        // width when expanded.
        for want in [toggled_width, want_width] {
            cx.update_window(window.into(), |_, window, cx| window.press("cmd-b", cx))
                .unwrap();
            assert_eq!(left_pane_width(cx, window), want, "{contents:?}");
        }
        cx.update_window(window.into(), |_, window, _| window.remove_window())
            .unwrap();
    }
}

/// Clicks the middle of a side dock, as a user would before using its
/// shortcut, so focus moves into the dock's panel.
fn click_inside_dock(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    shell: &Entity<Shell>,
    placement: DockPlacement,
) {
    let dock = shell.read_with(cx, |shell, _| shell.dock().clone());
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let (area, width) = {
            let dock = dock.read(cx);
            (
                dock.bounds(),
                dock.dock_size(placement).expect("the dock exists"),
            )
        };
        let x = match placement {
            DockPlacement::Right => area.right() - width / 2.,
            _ => area.left() + width / 2.,
        };
        let at = point(x, area.top() + area.size.height / 2.);
        window.pointer_down(at, MouseButton::Left, cx);
        window.pointer_up(at, MouseButton::Left, cx);
    })
    .unwrap();
}

#[gpui_kit::test]
fn shortcuts_still_work_after_the_focused_pane_changes(cx: &mut TestAppContext) {
    let (window, shell) = open_shell(cx, ShellModel::new());
    let (state, dock) =
        shell.read_with(cx, |shell, _| (shell.state().clone(), shell.dock().clone()));

    // The left pane: focus inside it, compact it, expand it again.
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(LEFT_PANE_CONTENT_ID).focused(), Some(false));
        let at = window.find(LEFT_PANE_ID).bounds().center();
        window.pointer_down(at, MouseButton::Left, cx);
        window.pointer_up(at, MouseButton::Left, cx);
        assert_eq!(
            window.find(LEFT_PANE_CONTENT_ID).focused(),
            Some(true),
            "the click moved focus into the left pane"
        );
        window.press("cmd-b", cx);
    })
    .unwrap();
    assert_eq!(mode(cx, &state), LeftPaneMode::Compact);
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("cmd-b", cx);
    })
    .unwrap();
    assert_eq!(
        mode(cx, &state),
        LeftPaneMode::Expanded,
        "a second cmd-b expands it"
    );

    // The right dock still hides and shows.
    let open = |cx: &mut TestAppContext| {
        dock.read_with(cx, |dock, _| dock.is_dock_open(DockPlacement::Right))
    };
    click_inside_dock(cx, window, &shell, DockPlacement::Right);
    cx.update_window(window.into(), |_, window, cx| window.press("cmd-alt-b", cx))
        .unwrap();
    assert!(!open(cx), "cmd-alt-b closed the right dock");
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("cmd-alt-b", cx);
    })
    .unwrap();
    assert!(open(cx), "a second cmd-alt-b reopened the right dock");
}

#[gpui_kit::test]
fn closing_the_window_saves_the_layout(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(home.path());
    let (window, shell) = open_shell_with(cx, ShellModel::new(), Some(store.clone()));
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    // Only the window holds the shell now, so closing it releases the shell.
    drop(shell);
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    let saved = store.load().expect("the layout was written on close");
    assert_eq!(
        saved_left_pane(&saved),
        Some(LeftPaneLayout::default()),
        "an expanded left pane at the default width"
    );
    let saved_window = store.load_window().expect("the window bounds were written");
    assert_eq!(saved_window.bounds.size, size(px(1200.), px(800.)));
    assert!(!saved_window.maximized);
}

#[gpui_kit::test]
fn a_late_bounds_listener_gets_the_current_rect_and_may_update_the_state(cx: &mut TestAppContext) {
    let (window, shell) = open_shell(cx, ShellModel::new());
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    let current = state
        .read_with(cx, |state, _| state.center_rect())
        .expect("the first frame reported the center");

    let seen: Rc<RefCell<Vec<PixelRect>>> = Rc::default();
    state.update(cx, |state, cx| {
        let (seen, weak) = (seen.clone(), cx.entity().downgrade());
        state.set_center_bounds_listener(
            move |rect, cx| {
                // Re-entering the state from the listener is allowed.
                let recorded = weak
                    .update(cx, |state, cx| {
                        cx.notify();
                        state.center_rect()
                    })
                    .unwrap();
                assert_eq!(recorded, Some(rect));
                seen.borrow_mut().push(rect);
            },
            cx,
        );
    });
    assert_eq!(
        *seen.borrow(),
        vec![current],
        "called once, after the update"
    );

    // An unchanged frame calls nothing; a change calls once, after the frame.
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    assert_eq!(seen.borrow().len(), 1);
    cx.update_window(window.into(), |_, window, cx| {
        window.click(TOGGLE_AGENTS_ID, cx);
        window.render_frame(cx);
        assert_eq!(
            seen.borrow().len(),
            1,
            "the listener never runs during the draw"
        );
    })
    .unwrap();
    assert_eq!(seen.borrow().len(), 2);
    assert!(seen.borrow()[1].w > current.w);
}

#[gpui_kit::test]
fn a_narrow_window_keeps_the_center_wide_enough(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(home.path());
    // A layout saved in a wide window: two 600 px docks, both open.
    let panel = |name: &str| PanelState {
        panel_name: name.into(),
        children: vec![],
        info: PanelInfo::panel(serde_json::Value::Null),
    };
    let dock = |name: &str, placement| Some(DockState::new(panel(name), placement, px(600.), true));
    store
        .save(&DockAreaState {
            version: Some(LAYOUT_VERSION),
            center: panel("Center"),
            left_dock: dock("Agents", DockPlacement::Left),
            right_dock: dock("Settings", DockPlacement::Right),
            bottom_dock: None,
        })
        .unwrap();

    // Reopened in a 900 px window: the right dock gives way first, and the
    // left pane is narrowed only where it is drawn, its remembered width kept
    // (for a larger window later).
    let (window, shell) = open_shell_full(cx, ShellModel::new(), Some(store), None, (900., 700.));
    let (dock, state) =
        shell.read_with(cx, |shell, _| (shell.dock().clone(), shell.state().clone()));
    let remembered = state.read_with(cx, |state, _| state.left_pane_layout().expanded_width);
    assert_eq!(remembered, px(600.));
    let right = dock.read_with(cx, |area, _| area.dock_size(DockPlacement::Right).unwrap());
    assert_eq!(right, px(100.), "the right dock is at its minimum");
    let left = left_pane_width(cx, window);
    assert_eq!(left, px(900. - RAIL_WIDTH) - right - MIN_CENTER_WIDTH);

    let mut scale = 1.0;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        scale = f64::from(window.scale_factor());
    })
    .unwrap();
    let rect = state.read_with(cx, |state, _| state.center_rect()).unwrap();
    assert!(
        rect.w / scale >= f64::from(f32::from(MIN_CENTER_WIDTH)) - 1.0,
        "{rect:?}"
    );
}

#[gpui_kit::test]
fn a_compact_left_pane_and_a_wide_right_dock_keep_the_center_wide_enough(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(home.path());
    let panel = |name: &str| PanelState {
        panel_name: name.into(),
        children: vec![],
        info: PanelInfo::panel(serde_json::Value::Null),
    };
    store
        .save(&with_left_pane(
            DockAreaState {
                version: Some(LAYOUT_VERSION),
                center: panel("Center"),
                left_dock: None,
                right_dock: Some(DockState::new(
                    panel("Settings"),
                    DockPlacement::Right,
                    px(900.),
                    true,
                )),
                bottom_dock: None,
            },
            LeftPaneLayout {
                mode: LeftPaneMode::Compact,
                expanded_width: px(300.),
            },
        ))
        .unwrap();
    let (window, shell) = open_shell_full(cx, ShellModel::new(), Some(store), None, (1000., 700.));
    let (dock, state) =
        shell.read_with(cx, |shell, _| (shell.dock().clone(), shell.state().clone()));
    assert_eq!(left_pane_width(cx, window), COMPACT_LEFT_PANE_WIDTH);
    let right = dock.read_with(cx, |area, _| area.dock_size(DockPlacement::Right).unwrap());
    assert!(
        px(1000. - RAIL_WIDTH) - COMPACT_LEFT_PANE_WIDTH - right >= MIN_CENTER_WIDTH,
        "{right:?}"
    );
    assert_eq!(
        state.read_with(cx, |state, _| state.left_pane_layout().expanded_width),
        px(300.),
        "the expanded width is remembered"
    );
    let mut scale = 1.0;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        scale = f64::from(window.scale_factor());
    })
    .unwrap();
    let rect = state.read_with(cx, |state, _| state.center_rect()).unwrap();
    assert!(
        rect.w / scale >= f64::from(f32::from(MIN_CENTER_WIDTH)) - 1.0,
        "{rect:?}"
    );
}

#[gpui_kit::test]
fn the_link_error_is_polled_only_while_disconnected(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("bin").join("smithagentsd");
    let mut link = LinkConfig::new(
        home.path().join("home"),
        missing,
        smithagents_core::VERSION.to_owned(),
    );
    // One failed start, then a long wait: the test never sees a retry.
    link.backoff_min = Duration::from_secs(3600);
    link.backoff_max = Duration::from_secs(3600);
    let (window, shell) = open_shell_full(cx, ShellModel::new(), None, Some(link), (1200., 800.));
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    let polling = |cx: &mut TestAppContext| state.read_with(cx, |s, _| s.is_polling_link_error());
    assert!(polling(cx), "connecting: polled");

    state.update(cx, |s, cx| {
        s.dispatch(Action::ConnectionChanged(Connection::Connected), cx)
    });
    assert!(!polling(cx), "connected: no polling");
    state.update(cx, |s, cx| {
        s.dispatch(Action::ConnectionChanged(Connection::Reconnecting), cx)
    });
    assert!(polling(cx), "reconnecting: polled again");

    // The link fails to start the missing daemon on its own thread; the poll
    // brings its error into the banner.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while state.read_with(cx, |s, _| s.link().and_then(|l| l.last_error()).is_none()) {
        assert!(
            std::time::Instant::now() < deadline,
            "the link reported no error"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let label = window
            .find(BANNER_ID)
            .label()
            .unwrap_or_default()
            .to_owned();
        assert!(
            label.starts_with("Reconnecting to the local daemon…\ncould not start "),
            "{label}"
        );
        assert!(label.contains("smithagentsd"), "{label}");
    })
    .unwrap();
}

/// The handle's center, after a frame.
fn resize_handle_center(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Point<Pixels> {
    let mut at = point(px(0.), px(0.));
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        at = window.find(LEFT_PANE_RESIZE_ID).bounds().center();
    })
    .unwrap();
    at
}

#[gpui_kit::test]
fn dragging_the_edge_stops_at_both_limits_and_saves_the_final_width(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(home.path());
    let (window, shell) = open_shell_with(cx, ShellModel::new(), Some(store.clone()));
    let (state, dock) =
        shell.read_with(cx, |shell, _| (shell.state().clone(), shell.dock().clone()));
    let stored = |cx: &mut TestAppContext| {
        state.read_with(cx, |state, _| state.left_pane_layout().expanded_width)
    };

    // Far left: the pane stops at its minimum, never at nothing.
    let from = resize_handle_center(cx, window);
    cx.update_window(window.into(), |_, window, cx| {
        window.drag(from, point(px(-500.), from.y), cx)
    })
    .unwrap();
    assert_eq!(stored(cx), MIN_LEFT_PANE_WIDTH);
    assert_eq!(left_pane_width(cx, window), MIN_LEFT_PANE_WIDTH);

    // Far right, with the right dock open: the center keeps its minimum.
    let right = dock.read_with(cx, |area, _| {
        assert!(area.is_dock_open(DockPlacement::Right));
        area.dock_size(DockPlacement::Right).unwrap()
    });
    let widest = px(1200. - RAIL_WIDTH) - right - MIN_CENTER_WIDTH;
    let from = resize_handle_center(cx, window);
    cx.update_window(window.into(), |_, window, cx| {
        window.drag(from, point(px(1180.), from.y), cx)
    })
    .unwrap();
    assert_eq!(stored(cx), widest);
    assert_eq!(left_pane_width(cx, window), widest);
    let mut scale = 1.0;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        scale = f64::from(window.scale_factor());
    })
    .unwrap();
    let rect = state.read_with(cx, |state, _| state.center_rect()).unwrap();
    assert!(
        (rect.w / scale - f64::from(f32::from(MIN_CENTER_WIDTH))).abs() < 1.0,
        "{rect:?}"
    );

    // Once the drag settles and the debounce elapses, the file has it.
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    let saved = store.load().expect("the layout was written");
    assert_eq!(
        saved_left_pane(&saved),
        Some(LeftPaneLayout {
            mode: LeftPaneMode::Expanded,
            expanded_width: widest
        })
    );
}

#[gpui_kit::test]
fn a_released_button_stops_the_resize(cx: &mut TestAppContext) {
    let (window, shell) = open_shell(cx, ShellModel::new());
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    let stored = |cx: &mut TestAppContext| {
        state.read_with(cx, |state, _| state.left_pane_layout().expanded_width)
    };
    let from = resize_handle_center(cx, window);
    cx.update_window(window.into(), |_, window, cx| {
        // As `drag` does: hover, press, then moves with the button held (the
        // first one starts the drag).
        window.pointer_move(from, None, cx);
        window.pointer_down(from, MouseButton::Left, cx);
        for dx in [10., 20., 40.] {
            window.pointer_move(point(from.x + px(dx), from.y), Some(MouseButton::Left), cx);
        }
    })
    .unwrap();
    let dragged = stored(cx);
    assert!(dragged > DEFAULT_LEFT_PANE_WIDTH + px(30.), "{dragged:?}");

    // The mouse-up was missed: the next move reports no pressed button.
    cx.update_window(window.into(), |_, window, cx| {
        window.pointer_move(point(from.x + px(120.), from.y), None, cx);
        window.pointer_move(
            point(from.x + px(160.), from.y),
            Some(MouseButton::Left),
            cx,
        );
    })
    .unwrap();
    assert_eq!(
        stored(cx),
        dragged,
        "the pane no longer follows the pointer"
    );
}

#[gpui_kit::test]
fn expanding_shrinks_a_wide_right_dock_so_the_center_keeps_its_minimum(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(home.path());
    let panel = |name: &str| PanelState {
        panel_name: name.into(),
        children: vec![],
        info: PanelInfo::panel(serde_json::Value::Null),
    };
    store
        .save(&with_left_pane(
            DockAreaState {
                version: Some(LAYOUT_VERSION),
                center: panel("Center"),
                left_dock: None,
                right_dock: Some(DockState::new(
                    panel("Settings"),
                    DockPlacement::Right,
                    px(900.),
                    true,
                )),
                bottom_dock: None,
            },
            LeftPaneLayout {
                mode: LeftPaneMode::Compact,
                expanded_width: DEFAULT_LEFT_PANE_WIDTH,
            },
        ))
        .unwrap();
    let (window, shell) = open_shell_full(cx, ShellModel::new(), Some(store), None, (1000., 700.));
    let (state, dock) =
        shell.read_with(cx, |shell, _| (shell.state().clone(), shell.dock().clone()));
    let available = px(1000. - RAIL_WIDTH);
    let right = |cx: &mut TestAppContext| {
        dock.read_with(cx, |area, _| area.dock_size(DockPlacement::Right).unwrap())
    };
    assert_eq!(
        right(cx),
        available - COMPACT_LEFT_PANE_WIDTH - MIN_CENTER_WIDTH
    );

    cx.update_window(window.into(), |_, window, cx| window.press("cmd-b", cx))
        .unwrap();
    assert_eq!(mode(cx, &state), LeftPaneMode::Expanded);
    assert_eq!(
        right(cx),
        available - DEFAULT_LEFT_PANE_WIDTH - MIN_CENTER_WIDTH,
        "the right dock gave way"
    );
    assert_eq!(left_pane_width(cx, window), DEFAULT_LEFT_PANE_WIDTH);
    let mut scale = 1.0;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        scale = f64::from(window.scale_factor());
    })
    .unwrap();
    let rect = state.read_with(cx, |state, _| state.center_rect()).unwrap();
    assert!(
        rect.w / scale >= f64::from(f32::from(MIN_CENTER_WIDTH)) - 1.0,
        "{rect:?}"
    );
}

#[gpui_kit::test]
fn the_left_pane_spans_the_height_and_the_rail_sits_at_the_far_right(cx: &mut TestAppContext) {
    // Not connected, so the banner is shown above the center.
    let (window, _shell) = open_shell(cx, ShellModel::new());
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let content = window.viewport_size();
        let left = window.find(LEFT_PANE_ID).bounds();
        assert_eq!(left.origin, point(px(0.), px(0.)), "{left:?}");
        assert_eq!(left.size.height, content.height, "full height");

        let banner = window.find(BANNER_ID).bounds();
        assert!(banner.left() >= left.right(), "{banner:?} vs {left:?}");
        assert_eq!(
            banner.top(),
            px(0.),
            "the banner is at the top of the center"
        );
        let toggle = window.find(TOGGLE_AGENTS_ID).bounds();
        assert!(toggle.left() >= left.right() && toggle.top() >= banner.bottom());

        let rail_button = window.find(RailEntry::Settings.element_id()).bounds();
        assert!(
            rail_button.left() >= content.width - px(RAIL_WIDTH),
            "{rail_button:?}"
        );
        assert!(rail_button.right() <= content.width);
        assert!(banner.right() <= content.width - px(RAIL_WIDTH));
    })
    .unwrap();
}

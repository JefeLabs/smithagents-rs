//! Headless checks of the shell window through gpui-kit's `test-support`
//! (`#[gpui_kit::test]`, real layout, hit-testing and clicks, no pixels).
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use common::ui::{
    agent, open_shell, open_shell_options, painted_at, theme_state, token_background,
};
use gpui_kit::base::Root;
use gpui_kit::component::dock::{
    DockArea, DockAreaState, DockPlacement, DockState, PanelInfo, PanelState,
};
use gpui_kit::test::{TestEventExt as _, TestWindowExt as _};
use gpui_kit::{
    point, px, size, AppContext as _, Background, Bounds, Entity, MouseButton, Pixels, Point, Role,
    TestAppContext, WindowHandle,
};
use smithagents_app::layout_store::{
    saved_dock, saved_left_pane, status_bar_height, with_left_pane, LayoutStore, LeftPaneLayout,
    COMPACT_LEFT_PANE_WIDTH, DEFAULT_LEFT_PANE_WIDTH, LAYOUT_VERSION, MIN_CENTER_WIDTH,
    MIN_LEFT_PANE_WIDTH,
};
use smithagents_app::link::{LinkConfig, LinkFailure};
use smithagents_app::model::{Action, Connection, ShellModel};
use smithagents_app::shell::{
    Shell, ShellOptions, LEFT_PANE_ID, LEFT_PANE_RESIZE_ID, SETTINGS_PANEL,
};
use smithagents_app::shell_state::ShellState;
use smithagents_app::theme_adapter::{region_tokens, Region};
use smithagents_app::view_models::{LeftPaneMode, StatusKind};
use smithagents_app::views::agents::{EXPANDED_TITLE_ID, LEFT_PANE_CONTENT_ID, NEW_AGENT_ID};
use smithagents_app::views::banner::BANNER_ID;
use smithagents_app::views::center::{CENTER_PANEL_ID, EMPTY_HINT_ID};
use smithagents_app::views::rail::{RailEntry, RAIL_ID, RAIL_WIDTH};
use smithagents_app::views::tabs::{close_button_id, TOGGLE_AGENTS_ID};
use smithagents_app::views::title_bar::TITLE_BAR_ID;
use smithagents_center_host::PixelRect;
use smithagents_core::agent::{Agent, AgentState};
use smithagents_core::event::{Event, EventKind};
use smithagents_theme::state::{AppearanceSettings, ThemeChoice};
use smithagents_theme::tokens::{Appearance, ThemeSet};

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
    size: (f32, f32),
) -> (WindowHandle<Root>, Entity<Shell>) {
    open_shell_options(
        cx,
        ShellOptions {
            model,
            layout,
            link,
            ..Default::default()
        },
        size,
    )
}

#[gpui_kit::test]
fn the_close_button_closes_the_tab(cx: &mut TestAppContext) {
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
        let title = window.find(EXPANDED_TITLE_ID);
        assert!(title.visible());
        assert_eq!(
            (title.role(), title.label()),
            (Some(Role::Heading), Some("Agents"))
        );
        assert!(
            window.find(NEW_AGENT_ID).bounds().size.width > px(40.),
            "expanded: the New agent button has its label"
        );
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
        let new_agent = window.find(NEW_AGENT_ID);
        assert!(new_agent.visible());
        assert!(
            new_agent.bounds().size.width <= px(32.),
            "compact: the New agent button is icon-only"
        );
        assert_eq!(new_agent.label(), Some("New agent"));
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
fn the_left_pane_spans_the_height_below_the_title_bar_and_the_rail_sits_at_the_far_right(
    cx: &mut TestAppContext,
) {
    // Not connected, so the banner is shown above the center.
    let (window, _shell) = open_shell(cx, ShellModel::new());
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let content = window.viewport_size();
        let title = window.find(TITLE_BAR_ID).bounds();
        let left = window.find(LEFT_PANE_ID).bounds();
        assert_eq!(left.origin, point(px(0.), title.bottom()), "{left:?}");
        assert_eq!(
            left.bottom(),
            content.height - status_bar_height(window.rem_size()),
            "to the top of the status bar"
        );

        let banner = window.find(BANNER_ID).bounds();
        assert!(banner.left() >= left.right(), "{banner:?} vs {left:?}");
        assert_eq!(
            banner.top(),
            title.bottom(),
            "the banner is at the top of the center, below the title bar"
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

// The right dock keeps the user's preferred width: the width it is drawn at
// is only the part of it that fits beside the left pane.

/// Seeds a layout file: the right dock `right` px wide (open or not) and the
/// left pane in `left_mode`.
fn seed_right_dock(store: &LayoutStore, right: f32, open: bool, left_mode: LeftPaneMode) {
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
                    px(right),
                    open,
                )),
                bottom_dock: None,
            },
            LeftPaneLayout {
                mode: left_mode,
                expanded_width: DEFAULT_LEFT_PANE_WIDTH,
            },
        ))
        .unwrap();
}

fn right_width(cx: &mut TestAppContext, dock: &Entity<DockArea>) -> Pixels {
    dock.read_with(cx, |area, _| area.dock_size(DockPlacement::Right).unwrap())
}

/// The right dock's width in the layout file, once the debounce has elapsed.
fn saved_right_width(cx: &mut TestAppContext, store: &LayoutStore) -> Pixels {
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    saved_dock(
        &store.load().expect("the layout was written"),
        DockPlacement::Right,
    )
    .expect("the right dock was written")
    .size
}

fn center_width(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    state: &Entity<ShellState>,
) -> f64 {
    let mut scale = 1.0;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        scale = f64::from(window.scale_factor());
    })
    .unwrap();
    state
        .read_with(cx, |state, _| state.center_rect())
        .unwrap()
        .w
        / scale
}

#[gpui_kit::test]
fn the_preferred_right_dock_width_comes_back_when_the_left_pane_compacts(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(home.path());
    seed_right_dock(&store, 500., true, LeftPaneMode::Compact);
    let (window, shell) = open_shell_full(
        cx,
        ShellModel::new(),
        Some(store.clone()),
        None,
        (1000., 700.),
    );
    let (state, dock) =
        shell.read_with(cx, |shell, _| (shell.state().clone(), shell.dock().clone()));
    assert_eq!(right_width(cx, &dock), px(500.));

    // Expanding makes the dock give way, keeping the center wide enough.
    cx.update_window(window.into(), |_, window, cx| window.press("cmd-b", cx))
        .unwrap();
    assert_eq!(mode(cx, &state), LeftPaneMode::Expanded);
    let shrunk = right_width(cx, &dock);
    assert_eq!(
        shrunk,
        px(1000. - RAIL_WIDTH) - DEFAULT_LEFT_PANE_WIDTH - MIN_CENTER_WIDTH
    );
    assert!(shrunk < px(500.));
    assert!(center_width(cx, window, &state) >= f64::from(f32::from(MIN_CENTER_WIDTH)) - 1.0);
    assert_eq!(
        saved_right_width(cx, &store),
        px(500.),
        "the file keeps the preference"
    );

    // Compacting gives the dock its width back, exactly.
    cx.update_window(window.into(), |_, window, cx| window.press("cmd-b", cx))
        .unwrap();
    assert_eq!(mode(cx, &state), LeftPaneMode::Compact);
    assert_eq!(right_width(cx, &dock), px(500.));
    assert_eq!(saved_right_width(cx, &store), px(500.));
}

#[gpui_kit::test]
fn a_fit_while_the_right_dock_is_closed_keeps_the_preference(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(home.path());
    seed_right_dock(&store, 500., false, LeftPaneMode::Compact);
    let (window, shell) = open_shell_full(
        cx,
        ShellModel::new(),
        Some(store.clone()),
        None,
        (1000., 700.),
    );
    let (state, dock) =
        shell.read_with(cx, |shell, _| (shell.state().clone(), shell.dock().clone()));
    assert!(!dock.read_with(cx, |dock, _| dock.is_dock_open(DockPlacement::Right)));

    cx.update_window(window.into(), |_, window, cx| window.press("cmd-b", cx))
        .unwrap();
    assert_eq!(mode(cx, &state), LeftPaneMode::Expanded);
    assert_eq!(
        saved_right_width(cx, &store),
        px(500.),
        "closed: still 500 in the file"
    );

    // Opening it from the rail shows the fitted width.
    cx.update_window(window.into(), |_, window, cx| {
        window.click(RailEntry::Settings.element_id(), cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert!(dock.read_with(cx, |dock, _| dock.is_dock_open(DockPlacement::Right)));
    assert_eq!(
        right_width(cx, &dock),
        px(1000. - RAIL_WIDTH) - DEFAULT_LEFT_PANE_WIDTH - MIN_CENTER_WIDTH
    );
    assert!(center_width(cx, window, &state) >= f64::from(f32::from(MIN_CENTER_WIDTH)) - 1.0);

    cx.update_window(window.into(), |_, window, cx| window.press("cmd-b", cx))
        .unwrap();
    assert_eq!(right_width(cx, &dock), px(500.));
    assert_eq!(saved_right_width(cx, &store), px(500.));
}

#[gpui_kit::test]
fn dragging_the_right_docks_handle_sets_the_preferred_width(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(home.path());
    seed_right_dock(&store, 300., true, LeftPaneMode::Compact);
    let (window, shell) = open_shell_full(
        cx,
        ShellModel::new(),
        Some(store.clone()),
        None,
        (1000., 700.),
    );
    let (state, dock) =
        shell.read_with(cx, |shell, _| (shell.state().clone(), shell.dock().clone()));

    let mut from = point(px(0.), px(0.));
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        from = window.find("resize-handle-right").bounds().center();
    })
    .unwrap();
    // The dock's right edge is the rail's left edge.
    let to = point(px(1000. - RAIL_WIDTH - 450.), from.y);
    cx.update_window(window.into(), |_, window, cx| window.drag(from, to, cx))
        .unwrap();
    assert_eq!(right_width(cx, &dock), px(450.));
    assert_eq!(
        saved_right_width(cx, &store),
        px(450.),
        "the drag is the new preference"
    );

    // It survives an expand and compact cycle.
    for _ in 0..2 {
        cx.update_window(window.into(), |_, window, cx| window.press("cmd-b", cx))
            .unwrap();
    }
    assert_eq!(mode(cx, &state), LeftPaneMode::Compact);
    assert_eq!(right_width(cx, &dock), px(450.));
    assert_eq!(saved_right_width(cx, &store), px(450.));
}

#[gpui_kit::test]
fn resizing_the_window_refits_an_open_right_dock(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(home.path());
    seed_right_dock(&store, 500., true, LeftPaneMode::Compact);
    let (window, shell) = open_shell_full(
        cx,
        ShellModel::new(),
        Some(store.clone()),
        None,
        (1000., 700.),
    );
    let (state, dock) =
        shell.read_with(cx, |shell, _| (shell.state().clone(), shell.dock().clone()));
    assert_eq!(right_width(cx, &dock), px(500.));

    cx.simulate_window_resize(window.into(), size(px(800.), px(700.)));
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    assert_eq!(
        right_width(cx, &dock),
        px(800. - RAIL_WIDTH) - COMPACT_LEFT_PANE_WIDTH - MIN_CENTER_WIDTH
    );
    assert!(center_width(cx, window, &state) >= f64::from(f32::from(MIN_CENTER_WIDTH)) - 1.0);
    assert_eq!(saved_right_width(cx, &store), px(500.));

    cx.simulate_window_resize(window.into(), size(px(1000.), px(700.)));
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    assert_eq!(right_width(cx, &dock), px(500.));
    assert_eq!(saved_right_width(cx, &store), px(500.));
}

// ---- Painted colors --------------------------------------------------------

/// A point `inset` inside the bottom-left corner of `bounds`, clear of any
/// control placed at the top or in the middle.
fn near_bottom_left(bounds: Bounds<Pixels>, inset: f32) -> Point<Pixels> {
    point(
        bounds.origin.x + px(inset),
        bounds.origin.y + bounds.size.height - px(inset),
    )
}

struct Painted {
    title_bar: Background,
    left_pane: Background,
    right_pane: Background,
    rail: Background,
    center: Background,
    tab_strip: Background,
    active_tab: Background,
}

fn painted_regions(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Painted {
    let mut painted = None;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let bounds = |id: &str, window: &mut gpui_kit::Window| window.find(id.to_string()).bounds();
        let title = bounds(TITLE_BAR_ID, window);
        let left = bounds(LEFT_PANE_ID, window);
        let right = bounds(SETTINGS_PANEL, window);
        let rail = bounds(RAIL_ID, window);
        let center = bounds(CENTER_PANEL_ID, window);
        let tab = window.within("center-tabs").find(0usize).bounds();
        painted = Some(Painted {
            title_bar: painted_at(window, near_bottom_left(title, 3.)),
            left_pane: painted_at(window, near_bottom_left(left, 20.)),
            right_pane: painted_at(window, near_bottom_left(right, 20.)),
            rail: painted_at(window, near_bottom_left(rail, 10.)),
            center: painted_at(window, near_bottom_left(center, 20.)),
            tab_strip: painted_at(
                window,
                point(
                    center.origin.x + center.size.width - px(6.),
                    tab.origin.y + tab.size.height / 2.,
                ),
            ),
            active_tab: painted_at(
                window,
                point(tab.origin.x + px(2.), tab.origin.y + tab.size.height / 2.),
            ),
        });
    })
    .unwrap();
    painted.unwrap()
}

#[gpui_kit::test]
fn every_region_paints_its_token_color(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let (window, _shell) = open_shell(cx, connected_with_tab(&mercury));
    let set = ThemeSet::embedded();
    let native = set.native(&theme_state(Appearance::Dark).theme).unwrap();
    let painted = painted_regions(cx, window);

    for (region, got) in [
        (Region::TitleBar, painted.title_bar),
        (Region::LeftPane, painted.left_pane),
        (Region::RightPane, painted.right_pane),
        (Region::Rail, painted.rail),
        (Region::Center, painted.center),
        (Region::TabStrip, painted.tab_strip),
    ] {
        let want = token_background(&native, region_tokens(region).0);
        assert_eq!(got, want, "{region:?}");
    }
    // The active tab is part of the content: the center surface.
    assert_eq!(
        painted.active_tab,
        token_background(&native, region_tokens(Region::Center).0),
        "active tab"
    );
    // The two card surfaces.
    assert_eq!(painted.left_pane, painted.right_pane, "both panes: card");
    assert_ne!(painted.rail, painted.left_pane, "rail card-2 vs pane card");
    assert_eq!(painted.rail, painted.tab_strip);
    assert_eq!(painted.rail, painted.title_bar);
}

#[gpui_kit::test]
fn switching_the_theme_repaints_the_regions(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let (window, shell) = open_shell(cx, connected_with_tab(&mercury));
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    let set = ThemeSet::embedded();
    let dark = painted_regions(cx, window);

    let light_state = theme_state(Appearance::Light);
    // The user's choice, through the state's ThemeController.
    state.update(cx, |state, cx| {
        state.set_appearance(
            AppearanceSettings {
                theme: ThemeChoice::Id(light_state.theme.clone()),
                ..AppearanceSettings::default()
            },
            cx,
        )
    });
    let light = painted_regions(cx, window);

    let native = set.native(&light_state.theme).unwrap();
    for (region, got) in [
        (Region::LeftPane, light.left_pane),
        (Region::Rail, light.rail),
        (Region::RightPane, light.right_pane),
        (Region::TitleBar, light.title_bar),
        (Region::Center, light.center),
        (Region::TabStrip, light.tab_strip),
    ] {
        assert_eq!(
            got,
            token_background(&native, region_tokens(region).0),
            "{region:?}"
        );
    }
    // The active tab is part of the content: the center surface.
    assert_eq!(
        light.active_tab,
        token_background(&native, region_tokens(Region::Center).0),
        "active tab"
    );
    assert_ne!(dark.left_pane, light.left_pane);
    assert_ne!(dark.rail, light.rail);
    assert_ne!(dark.tab_strip, light.tab_strip);
    assert_ne!(dark.active_tab, light.active_tab);
}

#[gpui_kit::test]
fn the_title_bar_spans_the_window_and_the_center_sits_below_it(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let (window, shell) = open_shell(cx, connected_with_tab(&mercury));
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    let seen: Rc<RefCell<Vec<PixelRect>>> = Rc::default();
    state.update(cx, |state, cx| {
        let seen = seen.clone();
        state.set_center_bounds_listener(move |rect, _| seen.borrow_mut().push(rect), cx);
    });
    let mut title = None;
    let mut left = None;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        title = Some(window.find(TITLE_BAR_ID).bounds());
        left = Some(window.find(LEFT_PANE_ID).bounds());
    })
    .unwrap();
    let (title, left) = (title.unwrap(), left.unwrap());
    assert_eq!(title.size.width, px(1200.), "the bar spans the window");
    assert_eq!(title.origin.y, px(0.));
    assert!(left.origin.y >= title.size.height, "the left pane is below");
    let rect = *seen.borrow().last().expect("the center reported");
    let scale = rect.scale;
    assert!(
        rect.y >= f64::from(f32::from(title.size.height)) * scale,
        "{rect:?} vs title {title:?}"
    );
}

#[gpui_kit::test]
fn the_center_pane_paints_its_own_token_not_the_background(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    // Tokens where the center has a color no other surface has (in the
    // shipped themes it equals the background).
    let mut set = ThemeSet::embedded().clone();
    for theme in &mut set.themes {
        let distinct = theme.colors["selection"];
        theme.colors.insert("center-pane".into(), distinct);
    }
    let set: &'static ThemeSet = Box::leak(Box::new(set));
    let (window, _) = open_shell_options(
        cx,
        ShellOptions {
            model: connected_with_tab(&mercury),
            theme_set: Some(set),
            ..Default::default()
        },
        (1200., 800.),
    );
    let native = set.native(&theme_state(Appearance::Dark).theme).unwrap();
    let painted = painted_regions(cx, window);
    assert_eq!(painted.center, token_background(&native, "selection"));
    assert_ne!(painted.center, token_background(&native, "background"));
}

#[gpui_kit::test]
fn a_daemon_of_another_version_is_named_in_the_banner_and_the_status_bar(cx: &mut TestAppContext) {
    let failure = LinkFailure::VersionMismatch {
        running: "0.0.9".into(),
        expected: "0.1.0".into(),
    };
    let wording = "A different SmithAgents daemon version is running (0.0.9; this app needs \
                   0.1.0). Quit it to continue.";
    let (window, shell) = open_shell_options(
        cx,
        ShellOptions {
            link_failure: Some(Rc::new(move || Some(failure.clone()))),
            ..Default::default()
        },
        (1200., 800.),
    );
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    state.update(cx, |state, cx| {
        state.dispatch(Action::ConnectionChanged(Connection::Reconnecting), cx)
    });
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(BANNER_ID).label(), Some(wording));
    })
    .unwrap();
    let status = state
        .read_with(cx, |state, _| state.status_message())
        .unwrap();
    assert_eq!(status.kind, StatusKind::Error);
    assert_eq!(status.text, wording);
}

#[gpui_kit::test]
fn streamed_chunks_do_not_redraw_the_shell_but_a_state_change_does(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let (_window, shell) = open_shell(cx, connected_with_tab(&mercury));
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    cx.run_until_parked();
    let notified = Rc::new(Cell::new(0usize));
    let _observe = cx.update(|cx| {
        let notified = notified.clone();
        cx.observe(&state, move |_, _| notified.set(notified.get() + 1))
    });
    let event = |seq: u64, kind: EventKind| {
        Action::Event(Event {
            seq,
            at: mercury.created_at,
            kind,
        })
    };

    for seq in 2..12 {
        let chunk = event(
            seq,
            EventKind::TurnDelta {
                agent_id: mercury.id,
                text: format!("chunk {seq}"),
            },
        );
        state.update(cx, |state, cx| state.dispatch(chunk, cx));
        cx.run_until_parked();
    }
    assert_eq!(
        notified.get(),
        0,
        "streamed text is the web tab's, not the shell's"
    );

    let working = Agent {
        state: AgentState::Working,
        ..mercury.clone()
    };
    let update = event(12, EventKind::AgentUpdated(working));
    state.update(cx, |state, cx| state.dispatch(update, cx));
    cx.run_until_parked();
    assert_eq!(notified.get(), 1, "the agent's state shows in the pane");
}

//! Headless checks of the icon rail and the right dock it drives: Settings
//! and Intents share the dock, one pane at a time and without a tab bar; the
//! rail, ⌘⌥I and a reveal switch or hide it; the pane shown is remembered in
//! `app-state.json`.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use common::ui::{agent, open_shell, open_shell_options};
use gpui_kit::base::Root;
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::test::{TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{AppContext as _, ElementId, Entity, Pixels, Role, TestAppContext, WindowHandle};
use smithagents_app::app_state::AppStateStore;
use smithagents_app::model::{Action, Connection, ShellModel};
use smithagents_app::shell::{Shell, ShellOptions};
use smithagents_app::view_models::SettingsSection;
use smithagents_app::views::agents::agent_row_id;
use smithagents_app::views::intents::INTENTS_TITLE_ID;
use smithagents_app::views::rail::RailEntry;
use smithagents_app::views::settings::{REPO_PATH_INPUT_ID, SETTINGS_TITLE_ID};
use smithagents_core::ids::AgentId;

fn frame(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

/// Runs `f` on the window after a fresh frame.
fn with_window<R>(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App) -> R,
) -> R {
    let mut out = None;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        out = Some(f(window, cx));
    })
    .unwrap();
    out.unwrap()
}

fn click(cx: &mut TestAppContext, window: WindowHandle<Root>, entry: RailEntry) {
    with_window(cx, window, |window, cx| {
        window.click(entry.element_id(), cx)
    });
    frame(cx, window);
}

fn press(cx: &mut TestAppContext, window: WindowHandle<Root>, key: &str) {
    with_window(cx, window, |window, cx| window.press(key, cx));
    frame(cx, window);
}

fn rail_active(cx: &mut TestAppContext, shell: &Entity<Shell>) -> Option<RailEntry> {
    shell.read_with(cx, |shell, cx| shell.rail_active(cx))
}

/// Whether the right dock is open, and its width.
fn right_dock(cx: &mut TestAppContext, shell: &Entity<Shell>) -> (bool, Option<Pixels>) {
    let dock = shell.read_with(cx, |shell, _| shell.dock().clone());
    dock.read_with(cx, |dock, _| {
        (
            dock.is_dock_open(DockPlacement::Right),
            dock.dock_size(DockPlacement::Right),
        )
    })
}

/// Whether the Settings heading and the Intents heading are drawn.
fn headings(cx: &mut TestAppContext, window: WindowHandle<Root>) -> (bool, bool) {
    with_window(cx, window, |window, _| {
        (
            window.try_find(SETTINGS_TITLE_ID).is_some(),
            window.try_find(INTENTS_TITLE_ID).is_some(),
        )
    })
}

fn open(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Shell>) {
    let (window, shell) = open_shell(cx, ShellModel::new());
    frame(cx, window);
    (window, shell)
}

#[gpui_kit::test]
fn the_rail_lists_settings_then_intents(cx: &mut TestAppContext) {
    assert_eq!(RailEntry::ALL, [RailEntry::Settings, RailEntry::Intents]);
    for &entry in RailEntry::ALL {
        assert_eq!(RailEntry::parse(entry.as_str()), Some(entry));
        assert_eq!(entry.placement(), DockPlacement::Right);
    }
    assert_eq!(RailEntry::parse("nope"), None);

    let (window, _shell) = open(cx);
    with_window(cx, window, |window, _| {
        let settings = window.find(RailEntry::Settings.element_id());
        let intents = window.find(RailEntry::Intents.element_id());
        assert_eq!(settings.label(), Some("Settings"));
        assert_eq!(intents.label(), Some("Intents"));
        assert!(
            intents.bounds().top() >= settings.bounds().bottom(),
            "Intents {:?} sits below Settings {:?}",
            intents.bounds(),
            settings.bounds()
        );
    });
}

#[gpui_kit::test]
fn selecting_intents_switches_the_right_pane_without_a_tab_bar(cx: &mut TestAppContext) {
    let (window, shell) = open(cx);
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Settings));
    assert_eq!(headings(cx, window), (true, false));
    let (open_before, width_before) = right_dock(cx, &shell);
    assert!(open_before);

    click(cx, window, RailEntry::Intents);

    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Intents));
    with_window(cx, window, |window, _| {
        let heading = window.find(INTENTS_TITLE_ID);
        assert!(heading.visible());
        assert_eq!(heading.role(), Some(Role::Heading));
        assert_eq!(heading.label(), Some("Intents"));
        assert!(window.try_find(SETTINGS_TITLE_ID).is_none());
        assert!(window.find_all_by_role(Role::Tab).is_empty(), "no tab bar");
    });
    assert_eq!(
        right_dock(cx, &shell),
        (true, width_before),
        "the dock stays open at its width"
    );
}

#[gpui_kit::test]
fn selecting_the_entry_on_screen_closes_the_dock(cx: &mut TestAppContext) {
    let (window, shell) = open(cx);

    click(cx, window, RailEntry::Intents);
    click(cx, window, RailEntry::Intents);
    assert!(!right_dock(cx, &shell).0, "the dock is closed");
    assert_eq!(rail_active(cx, &shell), None);

    click(cx, window, RailEntry::Settings);
    assert!(right_dock(cx, &shell).0, "the dock is open");
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Settings));
    assert_eq!(headings(cx, window), (true, false));
}

#[gpui_kit::test]
fn the_right_pane_is_remembered(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let store = AppStateStore::new(home.path());
    let options = || ShellOptions {
        app_state: Some(store.clone()),
        ..Default::default()
    };
    let (window, shell) = open_shell_options(cx, options(), (1200., 800.));
    frame(cx, window);
    click(cx, window, RailEntry::Intents);
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Intents));
    assert_eq!(store.load().right_pane, Some(RailEntry::Intents));
    drop(shell);
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    let (window, shell) = open_shell_options(cx, options(), (1200., 800.));
    frame(cx, window);
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Intents));
    assert_eq!(headings(cx, window), (false, true));
}

#[gpui_kit::test]
fn revealing_settings_from_intents_switches_back(cx: &mut TestAppContext) {
    let (window, shell) = open(cx);
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    click(cx, window, RailEntry::Intents);
    assert_eq!(headings(cx, window), (false, true));

    state.update(cx, |state, cx| {
        state.request_settings_focus(SettingsSection::Repositories, cx)
    });
    frame(cx, window);

    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Settings));
    assert_eq!(headings(cx, window), (true, false));
}

#[gpui_kit::test]
fn cmd_alt_i_toggles_intents(cx: &mut TestAppContext) {
    let (window, shell) = open(cx);

    press(cx, window, "cmd-alt-i");
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Intents));
    assert_eq!(headings(cx, window), (false, true));

    press(cx, window, "cmd-alt-i");
    assert!(!right_dock(cx, &shell).0, "the dock is closed");
    assert_eq!(rail_active(cx, &shell), None);

    // The closed pane had the focus; the key still works without it.
    press(cx, window, "cmd-alt-i");
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Intents));
    assert_eq!(headings(cx, window), (false, true));
}

#[gpui_kit::test]
fn clicking_intents_after_closing_the_dock_shows_intents_again(cx: &mut TestAppContext) {
    let (window, shell) = open(cx);
    click(cx, window, RailEntry::Intents);
    click(cx, window, RailEntry::Intents);
    assert!(!right_dock(cx, &shell).0, "the dock is closed");

    click(cx, window, RailEntry::Intents);

    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Intents));
    with_window(cx, window, |window, _| {
        assert!(window.find(INTENTS_TITLE_ID).visible());
        assert!(window.try_find(SETTINGS_TITLE_ID).is_none());
    });
}

/// Opens a connected shell with one agent, so the agents pane has a row to
/// focus.
fn open_with_an_agent(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Shell>, AgentId) {
    let mercury = agent("Mercury");
    let mut model = ShellModel::new();
    model.apply(Action::Snapshot {
        agents: vec![mercury.clone()],
        repos: vec![],
        latest_seq: 1,
    });
    model.apply(Action::ConnectionChanged(Connection::Connected));
    let (window, shell) = open_shell(cx, model);
    frame(cx, window);
    (window, shell, mercury.id)
}

/// Clicks `id` and checks that it took the focus.
fn focus(cx: &mut TestAppContext, window: WindowHandle<Root>, id: ElementId) {
    with_window(cx, window, |window, cx| window.click(id.clone(), cx));
    frame(cx, window);
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(id.clone()).focused(), Some(true), "{id:?}");
    });
}

// A view with its own key context (the agents pane, a text field) sits
// deeper than the shell's, where a binding without a context ranks too: the
// shell's keys must still win there.

#[gpui_kit::test]
fn cmd_alt_i_shows_intents_while_an_agent_row_has_the_focus(cx: &mut TestAppContext) {
    let (window, shell, agent) = open_with_an_agent(cx);
    focus(cx, window, agent_row_id(agent));

    press(cx, window, "cmd-alt-i");

    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Intents));
}

#[gpui_kit::test]
fn cmd_alt_i_shows_intents_while_a_text_field_has_the_focus(cx: &mut TestAppContext) {
    let (window, shell) = open(cx);
    focus(cx, window, REPO_PATH_INPUT_ID.into());

    press(cx, window, "cmd-alt-i");

    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Intents));
}

#[gpui_kit::test]
fn cmd_alt_b_toggles_settings_while_a_text_field_or_an_agent_row_has_the_focus(
    cx: &mut TestAppContext,
) {
    let (window, shell, agent) = open_with_an_agent(cx);
    focus(cx, window, REPO_PATH_INPUT_ID.into());
    press(cx, window, "cmd-alt-b");
    assert_eq!(rail_active(cx, &shell), None, "Settings was on screen");

    focus(cx, window, agent_row_id(agent));
    press(cx, window, "cmd-alt-b");
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Settings));
}

fn is_focused(cx: &mut TestAppContext, window: WindowHandle<Root>, id: ElementId) -> bool {
    with_window(cx, window, |window, _| {
        window.find(id).focused() == Some(true)
    })
}

#[gpui_kit::test]
fn closing_a_pane_gives_the_focus_back(cx: &mut TestAppContext) {
    let (window, shell, agent) = open_with_an_agent(cx);
    let row = agent_row_id(agent);
    // Settings is on screen at first: close it.
    press(cx, window, "cmd-alt-b");
    assert_eq!(rail_active(cx, &shell), None);

    for key in ["cmd-alt-b", "cmd-alt-i"] {
        focus(cx, window, row.clone());
        press(cx, window, key);
        assert!(rail_active(cx, &shell).is_some(), "{key} shows its pane");
        assert!(
            !is_focused(cx, window, row.clone()),
            "the pane took the focus"
        );
        press(cx, window, key);
        assert_eq!(rail_active(cx, &shell), None, "{key} closes it");
        assert!(is_focused(cx, window, row.clone()), "{key}: the row again");
    }

    // Switching panes first: closing the second still returns to the row.
    focus(cx, window, row.clone());
    press(cx, window, "cmd-alt-b");
    press(cx, window, "cmd-alt-i");
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Intents));
    press(cx, window, "cmd-alt-i");
    assert_eq!(rail_active(cx, &shell), None);
    assert!(is_focused(cx, window, row), "back on the row");
}

#[gpui_kit::test]
fn closing_a_pane_after_its_focus_source_left_focuses_the_shell(cx: &mut TestAppContext) {
    let (window, shell, agent) = open_with_an_agent(cx);
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    press(cx, window, "cmd-alt-b");
    focus(cx, window, agent_row_id(agent));
    press(cx, window, "cmd-alt-i");

    // The row goes while the pane is open.
    state.update(cx, |state, cx| {
        state.dispatch(
            Action::Snapshot {
                agents: vec![],
                repos: vec![],
                latest_seq: 2,
            },
            cx,
        )
    });
    frame(cx, window);
    press(cx, window, "cmd-alt-i");
    assert_eq!(rail_active(cx, &shell), None);

    // The shell has the focus, so its keys still work.
    press(cx, window, "cmd-alt-i");
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Intents));
}

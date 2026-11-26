//! Headless checks of the icon rail and the right dock it drives: Settings
//! and Intents share the dock, one pane at a time and without a tab bar; the
//! rail, ⌘⌥I and a reveal switch or hide it; the pane shown is remembered in
//! `app-state.json`.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use common::ui::{open_shell, open_shell_options};
use gpui_kit::base::Root;
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::test::{TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{AppContext as _, Entity, Pixels, Role, TestAppContext, WindowHandle};
use smithagents_app::app_state::AppStateStore;
use smithagents_app::model::ShellModel;
use smithagents_app::shell::{Shell, ShellOptions};
use smithagents_app::view_models::SettingsSection;
use smithagents_app::views::intents::INTENTS_TITLE_ID;
use smithagents_app::views::rail::RailEntry;
use smithagents_app::views::settings::SETTINGS_TITLE_ID;

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
}

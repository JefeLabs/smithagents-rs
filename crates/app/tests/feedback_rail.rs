//! The Feedback rail entry: where it sits, its pane in the right dock, ⌘⌥F,
//! and the 3-second press-and-hold that starts a capture. Time is the test
//! executor's clock (`advance_clock`) and the capture's frame hops are run by
//! hand, so nothing waits and nothing reaches the screen.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::time::Duration;

use common::feedback::{run_hop, FeedbackSetup};
use common::ui::{agent, open_shell_options, painted_at};
use gpui_kit::base::Root;
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::test::{TestEventExt as _, TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{
    point, px, AppContext as _, Background, ElementId, Entity, MouseButton, Pixels, Point, Role,
    TestAppContext, VisualTestContext, WindowHandle,
};
use smithagents_app::app_state::AppStateStore;
use smithagents_app::model::{Action, Connection, ShellModel};
use smithagents_app::shell::{Shell, ShellOptions};
use smithagents_app::view_models::SettingsSection;
use smithagents_app::views::agents::agent_row_id;
use smithagents_app::views::feedback::{FEEDBACK_TITLE_ID, HOLD_RING_ID};
use smithagents_app::views::rail::RailEntry;
use smithagents_app::views::settings::{REPO_PATH_INPUT_ID, SETTINGS_TITLE_ID};
use smithagents_capture::CaptureError;

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

/// Opens the shell from `options` (a [`FeedbackSetup`]'s, at least) in a
/// 1200x800 window.
fn open(cx: &mut TestAppContext, options: ShellOptions) -> (WindowHandle<Root>, Entity<Shell>) {
    let (window, shell) = open_shell_options(cx, options, (1200., 800.));
    frame(cx, window);
    (window, shell)
}

fn click(cx: &mut TestAppContext, window: WindowHandle<Root>, entry: RailEntry) {
    with_window(cx, window, |window, cx| {
        window.click(entry.element_id(), cx)
    });
    frame(cx, window);
}

fn press_key(cx: &mut TestAppContext, window: WindowHandle<Root>, key: &str) {
    with_window(cx, window, |window, cx| window.press(key, cx));
    frame(cx, window);
}

/// The center of the Feedback button.
fn button_center(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Point<Pixels> {
    with_window(cx, window, |window, _| {
        window
            .find(RailEntry::Feedback.element_id())
            .bounds()
            .center()
    })
}

/// The middle of the window, well away from the rail.
fn window_center() -> Point<Pixels> {
    point(px(600.), px(400.))
}

/// Moves onto the Feedback button and presses the left button there.
fn press(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    let at = button_center(cx, window);
    with_window(cx, window, |window, cx| {
        window.pointer_move(at, None, cx);
        window.pointer_down(at, MouseButton::Left, cx);
    });
    frame(cx, window);
}

/// Moves the pointer to `to` with the left button held.
fn drag_to(cx: &mut TestAppContext, window: WindowHandle<Root>, to: Point<Pixels>) {
    with_window(cx, window, |window, cx| {
        window.pointer_move(to, Some(MouseButton::Left), cx)
    });
    frame(cx, window);
}

/// Releases the left button at `at`.
fn release_at(cx: &mut TestAppContext, window: WindowHandle<Root>, at: Point<Pixels>) {
    with_window(cx, window, |window, cx| {
        window.pointer_up(at, MouseButton::Left, cx)
    });
    frame(cx, window);
}

/// Releases the left button on the Feedback button.
fn release(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    let at = button_center(cx, window);
    release_at(cx, window, at);
}

/// Moves the test clock on by `ms` milliseconds, running what falls due.
fn advance(cx: &mut TestAppContext, window: WindowHandle<Root>, ms: u64) {
    cx.executor().advance_clock(Duration::from_millis(ms));
    frame(cx, window);
}

fn ring_shown(cx: &mut TestAppContext, window: WindowHandle<Root>) -> bool {
    with_window(cx, window, |window, _| {
        window.try_find(HOLD_RING_ID).is_some()
    })
}

fn progress(cx: &mut TestAppContext, shell: &Entity<Shell>) -> f32 {
    shell.read_with(cx, |shell, cx| shell.hold_button().read(cx).progress(cx))
}

fn rail_active(cx: &mut TestAppContext, shell: &Entity<Shell>) -> Option<RailEntry> {
    shell.read_with(cx, |shell, cx| shell.rail_active(cx))
}

fn dock_open(cx: &mut TestAppContext, shell: &Entity<Shell>) -> bool {
    let dock = shell.read_with(cx, |shell, _| shell.dock().clone());
    dock.read_with(cx, |dock, _| dock.is_dock_open(DockPlacement::Right))
}

/// The background `entry`'s rail button paints at its center, with the
/// pointer moved away first so no hover counts. (A `Button` does not report
/// `selected` to the test snapshot; a selected one paints its own
/// background.)
fn button_paint(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    entry: RailEntry,
) -> Background {
    with_window(cx, window, |window, cx| {
        window.pointer_move(window_center(), None, cx);
        let center = window.find(entry.element_id()).bounds().center();
        painted_at(window, center)
    })
}

/// The Feedback heading's accessible name, if the heading is drawn.
fn feedback_heading(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Option<String> {
    with_window(cx, window, |window, _| {
        window.try_find(FEEDBACK_TITLE_ID).map(|heading| {
            assert_eq!(heading.role(), Some(Role::Heading));
            heading.label().unwrap_or_default().to_owned()
        })
    })
}

fn settings_heading_shown(cx: &mut TestAppContext, window: WindowHandle<Root>) -> bool {
    with_window(cx, window, |window, _| {
        window.try_find(SETTINGS_TITLE_ID).is_some()
    })
}

#[gpui_kit::test]
fn feedback_sits_directly_under_settings(cx: &mut TestAppContext) {
    assert_eq!(
        RailEntry::ALL,
        [RailEntry::Settings, RailEntry::Feedback, RailEntry::Intents]
    );
    assert_eq!(RailEntry::Feedback.label(), "Feedback");
    assert_eq!(RailEntry::Feedback.as_str(), "feedback");
    assert_eq!(RailEntry::parse("feedback"), Some(RailEntry::Feedback));
    assert_eq!(RailEntry::Feedback.placement(), DockPlacement::Right);
    assert_eq!(
        RailEntry::Feedback.element_id(),
        ElementId::from("rail-feedback")
    );

    let setup = FeedbackSetup::new();
    let (window, shell) = open(cx, setup.options());
    click(cx, window, RailEntry::Feedback);
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    with_window(cx, window, |window, _| {
        let settings = window.find(RailEntry::Settings.element_id()).bounds();
        let feedback = window.find(RailEntry::Feedback.element_id());
        let intents = window.find(RailEntry::Intents.element_id()).bounds();
        assert!(
            feedback.bounds().top() >= settings.bottom(),
            "Feedback {:?} sits below Settings {settings:?}",
            feedback.bounds()
        );
        assert!(
            feedback.bounds().bottom() <= intents.top(),
            "Feedback {:?} sits above Intents {intents:?}",
            feedback.bounds()
        );
        assert_eq!(feedback.label(), Some("Feedback"));
        assert_eq!(feedback.description(), Some("Feedback"), "the tooltip");
        assert!(window.find_all_by_role(Role::Tab).is_empty(), "no tab bar");
    });
}

#[gpui_kit::test]
fn a_click_opens_the_feedback_pane_and_a_second_click_closes_it(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell) = open(cx, setup.options());
    // Settings is on screen at first, its button selected.
    let selected = button_paint(cx, window, RailEntry::Settings);
    assert_ne!(button_paint(cx, window, RailEntry::Feedback), selected);
    click(cx, window, RailEntry::Settings);
    assert!(!dock_open(cx, &shell));

    click(cx, window, RailEntry::Feedback);
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    assert!(dock_open(cx, &shell));
    assert_eq!(feedback_heading(cx, window).as_deref(), Some("FEEDBACK"));
    assert_eq!(button_paint(cx, window, RailEntry::Feedback), selected);
    assert_ne!(button_paint(cx, window, RailEntry::Settings), selected);

    click(cx, window, RailEntry::Feedback);
    assert!(!dock_open(cx, &shell), "the dock is closed");
    assert_eq!(rail_active(cx, &shell), None);
    assert_ne!(button_paint(cx, window, RailEntry::Feedback), selected);

    // With Settings showing, Feedback switches the dock to it.
    click(cx, window, RailEntry::Settings);
    assert!(settings_heading_shown(cx, window));
    click(cx, window, RailEntry::Feedback);
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    assert!(dock_open(cx, &shell));
    assert!(!settings_heading_shown(cx, window));
    assert_eq!(feedback_heading(cx, window).as_deref(), Some("FEEDBACK"));
    assert_eq!(button_paint(cx, window, RailEntry::Feedback), selected);
}

#[gpui_kit::test]
fn the_ring_fills_over_three_seconds(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell) = open(cx, setup.options());
    assert!(!ring_shown(cx, window));
    assert_eq!(progress(cx, &shell), 0.0);

    press(cx, window);
    advance(cx, window, 1500);
    let half = progress(cx, &shell);
    assert!((half - 0.5).abs() <= 0.02, "progress at 1.5 s: {half}");
    assert!(ring_shown(cx, window));

    release(cx, window);
    assert!(!ring_shown(cx, window));
    assert_eq!(progress(cx, &shell), 0.0);
}

#[gpui_kit::test]
fn a_release_just_before_three_seconds_is_a_click(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell) = open(cx, setup.options());

    press(cx, window);
    advance(cx, window, 2990);
    release(cx, window);

    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    assert!(setup.capturer.calls().is_empty());
    assert_eq!(setup.hops.pending(), 0);

    advance(cx, window, 3000);
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    assert!(setup.capturer.calls().is_empty());
    assert_eq!(setup.hops.pending(), 0);
    assert!(!ring_shown(cx, window));
}

#[gpui_kit::test]
fn holding_three_seconds_captures_once_and_the_release_is_not_a_click(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell) = open(cx, setup.options());
    let before = rail_active(cx, &shell);

    press(cx, window);
    advance(cx, window, 2990);
    assert_eq!(setup.hops.pending(), 0, "not yet");
    advance(cx, window, 20);
    assert_eq!(setup.hops.pending(), 1, "the capture waits for a frame");
    assert!(
        !ring_shown(cx, window),
        "the ring is gone before the picture"
    );

    run_hop(cx, window, &setup);
    assert!(!ring_shown(cx, window));
    assert!(setup.capturer.calls().is_empty(), "one more frame first");
    assert_eq!(setup.hops.pending(), 1);
    run_hop(cx, window, &setup);
    assert!(!ring_shown(cx, window));
    assert_eq!(setup.capturer.calls().len(), 1);

    release(cx, window);
    assert_eq!(
        rail_active(cx, &shell),
        before,
        "the release did not toggle"
    );
    assert_eq!(setup.capturer.calls().len(), 1);
    assert_eq!(setup.hops.pending(), 0);
}

#[gpui_kit::test]
fn leaving_the_button_cancels_and_is_not_a_click(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell) = open(cx, setup.options());
    let before = (rail_active(cx, &shell), dock_open(cx, &shell));

    press(cx, window);
    drag_to(cx, window, window_center());
    advance(cx, window, 6000);
    assert_eq!(setup.hops.pending(), 0);
    assert!(setup.capturer.calls().is_empty());
    assert!(!ring_shown(cx, window));
    assert_eq!(progress(cx, &shell), 0.0);

    release_at(cx, window, window_center());
    assert_eq!((rail_active(cx, &shell), dock_open(cx, &shell)), before);
}

#[gpui_kit::test]
fn a_small_move_on_the_button_keeps_the_hold(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _shell) = open(cx, setup.options());

    press(cx, window);
    let on_button = button_center(cx, window) + point(px(1.), px(1.));
    drag_to(cx, window, on_button);
    advance(cx, window, 3010);

    assert_eq!(setup.hops.pending(), 1, "the hold completed");
}

#[gpui_kit::test]
fn a_failed_capture_after_a_hold_opens_the_pane(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    setup.capturer.set(Err(CaptureError::PermissionDenied));
    let (window, shell) = open(cx, setup.options());
    click(cx, window, RailEntry::Settings);
    assert!(!dock_open(cx, &shell), "the dock starts closed");

    press(cx, window);
    advance(cx, window, 3010);
    run_hop(cx, window, &setup);
    run_hop(cx, window, &setup);
    frame(cx, window);

    assert_eq!(setup.capturer.calls().len(), 1);
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    assert_eq!(feedback_heading(cx, window).as_deref(), Some("FEEDBACK"));
}

#[gpui_kit::test]
fn the_shortcut_toggles_the_pane(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell) = open(cx, setup.options());

    press_key(cx, window, "cmd-alt-f");
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    assert_eq!(feedback_heading(cx, window).as_deref(), Some("FEEDBACK"));

    press_key(cx, window, "cmd-alt-f");
    assert!(!dock_open(cx, &shell), "the dock is closed");
    assert_eq!(rail_active(cx, &shell), None);

    // The closed pane had the focus; the key still works without it.
    press_key(cx, window, "cmd-alt-f");
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
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
// deeper than the shell's: ⌘⌥F must still reach the shell from there.

#[gpui_kit::test]
fn the_shortcut_works_while_an_agent_row_has_the_focus(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let mercury = agent("Mercury");
    let mut model = ShellModel::new();
    model.apply(Action::Snapshot {
        agents: vec![mercury.clone()],
        repos: vec![],
        latest_seq: 1,
    });
    model.apply(Action::ConnectionChanged(Connection::Connected));
    let options = ShellOptions {
        model,
        ..setup.options()
    };
    let (window, shell) = open(cx, options);
    focus(cx, window, agent_row_id(mercury.id));

    press_key(cx, window, "cmd-alt-f");

    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
}

#[gpui_kit::test]
fn the_shortcut_works_while_a_text_field_has_the_focus(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell) = open(cx, setup.options());
    focus(cx, window, REPO_PATH_INPUT_ID.into());

    press_key(cx, window, "cmd-alt-f");

    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
}

#[gpui_kit::test]
fn the_feedback_pane_is_remembered(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let store = AppStateStore::new(setup.home.path());
    let options = || ShellOptions {
        app_state: Some(store.clone()),
        ..setup.options()
    };
    let (window, shell) = open(cx, options());
    click(cx, window, RailEntry::Feedback);
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    assert_eq!(store.load().right_pane, Some(RailEntry::Feedback));
    drop(shell);
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    let (window, shell) = open(cx, options());
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    assert_eq!(feedback_heading(cx, window).as_deref(), Some("FEEDBACK"));
}

#[gpui_kit::test]
fn reveal_settings_still_wins_over_feedback(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell) = open(cx, setup.options());
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    click(cx, window, RailEntry::Feedback);
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));

    state.update(cx, |state, cx| {
        state.request_settings_focus(SettingsSection::Repositories, cx)
    });
    frame(cx, window);

    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Settings));
    assert!(settings_heading_shown(cx, window));
    assert_eq!(feedback_heading(cx, window), None);
}

/// The window becomes the active one, as when the person clicks into it.
fn activate(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    with_window(cx, window, |window, _| window.activate_window());
    frame(cx, window);
}

/// Another window takes over: this one stops being active.
fn deactivate(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    VisualTestContext::from_window(window.into(), cx).deactivate_window();
    frame(cx, window);
}

/// Tab until the Feedback button has the keyboard focus.
fn tab_to_feedback(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    for _ in 0..60 {
        let focused = with_window(cx, window, |window, _| {
            window.find(RailEntry::Feedback.element_id()).focused()
        });
        if focused == Some(true) {
            return;
        }
        press_key(cx, window, "tab");
    }
    panic!("Tab never reached the Feedback button");
}

#[gpui_kit::test]
fn enter_on_the_feedback_entry_opens_the_pane_and_never_captures(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell) = open(cx, setup.options());
    activate(cx, window);
    tab_to_feedback(cx, window);

    press_key(cx, window, "enter");
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    assert_eq!(feedback_heading(cx, window).as_deref(), Some("FEEDBACK"));
    advance(cx, window, 6000);
    assert_eq!(setup.hops.pending(), 0, "no capture");
    assert!(setup.capturer.calls().is_empty());
    assert!(!ring_shown(cx, window));
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
}

#[gpui_kit::test]
fn a_hold_is_cancelled_when_the_window_goes_inactive(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell) = open(cx, setup.options());
    let before = rail_active(cx, &shell);
    activate(cx, window);

    press(cx, window);
    advance(cx, window, 1000);
    assert!(ring_shown(cx, window));
    deactivate(cx, window);
    advance(cx, window, 3000);

    assert_eq!(setup.hops.pending(), 0, "no capture");
    assert!(!ring_shown(cx, window));
    assert_eq!(progress(cx, &shell), 0.0);
    assert_eq!(rail_active(cx, &shell), before);
}

#[gpui_kit::test]
fn after_a_hold_whose_release_was_lost_the_next_key_press_is_a_click(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell) = open(cx, setup.options());
    let before = rail_active(cx, &shell);
    assert_ne!(before, Some(RailEntry::Feedback));
    activate(cx, window);

    press(cx, window);
    advance(cx, window, 3010);
    assert_eq!(setup.hops.pending(), 1, "the hold completed");
    // The button is released in another window: this one never hears it.
    deactivate(cx, window);
    activate(cx, window);
    tab_to_feedback(cx, window);
    press_key(cx, window, "enter");

    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    assert_eq!(feedback_heading(cx, window).as_deref(), Some("FEEDBACK"));
}

//! Headless checks of Settings -> Agent defaults: the limit on agents working
//! at once. Commands go to `FakeIntents`, so no daemon runs.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::rc::Rc;

use common::intents::{model_with, FakeIntents, IntentCall};
use common::ui::open_shell_options;
use gpui_kit::base::Root;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, Entity, Role, TestAppContext, WindowHandle};
use smithagents_app::model::Action;
use smithagents_app::shell::ShellOptions;
use smithagents_app::shell_state::ShellState;
use smithagents_app::view_models::{SettingsSection, NOT_CONNECTED_TEXT};
use smithagents_app::views::settings::{
    queue_limit_item_id, section_header_id, QUEUE_LIMIT_ERROR_ID, QUEUE_LIMIT_HELP,
    QUEUE_LIMIT_HELP_ID, QUEUE_LIMIT_ID, QUEUE_LIMIT_LABEL, QUEUE_LIMIT_LABEL_ID,
};
use smithagents_core::intent::AgentDefaults;

struct Opened {
    window: WindowHandle<Root>,
    state: Entity<ShellState>,
    fake: Rc<FakeIntents>,
    home: tempfile::TempDir,
}

/// Opens the shell, connected, with the defaults loaded as `limit` (or not
/// loaded at all for `None`), and the Settings pane on screen.
fn open(cx: &mut TestAppContext, defaults: Option<Option<u32>>) -> Opened {
    let fake = Rc::new(FakeIntents::default());
    let home = tempfile::tempdir().unwrap();
    let mut model = model_with(vec![], vec![], vec![]);
    if let Some(queue_limit) = defaults {
        model.apply(Action::AgentDefaultsLoaded(AgentDefaults { queue_limit }));
    }
    let (window, shell) = open_shell_options(
        cx,
        ShellOptions {
            model,
            intent_commands: Some(fake.clone()),
            app_state: Some(smithagents_app::app_state::AppStateStore::new(home.path())),
            ..Default::default()
        },
        (1200., 1000.),
    );
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    frame(cx, window);
    Opened {
        window,
        state,
        fake,
        home,
    }
}

fn frame(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

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

fn click(cx: &mut TestAppContext, window: WindowHandle<Root>, id: impl Into<ElementId>) {
    let id = id.into();
    with_window(cx, window, |window, cx| window.click(id, cx));
    frame(cx, window);
}

fn label_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    id: &'static str,
) -> Option<String> {
    with_window(cx, window, |window, _| {
        window
            .try_find(id)
            .and_then(|element| element.value().or(element.label()).map(str::to_owned))
    })
}

fn disabled(cx: &mut TestAppContext, window: WindowHandle<Root>) -> bool {
    with_window(cx, window, |window, _| {
        window.find(QUEUE_LIMIT_ID).disabled() == Some(true)
    })
}

/// The choice entries, in the menu's order, by element id, with whether
/// each is checked.
fn menu_entries(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Vec<(String, bool)> {
    with_window(cx, window, |window, _| {
        let menu = window.within("popup-menu");
        let items = menu.find_all_by_role(Role::MenuItem);
        std::iter::once(None)
            .chain((1..=16).map(Some))
            .filter_map(|limit| {
                let id = queue_limit_item_id(limit);
                let entry = menu.try_find(id.clone())?;
                let item = items
                    .iter()
                    .find(|item| entry.path().starts_with(item.path()))?;
                Some((id.to_string(), item.checked() == Some(true)))
            })
            .collect()
    })
}

fn calls(opened: &Opened) -> Vec<IntentCall> {
    opened.fake.calls.borrow().clone()
}

fn choose(cx: &mut TestAppContext, opened: &Opened, limit: Option<u32>) {
    click(cx, opened.window, QUEUE_LIMIT_ID);
    with_window(cx, opened.window, |window, cx| {
        window
            .within("popup-menu")
            .click(queue_limit_item_id(limit), cx)
    });
    frame(cx, opened.window);
}

fn open_pane(cx: &mut TestAppContext, opened: &Opened) {
    let _ = &opened.state;
    let _ = &opened.home;
    frame(cx, opened.window);
}

#[gpui_kit::test]
fn the_section_shows_the_current_limit(cx: &mut TestAppContext) {
    for (limit, shown) in [(None, "No limit"), (Some(3), "3")] {
        let opened = open(cx, Some(limit));
        open_pane(cx, &opened);
        let window = opened.window;
        assert_eq!(label_of(cx, window, QUEUE_LIMIT_ID).as_deref(), Some(shown));
        assert_eq!(
            label_of(cx, window, QUEUE_LIMIT_LABEL_ID).as_deref(),
            Some(QUEUE_LIMIT_LABEL)
        );
        assert_eq!(
            label_of(cx, window, QUEUE_LIMIT_HELP_ID).as_deref(),
            Some(QUEUE_LIMIT_HELP)
        );
        assert!(!disabled(cx, window));
    }
}

#[gpui_kit::test]
fn the_menu_offers_no_limit_and_one_to_sixteen(cx: &mut TestAppContext) {
    let opened = open(cx, Some(Some(3)));
    click(cx, opened.window, QUEUE_LIMIT_ID);
    let entries = menu_entries(cx, opened.window);
    assert_eq!(entries.len(), 17);
    let expected: Vec<(String, bool)> = std::iter::once(("queue-limit-none".to_owned(), false))
        .chain((1..=16).map(|n| (format!("queue-limit-{n}"), n == 3)))
        .collect();
    assert_eq!(entries, expected);
}

#[gpui_kit::test]
fn choosing_a_number_puts_it(cx: &mut TestAppContext) {
    let opened = open(cx, Some(None));
    let window = opened.window;
    choose(cx, &opened, Some(4));
    assert_eq!(
        calls(&opened),
        [IntentCall::SetDefaults(AgentDefaults {
            queue_limit: Some(4)
        })]
    );
    assert_eq!(
        label_of(cx, window, QUEUE_LIMIT_ID).as_deref(),
        Some("No limit")
    );
    assert!(disabled(cx, window), "disabled until the answer");

    // The daemon's answer reaches the model through the link.
    opened.fake.answer(Ok(common::intents::intent(
        "x",
        smithagents_core::intent::IntentState::Draft,
        smithagents_core::intent::Phase::Review,
    )));
    opened.state.update(cx, |state, cx| {
        state.dispatch(
            Action::AgentDefaultsLoaded(AgentDefaults {
                queue_limit: Some(4),
            }),
            cx,
        )
    });
    frame(cx, window);
    assert_eq!(label_of(cx, window, QUEUE_LIMIT_ID).as_deref(), Some("4"));
    assert!(!disabled(cx, window));
    assert_eq!(calls(&opened).len(), 1, "one request per press");
}

#[gpui_kit::test]
fn choosing_no_limit_puts_null(cx: &mut TestAppContext) {
    let opened = open(cx, Some(Some(5)));
    choose(cx, &opened, None);
    assert_eq!(
        calls(&opened),
        [IntentCall::SetDefaults(AgentDefaults { queue_limit: None })]
    );
}

#[gpui_kit::test]
fn a_failure_is_shown_under_the_control(cx: &mut TestAppContext) {
    let opened = open(cx, Some(None));
    let window = opened.window;
    opened.fake.queue(Err(FakeIntents::api_error(
        400,
        "invalid",
        "queue limit must be at most 64",
    )));
    choose(cx, &opened, Some(16));
    assert_eq!(
        label_of(cx, window, QUEUE_LIMIT_ERROR_ID).as_deref(),
        Some("Queue limit must be at most 64")
    );
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(QUEUE_LIMIT_ERROR_ID).role(), Some(Role::Status));
    });
    assert!(!disabled(cx, window));
    assert_eq!(
        label_of(cx, window, QUEUE_LIMIT_ID).as_deref(),
        Some("No limit")
    );
}

#[gpui_kit::test]
fn nothing_can_be_chosen_before_the_defaults_load(cx: &mut TestAppContext) {
    let opened = open(cx, None);
    let window = opened.window;
    assert!(disabled(cx, window));
    assert_eq!(
        label_of(cx, window, QUEUE_LIMIT_HELP_ID).as_deref(),
        Some(NOT_CONNECTED_TEXT)
    );
    click(cx, window, QUEUE_LIMIT_ID);
    with_window(cx, window, |window, _| {
        assert!(window.try_find("popup-menu").is_none());
    });
    assert!(calls(&opened).is_empty());
}

#[gpui_kit::test]
fn a_larger_stored_limit_is_shown_as_is(cx: &mut TestAppContext) {
    let opened = open(cx, Some(Some(40)));
    let window = opened.window;
    assert_eq!(label_of(cx, window, QUEUE_LIMIT_ID).as_deref(), Some("40"));
    click(cx, window, QUEUE_LIMIT_ID);
    let entries = menu_entries(cx, window);
    assert_eq!(entries.len(), 17);
    assert!(entries.iter().all(|(_, checked)| !checked));
}

#[gpui_kit::test]
fn the_section_collapses_and_is_remembered(cx: &mut TestAppContext) {
    let opened = open(cx, Some(Some(2)));
    let window = opened.window;
    click(
        cx,
        window,
        section_header_id(SettingsSection::AgentDefaults),
    );
    with_window(cx, window, |window, _| {
        assert!(window.try_find(QUEUE_LIMIT_ID).is_none());
        assert_eq!(
            window
                .find(section_header_id(SettingsSection::AgentDefaults))
                .expanded(),
            Some(false)
        );
    });
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(opened.home.path().join("app-state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        json["collapsed_settings_sections"],
        serde_json::json!(["agent_defaults"])
    );
}

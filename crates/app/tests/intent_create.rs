//! Headless checks of the Intents pane's create screen: the text box, the
//! five kind rows and their ⌥ keys, the repository line, and what a create
//! does on success and on failure. Commands go to `FakeIntents`, so no
//! daemon runs.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::path::PathBuf;
use std::rc::Rc;

use chrono::{DateTime, Utc};
use common::intents::{fixed_clock, intent, model_with, FakeIntents, IntentCall};
use common::ui::open_shell_options;
use gpui_kit::base::Root;
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::test::{TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{AppContext as _, ElementId, Entity, Keystroke, Role, TestAppContext, WindowHandle};
use smithagents_app::intent_vm::KIND_CHOICES;
use smithagents_app::shell::{Shell, ShellOptions};
use smithagents_app::shell_state::ShellState;
use smithagents_app::views::intents::{
    create_repo_item_id, kind_row_id, Busy, IntentsPane, Screen, CREATE_CLOSE_ID, CREATE_INPUT_ID,
    CREATE_REPO_ID, CREATE_SCREEN_ID, DETAIL_BACK_ID, INTENTS_NEW_ID, INTENT_ERROR_ID,
    KIND_DESCRIPTION_ID, KIND_SHORTCUT_ID, KIND_TITLE_ID,
};
use smithagents_app::views::rail::RailEntry;
use smithagents_app::views::settings::SETTINGS_TITLE_ID;
use smithagents_core::agent::Repo;
use smithagents_core::ids::RepoId;
use smithagents_core::intent::{CreateIntentRequest, IntentKind, IntentState, Phase, SeedInput};

/// The text box's accessible name.
const TEXT_LABEL: &str = "Describe what you want";

fn repo(path: &str, created: &str) -> Repo {
    let created_at: DateTime<Utc> = format!("2026-11-{created}T09:00:00Z").parse().unwrap();
    Repo {
        id: RepoId::new(),
        path: PathBuf::from(path),
        default_branch: "main".into(),
        created_at,
    }
}

/// The shell with the Intents pane on screen.
struct Opened {
    window: WindowHandle<Root>,
    state: Entity<ShellState>,
    pane: Entity<IntentsPane>,
    fake: Rc<FakeIntents>,
}

/// Opens the shell, connected, with these repositories and no intents, and
/// shows the Intents list.
fn open_list(cx: &mut TestAppContext, repos: Vec<Repo>) -> Opened {
    let fake = Rc::new(FakeIntents::default());
    let (window, shell) = open_shell_options(
        cx,
        ShellOptions {
            model: model_with(vec![], vec![], repos),
            intent_commands: Some(fake.clone()),
            clock: Some(fixed_clock("2026-11-26T10:15:00-08:00")),
            ..Default::default()
        },
        (1200., 800.),
    );
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    frame(cx, window);
    click(cx, window, RailEntry::Intents.element_id());
    let pane = intents_pane(cx, &shell);
    Opened {
        window,
        state,
        pane,
        fake,
    }
}

/// [`open_list`], then the New intent button.
fn open(cx: &mut TestAppContext, repos: Vec<Repo>) -> Opened {
    let opened = open_list(cx, repos);
    click(cx, opened.window, INTENTS_NEW_ID);
    assert_eq!(screen(cx, &opened), Screen::Create);
    opened
}

/// The Intents pane, found among the right dock's panels.
fn intents_pane(cx: &mut TestAppContext, shell: &Entity<Shell>) -> Entity<IntentsPane> {
    let dock = shell.read_with(cx, |shell, _| shell.dock().clone());
    dock.read_with(cx, |dock, _| {
        dock.layout(DockPlacement::Right)
            .expect("a right dock")
            .panels()
            .find_map(|id| dock.panel(id)?.view().downcast::<IntentsPane>().ok())
            .expect("the Intents pane is in the right dock")
    })
}

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

fn click(cx: &mut TestAppContext, window: WindowHandle<Root>, id: impl Into<ElementId>) {
    let id = id.into();
    with_window(cx, window, |window, cx| window.click(id, cx));
    frame(cx, window);
}

fn press(cx: &mut TestAppContext, window: WindowHandle<Root>, key: &str) {
    with_window(cx, window, |window, cx| window.press(key, cx));
    frame(cx, window);
}

/// Types `text` into the focused control.
fn type_text(cx: &mut TestAppContext, window: WindowHandle<Root>, text: &str) {
    with_window(cx, window, |window, cx| window.input(text, cx));
    frame(cx, window);
}

/// Presses `key` as macOS sends it: with the `character` the key types (⌥B
/// types "∫"). GPUI types that character only when no key binding takes the
/// key, as the macOS window does for a key outside an input method.
fn press_typing(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    key: &'static str,
    character: &str,
) {
    let keystroke = Keystroke {
        key_char: Some(character.to_owned()),
        ..Keystroke::parse(key).unwrap()
    };
    with_window(cx, window, |window, cx| {
        window.dispatch_keystroke(keystroke, cx);
    });
    frame(cx, window);
}

/// What the element `id` reads: a `Label`'s text (its value), else its name;
/// `None` when it is not drawn.
fn label_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    id: impl Into<ElementId>,
) -> Option<String> {
    let id = id.into();
    with_window(cx, window, |window, _| {
        window
            .try_find(id)
            .and_then(|element| element.value().or(element.label()).map(str::to_owned))
    })
}

/// The text in the box.
fn text(cx: &mut TestAppContext, window: WindowHandle<Root>) -> String {
    with_window(cx, window, |window, _| {
        window
            .within(CREATE_INPUT_ID)
            .find_by_label(TEXT_LABEL)
            .value()
            .unwrap_or_default()
            .to_owned()
    })
}

fn text_focused(cx: &mut TestAppContext, window: WindowHandle<Root>) -> bool {
    with_window(cx, window, |window, _| {
        window
            .within(CREATE_INPUT_ID)
            .find_by_label(TEXT_LABEL)
            .focused()
            == Some(true)
    })
}

/// Whether each kind row is disabled, in the table's order. (A button says
/// so only while it is.)
fn rows_disabled(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Vec<bool> {
    with_window(cx, window, |window, _| {
        KIND_CHOICES
            .iter()
            .map(|choice| window.find(kind_row_id(choice.kind)).disabled() == Some(true))
            .collect()
    })
}

fn screen(cx: &mut TestAppContext, opened: &Opened) -> Screen {
    opened.pane.read_with(cx, |pane, _| *pane.screen())
}

fn creates(opened: &Opened) -> Vec<CreateIntentRequest> {
    opened
        .fake
        .calls
        .borrow()
        .iter()
        .filter_map(|call| match call {
            IntentCall::Create(request) => Some(request.clone()),
            _ => None,
        })
        .collect()
}

fn create_request(repo: &Repo, kind: IntentKind, text: &str) -> CreateIntentRequest {
    CreateIntentRequest {
        repo_id: Some(repo.id),
        kind: Some(kind),
        seed: SeedInput::Text(text.to_owned()),
    }
}

fn error_line(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Option<String> {
    with_window(cx, window, |window, _| {
        window.try_find(INTENT_ERROR_ID).map(|line| {
            assert_eq!(line.role(), Some(Role::Status));
            line.label().unwrap().to_owned()
        })
    })
}

// ---- The ⌥ keys --------------------------------------------------------------

#[gpui_kit::test]
fn option_b_creates_a_bug_and_types_nothing(cx: &mut TestAppContext) {
    let only = repo("/work/alpha", "20");
    let opened = open(cx, vec![only.clone()]);
    let window = opened.window;
    assert!(text_focused(cx, window), "the box has the focus");

    type_text(cx, window, "Fix the zoom");
    assert_eq!(text(cx, window), "Fix the zoom");
    press(cx, window, "alt-b");
    assert_eq!(
        creates(&opened),
        [create_request(&only, IntentKind::Bug, "Fix the zoom")]
    );
    assert_eq!(text(cx, window), "Fix the zoom", "no character was typed");
}

#[gpui_kit::test]
fn an_option_key_types_no_character_on_the_create_screen(cx: &mut TestAppContext) {
    // macOS gives ⌥B the character "∫" and ⌥F "ƒ": the key binding takes the
    // key first, so neither reaches the box, blank or not. (`press` gives a
    // ⌥ key no character at all, so this sends the one macOS would.)
    let only = repo("/work/alpha", "20");
    let opened = open(cx, vec![only.clone()]);
    let window = opened.window;

    press_typing(cx, window, "alt-b", "∫");
    assert_eq!(text(cx, window), "", "blank: nothing is typed");
    assert!(creates(&opened).is_empty(), "blank: nothing is created");

    type_text(cx, window, "Add a zoom slider");
    opened.fake.queue(Err(FakeIntents::api_error(
        400,
        "invalid",
        "something went wrong",
    )));
    press_typing(cx, window, "alt-f", "ƒ");
    assert_eq!(text(cx, window), "Add a zoom slider");
    assert_eq!(
        creates(&opened),
        [create_request(
            &only,
            IntentKind::NewFeature,
            "Add a zoom slider"
        )]
    );
}

#[gpui_kit::test]
fn each_option_key_creates_its_kind(cx: &mut TestAppContext) {
    let only = repo("/work/alpha", "20");
    let opened = open(cx, vec![only.clone()]);
    let window = opened.window;
    type_text(cx, window, "Tidy up");
    for choice in KIND_CHOICES {
        opened
            .fake
            .queue(Err(FakeIntents::api_error(400, "invalid", "no")));
        press(cx, window, choice.key);
    }
    let kinds: Vec<_> = creates(&opened)
        .into_iter()
        .map(|request| request.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            Some(IntentKind::NewFeature),
            Some(IntentKind::Enhancement),
            Some(IntentKind::Bug),
            Some(IntentKind::Refactor),
            Some(IntentKind::Maintenance),
        ]
    );
}

#[gpui_kit::test]
fn shortcuts_work_only_on_the_create_screen(cx: &mut TestAppContext) {
    let opened = open(cx, vec![repo("/work/alpha", "20")]);
    let window = opened.window;
    type_text(cx, window, "Fix the zoom");
    press(cx, window, "escape");
    assert_eq!(screen(cx, &opened), Screen::List);

    press(cx, window, "alt-b");
    assert!(creates(&opened).is_empty(), "the list has no ⌥ keys");
    assert_eq!(screen(cx, &opened), Screen::List);
}

// ---- The screen --------------------------------------------------------------

#[gpui_kit::test]
fn the_screen_shows_five_kinds_with_descriptions_and_shortcuts(cx: &mut TestAppContext) {
    let opened = open(cx, vec![repo("/work/alpha", "20")]);
    let window = opened.window;

    with_window(cx, window, |window, _| {
        let rows: Vec<_> = KIND_CHOICES
            .iter()
            .map(|choice| window.find(kind_row_id(choice.kind)))
            .collect();
        let names: Vec<_> = rows.iter().map(|row| row.label().unwrap()).collect();
        assert_eq!(
            names,
            [
                "New feature, a new capability",
                "Enhancement, extend an existing capability",
                "Bug, a capability that misbehaves",
                "Refactor, restructure, same behavior",
                "Maintenance, upgrades, config, housekeeping",
            ]
        );
        assert!(rows.iter().all(|row| row.role() == Some(Role::Button)));
        let tops: Vec<_> = rows.iter().map(|row| row.bounds().origin.y).collect();
        assert!(
            tops.windows(2).all(|pair| pair[0] < pair[1]),
            "the rows are in the table's order, top to bottom"
        );
        assert!(
            window.try_find(kind_row_id(IntentKind::Task)).is_none(),
            "Task is not offered"
        );

        for choice in KIND_CHOICES {
            let row = window.within(kind_row_id(choice.kind));
            let read = |id: &'static str| row.find(id).value().map(str::to_owned);
            assert_eq!(read(KIND_TITLE_ID).as_deref(), Some(choice.title));
            assert_eq!(
                read(KIND_DESCRIPTION_ID).as_deref(),
                Some(choice.description)
            );
            assert_eq!(read(KIND_SHORTCUT_ID).as_deref(), Some(choice.shortcut));
        }

        // The close button and the five rows are the screen's only buttons:
        // the old app's paperclip is not drawn.
        let buttons: Vec<_> = window
            .within(CREATE_SCREEN_ID)
            .find_all_by_role(Role::Button)
            .into_iter()
            .map(|button| button.label().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(buttons.len(), 6, "{buttons:?}");
        assert!(buttons.contains(&"Close".to_owned()));
        assert!(window.try_find_by_label("Attach").is_none());
        assert!(window.try_find_by_label("Attach files").is_none());

        let close = window.find(CREATE_CLOSE_ID);
        assert_eq!(close.label(), Some("Close"));
    });
    assert_eq!(
        label_of(cx, window, CREATE_REPO_ID).as_deref(),
        Some("In alpha")
    );
}

#[gpui_kit::test]
fn rows_are_disabled_until_there_is_text(cx: &mut TestAppContext) {
    let opened = open(cx, vec![repo("/work/alpha", "20")]);
    let window = opened.window;
    assert_eq!(rows_disabled(cx, window), [true; 5], "empty");

    type_text(cx, window, "   ");
    press(cx, window, "enter");
    assert_eq!(text(cx, window), "   \n", "Enter adds a line");
    assert_eq!(rows_disabled(cx, window), [true; 5], "only blanks");
    click(cx, window, kind_row_id(IntentKind::Bug));
    press(cx, window, "alt-b");
    assert!(
        creates(&opened).is_empty(),
        "a disabled row creates nothing"
    );

    type_text(cx, window, "x");
    assert_eq!(rows_disabled(cx, window), [false; 5]);
}

#[gpui_kit::test]
fn choosing_a_kind_creates_and_opens_the_detail(cx: &mut TestAppContext) {
    let only = repo("/work/alpha", "20");
    let opened = open(cx, vec![only.clone()]);
    let window = opened.window;
    type_text(cx, window, "Tidy the store");
    click(cx, window, kind_row_id(IntentKind::Refactor));
    assert_eq!(
        creates(&opened),
        [create_request(
            &only,
            IntentKind::Refactor,
            "Tidy the store"
        )]
    );

    let created = intent(
        "tidy-the-store",
        IntentState::Draft,
        Phase::Polishing { pass: 1 },
    );
    opened.fake.answer(Ok(created.clone()));
    frame(cx, window);
    assert_eq!(screen(cx, &opened), Screen::Detail(created.id));
    assert_eq!(
        opened.state.read_with(cx, |state, _| state.last_repo_id()),
        Some(only.id)
    );

    // Back on the create screen the box is empty.
    click(cx, window, DETAIL_BACK_ID);
    click(cx, window, INTENTS_NEW_ID);
    assert_eq!(text(cx, window), "");
}

#[gpui_kit::test]
fn a_create_answered_after_the_screen_was_left_stays_on_the_list(cx: &mut TestAppContext) {
    let only = repo("/work/alpha", "20");
    let opened = open(cx, vec![only.clone()]);
    let window = opened.window;
    type_text(cx, window, "Tidy the store");
    click(cx, window, kind_row_id(IntentKind::Refactor));
    press(cx, window, "escape");
    assert_eq!(screen(cx, &opened), Screen::List);

    let created = intent(
        "tidy-the-store",
        IntentState::Draft,
        Phase::Polishing { pass: 1 },
    );
    opened.fake.answer(Ok(created));
    frame(cx, window);
    assert_eq!(screen(cx, &opened), Screen::List, "no jump to the detail");
    assert_eq!(
        opened.state.read_with(cx, |state, _| state.last_repo_id()),
        Some(only.id)
    );
    // The text was sent as it is: the box is empty for the next one.
    click(cx, window, INTENTS_NEW_ID);
    assert_eq!(text(cx, window), "");
}

#[gpui_kit::test]
fn text_typed_while_a_create_is_on_its_way_survives_the_answer(cx: &mut TestAppContext) {
    let opened = open(cx, vec![repo("/work/alpha", "20")]);
    let window = opened.window;
    type_text(cx, window, "Fix the zoom");
    click(cx, window, kind_row_id(IntentKind::Bug));
    press(cx, window, "escape");
    click(cx, window, INTENTS_NEW_ID);
    type_text(cx, window, " and the pan");
    assert_eq!(text(cx, window), "Fix the zoom and the pan");

    let created = intent(
        "fix-the-zoom",
        IntentState::Draft,
        Phase::Polishing { pass: 1 },
    );
    opened.fake.answer(Ok(created.clone()));
    frame(cx, window);
    // The create screen is on screen again, so the detail opens as usual.
    assert_eq!(screen(cx, &opened), Screen::Detail(created.id));
    click(cx, window, DETAIL_BACK_ID);
    click(cx, window, INTENTS_NEW_ID);
    assert_eq!(text(cx, window), "Fix the zoom and the pan");
}

#[gpui_kit::test]
fn the_row_shows_the_spinner_while_creating(cx: &mut TestAppContext) {
    let opened = open(cx, vec![repo("/work/alpha", "20")]);
    let window = opened.window;
    type_text(cx, window, "Fix the zoom");
    click(cx, window, kind_row_id(IntentKind::Bug));
    assert_eq!(
        opened.pane.read_with(cx, |pane, _| pane.busy()),
        Some(Busy::Create)
    );
    assert_eq!(rows_disabled(cx, window), [true; 5], "while creating");

    click(cx, window, kind_row_id(IntentKind::Bug));
    press(cx, window, "alt-e");
    assert_eq!(creates(&opened).len(), 1, "one create at a time");

    let created = intent(
        "fix-the-zoom",
        IntentState::Draft,
        Phase::Polishing { pass: 1 },
    );
    opened.fake.answer(Ok(created.clone()));
    frame(cx, window);
    assert_eq!(opened.pane.read_with(cx, |pane, _| pane.busy()), None);
    assert_eq!(screen(cx, &opened), Screen::Detail(created.id));
}

#[gpui_kit::test]
fn a_failed_create_keeps_the_text(cx: &mut TestAppContext) {
    let opened = open(cx, vec![repo("/work/alpha", "20")]);
    let window = opened.window;
    type_text(cx, window, "A very long story");
    opened.fake.queue(Err(FakeIntents::api_error(
        400,
        "invalid",
        "seed text must be at most 204800 bytes",
    )));
    click(cx, window, kind_row_id(IntentKind::Enhancement));

    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("Seed text must be at most 204800 bytes")
    );
    assert_eq!(text(cx, window), "A very long story");
    assert_eq!(screen(cx, &opened), Screen::Create);
    assert_eq!(rows_disabled(cx, window), [false; 5], "free to try again");
    with_window(cx, window, |window, _| {
        let line = window.find(INTENT_ERROR_ID).bounds();
        let last = window.find(kind_row_id(IntentKind::Maintenance)).bounds();
        assert!(line.origin.y > last.origin.y, "the line is under the rows");
    });
}

#[gpui_kit::test]
fn escape_and_close_return_to_the_list_and_keep_the_text(cx: &mut TestAppContext) {
    let opened = open(cx, vec![repo("/work/alpha", "20")]);
    let window = opened.window;
    type_text(cx, window, "Keep me");

    press(cx, window, "escape");
    assert_eq!(screen(cx, &opened), Screen::List);
    click(cx, window, INTENTS_NEW_ID);
    assert_eq!(screen(cx, &opened), Screen::Create);
    assert_eq!(text(cx, window), "Keep me");
    assert!(text_focused(cx, window), "the box has the focus again");

    click(cx, window, CREATE_CLOSE_ID);
    assert_eq!(screen(cx, &opened), Screen::List);
    click(cx, window, INTENTS_NEW_ID);
    assert_eq!(text(cx, window), "Keep me");
    assert!(creates(&opened).is_empty());
}

#[gpui_kit::test]
fn showing_the_pane_again_focuses_the_box(cx: &mut TestAppContext) {
    let opened = open(cx, vec![repo("/work/alpha", "20")]);
    let window = opened.window;
    // Focus listeners hear only an active window's focus.
    with_window(cx, window, |window, _| window.activate_window());
    frame(cx, window);
    type_text(cx, window, "Fix");
    click(cx, window, RailEntry::Settings.element_id());
    click(cx, window, RailEntry::Intents.element_id());
    assert_eq!(screen(cx, &opened), Screen::Create);
    assert!(text_focused(cx, window), "the shell focused the pane");
    type_text(cx, window, " the zoom");
    press(cx, window, "alt-b");
    assert_eq!(creates(&opened).len(), 1);
    assert_eq!(text(cx, window), "Fix the zoom");
}

#[gpui_kit::test]
fn tab_leaves_the_box_for_the_other_controls(cx: &mut TestAppContext) {
    // The text box would indent on Tab, so keyboard users could not reach
    // the repository menu and the rows from it.
    let opened = open(
        cx,
        vec![repo("/work/alpha", "20"), repo("/work/beta", "21")],
    );
    let window = opened.window;
    type_text(cx, window, "Fix the zoom");
    let focused = |cx: &mut TestAppContext, id: ElementId| {
        with_window(cx, window, |window, _| {
            window.find(id).focused() == Some(true)
        })
    };

    press(cx, window, "tab");
    assert!(
        focused(cx, CREATE_REPO_ID.into()),
        "Tab goes to the repository"
    );
    press(cx, window, "tab");
    assert!(focused(cx, kind_row_id(IntentKind::NewFeature)));
    press(cx, window, "shift-tab");
    press(cx, window, "shift-tab");
    assert!(text_focused(cx, window), "Shift-Tab comes back to the box");
    press(cx, window, "shift-tab");
    assert!(focused(cx, CREATE_CLOSE_ID.into()));
    assert_eq!(text(cx, window), "Fix the zoom", "Tab typed nothing");

    // Enter on a focused row chooses it.
    press(cx, window, "tab");
    press(cx, window, "tab");
    press(cx, window, "tab");
    press(cx, window, "tab");
    assert!(focused(cx, kind_row_id(IntentKind::Enhancement)));
    press(cx, window, "enter");
    assert_eq!(
        creates(&opened)
            .into_iter()
            .map(|request| request.kind)
            .collect::<Vec<_>>(),
        [Some(IntentKind::Enhancement)]
    );
}

// ---- The repository ----------------------------------------------------------

#[gpui_kit::test]
fn the_repository_is_named_and_can_be_changed(cx: &mut TestAppContext) {
    let alpha = repo("/work/alpha", "20");
    let beta = repo("/work/beta", "21");
    let opened = open_list(cx, vec![alpha.clone(), beta.clone()]);
    let window = opened.window;
    opened
        .state
        .update(cx, |state, _| state.set_last_repo_id(beta.id));
    click(cx, window, INTENTS_NEW_ID);

    assert_eq!(
        label_of(cx, window, CREATE_REPO_ID).as_deref(),
        Some("In beta")
    );
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(CREATE_REPO_ID).role(), Some(Role::Button));
    });

    click(cx, window, CREATE_REPO_ID);
    let checked = |cx: &mut TestAppContext, repo: &Repo| {
        with_window(cx, window, |window, _| {
            let menu = window.within("popup-menu");
            let entry = menu.find(create_repo_item_id(repo.id));
            assert_eq!(
                entry.label().map(str::to_owned),
                Some(format!(
                    "{}, {}",
                    repo.path.file_name().unwrap().to_string_lossy(),
                    repo.path.display()
                ))
            );
            menu.find_all_by_role(Role::MenuItem)
                .into_iter()
                .find(|item| entry.path().starts_with(item.path()))
                .expect("the entry is inside a menu item")
                .checked()
                == Some(true)
        })
    };
    assert!(checked(cx, &beta), "the chosen repository is checked");
    assert!(!checked(cx, &alpha));
    with_window(cx, window, |window, cx| {
        window
            .within("popup-menu")
            .click(create_repo_item_id(alpha.id), cx)
    });
    frame(cx, window);
    assert_eq!(
        label_of(cx, window, CREATE_REPO_ID).as_deref(),
        Some("In alpha")
    );

    // The menu took the focus; the box gets it back with a click.
    click(cx, window, CREATE_INPUT_ID);
    type_text(cx, window, "Fix the zoom");
    click(cx, window, kind_row_id(IntentKind::Bug));
    assert_eq!(
        creates(&opened),
        [create_request(&alpha, IntentKind::Bug, "Fix the zoom")]
    );
}

#[gpui_kit::test]
fn no_repository_points_to_settings(cx: &mut TestAppContext) {
    let opened = open(cx, vec![]);
    let window = opened.window;
    assert_eq!(
        label_of(cx, window, CREATE_REPO_ID).as_deref(),
        Some("Add a source repository first")
    );
    type_text(cx, window, "Fix the zoom");
    assert_eq!(rows_disabled(cx, window), [true; 5], "no repository");
    press(cx, window, "alt-b");
    assert!(creates(&opened).is_empty());

    click(cx, window, CREATE_REPO_ID);
    assert_eq!(
        opened.state.read_with(cx, |state, _| state.right_entry()),
        RailEntry::Settings
    );
    with_window(cx, window, |window, _| {
        assert!(
            window.try_find(SETTINGS_TITLE_ID).is_some(),
            "Settings is on screen"
        );
    });
}

//! Headless checks of the Intents pane's detail screen: the header (state,
//! slug, kind, title) and its two edits, the Markdown body, the polishing and
//! polish-failed states, and what happens to a command still in flight when
//! the user leaves its screen. Commands go to `FakeIntents`, so no daemon
//! runs; the model changes only when the test dispatches the daemon's copy.
//!
//! The body is a `TextView`, whose text a test cannot read: the view draws
//! it, but no observed element carries it as a label (checked: neither
//! "What happens" nor "## What happens" is found by label). So the tests
//! check that the body takes room and read the Markdown the pane hands the
//! view ([`IntentsPane::body_source`]).
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::rc::Rc;
use std::time::Duration;

use common::intents::{fixed_clock, intent, model_with, FakeIntents, IntentCall};
use common::ui::open_shell_options;
use gpui_kit::base::Root;
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::test::{ElementSnapshot, TestWindowExt as _};
use gpui_kit::{point, px, AppContext as _, ElementId, Entity, Role, TestAppContext, WindowHandle};
use smithagents_app::intent_vm::{SectionKind, KIND_CHOICES};
use smithagents_app::model::Action;
use smithagents_app::shell::{Shell, ShellOptions};
use smithagents_app::shell_state::ShellState;
use smithagents_app::view_models::DAEMON_SILENT_TEXT;
use smithagents_app::views::intents::{
    intent_card_id, kind_item_id, section_header_id, Busy, IntentsPane, Screen, DETAIL_BACK_ID,
    DETAIL_BODY_ID, DETAIL_KIND_ID, DETAIL_SCROLL_ID, DETAIL_SLUG_ID, DETAIL_STATE_ID,
    DETAIL_TITLE_ID, DETAIL_TITLE_INPUT_ID, INTENT_ERROR_ID, INTENT_LIST_ID, POLISHING_ID,
    POLISH_FAILED_ID, RETRY_ID, SEND_ID,
};
use smithagents_app::views::rail::RailEntry;
use smithagents_core::intent::{Intent, IntentKind, IntentState, Patch, Phase};

/// The shell with the Intents pane on screen.
struct Opened {
    window: WindowHandle<Root>,
    shell: Entity<Shell>,
    state: Entity<ShellState>,
    pane: Entity<IntentsPane>,
    fake: Rc<FakeIntents>,
}

/// Opens the shell, connected, with these intents and the given command
/// timeout (the shell's default without one), and shows the Intents list.
fn open_list(
    cx: &mut TestAppContext,
    intents: Vec<Intent>,
    command_timeout: Option<Duration>,
) -> Opened {
    let fake = Rc::new(FakeIntents::default());
    let (window, shell) = open_shell_options(
        cx,
        ShellOptions {
            model: model_with(intents, vec![], vec![]),
            intent_commands: Some(fake.clone()),
            clock: Some(fixed_clock("2026-11-26T10:15:00-08:00")),
            command_timeout,
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
        shell,
        state,
        pane,
        fake,
    }
}

/// Opens the shell with `intent` alone and shows its detail.
fn open_detail(cx: &mut TestAppContext, intent: &Intent) -> Opened {
    open_detail_with(cx, intent, None)
}

fn open_detail_with(
    cx: &mut TestAppContext,
    intent: &Intent,
    command_timeout: Option<Duration>,
) -> Opened {
    let opened = open_list(cx, vec![intent.clone()], command_timeout);
    click(cx, opened.window, intent_card_id(intent.id));
    assert_eq!(screen(cx, &opened), Screen::Detail(intent.id));
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

/// What an element reads to assistive technology: a `Label`'s text (its
/// value), else its name.
fn text(element: &ElementSnapshot) -> Option<String> {
    element.value().or(element.label()).map(str::to_owned)
}

/// What the element `id` reads, or `None` when it is not drawn.
fn label_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    id: impl Into<ElementId>,
) -> Option<String> {
    let id = id.into();
    with_window(cx, window, |window, _| {
        window.try_find(id).as_ref().and_then(text)
    })
}

fn role_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    id: impl Into<ElementId>,
) -> Option<Role> {
    let id = id.into();
    with_window(cx, window, |window, _| window.find(id).role())
}

fn is_drawn(cx: &mut TestAppContext, window: WindowHandle<Root>, id: impl Into<ElementId>) -> bool {
    let id = id.into();
    with_window(cx, window, |window, _| window.try_find(id).is_some())
}

fn screen(cx: &mut TestAppContext, opened: &Opened) -> Screen {
    opened.pane.read_with(cx, |pane, _| *pane.screen())
}

fn busy(cx: &mut TestAppContext, opened: &Opened) -> Option<Busy> {
    opened.pane.read_with(cx, |pane, _| pane.busy())
}

fn body_source(cx: &mut TestAppContext, opened: &Opened) -> Option<String> {
    opened.pane.read_with(cx, |pane, cx| pane.body_source(cx))
}

/// The daemon's copy of a change reaches the model.
fn upsert(cx: &mut TestAppContext, opened: &Opened, intent: &Intent) {
    let action = Action::IntentUpserted(intent.clone());
    opened
        .state
        .update(cx, |state, cx| state.dispatch(action, cx));
    frame(cx, opened.window);
}

/// `intent` after a change made a minute later.
fn changed(intent: &Intent, change: impl FnOnce(&mut Intent)) -> Intent {
    let mut next = intent.clone();
    change(&mut next);
    next.updated_at = intent.updated_at + chrono::Duration::minutes(1);
    next
}

fn calls(opened: &Opened) -> Vec<IntentCall> {
    opened.fake.calls.borrow().clone()
}

fn error_line(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Option<String> {
    with_window(cx, window, |window, _| {
        window.try_find(INTENT_ERROR_ID).map(|line| {
            assert_eq!(line.role(), Some(Role::Status));
            line.label().unwrap().to_owned()
        })
    })
}

/// Whether `id` is drawn as a button that is not disabled.
fn enabled_button(cx: &mut TestAppContext, window: WindowHandle<Root>, id: &'static str) -> bool {
    with_window(cx, window, |window, _| {
        window
            .try_find(id)
            .is_some_and(|el| el.role() == Some(Role::Button) && el.disabled() != Some(true))
    })
}

/// The kind menu's entries, top to bottom: what each reads and whether it is
/// checked.
fn kind_menu(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Vec<(String, bool)> {
    with_window(cx, window, |window, _| {
        let menu = window.within("popup-menu");
        let items = menu.find_all_by_role(Role::MenuItem);
        KIND_CHOICES
            .iter()
            .map(|choice| {
                let entry = menu.find(kind_item_id(choice.kind));
                // The entry's own node sits inside the menu's item, which
                // carries the check state.
                let checked = items
                    .iter()
                    .find(|item| entry.path().starts_with(item.path()))
                    .expect("the entry is inside a menu item")
                    .checked()
                    == Some(true);
                (text(&entry).unwrap(), checked)
            })
            .collect()
    })
}

fn choose_kind(cx: &mut TestAppContext, window: WindowHandle<Root>, kind: IntentKind) {
    click(cx, window, DETAIL_KIND_ID);
    with_window(cx, window, |window, cx| {
        window.within("popup-menu").click(kind_item_id(kind), cx)
    });
    frame(cx, window);
}

/// The title as the header draws it: its text when fixed; when it can be
/// edited, the button's name ("Edit title: <title>").
fn title(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Option<String> {
    label_of(cx, window, DETAIL_TITLE_ID)
}

fn title_input(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Option<(String, bool)> {
    with_window(cx, window, |window, _| {
        window.try_find(DETAIL_TITLE_INPUT_ID).map(|input| {
            (
                input.value().unwrap_or_default().to_owned(),
                input.focused() == Some(true),
            )
        })
    })
}

/// Clicks the title, selects what the input holds and types `text`.
fn retitle(cx: &mut TestAppContext, window: WindowHandle<Root>, text: &str) {
    click(cx, window, DETAIL_TITLE_ID);
    press(cx, window, "cmd-a");
    type_text(cx, window, text);
}

fn kind_patch(kind: IntentKind) -> Patch {
    Patch {
        kind: Some(kind),
        ..Default::default()
    }
}

fn title_patch(title: &str) -> Patch {
    Patch {
        title: Some(title.to_owned()),
        ..Default::default()
    }
}

// ---- Body --------------------------------------------------------------------

#[gpui_kit::test]
fn the_body_is_drawn_from_the_markdown(cx: &mut TestAppContext) {
    let markdown = "## What happens\n\nThe page is **wrong**.\n";
    let mut draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    draft.content.current_behavior = markdown.into();
    let opened = open_detail(cx, &draft);
    let window = opened.window;
    cx.run_until_parked();

    assert_eq!(body_source(cx, &opened).as_deref(), Some(markdown));
    with_window(cx, window, |window, _| {
        let body = window.find(DETAIL_BODY_ID).bounds();
        assert!(body.size.height > px(0.), "the body takes room: {body:?}");
        let scroll = window.find(DETAIL_SCROLL_ID).bounds();
        assert!(
            body.origin.y >= scroll.origin.y,
            "the body is in the scrolling column"
        );
    });
}

#[gpui_kit::test]
fn an_empty_body_shows_the_seed(cx: &mut TestAppContext) {
    // Review Focus 4: a blank body shows what the user wrote.
    let draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    let opened = open_detail(cx, &draft);
    assert_eq!(
        body_source(cx, &opened).as_deref(),
        Some("Seed of fix-zoom")
    );
    with_window(cx, opened.window, |window, _| {
        assert!(window.find(DETAIL_BODY_ID).bounds().size.height > px(0.));
    });

    let blank = changed(&draft, |i| i.content.current_behavior = " \r\n\t\n".into());
    upsert(cx, &opened, &blank);
    assert_eq!(
        body_source(cx, &opened).as_deref(),
        Some("Seed of fix-zoom")
    );
}

#[gpui_kit::test]
fn bodies_nobody_designed_for_draw(cx: &mut TestAppContext) {
    // Review Focus 4: each is drawn without a panic and takes room.
    let draft = intent("odd-bodies", IntentState::Draft, Phase::Review);
    let opened = open_detail(cx, &draft);
    let bodies = [
        "Just a sentence with no heading at all.",
        "```\nfn main() {}\n```",
        "```rust\nlet unclosed = true;\n",
        "## Windows lines\r\n\r\nOne line.\r\nTwo lines.\r\n",
        "## Überblick 日本語\n\nDie Seite ist falsch — 🙂 ½ café.\n",
        "#\n\n- \n\n> \n\n|a|b|\n|-|-|\n|1|",
    ];
    let mut current = draft;
    for body in bodies {
        current = changed(&current, |i| i.content.current_behavior = body.into());
        upsert(cx, &opened, &current);
        frame(cx, opened.window);
        assert_eq!(body_source(cx, &opened).as_deref(), Some(body));
        with_window(cx, opened.window, |window, _| {
            let height = window.find(DETAIL_BODY_ID).bounds().size.height;
            assert!(height > px(0.), "{body:?} takes room");
        });
    }
}

#[gpui_kit::test]
fn the_body_fetches_no_remote_images(cx: &mut TestAppContext) {
    // The body is model output: neither Markdown's image syntax nor an HTML
    // `<img>` in it may make the app fetch anything.
    use gpui_kit::http_client::{FakeHttpClient, Response};
    use std::sync::{Arc, Mutex};
    let requested = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = requested.clone();
    let client = FakeHttpClient::create(move |request| {
        seen.lock().unwrap().push(request.uri().to_string());
        async move { Ok(Response::builder().status(404).body(Default::default())?) }
    });
    cx.update(|cx| cx.set_http_client(client));

    let markdown = "## Look\n\n![chart](https://example.com/chart.png)\n\n\
                    Inline ![dot](http://example.com/dot.png) image.\n\n\
                    <img src=\"https://example.com/pixel.gif\" alt=\"pixel\">\n";
    let mut draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    draft.content.current_behavior = markdown.into();
    let opened = open_detail(cx, &draft);
    for _ in 0..3 {
        frame(cx, opened.window);
    }
    assert_eq!(body_source(cx, &opened).as_deref(), Some(markdown));
    assert_eq!(*requested.lock().unwrap(), Vec::<String>::new());
}

#[gpui_kit::test]
fn body_links_open_only_over_http(cx: &mut TestAppContext) {
    // The body is model output: a click on a link to a file or to another
    // app (Markdown, or an HTML block's `<a>`) opens nothing. Each body is
    // just its link, so a click near the body's top-left corner lands on it;
    // the last, which opens, shows that the clicks do. (Before the fix the
    // first two opened.)
    let mut draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    draft.content.current_behavior =
        "<p><a href=\"smithagents://x\">Open the app page</a></p>".into();
    let opened = open_detail(cx, &draft);
    let window = opened.window;
    let click_link = |cx: &mut TestAppContext| {
        with_window(cx, window, |window, cx| {
            window.click_at(DETAIL_BODY_ID, point(px(4.), px(8.)), cx)
        });
        frame(cx, window);
    };

    click_link(cx);
    assert_eq!(
        cx.opened_url(),
        None,
        "an HTML link to an app opens nothing"
    );

    let file = changed(&draft, |i| {
        i.content.current_behavior = "[Open the file](file:///etc/passwd)".into()
    });
    upsert(cx, &opened, &file);
    click_link(cx);
    assert_eq!(cx.opened_url(), None, "a Markdown file link opens nothing");

    let web = changed(&file, |i| {
        i.content.current_behavior = "[Open the docs](https://example.com/x)".into()
    });
    upsert(cx, &opened, &web);
    click_link(cx);
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/x"));
}

#[gpui_kit::test]
fn a_very_long_body_scrolls(cx: &mut TestAppContext) {
    // Review Focus 4: one 10,000-character line wraps inside the pane, and
    // the column scrolls instead of growing the pane.
    let line: String = "word".repeat(2_500);
    let mut draft = intent("long-line", IntentState::Draft, Phase::Review);
    draft.content.current_behavior = line.clone();
    let opened = open_detail(cx, &draft);
    let window = opened.window;
    cx.run_until_parked();
    frame(cx, window);
    assert_eq!(body_source(cx, &opened).map(|s| s.len()), Some(10_000));

    let dock = opened.shell.read_with(cx, |shell, _| shell.dock().clone());
    let dock_width = dock
        .read_with(cx, |dock, _| dock.dock_size(DockPlacement::Right))
        .expect("the right dock is open");
    with_window(cx, window, |window, _| {
        let pane = window.find(smithagents_app::shell::INTENTS_PANEL).bounds();
        assert!(
            pane.size.width <= dock_width,
            "the pane ({:?}) is no wider than the dock ({dock_width:?})",
            pane.size.width
        );
        let body = window.find(DETAIL_BODY_ID).bounds();
        assert!(
            body.right() <= pane.right(),
            "the body ({body:?}) stays inside the pane ({pane:?})"
        );
        let scroll = window.find(DETAIL_SCROLL_ID).bounds();
        assert!(
            scroll.bottom() <= pane.bottom(),
            "the scrolling column ends inside the pane"
        );
        assert!(
            body.size.height > scroll.size.height,
            "the line wraps into more than the column shows, so it scrolls"
        );
    });
}

// ---- Header ------------------------------------------------------------------

#[gpui_kit::test]
fn the_header_shows_state_slug_kind_and_title(cx: &mut TestAppContext) {
    let mut sent = intent("browser-pane-zoom-bounds", IntentState::Sent, Phase::Review);
    sent.content.kind = IntentKind::Bug;
    sent.content.title = "Fix Browser page drawn too big".into();
    let opened = open_detail(cx, &sent);
    let window = opened.window;

    assert_eq!(
        label_of(cx, window, DETAIL_STATE_ID).as_deref(),
        Some("Sent")
    );
    assert_eq!(
        label_of(cx, window, DETAIL_SLUG_ID).as_deref(),
        Some("browser-pane-zoom-bounds")
    );
    assert_eq!(label_of(cx, window, DETAIL_KIND_ID).as_deref(), Some("Bug"));
    assert_eq!(
        title(cx, window).as_deref(),
        Some("Fix Browser page drawn too big")
    );
    assert_ne!(role_of(cx, window, DETAIL_KIND_ID), Some(Role::Button));
    assert_ne!(role_of(cx, window, DETAIL_TITLE_ID), Some(Role::Button));

    // The fixed title and kind do nothing when clicked.
    click(cx, window, DETAIL_TITLE_ID);
    assert!(title_input(cx, window).is_none());
    click(cx, window, DETAIL_KIND_ID);
    assert!(!is_drawn(cx, window, "popup-menu"));

    with_window(cx, window, |window, _| {
        let back = window.find(DETAIL_BACK_ID);
        let state = window.find(DETAIL_STATE_ID);
        let slug = window.find(DETAIL_SLUG_ID);
        let title = window.find(DETAIL_TITLE_ID);
        let body = window.find(DETAIL_BODY_ID);
        assert!(
            back.bounds().origin.x < state.bounds().origin.x,
            "back, then state"
        );
        assert!(back.bounds().bottom() <= slug.bounds().origin.y + px(1.));
        assert!(slug.bounds().bottom() <= title.bounds().origin.y + px(1.));
        assert!(title.bounds().bottom() <= body.bounds().origin.y + px(1.));
    });
}

#[gpui_kit::test]
fn an_old_task_kind_reads_task(cx: &mut TestAppContext) {
    let mut sent = intent("old-one", IntentState::Sent, Phase::Review);
    sent.content.kind = IntentKind::Task;
    let opened = open_detail(cx, &sent);
    assert_eq!(
        label_of(cx, opened.window, DETAIL_KIND_ID).as_deref(),
        Some("Task")
    );

    // In review its menu offers the five kinds and checks none.
    let review = changed(&sent, |i| {
        i.state = IntentState::Draft;
        i.phase = Phase::Review;
    });
    upsert(cx, &opened, &review);
    let window = opened.window;
    assert_eq!(
        label_of(cx, window, DETAIL_KIND_ID).as_deref(),
        Some("Task")
    );
    click(cx, window, DETAIL_KIND_ID);
    let entries = kind_menu(cx, window);
    assert_eq!(entries.len(), 5);
    assert!(entries.iter().all(|(_, checked)| !checked), "{entries:?}");
}

#[gpui_kit::test]
fn the_kind_menu_patches_the_kind(cx: &mut TestAppContext) {
    let mut draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    draft.content.kind = IntentKind::Bug;
    let opened = open_detail(cx, &draft);
    let window = opened.window;
    assert_eq!(role_of(cx, window, DETAIL_KIND_ID), Some(Role::Button));
    assert_eq!(label_of(cx, window, DETAIL_KIND_ID).as_deref(), Some("Bug"));

    click(cx, window, DETAIL_KIND_ID);
    assert_eq!(
        kind_menu(cx, window),
        [
            ("New feature".to_owned(), false),
            ("Enhancement".to_owned(), false),
            ("Bug".to_owned(), true),
            ("Refactor".to_owned(), false),
            ("Maintenance".to_owned(), false),
        ]
    );
    press(cx, window, "escape");

    choose_kind(cx, window, IntentKind::Refactor);
    assert_eq!(
        calls(&opened),
        [IntentCall::Update(
            draft.id,
            kind_patch(IntentKind::Refactor)
        )]
    );
    assert_eq!(busy(cx, &opened), Some(Busy::Kind));
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(DETAIL_KIND_ID).disabled(), Some(true));
    });

    let refactor = changed(&draft, |i| i.content.kind = IntentKind::Refactor);
    opened.fake.answer(Ok(refactor.clone()));
    frame(cx, window);
    assert_eq!(busy(cx, &opened), None);
    assert_eq!(
        label_of(cx, window, DETAIL_KIND_ID).as_deref(),
        Some("Bug"),
        "nothing changes before the daemon's copy reaches the model"
    );
    upsert(cx, &opened, &refactor);
    assert_eq!(
        label_of(cx, window, DETAIL_KIND_ID).as_deref(),
        Some("Refactor")
    );

    // Choosing the kind it has sends nothing.
    choose_kind(cx, window, IntentKind::Refactor);
    assert_eq!(calls(&opened).len(), 1);
}

#[gpui_kit::test]
fn a_refused_kind_change_says_why(cx: &mut TestAppContext) {
    let draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    let opened = open_detail(cx, &draft);
    let window = opened.window;
    opened.fake.queue(Err(FakeIntents::api_error(
        409,
        "polishing",
        "intent is being polished",
    )));
    choose_kind(cx, window, IntentKind::Maintenance);
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("The intent is still being polished.")
    );
    assert_eq!(
        label_of(cx, window, DETAIL_KIND_ID).as_deref(),
        Some("Enhancement")
    );
    assert!(
        enabled_button(cx, window, DETAIL_KIND_ID),
        "free to try again"
    );
}

#[gpui_kit::test]
fn enter_saves_the_title_and_escape_cancels(cx: &mut TestAppContext) {
    let mut draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    draft.content.title = "Old title".into();
    let opened = open_detail(cx, &draft);
    let window = opened.window;
    assert_eq!(role_of(cx, window, DETAIL_TITLE_ID), Some(Role::Button));
    assert_eq!(title(cx, window).as_deref(), Some("Edit title: Old title"));

    click(cx, window, DETAIL_TITLE_ID);
    assert_eq!(
        title_input(cx, window),
        Some(("Old title".to_owned(), true)),
        "the input holds the title and has the focus"
    );
    assert!(
        !is_drawn(cx, window, DETAIL_TITLE_ID),
        "the input replaces it"
    );
    press(cx, window, "cmd-a");
    type_text(cx, window, "New title");
    press(cx, window, "enter");
    assert_eq!(
        calls(&opened),
        [IntentCall::Update(draft.id, title_patch("New title"))]
    );
    assert_eq!(busy(cx, &opened), Some(Busy::Title));
    assert!(
        title_input(cx, window).is_some(),
        "the input stays until the answer"
    );
    with_window(cx, window, |window, _| {
        assert_eq!(
            window.find(DETAIL_KIND_ID).disabled(),
            Some(true),
            "one command at a time"
        );
    });
    press(cx, window, "enter");
    assert_eq!(calls(&opened).len(), 1, "a second Enter sends nothing");

    let renamed = changed(&draft, |i| i.content.title = "New title".into());
    opened.fake.answer(Ok(renamed.clone()));
    frame(cx, window);
    assert!(title_input(cx, window).is_none(), "the answer closes it");
    assert_eq!(
        title(cx, window).as_deref(),
        Some("Edit title: Old title"),
        "the model has not changed yet"
    );
    upsert(cx, &opened, &renamed);
    assert_eq!(title(cx, window).as_deref(), Some("Edit title: New title"));

    // Escape closes it unchanged, and the focus is back on the title, where
    // Enter opens it again.
    retitle(cx, window, "Something else");
    press(cx, window, "escape");
    assert!(title_input(cx, window).is_none());
    assert_eq!(title(cx, window).as_deref(), Some("Edit title: New title"));
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(DETAIL_TITLE_ID).focused(), Some(true));
    });
    press(cx, window, "enter");
    assert_eq!(
        title_input(cx, window),
        Some(("New title".to_owned(), true)),
        "Enter on the title opens the input"
    );

    // Only spaces, or the same title (spaces around it trimmed): Enter just
    // closes it.
    press(cx, window, "cmd-a");
    type_text(cx, window, "   ");
    press(cx, window, "enter");
    assert!(title_input(cx, window).is_none());
    retitle(cx, window, "  New title ");
    press(cx, window, "enter");
    assert!(title_input(cx, window).is_none());
    assert_eq!(calls(&opened).len(), 1, "no call for blank or same text");
}

#[gpui_kit::test]
fn a_refused_title_keeps_the_input(cx: &mut TestAppContext) {
    let draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    let opened = open_detail(cx, &draft);
    let window = opened.window;
    opened.fake.queue(Err(FakeIntents::api_error(
        400,
        "invalid",
        "title must not be empty",
    )));
    retitle(cx, window, "A better title");
    press(cx, window, "enter");
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("Title must not be empty")
    );
    assert_eq!(
        title_input(cx, window).map(|(text, _)| text).as_deref(),
        Some("A better title"),
        "the text stays to try again"
    );
}

#[gpui_kit::test]
fn the_title_does_not_open_while_a_command_is_in_flight(cx: &mut TestAppContext) {
    let draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    let opened = open_detail(cx, &draft);
    let window = opened.window;
    choose_kind(cx, window, IntentKind::Maintenance);
    assert_eq!(busy(cx, &opened), Some(Busy::Kind));

    click(cx, window, DETAIL_TITLE_ID);
    assert!(title_input(cx, window).is_none(), "one command at a time");

    let maintenance = changed(&draft, |i| i.content.kind = IntentKind::Maintenance);
    opened.fake.answer(Ok(maintenance));
    frame(cx, window);
    click(cx, window, DETAIL_TITLE_ID);
    assert_eq!(
        title_input(cx, window),
        Some(("Fix zoom".to_owned(), true)),
        "free again once the daemon answered"
    );
}

#[gpui_kit::test]
fn a_long_unbroken_title_stays_inside_the_pane(cx: &mut TestAppContext) {
    // A fixed title (sent) and an editable one (in review), 300 characters
    // with no space to break at.
    let long = "W".repeat(300);
    let mut sent = intent("long-title", IntentState::Sent, Phase::Review);
    sent.content.title = long.clone();
    let opened = open_detail(cx, &sent);
    let window = opened.window;
    let dock = opened.shell.read_with(cx, |shell, _| shell.dock().clone());
    let dock_width = dock
        .read_with(cx, |dock, _| dock.dock_size(DockPlacement::Right))
        .expect("the right dock is open");
    let review = changed(&sent, |i| i.state = IntentState::Draft);
    let short = |intent: &Intent| changed(intent, |i| i.content.title = "Short".into());
    let title_height = |cx: &mut TestAppContext| {
        with_window(cx, window, |window, _| {
            window.find(DETAIL_TITLE_ID).bounds().size.height
        })
    };
    for (current, editable) in [(sent, false), (review, true)] {
        // One line high, as a short title is.
        let one_line = short(&current);
        upsert(cx, &opened, &one_line);
        let one_line_height = title_height(cx);
        let current = changed(&one_line, |i| i.content.title = long.clone());
        upsert(cx, &opened, &current);
        assert_eq!(
            role_of(cx, window, DETAIL_TITLE_ID) == Some(Role::Button),
            editable
        );
        assert_eq!(
            title_height(cx),
            one_line_height,
            "the title is cut to one line (editable: {editable})"
        );
        with_window(cx, window, |window, _| {
            let pane = window.find(smithagents_app::shell::INTENTS_PANEL).bounds();
            assert!(
                pane.size.width <= dock_width,
                "the pane ({:?}) is no wider than the dock ({dock_width:?})",
                pane.size.width
            );
            for id in [DETAIL_TITLE_ID, DETAIL_SLUG_ID, DETAIL_STATE_ID] {
                let bounds = window.find(id).bounds();
                assert!(
                    bounds.left() >= pane.left() && bounds.right() <= pane.right(),
                    "{id} {bounds:?} is inside the pane {pane:?} (editable: {editable})"
                );
            }
        });
    }
}

#[gpui_kit::test]
fn the_title_and_kind_are_fixed_while_polishing_and_after_send(cx: &mut TestAppContext) {
    let draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    let opened = open_detail(cx, &draft);
    let window = opened.window;
    let cases = [
        (IntentState::Draft, Phase::Polishing { pass: 1 }, false),
        (IntentState::Draft, Phase::Questions, true),
        (IntentState::Draft, Phase::Polishing { pass: 2 }, false),
        (IntentState::Draft, Phase::Review, true),
        (
            IntentState::Draft,
            Phase::Failed {
                pass: 2,
                message: "no".into(),
            },
            true,
        ),
        (IntentState::Complete, Phase::Review, true),
        (IntentState::Queued, Phase::Review, false),
        (IntentState::Sent, Phase::Review, false),
        (IntentState::Resolved, Phase::Review, false),
    ];
    let mut current = draft;
    for (state, phase, editable) in cases {
        current = changed(&current, |i| {
            i.state = state;
            i.phase = phase.clone();
        });
        upsert(cx, &opened, &current);
        let buttons = (
            role_of(cx, window, DETAIL_KIND_ID) == Some(Role::Button),
            role_of(cx, window, DETAIL_TITLE_ID) == Some(Role::Button),
        );
        assert_eq!(buttons, (editable, editable), "{state:?} {phase:?}");
    }
}

#[gpui_kit::test]
fn an_open_title_input_closes_when_the_intent_stops_being_editable(cx: &mut TestAppContext) {
    let draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    let opened = open_detail(cx, &draft);
    let window = opened.window;
    retitle(cx, window, "Half typed");
    let polishing = changed(&draft, |i| i.phase = Phase::Polishing { pass: 2 });
    upsert(cx, &opened, &polishing);
    assert!(title_input(cx, window).is_none());
    assert_eq!(title(cx, window).as_deref(), Some("Fix zoom"));

    // Editable again: the title, not a stale input.
    let review = changed(&polishing, |i| i.phase = Phase::Review);
    upsert(cx, &opened, &review);
    assert!(title_input(cx, window).is_none());
    assert_eq!(title(cx, window).as_deref(), Some("Edit title: Fix zoom"));
}

// ---- Polish states -----------------------------------------------------------

#[gpui_kit::test]
fn polishing_shows_the_pass_and_no_review_controls(cx: &mut TestAppContext) {
    let polishing = intent("fix-zoom", IntentState::Draft, Phase::Polishing { pass: 1 });
    let opened = open_detail(cx, &polishing);
    let window = opened.window;
    assert_eq!(
        label_of(cx, window, POLISHING_ID).as_deref(),
        Some("Polishing… (pass 1 of 2)")
    );
    assert_eq!(
        label_of(cx, window, DETAIL_STATE_ID).as_deref(),
        Some("Polishing")
    );
    assert!(!is_drawn(cx, window, POLISH_FAILED_ID));
    assert!(!is_drawn(cx, window, SEND_ID));
    for kind in SectionKind::ALL {
        assert!(!is_drawn(cx, window, section_header_id(kind)), "{kind:?}");
    }
    assert_eq!(
        body_source(cx, &opened).as_deref(),
        Some("Seed of fix-zoom"),
        "the blank body shows the seed"
    );

    let review = changed(&polishing, |i| i.phase = Phase::Review);
    upsert(cx, &opened, &review);
    assert!(!is_drawn(cx, window, POLISHING_ID));
    for kind in SectionKind::ALL {
        assert!(is_drawn(cx, window, section_header_id(kind)), "{kind:?}");
    }
}

#[gpui_kit::test]
fn a_failed_polish_offers_try_again(cx: &mut TestAppContext) {
    let failed = intent(
        "fix-zoom",
        IntentState::Draft,
        Phase::Failed {
            pass: 2,
            message: "the harness timed out".into(),
        },
    );
    let opened = open_detail(cx, &failed);
    let window = opened.window;
    assert_eq!(
        label_of(cx, window, POLISH_FAILED_ID).as_deref(),
        Some("Polish failed: the harness timed out")
    );
    assert_eq!(role_of(cx, window, POLISH_FAILED_ID), Some(Role::Status));
    assert_eq!(label_of(cx, window, RETRY_ID).as_deref(), Some("Try again"));
    assert!(enabled_button(cx, window, RETRY_ID));
    assert!(!is_drawn(cx, window, POLISHING_ID));

    click(cx, window, RETRY_ID);
    assert_eq!(calls(&opened), [IntentCall::Retry(failed.id)]);
    assert_eq!(busy(cx, &opened), Some(Busy::Retry));
    assert!(!enabled_button(cx, window, RETRY_ID), "busy");
    click(cx, window, RETRY_ID);
    assert_eq!(calls(&opened).len(), 1, "one retry per press");

    let retried = changed(&failed, |i| i.phase = Phase::Polishing { pass: 2 });
    opened.fake.answer(Ok(retried.clone()));
    upsert(cx, &opened, &retried);
    assert!(!is_drawn(cx, window, POLISH_FAILED_ID));
    assert_eq!(
        label_of(cx, window, POLISHING_ID).as_deref(),
        Some("Polishing… (pass 2 of 2)")
    );
}

// ---- Leaving -----------------------------------------------------------------

#[gpui_kit::test]
fn back_returns_to_the_list(cx: &mut TestAppContext) {
    let draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    let opened = open_detail(cx, &draft);
    let window = opened.window;
    assert_eq!(
        label_of(cx, window, DETAIL_BACK_ID).as_deref(),
        Some("Intents")
    );
    click(cx, window, DETAIL_BACK_ID);
    assert_eq!(screen(cx, &opened), Screen::List);
    assert!(is_drawn(cx, window, INTENT_LIST_ID));
    assert!(!is_drawn(cx, window, DETAIL_BODY_ID));
}

#[gpui_kit::test]
fn a_command_finishing_after_its_screen_is_left_stays_off_the_new_screen(cx: &mut TestAppContext) {
    // A command still in flight when the user leaves its screen runs to its
    // end and frees the controls, but its failure is not shown elsewhere.
    let draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    let opened = open_detail_with(cx, &draft, Some(Duration::from_secs(5)));
    let window = opened.window;

    choose_kind(cx, window, IntentKind::Bug);
    assert_eq!(busy(cx, &opened), Some(Busy::Kind));
    click(cx, window, DETAIL_BACK_ID);
    assert_eq!(screen(cx, &opened), Screen::List);
    assert_eq!(busy(cx, &opened), Some(Busy::Kind), "still running");
    opened.fake.answer(Err(FakeIntents::api_error(
        409,
        "invalid_state",
        "intent is not editable",
    )));
    frame(cx, window);
    assert_eq!(busy(cx, &opened), None, "the controls are free");
    assert_eq!(error_line(cx, window), None, "not on the list");
    click(cx, window, intent_card_id(draft.id));
    assert_eq!(error_line(cx, window), None, "nor back on the detail");

    // The same for a command the daemon never answers.
    choose_kind(cx, window, IntentKind::Refactor);
    assert_eq!(busy(cx, &opened), Some(Busy::Kind));
    click(cx, window, DETAIL_BACK_ID);
    cx.executor().advance_clock(Duration::from_secs(6));
    frame(cx, window);
    assert_eq!(busy(cx, &opened), None);
    assert_eq!(error_line(cx, window), None);

    // On the screen that sent it, the failure shows.
    click(cx, window, intent_card_id(draft.id));
    choose_kind(cx, window, IntentKind::Maintenance);
    cx.executor().advance_clock(Duration::from_secs(6));
    frame(cx, window);
    assert_eq!(error_line(cx, window).as_deref(), Some(DAEMON_SILENT_TEXT));
    assert_eq!(calls(&opened).len(), 3);
}

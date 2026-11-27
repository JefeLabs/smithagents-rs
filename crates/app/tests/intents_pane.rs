//! Headless checks of the Intents pane's list: the cards, the filter, the
//! empty states, opening a screen, and the command runner every screen sends
//! its daemon commands through (one at a time, given up when the daemon goes
//! silent or the connection drops). Commands go to `FakeIntents` and the
//! clock is fixed, so no daemon runs.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Utc};
use common::intents::{fixed_clock, intent, model_with, FakeIntents, IntentCall};
use common::ui::{agent, dark_native, open_shell_options, quad_at, token_background};
use futures::FutureExt as _;
use gpui_kit::base::Root;
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::test::{ElementSnapshot, TestWindowExt as _};
use gpui_kit::{
    px, AppContext as _, Background, Context, ElementId, Entity, Role, TestAppContext, Window,
    WindowHandle,
};
use smithagents_app::intent_vm::IntentFilterChoice;
use smithagents_app::link::{IntentAnswer, IntentCommands as _};
use smithagents_app::model::{Action, Connection, ShellModel};
use smithagents_app::shell::{Shell, ShellOptions};
use smithagents_app::shell_state::{Clock, ShellState};
use smithagents_app::theme_adapter::{agent_accent, srgb_to_hsla};
use smithagents_app::view_models::{DAEMON_SILENT_TEXT, NOT_CONNECTED_TEXT};
use smithagents_app::views::intents::{
    filter_item_id, intent_card_id, Busy, IntentsPane, Screen, CARD_ACCENT_ID, CARD_DOT_ID,
    CARD_HEADING_ID, CARD_LINE_ID, CARD_META_ID, CARD_SLUG_ID, CARD_TITLE_ID, CREATE_CLOSE_ID,
    DETAIL_BACK_ID, INTENTS_EMPTY_ID, INTENTS_FILTER_ID, INTENTS_NEW_ID, INTENT_ERROR_ID,
    INTENT_LIST_ID,
};
use smithagents_app::views::rail::RailEntry;
use smithagents_core::event::{Event, EventKind};
use smithagents_core::ids::IntentId;
use smithagents_core::intent::{Intent, IntentState, Phase};

/// What the fixed clock reads: 18:15 UTC, shown in a −08:00 zone.
const NOW: &str = "2026-11-26T10:15:00-08:00";

/// `hh:mm` UTC on the day the intents are made.
fn at(time: &str) -> DateTime<Utc> {
    format!("2026-11-26T{time}:00Z").parse().unwrap()
}

/// A Draft in review, last changed at `time`.
fn changed_at(slug: &str, time: &str) -> Intent {
    let mut intent = intent(slug, IntentState::Draft, Phase::Review);
    intent.updated_at = at(time);
    intent
}

/// The shell with the Intents pane on screen.
struct Opened {
    window: WindowHandle<Root>,
    state: Entity<ShellState>,
    pane: Entity<IntentsPane>,
    fake: Rc<FakeIntents>,
}

/// What a test opens the shell with.
struct Setup {
    model: ShellModel,
    clock: Clock,
    command_timeout: Option<Duration>,
}

impl Setup {
    /// A connected model with these intents and agents, and the fixed clock.
    fn new(intents: Vec<Intent>, agents: Vec<smithagents_core::agent::Agent>) -> Self {
        Self::with_model(model_with(intents, agents, vec![]))
    }

    fn with_model(model: ShellModel) -> Self {
        Self {
            model,
            clock: fixed_clock(NOW),
            command_timeout: None,
        }
    }

    /// Opens the shell, clicks the rail's Intents button and draws a frame.
    fn open(self, cx: &mut TestAppContext) -> Opened {
        let fake = Rc::new(FakeIntents::default());
        let (window, shell) = open_shell_options(
            cx,
            ShellOptions {
                model: self.model,
                intent_commands: Some(fake.clone()),
                clock: Some(self.clock),
                command_timeout: self.command_timeout,
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

/// What the part `part` of `intent`'s card reads, or `None` when the card
/// leaves it out.
fn card_part(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    intent: IntentId,
    part: &'static str,
) -> Option<String> {
    with_window(cx, window, |window, _| {
        window
            .within(intent_card_id(intent))
            .try_find(part)
            .as_ref()
            .and_then(text)
    })
}

/// The intents whose cards are drawn, top to bottom.
fn cards(cx: &mut TestAppContext, window: WindowHandle<Root>, among: &[&Intent]) -> Vec<IntentId> {
    with_window(cx, window, |window, _| {
        let mut drawn: Vec<_> = among
            .iter()
            .filter_map(|intent| {
                window
                    .try_find(intent_card_id(intent.id))
                    .map(|card| (card.bounds().origin.y, intent.id))
            })
            .collect();
        drawn.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        drawn.into_iter().map(|(_, id)| id).collect()
    })
}

fn screen(cx: &mut TestAppContext, pane: &Entity<IntentsPane>) -> Screen {
    pane.read_with(cx, |pane, _| *pane.screen())
}

fn busy(cx: &mut TestAppContext, pane: &Entity<IntentsPane>) -> Option<Busy> {
    pane.read_with(cx, |pane, _| pane.busy())
}

fn dispatch(cx: &mut TestAppContext, opened: &Opened, action: Action) {
    opened
        .state
        .update(cx, |state, cx| state.dispatch(action, cx));
    frame(cx, opened.window);
}

/// Opens the filter menu and chooses `choice`.
fn choose_filter(cx: &mut TestAppContext, window: WindowHandle<Root>, choice: IntentFilterChoice) {
    open_filter_menu(cx, window);
    with_window(cx, window, |window, cx| {
        window
            .within("popup-menu")
            .click(filter_item_id(choice), cx)
    });
    frame(cx, window);
}

fn open_filter_menu(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    click(cx, window, INTENTS_FILTER_ID);
}

/// Whether the open filter menu checks `choice`'s entry (`None`: no check
/// state, as for any entry not checked).
fn filter_checked(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    choice: IntentFilterChoice,
) -> Option<bool> {
    with_window(cx, window, |window, _| {
        let menu = window.within("popup-menu");
        let entry = menu.find(filter_item_id(choice));
        // The entry's own node sits inside the menu's item, which carries the
        // check state.
        menu.find_all_by_role(Role::MenuItem)
            .into_iter()
            .find(|item| entry.path().starts_with(item.path()))
            .expect("the entry is inside a menu item")
            .checked()
    })
}

/// The send as the link makes it: nothing goes out until the future is
/// polled, so a command `run` refuses is never sent. (`FakeIntents` records a
/// call when the future is made; the link's futures do nothing until then.)
fn send(fake: &Rc<FakeIntents>, id: IntentId) -> IntentAnswer {
    let fake = fake.clone();
    async move { fake.send_intent(id).await }.boxed_local()
}

/// Runs a send of `id` through the pane's runner.
fn run_send(
    cx: &mut TestAppContext,
    opened: &Opened,
    id: IntentId,
    done: impl FnOnce(&mut IntentsPane, Intent, &mut Window, &mut Context<IntentsPane>) + 'static,
) {
    let command = send(&opened.fake, id);
    let pane = opened.pane.clone();
    cx.update_window(opened.window.into(), |_, window, cx| {
        pane.update(cx, |pane, cx| {
            pane.run(Busy::Send, command, done, window, cx)
        })
    })
    .unwrap();
    frame(cx, opened.window);
}

fn sends(opened: &Opened) -> usize {
    opened
        .fake
        .calls
        .borrow()
        .iter()
        .filter(|call| matches!(call, IntentCall::Send(_)))
        .count()
}

fn error_line(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Option<String> {
    with_window(cx, window, |window, _| {
        window.try_find(INTENT_ERROR_ID).map(|line| {
            assert_eq!(line.role(), Some(Role::Status));
            line.label().unwrap().to_owned()
        })
    })
}

// ---- Cards -------------------------------------------------------------------

#[gpui_kit::test]
fn a_card_shows_slug_title_meta_and_preview(cx: &mut TestAppContext) {
    let mut mercury = agent("Mercury");
    mercury.state = smithagents_core::agent::AgentState::Done;
    let mut sent = intent("browser-pane-zoom-bounds", IntentState::Sent, Phase::Review);
    sent.content.title = "Fix Browser page drawn too big".into();
    sent.content.current_behavior = "## What happens\nThe page is drawn in the wrong place.".into();
    sent.agent_id = Some(mercury.id);
    sent.created_at = at("14:15");
    // Four hours before the clock's 18:15 UTC.
    sent.sent_at = Some(at("14:15"));
    let opened = Setup::new(vec![sent.clone()], vec![mercury.clone()]).open(cx);
    let window = opened.window;

    let part = |cx: &mut TestAppContext, id| card_part(cx, window, sent.id, id);
    assert_eq!(
        part(cx, CARD_SLUG_ID).as_deref(),
        Some("browser-pane-zoom-bounds")
    );
    assert_eq!(
        part(cx, CARD_TITLE_ID).as_deref(),
        Some("Fix Browser page drawn too big")
    );
    assert_eq!(
        part(cx, CARD_META_ID).as_deref(),
        Some("Nov 26, 2026, 6:15 AM | Sent | done | 4h")
    );
    assert_eq!(part(cx, CARD_HEADING_ID).as_deref(), Some("What happens"));
    assert_eq!(
        part(cx, CARD_LINE_ID).as_deref(),
        Some("The page is drawn in the wrong place.")
    );

    let native = dark_native();
    with_window(cx, window, |window, _| {
        let card = window.find(intent_card_id(sent.id));
        assert_eq!(card.role(), Some(Role::Button));
        assert_eq!(card.label(), Some("Fix Browser page drawn too big, Sent"));
        assert_eq!(
            window.find(INTENT_LIST_ID).role(),
            Some(Role::List),
            "the cards are a list"
        );
        let bar = window
            .within(intent_card_id(sent.id))
            .find(CARD_ACCENT_ID)
            .bounds();
        assert!(bar.size.width > px(0.) && bar.size.height > px(0.));
        assert_eq!(
            quad_at(window, bar.center()).background,
            Background::from(srgb_to_hsla(agent_accent(&native, mercury.color.h))),
            "the accent bar is the agent's color"
        );
        let dot = window
            .within(intent_card_id(sent.id))
            .find(CARD_DOT_ID)
            .bounds();
        assert_eq!(
            quad_at(window, dot.center()).background,
            token_background(&native, "status-ok"),
            "a done agent's dot"
        );
    });
}

#[gpui_kit::test]
fn a_card_leaves_out_what_it_lacks(cx: &mut TestAppContext) {
    // No agent, so no accent; a blank body, so the seed's text (as the
    // detail shows it), which has no heading.
    let draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    let opened = Setup::new(vec![draft.clone()], vec![]).open(cx);
    let window = opened.window;

    assert_eq!(
        card_part(cx, window, draft.id, CARD_TITLE_ID).as_deref(),
        Some("Fix zoom")
    );
    assert_eq!(card_part(cx, window, draft.id, CARD_HEADING_ID), None);
    assert_eq!(
        card_part(cx, window, draft.id, CARD_LINE_ID).as_deref(),
        Some("Seed of fix-zoom")
    );
    let native = dark_native();
    with_window(cx, window, |window, _| {
        let bar = window
            .within(intent_card_id(draft.id))
            .find(CARD_ACCENT_ID)
            .bounds();
        assert_eq!(
            quad_at(window, bar.center()).background,
            token_background(&native, "border"),
            "without an agent the bar is the card's border color"
        );
    });
}

#[gpui_kit::test]
fn cards_are_newest_first_and_follow_events(cx: &mut TestAppContext) {
    let older = changed_at("older-one", "14:15");
    let newer = changed_at("newer-one", "15:15");
    let opened = Setup::new(vec![older.clone(), newer.clone()], vec![]).open(cx);
    let window = opened.window;
    assert_eq!(cards(cx, window, &[&older, &newer]), [newer.id, older.id]);

    let mut renamed = older.clone();
    renamed.updated_at = at("16:15");
    renamed.content.title = "Renamed".into();
    dispatch(
        cx,
        &opened,
        Action::Event(Event {
            seq: 2,
            at: at("16:15"),
            kind: EventKind::IntentUpdated(renamed),
        }),
    );
    assert_eq!(cards(cx, window, &[&older, &newer]), [older.id, newer.id]);
    assert_eq!(
        card_part(cx, window, older.id, CARD_TITLE_ID).as_deref(),
        Some("Renamed")
    );
}

#[gpui_kit::test]
fn the_pane_redraws_every_minute_so_ages_advance(cx: &mut TestAppContext) {
    let draft = changed_at("fix-zoom", "18:10");
    let start: DateTime<FixedOffset> = DateTime::parse_from_rfc3339(NOW).unwrap();
    let now = Rc::new(Cell::new(start));
    let clock: Clock = {
        let now = now.clone();
        Rc::new(move || now.get())
    };
    let opened = Setup {
        clock,
        ..Setup::new(vec![draft.clone()], vec![])
    }
    .open(cx);
    let window = opened.window;
    assert_eq!(
        card_part(cx, window, draft.id, CARD_META_ID).as_deref(),
        Some("Nov 26, 2026, 6:15 AM | Review | 5m")
    );

    // A test frame redraws every view, so count the pane's own redraw
    // requests: in the app, nothing else redraws an idle pane.
    let redraws = Rc::new(Cell::new(0));
    let _counting = cx.update(|cx| {
        let redraws = redraws.clone();
        cx.observe(&opened.pane, move |_, _| redraws.set(redraws.get() + 1))
    });
    cx.executor().advance_clock(Duration::from_secs(59));
    cx.run_until_parked();
    assert_eq!(
        redraws.get(),
        0,
        "nothing asks for a redraw within the minute"
    );
    now.set(start + chrono::Duration::minutes(1));
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(redraws.get(), 1, "the minute's redraw");
    frame(cx, window);
    assert_eq!(
        card_part(cx, window, draft.id, CARD_META_ID).as_deref(),
        Some("Nov 26, 2026, 6:15 AM | Review | 6m"),
        "the minute timer redrew the card"
    );
}

// ---- Filter and empty states -------------------------------------------------

#[gpui_kit::test]
fn the_filter_narrows_the_list(cx: &mut TestAppContext) {
    let draft = intent("a-draft", IntentState::Draft, Phase::Review);
    let complete = intent("a-complete", IntentState::Complete, Phase::Review);
    let sent = intent("a-sent", IntentState::Sent, Phase::Review);
    let resolved = intent("a-resolved", IntentState::Resolved, Phase::Review);
    let all = [&draft, &complete, &sent, &resolved];
    let opened = Setup::new(all.iter().map(|i| (*i).clone()).collect(), vec![]).open(cx);
    let window = opened.window;
    assert_eq!(cards(cx, window, &all).len(), 4);
    assert_eq!(
        opened.pane.read_with(cx, |pane, _| pane.filter()),
        IntentFilterChoice::All
    );

    with_window(cx, window, |window, _| {
        let filter = window.find(INTENTS_FILTER_ID);
        assert_eq!(filter.label(), Some("Filter"));
        assert_eq!(window.find(INTENTS_NEW_ID).label(), Some("New intent"));
    });
    open_filter_menu(cx, window);
    with_window(cx, window, |window, _| {
        let menu = window.within("popup-menu");
        let labels: Vec<_> = IntentFilterChoice::ALL
            .iter()
            .map(|choice| text(&menu.find(filter_item_id(*choice))).unwrap())
            .collect();
        assert_eq!(
            labels,
            [
                "All",
                "Draft and polishing",
                "Complete and queued",
                "Sent",
                "Resolved"
            ]
        );
        assert!(menu.try_find_by_label("Show").is_some(), "labelled Show");
    });
    assert_eq!(
        filter_checked(cx, window, IntentFilterChoice::All),
        Some(true)
    );
    press(cx, window, "escape");

    choose_filter(cx, window, IntentFilterChoice::Sent);
    assert_eq!(cards(cx, window, &all), [sent.id]);
    assert_eq!(
        opened.pane.read_with(cx, |pane, _| pane.filter()),
        IntentFilterChoice::Sent
    );
    open_filter_menu(cx, window);
    assert_eq!(
        filter_checked(cx, window, IntentFilterChoice::Sent),
        Some(true)
    );
    assert_ne!(
        filter_checked(cx, window, IntentFilterChoice::All),
        Some(true),
        "one entry is checked"
    );
    press(cx, window, "escape");

    choose_filter(cx, window, IntentFilterChoice::Resolved);
    assert_eq!(cards(cx, window, &all), [resolved.id]);
    choose_filter(cx, window, IntentFilterChoice::All);
    assert_eq!(cards(cx, window, &all).len(), 4);
}

#[gpui_kit::test]
fn empty_states(cx: &mut TestAppContext) {
    // Never loaded and not connected.
    let opened = Setup::with_model(ShellModel::new()).open(cx);
    let window = opened.window;
    assert_eq!(
        label_of(cx, window, INTENTS_EMPTY_ID).as_deref(),
        Some(NOT_CONNECTED_TEXT)
    );
    assert!(label_of(cx, window, INTENT_LIST_ID).is_none());

    dispatch(cx, &opened, Action::IntentsLoaded(vec![]));
    dispatch(
        cx,
        &opened,
        Action::ConnectionChanged(Connection::Connected),
    );
    assert_eq!(
        label_of(cx, window, INTENTS_EMPTY_ID).as_deref(),
        Some("No intents yet. Press + to describe what you want.")
    );

    let draft = intent("a-draft", IntentState::Draft, Phase::Review);
    dispatch(cx, &opened, Action::IntentsLoaded(vec![draft.clone()]));
    assert_eq!(label_of(cx, window, INTENTS_EMPTY_ID), None);
    choose_filter(cx, window, IntentFilterChoice::Sent);
    assert_eq!(
        label_of(cx, window, INTENTS_EMPTY_ID).as_deref(),
        Some("No intents match this filter.")
    );
    assert!(cards(cx, window, &[&draft]).is_empty());
}

// ---- Screens -----------------------------------------------------------------

#[gpui_kit::test]
fn a_card_opens_its_detail_by_click_and_by_enter(cx: &mut TestAppContext) {
    let first = changed_at("first-one", "15:15");
    let second = changed_at("second-one", "14:15");
    let opened = Setup::new(vec![first.clone(), second.clone()], vec![]).open(cx);
    let window = opened.window;
    assert_eq!(screen(cx, &opened.pane), Screen::List);

    click(cx, window, intent_card_id(second.id));
    assert_eq!(screen(cx, &opened.pane), Screen::Detail(second.id));
    assert!(
        label_of(cx, window, INTENT_LIST_ID).is_none(),
        "the list is gone"
    );

    click(cx, window, DETAIL_BACK_ID);
    assert_eq!(screen(cx, &opened.pane), Screen::List);
    with_window(cx, window, |window, _| {
        assert_eq!(
            window.find(intent_card_id(second.id)).focused(),
            Some(true),
            "back gives the focus to the card the detail was opened from"
        );
    });
    press(cx, window, "enter");
    assert_eq!(screen(cx, &opened.pane), Screen::Detail(second.id));

    // Tab reaches a card, and Enter on it opens it.
    click(cx, window, DETAIL_BACK_ID);
    let focused = |cx: &mut TestAppContext| {
        with_window(cx, window, |window, _| {
            window.find(intent_card_id(first.id)).focused() == Some(true)
        })
    };
    for _ in 0..40 {
        if focused(cx) {
            break;
        }
        press(cx, window, "tab");
    }
    assert!(focused(cx), "Tab reaches the card");
    press(cx, window, "enter");
    assert_eq!(screen(cx, &opened.pane), Screen::Detail(first.id));
}

#[gpui_kit::test]
fn the_plus_button_opens_the_create_screen(cx: &mut TestAppContext) {
    let opened = Setup::new(vec![], vec![]).open(cx);
    let window = opened.window;
    click(cx, window, INTENTS_NEW_ID);
    assert_eq!(screen(cx, &opened.pane), Screen::Create);
    assert_eq!(
        label_of(cx, window, CREATE_CLOSE_ID).as_deref(),
        Some("Close")
    );
    click(cx, window, CREATE_CLOSE_ID);
    assert_eq!(screen(cx, &opened.pane), Screen::List);
}

#[gpui_kit::test]
fn a_reconnect_that_no_longer_has_the_open_intent_returns_to_the_list(cx: &mut TestAppContext) {
    let draft = intent("a-draft", IntentState::Draft, Phase::Review);
    let opened = Setup::new(vec![draft.clone()], vec![]).open(cx);
    click(cx, opened.window, intent_card_id(draft.id));
    assert_eq!(screen(cx, &opened.pane), Screen::Detail(draft.id));

    dispatch(
        cx,
        &opened,
        Action::ConnectionChanged(Connection::Reconnecting),
    );
    dispatch(cx, &opened, Action::IntentsLoaded(vec![draft.clone()]));
    assert_eq!(
        screen(cx, &opened.pane),
        Screen::Detail(draft.id),
        "a load that still has it keeps the detail"
    );

    dispatch(cx, &opened, Action::IntentsLoaded(vec![]));
    assert_eq!(screen(cx, &opened.pane), Screen::List);
}

// ---- The command runner ------------------------------------------------------

#[gpui_kit::test]
fn a_silent_daemon_frees_the_control_and_says_so(cx: &mut TestAppContext) {
    let draft = intent("a-draft", IntentState::Complete, Phase::Review);
    let opened = Setup {
        command_timeout: Some(Duration::from_secs(5)),
        ..Setup::new(vec![draft.clone()], vec![])
    }
    .open(cx);
    let window = opened.window;

    run_send(cx, &opened, draft.id, |_, _, _, _| panic!("no answer came"));
    assert_eq!(busy(cx, &opened.pane), Some(Busy::Send));
    assert_eq!(sends(&opened), 1);
    run_send(cx, &opened, draft.id, |_, _, _, _| panic!("never sent"));
    assert_eq!(sends(&opened), 1, "a second run while busy sends nothing");
    assert_eq!(busy(cx, &opened.pane), Some(Busy::Send));

    cx.executor().advance_clock(Duration::from_secs(4));
    frame(cx, window);
    assert_eq!(busy(cx, &opened.pane), Some(Busy::Send), "still waiting");
    assert_eq!(error_line(cx, window), None);
    cx.executor().advance_clock(Duration::from_secs(2));
    frame(cx, window);
    assert_eq!(busy(cx, &opened.pane), None, "given up after the timeout");
    assert_eq!(error_line(cx, window).as_deref(), Some(DAEMON_SILENT_TEXT));

    // The control is free again: the next run is sent and clears the line.
    run_send(cx, &opened, draft.id, |_, _, _, _| {});
    assert_eq!(sends(&opened), 2);
    assert_eq!(error_line(cx, window), None);
}

#[gpui_kit::test]
fn a_dropped_connection_frees_the_control_and_says_so(cx: &mut TestAppContext) {
    let draft = intent("a-draft", IntentState::Complete, Phase::Review);
    let opened = Setup::new(vec![draft.clone()], vec![]).open(cx);
    let window = opened.window;

    run_send(cx, &opened, draft.id, |_, _, _, _| panic!("no answer came"));
    assert_eq!(busy(cx, &opened.pane), Some(Busy::Send));
    dispatch(
        cx,
        &opened,
        Action::ConnectionChanged(Connection::Reconnecting),
    );
    assert_eq!(busy(cx, &opened.pane), None);
    assert_eq!(error_line(cx, window).as_deref(), Some(DAEMON_SILENT_TEXT));
}

#[gpui_kit::test]
fn an_answer_reaches_the_caller(cx: &mut TestAppContext) {
    let complete = intent("a-complete", IntentState::Complete, Phase::Review);
    let opened = Setup::new(vec![complete.clone()], vec![]).open(cx);
    let mut answer = complete.clone();
    answer.state = IntentState::Sent;
    opened.fake.queue(Ok(answer.clone()));
    let got = Rc::new(Cell::new(None));
    let seen = got.clone();
    run_send(cx, &opened, complete.id, move |pane, intent, _, _| {
        assert_eq!(pane.busy(), None, "not busy once the answer is in");
        seen.set(Some(intent.state));
    });
    assert_eq!(got.get(), Some(IntentState::Sent));
    assert_eq!(busy(cx, &opened.pane), None);
    assert_eq!(error_line(cx, opened.window), None);
}

#[gpui_kit::test]
fn a_refused_command_shows_the_daemons_sentence(cx: &mut TestAppContext) {
    let complete = intent("a-complete", IntentState::Complete, Phase::Review);
    let opened = Setup::new(vec![complete.clone()], vec![]).open(cx);
    let window = opened.window;
    opened.fake.queue(Err(FakeIntents::api_error(
        409,
        "invalid_state",
        "intent is not complete",
    )));
    run_send(cx, &opened, complete.id, |_, _, _, _| panic!("refused"));
    assert_eq!(busy(cx, &opened.pane), None);
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("The intent changed, so this is no longer possible.")
    );

    click(cx, window, INTENTS_NEW_ID);
    assert_eq!(screen(cx, &opened.pane), Screen::Create);
    click(cx, window, CREATE_CLOSE_ID);
    assert_eq!(error_line(cx, window), None, "opening a screen clears it");
}

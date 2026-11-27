//! Headless checks of the detail's actions: Send on a complete intent, the
//! queued reason, Open chat and Mark resolved on a sent one (with its
//! confirmation, whose Enter cancels), the start error above the bar, and
//! that Enter never sends. Commands go to `FakeIntents`, so no daemon runs;
//! the model changes only when the test dispatches the daemon's copy.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::rc::Rc;

use common::intents::{fixed_clock, intent, item, model_with, FakeIntents, IntentCall};
use common::ui::{agent, fake_connection, open_shell_options};
use gpui_kit::base::Root;
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::{ElementSnapshot, TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{
    px, AppContext as _, ElementId, Entity, Focusable as _, Role, TestAppContext, WindowHandle,
};
use smithagents_app::model::Action;
use smithagents_app::shell::{Shell, ShellOptions, INTENTS_PANEL};
use smithagents_app::shell_state::ShellState;
use smithagents_app::views::intents::{
    intent_card_id, item_confirm_id, Busy, IntentsPane, Screen, DETAIL_BACK_ID, DETAIL_STATE_ID,
    DETAIL_TITLE_ID, DETAIL_TITLE_INPUT_ID, INTENT_ERROR_ID, OPEN_CHAT_ID, QUEUED_REASON_ID,
    RESOLVE_CANCEL_ID, RESOLVE_CONFIRM_ID, RESOLVE_DESCRIPTION, RESOLVE_DESCRIPTION_ID, RESOLVE_ID,
    RESOLVE_TITLE_ID, SEND_ID, START_ERROR_ID,
};
use smithagents_app::views::rail::RailEntry;
use smithagents_core::agent::Agent;
use smithagents_core::ids::IntentId;
use smithagents_core::intent::{Intent, IntentState, Item, ItemStatus, Phase};

/// The shell with the Intents pane on screen.
struct Opened {
    window: WindowHandle<Root>,
    state: Entity<ShellState>,
    pane: Entity<IntentsPane>,
    fake: Rc<FakeIntents>,
}

/// Opens the shell, connected (with a daemon port and token, so tabs open),
/// with these intents and agents, shows the Intents list and opens the first
/// intent's detail.
fn open_detail(cx: &mut TestAppContext, intents: Vec<Intent>, agents: Vec<Agent>) -> Opened {
    let first = intents[0].id;
    let fake = Rc::new(FakeIntents::default());
    let (window, shell) = open_shell_options(
        cx,
        ShellOptions {
            model: model_with(intents, agents, vec![]),
            intent_commands: Some(fake.clone()),
            clock: Some(fixed_clock("2026-11-26T10:15:00-08:00")),
            connection_info: Some(fake_connection()),
            ..Default::default()
        },
        (1200., 800.),
    );
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    frame(cx, window);
    click(cx, window, RailEntry::Intents.element_id());
    let pane = intents_pane(cx, &shell);
    let opened = Opened {
        window,
        state,
        pane,
        fake,
    };
    show(cx, &opened, first);
    opened
}

/// Opens intent `id`'s detail from the list.
fn show(cx: &mut TestAppContext, opened: &Opened, id: IntentId) {
    if screen(cx, opened) != Screen::List {
        click(cx, opened.window, DETAIL_BACK_ID);
    }
    click(cx, opened.window, intent_card_id(id));
    assert_eq!(screen(cx, opened), Screen::Detail(id));
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

fn is_drawn(cx: &mut TestAppContext, window: WindowHandle<Root>, id: impl Into<ElementId>) -> bool {
    let id = id.into();
    with_window(cx, window, |window, _| window.try_find(id).is_some())
}

/// Whether `id` is drawn as a button that is not disabled.
fn enabled_button(cx: &mut TestAppContext, window: WindowHandle<Root>, id: &'static str) -> bool {
    with_window(cx, window, |window, _| {
        window
            .try_find(id)
            .is_some_and(|el| el.role() == Some(Role::Button) && el.disabled() != Some(true))
    })
}

/// Whether `id` is drawn as a button that is disabled.
fn disabled_button(cx: &mut TestAppContext, window: WindowHandle<Root>, id: &'static str) -> bool {
    with_window(cx, window, |window, _| {
        window
            .try_find(id)
            .is_some_and(|el| el.role() == Some(Role::Button) && el.disabled() == Some(true))
    })
}

/// How many buttons read `label`.
fn buttons_labelled(cx: &mut TestAppContext, window: WindowHandle<Root>, label: &str) -> usize {
    with_window(cx, window, |window, _| {
        window
            .find_all_by_role(Role::Button)
            .iter()
            .filter(|button| button.label() == Some(label))
            .count()
    })
}

fn is_focused(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    id: impl Into<ElementId>,
) -> bool {
    let id = id.into();
    with_window(cx, window, |window, _| {
        window.find(id).focused() == Some(true)
    })
}

fn screen(cx: &mut TestAppContext, opened: &Opened) -> Screen {
    opened.pane.read_with(cx, |pane, _| *pane.screen())
}

fn busy(cx: &mut TestAppContext, opened: &Opened) -> Option<Busy> {
    opened.pane.read_with(cx, |pane, _| pane.busy())
}

fn state_label(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Option<String> {
    label_of(cx, window, DETAIL_STATE_ID)
}

fn dialog_open(cx: &mut TestAppContext, window: WindowHandle<Root>) -> bool {
    with_window(cx, window, |window, cx| window.has_active_dialog(cx))
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

/// `intent` as the daemon has it once sent and started as `agent`.
fn sent_as(intent: &Intent, agent: &Agent) -> Intent {
    changed(intent, |i| {
        i.state = IntentState::Sent;
        i.agent_id = Some(agent.id);
        i.sent_at = Some(i.updated_at);
    })
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

/// None of the bar's parts is drawn.
fn no_bar(cx: &mut TestAppContext, window: WindowHandle<Root>) -> bool {
    [SEND_ID, QUEUED_REASON_ID, OPEN_CHAT_ID, RESOLVE_ID]
        .into_iter()
        .all(|id| !is_drawn(cx, window, id))
}

/// A complete intent and a sent one started as `agent`.
fn complete() -> Intent {
    intent("fix-zoom", IntentState::Complete, Phase::Review)
}

fn sent(agent: &Agent) -> Intent {
    let mut sent = intent("fix-zoom", IntentState::Sent, Phase::Review);
    sent.agent_id = Some(agent.id);
    sent
}

fn dependency(text: &str, on: IntentId, status: ItemStatus) -> Item {
    Item {
        intent_ref: Some(on),
        ..item(text, status)
    }
}

// ---- Send --------------------------------------------------------------------

#[gpui_kit::test]
fn a_complete_intent_offers_send(cx: &mut TestAppContext) {
    let complete = complete();
    let review = intent("in-review", IntentState::Draft, Phase::Review);
    let queued = intent("queued-one", IntentState::Queued, Phase::Review);
    let resolved = intent("resolved-one", IntentState::Resolved, Phase::Review);
    let opened = open_detail(
        cx,
        vec![
            complete.clone(),
            review.clone(),
            queued.clone(),
            resolved.clone(),
        ],
        vec![],
    );
    let window = opened.window;
    assert_eq!(buttons_labelled(cx, window, "Send"), 1);
    assert_eq!(label_of(cx, window, SEND_ID).as_deref(), Some("Send"));
    assert!(enabled_button(cx, window, SEND_ID));
    assert!(!is_drawn(cx, window, QUEUED_REASON_ID));
    assert!(!is_drawn(cx, window, OPEN_CHAT_ID));
    assert!(!is_drawn(cx, window, RESOLVE_ID));

    // A draft and a resolved intent have no bar at all.
    for (other, bar) in [(&review, false), (&queued, true), (&resolved, false)] {
        show(cx, &opened, other.id);
        assert_eq!(buttons_labelled(cx, window, "Send"), 0, "{}", other.slug);
        assert!(!is_drawn(cx, window, SEND_ID), "{}", other.slug);
        assert_eq!(!no_bar(cx, window), bar, "{}", other.slug);
    }
    assert!(calls(&opened).is_empty());
}

#[gpui_kit::test]
fn send_waits_for_the_daemon(cx: &mut TestAppContext) {
    // Review Focus 3: one request per press, and nothing changes before the
    // daemon's copy arrives.
    let mercury = agent("Mercury");
    let complete = complete();
    let opened = open_detail(cx, vec![complete.clone()], vec![mercury.clone()]);
    let window = opened.window;

    click(cx, window, SEND_ID);
    assert_eq!(calls(&opened), [IntentCall::Send(complete.id)]);
    assert_eq!(
        busy(cx, &opened),
        Some(Busy::Send),
        "Send shows the spinner"
    );
    assert!(disabled_button(cx, window, SEND_ID));
    assert_eq!(state_label(cx, window).as_deref(), Some("Complete"));

    click(cx, window, SEND_ID);
    with_window(cx, window, |window, cx| window.double_click(SEND_ID, cx));
    frame(cx, window);
    assert_eq!(calls(&opened).len(), 1, "a press while busy sends nothing");

    let sent = sent_as(&complete, &mercury);
    opened.fake.answer(Ok(sent.clone()));
    frame(cx, window);
    assert_eq!(busy(cx, &opened), None);
    assert_eq!(
        state_label(cx, window).as_deref(),
        Some("Complete"),
        "the view waits for the model"
    );

    upsert(cx, &opened, &sent);
    assert_eq!(state_label(cx, window).as_deref(), Some("Sent"));
    assert!(!is_drawn(cx, window, SEND_ID));
    assert_eq!(
        label_of(cx, window, OPEN_CHAT_ID).as_deref(),
        Some("Open chat")
    );
    assert!(enabled_button(cx, window, OPEN_CHAT_ID));
    assert_eq!(
        label_of(cx, window, RESOLVE_ID).as_deref(),
        Some("Mark resolved…")
    );
    assert!(enabled_button(cx, window, RESOLVE_ID));
    assert_eq!(error_line(cx, window), None);
}

#[gpui_kit::test]
fn a_refused_send_is_explained(cx: &mut TestAppContext) {
    // Review Focus 3: someone else changed the intent first.
    let complete = complete();
    let opened = open_detail(cx, vec![complete.clone()], vec![]);
    let window = opened.window;
    opened.fake.queue(Err(FakeIntents::api_error(
        409,
        "invalid_state",
        "intent is not complete",
    )));
    click(cx, window, SEND_ID);
    assert_eq!(calls(&opened), [IntentCall::Send(complete.id)]);
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("The intent changed, so this is no longer possible.")
    );
    assert_eq!(busy(cx, &opened), None);
    assert!(enabled_button(cx, window, SEND_ID), "Send is enabled again");
    assert_eq!(state_label(cx, window).as_deref(), Some("Complete"));

    // The daemon's copy shows what happened.
    let queued = changed(&complete, |i| i.state = IntentState::Queued);
    upsert(cx, &opened, &queued);
    assert_eq!(state_label(cx, window).as_deref(), Some("Queued"));
    assert!(!is_drawn(cx, window, SEND_ID));
    assert_eq!(
        label_of(cx, window, QUEUED_REASON_ID).as_deref(),
        Some("Waiting for a free agent slot.")
    );
}

#[gpui_kit::test]
fn enter_never_sends(cx: &mut TestAppContext) {
    let mut complete = complete();
    complete.content.criteria = vec![item("The zoom stays", ItemStatus::Pending)];
    let pending = complete.content.criteria[0].id;
    let opened = open_detail(cx, vec![complete.clone()], vec![]);
    let window = opened.window;
    let sends = |opened: &Opened| {
        calls(opened)
            .iter()
            .filter(|call| matches!(call, IntentCall::Send(_)))
            .count()
    };

    // The pane has the focus.
    let pane = opened.pane.clone();
    with_window(cx, window, |window, cx| {
        pane.read(cx).focus_handle(cx).focus(window, cx)
    });
    press(cx, window, "enter");
    assert!(calls(&opened).is_empty(), "Enter on the pane does nothing");

    // Inside the title input, after opening it: Enter saves the title.
    click(cx, window, DETAIL_TITLE_ID);
    assert!(is_focused(cx, window, DETAIL_TITLE_INPUT_ID));
    press(cx, window, "cmd-a");
    type_text(cx, window, "Fix the zoom");
    press(cx, window, "enter");
    assert_eq!(sends(&opened), 0);
    assert!(
        matches!(calls(&opened)[..], [IntentCall::Update(..)]),
        "{:?}",
        calls(&opened)
    );
    opened.fake.answer(Ok(changed(&complete, |i| {
        i.content.title = "Fix the zoom".to_owned()
    })));
    frame(cx, window);

    // On an item card: Enter activates the focused Confirm.
    for _ in 0..40 {
        if is_focused(cx, window, item_confirm_id(pending)) {
            break;
        }
        press(cx, window, "tab");
    }
    assert!(is_focused(cx, window, item_confirm_id(pending)));
    press(cx, window, "enter");
    assert_eq!(sends(&opened), 0);
    assert_eq!(calls(&opened).len(), 2, "Enter confirmed the item");
    assert!(
        !is_focused(cx, window, SEND_ID),
        "the focus never moves to Send"
    );
}

// ---- Queued ------------------------------------------------------------------

#[gpui_kit::test]
fn a_queued_intent_says_what_it_waits_for(cx: &mut TestAppContext) {
    let other = intent("repos-integration-pane", IntentState::Sent, Phase::Review);
    let mut queued = intent("fix-zoom", IntentState::Queued, Phase::Review);
    queued.content.depends_on = vec![dependency(
        "Needs the repositories pane",
        other.id,
        ItemStatus::Accepted,
    )];
    let opened = open_detail(cx, vec![queued.clone(), other.clone()], vec![]);
    let window = opened.window;
    assert_eq!(
        label_of(cx, window, QUEUED_REASON_ID).as_deref(),
        Some("Waiting for repos-integration-pane to be resolved.")
    );
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(QUEUED_REASON_ID).role(), Some(Role::Label));
    });
    assert!(!is_drawn(cx, window, SEND_ID));
    assert!(!is_drawn(cx, window, OPEN_CHAT_ID));
    assert!(!is_drawn(cx, window, RESOLVE_ID));

    // Once that intent is resolved, nothing it depends on is unresolved.
    let resolved = changed(&other, |i| i.state = IntentState::Resolved);
    upsert(cx, &opened, &resolved);
    assert_eq!(
        label_of(cx, window, QUEUED_REASON_ID).as_deref(),
        Some("Waiting for a free agent slot.")
    );
}

// ---- Open chat ---------------------------------------------------------------

#[gpui_kit::test]
fn open_chat_selects_the_agent_and_opens_its_tab(cx: &mut TestAppContext) {
    let venus = agent("Venus");
    let mercury = agent("Mercury");
    let sent = sent(&mercury);
    let opened = open_detail(cx, vec![sent], vec![venus, mercury.clone()]);
    let window = opened.window;
    let model = |cx: &mut TestAppContext| {
        opened.state.read_with(cx, |state, _| {
            let model = state.model();
            (model.selected(), model.active_tab())
        })
    };
    assert_eq!(model(cx), (None, None));

    click(cx, window, OPEN_CHAT_ID);
    assert_eq!(model(cx), (Some(mercury.id), Some(mercury.id)));
    assert!(calls(&opened).is_empty(), "no daemon command");
}

#[gpui_kit::test]
fn open_chat_is_disabled_when_the_agent_is_gone(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let sent = sent(&mercury);
    let opened = open_detail(cx, vec![sent], vec![]);
    let window = opened.window;
    assert!(disabled_button(cx, window, OPEN_CHAT_ID));
    // A button's tooltip is also its accessible description. (The tooltip's
    // own text is not observable headlessly: a managed tooltip registers no
    // test text, checked on an enabled button too.)
    with_window(cx, window, |window, _| {
        assert_eq!(
            window.find(OPEN_CHAT_ID).description(),
            Some("The agent was closed"),
            "the tooltip says why"
        );
    });

    click(cx, window, OPEN_CHAT_ID);
    let (selected, active) = opened.state.read_with(cx, |state, _| {
        (state.model().selected(), state.model().active_tab())
    });
    assert_eq!((selected, active), (None, None));
    assert!(
        enabled_button(cx, window, RESOLVE_ID),
        "it can still be resolved"
    );
}

// ---- Mark resolved -----------------------------------------------------------

#[gpui_kit::test]
fn mark_resolved_asks_first(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let opened = open_detail(cx, vec![sent(&mercury)], vec![mercury.clone()]);
    let window = opened.window;

    click(cx, window, RESOLVE_ID);
    assert!(dialog_open(cx, window));
    assert_eq!(
        label_of(cx, window, RESOLVE_TITLE_ID).as_deref(),
        Some("Mark as resolved?")
    );
    assert_eq!(
        label_of(cx, window, RESOLVE_DESCRIPTION_ID).as_deref(),
        Some(RESOLVE_DESCRIPTION)
    );
    assert_eq!(
        label_of(cx, window, RESOLVE_CANCEL_ID).as_deref(),
        Some("Cancel")
    );
    assert_eq!(
        label_of(cx, window, RESOLVE_CONFIRM_ID).as_deref(),
        Some("Mark resolved")
    );
    assert!(
        is_focused(cx, window, RESOLVE_CANCEL_ID),
        "Cancel is the default"
    );
    assert!(calls(&opened).is_empty(), "no call yet");

    // Cancel and Escape dismiss it.
    click(cx, window, RESOLVE_CANCEL_ID);
    assert!(!dialog_open(cx, window));
    click(cx, window, RESOLVE_ID);
    press(cx, window, "escape");
    assert!(!dialog_open(cx, window));
    assert!(calls(&opened).is_empty());
    assert_eq!(state_label(cx, window).as_deref(), Some("Sent"));
}

#[gpui_kit::test]
fn enter_in_the_dialog_cancels(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let opened = open_detail(cx, vec![sent(&mercury)], vec![mercury.clone()]);
    let window = opened.window;

    click(cx, window, RESOLVE_ID);
    press(cx, window, "enter");
    assert!(!dialog_open(cx, window), "Enter dismissed the dialog");
    assert!(calls(&opened).is_empty(), "no Resolve call");

    // Even with the focus on "Mark resolved".
    click(cx, window, RESOLVE_ID);
    press(cx, window, "tab");
    assert!(is_focused(cx, window, RESOLVE_CONFIRM_ID));
    press(cx, window, "enter");
    assert!(!dialog_open(cx, window));
    assert!(calls(&opened).is_empty());
    assert_eq!(state_label(cx, window).as_deref(), Some("Sent"));
}

#[gpui_kit::test]
fn confirming_resolves(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let sent = sent(&mercury);
    let opened = open_detail(cx, vec![sent.clone()], vec![mercury.clone()]);
    let window = opened.window;

    click(cx, window, RESOLVE_ID);
    click(cx, window, RESOLVE_CONFIRM_ID);
    assert!(!dialog_open(cx, window));
    assert_eq!(calls(&opened), [IntentCall::Resolve(sent.id)]);
    assert_eq!(busy(cx, &opened), Some(Busy::Resolve));
    assert!(disabled_button(cx, window, RESOLVE_ID));
    assert_eq!(state_label(cx, window).as_deref(), Some("Sent"));
    click(cx, window, RESOLVE_ID);
    assert!(!dialog_open(cx, window), "no second dialog while busy");
    assert_eq!(calls(&opened).len(), 1);

    let resolved = changed(&sent, |i| {
        i.state = IntentState::Resolved;
        i.resolved_at = Some(i.updated_at);
    });
    opened.fake.answer(Ok(resolved.clone()));
    upsert(cx, &opened, &resolved);
    assert_eq!(busy(cx, &opened), None);
    assert_eq!(state_label(cx, window).as_deref(), Some("Resolved"));
    assert!(no_bar(cx, window), "the bar is gone");
    assert_eq!(error_line(cx, window), None);
}

#[gpui_kit::test]
fn a_refused_resolve_is_explained(cx: &mut TestAppContext) {
    let mercury = agent("Mercury");
    let sent = sent(&mercury);
    let opened = open_detail(cx, vec![sent.clone()], vec![mercury.clone()]);
    let window = opened.window;
    opened.fake.queue(Err(FakeIntents::api_error(
        409,
        "invalid_state",
        "intent is not sent",
    )));
    click(cx, window, RESOLVE_ID);
    click(cx, window, RESOLVE_CONFIRM_ID);
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("The intent changed, so this is no longer possible.")
    );
    assert!(enabled_button(cx, window, RESOLVE_ID));
    assert_eq!(state_label(cx, window).as_deref(), Some("Sent"));
}

// ---- Start error -------------------------------------------------------------

#[gpui_kit::test]
fn a_start_error_is_shown_above_the_bar(cx: &mut TestAppContext) {
    let mut complete = complete();
    complete.last_start_error = Some("no directory at /moved/repo".to_owned());
    let opened = open_detail(cx, vec![complete.clone()], vec![]);
    let window = opened.window;
    let line = "The agent could not start: no directory at /moved/repo";
    with_window(cx, window, |window, _| {
        let error = window.find(START_ERROR_ID);
        assert_eq!(error.role(), Some(Role::Status));
        assert_eq!(error.label(), Some(line));
        let send = window.find(SEND_ID);
        assert!(
            error.bounds().bottom() <= send.bounds().top(),
            "{:?} is above {:?}",
            error.bounds(),
            send.bounds()
        );
    });

    // In every state, with or without a bar.
    let states = [
        (IntentState::Draft, Phase::Review),
        (IntentState::Queued, Phase::Review),
        (IntentState::Sent, Phase::Review),
        (IntentState::Resolved, Phase::Review),
    ];
    let mut current = complete;
    for (state, phase) in states {
        current = changed(&current, |i| {
            i.state = state;
            i.phase = phase;
        });
        upsert(cx, &opened, &current);
        assert_eq!(
            label_of(cx, window, START_ERROR_ID).as_deref(),
            Some(line),
            "{state:?}"
        );
    }

    // Cleared by the daemon: gone.
    let cleared = changed(&current, |i| i.last_start_error = None);
    upsert(cx, &opened, &cleared);
    assert!(!is_drawn(cx, window, START_ERROR_ID));
}

#[gpui_kit::test]
fn a_send_refused_with_a_start_error_shows_one_line(cx: &mut TestAppContext) {
    // The daemon refuses the Send with its error and records the same error
    // as the intent's start error: the start-error line says it once.
    const MESSAGE: &str = "git worktree add failed: no such directory";
    let complete = complete();
    let opened = open_detail(cx, vec![complete.clone()], vec![]);
    let window = opened.window;
    let start_error = format!("could not start the agent: {MESSAGE}");
    let line = format!("The agent could not start: {MESSAGE}");
    let failed = changed(&complete, |i| {
        i.last_start_error = Some(start_error.clone())
    });

    // The daemon's answer first, then its copy.
    opened
        .fake
        .queue(Err(FakeIntents::api_error(500, "git_error", MESSAGE)));
    click(cx, window, SEND_ID);
    assert_eq!(busy(cx, &opened), None);
    upsert(cx, &opened, &failed);
    assert_eq!(
        label_of(cx, window, START_ERROR_ID).as_deref(),
        Some(line.as_str())
    );
    assert_eq!(error_line(cx, window), None, "not said twice");

    // The copy first, then the answer (here the agent already attached,
    // whose message is the start error itself).
    let attached = "agent Mercury is already attached to this intent; close it to start again";
    let again = changed(&failed, |i| i.last_start_error = Some(attached.to_owned()));
    click(cx, window, SEND_ID);
    upsert(cx, &opened, &again);
    opened
        .fake
        .answer(Err(FakeIntents::api_error(400, "invalid", attached)));
    frame(cx, window);
    assert_eq!(busy(cx, &opened), None);
    assert_eq!(
        label_of(cx, window, START_ERROR_ID).as_deref(),
        Some(format!("The agent could not start: {attached}").as_str())
    );
    assert_eq!(error_line(cx, window), None, "not said twice");

    // A refusal the start error does not explain still has its own line.
    opened.fake.queue(Err(FakeIntents::api_error(
        409,
        "invalid_state",
        "intent is queued, expected complete",
    )));
    click(cx, window, SEND_ID);
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("The intent changed, so this is no longer possible.")
    );
    assert!(is_drawn(cx, window, START_ERROR_ID));
}

#[gpui_kit::test]
fn a_long_start_error_stays_inside_the_pane(cx: &mut TestAppContext) {
    // A path with no space to break at, 2,000 characters long.
    let long = format!("/moved/{}", "a".repeat(1993));
    let mut complete = complete();
    complete.last_start_error = Some(long);
    let opened = open_detail(cx, vec![complete], vec![]);
    with_window(cx, opened.window, |window, _| {
        let pane = window.find(INTENTS_PANEL).bounds();
        let line = window.find(START_ERROR_ID);
        let label = line.label().unwrap();
        assert!(label.starts_with("The agent could not start: /moved/aaa"));
        assert!(label.ends_with('…'), "cut");
        for (what, bounds) in [
            ("line", line.bounds()),
            ("Send", window.find(SEND_ID).bounds()),
        ] {
            assert!(
                bounds.left() >= pane.left()
                    && bounds.right() <= pane.right()
                    && bounds.bottom() <= pane.bottom(),
                "the {what} {bounds:?} is inside the pane {pane:?}"
            );
        }
        // The text breaks inside the path onto several lines (the line's
        // bounds keep the pane's width either way; text that does not wrap
        // runs past them on one line).
        assert!(
            line.bounds().size.height > px(48.),
            "the text wraps: {:?}",
            line.bounds()
        );
    });
}

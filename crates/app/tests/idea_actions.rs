//! Headless checks of an idea intent's actions in the detail: "Open pull
//! request…" (with its confirmation, whose Enter cancels), the pull
//! request's address once there is one, the "Not done" line, and "Create
//! implementation intent". Feedback commands go to `FakeFeedback` and intent
//! commands to `FakeIntents`, so no daemon runs; the model changes only when
//! the test dispatches the daemon's copy.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::rc::Rc;

use common::feedback_submit::{idea_intent, FakeFeedback, FeedbackCall};
use common::intents::{fixed_clock, intent, model_with, FakeIntents};
use common::ui::{agent, fake_connection, open_shell_options};
use gpui_kit::base::Root;
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::{ElementSnapshot, TestWindowExt as _};
use gpui_kit::{AppContext as _, ElementId, Entity, Role, TestAppContext, WindowHandle};
use smithagents_app::feedback::text::PR_DESCRIPTION;
use smithagents_app::model::Action;
use smithagents_app::shell::{Shell, ShellOptions};
use smithagents_app::shell_state::ShellState;
use smithagents_app::views::intents::{
    intent_card_id, Busy, IntentsPane, Screen, DETAIL_BACK_ID, IMPLEMENT_ID, INTENT_ERROR_ID,
    NOT_DONE_ID, OPEN_CHAT_ID, OPEN_PR_CANCEL_ID, OPEN_PR_CONFIRM_ID, OPEN_PR_DESCRIPTION_ID,
    OPEN_PR_ID, OPEN_PR_TITLE_ID, PR_COPY_ID, PR_URL_ID, RESOLVE_ID,
};
use smithagents_app::views::rail::RailEntry;
use smithagents_core::agent::{Agent, AgentState};
use smithagents_core::ids::IntentId;
use smithagents_core::intent::{Intent, IntentState, Phase};

const PR_URL: &str = "https://github.com/smithagents-test-invalid/widgets/pull/8";

/// The shell with the Intents pane on screen.
struct Opened {
    window: WindowHandle<Root>,
    state: Entity<ShellState>,
    pane: Entity<IntentsPane>,
    feedback: Rc<FakeFeedback>,
}

/// Opens the shell, connected, with these intents and agents and (`with`)
/// feedback commands, shows the Intents list and opens the first intent's
/// detail.
fn open_detail_with(
    cx: &mut TestAppContext,
    intents: Vec<Intent>,
    agents: Vec<Agent>,
    with_feedback: bool,
) -> Opened {
    let first = intents[0].id;
    let feedback = Rc::new(FakeFeedback::default());
    let (window, shell) = open_shell_options(
        cx,
        ShellOptions {
            model: model_with(intents, agents, vec![]),
            intent_commands: Some(Rc::new(FakeIntents::default())),
            feedback_commands: with_feedback.then(|| feedback.clone() as _),
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
        feedback,
    };
    show(cx, &opened, first);
    opened
}

fn open_detail(cx: &mut TestAppContext, intents: Vec<Intent>, agents: Vec<Agent>) -> Opened {
    open_detail_with(cx, intents, agents, true)
}

/// Opens intent `id`'s detail from the list.
fn show(cx: &mut TestAppContext, opened: &Opened, id: IntentId) {
    if screen(cx, opened) != Screen::List {
        click(cx, opened.window, DETAIL_BACK_ID);
    }
    click(cx, opened.window, intent_card_id(id));
    assert_eq!(screen(cx, opened), Screen::Detail(id));
}

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

fn enabled_button(cx: &mut TestAppContext, window: WindowHandle<Root>, id: &'static str) -> bool {
    with_window(cx, window, |window, _| {
        window
            .try_find(id)
            .is_some_and(|el| el.role() == Some(Role::Button) && el.disabled() != Some(true))
    })
}

fn disabled_button(cx: &mut TestAppContext, window: WindowHandle<Root>, id: &'static str) -> bool {
    with_window(cx, window, |window, _| {
        window
            .try_find(id)
            .is_some_and(|el| el.role() == Some(Role::Button) && el.disabled() == Some(true))
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

/// `intent` as the daemon has it with this pull request (or "Not done").
fn with_pr(intent: &Intent, url: &str) -> Intent {
    changed(intent, |i| {
        let idea = i.origin.as_mut().unwrap().idea.as_mut().unwrap();
        idea.pr_url = Some(url.to_owned());
    })
}

fn calls(opened: &Opened) -> Vec<FeedbackCall> {
    opened.feedback.calls.borrow().clone()
}

fn error_line(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Option<String> {
    with_window(cx, window, |window, _| {
        window.try_find(INTENT_ERROR_ID).map(|line| {
            assert_eq!(line.role(), Some(Role::Status));
            line.label().unwrap().to_owned()
        })
    })
}

/// Neither idea action is drawn.
fn no_idea_actions(cx: &mut TestAppContext, window: WindowHandle<Root>) -> bool {
    [OPEN_PR_ID, PR_URL_ID, IMPLEMENT_ID, NOT_DONE_ID]
        .into_iter()
        .all(|id| !is_drawn(cx, window, id))
}

/// An idea intent sent and started as `agent`, which is in `state`.
fn sent_idea(state: AgentState) -> (Intent, Agent) {
    let mut agent = agent("Mercury");
    agent.state = state;
    let mut idea = idea_intent(IntentState::Sent, None);
    idea.agent_id = Some(agent.id);
    (idea, agent)
}

// ---- Which intents offer them ------------------------------------------------

#[gpui_kit::test]
fn a_resolved_idea_intent_offers_both_buttons(cx: &mut TestAppContext) {
    let resolved = idea_intent(IntentState::Resolved, None);
    let (done, done_agent) = sent_idea(AgentState::Done);
    let (working, working_agent) = sent_idea(AgentState::Working);
    let mut working = working;
    working.slug = "still-working".into();
    let mut done = done;
    done.slug = "agent-done".into();
    let plain = intent("plain-one", IntentState::Resolved, Phase::Review);
    let opened = open_detail(
        cx,
        vec![
            resolved.clone(),
            done.clone(),
            working.clone(),
            plain.clone(),
        ],
        vec![done_agent, working_agent],
    );
    let window = opened.window;

    // Resolved: a bar of its own.
    assert_eq!(
        label_of(cx, window, OPEN_PR_ID).as_deref(),
        Some("Open pull request…")
    );
    assert!(enabled_button(cx, window, OPEN_PR_ID));
    assert_eq!(
        label_of(cx, window, IMPLEMENT_ID).as_deref(),
        Some("Create implementation intent")
    );
    assert!(enabled_button(cx, window, IMPLEMENT_ID));
    assert!(!is_drawn(cx, window, PR_URL_ID));
    assert!(!is_drawn(cx, window, NOT_DONE_ID));

    // Sent, its agent done: beside Open chat and Mark resolved.
    show(cx, &opened, done.id);
    assert!(enabled_button(cx, window, OPEN_PR_ID));
    assert!(enabled_button(cx, window, IMPLEMENT_ID));
    assert!(is_drawn(cx, window, OPEN_CHAT_ID));
    assert!(is_drawn(cx, window, RESOLVE_ID));

    // Sent, its agent still working; or a plain intent: neither.
    for other in [&working, &plain] {
        show(cx, &opened, other.id);
        assert!(no_idea_actions(cx, window), "{}", other.slug);
    }
    assert!(calls(&opened).is_empty());
}

#[gpui_kit::test]
fn without_feedback_commands_the_buttons_are_disabled(cx: &mut TestAppContext) {
    let resolved = idea_intent(IntentState::Resolved, None);
    let opened = open_detail_with(cx, vec![resolved], vec![], false);
    let window = opened.window;
    assert!(disabled_button(cx, window, OPEN_PR_ID));
    assert!(disabled_button(cx, window, IMPLEMENT_ID));
    click(cx, window, OPEN_PR_ID);
    assert!(!dialog_open(cx, window));
    click(cx, window, IMPLEMENT_ID);
    assert_eq!(busy(cx, &opened), None);
}

// ---- Open pull request -------------------------------------------------------

#[gpui_kit::test]
fn open_pull_request_asks_first(cx: &mut TestAppContext) {
    let resolved = idea_intent(IntentState::Resolved, None);
    let opened = open_detail(cx, vec![resolved.clone()], vec![]);
    let window = opened.window;

    click(cx, window, OPEN_PR_ID);
    assert!(dialog_open(cx, window));
    assert_eq!(
        label_of(cx, window, OPEN_PR_TITLE_ID).as_deref(),
        Some("Open a pull request?")
    );
    assert_eq!(
        label_of(cx, window, OPEN_PR_DESCRIPTION_ID).as_deref(),
        Some(PR_DESCRIPTION)
    );
    assert_eq!(
        PR_DESCRIPTION,
        "Pushes the idea branch to origin and opens a draft pull request. If the repository is public, the screenshots will be visible to anyone."
    );
    assert_eq!(
        label_of(cx, window, OPEN_PR_CANCEL_ID).as_deref(),
        Some("Cancel")
    );
    assert_eq!(
        label_of(cx, window, OPEN_PR_CONFIRM_ID).as_deref(),
        Some("Open pull request")
    );
    assert!(
        is_focused(cx, window, OPEN_PR_CANCEL_ID),
        "Cancel is the default"
    );

    // Enter cancels, even with the focus on the confirm button.
    press(cx, window, "enter");
    assert!(!dialog_open(cx, window));
    click(cx, window, OPEN_PR_ID);
    press(cx, window, "tab");
    assert!(is_focused(cx, window, OPEN_PR_CONFIRM_ID));
    press(cx, window, "enter");
    assert!(!dialog_open(cx, window));
    assert!(calls(&opened).is_empty(), "no call yet");

    click(cx, window, OPEN_PR_ID);
    click(cx, window, OPEN_PR_CONFIRM_ID);
    assert!(!dialog_open(cx, window));
    assert_eq!(calls(&opened), [FeedbackCall::OpenPullRequest(resolved.id)]);
    assert_eq!(
        busy(cx, &opened),
        Some(Busy::PullRequest),
        "it shows loading"
    );
    assert!(disabled_button(cx, window, OPEN_PR_ID));
    assert!(disabled_button(cx, window, IMPLEMENT_ID));

    let answered = with_pr(&resolved, PR_URL);
    opened.feedback.answer_intent(Ok(answered.clone()));
    frame(cx, window);
    assert_eq!(busy(cx, &opened), None);
    assert!(
        is_drawn(cx, window, OPEN_PR_ID),
        "the view waits for the model"
    );

    upsert(cx, &opened, &answered);
    assert_eq!(label_of(cx, window, PR_URL_ID).as_deref(), Some(PR_URL));
    assert!(!is_drawn(cx, window, OPEN_PR_ID));
    assert!(enabled_button(cx, window, IMPLEMENT_ID));
    assert_eq!(error_line(cx, window), None);
}

#[gpui_kit::test]
fn a_second_press_while_busy_sends_nothing(cx: &mut TestAppContext) {
    // Review Focus 5.
    let resolved = idea_intent(IntentState::Resolved, None);
    let opened = open_detail(cx, vec![resolved.clone()], vec![]);
    let window = opened.window;

    click(cx, window, OPEN_PR_ID);
    click(cx, window, OPEN_PR_CONFIRM_ID);
    assert_eq!(calls(&opened).len(), 1);

    click(cx, window, OPEN_PR_ID);
    assert!(!dialog_open(cx, window), "no second dialog while busy");
    click(cx, window, IMPLEMENT_ID);
    with_window(cx, window, |window, cx| {
        window.double_click(IMPLEMENT_ID, cx)
    });
    frame(cx, window);
    assert_eq!(calls(&opened), [FeedbackCall::OpenPullRequest(resolved.id)]);
    assert_eq!(busy(cx, &opened), Some(Busy::PullRequest));
}

#[gpui_kit::test]
fn a_refused_pull_request_shows_the_daemons_sentence(cx: &mut TestAppContext) {
    let resolved = idea_intent(IntentState::Resolved, None);
    let opened = open_detail(cx, vec![resolved.clone()], vec![]);
    let window = opened.window;
    opened.feedback.queue_intent(Err(FakeIntents::api_error(
        409,
        "gh_not_ready",
        "GitHub CLI (gh) is not signed in; run: gh auth login",
    )));

    click(cx, window, OPEN_PR_ID);
    click(cx, window, OPEN_PR_CONFIRM_ID);

    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("GitHub CLI (gh) is not signed in; run: gh auth login")
    );
    assert_eq!(busy(cx, &opened), None);
    assert!(enabled_button(cx, window, OPEN_PR_ID), "enabled again");

    // A failed step's sentence, capitalized.
    opened.feedback.queue_intent(Err(FakeIntents::api_error(
        502,
        "push_rejected",
        "git could not push to origin: the remote rejected the branch",
    )));
    click(cx, window, OPEN_PR_ID);
    click(cx, window, OPEN_PR_CONFIRM_ID);
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("Git could not push to origin: the remote rejected the branch")
    );
}

#[gpui_kit::test]
fn only_a_github_pull_request_address_is_shown_and_opened(cx: &mut TestAppContext) {
    let shown = idea_intent(IntentState::Resolved, Some(PR_URL));
    let mut odd = idea_intent(IntentState::Resolved, Some("javascript:alert(1)"));
    odd.slug = "odd-one".into();
    let opened = open_detail(cx, vec![shown.clone(), odd.clone()], vec![]);
    let window = opened.window;

    assert_eq!(label_of(cx, window, PR_URL_ID).as_deref(), Some(PR_URL));
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(PR_URL_ID).role(), Some(Role::Label));
    });
    // A right click opens nothing; a click does.
    with_window(cx, window, |window, cx| window.right_click(PR_URL_ID, cx));
    frame(cx, window);
    assert_eq!(cx.opened_url(), None);
    click(cx, window, PR_URL_ID);
    assert_eq!(cx.opened_url().as_deref(), Some(PR_URL));
    // "Copy link" puts the address on the clipboard.
    assert!(enabled_button(cx, window, PR_COPY_ID));
    click(cx, window, PR_COPY_ID);
    assert_eq!(
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .as_deref(),
        Some(PR_URL)
    );
    assert!(calls(&opened).is_empty());

    // A stored address that is not one is neither shown nor opened, and
    // offers no second pull request either.
    show(cx, &opened, odd.id);
    assert!(!is_drawn(cx, window, PR_URL_ID));
    assert!(!is_drawn(cx, window, PR_COPY_ID));
    assert!(!is_drawn(cx, window, OPEN_PR_ID));
    assert!(enabled_button(cx, window, IMPLEMENT_ID));
}

// ---- Not done ----------------------------------------------------------------

#[gpui_kit::test]
fn the_not_done_line_is_repeated(cx: &mut TestAppContext) {
    let mut resolved = idea_intent(IntentState::Resolved, Some(PR_URL));
    resolved
        .origin
        .as_mut()
        .unwrap()
        .idea
        .as_mut()
        .unwrap()
        .not_done = Some("The project did not start.".to_owned());
    let opened = open_detail(cx, vec![resolved], vec![]);
    let window = opened.window;
    assert_eq!(
        label_of(cx, window, NOT_DONE_ID).as_deref(),
        Some("Not done: The project did not start.")
    );
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(NOT_DONE_ID).role(), Some(Role::Status));
    });
}

// ---- Create implementation intent -------------------------------------------

#[gpui_kit::test]
fn create_implementation_intent_opens_the_new_intent(cx: &mut TestAppContext) {
    let resolved = idea_intent(IntentState::Resolved, Some(PR_URL));
    let opened = open_detail(cx, vec![resolved.clone()], vec![]);
    let window = opened.window;

    click(cx, window, IMPLEMENT_ID);
    assert_eq!(calls(&opened), [FeedbackCall::Implement(resolved.id)]);
    assert_eq!(busy(cx, &opened), Some(Busy::Implementation));
    assert!(disabled_button(cx, window, IMPLEMENT_ID));
    assert_eq!(screen(cx, &opened), Screen::Detail(resolved.id));

    let created = intent(
        "implement-zoom",
        IntentState::Draft,
        Phase::Polishing { pass: 1 },
    );
    opened.feedback.answer_intent(Ok(created.clone()));
    frame(cx, window);
    assert_eq!(busy(cx, &opened), None);
    assert_eq!(screen(cx, &opened), Screen::Detail(created.id));
    assert_eq!(error_line(cx, window), None);
}

#[gpui_kit::test]
fn a_refused_implementation_intent_stays_on_the_idea(cx: &mut TestAppContext) {
    let resolved = idea_intent(IntentState::Resolved, None);
    let opened = open_detail(cx, vec![resolved.clone()], vec![]);
    let window = opened.window;
    opened.feedback.queue_intent(Err(FakeIntents::api_error(
        409,
        "not_an_idea",
        "this intent is not an idea package",
    )));
    click(cx, window, IMPLEMENT_ID);
    assert_eq!(screen(cx, &opened), Screen::Detail(resolved.id));
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("This intent is not an idea package")
    );
    assert!(enabled_button(cx, window, IMPLEMENT_ID));
}

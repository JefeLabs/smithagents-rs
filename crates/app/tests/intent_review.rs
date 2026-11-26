//! Headless checks of the detail's review sections: the five collapsible
//! sections (title, counter, status, open or closed, the user's toggle kept
//! for the pane's lifetime); the Questions section (choosing an option,
//! skipping, one answer at a time, the daemon's refusals, read-only once the
//! questions phase is over); the item sections (Confirm, Edit and Skip, the
//! "n skipped" line, the accepted outline, one command at a time, read-only
//! outside review) and Depends on (the other intent's slug and title).
//! Commands go to `FakeIntents`, so no daemon runs; the model changes only
//! when the test dispatches the daemon's copy.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::rc::Rc;

use common::intents::{fixed_clock, intent, item, model_with, question, FakeIntents, IntentCall};
use common::ui::{dark_native, open_shell_options, quad_at};
use gpui_kit::base::Root;
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::test::{ElementSnapshot, TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{
    point, px, AppContext as _, ElementId, Entity, Hsla, Role, TestAppContext, WindowHandle,
};
use smithagents_app::intent_vm::SectionKind;
use smithagents_app::model::Action;
use smithagents_app::shell::{Shell, ShellOptions, INTENTS_PANEL};
use smithagents_app::shell_state::ShellState;
use smithagents_app::theme_adapter::srgb_to_hsla;
use smithagents_app::views::intents::{
    intent_card_id, item_card_id, item_confirm_id, item_edit_id, item_input_id, item_skip_id,
    question_card_id, question_option_id, question_skip_id, section_counter_id, section_header_id,
    section_status_id, skipped_line_id, Busy, IntentsPane, Screen, CARD_SLUG_ID, DETAIL_BACK_ID,
    INTENT_ERROR_ID, ITEM_TEXT_ID, POLISHING_ID, QUESTION_ANSWER_ID,
};
use smithagents_app::views::rail::RailEntry;
use smithagents_core::ids::{IntentId, ItemId};
use smithagents_core::intent::{
    Answer, AnswerInput, Intent, IntentState, Item, ItemStatus, Phase, Question, ReviewAction,
};

/// The shell with the Intents pane on screen.
struct Opened {
    window: WindowHandle<Root>,
    state: Entity<ShellState>,
    pane: Entity<IntentsPane>,
    fake: Rc<FakeIntents>,
}

/// Opens the shell, connected, with these intents, shows the Intents list
/// and opens the first intent's detail.
fn open_detail(cx: &mut TestAppContext, intents: Vec<Intent>) -> Opened {
    let first = intents[0].id;
    let fake = Rc::new(FakeIntents::default());
    let (window, shell) = open_shell_options(
        cx,
        ShellOptions {
            model: model_with(intents, vec![], vec![]),
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
    click(cx, window, intent_card_id(first));
    let opened = Opened {
        window,
        state,
        pane,
        fake,
    };
    assert_eq!(screen(cx, &opened), Screen::Detail(first));
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

/// `intent` with question `index` answered.
fn answered(intent: &Intent, index: usize, answer: Answer) -> Intent {
    changed(intent, |i| i.content.questions[index].answer = answer)
}

fn option(id: &str) -> Answer {
    Answer::Option {
        option_id: id.to_owned(),
    }
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

/// A section's header as drawn: its counter, its status and whether it is
/// expanded. `None` when the section is not drawn.
fn header(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    kind: SectionKind,
) -> Option<(String, String, Option<bool>)> {
    with_window(cx, window, |window, _| {
        let header = window.try_find(section_header_id(kind))?;
        assert_eq!(header.role(), Some(Role::Button), "{kind:?}");
        let counter = window.find(section_counter_id(kind));
        let status = window.find(section_status_id(kind));
        Some((
            text(&counter).unwrap(),
            text(&status).unwrap(),
            header.expanded(),
        ))
    })
}

fn expanded(cx: &mut TestAppContext, window: WindowHandle<Root>, kind: SectionKind) -> bool {
    header(cx, window, kind).expect("the section is drawn").2 == Some(true)
}

/// What a question's card shows under its text ("→ Yes"), if anything.
fn answer_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    question: &Question,
) -> Option<String> {
    let card = question_card_id(question.id);
    with_window(cx, window, |window, _| {
        window
            .within(card)
            .try_find(QUESTION_ANSWER_ID)
            .as_ref()
            .and_then(text)
    })
}

/// A question's buttons that are drawn, as (id, label, disabled): one per
/// option, then Skip.
fn question_buttons(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    question: &Question,
) -> Vec<(ElementId, String, bool)> {
    let ids: Vec<ElementId> = question
        .options
        .iter()
        .map(|option| question_option_id(question.id, &option.id))
        .chain([question_skip_id(question.id)])
        .collect();
    with_window(cx, window, |window, _| {
        ids.into_iter()
            .filter_map(|id| {
                let button = window.try_find(id.clone())?;
                assert_eq!(button.role(), Some(Role::Button));
                let label = button.label().unwrap_or_default().to_owned();
                Some((id, label, button.disabled() == Some(true)))
            })
            .collect()
    })
}

/// The labels of a question's buttons that are drawn and enabled.
fn enabled_buttons(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    question: &Question,
) -> Vec<String> {
    question_buttons(cx, window, question)
        .into_iter()
        .filter(|(_, _, disabled)| !disabled)
        .map(|(_, label, _)| label)
        .collect()
}

/// A draft in the questions phase with two questions: "Reset the zoom on
/// reload?" (Yes, No) still open, and "Which pages?" (All, Only the browser)
/// answered "All".
fn in_questions() -> Intent {
    let mut draft = intent("fix-zoom", IntentState::Draft, Phase::Questions);
    draft.content.questions = vec![
        question(
            "Reset the zoom on reload?",
            &["Yes", "No"],
            Answer::Unanswered,
        ),
        question("Which pages?", &["All", "Only the browser"], option("o1")),
    ];
    draft
}

// ---- Section frame -----------------------------------------------------------

#[gpui_kit::test]
fn section_headers_show_counters_and_status(cx: &mut TestAppContext) {
    let draft = in_questions();
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;

    assert_eq!(
        header(cx, window, SectionKind::Questions),
        Some((
            "1 of 2 answered".to_owned(),
            "1 to answer".to_owned(),
            Some(true)
        ))
    );
    assert_eq!(
        header(cx, window, SectionKind::Preconditions),
        Some(("0 of 0 accepted".to_owned(), "none".to_owned(), Some(false)))
    );
    assert_eq!(
        header(cx, window, SectionKind::DependsOn),
        Some(("0 picked".to_owned(), "none".to_owned(), Some(false)))
    );
    with_window(cx, window, |window, _| {
        let header = window.find(section_header_id(SectionKind::Questions));
        assert_eq!(
            header.label(),
            Some("Questions, 1 of 2 answered, 1 to answer"),
            "the header's name carries its counter and status"
        );
        // Top to bottom in `SectionKind::ALL` order, under the body.
        let tops: Vec<_> = SectionKind::ALL
            .iter()
            .map(|kind| window.find(section_header_id(*kind)).bounds().origin.y)
            .collect();
        assert!(tops.windows(2).all(|pair| pair[0] < pair[1]), "{tops:?}");
    });

    // The open section shows a card per question; the closed ones nothing.
    for question in &draft.content.questions {
        assert!(is_drawn(cx, window, question_card_id(question.id)));
    }
    assert_eq!(answer_of(cx, window, &draft.content.questions[0]), None);
    assert_eq!(
        answer_of(cx, window, &draft.content.questions[1]).as_deref(),
        Some("→ All")
    );
}

#[gpui_kit::test]
fn a_done_section_starts_collapsed_and_the_toggle_is_remembered(cx: &mut TestAppContext) {
    let draft = answered(&in_questions(), 0, Answer::Skipped);
    // Another intent, all answered too: its section has its own toggle.
    let mut other = in_questions();
    other.slug = "other-intent".into();
    other.content.questions[0].answer = option("o2");
    let opened = open_detail(cx, vec![draft.clone(), other.clone()]);
    let window = opened.window;

    assert_eq!(
        header(cx, window, SectionKind::Questions),
        Some((
            "2 of 2 answered".to_owned(),
            "✓ done".to_owned(),
            Some(false)
        ))
    );
    let first = &draft.content.questions[0];
    assert!(!is_drawn(cx, window, question_card_id(first.id)));

    click(cx, window, section_header_id(SectionKind::Questions));
    assert!(expanded(cx, window, SectionKind::Questions));
    assert_eq!(answer_of(cx, window, first).as_deref(), Some("→ Skipped"));

    // Back to the list and in again: still open.
    click(cx, window, DETAIL_BACK_ID);
    assert_eq!(screen(cx, &opened), Screen::List);
    click(cx, window, intent_card_id(draft.id));
    assert!(expanded(cx, window, SectionKind::Questions));
    assert!(is_drawn(cx, window, question_card_id(first.id)));

    // The other intent's section starts closed.
    click(cx, window, DETAIL_BACK_ID);
    click(cx, window, intent_card_id(other.id));
    assert!(!expanded(cx, window, SectionKind::Questions));

    // Closing is remembered too.
    click(cx, window, DETAIL_BACK_ID);
    click(cx, window, intent_card_id(draft.id));
    click(cx, window, section_header_id(SectionKind::Questions));
    assert!(!expanded(cx, window, SectionKind::Questions));
    click(cx, window, DETAIL_BACK_ID);
    click(cx, window, intent_card_id(draft.id));
    assert!(!expanded(cx, window, SectionKind::Questions));
    assert!(calls(&opened).is_empty(), "toggling sends nothing");
}

#[gpui_kit::test]
fn a_failed_polish_draws_only_the_sections_with_items(cx: &mut TestAppContext) {
    let mut failed = in_questions();
    failed.content.questions[0].answer = Answer::Skipped;
    failed.content.criteria = vec![item("The zoom stays at 100%", ItemStatus::Pending)];
    failed.phase = Phase::Failed {
        pass: 2,
        message: "the harness timed out".into(),
    };
    let opened = open_detail(cx, vec![failed.clone()]);
    let window = opened.window;
    let drawn: Vec<SectionKind> = SectionKind::ALL
        .into_iter()
        .filter(|kind| is_drawn(cx, window, section_header_id(*kind)))
        .collect();
    assert_eq!(drawn, [SectionKind::Questions, SectionKind::Criteria]);

    // A first pass that failed has nothing to show.
    let empty = changed(&failed, |i| {
        i.content.questions.clear();
        i.content.criteria.clear();
        i.phase = Phase::Failed {
            pass: 1,
            message: "no".into(),
        };
    });
    upsert(cx, &opened, &empty);
    for kind in SectionKind::ALL {
        assert!(!is_drawn(cx, window, section_header_id(kind)), "{kind:?}");
    }
}

// ---- Questions ---------------------------------------------------------------

#[gpui_kit::test]
fn choosing_an_option_answers_the_question(cx: &mut TestAppContext) {
    let draft = in_questions();
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let first = &draft.content.questions[0];
    assert_eq!(
        enabled_buttons(cx, window, first),
        ["Yes", "No", "Skip"],
        "a button per option, then Skip"
    );
    assert!(
        question_buttons(cx, window, &draft.content.questions[1]).is_empty(),
        "an answered question has no buttons"
    );

    click(cx, window, question_option_id(first.id, "o1"));
    assert_eq!(
        calls(&opened),
        [IntentCall::Answer(
            draft.id,
            first.id,
            AnswerInput::Option("o1".into())
        )]
    );
    assert_eq!(busy(cx, &opened), Some(Busy::Question(first.id)));
    assert_eq!(
        question_buttons(cx, window, first).len(),
        3,
        "the buttons stay while the answer is on its way"
    );

    let answer = answered(&draft, 0, option("o1"));
    opened.fake.answer(Ok(answer.clone()));
    frame(cx, window);
    assert_eq!(busy(cx, &opened), None);
    assert_eq!(
        enabled_buttons(cx, window, first),
        ["Yes", "No", "Skip"],
        "nothing changes before the daemon's copy reaches the model"
    );
    assert_eq!(answer_of(cx, window, first), None);

    upsert(cx, &opened, &answer);
    assert_eq!(answer_of(cx, window, first).as_deref(), Some("→ Yes"));
    assert!(question_buttons(cx, window, first).is_empty());
    assert_eq!(
        header(cx, window, SectionKind::Questions),
        Some((
            "2 of 2 answered".to_owned(),
            "✓ done".to_owned(),
            Some(true)
        )),
        "the section the user answered in stays open"
    );
}

#[gpui_kit::test]
fn skip_shows_skipped(cx: &mut TestAppContext) {
    let draft = in_questions();
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let first = &draft.content.questions[0];
    assert_eq!(
        label_of(cx, window, question_skip_id(first.id)).as_deref(),
        Some("Skip")
    );

    click(cx, window, question_skip_id(first.id));
    assert_eq!(
        calls(&opened),
        [IntentCall::Answer(draft.id, first.id, AnswerInput::Skip)]
    );
    assert_eq!(busy(cx, &opened), Some(Busy::Question(first.id)));

    let skipped = answered(&draft, 0, Answer::Skipped);
    opened.fake.answer(Ok(skipped.clone()));
    upsert(cx, &opened, &skipped);
    assert_eq!(answer_of(cx, window, first).as_deref(), Some("→ Skipped"));
    assert!(question_buttons(cx, window, first).is_empty());
}

#[gpui_kit::test]
fn options_are_disabled_while_another_answer_is_in_flight(cx: &mut TestAppContext) {
    // Review Focus 3: one request per press, one command at a time.
    let mut draft = in_questions();
    draft.content.questions[1].answer = Answer::Unanswered;
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let (first, second) = (&draft.content.questions[0], &draft.content.questions[1]);

    click(cx, window, question_option_id(first.id, "o1"));
    assert_eq!(calls(&opened).len(), 1);
    assert_eq!(busy(cx, &opened), Some(Busy::Question(first.id)));
    let disabled = |buttons: Vec<(ElementId, String, bool)>| {
        buttons
            .iter()
            .map(|(_, _, disabled)| *disabled)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        disabled(question_buttons(cx, window, second)),
        [true, true, true]
    );
    assert_eq!(
        disabled(question_buttons(cx, window, first)),
        [true, true, true],
        "every control waits, the pressed one with the spinner"
    );

    click(cx, window, question_option_id(second.id, "o1"));
    click(cx, window, question_skip_id(second.id));
    click(cx, window, question_option_id(first.id, "o1"));
    click(cx, window, question_skip_id(first.id));
    assert_eq!(calls(&opened).len(), 1, "a press while busy sends nothing");

    // The answer frees them.
    opened.fake.answer(Ok(answered(&draft, 0, option("o1"))));
    frame(cx, window);
    assert_eq!(
        enabled_buttons(cx, window, second),
        ["All", "Only the browser", "Skip"]
    );
    click(cx, window, question_skip_id(second.id));
    assert_eq!(
        calls(&opened)[1],
        IntentCall::Answer(draft.id, second.id, AnswerInput::Skip)
    );
}

#[gpui_kit::test]
fn an_already_answered_question_says_so(cx: &mut TestAppContext) {
    // Review Focus 3: someone else answered it first.
    let draft = in_questions();
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let first = &draft.content.questions[0];
    opened.fake.queue(Err(FakeIntents::api_error(
        409,
        "already_answered",
        "question already answered",
    )));
    click(cx, window, question_option_id(first.id, "o1"));
    assert_eq!(calls(&opened).len(), 1);
    assert_eq!(busy(cx, &opened), None);
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("That question is already answered.")
    );

    // The daemon's copy shows the answer it has.
    upsert(cx, &opened, &answered(&draft, 0, option("o2")));
    assert_eq!(answer_of(cx, window, first).as_deref(), Some("→ No"));
    assert!(question_buttons(cx, window, first).is_empty());
}

#[gpui_kit::test]
fn a_refused_answer_says_why_and_the_daemons_copy_shows(cx: &mut TestAppContext) {
    // Review Focus 3: the intent moved on before the answer arrived.
    let draft = in_questions();
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let first = &draft.content.questions[0];

    opened.fake.queue(Err(FakeIntents::api_error(
        409,
        "invalid_state",
        "intent is not in the questions phase",
    )));
    click(cx, window, question_skip_id(first.id));
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("The intent changed, so this is no longer possible.")
    );
    assert_eq!(
        enabled_buttons(cx, window, first),
        ["Yes", "No", "Skip"],
        "nothing changed on screen: free to try again"
    );

    opened.fake.queue(Err(FakeIntents::api_error(
        409,
        "polishing",
        "intent is being polished",
    )));
    click(cx, window, question_option_id(first.id, "o2"));
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("The intent is still being polished.")
    );
    assert_eq!(calls(&opened).len(), 2);

    // The daemon's copy: polishing its second pass, so no sections.
    let polishing = changed(&draft, |i| i.phase = Phase::Polishing { pass: 2 });
    upsert(cx, &opened, &polishing);
    assert!(is_drawn(cx, window, POLISHING_ID));
    assert!(!is_drawn(cx, window, question_card_id(first.id)));
    assert!(!is_drawn(
        cx,
        window,
        section_header_id(SectionKind::Questions)
    ));
}

#[gpui_kit::test]
fn questions_are_read_only_after_the_questions_phase(cx: &mut TestAppContext) {
    // An open question in a sent intent (the daemon refuses answers outside
    // the questions phase): its text, and no buttons.
    let mut sent = in_questions();
    sent.state = IntentState::Sent;
    sent.phase = Phase::Review;
    let opened = open_detail(cx, vec![sent.clone()]);
    let window = opened.window;
    let (first, second) = (
        sent.content.questions[0].clone(),
        sent.content.questions[1].clone(),
    );
    assert!(expanded(cx, window, SectionKind::Questions));
    assert!(is_drawn(cx, window, question_card_id(first.id)));
    assert!(question_buttons(cx, window, &first).is_empty());
    assert_eq!(answer_of(cx, window, &first), None);
    assert_eq!(answer_of(cx, window, &second).as_deref(), Some("→ All"));

    // The same in every state and phase but a draft's questions phase.
    let cases = [
        (IntentState::Draft, Phase::Review),
        (
            IntentState::Draft,
            Phase::Failed {
                pass: 2,
                message: "no".into(),
            },
        ),
        (IntentState::Complete, Phase::Review),
        (IntentState::Queued, Phase::Review),
        (IntentState::Resolved, Phase::Review),
        (IntentState::Draft, Phase::Questions),
    ];
    let mut current = sent;
    for (state, phase) in cases {
        current = changed(&current, |i| {
            i.state = state;
            i.phase = phase.clone();
        });
        upsert(cx, &opened, &current);
        let offered = !question_buttons(cx, window, &first).is_empty();
        let questions = state == IntentState::Draft && phase == Phase::Questions;
        assert_eq!(offered, questions, "{state:?} {phase:?}");
        assert_eq!(answer_of(cx, window, &second).as_deref(), Some("→ All"));
    }
}

#[gpui_kit::test]
fn a_long_option_stays_inside_the_pane(cx: &mut TestAppContext) {
    let mut draft = in_questions();
    let long = "Only the browser pane, ".repeat(20);
    draft.content.questions[0].options[0].label = long.clone();
    let opened = open_detail(cx, vec![draft.clone()]);
    let first = &draft.content.questions[0];
    with_window(cx, opened.window, |window, _| {
        let pane = window.find(INTENTS_PANEL).bounds();
        let card = window.find(question_card_id(first.id)).bounds();
        let button = window.find(question_option_id(first.id, "o1"));
        assert_eq!(button.label(), Some(long.as_str()), "its name is whole");
        let button = button.bounds();
        assert!(card.right() <= pane.right(), "{card:?} in {pane:?}");
        assert!(button.right() <= card.right(), "{button:?} in {card:?}");
    });
}

// ---- Items -------------------------------------------------------------------

const PENDING_TEXT: &str = "The zoom resets on reload";
const ACCEPTED_TEXT: &str = "The page fits the pane";
const DROPPED_TEXT: &str = "Zoom is remembered per site";

/// A draft in review whose acceptance criteria are, in order, one pending
/// ([`PENDING_TEXT`]), one accepted ([`ACCEPTED_TEXT`]) and one dropped
/// ([`DROPPED_TEXT`]).
fn in_review() -> Intent {
    let mut draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    draft.content.criteria = vec![
        item(PENDING_TEXT, ItemStatus::Pending),
        item(ACCEPTED_TEXT, ItemStatus::Accepted),
        item(DROPPED_TEXT, ItemStatus::Dropped),
    ];
    draft
}

/// `intent` after the daemon changed its acceptance criterion `index`.
fn criterion(intent: &Intent, index: usize, change: impl FnOnce(&mut Item)) -> Intent {
    changed(intent, |i| change(&mut i.content.criteria[index]))
}

/// An item's review buttons that are drawn, as (label, disabled), in the
/// order Confirm, Edit, Skip.
fn item_buttons(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    item: ItemId,
) -> Vec<(String, bool)> {
    let ids = [
        item_confirm_id(item),
        item_edit_id(item),
        item_skip_id(item),
    ];
    with_window(cx, window, |window, _| {
        ids.into_iter()
            .filter_map(|id| {
                let button = window.try_find(id)?;
                assert_eq!(button.role(), Some(Role::Button));
                let label = button.label().unwrap_or_default().to_owned();
                Some((label, button.disabled() == Some(true)))
            })
            .collect()
    })
}

/// The labels of an item's review buttons that are drawn and enabled.
fn enabled_item_buttons(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    item: ItemId,
) -> Vec<String> {
    item_buttons(cx, window, item)
        .into_iter()
        .filter(|(_, disabled)| !disabled)
        .map(|(label, _)| label)
        .collect()
}

/// What the part `part` of item `item`'s card reads; `None` when the card
/// or the part is not drawn.
fn item_part(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    item: ItemId,
    part: &'static str,
) -> Option<String> {
    let card = item_card_id(item);
    with_window(cx, window, |window, _| {
        window.try_find(card.clone())?;
        window.within(card).try_find(part).as_ref().and_then(text)
    })
}

/// The text item `item`'s card shows; `None` when it is not drawn.
fn item_text(cx: &mut TestAppContext, window: WindowHandle<Root>, item: ItemId) -> Option<String> {
    item_part(cx, window, item, ITEM_TEXT_ID)
}

/// The input that edits item `item`, as drawn: what it holds and whether it
/// has the focus.
fn item_input(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    item: ItemId,
) -> Option<(String, bool)> {
    with_window(cx, window, |window, _| {
        window.try_find(item_input_id(item)).map(|input| {
            (
                input.value().unwrap_or_default().to_owned(),
                input.focused() == Some(true),
            )
        })
    })
}

/// Whether any element reads `wanted`, as its text or its name.
fn shows_text(cx: &mut TestAppContext, window: WindowHandle<Root>, wanted: &str) -> bool {
    with_window(cx, window, |window, _| {
        window
            .elements()
            .iter()
            .any(|element| text(element).as_deref() == Some(wanted))
    })
}

/// The color of item `item`'s outline, read just inside its card's left
/// edge.
fn outline(cx: &mut TestAppContext, window: WindowHandle<Root>, item: ItemId) -> Hsla {
    with_window(cx, window, |window, _| {
        let card = window.find(item_card_id(item)).bounds();
        quad_at(window, point(card.left() + px(0.5), card.center().y)).border_color
    })
}

/// A token's color in the dark theme the tests open the shell in.
fn token(name: &str) -> Hsla {
    srgb_to_hsla(dark_native().colors[name])
}

/// Clicks Edit on `item`, selects what the input holds and types `text`.
fn edit(cx: &mut TestAppContext, window: WindowHandle<Root>, item: ItemId, text: &str) {
    click(cx, window, item_edit_id(item));
    press(cx, window, "cmd-a");
    type_text(cx, window, text);
}

fn review(intent: &Intent, item: ItemId, action: ReviewAction) -> IntentCall {
    IntentCall::Review(intent.id, item, action)
}

fn edit_action(text: &str) -> ReviewAction {
    ReviewAction::Edit(text.to_owned())
}

#[gpui_kit::test]
fn a_pending_item_offers_confirm_edit_and_skip(cx: &mut TestAppContext) {
    let mut draft = in_review();
    draft.content.preconditions = vec![item("The browser pane is open", ItemStatus::Pending)];
    draft.content.stories = vec![item(
        "As a reader I see the whole page",
        ItemStatus::Pending,
    )];
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let [pending, accepted, dropped] = [0, 1, 2].map(|i| draft.content.criteria[i].id);

    assert_eq!(
        header(cx, window, SectionKind::Criteria),
        Some((
            "1 of 2 accepted".to_owned(),
            "1 to review".to_owned(),
            Some(true)
        ))
    );
    assert_eq!(
        item_text(cx, window, pending).as_deref(),
        Some(PENDING_TEXT)
    );
    assert_eq!(
        item_buttons(cx, window, pending),
        [
            ("Confirm".to_owned(), false),
            ("Edit".to_owned(), false),
            ("Skip".to_owned(), false)
        ]
    );
    with_window(cx, window, |window, _| {
        let card = window.find(item_card_id(pending));
        assert_eq!(card.role(), Some(Role::Group));
        assert_eq!(
            card.label(),
            Some(PENDING_TEXT),
            "the card is named by its text"
        );
    });
    assert_eq!(
        item_text(cx, window, accepted).as_deref(),
        Some(ACCEPTED_TEXT)
    );
    assert!(
        item_buttons(cx, window, accepted).is_empty(),
        "an accepted item has no buttons"
    );
    assert_eq!(outline(cx, window, accepted), token("status-ok"));
    assert_eq!(outline(cx, window, pending), token("border"));

    // A dropped item is not drawn; the line under the cards counts it.
    assert!(!is_drawn(cx, window, item_card_id(dropped)));
    assert!(!shows_text(cx, window, DROPPED_TEXT));
    assert_eq!(
        label_of(cx, window, skipped_line_id(SectionKind::Criteria)).as_deref(),
        Some("1 skipped")
    );
    assert!(
        !is_drawn(cx, window, skipped_line_id(SectionKind::Preconditions)),
        "nothing skipped there"
    );

    // In the daemon's order.
    with_window(cx, window, |window, _| {
        let top = |id: ItemId| window.find(item_card_id(id)).bounds().origin.y;
        assert!(top(pending) < top(accepted));
    });

    // Preconditions and Stories offer the same.
    for (kind, item) in [
        (SectionKind::Preconditions, &draft.content.preconditions[0]),
        (SectionKind::Stories, &draft.content.stories[0]),
    ] {
        assert!(expanded(cx, window, kind), "{kind:?}");
        assert_eq!(item_text(cx, window, item.id), Some(item.text.clone()));
        assert_eq!(
            enabled_item_buttons(cx, window, item.id),
            ["Confirm", "Edit", "Skip"],
            "{kind:?}"
        );
    }
    assert!(calls(&opened).is_empty());
}

#[gpui_kit::test]
fn confirm_accepts_the_item(cx: &mut TestAppContext) {
    let draft = in_review();
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let pending = draft.content.criteria[0].id;
    assert_eq!(
        label_of(cx, window, item_confirm_id(pending)).as_deref(),
        Some("Confirm")
    );

    click(cx, window, item_confirm_id(pending));
    assert_eq!(
        calls(&opened),
        [review(&draft, pending, ReviewAction::Accept)]
    );
    assert_eq!(busy(cx, &opened), Some(Busy::Item(pending)));
    assert_eq!(
        item_buttons(cx, window, pending).len(),
        3,
        "the buttons stay while the answer is on its way"
    );

    let accepted = criterion(&draft, 0, |i| i.status = ItemStatus::Accepted);
    opened.fake.answer(Ok(accepted.clone()));
    frame(cx, window);
    assert_eq!(busy(cx, &opened), None);
    assert_eq!(
        enabled_item_buttons(cx, window, pending),
        ["Confirm", "Edit", "Skip"],
        "nothing changes before the daemon's copy reaches the model"
    );

    upsert(cx, &opened, &accepted);
    assert!(item_buttons(cx, window, pending).is_empty());
    assert_eq!(outline(cx, window, pending), token("status-ok"));
    assert_eq!(
        header(cx, window, SectionKind::Criteria),
        Some((
            "2 of 2 accepted".to_owned(),
            "✓ done".to_owned(),
            Some(true)
        )),
        "the section the user confirmed in stays open"
    );
}

#[gpui_kit::test]
fn skip_drops_the_item(cx: &mut TestAppContext) {
    let draft = in_review();
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let pending = draft.content.criteria[0].id;
    assert_eq!(
        label_of(cx, window, item_skip_id(pending)).as_deref(),
        Some("Skip")
    );

    click(cx, window, item_skip_id(pending));
    assert_eq!(
        calls(&opened),
        [review(&draft, pending, ReviewAction::Drop)]
    );
    assert_eq!(busy(cx, &opened), Some(Busy::Item(pending)));

    let dropped = criterion(&draft, 0, |i| i.status = ItemStatus::Dropped);
    opened.fake.answer(Ok(dropped.clone()));
    upsert(cx, &opened, &dropped);
    assert!(
        !is_drawn(cx, window, item_card_id(pending)),
        "a skipped item is not drawn"
    );
    assert!(!shows_text(cx, window, PENDING_TEXT));
    assert_eq!(
        label_of(cx, window, skipped_line_id(SectionKind::Criteria)).as_deref(),
        Some("2 skipped")
    );
    assert_eq!(
        header(cx, window, SectionKind::Criteria),
        Some((
            "1 of 1 accepted".to_owned(),
            "✓ done".to_owned(),
            Some(true)
        ))
    );
}

#[gpui_kit::test]
fn edit_saves_on_enter_as_edit_and_accept(cx: &mut TestAppContext) {
    let draft = in_review();
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let pending = draft.content.criteria[0].id;
    assert_eq!(
        label_of(cx, window, item_edit_id(pending)).as_deref(),
        Some("Edit")
    );

    click(cx, window, item_edit_id(pending));
    assert_eq!(
        item_input(cx, window, pending),
        Some((PENDING_TEXT.to_owned(), true)),
        "the input holds the item's text and has the focus"
    );
    assert_eq!(
        item_text(cx, window, pending),
        None,
        "the input replaces the text"
    );
    with_window(cx, window, |window, _| {
        let input = window
            .within(item_card_id(pending))
            .try_find(item_input_id(pending));
        assert!(input.is_some(), "the input is in the item's card");
    });
    assert!(
        item_buttons(cx, window, pending).is_empty(),
        "Enter saves and Escape cancels"
    );

    press(cx, window, "cmd-a");
    type_text(cx, window, "  Better wording ");
    press(cx, window, "enter");
    assert_eq!(
        calls(&opened),
        [review(&draft, pending, edit_action("Better wording"))],
        "trimmed; the daemon edits and accepts"
    );
    assert_eq!(busy(cx, &opened), Some(Busy::Item(pending)));
    press(cx, window, "enter");
    assert_eq!(calls(&opened).len(), 1, "a second Enter sends nothing");
    assert!(
        item_input(cx, window, pending).is_some(),
        "the input stays until the answer"
    );

    let edited = criterion(&draft, 0, |i| {
        i.text = "Better wording".into();
        i.edited = true;
        i.status = ItemStatus::Accepted;
    });
    opened.fake.answer(Ok(edited.clone()));
    frame(cx, window);
    assert_eq!(
        item_input(cx, window, pending),
        None,
        "the answer closes it"
    );
    assert_eq!(
        item_text(cx, window, pending).as_deref(),
        Some(PENDING_TEXT),
        "the model has not changed yet"
    );

    upsert(cx, &opened, &edited);
    assert_eq!(
        item_text(cx, window, pending).as_deref(),
        Some("Better wording")
    );
    assert!(item_buttons(cx, window, pending).is_empty());
    assert_eq!(
        header(cx, window, SectionKind::Criteria),
        Some((
            "2 of 2 accepted".to_owned(),
            "✓ done".to_owned(),
            Some(true)
        ))
    );
}

#[gpui_kit::test]
fn a_refused_edit_keeps_the_text(cx: &mut TestAppContext) {
    // A failed save never loses what the user typed.
    let draft = in_review();
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let pending = draft.content.criteria[0].id;
    opened.fake.queue(Err(FakeIntents::api_error(
        400,
        "invalid",
        "text must be 1 to 2000 characters",
    )));
    edit(cx, window, pending, "Better wording");
    press(cx, window, "enter");
    assert_eq!(busy(cx, &opened), None);
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("Text must be 1 to 2000 characters")
    );
    assert_eq!(
        item_input(cx, window, pending),
        Some(("Better wording".to_owned(), true)),
        "the text and the focus stay, to try again"
    );

    // Enter tries again.
    press(cx, window, "enter");
    assert_eq!(
        calls(&opened),
        [
            review(&draft, pending, edit_action("Better wording")),
            review(&draft, pending, edit_action("Better wording"))
        ]
    );
    assert_eq!(error_line(cx, window), None, "the next command clears it");
}

#[gpui_kit::test]
fn escape_and_blank_text_cancel_an_edit(cx: &mut TestAppContext) {
    let draft = in_review();
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let pending = draft.content.criteria[0].id;

    edit(cx, window, pending, "Something else");
    press(cx, window, "escape");
    assert_eq!(item_input(cx, window, pending), None);
    assert_eq!(
        item_text(cx, window, pending).as_deref(),
        Some(PENDING_TEXT)
    );
    with_window(cx, window, |window, _| {
        assert_eq!(
            window.find(item_edit_id(pending)).focused(),
            Some(true),
            "a keyboard user is back on Edit"
        );
    });

    // Enter on Edit opens the input again, with the item's text.
    press(cx, window, "enter");
    assert_eq!(
        item_input(cx, window, pending),
        Some((PENDING_TEXT.to_owned(), true))
    );

    // Only spaces: Enter closes it.
    press(cx, window, "cmd-a");
    type_text(cx, window, "   ");
    press(cx, window, "enter");
    assert_eq!(item_input(cx, window, pending), None);
    assert_eq!(
        item_text(cx, window, pending).as_deref(),
        Some(PENDING_TEXT)
    );
    assert_eq!(
        enabled_item_buttons(cx, window, pending),
        ["Confirm", "Edit", "Skip"]
    );
    assert!(
        calls(&opened).is_empty(),
        "no call for Escape or blank text"
    );
    assert_eq!(error_line(cx, window), None);
}

#[gpui_kit::test]
fn one_item_is_edited_at_a_time(cx: &mut TestAppContext) {
    let mut draft = in_review();
    draft.content.criteria[1].status = ItemStatus::Pending;
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let (first, second) = (draft.content.criteria[0].id, draft.content.criteria[1].id);

    edit(cx, window, first, "Half typed");
    click(cx, window, item_edit_id(second));
    assert_eq!(item_input(cx, window, first), None);
    assert_eq!(item_text(cx, window, first).as_deref(), Some(PENDING_TEXT));
    assert_eq!(
        enabled_item_buttons(cx, window, first),
        ["Confirm", "Edit", "Skip"]
    );
    assert_eq!(
        item_input(cx, window, second),
        Some((ACCEPTED_TEXT.to_owned(), true))
    );
    assert!(calls(&opened).is_empty());
}

#[gpui_kit::test]
fn an_open_edit_closes_when_the_item_can_no_longer_be_reviewed(cx: &mut TestAppContext) {
    let draft = in_review();
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let pending = draft.content.criteria[0].id;

    // Sent meanwhile: the item is read-only.
    edit(cx, window, pending, "Half typed");
    let queued = changed(&draft, |i| i.state = IntentState::Queued);
    upsert(cx, &opened, &queued);
    assert_eq!(item_input(cx, window, pending), None);
    assert_eq!(
        item_text(cx, window, pending).as_deref(),
        Some(PENDING_TEXT)
    );
    assert!(item_buttons(cx, window, pending).is_empty());

    // Reviewable again: the text and the buttons, not a stale input.
    let complete = changed(&queued, |i| i.state = IntentState::Complete);
    upsert(cx, &opened, &complete);
    assert_eq!(item_input(cx, window, pending), None);
    assert_eq!(
        enabled_item_buttons(cx, window, pending),
        ["Confirm", "Edit", "Skip"]
    );

    // Skipped by someone else: the card goes, and the input with it.
    edit(cx, window, pending, "Half typed");
    let dropped = criterion(&complete, 0, |i| i.status = ItemStatus::Dropped);
    upsert(cx, &opened, &dropped);
    assert_eq!(item_input(cx, window, pending), None);
    assert!(!is_drawn(cx, window, item_card_id(pending)));
    assert!(calls(&opened).is_empty());
}

#[gpui_kit::test]
fn a_second_press_while_busy_sends_nothing(cx: &mut TestAppContext) {
    // Review Focus 3: one request per press, one command at a time.
    let mut draft = in_review();
    draft.content.criteria[1].status = ItemStatus::Pending;
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let (a, b) = (draft.content.criteria[0].id, draft.content.criteria[1].id);

    click(cx, window, item_confirm_id(a));
    assert_eq!(calls(&opened).len(), 1);
    assert_eq!(
        busy(cx, &opened),
        Some(Busy::Item(a)),
        "A's Confirm shows the spinner"
    );
    let disabled = |buttons: Vec<(String, bool)>| {
        buttons
            .into_iter()
            .map(|(_, disabled)| disabled)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        disabled(item_buttons(cx, window, a)),
        [true; 3],
        "every control waits"
    );
    assert_eq!(disabled(item_buttons(cx, window, b)), [true; 3]);

    click(cx, window, item_confirm_id(b));
    click(cx, window, item_skip_id(a));
    click(cx, window, item_edit_id(b));
    click(cx, window, item_confirm_id(a));
    assert_eq!(calls(&opened).len(), 1, "a press while busy sends nothing");
    assert_eq!(item_input(cx, window, b), None, "nor opens an edit");

    // The answer frees them.
    opened.fake.answer(Ok(criterion(&draft, 0, |i| {
        i.status = ItemStatus::Accepted
    })));
    frame(cx, window);
    assert_eq!(
        enabled_item_buttons(cx, window, b),
        ["Confirm", "Edit", "Skip"]
    );
    click(cx, window, item_skip_id(b));
    assert_eq!(calls(&opened)[1], review(&draft, b, ReviewAction::Drop));
}

#[gpui_kit::test]
fn a_refusal_leaves_the_item_pending(cx: &mut TestAppContext) {
    // Review Focus 3: someone else changed the intent first.
    let draft = in_review();
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let pending = draft.content.criteria[0].id;

    opened.fake.queue(Err(FakeIntents::api_error(
        409,
        "polishing",
        "intent is being polished",
    )));
    click(cx, window, item_confirm_id(pending));
    assert_eq!(calls(&opened).len(), 1);
    assert_eq!(busy(cx, &opened), None);
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("The intent is still being polished.")
    );
    assert_eq!(
        item_text(cx, window, pending).as_deref(),
        Some(PENDING_TEXT)
    );
    assert_eq!(
        enabled_item_buttons(cx, window, pending),
        ["Confirm", "Edit", "Skip"],
        "nothing changed on screen: free to try again"
    );

    opened.fake.queue(Err(FakeIntents::api_error(
        409,
        "invalid_state",
        "intent is not in the review phase",
    )));
    click(cx, window, item_skip_id(pending));
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("The intent changed, so this is no longer possible.")
    );
    assert!(is_drawn(cx, window, item_card_id(pending)), "still pending");
    assert_eq!(calls(&opened).len(), 2);

    // The daemon's copy: sent meanwhile, so the item is read-only.
    let sent = changed(&draft, |i| i.state = IntentState::Sent);
    upsert(cx, &opened, &sent);
    assert_eq!(
        item_text(cx, window, pending).as_deref(),
        Some(PENDING_TEXT)
    );
    assert!(item_buttons(cx, window, pending).is_empty());
}

#[gpui_kit::test]
fn items_are_read_only_once_sent(cx: &mut TestAppContext) {
    let (mut sent, other) = with_dependency();
    sent.state = IntentState::Sent;
    sent.content.criteria = in_review().content.criteria;
    let opened = open_detail(cx, vec![sent.clone(), other]);
    let window = opened.window;
    let [pending, accepted] = [0, 1].map(|i| sent.content.criteria[i].id);
    let candidate = sent.content.depends_on[0].id;

    assert!(expanded(cx, window, SectionKind::Criteria));
    assert_eq!(
        item_text(cx, window, pending).as_deref(),
        Some(PENDING_TEXT)
    );
    assert!(item_buttons(cx, window, pending).is_empty());
    assert!(item_buttons(cx, window, candidate).is_empty());
    assert_eq!(
        item_text(cx, window, candidate).as_deref(),
        Some(DEPENDENCY_TITLE)
    );
    assert_eq!(
        outline(cx, window, accepted),
        token("status-ok"),
        "accepted outlines are still drawn"
    );
    assert_eq!(
        label_of(cx, window, skipped_line_id(SectionKind::Criteria)).as_deref(),
        Some("1 skipped")
    );

    // Offered only where the daemon takes a review: a draft in review, or a
    // complete intent.
    let cases = [
        (IntentState::Draft, Phase::Questions),
        (
            IntentState::Draft,
            Phase::Failed {
                pass: 2,
                message: "no".into(),
            },
        ),
        (IntentState::Queued, Phase::Review),
        (IntentState::Resolved, Phase::Review),
        (IntentState::Draft, Phase::Review),
        (IntentState::Complete, Phase::Review),
    ];
    let mut current = sent;
    for (state, phase) in cases {
        current = changed(&current, |i| {
            i.state = state;
            i.phase = phase.clone();
        });
        upsert(cx, &opened, &current);
        let reviewable = (state == IntentState::Draft && phase == Phase::Review)
            || state == IntentState::Complete;
        let offered = (
            !item_buttons(cx, window, pending).is_empty(),
            !item_buttons(cx, window, candidate).is_empty(),
        );
        assert_eq!(offered, (reviewable, reviewable), "{state:?} {phase:?}");
        assert_eq!(outline(cx, window, accepted), token("status-ok"));
    }
    assert!(calls(&opened).is_empty());
}

#[gpui_kit::test]
fn a_complete_intent_can_still_be_reviewed(cx: &mut TestAppContext) {
    let mut complete = intent("fix-zoom", IntentState::Complete, Phase::Review);
    complete.content.criteria = vec![item(ACCEPTED_TEXT, ItemStatus::Accepted)];
    let opened = open_detail(cx, vec![complete.clone()]);
    let window = opened.window;
    let accepted = complete.content.criteria[0].id;
    assert_eq!(
        header(cx, window, SectionKind::Criteria),
        Some((
            "1 of 1 accepted".to_owned(),
            "✓ done".to_owned(),
            Some(false)
        ))
    );
    click(cx, window, section_header_id(SectionKind::Criteria));
    assert_eq!(
        item_text(cx, window, accepted).as_deref(),
        Some(ACCEPTED_TEXT)
    );
    assert!(
        item_buttons(cx, window, accepted).is_empty(),
        "all accepted: nothing to review"
    );

    // A pending item arrives through an update: it can be reviewed.
    let added = changed(&complete, |i| {
        i.content
            .criteria
            .push(item(PENDING_TEXT, ItemStatus::Pending))
    });
    upsert(cx, &opened, &added);
    let pending = added.content.criteria[1].id;
    assert_eq!(
        enabled_item_buttons(cx, window, pending),
        ["Confirm", "Edit", "Skip"]
    );
    click(cx, window, item_confirm_id(pending));
    assert_eq!(
        calls(&opened),
        [review(&complete, pending, ReviewAction::Accept)]
    );
}

#[gpui_kit::test]
fn a_long_item_stays_inside_the_pane(cx: &mut TestAppContext) {
    let mut draft = in_review();
    let long = "The zoom resets on reload in every pane, ".repeat(20);
    draft.content.criteria[0].text = long.clone();
    let opened = open_detail(cx, vec![draft.clone()]);
    let pending = draft.content.criteria[0].id;
    assert_eq!(item_text(cx, opened.window, pending), Some(long));
    with_window(cx, opened.window, |window, _| {
        let pane = window.find(INTENTS_PANEL).bounds();
        let card = window.find(item_card_id(pending)).bounds();
        let text = window.within(item_card_id(pending)).find(ITEM_TEXT_ID);
        assert!(card.right() <= pane.right(), "{card:?} in {pane:?}");
        assert!(
            text.bounds().right() <= card.right(),
            "{:?} in {card:?}",
            text.bounds()
        );
    });
}

// ---- Depends on --------------------------------------------------------------

const DEPENDENCY_SLUG: &str = "browser-pane-zoom-bounds";
const DEPENDENCY_TITLE: &str = "Fix Browser page drawn too big";
const DEPENDENCY_REASON: &str = "Shares the zoom code";

/// A depends-on item: `on` is the intent it points at, `text` the reason.
fn dependency(text: &str, on: IntentId, status: ItemStatus) -> Item {
    Item {
        intent_ref: Some(on),
        ..item(text, status)
    }
}

/// A draft in review that may depend on another intent (its one depends-on
/// item, pending, gives [`DEPENDENCY_REASON`]), and that other intent:
/// [`DEPENDENCY_SLUG`], titled [`DEPENDENCY_TITLE`].
fn with_dependency() -> (Intent, Intent) {
    let mut other = intent(DEPENDENCY_SLUG, IntentState::Sent, Phase::Review);
    other.content.title = DEPENDENCY_TITLE.to_owned();
    let mut draft = intent("fix-zoom", IntentState::Draft, Phase::Review);
    draft.content.depends_on = vec![dependency(DEPENDENCY_REASON, other.id, ItemStatus::Pending)];
    (draft, other)
}

#[gpui_kit::test]
fn a_dependency_shows_the_other_intents_slug_and_title(cx: &mut TestAppContext) {
    let (mut draft, other) = with_dependency();
    draft.content.depends_on.push(dependency(
        "Uses the same pane",
        other.id,
        ItemStatus::Dropped,
    ));
    let opened = open_detail(cx, vec![draft.clone(), other]);
    let window = opened.window;
    let [candidate, dropped] = [0, 1].map(|i| draft.content.depends_on[i].id);

    assert_eq!(
        header(cx, window, SectionKind::DependsOn),
        Some(("0 picked".to_owned(), "1 to review".to_owned(), Some(true)))
    );
    assert_eq!(
        item_part(cx, window, candidate, CARD_SLUG_ID).as_deref(),
        Some(DEPENDENCY_SLUG)
    );
    assert_eq!(
        item_text(cx, window, candidate).as_deref(),
        Some(DEPENDENCY_TITLE)
    );
    with_window(cx, window, |window, _| {
        let card = window.find(item_card_id(candidate));
        assert_eq!(card.role(), Some(Role::Group));
        assert_eq!(
            card.label(),
            Some("browser-pane-zoom-bounds: Fix Browser page drawn too big")
        );
    });
    assert_eq!(
        item_buttons(cx, window, candidate),
        [("Confirm".to_owned(), false), ("Skip".to_owned(), false)],
        "Confirm and Skip; a dependency is not edited"
    );
    assert_eq!(outline(cx, window, candidate), token("border"));

    assert!(!is_drawn(cx, window, item_card_id(dropped)));
    assert_eq!(
        label_of(cx, window, skipped_line_id(SectionKind::DependsOn)).as_deref(),
        Some("1 skipped")
    );
}

#[gpui_kit::test]
fn confirming_a_dependency_picks_it(cx: &mut TestAppContext) {
    let (draft, other) = with_dependency();
    let opened = open_detail(cx, vec![draft.clone(), other]);
    let window = opened.window;
    let candidate = draft.content.depends_on[0].id;

    click(cx, window, item_confirm_id(candidate));
    assert_eq!(
        calls(&opened),
        [review(&draft, candidate, ReviewAction::Accept)]
    );
    assert_eq!(busy(cx, &opened), Some(Busy::Item(candidate)));

    let picked = changed(&draft, |i| {
        i.content.depends_on[0].status = ItemStatus::Accepted
    });
    opened.fake.answer(Ok(picked.clone()));
    upsert(cx, &opened, &picked);
    assert_eq!(
        header(cx, window, SectionKind::DependsOn),
        Some(("1 picked".to_owned(), "✓ done".to_owned(), Some(true)))
    );
    assert!(item_buttons(cx, window, candidate).is_empty());
    assert_eq!(outline(cx, window, candidate), token("status-ok"));
}

#[gpui_kit::test]
fn a_dependency_cycle_is_explained(cx: &mut TestAppContext) {
    // Review Focus 3: the other intent already depends on this one.
    let (draft, other) = with_dependency();
    let opened = open_detail(cx, vec![draft.clone(), other]);
    let window = opened.window;
    let candidate = draft.content.depends_on[0].id;
    opened.fake.queue(Err(FakeIntents::api_error(
        409,
        "dependency_cycle",
        "dependency would create a cycle",
    )));

    click(cx, window, item_confirm_id(candidate));
    assert_eq!(
        error_line(cx, window).as_deref(),
        Some("Those intents would depend on each other.")
    );
    assert_eq!(
        enabled_item_buttons(cx, window, candidate),
        ["Confirm", "Skip"],
        "still pending: it can be skipped"
    );
    click(cx, window, item_skip_id(candidate));
    assert_eq!(
        calls(&opened),
        [
            review(&draft, candidate, ReviewAction::Accept),
            review(&draft, candidate, ReviewAction::Drop)
        ]
    );
}

#[gpui_kit::test]
fn a_dependency_on_an_unknown_intent_shows_its_own_text(cx: &mut TestAppContext) {
    // The other intent is not in the model.
    let (draft, _) = with_dependency();
    let opened = open_detail(cx, vec![draft.clone()]);
    let window = opened.window;
    let candidate = draft.content.depends_on[0].id;

    assert_eq!(
        item_text(cx, window, candidate).as_deref(),
        Some(DEPENDENCY_REASON)
    );
    assert_eq!(item_part(cx, window, candidate, CARD_SLUG_ID), None);
    with_window(cx, window, |window, _| {
        assert_eq!(
            window.find(item_card_id(candidate)).label(),
            Some(DEPENDENCY_REASON)
        );
    });
    assert_eq!(
        enabled_item_buttons(cx, window, candidate),
        ["Confirm", "Skip"]
    );
}

//! Headless checks of the detail's review sections: the five collapsible
//! sections (title, counter, status, open or closed, the user's toggle kept
//! for the pane's lifetime) and the Questions section (choosing an option,
//! skipping, one answer at a time, the daemon's refusals, read-only once the
//! questions phase is over). Commands go to `FakeIntents`, so no daemon
//! runs; the model changes only when the test dispatches the daemon's copy.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::rc::Rc;

use common::intents::{fixed_clock, intent, item, model_with, question, FakeIntents, IntentCall};
use common::ui::open_shell_options;
use gpui_kit::base::Root;
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::test::{ElementSnapshot, TestWindowExt as _};
use gpui_kit::{AppContext as _, ElementId, Entity, Role, TestAppContext, WindowHandle};
use smithagents_app::intent_vm::SectionKind;
use smithagents_app::model::Action;
use smithagents_app::shell::{Shell, ShellOptions, INTENTS_PANEL};
use smithagents_app::shell_state::ShellState;
use smithagents_app::views::intents::{
    intent_card_id, question_card_id, question_option_id, question_skip_id, section_counter_id,
    section_header_id, section_status_id, Busy, IntentsPane, Screen, DETAIL_BACK_ID,
    INTENT_ERROR_ID, POLISHING_ID, QUESTION_ANSWER_ID,
};
use smithagents_app::views::rail::RailEntry;
use smithagents_core::intent::{
    Answer, AnswerInput, Intent, IntentState, ItemStatus, Phase, Question,
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

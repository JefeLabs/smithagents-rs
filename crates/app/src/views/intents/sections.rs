//! The detail's review sections under the body, in [`SectionKind::ALL`]
//! order: Questions, Preconditions, Acceptance criteria, Stories and Depends
//! on. After a failed polish pass only the sections that already have
//! questions or items are drawn.
//!
//! Each section is hand-made like the Settings sections: a full-width header
//! button (chevron, title, counter, and the status at the right, whose name
//! carries all three and which exposes whether it is expanded), then its
//! content while open. A section starts open while it has something to
//! answer or review ([`SectionVm::starts_open`]); a header click opens or
//! closes it, and that choice is kept per intent for the pane's lifetime. A
//! section the user answers in is kept open too, so the last answer does not
//! fold it away.
//!
//! Questions: one outlined card per question with its text. While the draft
//! is in its questions phase (the only time the daemon takes an answer) an
//! open question offers a button per option and Skip; a decided one shows its
//! answer ([`answer_text`]). Nothing changes on screen before the daemon's
//! copy reaches the model. The other four sections are filled by Task 9;
//! until then they draw their header only.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::{h_flex, v_flex, Disableable as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, AnyElement, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Role, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    Window,
};
use smithagents_core::ids::{IntentId, QuestionId};
use smithagents_core::intent::{Answer, AnswerInput, Intent, IntentState, Phase, Question};

use super::{Busy, IntentsPane, Paint};
use crate::intent_vm::{answer_text, section_items, section_vm, SectionKind, SectionVm};

/// Element id of a decided question's answer ("→ Yes"), inside its card
/// ([`question_card_id`]).
pub const QUESTION_ANSWER_ID: &str = "answer";

const SKIP_LABEL: &str = "Skip";

/// Element id of a section's header button (expands and collapses it).
pub fn section_header_id(kind: SectionKind) -> ElementId {
    ElementId::Name(format!("intent-section-{}", kind.as_str()).into())
}

/// Element id of a section's counter ("1 of 2 answered").
pub fn section_counter_id(kind: SectionKind) -> ElementId {
    ElementId::Name(format!("intent-counter-{}", kind.as_str()).into())
}

/// Element id of a section's status ("1 to answer", "✓ done", "none").
pub fn section_status_id(kind: SectionKind) -> ElementId {
    ElementId::Name(format!("intent-status-{}", kind.as_str()).into())
}

/// Element id of a question's card.
pub fn question_card_id(id: QuestionId) -> ElementId {
    ElementId::Name(format!("intent-question-{id}").into())
}

/// Element id of the button that answers `question` with the option whose id
/// is `option`.
pub fn question_option_id(question: QuestionId, option: &str) -> ElementId {
    ElementId::Name(format!("intent-option-{question}-{option}").into())
}

/// Element id of a question's Skip button.
pub fn question_skip_id(question: QuestionId) -> ElementId {
    ElementId::Name(format!("intent-question-skip-{question}").into())
}

/// Element id of a section (a labeled group).
fn section_group_id(kind: SectionKind) -> ElementId {
    ElementId::Name(format!("intent-group-{}", kind.as_str()).into())
}

/// Whether the section has any question or item, dropped ones included.
fn has_entries(intent: &Intent, kind: SectionKind) -> bool {
    match kind {
        SectionKind::Questions => !intent.content.questions.is_empty(),
        _ => !section_items(&intent.content, kind).is_empty(),
    }
}

impl IntentsPane {
    /// The sections under the body, stacked as one part; nothing when no
    /// section is drawn.
    pub(super) fn review_sections(
        &self,
        intent: &Intent,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let failed =
            intent.state == IntentState::Draft && matches!(intent.phase, Phase::Failed { .. });
        let sections: Vec<AnyElement> = SectionKind::ALL
            .into_iter()
            .filter(|kind| !failed || has_entries(intent, *kind))
            .map(|kind| {
                let content = match kind {
                    SectionKind::Questions => self.questions(intent, paint, cx),
                    // Task 9 fills these.
                    SectionKind::Preconditions
                    | SectionKind::Criteria
                    | SectionKind::Stories
                    | SectionKind::DependsOn => div().into_any_element(),
                };
                self.section(intent, kind, content, paint, cx)
            })
            .collect();
        if sections.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .w_full()
                .border_t_1()
                .border_color(paint.border)
                .children(sections)
                .into_any_element(),
        )
    }

    /// One collapsible section: header (chevron, title, counter, status at
    /// the right), then `content` while open.
    pub(crate) fn section(
        &self,
        intent: &Intent,
        kind: SectionKind,
        content: AnyElement,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> AnyElement {
        let vm = section_vm(&intent.content, kind);
        let open = self.section_open(intent.id, kind, &vm);
        let id = intent.id;
        // "✓ done" and "none" read as settled; "n to answer" and "n to
        // review" wait for the user.
        let status_color = paint.token(if vm.pending == 0 {
            "status-ok"
        } else {
            "status-warn"
        });
        let name = format!("{}, {}, {}", vm.title, vm.counter, vm.status);
        let header = Button::new(section_header_id(kind))
            .ghost()
            .small()
            .w_full()
            .icon(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .text_color(paint.muted_foreground),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_1p5()
                    .child(
                        Label::new(vm.title)
                            .flex_none()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(paint.foreground),
                    )
                    .child(
                        Label::new(vm.counter)
                            .id(section_counter_id(kind))
                            .min_w_0()
                            .text_xs()
                            .text_color(paint.muted_foreground)
                            .truncate(),
                    )
                    .child(div().flex_1())
                    .child(
                        Label::new(vm.status)
                            .id(section_status_id(kind))
                            .flex_none()
                            .text_xs()
                            .text_color(status_color),
                    ),
            )
            .accessibility_label(name)
            .aria_expanded(open)
            .on_click(
                cx.listener(move |this, _, _, cx| this.set_section_open(id, kind, !open, cx)),
            );
        v_flex()
            .id(section_group_id(kind))
            .role(Role::Group)
            .test_support()
            .aria_label(kind.title())
            .flex_none()
            .w_full()
            .border_b_1()
            .border_color(paint.border)
            .child(header)
            .when(open, |this| {
                this.child(div().w_full().px_1().pt_1().pb_3().child(content))
            })
            .into_any_element()
    }

    /// Whether a section is open: as the user last left it, else
    /// [`SectionVm::starts_open`].
    pub(crate) fn section_open(&self, intent: IntentId, kind: SectionKind, vm: &SectionVm) -> bool {
        self.section_toggles
            .get(&(intent, kind))
            .copied()
            .unwrap_or(vm.starts_open)
    }

    fn set_section_open(
        &mut self,
        intent: IntentId,
        kind: SectionKind,
        open: bool,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!(%intent, ?kind, open, "intent section toggled");
        self.section_toggles.insert((intent, kind), open);
        cx.notify();
    }

    /// The Questions section's content: a card per question.
    fn questions(&self, intent: &Intent, paint: &Paint, cx: &Context<Self>) -> AnyElement {
        // The daemon takes an answer only in a draft's questions phase.
        let answerable = intent.state == IntentState::Draft && intent.phase == Phase::Questions;
        v_flex()
            .w_full()
            .gap_2()
            .children(
                intent
                    .content
                    .questions
                    .iter()
                    .map(|question| self.question_card(intent.id, question, answerable, paint, cx)),
            )
            .into_any_element()
    }

    /// A question's card: its text, then its answer once decided, or (with
    /// `answerable`) a button per option and Skip while it is open.
    fn question_card(
        &self,
        intent: IntentId,
        question: &Question,
        answerable: bool,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let open = question.answer == Answer::Unanswered;
        v_flex()
            .id(question_card_id(question.id))
            .role(Role::Group)
            .test_support()
            .aria_label(question.text.clone())
            .w_full()
            .gap_1p5()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(paint.border)
            .child(
                Label::new(question.text.clone())
                    .text_sm()
                    .text_color(paint.foreground),
            )
            .when_some(answer_text(question), |this, answer| {
                this.child(
                    Label::new(answer)
                        .id(QUESTION_ANSWER_ID)
                        .text_sm()
                        .text_color(paint.muted_foreground),
                )
            })
            .when(answerable && open, |this| {
                this.child(self.question_buttons(intent, question, cx))
            })
    }

    /// A button per option, then Skip. All are disabled while any command is
    /// in flight; the pressed one shows the spinner.
    fn question_buttons(
        &self,
        intent: IntentId,
        question: &Question,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let qid = question.id;
        let busy = self.busy.is_some();
        let pressed = self
            .pressed_answer
            .as_ref()
            .filter(|_| self.busy == Some(Busy::Question(qid)));
        let button = |id: ElementId, label: String, answer: AnswerInput| {
            let loading = pressed == Some(&answer);
            Button::new(id)
                .label(label)
                .small()
                // A long label is cut rather than widening the pane.
                .max_w_full()
                .disabled(busy)
                .loading(loading)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.answer_question(intent, qid, answer.clone(), window, cx)
                }))
        };
        h_flex()
            .w_full()
            .flex_wrap()
            .gap_1()
            .children(question.options.iter().map(|option| {
                button(
                    question_option_id(qid, &option.id),
                    option.label.clone(),
                    AnswerInput::Option(option.id.clone()),
                )
                .outline()
            }))
            .child(
                button(
                    question_skip_id(qid),
                    SKIP_LABEL.to_owned(),
                    AnswerInput::Skip,
                )
                .ghost(),
            )
    }

    /// Sends `answer` to `question`; nothing while another command is in
    /// flight. The Questions section stays open from here on.
    fn answer_question(
        &mut self,
        intent: IntentId,
        question: QuestionId,
        answer: AnswerInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        tracing::debug!(%intent, %question, ?answer, "answering a question");
        self.section_toggles
            .insert((intent, SectionKind::Questions), true);
        self.pressed_answer = Some(answer.clone());
        self.command(
            Busy::Question(question),
            move |commands| commands.answer_question(intent, question, answer),
            |_, _, _, _| {},
            window,
            cx,
        );
    }
}

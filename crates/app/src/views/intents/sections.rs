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
//! section the user answers or reviews in is kept open too, so the last
//! answer or item does not fold it away. Nothing changes on screen before the
//! daemon's copy reaches the model.
//!
//! Questions: one outlined card per question with its text. While the draft
//! is in its questions phase (the only time the daemon takes an answer) an
//! open question offers a button per option and Skip; a decided one shows its
//! answer ([`answer_text`]).
//!
//! Preconditions, Acceptance criteria and Stories: one outlined card per item
//! the daemon has not dropped, in its order; a dropped item is only counted,
//! in a muted "n skipped" line under the cards. An accepted card is outlined
//! in `status-ok`. While the intent is a draft in review or is complete (the
//! states the daemon takes a review in) a pending card offers Confirm, Edit
//! and Skip. Edit puts the text in a one-line input, for one item at a time:
//! Enter sends the trimmed text, which the daemon saves and accepts (the
//! text unchanged is sent as Confirm), and the input closes when it answers
//! (it stays, with what was typed, when it refuses), or once the item is no
//! longer pending; Escape, or Enter on blank text, closes it unchanged.
//! Depends on is drawn alike, but a card names the other intent (its slug and
//! title, or the item's own text while that intent is not in the model) and
//! offers no Edit.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Escape, Input, InputState};
use gpui_kit::component::label::Label;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Disableable as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, AnyElement, App, AppContext as _, Context, ElementId, Entity, Focusable as _,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
};
use smithagents_core::ids::{IntentId, ItemId, QuestionId};
use smithagents_core::intent::{
    Answer, AnswerInput, Intent, IntentState, Item, ItemStatus, Phase, Question, ReviewAction,
};

use super::{Busy, IntentsPane, Paint, CARD_SLUG_ID};
use crate::intent_vm::{
    answer_text, dependency_vm, section_items, section_vm, SectionKind, SectionVm,
};

/// Element id of a decided question's answer ("→ Yes"), inside its card
/// ([`question_card_id`]).
pub const QUESTION_ANSWER_ID: &str = "answer";
/// Element id of an item's text, inside its card ([`item_card_id`]); in a
/// Depends on card, the other intent's title (or the item's own text while
/// that intent is not in the model).
pub const ITEM_TEXT_ID: &str = "text";

const SKIP_LABEL: &str = "Skip";
const CONFIRM_LABEL: &str = "Confirm";
const EDIT_LABEL: &str = "Edit";
/// The item input's accessible name and placeholder.
const ITEM_INPUT_LABEL: &str = "Item text";

/// The item the item input edits: its intent, its section and its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ItemEdit {
    intent: IntentId,
    kind: SectionKind,
    item: ItemId,
}

/// The item input: one line, Enter saves.
pub(super) fn new_item_input(window: &mut Window, cx: &mut App) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder(ITEM_INPUT_LABEL))
}

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

/// Element id of an item's card.
pub fn item_card_id(id: ItemId) -> ElementId {
    ElementId::Name(format!("intent-item-{id}").into())
}

/// Element id of an item's Confirm button.
pub fn item_confirm_id(id: ItemId) -> ElementId {
    ElementId::Name(format!("intent-item-confirm-{id}").into())
}

/// Element id of an item's Edit button.
pub fn item_edit_id(id: ItemId) -> ElementId {
    ElementId::Name(format!("intent-item-edit-{id}").into())
}

/// Element id of an item's Skip button.
pub fn item_skip_id(id: ItemId) -> ElementId {
    ElementId::Name(format!("intent-item-skip-{id}").into())
}

/// Element id of the input that edits an item's text.
pub fn item_input_id(id: ItemId) -> ElementId {
    ElementId::Name(format!("intent-item-input-{id}").into())
}

/// Element id of a section's "n skipped" line.
pub fn skipped_line_id(kind: SectionKind) -> ElementId {
    ElementId::Name(format!("intent-skipped-{}", kind.as_str()).into())
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

/// Whether the daemon takes a review of `intent`'s items: a draft in its
/// review phase, or a complete intent.
fn reviewable(intent: &Intent) -> bool {
    match intent.state {
        IntentState::Draft => intent.phase == Phase::Review,
        IntentState::Complete => true,
        IntentState::Queued | IntentState::Sent | IntentState::Resolved => false,
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
                    SectionKind::Preconditions
                    | SectionKind::Criteria
                    | SectionKind::Stories
                    | SectionKind::DependsOn => self.items(intent, kind, paint, cx),
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

    /// An item section's content (Preconditions, Acceptance criteria,
    /// Stories or Depends on): a card per item not dropped, in the daemon's
    /// order, then "n skipped" when any are.
    fn items(
        &self,
        intent: &Intent,
        kind: SectionKind,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> AnyElement {
        let reviewable = reviewable(intent);
        let skipped = section_vm(&intent.content, kind).skipped;
        v_flex()
            .w_full()
            .gap_2()
            .children(
                section_items(&intent.content, kind)
                    .iter()
                    .filter(|item| item.status != ItemStatus::Dropped)
                    .map(|item| self.item_card(intent.id, kind, item, reviewable, paint, cx)),
            )
            .when(skipped > 0, |this| {
                this.child(
                    Label::new(format!("{skipped} skipped"))
                        .id(skipped_line_id(kind))
                        .text_xs()
                        .text_color(paint.muted_foreground),
                )
            })
            .into_any_element()
    }

    /// An item's card: its text (or the input, while it is edited), then,
    /// with `reviewable`, Confirm, Edit and Skip while it is pending. An
    /// accepted card is outlined in `status-ok`.
    fn item_card(
        &self,
        intent: IntentId,
        kind: SectionKind,
        item: &Item,
        reviewable: bool,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let editing = reviewable && self.item_edit.is_some_and(|edit| edit.item == item.id);
        let (name, text) = if kind == SectionKind::DependsOn {
            self.dependency_text(item, paint, cx)
        } else {
            let text = item_text(item.text.clone(), paint).into_any_element();
            (item.text.clone(), text)
        };
        let outline = if item.status == ItemStatus::Accepted {
            paint.token("status-ok")
        } else {
            paint.border
        };
        v_flex()
            .id(item_card_id(item.id))
            .role(Role::Group)
            .test_support()
            .aria_label(name)
            .w_full()
            .gap_1p5()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(outline)
            .map(|this| {
                if editing {
                    this.child(self.item_editor(item.id, paint, cx))
                } else {
                    this.child(text)
                }
            })
            .when(
                reviewable && item.status == ItemStatus::Pending && !editing,
                |this| this.child(self.item_buttons(intent, kind, item.id, cx)),
            )
    }

    /// A dependency's text: the other intent's slug, in a monospace tag, and
    /// its title; or the item's own text while that intent is not in the
    /// model. Also the card's name.
    fn dependency_text(
        &self,
        item: &Item,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> (String, AnyElement) {
        let vm = dependency_vm(self.state.read(cx).model(), item);
        let name = match &vm.slug {
            Some(slug) => format!("{slug}: {}", vm.title),
            None => vm.title.clone(),
        };
        let text = v_flex()
            .w_full()
            .items_start()
            .gap_1()
            .when_some(vm.slug, |this, slug| {
                this.child(
                    Tag::secondary()
                        .small()
                        .max_w_full()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(
                            Label::new(slug)
                                .id(CARD_SLUG_ID)
                                .min_w_0()
                                .line_height(rems(1.))
                                .text_color(paint.token("secondary-foreground"))
                                .truncate(),
                        ),
                )
            })
            .child(item_text(vm.title, paint))
            .into_any_element();
        (name, text)
    }

    /// The input that edits `item`'s text: Enter saves, Escape cancels. It
    /// shows the spinner while the edit is on its way.
    fn item_editor(&self, item: ItemId, paint: &Paint, cx: &Context<Self>) -> impl IntoElement {
        let saving = self.busy == Some(Busy::Item(item));
        let muted = paint.muted_foreground;
        // The input passes Escape on; it ends here.
        div()
            .w_full()
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.close_item_edit(window, cx)))
            .child(
                Input::new(&self.item_input)
                    .id(item_input_id(item))
                    .aria_label(ITEM_INPUT_LABEL)
                    .small()
                    .when(saving, |input| {
                        input.suffix(Spinner::new().xsmall().color(muted))
                    }),
            )
    }

    /// Confirm, Edit (not for a dependency) and Skip. All are disabled while
    /// any command is in flight; the pressed one shows the spinner.
    fn item_buttons(
        &self,
        intent: IntentId,
        kind: SectionKind,
        item: ItemId,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let busy = self.busy.is_some();
        let pressed = self
            .pressed_review
            .as_ref()
            .filter(|_| self.busy == Some(Busy::Item(item)));
        let button = |id: ElementId, label: &'static str, action: ReviewAction| {
            let loading = pressed == Some(&action);
            Button::new(id)
                .label(label)
                .small()
                .disabled(busy)
                .loading(loading)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.review_item(intent, kind, item, action.clone(), window, cx)
                }))
        };
        h_flex()
            .w_full()
            .flex_wrap()
            .gap_1()
            .child(button(item_confirm_id(item), CONFIRM_LABEL, ReviewAction::Accept).outline())
            .when(kind != SectionKind::DependsOn, |this| {
                this.child(
                    Button::new(item_edit_id(item))
                        .label(EDIT_LABEL)
                        .small()
                        .outline()
                        .disabled(busy)
                        // Where closing the input puts a keyboard user.
                        .when(self.item_edit_return == Some(item), |button| {
                            button.track_focus(&self.item_edit_focus)
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.edit_item(intent, kind, item, window, cx)
                        })),
                )
            })
            .child(button(item_skip_id(item), SKIP_LABEL, ReviewAction::Drop).ghost())
    }

    /// Sends `action` for `item`, of section `kind`; nothing while another
    /// command is in flight. The section stays open from here on. The item
    /// input that sent it closes when the daemon answers; when it refuses,
    /// the input keeps what was typed.
    fn review_item(
        &mut self,
        intent: IntentId,
        kind: SectionKind,
        item: ItemId,
        action: ReviewAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        // Sent from the item input (an edit, or its unchanged text).
        let from_input = self.item_edit.is_some_and(|open| open.item == item);
        // The edited text is the user's; the log names the action only.
        let what = match &action {
            ReviewAction::Accept => "accept",
            ReviewAction::Edit(_) => "edit",
            ReviewAction::Drop => "drop",
        };
        tracing::debug!(%intent, %item, ?kind, what, "reviewing an item");
        self.section_toggles.insert((intent, kind), true);
        self.pressed_review = Some(action.clone());
        self.command(
            Busy::Item(item),
            move |commands| commands.review_item(intent, item, action),
            move |this, _, window, cx| {
                if from_input && this.item_edit.is_some_and(|open| open.item == item) {
                    this.close_item_edit(window, cx);
                }
            },
            window,
            cx,
        );
    }

    /// Opens the item input on `item`, holding its text, with the focus; an
    /// input open on another item closes unsaved. Nothing while a command
    /// is in flight, or when the item cannot be reviewed.
    fn edit_item(
        &mut self,
        intent: IntentId,
        kind: SectionKind,
        item: ItemId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        let text = self
            .state
            .read(cx)
            .model()
            .intent(intent)
            .filter(|open| reviewable(open))
            .and_then(|open| {
                section_items(&open.content, kind)
                    .iter()
                    .find(|candidate| candidate.id == item)
            })
            .map(|found| found.text.clone());
        let Some(text) = text else {
            return;
        };
        self.item_edit = Some(ItemEdit { intent, kind, item });
        self.item_edit_return = Some(item);
        self.item_input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Enter in the item input: non-blank text (trimmed) is sent as an edit,
    /// which the daemon also accepts; the item's own text unchanged is sent
    /// as a confirm, so the daemon does not mark it edited; blank text just
    /// closes the input. Nothing while a command is in flight.
    pub(super) fn save_item_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.item_edit else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let text = self.item_input.read(cx).value().trim().to_owned();
        if text.is_empty() {
            self.close_item_edit(window, cx);
            return;
        }
        let unchanged = self
            .state
            .read(cx)
            .model()
            .intent(edit.intent)
            .and_then(|intent| {
                section_items(&intent.content, edit.kind)
                    .iter()
                    .find(|item| item.id == edit.item)
            })
            .is_some_and(|item| item.text == text);
        let action = if unchanged {
            ReviewAction::Accept
        } else {
            ReviewAction::Edit(text)
        };
        self.review_item(edit.intent, edit.kind, edit.item, action, window, cx);
    }

    /// Closes the item input. A keyboard user goes back to the item's Edit
    /// button.
    fn close_item_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.item_edit.take().is_none() {
            return;
        }
        if self.item_input.focus_handle(cx).is_focused(window) {
            self.item_edit_focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Closes the item input once its item can no longer be edited: the
    /// intent left the states the daemon takes a review in, or the item is
    /// gone or no longer pending (accepted or dropped, here or elsewhere). A
    /// keyboard user stays in the pane.
    pub(super) fn close_stale_item_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.item_edit else {
            return;
        };
        let editable = self
            .state
            .read(cx)
            .model()
            .intent(edit.intent)
            .is_some_and(|intent| {
                reviewable(intent)
                    && section_items(&intent.content, edit.kind)
                        .iter()
                        .any(|item| item.id == edit.item && item.status == ItemStatus::Pending)
            });
        if editable {
            return;
        }
        tracing::debug!(item = %edit.item, "the edited item can no longer be reviewed");
        self.item_edit = None;
        if self.item_input.focus_handle(cx).is_focused(window) {
            self.focus.focus(window, cx);
        }
    }
}

/// An item's text in its card.
fn item_text(text: String, paint: &Paint) -> Label {
    Label::new(text)
        .id(ITEM_TEXT_ID)
        .w_full()
        .text_sm()
        .text_color(paint.foreground)
}

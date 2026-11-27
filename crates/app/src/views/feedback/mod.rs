//! The Feedback view in the right dock, the rail's Feedback button
//! ([`hold`]) and the annotator the shell draws over the window
//! ([`annotator`]). The pane shares the dock with Settings and Intents and is
//! drawn only while the rail has it selected ([`ShellState::right_entry`]).
//!
//! The pane shows one [`FeedbackScreen`] at a time. The draft, top to
//! bottom: the heading; "How to submit feedback", collapsed until the user
//! opens it (not remembered); the notices, a status in the `destructive`
//! color, while there are any (at most [`NOTICES_MAX_HEIGHT`] tall, then
//! they scroll); the last submission's result, a status; one card per
//! attachment ([`card`]), or what to do when there is none; and the buttons.
//! "Capture" takes a picture of the window as it is, "Add files…" asks for
//! files, "Discard" asks first, and "Submit…" opens the Submit screen
//! ([`submit`]), once the draft has an attachment and the daemon is there.
//! While the draft is locked (its submission is under way or was sent), it
//! cannot be changed: Capture and Add files are disabled and the cards have
//! no remove button.
//!
//! The pane only shows the [`Feedback`] entity and calls it: the draft, the
//! disk, the capturer and the picker are all there. It does release the
//! thumbnails the entity retires: a thumbnail drawn here stays in the
//! window's sprite atlas until it is dropped.
//!
//! The pane paints its whole area from `Region::RightPane`, like Settings;
//! everything else comes from tokens.

mod annotator;
mod card;
mod hold;
mod submit;

pub use annotator::{
    badge_id, Annotator, ANNOTATOR_BAR_HEIGHT, ANNOTATOR_COMMENT_ERROR_ID, ANNOTATOR_COMMENT_ID,
    ANNOTATOR_CONFIRM_DISCARD_ID, ANNOTATOR_DELETE_ID, ANNOTATOR_DISCARD_ID,
    ANNOTATOR_DISCARD_TITLE_ID, ANNOTATOR_DONE_ID, ANNOTATOR_ID, ANNOTATOR_KEEP_ID,
    ANNOTATOR_PICTURE_ERROR_ID, ANNOTATOR_TITLE_ID,
};
pub use card::{card_id, card_remove_id, comment_badge_id};
pub use hold::{HoldButton, HoldEvent, HOLD_RING_ID};
pub use submit::{
    submit_kind_id, submit_repo_item_id, PUBLIC_CANCEL_ID, PUBLIC_CONFIRM_ID,
    PUBLIC_CONFIRM_TITLE_ID, PUBLIC_WARNING_ID, SEND_INTENT_ID, SEND_ISSUE_ID,
    SEND_ISSUE_REASON_ID, SUBMIT_BACK_ID, SUBMIT_CONFIRM_ID, SUBMIT_ERROR_ID, SUBMIT_IDEA_HINT_ID,
    SUBMIT_IDEA_ID, SUBMIT_IDEA_LABEL_ID, SUBMIT_KEY_CONTEXT, SUBMIT_REPO_ID, SUBMIT_RETRY_ID,
    SUBMIT_START_OVER_ID, SUBMIT_SUMMARY_ID,
};

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::assets::IconName as CatalogIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::input::Escape;
use gpui_kit::component::label::Label;
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, AnyElement, App, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Rems,
    Render, Role, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled,
    Subscription, TestSupportExt as _, Window,
};

use crate::feedback::draft::AttachmentId;
use crate::feedback::state::Feedback;
use crate::feedback::text::{
    discard_description, DISCARD_CANCEL, DISCARD_CONFIRM, DISCARD_TITLE, EMPTY_TEXT, HELP_LINES,
    HELP_TITLE, SUBMIT_EMPTY_TOOLTIP,
};
use crate::shell::FEEDBACK_PANEL;
use crate::shell_state::ShellState;
use crate::theme_adapter::{region_paint, token_paint, Region};
use crate::view_models::NOT_CONNECTED_TEXT;
use crate::views::pane_heading;
use crate::views::rail::RailEntry;

use card::cards;

/// Element id of the pane's heading.
pub const FEEDBACK_TITLE_ID: &str = "feedback-title";
/// Element id of the help row's header button (it exposes whether the help
/// is expanded).
pub const FEEDBACK_HELP_ID: &str = "feedback-help";
/// Element id of the help's sentences, while expanded.
pub const FEEDBACK_HELP_TEXT_ID: &str = "feedback-help-text";
/// Element id of what the pane says when the draft is empty.
pub const FEEDBACK_EMPTY_ID: &str = "feedback-empty";
/// Element id of the notices: a status named by the notices, one per line.
pub const FEEDBACK_NOTICES_ID: &str = "feedback-notices";
/// Element id of the list of cards.
pub const FEEDBACK_LIST_ID: &str = "feedback-attachments";
pub const FEEDBACK_ADD_FILES_ID: &str = "feedback-add-files";
pub const FEEDBACK_CAPTURE_ID: &str = "feedback-capture";
pub const FEEDBACK_DISCARD_ID: &str = "feedback-discard";
pub const FEEDBACK_SUBMIT_ID: &str = "feedback-submit";
/// Element id of the last submission's result, on the draft (a status).
pub const FEEDBACK_RESULT_ID: &str = "feedback-result";
/// Element ids in the dialog Discard opens.
pub const DISCARD_DIALOG_TITLE_ID: &str = "feedback-discard-title";
pub const DISCARD_CANCEL_ID: &str = "feedback-discard-cancel";
pub const DISCARD_CONFIRM_ID: &str = "feedback-discard-confirm";

const TITLE: &str = "Feedback";
const HEADING: &str = "FEEDBACK";
const LIST_LABEL: &str = "Attachments";
const ADD_FILES_LABEL: &str = "Add files…";
const CAPTURE_LABEL: &str = "Capture";
const DISCARD_LABEL: &str = "Discard";
const SUBMIT_LABEL: &str = "Submit…";

/// The most the notices take; more scroll inside, so the cards and the
/// buttons keep their room.
pub const NOTICES_MAX_HEIGHT: Rems = rems(10.);

/// Registers the pane's key bindings: Escape on the Submit screen
/// ([`SUBMIT_KEY_CONTEXT`]) goes back to the draft. A text field handles
/// Escape itself and passes it on.
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Escape, Some(SUBMIT_KEY_CONTEXT))]);
}

/// What the pane shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedbackScreen {
    /// The draft: its cards and the buttons.
    Draft,
    /// The Submit screen ([`submit`]).
    Submit,
}

pub struct FeedbackPane {
    state: Entity<ShellState>,
    feedback: Entity<Feedback>,
    focus: FocusHandle,
    screen: FeedbackScreen,
    /// The Submit screen's own state.
    submit: submit::Submit,
    /// "Submit…"'s focus, so a keyboard user gets it back when the Submit
    /// screen closes.
    submit_button_focus: FocusHandle,
    /// Whether "How to submit feedback" is expanded; it starts collapsed.
    help_open: bool,
    /// The cards' scroll position.
    scroll: ScrollHandle,
    /// One focus handle per screenshot card, so each is a tab stop (Enter
    /// or Space reopens the annotator).
    card_focus: HashMap<AttachmentId, FocusHandle>,
    _subscriptions: Vec<Subscription>,
}

impl FeedbackPane {
    pub fn new(
        state: Entity<ShellState>,
        feedback: Entity<Feedback>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The dock draws the pane from a cache: the theme reaches it through
        // the state, the draft through the feedback entity.
        let submit = submit::Submit::new(window, cx);
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            submit::follow_main_idea(&submit, window, cx),
            cx.observe_in(&feedback, window, |_, feedback, window, cx| {
                // Thumbnails whose cards are gone leave the window's atlas.
                // `Some(window)`: the window being updated is skipped otherwise.
                let retired = feedback.update(cx, |feedback, _| feedback.take_retired_images());
                for image in retired {
                    cx.drop_image(image, Some(window));
                }
                cx.notify();
            }),
        ];
        Self {
            state,
            feedback,
            focus: cx.focus_handle(),
            screen: FeedbackScreen::Draft,
            submit,
            submit_button_focus: cx.focus_handle().tab_stop(true),
            help_open: false,
            scroll: ScrollHandle::new(),
            card_focus: HashMap::new(),
            _subscriptions: subscriptions,
        }
    }

    pub fn screen(&self) -> &FeedbackScreen {
        &self.screen
    }

    /// "How to submit feedback": a header button, then the three sentences
    /// while expanded.
    fn help(&self, paint: &Paint, cx: &Context<Self>) -> impl IntoElement {
        let open = self.help_open;
        v_flex()
            .flex_none()
            .w_full()
            .border_b_1()
            .border_color(paint.border)
            .child(
                Button::new(FEEDBACK_HELP_ID)
                    .ghost()
                    .small()
                    .w_full()
                    .justify_start()
                    .icon(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .text_color(paint.muted_foreground),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(paint.foreground)
                            .child(HELP_TITLE),
                    )
                    .accessibility_label(HELP_TITLE)
                    .aria_expanded(open)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.help_open = !this.help_open;
                        cx.notify();
                    })),
            )
            .when(open, |this| {
                this.child(
                    v_flex()
                        .id(FEEDBACK_HELP_TEXT_ID)
                        .role(Role::Group)
                        .test_support()
                        .aria_label(HELP_TITLE)
                        .w_full()
                        .px_3()
                        .pt_1()
                        .pb_3()
                        .gap_1p5()
                        .text_xs()
                        .text_color(paint.muted_foreground)
                        .children(HELP_LINES.iter().enumerate().map(|(index, line)| {
                            sentence(ElementId::named_usize("feedback-help-line", index), *line)
                        })),
                )
            })
    }

    /// Add files and Capture on the left; Discard and Submit on the right.
    /// "Submit…" needs an attachment, no work running on the draft and the
    /// daemon; disabled, it says why (an empty draft, or no daemon).
    fn buttons(&self, empty: bool, cx: &Context<Self>) -> impl IntoElement {
        let (capturing, locked, busy, submitting) = {
            let feedback = self.feedback.read(cx);
            (
                feedback.capturing(),
                feedback.locked(),
                feedback.busy(),
                feedback.submitting(),
            )
        };
        let connected = self.state.read(cx).feedback_commands().is_some();
        let why_not = if empty {
            Some(SUBMIT_EMPTY_TOOLTIP)
        } else if !connected {
            Some(NOT_CONNECTED_TEXT)
        } else {
            None
        };
        let can_submit = why_not.is_none() && !busy;
        h_flex()
            .flex_none()
            .w_full()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .px_3()
            .py_2()
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new(FEEDBACK_ADD_FILES_ID)
                            .label(ADD_FILES_LABEL)
                            .outline()
                            .small()
                            .disabled(locked)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.feedback
                                    .update(cx, |feedback, cx| feedback.add_files(window, cx))
                            })),
                    )
                    .child(
                        Button::new(FEEDBACK_CAPTURE_ID)
                            .icon(Icon::new(CatalogIcon::Camera))
                            .label(CAPTURE_LABEL)
                            .outline()
                            .small()
                            .disabled(capturing || locked)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.feedback
                                    .update(cx, |feedback, cx| feedback.capture(window, cx))
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new(FEEDBACK_DISCARD_ID)
                            .label(DISCARD_LABEL)
                            .ghost()
                            .small()
                            .disabled(empty || submitting)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.ask_to_discard(window, cx)),
                            ),
                    )
                    .child(
                        Button::new(FEEDBACK_SUBMIT_ID)
                            .label(SUBMIT_LABEL)
                            .primary()
                            .small()
                            .track_focus(&self.submit_button_focus)
                            .disabled(!can_submit)
                            .when_some(why_not, |this, why| this.tooltip(why))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_submit(window, cx)),
                            ),
                    ),
            )
    }

    /// The last submission's result, a status, while there is one.
    fn result_line(&self, paint: &Paint, cx: &Context<Self>) -> Option<AnyElement> {
        let text = self.feedback.read(cx).result()?.to_owned();
        Some(
            div()
                .id(FEEDBACK_RESULT_ID)
                .role(Role::Status)
                .test_support()
                .aria_label(text.clone())
                .flex_none()
                .w_full()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(paint.foreground)
                .child(text)
                .into_any_element(),
        )
    }

    fn ask_to_discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.feedback.read(cx).draft().attachments.len();
        if count == 0 {
            return;
        }
        let feedback = self.feedback.downgrade();
        let ask = Confirmation {
            title_id: DISCARD_DIALOG_TITLE_ID,
            title: DISCARD_TITLE,
            description: discard_description(count),
            cancel_id: DISCARD_CANCEL_ID,
            cancel: DISCARD_CANCEL,
            confirm_id: DISCARD_CONFIRM_ID,
            confirm: DISCARD_CONFIRM,
        };
        confirm(
            ask,
            move |_, cx| {
                let _ = feedback.update(cx, |feedback, cx| feedback.discard_all(cx));
            },
            window,
            cx,
        );
    }
}

/// What a confirmation asks, and the element ids of its title and buttons.
struct Confirmation {
    title_id: &'static str,
    title: &'static str,
    description: String,
    cancel_id: &'static str,
    cancel: &'static str,
    confirm_id: &'static str,
    confirm: &'static str,
}

/// Asks before something that cannot be undone: the title and the
/// description, a cancel button (focused, and what Enter does) and a danger
/// confirm button, the only way to `on_confirm`. Either button closes the
/// dialog. Used by the pane's Discard and the annotator's.
fn confirm(
    ask: Confirmation,
    on_confirm: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let Confirmation {
        title_id,
        title,
        description,
        cancel_id,
        cancel,
        confirm_id,
        confirm,
    } = ask;
    let description = SharedString::from(description);
    let on_confirm = Rc::new(on_confirm);
    let cancel_focus = cx.focus_handle();
    // The builder runs again whenever the dialog layer rebuilds; focus only
    // on the first run, so a rebuild never steals the focus back.
    let first_build = Rc::new(Cell::new(true));
    window.open_alert_dialog(cx, move |alert, window, cx| {
        if first_build.replace(false) {
            cancel_focus.focus(window, cx);
        }
        let on_confirm = on_confirm.clone();
        alert
            .title(
                div()
                    .id(title_id)
                    .role(Role::Heading)
                    .test_support()
                    .aria_label(title)
                    .text_color(cx.theme().popover_foreground)
                    .child(title),
            )
            // A `Label`, so assistive technology reads it too.
            .description(Label::new(description.clone()).text_color(cx.theme().muted_foreground))
            // Enter dispatches the dialog's `Confirm`, which runs this: here it
            // dismisses, like the cancel button. Returning `true` closes the
            // dialog.
            .on_ok(|_, _, _| true)
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new(cancel_id)
                            .label(cancel)
                            .outline()
                            .track_focus(&cancel_focus)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(Button::new(confirm_id).label(confirm).danger().on_click(
                        move |_, window, cx| {
                            on_confirm(window, cx);
                            window.close_dialog(cx);
                        },
                    )),
            )
    });
}

/// A line of text that assistive technology reads as one sentence.
fn sentence(id: impl Into<ElementId>, text: impl Into<SharedString>) -> impl IntoElement + Styled {
    let text = text.into();
    div()
        .id(id)
        .role(Role::Paragraph)
        .test_support()
        .aria_label(text.clone())
        .child(text)
}

/// The notices, one per line, as a status that announces itself; `None`
/// when there are none. Taller than [`NOTICES_MAX_HEIGHT`], they scroll.
fn notices_block(notices: &[String], paint: &Paint) -> Option<AnyElement> {
    if notices.is_empty() {
        return None;
    }
    Some(
        v_flex()
            .id(FEEDBACK_NOTICES_ID)
            .role(Role::Status)
            .test_support()
            .aria_label(notices.join("\n"))
            .flex_none()
            .w_full()
            .max_h(NOTICES_MAX_HEIGHT)
            .overflow_y_scroll()
            .px_3()
            .py_2()
            .gap_1()
            .text_xs()
            .text_color(paint.destructive)
            // Each keeps its height: the block scrolls instead.
            .children(
                notices
                    .iter()
                    .map(|notice| div().flex_none().child(notice.clone())),
            )
            .into_any_element(),
    )
}

impl BasePanel for FeedbackPane {
    fn panel_name(&self) -> &'static str {
        FEEDBACK_PANEL
    }

    /// Drawn only while the rail has Feedback selected, so the dock's group
    /// has one visible panel and draws no tab bar.
    fn visible(&self, cx: &App) -> bool {
        self.state.read(cx).right_entry() == RailEntry::Feedback
    }

    /// The dock is locked to the shell's layout; its panels never close.
    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for FeedbackPane {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(TITLE.into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        TITLE
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }

    /// The pane draws its own heading.
    fn title_bar(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for FeedbackPane {}

impl Focusable for FeedbackPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FeedbackPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = Paint::new(self.state.read(cx), cx);
        let on_submit = self.screen == FeedbackScreen::Submit;
        let content = if on_submit {
            self.submit_screen(&paint, cx).into_any_element()
        } else {
            self.draft_screen(&paint, cx).into_any_element()
        };
        v_flex()
            .id(FEEDBACK_PANEL)
            .test_support()
            .track_focus(&self.focus)
            // On the pane, so Escape reaches it wherever the focus is in it.
            .when(on_submit, |this| {
                this.key_context(SUBMIT_KEY_CONTEXT)
                    .on_action(cx.listener(|this, _: &Escape, window, cx| this.back(window, cx)))
            })
            .size_full()
            .bg(paint.surface)
            .text_color(paint.foreground)
            .child(
                pane_heading(FEEDBACK_TITLE_ID, HEADING, paint.foreground)
                    .flex_none()
                    .px_3()
                    .pt_3()
                    .pb_2(),
            )
            .child(content)
    }
}

impl FeedbackPane {
    /// The draft: the help, the notices, the result, the cards and the
    /// buttons.
    fn draft_screen(&mut self, paint: &Paint, cx: &mut Context<Self>) -> impl IntoElement {
        let (cards, notices, editable) = {
            let feedback = self.feedback.read(cx);
            (
                cards(feedback),
                notices_block(feedback.notices(), paint),
                !feedback.locked(),
            )
        };
        self.sync_card_focus(&cards, cx);
        let empty = cards.is_empty();
        let content = if empty {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .px_3()
                .py_2()
                .child(
                    sentence(FEEDBACK_EMPTY_ID, EMPTY_TEXT)
                        .text_xs()
                        .text_color(paint.muted_foreground),
                )
                .into_any_element()
        } else {
            v_flex()
                .id(FEEDBACK_LIST_ID)
                .role(Role::List)
                .test_support()
                .aria_label(LIST_LABEL)
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .px_2()
                .py_2()
                .gap_1p5()
                .children(
                    cards
                        .into_iter()
                        .map(|card| self.card(card, editable, paint, cx)),
                )
                .into_any_element()
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(self.help(paint, cx))
            .children(notices)
            .children(self.result_line(paint, cx))
            .child(content)
            .child(self.buttons(empty, cx))
    }
}

/// The colors one frame of the pane paints with, all from the theme.
struct Paint {
    surface: Hsla,
    foreground: Hsla,
    muted_foreground: Hsla,
    /// The notices, the Submit screen's error line, and the comment badges'
    /// fill.
    destructive: Hsla,
    /// The Submit screen's public-repository warning.
    warning: Hsla,
    /// The comment badges' number.
    background: Hsla,
    border: Hsla,
    /// A screenshot card under the pointer.
    hover: Hsla,
    /// The keyboard focus ring.
    ring: Hsla,
}

impl Paint {
    fn new(state: &ShellState, cx: &App) -> Self {
        let native = state.native_theme();
        let (surface, foreground) = region_paint(native, Region::RightPane, cx);
        let token = |name: &str| token_paint(native, name, cx);
        Self {
            surface,
            foreground,
            muted_foreground: token("muted-foreground"),
            destructive: token("destructive"),
            warning: token("status-warn"),
            background: token("background"),
            border: token("border"),
            hover: token("muted"),
            ring: token("ring"),
        }
    }
}

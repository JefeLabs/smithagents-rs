//! The Feedback view in the right dock, and the rail's Feedback button
//! ([`hold`]). The pane shares the dock with Settings and Intents and is
//! drawn only while the rail has it selected ([`ShellState::right_entry`]).
//!
//! Top to bottom: the heading; "How to submit feedback", collapsed until
//! the user opens it (not remembered); the notices, a status in the
//! `destructive` color, while there are any; one card per attachment, or
//! what to do when there is none; and the buttons. A card shows the
//! thumbnail (or a paperclip when there is none), the title (the
//! screenshot's number and comment count, or the file's name), each comment
//! after its numbered badge, and a remove button. Clicking a screenshot card
//! reopens the annotator. "Capture" takes a picture of the window as it is,
//! "Add files…" asks for files, "Discard" asks first, and "Submit…" waits
//! for the next update.
//!
//! The pane only shows the [`Feedback`] entity and calls it: the draft, the
//! disk, the capturer and the picker are all there. It does release the
//! thumbnails the entity retires: a thumbnail drawn here stays in the
//! window's sprite atlas until it is dropped.
//!
//! The pane paints its whole area from `Region::RightPane`, like Settings;
//! everything else comes from tokens.

mod hold;

pub use hold::{HoldButton, HoldEvent, HOLD_RING_ID};

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName as CatalogIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::label::Label;
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, img, px, AnyElement, App, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    RenderImage, Role, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled,
    Subscription, TestSupportExt as _, WeakEntity, Window,
};

use crate::feedback::draft::{AttachmentId, AttachmentKind};
use crate::feedback::state::Feedback;
use crate::feedback::text::{
    card_title, discard_description, DISCARD_CANCEL, DISCARD_CONFIRM, DISCARD_TITLE, EMPTY_TEXT,
    HELP_LINES, HELP_TITLE, SUBMIT_TOOLTIP, THUMBNAIL_MAX_WIDTH,
};
use crate::shell::FEEDBACK_PANEL;
use crate::shell_state::ShellState;
use crate::theme_adapter::{region_paint, token_paint, Region};
use crate::views::pane_heading;
use crate::views::rail::RailEntry;

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
/// Element ids in the dialog Discard opens.
pub const DISCARD_DIALOG_TITLE_ID: &str = "feedback-discard-title";
pub const DISCARD_CANCEL_ID: &str = "feedback-discard-cancel";
pub const DISCARD_CONFIRM_ID: &str = "feedback-discard-confirm";

/// Element id of an attachment's card; the card is named by its title.
pub fn card_id(id: &AttachmentId) -> ElementId {
    ElementId::Name(format!("feedback-card-{}", id.as_str()).into())
}

/// Element id of a card's remove button.
pub fn card_remove_id(id: &AttachmentId) -> ElementId {
    ElementId::Name(format!("feedback-card-remove-{}", id.as_str()).into())
}

/// Element id of a comment's numbered badge, inside its card.
pub fn comment_badge_id(number: usize) -> ElementId {
    ElementId::named_usize("feedback-comment-badge", number)
}

const TITLE: &str = "Feedback";
const HEADING: &str = "FEEDBACK";
const LIST_LABEL: &str = "Attachments";
const ADD_FILES_LABEL: &str = "Add files…";
const CAPTURE_LABEL: &str = "Capture";
const DISCARD_LABEL: &str = "Discard";
const SUBMIT_LABEL: &str = "Submit…";
const REMOVE_LABEL: &str = "Remove";

/// A comment badge's height, and its least width.
const BADGE_SIZE: f32 = 16.;

pub struct FeedbackPane {
    state: Entity<ShellState>,
    feedback: Entity<Feedback>,
    focus: FocusHandle,
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
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
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
            help_open: false,
            scroll: ScrollHandle::new(),
            card_focus: HashMap::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Keeps one focus handle per screenshot card drawn.
    fn sync_card_focus(&mut self, cards: &[Card], cx: &mut Context<Self>) {
        self.card_focus
            .retain(|id, _| cards.iter().any(|card| card.screenshot && &card.id == id));
        for card in cards.iter().filter(|card| card.screenshot) {
            self.card_focus
                .entry(card.id.clone())
                .or_insert_with(|| cx.focus_handle().tab_stop(true));
        }
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

    fn card(&self, card: Card, paint: &Paint, cx: &Context<Self>) -> impl IntoElement {
        let Card {
            id,
            title,
            screenshot,
            thumbnail,
            comments,
        } = card;
        let (hover, ring) = (paint.hover, paint.ring);
        let remove = id.clone();
        // A screen reader hears the card's title, then its comments.
        let described = (!comments.is_empty()).then(|| {
            comments
                .iter()
                .enumerate()
                .map(|(index, comment)| format!("{}. {comment}", index + 1))
                .collect::<Vec<_>>()
                .join("\n")
        });
        v_flex()
            .id(card_id(&id))
            .role(if screenshot {
                Role::Button
            } else {
                Role::ListItem
            })
            .test_support()
            .aria_label(title.clone())
            .when_some(described, |this, described| {
                this.aria_description(described)
            })
            .flex_none()
            .w_full()
            .min_w_0()
            .gap_1p5()
            .p_2()
            .border_1()
            .border_color(paint.border)
            .rounded(cx.theme().radius)
            .bg(paint.surface)
            .when(screenshot, |this| {
                let open = id.clone();
                this.track_focus(&self.card_focus[&id])
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .focus_visible(move |style| style.border_color(ring))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.feedback
                            .update(cx, |feedback, cx| feedback.open_annotator(&open, cx))
                    }))
            })
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1p5()
                    .items_center()
                    .when(thumbnail.is_none(), |this| {
                        this.child(
                            Icon::new(CatalogIcon::Paperclip)
                                .small()
                                .flex_none()
                                .text_color(paint.muted_foreground),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(paint.foreground)
                            .truncate()
                            .child(title),
                    )
                    .child(
                        Button::new(card_remove_id(&remove))
                            .icon(Icon::new(CatalogIcon::Trash).text_color(paint.muted_foreground))
                            .ghost()
                            .xsmall()
                            .tooltip(REMOVE_LABEL)
                            .accessibility_label(REMOVE_LABEL)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                // Not a click on the card.
                                cx.stop_propagation();
                                this.feedback
                                    .update(cx, |feedback, cx| feedback.remove(&remove, cx));
                            })),
                    ),
            )
            .when_some(thumbnail, |this, thumbnail| {
                this.child(thumbnail_view(thumbnail))
            })
            .children(
                comments
                    .into_iter()
                    .enumerate()
                    .map(|(index, comment)| comment_line(index + 1, comment, paint)),
            )
    }

    /// Add files and Capture on the left; Discard and Submit on the right.
    fn buttons(&self, capturing: bool, empty: bool, cx: &Context<Self>) -> impl IntoElement {
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
                            .disabled(capturing)
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
                            .disabled(empty)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.ask_to_discard(window, cx)),
                            ),
                    )
                    .child(
                        Button::new(FEEDBACK_SUBMIT_ID)
                            .label(SUBMIT_LABEL)
                            .primary()
                            .small()
                            .disabled(true)
                            .tooltip(SUBMIT_TOOLTIP),
                    ),
            )
    }

    fn ask_to_discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.feedback.read(cx).draft().attachments.len();
        if count == 0 {
            return;
        }
        confirm_discard(self.feedback.downgrade(), count, window, cx);
    }
}

/// Asks before discarding the draft: "Discard this feedback?" with Cancel
/// (focused, and what Enter does) and a danger "Discard", the only way to
/// discard.
fn confirm_discard(
    feedback: WeakEntity<Feedback>,
    count: usize,
    window: &mut Window,
    cx: &mut App,
) {
    let cancel_focus = cx.focus_handle();
    let description = SharedString::from(discard_description(count));
    // The builder runs again whenever the dialog layer rebuilds; focus only
    // on the first run, so a rebuild never steals the focus back.
    let first_build = Rc::new(Cell::new(true));
    window.open_alert_dialog(cx, move |alert, window, cx| {
        if first_build.replace(false) {
            cancel_focus.focus(window, cx);
        }
        let feedback = feedback.clone();
        alert
            .title(
                div()
                    .id(DISCARD_DIALOG_TITLE_ID)
                    .role(Role::Heading)
                    .test_support()
                    .aria_label(DISCARD_TITLE)
                    .text_color(cx.theme().popover_foreground)
                    .child(DISCARD_TITLE),
            )
            // A `Label`, so assistive technology reads it too.
            .description(Label::new(description.clone()).text_color(cx.theme().muted_foreground))
            // Enter dispatches the dialog's `Confirm`, which runs this: here it
            // dismisses, like Cancel. Returning `true` closes the dialog.
            .on_ok(|_, _, _| true)
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new(DISCARD_CANCEL_ID)
                            .label(DISCARD_CANCEL)
                            .outline()
                            .track_focus(&cancel_focus)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new(DISCARD_CONFIRM_ID)
                            .label(DISCARD_CONFIRM)
                            .danger()
                            .on_click(move |_, window, cx| {
                                let _ =
                                    feedback.update(cx, |feedback, cx| feedback.discard_all(cx));
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}

/// What one card shows.
struct Card {
    id: AttachmentId,
    title: String,
    screenshot: bool,
    thumbnail: Option<Arc<RenderImage>>,
    comments: Vec<String>,
}

/// One card per attachment, in the draft's order. A screenshot's number is
/// its place among the draft's screenshots.
fn cards(feedback: &Feedback) -> Vec<Card> {
    let mut screenshots = 0;
    feedback
        .draft()
        .attachments
        .iter()
        .map(|attachment| {
            let screenshot = attachment.kind == AttachmentKind::Screenshot;
            let title = if screenshot {
                screenshots += 1;
                card_title(screenshots, attachment.boxes.len())
            } else {
                attachment.original_name.clone()
            };
            Card {
                id: attachment.id.clone(),
                title,
                screenshot,
                thumbnail: feedback.thumbnail(&attachment.id),
                comments: attachment
                    .boxes
                    .iter()
                    .map(|annotation| annotation.comment.clone())
                    .collect(),
            }
        })
        .collect()
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
/// when there are none.
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
            .px_3()
            .py_2()
            .gap_1()
            .text_xs()
            .text_color(paint.destructive)
            .children(notices.iter().map(|notice| div().child(notice.clone())))
            .into_any_element(),
    )
}

/// The thumbnail at its own size, at most [`THUMBNAIL_MAX_WIDTH`] points
/// wide and never wider than the card, keeping its shape.
fn thumbnail_view(thumbnail: Arc<RenderImage>) -> impl IntoElement {
    let size = thumbnail.size(0);
    let (width, height) = (size.width.0.max(1) as f32, size.height.0.max(1) as f32);
    div()
        .flex_none()
        .w_full()
        .max_w(px(width.min(THUMBNAIL_MAX_WIDTH as f32)))
        .aspect_ratio(width / height)
        .child(img(thumbnail).size_full())
}

/// A comment after its number, in a badge: the number in `background` on a
/// `destructive` fill, the colors of the boxes drawn over a screenshot.
fn comment_line(number: usize, comment: String, paint: &Paint) -> impl IntoElement {
    h_flex()
        .id(ElementId::named_usize("feedback-comment", number))
        .w_full()
        .min_w_0()
        .gap_1p5()
        .items_start()
        .child(
            div()
                .id(comment_badge_id(number))
                .test_support()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .h(px(BADGE_SIZE))
                .min_w(px(BADGE_SIZE))
                .px_1()
                .rounded_full()
                .bg(paint.destructive)
                .text_color(paint.background)
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .child(number.to_string()),
        )
        .child(
            Label::new(comment)
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(paint.foreground),
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
        let (cards, notices, capturing) = {
            let feedback = self.feedback.read(cx);
            (
                cards(feedback),
                notices_block(feedback.notices(), &paint),
                feedback.capturing(),
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
                .children(cards.into_iter().map(|card| self.card(card, &paint, cx)))
                .into_any_element()
        };
        v_flex()
            .id(FEEDBACK_PANEL)
            .test_support()
            .track_focus(&self.focus)
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
            .child(self.help(&paint, cx))
            .children(notices)
            .child(content)
            .child(self.buttons(capturing, empty, cx))
    }
}

/// The colors one frame of the pane paints with, all from the theme.
struct Paint {
    surface: Hsla,
    foreground: Hsla,
    muted_foreground: Hsla,
    /// The notices, and the comment badges' fill.
    destructive: Hsla,
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
            background: token("background"),
            border: token("border"),
            hover: token("muted"),
            ring: token("ring"),
        }
    }
}

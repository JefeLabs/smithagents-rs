//! The Feedback pane's cards: one per attachment, showing the thumbnail (or
//! a paperclip when there is none), the title (the screenshot's number and
//! comment count, or the file's name), each comment after its numbered
//! badge, and a remove button. A screenshot card is a tab stop; clicking it,
//! or Enter or Space on it, reopens the annotator.

use std::sync::Arc;

use gpui_kit::assets::IconName as CatalogIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, img, px, rems, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Rems, RenderImage, Role, StatefulInteractiveElement as _, Styled,
    TestSupportExt as _,
};

use crate::feedback::draft::{AttachmentId, AttachmentKind};
use crate::feedback::state::Feedback;
use crate::feedback::text::{card_title, THUMBNAIL_MAX_WIDTH};

use super::{FeedbackPane, Paint};

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

const REMOVE_LABEL: &str = "Remove";

/// A comment badge's height, and its least width: it grows with the text.
const BADGE_SIZE: Rems = rems(1.);

impl FeedbackPane {
    /// Keeps one focus handle per screenshot card drawn.
    pub(super) fn sync_card_focus(&mut self, cards: &[Card], cx: &mut Context<Self>) {
        self.card_focus
            .retain(|id, _| cards.iter().any(|card| card.screenshot && &card.id == id));
        for card in cards.iter().filter(|card| card.screenshot) {
            self.card_focus
                .entry(card.id.clone())
                .or_insert_with(|| cx.focus_handle().tab_stop(true));
        }
    }

    /// One attachment's card; a screenshot's is a button that reopens the
    /// annotator.
    pub(super) fn card(&self, card: Card, paint: &Paint, cx: &Context<Self>) -> impl IntoElement {
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
}

/// What one card shows.
pub(super) struct Card {
    id: AttachmentId,
    title: String,
    screenshot: bool,
    thumbnail: Option<Arc<RenderImage>>,
    comments: Vec<String>,
}

/// One card per attachment, in the draft's order. A screenshot's number is
/// its place among the draft's screenshots.
pub(super) fn cards(feedback: &Feedback) -> Vec<Card> {
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
                .h(BADGE_SIZE)
                .min_w(BADGE_SIZE)
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

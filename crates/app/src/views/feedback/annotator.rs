//! The annotator: a full-window overlay on one screenshot. The shell draws
//! it over everything (title bar, panes, dock, rail) while
//! [`Feedback::annotating`] names a screenshot.
//!
//! A bar of [`ANNOTATOR_BAR_HEIGHT`] holds the title ("Screenshot n of m",
//! or a limit sentence), "Discard" and "Done". Below it the picture is drawn
//! whole ([`ObjectFit::Contain`]); a drag over it draws a box and opens a
//! comment under the box. Enter saves the comment (shift-Enter adds a line),
//! Escape drops a new box or cancels an edit; a box saved with no comment is
//! dropped. Each saved box has a numbered badge; clicking it (or Enter or
//! Space on it: badges are tab stops) opens its comment again, where
//! "Delete" removes it and the later boxes renumber. Every change is saved
//! at once through [`Feedback::set_boxes`]. After "Done" the annotator only
//! waits for the annotated picture to be drawn, then closes.
//!
//! The keyboard stays inside: Tab and shift-Tab cycle through the
//! annotator's own controls (a focus trap), and the shell under it handles
//! none of its keys while it is open.
//!
//! Boxes are fractions of the picture, so they keep their place at any
//! window size: the pointer is converted through the rectangle the picture
//! is drawn in (never the area around it), and a drag that starts or ends
//! outside the picture is clamped to it.
//!
//! The decoded picture lives in the window's sprite atlas until
//! [`Annotator::release`] drops it; the shell calls it whenever the
//! annotator closes.
//!
//! Boxes and badges are drawn in the `destructive` token, a badge's number
//! in `background`; the bar paints from `Region::TitleBar`.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Escape, InputEvent, Textarea, TextareaState};
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Disableable as _, FocusTrapElement as _, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    anchored, canvas, deferred, div, fill, img, outline, point, px, relative, size, AnyElement,
    App, AppContext as _, BorderStyle, Bounds, Context, ElementId, Entity, FocusHandle, Focusable,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement as _, Pixels, Point, Render,
    RenderImage, Role, Size, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    Subscription, Task, TestSupportExt as _, Window,
};
use smithagents_theme::native::NativeTheme;

use crate::feedback::annotation::{add_box, big_enough, clean_comment, Annotation, FracRect};
use crate::feedback::draft::{AttachmentId, AttachmentKind};
use crate::feedback::state::Feedback;
use crate::feedback::text::{
    annotator_discard_description, annotator_title, ANNOTATOR_DISCARD_CONFIRM,
    ANNOTATOR_DISCARD_KEEP, ANNOTATOR_DISCARD_TITLE, COMMENT_HINT, COMMENT_TOO_LONG_TEXT,
    MAX_BOXES, TOO_MANY_BOXES_TEXT,
};
use crate::theme_adapter::{region_paint, token_paint, Region};

use super::{confirm, Confirmation};

/// Element id of the annotator (the whole window).
pub const ANNOTATOR_ID: &str = "feedback-annotator";
/// Element id of the bar's title: "Screenshot n of m · …", or the sentence
/// saying a press was refused.
pub const ANNOTATOR_TITLE_ID: &str = "feedback-annotator-title";
pub const ANNOTATOR_DONE_ID: &str = "feedback-annotator-done";
pub const ANNOTATOR_DISCARD_ID: &str = "feedback-annotator-discard";
/// Element id of the comment's frame; the text field is inside it.
pub const ANNOTATOR_COMMENT_ID: &str = "feedback-annotator-comment";
/// Element id of the sentence under a comment that is too long (a status).
pub const ANNOTATOR_COMMENT_ERROR_ID: &str = "feedback-annotator-comment-error";
pub const ANNOTATOR_DELETE_ID: &str = "feedback-annotator-delete";
/// Element ids in the dialog Discard opens when the screenshot has boxes.
pub const ANNOTATOR_DISCARD_TITLE_ID: &str = "feedback-annotator-discard-title";
pub const ANNOTATOR_KEEP_ID: &str = "feedback-annotator-keep";
pub const ANNOTATOR_CONFIRM_DISCARD_ID: &str = "feedback-annotator-confirm-discard";
/// The bar's height in points; the picture is drawn below it.
pub const ANNOTATOR_BAR_HEIGHT: f32 = 44.;

/// Element id of a box's numbered badge; the badge is named by its number.
pub fn badge_id(number: usize) -> ElementId {
    ElementId::named_usize("feedback-badge", number)
}

const AREA_ID: &str = "feedback-annotator-area";
const PANEL_ID: &str = "feedback-annotator-panel";
const DONE_LABEL: &str = "Done";
const DISCARD_LABEL: &str = "Discard";
const DELETE_LABEL: &str = "Delete";
const COMMENT_LABEL: &str = "Comment";
/// A badge's accessible name is this and its number.
const BADGE_LABEL: &str = "Comment";
/// The comment grows from one line to this many, then scrolls.
const COMMENT_ROWS: (usize, usize) = (1, 4);
const PANEL_WIDTH: f32 = 280.;
/// Space between a box's bottom edge and its comment.
const PANEL_GAP: f32 = 6.;
/// A badge's height, and its least width.
const BADGE_SIZE: f32 = 20.;
/// How strongly a box is filled (its outline is solid).
const BOX_FILL_OPACITY: f32 = 0.12;
/// Room for the window's traffic lights, which macOS draws over the bar.
#[cfg(target_os = "macos")]
const BAR_LEFT_INSET: f32 = 80.;
#[cfg(not(target_os = "macos"))]
const BAR_LEFT_INSET: f32 = 12.;

/// A drag in progress, in window coordinates.
#[derive(Debug, Clone, Copy)]
struct Drag {
    from: Point<Pixels>,
    to: Point<Pixels>,
}

/// The box an open comment belongs to.
#[derive(Debug, Clone, Copy)]
enum Commented {
    /// Drawn just now; saved only with its comment.
    New(FracRect),
    /// A saved box, by its index.
    Saved(usize),
}

#[derive(Debug, Clone, Copy)]
struct Comment {
    target: Commented,
    /// Enter was pressed on more than the limit; cleared by the next edit.
    too_long: bool,
}

pub struct Annotator {
    feedback: Entity<Feedback>,
    id: AttachmentId,
    /// The stored picture, once decoded.
    image: Option<Arc<RenderImage>>,
    loading: Option<Task<()>>,
    /// The screenshot's boxes as saved.
    boxes: Vec<Annotation>,
    focus: FocusHandle,
    text: Entity<TextareaState>,
    comment: Option<Comment>,
    drag: Option<Drag>,
    /// A press was refused because the screenshot has [`MAX_BOXES`]; the
    /// title says so until the next action.
    full: bool,
    /// Done was pressed: the annotated picture is being drawn, and the
    /// annotator only waits to close.
    closing: bool,
    /// One per badge, so each is a tab stop.
    badge_focus: Vec<FocusHandle>,
    /// The picture's area as last painted: pointer events convert through it.
    area: Rc<Cell<Option<Bounds<Pixels>>>>,
    native: Option<Rc<NativeTheme>>,
    _subscriptions: Vec<Subscription>,
}

impl Annotator {
    /// The annotator on screenshot `id`, with its saved boxes; the picture
    /// is decoded off the UI thread ([`Feedback::load_image`]).
    pub fn new(
        feedback: Entity<Feedback>,
        id: AttachmentId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let boxes = feedback
            .read(cx)
            .draft()
            .get(&id)
            .map(|shot| shot.boxes.clone())
            .unwrap_or_default();
        let text = cx.new(|cx| {
            let mut text = TextareaState::new(window, cx)
                .auto_grow(COMMENT_ROWS.0, COMMENT_ROWS.1)
                .placeholder(COMMENT_HINT);
            // Enter saves (the field emits PressEnter and adds no line);
            // shift-Enter adds a line.
            text.set_submit_on_enter(true, cx);
            text
        });
        let load = feedback.update(cx, |feedback, cx| feedback.load_image(&id, cx));
        let loading = cx.spawn(async move |this, cx| {
            let image = load.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = None;
                this.image = image;
                cx.notify();
            });
        });
        let subscriptions = vec![
            // The title counts the draft's screenshots.
            cx.observe(&feedback, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &text,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { shift: false, .. } => this.save_comment(window, cx),
                    InputEvent::Change => {
                        if let Some(comment) = this.comment.as_mut().filter(|c| c.too_long) {
                            comment.too_long = false;
                            cx.notify();
                        }
                    }
                    _ => {}
                },
            ),
        ];
        Self {
            feedback,
            id,
            image: None,
            loading: Some(loading),
            boxes,
            focus: cx.focus_handle(),
            text,
            comment: None,
            drag: None,
            full: false,
            closing: false,
            badge_focus: Vec::new(),
            area: Rc::default(),
            native: None,
            _subscriptions: subscriptions,
        }
    }

    /// The decoded picture; `None` until it is decoded (or when it does not
    /// decode).
    pub fn image(&self) -> Option<Arc<RenderImage>> {
        self.image.clone()
    }

    /// The screenshot's boxes as saved, in their numbers' order.
    pub fn boxes(&self) -> &[Annotation] {
        &self.boxes
    }

    /// Drops the picture from the window's sprite atlas (and stops a decode
    /// still running). Called when the annotator closes.
    pub fn release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = None;
        if let Some(image) = self.image.take() {
            // `Some(window)`: the window being updated is skipped otherwise.
            cx.drop_image(image, Some(window));
        }
    }

    /// The theme to paint with (the shell's, given on every frame).
    pub(crate) fn set_theme(&mut self, native: Option<Rc<NativeTheme>>, cx: &mut Context<Self>) {
        let same = match (&self.native, &native) {
            (Some(held), Some(given)) => Rc::ptr_eq(held, given),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.native = native;
            cx.notify();
        }
    }

    /// Where the picture is drawn, in window coordinates, once it is
    /// decoded and painted.
    fn picture_bounds(&self) -> Option<Bounds<Pixels>> {
        let image = self.image.as_ref()?;
        Some(ObjectFit::Contain.get_bounds(self.area.get()?, image.size(0)))
    }

    fn on_press(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        self.full = false;
        cx.notify();
        if self.comment.is_some() {
            // An open comment is answered first (Enter, Escape or Delete),
            // and keeps the focus: the annotator does not take it.
            window.prevent_default();
            return;
        }
        if self.image.is_none() {
            return;
        }
        if self.boxes.len() >= MAX_BOXES {
            tracing::debug!("a screenshot already has the most boxes it can");
            self.full = true;
            return;
        }
        self.drag = Some(Drag {
            from: event.position,
            to: event.position,
        });
    }

    fn on_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            // The release was missed: the drag ends where it was last seen.
            let drag = *drag;
            self.drag = None;
            self.finish_drag(drag, window, cx);
            return;
        }
        drag.to = event.position;
        cx.notify();
    }

    /// A release on the area or anywhere else ends the drag there.
    fn on_release(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mut drag) = self.drag.take() {
            drag.to = event.position;
            self.finish_drag(drag, window, cx);
        }
    }

    /// A box big enough opens its comment; a smaller one is nothing.
    fn finish_drag(&mut self, drag: Drag, window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
        let Some(picture) = self.picture_bounds() else {
            return;
        };
        if let Some(rect) = dragged_box(picture, drag.from, drag.to) {
            self.open_comment(Commented::New(rect), String::new(), window, cx);
        }
    }

    /// A badge's click: its box's comment, to edit (not while another
    /// comment is open).
    fn edit_box(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.full = false;
        cx.notify();
        if self.comment.is_some() {
            return;
        }
        if let Some(text) = self.boxes.get(index).map(|saved| saved.comment.clone()) {
            self.open_comment(Commented::Saved(index), text, window, cx);
        }
    }

    fn open_comment(
        &mut self,
        target: Commented,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }
        self.comment = Some(Comment {
            target,
            too_long: false,
        });
        self.text.update(cx, |field, cx| {
            let end = text.len();
            field.set_value(text, window, cx);
            field.set_selected_range(end..end, cx);
            field.focus(window, cx);
        });
        cx.notify();
    }

    /// Closes the comment without saving it; the focus goes back to the
    /// annotator.
    fn close_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.comment.take().is_some() {
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Enter: saves the comment, or drops its box when it is empty. A
    /// comment over the limit stays open and says so.
    fn save_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.full = false;
        let Some(comment) = self.comment.as_mut() else {
            return;
        };
        let cleaned = clean_comment(&self.text.read(cx).value());
        let Ok(cleaned) = cleaned else {
            // `clean_comment` refuses only a comment that is too long.
            comment.too_long = true;
            cx.notify();
            return;
        };
        let target = comment.target;
        self.close_comment(window, cx);
        let mut boxes = self.boxes.clone();
        match (target, cleaned) {
            (Commented::New(rect), Some(text)) => {
                if add_box(&mut boxes, rect, text).is_err() {
                    self.full = true;
                    return;
                }
            }
            (Commented::New(_), None) => return,
            (Commented::Saved(index), Some(text)) => match boxes.get_mut(index) {
                Some(saved) => saved.comment = text,
                None => return,
            },
            (Commented::Saved(index), None) => {
                if index >= boxes.len() {
                    return;
                }
                boxes.remove(index);
            }
        }
        self.save_boxes(boxes, cx);
    }

    /// Escape: drops a new box, or leaves a saved one as it was. With no
    /// comment open it is not the annotator's.
    fn on_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        self.full = false;
        cx.notify();
        if self.comment.is_none() {
            cx.propagate();
            return;
        }
        self.close_comment(window, cx);
    }

    /// "Delete": removes the comment's box (a new one is just dropped); the
    /// later boxes take the numbers down by one.
    fn delete_box(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(comment) = self.comment else {
            return;
        };
        self.close_comment(window, cx);
        if let Commented::Saved(index) = comment.target {
            if index < self.boxes.len() {
                let mut boxes = self.boxes.clone();
                boxes.remove(index);
                self.save_boxes(boxes, cx);
            }
        }
    }

    fn save_boxes(&mut self, boxes: Vec<Annotation>, cx: &mut Context<Self>) {
        self.boxes = boxes.clone();
        let id = self.id.clone();
        self.feedback
            .update(cx, |feedback, cx| feedback.set_boxes(&id, boxes, cx));
        cx.notify();
    }

    /// "Done": a comment still open is saved as Enter would; then the
    /// annotated picture is drawn and the annotator closes. Until it
    /// closes, Done is busy and the picture, the badges and Discard do
    /// nothing, so nothing is drawn that the picture would miss.
    fn done(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        self.full = false;
        cx.notify();
        if self.comment.is_some() {
            self.save_comment(window, cx);
            if self.comment.is_some() {
                // Too long: it says so and stays.
                return;
            }
        }
        self.closing = true;
        // The buttons are about to be disabled: keep the focus in here.
        self.focus.focus(window, cx);
        self.feedback
            .update(cx, |feedback, cx| feedback.finish_annotating(cx));
    }

    /// "Discard": removes the screenshot, at once when it has no boxes,
    /// else after asking ("Discard this screenshot?", Keep focused). An open
    /// comment is dropped first.
    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        self.full = false;
        cx.notify();
        self.close_comment(window, cx);
        let comments = self.boxes.len();
        if comments == 0 {
            self.feedback
                .update(cx, |feedback, cx| feedback.discard_annotated(cx));
            return;
        }
        let feedback = self.feedback.downgrade();
        let ask = Confirmation {
            title_id: ANNOTATOR_DISCARD_TITLE_ID,
            title: ANNOTATOR_DISCARD_TITLE,
            description: annotator_discard_description(comments),
            cancel_id: ANNOTATOR_KEEP_ID,
            cancel: ANNOTATOR_DISCARD_KEEP,
            confirm_id: ANNOTATOR_CONFIRM_DISCARD_ID,
            confirm: ANNOTATOR_DISCARD_CONFIRM,
        };
        confirm(
            ask,
            move |_, cx| {
                let _ = feedback.update(cx, |feedback, cx| feedback.discard_annotated(cx));
            },
            window,
            cx,
        );
    }

    /// "Screenshot n of m · …": n is this screenshot's place among the
    /// draft's screenshots.
    fn title(&self, cx: &App) -> String {
        if self.full {
            return TOO_MANY_BOXES_TEXT.to_string();
        }
        let draft = self.feedback.read(cx).draft();
        let screenshots: Vec<&AttachmentId> = draft
            .attachments
            .iter()
            .filter(|attachment| attachment.kind == AttachmentKind::Screenshot)
            .map(|attachment| &attachment.id)
            .collect();
        let number = screenshots
            .iter()
            .position(|id| **id == self.id)
            .map_or(0, |index| index + 1);
        annotator_title(number, screenshots.len())
    }

    fn bar(&self, paint: &Paint, cx: &Context<Self>) -> impl IntoElement {
        let title = self.title(cx);
        h_flex()
            .flex_none()
            .w_full()
            .h(px(ANNOTATOR_BAR_HEIGHT))
            .pl(px(BAR_LEFT_INSET))
            .pr_3()
            .gap_2()
            .items_center()
            .bg(paint.bar)
            .border_b_1()
            .border_color(paint.border)
            .child(
                div()
                    .id(ANNOTATOR_TITLE_ID)
                    // A refusal is announced; the title is a heading.
                    .role(if self.full {
                        Role::Status
                    } else {
                        Role::Heading
                    })
                    .test_support()
                    .aria_label(title.clone())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if self.full {
                        paint.destructive
                    } else {
                        paint.bar_foreground
                    })
                    .child(title),
            )
            .child(
                Button::new(ANNOTATOR_DISCARD_ID)
                    .label(DISCARD_LABEL)
                    .ghost()
                    .small()
                    .disabled(self.closing)
                    .on_click(cx.listener(|this, _, window, cx| this.discard(window, cx))),
            )
            .child(
                Button::new(ANNOTATOR_DONE_ID)
                    .label(DONE_LABEL)
                    .primary()
                    .small()
                    .loading(self.closing)
                    .disabled(self.closing)
                    .on_click(cx.listener(|this, _, window, cx| this.done(window, cx))),
            )
    }

    /// The picture, its boxes and badges, and the open comment.
    fn picture_area(
        &self,
        area: Bounds<Pixels>,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let content = self.image.clone().map(|image| {
            let image_size = image.size(0);
            let picture = ObjectFit::Contain.get_bounds(area, image_size);
            let mut shapes: Vec<FracRect> = self.boxes.iter().map(|saved| saved.rect).collect();
            if let Some(Comment {
                target: Commented::New(rect),
                ..
            }) = self.comment
            {
                shapes.push(rect);
            }
            let dragging = self.drag;
            let (recorded, color) = (self.area.clone(), paint.destructive);
            let drawn = canvas(
                move |bounds, _, _| recorded.set(Some(bounds)),
                move |bounds, _, window, _| {
                    let picture = ObjectFit::Contain.get_bounds(bounds, image_size);
                    for rect in shapes {
                        paint_box(window, frac_bounds(picture, rect), color);
                    }
                    if let Some(drag) = dragging {
                        let (a, b) = (clamped(picture, drag.from), clamped(picture, drag.to));
                        let corner = point(px(a.0.min(b.0)), px(a.1.min(b.1)));
                        let extent = size(px((a.0 - b.0).abs()), px((a.1 - b.1).abs()));
                        paint_box(window, Bounds::new(corner, extent), color);
                    }
                },
            )
            .absolute()
            .inset_0();
            [
                img(image)
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    .into_any_element(),
                drawn.into_any_element(),
                self.over_picture(area, picture, paint, cx)
                    .into_any_element(),
            ]
        });
        div()
            .id(AREA_ID)
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .bg(paint.background)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_press))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_release))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_release))
            .children(content.into_iter().flatten())
    }

    /// A layer the size of the drawn picture, so badges and the comment
    /// are placed by fractions of the picture.
    fn over_picture(
        &self,
        area: Bounds<Pixels>,
        picture: Bounds<Pixels>,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let badges = self
            .boxes
            .iter()
            .zip(&self.badge_focus)
            .enumerate()
            .map(|(index, (saved, focus))| badge(index, saved, focus, paint, cx));
        let comment = self.comment.map(|comment| {
            let rect = match comment.target {
                Commented::New(rect) => Some(rect),
                Commented::Saved(index) => self.boxes.get(index).map(|saved| saved.rect),
            };
            let rect = rect.unwrap_or(FracRect {
                x: 0.,
                y: 0.,
                w: 0.,
                h: 0.,
            });
            // Under the box, kept inside the window.
            div()
                .absolute()
                .left(relative(rect.x))
                .top(relative(rect.y + rect.h))
                .pt(px(PANEL_GAP))
                .child(deferred(
                    anchored().snap_to_window().child(self.comment_panel(
                        comment.too_long,
                        paint,
                        cx,
                    )),
                ))
        });
        div()
            .absolute()
            .left(picture.origin.x - area.origin.x)
            .top(picture.origin.y - area.origin.y)
            .w(picture.size.width)
            .h(picture.size.height)
            .children(badges)
            .children(comment)
    }

    fn comment_panel(&self, too_long: bool, paint: &Paint, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .id(PANEL_ID)
            // A press on the comment is not a press on the picture.
            .occlude()
            .w(px(PANEL_WIDTH))
            .gap_1p5()
            .p_2()
            .bg(paint.popover)
            .text_color(paint.popover_foreground)
            .border_1()
            .border_color(paint.border)
            .rounded(cx.theme().radius)
            .shadow_md()
            .child(
                div()
                    .id(ANNOTATOR_COMMENT_ID)
                    .test_support()
                    .w_full()
                    .child(Textarea::new(&self.text).aria_label(COMMENT_LABEL).small()),
            )
            .when(too_long, |this| {
                this.child(
                    div()
                        .id(ANNOTATOR_COMMENT_ERROR_ID)
                        .role(Role::Status)
                        .test_support()
                        .aria_label(COMMENT_TOO_LONG_TEXT)
                        .text_xs()
                        .text_color(paint.destructive)
                        .child(COMMENT_TOO_LONG_TEXT),
                )
            })
            .child(
                h_flex().w_full().justify_end().child(
                    Button::new(ANNOTATOR_DELETE_ID)
                        .label(DELETE_LABEL)
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(|this, _, window, cx| this.delete_box(window, cx))),
                ),
            )
    }
}

/// Box `index`'s badge at the box's top-left corner: its number in
/// `background` on a `destructive` fill, named "Comment n" for assistive
/// technology. A click, or Enter or Space while it has the focus (it is a
/// tab stop), opens its comment; a press on it never starts a drag.
fn badge(
    index: usize,
    saved: &Annotation,
    focus: &FocusHandle,
    paint: &Paint,
    cx: &Context<Annotator>,
) -> AnyElement {
    let number = index + 1;
    let ring = paint.ring;
    div()
        .id(badge_id(number))
        .role(Role::Button)
        .test_support()
        .aria_label(format!("{BADGE_LABEL} {number}"))
        .aria_description(saved.comment.clone())
        .track_focus(focus)
        .focus_visible(move |style| style.border_color(ring))
        .absolute()
        .left(relative(saved.rect.x))
        .top(relative(saved.rect.y))
        .occlude()
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .h(px(BADGE_SIZE))
        .min_w(px(BADGE_SIZE))
        .px_1()
        .rounded_full()
        .border_2()
        .border_color(paint.destructive)
        .bg(paint.destructive)
        .text_color(paint.background)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .child(number.to_string())
        .on_click(cx.listener(move |this, _, window, cx| this.edit_box(index, window, cx)))
        .into_any_element()
}

/// The window below the bar, where the picture is drawn. The annotator
/// covers the whole window, so this is known while rendering, before the
/// frame is laid out (the badges are placed from it; the pointer is
/// converted through the area as painted).
fn image_area(viewport: Size<Pixels>) -> Bounds<Pixels> {
    let bar = px(ANNOTATOR_BAR_HEIGHT);
    Bounds::new(
        point(px(0.), bar),
        size(viewport.width, (viewport.height - bar).max(px(0.))),
    )
}

/// `at` moved into `picture`, as plain numbers.
fn clamped(picture: Bounds<Pixels>, at: Point<Pixels>) -> (f32, f32) {
    let (left, top) = (f32::from(picture.left()), f32::from(picture.top()));
    let (right, bottom) = (f32::from(picture.right()), f32::from(picture.bottom()));
    (
        f32::from(at.x).max(left).min(right),
        f32::from(at.y).max(top).min(bottom),
    )
}

/// The box a drag from `a` to `b` draws over the picture drawn at `picture`
/// (window coordinates), as fractions of the picture. Each corner is clamped
/// to the picture first; `None` when the box drawn is not
/// [`big_enough`].
fn dragged_box(picture: Bounds<Pixels>, a: Point<Pixels>, b: Point<Pixels>) -> Option<FracRect> {
    let (left, top) = (f32::from(picture.left()), f32::from(picture.top()));
    let (width, height) = (
        f32::from(picture.size.width),
        f32::from(picture.size.height),
    );
    if !(width > 0. && height > 0.) {
        return None;
    }
    let (a, b) = (clamped(picture, a), clamped(picture, b));
    if !big_enough((a.0 - b.0).abs(), (a.1 - b.1).abs()) {
        return None;
    }
    let fraction = |(x, y): (f32, f32)| ((x - left) / width, (y - top) / height);
    Some(FracRect::from_corners(fraction(a), fraction(b)))
}

/// Where `rect` (fractions of the picture) is drawn.
fn frac_bounds(picture: Bounds<Pixels>, rect: FracRect) -> Bounds<Pixels> {
    Bounds::new(
        point(
            picture.origin.x + picture.size.width * rect.x,
            picture.origin.y + picture.size.height * rect.y,
        ),
        size(picture.size.width * rect.w, picture.size.height * rect.h),
    )
}

/// A box: a faint fill and a solid outline.
fn paint_box(window: &mut Window, bounds: Bounds<Pixels>, color: Hsla) {
    window.paint_quad(fill(bounds, color.opacity(BOX_FILL_OPACITY)));
    window.paint_quad(outline(bounds, color, BorderStyle::Solid));
}

impl Focusable for Annotator {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Annotator {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = Paint::new(self.native.as_deref(), cx);
        let area = image_area(window.viewport_size());
        let badges = self.boxes.len();
        self.badge_focus.truncate(badges);
        while self.badge_focus.len() < badges {
            self.badge_focus.push(cx.focus_handle().tab_stop(true));
        }
        v_flex()
            .id(ANNOTATOR_ID)
            .test_support()
            .on_action(cx.listener(Self::on_escape))
            .size_full()
            .bg(paint.background)
            .child(self.bar(&paint, cx))
            .child(self.picture_area(area, &paint, cx))
            // Tab and shift-Tab cycle in here; the shell under it is out of
            // reach (it also stops handling its keys while this is open).
            .focus_trap(ANNOTATOR_ID, &self.focus)
    }
}

/// The colors one frame of the annotator paints with, all from the theme.
struct Paint {
    bar: Hsla,
    bar_foreground: Hsla,
    /// Around the picture, and a badge's number.
    background: Hsla,
    border: Hsla,
    /// Boxes, badges, and what a refusal says.
    destructive: Hsla,
    popover: Hsla,
    popover_foreground: Hsla,
    /// A badge with the keyboard focus.
    ring: Hsla,
}

impl Paint {
    fn new(native: Option<&NativeTheme>, cx: &App) -> Self {
        let (bar, bar_foreground) = region_paint(native, Region::TitleBar, cx);
        let token = |name: &str| token_paint(native, name, cx);
        Self {
            bar,
            bar_foreground,
            background: token("background"),
            border: token("border"),
            destructive: token("destructive"),
            popover: token("popover"),
            popover_foreground: token("popover-foreground"),
            ring: token("ring"),
        }
    }
}

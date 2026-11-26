//! The Feedback rail button. A click shows or hides the Feedback pane;
//! holding it for [`HOLD`] takes a picture of the window.
//!
//! A left press starts the hold: a timer for [`HOLD`] and a repaint loop that
//! redraws the ring, whose fill is the time held so far (the executor's
//! clock, never an animation). A release on the button before the end is a
//! click. A release elsewhere, the pointer leaving the button, or a move
//! without the left button held (its release was missed) ends the hold with
//! nothing done. At the end of the hold the ring goes away and the button
//! says [`HoldEvent::Completed`]; the release that follows is not a click.

use std::time::{Duration, Instant};

use gpui_kit::component::progress::ProgressCircle;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, App, Context, EventEmitter, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement as _, Render, StatefulInteractiveElement as _,
    Styled as _, Task, Window,
};

use crate::feedback::text::HOLD;
use crate::views::rail::{rail_button, RailEntry};

/// Element id of the ring drawn over the button while it is held.
pub const HOLD_RING_ID: &str = "rail-feedback-ring";

/// The area that is "on the button" for the hold (the button's own bounds).
const HOLD_AREA_ID: &str = "rail-feedback-hold";

/// How often the ring is redrawn while the button is held (about 30 a
/// second).
const REPAINT_EVERY: Duration = Duration::from_millis(33);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldEvent {
    /// A click: show or hide the Feedback pane.
    Clicked,
    /// Held for [`HOLD`]: capture.
    Completed,
}

pub struct HoldButton {
    selected: bool,
    icon_color: Hsla,
    /// When the press began, while the button is held.
    pressed_at: Option<Instant>,
    /// The wait for [`HOLD`] and the ring's repaint loop; dropping them ends
    /// the hold.
    hold: Option<Task<()>>,
    repaint: Option<Task<()>>,
    /// A hold completed: the click its release makes is not one.
    swallow_click: bool,
}

impl EventEmitter<HoldEvent> for HoldButton {}

impl HoldButton {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            selected: false,
            icon_color: cx.theme().foreground,
            pressed_at: None,
            hold: None,
            repaint: None,
            swallow_click: false,
        }
    }

    /// Draws the button selected (the Feedback pane is on screen) or not.
    pub fn set_selected(&mut self, selected: bool, cx: &mut Context<Self>) {
        if self.selected != selected {
            self.selected = selected;
            cx.notify();
        }
    }

    /// The icon's color: the rail's foreground, as the other rail buttons.
    pub(crate) fn set_icon_color(&mut self, color: Hsla, cx: &mut Context<Self>) {
        if self.icon_color != color {
            self.icon_color = color;
            cx.notify();
        }
    }

    /// How far the hold is: 0.0 when the button is not held, else the time
    /// held over [`HOLD`], at most 1.0.
    pub fn progress(&self, cx: &App) -> f32 {
        let Some(pressed_at) = self.pressed_at else {
            return 0.0;
        };
        let held = cx
            .background_executor()
            .now()
            .saturating_duration_since(pressed_at);
        (held.as_secs_f32() / HOLD.as_secs_f32()).min(1.0)
    }

    fn holding(&self) -> bool {
        self.pressed_at.is_some()
    }

    fn on_press(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        tracing::debug!("feedback hold: pressed");
        self.swallow_click = false;
        self.pressed_at = Some(cx.background_executor().now());
        let timer = cx.background_executor().timer(HOLD);
        self.hold = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| this.complete(cx));
        }));
        self.repaint = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(REPAINT_EVERY).await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        }));
        cx.notify();
    }

    /// Ends the hold without completing it (the tasks are dropped).
    fn cancel(&mut self, why: &'static str, cx: &mut Context<Self>) {
        if !self.holding() {
            return;
        }
        tracing::debug!(why, "feedback hold: ended before {HOLD:?}");
        self.stop(cx);
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.pressed_at = None;
        self.hold = None;
        self.repaint = None;
        cx.notify();
    }

    fn complete(&mut self, cx: &mut Context<Self>) {
        tracing::debug!("feedback hold: completed");
        self.stop(cx);
        self.swallow_click = true;
        cx.emit(HoldEvent::Completed);
    }

    fn on_click(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.swallow_click) {
            tracing::debug!("feedback hold: the release after a capture is not a click");
            return;
        }
        cx.emit(HoldEvent::Clicked);
    }
}

impl Render for HoldButton {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let progress = self.progress(cx);
        let button = rail_button(RailEntry::Feedback, self.selected, self.icon_color)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_press))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.cancel("released", cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    // No click comes from a release off the button.
                    this.swallow_click = false;
                    this.cancel("released off the button", cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if event.pressed_button != Some(MouseButton::Left) {
                    this.cancel("the left button is up", cx);
                }
            }))
            .on_click(cx.listener(|this, _, _, cx| this.on_click(cx)));
        // Leaving is watched on this area, the button's size, not on the
        // button: the button's tooltip already takes its one hover listener,
        // and the button's hover reads false on any move while it is pressed
        // (it waits for its click), so a tremor would end the hold. This
        // area has no click of its own: its hover follows only where the
        // pointer is.
        div()
            .id(HOLD_AREA_ID)
            .relative()
            .flex_none()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !hovered {
                    this.cancel("the pointer left the button", cx);
                }
            }))
            .child(button)
            .when(self.holding(), |this| {
                this.child(
                    ProgressCircle::new(HOLD_RING_ID)
                        .value(progress * 100.)
                        .absolute()
                        .inset_0()
                        .size_full(),
                )
            })
    }
}

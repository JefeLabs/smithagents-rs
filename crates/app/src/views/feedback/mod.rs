//! The Feedback view in the right dock, and the rail's Feedback button
//! ([`hold`]). The pane shares the dock with Settings and Intents and is
//! drawn only while the rail has it selected ([`ShellState::right_entry`]).
//! For now it shows only its heading.
//!
//! The pane paints its whole area from `Region::RightPane`, like Settings.

mod hold;

pub use hold::{HoldButton, HoldEvent, HOLD_RING_ID};

use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::v_flex;
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription,
    TestSupportExt as _, Window,
};

use crate::feedback::state::Feedback;
use crate::shell::FEEDBACK_PANEL;
use crate::shell_state::ShellState;
use crate::theme_adapter::{region_paint, Region};
use crate::views::pane_heading;
use crate::views::rail::RailEntry;

/// Element id of the pane's heading.
pub const FEEDBACK_TITLE_ID: &str = "feedback-title";

const TITLE: &str = "Feedback";
const HEADING: &str = "FEEDBACK";

pub struct FeedbackPane {
    state: Entity<ShellState>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl FeedbackPane {
    pub fn new(
        state: Entity<ShellState>,
        feedback: Entity<Feedback>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The dock draws the pane from a cache: the theme reaches it through
        // the state, the draft through the feedback entity.
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.observe(&feedback, |_, _, cx| cx.notify()),
        ];
        Self {
            state,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }
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
        let native = self.state.read(cx).native_theme();
        let (surface, foreground) = region_paint(native, Region::RightPane, cx);
        v_flex()
            .id(FEEDBACK_PANEL)
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .bg(surface)
            .text_color(foreground)
            .child(
                pane_heading(FEEDBACK_TITLE_ID, HEADING, foreground)
                    .flex_none()
                    .px_3()
                    .pt_3()
                    .pb_2(),
            )
    }
}

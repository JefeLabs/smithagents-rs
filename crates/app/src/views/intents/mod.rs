//! The Intents view in the right dock. It shares the dock with Settings and
//! is drawn only while the rail has it selected
//! ([`ShellState::right_entry`]).
//!
//! For now it draws only its heading. The pane paints its whole area from
//! `Region::RightPane` (`card`, `card-foreground`), like Settings.

use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::v_flex;
use gpui_kit::{
    div, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window,
};

use crate::shell::INTENTS_PANEL;
use crate::shell_state::ShellState;
use crate::theme_adapter::{region_paint, Region};
use crate::views::rail::RailEntry;

/// Element id of the pane's heading.
pub const INTENTS_TITLE_ID: &str = "intents-title";

const TITLE: &str = "Intents";

pub struct IntentsPane {
    state: Entity<ShellState>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl IntentsPane {
    pub fn new(state: Entity<ShellState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The dock draws the pane from a cache: a theme change reaches it
        // through the state.
        let subscriptions = vec![cx.observe(&state, |_, _, cx| cx.notify())];
        Self {
            state,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }
}

impl BasePanel for IntentsPane {
    fn panel_name(&self) -> &'static str {
        INTENTS_PANEL
    }

    /// Drawn only while the rail has Intents selected, so the dock's group
    /// has one visible panel and draws no tab bar.
    fn visible(&self, cx: &App) -> bool {
        self.state.read(cx).right_entry() == RailEntry::Intents
    }

    /// The dock is locked to the shell's layout; its panels never close.
    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for IntentsPane {
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

impl EventEmitter<PanelEvent> for IntentsPane {}

impl Focusable for IntentsPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for IntentsPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let native = self.state.read(cx).native_theme();
        let (surface, foreground) = region_paint(native, Region::RightPane, cx);
        v_flex()
            .id(INTENTS_PANEL)
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .bg(surface)
            .text_color(foreground)
            .child(
                div()
                    .id(INTENTS_TITLE_ID)
                    // A node needs an id and a role to reach assistive
                    // technology.
                    .role(Role::Heading)
                    .test_support()
                    .aria_label(TITLE)
                    .flex_none()
                    .px_3()
                    .pt_3()
                    .pb_2()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(foreground)
                    .child(TITLE),
            )
    }
}

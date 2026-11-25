//! The left pane's content until Task 13's agents pane replaces it. It reads
//! the pane's [`LeftPaneMode`] from [`ShellState`]: the title when expanded,
//! an icon-only marker when compact.

use gpui_kit::component::{v_flex, ActiveTheme as _, Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, App, Context, Entity, FocusHandle, Focusable, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _,
    Subscription, TestSupportExt as _, Window,
};

use crate::shell_state::ShellState;
use crate::view_models::LeftPaneMode;

/// Element id of the pane's root, which holds the pane's focus.
pub const LEFT_PANE_CONTENT_ID: &str = "agents-pane";
/// Element id of the title shown when expanded.
pub const EXPANDED_TITLE_ID: &str = "agents-title";
/// Element id of the marker shown when compact.
pub const COMPACT_MARKER_ID: &str = "agents-compact-marker";

pub struct LeftPanePlaceholder {
    state: Entity<ShellState>,
    focus: FocusHandle,
    _observe: Subscription,
}

impl LeftPanePlaceholder {
    pub fn new(state: Entity<ShellState>, cx: &mut Context<Self>) -> Self {
        Self {
            _observe: cx.observe(&state, |_, _, cx| cx.notify()),
            state,
            focus: cx.focus_handle(),
        }
    }
}

impl Focusable for LeftPanePlaceholder {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for LeftPanePlaceholder {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.state.read(cx).left_pane_mode();
        let theme = cx.theme();
        // The focus is tracked on the root in both modes, so switching modes
        // never drops a focused pane out of the tree.
        v_flex()
            .id(LEFT_PANE_CONTENT_ID)
            // Observed before `track_focus`, so tests can read its focus.
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .map(|this| match mode {
                LeftPaneMode::Expanded => this
                    .gap_2()
                    .p_3()
                    .child(
                        div()
                            .id(EXPANDED_TITLE_ID)
                            .test_support()
                            .aria_label("Agents")
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child("Agents"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Your agents will be listed here."),
                    ),
                LeftPaneMode::Compact => this.items_center().pt_3().child(
                    div()
                        .id(COMPACT_MARKER_ID)
                        .test_support()
                        .aria_label("Agents")
                        .child(Icon::new(IconName::Bot).text_color(theme.muted_foreground)),
                ),
            })
    }
}

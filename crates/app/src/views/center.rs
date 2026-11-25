//! The center dock panel: the connection banner, the tab strip, and the
//! content area that reserves the web engine's rectangle.

use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::{v_flex, ActiveTheme as _, ElementExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window,
};

use crate::model::Action;
use crate::shell_state::{report_center_bounds, ShellState};
use crate::theme_adapter::{agent_accent, srgb_to_hsla};
use crate::view_models::{center_rect, tab_title, LeftPaneMode};
use crate::views::banner::banner;
use crate::views::tabs::{tab_strip, TabItem, TabStripActions};

/// Element id of the content area whose bounds go to the web engine.
pub const CONTENT_ID: &str = "center-content";
/// Element id of the hint shown when no tab is open.
pub const EMPTY_HINT_ID: &str = "no-chat-hint";
/// What the center says when no tab is open.
pub const EMPTY_HINT: &str = "No chat open";

pub struct CenterPanel {
    state: Entity<ShellState>,
    focus: FocusHandle,
    _observe: Subscription,
}

impl CenterPanel {
    /// The persisted panel name. Never change it once shipped.
    pub const NAME: &'static str = "Center";

    pub fn new(state: Entity<ShellState>, cx: &mut Context<Self>) -> Self {
        Self {
            // Also covers the left pane's mode, which the strip's toggle shows.
            _observe: cx.observe(&state, |_, _, cx| cx.notify()),
            state,
            focus: cx.focus_handle(),
        }
    }

    fn tab_items(&self, cx: &App) -> Vec<TabItem> {
        let state = self.state.read(cx);
        let model = state.model();
        let agents = model.agents();
        model
            .tabs()
            .iter()
            .filter_map(|id| agents.iter().find(|a| a.id == *id))
            .map(|agent| TabItem {
                agent: agent.id,
                title: tab_title(agent).into(),
                accent: state
                    .native_theme()
                    .map(|native| srgb_to_hsla(agent_accent(native, agent.color.h))),
            })
            .collect()
    }

    fn strip_actions(&self) -> TabStripActions {
        let (open, close) = (self.state.downgrade(), self.state.downgrade());
        let toggle = self.state.downgrade();
        TabStripActions {
            activate: Box::new(move |agent, _, cx| {
                let _ = open.update(cx, |state, cx| state.dispatch(Action::OpenTab(agent), cx));
            }),
            close: Box::new(move |agent, _, cx| {
                let _ = close.update(cx, |state, cx| state.dispatch(Action::CloseTab(agent), cx));
            }),
            toggle_left_pane: Box::new(move |_, cx| {
                tracing::debug!(source = "strip", "left pane toggle");
                let _ = toggle.update(cx, |state, cx| state.toggle_left_pane(cx));
            }),
        }
    }
}

impl BasePanel for CenterPanel {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for CenterPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Chats")
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }

    /// The panel draws its own chrome (the tab strip).
    fn title_bar(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for CenterPanel {}

impl Focusable for CenterPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for CenterPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self.tab_items(cx);
        let no_tabs = items.is_empty();
        let state = self.state.read(cx);
        let banner_text = state.banner();
        let active = state.model().active_tab();
        let left_expanded = state.left_pane_mode() == LeftPaneMode::Expanded;
        let report = {
            let state = self.state.downgrade();
            move |bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App| {
                let rect = center_rect(
                    bounds.origin.x.into(),
                    bounds.origin.y.into(),
                    bounds.size.width.into(),
                    bounds.size.height.into(),
                    window.scale_factor(),
                );
                report_center_bounds(&state, rect, cx);
            }
        };
        let theme = cx.theme();

        v_flex()
            .id("center-panel")
            .track_focus(&self.focus)
            .size_full()
            .bg(theme.background)
            .when_some(banner_text, |this, text| this.child(banner(&text, cx)))
            .child(tab_strip(
                items,
                active,
                left_expanded,
                self.strip_actions(),
                cx,
            ))
            .child(
                // Reserved for the web engine, which draws over this rectangle;
                // its bounds are reported on every prepaint that lays it out.
                div()
                    .id(CONTENT_ID)
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .on_prepaint(report)
                    .when(no_tabs, |this| {
                        this.child(
                            v_flex()
                                .id(EMPTY_HINT_ID)
                                .test_support()
                                .aria_label(EMPTY_HINT)
                                .size_full()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(EMPTY_HINT),
                        )
                    }),
            )
    }
}

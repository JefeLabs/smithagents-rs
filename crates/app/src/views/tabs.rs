//! The center tab strip: one tab per open agent chat, plus the control that
//! switches the left (agents) pane between expanded and compact.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{h_flex, ActiveTheme as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, App, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, Window,
};
use smithagents_core::ids::AgentId;

/// Element id of a tab's close button; `ix` is the tab's position.
pub fn close_button_id(ix: usize) -> (&'static str, usize) {
    ("close-tab", ix)
}

/// Element id of the button that compacts or expands the agents pane.
pub const TOGGLE_AGENTS_ID: &str = "toggle-agents-pane";

/// What one tab shows.
#[derive(Debug, Clone, PartialEq)]
pub struct TabItem {
    pub agent: AgentId,
    pub title: SharedString,
    /// The agent's color; `None` only if no theme is known.
    pub accent: Option<Hsla>,
}

/// A callback for one tab, given its agent.
pub type TabCallback = Box<dyn Fn(AgentId, &mut Window, &mut App)>;
/// A callback for a control of the strip itself.
pub type StripCallback = Box<dyn Fn(&mut Window, &mut App)>;

/// The strip's callbacks.
pub struct TabStripActions {
    pub activate: TabCallback,
    pub close: TabCallback,
    pub toggle_left_pane: StripCallback,
}

/// Draws the strip. `active` is highlighted; `left_expanded` picks the
/// toggle's icon (the same icons the dock skin uses).
pub fn tab_strip(
    items: Vec<TabItem>,
    active: Option<AgentId>,
    left_expanded: bool,
    actions: TabStripActions,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let ids: Vec<AgentId> = items.iter().map(|item| item.agent).collect();
    let selected = active.and_then(|active| ids.iter().position(|id| *id == active));
    let TabStripActions {
        activate,
        close,
        toggle_left_pane,
    } = actions;
    let close = std::rc::Rc::new(close);

    let toggle = Button::new(TOGGLE_AGENTS_ID)
        .icon(if left_expanded {
            IconName::PanelLeft
        } else {
            IconName::PanelLeftOpen
        })
        .ghost()
        .xsmall()
        .tab_stop(false)
        .tooltip(if left_expanded {
            "Compact the agents pane"
        } else {
            "Expand the agents pane"
        })
        .accessibility_label("Compact or expand the agents pane")
        .on_click(move |_, window, cx| toggle_left_pane(window, cx));

    let tabs = items.into_iter().enumerate().map(|(ix, item)| {
        let close = close.clone();
        let agent = item.agent;
        Tab::new()
            .aria_label(item.title.clone())
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .when_some(item.accent, |this, accent| {
                        this.child(div().flex_none().size_2().rounded_full().bg(accent))
                    })
                    .child(item.title),
            )
            .suffix(
                Button::new(close_button_id(ix))
                    .icon(IconName::Close)
                    .ghost()
                    .xsmall()
                    .tab_stop(false)
                    .mr_1()
                    .tooltip("Close tab")
                    .accessibility_label("Close tab")
                    .on_click(move |_, window, cx| {
                        // The tab under the button must not also activate.
                        cx.stop_propagation();
                        close(agent, window, cx);
                    }),
            )
    });

    TabBar::new("center-tabs")
        .prefix(
            h_flex()
                .flex_none()
                .h_full()
                .items_center()
                .px_2()
                .border_b_1()
                .border_color(theme.border)
                .child(toggle),
        )
        .children(tabs)
        .when_some(selected, TabBar::selected_index)
        .on_click(move |ix, window, cx| {
            if let Some(agent) = ids.get(*ix) {
                activate(*agent, window, cx);
            }
        })
}

//! The center tab strip: one tab per open agent chat, plus the control that
//! switches the left (agents) pane between expanded and compact.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{h_flex, ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, App, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    SharedString, Styled as _, TestSupportExt as _, Window,
};
use smithagents_core::ids::AgentId;
use smithagents_theme::native::NativeTheme;

use crate::theme_adapter::{region_paint, Region};

/// Element id of a tab's close button; `ix` is the tab's position.
pub fn close_button_id(ix: usize) -> (&'static str, usize) {
    ("close-tab", ix)
}

/// Element id of a tab's title text; `ix` is the tab's position.
pub fn tab_title_id(ix: usize) -> (&'static str, usize) {
    ("tab-title", ix)
}

/// The widest a tab is drawn; a longer title is cut short with an ellipsis,
/// so one long agent name never pushes the other tabs off the strip.
pub const TAB_MAX_WIDTH: Pixels = px(240.);

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
    native: Option<&NativeTheme>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let (_, strip_foreground) = region_paint(native, Region::TabStrip, cx);
    // The active tab sits on the center surface, so its icon follows it.
    let (_, active_foreground) = region_paint(native, Region::Center, cx);
    let ids: Vec<AgentId> = items.iter().map(|item| item.agent).collect();
    let selected = active.and_then(|active| ids.iter().position(|id| *id == active));
    let TabStripActions {
        activate,
        close,
        toggle_left_pane,
    } = actions;
    let close = std::rc::Rc::new(close);

    let toggle = Button::new(TOGGLE_AGENTS_ID)
        .icon(
            Icon::new(if left_expanded {
                IconName::PanelLeft
            } else {
                IconName::PanelLeftOpen
            })
            .text_color(strip_foreground),
        )
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
        let icon_color = if selected == Some(ix) {
            active_foreground
        } else {
            strip_foreground
        };
        let close_label: SharedString = format!("Close {}", item.title).into();
        Tab::new()
            .aria_label(item.title.clone())
            .child(
                h_flex()
                    .min_w_0()
                    .gap_1p5()
                    .items_center()
                    .when_some(item.accent, |this, accent| {
                        this.child(div().flex_none().size_2().rounded_full().bg(accent))
                    })
                    .child(
                        div()
                            .id(tab_title_id(ix))
                            .test_support()
                            .min_w_0()
                            .truncate()
                            .child(item.title),
                    ),
            )
            .suffix(
                Button::new(close_button_id(ix))
                    .icon(Icon::new(IconName::Close).text_color(icon_color))
                    .ghost()
                    .xsmall()
                    .tab_stop(false)
                    .mr_1()
                    .tooltip(close_label.clone())
                    .accessibility_label(close_label)
                    .on_click(move |_, window, cx| {
                        // The tab under the button must not also activate.
                        cx.stop_propagation();
                        close(agent, window, cx);
                    }),
            )
    });

    TabBar::new("center-tabs")
        .max_width(TAB_MAX_WIDTH)
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

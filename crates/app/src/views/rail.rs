//! The icon rail at the far right edge. Each entry shows its view in a dock;
//! v1 has one, Settings, in the right dock. Plan 3b adds Intents here.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::component::{
    v_flex, ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _,
};
use gpui_kit::{
    px, App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _,
    TestSupportExt as _, Window,
};
use smithagents_theme::native::NativeTheme;

use crate::theme_adapter::{region_paint, Region};

/// Element id of the rail.
pub const RAIL_ID: &str = "icon-rail";

/// Width of the rail.
pub const RAIL_WIDTH: f32 = 40.;

/// One rail button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RailEntry {
    Settings,
}

impl RailEntry {
    /// Every entry, top to bottom.
    pub const ALL: &'static [RailEntry] = &[RailEntry::Settings];

    /// The tooltip and accessible name.
    pub fn label(self) -> &'static str {
        match self {
            RailEntry::Settings => "Settings",
        }
    }

    /// The element id of the entry's button.
    pub fn element_id(self) -> ElementId {
        match self {
            RailEntry::Settings => "rail-settings".into(),
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            RailEntry::Settings => IconName::Settings,
        }
    }

    /// The dock the entry's view lives in.
    pub fn placement(self) -> DockPlacement {
        match self {
            RailEntry::Settings => DockPlacement::Right,
        }
    }
}

/// Draws the rail. `active` is the entry whose view is on screen (it draws
/// selected); clicking an entry calls `on_select` with it.
pub fn rail(
    active: Option<RailEntry>,
    on_select: impl Fn(RailEntry, &mut Window, &mut App) + Clone + 'static,
    native: Option<&NativeTheme>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let (surface, foreground) = region_paint(native, Region::Rail, cx);
    v_flex()
        .id(RAIL_ID)
        .test_support()
        .flex_none()
        .h_full()
        .w(px(RAIL_WIDTH))
        .items_center()
        .gap_1()
        .py_2()
        .bg(surface)
        .text_color(foreground)
        .border_l_1()
        .border_color(theme.border)
        .children(RailEntry::ALL.iter().map(|&entry| {
            let on_select = on_select.clone();
            Button::new(entry.element_id())
                .icon(Icon::new(entry.icon()).text_color(foreground))
                .ghost()
                .small()
                .selected(active == Some(entry))
                .tooltip(entry.label())
                .accessibility_label(entry.label())
                .on_click(move |_, window, cx| on_select(entry, window, cx))
        }))
}

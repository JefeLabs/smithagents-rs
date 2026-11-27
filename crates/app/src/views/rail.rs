//! The icon rail at the far right edge. Each entry shows its view in a dock;
//! Settings, Feedback and Intents share the right dock, one at a time: the
//! state keeps which one (`ShellState::right_entry`), and
//! `Shell::show_rail_entry` switches it. The Feedback button is a view of its
//! own ([`HoldButton`](crate::views::feedback::HoldButton)): holding it
//! captures.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::component::{v_flex, ActiveTheme as _, Icon, Selectable as _, Sizable as _};
use gpui_kit::{
    px, AnyElement, App, ElementId, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, TestSupportExt as _, Window,
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
    Feedback,
    Intents,
}

impl RailEntry {
    /// Every entry, top to bottom.
    pub const ALL: &'static [RailEntry] =
        &[RailEntry::Settings, RailEntry::Feedback, RailEntry::Intents];

    /// The tooltip and accessible name.
    pub fn label(self) -> &'static str {
        match self {
            RailEntry::Settings => "Settings",
            RailEntry::Feedback => "Feedback",
            RailEntry::Intents => "Intents",
        }
    }

    /// The element id of the entry's button.
    pub fn element_id(self) -> ElementId {
        match self {
            RailEntry::Settings => "rail-settings".into(),
            RailEntry::Feedback => "rail-feedback".into(),
            RailEntry::Intents => "rail-intents".into(),
        }
    }

    /// The button's icon (from the full catalog; one outside the Kit's
    /// defaults must be in [`AppIcons`](crate::assets::AppIcons)).
    pub fn icon(self) -> IconName {
        match self {
            RailEntry::Settings => IconName::Settings,
            RailEntry::Feedback => IconName::MessageSquarePlus,
            RailEntry::Intents => IconName::Lightbulb,
        }
    }

    /// The dock the entry's view lives in.
    pub fn placement(self) -> DockPlacement {
        match self {
            RailEntry::Settings | RailEntry::Feedback | RailEntry::Intents => DockPlacement::Right,
        }
    }

    /// The name `app-state.json` keeps. Never change one once shipped.
    pub fn as_str(self) -> &'static str {
        match self {
            RailEntry::Settings => "settings",
            RailEntry::Feedback => "feedback",
            RailEntry::Intents => "intents",
        }
    }

    /// The entry [`RailEntry::as_str`] names, or `None` for any other text.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|entry| entry.as_str() == text)
    }
}

/// The button the rail draws for `entry`, without its click: an icon in
/// `foreground`, named and described by the entry's label.
pub(crate) fn rail_button(entry: RailEntry, selected: bool, foreground: Hsla) -> Button {
    Button::new(entry.element_id())
        .icon(Icon::new(entry.icon()).text_color(foreground))
        .ghost()
        .small()
        .selected(selected)
        .tooltip(entry.label())
        .accessibility_label(entry.label())
}

/// Draws the rail. `active` is the entry whose view is on screen (it draws
/// selected); clicking an entry calls `on_select` with it. `feedback` is
/// drawn in place of the plain button for [`RailEntry::Feedback`].
pub fn rail(
    active: Option<RailEntry>,
    on_select: impl Fn(RailEntry, &mut Window, &mut App) + Clone + 'static,
    feedback: AnyElement,
    native: Option<&NativeTheme>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let (surface, foreground) = region_paint(native, Region::Rail, cx);
    let mut feedback = Some(feedback);
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
        .children(RailEntry::ALL.iter().filter_map(|&entry| {
            if entry == RailEntry::Feedback {
                return feedback.take();
            }
            let on_select = on_select.clone();
            let button = rail_button(entry, active == Some(entry), foreground)
                .on_click(move |_, window, cx| on_select(entry, window, cx));
            Some(button.into_any_element())
        }))
}

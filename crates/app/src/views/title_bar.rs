//! The app-drawn title bar across the top of the window. The window opens
//! with a transparent native title bar (traffic lights only); this bar
//! paints the surface, moves the window when dragged and zooms it on a
//! double click (gpui-component's `TitleBar` does both).

use gpui_kit::component::TitleBar;
use gpui_kit::{div, App, IntoElement, ParentElement as _, Styled as _};
use smithagents_theme::native::NativeTheme;

use crate::theme_adapter::{region_paint, Region};
use crate::WINDOW_TITLE;

/// Element id of the title bar (set by `TitleBar` itself).
pub const TITLE_BAR_ID: &str = "title-bar";

/// The bar's height, as gpui-component draws it.
pub const TITLE_BAR_HEIGHT: f32 = 34.;

/// Draws the title bar from the [`Region::TitleBar`] tokens.
pub fn title_bar(native: Option<&NativeTheme>, cx: &App) -> impl IntoElement {
    let (surface, foreground) = region_paint(native, Region::TitleBar, cx);
    TitleBar::new()
        .bg(surface)
        .text_color(foreground)
        .child(div().text_sm().child(WINDOW_TITLE))
}

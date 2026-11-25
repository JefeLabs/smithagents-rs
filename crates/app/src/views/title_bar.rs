//! The app-drawn title bar across the top of the window. The window opens
//! with a transparent native title bar (traffic lights only); this bar
//! paints the surface, moves the window when dragged and zooms it on a
//! double click (gpui-component's `TitleBar` does both).
//!
//! Its height is in rems ([`TITLE_BAR_HEIGHT_REMS`]), so it grows with the
//! text scale.
//!
//! At its top right sits the update chip, shown only while an update needs
//! attention (see [`crate::view_models::update_button`]). The chip takes the
//! press (default prevented, propagation stopped), so clicking it neither
//! starts a window move nor counts toward the bar's double-click zoom.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::{Disableable as _, Sizable as _, TitleBar};
use gpui_kit::{
    div, px, rems, App, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
};
use smithagents_theme::native::NativeTheme;

use crate::layout_store::TITLE_BAR_HEIGHT_REMS;
use crate::theme_adapter::{region_paint, token_paint, Region};
use crate::view_models::UpdateButtonVm;
use crate::WINDOW_TITLE;

/// Element id of the title bar (set by `TitleBar` itself).
pub const TITLE_BAR_ID: &str = "title-bar";
/// Element id of the window title text.
pub const TITLE_TEXT_ID: &str = "title-text";
/// Element id of the update chip.
pub const UPDATE_CHIP_ID: &str = "update-chip";
/// Element id of the chip's wrapper, which shields it from the bar.
pub const UPDATE_CHIP_SLOT_ID: &str = "update-chip-slot";
/// The tokens the chip paints with: its fill and its label (a validated
/// pair, 4.5:1). `primary` also stands apart from the bar's `card-2`, so the
/// chip reads as a control; its hover and pressed fills are gpui-component's
/// primary button states, which the framework derives from `primary`.
pub const UPDATE_CHIP_TOKENS: (&str, &str) = ("primary", "primary-foreground");

/// The chip's height inside the bar, in rems: 22 px at 100% text, growing
/// with the text scale like the bar ([`TITLE_BAR_HEIGHT_REMS`]).
pub const UPDATE_CHIP_HEIGHT_REMS: f32 = 1.375;
/// Space between the chip and the window's right edge.
pub const UPDATE_CHIP_MARGIN: f32 = 8.;

/// Draws the title bar from the [`Region::TitleBar`] tokens, with the update
/// chip at its right when `update` is `Some`; `on_update` runs when it is
/// clicked.
pub fn title_bar(
    native: Option<&NativeTheme>,
    update: Option<UpdateButtonVm>,
    on_update: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let (surface, foreground) = region_paint(native, Region::TitleBar, cx);
    let chip = update.map(|vm| {
        // The fill, hover, pressed and focus states come from the primary
        // variant; the label sets its own color (a `Label` defaults to the
        // theme's foreground).
        let fg = token_paint(native, UPDATE_CHIP_TOKENS.1, cx);
        div()
            .id(UPDATE_CHIP_SLOT_ID)
            .flex_none()
            .pr(px(UPDATE_CHIP_MARGIN))
            // The bar starts a move on a press nothing took, and counts
            // clicks for its zoom: take both here.
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                Button::new(UPDATE_CHIP_ID)
                    .child(Label::new(vm.label).text_xs().text_color(fg))
                    .primary()
                    .xsmall()
                    .h(rems(UPDATE_CHIP_HEIGHT_REMS))
                    .px_2()
                    .rounded_full()
                    .disabled(!vm.enabled)
                    .accessibility_label(vm.a11y_label)
                    .on_click(move |_, window, cx| on_update(window, cx)),
            )
    });
    TitleBar::new()
        // gpui-component draws the bar 34 px high whatever the text size;
        // the same height in rems grows with the text scale.
        .h(rems(TITLE_BAR_HEIGHT_REMS))
        .bg(surface)
        .text_color(foreground)
        .child(
            div()
                .id(TITLE_TEXT_ID)
                .test_support()
                .text_sm()
                .child(WINDOW_TITLE),
        )
        .children(chip)
}

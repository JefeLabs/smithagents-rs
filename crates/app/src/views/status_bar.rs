//! The status bar across the bottom of the window, under every other region.
//!
//! Three zones in one row: the left is reserved (empty for now), the center
//! shows one status message, the right shows the update button (only while
//! there is an update to install) and the version. The left and right zones
//! share the row's spare room equally, so the message is centered in the
//! window while it fits; a long one is truncated with an ellipsis and never
//! reaches the right zone, whose content is never squeezed.
//!
//! The bar is a `Region` landmark named "Status bar"; the message is a
//! `Status` node (a polite live region), like the connection banner.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{h_flex, Disableable as _, Sizable as _};
use gpui_kit::{
    div, px, App, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
};
use smithagents_theme::native::NativeTheme;

use crate::layout_store::STATUS_BAR_HEIGHT;
use crate::theme_adapter::{region_paint, token_paint, Region};
use crate::update::UpdateState;
use crate::view_models::{update_button, version_text, StatusKind, StatusMessage, UpdateButtonVm};

/// Element id of the bar.
pub const STATUS_BAR_ID: &str = "status-bar";
/// Element id of the message (its container; absent when there is none).
pub const STATUS_MESSAGE_ID: &str = "status-message";
/// Element id of the dot before an error message.
pub const STATUS_ERROR_DOT_ID: &str = "status-error-dot";
/// Element id of the version text.
pub const STATUS_VERSION_ID: &str = "status-version";
/// Element id of the update button.
pub const UPDATE_BUTTON_ID: &str = "update-button";
/// Element id of the right zone (update button and version).
pub const STATUS_RIGHT_ID: &str = "status-right";
/// The accessible name of the bar.
pub const STATUS_BAR_LABEL: &str = "Status bar";

const DOT: f32 = 6.;

/// Draws the bar. `on_update` runs when the update button is clicked.
pub fn status_bar(
    message: Option<StatusMessage>,
    update: &UpdateState,
    native: Option<&NativeTheme>,
    on_update: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let (surface, foreground) = region_paint(native, Region::BottomBar, cx);
    let button = update_button(update).map(|vm| update_button_view(vm, foreground, on_update));

    let center = message.map(|message| {
        let error_dot = (message.kind == StatusKind::Error).then(|| {
            div()
                .id(STATUS_ERROR_DOT_ID)
                .test_support()
                .flex_none()
                .size(px(DOT))
                .rounded_full()
                .bg(token_paint(native, "status-error", cx))
        });
        h_flex()
            .id(STATUS_MESSAGE_ID)
            .role(Role::Status)
            .test_support()
            .aria_label(message.text.clone())
            .flex_shrink(1.)
            .min_w_0()
            .items_center()
            .gap_1p5()
            .children(error_dot)
            .child(div().min_w_0().truncate().child(message.text))
    });

    h_flex()
        .id(STATUS_BAR_ID)
        .role(Role::Region)
        .test_support()
        .aria_label(STATUS_BAR_LABEL)
        .flex_none()
        .w_full()
        .h(STATUS_BAR_HEIGHT)
        .items_center()
        .gap_2()
        .px_2()
        .overflow_hidden()
        .bg(surface)
        .text_color(foreground)
        .text_xs()
        // Reserved, so the center stays centered in the window.
        .child(div().flex_1().flex_basis(px(0.)).min_w_0().h_full())
        .child(
            h_flex()
                .flex_shrink(1.)
                .min_w_0()
                .justify_center()
                .overflow_hidden()
                .children(center),
        )
        .child(
            h_flex()
                .id(STATUS_RIGHT_ID)
                .test_support()
                .flex_1()
                .flex_basis(px(0.))
                .justify_end()
                .items_center()
                .gap_2()
                .whitespace_nowrap()
                .children(button)
                .child(
                    div()
                        .id(STATUS_VERSION_ID)
                        .test_support()
                        .flex_none()
                        .child(version_text(smithagents_core::VERSION)),
                ),
        )
}

fn update_button_view(
    vm: UpdateButtonVm,
    foreground: gpui_kit::Hsla,
    on_update: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    Button::new(UPDATE_BUTTON_ID)
        .label(vm.label)
        .ghost()
        .xsmall()
        .text_color(foreground)
        .disabled(!vm.enabled)
        .accessibility_label(vm.a11y_label)
        .on_click(move |_, window, cx| on_update(window, cx))
}

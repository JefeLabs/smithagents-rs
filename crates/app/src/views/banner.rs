//! The connection banner above the center area.

use gpui_kit::component::{v_flex, ActiveTheme as _, Icon, IconName};
use gpui_kit::{
    div, App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
};

use crate::view_models::{BannerSeverity, BannerVm};

/// Element id of the banner, for tests and accessibility.
pub const BANNER_ID: &str = "connection-banner";

/// Draws [`crate::view_models::connection_banner`]: the first line as the
/// headline, any further lines (the link's error) muted below it, after a
/// warning icon, or an error icon for an error. The whole text is the
/// element's accessible label, announced as a status.
pub fn banner(vm: &BannerVm, cx: &App) -> impl IntoElement {
    let text = vm.text.as_str();
    let mut lines = text.lines();
    let headline = lines.next().unwrap_or_default().to_owned();
    let details: Vec<String> = lines.map(str::to_owned).collect();
    let theme = cx.theme();
    let (icon, icon_color) = match vm.severity {
        BannerSeverity::Warning => (IconName::TriangleAlert, theme.warning),
        BannerSeverity::Error => (IconName::CircleX, theme.danger),
    };

    div()
        .id(BANNER_ID)
        .role(Role::Status)
        .test_support()
        .aria_label(text.to_owned())
        .flex()
        .flex_none()
        .w_full()
        .items_start()
        .gap_2()
        .px_3()
        .py_2()
        .bg(theme.muted)
        .border_b_1()
        .border_color(theme.border)
        .child(Icon::new(icon).text_color(icon_color).flex_none())
        .child(
            v_flex()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(headline),
                )
                .children(details.into_iter().map(|line| {
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(line)
                })),
        )
}

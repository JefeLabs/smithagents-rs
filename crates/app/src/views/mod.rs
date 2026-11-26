//! The shell's GPUI views. Colors come only from the active gpui-component
//! theme (`cx.theme()`) and, for regions, tokens and agent accents, from
//! [`crate::theme_adapter`]; nothing here writes a literal color.

pub mod agents;
pub mod banner;
pub mod center;
pub mod feedback;
pub mod intents;
pub mod rail;
pub mod settings;
pub mod status_bar;
pub mod tabs;
pub mod title_bar;

use futures::future::{select, Either, LocalBoxFuture};
use gpui_kit::{
    div, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Role, SharedString, StatefulInteractiveElement as _, Styled, Task, TestSupportExt as _,
};

/// `command`'s answer, or `None` if `timer` (the command timeout) ends first.
/// Giving up drops the future; the daemon may still finish the command, and
/// its result then arrives through its events.
pub(crate) async fn answered<T>(command: LocalBoxFuture<'static, T>, timer: Task<()>) -> Option<T> {
    match select(command, timer).await {
        Either::Left((answer, _)) => Some(answer),
        Either::Right(((), _)) => None,
    }
}

/// A pane's heading: `title` in small semibold type in `color`, a heading
/// named `title` for assistive technology. The caller places it (padding,
/// truncation).
pub(crate) fn pane_heading(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    color: Hsla,
) -> impl IntoElement + Styled {
    let title = title.into();
    div()
        .id(id)
        // A node needs an id and a role to reach assistive technology.
        .role(Role::Heading)
        .test_support()
        .aria_label(title.clone())
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(title)
}

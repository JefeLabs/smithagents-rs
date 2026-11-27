//! The shell's GPUI views. Colors come only from the active gpui-component
//! theme (`cx.theme()`) and, for regions, tokens and agent accents, from
//! [`crate::theme_adapter`]; nothing here writes a literal color.

pub mod agents;
pub mod banner;
pub mod center;
pub mod intents;
pub mod rail;
pub mod settings;
pub mod status_bar;
pub mod tabs;
pub mod title_bar;

use futures::future::{select, Either, LocalBoxFuture};
use gpui_kit::Task;

/// `command`'s answer, or `None` if `timer` (the command timeout) ends first.
/// Giving up drops the future; the daemon may still finish the command, and
/// its result then arrives through its events.
pub(crate) async fn answered<T>(command: LocalBoxFuture<'static, T>, timer: Task<()>) -> Option<T> {
    match select(command, timer).await {
        Either::Left((answer, _)) => Some(answer),
        Either::Right(((), _)) => None,
    }
}

//! The shell's GPUI views. Colors come only from the active gpui-component
//! theme (`cx.theme()`) and, for regions, tokens and agent accents, from
//! [`crate::theme_adapter`]; nothing here writes a literal color.

pub mod agents;
pub mod banner;
pub mod center;
pub mod placeholder;
pub mod rail;
pub mod tabs;
pub mod title_bar;

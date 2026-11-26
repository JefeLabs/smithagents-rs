//! Feedback: the annotation model, the annotated PNG, the checks on attached
//! files and the draft on disk (pure code: no GPUI, no screen), plus the
//! [`state::Feedback`] entity that holds the draft and runs the capture. No
//! views here.

pub mod annotation;
pub mod draft;
pub mod files;
pub mod render;
pub mod state;
pub mod text;
mod work;

//! Feedback: the annotation model, the annotated PNG, the checks on attached
//! files, the draft on disk and the Submit screen's view model (pure code:
//! no GPUI, no screen), plus the [`state::Feedback`] entity that holds the
//! draft, runs the capture and keeps the pending submission. No views here.

pub mod annotation;
pub mod draft;
pub mod files;
pub mod render;
pub mod state;
pub mod submit_vm;
pub mod text;
mod work;

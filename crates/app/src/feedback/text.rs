//! The limits and the exact sentences the feedback feature shows.

use std::time::Duration;

/// How long the Feedback button is held to capture.
pub const HOLD: Duration = Duration::from_secs(3);
/// A box must be at least this many points each way.
pub const MIN_BOX_POINTS: f32 = 8.0;
pub const MAX_BOXES: usize = 20;
pub const MAX_COMMENT_CHARS: usize = 500;
pub const THUMBNAIL_MAX_WIDTH: u32 = 320;
pub const MAX_ATTACHMENTS: usize = 10;
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_VIDEO_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 100 * 1024 * 1024;

pub const PERMISSION_TEXT: &str = "SmithAgents needs Screen Recording permission to capture its window. Allow it in System Settings → Privacy & Security → Screen Recording, then try again.";
pub const WINDOW_NOT_FOUND_TEXT: &str =
    "Could not find the SmithAgents window to capture. Try again.";
pub const UNSUPPORTED_TEXT: &str =
    "Capturing the window is not supported on this system. You can still add files.";
pub const DRAFT_FULL_TEXT: &str = "A draft holds at most 10 attachments. Remove one first.";
pub const BROKEN_DRAFT_TEXT: &str =
    "The saved feedback draft could not be read and was set aside. This is a new, empty draft.";
pub const TOO_MANY_BOXES_TEXT: &str = "A screenshot can have at most 20 boxes.";
pub const COMMENT_TOO_LONG_TEXT: &str = "A comment can be at most 500 characters.";
pub const SUBMIT_TOOLTIP: &str = "Submitting arrives with the next update";
pub const COMMENT_HINT: &str = "Enter saves · Esc drops";
pub const EMPTY_TEXT: &str =
    "No feedback yet. Hold the Feedback button for 3 seconds, or press Capture.";
pub const HELP_TITLE: &str = "How to submit feedback";
pub const HELP_LINES: [&str; 3] = [
    "Hold the Feedback button for 3 seconds, or press Capture, to take a picture of this window.",
    "Drag a box over what you mean and type a comment. Add as many boxes as you need.",
    "Add more screenshots or files. The draft is kept until you discard it.",
];
pub const DISCARD_TITLE: &str = "Discard this feedback?";
pub const DISCARD_CANCEL: &str = "Cancel";
pub const DISCARD_CONFIRM: &str = "Discard";
pub const ANNOTATOR_DISCARD_TITLE: &str = "Discard this screenshot?";
pub const ANNOTATOR_DISCARD_KEEP: &str = "Keep";
pub const ANNOTATOR_DISCARD_CONFIRM: &str = "Discard";

pub fn capture_failed_text(message: &str) -> String {
    format!("Could not capture the window: {message}.")
}

pub fn picker_failed_text(error: &str) -> String {
    format!("Could not open the file picker: {error}.")
}

pub fn save_failed_text(error: &str) -> String {
    format!("Could not save the feedback draft: {error}.")
}

pub fn missing_files_text(count: usize) -> String {
    if count == 1 {
        "1 attachment was removed because its file is missing.".to_string()
    } else {
        format!("{count} attachments were removed because their files are missing.")
    }
}

pub fn annotator_title(number: usize, total: usize) -> String {
    format!("Screenshot {number} of {total} · Drag a box over what you mean")
}

pub fn card_title(number: usize, comments: usize) -> String {
    if comments == 1 {
        format!("Screenshot {number} · 1 comment")
    } else {
        format!("Screenshot {number} · {comments} comments")
    }
}

pub fn discard_description(attachments: usize) -> String {
    if attachments == 1 {
        "1 attachment and its comments are deleted. This cannot be undone.".to_string()
    } else {
        format!("{attachments} attachments and their comments are deleted. This cannot be undone.")
    }
}

pub fn annotator_discard_description(comments: usize) -> String {
    if comments == 1 {
        "Its 1 comment is deleted.".to_string()
    } else {
        format!("Its {comments} comments are deleted.")
    }
}

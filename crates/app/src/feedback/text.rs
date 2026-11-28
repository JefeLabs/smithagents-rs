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
/// The pane keeps at most this many notices; one more line counts the rest.
pub const MAX_NOTICES: usize = 20;
/// How long a capture waits for its two frames before it gives up.
pub const CAPTURE_FRAMES_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a capture waits for the picture itself before it gives up.
pub const CAPTURE_PICTURE_TIMEOUT: Duration = Duration::from_secs(10);

/// macOS keeps reporting the permission as missing until the app restarts
/// after it is granted, hence "quit and reopen" rather than "try again".
pub const PERMISSION_TEXT: &str = "SmithAgents needs Screen Recording permission to capture its window. Allow it in System Settings → Privacy & Security → Screen Recording, then quit and reopen SmithAgents.";
pub const WINDOW_NOT_FOUND_TEXT: &str =
    "Could not find the SmithAgents window to capture. Try again.";
pub const UNSUPPORTED_TEXT: &str =
    "Capturing the window is not supported on this system. You can still add files.";
pub const DRAFT_FULL_TEXT: &str = "A draft holds at most 10 attachments. Remove one first.";
pub const BROKEN_DRAFT_TEXT: &str =
    "The saved feedback draft could not be read and was set aside. This is a new, empty draft.";
pub const TOO_MANY_BOXES_TEXT: &str = "A screenshot can have at most 20 boxes.";
pub const COMMENT_TOO_LONG_TEXT: &str = "A comment can be at most 500 characters.";
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
/// Shown in the annotator, where the picture would be, when it does not decode.
pub const PICTURE_UNREADABLE_TEXT: &str =
    "This screenshot could not be read. Discard it and capture again.";
/// The [`capture_failed_text`] reason when the window never drew the two
/// frames a capture waits for (a hidden or minimized window).
pub const CAPTURE_FRAMES_MISSED: &str = "the window was not drawn";
/// The [`save_failed_text`] reason while a damaged draft that could not be
/// set aside is still in the draft's place: nothing is written over it.
pub const DRAFT_NOT_SET_ASIDE: &str =
    "the saved draft could not be read or set aside, so it is left untouched";
/// The [`save_failed_text`] reason when the draft's folder is a link (or a
/// file): nothing is read, written or deleted through it.
pub const DRAFT_FOLDER_NOT_A_FOLDER: &str = "the draft's folder is a link or a file, not a folder";
/// The [`capture_failed_text`] reason when the picture did not come within
/// [`CAPTURE_PICTURE_TIMEOUT`].
pub const CAPTURE_TOO_SLOW: &str = "taking the picture took too long";

pub fn capture_failed_text(message: &str) -> String {
    format!("Could not capture the window: {message}.")
}

/// The annotated picture could not be drawn (its screenshot does not
/// decode, or the drawing could not be encoded): not a failed save.
pub fn draw_failed_text(error: &str) -> String {
    format!("Could not draw the boxes on the screenshot: {error}.")
}

/// The files of a pick that were not looked at once the draft was full.
pub fn more_files_text(count: usize) -> String {
    if count == 1 {
        "1 more file was not added: the draft is full.".to_string()
    } else {
        format!("{count} more files were not added: the draft is full.")
    }
}

/// The line after [`MAX_NOTICES`] notices, counting the ones not shown.
pub fn more_notices_text(count: usize) -> String {
    format!("…and {count} more.")
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

/// What loading removed from the saved draft: `missing` attachments whose
/// files are gone, and `over_limit` beyond [`MAX_ATTACHMENTS`]. `None` when
/// nothing was removed.
pub fn removed_attachments_text(missing: usize, over_limit: usize) -> Option<String> {
    let total = missing + over_limit;
    let (count, verb) = if total == 1 {
        ("1 attachment".to_string(), "was")
    } else {
        (format!("{total} attachments"), "were")
    };
    match (missing, over_limit) {
        (0, 0) => None,
        (_, 0) => Some(missing_files_text(missing)),
        (0, _) => Some(format!(
            "{count} {verb} removed because a draft holds at most 10."
        )),
        _ => Some(format!(
            "{count} {verb} removed because their files are missing or a draft holds at most 10."
        )),
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
    if attachments == 0 {
        "The draft is deleted. This cannot be undone.".to_string()
    } else if attachments == 1 {
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

// ---- Submit ------------------------------------------------------------------

/// The most characters a main idea may have (the daemon's limit).
pub const MAIN_IDEA_MAX_CHARS: usize = 20_000;

/// Why "Submit…" is disabled on an empty draft.
pub const SUBMIT_EMPTY_TOOLTIP: &str = "Add a screenshot or a file first";
pub const MAIN_IDEA_LABEL: &str = "Main idea";
pub const MAIN_IDEA_HINT: &str = "The first line is the title.";
pub const KIND_LABEL: &str = "Kind";
pub const REPO_LABEL: &str = "Repository";
pub const SEND_AS_LABEL: &str = "Send it as";
pub const SEND_AS_INTENT: &str = "An intent to work on now";
pub const SEND_AS_ISSUE: &str = "A GitHub issue";
/// The repository button while repositories exist but none is chosen.
pub const CHOOSE_REPO_TEXT: &str = "Choose a repository";
pub const BACK_LABEL: &str = "Back";
pub const CREATE_INTENT_LABEL: &str = "Create intent";
pub const CREATE_ISSUE_LABEL: &str = "Create issue";
pub const RETRY_LABEL: &str = "Retry";
pub const START_OVER_LABEL: &str = "Start over";

/// Why the issue choice is unavailable.
pub const NO_REMOTE_REASON: &str = "This repository has no GitHub remote.";
pub const GH_MISSING_REASON: &str = "Install the GitHub CLI (gh) to create issues.";
pub const GH_SIGNED_OUT_REASON: &str = "Sign in first: gh auth login";
/// The daemon's reason while the feedback plugin is disabled, failed to load
/// or was stopped.
pub const GH_TURNED_OFF: &str = "turned off or not running";
pub const PLUGIN_OFF_REASON: &str = "The feedback plugin is turned off or not running.";

/// Any other reason the daemon gives for `gh` not being ready.
pub fn gh_not_ready_reason(reason: &str) -> String {
    format!("The GitHub CLI (gh) is not ready: {reason}.")
}

/// The idea package's box under "An intent to work on now", and its parts.
pub const IDEA_LABEL: &str = "Prepare an idea PR for teammates";
pub const IDEA_DEMO_LABEL: &str = "Demo video";
pub const IDEA_DIAGRAMS_LABEL: &str = "Diagrams";
pub const IDEA_EXPLAINER_LABEL: &str = "Explainer doc";
/// Under "Demo video": what it needs, or why it cannot be chosen.
pub const DEMO_HELP: &str = "Only works for projects a browser can show.";
pub const DEMO_MISSING_REASON: &str = "Install agent-browser to record a demo video.";
/// The summary while the idea package is ticked with none of its parts.
pub const IDEA_NEEDS_PART: &str = "Choose at least one part.";

pub const PUBLIC_WARNING: &str =
    "This repository is public. The screenshots will be visible to anyone.";
pub const MAYBE_PUBLIC_WARNING: &str =
    "This repository may be public. The screenshots will be visible to anyone.";
/// The same for a plain intent: its agent opens a pull request there.
pub const INTENT_PUBLIC_WARNING: &str =
    "This repository is public. The agent's pull request will include the screenshots, visible to anyone.";
pub const INTENT_MAYBE_PUBLIC_WARNING: &str =
    "This repository may be public. The agent's pull request will include the screenshots, visible to anyone.";
/// The question asked before every send to a repository that is (or may
/// be) public; the warning is its description.
pub const PUBLIC_CONFIRM_TITLE: &str = "Send the screenshots to GitHub?";
pub const PUBLIC_CONFIRM_CANCEL: &str = "Cancel";

pub fn summary_intent(attachments: &str) -> String {
    format!(
        "Creates the issue folder with {attachments} on a new branch, and an intent to review and send."
    )
}

pub fn summary_issue(attachments: &str) -> String {
    format!(
        "Creates the issue folder with {attachments}, pushes the branch to origin and opens a GitHub issue."
    )
}

/// `parts` is already joined ("an explainer doc, diagrams and a demo video").
pub fn summary_idea(attachments: &str, parts: &str) -> String {
    format!(
        "Creates the issue folder with {attachments}. An agent adds {parts} on an idea branch, and you open the pull request."
    )
}

pub fn result_intent(title: &str, branch: &str, sha7: &str) -> String {
    format!("Created the intent “{title}”. Files: {branch} ({sha7}).")
}

pub fn result_issue(url: &str, branch: &str, sha7: &str) -> String {
    format!("Created {url}. Files: {branch} ({sha7}).")
}

/// The result of an issue whose address is not one the app shows (not a
/// github.com issue address).
pub fn result_issue_unnamed(branch: &str, sha7: &str) -> String {
    format!("Created a GitHub issue. Files: {branch} ({sha7}).")
}

pub fn partial_text(branch: &str, sha7: &str, message: &str) -> String {
    format!("The files are committed on {branch} ({sha7}), but: {message}")
}

pub const NOTHING_CREATED_TEXT: &str = "Nothing was created. Submit again.";

/// Instead of [`NOTHING_CREATED_TEXT`] when the daemon has no record of a
/// submission whose draft names its commit: that commit is still there.
pub fn no_record_text(branch: &str, sha7: &str) -> String {
    format!(
        "The daemon has no record of this submission. Its files are still committed on {branch} ({sha7}). Submit again to send it as a new one."
    )
}

/// An idea intent's actions in its detail, and the question "Open pull
/// request…" asks first (the public-repository warning is part of it).
pub const OPEN_PR_LABEL: &str = "Open pull request…";
pub const PR_TITLE: &str = "Open a pull request?";
pub const PR_DESCRIPTION: &str = "Pushes the idea branch to origin and opens a draft pull request. If the repository is public, the screenshots will be visible to anyone.";
pub const OPEN_PR_CONFIRM_LABEL: &str = "Open pull request";
/// "Create implementation intent…" and the question it asks first.
pub const IMPLEMENT_LABEL: &str = "Create implementation intent…";
pub const IMPLEMENT_TITLE: &str = "Create an implementation intent?";
pub const IMPLEMENT_DESCRIPTION: &str = "Creates an intent to build this idea. Its agent's pull request will include the idea's screenshots and explainer. If the repository is public, they will be visible to anyone.";
pub const IMPLEMENT_CONFIRM_LABEL: &str = "Create implementation intent";
/// The tooltip of the button that copies the pull request's address.
pub const COPY_LINK_LABEL: &str = "Copy link";
pub const REPO_GONE_TEXT: &str = "That repository no longer exists.";
/// The Submit screen's Repository line when no repository has source
/// control (Feedback works by committing files), and the daemon's refusal of
/// a submission to a plain folder.
pub const NEEDS_SOURCE_CONTROL_TEXT: &str = "Feedback needs a repository with source control.";
pub const LINK_STOPPED_TEXT: &str = "The daemon link has stopped";
/// "Start over"'s tooltip.
pub const START_OVER_TOOLTIP: &str = "Keep the screenshots and text, and send as a new submission.";
/// What Start over asks first while the daemon may still be making the
/// submission (it took the request and did not answer).
pub const START_OVER_TITLE: &str = "Start over?";
pub const START_OVER_RISK_TEXT: &str =
    "The daemon may still be finishing this submission. Starting over can create it twice.";
pub const START_OVER_CANCEL: &str = "Cancel";
pub const ISSUE_UNCONFIRMED_TEXT: &str =
    "GitHub has not confirmed whether the issue was created. Try again in a minute.";
/// A change to the draft while its submission is sent or under way.
pub const SUBMISSION_LOCKED_TEXT: &str =
    "This feedback was sent or is being sent. Retry or start over in Submit before changing it.";
/// The Submit screen of a draft locked with no answer to show (a quit or a
/// crash while it was sent, or an answer the app could not use).
pub const MAY_HAVE_BEEN_SENT_TEXT: &str = "This feedback may already have been sent. Press Retry to find out, or Start over to send it as a new submission.";
/// Added to Discard's question while something may exist under the draft's
/// submission.
pub const DISCARD_PENDING_TEXT: &str = "A submission of this draft may still be in progress. Discarding deletes the screenshots here; anything already sent stays.";
/// Why a press of Create or Retry sent nothing.
pub const DRAFT_BUSY_TEXT: &str = "The draft is still being changed. Try again when it is done.";
pub const SUBMIT_BUSY_TEXT: &str = "A submission is already under way.";
pub const DRAFT_NOT_READY_TEXT: &str = "The draft is not ready to send yet. Try again in a moment.";
/// The submission could not be written to the draft's folder first, which
/// is what keeps the draft locked across a quit or a lost connection.
pub const SUBMISSION_NOT_SAVED_TEXT: &str =
    "The submission could not be saved in the draft's folder, so nothing was sent. Try again.";
pub const MAIN_IDEA_TOO_LONG_TEXT: &str = "The main idea can be at most 20,000 characters.";
pub const UPLOAD_TOO_LARGE_TEXT: &str =
    "The attachments cannot be sent: together they are larger than 100 MB.";

/// Why attachment `number` (1-based) cannot be sent; `reason` ends with a
/// full stop.
pub fn attachment_refused_text(number: usize, reason: &str) -> String {
    format!("Attachment {number} cannot be sent: {reason}")
}

pub const TOO_MANY_BOXES_REASON: &str = "a screenshot can have at most 20 boxes.";
pub const COMMENT_TOO_LONG_REASON: &str = "a comment can be at most 500 characters.";
pub const IMAGE_TOO_LARGE_REASON: &str = "an image can be at most 10 MB.";
pub const VIDEO_TOO_LARGE_REASON: &str = "a video can be at most 50 MB.";

//! The Submit screen's view model: every sentence, choice and check it
//! draws, derived without GPUI (or a disk) so it can be unit-tested.
//!
//! Nothing of a daemon answer is shown but its fixed parts: the message of a
//! refusal the daemon answered as JSON (one line, bounded), the step message
//! of a partial failure, and a github.com issue address that checks out.

use std::borrow::Cow;

use smithagents_client::{ClientError, FeedbackFile};
use smithagents_core::agent::Repo;
use smithagents_core::feedback::{
    AttachmentKind as WireKind, FeedbackAttachment, FeedbackOutcome, FeedbackTarget, IdeaOptions,
    SendAs, ToolReadiness,
};
use smithagents_core::ids::RepoId;

use super::draft::{AttachmentKind, Draft};
use super::text::{
    attachment_refused_text, gh_not_ready_reason, partial_text, result_intent, result_issue,
    result_issue_unnamed, summary_idea, summary_intent, summary_issue, COMMENT_TOO_LONG_REASON,
    CREATE_INTENT_LABEL, CREATE_ISSUE_LABEL, DRAFT_FULL_TEXT, GH_MISSING_REASON,
    GH_SIGNED_OUT_REASON, GH_TURNED_OFF, IMAGE_TOO_LARGE_REASON, ISSUE_UNCONFIRMED_TEXT,
    MAIN_IDEA_MAX_CHARS, MAIN_IDEA_TOO_LONG_TEXT, MAX_ATTACHMENTS, MAX_BOXES, MAX_COMMENT_CHARS,
    MAX_IMAGE_BYTES, MAX_TOTAL_BYTES, MAX_VIDEO_BYTES, MAYBE_PUBLIC_WARNING, NOTHING_CREATED_TEXT,
    NO_REMOTE_REASON, PLUGIN_OFF_REASON, PUBLIC_WARNING, TOO_MANY_BOXES_REASON,
    UPLOAD_TOO_LARGE_TEXT, VIDEO_TOO_LARGE_REASON,
};
use crate::link::LinkError;
use crate::view_models::{
    capitalized, one_line, DAEMON_UNREACHABLE_TEXT, FILE_UNREADABLE_TEXT, NOT_CONNECTED_TEXT,
};

/// How many characters of a daemon's message a line shows.
const MESSAGE_CHARS: usize = 300;
/// How many characters of the daemon's reason for a tool a line shows.
const REASON_CHARS: usize = 120;
/// The daemon's reason when `gh auth status` failed, and when `gh` is not
/// installed.
const GH_NOT_SIGNED_IN: &str = "not signed in";
const GH_NOT_INSTALLED: &str = "not installed";
/// What a failed command says when the daemon's answer has no sentence to show.
const COULD_NOT_TEXT: &str = "The daemon could not do that.";

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// "1 screenshot", "2 screenshots and 1 file", "3 files": screenshots first;
/// files are the images and videos.
pub fn attachments_phrase(draft: &Draft) -> String {
    let screenshots = draft.screenshot_count();
    let files = draft.attachments.len() - screenshots;
    let shots = plural(screenshots, "screenshot", "screenshots");
    let others = plural(files, "file", "files");
    match (screenshots, files) {
        (_, 0) => shots,
        (0, _) => others,
        _ => format!("{shots} and {others}"),
    }
}

/// `text` on one line, at most [`MESSAGE_CHARS`] characters ("…" when cut).
fn bounded(text: &str) -> String {
    bounded_to(text, MESSAGE_CHARS)
}

/// `text` on one line, at most `chars` characters ("…" when cut).
fn bounded_to(text: &str, chars: usize) -> String {
    let line = one_line(text);
    let mut kept: String = line.chars().take(chars).collect();
    if kept.len() < line.len() {
        kept.push('…');
    }
    kept
}

/// Why "A GitHub issue" cannot be chosen for `target`; `None` when it can.
/// Without a target or a github.com remote, [`NO_REMOTE_REASON`]; then
/// `gh`'s readiness as the daemon reported it: signed out, not installed,
/// the feedback plugin off, or any other reason in the daemon's words.
pub fn issue_reason(
    target: Option<&FeedbackTarget>,
    gh: &ToolReadiness,
) -> Option<Cow<'static, str>> {
    if !target.is_some_and(|target| target.has_github_remote) {
        return Some(NO_REMOTE_REASON.into());
    }
    if gh.ready {
        return None;
    }
    Some(match gh.reason.as_deref().map(str::trim) {
        Some(GH_NOT_SIGNED_IN) => GH_SIGNED_OUT_REASON.into(),
        None | Some("") | Some(GH_NOT_INSTALLED) => GH_MISSING_REASON.into(),
        Some(GH_TURNED_OFF) => PLUGIN_OFF_REASON.into(),
        Some(other) => gh_not_ready_reason(&bounded_to(other, REASON_CHARS)).into(),
    })
}

/// The warning shown for a GitHub issue: the repository is public, or its
/// visibility is not known (also when nothing is known of it). An intent
/// never leaves the machine, so it has none.
pub fn public_warning(send_as: SendAs, target: Option<&FeedbackTarget>) -> Option<&'static str> {
    if send_as != SendAs::GithubIssue {
        return None;
    }
    match target.and_then(|target| target.is_public) {
        Some(true) => Some(PUBLIC_WARNING),
        Some(false) => None,
        None => Some(MAYBE_PUBLIC_WARNING),
    }
}

/// The idea package's parts as a phrase: "an explainer doc, diagrams and a
/// demo video".
fn idea_parts(idea: IdeaOptions) -> String {
    let parts: Vec<&str> = [
        (idea.explainer, "an explainer doc"),
        (idea.diagrams, "diagrams"),
        (idea.demo_video, "a demo video"),
    ]
    .into_iter()
    .filter_map(|(chosen, part)| chosen.then_some(part))
    .collect();
    match parts.as_slice() {
        [] => String::new(),
        [only] => (*only).to_owned(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    }
}

/// What Create will do, in one sentence.
pub fn summary(send_as: SendAs, idea: Option<IdeaOptions>, draft: &Draft) -> String {
    let attachments = attachments_phrase(draft);
    match (send_as, idea) {
        (SendAs::Intent, Some(idea)) => summary_idea(&attachments, &idea_parts(idea)),
        (SendAs::Intent, None) => summary_intent(&attachments),
        (SendAs::GithubIssue, _) => summary_issue(&attachments),
    }
}

pub fn confirm_label(send_as: SendAs) -> &'static str {
    match send_as {
        SendAs::Intent => CREATE_INTENT_LABEL,
        SendAs::GithubIssue => CREATE_ISSUE_LABEL,
    }
}

/// The repository the screen starts on: the last used while it exists, else
/// the meta repository while it exists, else none (the user chooses).
pub fn starting_repo(
    repos: &[Repo],
    last_used: Option<RepoId>,
    meta: Option<RepoId>,
) -> Option<RepoId> {
    let exists = |id: &RepoId| repos.iter().any(|repo| repo.id == *id);
    last_used.filter(exists).or_else(|| meta.filter(exists))
}

/// Whether Create can be pressed: a main idea with some text, a repository,
/// an issue only while the issue choice is available, and an idea package
/// only for an intent and with at least one part.
pub fn can_confirm(
    main_idea: &str,
    repo: Option<RepoId>,
    send_as: SendAs,
    issue_reason: Option<&str>,
    idea: Option<IdeaOptions>,
) -> bool {
    let idea_ok = idea.is_none_or(|idea| {
        send_as == SendAs::Intent && (idea.explainer || idea.diagrams || idea.demo_video)
    });
    !main_idea.trim().is_empty()
        && repo.is_some()
        && !(send_as == SendAs::GithubIssue && issue_reason.is_some())
        && idea_ok
}

/// Our sentence for a daemon error code a submission or a retry can answer
/// with. Every other code shows the daemon's own (fixed) message.
fn feedback_code_text(code: &str) -> Option<&'static str> {
    Some(match code {
        "submission_not_found" => NOTHING_CREATED_TEXT,
        "issue_unconfirmed" => ISSUE_UNCONFIRMED_TEXT,
        "repo_not_found" => "That repository no longer exists.",
        _ => return None,
    })
}

/// What a failed submission or retry says, as one line, by
/// [`intent_error_text`](crate::intent_vm::intent_error_text)'s rules: our
/// sentence for a known code, else the daemon's message (first letter
/// capitalized, on one line, bounded). An answer that was not the daemon's
/// JSON (`http_<status>`: a proxy's page, a plain-text rejection) shows
/// none of its body.
pub fn submit_error_text(error: &LinkError) -> String {
    match error {
        LinkError::NotConnected => NOT_CONNECTED_TEXT.to_owned(),
        LinkError::Stopped => "The daemon link has stopped".to_owned(),
        LinkError::Client(ClientError::Http(_) | ClientError::Ws(_)) => {
            DAEMON_UNREACHABLE_TEXT.to_owned()
        }
        LinkError::Client(ClientError::File(_)) => FILE_UNREADABLE_TEXT.to_owned(),
        LinkError::Client(ClientError::Api { code, message, .. }) => {
            if let Some(text) = feedback_code_text(code) {
                return text.to_owned();
            }
            let message = bounded(message);
            if code.starts_with("http_") || message.is_empty() {
                COULD_NOT_TEXT.to_owned()
            } else {
                capitalized(&message)
            }
        }
    }
}

/// The first 7 characters of a commit id.
fn sha7(commit: &str) -> String {
    commit.chars().take(7).collect()
}

/// Whether `url` is an issue address the app shows: exactly
/// `https://github.com/<owner>/<name>/issues/<number>`, names of the
/// characters GitHub allows.
pub fn shows_issue_url(url: &str) -> bool {
    let Some(path) = url.strip_prefix("https://github.com/") else {
        return false;
    };
    let name = |part: &str| {
        (1..=100).contains(&part.len())
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    };
    match path.split('/').collect::<Vec<_>>().as_slice() {
        [owner, repo, "issues", number] => {
            name(owner)
                && name(repo)
                && (1..=20).contains(&number.len())
                && number.bytes().all(|b| b.is_ascii_digit())
        }
        _ => false,
    }
}

/// The success sentence of a complete outcome; `None` while a step failed.
pub fn result_text(outcome: &FeedbackOutcome) -> Option<String> {
    if outcome.failed.is_some() {
        return None;
    }
    let branch = bounded(&outcome.branch);
    let sha7 = sha7(&outcome.commit);
    Some(match (&outcome.intent, &outcome.issue_url) {
        (Some(intent), _) => result_intent(&bounded(&intent.content.title), &branch, &sha7),
        (None, Some(url)) if shows_issue_url(url) => result_issue(url, &branch, &sha7),
        (None, _) => result_issue_unnamed(&branch, &sha7),
    })
}

/// What an outcome whose step failed says: where the files are committed and
/// the daemon's sentence for the step; `None` for a complete one.
pub fn failure_text(outcome: &FeedbackOutcome) -> Option<String> {
    let failed = outcome.failed.as_ref()?;
    let message = match feedback_code_text(&failed.code) {
        Some(text) => text.to_owned(),
        None => bounded(&failed.message),
    };
    Some(partial_text(
        &bounded(&outcome.branch),
        &sha7(&outcome.commit),
        &message,
    ))
}

/// The attachment a part (`file-<i>` or `annotated-<i>`) belongs to, and
/// whether it is the annotated picture.
fn part_of(part: &str) -> Option<(usize, bool)> {
    let (number, annotated) = match part.strip_prefix("annotated-") {
        Some(number) => (number, true),
        None => (part.strip_prefix("file-")?, false),
    };
    Some((number.parse().ok()?, annotated))
}

/// Why the daemon would refuse this upload, checked before anything is
/// sent (a real client may see a dropped connection instead of the
/// daemon's sentence): at most [`MAX_ATTACHMENTS`]; a main idea of at most
/// [`MAIN_IDEA_MAX_CHARS`] characters; per screenshot at most [`MAX_BOXES`]
/// boxes with comments of at most [`MAX_COMMENT_CHARS`] characters; per
/// file at most [`MAX_IMAGE_BYTES`] (an image, a screenshot or its annotated
/// picture) or [`MAX_VIDEO_BYTES`] (a video); every file together at most
/// [`MAX_TOTAL_BYTES`]. `files` pairs each part with its size on disk
/// (`None`: it cannot be read). `None` when the daemon would take it.
pub fn upload_problem(
    main_idea: &str,
    attachments: &[FeedbackAttachment],
    files: &[(FeedbackFile, Option<u64>)],
) -> Option<String> {
    if attachments.len() > MAX_ATTACHMENTS {
        return Some(DRAFT_FULL_TEXT.to_owned());
    }
    if main_idea.chars().nth(MAIN_IDEA_MAX_CHARS).is_some() {
        return Some(MAIN_IDEA_TOO_LONG_TEXT.to_owned());
    }
    for (at, attachment) in attachments.iter().enumerate() {
        let number = at + 1;
        if attachment.boxes.len() > MAX_BOXES {
            return Some(attachment_refused_text(number, TOO_MANY_BOXES_REASON));
        }
        let long = attachment
            .boxes
            .iter()
            .any(|b| b.comment.chars().nth(MAX_COMMENT_CHARS).is_some());
        if long {
            return Some(attachment_refused_text(number, COMMENT_TOO_LONG_REASON));
        }
    }
    let mut total = 0u64;
    for (file, size) in files {
        let Some(size) = *size else {
            return Some(FILE_UNREADABLE_TEXT.to_owned());
        };
        let Some((number, annotated)) = part_of(&file.part) else {
            continue;
        };
        let video = !annotated
            && attachments
                .get(number.wrapping_sub(1))
                .is_some_and(|a| a.kind == WireKind::Video);
        let (limit, reason) = if video {
            (MAX_VIDEO_BYTES, VIDEO_TOO_LARGE_REASON)
        } else {
            (MAX_IMAGE_BYTES, IMAGE_TOO_LARGE_REASON)
        };
        if size > limit {
            return Some(attachment_refused_text(number, reason));
        }
        total = total.saturating_add(size);
    }
    (total > MAX_TOTAL_BYTES).then(|| UPLOAD_TOO_LARGE_TEXT.to_owned())
}

/// The wire kind of a draft attachment.
pub fn wire_kind(kind: AttachmentKind) -> WireKind {
    match kind {
        AttachmentKind::Screenshot => WireKind::Screenshot,
        AttachmentKind::Image => WireKind::Image,
        AttachmentKind::Video => WireKind::Video,
    }
}

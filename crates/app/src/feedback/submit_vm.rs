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
    AttachmentKind as WireKind, FeedbackAttachment, FeedbackKind, FeedbackOutcome, FeedbackTarget,
    IdeaOptions, SendAs, ToolReadiness,
};
use smithagents_core::ids::RepoId;

use super::draft::{AttachmentKind, Draft};
use super::text::{
    attachment_refused_text, gh_not_ready_reason, no_record_text, partial_text, result_intent,
    result_issue, result_issue_unnamed, summary_idea, summary_intent, summary_issue,
    COMMENT_TOO_LONG_REASON, CREATE_INTENT_LABEL, CREATE_ISSUE_LABEL, DEMO_HELP,
    DEMO_MISSING_REASON, DRAFT_FULL_TEXT, GH_MISSING_REASON, GH_SIGNED_OUT_REASON, GH_TURNED_OFF,
    IDEA_NEEDS_PART, IMAGE_TOO_LARGE_REASON, INTENT_MAYBE_PUBLIC_WARNING, INTENT_PUBLIC_WARNING,
    ISSUE_UNCONFIRMED_TEXT, LINK_STOPPED_TEXT, MAIN_IDEA_MAX_CHARS, MAIN_IDEA_TOO_LONG_TEXT,
    MAX_ATTACHMENTS, MAX_BOXES, MAX_COMMENT_CHARS, MAX_IMAGE_BYTES, MAX_TOTAL_BYTES,
    MAX_VIDEO_BYTES, MAYBE_PUBLIC_WARNING, NOTHING_CREATED_TEXT, NO_REMOTE_REASON,
    PLUGIN_OFF_REASON, PUBLIC_WARNING, REPO_GONE_TEXT, TOO_MANY_BOXES_REASON,
    UPLOAD_TOO_LARGE_TEXT, VIDEO_TOO_LARGE_REASON,
};
use crate::link::LinkError;
use crate::view_models::{
    capitalized, one_line, DAEMON_UNREACHABLE_TEXT, FILE_UNREADABLE_TEXT, NOT_CONNECTED_TEXT,
};

/// The feedback kinds, in the order the Submit screen offers them.
pub const KINDS: [FeedbackKind; 3] = [
    FeedbackKind::NewFeature,
    FeedbackKind::Enhancement,
    FeedbackKind::Bug,
];

/// The error codes of the refusals after which the daemon has recorded and
/// committed nothing under the submission's id, so the draft of a first
/// send stays editable and the same id can be sent again. (A re-upload is
/// sent only because an earlier request under the id went unanswered, and
/// that request may still make its commit: a refusal of the re-upload says
/// nothing of it, which is why the caller keeps a re-upload locked.) The
/// one place this list is kept. It mirrors `FeedbackError::code` and
/// `FeedbackError::status` in `crates/server/src/feedback/mod.rs` and where
/// each is raised before a record or a commit exists: `upload::check_request`
/// and `upload::stage` (`invalid`, `unsupported_file`, `payload_too_large`;
/// `invalid` also for an idea package with no part or sent as an issue),
/// `FeedbackService::require_github` (`no_github_remote`, `gh_not_ready`),
/// `FeedbackService::github_of` (`origin_unreadable`: git cannot read the
/// repository, its folder gone or no longer a git repository; asked by
/// `FeedbackService::precheck`, also through `require_github`),
/// `FeedbackService::require_tool` (`tool_missing`, also an idea package's
/// demo video without `agent-browser`), `FeedbackService::check_publication`
/// (which runs before a commit is made or one left by a stopped daemon is
/// taken up, so nothing is recorded under the id when it refuses):
/// `unpushed_commits` from `repo::require_published_default_branch` (for an
/// issue: the default branch has commits `origin` has not got, or nothing
/// was ever fetched; for an idea package to a GitHub repository: the same,
/// once it was fetched) and `public_not_acknowledged` from
/// `FeedbackService::acknowledgement` (for an issue, or a plain intent to a
/// GitHub repository, that is or may be public, sent without the user's
/// acknowledgement), and `repo::place_and_commit` (`default_branch_missing`,
/// `git_identity_missing`, `no_free_name`, `name_taken`, `folder_blocked`,
/// `branch_blocked`). They count only as a 4xx answer: the daemon sends
/// `git_failed` and `io_error` as 500, and any 5xx locks. (For a recorded
/// submission, `unpushed_commits` before a push and `origin_unreadable` are
/// failed steps instead.)
pub const NOTHING_MADE_CODES: [&str; 15] = [
    "invalid",
    "unsupported_file",
    "payload_too_large",
    "no_github_remote",
    "gh_not_ready",
    "origin_unreadable",
    TOOL_MISSING,
    "default_branch_missing",
    "git_identity_missing",
    "no_free_name",
    "name_taken",
    "folder_blocked",
    "branch_blocked",
    "unpushed_commits",
    PUBLIC_NOT_ACKNOWLEDGED,
];

/// The daemon's code for a GitHub issue or a plain intent to a repository
/// that is or may be public, sent without the user's acknowledgement: the
/// app asks ([`maybe_public_warning`]) and sends again with it.
pub const PUBLIC_NOT_ACKNOWLEDGED: &str = "public_not_acknowledged";

/// The daemon's code when a tool the submission needs is not ready: only
/// `agent-browser`, for an idea package's demo video.
pub const TOOL_MISSING: &str = "tool_missing";

/// What a failed send (or retry) leaves under the submission's id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendFailure {
    /// The request provably never left: not connected, the daemon refused
    /// the connection, or a local file could not be opened. The draft stays
    /// as it was.
    NotSent,
    /// The daemon refused it and made nothing ([`NOTHING_MADE_CODES`]): the
    /// draft stays editable, under the same id.
    Refused,
    /// The daemon has no such repository: the id is dropped, the draft stays
    /// editable.
    RepoGone,
    /// The daemon has no record of the id (`submission_not_found`).
    NotRecorded,
    /// Something may exist under the id: a 5xx, an unknown code, a transport
    /// failure after the request was sent. The draft is locked.
    MayExist,
}

/// Whether the daemon refused a request because the repository is or may
/// be public and the user did not acknowledge it ([`PUBLIC_NOT_ACKNOWLEDGED`]).
pub fn asks_to_acknowledge(error: &LinkError) -> bool {
    matches!(error, LinkError::Client(ClientError::Api { code, .. }) if code == PUBLIC_NOT_ACKNOWLEDGED)
}

pub fn send_failure(error: &LinkError) -> SendFailure {
    match error {
        LinkError::NotConnected | LinkError::Client(ClientError::File(_)) => SendFailure::NotSent,
        LinkError::Client(ClientError::Http(e)) if e.is_connect() => SendFailure::NotSent,
        LinkError::Stopped | LinkError::Client(ClientError::Http(_) | ClientError::Ws(_)) => {
            SendFailure::MayExist
        }
        LinkError::Client(ClientError::Api { status, code, .. }) => match code.as_str() {
            "submission_not_found" => SendFailure::NotRecorded,
            "repo_not_found" => SendFailure::RepoGone,
            code if *status < 500 && NOTHING_MADE_CODES.contains(&code) => SendFailure::Refused,
            _ => SendFailure::MayExist,
        },
    }
}

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

/// The warning shown (and asked about before a send) when the screenshots
/// will reach GitHub: for a GitHub issue, unless the repository is known to
/// be private (also when nothing is known of it); for a plain intent to a
/// repository with a GitHub remote, whose agent opens a pull request there,
/// unless it is known to be private. An idea package has none: nothing of
/// it leaves the machine before "Open pull request…" or "Create
/// implementation intent…", which ask themselves. Neither has one for a
/// repository without a GitHub remote.
pub fn public_warning(
    send_as: SendAs,
    idea: Option<IdeaOptions>,
    target: Option<&FeedbackTarget>,
) -> Option<&'static str> {
    let known = target.and_then(|target| target.is_public);
    match (send_as, idea) {
        (SendAs::GithubIssue, _) => match known {
            Some(true) => Some(PUBLIC_WARNING),
            Some(false) => None,
            None => Some(MAYBE_PUBLIC_WARNING),
        },
        (SendAs::Intent, Some(_)) => None,
        (SendAs::Intent, None) => match known {
            _ if !target.is_some_and(|target| target.has_github_remote) => None,
            Some(true) => Some(INTENT_PUBLIC_WARNING),
            Some(false) => None,
            None => Some(INTENT_MAYBE_PUBLIC_WARNING),
        },
    }
}

/// What is asked when the daemon refuses a send to a repository the app
/// took for private, or knew nothing of (`public_not_acknowledged`): that it
/// may be public, in the words of `send_as`.
pub fn maybe_public_warning(send_as: SendAs) -> &'static str {
    match send_as {
        SendAs::GithubIssue => MAYBE_PUBLIC_WARNING,
        SendAs::Intent => INTENT_MAYBE_PUBLIC_WARNING,
    }
}

/// The parts ticking "Prepare an idea PR for teammates" starts with: the
/// explainer doc.
pub const IDEA_START: IdeaOptions = IdeaOptions {
    explainer: true,
    diagrams: false,
    demo_video: false,
};

/// Whether an idea package has any part.
fn has_part(idea: IdeaOptions) -> bool {
    idea.explainer || idea.diagrams || idea.demo_video
}

/// What "Demo video" says under it: what it needs while `agent-browser` is
/// ready, else how to get it.
pub fn demo_help(agent_browser_ready: bool) -> &'static str {
    if agent_browser_ready {
        DEMO_HELP
    } else {
        DEMO_MISSING_REASON
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

/// What Create will do, in one sentence; for an idea package with no part,
/// that one is needed.
pub fn summary(send_as: SendAs, idea: Option<IdeaOptions>, draft: &Draft) -> String {
    let attachments = attachments_phrase(draft);
    match (send_as, idea) {
        (SendAs::Intent, Some(idea)) if !has_part(idea) => IDEA_NEEDS_PART.to_owned(),
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
    let idea_ok = idea.is_none_or(|idea| send_as == SendAs::Intent && has_part(idea));
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
        "repo_not_found" => REPO_GONE_TEXT,
        TOOL_MISSING => DEMO_MISSING_REASON,
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
        LinkError::Stopped => LINK_STOPPED_TEXT.to_owned(),
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
    is_github_address(url, "issues")
}

/// Whether `url` is exactly `https://github.com/<owner>/<name>/<kind>/<number>`
/// (`kind` is `issues` or `pull`), names of the characters GitHub allows, a
/// number of 1 to 20 digits, nothing after it.
pub(crate) fn is_github_address(url: &str, kind: &str) -> bool {
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
        [owner, repo, segment, number] if *segment == kind => {
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

/// What the screen says when the daemon has no record of the submission
/// (`submission_not_found`): with the draft's recorded `outcome` (its files
/// were committed, a later step failed), where those files still are, its
/// branch and commit bounded as in [`failure_text`]; without one,
/// [`NOTHING_CREATED_TEXT`].
pub fn not_recorded_text(outcome: Option<&FeedbackOutcome>) -> String {
    match outcome {
        Some(outcome) => no_record_text(&bounded(&outcome.branch), &sha7(&outcome.commit)),
        None => NOTHING_CREATED_TEXT.to_owned(),
    }
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

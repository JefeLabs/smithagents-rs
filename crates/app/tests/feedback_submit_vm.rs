//! The Submit screen's view model: every sentence, choice and check it draws,
//! without GPUI.

mod common;

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use common::feedback_submit::{outcome_with_intent, outcome_with_issue};
use common::intents::{intent, FakeIntents};
use smithagents_app::feedback::draft::{Attachment, AttachmentId, AttachmentKind, Draft};
use smithagents_app::feedback::submit_vm::{
    attachments_phrase, can_confirm, confirm_label, issue_reason, public_warning, result_text,
    starting_repo, submit_error_text, summary, upload_problem,
};
use smithagents_app::feedback::text::{
    DRAFT_FULL_TEXT, GH_MISSING_REASON, GH_SIGNED_OUT_REASON, MAYBE_PUBLIC_WARNING,
    NOTHING_CREATED_TEXT, NO_REMOTE_REASON, PUBLIC_WARNING,
};
use smithagents_app::link::LinkError;
use smithagents_app::view_models::{
    DAEMON_UNREACHABLE_TEXT, FILE_UNREADABLE_TEXT, NOT_CONNECTED_TEXT,
};
use smithagents_client::{ClientError, FeedbackFile};
use smithagents_core::agent::Repo;
use smithagents_core::feedback::{
    AttachmentKind as WireKind, FeedbackAttachment, FeedbackBox, FeedbackFailure, FeedbackStep,
    FeedbackTarget, IdeaOptions, SendAs, SubmissionId, ToolReadiness,
};
use smithagents_core::ids::RepoId;
use smithagents_core::intent::{IntentState, Phase};

const MB: u64 = 1024 * 1024;

fn attachment(kind: AttachmentKind, n: usize) -> Attachment {
    let stored_name = match kind {
        AttachmentKind::Screenshot => format!("screenshot-{n}.png"),
        AttachmentKind::Image => format!("image-{n}.png"),
        AttachmentKind::Video => format!("clip-{n}.mp4"),
    };
    Attachment {
        id: AttachmentId::new(),
        kind,
        original_name: stored_name.clone(),
        stored_name,
        size: 10,
        width: None,
        height: None,
        boxes: Vec::new(),
        rendered: true,
    }
}

fn draft_of(kinds: &[AttachmentKind]) -> Draft {
    Draft {
        attachments: kinds
            .iter()
            .enumerate()
            .map(|(i, kind)| attachment(*kind, i + 1))
            .collect(),
        ..Draft::default()
    }
}

fn target(has_github_remote: bool, is_public: Option<bool>) -> FeedbackTarget {
    FeedbackTarget {
        repo_id: RepoId::new(),
        has_github_remote,
        is_public,
    }
}

fn ready() -> ToolReadiness {
    ToolReadiness {
        ready: true,
        reason: None,
    }
}

fn not_ready(reason: Option<&str>) -> ToolReadiness {
    ToolReadiness {
        ready: false,
        reason: reason.map(str::to_owned),
    }
}

fn repo(path: &str, created: &str) -> Repo {
    let at: DateTime<Utc> = format!("2026-11-{created}T09:00:00Z").parse().unwrap();
    Repo {
        id: RepoId::new(),
        path: path.into(),
        default_branch: "main".into(),
        created_at: at,
    }
}

#[test]
fn attachment_phrases() {
    use AttachmentKind::{Image, Screenshot, Video};
    for (kinds, phrase) in [
        (&[Screenshot][..], "1 screenshot"),
        (&[Screenshot, Screenshot][..], "2 screenshots"),
        (&[Image][..], "1 file"),
        (
            &[Screenshot, Video, Screenshot][..],
            "2 screenshots and 1 file",
        ),
        (&[Image, Video, Image][..], "3 files"),
    ] {
        assert_eq!(attachments_phrase(&draft_of(kinds)), phrase, "{kinds:?}");
    }
}

#[test]
fn the_issue_choice_says_why_it_is_unavailable() {
    let with_remote = target(true, Some(false));
    let reason = |target: Option<&FeedbackTarget>, gh: &ToolReadiness| {
        issue_reason(target, gh).map(|reason| reason.into_owned())
    };
    assert_eq!(reason(None, &ready()).as_deref(), Some(NO_REMOTE_REASON));
    assert_eq!(
        reason(Some(&target(false, None)), &ready()).as_deref(),
        Some(NO_REMOTE_REASON)
    );
    assert_eq!(
        reason(Some(&with_remote), &not_ready(Some("not signed in"))).as_deref(),
        Some(GH_SIGNED_OUT_REASON)
    );
    assert_eq!(
        reason(Some(&with_remote), &not_ready(Some("not installed"))).as_deref(),
        Some(GH_MISSING_REASON)
    );
    assert_eq!(
        reason(Some(&with_remote), &not_ready(None)).as_deref(),
        Some(GH_MISSING_REASON)
    );
    assert_eq!(reason(Some(&with_remote), &ready()), None);

    // The sentences, verbatim.
    assert_eq!(NO_REMOTE_REASON, "This repository has no GitHub remote.");
    assert_eq!(
        GH_MISSING_REASON,
        "Install the GitHub CLI (gh) to create issues."
    );
    assert_eq!(GH_SIGNED_OUT_REASON, "Sign in first: gh auth login");
}

#[test]
fn any_other_readiness_shows_the_daemons_reason() {
    let with_remote = target(true, Some(false));
    let reason = |text: &str| {
        issue_reason(Some(&with_remote), &not_ready(Some(text))).map(|reason| reason.into_owned())
    };
    assert_eq!(
        reason("turned off or not running").as_deref(),
        Some("The feedback plugin is turned off or not running.")
    );
    assert_eq!(
        reason("too old").as_deref(),
        Some("The GitHub CLI (gh) is not ready: too old.")
    );
    // On one line, and never longer than a sentence.
    let long = format!("line one\nline two {}", "x".repeat(500));
    let shown = reason(&long).unwrap();
    assert!(!shown.contains('\n'), "{shown}");
    assert!(shown.chars().count() < 300, "{}", shown.len());
}

#[test]
fn the_public_warning_depends_on_what_is_known() {
    assert_eq!(
        public_warning(SendAs::GithubIssue, Some(&target(true, Some(true)))),
        Some(PUBLIC_WARNING)
    );
    assert_eq!(
        public_warning(SendAs::GithubIssue, Some(&target(true, None))),
        Some(MAYBE_PUBLIC_WARNING)
    );
    assert_eq!(
        public_warning(SendAs::GithubIssue, Some(&target(true, Some(false)))),
        None
    );
    for is_public in [Some(true), None, Some(false)] {
        assert_eq!(
            public_warning(SendAs::Intent, Some(&target(true, is_public))),
            None
        );
    }
    assert_eq!(public_warning(SendAs::Intent, None), None);
    // Nothing known about the repository: it may be public.
    assert_eq!(
        public_warning(SendAs::GithubIssue, None),
        Some(MAYBE_PUBLIC_WARNING)
    );
    assert_eq!(
        PUBLIC_WARNING,
        "This repository is public. The screenshots will be visible to anyone."
    );
    assert_eq!(
        MAYBE_PUBLIC_WARNING,
        "This repository may be public. The screenshots will be visible to anyone."
    );
}

#[test]
fn summaries() {
    use AttachmentKind::{Image, Screenshot};
    let one = draft_of(&[Screenshot]);
    assert_eq!(
        summary(SendAs::Intent, None, &one),
        "Creates the issue folder with 1 screenshot on a new branch, and an intent to review and send."
    );
    assert_eq!(
        summary(SendAs::GithubIssue, None, &one),
        "Creates the issue folder with 1 screenshot, pushes the branch to origin and opens a GitHub issue."
    );
    let three = draft_of(&[Screenshot, Image, Screenshot]);
    assert_eq!(
        summary(SendAs::GithubIssue, None, &three),
        "Creates the issue folder with 2 screenshots and 1 file, pushes the branch to origin and opens a GitHub issue."
    );
    let idea = IdeaOptions {
        explainer: true,
        diagrams: true,
        demo_video: true,
    };
    assert_eq!(
        summary(SendAs::Intent, Some(idea), &one),
        "Creates the issue folder with 1 screenshot. An agent adds an explainer doc, diagrams and a demo video on an idea branch, and you open the pull request."
    );
}

#[test]
fn the_confirm_button_names_what_it_creates() {
    assert_eq!(confirm_label(SendAs::Intent), "Create intent");
    assert_eq!(confirm_label(SendAs::GithubIssue), "Create issue");
}

#[test]
fn the_starting_repository() {
    let alpha = repo("/work/alpha", "20");
    let beta = repo("/work/beta", "21");
    let repos = [alpha.clone(), beta.clone()];
    let gone = RepoId::new();
    assert_eq!(
        starting_repo(&repos, Some(beta.id), Some(alpha.id)),
        Some(beta.id),
        "the last used while it exists"
    );
    assert_eq!(
        starting_repo(&repos, Some(gone), Some(alpha.id)),
        Some(alpha.id),
        "else the meta repository"
    );
    assert_eq!(starting_repo(&repos, None, Some(beta.id)), Some(beta.id));
    assert_eq!(starting_repo(&repos, Some(gone), None), None, "else none");
    assert_eq!(starting_repo(&repos, None, Some(gone)), None);
    assert_eq!(starting_repo(&[], None, None), None);
}

#[test]
fn confirm_needs_a_main_idea_and_a_repository() {
    let repo = Some(RepoId::new());
    assert!(can_confirm(
        "Fix the zoom",
        repo,
        SendAs::Intent,
        None,
        None
    ));
    assert!(can_confirm(
        "Fix the zoom",
        repo,
        SendAs::GithubIssue,
        None,
        None
    ));
    assert!(!can_confirm("", repo, SendAs::Intent, None, None));
    assert!(!can_confirm(" \n\t ", repo, SendAs::Intent, None, None));
    assert!(!can_confirm(
        "Fix the zoom",
        None,
        SendAs::Intent,
        None,
        None
    ));
    assert!(!can_confirm(
        "Fix the zoom",
        repo,
        SendAs::GithubIssue,
        Some(NO_REMOTE_REASON),
        None
    ));
    // A reason for the issue choice does not stop an intent.
    assert!(can_confirm(
        "Fix the zoom",
        repo,
        SendAs::Intent,
        Some(NO_REMOTE_REASON),
        None
    ));
}

#[test]
fn result_sentences() {
    let id = SubmissionId::new();
    let created = intent(
        "fix-the-zoom",
        IntentState::Draft,
        Phase::Polishing { pass: 1 },
    );
    let repo = created.repo_id;
    assert_eq!(
        result_text(&outcome_with_intent(id, created.clone())).as_deref(),
        Some(
            "Created the intent “Fix the zoom”. Files: feedback/2026-11-26-fix-the-zoom (0123abc)."
        )
    );
    assert_eq!(
        result_text(&outcome_with_issue(
            id,
            repo,
            "https://github.com/o/r/issues/7"
        ))
        .as_deref(),
        Some("Created https://github.com/o/r/issues/7. Files: feedback/2026-11-26-fix-the-zoom (0123abc).")
    );
    let mut partial = outcome_with_intent(id, created);
    partial.intent = None;
    partial.failed = Some(FeedbackFailure {
        step: FeedbackStep::CreateIntent,
        code: "store_error".into(),
        message: "the intent could not be saved".into(),
    });
    assert_eq!(result_text(&partial), None);
}

#[test]
fn only_a_github_issue_address_is_shown() {
    let id = SubmissionId::new();
    let repo = RepoId::new();
    for url in [
        "http://github.com/o/r/issues/7",
        "https://github.com.evil.test/o/r/issues/7",
        "https://evil.test/o/r/issues/7",
        "javascript:alert(1)",
        "https://github.com/o/r/issues/7\nmore",
    ] {
        let text = result_text(&outcome_with_issue(id, repo, url)).unwrap();
        assert_eq!(
            text, "Created a GitHub issue. Files: feedback/2026-11-26-fix-the-zoom (0123abc).",
            "{url:?}"
        );
    }
}

fn api(status: u16, code: &str, message: &str) -> LinkError {
    FakeIntents::api_error(status, code, message)
}

#[test]
fn error_sentences() {
    assert_eq!(
        submit_error_text(&api(
            409,
            "default_branch_missing",
            "the repository's default branch main does not exist locally"
        )),
        "The repository's default branch main does not exist locally"
    );
    assert_eq!(
        submit_error_text(&LinkError::NotConnected),
        NOT_CONNECTED_TEXT
    );
    assert_eq!(
        submit_error_text(&api(404, "submission_not_found", "no such submission")),
        NOTHING_CREATED_TEXT
    );
    assert_eq!(NOTHING_CREATED_TEXT, "Nothing was created. Submit again.");
    assert_eq!(
        submit_error_text(&api(404, "repo_not_found", "repo not found")),
        "That repository no longer exists."
    );
    assert_eq!(
        submit_error_text(&api(409, "issue_unconfirmed", "anything")),
        "GitHub has not confirmed whether the issue was created. Try again in a minute."
    );
}

#[test]
fn the_daemons_codes_show_its_own_sentence() {
    // The codes the feedback routes answer with that have no sentence of the
    // app's: each shows the daemon's fixed message, first letter capitalized.
    for (code, message) in [
        (
            "name_taken",
            "an issue folder or branch of this name was created meanwhile; submit again",
        ),
        (
            "folder_blocked",
            "docs/issues is a file in this repository, so feedback cannot be stored there.",
        ),
        (
            "branch_blocked",
            "a branch named feedback exists in this repository, so no branch can be created under feedback/",
        ),
        (
            "evidence_in_repo",
            "this intent's evidence is in its repository",
        ),
        (
            "commit_missing",
            "The feedback commit is no longer in this repository, so it cannot be pushed.",
        ),
        ("gh_bad_answer", "GitHub CLI (gh) gave an answer that is not an issue address"),
        ("no_github_remote", "the repository's origin is not a GitHub repository"),
        ("a_code_from_a_newer_daemon", "something new went wrong"),
    ] {
        let text = submit_error_text(&api(409, code, message));
        let mut chars = message.chars();
        let expected: String = chars
            .next()
            .unwrap()
            .to_uppercase()
            .chain(chars)
            .collect();
        assert_eq!(text, expected, "{code}");
    }
}

#[test]
fn nothing_but_the_daemons_fixed_message_is_shown() {
    // A body that is not the daemon's JSON (a proxy's page, a plain-text
    // rejection) is never shown.
    let text = submit_error_text(&api(413, "http_413", "<html>length limit exceeded</html>"));
    assert!(!text.contains("html") && !text.contains("length"), "{text}");
    // A long or many-lined message is one bounded line.
    let text = submit_error_text(&api(
        409,
        "x",
        &format!("first\nsecond {}", "y".repeat(800)),
    ));
    assert!(
        !text.contains('\n') && text.chars().count() <= 301,
        "{text}"
    );
    // A local file that could not be opened, and a broken connection.
    let file = LinkError::Client(ClientError::File(std::io::Error::other("gone")));
    assert_eq!(submit_error_text(&file), FILE_UNREADABLE_TEXT);
    let http = reqwest::Client::new()
        .get("not a url")
        .build()
        .expect_err("an invalid URL is a reqwest error");
    assert_eq!(
        submit_error_text(&LinkError::Client(ClientError::Http(http))),
        DAEMON_UNREACHABLE_TEXT
    );
}

fn wire(kind: WireKind, boxes: Vec<FeedbackBox>) -> FeedbackAttachment {
    FeedbackAttachment {
        kind,
        original_name: "a.png".into(),
        boxes,
    }
}

fn feedback_box(comment: &str) -> FeedbackBox {
    FeedbackBox {
        x: 0.1,
        y: 0.1,
        w: 0.2,
        h: 0.2,
        comment: comment.into(),
    }
}

fn part(name: &str, bytes: Option<u64>) -> (FeedbackFile, Option<u64>) {
    (
        FeedbackFile {
            part: name.into(),
            path: PathBuf::from(format!("/draft/{name}")),
        },
        bytes,
    )
}

#[test]
fn the_daemons_limits_are_checked_before_anything_is_sent() {
    let shot = wire(WireKind::Screenshot, vec![feedback_box("the toolbar")]);
    let fine = [part("file-1", Some(MB)), part("annotated-1", Some(MB))];
    assert_eq!(
        upload_problem("Fix the zoom", std::slice::from_ref(&shot), &fine),
        None
    );

    // Ten attachments at most.
    let eleven: Vec<_> = (0..11).map(|_| wire(WireKind::Image, vec![])).collect();
    assert_eq!(
        upload_problem("Fix the zoom", &eleven, &[]).as_deref(),
        Some(DRAFT_FULL_TEXT)
    );
    // The main idea, in characters (not bytes).
    let idea = "é".repeat(20_000);
    assert_eq!(
        upload_problem(&idea, std::slice::from_ref(&shot), &fine),
        None
    );
    assert_eq!(
        upload_problem(&format!("{idea}é"), std::slice::from_ref(&shot), &fine).as_deref(),
        Some("The main idea can be at most 20,000 characters.")
    );
    // Boxes and comments, in characters.
    let many = wire(
        WireKind::Screenshot,
        (0..21).map(|_| feedback_box("x")).collect(),
    );
    assert_eq!(
        upload_problem("Fix the zoom", &[many], &fine).as_deref(),
        Some("Attachment 1 cannot be sent: a screenshot can have at most 20 boxes.")
    );
    let at_limit = wire(WireKind::Screenshot, vec![feedback_box(&"é".repeat(500))]);
    assert_eq!(upload_problem("Fix the zoom", &[at_limit], &fine), None);
    let long = wire(WireKind::Screenshot, vec![feedback_box(&"é".repeat(501))]);
    assert_eq!(
        upload_problem("Fix the zoom", &[shot.clone(), long], &fine).as_deref(),
        Some("Attachment 2 cannot be sent: a comment can be at most 500 characters.")
    );
    // Each file's size, the annotated image included.
    let image = wire(WireKind::Image, vec![]);
    let video = wire(WireKind::Video, vec![]);
    let two = [image.clone(), video.clone()];
    assert_eq!(
        upload_problem(
            "Fix the zoom",
            &two,
            &[part("file-1", Some(10 * MB)), part("file-2", Some(50 * MB))]
        ),
        None
    );
    assert_eq!(
        upload_problem(
            "Fix the zoom",
            &two,
            &[part("file-1", Some(10 * MB + 1)), part("file-2", Some(MB))]
        )
        .as_deref(),
        Some("Attachment 1 cannot be sent: an image can be at most 10 MB.")
    );
    assert_eq!(
        upload_problem(
            "Fix the zoom",
            &two,
            &[part("file-1", Some(MB)), part("file-2", Some(50 * MB + 1))]
        )
        .as_deref(),
        Some("Attachment 2 cannot be sent: a video can be at most 50 MB.")
    );
    assert_eq!(
        upload_problem(
            "Fix the zoom",
            std::slice::from_ref(&shot),
            &[
                part("file-1", Some(MB)),
                part("annotated-1", Some(10 * MB + 1))
            ]
        )
        .as_deref(),
        Some("Attachment 1 cannot be sent: an image can be at most 10 MB.")
    );
    // 100 MB in all, every part counted.
    let shots: Vec<_> = (0..6).map(|_| wire(WireKind::Screenshot, vec![])).collect();
    let mut parts = Vec::new();
    for i in 1..=6 {
        parts.push(part(&format!("file-{i}"), Some(9 * MB)));
        parts.push(part(&format!("annotated-{i}"), Some(9 * MB)));
    }
    assert_eq!(
        upload_problem("Fix the zoom", &shots, &parts).as_deref(),
        Some("The attachments cannot be sent: together they are larger than 100 MB.")
    );
    // A file that cannot be read.
    assert_eq!(
        upload_problem(
            "Fix the zoom",
            std::slice::from_ref(&shot),
            &[part("file-1", None), part("annotated-1", Some(MB))]
        )
        .as_deref(),
        Some(FILE_UNREADABLE_TEXT)
    );
}

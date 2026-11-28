//! The Submit screen's view model: every sentence, choice and check it draws,
//! without GPUI.

mod common;

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use common::feedback_submit::{outcome_with_intent, outcome_with_issue};
use common::intents::{intent, FakeIntents};
use smithagents_app::feedback::draft::{Attachment, AttachmentId, AttachmentKind, Draft};
use smithagents_app::feedback::submit_vm::{
    asks_to_acknowledge, attachments_phrase, can_confirm, confirm_label, demo_help, failure_text,
    feedback_repos, first_send_failure, issue_reason, maybe_public_warning, not_recorded_text,
    public_warning, repo_line_text, result_text, send_failure, starting_repo, submit_error_text,
    summary, upload_problem, SendFailure, IDEA_START, KINDS,
};
use smithagents_app::feedback::text::{
    DEMO_HELP, DEMO_MISSING_REASON, DRAFT_FULL_TEXT, GH_MISSING_REASON, GH_SIGNED_OUT_REASON,
    IDEA_DEMO_LABEL, IDEA_DIAGRAMS_LABEL, IDEA_EXPLAINER_LABEL, IDEA_LABEL, IDEA_NEEDS_PART,
    INTENT_MAYBE_PUBLIC_WARNING, INTENT_PUBLIC_WARNING, LINK_STOPPED_TEXT, MAYBE_PUBLIC_WARNING,
    NEEDS_SOURCE_CONTROL_TEXT, NOTHING_CREATED_TEXT, NO_REMOTE_REASON, PUBLIC_WARNING,
    REPO_GONE_TEXT, START_OVER_RISK_TEXT, START_OVER_TOOLTIP,
};
use smithagents_app::link::LinkError;
use smithagents_app::view_models::{
    DAEMON_UNREACHABLE_TEXT, FILE_UNREADABLE_TEXT, NEED_REPO_TEXT, NOT_CONNECTED_TEXT,
};
use smithagents_client::{ClientError, FeedbackFile};
use smithagents_core::agent::{Repo, SourceControl};
use smithagents_core::feedback::{
    AttachmentKind as WireKind, FeedbackAttachment, FeedbackBox, FeedbackFailure, FeedbackKind,
    FeedbackOutcome, FeedbackStep, FeedbackTarget, IdeaOptions, SendAs, SubmissionId,
    ToolReadiness,
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
        source_control: Some(SourceControl::Git {
            default_branch: "main".into(),
        }),
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
    // `(has a GitHub remote, is public)` of the target, or no target.
    type Known = Option<(bool, Option<bool>)>;
    let warn = |send_as, idea, known: Known| {
        let target = known.map(|(remote, public)| target(remote, public));
        public_warning(send_as, idea, target.as_ref())
    };
    // A GitHub issue.
    let issue = |known: Known| warn(SendAs::GithubIssue, None, known);
    assert_eq!(issue(Some((true, Some(true)))), Some(PUBLIC_WARNING));
    assert_eq!(issue(Some((true, None))), Some(MAYBE_PUBLIC_WARNING));
    assert_eq!(issue(Some((true, Some(false)))), None);
    // Nothing known about the repository: it may be public.
    assert_eq!(issue(None), Some(MAYBE_PUBLIC_WARNING));

    // A plain intent to a repository with a GitHub remote: its agent's pull
    // request carries the screenshots (A-C1).
    let intent = |known: Known| warn(SendAs::Intent, None, known);
    assert_eq!(
        intent(Some((true, Some(true)))),
        Some(INTENT_PUBLIC_WARNING)
    );
    assert_eq!(
        intent(Some((true, None))),
        Some(INTENT_MAYBE_PUBLIC_WARNING)
    );
    assert_eq!(intent(Some((true, Some(false)))), None);
    // No GitHub remote, or nothing known (the daemon still asks): none.
    for is_public in [Some(true), None, Some(false)] {
        assert_eq!(intent(Some((false, is_public))), None);
    }
    assert_eq!(intent(None), None);
    // An idea package: never at Submit.
    for is_public in [Some(true), None, Some(false)] {
        assert_eq!(
            warn(SendAs::Intent, Some(IDEA_START), Some((true, is_public))),
            None
        );
    }
    // What the daemon's refusal makes the app ask.
    assert_eq!(
        maybe_public_warning(SendAs::GithubIssue),
        MAYBE_PUBLIC_WARNING
    );
    assert_eq!(
        maybe_public_warning(SendAs::Intent),
        INTENT_MAYBE_PUBLIC_WARNING
    );

    assert_eq!(
        PUBLIC_WARNING,
        "This repository is public. The screenshots will be visible to anyone."
    );
    assert_eq!(
        MAYBE_PUBLIC_WARNING,
        "This repository may be public. The screenshots will be visible to anyone."
    );
    assert_eq!(
        INTENT_PUBLIC_WARNING,
        "This repository is public. The agent's pull request will include the screenshots, visible to anyone."
    );
    assert_eq!(
        INTENT_MAYBE_PUBLIC_WARNING,
        "This repository may be public. The agent's pull request will include the screenshots, visible to anyone."
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

fn parts(explainer: bool, diagrams: bool, demo_video: bool) -> IdeaOptions {
    IdeaOptions {
        explainer,
        diagrams,
        demo_video,
    }
}

#[test]
fn idea_summaries() {
    let one = draft_of(&[AttachmentKind::Screenshot]);
    assert_eq!(
        summary(SendAs::Intent, Some(parts(true, false, true)), &one),
        "Creates the issue folder with 1 screenshot. An agent adds an explainer doc and a demo video on an idea branch, and you open the pull request."
    );
    assert!(summary(SendAs::Intent, Some(parts(true, true, true)), &one)
        .contains("An agent adds an explainer doc, diagrams and a demo video on"),);
    assert!(
        summary(SendAs::Intent, Some(parts(false, true, false)), &one)
            .contains("An agent adds diagrams on an idea branch")
    );
    assert_eq!(
        summary(SendAs::Intent, Some(parts(false, false, false)), &one),
        IDEA_NEEDS_PART
    );
    assert_eq!(IDEA_NEEDS_PART, "Choose at least one part.");

    let repo = Some(RepoId::new());
    assert!(!can_confirm(
        "Fix the zoom",
        repo,
        SendAs::Intent,
        None,
        Some(parts(false, false, false))
    ));
    assert!(can_confirm(
        "Fix the zoom",
        repo,
        SendAs::Intent,
        None,
        Some(parts(false, true, false))
    ));
    // An idea package is only ever an intent.
    assert!(!can_confirm(
        "Fix the zoom",
        repo,
        SendAs::GithubIssue,
        None,
        Some(parts(true, false, false))
    ));
}

#[test]
fn the_idea_sentences() {
    assert_eq!(IDEA_LABEL, "Prepare an idea PR for teammates");
    assert_eq!(
        [IDEA_DEMO_LABEL, IDEA_DIAGRAMS_LABEL, IDEA_EXPLAINER_LABEL],
        ["Demo video", "Diagrams", "Explainer doc"]
    );
    assert_eq!(demo_help(true), DEMO_HELP);
    assert_eq!(demo_help(false), DEMO_MISSING_REASON);
    assert_eq!(DEMO_HELP, "Only works for projects a browser can show.");
    assert_eq!(
        DEMO_MISSING_REASON,
        "Install agent-browser to record a demo video."
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
fn feedback_goes_only_to_a_repository_with_source_control() {
    let alpha = repo("/work/alpha", "20");
    let folder = Repo {
        source_control: None,
        ..repo("/work/folder", "21")
    };
    let both = [folder.clone(), alpha.clone()];

    // No repository with source control: the sentence, in place of the menu.
    assert_eq!(
        repo_line_text(std::slice::from_ref(&folder)),
        Some("Feedback needs a repository with source control.")
    );
    assert_eq!(
        NEEDS_SOURCE_CONTROL_TEXT,
        "Feedback needs a repository with source control."
    );
    // No repository at all: the way to add one, as before.
    assert_eq!(repo_line_text(&[]), Some(NEED_REPO_TEXT));
    assert_eq!(repo_line_text(&both), None);
    assert_eq!(
        feedback_repos(&both).map(|r| r.id).collect::<Vec<_>>(),
        [alpha.id],
        "the menu offers only the repository with source control"
    );
    // A plain folder used last is not where the screen starts.
    assert_eq!(
        starting_repo(&both, Some(folder.id), Some(alpha.id)),
        Some(alpha.id)
    );
    assert_eq!(starting_repo(&both, Some(folder.id), None), None);

    // The daemon's refusal reads as the same sentence, and so does a
    // recorded submission's step that failed for it.
    let refused = api(409, "needs_source_control", "x");
    assert_eq!(
        submit_error_text(&refused),
        "Feedback needs a repository with source control."
    );
    let failed = FeedbackOutcome {
        intent: None,
        failed: Some(FeedbackFailure {
            step: FeedbackStep::CreateIntent,
            code: "needs_source_control".into(),
            message: "x".into(),
        }),
        ..outcome_with_issue(
            SubmissionId::new(),
            alpha.id,
            "https://github.com/acme/app/issues/1",
        )
    };
    assert!(
        failure_text(&failed)
            .unwrap()
            .ends_with("but: Feedback needs a repository with source control."),
        "{:?}",
        failure_text(&failed)
    );
    // A first Create made nothing under its id; a retry or a re-upload of a
    // recorded submission may have its commit, so it is not a refusal that
    // made nothing.
    assert_eq!(first_send_failure(&refused), SendFailure::Refused);
    assert_eq!(send_failure(&refused), SendFailure::MayExist);
    // Otherwise a first Create reads as any send.
    for error in [
        api(409, "default_branch_missing", "x"),
        api(500, "needs_source_control", "x"),
        api(409, "a_code_from_a_newer_daemon", "x"),
        LinkError::NotConnected,
    ] {
        assert_eq!(
            first_send_failure(&error),
            send_failure(&error),
            "{error:?}"
        );
    }
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
    // The only tool a submission needs is agent-browser, for a demo video.
    assert_eq!(
        submit_error_text(&api(409, "tool_missing", "agent-browser is not installed")),
        DEMO_MISSING_REASON
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
        (
            "unpushed_commits",
            "The default branch has commits that are not on origin. Push them first, so they are not published with this feedback.",
        ),
        (
            "unpushed_commits",
            "This repository has never been fetched from origin, so it is not known what is already published. Run git fetch first.",
        ),
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
    let file = LinkError::Client(ClientError::File {
        path: "/moved/shot.png".into(),
        source: std::io::Error::other("gone"),
    });
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

/// A connect error: the port was free a moment ago, so nothing listens (on
/// the loopback only; no proxy).
fn connect_error() -> reqwest::Error {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        client
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .expect_err("nothing listens there")
    })
}

#[test]
fn what_a_failed_send_leaves_under_its_id() {
    // Refusals after which the daemon recorded and committed nothing, at
    // the status the daemon answers them with (`FeedbackError::status`).
    for (status, code) in [
        (400, "invalid"),
        (400, "unsupported_file"),
        (413, "payload_too_large"),
        (409, "no_github_remote"),
        (409, "gh_not_ready"),
        (409, "tool_missing"),
        (409, "default_branch_missing"),
        (409, "git_identity_missing"),
        (409, "no_free_name"),
        (409, "name_taken"),
        (409, "folder_blocked"),
        (409, "branch_blocked"),
        (409, "unpushed_commits"),
        (409, "public_not_acknowledged"),
        // W2-3: git cannot read the repository, before anything is made.
        (409, "origin_unreadable"),
    ] {
        assert_eq!(
            send_failure(&api(status, code, "x")),
            SendFailure::Refused,
            "{code}"
        );
    }
    // Something may exist: any 5xx (whatever its code), an unknown code,
    // a body that is not the daemon's.
    for (status, code) in [
        (500, "git_failed"),
        (500, "io_error"),
        (500, "store_error"),
        (503, "invalid"),
        (409, "a_code_from_a_newer_daemon"),
        (413, "http_413"),
        (409, "intent_not_found"),
    ] {
        assert_eq!(
            send_failure(&api(status, code, "x")),
            SendFailure::MayExist,
            "{status} {code}"
        );
    }
    assert_eq!(
        send_failure(&api(404, "repo_not_found", "repo not found")),
        SendFailure::RepoGone
    );
    assert_eq!(
        send_failure(&api(404, "submission_not_found", "no such submission")),
        SendFailure::NotRecorded
    );

    // The request provably never left.
    assert_eq!(send_failure(&LinkError::NotConnected), SendFailure::NotSent);
    let file = LinkError::Client(ClientError::File {
        path: "/moved/shot.png".into(),
        source: std::io::Error::other("gone"),
    });
    assert_eq!(send_failure(&file), SendFailure::NotSent);
    let refused = connect_error();
    assert!(refused.is_connect());
    assert_eq!(
        send_failure(&LinkError::Client(ClientError::Http(refused))),
        SendFailure::NotSent
    );
    // It may have left: a transport failure after connecting, a stopped link.
    let broken = reqwest::Client::new()
        .get("not a url")
        .build()
        .expect_err("an invalid URL is a reqwest error");
    assert!(!broken.is_connect());
    assert_eq!(
        send_failure(&LinkError::Client(ClientError::Http(broken))),
        SendFailure::MayExist
    );
    let ws = LinkError::Client(ClientError::Ws(
        tokio_tungstenite::tungstenite::Error::ConnectionClosed,
    ));
    assert_eq!(send_failure(&ws), SendFailure::MayExist);
    assert_eq!(send_failure(&LinkError::Stopped), SendFailure::MayExist);
}

#[test]
fn a_submission_the_daemon_has_no_record_of_names_what_is_still_committed() {
    // W2-7: the draft's own record of a failed step names a branch and a
    // commit, which are still in the repository.
    let id = SubmissionId::new();
    let mut partial = outcome_with_intent(
        id,
        intent(
            "fix-the-zoom",
            IntentState::Draft,
            Phase::Polishing { pass: 1 },
        ),
    );
    partial.intent = None;
    partial.failed = Some(FeedbackFailure {
        step: FeedbackStep::Push,
        code: "push_rejected".into(),
        message: "git could not push to origin: the remote rejected the branch".into(),
    });
    assert_eq!(
        not_recorded_text(Some(&partial)),
        "The daemon has no record of this submission. Its files are still committed on feedback/2026-11-26-fix-the-zoom (0123abc). Submit again to send it as a new one."
    );
    // The branch is bounded as in `failure_text`: one line, at most 300
    // characters.
    partial.branch = format!("feedback/{}\nLEAK", "x".repeat(400));
    let text = not_recorded_text(Some(&partial));
    assert!(!text.contains('\n') && !text.contains("LEAK"), "{text}");
    assert!(
        text.contains(&format!("feedback/{}…", "x".repeat(291))),
        "{text}"
    );
    // Without a recorded outcome nothing names a commit.
    assert_eq!(not_recorded_text(None), NOTHING_CREATED_TEXT);
}

#[test]
fn the_fixed_sentences_of_this_round() {
    assert_eq!(REPO_GONE_TEXT, "That repository no longer exists.");
    // (A send while another is under way cannot be started from the
    // screen, whose buttons wait: only its sentence is checked.)
    assert_eq!(
        smithagents_app::feedback::text::SUBMIT_BUSY_TEXT,
        "A submission is already under way."
    );
    assert_eq!(LINK_STOPPED_TEXT, "The daemon link has stopped");
    assert_eq!(submit_error_text(&LinkError::Stopped), LINK_STOPPED_TEXT);
    assert_eq!(
        START_OVER_TOOLTIP,
        "Keep the screenshots and text, and send as a new submission."
    );
    assert_eq!(
        START_OVER_RISK_TEXT,
        "The daemon may still be finishing this submission. Starting over can create it twice."
    );
    assert_eq!(
        KINDS,
        [
            FeedbackKind::NewFeature,
            FeedbackKind::Enhancement,
            FeedbackKind::Bug
        ]
    );
}

#[test]
fn only_the_acknowledgement_refusal_asks_again() {
    assert!(asks_to_acknowledge(&api(
        409,
        "public_not_acknowledged",
        "This repository may be public. The screenshots will be visible to anyone. Confirm that before sending.",
    )));
    for e in [
        api(
            409,
            "gh_not_ready",
            "GitHub CLI (gh) is not signed in; run: gh auth login",
        ),
        api(409, "unpushed_commits", "x"),
        LinkError::NotConnected,
    ] {
        assert!(!asks_to_acknowledge(&e), "{e:?}");
    }
}

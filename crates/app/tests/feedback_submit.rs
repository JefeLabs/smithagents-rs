//! The Feedback pane's Submit screen: the draft read-only, the main idea and
//! kind (kept in the draft), the repository, "Send it as", the summary and
//! the public warning, and what Create, Retry and Start over send. Commands
//! go to `FakeFeedback`, so no daemon runs and nothing leaves the machine;
//! the capturer is a fake and the draft lives in a temporary home.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::fs;
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use common::feedback::{act, capture_now, png, run_hop, FeedbackSetup};
use common::feedback_submit::{
    outcome_with_intent, outcome_with_issue, targets, FakeFeedback, FeedbackCall, BRANCH,
};
use common::intents::{intent, model_with, FakeIntents};
use common::ui::open_shell_options;
use gpui_kit::base::Root;
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::{TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{AppContext as _, ElementId, Entity, Role, TestAppContext, WindowHandle};
use smithagents_app::app_state::AppStateStore;
use smithagents_app::feedback::annotation::{Annotation, FracRect};
use smithagents_app::feedback::draft::AttachmentId;
use smithagents_app::feedback::state::{Feedback, SentTo, SUBMIT_TIMEOUT};
use smithagents_app::feedback::text::{
    EMPTY_TEXT, MAYBE_PUBLIC_WARNING, NOTHING_CREATED_TEXT, PUBLIC_WARNING, REPO_GONE_TEXT,
    START_OVER_RISK_TEXT, START_OVER_TOOLTIP, SUBMISSION_LOCKED_TEXT,
};
use smithagents_app::link::LinkError;
use smithagents_app::model::Action;
use smithagents_app::shell::Shell;
use smithagents_app::shell_state::ShellState;
use smithagents_app::view_models::{DAEMON_UNREACHABLE_TEXT, NOT_CONNECTED_TEXT};
use smithagents_app::views::feedback::{
    card_id, card_remove_id, submit_kind_id, submit_repo_item_id, FeedbackPane, FeedbackScreen,
    DISCARD_CONFIRM_ID, FEEDBACK_ADD_FILES_ID, FEEDBACK_CAPTURE_ID, FEEDBACK_DISCARD_ID,
    FEEDBACK_RESULT_ID, FEEDBACK_SUBMIT_ID, IDEA_CHECK_ID, IDEA_DEMO_HELP_ID, IDEA_DEMO_ID,
    IDEA_DIAGRAMS_ID, IDEA_EXPLAINER_ID, PUBLIC_CANCEL_ID, PUBLIC_CONFIRM_ID, PUBLIC_WARNING_ID,
    SEND_INTENT_ID, SEND_ISSUE_ID, SEND_ISSUE_REASON_ID, START_OVER_CANCEL_ID,
    START_OVER_CONFIRM_ID, SUBMIT_BACK_ID, SUBMIT_CONFIRM_ID, SUBMIT_ERROR_ID, SUBMIT_IDEA_HINT_ID,
    SUBMIT_IDEA_ID, SUBMIT_IDEA_LABEL_ID, SUBMIT_REPO_ID, SUBMIT_RETRY_ID, SUBMIT_START_OVER_ID,
    SUBMIT_SUMMARY_ID,
};
use smithagents_app::views::intents::{IntentsPane, Screen};
use smithagents_app::views::rail::RailEntry;
use smithagents_client::ClientError;
use smithagents_core::agent::Repo;
use smithagents_core::feedback::{
    AttachmentKind, FeedbackFailure, FeedbackKind, FeedbackSettings, FeedbackStep, FeedbackTargets,
    IdeaOptions, SendAs, ToolReadiness,
};
use smithagents_core::ids::RepoId;
use smithagents_core::intent::{Intent, IntentState, Phase};

const KINDS: [FeedbackKind; 3] = [
    FeedbackKind::NewFeature,
    FeedbackKind::Enhancement,
    FeedbackKind::Bug,
];

struct Opened {
    window: WindowHandle<Root>,
    shell: Entity<Shell>,
    state: Entity<ShellState>,
    feedback: Entity<Feedback>,
    fake: Rc<FakeFeedback>,
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

/// alpha (the older) and beta.
fn two_repos() -> Vec<Repo> {
    vec![repo("/work/alpha", "20"), repo("/work/beta", "21")]
}

/// Opens the shell, connected, on `setup`'s home with `repos` and the
/// feedback settings `meta` (the meta repository), and shows the Feedback
/// pane. Commands go to a new `FakeFeedback`; `last_used` is remembered in
/// the home's `app-state.json`.
fn open(
    cx: &mut TestAppContext,
    setup: &FeedbackSetup,
    repos: &[Repo],
    meta: Option<RepoId>,
) -> Opened {
    let fake = Rc::new(FakeFeedback::default());
    let mut model = model_with(vec![], vec![], repos.to_vec());
    model.apply(Action::FeedbackSettingsLoaded(FeedbackSettings {
        meta_repo_id: meta,
    }));
    let mut options = setup.options();
    options.model = model;
    options.feedback_commands = Some(fake.clone());
    options.intent_commands = Some(Rc::new(FakeIntents::default()));
    options.app_state = Some(AppStateStore::new(setup.home.path()));
    let (window, shell) = open_shell_options(cx, options, (1200., 1400.));
    cx.run_until_parked();
    frame(cx, window);
    // A second shell on the same home remembers the pane shown last.
    if shell.read_with(cx, |shell, cx| shell.rail_active(cx)) != Some(RailEntry::Feedback) {
        click(cx, window, RailEntry::Feedback.element_id());
    }
    assert_eq!(
        shell.read_with(cx, |shell, cx| shell.rail_active(cx)),
        Some(RailEntry::Feedback)
    );
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    let feedback = shell.read_with(cx, |shell, _| shell.feedback().clone());
    Opened {
        window,
        shell,
        state,
        feedback,
        fake,
    }
}

/// Closes the window, so a second shell can open on the same home.
fn close(cx: &mut TestAppContext, opened: Opened) {
    let Opened { window, shell, .. } = opened;
    drop(shell);
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
}

fn frame(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

/// Runs `f` on the window after a fresh frame.
fn with_window<R>(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App) -> R,
) -> R {
    let mut out = None;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        out = Some(f(window, cx));
    })
    .unwrap();
    out.unwrap()
}

fn click(cx: &mut TestAppContext, window: WindowHandle<Root>, id: impl Into<ElementId>) {
    let id = id.into();
    with_window(cx, window, |window, cx| window.click(id, cx));
    frame(cx, window);
}

fn press(cx: &mut TestAppContext, window: WindowHandle<Root>, key: &str) {
    with_window(cx, window, |window, cx| window.press(key, cx));
    frame(cx, window);
}

/// The text of the element `id` (a label's value, else its name), or
/// `None` when it is not drawn.
fn text_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    id: impl Into<ElementId>,
) -> Option<String> {
    let id = id.into();
    with_window(cx, window, |window, _| {
        window
            .try_find(id)
            .and_then(|element| element.value().or(element.label()).map(str::to_owned))
    })
}

fn drawn(cx: &mut TestAppContext, window: WindowHandle<Root>, id: impl Into<ElementId>) -> bool {
    let id = id.into();
    with_window(cx, window, |window, _| window.try_find(id).is_some())
}

fn disabled(cx: &mut TestAppContext, window: WindowHandle<Root>, id: impl Into<ElementId>) -> bool {
    let id = id.into();
    with_window(cx, window, |window, _| window.find(id).disabled()) == Some(true)
}

fn checked(cx: &mut TestAppContext, window: WindowHandle<Root>, id: impl Into<ElementId>) -> bool {
    let id = id.into();
    with_window(cx, window, |window, _| window.find(id).checked()) == Some(true)
}

/// Whether a toggle button (a kind) is pressed.
fn pressed(cx: &mut TestAppContext, window: WindowHandle<Root>, id: impl Into<ElementId>) -> bool {
    let id = id.into();
    with_window(cx, window, |window, _| {
        let button = window.find(id);
        assert_eq!(button.role(), Some(Role::Button));
        button.checked()
    }) == Some(true)
}

/// Whether the main idea's text box is disabled (the field named "Main
/// idea" sits inside the input that says so).
fn idea_disabled(cx: &mut TestAppContext, window: WindowHandle<Root>) -> bool {
    with_window(cx, window, |window, _| {
        let frame = window.within(SUBMIT_IDEA_ID);
        let field = frame.find_by_label("Main idea");
        assert_eq!(field.role(), Some(Role::MultilineTextInput));
        frame
            .elements()
            .into_iter()
            .filter(|element| field.path().starts_with(element.path()))
            .find_map(|element| element.disabled())
    }) == Some(true)
}

/// The main idea's text box as typed.
fn idea_text(cx: &mut TestAppContext, opened: &Opened) -> String {
    let pane = feedback_pane(cx, &opened.shell);
    pane.read_with(cx, |pane, cx| pane.main_idea_text(cx))
}

fn feedback_pane(cx: &mut TestAppContext, shell: &Entity<Shell>) -> Entity<FeedbackPane> {
    let dock = shell.read_with(cx, |shell, _| shell.dock().clone());
    dock.read_with(cx, |dock, _| {
        dock.layout(DockPlacement::Right)
            .expect("a right dock")
            .panels()
            .find_map(|id| dock.panel(id)?.view().downcast::<FeedbackPane>().ok())
            .expect("the Feedback pane is in the right dock")
    })
}

fn intents_pane(cx: &mut TestAppContext, shell: &Entity<Shell>) -> Entity<IntentsPane> {
    let dock = shell.read_with(cx, |shell, _| shell.dock().clone());
    dock.read_with(cx, |dock, _| {
        dock.layout(DockPlacement::Right)
            .expect("a right dock")
            .panels()
            .find_map(|id| dock.panel(id)?.view().downcast::<IntentsPane>().ok())
            .expect("the Intents pane is in the right dock")
    })
}

fn screen(cx: &mut TestAppContext, opened: &Opened) -> FeedbackScreen {
    let pane = feedback_pane(cx, &opened.shell);
    pane.read_with(cx, |pane, _| pane.screen().clone())
}

fn submitting(cx: &mut TestAppContext, opened: &Opened) -> bool {
    let pane = feedback_pane(cx, &opened.shell);
    pane.read_with(cx, |pane, _| pane.submitting())
}

fn note(comment: &str) -> Annotation {
    Annotation {
        rect: FracRect {
            x: 0.25,
            y: 0.25,
            w: 0.25,
            h: 0.25,
        },
        comment: comment.to_string(),
    }
}

/// Captures (without the hold), saves `comments` as boxes and presses Done:
/// the screenshot's id.
fn screenshot(
    cx: &mut TestAppContext,
    opened: &Opened,
    setup: &FeedbackSetup,
    comments: &[&str],
) -> AttachmentId {
    let (window, feedback) = (opened.window, &opened.feedback);
    capture_now(cx, window, feedback, setup);
    let id = cx
        .read(|cx| feedback.read(cx).annotating().cloned())
        .expect("the capture opened the annotator");
    let boxes: Vec<Annotation> = comments.iter().map(|comment| note(comment)).collect();
    act(cx, window, feedback, |f, _, cx| f.set_boxes(&id, boxes, cx));
    act(cx, window, feedback, |f, _, cx| f.finish_annotating(cx));
    frame(cx, window);
    id
}

/// Opens the Submit screen, the targets answered with `targets`.
fn open_submit(cx: &mut TestAppContext, opened: &Opened, targets: FeedbackTargets) {
    opened.fake.queue_targets(Ok(targets));
    click(cx, opened.window, FEEDBACK_SUBMIT_ID);
    assert_eq!(screen(cx, opened), FeedbackScreen::Submit);
}

/// Types `text` at the end of the main idea.
fn type_idea(cx: &mut TestAppContext, window: WindowHandle<Root>, text: &str) {
    click(cx, window, SUBMIT_IDEA_ID);
    with_window(cx, window, |window, cx| window.input(text, cx));
    frame(cx, window);
}

/// Opens a shell with alpha (the meta repository) and beta, captures one
/// screenshot with the box "the toolbar", opens the Submit screen (alpha
/// with no GitHub remote) and types "Fix the zoom".
fn ready_to_send(cx: &mut TestAppContext, setup: &FeedbackSetup, repos: &[Repo]) -> Opened {
    let opened = open(cx, setup, repos, Some(repos[0].id));
    screenshot(cx, &opened, setup, &["the toolbar"]);
    open_submit(cx, &opened, no_remotes(repos));
    type_idea(cx, opened.window, "Fix the zoom");
    opened
}

fn no_remotes(repos: &[Repo]) -> FeedbackTargets {
    let list: Vec<_> = repos.iter().map(|repo| (repo.id, false, None)).collect();
    targets(&list, true, false)
}

fn created_intent(repo: RepoId) -> Intent {
    let mut created = intent(
        "fix-the-zoom",
        IntentState::Draft,
        Phase::Polishing { pass: 1 },
    );
    created.repo_id = repo;
    created
}

fn push_failure() -> FeedbackFailure {
    FeedbackFailure {
        step: FeedbackStep::Push,
        code: "push_rejected".into(),
        message: "git could not push to origin: the remote rejected the branch".into(),
    }
}

fn api(status: u16, code: &str, message: &str) -> LinkError {
    FakeIntents::api_error(status, code, message)
}

/// The id of the one submission sent so far.
fn sent_id(opened: &Opened) -> smithagents_core::feedback::SubmissionId {
    let submits = opened.fake.submits();
    assert_eq!(submits.len(), 1, "one submission sent");
    submits[0].0.submission_id
}

fn controls() -> Vec<ElementId> {
    let mut ids: Vec<ElementId> = vec![
        SUBMIT_BACK_ID.into(),
        SUBMIT_REPO_ID.into(),
        SEND_INTENT_ID.into(),
        SEND_ISSUE_ID.into(),
    ];
    ids.extend(KINDS.iter().map(|kind| submit_kind_id(*kind)));
    ids
}

// ---- Step 1: Radio in headless tests --------------------------------------

#[gpui_kit::test]
fn the_send_as_radios_report_checked_and_disabled(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = open(cx, &setup, &repos, Some(repos[0].id));
    let window = opened.window;
    screenshot(cx, &opened, &setup, &["the toolbar"]);

    // While the targets load, the issue choice is disabled with no reason.
    click(cx, window, FEEDBACK_SUBMIT_ID);
    with_window(cx, window, |window, _| {
        let issue = window.find(SEND_ISSUE_ID);
        assert_eq!(issue.role(), Some(Role::RadioButton));
        assert_eq!(issue.checked(), Some(false));
        assert_eq!(issue.disabled(), Some(true));
        assert_eq!(issue.label(), Some("A GitHub issue"));
        let intent = window.find(SEND_INTENT_ID);
        assert_eq!(intent.checked(), Some(true));
        assert_eq!(intent.label(), Some("An intent to work on now"));
    });
    assert!(!drawn(cx, window, SEND_ISSUE_REASON_ID));

    let list: Vec<_> = repos.iter().map(|r| (r.id, true, Some(false))).collect();
    opened.fake.answer_targets(Ok(targets(&list, true, false)));
    frame(cx, window);
    assert!(!disabled(cx, window, SEND_ISSUE_ID));
    click(cx, window, SEND_ISSUE_ID);
    assert!(checked(cx, window, SEND_ISSUE_ID));
    assert!(!checked(cx, window, SEND_INTENT_ID));
}

// ---- The pane ---------------------------------------------------------------

#[gpui_kit::test]
fn submit_is_enabled_only_with_an_attachment(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let opened = open(cx, &setup, &two_repos(), None);
    let window = opened.window;

    with_window(cx, window, |window, _| {
        let submit = window.find(FEEDBACK_SUBMIT_ID);
        assert_eq!(submit.label(), Some("Submit…"));
        assert_eq!(submit.disabled(), Some(true));
        assert_eq!(
            submit.description(),
            Some("Add a screenshot or a file first")
        );
    });
    click(cx, window, FEEDBACK_SUBMIT_ID);
    assert_eq!(screen(cx, &opened), FeedbackScreen::Draft);
    assert!(opened.fake.calls.borrow().is_empty());

    screenshot(cx, &opened, &setup, &["the toolbar"]);
    assert!(!disabled(cx, window, FEEDBACK_SUBMIT_ID));
    click(cx, window, FEEDBACK_SUBMIT_ID);
    assert_eq!(screen(cx, &opened), FeedbackScreen::Submit);
    assert_eq!(*opened.fake.calls.borrow(), [FeedbackCall::Targets]);
}

#[gpui_kit::test]
fn the_screen_shows_the_draft_read_only(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = open(cx, &setup, &repos, Some(repos[0].id));
    let window = opened.window;
    let shot = screenshot(cx, &opened, &setup, &["the toolbar"]);
    open_submit(cx, &opened, no_remotes(&repos));

    assert_eq!(
        text_of(cx, window, card_id(&shot)).as_deref(),
        Some("Screenshot 1 · 1 comment")
    );
    with_window(cx, window, |window, _| {
        let card = window.find(card_id(&shot));
        assert_ne!(card.role(), Some(Role::Button), "not clickable");
        assert!(window
            .within(card_id(&shot))
            .elements()
            .iter()
            .any(|element| element.value() == Some("the toolbar")));
    });
    assert!(!drawn(cx, window, card_remove_id(&shot)));
    click(cx, window, card_id(&shot));
    assert_eq!(
        cx.read(|cx| opened.feedback.read(cx).annotating().cloned()),
        None
    );

    for (kind, label) in KINDS.iter().zip(["New feature", "Enhancement", "Bug"]) {
        assert_eq!(
            text_of(cx, window, submit_kind_id(*kind)).as_deref(),
            Some(label)
        );
    }
    assert!(pressed(
        cx,
        window,
        submit_kind_id(FeedbackKind::NewFeature)
    ));
    assert_eq!(text_of(cx, window, SUBMIT_BACK_ID).as_deref(), Some("Back"));
    assert_eq!(
        text_of(cx, window, SUBMIT_CONFIRM_ID).as_deref(),
        Some("Create intent")
    );
    assert!(disabled(cx, window, SUBMIT_CONFIRM_ID), "no main idea yet");
    assert_eq!(
        text_of(cx, window, SUBMIT_SUMMARY_ID).as_deref(),
        Some("Creates the issue folder with 1 screenshot on a new branch, and an intent to review and send.")
    );
    assert_eq!(
        text_of(cx, window, SUBMIT_IDEA_LABEL_ID).as_deref(),
        Some("Main idea")
    );
    assert_eq!(
        text_of(cx, window, SUBMIT_IDEA_HINT_ID).as_deref(),
        Some("The first line is the title.")
    );
}

#[gpui_kit::test]
fn the_main_idea_and_kind_are_kept_in_the_draft(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;
    assert!(!disabled(cx, window, SUBMIT_CONFIRM_ID));

    click(cx, window, submit_kind_id(FeedbackKind::Bug));
    assert!(pressed(cx, window, submit_kind_id(FeedbackKind::Bug)));
    assert!(!pressed(
        cx,
        window,
        submit_kind_id(FeedbackKind::NewFeature)
    ));

    click(cx, window, SUBMIT_BACK_ID);
    assert_eq!(screen(cx, &opened), FeedbackScreen::Draft);
    open_submit(cx, &opened, no_remotes(&repos));
    assert_eq!(idea_text(cx, &opened), "Fix the zoom");
    assert!(pressed(cx, window, submit_kind_id(FeedbackKind::Bug)));

    let json = fs::read_to_string(setup.draft_dir().join("draft.json")).unwrap();
    let compact: String = json.split_whitespace().collect();
    assert!(compact.contains(r#""main_idea":"Fixthezoom""#), "{json}");
    assert!(compact.contains(r#""kind":"bug""#), "{json}");
    let draft: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(draft["main_idea"], "Fix the zoom");
    assert_eq!(draft["kind"], "bug");
}

#[gpui_kit::test]
fn the_repository_starts_on_the_last_used_then_the_meta_repository(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let (alpha, beta) = (repos[0].clone(), repos[1].clone());

    // The meta repository, with nothing used yet.
    let opened = open(cx, &setup, &repos, Some(beta.id));
    let window = opened.window;
    screenshot(cx, &opened, &setup, &[]);
    open_submit(cx, &opened, no_remotes(&repos));
    assert_eq!(text_of(cx, window, SUBMIT_REPO_ID).as_deref(), Some("beta"));

    // The last used wins.
    opened
        .state
        .update(cx, |state, _| state.set_last_repo_id(alpha.id));
    click(cx, window, SUBMIT_BACK_ID);
    open_submit(cx, &opened, no_remotes(&repos));
    assert_eq!(
        text_of(cx, window, SUBMIT_REPO_ID).as_deref(),
        Some("alpha")
    );

    // The menu changes it.
    click(cx, window, SUBMIT_REPO_ID);
    with_window(cx, window, |window, cx| {
        window
            .within("popup-menu")
            .click(submit_repo_item_id(beta.id), cx)
    });
    frame(cx, window);
    assert_eq!(text_of(cx, window, SUBMIT_REPO_ID).as_deref(), Some("beta"));
    close(cx, opened);

    // Neither: the repository is to be chosen, and nothing can be sent.
    let other = FeedbackSetup::new();
    let opened = open(cx, &other, &repos, None);
    let window = opened.window;
    screenshot(cx, &opened, &other, &[]);
    open_submit(cx, &opened, no_remotes(&repos));
    type_idea(cx, window, "Fix the zoom");
    assert_eq!(
        text_of(cx, window, SUBMIT_REPO_ID).as_deref(),
        Some("Choose a repository")
    );
    assert!(disabled(cx, window, SUBMIT_CONFIRM_ID));
    close(cx, opened);

    // No repository at all: the way to Settings.
    let none = FeedbackSetup::new();
    let opened = open(cx, &none, &[], None);
    let window = opened.window;
    screenshot(cx, &opened, &none, &[]);
    open_submit(cx, &opened, targets(&[], true, false));
    assert_eq!(
        text_of(cx, window, SUBMIT_REPO_ID).as_deref(),
        Some("Add a source repository first")
    );
    click(cx, window, SUBMIT_REPO_ID);
    assert_eq!(
        opened
            .shell
            .read_with(cx, |shell, cx| shell.rail_active(cx)),
        Some(RailEntry::Settings)
    );
}

#[gpui_kit::test]
fn the_issue_choice_is_disabled_with_its_reason(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let (alpha, beta) = (repos[0].clone(), repos[1].clone());
    let opened = open(cx, &setup, &repos, Some(alpha.id));
    let window = opened.window;
    screenshot(cx, &opened, &setup, &[]);

    // No GitHub remote.
    open_submit(
        cx,
        &opened,
        targets(
            &[(alpha.id, false, None), (beta.id, true, Some(false))],
            true,
            false,
        ),
    );
    assert!(disabled(cx, window, SEND_ISSUE_ID));
    assert_eq!(
        text_of(cx, window, SEND_ISSUE_REASON_ID).as_deref(),
        Some("This repository has no GitHub remote.")
    );

    // The other repository has one, and gh is ready.
    click(cx, window, SUBMIT_REPO_ID);
    with_window(cx, window, |window, cx| {
        window
            .within("popup-menu")
            .click(submit_repo_item_id(beta.id), cx)
    });
    frame(cx, window);
    assert!(!disabled(cx, window, SEND_ISSUE_ID));
    assert!(!drawn(cx, window, SEND_ISSUE_REASON_ID));

    // Chosen there, the issue goes back to the intent with alpha.
    click(cx, window, SEND_ISSUE_ID);
    assert!(checked(cx, window, SEND_ISSUE_ID));
    click(cx, window, SUBMIT_REPO_ID);
    with_window(cx, window, |window, cx| {
        window
            .within("popup-menu")
            .click(submit_repo_item_id(alpha.id), cx)
    });
    frame(cx, window);
    assert!(checked(cx, window, SEND_INTENT_ID));
    assert!(!checked(cx, window, SEND_ISSUE_ID));

    // gh is not installed.
    click(cx, window, SUBMIT_BACK_ID);
    open_submit(
        cx,
        &opened,
        targets(&[(alpha.id, true, Some(false))], false, false),
    );
    assert!(disabled(cx, window, SEND_ISSUE_ID));
    assert_eq!(
        text_of(cx, window, SEND_ISSUE_REASON_ID).as_deref(),
        Some("Install the GitHub CLI (gh) to create issues.")
    );

    // Signed out, and the feedback plugin turned off: the daemon's reason.
    for (reason, text) in [
        ("not signed in", "Sign in first: gh auth login"),
        (
            "turned off or not running",
            "The feedback plugin is turned off or not running.",
        ),
    ] {
        click(cx, window, SUBMIT_BACK_ID);
        let mut answer = targets(&[(alpha.id, true, Some(false))], true, false);
        answer.gh = ToolReadiness {
            ready: false,
            reason: Some(reason.into()),
        };
        open_submit(cx, &opened, answer);
        assert!(disabled(cx, window, SEND_ISSUE_ID));
        assert_eq!(
            text_of(cx, window, SEND_ISSUE_REASON_ID).as_deref(),
            Some(text)
        );
    }
}

#[gpui_kit::test]
fn choosing_an_issue_changes_the_button_the_summary_and_the_warning(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let alpha = repos[0].id;
    let opened = open(cx, &setup, &repos, Some(alpha));
    let window = opened.window;
    screenshot(cx, &opened, &setup, &[]);

    for (is_public, warning) in [
        (Some(true), Some(PUBLIC_WARNING)),
        (None, Some(MAYBE_PUBLIC_WARNING)),
        (Some(false), None),
    ] {
        open_submit(
            cx,
            &opened,
            targets(&[(alpha, true, is_public)], true, false),
        );
        assert!(!drawn(cx, window, PUBLIC_WARNING_ID), "not for an intent");
        click(cx, window, SEND_ISSUE_ID);
        assert_eq!(
            text_of(cx, window, SUBMIT_CONFIRM_ID).as_deref(),
            Some("Create issue")
        );
        assert_eq!(
            text_of(cx, window, SUBMIT_SUMMARY_ID).as_deref(),
            Some("Creates the issue folder with 1 screenshot, pushes the branch to origin and opens a GitHub issue.")
        );
        assert_eq!(
            text_of(cx, window, PUBLIC_WARNING_ID).as_deref(),
            warning,
            "{is_public:?}"
        );
        click(cx, window, SUBMIT_BACK_ID);
    }
}

#[gpui_kit::test]
fn a_public_repository_is_confirmed_every_time(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let alpha = repos[0].id;
    let opened = open(cx, &setup, &repos, Some(alpha));
    let window = opened.window;
    screenshot(cx, &opened, &setup, &["the toolbar"]);
    open_submit(
        cx,
        &opened,
        targets(&[(alpha, true, Some(true))], true, false),
    );
    type_idea(cx, window, "Fix the zoom");
    click(cx, window, SEND_ISSUE_ID);

    // Cancel sends nothing.
    click(cx, window, SUBMIT_CONFIRM_ID);
    assert!(with_window(cx, window, |window, cx| window.has_active_dialog(cx)));
    with_window(cx, window, |window, _| {
        assert!(window
            .find_all_by_role(Role::Label)
            .iter()
            .any(|label| label.value() == Some(PUBLIC_WARNING)));
    });
    click(cx, window, PUBLIC_CANCEL_ID);
    assert!(!with_window(cx, window, |window, cx| window.has_active_dialog(cx)));
    assert!(opened.fake.submits().is_empty());

    // Asked again, and only the confirmation sends.
    click(cx, window, SUBMIT_CONFIRM_ID);
    assert!(with_window(cx, window, |window, cx| window.has_active_dialog(cx)));
    assert!(opened.fake.submits().is_empty());
    click(cx, window, PUBLIC_CONFIRM_ID);
    assert!(!with_window(cx, window, |window, cx| window.has_active_dialog(cx)));
    let submits = opened.fake.submits();
    assert_eq!(submits.len(), 1);
    assert_eq!(submits[0].0.send_as, SendAs::GithubIssue);

    // The push failed: Retry sends the screenshots again, so it asks again.
    let id = submits[0].0.submission_id;
    let mut partial = outcome_with_issue(id, alpha, "unused");
    partial.issue_url = None;
    partial.pushed = false;
    partial.failed = Some(push_failure());
    opened.fake.answer_outcome(Ok(partial));
    frame(cx, window);
    click(cx, window, SUBMIT_RETRY_ID);
    assert!(with_window(cx, window, |window, cx| window.has_active_dialog(cx)));
    assert!(opened.fake.retries().is_empty());
    click(cx, window, PUBLIC_CONFIRM_ID);
    assert_eq!(opened.fake.retries(), [id]);

    opened.fake.answer_outcome(Ok(outcome_with_issue(
        id,
        alpha,
        "https://github.com/smithagents-test-invalid/widgets/issues/7",
    )));
    frame(cx, window);
    assert_eq!(screen(cx, &opened), FeedbackScreen::Draft);
    assert_eq!(
        text_of(cx, window, FEEDBACK_RESULT_ID).as_deref(),
        Some("Created https://github.com/smithagents-test-invalid/widgets/issues/7. Files: feedback/2026-11-26-fix-the-zoom (0123abc).")
    );
    assert!(!setup.draft_dir().exists());
}

// ---- Sending ----------------------------------------------------------------

#[gpui_kit::test]
fn create_intent_sends_the_draft_and_opens_the_intent(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let alpha = repos[0].id;
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;

    click(cx, window, SUBMIT_CONFIRM_ID);
    let submits = opened.fake.submits();
    assert_eq!(submits.len(), 1);
    let (request, files) = &submits[0];
    assert_eq!(request.main_idea, "Fix the zoom");
    assert_eq!(request.repo_id, alpha);
    assert_eq!(request.send_as, SendAs::Intent);
    assert_eq!(request.kind, FeedbackKind::NewFeature);
    assert_eq!(request.idea, None);
    assert_eq!(request.platform, std::env::consts::OS);
    assert_eq!(request.app_version, smithagents_core::VERSION);
    assert_eq!(request.attachments.len(), 1);
    let attachment = &request.attachments[0];
    assert_eq!(attachment.kind, AttachmentKind::Screenshot);
    assert_eq!(attachment.boxes.len(), 1);
    assert_eq!(attachment.boxes[0].comment, "the toolbar");
    assert_eq!((attachment.boxes[0].x, attachment.boxes[0].w), (0.25, 0.25));
    let dir = setup.draft_dir();
    let parts: Vec<_> = files
        .iter()
        .map(|file| (file.part.as_str(), file.path.clone()))
        .collect();
    assert_eq!(
        parts,
        [
            ("file-1", dir.join("screenshot-1.png")),
            ("annotated-1", dir.join("screenshot-1.annotated.png")),
        ]
    );

    // Before the answer every control waits.
    assert!(submitting(cx, &opened), "the button shows loading");
    assert!(disabled(cx, window, SUBMIT_CONFIRM_ID));
    for id in controls() {
        assert!(disabled(cx, window, id.clone()), "{id:?}");
    }
    assert!(idea_disabled(cx, window));

    let created = created_intent(alpha);
    opened.state.update(cx, |state, cx| {
        state.dispatch(Action::IntentUpserted(created.clone()), cx)
    });
    opened.fake.answer_outcome(Ok(outcome_with_intent(
        request.submission_id,
        created.clone(),
    )));
    frame(cx, window);

    assert!(!setup.draft_dir().exists(), "the draft is gone");
    assert_eq!(
        opened
            .shell
            .read_with(cx, |shell, cx| shell.rail_active(cx)),
        Some(RailEntry::Intents)
    );
    let intents = intents_pane(cx, &opened.shell);
    assert_eq!(
        intents.read_with(cx, |pane, _| *pane.screen()),
        Screen::Detail(created.id)
    );
    assert_eq!(
        opened.state.read_with(cx, |state, _| state.last_repo_id()),
        Some(alpha)
    );

    click(cx, window, RailEntry::Feedback.element_id());
    assert_eq!(screen(cx, &opened), FeedbackScreen::Draft);
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(FEEDBACK_RESULT_ID).role(), Some(Role::Status));
    });
    assert_eq!(
        text_of(cx, window, FEEDBACK_RESULT_ID).as_deref(),
        Some(
            "Created the intent “Fix the zoom”. Files: feedback/2026-11-26-fix-the-zoom (0123abc)."
        )
    );
    assert_eq!(
        text_of(cx, window, "feedback-empty").as_deref(),
        Some(EMPTY_TEXT)
    );

    // The next change to the draft clears it.
    screenshot(cx, &opened, &setup, &[]);
    assert!(!drawn(cx, window, FEEDBACK_RESULT_ID));
}

#[gpui_kit::test]
fn a_second_press_sends_nothing(cx: &mut TestAppContext) {
    // Review Focus 5.
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;

    click(cx, window, SUBMIT_CONFIRM_ID);
    click(cx, window, SUBMIT_CONFIRM_ID);
    with_window(cx, window, |window, cx| {
        window.double_click(SUBMIT_CONFIRM_ID, cx)
    });
    frame(cx, window);
    assert_eq!(opened.fake.submits().len(), 1);
    assert_eq!(opened.fake.outcomes_waiting(), 1);
}

#[gpui_kit::test]
fn a_refusal_keeps_the_draft_and_shows_the_daemons_sentence(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;
    opened.fake.queue_outcome(Err(api(
        409,
        "default_branch_missing",
        "the repository's default branch main does not exist locally",
    )));

    click(cx, window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);
    let sentence = "The repository's default branch main does not exist locally";
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(SUBMIT_ERROR_ID).role(), Some(Role::Status));
    });
    assert_eq!(
        text_of(cx, window, SUBMIT_ERROR_ID).as_deref(),
        Some(sentence)
    );
    assert!(!submitting(cx, &opened));
    assert_eq!(
        cx.read(|cx| opened.feedback.read(cx).draft().attachments.len()),
        1,
        "the card is still there"
    );
    assert!(setup.draft_dir().join("submission.json").is_file());

    // Nothing was made under the id: the form stays as it was.
    assert!(!cx.read(|cx| opened.feedback.read(cx).locked()));
    assert!(!idea_disabled(cx, window));
    assert!(!disabled(cx, window, submit_kind_id(FeedbackKind::Bug)));
    assert!(!disabled(cx, window, SUBMIT_CONFIRM_ID));
    assert!(!drawn(cx, window, SUBMIT_RETRY_ID));

    // The sentence stays after Back and on reopening; the draft never says
    // it was sent.
    click(cx, window, SUBMIT_BACK_ID);
    assert!(!disabled(cx, window, FEEDBACK_CAPTURE_ID));
    assert!(cx.read(|cx| opened.feedback.read(cx).notices().is_empty()));
    open_submit(cx, &opened, no_remotes(&repos));
    assert_eq!(
        text_of(cx, window, SUBMIT_ERROR_ID).as_deref(),
        Some(sentence)
    );

    // A second confirm sends the same id; the sentence goes with the send.
    click(cx, window, SUBMIT_CONFIRM_ID);
    let submits = opened.fake.submits();
    assert_eq!(submits.len(), 2);
    assert_eq!(submits[1].0.submission_id, id);
    assert!(!drawn(cx, window, SUBMIT_ERROR_ID));
}

#[gpui_kit::test]
fn a_sent_submission_keeps_its_text_and_target(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;
    // A 5xx: the daemon may have made something under the id.
    opened
        .fake
        .queue_outcome(Err(api(500, "store_error", "the store could not save")));
    click(cx, window, SUBMIT_CONFIRM_ID);

    // Text, kind and target are what the daemon saw.
    assert!(idea_disabled(cx, window));
    for id in controls()
        .into_iter()
        .filter(|id| *id != ElementId::from(SUBMIT_BACK_ID))
    {
        assert!(disabled(cx, window, id.clone()), "{id:?}");
    }
    assert!(!disabled(cx, window, SUBMIT_BACK_ID));
    assert!(!disabled(cx, window, SUBMIT_RETRY_ID));
    assert_eq!(
        text_of(cx, window, SUBMIT_RETRY_ID).as_deref(),
        Some("Retry")
    );
    assert_eq!(
        text_of(cx, window, SUBMIT_START_OVER_ID).as_deref(),
        Some("Start over")
    );
    with_window(cx, window, |window, _| {
        assert_eq!(
            window.find(SUBMIT_START_OVER_ID).description(),
            Some(START_OVER_TOOLTIP)
        );
    });

    // So are the attachments.
    click(cx, window, SUBMIT_BACK_ID);
    let shot = cx.read(|cx| opened.feedback.read(cx).draft().attachments[0].id.clone());
    assert!(disabled(cx, window, FEEDBACK_CAPTURE_ID));
    assert!(disabled(cx, window, FEEDBACK_ADD_FILES_ID));
    assert!(!drawn(cx, window, card_remove_id(&shot)));
    act(cx, window, &opened.feedback, |f, _, cx| f.remove(&shot, cx));
    assert_eq!(
        cx.read(|cx| opened.feedback.read(cx).draft().attachments.len()),
        1
    );
    // A capture (the rail's hold) is refused and says why.
    act(cx, window, &opened.feedback, |f, window, cx| {
        f.capture(window, cx)
    });
    assert_eq!(setup.hops.pending(), 0, "nothing is captured");
    assert_eq!(
        cx.read(|cx| opened.feedback.read(cx).notices().to_vec()),
        [SUBMISSION_LOCKED_TEXT]
    );
    assert_eq!(
        cx.read(|cx| opened.feedback.read(cx).draft().attachments.len()),
        1
    );

    // Start over makes a new submission of it.
    open_submit(cx, &opened, no_remotes(&repos));
    let first = sent_id(&opened);
    click(cx, window, SUBMIT_START_OVER_ID);
    assert!(!idea_disabled(cx, window));
    assert!(!disabled(cx, window, submit_kind_id(FeedbackKind::Bug)));
    assert!(drawn(cx, window, SUBMIT_CONFIRM_ID));
    assert!(!drawn(cx, window, SUBMIT_RETRY_ID));
    assert!(!drawn(cx, window, SUBMIT_ERROR_ID));
    type_idea(cx, window, " again");
    click(cx, window, SUBMIT_CONFIRM_ID);
    let submits = opened.fake.submits();
    assert_eq!(submits.len(), 2);
    assert_ne!(submits[1].0.submission_id, first);
    assert_eq!(submits[1].0.main_idea, "Fix the zoom again");
}

#[gpui_kit::test]
fn a_request_that_never_left_leaves_the_form_as_it_was(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;
    opened.fake.queue_outcome(Err(LinkError::NotConnected));

    click(cx, window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);
    assert_eq!(
        text_of(cx, window, SUBMIT_ERROR_ID).as_deref(),
        Some(NOT_CONNECTED_TEXT)
    );
    assert!(!cx.read(|cx| opened.feedback.read(cx).locked()));
    let json = fs::read_to_string(setup.draft_dir().join("submission.json")).unwrap();
    assert!(!json.contains("sent"), "{json}");
    assert!(!idea_disabled(cx, window));
    assert!(!disabled(cx, window, SUBMIT_CONFIRM_ID));
    assert!(!drawn(cx, window, SUBMIT_RETRY_ID));
    click(cx, window, SUBMIT_CONFIRM_ID);
    let submits = opened.fake.submits();
    assert_eq!(submits.len(), 2);
    assert_eq!(
        submits[1].0.submission_id, id,
        "the same draft, the same id"
    );
}

#[gpui_kit::test]
fn a_connection_lost_after_sending_locks_the_draft(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;
    // Not a connect error: the request may have reached the daemon.
    let broken = reqwest::Client::new()
        .get("not a url")
        .build()
        .expect_err("an invalid URL is a reqwest error");
    opened
        .fake
        .queue_outcome(Err(LinkError::Client(ClientError::Http(broken))));

    click(cx, window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);
    assert_eq!(
        text_of(cx, window, SUBMIT_ERROR_ID).as_deref(),
        Some(DAEMON_UNREACHABLE_TEXT)
    );
    assert!(cx.read(|cx| opened.feedback.read(cx).locked()));
    assert!(idea_disabled(cx, window));
    assert!(!drawn(cx, window, SUBMIT_CONFIRM_ID));
    click(cx, window, SUBMIT_RETRY_ID);
    assert_eq!(opened.fake.retries(), [id], "the retry route first");
    assert_eq!(opened.fake.submits().len(), 1);
}

#[gpui_kit::test]
fn a_window_closed_while_sending_leaves_a_locked_draft(cx: &mut TestAppContext) {
    // Review F2: the marker is on disk before the request leaves.
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_to_send(cx, &setup, &repos);
    click(cx, opened.window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);
    let json = fs::read_to_string(setup.draft_dir().join("submission.json")).unwrap();
    assert!(json.contains("sent"), "{json}");
    close(cx, opened);

    let opened = open(cx, &setup, &repos, Some(repos[0].id));
    let window = opened.window;
    assert!(cx.read(|cx| opened.feedback.read(cx).locked()));
    open_submit(cx, &opened, no_remotes(&repos));
    assert!(drawn(cx, window, SUBMIT_RETRY_ID));
    assert!(!drawn(cx, window, SUBMIT_CONFIRM_ID));
    assert!(idea_disabled(cx, window));
    click(cx, window, SUBMIT_RETRY_ID);
    assert_eq!(opened.fake.retries(), [id], "the same id");
    assert!(opened.fake.submits().is_empty(), "nothing is uploaded yet");
}

#[gpui_kit::test]
fn a_partial_failure_offers_retry_and_start_over(cx: &mut TestAppContext) {
    // Review Focus 2.
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let alpha = repos[0].id;
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;

    click(cx, window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);
    let mut partial = outcome_with_intent(id, created_intent(alpha));
    partial.intent = None;
    partial.failed = Some(push_failure());
    opened.fake.answer_outcome(Ok(partial));
    frame(cx, window);

    assert_eq!(
        text_of(cx, window, SUBMIT_ERROR_ID).as_deref(),
        Some("The files are committed on feedback/2026-11-26-fix-the-zoom (0123abc), but: git could not push to origin: the remote rejected the branch")
    );
    assert!(drawn(cx, window, SUBMIT_RETRY_ID));
    assert!(drawn(cx, window, SUBMIT_START_OVER_ID));
    assert!(!drawn(cx, window, SUBMIT_CONFIRM_ID));
    for id in controls()
        .into_iter()
        .filter(|id| *id != ElementId::from(SUBMIT_BACK_ID))
    {
        assert!(disabled(cx, window, id.clone()), "{id:?}");
    }
    let dir = setup.draft_dir();
    for name in [
        "draft.json",
        "submission.json",
        "screenshot-1.png",
        "screenshot-1.annotated.png",
    ] {
        assert!(dir.join(name).is_file(), "{name}");
    }

    click(cx, window, SUBMIT_RETRY_ID);
    assert_eq!(opened.fake.retries(), [id]);
    assert_eq!(opened.fake.submits().len(), 1, "nothing is uploaded again");
    assert!(submitting(cx, &opened));
    let created = created_intent(alpha);
    opened.state.update(cx, |state, cx| {
        state.dispatch(Action::IntentUpserted(created.clone()), cx)
    });
    opened
        .fake
        .answer_outcome(Ok(outcome_with_intent(id, created)));
    frame(cx, window);
    click(cx, window, RailEntry::Feedback.element_id());
    assert_eq!(
        text_of(cx, window, FEEDBACK_RESULT_ID).as_deref(),
        Some(
            "Created the intent “Fix the zoom”. Files: feedback/2026-11-26-fix-the-zoom (0123abc)."
        )
    );
    assert!(!dir.exists(), "the draft is gone");
}

#[gpui_kit::test]
fn a_pending_submission_survives_a_restart(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let alpha = repos[0].id;
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;
    click(cx, window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);
    let mut partial = outcome_with_intent(id, created_intent(alpha));
    partial.intent = None;
    partial.failed = Some(push_failure());
    opened.fake.answer_outcome(Ok(partial));
    frame(cx, window);
    close(cx, opened);

    let opened = open(cx, &setup, &repos, Some(alpha));
    let window = opened.window;
    open_submit(cx, &opened, no_remotes(&repos));
    assert_eq!(*opened.fake.calls.borrow(), [FeedbackCall::Targets]);
    assert!(text_of(cx, window, SUBMIT_ERROR_ID)
        .is_some_and(|text| text.starts_with(&format!("The files are committed on {BRANCH}"))),);
    assert!(drawn(cx, window, SUBMIT_RETRY_ID));
    assert!(!drawn(cx, window, SUBMIT_CONFIRM_ID));
    assert_eq!(idea_text(cx, &opened), "Fix the zoom");
    assert!(idea_disabled(cx, window));

    click(cx, window, SUBMIT_RETRY_ID);
    assert_eq!(opened.fake.retries(), [id], "the same submission");
}

#[gpui_kit::test]
fn retry_of_a_submission_the_daemon_never_saw_starts_over(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let alpha = repos[0].id;
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;
    click(cx, window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);
    let mut partial = outcome_with_intent(id, created_intent(alpha));
    partial.intent = None;
    partial.failed = Some(push_failure());
    opened.fake.answer_outcome(Ok(partial));
    frame(cx, window);

    opened
        .fake
        .queue_outcome(Err(api(404, "submission_not_found", "no such submission")));
    click(cx, window, SUBMIT_RETRY_ID);
    assert_eq!(
        text_of(cx, window, SUBMIT_ERROR_ID).as_deref(),
        Some(NOTHING_CREATED_TEXT)
    );
    assert!(!idea_disabled(cx, window));
    assert!(!disabled(cx, window, SUBMIT_CONFIRM_ID));
    assert!(!setup.draft_dir().join("submission.json").exists());

    click(cx, window, SUBMIT_CONFIRM_ID);
    let submits = opened.fake.submits();
    assert_eq!(submits.len(), 2);
    assert_ne!(submits[1].0.submission_id, id);
}

#[gpui_kit::test]
fn a_silent_daemon_frees_the_screen(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;
    click(cx, window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);

    cx.executor()
        .advance_clock(SUBMIT_TIMEOUT - Duration::from_secs(1));
    frame(cx, window);
    assert!(submitting(cx, &opened), "still waiting");

    cx.executor().advance_clock(Duration::from_secs(2));
    frame(cx, window);
    assert_eq!(
        text_of(cx, window, SUBMIT_ERROR_ID).as_deref(),
        Some("The daemon did not answer")
    );
    assert!(!submitting(cx, &opened));
    for id in [SUBMIT_BACK_ID, SUBMIT_RETRY_ID, SUBMIT_START_OVER_ID] {
        assert!(!disabled(cx, window, id), "{id}");
    }
    assert_eq!(
        cx.read(|cx| opened.feedback.read(cx).pending().map(|p| p.submission_id)),
        Some(id),
        "the pending id is kept"
    );
    // The daemon took it: the retry route first, which knows of no such
    // submission, so the same request goes again under the same id.
    opened
        .fake
        .queue_outcome(Err(api(404, "submission_not_found", "no such submission")));
    click(cx, window, SUBMIT_RETRY_ID);
    assert_eq!(opened.fake.retries(), [id]);
    let submits = opened.fake.submits();
    assert_eq!(submits.len(), 2);
    assert_eq!(submits[1].0.submission_id, id);
    assert_eq!(submits[1].0.main_idea, "Fix the zoom");
    assert!(submitting(cx, &opened));
}

#[gpui_kit::test]
fn starting_over_a_submission_that_may_still_run_asks_first(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;
    click(cx, window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);
    cx.executor()
        .advance_clock(SUBMIT_TIMEOUT + Duration::from_secs(1));
    frame(cx, window);

    click(cx, window, SUBMIT_START_OVER_ID);
    assert!(with_window(cx, window, |window, cx| window.has_active_dialog(cx)));
    with_window(cx, window, |window, _| {
        assert!(window
            .find_all_by_role(Role::Label)
            .iter()
            .any(|label| label.value() == Some(START_OVER_RISK_TEXT)));
        assert_eq!(window.find(START_OVER_CANCEL_ID).focused(), Some(true));
    });
    click(cx, window, START_OVER_CANCEL_ID);
    assert!(!with_window(cx, window, |window, cx| window.has_active_dialog(cx)));
    assert!(
        cx.read(|cx| opened.feedback.read(cx).locked()),
        "nothing changed"
    );

    click(cx, window, SUBMIT_START_OVER_ID);
    click(cx, window, START_OVER_CONFIRM_ID);
    assert!(!cx.read(|cx| opened.feedback.read(cx).locked()));
    assert!(drawn(cx, window, SUBMIT_CONFIRM_ID));
    click(cx, window, SUBMIT_CONFIRM_ID);
    let submits = opened.fake.submits();
    assert_eq!(submits.len(), 2);
    assert_ne!(submits[1].0.submission_id, id);
}

#[gpui_kit::test]
fn a_forgotten_submission_keeps_its_target_after_a_restart(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let (alpha, beta) = (repos[0].id, repos[1].id);
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;
    click(cx, window, SUBMIT_REPO_ID);
    with_window(cx, window, |window, cx| {
        window
            .within("popup-menu")
            .click(submit_repo_item_id(beta), cx)
    });
    frame(cx, window);
    click(cx, window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);
    let mut partial = outcome_with_intent(id, created_intent(beta));
    partial.intent = None;
    partial.failed = Some(FeedbackFailure {
        step: FeedbackStep::CreateIntent,
        code: "store_error".into(),
        message: "the intent could not be saved".into(),
    });
    opened.fake.answer_outcome(Ok(partial));
    frame(cx, window);
    close(cx, opened);

    // After a restart the screen would start on the meta repository, alpha.
    let opened = open(cx, &setup, &repos, Some(alpha));
    let window = opened.window;
    open_submit(cx, &opened, no_remotes(&repos));
    assert_eq!(text_of(cx, window, SUBMIT_REPO_ID).as_deref(), Some("beta"));
    opened
        .fake
        .queue_outcome(Err(api(404, "submission_not_found", "no such submission")));
    click(cx, window, SUBMIT_RETRY_ID);
    assert_eq!(
        text_of(cx, window, SUBMIT_ERROR_ID).as_deref(),
        Some(NOTHING_CREATED_TEXT)
    );
    assert!(!cx.read(|cx| opened.feedback.read(cx).locked()));
    assert_eq!(
        text_of(cx, window, SUBMIT_REPO_ID).as_deref(),
        Some("beta"),
        "the repository it was sent to"
    );
}

#[gpui_kit::test]
fn a_repository_that_is_gone_forgets_the_submission(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;
    opened
        .fake
        .queue_outcome(Err(api(404, "repo_not_found", "repo not found")));
    click(cx, window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);
    assert_eq!(
        text_of(cx, window, SUBMIT_ERROR_ID).as_deref(),
        Some(REPO_GONE_TEXT)
    );
    assert_eq!(
        cx.read(|cx| opened.feedback.read(cx).pending().cloned()),
        None
    );
    assert!(!idea_disabled(cx, window));
    click(cx, window, SUBMIT_CONFIRM_ID);
    let submits = opened.fake.submits();
    assert_eq!(submits.len(), 2);
    assert_ne!(submits[1].0.submission_id, id);
}

#[gpui_kit::test]
fn success_while_another_pane_shows_switches_nothing(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let alpha = repos[0].id;
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;
    click(cx, window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);

    click(cx, window, RailEntry::Settings.element_id());
    let created = created_intent(alpha);
    opened.state.update(cx, |state, cx| {
        state.dispatch(Action::IntentUpserted(created.clone()), cx)
    });
    opened
        .fake
        .answer_outcome(Ok(outcome_with_intent(id, created.clone())));
    frame(cx, window);
    assert_eq!(
        opened
            .shell
            .read_with(cx, |shell, cx| shell.rail_active(cx)),
        Some(RailEntry::Settings)
    );
    let intents = intents_pane(cx, &opened.shell);
    assert_ne!(
        intents.read_with(cx, |pane, _| *pane.screen()),
        Screen::Detail(created.id)
    );

    click(cx, window, RailEntry::Feedback.element_id());
    assert_eq!(screen(cx, &opened), FeedbackScreen::Draft);
    assert_eq!(
        text_of(cx, window, FEEDBACK_RESULT_ID).as_deref(),
        Some(
            "Created the intent “Fix the zoom”. Files: feedback/2026-11-26-fix-the-zoom (0123abc)."
        )
    );
    assert!(!setup.draft_dir().exists());
}

#[gpui_kit::test]
fn a_locked_draft_without_attachments_can_be_discarded(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let alpha = repos[0].id;
    let opened = ready_to_send(cx, &setup, &repos);
    click(cx, opened.window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);
    let mut partial = outcome_with_intent(id, created_intent(alpha));
    partial.intent = None;
    partial.failed = Some(push_failure());
    opened.fake.answer_outcome(Ok(partial));
    frame(cx, opened.window);
    close(cx, opened);
    // Its files went missing meanwhile.
    for name in ["screenshot-1.png", "screenshot-1.annotated.png"] {
        fs::remove_file(setup.draft_dir().join(name)).unwrap();
    }

    let opened = open(cx, &setup, &repos, Some(alpha));
    let window = opened.window;
    assert!(cx.read(|cx| opened.feedback.read(cx).draft().is_empty()));
    assert!(cx.read(|cx| opened.feedback.read(cx).pending().is_some()));
    assert!(disabled(cx, window, FEEDBACK_SUBMIT_ID));
    assert!(!disabled(cx, window, FEEDBACK_DISCARD_ID), "not a dead end");
    click(cx, window, FEEDBACK_DISCARD_ID);
    click(cx, window, DISCARD_CONFIRM_ID);
    assert!(!setup.draft_dir().exists());
    assert_eq!(
        cx.read(|cx| opened.feedback.read(cx).pending().cloned()),
        None
    );
    assert!(!disabled(cx, window, FEEDBACK_CAPTURE_ID));
}

#[gpui_kit::test]
fn a_capture_or_a_file_landing_while_locked_is_refused(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let alpha = repos[0].id;
    let opened = ready_to_send(cx, &setup, &repos);
    let (window, feedback) = (opened.window, opened.feedback.clone());
    let lock = |f: &mut Feedback, cx: &mut gpui_kit::Context<Feedback>| {
        f.begin_submission(cx);
        f.mark_sent(
            SentTo {
                repo_id: alpha,
                send_as: SendAs::Intent,
                idea: None,
            },
            cx,
        );
    };

    // A capture that started before the lock lands after it.
    act(cx, window, &feedback, |f, window, cx| {
        f.capture(window, cx);
        lock(f, cx);
    });
    run_hop(cx, window, &setup);
    run_hop(cx, window, &setup);
    let draft = cx.read(|cx| feedback.read(cx).draft().clone());
    assert_eq!(draft.attachments.len(), 1, "the picture is not added");
    assert_eq!(
        cx.read(|cx| feedback.read(cx).notices().to_vec()),
        [SUBMISSION_LOCKED_TEXT]
    );
    assert!(!setup.draft_dir().join("screenshot-2.png").exists());

    // A file being copied in when the lock comes.
    act(cx, window, &feedback, |f, _, cx| f.forget_submission(cx));
    setup.pick(vec![setup.picked_file("good.png", &png(64, 48))]);
    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx);
        lock(f, cx);
    });
    frame(cx, window);
    assert_eq!(cx.read(|cx| feedback.read(cx).draft().attachments.len()), 1);
    let names = common::feedback::file_names(&setup.draft_dir());
    assert!(
        !names.iter().any(|name| name.starts_with("image-")),
        "{names:?}"
    );
}

#[gpui_kit::test]
fn no_send_starts_while_a_capture_runs(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_to_send(cx, &setup, &repos);
    let (window, feedback) = (opened.window, opened.feedback.clone());
    act(cx, window, &feedback, |f, window, cx| f.capture(window, cx));
    assert!(cx.read(|cx| feedback.read(cx).capturing()));
    assert!(disabled(cx, window, SUBMIT_CONFIRM_ID));
    click(cx, window, SUBMIT_CONFIRM_ID);
    assert!(opened.fake.submits().is_empty());
    assert!(!setup.draft_dir().join("submission.json").exists());
}

#[gpui_kit::test]
fn the_daemons_limits_are_checked_before_uploading(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_to_send(cx, &setup, &repos);
    let window = opened.window;
    // The annotated picture grew past what the daemon takes for an image.
    let annotated = setup.draft_dir().join("screenshot-1.annotated.png");
    let mut big = png(16, 16);
    big.resize(10 * 1024 * 1024 + 1, 0);
    fs::write(&annotated, big).unwrap();

    click(cx, window, SUBMIT_CONFIRM_ID);
    assert_eq!(
        text_of(cx, window, SUBMIT_ERROR_ID).as_deref(),
        Some("Attachment 1 cannot be sent: an image can be at most 10 MB.")
    );
    assert!(opened.fake.submits().is_empty(), "nothing is sent");
    assert!(!setup.draft_dir().join("submission.json").exists());
    assert!(!idea_disabled(cx, window));
}

#[gpui_kit::test]
fn escape_and_back_return_the_focus(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = open(cx, &setup, &repos, Some(repos[0].id));
    let window = opened.window;
    screenshot(cx, &opened, &setup, &[]);
    opened.fake.queue_targets(Ok(no_remotes(&repos)));
    opened.fake.queue_targets(Ok(no_remotes(&repos)));

    let submit_focused = |cx: &mut TestAppContext| {
        with_window(cx, window, |window, _| {
            window
                .try_find(FEEDBACK_SUBMIT_ID)
                .and_then(|b| b.focused())
        }) == Some(true)
    };
    for _ in 0..60 {
        if submit_focused(cx) {
            break;
        }
        press(cx, window, "tab");
    }
    assert!(submit_focused(cx), "Tab reaches Submit…");

    press(cx, window, "enter");
    assert_eq!(screen(cx, &opened), FeedbackScreen::Submit);
    press(cx, window, "escape");
    assert_eq!(screen(cx, &opened), FeedbackScreen::Draft);
    assert!(submit_focused(cx), "the focus is back on Submit…");

    press(cx, window, "enter");
    assert_eq!(screen(cx, &opened), FeedbackScreen::Submit);
    click(cx, window, SUBMIT_BACK_ID);
    assert_eq!(screen(cx, &opened), FeedbackScreen::Draft);
    assert!(submit_focused(cx));
}

// ---- The idea package -------------------------------------------------------

/// The three parts' boxes, in the order the screen shows them.
const IDEA_PARTS: [&str; 3] = [IDEA_DEMO_ID, IDEA_DIAGRAMS_ID, IDEA_EXPLAINER_ID];

/// Like [`ready_to_send`], with alpha on GitHub (private), `gh` ready and
/// `agent-browser` ready or not.
fn ready_for_an_idea(
    cx: &mut TestAppContext,
    setup: &FeedbackSetup,
    repos: &[Repo],
    agent_browser: bool,
) -> Opened {
    let opened = open(cx, setup, repos, Some(repos[0].id));
    screenshot(cx, &opened, setup, &["the toolbar"]);
    let list: Vec<_> = repos.iter().map(|r| (r.id, true, Some(false))).collect();
    open_submit(cx, &opened, targets(&list, true, agent_browser));
    type_idea(cx, opened.window, "Fix the zoom");
    opened
}

fn parts(explainer: bool, diagrams: bool, demo_video: bool) -> IdeaOptions {
    IdeaOptions {
        explainer,
        diagrams,
        demo_video,
    }
}

/// Which of the four boxes are checked: the parent, then the parts.
fn ticks(cx: &mut TestAppContext, window: WindowHandle<Root>) -> [bool; 4] {
    [
        checked(cx, window, IDEA_CHECK_ID),
        checked(cx, window, IDEA_DEMO_ID),
        checked(cx, window, IDEA_DIAGRAMS_ID),
        checked(cx, window, IDEA_EXPLAINER_ID),
    ]
}

#[gpui_kit::test]
fn the_idea_boxes_appear_under_the_intent_choice(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_for_an_idea(cx, &setup, &repos, true);
    let window = opened.window;

    with_window(cx, window, |window, _| {
        let parent = window.find(IDEA_CHECK_ID);
        assert_eq!(parent.role(), Some(Role::CheckBox));
        assert_eq!(parent.label(), Some("Prepare an idea PR for teammates"));
        assert_eq!(parent.checked(), Some(false));
        assert_ne!(parent.disabled(), Some(true));
    });
    for id in IDEA_PARTS {
        assert!(!drawn(cx, window, id), "{id}");
    }

    click(cx, window, IDEA_CHECK_ID);
    for (id, label) in IDEA_PARTS
        .into_iter()
        .zip(["Demo video", "Diagrams", "Explainer doc"])
    {
        assert_eq!(text_of(cx, window, id).as_deref(), Some(label));
        with_window(cx, window, |window, _| {
            assert_eq!(window.find(id).role(), Some(Role::CheckBox));
        });
    }
    // Ticking the parent ticks "Explainer doc".
    assert_eq!(ticks(cx, window), [true, false, false, true]);
    assert_eq!(
        text_of(cx, window, SUBMIT_SUMMARY_ID).as_deref(),
        Some("Creates the issue folder with 1 screenshot. An agent adds an explainer doc on an idea branch, and you open the pull request.")
    );
    // The parts sit under the parent, which sits under the intent choice.
    with_window(cx, window, |window, _| {
        let left = |id: &'static str| window.find(id).bounds().origin.x;
        assert!(left(IDEA_EXPLAINER_ID) > left(IDEA_CHECK_ID));
        assert!(left(IDEA_CHECK_ID) > left(SEND_INTENT_ID));
        let top = |id: &'static str| window.find(id).bounds().origin.y;
        assert!(top(SEND_INTENT_ID) < top(IDEA_CHECK_ID));
        assert!(top(IDEA_CHECK_ID) < top(IDEA_DEMO_ID));
        assert!(top(IDEA_EXPLAINER_ID) < top(SEND_ISSUE_ID));
    });

    // Choosing "A GitHub issue" hides all four; back on the intent, the
    // parent is there again.
    click(cx, window, SEND_ISSUE_ID);
    assert!(!drawn(cx, window, IDEA_CHECK_ID));
    for id in IDEA_PARTS {
        assert!(!drawn(cx, window, id), "{id}");
    }
    assert_eq!(
        text_of(cx, window, SUBMIT_CONFIRM_ID).as_deref(),
        Some("Create issue")
    );
    click(cx, window, SEND_INTENT_ID);
    assert_eq!(ticks(cx, window), [true, false, false, true]);

    // Unticking the parent hides the parts again.
    click(cx, window, IDEA_CHECK_ID);
    assert!(!checked(cx, window, IDEA_CHECK_ID));
    assert!(!drawn(cx, window, IDEA_EXPLAINER_ID));

    // Nothing is remembered when the screen opens again, nor in the draft.
    click(cx, window, IDEA_CHECK_ID);
    click(cx, window, IDEA_DIAGRAMS_ID);
    click(cx, window, SUBMIT_BACK_ID);
    let list: Vec<_> = repos.iter().map(|r| (r.id, true, Some(false))).collect();
    open_submit(cx, &opened, targets(&list, true, true));
    assert!(!checked(cx, window, IDEA_CHECK_ID));
    assert!(!drawn(cx, window, IDEA_DIAGRAMS_ID));
    let draft = fs::read_to_string(setup.draft_dir().join("draft.json")).unwrap();
    assert!(!draft.contains("diagrams"), "{draft}");
}

#[gpui_kit::test]
fn demo_video_needs_agent_browser(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_for_an_idea(cx, &setup, &repos, false);
    let window = opened.window;
    click(cx, window, IDEA_CHECK_ID);

    assert!(disabled(cx, window, IDEA_DEMO_ID));
    assert!(!disabled(cx, window, IDEA_DIAGRAMS_ID));
    assert!(!disabled(cx, window, IDEA_EXPLAINER_ID));
    assert_eq!(
        text_of(cx, window, IDEA_DEMO_HELP_ID).as_deref(),
        Some("Install agent-browser to record a demo video.")
    );
    click(cx, window, IDEA_DEMO_ID);
    assert!(!checked(cx, window, IDEA_DEMO_ID), "a disabled box stays");

    // Ready: enabled, and the help says what it needs.
    click(cx, window, SUBMIT_BACK_ID);
    let list: Vec<_> = repos.iter().map(|r| (r.id, true, Some(false))).collect();
    open_submit(cx, &opened, targets(&list, true, true));
    click(cx, window, IDEA_CHECK_ID);
    assert!(!disabled(cx, window, IDEA_DEMO_ID));
    assert_eq!(
        text_of(cx, window, IDEA_DEMO_HELP_ID).as_deref(),
        Some("Only works for projects a browser can show.")
    );
    click(cx, window, IDEA_DEMO_ID);
    assert!(checked(cx, window, IDEA_DEMO_ID));

    // While the targets load, it is disabled with no sentence.
    click(cx, window, SUBMIT_BACK_ID);
    click(cx, window, FEEDBACK_SUBMIT_ID);
    click(cx, window, IDEA_CHECK_ID);
    assert!(disabled(cx, window, IDEA_DEMO_ID));
    assert!(!drawn(cx, window, IDEA_DEMO_HELP_ID));
}

#[gpui_kit::test]
fn an_idea_request_carries_the_ticked_parts(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_for_an_idea(cx, &setup, &repos, true);
    let window = opened.window;

    click(cx, window, IDEA_CHECK_ID);
    click(cx, window, IDEA_DIAGRAMS_ID);
    assert_eq!(
        text_of(cx, window, SUBMIT_CONFIRM_ID).as_deref(),
        Some("Create intent")
    );
    click(cx, window, SUBMIT_CONFIRM_ID);

    let submits = opened.fake.submits();
    assert_eq!(submits.len(), 1);
    let request = &submits[0].0;
    assert_eq!(request.send_as, SendAs::Intent);
    assert_eq!(request.idea, Some(parts(true, true, false)));
    assert_eq!(request.repo_id, repos[0].id);
    // Every box waits with the rest of the screen.
    for id in [
        IDEA_CHECK_ID,
        IDEA_DEMO_ID,
        IDEA_DIAGRAMS_ID,
        IDEA_EXPLAINER_ID,
    ] {
        assert!(disabled(cx, window, id), "{id}");
    }

    // Without the parent ticked, no idea goes.
    let other = FeedbackSetup::new();
    let opened = ready_for_an_idea(cx, &other, &repos, true);
    click(cx, opened.window, SUBMIT_CONFIRM_ID);
    assert_eq!(opened.fake.submits()[0].0.idea, None);
}

#[gpui_kit::test]
fn no_part_means_no_confirm(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_for_an_idea(cx, &setup, &repos, true);
    let window = opened.window;
    click(cx, window, IDEA_CHECK_ID);

    click(cx, window, IDEA_EXPLAINER_ID);

    assert_eq!(ticks(cx, window), [true, false, false, false]);
    assert_eq!(
        text_of(cx, window, SUBMIT_SUMMARY_ID).as_deref(),
        Some("Choose at least one part.")
    );
    assert!(disabled(cx, window, SUBMIT_CONFIRM_ID));
    click(cx, window, SUBMIT_CONFIRM_ID);
    assert!(opened.fake.submits().is_empty());

    click(cx, window, IDEA_DIAGRAMS_ID);
    assert!(!disabled(cx, window, SUBMIT_CONFIRM_ID));
}

#[gpui_kit::test]
fn the_idea_boxes_are_part_of_the_locked_draft(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_for_an_idea(cx, &setup, &repos, true);
    let window = opened.window;
    click(cx, window, IDEA_CHECK_ID);
    click(cx, window, IDEA_DIAGRAMS_ID);
    // A 5xx: something may exist under the id.
    opened
        .fake
        .queue_outcome(Err(api(500, "store_error", "the store could not save")));
    click(cx, window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);

    assert!(cx.read(|cx| opened.feedback.read(cx).locked()));
    assert_eq!(ticks(cx, window), [true, false, true, true]);
    for id in [
        IDEA_CHECK_ID,
        IDEA_DEMO_ID,
        IDEA_DIAGRAMS_ID,
        IDEA_EXPLAINER_ID,
    ] {
        assert!(disabled(cx, window, id), "{id}");
    }
    click(cx, window, IDEA_DIAGRAMS_ID);
    assert!(checked(cx, window, IDEA_DIAGRAMS_ID), "it stays as sent");
    let json = fs::read_to_string(setup.draft_dir().join("submission.json")).unwrap();
    assert!(json.contains("\"diagrams\": true"), "{json}");
    close(cx, opened);

    // After a restart the screen shows what was sent, and the request that
    // goes again (the daemon has no record of it) is the same.
    let opened = open(cx, &setup, &repos, Some(repos[0].id));
    let window = opened.window;
    let list: Vec<_> = repos.iter().map(|r| (r.id, true, Some(false))).collect();
    open_submit(cx, &opened, targets(&list, true, true));
    assert_eq!(ticks(cx, window), [true, false, true, true]);
    assert!(disabled(cx, window, IDEA_CHECK_ID));
    assert_eq!(
        text_of(cx, window, SUBMIT_SUMMARY_ID).as_deref(),
        Some("Creates the issue folder with 1 screenshot. An agent adds an explainer doc and diagrams on an idea branch, and you open the pull request.")
    );
    opened
        .fake
        .queue_outcome(Err(api(404, "submission_not_found", "no such submission")));
    click(cx, window, SUBMIT_RETRY_ID);
    assert_eq!(opened.fake.retries(), [id]);
    let submits = opened.fake.submits();
    assert_eq!(submits.len(), 1);
    assert_eq!(submits[0].0.submission_id, id);
    assert_eq!(submits[0].0.idea, Some(parts(true, true, false)));

    // Start over keeps the boxes as they were sent, and they can change.
    opened
        .fake
        .answer_outcome(Err(api(500, "store_error", "the store could not save")));
    frame(cx, window);
    click(cx, window, SUBMIT_START_OVER_ID);
    assert!(!cx.read(|cx| opened.feedback.read(cx).locked()));
    assert_eq!(ticks(cx, window), [true, false, true, true]);
    assert!(!disabled(cx, window, IDEA_DIAGRAMS_ID));
    click(cx, window, IDEA_DIAGRAMS_ID);
    click(cx, window, SUBMIT_CONFIRM_ID);
    let submits = opened.fake.submits();
    assert_eq!(submits.len(), 2);
    assert_ne!(submits[1].0.submission_id, id);
    assert_eq!(submits[1].0.idea, Some(parts(true, false, false)));
}

#[gpui_kit::test]
fn a_refused_demo_video_is_unticked_and_the_targets_reload(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_for_an_idea(cx, &setup, &repos, true);
    let window = opened.window;
    click(cx, window, IDEA_CHECK_ID);
    click(cx, window, IDEA_DEMO_ID);
    // The daemon's agent-browser went away meanwhile: refused before
    // anything was made.
    opened.fake.queue_outcome(Err(api(
        409,
        "tool_missing",
        "agent-browser is not installed",
    )));
    click(cx, window, SUBMIT_CONFIRM_ID);
    let id = sent_id(&opened);

    assert_eq!(
        text_of(cx, window, SUBMIT_ERROR_ID).as_deref(),
        Some("Install agent-browser to record a demo video.")
    );
    // The form stays editable; "Demo video" is unticked and disabled
    // until the targets say otherwise, and they are asked for again.
    assert!(!cx.read(|cx| opened.feedback.read(cx).locked()));
    assert_eq!(ticks(cx, window), [true, false, false, true]);
    assert!(disabled(cx, window, IDEA_DEMO_ID));
    assert!(!disabled(cx, window, IDEA_DIAGRAMS_ID));
    assert_eq!(
        text_of(cx, window, IDEA_DEMO_HELP_ID).as_deref(),
        Some("Install agent-browser to record a demo video.")
    );
    let targets_calls = || {
        opened
            .fake
            .calls
            .borrow()
            .iter()
            .filter(|call| **call == FeedbackCall::Targets)
            .count()
    };
    assert_eq!(targets_calls(), 2, "the targets are asked for again");
    let list: Vec<_> = repos.iter().map(|r| (r.id, true, Some(false))).collect();
    opened.fake.answer_targets(Ok(targets(&list, true, false)));
    frame(cx, window);
    assert!(disabled(cx, window, IDEA_DEMO_ID));
    assert_eq!(
        text_of(cx, window, SUBMIT_ERROR_ID).as_deref(),
        Some("Install agent-browser to record a demo video."),
        "the sentence stays"
    );

    // The same id is sent again with what is ticked now.
    click(cx, window, IDEA_DIAGRAMS_ID);
    click(cx, window, SUBMIT_CONFIRM_ID);
    let submits = opened.fake.submits();
    assert_eq!(submits.len(), 2);
    assert_eq!(submits[1].0.submission_id, id, "the same id");
    assert_eq!(submits[1].0.idea, Some(parts(true, true, false)));
}

#[gpui_kit::test]
fn a_demo_video_comes_back_when_the_reloaded_targets_have_agent_browser(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let repos = two_repos();
    let opened = ready_for_an_idea(cx, &setup, &repos, true);
    let window = opened.window;
    click(cx, window, IDEA_CHECK_ID);
    click(cx, window, IDEA_DEMO_ID);
    opened.fake.queue_outcome(Err(api(
        409,
        "tool_missing",
        "agent-browser is not installed",
    )));
    click(cx, window, SUBMIT_CONFIRM_ID);
    assert!(disabled(cx, window, IDEA_DEMO_ID));

    let list: Vec<_> = repos.iter().map(|r| (r.id, true, Some(false))).collect();
    opened.fake.answer_targets(Ok(targets(&list, true, true)));
    frame(cx, window);

    assert!(!disabled(cx, window, IDEA_DEMO_ID));
    assert!(!checked(cx, window, IDEA_DEMO_ID), "it stays unticked");
    assert_eq!(
        text_of(cx, window, IDEA_DEMO_HELP_ID).as_deref(),
        Some("Only works for projects a browser can show.")
    );
}

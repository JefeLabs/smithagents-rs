//! The Feedback pane's Submit screen, one scrolling column, top to bottom:
//! "Back"; the attachments, read-only; the main idea (kept in the draft as
//! it is typed); the kind (three buttons); the repository (a menu); "Send it
//! as" (an intent, or a GitHub issue when the repository's `origin` is on
//! GitHub and `gh` is ready, else why not); what Create will do; the
//! public-repository warning; the error line; and Create, or Retry and
//! Start over.
//!
//! One command at a time: while a submission or a retry is under way every
//! control waits and the pressed button shows the spinner; nothing changes
//! before the daemon answers. Sending to GitHub a repository that is (or may
//! be) public will show (an issue, or a plain intent whose agent opens a
//! pull request there: see [`public_warning`]) asks first, every time. The
//! limits the daemon checks are checked here first, so nothing is uploaded
//! that it would refuse.
//!
//! The draft keeps one submission id ([`Pending`]) for every send of it. A
//! success deletes the draft, says what was created on the draft screen and,
//! while the Feedback pane is the one showing, opens a created intent in the
//! Intents pane. The target is written before a request leaves; while
//! something may exist under the id (a step failed after the commit, no
//! answer came, the connection broke after sending, a 5xx or an unknown
//! code: see [`send_failure`]) the text, kind and target stay as sent, and
//! the screen offers Retry (the retry route first; the same request again
//! only when the daemon has no record of it) and Start over (a new
//! submission; what was committed stays; asked first while no answer came).
//! A request that never left, and a refusal after which the daemon made
//! nothing, leave the form editable under the same id; the refusal's
//! sentence stays until the next send. A refused re-upload (sent only after
//! an earlier request under the id went unanswered, which may still make
//! its commit) leaves the draft locked as it was.
//!
//! Escape and "Back" go back to the draft (not while a command is under way),
//! and the focus returns to where it was when the screen opened.
//!
//! The idea package's boxes, under the intent's radio, are in [`idea`].

mod idea;

use std::borrow::Cow;
use std::fs;
use std::path::Path;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::radio::Radio;
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, AnyElement, App, AppContext as _, Context, ElementId, Entity, FocusHandle,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _,
    WeakEntity, Window,
};
use smithagents_client::{ClientError, FeedbackFile};
use smithagents_core::feedback::{
    effective_meta_repo, FeedbackKind, FeedbackOutcome, FeedbackRequest, FeedbackTarget,
    FeedbackTargets, IdeaOptions, SendAs, SubmissionId,
};
use smithagents_core::ids::RepoId;

use super::card::cards;
use super::{confirm, Confirmation, FeedbackPane, FeedbackScreen, Paint};
use crate::feedback::state::{Pending, SentTo, SUBMIT_TIMEOUT};
use crate::feedback::submit_vm::{
    asks_to_acknowledge, can_confirm, confirm_label, failure_text, issue_reason,
    maybe_public_warning, not_recorded_text, public_warning, result_text, send_failure,
    shows_issue_url, starting_repo, submit_error_text, summary, upload_problem, SendFailure, KINDS,
};
use crate::feedback::text::{
    BACK_LABEL, CHOOSE_REPO_TEXT, DRAFT_BUSY_TEXT, DRAFT_NOT_READY_TEXT, KIND_LABEL,
    MAIN_IDEA_HINT, MAIN_IDEA_LABEL, MAY_HAVE_BEEN_SENT_TEXT, PUBLIC_CONFIRM_CANCEL,
    PUBLIC_CONFIRM_TITLE, REPO_LABEL, RETRY_LABEL, SEND_AS_INTENT, SEND_AS_ISSUE, SEND_AS_LABEL,
    START_OVER_CANCEL, START_OVER_LABEL, START_OVER_RISK_TEXT, START_OVER_TITLE,
    START_OVER_TOOLTIP, SUBMIT_BUSY_TEXT,
};
use crate::intent_vm::kind_label;
use crate::link::LinkError;
use crate::view_models::{
    repo_title, RepoChoiceVm, SettingsSection, DAEMON_SILENT_TEXT, NEED_REPO_TEXT,
    NOT_CONNECTED_TEXT,
};
use crate::views::answered;
use crate::views::rail::RailEntry;

pub use idea::{
    IDEA_CHECK_ID, IDEA_DEMO_HELP_ID, IDEA_DEMO_ID, IDEA_DIAGRAMS_ID, IDEA_EXPLAINER_ID,
};

/// Key context of the Submit screen (on the pane while it shows the
/// screen), for Escape.
pub const SUBMIT_KEY_CONTEXT: &str = "FeedbackSubmit";
pub const SUBMIT_BACK_ID: &str = "feedback-submit-back";
/// Element id of the frame around the main idea's text box (the box itself
/// is named "Main idea").
pub const SUBMIT_IDEA_ID: &str = "feedback-main-idea";
/// Element ids of the main idea's name above its box and the hint below.
pub const SUBMIT_IDEA_LABEL_ID: &str = "feedback-main-idea-label";
pub const SUBMIT_IDEA_HINT_ID: &str = "feedback-main-idea-hint";
/// Element id of the repository button: the menu's, or the way to Settings.
pub const SUBMIT_REPO_ID: &str = "feedback-repo";
pub const SEND_INTENT_ID: &str = "feedback-send-intent";
pub const SEND_ISSUE_ID: &str = "feedback-send-issue";
/// Element id of why "A GitHub issue" cannot be chosen.
pub const SEND_ISSUE_REASON_ID: &str = "feedback-send-issue-reason";
pub const SUBMIT_SUMMARY_ID: &str = "feedback-submit-summary";
pub const PUBLIC_WARNING_ID: &str = "feedback-public-warning";
/// Element id of the screen's one error line (a status).
pub const SUBMIT_ERROR_ID: &str = "feedback-submit-error";
pub const SUBMIT_CONFIRM_ID: &str = "feedback-submit-confirm";
pub const SUBMIT_RETRY_ID: &str = "feedback-submit-retry";
pub const SUBMIT_START_OVER_ID: &str = "feedback-submit-start-over";
/// Element ids in the dialog a send to a public repository opens.
pub const PUBLIC_CONFIRM_TITLE_ID: &str = "feedback-public-confirm-title";
pub const PUBLIC_CANCEL_ID: &str = "feedback-public-cancel";
pub const PUBLIC_CONFIRM_ID: &str = "feedback-public-confirm";
/// Element ids in the dialog Start over opens while the daemon may still be
/// making the submission.
pub const START_OVER_TITLE_ID: &str = "feedback-start-over-title";
pub const START_OVER_CANCEL_ID: &str = "feedback-start-over-cancel";
pub const START_OVER_CONFIRM_ID: &str = "feedback-start-over-confirm";

const SCREEN_ID: &str = "feedback-submit";
/// The main idea grows from this many lines to this many, then scrolls.
const IDEA_ROWS: (usize, usize) = (3, 10);
/// The repository menu's label and widest, in rems.
const REPO_MENU_LABEL: &str = "Send feedback to";
const REPO_MENU_MAX_WIDTH: f32 = 26.;

/// Element id of `kind`'s button: `feedback-kind-<wire name>`.
pub fn submit_kind_id(kind: FeedbackKind) -> ElementId {
    ElementId::Name(format!("feedback-kind-{}", kind.intent_kind().wire_name()).into())
}

/// Element id of `repo`'s entry in the repository menu.
pub fn submit_repo_item_id(repo: RepoId) -> ElementId {
    ElementId::Name(format!("feedback-repo-{repo}").into())
}

/// What a press sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// The draft as a request (Create).
    Submit,
    /// The retry route: the steps of the recorded submission that did not
    /// succeed (Retry, always first).
    Retry,
    /// The same request again under the same id: what Retry sends when the
    /// daemon has no record of a submission whose answer never came.
    Reupload,
}

/// The Submit screen's state, kept in the pane.
pub(super) struct Submit {
    /// The main idea's text box.
    idea: Entity<TextareaState>,
    /// The daemon's targets; `None` while they load.
    targets: Option<FeedbackTargets>,
    targets_task: Option<Task<()>>,
    /// The repository picked in the menu.
    picked_repo: Option<RepoId>,
    send_as: SendAs,
    /// The idea package's parts as ticked; `None` while "Prepare an idea PR
    /// for teammates" is not.
    package: Option<IdeaOptions>,
    /// The screen's error line.
    error: Option<String>,
    /// The last send's sentence and the submission it is about: shown again
    /// whenever the screen opens on that submission, until the next send.
    answer: Option<(SubmissionId, String)>,
    /// The command under way, and its task; dropping the task gives it up.
    busy: Option<Attempt>,
    command: Option<Task<()>>,
    /// Where the focus was when the screen opened.
    focus_before: Option<FocusHandle>,
}

impl Submit {
    pub(super) fn new(window: &mut Window, cx: &mut App) -> Self {
        let idea = cx.new(|cx| TextareaState::new(window, cx).auto_grow(IDEA_ROWS.0, IDEA_ROWS.1));
        Self {
            idea,
            targets: None,
            targets_task: None,
            picked_repo: None,
            send_as: SendAs::Intent,
            package: None,
            error: None,
            answer: None,
            busy: None,
            command: None,
            focus_before: None,
        }
    }
}

/// Keeps the draft's main idea as the box changes.
pub(super) fn follow_main_idea(
    submit: &Submit,
    window: &mut Window,
    cx: &mut Context<FeedbackPane>,
) -> Subscription {
    cx.subscribe_in(
        &submit.idea,
        window,
        |this, idea, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                let text = idea.read(cx).value().to_string();
                this.feedback
                    .update(cx, |feedback, cx| feedback.set_main_idea(text, cx));
                cx.notify();
            }
        },
    )
}

/// The size of each file on disk; `None` for one that is not a regular file.
fn sizes_on_disk(files: Vec<FeedbackFile>) -> Vec<(FeedbackFile, Option<u64>)> {
    files
        .into_iter()
        .map(|file| {
            let size = size_of(&file.path);
            (file, size)
        })
        .collect()
}

fn size_of(path: &Path) -> Option<u64> {
    fs::symlink_metadata(path)
        .ok()
        .filter(|meta| meta.file_type().is_file())
        .map(|meta| meta.len())
}

/// Whether the daemon answered (as opposed to no answer coming at all).
fn answered_by_daemon(error: &LinkError) -> bool {
    matches!(error, LinkError::Client(ClientError::Api { .. }))
}

/// What the repository line shows.
enum RepoLine {
    /// No repository yet: the way to Settings.
    Missing,
    /// The chosen repository's name (or "Choose a repository"), and the
    /// menu of all.
    Menu {
        label: String,
        choices: Vec<RepoChoiceVm>,
    },
}

impl FeedbackPane {
    /// Whether a submission or a retry is under way (the pressed button
    /// shows the spinner).
    pub fn submitting(&self) -> bool {
        self.submit.busy.is_some()
    }

    /// The main idea's text box as typed.
    pub fn main_idea_text(&self, cx: &App) -> String {
        self.submit.idea.read(cx).value().to_string()
    }

    /// Opens the Submit screen and asks the daemon for the targets. Needs an
    /// attachment, no work on the draft and the daemon ("Submit…" is
    /// disabled otherwise). A pending submission shows at once what is known
    /// of it: the last answer's sentence, its failed step, or, sent with no
    /// answer kept, that it may already have been sent.
    pub(super) fn open_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (empty, busy, pending) = {
            let feedback = self.feedback.read(cx);
            (
                feedback.draft().is_empty(),
                feedback.busy(),
                feedback.pending().cloned(),
            )
        };
        if empty || busy || self.state.read(cx).feedback_commands().is_none() {
            return;
        }
        tracing::debug!("feedback submit screen");
        self.submit.focus_before = window.focused(cx);
        self.screen = FeedbackScreen::Submit;
        let submit = &mut self.submit;
        let id = pending.as_ref().map(|p| p.submission_id);
        submit.error = submit
            .answer
            .as_ref()
            .filter(|(about, _)| Some(*about) == id)
            .map(|(_, text)| text.clone())
            .or_else(|| {
                pending
                    .as_ref()
                    .and_then(|p| p.outcome.as_ref())
                    .and_then(failure_text)
            })
            .or_else(|| {
                pending
                    .as_ref()
                    .filter(|p| p.sent.is_some())
                    .map(|_| MAY_HAVE_BEEN_SENT_TEXT.to_owned())
            });
        submit.picked_repo = None;
        submit.send_as = SendAs::Intent;
        submit.package = None;
        submit.targets = None;
        self.load_targets(window, cx);
        let main_idea = self.feedback.read(cx).draft().main_idea.clone();
        let locked = self.feedback.read(cx).locked();
        self.submit.idea.update(cx, |idea, cx| {
            if idea.value() != main_idea {
                idea.set_value(main_idea, window, cx);
            }
            if !locked {
                idea.focus(window, cx);
            }
        });
        if locked {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Asks the daemon for the targets; what the screen has meanwhile stays
    /// until they come. A failure is said on the error line unless it has a
    /// sentence already.
    fn load_targets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(targets) = self
            .state
            .read(cx)
            .feedback_commands()
            .map(|commands| commands.feedback_targets())
        else {
            return;
        };
        let timer = cx
            .background_executor()
            .timer(self.state.read(cx).command_timeout());
        self.submit.targets_task = Some(cx.spawn_in(window, async move |this, cx| {
            let answer = answered(targets, timer).await;
            let _ = this.update(cx, |this, cx| {
                this.submit.targets_task = None;
                match answer {
                    Some(Ok(targets)) => this.submit.targets = Some(targets),
                    Some(Err(e)) => {
                        tracing::warn!("the feedback targets did not load: {e}");
                        this.submit.error.get_or_insert(submit_error_text(&e));
                    }
                    None => {
                        tracing::warn!("the daemon did not answer the feedback targets in time");
                        this.submit
                            .error
                            .get_or_insert(DAEMON_SILENT_TEXT.to_owned());
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Back to the draft, unless a command is under way; the focus returns
    /// to where it was when the screen opened (onto the pane when that is
    /// gone).
    pub(super) fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen != FeedbackScreen::Submit || self.submit.busy.is_some() {
            return;
        }
        self.close_submit(window, cx);
    }

    fn close_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        tracing::debug!("feedback draft screen");
        self.screen = FeedbackScreen::Draft;
        self.submit.error = None;
        self.submit.targets_task = None;
        let before = self.submit.focus_before.take();
        // Only a focus still in the screen goes back: an answer that comes
        // later never takes the focus from where the user is now.
        if self.focus.contains_focused(window, cx) {
            let restore = before
                .filter(|focus| *focus == self.submit_button_focus || focus.is_rendered(window));
            restore
                .unwrap_or_else(|| self.focus.clone())
                .focus(window, cx);
        }
        cx.notify();
    }

    /// What the pending submission was sent for, while something may exist
    /// under its id.
    fn sent_to(&self, cx: &App) -> Option<SentTo> {
        self.feedback.read(cx).pending().and_then(|p| p.sent)
    }

    /// The repository Create sends to: the one sent to, the one picked
    /// while it exists, else the last used, else the meta repository.
    fn chosen_repo(&self, cx: &App) -> Option<RepoId> {
        if let Some(sent) = self.sent_to(cx) {
            return Some(sent.repo_id);
        }
        let state = self.state.read(cx);
        let model = state.model();
        let repos = model.repos();
        if let Some(picked) = self
            .submit
            .picked_repo
            .filter(|id| repos.iter().any(|repo| repo.id == *id))
        {
            return Some(picked);
        }
        let stored = model
            .feedback_settings()
            .and_then(|settings| settings.meta_repo_id)
            .or_else(|| self.submit.targets.as_ref()?.meta_repo_id);
        starting_repo(
            repos,
            state.last_repo_id(),
            effective_meta_repo(stored, repos),
        )
    }

    fn target(&self, repo: Option<RepoId>) -> Option<&FeedbackTarget> {
        let repo = repo?;
        self.submit
            .targets
            .as_ref()?
            .repos
            .iter()
            .find(|target| target.repo_id == repo)
    }

    /// Why "A GitHub issue" cannot be chosen for `repo`; `None` while the
    /// targets load (it is disabled then, with no reason).
    fn issue_reason(&self, repo: Option<RepoId>) -> Option<Cow<'static, str>> {
        let targets = self.submit.targets.as_ref()?;
        issue_reason(self.target(repo), &targets.gh)
    }

    /// What Create sends as: the one sent as; else the choice, an intent
    /// whenever the issue is unavailable.
    fn chosen_send_as(&self, repo: Option<RepoId>, cx: &App) -> SendAs {
        if let Some(sent) = self.sent_to(cx) {
            return sent.send_as;
        }
        let available = self.submit.targets.is_some() && self.issue_reason(repo).is_none();
        match self.submit.send_as {
            SendAs::GithubIssue if available => SendAs::GithubIssue,
            _ => SendAs::Intent,
        }
    }

    fn pick_repo(&mut self, repo: RepoId, cx: &mut Context<Self>) {
        tracing::debug!(%repo, "feedback goes to");
        self.submit.picked_repo = Some(repo);
        if self.issue_reason(Some(repo)).is_some() || self.submit.targets.is_none() {
            self.submit.send_as = SendAs::Intent;
        }
        cx.notify();
    }

    fn choose_send_as(&mut self, send_as: SendAs, cx: &mut Context<Self>) {
        if self.submit.busy.is_none() && !self.feedback.read(cx).locked() {
            self.submit.send_as = send_as;
            cx.notify();
        }
    }

    fn choose_kind(&mut self, kind: FeedbackKind, cx: &mut Context<Self>) {
        if self.submit.busy.is_none() {
            self.feedback
                .update(cx, |feedback, cx| feedback.set_kind(kind, cx));
        }
    }

    /// Create: asks first for a public repository, then sends.
    fn press_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submit.busy.is_some() || self.feedback.read(cx).locked() {
            return;
        }
        let repo = self.chosen_repo(cx);
        let send_as = self.chosen_send_as(repo, cx);
        let reason = self.issue_reason(repo);
        let idea = self.chosen_idea(send_as, cx);
        let main_idea = self.feedback.read(cx).draft().main_idea.clone();
        if !can_confirm(&main_idea, repo, send_as, reason.as_deref(), idea) {
            return;
        }
        let Some(repo) = repo else {
            return;
        };
        self.ask_then_send(
            SentTo {
                repo_id: repo,
                send_as,
                idea,
            },
            Attempt::Submit,
            window,
            cx,
        );
    }

    /// Retry: the retry route first (it runs what did not succeed of a
    /// recorded submission); the request goes again only when the daemon
    /// has no record of one whose answer never came.
    fn press_retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submit.busy.is_some() {
            return;
        }
        let Some(to) = self.sent_to(cx) else {
            return;
        };
        self.ask_then_send(to, Attempt::Retry, window, cx);
    }

    /// Start over; while the daemon may still be making the submission (it
    /// took the request and no answer came), after asking.
    fn press_start_over(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submit.busy.is_some() {
            return;
        }
        let awaiting = self.feedback.read(cx).pending().is_some_and(|p| p.awaiting);
        if !awaiting {
            self.start_over(window, cx);
            return;
        }
        let pane: WeakEntity<Self> = cx.entity().downgrade();
        let ask = Confirmation {
            title_id: START_OVER_TITLE_ID,
            title: START_OVER_TITLE,
            description: START_OVER_RISK_TEXT.to_owned(),
            cancel_id: START_OVER_CANCEL_ID,
            cancel: START_OVER_CANCEL,
            confirm_id: START_OVER_CONFIRM_ID,
            confirm: START_OVER_LABEL,
        };
        confirm(
            ask,
            move |window, cx| {
                let _ = pane.update(cx, |pane, cx| pane.start_over(window, cx));
            },
            window,
            cx,
        );
    }

    /// Start over: the pending submission is dropped (what it committed
    /// stays) and the form can change again, on the same choices.
    fn start_over(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submit.busy.is_some() {
            return;
        }
        tracing::debug!("feedback submission started over");
        self.forget_keeping_target(cx);
        self.submit.error = None;
        self.submit
            .idea
            .update(cx, |idea, cx| idea.focus(window, cx));
        cx.notify();
    }

    /// Drops the pending submission; the form keeps the target it was sent
    /// for (the screen would otherwise start on another after a restart).
    fn forget_keeping_target(&mut self, cx: &mut Context<Self>) {
        if let Some(sent) = self.sent_to(cx) {
            self.submit.picked_repo = Some(sent.repo_id);
            self.submit.send_as = sent.send_as;
            self.submit.package = sent.idea;
        }
        self.submit.answer = None;
        self.feedback
            .update(cx, |feedback, cx| feedback.forget_submission(cx));
    }

    /// Sends now, or, when the screenshots will reach GitHub in a
    /// repository that is or may be public ([`public_warning`]: an issue, or
    /// a plain intent whose agent opens a pull request there), after the
    /// user confirms (every time): only then does the request carry
    /// `public_acknowledged`.
    fn ask_then_send(
        &mut self,
        to: SentTo,
        attempt: Attempt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.target(Some(to.repo_id));
        let Some(warning) = public_warning(to.send_as, to.idea, target) else {
            self.send(to, attempt, false, window, cx);
            return;
        };
        self.ask_public(to, attempt, warning, window, cx);
    }

    /// Asks whether the screenshots may go to a repository that is or may
    /// be public (`warning` says which); confirmed, sends acknowledged.
    fn ask_public(
        &mut self,
        to: SentTo,
        attempt: Attempt,
        warning: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane: WeakEntity<Self> = cx.entity().downgrade();
        let ask = Confirmation {
            title_id: PUBLIC_CONFIRM_TITLE_ID,
            title: PUBLIC_CONFIRM_TITLE,
            description: warning.to_owned(),
            cancel_id: PUBLIC_CANCEL_ID,
            cancel: PUBLIC_CONFIRM_CANCEL,
            confirm_id: PUBLIC_CONFIRM_ID,
            confirm: match attempt {
                Attempt::Submit => confirm_label(to.send_as),
                Attempt::Retry | Attempt::Reupload => RETRY_LABEL,
            },
        };
        confirm(
            ask,
            move |window, cx| {
                let _ = pane.update(cx, |pane, cx| pane.send(to, attempt, true, window, cx));
            },
            window,
            cx,
        );
    }

    /// Sends the draft (or retries the pending submission) to `to`: one at
    /// a time, with every control waiting, for at most [`SUBMIT_TIMEOUT`];
    /// never while a capture or other work on the draft runs. A draft the
    /// daemon would refuse is not sent: the error line says why. The target
    /// is on disk before the request leaves ([`Feedback::mark_sent`]).
    /// `acknowledged`: the user confirmed that a public repository shows the
    /// screenshots to anyone (the retry route carries nothing: the daemon
    /// does not ask again for a recorded submission).
    fn send(
        &mut self,
        to: SentTo,
        attempt: Attempt,
        acknowledged: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.submit.busy.is_some() {
            tracing::debug!("a feedback submission is already under way");
            self.submit.error = Some(SUBMIT_BUSY_TEXT.to_owned());
            cx.notify();
            return;
        }
        let (capturing, working, before) = {
            let feedback = self.feedback.read(cx);
            (
                feedback.capturing(),
                feedback.busy(),
                feedback.pending().map(|p| (p.sent, p.awaiting)),
            )
        };
        if capturing || working {
            tracing::debug!("the draft is still being changed; nothing is sent");
            self.submit.error = Some(DRAFT_BUSY_TEXT.to_owned());
            cx.notify();
            return;
        }
        let before = before.unwrap_or((None, false));
        if self.state.read(cx).feedback_commands().is_none() {
            self.submit.error = Some(NOT_CONNECTED_TEXT.to_owned());
            cx.notify();
            return;
        }
        enum Sending {
            Submit(FeedbackRequest, Vec<FeedbackFile>),
            Retry(SubmissionId),
        }
        let sending = match attempt {
            Attempt::Retry => {
                let Some(id) = self.feedback.read(cx).pending().map(|p| p.submission_id) else {
                    return;
                };
                tracing::info!(submission_id = %id, "retrying a feedback submission");
                Sending::Retry(id)
            }
            Attempt::Submit | Attempt::Reupload => {
                let (upload, main_idea, kind) = {
                    let feedback = self.feedback.read(cx);
                    (
                        feedback.upload(),
                        feedback.draft().main_idea.clone(),
                        feedback.kind(),
                    )
                };
                let Some((attachments, files)) = upload else {
                    tracing::debug!("the draft is not ready to send");
                    self.submit.error = Some(DRAFT_NOT_READY_TEXT.to_owned());
                    cx.notify();
                    return;
                };
                let files = sizes_on_disk(files);
                if let Some(problem) = upload_problem(&main_idea, &attachments, &files) {
                    tracing::info!("the feedback draft is over a limit; nothing is sent");
                    self.submit.error = Some(problem);
                    cx.notify();
                    return;
                }
                let submission_id = self
                    .feedback
                    .update(cx, |feedback, cx| feedback.begin_submission(cx));
                let request = FeedbackRequest {
                    submission_id,
                    main_idea,
                    kind,
                    repo_id: to.repo_id,
                    send_as: to.send_as,
                    idea: to.idea,
                    app_version: smithagents_core::VERSION.to_owned(),
                    platform: std::env::consts::OS.to_owned(),
                    attachments,
                    public_acknowledged: acknowledged,
                };
                tracing::info!(%submission_id, send_as = ?to.send_as, "sending feedback");
                Sending::Submit(request, files.into_iter().map(|(file, _)| file).collect())
            }
        };
        // On disk before the request leaves: a quit or a lost connection
        // from here on leaves the draft locked.
        self.feedback
            .update(cx, |feedback, cx| feedback.mark_sent(to, cx));
        let Some(command) = self
            .state
            .read(cx)
            .feedback_commands()
            .map(|commands| match sending {
                Sending::Submit(request, files) => commands.submit_feedback(request, files),
                Sending::Retry(id) => commands.retry_feedback(id),
            })
        else {
            return;
        };
        self.submit.busy = Some(attempt);
        self.submit.error = None;
        self.submit.answer = None;
        self.feedback
            .update(cx, |feedback, cx| feedback.set_submitting(true, cx));
        let timer = cx.background_executor().timer(SUBMIT_TIMEOUT);
        self.submit.command = Some(cx.spawn_in(window, async move |this, cx| {
            let answer = answered(command, timer).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish(to, attempt, acknowledged, before, answer, window, cx)
            });
        }));
        cx.notify();
    }

    /// `answer` is `None` when the daemon did not answer in time; `before`
    /// is where the submission stood before this attempt. Whatever may exist
    /// under the id keeps the draft locked (see [`send_failure`]).
    #[allow(clippy::too_many_arguments)]
    fn finish(
        &mut self,
        to: SentTo,
        attempt: Attempt,
        acknowledged: bool,
        before: (Option<SentTo>, bool),
        answer: Option<Result<FeedbackOutcome, LinkError>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.submit.busy = None;
        self.submit.command = None;
        self.feedback
            .update(cx, |feedback, cx| feedback.set_submitting(false, cx));
        let pending = self.feedback.read(cx).pending().cloned();
        let error = match answer {
            None => {
                tracing::warn!(
                    ?attempt,
                    "the daemon did not answer a feedback submission in time"
                );
                // It took the request: it may still make it, so the marker
                // stays, awaiting.
                Some(DAEMON_SILENT_TEXT.to_owned())
            }
            Some(Ok(outcome)) => {
                let ours = pending
                    .as_ref()
                    .is_some_and(|p| p.submission_id == outcome.submission_id);
                match result_text(&outcome) {
                    // Only an answer about this submission that names what
                    // was made deletes the draft.
                    Some(result) if ours && names_what_was_made(&outcome) => {
                        self.succeeded(outcome, result, window, cx);
                        None
                    }
                    // Only a failed step of this submission is described.
                    None if ours => {
                        tracing::warn!(
                            submission_id = %outcome.submission_id,
                            step = ?outcome.failed.as_ref().map(|f| f.step),
                            "a feedback submission is committed, but a step failed"
                        );
                        let text = failure_text(&outcome);
                        self.feedback
                            .update(cx, |feedback, cx| feedback.record_outcome(outcome, cx));
                        text
                    }
                    _ => {
                        tracing::warn!(
                            submission_id = %outcome.submission_id,
                            "a feedback answer that does not complete this submission"
                        );
                        self.feedback
                            .update(cx, |feedback, cx| feedback.mark_answered(cx));
                        Some(MAY_HAVE_BEEN_SENT_TEXT.to_owned())
                    }
                }
            }
            Some(Err(e)) => {
                tracing::warn!(?attempt, "a feedback submission failed: {e}");
                match send_failure(&e) {
                    SendFailure::NotSent => {
                        // Nothing left: the submission is where it was.
                        self.feedback
                            .update(cx, |feedback, cx| feedback.set_sent(before.0, before.1, cx));
                        Some(submit_error_text(&e))
                    }
                    SendFailure::Refused => {
                        // The daemon made nothing for this request. A
                        // recorded submission stays as it was, and so does
                        // one re-uploaded: an earlier request under its id
                        // went unanswered and may still make its commit.
                        let (sent, awaiting) = match attempt {
                            Attempt::Retry | Attempt::Reupload => before,
                            Attempt::Submit => (None, false),
                        };
                        self.feedback
                            .update(cx, |feedback, cx| feedback.set_sent(sent, awaiting, cx));
                        self.after_refusal(&e, window, cx);
                        // The app took the repository for private, the
                        // daemon does not: asked now, and sent again under
                        // the same id once confirmed. Only while the pane
                        // is on screen: otherwise the sentence stays, and
                        // the next Create meets the same question.
                        let showing = self.state.read(cx).right_entry() == RailEntry::Feedback
                            && self.focus.is_rendered(window);
                        if !acknowledged
                            && attempt != Attempt::Retry
                            && asks_to_acknowledge(&e)
                            && showing
                        {
                            tracing::info!("the daemon asks to confirm a public repository");
                            let warning = maybe_public_warning(to.send_as);
                            self.ask_public(to, attempt, warning, window, cx);
                        }
                        Some(submit_error_text(&e))
                    }
                    SendFailure::NotRecorded
                        if attempt == Attempt::Retry
                            && pending.as_ref().is_some_and(|p| p.outcome.is_none()) =>
                    {
                        tracing::info!(
                            "no record of a submission whose answer never came; sending it again"
                        );
                        self.send(to, Attempt::Reupload, acknowledged, window, cx);
                        cx.notify();
                        return;
                    }
                    SendFailure::NotRecorded => {
                        tracing::info!("the daemon has no such feedback submission; starting over");
                        // What the draft's record names is still committed.
                        let text =
                            not_recorded_text(pending.as_ref().and_then(|p| p.outcome.as_ref()));
                        self.forget_keeping_target(cx);
                        Some(text)
                    }
                    SendFailure::RepoGone => {
                        self.forget_keeping_target(cx);
                        Some(submit_error_text(&e))
                    }
                    SendFailure::MayExist => {
                        if answered_by_daemon(&e) {
                            self.feedback
                                .update(cx, |feedback, cx| feedback.mark_answered(cx));
                        }
                        Some(submit_error_text(&e))
                    }
                }
            }
        };
        if let Some(text) = error {
            if let Some(id) = self.feedback.read(cx).pending().map(|p| p.submission_id) {
                self.submit.answer = Some((id, text.clone()));
            }
            self.submit.error = Some(text);
        }
        cx.notify();
    }

    /// The daemon made it all: the repository the daemon names becomes the
    /// last used, the draft goes, the draft screen says what was created,
    /// and, while the Feedback pane is still the one showing, a created
    /// intent opens in the Intents pane.
    fn succeeded(
        &mut self,
        outcome: FeedbackOutcome,
        result: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tracing::info!(submission_id = %outcome.submission_id, "feedback submitted");
        let showing = self.state.read(cx).right_entry() == RailEntry::Feedback
            && self.focus.is_rendered(window);
        self.submit.answer = None;
        self.state
            .update(cx, |state, _| state.set_last_repo_id(outcome.repo_id));
        self.feedback
            .update(cx, |feedback, cx| feedback.finish_submission(result, cx));
        self.submit.picked_repo = None;
        self.submit.send_as = SendAs::Intent;
        self.submit.package = None;
        self.close_submit(window, cx);
        if let Some(intent) = outcome.intent.filter(|_| showing) {
            self.state
                .update(cx, |state, cx| state.open_intent(intent.id, cx));
        }
    }

    // ---- Drawing --------------------------------------------------------

    pub(super) fn submit_screen(&self, paint: &Paint, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.submit.busy;
        let (locked, pending, kind, draft) = {
            let feedback = self.feedback.read(cx);
            (
                feedback.pending().is_some_and(|p| p.sent.is_some()),
                feedback.pending().cloned(),
                feedback.kind(),
                feedback.draft().clone(),
            )
        };
        // While the daemon has the submission, or a command is under way.
        let frozen = locked || busy.is_some();
        let repo = self.chosen_repo(cx);
        let send_as = self.chosen_send_as(repo, cx);
        let reason = self.issue_reason(repo);
        let idea = self.chosen_idea(send_as, cx);
        let issue_disabled = frozen || self.submit.targets.is_none() || reason.is_some();
        let warning = public_warning(send_as, idea, self.target(repo));
        let cards = cards(self.feedback.read(cx));
        v_flex()
            .id(SCREEN_ID)
            .test_support()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .w_full()
                    .px_3()
                    .pb_3()
                    .gap_3()
                    .child(
                        h_flex().child(
                            Button::new(SUBMIT_BACK_ID)
                                .label(BACK_LABEL)
                                .ghost()
                                .small()
                                .disabled(busy.is_some())
                                .on_click(cx.listener(|this, _, window, cx| this.back(window, cx))),
                        ),
                    )
                    .child(
                        v_flex().w_full().gap_1p5().children(
                            cards
                                .into_iter()
                                .map(|card| self.card(card, false, paint, cx)),
                        ),
                    )
                    .child(self.idea_field(frozen, paint))
                    .child(self.kind_row(kind, frozen, paint, cx))
                    .child(self.repo_row(repo, frozen, paint, cx))
                    .child(self.send_as_row(
                        send_as,
                        issue_disabled,
                        reason.as_deref(),
                        frozen,
                        paint,
                        cx,
                    ))
                    .child(
                        Label::new(summary(send_as, idea, &draft))
                            .id(SUBMIT_SUMMARY_ID)
                            .text_xs()
                            .text_color(paint.muted_foreground),
                    )
                    .when_some(warning, |this, warning| {
                        this.child(
                            Label::new(warning)
                                .id(PUBLIC_WARNING_ID)
                                .text_xs()
                                .text_color(paint.warning),
                        )
                    })
                    .children(self.error_line(paint))
                    .child(self.actions(
                        locked,
                        pending.as_ref(),
                        repo,
                        send_as,
                        reason.as_deref(),
                        cx,
                    )),
            )
    }

    /// "Main idea", its text box and the hint.
    fn idea_field(&self, frozen: bool, paint: &Paint) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_1()
            .child(section_label(MAIN_IDEA_LABEL, paint).id(SUBMIT_IDEA_LABEL_ID))
            .child(
                div().id(SUBMIT_IDEA_ID).test_support().w_full().child(
                    Textarea::new(&self.submit.idea)
                        .aria_label(MAIN_IDEA_LABEL)
                        .disabled(frozen)
                        .small(),
                ),
            )
            .child(
                Label::new(MAIN_IDEA_HINT)
                    .id(SUBMIT_IDEA_HINT_ID)
                    .text_xs()
                    .text_color(paint.muted_foreground),
            )
    }

    fn kind_row(
        &self,
        chosen: FeedbackKind,
        frozen: bool,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_1()
            .child(section_label(KIND_LABEL, paint))
            .child(h_flex().flex_wrap().gap_1().children(KINDS.map(|kind| {
                Button::new(submit_kind_id(kind))
                    .label(kind_label(kind.intent_kind()))
                    .outline()
                    .small()
                    .selected(kind == chosen)
                    .toggled(kind == chosen)
                    .disabled(frozen)
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_kind(kind, cx)))
            })))
    }

    /// "Repository": a menu of the repositories (its button names the
    /// chosen one), or the way to Settings when there is none.
    fn repo_row(
        &self,
        chosen: Option<RepoId>,
        frozen: bool,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let line = {
            let repos = self.state.read(cx).model().repos();
            if repos.is_empty() {
                RepoLine::Missing
            } else {
                let label = chosen
                    .and_then(|id| repos.iter().find(|repo| repo.id == id))
                    .map_or_else(
                        || CHOOSE_REPO_TEXT.to_owned(),
                        |repo| repo_title(&repo.path),
                    );
                // No entry is checked until one is chosen.
                let choices = repos
                    .iter()
                    .map(|repo| RepoChoiceVm {
                        id: repo.id,
                        title: repo_title(&repo.path),
                        path: repo.path.display().to_string(),
                        preselected: Some(repo.id) == chosen,
                    })
                    .collect();
                RepoLine::Menu { label, choices }
            }
        };
        let button: AnyElement = match line {
            RepoLine::Missing => Button::new(SUBMIT_REPO_ID)
                .label(NEED_REPO_TEXT)
                .outline()
                .small()
                .disabled(frozen)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.update(cx, |state, cx| {
                        state.request_settings_focus(SettingsSection::Repositories, cx)
                    })
                }))
                .into_any_element(),
            RepoLine::Menu { label, choices } => {
                let pane = cx.entity().downgrade();
                Button::new(SUBMIT_REPO_ID)
                    .label(label)
                    .outline()
                    .small()
                    .dropdown_caret(true)
                    .disabled(frozen)
                    .dropdown_menu(move |menu, window, cx| {
                        repo_menu(menu, &choices, &pane, window, cx)
                    })
                    .into_any_element()
            }
        };
        v_flex()
            .w_full()
            .gap_1()
            .child(section_label(REPO_LABEL, paint))
            .child(h_flex().min_w_0().child(button))
    }

    /// "Send it as": two radios, the idea package's boxes under the intent's
    /// while it is chosen, and why the issue cannot be chosen.
    fn send_as_row(
        &self,
        send_as: SendAs,
        issue_disabled: bool,
        reason: Option<&str>,
        frozen: bool,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_1p5()
            .child(section_label(SEND_AS_LABEL, paint))
            .child(
                Radio::new(SEND_INTENT_ID)
                    .label(SEND_AS_INTENT)
                    .small()
                    .checked(send_as == SendAs::Intent)
                    .disabled(frozen)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        if *checked {
                            this.choose_send_as(SendAs::Intent, cx)
                        }
                    })),
            )
            .when(send_as == SendAs::Intent, |this| {
                this.child(self.idea_boxes(self.chosen_idea(send_as, cx), frozen, paint, cx))
            })
            .child(
                Radio::new(SEND_ISSUE_ID)
                    .label(SEND_AS_ISSUE)
                    .small()
                    .checked(send_as == SendAs::GithubIssue)
                    .disabled(issue_disabled)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        if *checked {
                            this.choose_send_as(SendAs::GithubIssue, cx)
                        }
                    })),
            )
            .when_some(reason.filter(|_| !frozen), |this, reason| {
                this.child(
                    Label::new(reason.to_owned())
                        .id(SEND_ISSUE_REASON_ID)
                        .text_xs()
                        .text_color(paint.muted_foreground),
                )
            })
    }

    /// The screen's error line, a status that announces itself.
    fn error_line(&self, paint: &Paint) -> Option<AnyElement> {
        let text = self.submit.error.clone()?;
        Some(
            div()
                .id(SUBMIT_ERROR_ID)
                .role(Role::Status)
                .test_support()
                .aria_label(text.clone())
                .w_full()
                .text_xs()
                .text_color(paint.destructive)
                .child(text)
                .into_any_element(),
        )
    }

    /// Create, or (while something may exist under the submission's id)
    /// Retry and Start over.
    fn actions(
        &self,
        locked: bool,
        pending: Option<&Pending>,
        repo: Option<RepoId>,
        send_as: SendAs,
        reason: Option<&str>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let busy = self.submit.busy;
        // Nothing is sent while a capture or other work on the draft runs.
        let working = {
            let feedback = self.feedback.read(cx);
            feedback.capturing() || feedback.busy()
        };
        let row = h_flex().w_full().justify_end().gap_1();
        // A first send shows its spinner on Create, though the draft is
        // locked from the moment its request leaves.
        if locked && pending.is_some() && busy != Some(Attempt::Submit) {
            return row
                .child(
                    Button::new(SUBMIT_START_OVER_ID)
                        .label(START_OVER_LABEL)
                        .ghost()
                        .small()
                        .tooltip(START_OVER_TOOLTIP)
                        .disabled(busy.is_some())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.press_start_over(window, cx)),
                        ),
                )
                .child(
                    Button::new(SUBMIT_RETRY_ID)
                        .label(RETRY_LABEL)
                        .primary()
                        .small()
                        .disabled(busy.is_some() || working)
                        .loading(busy.is_some())
                        .on_click(cx.listener(|this, _, window, cx| this.press_retry(window, cx))),
                );
        }
        let (main_idea, ready) = {
            let feedback = self.feedback.read(cx);
            (
                feedback.draft().main_idea.clone(),
                feedback.upload().is_some(),
            )
        };
        let enabled = busy.is_none()
            && !working
            && ready
            && can_confirm(
                &main_idea,
                repo,
                send_as,
                reason,
                self.chosen_idea(send_as, cx),
            );
        row.child(
            Button::new(SUBMIT_CONFIRM_ID)
                .label(confirm_label(send_as))
                .primary()
                .small()
                .disabled(!enabled)
                .loading(busy == Some(Attempt::Submit))
                .on_click(cx.listener(|this, _, window, cx| this.press_confirm(window, cx))),
        )
    }
}

/// Whether `outcome` names what was made (an intent, or an issue address
/// the app shows): with being about the pending submission, what completes
/// it. Any other answer is not taken for a success.
fn names_what_was_made(outcome: &FeedbackOutcome) -> bool {
    outcome.intent.is_some() || outcome.issue_url.as_deref().is_some_and(shows_issue_url)
}

/// A section's name over its control.
fn section_label(text: &'static str, paint: &Paint) -> Label {
    Label::new(text)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(paint.muted_foreground)
}

/// The repository menu: one entry per repository, its folder's name over its
/// full path, the chosen one checked.
fn repo_menu(
    mut menu: PopupMenu,
    choices: &[RepoChoiceVm],
    pane: &WeakEntity<FeedbackPane>,
    window: &Window,
    cx: &App,
) -> PopupMenu {
    let muted = cx.theme().muted_foreground;
    // The menu takes its width in pixels; it follows the text size.
    let max_width = rems(REPO_MENU_MAX_WIDTH).to_pixels(window.rem_size());
    menu = menu.label(REPO_MENU_LABEL).max_w(max_width);
    for choice in choices {
        let (title, path, repo) = (choice.title.clone(), choice.path.clone(), choice.id);
        let pane = pane.clone();
        menu = menu.item(
            // As the intent tool's menu: the entry carries its own named node.
            PopupMenuItem::element(move |_, _| {
                v_flex()
                    .id(submit_repo_item_id(repo))
                    .role(Role::Group)
                    .test_support()
                    .aria_label(format!("{title}, {path}"))
                    .min_w_0()
                    .py_0p5()
                    .child(div().text_sm().child(title.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(path.clone()),
                    )
            })
            .checked(choice.preselected)
            .on_click(move |_, _, cx| {
                let _ = pane.update(cx, |pane, cx| pane.pick_repo(repo, cx));
            }),
        );
    }
    menu
}

//! The Source repositories section: the daemon's repositories, and an "Add
//! repository" form: which of the three the folder is ([`ADD_CHOICES`]: a
//! folder with no source control, selected first, an existing git
//! repository, or a new one), above a field for its local path (typed, or
//! chosen with the native folder picker; a new repository's folder is missing
//! or empty).
//!
//! The add flow: [`validate_repo_path_input`] first (a refusal shows under
//! the field and sends nothing; a URL says cloning is not supported yet),
//! then `add_repo` with the choice through the commands seam. While it runs,
//! the choice, the field and its buttons are disabled, so a second Enter or
//! click sends nothing; it is given up after the shell's command timeout, or
//! when the connection drops, with "The daemon did not answer". The daemon
//! may still finish it: when a repository matching the path appears in the
//! model afterwards, that line and the field clear as on success. On success
//! the field clears; the repository itself arrives through the model
//! (`Action::RepoAdded`, which also sets the "Repository added" status). On
//! failure our sentence for the daemon's answer shows under the field and the
//! typed text stays. The git choices are always offered: where git cannot
//! run, the daemon's `git_not_installed` says so there.
//!
//! Each row says its source control ("Source control: None", or "Source
//! control: Git, default branch main") and has the button that turns it on
//! or off. Turning it off for the repository intents and feedback use asks
//! first (Cancel is the default, as Enter); the others switch at once. One
//! switch at a time; nothing is optimistic: the row changes when the daemon's
//! answer reaches the model (`Action::RepoUpdated`). A refusal's sentence
//! shows in the row, and a switch that gets no answer is given up as an add
//! is.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::Input;
use gpui_kit::component::label::Label;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, App, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Role, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, WeakEntity, Window,
};
use smithagents_core::agent::Repo;
use smithagents_core::feedback::FeedbackSettings;
use smithagents_core::ids::RepoId;

use super::appearance::{Choice, OptionVm};
use super::{caption, Paint, SentAdd, SettingsPane};
use crate::link::LinkError;
use crate::view_models::{
    repo_error_text, repo_matches_sent_path, repo_rows, validate_repo_path_input, RepoRowVm,
    ADD_CHOICES, DAEMON_SILENT_TEXT, NOT_CONNECTED_TEXT, NO_REPOS_TEXT,
};
use crate::views::answered;

/// Element id of the list of repositories.
pub const REPO_LIST_ID: &str = "repo-list";
/// Element id of the hint shown when there is no repository.
pub const REPOS_EMPTY_ID: &str = "repos-empty";
/// Element id of the path field.
pub const REPO_PATH_INPUT_ID: &str = "repo-path";
/// Element id of the line under the path field (a status).
pub const REPO_PATH_ERROR_ID: &str = "repo-path-error";
/// Element id of the "Browse…" button (the native folder picker).
pub const BROWSE_REPO_ID: &str = "browse-repo";
/// Element id of the "Add" button.
pub const ADD_REPO_ID: &str = "add-repo";
/// The path field's accessible name.
pub const REPO_PATH_LABEL: &str = "Repository path";
/// Element id of the add form's choice of source control (a radio group
/// named [`ADD_CHOICES_LABEL`]; its radios are `0..`).
pub const ADD_CHOICES_ID: &str = "repo-add-choices";
pub const ADD_CHOICES_LABEL: &str = "Source control";

/// The row button of a repository without source control, and of one with.
const TURN_ON_LABEL: &str = "Turn on source control";
const TURN_OFF_LABEL: &str = "Turn off source control";
/// What turning it off for the repository intents and feedback use asks.
const TURN_OFF_TITLE: &str = "Turn off source control?";
const TURN_OFF_META_QUESTION: &str =
    "Intents and feedback use this repository. With source control off they can no longer use it. Turn it off?";
/// Element ids of that question's title, text and buttons.
pub const TURN_OFF_TITLE_ID: &str = "repo-turn-off-title";
pub const TURN_OFF_DESCRIPTION_ID: &str = "repo-turn-off-description";
pub const TURN_OFF_CANCEL_ID: &str = "repo-turn-off-cancel";
pub const TURN_OFF_CONFIRM_ID: &str = "repo-turn-off-confirm";

/// Element id of the line under the list that says why choosing the meta
/// repository failed (a status).
pub const REPO_META_ERROR_ID: &str = "repo-meta-error";
/// What the tag on the meta repository's row says.
pub const META_TAG: &str = "Intents and feedback";
/// The button that makes a repository the meta repository.
const USE_META_LABEL: &str = "Use for intents and feedback";

pub(super) const PATH_PLACEHOLDER: &str = "/path/to/a/folder";

/// Element id of a repository's row.
pub fn repo_row_id(id: RepoId) -> ElementId {
    ElementId::Name(format!("repo-row-{id}").into())
}

/// Element id of the tag on the meta repository's row (a [`Tag`] reading
/// [`META_TAG`]).
pub fn repo_meta_tag_id(id: RepoId) -> ElementId {
    ElementId::Name(format!("repo-meta-{id}").into())
}

/// Element id of a row's "Use for intents and feedback" button.
pub fn repo_use_meta_id(id: RepoId) -> ElementId {
    ElementId::Name(format!("repo-use-meta-{id}").into())
}

/// Element id of a row's source control line (a label).
pub fn repo_source_control_id(id: RepoId) -> ElementId {
    ElementId::Name(format!("repo-source-control-{id}").into())
}

/// Element id of a row's "Turn on source control" or "Turn off source
/// control" button.
pub fn repo_switch_id(id: RepoId) -> ElementId {
    ElementId::Name(format!("repo-switch-{id}").into())
}

/// Element id of the line in a row that says why its switch failed (a
/// status).
pub fn repo_switch_error_id(id: RepoId) -> ElementId {
    ElementId::Name(format!("repo-switch-error-{id}").into())
}

impl SettingsPane {
    /// Sends the choice of `id` as the meta repository. Nothing is
    /// optimistic: the tag moves when the daemon's answer reaches the model
    /// (`Action::FeedbackSettingsLoaded`). Does nothing while a choice is in
    /// flight.
    fn choose_meta_repo(&mut self, id: RepoId, window: &mut Window, cx: &mut Context<Self>) {
        if self.meta_command.is_some() {
            tracing::debug!("a meta repository change is already in flight");
            return;
        }
        let (command, timeout) = {
            let state = self.state.read(cx);
            (
                state.feedback_commands().map(|commands| {
                    commands.set_feedback_settings(FeedbackSettings {
                        meta_repo_id: Some(id),
                    })
                }),
                state.command_timeout(),
            )
        };
        let Some(command) = command else {
            self.meta_error = Some(NOT_CONNECTED_TEXT.to_owned());
            cx.notify();
            return;
        };
        tracing::debug!(repo = %id, "choosing the meta repository");
        self.meta_error = None;
        let timer = cx.background_executor().timer(timeout);
        let task = cx.spawn_in(window, async move |this, cx| {
            let answer = answered(command, timer).await;
            let _ = this.update(cx, |this, cx| this.finish_meta_repo(answer, cx));
        });
        self.meta_command = Some((id, task));
        cx.notify();
    }

    /// `answer` is `None` when the daemon did not answer in time. On success
    /// the model already holds the stored settings (the link sent them).
    fn finish_meta_repo(
        &mut self,
        answer: Option<Result<FeedbackSettings, LinkError>>,
        cx: &mut Context<Self>,
    ) {
        self.meta_command = None;
        match answer {
            None => {
                tracing::warn!("the daemon did not answer a meta repository change in time");
                self.meta_error = Some(DAEMON_SILENT_TEXT.to_owned());
            }
            Some(Ok(stored)) => tracing::debug!(?stored, "feedback settings stored"),
            Some(Err(e)) => {
                tracing::warn!("cannot set the meta repository: {e}");
                self.meta_error = Some(repo_error_text(&e));
            }
        }
        cx.notify();
    }

    /// Stops waiting for the change in flight (the connection dropped).
    pub(super) fn give_up_meta_repo(&mut self) {
        tracing::warn!("the connection dropped while a meta repository change waited");
        self.meta_command = None;
        self.meta_error = Some(DAEMON_SILENT_TEXT.to_owned());
    }

    /// The add form's choice `ix` of [`ADD_CHOICES`] is selected; nothing
    /// while an add runs.
    pub(super) fn choose_add_choice(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.adding.is_some() {
            return;
        }
        let Some((choice, _)) = ADD_CHOICES.get(ix) else {
            return;
        };
        tracing::debug!(?choice, "source control of the next add chosen");
        self.add_choice = *choice;
        cx.notify();
    }

    /// The row button of `id`: turning its source control off for the meta
    /// repository asks first; anything else is sent at once. Nothing while
    /// a switch is in flight.
    fn ask_to_switch(&mut self, id: RepoId, window: &mut Window, cx: &mut Context<Self>) {
        if self.switching.is_some() {
            tracing::debug!("a source control switch is already in flight");
            return;
        }
        let row = {
            let model = self.state.read(cx).model();
            repo_rows(model.repos(), model.feedback_settings())
                .into_iter()
                .find(|row| row.id == id)
        };
        let Some(row) = row else {
            return;
        };
        if row.has_source_control && row.is_meta {
            confirm_turn_off(cx.entity().downgrade(), id, window, cx);
        } else {
            self.switch_source_control(id, !row.has_source_control, window, cx);
        }
    }

    /// Sends the switch of `id`'s source control. Nothing is optimistic: the
    /// row changes when the daemon's answer reaches the model
    /// (`Action::RepoUpdated`, which the link sends).
    fn switch_source_control(
        &mut self,
        id: RepoId,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.switching.is_some() {
            return;
        }
        let (command, timeout) = {
            let state = self.state.read(cx);
            (
                state
                    .agent_commands()
                    .map(|commands| commands.set_source_control(id, on)),
                state.command_timeout(),
            )
        };
        let Some(command) = command else {
            self.switch_error = Some((id, NOT_CONNECTED_TEXT.to_owned()));
            cx.notify();
            return;
        };
        tracing::debug!(repo = %id, on, "switching a repository's source control");
        self.switch_error = None;
        let timer = cx.background_executor().timer(timeout);
        let task = cx.spawn_in(window, async move |this, cx| {
            let answer = answered(command, timer).await;
            let _ = this.update(cx, |this, cx| this.finish_switch(id, answer, cx));
        });
        self.switching = Some((id, task));
        cx.notify();
    }

    /// `answer` is `None` when the daemon did not answer in time.
    fn finish_switch(
        &mut self,
        id: RepoId,
        answer: Option<Result<Repo, LinkError>>,
        cx: &mut Context<Self>,
    ) {
        self.switching = None;
        match answer {
            None => {
                tracing::warn!(repo = %id, "the daemon did not answer a source control switch in time");
                self.switch_error = Some((id, DAEMON_SILENT_TEXT.to_owned()));
            }
            Some(Ok(repo)) => {
                tracing::debug!(repo = %repo.id, plain = repo.is_plain(), "source control switched");
            }
            Some(Err(e)) => {
                tracing::warn!(repo = %id, "cannot switch the source control: {e}");
                self.switch_error = Some((id, repo_error_text(&e)));
            }
        }
        cx.notify();
    }

    /// Stops waiting for the switch in flight (the connection dropped).
    pub(super) fn give_up_switch(&mut self) {
        tracing::warn!("the connection dropped while a source control switch waited");
        if let Some((id, _)) = self.switching.take() {
            self.switch_error = Some((id, DAEMON_SILENT_TEXT.to_owned()));
        }
    }

    /// Validates the typed path and, if it is acceptable, sends the add.
    pub(super) fn submit_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.adding.is_some() {
            tracing::debug!("an add is already in flight");
            return;
        }
        let raw = self.path_input.read(cx).value();
        let path = match validate_repo_path_input(&raw) {
            Ok(path) => path,
            Err(text) => {
                tracing::debug!(%text, "repository path refused");
                self.path_error = Some(text.to_owned());
                cx.notify();
                return;
            }
        };
        let choice = self.add_choice;
        let (add, timeout) = {
            let state = self.state.read(cx);
            (
                state
                    .agent_commands()
                    .map(|commands| commands.add_repo(&path, choice)),
                state.command_timeout(),
            )
        };
        let Some(add) = add else {
            self.path_error = Some(NOT_CONNECTED_TEXT.to_owned());
            cx.notify();
            return;
        };
        tracing::debug!(path = %path.display(), ?choice, "adding a repository");
        self.path_error = None;
        self.given_up = None;
        self.sent = Some(SentAdd {
            known: self
                .state
                .read(cx)
                .model()
                .repos()
                .iter()
                .map(|repo| repo.id)
                .collect(),
            path,
            canonical: None,
        });
        let timer = cx.background_executor().timer(timeout);
        self.adding = Some(cx.spawn_in(window, async move |this, cx| {
            let answer = answered(add, timer).await;
            let _ = this.update_in(cx, |this, window, cx| this.finish_add(answer, window, cx));
        }));
        cx.notify();
    }

    /// `answer` is `None` when the daemon did not answer in time.
    fn finish_add(
        &mut self,
        answer: Option<Result<Repo, LinkError>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match answer {
            None => {
                tracing::warn!("the daemon did not answer an add in time");
                self.give_up_add();
            }
            Some(Ok(repo)) => {
                self.adding = None;
                self.sent = None;
                tracing::debug!(repo = %repo.id, path = %repo.path.display(), "repository added");
                self.path_error = None;
                self.path_input
                    .update(cx, |input, cx| input.set_value("", window, cx));
            }
            Some(Err(e)) => {
                self.adding = None;
                self.sent = None;
                tracing::warn!("cannot add the repository: {e}");
                self.path_error = Some(repo_error_text(&e));
            }
        }
        cx.notify();
    }

    /// Stops waiting for the add in flight (timeout, connection drop) and
    /// says so; its repository may still appear later
    /// ([`SettingsPane::notice_late_add`]).
    pub(super) fn give_up_add(&mut self) {
        self.adding = None;
        self.given_up = self.sent.take().map(|mut sent| {
            sent.canonical = std::fs::canonicalize(&sent.path).ok();
            sent
        });
        self.path_error = Some(DAEMON_SILENT_TEXT.to_owned());
    }

    /// A given-up add whose repository has now appeared (the daemon finished
    /// it after all): the "did not answer" line goes, and the field clears if
    /// it still holds that path.
    pub(super) fn notice_late_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(given_up) = &self.given_up else {
            return;
        };
        let arrived = self.state.read(cx).model().repos().iter().any(|repo| {
            !given_up.known.contains(&repo.id)
                && repo_matches_sent_path(&given_up.path, given_up.canonical.as_deref(), &repo.path)
        });
        if !arrived {
            return;
        }
        tracing::debug!(path = %given_up.path.display(), "a given-up add arrived after all");
        if self.path_error.as_deref() == Some(DAEMON_SILENT_TEXT) {
            self.path_error = None;
        }
        let typed = self.path_input.read(cx).value();
        if validate_repo_path_input(&typed).ok().as_ref() == Some(&given_up.path) {
            self.path_input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.given_up = None;
        cx.notify();
    }

    /// Opens the native folder picker; the folder chosen fills the field
    /// (it is not added until Add or Enter).
    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.adding.is_some() || self.browsing.is_some() {
            return;
        }
        tracing::debug!("opening the folder picker");
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        self.browsing = Some(cx.spawn_in(window, async move |this, cx| {
            let picked = picked.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.browsing = None;
                match picked {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            this.fill_path(path, window, cx);
                        }
                    }
                    // Cancelled, or the picker went away.
                    Ok(Ok(None)) | Err(_) => {}
                    Ok(Err(e)) => {
                        tracing::warn!("the folder picker failed: {e:#}");
                        this.path_error = Some(format!("Could not open the folder picker: {e}"));
                    }
                }
                cx.notify();
            });
        }));
    }

    fn fill_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        tracing::debug!(path = %path.display(), "folder chosen");
        let text = path.display().to_string();
        self.path_error = None;
        self.path_input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            input.focus(window, cx);
        });
    }

    pub(super) fn repositories_content(
        &self,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let rows = {
            let model = self.state.read(cx).model();
            repo_rows(model.repos(), model.feedback_settings())
        };
        let adding = self.adding.is_some();
        let choosing = self.meta_command.as_ref().map(|(id, _)| *id);
        let switches = Switches {
            busy: self.switching.as_ref().map(|(id, _)| *id),
            error: self.switch_error.clone(),
        };
        let choices = ADD_CHOICES
            .iter()
            .map(|(choice, label)| OptionVm {
                label: (*label).to_owned(),
                selected: *choice == self.add_choice,
            })
            .collect();
        v_flex()
            .w_full()
            .gap_3()
            .child(if rows.is_empty() {
                // A `Label` is in the accessibility tree; plain text is not.
                Label::new(NO_REPOS_TEXT)
                    .id(REPOS_EMPTY_ID)
                    .text_xs()
                    .text_color(paint.muted_foreground)
                    .into_any_element()
            } else {
                v_flex()
                    .id(REPO_LIST_ID)
                    .role(Role::List)
                    .test_support()
                    .aria_label("Source repositories")
                    .w_full()
                    .gap_2()
                    .children(
                        rows.into_iter()
                            .map(|row| repo_row(row, choosing, &switches, paint, cx)),
                    )
                    .into_any_element()
            })
            .when_some(self.meta_error.clone(), |this, text| {
                this.child(
                    div()
                        .id(REPO_META_ERROR_ID)
                        .role(Role::Status)
                        .test_support()
                        .aria_label(text.clone())
                        .text_xs()
                        .text_color(paint.destructive)
                        .child(text),
                )
            })
            .child(
                v_flex()
                    .w_full()
                    .gap_1p5()
                    .child(caption("Add repository", paint))
                    .child(self.radio_group(Choice::AddSourceControl, choices, paint, cx))
                    .child(
                        Input::new(&self.path_input)
                            .id(REPO_PATH_INPUT_ID)
                            .aria_label(REPO_PATH_LABEL)
                            .small()
                            .disabled(adding),
                    )
                    .when_some(self.path_error.clone(), |this, text| {
                        // The input has no way to point at its error, so the
                        // line is a status that announces itself.
                        this.child(
                            div()
                                .id(REPO_PATH_ERROR_ID)
                                .role(Role::Status)
                                .test_support()
                                .aria_label(text.clone())
                                .text_xs()
                                .text_color(paint.destructive)
                                .child(text),
                        )
                    })
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .justify_end()
                            .child(
                                Button::new(BROWSE_REPO_ID)
                                    .label("Browse…")
                                    .outline()
                                    .small()
                                    .accessibility_label("Choose a folder")
                                    .disabled(adding || self.browsing.is_some())
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.browse(window, cx)),
                                    ),
                            )
                            .child(
                                Button::new(ADD_REPO_ID)
                                    .label("Add")
                                    .primary()
                                    .small()
                                    .accessibility_label("Add repository")
                                    .loading(adding)
                                    .disabled(adding)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit_repo(window, cx)
                                    })),
                            ),
                    ),
            )
    }
}

/// The source control switch as the rows draw it: the repository whose
/// switch is in flight, and the last failed one with its sentence.
struct Switches {
    busy: Option<RepoId>,
    error: Option<(RepoId, String)>,
}

/// A repository: its name over its full path and its source control, with
/// the meta tag or the button that makes it the meta repository, the button
/// that turns its source control on or off, and why that last failed.
/// `choosing` is the repository whose meta choice is in flight.
fn repo_row(
    row: RepoRowVm,
    choosing: Option<RepoId>,
    switches: &Switches,
    paint: &Paint,
    cx: &Context<SettingsPane>,
) -> impl IntoElement {
    let id = row.id;
    let switch_error = switches
        .error
        .as_ref()
        .filter(|(failed, _)| *failed == id)
        .map(|(_, text)| text.clone());
    v_flex()
        .id(repo_row_id(row.id))
        .role(Role::ListItem)
        .test_support()
        .aria_label(format!(
            "{}, {}, {}",
            row.name, row.path, row.source_control
        ))
        .w_full()
        .min_w_0()
        .child(
            Label::new(row.name)
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(paint.foreground)
                .truncate(),
        )
        .child(
            Label::new(row.path)
                .text_xs()
                .text_color(paint.muted_foreground)
                .truncate(),
        )
        .child(
            Label::new(row.source_control)
                .id(repo_source_control_id(id))
                .text_xs()
                .text_color(paint.muted_foreground)
                .truncate(),
        )
        .when(row.is_meta, |this| {
            this.child(
                div().child(
                    Tag::secondary().small().child(
                        Label::new(META_TAG)
                            .id(repo_meta_tag_id(id))
                            .text_color(cx.theme().secondary_foreground),
                    ),
                ),
            )
        })
        .child(
            h_flex()
                .flex_wrap()
                .gap_2()
                .when(row.can_set_meta, |this| {
                    this.child(
                        Button::new(repo_use_meta_id(id))
                            .label(USE_META_LABEL)
                            .outline()
                            .small()
                            .loading(choosing == Some(id))
                            .disabled(choosing.is_some())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.choose_meta_repo(id, window, cx)
                            })),
                    )
                })
                .child(
                    Button::new(repo_switch_id(id))
                        .label(if row.has_source_control {
                            TURN_OFF_LABEL
                        } else {
                            TURN_ON_LABEL
                        })
                        .outline()
                        .small()
                        .loading(switches.busy == Some(id))
                        .disabled(switches.busy.is_some())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.ask_to_switch(id, window, cx)
                        })),
                ),
        )
        .when_some(switch_error, |this, text| {
            this.child(
                div()
                    .id(repo_switch_error_id(id))
                    .role(Role::Status)
                    .test_support()
                    .aria_label(text.clone())
                    .text_xs()
                    .text_color(paint.destructive)
                    .child(text),
            )
        })
}

/// Asks before turning off the source control of the repository intents
/// and feedback use ([`TURN_OFF_META_QUESTION`]), with Cancel (focused, and
/// what Enter does) and a "Turn off source control" button, the only way to
/// go ahead.
fn confirm_turn_off(pane: WeakEntity<SettingsPane>, id: RepoId, window: &mut Window, cx: &mut App) {
    let cancel_focus = cx.focus_handle();
    // The builder runs again whenever the dialog layer rebuilds; focus only
    // on the first run, so a rebuild never steals the focus back.
    let first_build = Rc::new(Cell::new(true));
    window.open_alert_dialog(cx, move |alert, window, cx| {
        if first_build.replace(false) {
            cancel_focus.focus(window, cx);
        }
        let pane = pane.clone();
        alert
            .title(
                Label::new(TURN_OFF_TITLE)
                    .id(TURN_OFF_TITLE_ID)
                    .text_color(cx.theme().popover_foreground),
            )
            .description(
                Label::new(TURN_OFF_META_QUESTION)
                    .id(TURN_OFF_DESCRIPTION_ID)
                    .text_color(cx.theme().muted_foreground),
            )
            // Enter dispatches the dialog's `Confirm`, which runs this: here it
            // dismisses, like Cancel. Returning `true` closes the dialog.
            .on_ok(|_, _, _| true)
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new(TURN_OFF_CANCEL_ID)
                            .label("Cancel")
                            .outline()
                            .track_focus(&cancel_focus)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new(TURN_OFF_CONFIRM_ID)
                            .label(TURN_OFF_LABEL)
                            .danger()
                            .on_click(move |_, window, cx| {
                                let _ = pane.update(cx, |pane, cx| {
                                    pane.switch_source_control(id, false, window, cx)
                                });
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}

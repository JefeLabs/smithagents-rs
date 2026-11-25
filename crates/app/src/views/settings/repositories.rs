//! The Source repositories section: the daemon's repositories, and an "Add
//! repository" field for a local path (typed, or chosen with the native
//! folder picker).
//!
//! The add flow: [`validate_repo_path_input`] first (a refusal shows under
//! the field and sends nothing; a URL says cloning is not supported yet),
//! then `add_repo` through the commands seam. While it runs, the field and
//! its buttons are disabled, so a second Enter or click sends nothing; it is
//! given up after the shell's command timeout, or when the connection drops,
//! with "The daemon did not answer". The daemon may still finish it: when a
//! repository matching the path appears in the model afterwards, that line
//! and the field clear as on success. On success the field clears; the
//! repository itself arrives through the model (`Action::RepoAdded`, which
//! also sets the "Repository added" status). On failure the daemon's message
//! shows under the field and the typed text stays.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::label::Label;
use gpui_kit::component::{h_flex, v_flex, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    PathPromptOptions, Role, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    Window,
};
use smithagents_core::agent::Repo;
use smithagents_core::ids::RepoId;

use super::{caption, Paint, SentAdd, SettingsPane};
use crate::link::LinkError;
use crate::view_models::{
    repo_error_text, repo_matches_sent_path, repo_rows, validate_repo_path_input, RepoRowVm,
    DAEMON_SILENT_TEXT, NOT_CONNECTED_TEXT, NO_REPOS_TEXT,
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

pub(super) const PATH_PLACEHOLDER: &str = "/path/to/a/git/repository";

/// Element id of a repository's row.
pub fn repo_row_id(id: RepoId) -> ElementId {
    ElementId::Name(format!("repo-row-{id}").into())
}

impl SettingsPane {
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
        let (add, timeout) = {
            let state = self.state.read(cx);
            (
                state
                    .agent_commands()
                    .map(|commands| commands.add_repo(&path)),
                state.command_timeout(),
            )
        };
        let Some(add) = add else {
            self.path_error = Some(NOT_CONNECTED_TEXT.to_owned());
            cx.notify();
            return;
        };
        tracing::debug!(path = %path.display(), "adding a repository");
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
        let rows = repo_rows(self.state.read(cx).model().repos());
        let adding = self.adding.is_some();
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
                    .children(rows.into_iter().map(|row| repo_row(row, paint)))
                    .into_any_element()
            })
            .child(
                v_flex()
                    .w_full()
                    .gap_1p5()
                    .child(caption("Add repository", paint))
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

/// A repository: its name over its full path and default branch.
fn repo_row(row: RepoRowVm, paint: &Paint) -> impl IntoElement {
    v_flex()
        .id(repo_row_id(row.id))
        .role(Role::ListItem)
        .test_support()
        .aria_label(format!(
            "{}, {}, default branch {}",
            row.name, row.path, row.default_branch
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
            Label::new(format!("Default branch: {}", row.default_branch))
                .text_xs()
                .text_color(paint.muted_foreground)
                .truncate(),
        )
}

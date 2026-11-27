//! The detail's actions, fixed at the bottom under the error line: why the
//! agent could not start, if it could not, then a bar with what the intent's
//! state lets the user do next.
//!
//! - Complete: "Send", which asks the daemon to queue the intent (it starts
//!   an agent when a slot is free).
//! - Queued: why it has not started yet ([`queued_reason`]).
//! - Sent: "Open chat", which selects the intent's agent and opens its tab
//!   (disabled, with the tooltip "The agent was closed", once the model no
//!   longer has the agent), and "Mark resolved…", which asks first.
//! - A draft, or a resolved intent: no bar, except for an idea intent.
//!
//! An idea intent that is resolved, or sent with its agent done
//! ([`idea_actions_shown`]), adds to its bar: what its agent could not do
//! ("Not done: …", a status line above the buttons); "Open pull request…",
//! which asks first and then has the daemon push the idea branch and open a
//! draft pull request, or once there is one its address (only a github.com
//! pull request's, [`shows_pull_request_url`]), which opens in the browser;
//! and "Create implementation intent", which opens the intent the daemon
//! answers with. Both run through the feedback commands, disabled without
//! them.
//!
//! The start error ("The agent could not start: …") is drawn in every state
//! while the daemon keeps it. A Send the daemon refuses with that same error
//! shows only this line, not the screen's error line too. The bar's parts sit
//! in one row that wraps. Every command runs through [`IntentsPane::run`], so
//! nothing changes on screen before the daemon's copy reaches the model, and
//! a second press while one is in flight sends nothing.
//!
//! Enter never sends, resolves or opens a pull request: no key binding
//! dispatches them, and the pane never moves the focus onto them (a button
//! the user tabbed to still takes Enter, as every button does). A
//! confirmation opens with the focus on Cancel, and Enter there, which
//! gpui-base binds to the dialog's `Confirm` (running `on_ok`), dismisses it
//! like Cancel. Only its confirm button goes ahead.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::label::Label;
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, AnyElement, App, ClipboardItem, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, WeakEntity, Window,
};
use smithagents_core::ids::{AgentId, IntentId};
use smithagents_core::intent::{Intent, IntentState};

use super::detail::opens_a_link;
use super::{Busy, IntentsPane, Paint, Screen};
use crate::feedback::text::{
    COPY_LINK_LABEL, IMPLEMENT_LABEL, OPEN_PR_CONFIRM_LABEL, OPEN_PR_LABEL, PR_DESCRIPTION,
    PR_TITLE,
};
use crate::intent_vm::{
    idea_actions_shown, not_done_text, queued_reason, shows_pull_request_url, start_error_text,
};
use crate::link::{FeedbackCommands, IntentAnswer};
use crate::model::Action;
use crate::view_models::NOT_CONNECTED_TEXT;

/// Element id of the Send button.
pub const SEND_ID: &str = "intent-send";
/// Element id of why a queued intent has not started.
pub const QUEUED_REASON_ID: &str = "intent-queued-reason";
/// Element id of the Open chat button.
pub const OPEN_CHAT_ID: &str = "intent-open-chat";
/// Element id of the Mark resolved… button (it opens the confirmation).
pub const RESOLVE_ID: &str = "intent-resolve";
/// Element ids of the confirmation's title, text and buttons.
pub const RESOLVE_TITLE_ID: &str = "intent-resolve-title";
pub const RESOLVE_DESCRIPTION_ID: &str = "intent-resolve-description";
pub const RESOLVE_CANCEL_ID: &str = "intent-resolve-cancel";
pub const RESOLVE_CONFIRM_ID: &str = "intent-resolve-confirm";
/// Element id of "The agent could not start: …" (a status).
pub const START_ERROR_ID: &str = "intent-start-error";
/// The confirmation's text.
pub const RESOLVE_DESCRIPTION: &str =
    "This means the work has landed on the default branch, so intents that depend on it can start.";
/// Element id of an idea intent's "Open pull request…" button (it opens the
/// confirmation), and of the confirmation's title, text and buttons.
pub const OPEN_PR_ID: &str = "intent-open-pr";
pub const OPEN_PR_TITLE_ID: &str = "intent-open-pr-title";
pub const OPEN_PR_DESCRIPTION_ID: &str = "intent-open-pr-description";
pub const OPEN_PR_CANCEL_ID: &str = "intent-open-pr-cancel";
pub const OPEN_PR_CONFIRM_ID: &str = "intent-open-pr-confirm";
/// Element id of the idea's pull request address (a label).
pub const PR_URL_ID: &str = "intent-pr-url";
/// Element id of "Not done: …" (a status).
pub const NOT_DONE_ID: &str = "intent-not-done";
/// Element id of the "Create implementation intent" button.
pub const IMPLEMENT_ID: &str = "intent-implement";
/// Element id of the button that copies the pull request's address.
pub const PR_COPY_ID: &str = "intent-pr-copy";
/// Element id of the pull request address's clickable area.
const PR_LINK_ID: &str = "intent-pr-link";

const SEND_LABEL: &str = "Send";
const OPEN_CHAT_LABEL: &str = "Open chat";
const AGENT_CLOSED_TEXT: &str = "The agent was closed";
const RESOLVE_LABEL: &str = "Mark resolved…";
const RESOLVE_TITLE: &str = "Mark as resolved?";
const RESOLVE_CONFIRM_LABEL: &str = "Mark resolved";
const CANCEL_LABEL: &str = "Cancel";

impl IntentsPane {
    /// The start error, then the bar; `None` when there is neither.
    pub(super) fn actions(
        &self,
        intent: &Intent,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let start_error = intent
            .last_start_error
            .as_deref()
            .map(|error| start_error_line(error, paint));
        let idea = self.idea_shown(intent, cx);
        let not_done = intent
            .origin
            .as_ref()
            .and_then(|origin| origin.idea.as_ref())
            .and_then(|idea| idea.not_done.as_deref())
            .filter(|_| idea)
            .map(|text| not_done_line(text, paint));
        let bar = self.action_bar(intent, paint, cx);
        if start_error.is_none() && not_done.is_none() && bar.is_none() {
            return None;
        }
        Some(
            v_flex()
                .flex_none()
                .w_full()
                .children(start_error)
                .children(not_done)
                .children(bar)
                .into_any_element(),
        )
    }

    /// Whether `intent` offers the idea actions ([`idea_actions_shown`],
    /// with its agent's state as the model has it).
    fn idea_shown(&self, intent: &Intent, cx: &Context<Self>) -> bool {
        let agent = intent
            .agent_id
            .and_then(|agent| self.state.read(cx).model().agent(agent))
            .map(|agent| agent.state);
        idea_actions_shown(intent, agent)
    }

    /// An idea intent's parts of the bar: "Open pull request…" or its
    /// address, then "Create implementation intent". Empty for any other
    /// intent.
    fn idea_parts(&self, intent: &Intent, cx: &Context<Self>) -> Vec<AnyElement> {
        let Some(idea) = intent
            .origin
            .as_ref()
            .and_then(|origin| origin.idea.as_ref())
            .filter(|_| self.idea_shown(intent, cx))
        else {
            return Vec::new();
        };
        let id = intent.id;
        let disabled = self.busy.is_some() || self.state.read(cx).feedback_commands().is_none();
        let mut parts = Vec::new();
        match idea.pr_url.as_deref() {
            None => parts.push(
                Button::new(OPEN_PR_ID)
                    .label(OPEN_PR_LABEL)
                    .outline()
                    .small()
                    .disabled(disabled)
                    .loading(self.busy == Some(Busy::PullRequest))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.ask_to_open_pull_request(id, window, cx)
                    }))
                    .into_any_element(),
            ),
            Some(url) if shows_pull_request_url(url) => {
                parts.push(pull_request_link(url, cx));
                parts.push(copy_link_button(url));
            }
            // Not an address the app shows; there is a pull request all the
            // same, so no second one is offered.
            Some(_) => {}
        }
        parts.push(
            Button::new(IMPLEMENT_ID)
                .label(IMPLEMENT_LABEL)
                .outline()
                .small()
                .disabled(disabled)
                .loading(self.busy == Some(Busy::Implementation))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.create_implementation(id, window, cx)
                }))
                .into_any_element(),
        );
        parts
    }

    /// What the intent's state offers, and an idea intent's actions; `None`
    /// for a draft or a resolved intent that is no idea. The bar's parts sit
    /// in one row that wraps, so more of them fit a narrow pane.
    fn action_bar(&self, intent: &Intent, paint: &Paint, cx: &Context<Self>) -> Option<AnyElement> {
        let id = intent.id;
        let busy = self.busy.is_some();
        let idea = self.idea_parts(intent, cx);
        let mut parts: Vec<AnyElement> = match intent.state {
            IntentState::Draft => return None,
            // Nothing to offer, but an idea's actions.
            IntentState::Resolved if idea.is_empty() => return None,
            IntentState::Resolved => Vec::new(),
            IntentState::Complete => vec![Button::new(SEND_ID)
                .label(SEND_LABEL)
                .primary()
                .small()
                .disabled(busy)
                .loading(self.busy == Some(Busy::Send))
                .on_click(cx.listener(move |this, _, window, cx| this.send(id, window, cx)))
                .into_any_element()],
            IntentState::Queued => {
                vec![
                    Label::new(queued_reason(self.state.read(cx).model(), intent))
                        .id(QUEUED_REASON_ID)
                        .w_full()
                        .min_w_0()
                        .text_sm()
                        .text_color(paint.muted_foreground)
                        .into_any_element(),
                ]
            }
            IntentState::Sent => {
                let agent = intent
                    .agent_id
                    .filter(|agent| self.state.read(cx).model().agent(*agent).is_some());
                vec![
                    open_chat_button(agent, cx).into_any_element(),
                    Button::new(RESOLVE_ID)
                        .label(RESOLVE_LABEL)
                        .outline()
                        .small()
                        .disabled(busy)
                        .loading(self.busy == Some(Busy::Resolve))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.ask_to_resolve(id, window, cx)
                        }))
                        .into_any_element(),
                ]
            }
        };
        parts.extend(idea);
        Some(
            h_flex()
                .flex_none()
                .flex_wrap()
                .w_full()
                .min_w_0()
                .items_center()
                .justify_end()
                .gap_2()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(paint.border)
                .children(parts)
                .into_any_element(),
        )
    }

    /// Whether the error line only repeats the start-error line: it shows
    /// the daemon's refusal of a Send, and `intent`'s start error, which the
    /// daemon records with the same message, ends with that message. (The
    /// refusal and the intent's copy arrive in either order.)
    pub(super) fn error_line_repeats_start_error(&self, intent: &Intent) -> bool {
        let (Some(error), Some((line, message)), Some(start_error)) = (
            self.error.as_deref(),
            self.send_refusal.as_ref(),
            intent.last_start_error.as_deref(),
        ) else {
            return false;
        };
        let message = message.trim();
        error == line && !message.is_empty() && start_error.trim_end().ends_with(message)
    }

    /// Asks the daemon to send the intent; nothing while another command is
    /// in flight.
    fn send(&mut self, id: IntentId, window: &mut Window, cx: &mut Context<Self>) {
        tracing::debug!(%id, "sending an intent");
        self.command(
            Busy::Send,
            move |commands| commands.send_intent(id),
            |_, _, _, _| {},
            window,
            cx,
        );
    }

    /// Opens the confirmation; nothing while a command is in flight.
    fn ask_to_resolve(&mut self, id: IntentId, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        confirm(
            cx.entity().downgrade(),
            &RESOLVE_CONFIRMATION,
            move |pane, window, cx| pane.resolve(id, window, cx),
            window,
            cx,
        );
    }

    /// Opens the pull request's confirmation; nothing while a command is in
    /// flight or without feedback commands.
    fn ask_to_open_pull_request(
        &mut self,
        id: IntentId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() || self.state.read(cx).feedback_commands().is_none() {
            return;
        }
        confirm(
            cx.entity().downgrade(),
            &OPEN_PR_CONFIRMATION,
            move |pane, window, cx| pane.open_pull_request(id, window, cx),
            window,
            cx,
        );
    }

    /// Asks the daemon to push the idea branch and open its pull request;
    /// nothing while another command is in flight.
    fn open_pull_request(&mut self, id: IntentId, window: &mut Window, cx: &mut Context<Self>) {
        tracing::debug!(%id, "opening an idea's pull request");
        self.feedback_command(
            Busy::PullRequest,
            move |commands| commands.open_pull_request(id),
            |_, _, _, _| {},
            window,
            cx,
        );
    }

    /// Asks the daemon for the idea's implementation intent, and opens it;
    /// nothing while another command is in flight.
    fn create_implementation(&mut self, id: IntentId, window: &mut Window, cx: &mut Context<Self>) {
        tracing::debug!(%id, "creating an idea's implementation intent");
        self.feedback_command(
            Busy::Implementation,
            move |commands| commands.create_implementation_intent(id),
            |pane, created, window, cx| pane.open_screen(Screen::Detail(created.id), window, cx),
            window,
            cx,
        );
    }

    /// [`IntentsPane::run`] for a feedback command `make` builds: while
    /// another command is in flight it does nothing (`make` is not called,
    /// so nothing is sent); without feedback commands the error line says
    /// so.
    fn feedback_command(
        &mut self,
        key: Busy,
        make: impl FnOnce(&dyn FeedbackCommands) -> IntentAnswer,
        done: impl FnOnce(&mut Self, Intent, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(busy) = self.busy {
            tracing::debug!(?key, ?busy, "an intent command is already in flight");
            return;
        }
        let Some(command) = self.state.read(cx).feedback_commands().map(make) else {
            self.error = Some(NOT_CONNECTED_TEXT.to_owned());
            cx.notify();
            return;
        };
        self.run(key, command, done, window, cx);
    }

    /// Asks the daemon to mark the intent resolved; nothing while another
    /// command is in flight.
    fn resolve(&mut self, id: IntentId, window: &mut Window, cx: &mut Context<Self>) {
        tracing::debug!(%id, "resolving an intent");
        self.command(
            Busy::Resolve,
            move |commands| commands.resolve_intent(id),
            |_, _, _, _| {},
            window,
            cx,
        );
    }

    /// Selects the agent and opens its tab, as the agents pane does.
    fn open_chat(&mut self, agent: AgentId, cx: &mut Context<Self>) {
        tracing::debug!(%agent, "opening an intent's chat");
        self.state
            .update(cx, |state, cx| state.dispatch(Action::OpenTab(agent), cx));
    }
}

/// "The agent could not start: …" ([`start_error_text`]), a status in
/// `destructive`.
fn start_error_line(error: &str, paint: &Paint) -> impl IntoElement {
    let text = start_error_text(error);
    div()
        .id(START_ERROR_ID)
        .role(Role::Status)
        .test_support()
        .aria_label(text.clone())
        .w_full()
        // A long path has no space to break at: it wraps where it reaches
        // the edge instead of widening the pane.
        .min_w_0()
        .whitespace_normal()
        .px_3()
        .pb_2()
        .text_xs()
        .text_color(paint.destructive)
        .child(text)
}

/// Open chat for `agent`, the intent's agent while the model has it;
/// without one it is disabled and its tooltip says why.
fn open_chat_button(agent: Option<AgentId>, cx: &Context<IntentsPane>) -> Button {
    Button::new(OPEN_CHAT_ID)
        .label(OPEN_CHAT_LABEL)
        .primary()
        .small()
        .map(|button| match agent {
            Some(agent) => {
                button.on_click(cx.listener(move |this, _, _, cx| this.open_chat(agent, cx)))
            }
            None => button.disabled(true).tooltip(AGENT_CLOSED_TEXT),
        })
}

/// "Not done: …" ([`not_done_text`]), a status in `muted-foreground`.
fn not_done_line(text: &str, paint: &Paint) -> impl IntoElement {
    let text = not_done_text(text);
    div()
        .id(NOT_DONE_ID)
        .role(Role::Status)
        .test_support()
        .aria_label(text.clone())
        .w_full()
        .min_w_0()
        .whitespace_normal()
        .px_3()
        .pb_2()
        .text_xs()
        .text_color(paint.muted_foreground)
        .child(text)
}

/// "Copy link": puts the pull request's address on the clipboard, only when
/// [`shows_pull_request_url`] allows it (the caller checked; the click
/// checks again).
fn copy_link_button(url: &str) -> AnyElement {
    let url = url.to_owned();
    Button::new(PR_COPY_ID)
        .icon(IconName::Copy)
        .ghost()
        .xsmall()
        .tooltip(COPY_LINK_LABEL)
        .on_click(move |_, _, cx| {
            if shows_pull_request_url(&url) {
                cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
            }
        })
        .into_any_element()
}

/// The pull request's address, a label in the link color that opens it in
/// the browser on the presses a link opens on ([`opens_a_link`]: not a right
/// click), only when [`shows_pull_request_url`] allows it (the caller
/// checked; the click checks again).
fn pull_request_link(url: &str, cx: &Context<IntentsPane>) -> AnyElement {
    let url = SharedString::from(url.to_owned());
    div()
        .id(PR_LINK_ID)
        .min_w_0()
        .cursor_pointer()
        .child(
            Label::new(url.clone())
                .id(PR_URL_ID)
                .text_sm()
                .underline()
                .text_color(cx.theme().link),
        )
        .on_click(move |event, _, cx| {
            if opens_a_link(event) && shows_pull_request_url(&url) {
                cx.open_url(&url);
            }
        })
        .into_any_element()
}

/// What a confirmation says, with the ids of its parts.
struct Confirmation {
    title_id: &'static str,
    title: &'static str,
    description_id: &'static str,
    description: &'static str,
    cancel_id: &'static str,
    confirm_id: &'static str,
    confirm: &'static str,
}

/// "Mark as resolved?"
const RESOLVE_CONFIRMATION: Confirmation = Confirmation {
    title_id: RESOLVE_TITLE_ID,
    title: RESOLVE_TITLE,
    description_id: RESOLVE_DESCRIPTION_ID,
    description: RESOLVE_DESCRIPTION,
    cancel_id: RESOLVE_CANCEL_ID,
    confirm_id: RESOLVE_CONFIRM_ID,
    confirm: RESOLVE_CONFIRM_LABEL,
};

/// "Open a pull request?", which says that a public repository shows the
/// screenshots to anyone (asked every time).
const OPEN_PR_CONFIRMATION: Confirmation = Confirmation {
    title_id: OPEN_PR_TITLE_ID,
    title: PR_TITLE,
    description_id: OPEN_PR_DESCRIPTION_ID,
    description: PR_DESCRIPTION,
    cancel_id: OPEN_PR_CANCEL_ID,
    confirm_id: OPEN_PR_CONFIRM_ID,
    confirm: OPEN_PR_CONFIRM_LABEL,
};

/// Asks first: `what`'s title and text, with Cancel (focused, and what
/// Enter does) and its confirm button, the only way to `go_ahead`.
fn confirm(
    pane: WeakEntity<IntentsPane>,
    what: &'static Confirmation,
    go_ahead: impl Fn(&mut IntentsPane, &mut Window, &mut Context<IntentsPane>) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let go_ahead = Rc::new(go_ahead);
    let cancel_focus = cx.focus_handle();
    // The builder runs again whenever the dialog layer rebuilds; focus only
    // on the first run, so a rebuild never steals the focus back.
    let first_build = Rc::new(Cell::new(true));
    window.open_alert_dialog(cx, move |alert, window, cx| {
        if first_build.replace(false) {
            cancel_focus.focus(window, cx);
        }
        let pane = pane.clone();
        let go_ahead = go_ahead.clone();
        alert
            .title(
                Label::new(what.title)
                    .id(what.title_id)
                    .text_color(cx.theme().popover_foreground),
            )
            .description(
                Label::new(what.description)
                    .id(what.description_id)
                    .text_color(cx.theme().muted_foreground),
            )
            // Enter dispatches the dialog's `Confirm`, which runs this: here it
            // dismisses, like Cancel. Returning `true` closes the dialog.
            .on_ok(|_, _, _| true)
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new(what.cancel_id)
                            .label(CANCEL_LABEL)
                            .outline()
                            .track_focus(&cancel_focus)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new(what.confirm_id)
                            .label(what.confirm)
                            .primary()
                            .on_click(move |_, window, cx| {
                                let _ = pane.update(cx, |pane, cx| go_ahead(pane, window, cx));
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}

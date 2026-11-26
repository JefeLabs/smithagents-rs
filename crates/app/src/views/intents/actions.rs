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
//! - A draft, or a resolved intent: no bar.
//!
//! The start error ("The agent could not start: …") is drawn in every state
//! while the daemon keeps it. Send and Mark resolved run through
//! [`IntentsPane::run`], so nothing changes on screen before the daemon's
//! copy reaches the model, and a second press while one is in flight sends
//! nothing.
//!
//! Enter never sends or resolves: no key binding dispatches either, and the
//! pane never moves the focus onto them (a button the user tabbed to still
//! takes Enter, as every button does). The confirmation opens with the focus
//! on Cancel, and Enter there, which gpui-base binds to the dialog's
//! `Confirm` (running `on_ok`), dismisses it like Cancel. Only the "Mark
//! resolved" button resolves.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::label::Label;
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, WeakEntity, Window,
};
use smithagents_core::ids::{AgentId, IntentId};
use smithagents_core::intent::{Intent, IntentState};

use super::{Busy, IntentsPane, Paint};
use crate::intent_vm::queued_reason;
use crate::model::Action;

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
        let bar = self.action_bar(intent, paint, cx);
        if start_error.is_none() && bar.is_none() {
            return None;
        }
        Some(
            v_flex()
                .flex_none()
                .w_full()
                .children(start_error)
                .children(bar)
                .into_any_element(),
        )
    }

    /// What the intent's state offers; `None` for a draft or a resolved
    /// intent.
    fn action_bar(&self, intent: &Intent, paint: &Paint, cx: &Context<Self>) -> Option<AnyElement> {
        let id = intent.id;
        let busy = self.busy.is_some();
        let content = match intent.state {
            IntentState::Draft | IntentState::Resolved => return None,
            IntentState::Complete => Button::new(SEND_ID)
                .label(SEND_LABEL)
                .primary()
                .small()
                .disabled(busy)
                .loading(self.busy == Some(Busy::Send))
                .on_click(cx.listener(move |this, _, window, cx| this.send(id, window, cx)))
                .into_any_element(),
            IntentState::Queued => Label::new(queued_reason(self.state.read(cx).model(), intent))
                .id(QUEUED_REASON_ID)
                .w_full()
                .min_w_0()
                .text_sm()
                .text_color(paint.muted_foreground)
                .into_any_element(),
            IntentState::Sent => {
                let agent = intent
                    .agent_id
                    .filter(|agent| self.state.read(cx).model().agent(*agent).is_some());
                h_flex()
                    .gap_2()
                    .child(open_chat_button(agent, cx))
                    .child(
                        Button::new(RESOLVE_ID)
                            .label(RESOLVE_LABEL)
                            .outline()
                            .small()
                            .disabled(busy)
                            .loading(self.busy == Some(Busy::Resolve))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.ask_to_resolve(id, window, cx)
                            })),
                    )
                    .into_any_element()
            }
        };
        Some(
            h_flex()
                .flex_none()
                .w_full()
                .min_w_0()
                .items_center()
                .justify_end()
                .gap_2()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(paint.border)
                .child(content)
                .into_any_element(),
        )
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
        confirm_resolve(cx.entity().downgrade(), id, window, cx);
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

/// "The agent could not start: …", a status in `destructive`.
fn start_error_line(error: &str, paint: &Paint) -> impl IntoElement {
    let text = format!("The agent could not start: {error}");
    div()
        .id(START_ERROR_ID)
        .role(Role::Status)
        .test_support()
        .aria_label(text.clone())
        .w_full()
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

/// Asks before resolving: "Mark as resolved?" with Cancel (focused, and
/// what Enter does) and "Mark resolved", the only way to resolve.
fn confirm_resolve(pane: WeakEntity<IntentsPane>, id: IntentId, window: &mut Window, cx: &mut App) {
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
                Label::new(RESOLVE_TITLE)
                    .id(RESOLVE_TITLE_ID)
                    .text_color(cx.theme().popover_foreground),
            )
            .description(
                Label::new(RESOLVE_DESCRIPTION)
                    .id(RESOLVE_DESCRIPTION_ID)
                    .text_color(cx.theme().muted_foreground),
            )
            // Enter dispatches the dialog's `Confirm`, which runs this: here it
            // dismisses, like Cancel. Returning `true` closes the dialog.
            .on_ok(|_, _, _| true)
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new(RESOLVE_CANCEL_ID)
                            .label(CANCEL_LABEL)
                            .outline()
                            .track_focus(&cancel_focus)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new(RESOLVE_CONFIRM_ID)
                            .label(RESOLVE_CONFIRM_LABEL)
                            .primary()
                            .on_click(move |_, window, cx| {
                                let _ = pane.update(cx, |pane, cx| pane.resolve(id, window, cx));
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}

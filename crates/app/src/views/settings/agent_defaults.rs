//! The Agent defaults section: "Agents working at once", the limit on agents
//! replying at the same time (the daemon queues intents beyond it).
//!
//! A small outline button shows the current limit and opens a menu: "No
//! limit", then 1 to 16, the current one checked. A stored limit above 16
//! (set through the API) is shown as its number with no entry checked, and
//! stays until the user picks another. Choosing an entry sends
//! `set_agent_defaults` through the intent commands; nothing is optimistic:
//! the button keeps its label, loading and disabled, until the daemon's answer
//! reaches the model (`Action::AgentDefaultsLoaded`). A refusal, or no answer
//! within the command timeout, shows under the control. While the model has no
//! defaults the button is disabled and the help line says so.

use gpui_kit::component::button::Button;
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{v_flex, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, WeakEntity, Window,
};
use smithagents_core::intent::AgentDefaults;

use super::{Paint, SettingsPane};
use crate::intent_vm::{intent_error_text, queue_limit_label, QUEUE_LIMIT_CHOICES};
use crate::link::LinkError;
use crate::view_models::{DAEMON_SILENT_TEXT, NOT_CONNECTED_TEXT};
use crate::views::answered;

/// The caption over the control, and its accessible group name.
pub const QUEUE_LIMIT_LABEL: &str = "Agents working at once";
/// Element id of the caption.
pub const QUEUE_LIMIT_LABEL_ID: &str = "queue-limit-label";
/// Element id of the dropdown button; its label is the current limit.
pub const QUEUE_LIMIT_ID: &str = "queue-limit";
/// The help line under the control.
pub const QUEUE_LIMIT_HELP: &str =
    "Every agent that is replying counts. Agents you start yourself never wait.";
/// Element id of the help line.
pub const QUEUE_LIMIT_HELP_ID: &str = "queue-limit-help";
/// Element id of the line under the control that says why a change failed.
pub const QUEUE_LIMIT_ERROR_ID: &str = "queue-limit-error";

/// What the button says while the model has no defaults to show.
const UNAVAILABLE_LABEL: &str = "Unavailable";

/// Element id of `limit`'s entry in the menu ("queue-limit-none" or
/// "queue-limit-<n>").
pub fn queue_limit_item_id(limit: Option<u32>) -> ElementId {
    ElementId::Name(match limit {
        None => "queue-limit-none".into(),
        Some(n) => format!("queue-limit-{n}").into(),
    })
}

impl SettingsPane {
    /// Sends the new limit. Does nothing while a change is in flight or
    /// without defaults to change.
    fn set_queue_limit(&mut self, limit: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        if self.defaults_command.is_some() {
            tracing::debug!("a queue limit change is already in flight");
            return;
        }
        let (command, timeout) = {
            let state = self.state.read(cx);
            (
                state.intent_commands().map(|commands| {
                    commands.set_agent_defaults(AgentDefaults { queue_limit: limit })
                }),
                state.command_timeout(),
            )
        };
        let Some(command) = command else {
            self.defaults_error = Some(NOT_CONNECTED_TEXT.to_owned());
            cx.notify();
            return;
        };
        tracing::debug!(?limit, "setting the queue limit");
        self.defaults_error = None;
        let timer = cx.background_executor().timer(timeout);
        self.defaults_command = Some(cx.spawn_in(window, async move |this, cx| {
            let answer = answered(command, timer).await;
            let _ = this.update(cx, |this, cx| this.finish_queue_limit(answer, cx));
        }));
        cx.notify();
    }

    /// `answer` is `None` when the daemon did not answer in time. On success
    /// the model already holds the stored defaults (the link sent them).
    fn finish_queue_limit(
        &mut self,
        answer: Option<Result<AgentDefaults, LinkError>>,
        cx: &mut Context<Self>,
    ) {
        self.defaults_command = None;
        match answer {
            None => {
                tracing::warn!("the daemon did not answer a queue limit change in time");
                self.defaults_error = Some(DAEMON_SILENT_TEXT.to_owned());
            }
            Some(Ok(stored)) => tracing::debug!(?stored, "queue limit stored"),
            Some(Err(e)) => {
                tracing::warn!("cannot set the queue limit: {e}");
                self.defaults_error = Some(intent_error_text(&e));
            }
        }
        cx.notify();
    }

    /// Stops waiting for the change in flight (the connection dropped).
    pub(super) fn give_up_defaults(&mut self) {
        tracing::warn!("the connection dropped while a queue limit change waited");
        self.defaults_command = None;
        self.defaults_error = Some(DAEMON_SILENT_TEXT.to_owned());
    }

    pub(super) fn agent_defaults_content(
        &self,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let current = self
            .state
            .read(cx)
            .model()
            .agent_defaults()
            .map(|defaults| defaults.queue_limit);
        let busy = self.defaults_command.is_some();
        let pane = cx.entity().downgrade();
        let label = current.map_or_else(|| UNAVAILABLE_LABEL.to_owned(), queue_limit_label);
        v_flex()
            .w_full()
            .gap_1p5()
            .child(
                Label::new(QUEUE_LIMIT_LABEL)
                    .id(QUEUE_LIMIT_LABEL_ID)
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(paint.muted_foreground),
            )
            .child(
                div().child(
                    Button::new(QUEUE_LIMIT_ID)
                        .label(label)
                        .outline()
                        .small()
                        .dropdown_caret(true)
                        .loading(busy)
                        .disabled(busy || current.is_none())
                        .dropdown_menu(move |menu, _, _| queue_limit_menu(menu, current, &pane)),
                ),
            )
            .when_some(self.defaults_error.clone(), |this, text| {
                this.child(
                    div()
                        .id(QUEUE_LIMIT_ERROR_ID)
                        .role(Role::Status)
                        .test_support()
                        .aria_label(text.clone())
                        .text_xs()
                        .text_color(paint.destructive)
                        .child(text),
                )
            })
            .child(
                Label::new(if current.is_some() {
                    QUEUE_LIMIT_HELP
                } else {
                    NOT_CONNECTED_TEXT
                })
                .id(QUEUE_LIMIT_HELP_ID)
                .text_xs()
                .text_color(paint.muted_foreground),
            )
    }
}

/// The menu: "No limit", then 1 to 16, the current limit checked (`0` is
/// no limit too).
fn queue_limit_menu(
    mut menu: PopupMenu,
    current: Option<Option<u32>>,
    pane: &WeakEntity<SettingsPane>,
) -> PopupMenu {
    let current = current.map(|limit| limit.filter(|n| *n != 0));
    let choices = std::iter::once(None).chain(QUEUE_LIMIT_CHOICES.map(Some));
    for choice in choices {
        let text = queue_limit_label(choice);
        let pane = pane.clone();
        menu = menu.item(
            // An element item's menu-item node has no name of its own, so
            // the entry carries a named node (as the repository pickers do).
            PopupMenuItem::element(move |_, _| {
                div()
                    .id(queue_limit_item_id(choice))
                    .role(Role::Group)
                    .test_support()
                    .aria_label(text.clone())
                    .text_sm()
                    .child(text.clone())
            })
            .checked(current == Some(choice))
            .on_click(move |_, window, cx| {
                let _ = pane.update(cx, |pane, cx| pane.set_queue_limit(choice, window, cx));
            }),
        );
    }
    menu
}

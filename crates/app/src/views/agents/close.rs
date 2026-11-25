//! The "Close agent" context action and its confirmation.
//!
//! The confirmation is a modal dialog whose safe choice is the default: it
//! opens with the focus on Cancel, and Enter, which gpui-base binds to the
//! dialog's `Confirm` (running `on_ok`), dismisses it without closing the
//! agent. Only the "Close agent" button closes. Escape dismisses as usual.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, IconName, WindowExt as _};
use gpui_kit::{App, Context, ParentElement as _, Styled as _, WeakEntity, Window};
use smithagents_core::ids::AgentId;

use super::{
    answered, notify_failure, AgentsPane, CANCEL_CLOSE_ID, CLOSE_AGENT_LABEL, CLOSE_TITLE_ID,
    CONFIRM_CLOSE_ID,
};
use crate::link::LinkError;
use crate::view_models::{command_error_text, DAEMON_SILENT_TEXT, NOT_CONNECTED_TEXT};

const FAILED: &str = "Could not close the agent";

impl AgentsPane {
    /// Sends the close, once per agent while one is in flight, and waits for
    /// it at most the shell's command timeout. The agent leaves the list only
    /// when the daemon's event says it closed.
    fn close_agent(&mut self, id: AgentId, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing.contains_key(&id) {
            tracing::debug!(agent = %id, "a close is already in flight");
            return;
        }
        let (close, timeout) = {
            let state = self.state.read(cx);
            (
                state
                    .agent_commands()
                    .map(|commands| commands.close_agent(id)),
                state.command_timeout(),
            )
        };
        let Some(close) = close else {
            notify_failure(FAILED, NOT_CONNECTED_TEXT, window, cx);
            return;
        };
        tracing::debug!(agent = %id, "closing an agent");
        let timer = cx.background_executor().timer(timeout);
        let task = cx.spawn_in(window, async move |this, cx| {
            let answer = answered(close, timer).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_close(id, answer, window, cx)
            });
        });
        self.closing.insert(id, task);
    }

    /// `answer` is `None` when the daemon did not answer in time.
    fn finish_close(
        &mut self,
        id: AgentId,
        answer: Option<Result<(), LinkError>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.closing.remove(&id);
        match answer {
            Some(Ok(())) => {}
            Some(Err(e)) => {
                tracing::warn!("cannot close agent {id}: {e}");
                notify_failure(FAILED, command_error_text(&e), window, cx);
            }
            None => {
                tracing::warn!("the daemon did not answer the close of agent {id} in time");
                notify_failure(FAILED, DAEMON_SILENT_TEXT, window, cx);
            }
        }
    }

    /// The context menu of an agent's row or marker.
    pub(super) fn close_menu(
        &self,
        id: AgentId,
        name: String,
        cx: &Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let pane = cx.entity().downgrade();
        move |menu, _, _| {
            let (pane, name) = (pane.clone(), name.clone());
            menu.item(
                PopupMenuItem::new(CLOSE_AGENT_LABEL)
                    .icon(IconName::Close)
                    .on_click(move |_, window, cx| {
                        let (pane, name) = (pane.clone(), name.clone());
                        // After the menu has closed and given focus back, so
                        // the dialog keeps the focus it takes.
                        window.defer(cx, move |window, cx| {
                            confirm_close(pane, id, name, window, cx)
                        });
                    }),
            )
        }
    }
}

/// Asks before closing an agent: "Close <name>?" with Cancel (focused, and
/// what Enter does) and a danger "Close agent" button, the only way to close.
fn confirm_close(
    pane: WeakEntity<AgentsPane>,
    id: AgentId,
    name: String,
    window: &mut Window,
    cx: &mut App,
) {
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
                Label::new(format!("Close {name}?"))
                    .id(CLOSE_TITLE_ID)
                    .text_color(cx.theme().popover_foreground),
            )
            .description("Its chat tab closes and it leaves the list. Its worktree stays on disk.")
            // Enter dispatches the dialog's `Confirm`, which runs this: here it
            // dismisses, like Cancel. Returning `true` closes the dialog.
            .on_ok(|_, _, _| true)
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new(CANCEL_CLOSE_ID)
                            .label("Cancel")
                            .outline()
                            .track_focus(&cancel_focus)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new(CONFIRM_CLOSE_ID)
                            .label(CLOSE_AGENT_LABEL)
                            .danger()
                            .on_click(move |_, window, cx| {
                                let _ =
                                    pane.update(cx, |pane, cx| pane.close_agent(id, window, cx));
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}

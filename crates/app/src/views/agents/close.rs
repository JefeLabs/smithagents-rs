//! The "Close agent" context action and its confirmation.

use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{IconName, WindowExt as _};
use gpui_kit::{App, Context, WeakEntity, Window};
use smithagents_core::ids::AgentId;

use super::{notify_failure, AgentsPane, CLOSE_AGENT_LABEL};
use crate::view_models::{command_error_text, NOT_CONNECTED_TEXT};

impl AgentsPane {
    fn close_agent(&mut self, id: AgentId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.closing.insert(id) {
            return;
        }
        let close = self
            .state
            .read(cx)
            .agent_commands()
            .map(|commands| commands.close_agent(id));
        let Some(close) = close else {
            self.closing.remove(&id);
            notify_failure("Could not close the agent", NOT_CONNECTED_TEXT, window, cx);
            return;
        };
        tracing::debug!(agent = %id, "closing an agent");
        cx.spawn_in(window, async move |this, cx| {
            let result = close.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.closing.remove(&id);
                if let Err(e) = result {
                    tracing::warn!("cannot close agent {id}: {e}");
                    notify_failure(
                        "Could not close the agent",
                        command_error_text(&e),
                        window,
                        cx,
                    );
                }
            });
        })
        .detach();
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

/// Asks before closing an agent; OK sends the close.
fn confirm_close(
    pane: WeakEntity<AgentsPane>,
    id: AgentId,
    name: String,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_alert_dialog(cx, move |alert, _, _| {
        let pane = pane.clone();
        alert
            .confirm()
            .title(format!("Close {name}?"))
            .description("Its chat tab closes and it leaves the list. Its worktree stays on disk.")
            .ok_text(CLOSE_AGENT_LABEL)
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                let _ = pane.update(cx, |pane, cx| pane.close_agent(id, window, cx));
                true
            })
    });
}

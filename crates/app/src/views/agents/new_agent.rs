//! The New agent button and its flow: the plan for the repositories there
//! are, the picker, the create in flight, and selecting the created agent.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::NotificationType;
use gpui_kit::component::{
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Role, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, WeakEntity, Window,
};
use smithagents_core::agent::Agent;
use smithagents_core::ids::{AgentId, RepoId};

use super::{
    notify, notify_link_error, notify_unreachable, repo_choice_id, AgentsPane, AwaitedAgent, Paint,
    NEW_AGENT_ID, NEW_AGENT_LABEL,
};
use crate::link::LinkError;
use crate::model::Action;
use crate::view_models::{
    new_agent_plan, NewAgentPlan, RepoChoiceVm, SettingsSection, StatusMessage, DAEMON_SILENT_TEXT,
    NEED_REPO_TEXT, NOT_CONNECTED_TEXT,
};
use crate::views::answered;

impl AgentsPane {
    /// The New agent button's click when it is not a picker: follows the plan
    /// for the repositories there are now.
    fn on_new_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.creating() {
            return;
        }
        let plan = {
            let state = self.state.read(cx);
            new_agent_plan(state.model().repos(), state.last_repo_id())
        };
        tracing::debug!(?plan, "new agent");
        match plan {
            NewAgentPlan::NeedRepo => {
                // Settings opens on Source repositories with its path field
                // focused.
                self.state.update(cx, |state, cx| {
                    state.request_settings_focus(SettingsSection::Repositories, cx)
                });
                notify(NotificationType::Info, None, NEED_REPO_TEXT, window, cx);
            }
            NewAgentPlan::Create(repo) => self.start_create(repo, window, cx),
            // Repositories arrived since the last frame: the next frame draws
            // the picker.
            NewAgentPlan::Pick { .. } => cx.notify(),
        }
    }

    /// A repository chosen in the picker: remembered, then created in.
    fn pick_repo(&mut self, repo: RepoId, window: &mut Window, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, _| state.set_last_repo_id(repo));
        self.start_create(repo, window, cx);
    }

    /// Sends the create and waits for it, at most the shell's command
    /// timeout. The task is kept in `self.create`; dropping it (the
    /// connection dropped) gives the create up.
    fn start_create(&mut self, repo: RepoId, window: &mut Window, cx: &mut Context<Self>) {
        if self.creating() {
            tracing::debug!("a create is already in flight");
            return;
        }
        let (create, selected_before, timeout) = {
            let state = self.state.read(cx);
            (
                state
                    .agent_commands()
                    .map(|commands| commands.create_agent(repo)),
                state.model().selected(),
                state.command_timeout(),
            )
        };
        let Some(create) = create else {
            notify_unreachable("Could not create the agent", NOT_CONNECTED_TEXT, window, cx);
            return;
        };
        tracing::debug!(%repo, "creating an agent");
        let timer = cx.background_executor().timer(timeout);
        self.create = Some(cx.spawn_in(window, async move |this, cx| {
            let answer = answered(create, timer).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_create(answer, selected_before, window, cx)
            });
        }));
        cx.notify();
    }

    /// `answer` is `None` when the daemon did not answer in time.
    fn finish_create(
        &mut self,
        answer: Option<Result<Agent, LinkError>>,
        selected_before: Option<AgentId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.create = None;
        let Some(result) = answer else {
            tracing::warn!("the daemon did not answer a create in time");
            notify_unreachable("Could not create the agent", DAEMON_SILENT_TEXT, window, cx);
            cx.notify();
            return;
        };
        match result {
            Ok(agent) => {
                tracing::debug!(agent = %agent.id, name = %agent.name, "agent created");
                let text = format!("Agent {} created", agent.name);
                self.state.update(cx, |state, cx| {
                    state.set_status(StatusMessage::info(text), cx)
                });
                self.awaiting = Some(AwaitedAgent {
                    id: agent.id,
                    selected_before,
                });
                self.select_awaited(cx);
            }
            Err(e) => {
                tracing::warn!("cannot create an agent: {e}");
                notify_link_error(&self.state, "Could not create the agent", &e, window, cx);
            }
        }
        cx.notify();
    }

    /// Selects the created agent once the model has it, unless the user
    /// selected another agent since the create started.
    pub(super) fn select_awaited(&mut self, cx: &mut Context<Self>) {
        let Some(awaited) = self.awaiting else {
            return;
        };
        let untouched = {
            let model = self.state.read(cx).model();
            if !model.agents().iter().any(|agent| agent.id == awaited.id) {
                return;
            }
            model.selected() == awaited.selected_before
        };
        self.awaiting = None;
        if untouched {
            self.dispatch(Action::SelectAgent(awaited.id), cx);
        }
    }

    pub(super) fn new_agent_button(
        &self,
        compact: bool,
        repos_plan: (NewAgentPlan, Vec<RepoChoiceVm>),
        paint: &Paint,
        cx: &Context<Self>,
    ) -> AnyElement {
        let button = Button::new(NEW_AGENT_ID)
            .icon(Icon::new(IconName::Plus).text_color(paint.foreground))
            .ghost()
            .small()
            .track_focus(&self.new_agent_focus)
            .text_color(paint.foreground)
            .accessibility_label(NEW_AGENT_LABEL)
            .map(|button| {
                if compact {
                    button.tooltip(NEW_AGENT_LABEL)
                } else {
                    button.label(NEW_AGENT_LABEL)
                }
            })
            .loading(self.creating())
            .disabled(self.creating());
        match repos_plan {
            (NewAgentPlan::Pick { .. }, choices) if !self.creating() => {
                let pane = cx.entity().downgrade();
                button
                    .dropdown_menu(move |menu, _, cx| picker_menu(menu, &choices, &pane, cx))
                    .into_any_element()
            }
            _ => button
                .on_click(cx.listener(|this, _, window, cx| this.on_new_agent(window, cx)))
                .into_any_element(),
        }
    }
}

/// The repository picker: one entry per choice, the folder's name over its
/// full path, the preselected one first and checked.
fn picker_menu(
    mut menu: PopupMenu,
    choices: &[RepoChoiceVm],
    pane: &WeakEntity<AgentsPane>,
    cx: &App,
) -> PopupMenu {
    let muted = cx.theme().muted_foreground;
    menu = menu.label("New agent in").max_w(px(420.));
    for (ix, choice) in choices.iter().enumerate() {
        let (title, path, repo) = (choice.title.clone(), choice.path.clone(), choice.id);
        let pane = pane.clone();
        menu = menu.item(
            // gpui-component gives an element item's menu-item node no name
            // (`PopupMenuItem::a11y_label` is `None` for it), so the entry
            // carries its own named node; its two lines are plain text, so a
            // screen reader does not read them a second time.
            PopupMenuItem::element(move |_, _| {
                v_flex()
                    .id(repo_choice_id(ix))
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
            .on_click(move |_, window, cx| {
                let _ = pane.update(cx, |pane, cx| pane.pick_repo(repo, window, cx));
            }),
        );
    }
    menu
}

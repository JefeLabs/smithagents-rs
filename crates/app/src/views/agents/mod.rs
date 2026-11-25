//! The agents pane: the left pane's content, in both of its modes.
//!
//! - **Expanded:** a header with the title and a New agent button, then one
//!   row per agent: a left accent bar in the agent's color, the name (always
//!   shown, truncated only if it must be), and a status dot with the state's
//!   words.
//! - **Compact** (the fixed 56 px strip, about 55 px of content): an icon-only
//!   New agent button, then one marker per agent: the agent's color with its
//!   initial on it, and its status dot. The tooltip and the accessibility
//!   label carry "<name>, <state>", so the color is never the only identifier
//!   and colliding initials (Mercury, Mars) are told apart.
//!
//! The status dot's shape is a second cue next to its color ([`state_dot`]):
//! idle is a hollow ring, quiet is the working dot dimmed.
//!
//! In both modes a click selects and a double-click opens the agent's chat
//! tab. On the focused row or marker (listbox keys), Space selects and Enter
//! opens; Up and Down move the selection, the keyboard focus and the scroll
//! position with it. The context menu's "Close agent" asks first; the
//! confirmation's default (Enter) is Cancel. A row and a marker of the same
//! agent share one focus handle, so switching modes keeps the focus.
//!
//! The New agent button follows [`new_agent_plan`]: with no repository it
//! shows the Settings view and a hint; with one it creates right away; with
//! several it opens a menu of them (the preselected one first and checked).
//! A create in flight disables the button. Agents are never inserted here: a
//! created agent arrives through the daemon's events and is selected when it
//! appears, unless the user selected another agent meanwhile. A failed
//! command shows a transient notification with the daemon's message. A
//! command the daemon does not answer within the shell's command timeout, or
//! that is still waiting when the connection drops, is given up with "The
//! daemon did not answer", so the button never stays disabled.
//!
//! Every color comes from the theme: the region's surface and foreground
//! ([`region_paint`]), the status and selection tokens ([`token_paint`]) and
//! the agent accent ([`agent_accent`]).

mod close;
mod new_agent;

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::label::Label;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, rems, App, ClickEvent, Context, Div, ElementId, Entity, FocusHandle, Focusable,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, KeyBinding, KeyboardButton,
    KeyboardClickEvent, ParentElement as _, Pixels, Render, Role, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
};
use smithagents_core::ids::AgentId;
use smithagents_theme::native::NativeTheme;

use crate::link::LinkError;
use crate::model::{Action, Connection};
use crate::shell_state::ShellState;
use crate::theme_adapter::{
    agent_accent, initial_text_color, region_paint, srgb_to_hsla, token_color, token_paint, Region,
};
use crate::view_models::{
    agent_rows, command_error_text, new_agent_plan, repo_choices, state_dot, AgentRowVm, DotStyle,
    LeftPaneMode, NewAgentPlan, StatusMessage, DAEMON_SILENT_TEXT,
};

gpui_kit::actions!(
    smithagents_agents,
    [
        /// Select the agent above the selected one (the last if none is), and
        /// move the keyboard focus to it.
        SelectPreviousAgent,
        /// Select the agent below the selected one (the first if none is), and
        /// move the keyboard focus to it.
        SelectNextAgent,
    ]
);

/// Key context of the pane, for its key bindings.
pub const KEY_CONTEXT: &str = "AgentsPane";
/// Element id of the pane's root, which holds the pane's focus in both modes.
pub const LEFT_PANE_CONTENT_ID: &str = "agents-pane";
/// Element id of the "Agents" title (expanded only).
pub const EXPANDED_TITLE_ID: &str = "agents-title";
/// Element id of the New agent button, in both modes.
pub const NEW_AGENT_ID: &str = "new-agent";
/// Element id of the list of rows or markers.
pub const AGENT_LIST_ID: &str = "agent-list";
/// Element id of the hint shown when there is no agent (expanded only).
pub const AGENTS_EMPTY_ID: &str = "agents-empty";
/// Element id of a row's accent bar, inside the row.
pub const ACCENT_BAR_ID: &str = "accent";
/// Element id of a marker's color square, inside the marker.
pub const SWATCH_ID: &str = "swatch";
/// Element id of a row's or a marker's status dot, inside it.
pub const STATUS_DOT_ID: &str = "status";
/// Element id of the text of the pane's notification.
pub const NOTICE_ID: &str = "agents-notice";

/// The New agent button's words (its label when expanded, its tooltip and
/// accessible name always).
pub const NEW_AGENT_LABEL: &str = "New agent";
/// The context action's words, and the confirmation's close button.
pub const CLOSE_AGENT_LABEL: &str = "Close agent";
/// Element id of the confirmation's title ("Close <name>?").
pub const CLOSE_TITLE_ID: &str = "close-agent-title";
/// Element id of the confirmation's Cancel button (focused when it opens).
pub const CANCEL_CLOSE_ID: &str = "cancel-close-agent";
/// Element id of the confirmation's "Close agent" button.
pub const CONFIRM_CLOSE_ID: &str = "confirm-close-agent";
/// What the expanded pane says when there is no agent.
pub const NO_AGENTS_TEXT: &str = "No agents yet";

/// The least height of an expanded row, in rems: its two lines of text (a
/// `Label` line is 1.25 rem), 40 px at 100%. The row grows with its content,
/// so the text scale never makes rows overlap.
const ROW_MIN_HEIGHT_REMS: f32 = 2.5;
const ACCENT_BAR_WIDTH: f32 = 3.;
const ROW_DOT: f32 = 8.;
/// A compact marker's box: a 2 px selection ring, 2 px of space, then the
/// 32 px color square.
const MARKER_SIZE: f32 = 40.;
const MARKER_INSET: f32 = 2.;
/// A marker's status dot, including its 2 px ring in the pane's surface color
/// that sets it off from the agent color.
const MARKER_DOT: f32 = 12.;
const MARKER_DOT_GAP: f32 = 2.;
/// The opacity of a dimmed (quiet) dot.
const DIMMED: f32 = 0.5;

/// Registers the pane's key bindings: Up and Down move the selection.
/// Enter and Space need no binding: they activate the focused row or marker
/// (a GPUI keyboard click), Enter to open and Space to select.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPreviousAgent, Some(KEY_CONTEXT)),
        KeyBinding::new("down", SelectNextAgent, Some(KEY_CONTEXT)),
    ]);
}

/// Element id of an agent's expanded row.
pub fn agent_row_id(id: AgentId) -> ElementId {
    ElementId::Name(format!("agent-row-{id}").into())
}

/// Element id of an agent's compact marker.
pub fn agent_marker_id(id: AgentId) -> ElementId {
    ElementId::Name(format!("agent-marker-{id}").into())
}

/// Element id of the repository picker's entry `ix` (in the picker's order).
pub fn repo_choice_id(ix: usize) -> ElementId {
    ("repo-choice", ix).into()
}

/// The agent a successful create returned, until the daemon's event adds it.
#[derive(Debug, Clone, Copy)]
struct AwaitedAgent {
    id: AgentId,
    /// The selection when the create started: if it changed by the time the
    /// agent appears, the user chose another agent and keeps it.
    selected_before: Option<AgentId>,
}

/// The pane's notifications replace each other rather than stack.
struct AgentsNotice;

pub struct AgentsPane {
    state: Entity<ShellState>,
    /// The pane's own focus, tracked on its root in both modes.
    focus: FocusHandle,
    /// One focus handle per agent, shared by its row and its marker.
    item_focus: HashMap<AgentId, FocusHandle>,
    /// The list's scroll position, so the keyboard keeps the selection in
    /// view.
    scroll: ScrollHandle,
    /// The create in flight, if any; the New agent button is disabled while
    /// it runs. Dropping it gives the create up.
    create: Option<Task<()>>,
    awaiting: Option<AwaitedAgent>,
    /// Closes in flight, so a second request for the same agent sends
    /// nothing. Dropping one gives it up.
    closing: HashMap<AgentId, Task<()>>,
    /// The connection as last seen, to notice it dropping.
    connection: Connection,
    _observe: Subscription,
}

impl AgentsPane {
    pub fn new(state: Entity<ShellState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            connection: state.read(cx).model().connection(),
            _observe: cx.observe_in(&state, window, |this, _, window, cx| {
                this.on_state_changed(window, cx)
            }),
            state,
            focus: cx.focus_handle(),
            item_focus: HashMap::new(),
            scroll: ScrollHandle::new(),
            create: None,
            awaiting: None,
            closing: HashMap::new(),
        }
    }

    fn on_state_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let connection = self.state.read(cx).model().connection();
        let dropped =
            self.connection == Connection::Connected && connection != Connection::Connected;
        self.connection = connection;
        if dropped {
            self.give_up_commands(window, cx);
        }
        self.select_awaited(cx);
        cx.notify();
    }

    /// The connection dropped while commands waited: their answers may never
    /// come (the daemon may still have done the work, which then arrives with
    /// the next snapshot), so stop waiting and say so.
    fn give_up_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = if self.create.take().is_some() {
            Some("Could not create the agent")
        } else if !self.closing.is_empty() {
            Some("Could not close the agent")
        } else {
            None
        };
        self.closing.clear();
        if let Some(title) = title {
            tracing::warn!("the connection dropped while a command waited");
            notify_unreachable(title, DAEMON_SILENT_TEXT, window, cx);
        }
    }

    /// Whether a create is in flight.
    fn creating(&self) -> bool {
        self.create.is_some()
    }

    fn dispatch(&self, action: Action, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.dispatch(action, cx));
    }

    /// A click or Space on the focused item selects; the second click of a
    /// double-click, or Enter on the focused item, opens the agent's tab.
    fn on_item_click(&mut self, id: AgentId, event: &ClickEvent, cx: &mut Context<Self>) {
        let opens = match event {
            ClickEvent::Keyboard(KeyboardClickEvent { button, .. }) => {
                *button == KeyboardButton::Enter
            }
            _ => event.click_count() >= 2,
        };
        if opens {
            tracing::debug!(agent = %id, keyboard = event.is_keyboard(), "open agent tab");
            self.dispatch(Action::OpenTab(id), cx);
        } else {
            self.dispatch(Action::SelectAgent(id), cx);
        }
    }

    fn select_previous(
        &mut self,
        _: &SelectPreviousAgent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(false, window, cx);
    }

    fn select_next(&mut self, _: &SelectNextAgent, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(true, window, cx);
    }

    /// Moves the selection one agent down (or up), stopping at the ends,
    /// focuses the newly selected item so Enter opens it, and scrolls it into
    /// view.
    fn move_selection(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (ids, selected) = {
            let model = self.state.read(cx).model();
            let ids: Vec<AgentId> = model.agents().iter().map(|agent| agent.id).collect();
            (ids, model.selected())
        };
        let Some(last) = ids.len().checked_sub(1) else {
            return;
        };
        let current = selected.and_then(|selected| ids.iter().position(|id| *id == selected));
        let next = match (current, down) {
            (Some(ix), true) => (ix + 1).min(last),
            (Some(ix), false) => ix.saturating_sub(1),
            (None, true) => 0,
            (None, false) => last,
        };
        let id = ids[next];
        self.dispatch(Action::SelectAgent(id), cx);
        // The list's children are the rows (or markers), in this order.
        self.scroll.scroll_to_item(next);
        if let Some(focus) = self.item_focus.get(&id) {
            focus.focus(window, cx);
        }
    }

    /// Keeps one focus handle per open agent.
    fn sync_item_focus(&mut self, rows: &[AgentRowVm], cx: &mut Context<Self>) {
        self.item_focus
            .retain(|id, _| rows.iter().any(|row| row.id == *id));
        for row in rows {
            self.item_focus
                .entry(row.id)
                .or_insert_with(|| cx.focus_handle().tab_stop(true));
        }
    }

    // ---- Drawing ----------------------------------------------------------

    fn expanded_row(&self, row: AgentRowVm, paint: &Paint, cx: &Context<Self>) -> impl IntoElement {
        let id = row.id;
        let selected = row.selected;
        let (row_bg, name_color) = if selected {
            (paint.selected, paint.selected_foreground)
        } else {
            (paint.surface, paint.foreground)
        };
        let ring = paint.ring;
        let hover = paint.hover;
        h_flex()
            .id(agent_row_id(id))
            .role(Role::ListBoxOption)
            .test_support()
            .track_focus(&self.item_focus[&id])
            .aria_label(row.a11y_label.clone())
            .aria_selected(selected)
            .when(row.has_open_tab, |this| this.aria_description("Chat open"))
            .flex_none()
            .w_full()
            .min_h(rems(ROW_MIN_HEIGHT_REMS))
            .items_center()
            .gap_2()
            .pr_2()
            .bg(row_bg)
            .border_1()
            .border_color(row_bg)
            .when(!selected, |this| this.hover(move |style| style.bg(hover)))
            .focus_visible(move |style| style.border_color(ring))
            .on_click(cx.listener(move |this, event, _, cx| this.on_item_click(id, event, cx)))
            .child(
                div()
                    .id(ACCENT_BAR_ID)
                    .test_support()
                    .flex_none()
                    .w(px(ACCENT_BAR_WIDTH))
                    // The row's height comes from its text, so stretch.
                    .self_stretch()
                    .when_some(paint.accent(row.accent_hue), |this, accent| this.bg(accent)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        Label::new(row.name.clone())
                            .text_sm()
                            .text_color(name_color)
                            .truncate(),
                    )
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1p5()
                            .items_center()
                            .child({
                                let (style, color) = paint.dot(&row);
                                status_dot(style, ROW_DOT, color)
                                    .id(STATUS_DOT_ID)
                                    .test_support()
                            })
                            .child(
                                Label::new(row.state_label)
                                    .text_xs()
                                    .text_color(paint.muted_foreground)
                                    .truncate(),
                            ),
                    ),
            )
            .context_menu(self.close_menu(id, row.name, cx))
    }

    fn compact_marker(
        &self,
        row: AgentRowVm,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let id = row.id;
        let (dot_style, dot_color) = paint.dot(&row);
        let label: SharedString = row.a11y_label.clone().into();
        let tooltip = label.clone();
        let focus_ring = paint.foreground;
        div()
            .id(agent_marker_id(id))
            .role(Role::ListBoxOption)
            .test_support()
            .track_focus(&self.item_focus[&id])
            .aria_label(label)
            .aria_selected(row.selected)
            .when(row.has_open_tab, |this| this.aria_description("Chat open"))
            .tooltip(move |window, cx| {
                let text = tooltip.clone();
                Tooltip::element(move |_, _| Label::new(text.clone())).build(window, cx)
            })
            .relative()
            .flex_none()
            .size(px(MARKER_SIZE))
            .p(px(MARKER_INSET))
            .rounded(paint.radius + px(MARKER_INSET * 2.))
            .border_2()
            .border_color(if row.selected {
                paint.ring
            } else {
                paint.surface
            })
            .focus_visible(move |style| style.border_color(focus_ring))
            .on_click(cx.listener(move |this, event, _, cx| this.on_item_click(id, event, cx)))
            .child(
                div()
                    .id(SWATCH_ID)
                    .test_support()
                    .size_full()
                    .rounded(paint.radius)
                    .flex()
                    .items_center()
                    .justify_center()
                    .when_some(paint.accent(row.accent_hue), |this, accent| this.bg(accent))
                    .child(
                        Label::new(row.initial.clone())
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(paint.initial_color(row.accent_hue)),
                    ),
            )
            .child(
                // A ring in the pane's surface sets the dot off from the
                // agent color.
                div()
                    .absolute()
                    .right_0()
                    .bottom_0()
                    .size(px(MARKER_DOT))
                    .p(px(MARKER_DOT_GAP))
                    .rounded_full()
                    .bg(paint.surface)
                    .child(
                        status_dot(dot_style, MARKER_DOT - 2. * MARKER_DOT_GAP, dot_color)
                            .id(STATUS_DOT_ID)
                            .test_support(),
                    ),
            )
            .context_menu(self.close_menu(id, row.name, cx))
    }
}

impl Focusable for AgentsPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AgentsPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (mode, rows, plan) = {
            let state = self.state.read(cx);
            let model = state.model();
            let plan = new_agent_plan(model.repos(), state.last_repo_id());
            let choices = match &plan {
                NewAgentPlan::Pick { repos, preselected } => {
                    repo_choices(model.repos(), repos, *preselected)
                }
                _ => Vec::new(),
            };
            (state.left_pane_mode(), agent_rows(model), (plan, choices))
        };
        self.sync_item_focus(&rows, cx);
        let paint = Paint::new(self.state.read(cx).shared_native_theme(), cx);
        let compact = mode == LeftPaneMode::Compact;
        let new_agent = self.new_agent_button(compact, plan, &paint, cx);
        let list = v_flex()
            .id(AGENT_LIST_ID)
            .role(Role::ListBox)
            .test_support()
            .aria_label("Agents")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll);

        // The focus is tracked on the root in both modes, so switching modes
        // never drops a focused pane out of the tree.
        v_flex()
            .id(LEFT_PANE_CONTENT_ID)
            // Observed before `track_focus`, so tests can read its focus.
            .test_support()
            .track_focus(&self.focus)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .size_full()
            .bg(paint.surface)
            .text_color(paint.foreground)
            .map(|this| match mode {
                LeftPaneMode::Expanded => this
                    .child(
                        h_flex()
                            .flex_none()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .pl_3()
                            .pr_1()
                            .py_2()
                            .child(
                                div()
                                    .id(EXPANDED_TITLE_ID)
                                    // A node needs an id and a role to reach
                                    // assistive technology.
                                    .role(Role::Heading)
                                    .test_support()
                                    .aria_label("Agents")
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Agents"),
                            )
                            .child(new_agent),
                    )
                    .child(if rows.is_empty() {
                        // A `Label` is in the accessibility tree; plain text
                        // is not.
                        Label::new(NO_AGENTS_TEXT)
                            .id(AGENTS_EMPTY_ID)
                            .px_3()
                            .text_xs()
                            .text_color(paint.muted_foreground)
                            .into_any_element()
                    } else {
                        list.gap_0p5()
                            .children(
                                rows.into_iter()
                                    .map(|row| self.expanded_row(row, &paint, cx)),
                            )
                            .into_any_element()
                    }),
                LeftPaneMode::Compact => this.items_center().gap_2().pt_2().child(new_agent).child(
                    list.items_center().gap_1p5().pb_2().children(
                        rows.into_iter()
                            .map(|row| self.compact_marker(row, &paint, cx)),
                    ),
                ),
            })
    }
}

/// A status dot of `size` px in `style` ([`state_dot`]): a filled disc, or a
/// hollow 2 px ring; dimmed at half opacity.
fn status_dot(style: DotStyle, size: f32, color: Hsla) -> Div {
    div()
        .flex_none()
        .size(px(size))
        .rounded_full()
        .map(|this| {
            if style.filled {
                this.bg(color)
            } else {
                this.border_2().border_color(color)
            }
        })
        .when(style.dimmed, |this| this.opacity(DIMMED))
}

/// The colors one frame of the pane paints with, all from the theme.
struct Paint {
    /// Shared with the state: no copy of the theme per frame.
    native: Option<Rc<NativeTheme>>,
    surface: Hsla,
    foreground: Hsla,
    muted_foreground: Hsla,
    /// A selected row's background and text.
    selected: Hsla,
    selected_foreground: Hsla,
    hover: Hsla,
    /// The selection ring of a marker and the focus ring of a row.
    ring: Hsla,
    radius: Pixels,
}

impl Paint {
    fn new(native: Option<Rc<NativeTheme>>, cx: &App) -> Self {
        let (surface, foreground) = region_paint(native.as_deref(), Region::LeftPane, cx);
        let token = |name: &str| token_paint(native.as_deref(), name, cx);
        Self {
            surface,
            foreground,
            muted_foreground: token("muted-foreground"),
            selected: token("accent"),
            selected_foreground: token("accent-foreground"),
            hover: token("muted"),
            ring: token("ring"),
            radius: cx.theme().radius,
            native,
        }
    }

    /// The agent color for `hue`; `None` only without a native theme.
    fn accent(&self, hue: f32) -> Option<Hsla> {
        self.native
            .as_ref()
            .map(|native| srgb_to_hsla(agent_accent(native, hue)))
    }

    /// The text color of an initial drawn on the agent color for `hue`: the
    /// more readable of two theme tokens ([`initial_text_color`]).
    fn initial_color(&self, hue: f32) -> Hsla {
        self.native
            .as_ref()
            .and_then(|native| initial_text_color(native, agent_accent(native, hue)))
            .map_or(self.foreground, srgb_to_hsla)
    }

    /// A row's status dot: its shape ([`state_dot`]) and its token's color.
    fn dot(&self, row: &AgentRowVm) -> (DotStyle, Hsla) {
        let style = state_dot(row.state_kind);
        let color = self
            .native
            .as_ref()
            .and_then(|native| token_color(native, style.token))
            .unwrap_or(self.muted_foreground);
        (style, color)
    }
}

/// A failed command: a notification, and the status bar's error message
/// "<title>: <text>".
fn notify_failure(
    state: &Entity<ShellState>,
    title: &'static str,
    text: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) {
    let text: SharedString = text.into();
    state.update(cx, |state, cx| {
        state.set_status(StatusMessage::error(format!("{title}: {text}")), cx)
    });
    notify(NotificationType::Error, Some(title), text, window, cx);
}

/// A failure caused by the connection (not connected, the link stopped, the
/// daemon silent): a notification only. The status bar already says what the
/// connection is doing, and an error left there would outlive it.
fn notify_unreachable(
    title: &'static str,
    text: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) {
    notify(NotificationType::Error, Some(title), text, window, cx);
}

/// A command's [`LinkError`]: an unreachable daemon is a notification only,
/// anything the daemon answered is a failure with a status.
fn notify_link_error(
    state: &Entity<ShellState>,
    title: &'static str,
    error: &LinkError,
    window: &mut Window,
    cx: &mut App,
) {
    let text = command_error_text(error);
    match error {
        LinkError::NotConnected | LinkError::Stopped => notify_unreachable(title, text, window, cx),
        LinkError::Client(_) => notify_failure(state, title, text, window, cx),
    }
}

/// Shows a transient notification whose text is a [`Label`] with id
/// [`NOTICE_ID`] (so it is readable by assistive technology and by tests). A
/// newer one replaces an older one.
fn notify(
    kind: NotificationType,
    title: Option<&'static str>,
    text: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) {
    let text: SharedString = text.into();
    tracing::debug!(%text, "agents notice");
    window.push_notification(
        Notification::new()
            .id::<AgentsNotice>()
            .with_type(kind)
            .when_some(title, |note, title| note.title(title))
            .content(move |_, _, cx| {
                Label::new(text.clone())
                    .id(NOTICE_ID)
                    .text_sm()
                    .text_color(cx.theme().popover_foreground)
                    .into_any_element()
            }),
        cx,
    );
}

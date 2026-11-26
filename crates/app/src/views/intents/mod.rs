//! The Intents view in the right dock. It shares the dock with Settings and
//! is drawn only while the rail has it selected
//! ([`ShellState::right_entry`]).
//!
//! The pane shows one [`Screen`] at a time: the list ([`list`]), the create
//! screen ([`create`]) or an intent's detail ([`detail`]). The filter, and
//! the create screen's text, live in the pane, so they reset when the app
//! restarts.
//!
//! Every daemon command goes through [`IntentsPane::run`]: one at a time, so
//! while one is in flight the others do nothing. Nothing changes on screen
//! until the daemon's answer or event reaches the model. A command the daemon
//! does not answer within the shell's command timeout, or that is still
//! waiting when the connection drops, is given up with "The daemon did not
//! answer". A failed command shows the screen's one error line, a status
//! ([`INTENT_ERROR_ID`]); opening a screen or running the next command
//! clears it. Leaving a screen does not give up its command, which the
//! daemon may already be acting on: it runs to its end and frees the
//! controls, but its failure shows only on the screen it was sent from. A
//! detail whose intent the model no longer has (a reload after a reconnect)
//! goes back to the list. The list is redrawn every minute so its ages
//! advance.
//!
//! The pane paints its whole area from `Region::RightPane` (`card`,
//! `card-foreground`), like Settings; everything else comes from tokens and
//! the agent accents.

mod create;
mod detail;
mod list;

pub use create::{
    create_repo_item_id, kind_row_id, ChooseBug, ChooseEnhancement, ChooseMaintenance,
    ChooseNewFeature, ChooseRefactor, CREATE_CLOSE_ID, CREATE_INPUT_ID, CREATE_KEY_CONTEXT,
    CREATE_REPO_ID, CREATE_SCREEN_ID, CREATE_TITLE_ID, KIND_DESCRIPTION_ID, KIND_SHORTCUT_ID,
    KIND_TITLE_ID,
};
pub use detail::{
    kind_item_id, DETAIL_BACK_ID, DETAIL_BODY_ID, DETAIL_KIND_ID, DETAIL_SCROLL_ID, DETAIL_SLUG_ID,
    DETAIL_STATE_ID, DETAIL_TITLE_ID, DETAIL_TITLE_INPUT_ID, POLISHING_ID, POLISH_FAILED_ID,
    RETRY_ID,
};

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::v_flex;
use gpui_kit::{
    div, AnyElement, App, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Role, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    TestSupportExt as _, Window,
};
use smithagents_core::ids::{IntentId, ItemId, QuestionId, RepoId};
use smithagents_core::intent::{Intent, IntentKind};
use smithagents_theme::native::NativeTheme;

use crate::intent_vm::{header_editable, intent_error_text, IntentFilterChoice};
use crate::link::{IntentAnswer, IntentCommands, LinkError};
use crate::model::Connection;
use crate::shell::INTENTS_PANEL;
use crate::shell_state::ShellState;
use crate::theme_adapter::{
    agent_accent, region_paint, srgb_to_hsla, token_color, token_paint, Region,
};
use crate::view_models::{DAEMON_SILENT_TEXT, NOT_CONNECTED_TEXT};
use crate::views::answered;
use crate::views::rail::RailEntry;

/// Key context of the pane, for its key bindings.
pub const KEY_CONTEXT: &str = "IntentsPane";
/// Element id of the list's heading.
pub const INTENTS_TITLE_ID: &str = "intents-title";
/// Element id of the filter button (it opens the filter menu).
pub const INTENTS_FILTER_ID: &str = "intents-filter";
/// Element id of the New intent button.
pub const INTENTS_NEW_ID: &str = "intents-new";
/// Element id of the list of cards.
pub const INTENT_LIST_ID: &str = "intent-list";
/// Element id of what the list says instead of cards.
pub const INTENTS_EMPTY_ID: &str = "intents-empty";
/// Element id of the screen's one error line (a status).
pub const INTENT_ERROR_ID: &str = "intent-error";
/// Element ids of a card's parts, inside the card ([`intent_card_id`]).
pub const CARD_DOT_ID: &str = "dot";
pub const CARD_ACCENT_ID: &str = "accent";
pub const CARD_SLUG_ID: &str = "slug";
pub const CARD_TITLE_ID: &str = "title";
pub const CARD_META_ID: &str = "meta";
pub const CARD_HEADING_ID: &str = "heading";
pub const CARD_LINE_ID: &str = "line";
const TITLE: &str = "Intents";

/// How often the list is redrawn, so its ages ("5m") advance.
const AGE_REFRESH: Duration = Duration::from_secs(60);

/// Element id of an intent's card.
pub fn intent_card_id(id: IntentId) -> ElementId {
    ElementId::Name(format!("intent-card-{id}").into())
}

/// Element id of the filter menu's entry for `choice`.
pub fn filter_item_id(choice: IntentFilterChoice) -> ElementId {
    ElementId::Name(format!("intents-filter-{}", choice.as_str()).into())
}

/// Registers the pane's key bindings: the create screen's ⌥ keys and Escape
/// ([`create`]). The list needs none: Enter and Space activate the focused
/// card (a GPUI keyboard click).
pub fn init(cx: &mut App) {
    cx.bind_keys(create::key_bindings());
}

/// What the pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    List,
    Create,
    Detail(IntentId),
}

/// Which command is in flight: the control that sent it shows the spinner.
/// Public, like [`IntentsPane::busy`] and [`IntentsPane::run`], for the
/// headless tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Busy {
    Create,
    Title,
    Kind,
    Retry,
    Question(QuestionId),
    Item(ItemId),
    Send,
    Resolve,
}

pub struct IntentsPane {
    state: Entity<ShellState>,
    focus: FocusHandle,
    screen: Screen,
    /// The detail's intent was in the model since the detail opened: if it
    /// leaves (a reload without it), the pane goes back to the list. One
    /// just created may arrive after its detail opens.
    detail_seen: bool,
    filter: IntentFilterChoice,
    /// The screen's error line.
    error: Option<String>,
    /// The command in flight, and its task; dropping the task gives it up.
    busy: Option<Busy>,
    command: Option<Task<()>>,
    /// The screen the command in flight was sent from: only there does its
    /// failure show.
    command_screen: Option<Screen>,
    /// The connection as last seen, to notice it dropping.
    connection: Connection,
    /// One focus handle per card drawn, so each card is a tab stop.
    card_focus: HashMap<IntentId, FocusHandle>,
    /// The list's scroll position.
    scroll: ScrollHandle,
    /// The create screen's text, kept between visits.
    text_box: Entity<TextareaState>,
    /// The repository picked in the create screen's menu.
    picked_repo: Option<RepoId>,
    /// The kind of the create in flight, whose row shows the spinner.
    creating_kind: Option<IntentKind>,
    /// The detail's scroll position; a new one for each detail opened.
    detail_scroll: ScrollHandle,
    /// The detail's title as a button (a tab stop), and the input that
    /// replaces it while the title of `editing_title` is edited.
    title_focus: FocusHandle,
    title_input: Entity<InputState>,
    editing_title: Option<IntentId>,
    _age_refresh: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl IntentsPane {
    pub fn new(state: Entity<ShellState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let text_box = create::new_text_box(window, cx);
        let title_input = detail::new_title_input(window, cx);
        let subscriptions = vec![
            cx.observe_in(&state, window, |this, _, window, cx| {
                this.on_state_changed(window, cx)
            }),
            // The kind rows follow whether the text is blank.
            cx.subscribe_in(&text_box, window, |_, _, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &title_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.save_title(window, cx);
                    }
                },
            ),
            // The shell focuses the pane when it shows it: on the create
            // screen the text box takes that focus, as when the screen opens.
            cx.on_focus(&focus, window, |this, window, cx| {
                if this.screen == Screen::Create {
                    this.text_box
                        .update(cx, |text_box, cx| text_box.focus(window, cx));
                }
            }),
        ];
        Self {
            connection: state.read(cx).model().connection(),
            state,
            focus,
            screen: Screen::List,
            detail_seen: false,
            filter: IntentFilterChoice::default(),
            error: None,
            busy: None,
            command: None,
            command_screen: None,
            card_focus: HashMap::new(),
            scroll: ScrollHandle::new(),
            text_box,
            picked_repo: None,
            creating_kind: None,
            detail_scroll: ScrollHandle::new(),
            title_focus: cx.focus_handle().tab_stop(true),
            title_input,
            editing_title: None,
            _age_refresh: refresh_ages(cx),
            _subscriptions: subscriptions,
        }
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn filter(&self) -> IntentFilterChoice {
        self.filter
    }

    /// The command in flight, if any.
    pub fn busy(&self) -> Option<Busy> {
        self.busy
    }

    /// Shows `screen` and clears the error line. The create screen's text box
    /// takes the focus. Otherwise a keyboard user stays in the pane: back on
    /// the list the focus goes to the card the detail was opened from,
    /// elsewhere to the pane.
    pub(crate) fn open_screen(
        &mut self,
        screen: Screen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!(?screen, "intents screen");
        let previous = std::mem::replace(&mut self.screen, screen);
        self.error = None;
        self.editing_title = None;
        self.detail_seen = match screen {
            Screen::Detail(id) => self.state.read(cx).model().intent(id).is_some(),
            Screen::List | Screen::Create => false,
        };
        if screen != previous && matches!(screen, Screen::Detail(_)) {
            self.detail_scroll = ScrollHandle::new();
        }
        if screen == Screen::Create {
            self.text_box
                .update(cx, |text_box, cx| text_box.focus(window, cx));
        } else if self.focus.contains_focused(window, cx) {
            let card = match (previous, screen) {
                (Screen::Detail(id), Screen::List) => self.card_focus.get(&id),
                _ => None,
            };
            card.unwrap_or(&self.focus).focus(window, cx);
        }
        cx.notify();
    }

    /// Sends one command. Does nothing while another is in flight: `command`
    /// is dropped unpolled, so it is never sent. Clears the error line, marks
    /// `key` busy and waits for the answer at most the shell's command
    /// timeout; then clears busy and either calls `done` with the answer or
    /// sets the error line (`intent_error_text`, or "The daemon did not
    /// answer" on a timeout) if the pane still shows the screen the command
    /// was sent from. Opening another screen meanwhile does not give the
    /// command up. The model learns of the answer from the link, not from
    /// here.
    pub fn run(
        &mut self,
        key: Busy,
        command: IntentAnswer,
        done: impl FnOnce(&mut Self, Intent, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(busy) = self.busy {
            tracing::debug!(?key, ?busy, "an intent command is already in flight");
            return;
        }
        tracing::debug!(?key, "sending an intent command");
        self.error = None;
        self.busy = Some(key);
        self.command_screen = Some(self.screen);
        let timer = cx
            .background_executor()
            .timer(self.state.read(cx).command_timeout());
        self.command = Some(cx.spawn_in(window, async move |this, cx| {
            let answer = answered(command, timer).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish(key, answer, done, window, cx)
            });
        }));
        cx.notify();
    }

    /// `answer` is `None` when the daemon did not answer in time.
    fn finish(
        &mut self,
        key: Busy,
        answer: Option<Result<Intent, LinkError>>,
        done: impl FnOnce(&mut Self, Intent, &mut Window, &mut Context<Self>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = None;
        self.command = None;
        let sent_from = self.command_screen.take();
        match answer {
            None => {
                tracing::warn!(?key, "the daemon did not answer an intent command in time");
                self.show_failure(sent_from, DAEMON_SILENT_TEXT.to_owned());
            }
            Some(Ok(intent)) => {
                tracing::debug!(?key, intent = %intent.id, "intent command answered");
                done(self, intent, window, cx);
            }
            Some(Err(e)) => {
                tracing::warn!(?key, "intent command failed: {e}");
                self.show_failure(sent_from, intent_error_text(&e));
            }
        }
        cx.notify();
    }

    /// Sends the command `make` builds through [`Self::run`]. While another
    /// command is in flight it does nothing (`make` is not called, so nothing
    /// is sent); without a link the error line says so.
    fn command(
        &mut self,
        key: Busy,
        make: impl FnOnce(&dyn IntentCommands) -> IntentAnswer,
        done: impl FnOnce(&mut Self, Intent, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(busy) = self.busy {
            tracing::debug!(?key, ?busy, "an intent command is already in flight");
            return;
        }
        let Some(command) = self.state.read(cx).intent_commands().map(make) else {
            self.error = Some(NOT_CONNECTED_TEXT.to_owned());
            cx.notify();
            return;
        };
        self.run(key, command, done, window, cx);
    }

    /// Puts a command's failure on the error line if the pane still shows
    /// `sent_from`, the screen the command was sent from; a failure never
    /// lands on another screen.
    fn show_failure(&mut self, sent_from: Option<Screen>, text: String) {
        if sent_from == Some(self.screen) {
            self.error = Some(text);
        } else {
            tracing::debug!(
                ?sent_from,
                screen = ?self.screen,
                "a command failed after its screen was left"
            );
        }
    }

    /// Stops waiting for the command in flight and says so (on the screen
    /// that sent it). The daemon may still finish it; its change then
    /// arrives through the events.
    fn give_up_command(&mut self) {
        tracing::warn!(busy = ?self.busy, "the connection dropped while an intent command waited");
        self.command = None;
        self.busy = None;
        let sent_from = self.command_screen.take();
        self.show_failure(sent_from, DAEMON_SILENT_TEXT.to_owned());
    }

    fn on_state_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (connection, detail_present, title_editable) = {
            let model = self.state.read(cx).model();
            let present = match self.screen {
                Screen::Detail(id) => Some(model.intent(id).is_some()),
                Screen::List | Screen::Create => None,
            };
            let editable = self
                .editing_title
                .map(|id| model.intent(id).is_some_and(header_editable));
            (model.connection(), present, editable)
        };
        // The intent is being polished, or was sent, while its title input
        // was open: the input goes, and a keyboard user stays in the pane.
        if title_editable == Some(false) {
            self.editing_title = None;
            if self.title_input.focus_handle(cx).is_focused(window) {
                self.focus.focus(window, cx);
            }
        }
        let dropped =
            self.connection == Connection::Connected && connection != Connection::Connected;
        self.connection = connection;
        // The answer may never come.
        if dropped && self.busy.is_some() {
            self.give_up_command();
        }
        match detail_present {
            Some(true) => self.detail_seen = true,
            Some(false) if self.detail_seen => {
                tracing::debug!("the open intent is gone; back to the list");
                self.open_screen(Screen::List, window, cx);
            }
            _ => {}
        }
        // The dock draws the pane from a cache: the model and the theme reach
        // it through the state.
        cx.notify();
    }

    /// The screen's error line, a status that announces itself, if a command
    /// failed.
    pub(crate) fn error_line(&self, paint: &Paint) -> Option<AnyElement> {
        let text = self.error.clone()?;
        Some(
            div()
                .id(INTENT_ERROR_ID)
                .role(Role::Status)
                .test_support()
                .aria_label(text.clone())
                .flex_none()
                .px_3()
                .pb_2()
                .text_xs()
                .text_color(paint.destructive)
                .child(text)
                .into_any_element(),
        )
    }
}

/// Redraws the pane every [`AGE_REFRESH`] while it exists.
fn refresh_ages(cx: &mut Context<IntentsPane>) -> Task<()> {
    cx.spawn(async move |pane, cx| loop {
        cx.background_executor().timer(AGE_REFRESH).await;
        if pane.update(cx, |_, cx| cx.notify()).is_err() {
            break;
        }
    })
}

impl BasePanel for IntentsPane {
    fn panel_name(&self) -> &'static str {
        INTENTS_PANEL
    }

    /// Drawn only while the rail has Intents selected, so the dock's group
    /// has one visible panel and draws no tab bar.
    fn visible(&self, cx: &App) -> bool {
        self.state.read(cx).right_entry() == RailEntry::Intents
    }

    /// The dock is locked to the shell's layout; its panels never close.
    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for IntentsPane {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(TITLE.into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        TITLE
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }

    /// The pane draws its own heading.
    fn title_bar(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for IntentsPane {}

impl Focusable for IntentsPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for IntentsPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = Paint::new(self.state.read(cx), cx);
        let content = match self.screen {
            Screen::List => self.list_screen(&paint, cx).into_any_element(),
            Screen::Create => self.create_screen(&paint, cx).into_any_element(),
            Screen::Detail(id) => self.detail_screen(id, &paint, cx),
        };
        v_flex()
            .id(INTENTS_PANEL)
            .test_support()
            .track_focus(&self.focus)
            .key_context(KEY_CONTEXT)
            .size_full()
            .bg(paint.surface)
            .text_color(paint.foreground)
            .child(content)
    }
}

/// The colors one frame of the pane paints with, all from the theme.
pub(crate) struct Paint {
    /// Shared with the state: no copy of the theme per frame.
    native: Option<Rc<NativeTheme>>,
    surface: Hsla,
    foreground: Hsla,
    muted_foreground: Hsla,
    /// The error line.
    destructive: Hsla,
    /// A card's outline, and its bar without an agent.
    border: Hsla,
    /// A card under the pointer.
    hover: Hsla,
    /// The keyboard focus ring.
    ring: Hsla,
}

impl Paint {
    fn new(state: &ShellState, cx: &App) -> Self {
        let native = state.shared_native_theme();
        let (surface, foreground) = region_paint(native.as_deref(), Region::RightPane, cx);
        let token = |name: &str| token_paint(native.as_deref(), name, cx);
        Self {
            surface,
            foreground,
            muted_foreground: token("muted-foreground"),
            destructive: token("destructive"),
            border: token("border"),
            hover: token("muted"),
            ring: token("ring"),
            native,
        }
    }

    /// A token's color (a status dot's `status-ok`, `status-warn` and so
    /// on); the foreground without a native theme.
    pub(crate) fn token(&self, token: &str) -> Hsla {
        self.native
            .as_deref()
            .and_then(|native| token_color(native, token))
            .unwrap_or(self.foreground)
    }

    /// The agent color for `hue`; `None` only without a native theme.
    fn accent(&self, hue: f32) -> Option<Hsla> {
        self.native
            .as_deref()
            .map(|native| srgb_to_hsla(agent_accent(native, hue)))
    }
}

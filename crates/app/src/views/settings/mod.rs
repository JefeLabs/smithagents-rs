//! The Settings view in the right dock (drawn while the rail has it
//! selected; Intents shares the dock): a scrollable pane of collapsible
//! sections, both expanded until the user collapses one (remembered in
//! `app-state.json`):
//!
//! - **Appearance** (app scope, [`appearance`]): the theme ("System" or a
//!   theme) and the font as radio groups (Up/Down or Left/Right move to and
//!   select the adjacent option, wrapping), and the text size as a stepper
//!   over the token set's scale. A change applies live and is saved to
//!   `app-settings.json`.
//! - **Agent defaults** (Space scope, [`agent_defaults`]): "Agents working at
//!   once", a menu of "No limit" and 1 to 16. The daemon's answer, not the
//!   press, changes the value shown.
//! - **Source repositories** (Space scope, [`repositories`]): the daemon's
//!   repositories and an "Add repository" field for a local path.
//!
//! Each section is a labeled group; its header is a button that exposes
//! whether it is expanded. The New agent button, when there is no repository,
//! asks for Source repositories ([`ShellEvent::FocusSettings`]): the section
//! is expanded and scrolled toward the top of the pane, as far as the content
//! below it allows (it is the last section, so in a short pane it ends at the
//! bottom, fully in view), and its path field takes the focus. No other
//! section is ever asked for.
//!
//! The pane paints its whole area from `Region::RightPane` (`card`,
//! `card-foreground`); muted text, errors and borders come from tokens.

mod agent_defaults;
mod appearance;
mod repositories;

use std::collections::HashSet;
use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{v_flex, ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, AnyElement, App, AppContext as _, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    Role, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Task, TestSupportExt as _, Window,
};
use smithagents_core::ids::RepoId;

use crate::model::Connection;
use crate::shell::SETTINGS_PANEL;
use crate::shell_state::{ShellEvent, ShellState};
use crate::theme_adapter::{region_paint, token_paint, Region};
use crate::view_models::SettingsSection;
use crate::views::pane_heading;
use crate::views::rail::RailEntry;

pub use agent_defaults::{
    queue_limit_item_id, QUEUE_LIMIT_ERROR_ID, QUEUE_LIMIT_HELP, QUEUE_LIMIT_HELP_ID,
    QUEUE_LIMIT_ID, QUEUE_LIMIT_LABEL, QUEUE_LIMIT_LABEL_ID,
};
pub use appearance::{
    init, SelectNextOption, SelectPreviousOption, FONT_LABEL, FONT_OPTIONS_ID, RADIO_KEY_CONTEXT,
    TEXT_SIZE_DOWN_ID, TEXT_SIZE_ID, TEXT_SIZE_LABEL, TEXT_SIZE_UP_ID, TEXT_SIZE_VALUE_ID,
    THEME_LABEL, THEME_OPTIONS_ID,
};
pub use repositories::{
    repo_row_id, ADD_REPO_ID, BROWSE_REPO_ID, REPOS_EMPTY_ID, REPO_LIST_ID, REPO_PATH_ERROR_ID,
    REPO_PATH_INPUT_ID, REPO_PATH_LABEL,
};

/// Element id of the pane's heading.
pub const SETTINGS_TITLE_ID: &str = "settings-title";
/// Element id of the scrolling container that holds the sections.
pub const SETTINGS_SCROLL_ID: &str = "settings-scroll";

/// Element id of a section (a labeled group).
pub fn section_id(section: SettingsSection) -> ElementId {
    ElementId::Name(format!("settings-section-{}", section.as_str()).into())
}

/// Element id of a section's header button (expands and collapses it).
pub fn section_header_id(section: SettingsSection) -> ElementId {
    ElementId::Name(format!("settings-header-{}", section.as_str()).into())
}

pub struct SettingsPane {
    state: Entity<ShellState>,
    focus: FocusHandle,
    /// The sections' scroll position (each section is one child).
    scroll: ScrollHandle,
    /// The "Add repository" path field.
    path_input: Entity<InputState>,
    /// The line under the path field: why the last add was refused or
    /// failed. Cleared when the text changes.
    path_error: Option<String>,
    /// The add in flight, if any; the field and its buttons are disabled
    /// while it runs. Dropping it gives the add up.
    adding: Option<Task<()>>,
    /// What the add in flight was sent for.
    sent: Option<SentAdd>,
    /// An add given up on (timeout, connection drop) that the daemon may
    /// still finish: when its repository appears, the stale "did not
    /// answer" line and the field are cleared.
    given_up: Option<SentAdd>,
    /// The folder picker, while it is open.
    browsing: Option<Task<()>>,
    /// The queue limit change in flight, if any. Dropping it gives it up.
    defaults_command: Option<Task<()>>,
    /// The line under the queue limit: why the last change failed.
    defaults_error: Option<String>,
    /// The connection as last seen, to notice it dropping.
    connection: Connection,
    /// One focus handle per theme option, and per font option (their radio
    /// groups move the focus with the arrow keys).
    theme_focus: Vec<FocusHandle>,
    font_focus: Vec<FocusHandle>,
    _subscriptions: Vec<Subscription>,
}

/// An add as it was sent.
struct SentAdd {
    path: PathBuf,
    /// The path resolved on disk, filled in when the add is given up (the
    /// daemon records the git top level of the real path).
    canonical: Option<PathBuf>,
    /// The repositories the model had when it was sent: a repository that
    /// appears later and matches the path is this add's.
    known: HashSet<RepoId>,
}

impl SettingsPane {
    pub fn new(state: Entity<ShellState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let path_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(repositories::PATH_PLACEHOLDER));
        let subscriptions = vec![
            cx.observe_in(&state, window, |this, _, window, cx| {
                this.on_state_changed(window, cx)
            }),
            cx.subscribe_in(&state, window, |this, _, event: &ShellEvent, window, cx| {
                if let ShellEvent::FocusSettings(section) = event {
                    this.focus_section(*section, window, cx);
                }
            }),
            cx.subscribe_in(
                &path_input,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.submit_repo(window, cx),
                    InputEvent::Change => {
                        if this.path_error.take().is_some() {
                            cx.notify();
                        }
                    }
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            ),
        ];
        Self {
            connection: state.read(cx).model().connection(),
            state,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            path_input,
            path_error: None,
            adding: None,
            sent: None,
            given_up: None,
            browsing: None,
            defaults_command: None,
            defaults_error: None,
            theme_focus: Vec::new(),
            font_focus: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    fn on_state_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let connection = self.state.read(cx).model().connection();
        let dropped =
            self.connection == Connection::Connected && connection != Connection::Connected;
        self.connection = connection;
        // The answer may never come; the repository, if the daemon added it,
        // arrives with the next snapshot.
        if dropped && self.adding.is_some() {
            tracing::warn!("the connection dropped while an add waited");
            self.give_up_add();
        }
        if dropped && self.defaults_command.is_some() {
            self.give_up_defaults();
        }
        self.notice_late_add(window, cx);
        cx.notify();
    }

    /// Scrolls toward `section` (expanded by the request), as far as the
    /// content below it allows, and, for Source repositories, focuses the
    /// path field.
    fn focus_section(
        &mut self,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!(?section, "focusing a settings section");
        self.scroll.scroll_to_top_of_item(section.index());
        if section == SettingsSection::Repositories {
            self.path_input
                .update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    fn toggle_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let expanded = state.settings_section_expanded(section);
            state.set_settings_section_expanded(section, !expanded, cx)
        });
    }

    /// One collapsible section: a header button, then `content` while
    /// expanded.
    fn section(
        &self,
        section: SettingsSection,
        content: AnyElement,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let expanded = self.state.read(cx).settings_section_expanded(section);
        let title = section.title();
        v_flex()
            .id(section_id(section))
            .role(Role::Group)
            .test_support()
            .aria_label(title)
            .flex_none()
            .w_full()
            .border_b_1()
            .border_color(paint.border)
            .child(
                Button::new(section_header_id(section))
                    .ghost()
                    .small()
                    .w_full()
                    .justify_start()
                    .icon(
                        Icon::new(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .text_color(paint.muted_foreground),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(paint.foreground)
                            .child(title),
                    )
                    .accessibility_label(title)
                    .aria_expanded(expanded)
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_section(section, cx))),
            )
            .when(expanded, |this| {
                this.child(div().w_full().px_3().pt_1().pb_3().child(content))
            })
    }
}

impl BasePanel for SettingsPane {
    fn panel_name(&self) -> &'static str {
        SETTINGS_PANEL
    }

    /// Drawn only while the rail has Settings selected, so the dock's group
    /// has one visible panel and draws no tab bar.
    fn visible(&self, cx: &App) -> bool {
        self.state.read(cx).right_entry() == RailEntry::Settings
    }

    /// The dock is locked to the shell's layout; its panels never close.
    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for SettingsPane {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some("Settings".into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Settings"
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

impl EventEmitter<PanelEvent> for SettingsPane {}

impl Focusable for SettingsPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SettingsPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_radio_focus(cx);
        let paint = Paint::new(self.state.read(cx), cx);
        // One child per section, in `SettingsSection::ALL` order, which is
        // what `SettingsSection::index` (the scroll target) counts.
        let sections: Vec<_> = SettingsSection::ALL
            .into_iter()
            .map(|section| {
                let content = match section {
                    SettingsSection::Appearance => {
                        self.appearance_content(&paint, cx).into_any_element()
                    }
                    SettingsSection::AgentDefaults => {
                        self.agent_defaults_content(&paint, cx).into_any_element()
                    }
                    SettingsSection::Repositories => {
                        self.repositories_content(&paint, cx).into_any_element()
                    }
                };
                self.section(section, content, &paint, cx)
            })
            .collect();
        v_flex()
            .id(SETTINGS_PANEL)
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .bg(paint.surface)
            .text_color(paint.foreground)
            .child(
                pane_heading(SETTINGS_TITLE_ID, "Settings", paint.foreground)
                    .flex_none()
                    .px_3()
                    .pt_3()
                    .pb_2(),
            )
            .child(
                v_flex()
                    .id(SETTINGS_SCROLL_ID)
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(sections),
            )
    }
}

/// The colors one frame of the pane paints with, all from the theme.
struct Paint {
    surface: Hsla,
    foreground: Hsla,
    muted_foreground: Hsla,
    /// Inline errors.
    destructive: Hsla,
    border: Hsla,
    /// A checked radio's ring and fill, and its dot.
    primary: Hsla,
    primary_foreground: Hsla,
    /// An unchecked radio's ring.
    input: Hsla,
    /// The keyboard focus ring.
    ring: Hsla,
    radius: gpui_kit::Pixels,
}

impl Paint {
    fn new(state: &ShellState, cx: &App) -> Self {
        let native = state.native_theme();
        let (surface, foreground) = region_paint(native, Region::RightPane, cx);
        Self {
            surface,
            foreground,
            muted_foreground: token_paint(native, "muted-foreground", cx),
            destructive: token_paint(native, "destructive", cx),
            border: token_paint(native, "border", cx),
            primary: token_paint(native, "primary", cx),
            primary_foreground: token_paint(native, "primary-foreground", cx),
            input: token_paint(native, "input", cx),
            ring: token_paint(native, "ring", cx),
            radius: cx.theme().radius,
        }
    }
}

/// A field's visible caption (its accessible name is on the control).
fn caption(text: &'static str, paint: &Paint) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(paint.muted_foreground)
        .child(text)
}

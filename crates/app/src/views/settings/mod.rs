//! The Settings view in the right dock: a scrollable pane of collapsible
//! sections, both expanded until the user collapses one (remembered in
//! `app-state.json`):
//!
//! - **Appearance** (app scope, [`appearance`]): the theme ("System" or a
//!   theme), the text size (a stepper over the token set's scale) and the
//!   font. A change applies live and is saved to `app-settings.json`.
//! - **Source repositories** (Space scope, [`repositories`]): the daemon's
//!   repositories and an "Add repository" field for a local path.
//!
//! Each section is a labeled group; its header is a button that exposes
//! whether it is expanded. The New agent button, when there is no repository,
//! asks for Source repositories ([`ShellEvent::FocusSettings`]): the section
//! is expanded, scrolled to the top and its path field focused.
//!
//! The pane paints its whole area from `Region::RightPane` (`card`,
//! `card-foreground`); muted text, errors and borders come from tokens.
//! Agent defaults (another Space section in the spec) wait for the daemon to
//! store them (Plan 2).

mod appearance;
mod repositories;

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

use crate::model::Connection;
use crate::shell::SETTINGS_PANEL;
use crate::shell_state::{ShellEvent, ShellState};
use crate::theme_adapter::{region_paint, token_paint, Region};
use crate::view_models::{SettingsSection, DAEMON_SILENT_TEXT};

pub use appearance::{
    FONT_FIELD_ID, FONT_LABEL, FONT_OPTIONS_ID, TEXT_SIZE_DOWN_ID, TEXT_SIZE_ID, TEXT_SIZE_LABEL,
    TEXT_SIZE_UP_ID, TEXT_SIZE_VALUE_ID, THEME_FIELD_ID, THEME_LABEL, THEME_OPTIONS_ID,
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
    /// The folder picker, while it is open.
    browsing: Option<Task<()>>,
    /// The connection as last seen, to notice it dropping.
    connection: Connection,
    _subscriptions: Vec<Subscription>,
}

impl SettingsPane {
    pub fn new(state: Entity<ShellState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let path_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(repositories::PATH_PLACEHOLDER));
        let subscriptions = vec![
            cx.observe(&state, |this, _, cx| this.on_state_changed(cx)),
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
            browsing: None,
            _subscriptions: subscriptions,
        }
    }

    fn on_state_changed(&mut self, cx: &mut Context<Self>) {
        let connection = self.state.read(cx).model().connection();
        let dropped =
            self.connection == Connection::Connected && connection != Connection::Connected;
        self.connection = connection;
        // The answer may never come; the repository, if the daemon added it,
        // arrives with the next snapshot.
        if dropped && self.adding.take().is_some() {
            tracing::warn!("the connection dropped while an add waited");
            self.path_error = Some(DAEMON_SILENT_TEXT.to_owned());
        }
        cx.notify();
    }

    /// Scrolls `section` (expanded by the request) toward the top of the
    /// pane, as far as the content below it allows, and focuses its first
    /// field.
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
        let paint = Paint::new(self.state.read(cx), cx);
        let appearance = self.appearance_content(&paint, cx).into_any_element();
        let repositories = self.repositories_content(&paint, cx).into_any_element();
        v_flex()
            .id(SETTINGS_PANEL)
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .bg(paint.surface)
            .text_color(paint.foreground)
            .child(
                div()
                    .id(SETTINGS_TITLE_ID)
                    // A node needs an id and a role to reach assistive
                    // technology.
                    .role(Role::Heading)
                    .test_support()
                    .aria_label("Settings")
                    .flex_none()
                    .px_3()
                    .pt_3()
                    .pb_2()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(paint.foreground)
                    .child("Settings"),
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
                    // One child per section, in `SettingsSection::index` order.
                    .child(self.section(SettingsSection::Appearance, appearance, &paint, cx))
                    .child(self.section(SettingsSection::Repositories, repositories, &paint, cx)),
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
            border: cx.theme().border,
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

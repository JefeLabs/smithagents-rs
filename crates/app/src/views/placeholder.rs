//! A stand-in dock panel for the right (Settings) dock until Task 14 fills
//! it. It draws its own heading, so the dock draws no title bar for it. (The
//! left pane is not a dock; its stand-in is `views::left_pane`.)

use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::{v_flex, ActiveTheme as _};
use gpui_kit::{Entity, Subscription, TestSupportExt as _};

use crate::shell_state::ShellState;
use crate::theme_adapter::{region_paint, Region};
use gpui_kit::{
    div, App, Context, EventEmitter, FocusHandle, Focusable, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Window,
};

pub struct PlaceholderPanel {
    /// The persisted panel name. Never change it once shipped.
    name: &'static str,
    title: SharedString,
    note: SharedString,
    focus: FocusHandle,
    state: Entity<ShellState>,
    _observe: Subscription,
}

impl PlaceholderPanel {
    pub fn new(
        state: Entity<ShellState>,
        name: &'static str,
        title: impl Into<SharedString>,
        note: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            name,
            title: title.into(),
            note: note.into(),
            focus: cx.focus_handle(),
            _observe: cx.observe(&state, |_, _, cx| cx.notify()),
            state,
        }
    }
}

impl BasePanel for PlaceholderPanel {
    fn panel_name(&self) -> &'static str {
        self.name
    }

    /// The dock is locked to the shell's layout; its panels never close.
    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for PlaceholderPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.title.clone())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.title.clone()
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }

    fn title_bar(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for PlaceholderPanel {}

impl Focusable for PlaceholderPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PlaceholderPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (surface, foreground) =
            region_paint(self.state.read(cx).native_theme(), Region::RightPane, cx);
        v_flex()
            .id(self.name)
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .bg(surface)
            .text_color(foreground)
            .gap_2()
            .p_3()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(foreground)
                    .child(self.title.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(self.note.clone()),
            )
    }
}

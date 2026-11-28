//! The Appearance section: theme, text size and font. Every change builds
//! new `AppearanceSettings` and hands them to the shell state
//! ([`ShellState::set_appearance`](crate::shell_state::ShellState::set_appearance)),
//! which resolves, saves and applies them live.
//!
//! The theme and font choices are radio groups drawn here on gpui-base's
//! unstyled `Radio` with focus handles the pane owns, because gpui-component's
//! `RadioGroup` keeps its radios' focus to itself and has no arrow keys. Each
//! option is a Tab stop; Space or Enter selects the focused one; Up/Left and
//! Down/Right move the focus to the adjacent option and select it, wrapping
//! at the ends, as radio groups conventionally do. The "Add repository"
//! form's source control choice ([`super::repositories`]) is drawn the same
//! way.

use gpui_kit::base::Radio;
use gpui_kit::component::button::Button;
use gpui_kit::component::label::Label;
use gpui_kit::component::{h_flex, v_flex, Disableable as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, App, Context, FocusHandle, InteractiveElement as _, IntoElement, KeyBinding,
    ParentElement as _, Role, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    Window,
};
use smithagents_theme::state::AppearanceSettings;

use super::repositories::{ADD_CHOICES_ID, ADD_CHOICES_LABEL};
use super::{caption, Paint, SettingsPane};
use crate::view_models::{
    font_options, stepped_text_scale, text_scale_label, text_scale_steps, theme_options,
    ADD_CHOICES,
};

gpui_kit::actions!(
    smithagents_settings,
    [
        /// Move to the option before the focused one in a settings radio
        /// group (the last after the first) and select it.
        SelectPreviousOption,
        /// Move to the option after the focused one in a settings radio group
        /// (the first after the last) and select it.
        SelectNextOption,
    ]
);

/// Key context of a settings radio group, for its arrow keys.
pub const RADIO_KEY_CONTEXT: &str = "SettingsRadioGroup";

/// Element id of the theme choice (a radio group named "Theme"; its radios
/// are `0..`).
pub const THEME_OPTIONS_ID: &str = "theme-options";
/// Element id of the text size stepper (a labeled group).
pub const TEXT_SIZE_ID: &str = "text-size";
/// Element id of the stepper's "smaller" button.
pub const TEXT_SIZE_DOWN_ID: &str = "text-size-down";
/// Element id of the stepper's "larger" button.
pub const TEXT_SIZE_UP_ID: &str = "text-size-up";
/// Element id of the stepper's value ("100%").
pub const TEXT_SIZE_VALUE_ID: &str = "text-size-value";
/// Element id of the font choice (a radio group named "Font"; its radios are
/// `0..`).
pub const FONT_OPTIONS_ID: &str = "font-options";

pub const THEME_LABEL: &str = "Theme";
pub const TEXT_SIZE_LABEL: &str = "Text size";
pub const FONT_LABEL: &str = "Font";

/// The width of the stepper's value, in rems so it grows with the text: it
/// holds "150%" at the largest scale.
const VALUE_WIDTH_REMS: f32 = 3.5;

/// Registers the radio groups' arrow keys.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPreviousOption, Some(RADIO_KEY_CONTEXT)),
        KeyBinding::new("left", SelectPreviousOption, Some(RADIO_KEY_CONTEXT)),
        KeyBinding::new("down", SelectNextOption, Some(RADIO_KEY_CONTEXT)),
        KeyBinding::new("right", SelectNextOption, Some(RADIO_KEY_CONTEXT)),
    ]);
}

/// The pane's radio groups: this section's two, and the add form's source
/// control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Choice {
    Theme,
    Font,
    AddSourceControl,
}

impl Choice {
    fn id(self) -> &'static str {
        match self {
            Choice::Theme => THEME_OPTIONS_ID,
            Choice::Font => FONT_OPTIONS_ID,
            Choice::AddSourceControl => ADD_CHOICES_ID,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Choice::Theme => THEME_LABEL,
            Choice::Font => FONT_LABEL,
            Choice::AddSourceControl => ADD_CHOICES_LABEL,
        }
    }
}

/// One option of a radio group: its label and whether it is selected.
pub(super) struct OptionVm {
    pub(super) label: String,
    pub(super) selected: bool,
}

impl SettingsPane {
    /// Keeps one focus handle per option of each radio group (the theme and
    /// font lists come from the token set, so they only grow with it).
    pub(super) fn sync_radio_focus(&mut self, cx: &mut Context<Self>) {
        let set = self.state.read(cx).theme_set();
        let (themes, fonts) = (set.themes.len() + 1, set.base.fonts.len());
        for (handles, count) in [
            (&mut self.theme_focus, themes),
            (&mut self.font_focus, fonts),
            (&mut self.add_choice_focus, ADD_CHOICES.len()),
        ] {
            handles.truncate(count);
            while handles.len() < count {
                handles.push(cx.focus_handle().tab_stop(true));
            }
        }
    }

    fn focus_handles(&self, choice: Choice) -> &[FocusHandle] {
        match choice {
            Choice::Theme => &self.theme_focus,
            Choice::Font => &self.font_focus,
            Choice::AddSourceControl => &self.add_choice_focus,
        }
    }

    /// Whether `choice`'s radios wait: the add form's while an add runs.
    pub(super) fn choice_disabled(&self, choice: Choice) -> bool {
        choice == Choice::AddSourceControl && self.adding.is_some()
    }

    /// The settings now, changed by `change`, handed to the shell state.
    fn change_appearance(
        &mut self,
        change: impl FnOnce(&mut AppearanceSettings),
        cx: &mut Context<Self>,
    ) {
        let mut settings = self.state.read(cx).appearance_settings().clone();
        change(&mut settings);
        self.state
            .update(cx, |state, cx| state.set_appearance(settings, cx));
    }

    fn choose(&mut self, choice: Choice, ix: usize, cx: &mut Context<Self>) {
        match choice {
            Choice::Theme => self.choose_theme(ix, cx),
            Choice::Font => self.choose_font(ix, cx),
            Choice::AddSourceControl => self.choose_add_choice(ix, cx),
        }
    }

    fn choose_theme(&mut self, ix: usize, cx: &mut Context<Self>) {
        let choice = {
            let state = self.state.read(cx);
            theme_options(state.theme_set(), &state.appearance_settings().theme)
                .into_iter()
                .nth(ix)
                .map(|option| option.choice)
        };
        let Some(choice) = choice else {
            return;
        };
        tracing::debug!(?choice, "theme chosen");
        self.change_appearance(|settings| settings.theme = choice, cx);
    }

    fn choose_font(&mut self, ix: usize, cx: &mut Context<Self>) {
        let font = {
            let state = self.state.read(cx);
            font_options(state.theme_set(), &state.appearance_settings().font)
                .into_iter()
                .nth(ix)
                .map(|option| option.id)
        };
        let Some(font) = font else {
            return;
        };
        tracing::debug!(%font, "font chosen");
        self.change_appearance(|settings| settings.font = font, cx);
    }

    /// An arrow key in a radio group: the option after (`forward`) or before
    /// the focused one, wrapping, takes the focus and is selected.
    fn step_choice(
        &mut self,
        choice: Choice,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.choice_disabled(choice) {
            return;
        }
        let handles = self.focus_handles(choice);
        let count = handles.len();
        let Some(current) = handles.iter().position(|handle| handle.is_focused(window)) else {
            return;
        };
        let next = if forward {
            (current + 1) % count
        } else {
            (current + count - 1) % count
        };
        tracing::debug!(?choice, next, "radio group arrow key");
        handles[next].focus(window, cx);
        self.choose(choice, next, cx);
    }

    /// One step larger (`up`) or smaller, from the scale in effect.
    fn step_text_size(&mut self, up: bool, cx: &mut Context<Self>) {
        let next = {
            let state = self.state.read(cx);
            let steps = text_scale_steps(&state.theme_set().base.text_scale);
            stepped_text_scale(&steps, state.theme_state().text_scale, up)
        };
        let Some(next) = next else {
            return;
        };
        tracing::debug!(next, "text size stepped");
        self.change_appearance(|settings| settings.text_scale = next, cx);
    }

    /// A radio group named after `choice`: one Tab stop per option, arrow
    /// keys between them. While [`SettingsPane::choice_disabled`], its
    /// radios are disabled and dimmed.
    pub(super) fn radio_group(
        &self,
        choice: Choice,
        options: Vec<OptionVm>,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let handles = self.focus_handles(choice);
        let count = options.len();
        let disabled = self.choice_disabled(choice);
        let pane = cx.entity().downgrade();
        v_flex()
            .id(choice.id())
            .role(Role::RadioGroup)
            .test_support()
            .aria_label(choice.label())
            .key_context(RADIO_KEY_CONTEXT)
            .on_action(
                cx.listener(move |this, _: &SelectPreviousOption, window, cx| {
                    this.step_choice(choice, false, window, cx)
                }),
            )
            .on_action(cx.listener(move |this, _: &SelectNextOption, window, cx| {
                this.step_choice(choice, true, window, cx)
            }))
            .gap_0p5()
            .children(
                options
                    .into_iter()
                    .zip(handles)
                    .enumerate()
                    .map(|(ix, (option, handle))| {
                        let pane = pane.clone();
                        let ring = paint.ring;
                        Radio::new(ix)
                            .track_focus(handle)
                            .checked(option.selected)
                            .disabled(disabled)
                            .styles(|styles| styles.disabled(|style| style.opacity(0.5)))
                            .accessibility_label(option.label.clone())
                            .set_position(ix + 1, count)
                            .on_change(move |_, _, _, cx| {
                                let _ = pane.update(cx, |this, cx| this.choose(choice, ix, cx));
                            })
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_1()
                            .py_0p5()
                            .rounded(paint.radius)
                            .border_1()
                            .border_color(paint.surface)
                            .focus_visible(move |style| style.border_color(ring))
                            .child(radio_indicator(option.selected, paint))
                            .child(
                                Label::new(option.label)
                                    .text_sm()
                                    .text_color(paint.foreground),
                            )
                    }),
            )
    }

    pub(super) fn appearance_content(&self, paint: &Paint, cx: &Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let set = state.theme_set();
        let settings = state.appearance_settings();
        let themes = theme_options(set, &settings.theme)
            .into_iter()
            .map(|option| OptionVm {
                label: option.label,
                selected: option.selected,
            })
            .collect();
        let fonts = font_options(set, &settings.font)
            .into_iter()
            .map(|option| OptionVm {
                label: option.label,
                selected: option.selected,
            })
            .collect();
        let scale = state.theme_state().text_scale;
        let steps = text_scale_steps(&set.base.text_scale);
        let smaller = stepped_text_scale(&steps, scale, false);
        let larger = stepped_text_scale(&steps, scale, true);
        let scale_text = text_scale_label(scale);

        v_flex()
            .w_full()
            .gap_3()
            .child(
                v_flex()
                    .gap_1p5()
                    .child(caption(THEME_LABEL, paint))
                    .child(self.radio_group(Choice::Theme, themes, paint, cx)),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(caption(TEXT_SIZE_LABEL, paint))
                    .child(
                        h_flex()
                            .id(TEXT_SIZE_ID)
                            .role(Role::Group)
                            .test_support()
                            .aria_label(TEXT_SIZE_LABEL)
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new(TEXT_SIZE_DOWN_ID)
                                    .icon(Icon::new(IconName::Minus))
                                    .outline()
                                    .small()
                                    .accessibility_label("Smaller text")
                                    .tooltip("Smaller text")
                                    .disabled(smaller.is_none())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.step_text_size(false, cx)
                                        }),
                                    ),
                            )
                            .child(
                                Label::new(scale_text)
                                    .id(TEXT_SIZE_VALUE_ID)
                                    .flex_none()
                                    .w(rems(VALUE_WIDTH_REMS))
                                    .text_center()
                                    .text_sm()
                                    .text_color(paint.foreground),
                            )
                            .child(
                                Button::new(TEXT_SIZE_UP_ID)
                                    .icon(Icon::new(IconName::Plus))
                                    .outline()
                                    .small()
                                    .accessibility_label("Larger text")
                                    .tooltip("Larger text")
                                    .disabled(larger.is_none())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.step_text_size(true, cx)),
                                    ),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(caption(FONT_LABEL, paint))
                    .child(self.radio_group(Choice::Font, fonts, paint, cx)),
            )
    }
}

/// A radio's circle: a `primary` disc with a `primary-foreground` dot when
/// checked, an `input` ring when not.
fn radio_indicator(checked: bool, paint: &Paint) -> impl IntoElement {
    div()
        .flex_none()
        .size(rems(1.))
        .rounded_full()
        .border_1()
        .border_color(if checked { paint.primary } else { paint.input })
        .flex()
        .items_center()
        .justify_center()
        .when(checked, |this| {
            this.bg(paint.primary).child(
                div()
                    .size(rems(0.375))
                    .rounded_full()
                    .bg(paint.primary_foreground),
            )
        })
}

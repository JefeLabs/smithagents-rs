//! The Appearance section: theme, text size and font. Every change builds
//! new `AppearanceSettings` and hands them to the shell state
//! ([`ShellState::set_appearance`](crate::shell_state::ShellState::set_appearance)),
//! which resolves, saves and applies them live.

use gpui_kit::component::button::Button;
use gpui_kit::component::label::Label;
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::{h_flex, v_flex, Disableable as _, Icon, IconName, Sizable as _};
use gpui_kit::{
    div, px, Context, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
};
use smithagents_theme::state::AppearanceSettings;

use super::{caption, Paint, SettingsPane};
use crate::view_models::{
    font_options, stepped_text_scale, text_scale_label, text_scale_steps, theme_options,
};

/// Element id of the theme choice (a radio group; its radios are `0..`).
pub const THEME_OPTIONS_ID: &str = "theme-options";
/// Element id of the text size stepper (a labeled group).
pub const TEXT_SIZE_ID: &str = "text-size";
/// Element id of the stepper's "smaller" button.
pub const TEXT_SIZE_DOWN_ID: &str = "text-size-down";
/// Element id of the stepper's "larger" button.
pub const TEXT_SIZE_UP_ID: &str = "text-size-up";
/// Element id of the stepper's value ("100%").
pub const TEXT_SIZE_VALUE_ID: &str = "text-size-value";
/// Element id of the font choice (a radio group; its radios are `0..`).
pub const FONT_OPTIONS_ID: &str = "font-options";

pub const THEME_LABEL: &str = "Theme";
pub const TEXT_SIZE_LABEL: &str = "Text size";
pub const FONT_LABEL: &str = "Font";

/// Element id of the theme field (a group named "Theme").
pub const THEME_FIELD_ID: &str = "theme-field";
/// Element id of the font field (a group named "Font").
pub const FONT_FIELD_ID: &str = "font-field";

impl SettingsPane {
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

    pub(super) fn appearance_content(&self, paint: &Paint, cx: &Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let set = state.theme_set();
        let settings = state.appearance_settings();
        let themes = theme_options(set, &settings.theme);
        let fonts = font_options(set, &settings.font);
        let scale = state.theme_state().text_scale;
        let steps = text_scale_steps(&set.base.text_scale);
        let smaller = stepped_text_scale(&steps, scale, false);
        let larger = stepped_text_scale(&steps, scale, true);
        let scale_text = text_scale_label(scale);

        let theme_group = RadioGroup::vertical(THEME_OPTIONS_ID)
            .selected_index(themes.iter().position(|option| option.selected))
            .children(themes.iter().map(|option| {
                Radio::new(option.label.clone())
                    .label(option.label.clone())
                    .small()
            }))
            .on_change(cx.listener(|this, ix: &usize, _, cx| this.choose_theme(*ix, cx)));
        let font_group = RadioGroup::vertical(FONT_OPTIONS_ID)
            .selected_index(fonts.iter().position(|option| option.selected))
            .children(fonts.iter().map(|option| {
                Radio::new(option.id.clone())
                    .label(option.label.clone())
                    .small()
            }))
            .on_change(cx.listener(|this, ix: &usize, _, cx| this.choose_font(*ix, cx)));

        v_flex()
            .w_full()
            .gap_3()
            .child(field(THEME_FIELD_ID, THEME_LABEL, theme_group, paint))
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
                                    .w(px(48.))
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
            .child(field(FONT_FIELD_ID, FONT_LABEL, font_group, paint))
    }
}

/// A caption over `control`, inside a group named `label` (the radio group
/// itself cannot carry a name in this gpui-component).
fn field(
    id: &'static str,
    label: &'static str,
    control: impl IntoElement,
    paint: &Paint,
) -> impl IntoElement {
    v_flex()
        .id(id)
        .role(Role::Group)
        .test_support()
        .aria_label(label)
        .gap_1p5()
        .child(caption(label, paint))
        .child(div().child(control))
}

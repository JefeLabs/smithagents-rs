//! Maps the Smith design tokens onto gpui-component's theme system.
//!
//! Everything except [`apply_theme_state`], [`system_appearance`] and
//! [`observe_system_appearance`] is pure. Colors come only from
//! [`NativeTheme`]; there are no literal colors here.

use std::collections::BTreeMap;

use gpui_kit::component::theme::{Theme, ThemeConfig, ThemeRegistry};
use gpui_kit::{px, App, Global, Subscription, Window, WindowAppearance};
use serde::Serialize;
use smithagents_theme::color::Srgb;
use smithagents_theme::native::NativeTheme;
use smithagents_theme::state::ThemeState;
use smithagents_theme::tokens::{Appearance, ThemeSet};

/// gpui-component color key -> Smith token name. The single mapping table.
/// Every key is a `ThemeConfigColors` serde name in gpui-component 0.11.0.
pub const COLOR_KEY_MAP: &[(&str, &str)] = &[
    ("background", "background"),
    ("foreground", "foreground"),
    ("border", "border"),
    ("input.border", "input"),
    ("ring", "ring"),
    ("selection.background", "selection"),
    ("accent.background", "accent"),
    ("accent.foreground", "accent-foreground"),
    ("primary.background", "primary"),
    ("primary.foreground", "primary-foreground"),
    ("secondary.background", "secondary"),
    ("secondary.foreground", "secondary-foreground"),
    ("muted.background", "muted"),
    ("muted.foreground", "muted-foreground"),
    ("popover.background", "popover"),
    ("popover.foreground", "popover-foreground"),
    ("list.background", "card"),
    ("list.hover.background", "accent"),
    ("list.active.background", "selection"),
    ("danger.background", "status-error"),
    ("success.background", "status-ok"),
    ("warning.background", "status-warn"),
    ("info.background", "status-info"),
];

/// The base font size gpui-component uses at text scale 1.0, in pixels. The
/// window's rem size follows `Theme::font_size`, so scaling it scales text.
const BASE_FONT_SIZE_PX: f32 = 16.0;

#[derive(Serialize)]
struct SetJson {
    name: &'static str,
    themes: Vec<ConfigJson>,
}

#[derive(Serialize)]
struct ConfigJson {
    name: String,
    mode: &'static str,
    radius: usize,
    colors: BTreeMap<&'static str, String>,
}

/// A gpui-component `ThemeSet` JSON document with one config per Smith theme
/// (`name` is the theme id). Deterministic: same set, same bytes.
pub fn theme_set_json(set: &ThemeSet) -> String {
    let themes = set
        .themes
        .iter()
        .filter_map(|t| set.native(&t.id))
        .map(|native| ConfigJson {
            name: native.id.clone(),
            mode: match native.appearance {
                Appearance::Dark => "dark",
                Appearance::Light => "light",
            },
            radius: native.radius_px.max(0.0).round() as usize,
            colors: COLOR_KEY_MAP
                .iter()
                .filter_map(|(key, token)| Some((*key, native.colors.get(*token)?.hex())))
                .collect(),
        })
        .collect();
    serde_json::to_string_pretty(&SetJson {
        name: "SmithAgents",
        themes,
    })
    .expect("a theme set always serializes")
}

/// The agent accent color for `hue` (degrees) in `native`.
pub fn agent_accent(native: &NativeTheme, hue: f32) -> Srgb {
    native.agent_color(f64::from(hue))
}

pub fn appearance_from_window(appearance: WindowAppearance) -> Appearance {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark,
        WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
    }
}

/// The OS light/dark appearance.
pub fn system_appearance(cx: &App) -> Appearance {
    appearance_from_window(cx.window_appearance())
}

/// Calls `callback` with the new appearance whenever the OS appearance
/// changes for `window`. Keep the returned subscription alive.
pub fn observe_system_appearance(
    window: &Window,
    mut callback: impl FnMut(Appearance, &mut App) + 'static,
) -> Subscription {
    window.observe_window_appearance(move |window, cx| {
        callback(appearance_from_window(window.appearance()), cx);
    })
}

/// Marks that the generated theme set is registered.
struct ThemesRegistered;
impl Global for ThemesRegistered {}

fn register_themes(set: &ThemeSet, cx: &mut App) {
    if cx.has_global::<ThemesRegistered>() {
        return;
    }
    if let Err(e) = ThemeRegistry::global_mut(cx).load_themes_from_str(&theme_set_json(set)) {
        tracing::error!("cannot register the Smith themes: {e:#}");
        return;
    }
    cx.set_global(ThemesRegistered);
}

/// Registers the Smith themes (once) and applies the one `state` names.
///
/// An unknown theme id falls back to the system pair's theme for
/// `state.appearance`. Font family comes from `state.font` and the base font
/// size is 16px times `state.text_scale`; gpui-component's `Root` sets the
/// window rem size from `Theme::font_size` every frame, so that scales text.
/// Needs a running GPUI app, so it is checked by launching, not unit tests.
pub fn apply_theme_state(state: &ThemeState, cx: &mut App) {
    let set = ThemeSet::embedded();
    register_themes(set, cx);
    let pair = &set.base.system_pair;
    let fallback = match state.appearance {
        Appearance::Dark => &pair.dark,
        Appearance::Light => &pair.light,
    };
    let config: Option<std::rc::Rc<ThemeConfig>> = {
        let themes = ThemeRegistry::global(cx).themes();
        themes
            .get(state.theme.as_str())
            .or_else(|| themes.get(fallback.as_str()))
            .cloned()
    };
    let Some(config) = config else {
        tracing::error!("theme {} is not registered", state.theme);
        return;
    };
    let font = set.base.fonts.iter().find(|f| f.id == state.font);
    let size = px(BASE_FONT_SIZE_PX * state.text_scale as f32);
    Theme::update(cx, |theme| {
        theme.apply_config(&config);
        if let Some(font) = font {
            theme.font_family = font.native_sans.clone().into();
            theme.mono_font_family = font.native_mono.clone().into();
        }
        theme.font_size = size;
    });
}

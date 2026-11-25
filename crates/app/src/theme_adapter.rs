//! Maps the Smith design tokens onto gpui-component's theme system.
//!
//! Everything except [`apply_theme_state`], [`system_appearance`] and
//! [`observe_system_appearance`] is pure. Colors come only from
//! [`NativeTheme`]; there are no literal colors here.

use std::collections::BTreeMap;

use gpui_kit::component::highlighter::HighlightTheme;
use gpui_kit::component::theme::{try_parse_color, Theme, ThemeRegistry};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{px, App, Global, Hsla, Rgba, Subscription, Window, WindowAppearance};
use serde::Serialize;
use smithagents_theme::color::Srgb;
use smithagents_theme::native::NativeTheme;
use smithagents_theme::state::ThemeState;
use smithagents_theme::tokens::{Appearance, ThemeSet};

/// gpui-component color key -> Smith token name. The single mapping table.
/// Every key is a `ThemeConfigColors` serde name in gpui-component 0.11.0.
///
/// Notes: the framework clamps `list.active.background` to 0.2 alpha, so a
/// selected row is fainter than a hovered one; hover therefore uses the weaker
/// `muted` and `list.active.border` (`ring`) marks the selection. `overlay`
/// is not mapped: it needs an alpha no token carries, and it has no fallback
/// color we could derive without a literal.
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
    ("list.hover.background", "muted"),
    ("list.active.background", "selection"),
    ("list.active.border", "ring"),
    ("list.highlight.foreground", "ring"),
    ("tab_bar.background", "card-2"),
    ("tab.foreground", "muted-foreground"),
    ("tab.active.background", "center-pane"),
    ("tab.active.foreground", "center-pane-foreground"),
    ("sidebar.background", "card"),
    ("sidebar.foreground", "card-foreground"),
    ("title_bar.background", "card-2"),
    ("status_bar.background", "card-2"),
    ("base.red", "status-error"),
    ("base.green", "status-ok"),
    ("base.yellow", "status-warn"),
    ("base.blue", "status-info"),
    ("danger.background", "status-error"),
    ("success.background", "status-ok"),
    ("warning.background", "status-warn"),
    ("info.background", "status-info"),
];

/// A shell surface that paints from one of the theme's surface tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Region {
    /// The center pane: the active chat tab and its content.
    Center,
    LeftPane,
    /// The right activity pane.
    RightPane,
    TitleBar,
    /// The right activity toolbar (the icon rail).
    Rail,
    TabStrip,
    BottomBar,
}

/// The `(surface token, foreground token)` a region paints with. This is the
/// one place the region-to-surface mapping lives; change a row to restyle a
/// region (the gpui-component rows in [`COLOR_KEY_MAP`] for the title bar,
/// tab bar, status bar and active tab must follow).
pub fn region_tokens(region: Region) -> (&'static str, &'static str) {
    match region {
        Region::Center => ("center-pane", "center-pane-foreground"),
        Region::LeftPane | Region::RightPane => ("card", "card-foreground"),
        Region::TitleBar | Region::Rail | Region::TabStrip | Region::BottomBar => {
            ("card-2", "card-2-foreground")
        }
    }
}

/// A region's `(surface, foreground)` colors in `native`, or `None` if the
/// theme lacks a token (a validated set never does).
pub fn region_colors(native: &NativeTheme, region: Region) -> Option<(Hsla, Hsla)> {
    let (surface, foreground) = region_tokens(region);
    Some((
        srgb_to_hsla(*native.colors.get(surface)?),
        srgb_to_hsla(*native.colors.get(foreground)?),
    ))
}

/// A region's `(surface, foreground)` for painting.
///
/// Without a native theme (never in the app; the shell always has one) it
/// falls back to the framework theme's base colors and logs once.
///
/// The native theme the views read here (`ShellState::native_theme`) and the
/// theme gpui-component paints with are two copies of one choice. Change the
/// theme only through [`apply_theme`], which updates both; Task 15's runtime
/// theme change must call it.
pub fn region_paint(native: Option<&NativeTheme>, region: Region, cx: &App) -> (Hsla, Hsla) {
    native
        .and_then(|native| region_colors(native, region))
        .unwrap_or_else(|| {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                tracing::debug!(
                    ?region,
                    "no native theme; painting the region with the framework colors"
                );
            });
            let theme = cx.theme();
            (theme.background, theme.foreground)
        })
}

/// Applies `state` everywhere the theme lives: gpui-component's theme
/// ([`apply_theme_state`]) and, when `shell` is given, the shell state's
/// native theme (which the views' region colors and agent accents read).
/// Returns the native theme. The shell is `None` only at startup, before it
/// exists; it takes the returned theme through `ShellOptions`.
pub fn apply_theme(
    set: &ThemeSet,
    state: &ThemeState,
    shell: Option<&gpui_kit::Entity<crate::shell_state::ShellState>>,
    cx: &mut App,
) -> Option<NativeTheme> {
    apply_theme_state(set, state, cx);
    let native = set.native(&state.theme).or_else(|| {
        let pair = &set.base.system_pair;
        set.native(match state.appearance {
            Appearance::Dark => &pair.dark,
            Appearance::Light => &pair.light,
        })
    });
    if let (Some(shell), Some(native)) = (shell, native.clone()) {
        shell.update(cx, |shell, cx| shell.set_native_theme(native, cx));
    }
    native
}

/// The base font size gpui-component uses at text scale 1.0, in pixels. The
/// window's rem size follows `Theme::font_size`, so scaling it scales text.
const BASE_FONT_SIZE_PX: f32 = 16.0;
/// gpui-component's default monospace size, scaled alongside the base size.
const BASE_MONO_FONT_SIZE_PX: f32 = 13.0;

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
    highlight: serde_json::Value,
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
            highlight: highlight_json(&native),
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

/// The framework's default syntax colors for the theme's mode, with the
/// editor background and foreground taken from our tokens. Emitting it in the
/// config makes `apply_config` switch syntax colors together with the mode
/// (it otherwise leaves the previous highlight theme in place).
fn highlight_json(native: &NativeTheme) -> serde_json::Value {
    let base = match native.appearance {
        Appearance::Dark => HighlightTheme::default_dark(),
        Appearance::Light => HighlightTheme::default_light(),
    };
    let mut style = base.style.clone();
    let color = |token: &str| {
        native
            .colors
            .get(token)
            .and_then(|c| try_parse_color(&c.hex()).ok())
    };
    style.editor_background = color("background");
    style.editor_foreground = color("foreground");
    serde_json::to_value(&style).expect("a highlight style always serializes")
}

/// The agent accent color for `hue` (degrees) in `native`.
pub fn agent_accent(native: &NativeTheme, hue: f32) -> Srgb {
    native.agent_color(f64::from(hue))
}

/// A token color as a GPUI color. The one place an [`Srgb`] becomes an
/// [`Hsla`], so views never build colors themselves.
pub fn srgb_to_hsla(color: Srgb) -> Hsla {
    let channel = |v: u8| f32::from(v) / 255.0;
    Rgba {
        r: channel(color.r),
        g: channel(color.g),
        b: channel(color.b),
        a: 1.0,
    }
    .into()
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
/// `state.appearance`. Both of the framework's mode slots are set from the
/// set's system pair, so a later `Theme::change` or `sync_system_appearance`
/// still lands on a Smith theme. An unknown font id resets to the first font.
/// The base font size is 16px (mono 13px) times `state.text_scale` (1.0 when
/// not finite); gpui-component's `Root` sets the window rem size from
/// `Theme::font_size` every frame, so that scales text. Needs a running GPUI
/// app, so it is checked by launching, not unit tests.
pub fn apply_theme_state(set: &ThemeSet, state: &ThemeState, cx: &mut App) {
    register_themes(set, cx);
    let pair = &set.base.system_pair;
    let fallback = match state.appearance {
        Appearance::Dark => &pair.dark,
        Appearance::Light => &pair.light,
    };
    let (chosen, dark, light) = {
        let themes = ThemeRegistry::global(cx).themes();
        (
            themes
                .get(state.theme.as_str())
                .or_else(|| themes.get(fallback.as_str()))
                .cloned(),
            themes.get(pair.dark.as_str()).cloned(),
            themes.get(pair.light.as_str()).cloned(),
        )
    };
    let Some(chosen) = chosen else {
        tracing::error!("theme {} is not registered", state.theme);
        return;
    };
    let font = set
        .base
        .fonts
        .iter()
        .find(|f| f.id == state.font)
        .or_else(|| set.base.fonts.first());
    let scale = if state.text_scale.is_finite() {
        state.text_scale as f32
    } else {
        1.0
    };
    Theme::update(cx, |theme| {
        if let Some(dark) = dark {
            theme.dark_theme = dark;
        }
        if let Some(light) = light {
            theme.light_theme = light;
        }
        // When the mode is unchanged, `Theme::update` may reload the mode's
        // config over our edits below (mod.rs, "installed_by_edit"). That is
        // safe only while the generated configs carry no `font.*` keys, which
        // `theme_set_json` guarantees.
        theme.apply_config(&chosen);
        if let Some(font) = font {
            theme.font_family = font.native_sans.clone().into();
            theme.mono_font_family = font.native_mono.clone().into();
        }
        theme.font_size = px(BASE_FONT_SIZE_PX * scale);
        theme.mono_font_size = px(BASE_MONO_FONT_SIZE_PX * scale);
    });
}

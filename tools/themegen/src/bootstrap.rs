//! Page-side theme bootstrap script.
//!
//! The generated JavaScript mirrors the resolution rules of
//! `smithagents_theme::state`: an unknown theme id falls back to the system pair for the
//! current color scheme, an unknown font to the first font, and a non-finite text scale to
//! the default; finite scales are clamped to the bounds.

use serde_json::json;
use smithagents_theme::tokens::{Appearance, ThemeSet};

const TEMPLATE: &str = r#"(function () {
  "use strict";
  var DATA = __DATA__;

  function isObject(v) {
    return typeof v === "object" && v !== null && !Array.isArray(v);
  }

  function findTheme(id) {
    for (var i = 0; i < DATA.themes.length; i++) {
      if (DATA.themes[i].id === id) return DATA.themes[i];
    }
    return null;
  }

  function prefersDark() {
    try {
      if (typeof window.matchMedia === "function") {
        var mq = window.matchMedia("(prefers-color-scheme: dark)");
        return !!(mq && mq.matches);
      }
    } catch (e) {}
    return true;
  }

  function resolve(state) {
    var s = isObject(state) ? state : {};
    var dark = prefersDark();
    var theme = typeof s.theme === "string" ? findTheme(s.theme) : null;
    if (!theme) theme = findTheme(dark ? DATA.pair.dark : DATA.pair.light);
    if (!theme && DATA.themes.length > 0) theme = DATA.themes[0];
    var themeId = theme ? theme.id : dark ? DATA.pair.dark : DATA.pair.light;
    var appearance = theme ? theme.appearance : dark ? "dark" : "light";

    var font = DATA.fonts.length > 0 ? DATA.fonts[0] : "";
    if (typeof s.font === "string" && DATA.fonts.indexOf(s.font) !== -1) font = s.font;

    var raw = s.text_scale;
    var scale = DATA.scale.default;
    if (typeof raw === "number" && isFinite(raw)) {
      scale = Math.min(Math.max(raw, DATA.scale.min), DATA.scale.max);
    }
    return { theme: themeId, appearance: appearance, font: font, scale: scale };
  }

  function apply(state) {
    try {
      var r = resolve(state);
      var root = document.documentElement;
      root.setAttribute("data-theme", r.theme);
      root.setAttribute("data-font", r.font);
      root.style.setProperty("--text-scale", String(r.scale));
      root.style.colorScheme = r.appearance;
    } catch (e) {}
  }

  try {
    window.__smithagentsApplyTheme = apply;
    apply(window.__SMITH_THEME__);
  } catch (e) {}
})();
"#;

fn appearance_str(a: Appearance) -> &'static str {
    match a {
        Appearance::Dark => "dark",
        Appearance::Light => "light",
    }
}

/// Renders the self-contained, deterministic bootstrap script for `set`.
pub fn render_bootstrap(set: &ThemeSet) -> String {
    let base = &set.base;
    let data = json!({
        "themes": set.themes.iter().map(|t| json!({
            "id": t.id,
            "appearance": appearance_str(t.appearance),
        })).collect::<Vec<_>>(),
        "fonts": base.fonts.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        "pair": { "dark": base.system_pair.dark, "light": base.system_pair.light },
        "scale": {
            "min": base.text_scale.min,
            "max": base.text_scale.max,
            "default": base.text_scale.default,
        },
    });
    TEMPLATE.replace("__DATA__", &data.to_string())
}

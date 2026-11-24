(function () {
  "use strict";
  var DATA = {"fonts":["system","mono"],"pair":{"dark":"smith-dark","light":"smith-light"},"scale":{"default":1.0,"max":1.5,"min":0.85},"themes":[{"appearance":"dark","id":"smith-dark"},{"appearance":"light","id":"smith-light"}]};

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

    var raw = s.text_scale !== undefined ? s.text_scale : s.textScale;
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

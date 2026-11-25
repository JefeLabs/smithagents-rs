use std::path::Path;

use gpui_kit::component::theme::{ThemeMode, ThemeSet as GpuiThemeSet};
use gpui_kit::WindowAppearance;
use serde_json::Value;
use smithagents_app::theme_adapter::{
    agent_accent, appearance_from_window, region_colors, region_tokens, srgb_to_hsla,
    theme_set_json, Region, COLOR_KEY_MAP,
};
use smithagents_theme::color::{Oklch, Srgb};
use smithagents_theme::tokens::{Appearance, ThemeSet};

fn parse(set: &ThemeSet) -> Value {
    serde_json::from_str(&theme_set_json(set)).expect("valid json")
}

fn config<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["themes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("no theme {name}"))
}

#[test]
fn one_config_per_theme_with_mode() {
    let set = ThemeSet::embedded();
    let v = parse(set);
    assert_eq!(v["themes"].as_array().unwrap().len(), 2);
    assert_eq!(config(&v, "smith-dark")["mode"], "dark");
    assert_eq!(config(&v, "smith-light")["mode"], "light");
}

#[test]
fn every_mapped_key_is_present_per_theme_and_values_match_native() {
    let set = ThemeSet::embedded();
    let v = parse(set);
    for theme in &set.themes {
        let native = set.native(&theme.id).unwrap();
        let colors = &config(&v, &theme.id)["colors"];
        for (key, token) in COLOR_KEY_MAP {
            let want = native.colors[*token].hex();
            assert_eq!(colors[*key], want.as_str(), "{} {key}", theme.id);
        }
        assert_eq!(
            colors["background"],
            native.colors["background"].hex().as_str()
        );
        assert_eq!(
            colors["primary.background"],
            native.colors["primary"].hex().as_str()
        );
    }
}

#[test]
fn output_is_deterministic() {
    let set = ThemeSet::embedded();
    assert_eq!(theme_set_json(set), theme_set_json(set));
}

#[test]
fn parses_with_gpui_components_own_deserializer() {
    let set = ThemeSet::embedded();
    let parsed: GpuiThemeSet = serde_json::from_str(&theme_set_json(set)).unwrap();
    assert_eq!(parsed.themes.len(), 2);
    let dark = parsed
        .themes
        .iter()
        .find(|t| t.name == "smith-dark")
        .unwrap();
    assert_eq!(dark.mode, ThemeMode::Dark);
    let native = set.native("smith-dark").unwrap();
    assert_eq!(
        dark.colors.background.as_ref().map(|s| s.to_string()),
        Some(native.colors["background"].hex())
    );
}

fn repo_tokens() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../design/tokens")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

#[test]
fn a_third_theme_file_appears_without_code_changes() {
    let dir = tempfile::tempdir().unwrap();
    let tokens = dir.path().join("tokens");
    copy_dir(&repo_tokens(), &tokens);
    let mut third: Value = serde_json::from_str(
        &std::fs::read_to_string(tokens.join("themes/smith-light.json")).unwrap(),
    )
    .unwrap();
    third["id"] = "smith-third".into();
    third["name"] = "Smith Third".into();
    std::fs::write(
        tokens.join("themes/smith-third.json"),
        serde_json::to_string(&third).unwrap(),
    )
    .unwrap();
    let set = ThemeSet::load_dir(&tokens).unwrap();
    let v = parse(&set);
    assert_eq!(v["themes"].as_array().unwrap().len(), 3);
    assert_eq!(config(&v, "smith-third")["mode"], "light");
}

#[test]
fn agent_accent_for_light_theme_matches_the_plan_value() {
    let set = ThemeSet::embedded();
    let native = set.native("smith-light").unwrap();
    let want = Oklch {
        l: 0.62,
        c: 0.10,
        h: 190.0,
    }
    .to_srgb()
    .unwrap();
    assert_eq!(agent_accent(&native, 190.0), want);
}

#[test]
fn window_appearance_maps_to_appearance() {
    assert_eq!(
        appearance_from_window(WindowAppearance::Dark),
        Appearance::Dark
    );
    assert_eq!(
        appearance_from_window(WindowAppearance::VibrantDark),
        Appearance::Dark
    );
    assert_eq!(
        appearance_from_window(WindowAppearance::Light),
        Appearance::Light
    );
    assert_eq!(
        appearance_from_window(WindowAppearance::VibrantLight),
        Appearance::Light
    );
}

/// The expected token-to-key pairs, pinned literally so a wrong pairing fails.
const PINNED: &[(&str, &str)] = &[
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

#[test]
fn mapping_table_is_the_pinned_list() {
    let mut got: Vec<_> = COLOR_KEY_MAP.to_vec();
    let mut want: Vec<_> = PINNED.to_vec();
    got.sort();
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn every_mapped_key_is_a_real_gpui_component_key() {
    let set = ThemeSet::embedded();
    let parsed: GpuiThemeSet = serde_json::from_str(&theme_set_json(set)).unwrap();
    for theme in &parsed.themes {
        let colors = serde_json::to_value(&theme.colors).unwrap();
        for (key, _) in COLOR_KEY_MAP {
            assert!(
                colors[*key].is_string(),
                "{}: key {key} is not a gpui-component color key",
                theme.name
            );
        }
    }
}

#[test]
fn each_config_carries_a_highlight_theme_for_its_mode() {
    let set = ThemeSet::embedded();
    let parsed: GpuiThemeSet = serde_json::from_str(&theme_set_json(set)).unwrap();
    for theme in &parsed.themes {
        let h = theme.highlight.as_ref().expect("highlight block");
        let native = set.native(&theme.name).unwrap();
        let bg = gpui_kit::component::theme::try_parse_color(&native.colors["background"].hex())
            .unwrap();
        assert_eq!(h.editor_background, Some(bg), "{}", theme.name);
    }
}

#[test]
fn srgb_to_hsla_keeps_the_color() {
    let red = srgb_to_hsla(Srgb { r: 255, g: 0, b: 0 });
    assert!(red.h.abs() < 1e-6 && (red.s - 1.0).abs() < 1e-6 && (red.l - 0.5).abs() < 1e-6);
    assert_eq!(red.a, 1.0);

    // Every theme's accent for a few hues survives the trip back to 8-bit RGB.
    let set = ThemeSet::embedded();
    for theme in &set.themes {
        let native = set.native(&theme.id).unwrap();
        for hue in [0.0, 95.0, 210.0, 300.0] {
            let srgb = agent_accent(&native, hue);
            let back = srgb_to_hsla(srgb).to_rgb();
            let byte = |v: f32| (v * 255.0).round() as u8;
            assert_eq!(
                (byte(back.r), byte(back.g), byte(back.b)),
                (srgb.r, srgb.g, srgb.b),
                "{} hue {hue}",
                theme.id
            );
        }
    }
}

#[test]
fn region_mapping_is_pinned() {
    let want = [
        (Region::Center, ("center-pane", "center-pane-foreground")),
        (Region::LeftPane, ("card", "card-foreground")),
        (Region::RightPane, ("card", "card-foreground")),
        (Region::TitleBar, ("card-2", "card-2-foreground")),
        (Region::Rail, ("card-2", "card-2-foreground")),
        (Region::TabStrip, ("card-2", "card-2-foreground")),
        (Region::BottomBar, ("card-2", "card-2-foreground")),
    ];
    for (region, tokens) in want {
        assert_eq!(region_tokens(region), tokens, "{region:?}");
    }
}

#[test]
fn region_colors_come_from_the_theme_tokens() {
    let set = ThemeSet::embedded();
    for theme in &set.themes {
        let native = set.native(&theme.id).unwrap();
        let (surface, fg) = region_colors(&native, Region::Rail).unwrap();
        assert_eq!(surface, srgb_to_hsla(native.colors["card-2"]));
        assert_eq!(fg, srgb_to_hsla(native.colors["card-2-foreground"]));
    }
}

#[test]
fn gpui_rows_agree_with_the_region_mapping() {
    let get = |key: &str| COLOR_KEY_MAP.iter().find(|(k, _)| *k == key).unwrap().1;
    assert_eq!(
        get("title_bar.background"),
        region_tokens(Region::TitleBar).0
    );
    assert_eq!(get("tab_bar.background"), region_tokens(Region::TabStrip).0);
    assert_eq!(
        get("status_bar.background"),
        region_tokens(Region::BottomBar).0
    );
    assert_eq!(get("sidebar.background"), region_tokens(Region::LeftPane).0);
    assert_eq!(
        get("tab.active.background"),
        region_tokens(Region::Center).0
    );
    assert_eq!(
        get("tab.active.foreground"),
        region_tokens(Region::Center).1
    );
}

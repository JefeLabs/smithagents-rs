use std::path::Path;

use gpui_kit::component::theme::{ThemeMode, ThemeSet as GpuiThemeSet};
use gpui_kit::WindowAppearance;
use serde_json::Value;
use smithagents_app::theme_adapter::{
    agent_accent, appearance_from_window, theme_set_json, COLOR_KEY_MAP,
};
use smithagents_theme::color::Oklch;
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

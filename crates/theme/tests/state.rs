use smithagents_theme::state::{AppearanceSettings, ThemeChoice, ThemeController, ThemeState};
use smithagents_theme::tokens::{Appearance, ThemeSet};

fn settings(theme: ThemeChoice, scale: f64, font: &str) -> AppearanceSettings {
    AppearanceSettings {
        theme,
        text_scale: scale,
        font: font.into(),
    }
}

fn ctl(theme: ThemeChoice, system: Appearance) -> ThemeController {
    ThemeController::new(
        ThemeSet::embedded().clone(),
        settings(theme, 1.0, "system"),
        system,
    )
}

#[test]
fn system_follows_system_appearance() {
    let mut c = ctl(ThemeChoice::System, Appearance::Dark);
    assert_eq!(c.state().theme, "smith-dark");
    let s = c.set_system_appearance(Appearance::Light).expect("changed");
    assert_eq!(s.theme, "smith-light");
    assert_eq!(s.appearance, Appearance::Light);
    assert_eq!(c.set_system_appearance(Appearance::Light), None);
}

#[test]
fn explicit_theme_ignores_system_flip() {
    let mut c = ctl(ThemeChoice::Id("smith-dark".into()), Appearance::Dark);
    assert_eq!(c.set_system_appearance(Appearance::Light), None);
    assert_eq!(c.state().theme, "smith-dark");
}

#[test]
fn identical_settings_return_none() {
    let mut c = ctl(ThemeChoice::System, Appearance::Dark);
    assert_eq!(
        c.update_settings(settings(ThemeChoice::System, 1.0, "system")),
        None
    );
}

#[test]
fn stale_settings_are_sanitized() {
    let mut c = ctl(ThemeChoice::System, Appearance::Dark);
    let s = c.update_settings(settings(
        ThemeChoice::Id("deleted-theme".into()),
        f64::NAN,
        "comic",
    ));
    assert_eq!(s, None, "resolves to the same state as before");
    let st = c.state();
    assert_eq!(st.theme, "smith-dark");
    assert_eq!(st.font, "system");
    assert_eq!(st.text_scale, 1.0);

    let s = c
        .update_settings(settings(ThemeChoice::System, 9.0, "system"))
        .unwrap();
    assert_eq!(s.text_scale, 1.5);
    let s = c
        .update_settings(settings(ThemeChoice::System, 0.1, "system"))
        .unwrap();
    assert_eq!(s.text_scale, 0.85);
    let s = c
        .update_settings(settings(ThemeChoice::System, f64::INFINITY, "system"))
        .unwrap();
    assert_eq!(s.text_scale, 1.0);
}

#[test]
fn unknown_theme_follows_system_flip() {
    let mut c = ctl(ThemeChoice::Id("deleted-theme".into()), Appearance::Dark);
    assert_eq!(c.state().theme, "smith-dark");
    let s = c.set_system_appearance(Appearance::Light).unwrap();
    assert_eq!(s.theme, "smith-light");
}

#[test]
fn state_json_is_exact() {
    let c = ctl(ThemeChoice::System, Appearance::Dark);
    let json = serde_json::to_string(&c.state()).unwrap();
    assert_eq!(
        json,
        r#"{"theme":"smith-dark","appearance":"dark","text_scale":1.0,"font":"system"}"#
    );
    let back: ThemeState = serde_json::from_str(&json).unwrap();
    assert_eq!(back, c.state());
}

#[test]
fn choice_serializes_as_plain_string() {
    assert_eq!(
        serde_json::to_string(&ThemeChoice::System).unwrap(),
        r#""system""#
    );
    assert_eq!(
        serde_json::to_string(&ThemeChoice::Id("smith-dark".into())).unwrap(),
        r#""smith-dark""#
    );
    let c: ThemeChoice = serde_json::from_str(r#""system""#).unwrap();
    assert_eq!(c, ThemeChoice::System);
    let c: ThemeChoice = serde_json::from_str(r#""x""#).unwrap();
    assert_eq!(c, ThemeChoice::Id("x".into()));
}

#[test]
fn third_theme_is_honored() {
    let dir = tempfile::tempdir().unwrap();
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../design/tokens");
    std::fs::create_dir(dir.path().join("themes")).unwrap();
    for f in ["base.json", "agent-palette.json"] {
        std::fs::copy(src.join(f), dir.path().join(f)).unwrap();
    }
    for f in ["smith-dark.json", "smith-light.json"] {
        std::fs::copy(
            src.join("themes").join(f),
            dir.path().join("themes").join(f),
        )
        .unwrap();
    }
    let third = std::fs::read_to_string(src.join("themes/smith-dark.json"))
        .unwrap()
        .replace("smith-dark", "third");
    std::fs::write(dir.path().join("themes/third.json"), third).unwrap();
    let set = ThemeSet::load_dir(dir.path()).unwrap();
    let c = ThemeController::new(
        set,
        settings(ThemeChoice::Id("third".into()), 1.0, "system"),
        Appearance::Light,
    );
    assert_eq!(c.state().theme, "third");
    assert_eq!(c.state().appearance, Appearance::Dark);
}

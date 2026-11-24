use smithagents_theme::color::Oklch;
use smithagents_theme::tokens::{ThemeSet, COLOR_TOKENS};

#[test]
fn shipped_themes_have_every_token() {
    let set = ThemeSet::embedded();
    for id in ["smith-light", "smith-dark"] {
        let n = set.native(id).expect(id);
        for t in COLOR_TOKENS {
            assert!(n.colors.contains_key(*t), "{id} missing {t}");
        }
    }
}

#[test]
fn backgrounds_have_expected_luminance() {
    let set = ThemeSet::embedded();
    let light = set.native("smith-light").unwrap();
    let dark = set.native("smith-dark").unwrap();
    assert!(light.colors["background"].relative_luminance() > 0.9);
    assert!(dark.colors["background"].relative_luminance() < 0.05);
}

#[test]
fn agent_color_uses_theme_lightness_and_chroma() {
    let n = ThemeSet::embedded().native("smith-light").unwrap();
    let expected = Oklch {
        l: 0.62,
        c: 0.10,
        h: 190.0,
    }
    .to_srgb()
    .unwrap();
    assert_eq!(n.agent_color(190.0), expected);
}

#[test]
fn unknown_id_is_none() {
    assert!(ThemeSet::embedded().native("nope").is_none());
}

use smithagents_theme::color::Oklch;
use smithagents_theme::tokens::{Appearance, ThemeSet};
use smithagents_theme::validate::{validate, ValidationError as E};

fn set() -> ThemeSet {
    ThemeSet::embedded().clone()
}

fn errors(set: &ThemeSet) -> Vec<E> {
    validate(set).expect_err("expected validation errors")
}

fn of(set: &mut ThemeSet, a: Appearance) -> &mut smithagents_theme::tokens::ThemeFile {
    set.themes.iter_mut().find(|t| t.appearance == a).unwrap()
}

#[test]
fn shipped_set_is_valid() {
    assert_eq!(validate(ThemeSet::embedded()), Ok(()));
}

#[test]
fn missing_token() {
    let mut s = set();
    of(&mut s, Appearance::Dark).colors.remove("border");
    let e = errors(&s);
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(matches!(&e[0], E::MissingToken { token, .. } if token == "border"));
}

#[test]
fn unknown_token() {
    let mut s = set();
    let o = Oklch {
        l: 0.5,
        c: 0.0,
        h: 0.0,
    };
    of(&mut s, Appearance::Dark)
        .colors
        .insert("bogus".into(), o);
    let e = errors(&s);
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(matches!(&e[0], E::UnknownToken { token, .. } if token == "bogus"));
}

#[test]
fn out_of_gamut() {
    let mut s = set();
    of(&mut s, Appearance::Dark).colors.insert(
        "background".into(),
        Oklch {
            l: 0.7,
            c: 0.4,
            h: 150.0,
        },
    );
    let e = errors(&s);
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(matches!(&e[0], E::OutOfGamut { token, .. } if token == "background"));
}

#[test]
fn low_contrast() {
    let mut s = set();
    let t = of(&mut s, Appearance::Dark);
    let mut fg = t.colors["foreground"];
    fg.l = 0.5;
    t.colors.insert("foreground".into(), fg);
    let e = errors(&s);
    assert!(
        e.iter().any(|e| matches!(e,
            E::LowContrast { foreground, background, .. }
            if foreground == "foreground" && background == "background")),
        "{e:?}"
    );
}

#[test]
fn status_error_hue() {
    let mut s = set();
    let t = of(&mut s, Appearance::Dark);
    let mut c = t.colors["status-error"];
    c.h = 250.0;
    t.colors.insert("status-error".into(), c);
    let e = errors(&s);
    assert!(
        e.iter().any(|e| matches!(e,
            E::StatusHueOutsideReservedRange { token, .. } if token == "status-error")),
        "{e:?}"
    );
}

#[test]
fn info_too_saturated() {
    let mut s = set();
    let t = of(&mut s, Appearance::Dark);
    let mut c = t.colors["status-info"];
    c.c = 0.2;
    t.colors.insert("status-info".into(), c);
    let e = errors(&s);
    assert!(
        e.iter().any(|e| matches!(e, E::InfoTooSaturated { .. })),
        "{e:?}"
    );
}

#[test]
fn agent_low_contrast() {
    let mut s = set();
    let t = of(&mut s, Appearance::Light);
    t.agent.l = 0.72;
    t.agent.c = 0.12;
    let e = errors(&s);
    assert!(!e.is_empty());
    assert!(
        e.iter().all(|e| matches!(e, E::AgentLowContrast { .. })),
        "{e:?}"
    );
    for h in &s.agent_hues {
        assert!(
            e.iter()
                .any(|e| matches!(e, E::AgentLowContrast { hue, .. } if hue == h)),
            "no error for hue {h}: {e:?}"
        );
    }
}

#[test]
fn bad_theme_ids() {
    for id in ["Smith Dark", "system"] {
        let mut s = set();
        of(&mut s, Appearance::Dark).id = id.into();
        let e = errors(&s);
        assert!(
            e.iter()
                .any(|e| matches!(e, E::BadThemeId { id: i } if i == id)),
            "{e:?}"
        );
    }
}

#[test]
fn duplicate_ids() {
    let mut s = set();
    let t = of(&mut s, Appearance::Dark).clone();
    s.themes.push(t);
    let e = errors(&s);
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(matches!(&e[0], E::DuplicateThemeId { .. }));
}

#[test]
fn missing_light_theme() {
    let mut s = set();
    s.themes.retain(|t| t.appearance != Appearance::Light);
    let e = errors(&s);
    assert!(
        e.iter().any(|e| matches!(
            e,
            E::MissingAppearance {
                appearance: Appearance::Light
            }
        )),
        "{e:?}"
    );
}

#[test]
fn bad_system_pair() {
    let mut s = set();
    s.base.system_pair.light = s.base.system_pair.dark.clone();
    let e = errors(&s);
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(matches!(
        &e[0],
        E::BadSystemPair {
            which: Appearance::Light,
            ..
        }
    ));
}

#[test]
fn fonts_and_text_scale() {
    let mut s = set();
    s.base.fonts.clear();
    assert!(errors(&s).iter().any(|e| matches!(e, E::NoFonts)));
    let mut s = set();
    s.base.text_scale.default = s.base.text_scale.max + 1.0;
    assert!(errors(&s).iter().any(|e| matches!(e, E::BadTextScale)));
    let mut s = set();
    s.base.text_scale.min = f64::NAN;
    assert!(errors(&s).iter().any(|e| matches!(e, E::BadTextScale)));
}

#[test]
fn two_mutations_report_both() {
    let mut s = set();
    of(&mut s, Appearance::Dark).colors.remove("border");
    s.base.fonts.clear();
    let e = errors(&s);
    assert!(e.iter().any(|e| matches!(e, E::MissingToken { .. })));
    assert!(e.iter().any(|e| matches!(e, E::NoFonts)));
}

#[test]
fn missing_reserved_hue_range_is_reported_and_other_rules_still_run() {
    let mut s = set();
    s.reserved_hue_ranges.remove("yellow");
    // Break an unrelated rule to prove validation continues.
    of(&mut s, Appearance::Dark).colors.remove("border");
    let e = errors(&s);
    assert_eq!(
        e.iter()
            .filter(|x| matches!(x, E::MissingReservedHueRange { name } if name == "yellow"))
            .count(),
        1,
        "{e:?}"
    );
    assert!(e
        .iter()
        .any(|x| matches!(x, E::MissingToken { token, .. } if token == "border")));
}

#[test]
fn bad_font_id() {
    let mut s = set();
    s.base.fonts[0].id = "a\"b".into();
    let e = errors(&s);
    assert!(
        e.iter()
            .any(|x| matches!(x, E::BadFontId { id } if id == "a\"b")),
        "{e:?}"
    );
}

#[test]
fn duplicate_font_id() {
    let mut s = set();
    let dup = s.base.fonts[0].clone();
    s.base.fonts.push(dup.clone());
    let e = errors(&s);
    assert!(
        e.iter()
            .any(|x| matches!(x, E::DuplicateFontId { id } if *id == dup.id)),
        "{e:?}"
    );
}

#[test]
fn missing_card_2() {
    let mut s = set();
    of(&mut s, Appearance::Light).colors.remove("card-2");
    let e = errors(&s);
    assert!(
        e.iter()
            .any(|e| matches!(e, E::MissingToken { token, .. } if token == "card-2")),
        "{e:?}"
    );
}

#[test]
fn low_contrast_card_2_foreground() {
    let mut s = set();
    let t = of(&mut s, Appearance::Dark);
    let mut fg = t.colors["card-2-foreground"];
    fg.l = 0.4;
    t.colors.insert("card-2-foreground".into(), fg);
    let e = errors(&s);
    assert!(
        e.iter().any(|e| matches!(e,
            E::LowContrast { foreground, background, .. }
            if foreground == "card-2-foreground" && background == "card-2")),
        "{e:?}"
    );
}

#[test]
fn agent_low_contrast_against_card_2() {
    // Make card-2 the only surface an agent color fails against.
    let mut s = set();
    let t = of(&mut s, Appearance::Dark);
    t.colors.insert(
        "card-2".into(),
        Oklch {
            l: 0.72,
            c: 0.0,
            h: 0.0,
        },
    );
    let e = errors(&s);
    assert!(
        e.iter().any(|e| matches!(e,
            E::AgentLowContrast { against, .. } if against == "card-2")),
        "{e:?}"
    );
}

#[test]
fn indistinct_card_surfaces() {
    let mut s = set();
    let t = of(&mut s, Appearance::Dark);
    let card = t.colors["card"];
    t.colors.insert("card-2".into(), card);
    let e = errors(&s);
    assert!(
        e.iter().any(|e| matches!(e,
            E::IndistinctSurfaces { a, b, .. } if a == "card" && b == "card-2")),
        "{e:?}"
    );
}

#[test]
fn missing_center() {
    let mut s = set();
    of(&mut s, Appearance::Dark).colors.remove("center");
    let e = errors(&s);
    assert!(
        e.iter()
            .any(|e| matches!(e, E::MissingToken { token, .. } if token == "center")),
        "{e:?}"
    );
}

#[test]
fn low_contrast_center_foreground() {
    let mut s = set();
    let t = of(&mut s, Appearance::Light);
    let mut fg = t.colors["center-foreground"];
    fg.l = 0.8;
    t.colors.insert("center-foreground".into(), fg);
    let e = errors(&s);
    assert!(
        e.iter().any(|e| matches!(e,
            E::LowContrast { foreground, background, .. }
            if foreground == "center-foreground" && background == "center")),
        "{e:?}"
    );
}

#[test]
fn agent_low_contrast_against_center() {
    let mut s = set();
    let t = of(&mut s, Appearance::Dark);
    t.colors.insert(
        "center".into(),
        Oklch {
            l: 0.72,
            c: 0.0,
            h: 0.0,
        },
    );
    let e = errors(&s);
    assert!(
        e.iter().any(|e| matches!(e,
            E::AgentLowContrast { against, .. } if against == "center")),
        "{e:?}"
    );
}

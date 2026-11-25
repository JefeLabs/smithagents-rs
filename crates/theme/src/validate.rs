//! Validation of a [`ThemeSet`]: token completeness, sRGB gamut, WCAG
//! contrast and status-color rules. Collects every error.

use std::collections::{BTreeMap, HashSet};

use crate::color::{contrast_ratio, Oklch, Srgb};
use crate::tokens::{Appearance, ThemeFile, ThemeSet, COLOR_TOKENS};

/// `(foreground token, background token, minimum contrast)`.
pub const CONTRAST_RULES: &[(&str, &str, f64)] = &[
    ("foreground", "background", 4.5),
    ("card-foreground", "card", 4.5),
    ("card-2-foreground", "card-2", 4.5),
    ("center-pane-foreground", "center-pane", 4.5),
    // Hovered inactive tabs: center text on the tab strip.
    ("center-pane-foreground", "card-2", 4.5),
    ("popover-foreground", "popover", 4.5),
    ("primary-foreground", "primary", 4.5),
    ("secondary-foreground", "secondary", 4.5),
    ("accent-foreground", "accent", 4.5),
    // A control surface on the bar it sits on (the title bar's update chip):
    // non-text contrast, 3:1.
    ("primary", "card-2", 3.0),
    ("muted-foreground", "background", 4.5),
    ("muted-foreground", "muted", 4.5),
    ("muted-foreground", "card", 4.5),
    ("muted-foreground", "card-2", 4.5),
    ("muted-foreground", "center-pane", 4.5),
    ("destructive", "background", 3.0),
    ("destructive", "card", 3.0),
    ("destructive", "card-2", 3.0),
    ("destructive", "center-pane", 3.0),
    ("ring", "background", 3.0),
    ("ring", "card", 3.0),
    ("ring", "card-2", 3.0),
    ("ring", "center-pane", 3.0),
    ("status-ok", "background", 3.0),
    ("status-ok", "card", 3.0),
    ("status-ok", "card-2", 3.0),
    ("status-ok", "center-pane", 3.0),
    ("status-warn", "background", 3.0),
    ("status-warn", "card", 3.0),
    ("status-warn", "card-2", 3.0),
    ("status-warn", "center-pane", 3.0),
    ("status-error", "background", 3.0),
    ("status-error", "card", 3.0),
    ("status-error", "card-2", 3.0),
    ("status-error", "center-pane", 3.0),
    ("status-info", "background", 3.0),
    ("status-info", "card", 3.0),
    ("status-info", "card-2", 3.0),
    ("status-info", "center-pane", 3.0),
];

/// Status token and the reserved hue range its hue must fall in.
const STATUS_HUES: &[(&str, &str)] = &[
    ("status-error", "red"),
    ("status-warn", "yellow"),
    ("status-ok", "green"),
];

const INFO_MAX_CHROMA: f64 = 0.05;
const AGENT_MIN_CONTRAST: f64 = 3.0;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("theme id `{id}` must match ^[a-z][a-z0-9-]*$ and not be `system`")]
    BadThemeId { id: String },
    #[error("duplicate theme id `{id}`")]
    DuplicateThemeId { id: String },
    #[error("theme `{theme}`: missing token `{token}`")]
    MissingToken { theme: String, token: String },
    #[error("theme `{theme}`: unknown token `{token}`")]
    UnknownToken { theme: String, token: String },
    #[error("theme `{theme}`: token `{token}` is outside the sRGB gamut")]
    OutOfGamut { theme: String, token: String },
    #[error(
        "theme `{theme}`: `{foreground}` on `{background}` has contrast {ratio:.2}, needs {min}"
    )]
    LowContrast {
        theme: String,
        foreground: String,
        background: String,
        ratio: f64,
        min: f64,
    },
    #[error("theme `{theme}`: `{token}` hue is outside its reserved hue range")]
    StatusHueOutsideReservedRange { theme: String, token: String },
    #[error("theme `{theme}`: `status-info` chroma must be at most 0.05")]
    InfoTooSaturated { theme: String },
    #[error("theme `{theme}`: agent hue {hue} is outside the sRGB gamut")]
    AgentOutOfGamut { theme: String, hue: f64 },
    #[error("theme `{theme}`: agent hue {hue} on `{against}` has contrast {ratio:.2}, needs 3.0")]
    AgentLowContrast {
        theme: String,
        hue: f64,
        against: String,
        ratio: f64,
    },
    #[error("theme `{theme}`: `{a}` and `{b}` are the same color; the card surfaces must differ")]
    IndistinctSurfaces { theme: String, a: String, b: String },
    #[error("the set needs at least one {appearance:?} theme")]
    MissingAppearance { appearance: Appearance },
    #[error("system_pair.{which:?} names `{id}`, which is not a theme of that appearance")]
    BadSystemPair { which: Appearance, id: String },
    #[error("reserved hue range `{name}` is missing")]
    MissingReservedHueRange { name: String },
    #[error("font id `{id}` must match ^[a-z][a-z0-9-]*$")]
    BadFontId { id: String },
    #[error("duplicate font id `{id}`")]
    DuplicateFontId { id: String },
    #[error("no fonts defined")]
    NoFonts,
    #[error("text scale must satisfy min <= default <= max with finite values")]
    BadTextScale,
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && id != "system"
}

fn valid_font_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn in_range(h: f64, (start, end): (f64, f64)) -> bool {
    let h = h.rem_euclid(360.0);
    if start <= end {
        (start..=end).contains(&h)
    } else {
        h >= start || h <= end
    }
}

fn validate_theme(set: &ThemeSet, t: &ThemeFile, errs: &mut Vec<ValidationError>) {
    let theme = t.id.clone();
    for token in COLOR_TOKENS {
        if !t.colors.contains_key(*token) {
            errs.push(ValidationError::MissingToken {
                theme: theme.clone(),
                token: (*token).into(),
            });
        }
    }
    for token in t.colors.keys() {
        if !COLOR_TOKENS.contains(&token.as_str()) {
            errs.push(ValidationError::UnknownToken {
                theme: theme.clone(),
                token: token.clone(),
            });
        }
    }

    // Quantized sRGB for every token that is present and in gamut.
    let mut rgb: BTreeMap<&str, Srgb> = BTreeMap::new();
    for (token, color) in &t.colors {
        match color.to_srgb() {
            Ok(s) => {
                rgb.insert(token.as_str(), s);
            }
            Err(_) => errs.push(ValidationError::OutOfGamut {
                theme: theme.clone(),
                token: token.clone(),
            }),
        }
    }

    for &(fg, bg, min) in CONTRAST_RULES {
        if let (Some(f), Some(b)) = (rgb.get(fg), rgb.get(bg)) {
            let ratio = contrast_ratio(*f, *b);
            if ratio < min {
                errs.push(ValidationError::LowContrast {
                    theme: theme.clone(),
                    foreground: fg.into(),
                    background: bg.into(),
                    ratio,
                    min,
                });
            }
        }
    }

    if let (Some(a), Some(b)) = (rgb.get("card"), rgb.get("card-2")) {
        if a == b {
            errs.push(ValidationError::IndistinctSurfaces {
                theme: theme.clone(),
                a: "card".into(),
                b: "card-2".into(),
            });
        }
    }

    for &(token, range) in STATUS_HUES {
        if let (Some(color), Some(r)) = (t.colors.get(token), set.reserved_hue_ranges.get(range)) {
            if !in_range(color.h, *r) {
                errs.push(ValidationError::StatusHueOutsideReservedRange {
                    theme: theme.clone(),
                    token: token.into(),
                });
            }
        }
    }
    if t.colors
        .get("status-info")
        .is_some_and(|c| c.c > INFO_MAX_CHROMA)
    {
        errs.push(ValidationError::InfoTooSaturated {
            theme: theme.clone(),
        });
    }

    for &hue in &set.agent_hues {
        let color = Oklch {
            l: t.agent.l,
            c: t.agent.c,
            h: hue,
        };
        let Ok(s) = color.to_srgb() else {
            errs.push(ValidationError::AgentOutOfGamut {
                theme: theme.clone(),
                hue,
            });
            continue;
        };
        for against in ["background", "card", "card-2", "center-pane"] {
            if let Some(b) = rgb.get(against) {
                let ratio = contrast_ratio(s, *b);
                if ratio < AGENT_MIN_CONTRAST {
                    errs.push(ValidationError::AgentLowContrast {
                        theme: theme.clone(),
                        hue,
                        against: against.into(),
                        ratio,
                    });
                }
            }
        }
    }
}

/// Validates the whole set, collecting every error.
pub fn validate(set: &ThemeSet) -> Result<(), Vec<ValidationError>> {
    let mut errs = Vec::new();

    let mut seen = HashSet::new();
    for t in &set.themes {
        if !valid_id(&t.id) {
            errs.push(ValidationError::BadThemeId { id: t.id.clone() });
        }
        if !seen.insert(t.id.as_str()) {
            errs.push(ValidationError::DuplicateThemeId { id: t.id.clone() });
        }
        validate_theme(set, t, &mut errs);
    }

    for appearance in [Appearance::Dark, Appearance::Light] {
        if !set.themes.iter().any(|t| t.appearance == appearance) {
            errs.push(ValidationError::MissingAppearance { appearance });
        }
    }
    let pair = &set.base.system_pair;
    for (which, id) in [
        (Appearance::Dark, &pair.dark),
        (Appearance::Light, &pair.light),
    ] {
        if !set.theme(id).is_some_and(|t| t.appearance == which) {
            errs.push(ValidationError::BadSystemPair {
                which,
                id: id.clone(),
            });
        }
    }

    for name in ["red", "yellow", "green"] {
        if !set.reserved_hue_ranges.contains_key(name) {
            errs.push(ValidationError::MissingReservedHueRange { name: name.into() });
        }
    }

    if set.base.fonts.is_empty() {
        errs.push(ValidationError::NoFonts);
    }
    let mut seen_fonts = HashSet::new();
    for f in &set.base.fonts {
        if !valid_font_id(&f.id) {
            errs.push(ValidationError::BadFontId { id: f.id.clone() });
        }
        if !seen_fonts.insert(f.id.as_str()) {
            errs.push(ValidationError::DuplicateFontId { id: f.id.clone() });
        }
    }
    let s = &set.base.text_scale;
    let finite = s.min.is_finite() && s.default.is_finite() && s.max.is_finite();
    if !(finite && s.min <= s.default && s.default <= s.max) {
        errs.push(ValidationError::BadTextScale);
    }

    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

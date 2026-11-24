//! Shell-side theme state: persisted settings, resolution, change detection.

use serde::{Deserialize, Serialize};

use crate::tokens::{Appearance, ThemeSet};

/// The user's theme choice, serialized as `"system"` or a theme id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ThemeChoice {
    System,
    Id(String),
}

impl From<String> for ThemeChoice {
    fn from(s: String) -> Self {
        if s == "system" {
            ThemeChoice::System
        } else {
            ThemeChoice::Id(s)
        }
    }
}

impl From<ThemeChoice> for String {
    fn from(c: ThemeChoice) -> Self {
        match c {
            ThemeChoice::System => "system".into(),
            ThemeChoice::Id(id) => id,
        }
    }
}

/// What the shell persists.
///
/// Deserialization is lenient: missing fields take defaults and a non-numeric
/// `text_scale` (a NaN is persisted as `null`) becomes NaN, which resolution
/// replaces with the default scale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    pub theme: ThemeChoice,
    #[serde(deserialize_with = "lenient_f64")]
    pub text_scale: f64,
    pub font: String,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::System,
            text_scale: 1.0,
            font: "system".into(),
        }
    }
}

fn lenient_f64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(v.as_f64().unwrap_or(f64::NAN))
}

/// What is pushed to views.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeState {
    pub theme: String,
    pub appearance: Appearance,
    pub text_scale: f64,
    pub font: String,
}

pub struct ThemeController {
    set: ThemeSet,
    settings: AppearanceSettings,
    system: Appearance,
    state: ThemeState,
}

impl ThemeController {
    pub fn new(set: ThemeSet, settings: AppearanceSettings, system: Appearance) -> Self {
        let state = resolve(&set, &settings, system);
        Self {
            set,
            settings,
            system,
            state,
        }
    }

    pub fn state(&self) -> ThemeState {
        self.state.clone()
    }

    pub fn update_settings(&mut self, settings: AppearanceSettings) -> Option<ThemeState> {
        self.settings = settings;
        self.refresh()
    }

    pub fn set_system_appearance(&mut self, system: Appearance) -> Option<ThemeState> {
        self.system = system;
        self.refresh()
    }

    fn refresh(&mut self) -> Option<ThemeState> {
        let next = resolve(&self.set, &self.settings, self.system);
        if next == self.state {
            return None;
        }
        self.state = next.clone();
        Some(next)
    }
}

fn resolve(set: &ThemeSet, settings: &AppearanceSettings, system: Appearance) -> ThemeState {
    let pair = &set.base.system_pair;
    let system_id = match system {
        Appearance::Dark => &pair.dark,
        Appearance::Light => &pair.light,
    };
    let theme = match &settings.theme {
        ThemeChoice::Id(id) => set.theme(id),
        ThemeChoice::System => None,
    }
    .or_else(|| set.theme(system_id))
    .or_else(|| set.themes.first());

    let (theme_id, appearance) = match theme {
        Some(t) => (t.id.clone(), t.appearance),
        None => (system_id.clone(), system),
    };

    let font = if set.base.fonts.iter().any(|f| f.id == settings.font) {
        settings.font.clone()
    } else {
        set.base
            .fonts
            .first()
            .map(|f| f.id.clone())
            .unwrap_or_else(|| settings.font.clone())
    };

    let ts = &set.base.text_scale;
    let text_scale = if settings.text_scale.is_finite() {
        settings.text_scale.clamp(ts.min, ts.max)
    } else {
        ts.default
    };

    ThemeState {
        theme: theme_id,
        appearance,
        text_scale,
        font,
    }
}

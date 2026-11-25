//! The appearance settings at runtime: the saved [`AppearanceSettings`], the
//! [`ThemeController`] that resolves them against the OS appearance, and the
//! store they are saved to.
//!
//! Pure (no GPUI): the shell state owns one and applies what it resolves
//! ([`crate::shell_state::ShellState::set_appearance`]).

use std::io;

use smithagents_theme::state::{AppearanceSettings, ThemeController, ThemeState};
use smithagents_theme::tokens::{Appearance, ThemeSet};

use crate::settings::AppSettingsStore;

/// What one settings change did.
#[derive(Debug, Default)]
pub struct AppearanceChange {
    /// The newly resolved theme, when it differs from the one before.
    pub state: Option<ThemeState>,
    /// The changed settings could not be saved. They still apply for this run.
    pub save_error: Option<io::Error>,
}

pub struct AppearanceRuntime {
    set: &'static ThemeSet,
    controller: ThemeController,
    settings: AppearanceSettings,
    /// `None` keeps the settings in memory only.
    store: Option<AppSettingsStore>,
}

impl AppearanceRuntime {
    /// Starts from the settings `store` holds (defaults when it is missing,
    /// empty or corrupt, or when there is no store) and the OS's `system`
    /// appearance.
    pub fn new(
        set: &'static ThemeSet,
        store: Option<AppSettingsStore>,
        system: Appearance,
    ) -> Self {
        let settings = store
            .as_ref()
            .map(AppSettingsStore::load)
            .unwrap_or_default();
        Self {
            set,
            controller: ThemeController::new(set.clone(), settings.clone(), system),
            settings,
            store,
        }
    }

    pub fn set(&self) -> &'static ThemeSet {
        self.set
    }

    /// The user's choices as saved (not resolved: a stale theme id stays).
    pub fn settings(&self) -> &AppearanceSettings {
        &self.settings
    }

    /// The theme the settings resolve to now.
    pub fn state(&self) -> ThemeState {
        self.controller.state()
    }

    /// Takes the user's new `settings`. Changed settings are saved even when
    /// they resolve to the theme already in effect (choosing "Smith Dark"
    /// while "System" shows it is still a choice to remember); unchanged
    /// ones write nothing.
    pub fn update_settings(&mut self, settings: AppearanceSettings) -> AppearanceChange {
        if settings == self.settings {
            return AppearanceChange::default();
        }
        self.settings = settings.clone();
        let state = self.controller.update_settings(settings);
        let save_error = self
            .store
            .as_ref()
            .and_then(|store| store.save(&self.settings).err());
        AppearanceChange { state, save_error }
    }

    /// The OS appearance changed; returns the new theme if that changes it
    /// (only while the theme choice is "System").
    pub fn set_system_appearance(&mut self, system: Appearance) -> Option<ThemeState> {
        self.controller.set_system_appearance(system)
    }
}

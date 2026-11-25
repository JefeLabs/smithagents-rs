use std::os::unix::fs::PermissionsExt;

use smithagents_app::settings::AppSettingsStore;
use smithagents_theme::state::{AppearanceSettings, ThemeChoice};

fn names(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = AppSettingsStore::new(dir.path());
    let s = AppearanceSettings {
        theme: ThemeChoice::Id("smith-light".into()),
        text_scale: 1.25,
        font: "mono".into(),
    };
    store.save(&s).unwrap();
    assert_eq!(store.load(), s);
}

#[test]
fn missing_empty_and_corrupt_load_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let store = AppSettingsStore::new(dir.path());
    assert_eq!(store.load(), AppearanceSettings::default());
    let file = dir.path().join("app-settings.json");
    std::fs::write(&file, "").unwrap();
    assert_eq!(store.load(), AppearanceSettings::default());
    std::fs::write(&file, "{not json").unwrap();
    assert_eq!(store.load(), AppearanceSettings::default());
}

#[test]
fn partially_valid_file_loads_sanely() {
    let dir = tempfile::tempdir().unwrap();
    let store = AppSettingsStore::new(dir.path());
    std::fs::write(
        dir.path().join("app-settings.json"),
        r#"{"theme":"gone","text_scale":null}"#,
    )
    .unwrap();
    let s = store.load();
    assert_eq!(s.theme, ThemeChoice::Id("gone".into()));
    assert!(s.text_scale.is_nan());
    assert_eq!(s.font, "system");
}

#[test]
fn nan_scale_round_trips_to_a_value_resolution_replaces() {
    let dir = tempfile::tempdir().unwrap();
    let store = AppSettingsStore::new(dir.path());
    for bad in [f64::NAN, f64::INFINITY] {
        let s = AppearanceSettings {
            text_scale: bad,
            ..AppearanceSettings::default()
        };
        store.save(&s).unwrap();
        assert!(!store.load().text_scale.is_finite());
    }
}

#[test]
fn file_is_private_and_no_temp_file_is_left() {
    let dir = tempfile::tempdir().unwrap();
    let store = AppSettingsStore::new(dir.path());
    store.save(&AppearanceSettings::default()).unwrap();
    store.save(&AppearanceSettings::default()).unwrap();
    let mode = std::fs::metadata(dir.path().join("app-settings.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(names(dir.path()), vec!["app-settings.json"]);
}

#[test]
fn creates_missing_home_0700_and_leaves_existing_home_alone() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("nested").join("home");
    AppSettingsStore::new(&home)
        .save(&AppearanceSettings::default())
        .unwrap();
    assert_eq!(
        std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
        0o700
    );

    let existing = dir.path().join("existing");
    std::fs::create_dir(&existing).unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
    AppSettingsStore::new(&existing)
        .save(&AppearanceSettings::default())
        .unwrap();
    assert_eq!(
        std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn failed_save_leaves_no_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    // The destination is a directory, so the final rename fails.
    std::fs::create_dir(dir.path().join("app-settings.json")).unwrap();
    let store = AppSettingsStore::new(dir.path());
    assert!(store.save(&AppearanceSettings::default()).is_err());
    assert_eq!(names(dir.path()), vec!["app-settings.json"]);
}

#[test]
fn stale_temp_file_does_not_break_save_and_is_not_touched() {
    let dir = tempfile::tempdir().unwrap();
    let stale = dir
        .path()
        .join(format!(".app-settings.json.{}.tmp", std::process::id()));
    std::fs::write(&stale, "stale").unwrap();
    std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644)).unwrap();
    let store = AppSettingsStore::new(dir.path());
    store.save(&AppearanceSettings::default()).unwrap();
    assert_eq!(std::fs::read_to_string(&stale).unwrap(), "stale");
    let mode = std::fs::metadata(dir.path().join("app-settings.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

mod runtime {
    use smithagents_app::appearance::AppearanceRuntime;
    use smithagents_app::settings::AppSettingsStore;
    use smithagents_theme::state::{AppearanceSettings, ThemeChoice};
    use smithagents_theme::tokens::{Appearance, ThemeSet};

    fn light() -> AppearanceSettings {
        AppearanceSettings {
            theme: ThemeChoice::Id("smith-light".into()),
            ..AppearanceSettings::default()
        }
    }

    #[test]
    fn it_starts_from_the_saved_settings_and_the_system_appearance() {
        let dir = tempfile::tempdir().unwrap();
        let store = AppSettingsStore::new(dir.path());
        let runtime =
            AppearanceRuntime::new(ThemeSet::embedded(), Some(store.clone()), Appearance::Light);
        assert_eq!(runtime.settings(), &AppearanceSettings::default());
        assert_eq!(
            runtime.state().theme,
            "smith-light",
            "System follows the OS"
        );

        store
            .save(&AppearanceSettings {
                font: "mono".into(),
                ..light()
            })
            .unwrap();
        let runtime = AppearanceRuntime::new(ThemeSet::embedded(), Some(store), Appearance::Dark);
        assert_eq!(
            runtime.state().theme,
            "smith-light",
            "an explicit theme wins"
        );
        assert_eq!(runtime.state().font, "mono");
    }

    #[test]
    fn a_change_is_resolved_and_saved() {
        let dir = tempfile::tempdir().unwrap();
        let store = AppSettingsStore::new(dir.path());
        let mut runtime =
            AppearanceRuntime::new(ThemeSet::embedded(), Some(store.clone()), Appearance::Dark);
        let change = runtime.update_settings(light());
        assert!(change.save_error.is_none());
        assert_eq!(change.state.unwrap().theme, "smith-light");
        assert_eq!(runtime.settings(), &light());
        assert_eq!(store.load(), light());
    }

    #[test]
    fn a_choice_that_resolves_to_the_same_theme_is_still_saved() {
        let dir = tempfile::tempdir().unwrap();
        let store = AppSettingsStore::new(dir.path());
        let mut runtime =
            AppearanceRuntime::new(ThemeSet::embedded(), Some(store.clone()), Appearance::Dark);
        let dark = AppearanceSettings {
            theme: ThemeChoice::Id("smith-dark".into()),
            ..AppearanceSettings::default()
        };
        let change = runtime.update_settings(dark.clone());
        assert!(
            change.state.is_none(),
            "System already resolved to Smith Dark"
        );
        assert_eq!(store.load(), dark, "but the explicit choice is remembered");
        // An unchanged choice writes nothing.
        std::fs::remove_file(dir.path().join("app-settings.json")).unwrap();
        let change = runtime.update_settings(dark);
        assert!(change.state.is_none() && change.save_error.is_none());
        assert!(!dir.path().join("app-settings.json").exists());
    }

    #[test]
    fn a_failed_save_is_reported_and_the_change_still_applies() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the home directory should be: every write fails.
        let home = dir.path().join("home");
        std::fs::write(&home, "not a directory").unwrap();
        let mut runtime = AppearanceRuntime::new(
            ThemeSet::embedded(),
            Some(AppSettingsStore::new(&home)),
            Appearance::Dark,
        );
        let change = runtime.update_settings(light());
        assert!(change.save_error.is_some());
        assert_eq!(change.state.unwrap().theme, "smith-light");
        assert_eq!(runtime.settings(), &light());
    }

    #[test]
    fn system_follows_the_os_and_an_explicit_theme_does_not() {
        let mut runtime = AppearanceRuntime::new(ThemeSet::embedded(), None, Appearance::Light);
        assert_eq!(
            runtime
                .set_system_appearance(Appearance::Dark)
                .unwrap()
                .theme,
            "smith-dark"
        );
        assert!(
            runtime.set_system_appearance(Appearance::Dark).is_none(),
            "no change"
        );
        runtime.update_settings(light());
        assert!(runtime.set_system_appearance(Appearance::Light).is_none());
        assert!(runtime.set_system_appearance(Appearance::Dark).is_none());
        assert_eq!(runtime.state().theme, "smith-light");
    }
}

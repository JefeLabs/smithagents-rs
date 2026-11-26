use std::os::unix::fs::PermissionsExt;

use smithagents_app::app_state::{AppState, AppStateStore};
use smithagents_app::view_models::SettingsSection;
use smithagents_app::views::rail::RailEntry;
use smithagents_core::ids::RepoId;

#[test]
fn the_last_repo_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let store = AppStateStore::new(dir.path());
    let id = RepoId::new();
    store
        .save(&AppState {
            last_repo_id: Some(id),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(store.load().last_repo_id, Some(id));

    let text = std::fs::read_to_string(dir.path().join("app-state.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["last_repo_id"], id.to_string());
}

#[test]
fn missing_empty_and_corrupt_files_load_as_nothing_remembered() {
    let dir = tempfile::tempdir().unwrap();
    let store = AppStateStore::new(dir.path());
    assert_eq!(store.load(), AppState::default());
    let file = dir.path().join("app-state.json");
    for text in [
        "",
        "   ",
        "{not json",
        "[]",
        "null",
        r#"{"last_repo_id": 7}"#,
    ] {
        std::fs::write(&file, text).unwrap();
        assert_eq!(store.load(), AppState::default(), "{text:?}");
    }
}

#[test]
fn an_invalid_id_is_forgotten_and_unknown_fields_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let store = AppStateStore::new(dir.path());
    let file = dir.path().join("app-state.json");
    std::fs::write(&file, r#"{"last_repo_id": "not-a-uuid"}"#).unwrap();
    assert_eq!(store.load().last_repo_id, None);

    let id = RepoId::new();
    std::fs::write(
        &file,
        format!(r#"{{"last_repo_id": "{id}", "from_a_newer_app": true}}"#),
    )
    .unwrap();
    assert_eq!(store.load().last_repo_id, Some(id));

    std::fs::write(&file, r#"{"last_repo_id": null}"#).unwrap();
    assert_eq!(store.load().last_repo_id, None);
}

#[test]
fn the_file_is_private_and_a_missing_home_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let store = AppStateStore::new(&home);
    store.save(&AppState::default()).unwrap();
    let mode = std::fs::metadata(home.join("app-state.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let names: Vec<String> = std::fs::read_dir(&home)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["app-state.json"], "no temp file left behind");
}

#[test]
fn collapsed_settings_sections_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = AppStateStore::new(dir.path());
    assert!(store.load().collapsed_sections.is_empty(), "all expanded");
    let id = RepoId::new();
    store
        .save(&AppState {
            last_repo_id: Some(id),
            collapsed_sections: vec![SettingsSection::Repositories],
            ..Default::default()
        })
        .unwrap();
    let loaded = store.load();
    assert_eq!(loaded.collapsed_sections, [SettingsSection::Repositories]);
    assert_eq!(loaded.last_repo_id, Some(id), "the other field survives");
    let text = std::fs::read_to_string(dir.path().join("app-state.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        json["collapsed_settings_sections"],
        serde_json::json!(["repositories"])
    );
}

#[test]
fn collapsed_sections_load_leniently() {
    let dir = tempfile::tempdir().unwrap();
    let store = AppStateStore::new(dir.path());
    let file = dir.path().join("app-state.json");
    let id = RepoId::new();
    for (text, want) in [
        (
            r#"{"collapsed_settings_sections": ["appearance", "agent-defaults", 3, "appearance"]}"#,
            vec![SettingsSection::Appearance],
        ),
        (r#"{"collapsed_settings_sections": "appearance"}"#, vec![]),
        (r#"{"collapsed_settings_sections": null}"#, vec![]),
    ] {
        std::fs::write(&file, text).unwrap();
        assert_eq!(store.load().collapsed_sections, want, "{text}");
    }
    // A bad sections value does not lose the repository.
    std::fs::write(
        &file,
        format!(r#"{{"last_repo_id": "{id}", "collapsed_settings_sections": {{}}}}"#),
    )
    .unwrap();
    let loaded = store.load();
    assert_eq!(loaded.last_repo_id, Some(id));
    assert!(loaded.collapsed_sections.is_empty());
}

#[test]
fn the_right_pane_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let store = AppStateStore::new(dir.path());
    assert_eq!(store.load().right_pane, None);
    let id = RepoId::new();
    store
        .save(&AppState {
            last_repo_id: Some(id),
            right_pane: Some(RailEntry::Intents),
            ..Default::default()
        })
        .unwrap();
    let loaded = store.load();
    assert_eq!(loaded.right_pane, Some(RailEntry::Intents));
    assert_eq!(loaded.last_repo_id, Some(id), "the other field survives");
    let text = std::fs::read_to_string(dir.path().join("app-state.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["right_pane"], "intents");

    store
        .save(&AppState {
            right_pane: Some(RailEntry::Settings),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(store.load().right_pane, Some(RailEntry::Settings));

    store
        .save(&AppState {
            right_pane: Some(RailEntry::Feedback),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(store.load().right_pane, Some(RailEntry::Feedback));
    let text = std::fs::read_to_string(dir.path().join("app-state.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["right_pane"], "feedback");
}

#[test]
fn an_unknown_or_missing_right_pane_loads_as_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = AppStateStore::new(dir.path());
    let file = dir.path().join("app-state.json");
    let id = RepoId::new();
    for text in [
        format!(r#"{{"last_repo_id": "{id}", "right_pane": "nope"}}"#),
        format!(r#"{{"last_repo_id": "{id}", "right_pane": 3}}"#),
        format!(r#"{{"last_repo_id": "{id}", "right_pane": null}}"#),
        format!(r#"{{"last_repo_id": "{id}"}}"#),
    ] {
        std::fs::write(&file, &text).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.right_pane, None, "{text}");
        assert_eq!(loaded.last_repo_id, Some(id), "{text}");
    }
}

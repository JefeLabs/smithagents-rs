use std::os::unix::fs::PermissionsExt;

use smithagents_app::app_state::{AppState, AppStateStore};
use smithagents_core::ids::RepoId;

#[test]
fn the_last_repo_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let store = AppStateStore::new(dir.path());
    let id = RepoId::new();
    store
        .save(&AppState {
            last_repo_id: Some(id),
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

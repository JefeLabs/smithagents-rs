use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use gpui_kit::component::dock::{DockAreaState, DockPlacement, DockState, PanelInfo, PanelState};
use gpui_kit::px;
use smithagents_app::layout_store::{
    saved_dock, LayoutStore, SavedDock, LAYOUT_FILE_NAME, LAYOUT_VERSION, MAX_DOCK_SIZE,
    MIN_DOCK_SIZE,
};

fn panel(name: &str) -> PanelState {
    PanelState {
        panel_name: name.into(),
        children: vec![],
        info: PanelInfo::panel(serde_json::Value::Null),
    }
}

fn state(left: Option<(f32, bool)>, right: Option<(f32, bool)>) -> DockAreaState {
    DockAreaState {
        version: Some(LAYOUT_VERSION),
        center: panel("Center"),
        left_dock: left.map(|(size, open)| {
            DockState::new(panel("Agents"), DockPlacement::Left, px(size), open)
        }),
        right_dock: right.map(|(size, open)| {
            DockState::new(panel("Settings"), DockPlacement::Right, px(size), open)
        }),
        bottom_dock: None,
    }
}

fn mode(path: &std::path::Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn round_trip_through_the_home() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(tmp.path());
    let saved = state(Some((260.0, true)), Some((320.0, false)));
    store.save(&saved).unwrap();
    assert_eq!(store.load(), Some(saved));
    assert_eq!(store.path(), tmp.path().join(LAYOUT_FILE_NAME));
    assert_eq!(LAYOUT_FILE_NAME, "app-layout.json");
}

#[test]
fn file_is_private_and_no_temp_file_is_left() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("new-home");
    let store = LayoutStore::new(&home);
    store.save(&state(Some((240.0, true)), None)).unwrap();
    assert_eq!(mode(&store.path()), 0o600);
    assert_eq!(mode(&home), 0o700, "a missing home is created owner-only");
    let names: Vec<_> = fs::read_dir(&home)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, vec![LAYOUT_FILE_NAME.to_string()]);
}

#[test]
fn missing_empty_corrupt_or_foreign_files_load_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(tmp.path());
    assert_eq!(store.load(), None, "missing");

    fs::write(store.path(), "").unwrap();
    assert_eq!(store.load(), None, "empty");

    fs::write(store.path(), "{ not json").unwrap();
    assert_eq!(store.load(), None, "corrupt");

    fs::write(store.path(), r#"{"something":"else"}"#).unwrap();
    assert_eq!(store.load(), None, "valid JSON of another shape");

    fs::remove_file(store.path()).unwrap();
    fs::create_dir(store.path()).unwrap();
    assert_eq!(store.load(), None, "a directory at the path");
}

#[test]
fn another_layout_version_loads_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(tmp.path());
    for version in [None, Some(LAYOUT_VERSION + 1), Some(0)] {
        let mut other = state(Some((240.0, true)), None);
        other.version = version;
        store.save(&other).unwrap();
        assert_eq!(store.load(), None, "version {version:?}");
    }
}

#[test]
fn saved_dock_reads_size_and_open_state() {
    let s = state(Some((260.0, false)), Some((320.0, true)));
    assert_eq!(
        saved_dock(&s, DockPlacement::Left),
        Some(SavedDock {
            size: px(260.0),
            open: false
        })
    );
    assert_eq!(
        saved_dock(&s, DockPlacement::Right),
        Some(SavedDock {
            size: px(320.0),
            open: true
        })
    );
    assert_eq!(saved_dock(&s, DockPlacement::Bottom), None);
    assert_eq!(saved_dock(&s, DockPlacement::Center), None);
    assert_eq!(saved_dock(&state(None, None), DockPlacement::Left), None);
}

#[test]
fn saved_dock_clamps_sizes_and_ignores_nonsense() {
    let tiny = state(Some((3.0, true)), Some((100_000.0, true)));
    assert_eq!(
        saved_dock(&tiny, DockPlacement::Left).map(|d| d.size),
        Some(MIN_DOCK_SIZE)
    );
    assert_eq!(
        saved_dock(&tiny, DockPlacement::Right).map(|d| d.size),
        Some(MAX_DOCK_SIZE)
    );

    let nan = state(Some((f32::NAN, false)), None);
    assert_eq!(
        saved_dock(&nan, DockPlacement::Left),
        None,
        "a non-finite size is not restored"
    );

    // A dock filed under the wrong side is not trusted.
    let mut swapped = state(None, None);
    swapped.left_dock = Some(DockState::new(
        panel("Settings"),
        DockPlacement::Right,
        px(300.0),
        true,
    ));
    assert_eq!(saved_dock(&swapped, DockPlacement::Left), None);
}

use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use gpui_kit::component::dock::{DockAreaState, DockPlacement, DockState, PanelInfo, PanelState};
use gpui_kit::{point, px, size, Bounds, Pixels, WindowBounds};
use smithagents_app::layout_store::{
    fit_side_docks, restored_window_bounds, saved_dock, saved_window, LayoutStore, SavedDock,
    LAYOUT_FILE_NAME, LAYOUT_VERSION, MAX_DOCK_SIZE, MIN_CENTER_WIDTH, MIN_DOCK_SIZE,
    WINDOW_FILE_NAME,
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

fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds {
        origin: point(px(x), px(y)),
        size: size(px(w), px(h)),
    }
}

#[test]
fn side_docks_that_fit_are_left_alone() {
    assert_eq!(
        fit_side_docks(px(240.), px(300.), px(1160.)),
        (px(240.), px(300.))
    );
    // Exactly the center minimum left over still fits.
    let room = f32::from(MIN_CENTER_WIDTH) + 540.0;
    assert_eq!(
        fit_side_docks(px(240.), px(300.), px(room)),
        (px(240.), px(300.))
    );
}

#[test]
fn side_docks_shrink_proportionally_to_keep_the_center() {
    let (left, right) = fit_side_docks(px(400.), px(400.), px(1000.));
    assert_eq!((left, right), (px(340.), px(340.)));
    assert!(px(1000.) - left - right >= MIN_CENTER_WIDTH);

    let (left, right) = fit_side_docks(px(600.), px(200.), px(840.));
    // 520 px for both, split 3:1.
    assert_eq!((left, right), (px(390.), px(130.)));
    assert!(px(840.) - left - right >= MIN_CENTER_WIDTH);
}

#[test]
fn side_docks_never_go_below_their_minimum() {
    let (left, right) = fit_side_docks(px(300.), px(300.), px(300.));
    assert_eq!((left, right), (MIN_DOCK_SIZE, MIN_DOCK_SIZE));
    let (left, right) = fit_side_docks(px(900.), px(100.), px(600.));
    assert!(left >= MIN_DOCK_SIZE && right >= MIN_DOCK_SIZE);
    assert!(px(600.) - left - right >= MIN_CENTER_WIDTH);
    assert_eq!(
        fit_side_docks(px(240.), px(300.), px(f32::NAN)),
        (px(240.), px(300.)),
        "an unknown width changes nothing"
    );
}

fn windowed(x: f32, y: f32, w: f32, h: f32) -> smithagents_app::layout_store::SavedWindow {
    saved_window(
        WindowBounds::Windowed(bounds(x, y, w, h + 32.)),
        size(px(w), px(h)),
        Some("display-1".into()),
    )
    .unwrap()
}

#[test]
fn a_window_is_saved_as_its_frame_origin_and_content_size() {
    // The platform reports the frame (title bar included) but opens windows
    // from a content size: saving the content size keeps relaunches stable.
    let saved = saved_window(
        WindowBounds::Windowed(bounds(93., 115., 1200., 792.)),
        size(px(1200.), px(760.)),
        Some("display-1".into()),
    )
    .unwrap();
    assert_eq!(saved.bounds, bounds(93., 115., 1200., 760.));
    assert_eq!(saved.display.as_deref(), Some("display-1"));
    assert!(!saved.maximized);
    assert_eq!(saved.version, LAYOUT_VERSION);

    let maximized = saved_window(
        WindowBounds::Maximized(bounds(0., 0., 1512., 950.)),
        size(px(1512.), px(918.)),
        None,
    )
    .unwrap();
    assert!(maximized.maximized);

    assert_eq!(
        saved_window(
            WindowBounds::Fullscreen(bounds(0., 0., 1000., 700.)),
            size(px(1512.), px(982.)),
            None
        ),
        None,
        "fullscreen keeps the last windowed save"
    );
}

#[test]
fn window_round_trip_and_leniency() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(tmp.path());
    assert_eq!(store.load_window(), None, "missing");
    let saved = windowed(10., 20., 1300., 800.);
    store.save_window(&saved).unwrap();
    assert_eq!(store.load_window(), Some(saved.clone()));
    assert_eq!(mode(&tmp.path().join(WINDOW_FILE_NAME)), 0o600);
    assert_eq!(WINDOW_FILE_NAME, "app-window.json");

    fs::write(tmp.path().join(WINDOW_FILE_NAME), "{ nope").unwrap();
    assert_eq!(store.load_window(), None, "corrupt");
    let mut other = saved;
    other.version = LAYOUT_VERSION + 1;
    store.save_window(&other).unwrap();
    assert_eq!(store.load_window(), None, "another version");
}

#[test]
fn a_saved_window_reopens_only_where_it_can_be_seen_and_grabbed() {
    // Bounds are relative to the saved display, whose own bounds start at 0,0.
    let display = Some(bounds(0., 0., 1512., 982.));
    let on_screen = windowed(100., 80., 1200., 760.);
    assert_eq!(
        restored_window_bounds(&on_screen, display),
        Some(WindowBounds::Windowed(bounds(100., 80., 1200., 760.)))
    );
    let mut maximized = on_screen.clone();
    maximized.maximized = true;
    assert_eq!(
        restored_window_bounds(&maximized, display),
        Some(WindowBounds::Maximized(bounds(100., 80., 1200., 760.)))
    );

    assert_eq!(
        restored_window_bounds(&on_screen, None),
        None,
        "the display is gone"
    );
    let beyond = windowed(3000., 80., 1200., 760.);
    assert_eq!(
        restored_window_bounds(&beyond, display),
        None,
        "past the display's edge (it got smaller)"
    );
    let sliver = windowed(-1150., 80., 1200., 760.);
    assert_eq!(
        restored_window_bounds(&sliver, display),
        None,
        "only a 50 px sliver is on screen"
    );
    let above = windowed(100., -750., 1200., 760.);
    assert_eq!(
        restored_window_bounds(&above, display),
        None,
        "the title bar is off the top"
    );
    let tiny = windowed(100., 80., 200., 100.);
    assert_eq!(restored_window_bounds(&tiny, display), None, "too small");
    let nan = windowed(f32::NAN, 80., 1200., 760.);
    assert_eq!(restored_window_bounds(&nan, display), None, "not finite");
}

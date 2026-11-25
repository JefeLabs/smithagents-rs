use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use gpui_kit::component::dock::{DockAreaState, DockPlacement, DockState, PanelInfo, PanelState};
use gpui_kit::{point, px, size, Bounds, Pixels, WindowBounds};
use smithagents_app::layout_store::{
    fitted_right_width, left_pane_extent, restored_window_bounds, saved_dock, saved_left_pane,
    saved_window, with_left_pane, LayoutStore, LeftPaneLayout, SavedDock, COMPACT_LEFT_PANE_WIDTH,
    DEFAULT_LEFT_PANE_WIDTH, DEFAULT_RIGHT_DOCK_SIZE, LAYOUT_FILE_NAME, LAYOUT_VERSION,
    MAX_DOCK_SIZE, MAX_LEFT_PANE_WIDTH, MIN_CENTER_WIDTH, MIN_DOCK_SIZE, MIN_LEFT_PANE_WIDTH,
    WINDOW_FILE_NAME,
};
use smithagents_app::view_models::LeftPaneMode;
use smithagents_app::views::rail::RAIL_WIDTH;

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

fn left(mode: LeftPaneMode, width: f32) -> LeftPaneLayout {
    LeftPaneLayout {
        mode,
        expanded_width: px(width),
    }
}

/// The left dock as older builds wrote it: a tab group holding the Agents
/// panel, with no mode of its own.
fn old_format(size: f32, open: bool) -> DockAreaState {
    let mut s = state(None, Some((300.0, true)));
    s.left_dock = Some(DockState::new(
        PanelState {
            panel_name: "TabPanel".into(),
            children: vec![panel("Agents")],
            info: PanelInfo::tabs(0),
        },
        DockPlacement::Left,
        px(size),
        open,
    ));
    s
}

#[test]
fn the_left_pane_defaults_to_expanded_at_the_default_width() {
    assert_eq!(
        LeftPaneLayout::default(),
        left(LeftPaneMode::Expanded, f32::from(DEFAULT_LEFT_PANE_WIDTH))
    );
}

#[test]
fn the_left_pane_mode_and_expanded_width_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(tmp.path());
    for pane in [
        left(LeftPaneMode::Compact, 310.0),
        left(LeftPaneMode::Expanded, 275.0),
    ] {
        let written = with_left_pane(state(None, Some((300.0, false))), pane);
        assert_eq!(saved_left_pane(&written), Some(pane));
        store.save(&written).unwrap();
        let loaded = store.load().expect("the file loads");
        assert_eq!(saved_left_pane(&loaded), Some(pane));
        // The right dock is untouched.
        assert_eq!(
            saved_dock(&loaded, DockPlacement::Right),
            Some(SavedDock {
                size: px(300.0),
                open: false
            })
        );
    }
    let text = fs::read_to_string(store.path()).unwrap();
    assert!(
        text.contains("\"mode\": \"expanded\""),
        "the mode is spelled out: {text}"
    );
}

#[test]
fn an_old_layout_with_the_left_dock_closed_restores_as_compact() {
    assert_eq!(
        saved_left_pane(&old_format(260.0, false)),
        Some(left(LeftPaneMode::Compact, 260.0)),
        "closed meant hidden; it is now compact, keeping the width"
    );
    assert_eq!(
        saved_left_pane(&old_format(260.0, true)),
        Some(left(LeftPaneMode::Expanded, 260.0))
    );
}

#[test]
fn a_bad_left_pane_entry_restores_nothing_or_a_clamped_width() {
    assert_eq!(saved_left_pane(&state(None, None)), None, "no left dock");
    assert_eq!(saved_left_pane(&old_format(f32::NAN, true)), None);
    assert_eq!(saved_left_pane(&old_format(f32::INFINITY, true)), None);
    for tiny in [3.0, 0.0, -50.0] {
        assert_eq!(
            saved_left_pane(&old_format(tiny, true)).map(|p| p.expanded_width),
            Some(MIN_LEFT_PANE_WIDTH),
            "{tiny}"
        );
    }
    assert_eq!(
        saved_left_pane(&old_format(99_999.0, false)).map(|p| p.expanded_width),
        Some(MAX_LEFT_PANE_WIDTH)
    );
    let mut wrong_side = old_format(260.0, true);
    let dock = wrong_side.left_dock.take().unwrap();
    wrong_side.left_dock = Some(DockState::new(
        dock.panel().clone(),
        DockPlacement::Right,
        dock.size(),
        dock.open(),
    ));
    assert_eq!(saved_left_pane(&wrong_side), None);

    // A mode this build does not know falls back to the open flag.
    let mut unknown = with_left_pane(state(None, None), left(LeftPaneMode::Compact, 260.0));
    let text = serde_json::to_string(&unknown)
        .unwrap()
        .replace("\"compact\"", "\"sideways\"");
    unknown = serde_json::from_str(&text).unwrap();
    assert_eq!(
        saved_left_pane(&unknown),
        Some(left(LeftPaneMode::Compact, 260.0))
    );
}

#[test]
fn the_left_pane_has_its_own_named_limits() {
    assert_eq!(COMPACT_LEFT_PANE_WIDTH, px(56.));
    assert_eq!(MIN_LEFT_PANE_WIDTH, px(100.));
    assert_eq!(MAX_LEFT_PANE_WIDTH, px(1000.));
    assert!(COMPACT_LEFT_PANE_WIDTH < MIN_LEFT_PANE_WIDTH);
    assert!(DEFAULT_LEFT_PANE_WIDTH > MIN_LEFT_PANE_WIDTH);
}

/// A window `available` wide beside the rail.
fn window(available: f32) -> Pixels {
    px(available + RAIL_WIDTH)
}

#[test]
fn the_right_dock_keeps_its_preferred_width_when_it_fits() {
    assert_eq!(
        fitted_right_width(px(300.), window(1000.), COMPACT_LEFT_PANE_WIDTH),
        px(300.)
    );
    // Exactly enough room is enough.
    let exact = 1000. - 56. - 320.;
    assert_eq!(
        fitted_right_width(px(exact), window(1000.), COMPACT_LEFT_PANE_WIDTH),
        px(exact)
    );
}

#[test]
fn the_right_dock_shrinks_to_keep_the_center_and_returns_with_room() {
    let compact = fitted_right_width(px(800.), window(1000.), COMPACT_LEFT_PANE_WIDTH);
    assert_eq!(
        compact,
        px(1000.) - COMPACT_LEFT_PANE_WIDTH - MIN_CENTER_WIDTH
    );
    let expanded = fitted_right_width(px(800.), window(1000.), px(400.));
    assert_eq!(expanded, px(1000. - 400.) - MIN_CENTER_WIDTH);
    assert!(expanded < compact);
    // The preferred width is an input, never changed: more room gives it back.
    assert_eq!(
        fitted_right_width(px(800.), window(2000.), px(400.)),
        px(800.)
    );
}

#[test]
fn the_right_dock_never_drops_below_its_minimum() {
    assert_eq!(
        fitted_right_width(px(800.), window(300.), COMPACT_LEFT_PANE_WIDTH),
        MIN_DOCK_SIZE
    );
    assert_eq!(
        fitted_right_width(px(600.), window(860.), px(600.)),
        MIN_DOCK_SIZE
    );
    // A preferred width below the minimum or above the maximum is clamped.
    assert_eq!(
        fitted_right_width(px(10.), window(2000.), COMPACT_LEFT_PANE_WIDTH),
        MIN_DOCK_SIZE
    );
    assert_eq!(
        fitted_right_width(px(5000.), window(9000.), COMPACT_LEFT_PANE_WIDTH),
        MAX_DOCK_SIZE
    );
}

#[test]
fn the_right_dock_fit_survives_non_finite_inputs() {
    // An unknown window or left width changes nothing.
    assert_eq!(
        fitted_right_width(px(300.), px(f32::NAN), px(240.)),
        px(300.)
    );
    assert_eq!(
        fitted_right_width(px(300.), window(1000.), px(f32::NAN)),
        px(300.)
    );
    assert_eq!(
        fitted_right_width(px(300.), px(f32::INFINITY), px(240.)),
        px(300.)
    );
    // An unknown preferred width falls back to the default.
    assert_eq!(
        fitted_right_width(px(f32::NAN), window(2000.), px(240.)),
        DEFAULT_RIGHT_DOCK_SIZE
    );
}

#[test]
fn a_cramped_window_narrows_only_the_drawn_left_pane() {
    // The right dock is at its minimum and the left pane is narrowed only
    // where it is drawn, its remembered 600 px kept for a larger window.
    let wide = left(LeftPaneMode::Expanded, 600.0);
    let right = fitted_right_width(px(600.), window(860.), wide.expanded_width);
    assert_eq!(right, MIN_DOCK_SIZE);
    let drawn = left_pane_extent(wide, right, px(860.));
    assert_eq!(drawn, px(860.) - right - MIN_CENTER_WIDTH);
    assert_eq!(wide.expanded_width, px(600.));
    assert_eq!(
        left_pane_extent(wide, right, px(2000.)),
        px(600.),
        "a larger window draws the remembered width again"
    );
}

#[test]
fn the_left_pane_is_never_zero_wide() {
    assert_eq!(
        left_pane_extent(left(LeftPaneMode::Compact, 300.0), px(300.), px(1160.)),
        COMPACT_LEFT_PANE_WIDTH
    );
    assert_eq!(
        left_pane_extent(left(LeftPaneMode::Expanded, 240.0), px(300.), px(1160.)),
        px(240.)
    );
    // Expanded gives way so the center keeps its minimum...
    assert_eq!(
        left_pane_extent(left(LeftPaneMode::Expanded, 600.0), px(300.), px(1160.)),
        px(1160.) - px(300.) - MIN_CENTER_WIDTH
    );
    // ...but never below the minimum, and never to nothing, whatever the
    // expanded width or the window.
    for expanded in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -50.0, 0.0] {
        let width = left_pane_extent(left(LeftPaneMode::Expanded, expanded), px(300.), px(1160.));
        assert!(
            width >= MIN_LEFT_PANE_WIDTH && f32::from(width).is_finite(),
            "expanded {expanded}: {width:?}"
        );
    }
    for available in [0.0, 50.0, 400.0, f32::NAN, f32::INFINITY] {
        for mode in [LeftPaneMode::Expanded, LeftPaneMode::Compact] {
            let width = left_pane_extent(left(mode, 240.0), px(300.), px(available));
            assert!(
                width >= COMPACT_LEFT_PANE_WIDTH && f32::from(width).is_finite(),
                "{mode:?} at {available}: {width:?}"
            );
        }
    }
}

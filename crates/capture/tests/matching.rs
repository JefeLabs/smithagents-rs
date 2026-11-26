//! Own-window matching, the capture check and the fake. Pure: nothing here
//! lists windows or captures the screen.

use smithagents_capture::{
    checked, pick_window, test_pattern, CaptureError, Capturer, FakeCapturer, Frame, Screenshot,
    WindowInfo, WindowTarget, EMPTY_CAPTURE, WRONG_SIZE_CAPTURE,
};

fn target(pid: u32, width: f32, height: f32) -> WindowTarget {
    WindowTarget {
        pid,
        frame: Frame {
            x: 100.0,
            y: 50.0,
            width,
            height,
        },
        scale: 2.0,
    }
}

fn app_window() -> WindowTarget {
    target(42, 1200.0, 760.0)
}

fn window(pid: u32, width: u32, height: u32) -> WindowInfo {
    WindowInfo {
        pid,
        width,
        height,
        minimized: false,
    }
}

fn gray(width: u32, height: u32) -> Screenshot {
    Screenshot {
        width,
        height,
        rgba: [128, 128, 128, 255].repeat((width * height) as usize),
    }
}

fn empty() -> Result<Screenshot, CaptureError> {
    Err(CaptureError::Failed(EMPTY_CAPTURE.into()))
}

#[test]
fn picks_the_window_of_this_process_with_the_matching_size() {
    let windows = [
        window(7, 1200, 760),
        window(42, 1200, 760),
        window(42, 300, 200),
    ];
    assert_eq!(pick_window(&windows, &app_window()), Some(1));
}

#[test]
fn the_closest_size_wins_and_the_first_on_a_tie() {
    let windows = [
        window(42, 1201, 761),
        window(42, 1200, 760),
        window(42, 1200, 760),
    ];
    assert_eq!(pick_window(&windows, &app_window()), Some(1));
}

#[test]
fn two_points_off_matches_and_three_does_not() {
    assert_eq!(
        pick_window(&[window(42, 1202, 758)], &app_window()),
        Some(0)
    );
    assert_eq!(pick_window(&[window(42, 1203, 760)], &app_window()), None);
}

#[test]
fn a_minimized_window_never_matches() {
    let minimized = WindowInfo {
        minimized: true,
        ..window(42, 1200, 760)
    };
    assert_eq!(pick_window(&[minimized], &app_window()), None);
    assert_eq!(pick_window(&[], &app_window()), None);
}

#[test]
fn an_empty_or_single_color_capture_is_refused() {
    let nothing = |width, height| Screenshot {
        width,
        height,
        rgba: Vec::new(),
    };
    assert_eq!(checked(nothing(0, 0)), empty());
    assert_eq!(checked(nothing(4, 0)), empty());
    assert_eq!(checked(gray(4, 4)), empty());

    let mut one_pixel_changed = gray(4, 4);
    one_pixel_changed.rgba[4 * 9] = 127;
    assert_eq!(checked(one_pixel_changed.clone()), Ok(one_pixel_changed));
}

#[test]
fn a_buffer_of_the_wrong_length_is_refused() {
    let short = Screenshot {
        width: 4,
        height: 4,
        rgba: vec![0; 60],
    };
    assert_eq!(
        checked(short),
        Err(CaptureError::Failed(WRONG_SIZE_CAPTURE.into()))
    );
}

#[test]
fn the_fake_records_each_target_and_answers_what_it_was_given() {
    let fake = FakeCapturer::new(Ok(test_pattern(3, 2)));
    let first = app_window();
    let second = target(42, 800.0, 600.0);

    assert_eq!(fake.capture_window(&first), Ok(test_pattern(3, 2)));
    assert_eq!(fake.capture_window(&second), Ok(test_pattern(3, 2)));
    assert_eq!(fake.calls(), vec![first, second]);

    fake.set(Err(CaptureError::PermissionDenied));
    assert_eq!(
        fake.capture_window(&first),
        Err(CaptureError::PermissionDenied)
    );
    assert_eq!(fake.calls(), vec![first, second, first]);

    let pattern = checked(test_pattern(2400, 1512)).expect("a test pattern is never empty");
    assert_eq!((pattern.width, pattern.height), (2400, 1512));
    assert_eq!(pattern.rgba.len(), 2400 * 1512 * 4);

    assert_eq!(CaptureError::Failed("x".into()).to_string(), "x");
}

#[test]
fn the_test_pattern_has_no_two_equal_neighbours() {
    let (width, height) = (300, 260);
    let shot = test_pattern(width, height);
    let pixel = |x: u32, y: u32| {
        let at = ((y * width + x) * 4) as usize;
        &shot.rgba[at..at + 4]
    };
    for y in 0..height {
        for x in 0..width {
            if x + 1 < width {
                assert_ne!(pixel(x, y), pixel(x + 1, y), "at ({x}, {y}) and its right");
            }
            if y + 1 < height {
                assert_ne!(pixel(x, y), pixel(x, y + 1), "at ({x}, {y}) and below");
            }
        }
    }
}

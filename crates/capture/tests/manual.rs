//! The one real capture. Never run by the gate: a person runs it by hand,
//! with the desktop app open, as described in the plan's Manual check.

use smithagents_capture::{checked, Capturer, Frame, WindowTarget, XcapCapturer};

const HOW_TO: &str = "set SMITHAGENTS_CAPTURE_PID to the desktop app's process id and \
     SMITHAGENTS_CAPTURE_SIZE to its window size in points, such as 1200x760, then run \
     `cargo test -p smithagents-capture --test manual -- --ignored --nocapture`";

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set: {HOW_TO}"))
}

#[test]
#[ignore = "manual: captures the screen and may show the Screen Recording prompt; see docs/superpowers/plans/2026-11-26-feedback-capture.md, Manual check"]
fn real_capture_of_a_running_app_window() {
    let pid: u32 = env("SMITHAGENTS_CAPTURE_PID")
        .parse()
        .unwrap_or_else(|_| panic!("SMITHAGENTS_CAPTURE_PID is not a number: {HOW_TO}"));
    let size = env("SMITHAGENTS_CAPTURE_SIZE");
    let (width, height) = size
        .split_once('x')
        .and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)))
        .unwrap_or_else(|| panic!("SMITHAGENTS_CAPTURE_SIZE is not WIDTHxHEIGHT: {HOW_TO}"));
    // The capturer matches on the process and the size only; the origin and
    // the scale are not used to find the window.
    let target = WindowTarget {
        pid,
        frame: Frame {
            x: 0.0,
            y: 0.0,
            width,
            height,
        },
        scale: 1.0,
    };

    let shot = XcapCapturer::new()
        .capture_window(&target)
        .and_then(checked)
        .unwrap_or_else(|error| panic!("the capture failed: {error:?} ({error})"));

    let dir = tempfile::tempdir().expect("a temporary directory").keep();
    let path = dir.join("capture.png");
    image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba)
        .expect("checked guarantees the buffer length")
        .save(&path)
        .expect("the PNG is written");
    println!(
        "wrote {} ({} x {} pixels)",
        path.display(),
        shot.width,
        shot.height
    );
}

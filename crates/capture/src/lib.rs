//! Window capture for feedback: a small [`Capturer`] trait, the real
//! [`XcapCapturer`], a [`FakeCapturer`] for tests, and the pure rules for
//! finding the app's own window ([`pick_window`]) and refusing a capture that
//! is not a usable picture ([`checked`]).

mod fake;
mod xcap_capturer;

pub use fake::{test_pattern, FakeCapturer};
pub use xcap_capturer::XcapCapturer;

/// A window's frame in screen points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// The window to capture: the process that owns it, its frame, and its
/// backing scale (pixels per point).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowTarget {
    pub pid: u32,
    pub frame: Frame,
    pub scale: f32,
}

/// A captured picture: 4 bytes (RGBA) per pixel, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screenshot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Why a capture produced no picture.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("screen recording permission is not granted")]
    PermissionDenied,
    #[error("the window to capture was not found")]
    WindowNotFound,
    #[error("capturing a window is not supported on this system")]
    Unsupported,
    #[error("{0}")]
    Failed(String),
}

/// Takes a picture of one window.
pub trait Capturer: Send + Sync {
    fn capture_window(&self, target: &WindowTarget) -> Result<Screenshot, CaptureError>;
}

/// How far, in points, a listed window's width and height may each be from
/// the target's frame and still be the target.
pub const SIZE_TOLERANCE_POINTS: f32 = 2.0;
/// The [`CaptureError::Failed`] message for a picture with nothing on it.
pub const EMPTY_CAPTURE: &str = "the capture came back empty";
/// The [`CaptureError::Failed`] message for a buffer that does not match its size.
pub const WRONG_SIZE_CAPTURE: &str = "the capture has the wrong size";

/// One listed window, as [`pick_window`] sees it. Width and height are in points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub pid: u32,
    pub width: u32,
    pub height: u32,
    pub minimized: bool,
}

/// Index of the window to capture: same pid, not minimized, width and height each within
/// SIZE_TOLERANCE_POINTS of the target's frame; the smallest |dw| + |dh| wins, the earliest on a tie.
pub fn pick_window(windows: &[WindowInfo], target: &WindowTarget) -> Option<usize> {
    windows
        .iter()
        .enumerate()
        .filter(|(_, window)| window.pid == target.pid && !window.minimized)
        .filter_map(|(index, window)| {
            let dw = (window.width as f32 - target.frame.width).abs();
            let dh = (window.height as f32 - target.frame.height).abs();
            // Written so that a frame that is not a number matches nothing.
            (dw <= SIZE_TOLERANCE_POINTS && dh <= SIZE_TOLERANCE_POINTS).then_some((index, dw + dh))
        })
        // `min_by` keeps the first of equal minimums: the earliest window wins a tie.
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(index, _)| index)
}

/// Failed(WRONG_SIZE_CAPTURE) when rgba.len() != width * height * 4; Failed(EMPTY_CAPTURE) when
/// width or height is 0 or every pixel equals the first one; otherwise the screenshot unchanged.
pub fn checked(shot: Screenshot) -> Result<Screenshot, CaptureError> {
    let expected_len = (shot.width as usize)
        .checked_mul(shot.height as usize)
        .and_then(|pixels| pixels.checked_mul(4));
    if expected_len != Some(shot.rgba.len()) {
        return Err(CaptureError::Failed(WRONG_SIZE_CAPTURE.into()));
    }
    // The length is a whole number of pixels, so nothing is left over.
    let (pixels, _) = shot.rgba.as_chunks::<4>();
    let has_two_colors = pixels
        .split_first()
        .is_some_and(|(first, rest)| rest.iter().any(|pixel| pixel != first));
    if !has_two_colors {
        return Err(CaptureError::Failed(EMPTY_CAPTURE.into()));
    }
    Ok(shot)
}

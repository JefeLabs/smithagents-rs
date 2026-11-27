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

/// One listed window, as [`pick_window`] sees it. Position, width and height are in points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub pid: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub minimized: bool,
}

/// Index of the window to capture: same pid, not minimized, width and height each within
/// SIZE_TOLERANCE_POINTS of the target's frame. Among those, the smallest |dw| + |dh| wins,
/// then the one nearest the target's origin, then the earliest.
///
/// The position only orders windows that already match; it never excludes one: GPUI's
/// origin on macOS is relative to the window's own screen, so on a second display it
/// disagrees with the system's window list.
pub fn pick_window(windows: &[WindowInfo], target: &WindowTarget) -> Option<usize> {
    windows
        .iter()
        .enumerate()
        .filter(|(_, window)| window.pid == target.pid && !window.minimized)
        .filter_map(|(index, window)| {
            let dw = (window.width as f32 - target.frame.width).abs();
            let dh = (window.height as f32 - target.frame.height).abs();
            let dx = window.x as f32 - target.frame.x;
            let dy = window.y as f32 - target.frame.y;
            // Written so that a frame that is not a number matches nothing.
            (dw <= SIZE_TOLERANCE_POINTS && dh <= SIZE_TOLERANCE_POINTS).then_some((
                index,
                dw + dh,
                dx.hypot(dy),
            ))
        })
        // `min_by` keeps the first of equal minimums: the earliest window wins a tie.
        .min_by(|(_, size_a, place_a), (_, size_b, place_b)| {
            size_a
                .total_cmp(size_b)
                .then_with(|| place_a.total_cmp(place_b))
        })
        .map(|(index, _, _)| index)
}

/// Failed(WRONG_SIZE_CAPTURE) when rgba.len() != width * height * 4; Failed(EMPTY_CAPTURE) when
/// no pixel is fully opaque (or there are no pixels) or every fully opaque pixel has the color
/// of the first one; otherwise the screenshot unchanged.
///
/// Only fully opaque pixels (alpha 255) count, compared by color: a real window capture has
/// rounded corners, transparent and then anti-aliased (partly opaque, of other colors), which
/// would make a window of one color look like a picture.
pub fn checked(shot: Screenshot) -> Result<Screenshot, CaptureError> {
    let expected_len = (shot.width as usize)
        .checked_mul(shot.height as usize)
        .and_then(|pixels| pixels.checked_mul(4));
    if expected_len != Some(shot.rgba.len()) {
        return Err(CaptureError::Failed(WRONG_SIZE_CAPTURE.into()));
    }
    // The length is a whole number of pixels, so nothing is left over.
    let (pixels, _) = shot.rgba.as_chunks::<4>();
    let mut opaque = pixels
        .iter()
        .filter(|pixel| pixel[3] == u8::MAX)
        .map(|pixel| &pixel[..3]);
    let has_two_colors = opaque
        .next()
        .is_some_and(|first| opaque.any(|color| color != first));
    if !has_two_colors {
        return Err(CaptureError::Failed(EMPTY_CAPTURE.into()));
    }
    Ok(shot)
}

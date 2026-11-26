//! The real [`Capturer`]: the only code that touches xcap and the macOS
//! Screen Recording permission. No automated test runs it (see the manual
//! test in `tests/manual.rs`).

use crate::{pick_window, CaptureError, Capturer, Screenshot, WindowInfo, WindowTarget};

/// Captures a window of this machine's screen with the `xcap` library.
#[derive(Debug, Default)]
pub struct XcapCapturer;

impl XcapCapturer {
    pub fn new() -> Self {
        Self
    }
}

/// Whether this process already asked macOS for Screen Recording permission.
/// The request shows the system prompt, so it is made at most once per run.
#[cfg(target_os = "macos")]
static PERMISSION_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

impl Capturer for XcapCapturer {
    fn capture_window(&self, target: &WindowTarget) -> Result<Screenshot, CaptureError> {
        #[cfg(target_os = "macos")]
        if !objc2_core_graphics::CGPreflightScreenCaptureAccess() {
            use std::sync::atomic::Ordering;
            if !PERMISSION_REQUESTED.swap(true, Ordering::Relaxed) {
                objc2_core_graphics::CGRequestScreenCaptureAccess();
            }
            return Err(CaptureError::PermissionDenied);
        }

        let (windows, infos): (Vec<xcap::Window>, Vec<WindowInfo>) = xcap::Window::all()
            .map_err(from_xcap)?
            .into_iter()
            .filter_map(|window| {
                let info = WindowInfo {
                    pid: window.pid().ok()?,
                    width: window.width().ok()?,
                    height: window.height().ok()?,
                    minimized: window.is_minimized().ok()?,
                };
                Some((window, info))
            })
            .unzip();
        let index = pick_window(&infos, target).ok_or(CaptureError::WindowNotFound)?;
        let image = windows[index].capture_image().map_err(from_xcap)?;
        Ok(Screenshot {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        })
    }
}

fn from_xcap(error: xcap::XCapError) -> CaptureError {
    match error {
        xcap::XCapError::NotSupported => CaptureError::Unsupported,
        other => CaptureError::Failed(other.to_string()),
    }
}

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

        let mut windows = Vec::new();
        let mut infos = Vec::new();
        for window in xcap::Window::all().map_err(from_xcap)? {
            // The process first: another process's window is skipped before
            // anything else is read from it. One whose process cannot be read
            // is not known to be ours.
            let Ok(pid) = window.pid() else {
                continue;
            };
            if pid != target.pid {
                continue;
            }
            // Ours: a value that cannot be read is a failure, not a skip.
            let info = our_window(&window, pid)
                .map_err(|error| CaptureError::Failed(error.to_string()))?;
            windows.push(window);
            infos.push(info);
        }
        let index = pick_window(&infos, target).ok_or_else(|| {
            tracing::debug!(frame = ?target.frame, windows = ?infos, "feedback capture: no window matches");
            CaptureError::WindowNotFound
        })?;
        tracing::debug!(
            frame = ?target.frame,
            window = ?infos[index],
            "feedback capture: the window to capture"
        );
        let image = windows[index].capture_image().map_err(from_xcap)?;
        Ok(Screenshot {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        })
    }
}

/// What [`pick_window`] needs of one of this process's windows.
fn our_window(window: &xcap::Window, pid: u32) -> Result<WindowInfo, xcap::XCapError> {
    Ok(WindowInfo {
        pid,
        x: window.x()?,
        y: window.y()?,
        width: window.width()?,
        height: window.height()?,
        // macOS lists on-screen windows only: a minimized one is never listed.
        #[cfg(target_os = "macos")]
        minimized: false,
        #[cfg(not(target_os = "macos"))]
        minimized: window.is_minimized()?,
    })
}

fn from_xcap(error: xcap::XCapError) -> CaptureError {
    match error {
        xcap::XCapError::NotSupported => CaptureError::Unsupported,
        other => CaptureError::Failed(other.to_string()),
    }
}

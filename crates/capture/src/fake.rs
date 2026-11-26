//! A [`Capturer`] for tests: answers what it was given and records every
//! target it was asked for. It never touches the screen.

use std::sync::{Mutex, MutexGuard};

use crate::{CaptureError, Capturer, Screenshot, WindowTarget};

pub struct FakeCapturer {
    state: Mutex<FakeState>,
}

struct FakeState {
    answer: Result<Screenshot, CaptureError>,
    calls: Vec<WindowTarget>,
}

impl FakeCapturer {
    pub fn new(answer: Result<Screenshot, CaptureError>) -> Self {
        Self {
            state: Mutex::new(FakeState {
                answer,
                calls: Vec::new(),
            }),
        }
    }

    /// The answer to every later call.
    pub fn set(&self, answer: Result<Screenshot, CaptureError>) {
        self.state().answer = answer;
    }

    /// Every target asked for, in order.
    pub fn calls(&self) -> Vec<WindowTarget> {
        self.state().calls.clone()
    }

    fn state(&self) -> MutexGuard<'_, FakeState> {
        // A test that panicked while holding the lock left valid data behind.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Capturer for FakeCapturer {
    fn capture_window(&self, target: &WindowTarget) -> Result<Screenshot, CaptureError> {
        let mut state = self.state();
        state.calls.push(*target);
        state.answer.clone()
    }
}

/// A width × height picture with no two neighbouring pixels equal (never "empty").
///
/// Red counts the column and green the row (each modulo 256), so horizontal
/// neighbours differ in red and vertical ones in green. A picture of one
/// pixel, or of none, is still refused by [`crate::checked`].
pub fn test_pattern(width: u32, height: u32) -> Screenshot {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 128, 255]);
        }
    }
    Screenshot {
        width,
        height,
        rgba,
    }
}

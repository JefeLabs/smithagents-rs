//! The update seam: what the status bar's update button talks to.
//!
//! [`Updater`] is the app's only view of updating itself, mirroring the
//! [`AgentCommands`](crate::link::AgentCommands) seam: object-safe, futures
//! that are `'static` and need not be `Send` (the shell awaits them on
//! GPUI's foreground executor), so tests inject a fake through
//! [`ShellOptions`](crate::shell::ShellOptions).
//!
//! There is no real updater yet. A real one needs a release channel to ask
//! for the newest version, an app bundle to replace, and signature
//! verification of the download before it is installed; none of these exist
//! today. [`NoUpdater`] is what the app runs with: it always reports
//! [`UpdateState::UpToDate`], so the update chip (in the title bar) never
//! appears in production builds.
//!
//! What a real implementation must do:
//! - Bound `check` and `install` with its own timeouts; the shell does not
//!   time them out, and an install that never answers leaves the chip on
//!   "Installing…".
//! - Be cancel-safe: the shell may drop a future (a replaced updater, the
//!   window closing), and dropping it must never leave a half-installed app.
//! - Verify the download's signature before replacing anything.

use futures::future::LocalBoxFuture;
use futures::FutureExt as _;

/// Where the app stands with updates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpdateState {
    /// No newer version is known.
    #[default]
    UpToDate,
    /// `version` can be installed.
    Available { version: String },
    /// `version` is being installed.
    Installing { version: String },
    /// Installing `version` failed with `message`; it can be retried.
    Failed { version: String, message: String },
    /// `version` is installed; the app must restart to run it. Final for this
    /// run: no further check or install happens.
    RestartRequired { version: String },
}

/// Detects and installs updates.
pub trait Updater {
    /// Looks for a newer version.
    fn check(&self) -> LocalBoxFuture<'static, UpdateState>;
    /// Installs `version` (as reported by [`Updater::check`]). The text of an
    /// error is shown to the user.
    fn install(&self, version: String) -> LocalBoxFuture<'static, Result<(), String>>;
}

/// The production updater until a real one exists: never finds an update.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoUpdater;

impl Updater for NoUpdater {
    fn check(&self) -> LocalBoxFuture<'static, UpdateState> {
        async { UpdateState::UpToDate }.boxed_local()
    }

    fn install(&self, _version: String) -> LocalBoxFuture<'static, Result<(), String>> {
        async { Err("updates are not available in this build".to_owned()) }.boxed_local()
    }
}

//! Typed client for the SmithAgents daemon, and find-or-start.
//!
//! [`ensure_daemon`] finds the daemon for a home directory through its
//! lockfile, starting one when none is running, and returns a [`Client`] for it.

mod api;
mod ensure;

pub use api::{Client, ClientError, FeedbackFile, Health};
pub use ensure::{ensure_daemon, ensure_daemon_with, EnsureError, SpawnOptions};

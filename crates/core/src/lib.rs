pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The wire protocol between the daemon and its clients. The daemon reports
/// it in `GET /v1/health`, and a client refuses a daemon that reports another
/// number, even one of the same [`VERSION`]: every build carries the same
/// version, and a daemon outlives the app that started it, so the version
/// alone cannot tell a daemon built from other code.
///
/// Raise it by one, by hand, with any change after which a peer built before
/// the change and a peer built after it no longer understand each other: a
/// route or event type the other side relies on, a new variant of an enum on
/// the wire, a field that is no longer optional, a changed meaning. A field
/// the older side can ignore or default does not need it.
pub const PROTOCOL: u32 = 2;

pub mod agent;
pub mod daemon;
pub mod event;
pub mod feedback;
pub mod ids;
pub mod intent;
pub mod palette;
pub mod plugin;

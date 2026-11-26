//! Finding plugins on disk, building a runtime for each, and running their
//! syncs; and the built-in plugins compiled into the daemon.

mod discovery;
mod embedded;
mod manager;
mod settings;

pub use discovery::{builtin_plugins_dir, discover, load_runtime, Discovered, Origin, Rejected};
pub use embedded::{embedded_plugins, FEEDBACK_PLUGIN_ID};
pub(crate) use manager::clean_message;
#[cfg(feature = "test-util")]
pub use manager::WallClock;
pub use manager::{LoadedPlugin, ManagerConfig, ManagerError, PluginManager};

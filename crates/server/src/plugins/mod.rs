//! Finding plugins on disk, building a runtime for each, and running their syncs.

mod discovery;
mod manager;
mod settings;

pub use discovery::{builtin_plugins_dir, discover, load_runtime, Discovered, Origin, Rejected};
#[cfg(feature = "test-util")]
pub use manager::WallClock;
pub use manager::{LoadedPlugin, ManagerConfig, ManagerError, PluginManager};

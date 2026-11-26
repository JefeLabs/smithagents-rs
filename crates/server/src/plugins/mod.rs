//! Finding plugins on disk and building a runtime for each.

mod discovery;

pub use discovery::{builtin_plugins_dir, discover, load_runtime, Discovered, Origin, Rejected};

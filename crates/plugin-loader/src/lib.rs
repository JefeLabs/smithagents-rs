//! Loads a built-in plugin from a native library: the only code in the
//! workspace that needs `unsafe`, kept to opening the library and fetching
//! its entry. Everything else crosses the boundary through stabby's checked
//! interfaces in `smithagents_plugin_protocol::dylib`.
//!
//! A loaded library is never unloaded: the plugin object and anything it
//! hands out point into the library's code.

use std::error::Error;
use std::path::{Path, PathBuf};

use libloading::Library;
use smithagents_plugin_protocol::dylib::{
    BoxedDylibHost, BoxedDylibPlugin, DylibEntry, DylibPluginDyn, DYLIB_ENTRY_SYMBOL,
};
use stabby::libloading::StabbyLibrary;

/// Why a library could not be loaded as a plugin. Messages are one line.
#[derive(Debug, thiserror::Error)]
pub enum DylibLoadError {
    /// The file is missing or is not a library for this platform.
    #[error("cannot load {} as a library: {message}", path.display())]
    Open { path: PathBuf, message: String },
    /// The library does not export the plugin entry.
    #[error("{} has no plugin entry `{}`: {message}", path.display(), entry_name())]
    NoEntry { path: PathBuf, message: String },
    /// The entry's type, as stabby reports it, differs from this daemon's:
    /// the library was built against another plugin interface.
    #[error(
        "{}: the plugin entry `{}` does not match this daemon's plugin interface \
         (stabby report mismatch); rebuild the plugin with this version",
        path.display(),
        entry_name()
    )]
    Mismatch { path: PathBuf },
}

fn entry_name() -> &'static str {
    std::str::from_utf8(DYLIB_ENTRY_SYMBOL).unwrap_or("?")
}

/// The plugin object of a loaded library.
pub struct LoadedPlugin {
    plugin: BoxedDylibPlugin,
}

/// Opens `path`, checks its entry against [`DylibEntry`] and calls it with
/// `host`. Only built-in plugins, which ship beside the daemon, may be loaded
/// this way: opening a library runs its code in the daemon's process.
pub fn load(path: &Path, host: BoxedDylibHost) -> Result<LoadedPlugin, DylibLoadError> {
    // SAFETY: opening a library runs its initialisers in this process. The
    // caller passes only built-in plugins, built from this workspace against
    // the same interface crate. The library is leaked right after, never
    // unloaded, so code and data it hands out stay valid for the process.
    #[allow(unsafe_code)]
    let library = unsafe { Library::new(path) }.map_err(|e| DylibLoadError::Open {
        path: path.to_path_buf(),
        message: describe(&e),
    })?;
    let library: &'static Library = Box::leak(Box::new(library));

    // SAFETY: stabby looks up the entry's `_stabbied_v3` companion, which
    // compares the type report compiled into the library with `DylibEntry`'s
    // report here and returns the function only when they agree, so the
    // pointer has the type we call it with. The leaked library outlives it.
    #[allow(unsafe_code)]
    let entry = unsafe { library.get_stabbied::<DylibEntry>(DYLIB_ENTRY_SYMBOL) }.map_err(|e| {
        let path = path.to_path_buf();
        // A missing symbol is libloading's error; a mismatch is stabby's own
        // message, which holds both type reports (kilobytes) and is dropped.
        match e.downcast_ref::<libloading::Error>() {
            Some(e) => DylibLoadError::NoEntry {
                path,
                message: describe(e),
            },
            None => DylibLoadError::Mismatch { path },
        }
    })?;
    // The plugin side of `export_dylib_plugin!` catches panics in the entry.
    let plugin = entry(host);
    Ok(LoadedPlugin { plugin })
}

impl LoadedPlugin {
    /// One call: a JSON `PluginCall` in, a JSON `PluginReply` out. It blocks
    /// until the plugin answers.
    pub fn call(&self, input: Vec<u8>) -> Vec<u8> {
        let reply = self.plugin.call(stabby::vec::Vec::from(input.as_slice()));
        reply.as_slice().to_vec()
    }
}

/// The error and its causes on one line: libloading keeps the system's
/// message in the cause, and that message can span lines.
fn describe(err: &dyn Error) -> String {
    let mut text = err.to_string();
    let mut cause = err.source();
    while let Some(err) = cause {
        text.push_str(": ");
        text.push_str(&err.to_string());
        cause = err.source();
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

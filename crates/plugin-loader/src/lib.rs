//! Loads a built-in plugin from a native library: the only hand-written
//! `unsafe` in the workspace, kept to opening the library and fetching its
//! entry. Everything else crosses the boundary through stabby's checked
//! interfaces in `smithagents_plugin_protocol::dylib`. (stabby's and
//! wit-bindgen's macros also expand to `unsafe` in the crates that use them;
//! the workspace's `forbid(unsafe_code)` does not see macro output.)
//!
//! Opening a library runs its code in this process, so which file may be
//! opened is decided here, beside the `unsafe`, and not left to callers:
//! [`load`] opens only what lies inside [`builtin_dir`]. [`load_unchecked`]
//! opens any path and is `unsafe` for that reason.
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
    /// The file is not inside the plugins folder beside the running
    /// executable. It was not opened.
    #[error(
        "{} is not in the plugins folder beside this executable; a native plugin is loaded only from there",
        path.display()
    )]
    NotBuiltin { path: PathBuf },
}

fn entry_name() -> &'static str {
    std::str::from_utf8(DYLIB_ENTRY_SYMBOL).unwrap_or("?")
}

/// The plugin object of a loaded library.
pub struct LoadedPlugin {
    plugin: BoxedDylibPlugin,
}

/// `<directory of the running executable>/plugins`, with the executable's
/// links resolved: the one folder native plugins are loaded from. Whoever
/// can write there can replace the executable itself, so what lies in it is
/// trusted as much as the executable is. `None` when the executable's path
/// cannot be found.
pub fn builtin_dir() -> Option<PathBuf> {
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    Some(exe.parent()?.join("plugins"))
}

/// Opens the library at `path`, checks its entry against [`DylibEntry`] and
/// calls it with `host`, if the file lies inside [`builtin_dir`] once every
/// link in `path` is resolved. Anything else is refused unopened
/// ([`DylibLoadError::NotBuiltin`]): opening a library runs its code in this
/// process.
///
/// # Errors
///
/// [`DylibLoadError::Open`] for a file that is missing or is not a library,
/// `NotBuiltin` for one outside the plugins folder, `NoEntry` and `Mismatch`
/// for a library that is not a plugin built against this interface.
pub fn load(path: &Path, host: BoxedDylibHost) -> Result<LoadedPlugin, DylibLoadError> {
    let real = std::fs::canonicalize(path).map_err(|e| DylibLoadError::Open {
        path: path.to_path_buf(),
        message: describe(&e),
    })?;
    let builtin = builtin_dir().and_then(|dir| std::fs::canonicalize(dir).ok());
    if !builtin.is_some_and(|dir| real.starts_with(dir)) {
        return Err(DylibLoadError::NotBuiltin {
            path: path.to_path_buf(),
        });
    }
    // SAFETY: `real` is a file inside the plugins folder that ships beside
    // this executable, and nothing in its path is a link to elsewhere. Only
    // who can replace the executable can put a file there, and what ships
    // there is built from this workspace against this interface crate.
    #[expect(
        unsafe_code,
        reason = "a library inside the executable's own plugins folder"
    )]
    unsafe {
        load_unchecked(&real, host)
    }
}

/// [`load`] without the check of where the file is.
///
/// # Safety
///
/// `path` must name a library built against this build of
/// `smithagents-plugin-protocol`, from code that is trusted as much as this
/// process: opening it runs its initialisers here, and its entry is called.
///
/// # Errors
///
/// As [`load`], without `NotBuiltin`.
#[expect(unsafe_code, reason = "opening a library cannot be done without it")]
pub unsafe fn load_unchecked(
    path: &Path,
    host: BoxedDylibHost,
) -> Result<LoadedPlugin, DylibLoadError> {
    // SAFETY: opening a library runs its initialisers in this process, which
    // the caller has vouched for. The library is leaked right after, never
    // unloaded, so code and data it hands out stay valid for the process.
    #[expect(unsafe_code, reason = "opening a library the caller vouches for")]
    let library = unsafe { Library::new(path) }.map_err(|e| DylibLoadError::Open {
        path: path.to_path_buf(),
        message: describe(&e),
    })?;
    let library: &'static Library = Box::leak(Box::new(library));

    // SAFETY: stabby looks up the entry's `_stabbied_v3` companion, which
    // compares the type report compiled into the library with `DylibEntry`'s
    // report here and returns the function only when they agree, so the
    // pointer has the type we call it with. The leaked library outlives it.
    #[expect(unsafe_code, reason = "fetching the entry stabby has checked")]
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

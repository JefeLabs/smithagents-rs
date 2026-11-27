//! Loads a built-in plugin from a native library: the only hand-written
//! `unsafe` in the workspace, kept to opening the library and fetching its
//! entry. Everything else crosses the boundary through stabby's checked
//! interfaces in `smithagents_plugin_protocol::dylib`. (stabby's and
//! wit-bindgen's macros also expand to `unsafe` in the crates that use them;
//! the workspace's `forbid(unsafe_code)` does not see macro output.)
//!
//! Opening a library runs its code in this process, so which file may be
//! opened is decided here, beside the `unsafe`, and not left to callers:
//! [`load`] opens only what lies inside [`builtin_dir`], and only when no
//! other user could have written the file or a folder on the way to it.
//! [`load_unchecked`] opens any path and is `unsafe` for that reason.
//!
//! What this cannot decide is the process's environment. On macOS a
//! `DYLD_LIBRARY_PATH` makes the system's loader look there first for a file
//! of the same name. Whoever sets the daemon's environment already chooses
//! what code it runs (`DYLD_INSERT_LIBRARIES`), so the environment is trusted
//! as the process is.
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
    /// The library, or a folder between it and the plugins folder, could
    /// have been written by another user. Nothing was opened.
    #[error("{} {reason}; a native plugin is not loaded from there", path.display())]
    Untrusted {
        /// The file or folder that is not trusted, which may be a folder
        /// above the library.
        path: PathBuf,
        reason: &'static str,
    },
}

fn entry_name() -> &'static str {
    std::str::from_utf8(DYLIB_ENTRY_SYMBOL).unwrap_or("?")
}

/// The plugin object of a loaded library.
pub struct LoadedPlugin {
    plugin: BoxedDylibPlugin,
}

/// `<directory of the running executable>/plugins`, with the executable's
/// links resolved: the one folder native plugins are loaded from. `None`
/// when the executable's path cannot be found.
///
/// The path is all this says. [`load`] opens a file there only when the
/// folder is a real folder, not a link, and neither it nor anything between
/// it and the file could have been written by another user.
pub fn builtin_dir() -> Option<PathBuf> {
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    Some(exe.parent()?.join("plugins"))
}

/// Opens the library at `path`, checks its entry against [`DylibEntry`] and
/// calls it with `host`, if the file may be trusted with that: opening a
/// library runs its code in this process. With every link in `path`
/// resolved, the file must lie inside [`builtin_dir`], which must be a real
/// folder and not a link; and the file, that folder and every folder between
/// them must belong to this user or to root and be writable by nobody else.
/// Anything else is refused unopened.
///
/// # Errors
///
/// [`DylibLoadError::Open`] for a file that is missing or is not a library,
/// `NotBuiltin` for one outside the plugins folder, `Untrusted` for one
/// another user could have written, `NoEntry` and `Mismatch` for a library
/// that is not a plugin built against this interface.
pub fn load(path: &Path, host: BoxedDylibHost) -> Result<LoadedPlugin, DylibLoadError> {
    let real = trusted_library(builtin_dir().as_deref(), path)?;
    // SAFETY: `trusted_library` has just found `real` to be a file inside
    // the plugins folder beside this executable, reached through no link,
    // where it, that folder and every folder between them belong to this
    // user or to root and are writable by nobody else. So only this user or
    // root can have put the file there or can replace it before it is
    // opened, and both can already run any code as this process. What this
    // user puts there is what ships with the executable: plugins built from
    // this workspace, which export the entry `export_dylib_plugin!` makes.
    #[expect(
        unsafe_code,
        reason = "a library in the executable's own plugins folder that only this user or root can write"
    )]
    unsafe {
        load_unchecked(&real, host)
    }
}

/// The resolved path of `path`, when it is a file that may be opened as a
/// native plugin from the folder `plugins`; see [`load`] for the rules.
fn trusted_library(plugins: Option<&Path>, path: &Path) -> Result<PathBuf, DylibLoadError> {
    let open_error = |e: std::io::Error| DylibLoadError::Open {
        path: path.to_path_buf(),
        message: describe(&e),
    };
    let not_builtin = || DylibLoadError::NotBuiltin {
        path: path.to_path_buf(),
    };
    let real = std::fs::canonicalize(path).map_err(open_error)?;
    let plugins = plugins
        // A `plugins` that is a link is wherever it points, not the folder
        // beside the executable.
        .filter(|dir| std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir()))
        .and_then(|dir| std::fs::canonicalize(dir).ok())
        .ok_or_else(not_builtin)?;
    if real == plugins || !real.starts_with(&plugins) {
        return Err(not_builtin());
    }
    // `real` holds no link, so each step up is the folder that holds the
    // last: the file, then every folder up to and including `plugins`.
    for held in real.ancestors() {
        let meta = std::fs::metadata(held).map_err(open_error)?;
        if let Some(reason) = distrust(&meta) {
            return Err(DylibLoadError::Untrusted {
                path: held.to_path_buf(),
                reason,
            });
        }
        if held == plugins {
            break;
        }
    }
    Ok(real)
}

/// Why a file or folder may not be trusted with code, if it may not. The
/// daemon's discovery applies the same rule earlier, to say it in its own
/// words; this is the check the `unsafe` in [`load`] rests on.
#[cfg(unix)]
fn distrust(meta: &std::fs::Metadata) -> Option<&'static str> {
    use std::os::unix::fs::MetadataExt;
    distrust_for(meta.uid(), meta.mode(), rustix::process::geteuid().as_raw())
}

#[cfg(not(unix))]
fn distrust(_meta: &std::fs::Metadata) -> Option<&'static str> {
    None
}

/// Owned by `euid` or root, and not writable by the group or others.
#[cfg(unix)]
fn distrust_for(uid: u32, mode: u32, euid: u32) -> Option<&'static str> {
    if uid != euid && uid != 0 {
        Some("is not owned by the current user or root")
    } else if mode & 0o022 != 0 {
        Some("is writable by other users")
    } else {
        None
    }
}

/// [`load`] without the checks of where the file is and who can write it.
///
/// # Safety
///
/// Opening `path` runs its initialisers in this process, and its entry is
/// called if stabby's report of the entry's type matches: the file must hold
/// only code trusted as much as this process, and an entry it exports under
/// [`DYLIB_ENTRY_SYMBOL`] must be the one `export_dylib_plugin!` generates.
/// A file that is not a library, or a library with no such entry, is
/// refused with an error and breaks nothing.
///
/// # Errors
///
/// As [`load`], without `NotBuiltin` and `Untrusted`.
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

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A plugins folder with one file in a plugin's folder, all modes set
    /// here so the umask has no say. Returns the temp folder, the plugins
    /// folder and the file.
    fn plugins_with_a_library() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let plugin = plugins.join("github");
        fs::create_dir_all(&plugin).unwrap();
        let library = plugin.join("libplugin.dylib");
        fs::write(&library, b"not a library; it is never opened here").unwrap();
        set_mode(&plugins, 0o755);
        set_mode(&plugin, 0o755);
        set_mode(&library, 0o755);
        (tmp, plugins, library)
    }

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn a_file_in_a_plain_plugins_folder_is_trusted() {
        let (_tmp, plugins, library) = plugins_with_a_library();
        let real = trusted_library(Some(&plugins), &library).unwrap();
        assert_eq!(real, fs::canonicalize(&library).unwrap());
    }

    #[test]
    fn a_plugins_folder_that_is_a_link_is_not_the_plugins_folder() {
        // The link would move the whole check to wherever it points.
        let (tmp, plugins, _library) = plugins_with_a_library();
        let beside_the_executable = tmp.path().join("bin");
        fs::create_dir(&beside_the_executable).unwrap();
        let link = beside_the_executable.join("plugins");
        std::os::unix::fs::symlink(&plugins, &link).unwrap();

        let through_the_link = link.join("github").join("libplugin.dylib");
        assert!(through_the_link.exists());
        let err = trusted_library(Some(&link), &through_the_link).unwrap_err();
        assert!(matches!(err, DylibLoadError::NotBuiltin { .. }), "{err}");
    }

    #[test]
    fn with_no_plugins_folder_nothing_is_built_in() {
        let (tmp, _plugins, library) = plugins_with_a_library();
        for plugins in [None, Some(tmp.path().join("absent"))] {
            let err = trusted_library(plugins.as_deref(), &library).unwrap_err();
            assert!(matches!(err, DylibLoadError::NotBuiltin { .. }), "{err}");
        }
    }

    #[test]
    fn the_plugins_folder_itself_is_not_a_library_in_it() {
        let (_tmp, plugins, _library) = plugins_with_a_library();
        let err = trusted_library(Some(&plugins), &plugins).unwrap_err();
        assert!(matches!(err, DylibLoadError::NotBuiltin { .. }), "{err}");
    }

    #[test]
    fn what_another_user_can_write_is_not_trusted() {
        // The file, its folder and the plugins folder, one at a time.
        for (writable, mode) in [(0, 0o757), (1, 0o775), (2, 0o777), (0, 0o775)] {
            let (_tmp, plugins, library) = plugins_with_a_library();
            let plugin = library.parent().unwrap().to_path_buf();
            let target = [&library, &plugin, &plugins][writable];
            set_mode(target, mode);

            let err = trusted_library(Some(&plugins), &library).unwrap_err();
            match &err {
                DylibLoadError::Untrusted { path, reason } => {
                    assert_eq!(path, &fs::canonicalize(target).unwrap(), "{err}");
                    assert_eq!(*reason, "is writable by other users");
                }
                other => panic!("expected Untrusted for {}, got {other}", target.display()),
            }
        }
    }

    #[test]
    fn the_owner_must_be_this_user_or_root() {
        assert_eq!(distrust_for(501, 0o755, 501), None);
        assert_eq!(distrust_for(0, 0o755, 501), None);
        assert_eq!(
            distrust_for(502, 0o755, 501),
            Some("is not owned by the current user or root")
        );
        assert_eq!(
            distrust_for(501, 0o775, 501),
            Some("is writable by other users")
        );
    }
}

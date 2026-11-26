//! Plugin discovery: which directories hold a plugin that may load, and how
//! it is loaded. This is the only place that keeps native (dylib) plugins to
//! the built-in directory: the loader crate accepts any path.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use smithagents_plugin_host::{
    DylibRuntime, LoadError, PluginRuntime, WasmEngine, WasmLimits, WasmRuntime,
};
use smithagents_plugin_protocol::{HostMode, Manifest};

/// The longest a single call into a loaded plugin may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Larger manifests are refused unread.
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Shipped beside the daemon; may be native.
    Builtin,
    /// Placed by the user; never native.
    Installed,
}

#[derive(Debug, Clone)]
pub struct Discovered {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub origin: Origin,
}

/// A directory that holds no loadable plugin. `reason` is a plain sentence
/// for the user and carries no file contents beyond a manifest parse error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub dir: PathBuf,
    pub reason: String,
}

/// `<directory of the running executable>/plugins`.
pub fn builtin_plugins_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("plugins"))
}

/// Scans both directories. A built-in plugin wins over an installed one with
/// the same id. Results are sorted by id (rejections by directory).
pub fn discover(builtin: Option<&Path>, installed: &Path) -> (Vec<Discovered>, Vec<Rejected>) {
    let mut found = Vec::new();
    let mut rejected = Vec::new();
    if let Some(dir) = builtin {
        scan(dir, Origin::Builtin, &mut found, &mut rejected);
    }
    let mut from_installed = Vec::new();
    scan(
        installed,
        Origin::Installed,
        &mut from_installed,
        &mut rejected,
    );
    for d in from_installed {
        if found.iter().any(|b| b.manifest.id == d.manifest.id) {
            rejected.push(Rejected {
                dir: d.dir,
                reason: "a built-in plugin has the same id".into(),
            });
        } else {
            found.push(d);
        }
    }
    found.sort_by(|a, b| a.manifest.id.as_ref().cmp(b.manifest.id.as_ref()));
    rejected.sort_by(|a, b| a.dir.cmp(&b.dir));
    (found, rejected)
}

fn scan(root: &Path, origin: Origin, found: &mut Vec<Discovered>, rejected: &mut Vec<Rejected>) {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            rejected.push(Rejected {
                dir: root.to_path_buf(),
                reason: "the plugins directory cannot be read".into(),
            });
            return;
        }
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        // Plain files beside the plugin directories are not plugins.
        let Ok(meta) = fs::symlink_metadata(&dir) else {
            continue;
        };
        if !meta.is_dir() && !meta.file_type().is_symlink() {
            continue;
        }
        match check_dir(&dir, &meta, origin) {
            Ok(manifest) => found.push(Discovered {
                manifest,
                dir,
                origin,
            }),
            Err(reason) => rejected.push(Rejected { dir, reason }),
        }
    }
}

fn check_dir(dir: &Path, meta: &fs::Metadata, origin: Origin) -> Result<Manifest, String> {
    if meta.file_type().is_symlink() {
        return Err("the plugin directory is a symbolic link".into());
    }
    if is_writable_by_others(meta) {
        return Err("the plugin directory is writable by other users".into());
    }
    let manifest = read_manifest(&dir.join("plugin.toml"))?;

    let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if dir_name != manifest.id.as_ref() {
        return Err(format!(
            "the directory name does not match the plugin id {}",
            manifest.id
        ));
    }
    match manifest.host {
        HostMode::Dylib if origin == Origin::Installed => {
            return Err("native plugins can only be built in".into());
        }
        HostMode::Process | HostMode::InProcess => {
            return Err(format!("host mode {} is not supported yet", manifest.host));
        }
        _ => {}
    }
    check_entry(dir, &manifest)?;
    Ok(manifest)
}

fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let meta = fs::symlink_metadata(path).map_err(|_| "plugin.toml cannot be read".to_string())?;
    if !meta.is_file() {
        return Err("plugin.toml is not a regular file".into());
    }
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err("plugin.toml is larger than 256 KiB".into());
    }
    let text = fs::read_to_string(path).map_err(|_| "plugin.toml cannot be read".to_string())?;
    Manifest::parse(&text).map_err(|e| format!("plugin.toml is not valid: {e}"))
}

fn check_entry(dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let name = manifest
        .entry
        .as_deref()
        .ok_or_else(|| "the manifest names no entry file".to_string())?;
    let entry = dir.join(name);
    let missing = || "the entry file does not exist".to_string();
    fs::symlink_metadata(&entry).map_err(|_| missing())?;
    // Compare resolved paths so a symlink cannot point out of the directory.
    let real_entry = fs::canonicalize(&entry).map_err(|_| missing())?;
    let real_dir =
        fs::canonicalize(dir).map_err(|_| "the plugin directory cannot be read".to_string())?;
    if !real_entry.starts_with(&real_dir) {
        return Err("the entry file is outside the plugin directory".into());
    }
    let meta = fs::metadata(&real_entry).map_err(|_| missing())?;
    if !meta.is_file() {
        return Err("the entry file is not a regular file".into());
    }
    if is_writable_by_others(&meta) {
        return Err("the entry file is writable by other users".into());
    }
    Ok(())
}

/// Group- or world-writable. Permission bits do not exist off Unix, so other
/// platforms skip the check.
fn is_writable_by_others(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o022 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// Builds the runtime for a discovered plugin. Refuses a native plugin that
/// is not built in, whatever `discover` decided: this function is public.
pub async fn load_runtime(
    d: &Discovered,
    engine: &WasmEngine,
) -> Result<Arc<dyn PluginRuntime>, LoadError> {
    let entry = d
        .manifest
        .entry
        .as_deref()
        .ok_or_else(|| LoadError::Invalid {
            message: "the manifest names no entry file".into(),
        })?;
    let path = d.dir.join(entry);
    match d.manifest.host {
        HostMode::Dylib => {
            if d.origin != Origin::Builtin {
                return Err(LoadError::Refused {
                    message: "native plugins can only be built in".into(),
                });
            }
            Ok(Arc::new(DylibRuntime::load(&path, CALL_TIMEOUT)?))
        }
        HostMode::Wasm => Ok(Arc::new(
            WasmRuntime::load(engine, &path, WasmLimits::default()).await?,
        )),
        mode @ (HostMode::Process | HostMode::InProcess) => Err(LoadError::Refused {
            message: format!("host mode {mode} is not supported yet"),
        }),
    }
}

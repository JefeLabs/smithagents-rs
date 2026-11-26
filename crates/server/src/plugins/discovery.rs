//! Plugin discovery: which directories hold a plugin that may load, and how
//! it is loaded. This is the only place that keeps native (dylib) plugins to
//! the built-in directory: the loader crate accepts any path.
//!
//! Trust model (Unix; other platforms have no permission bits to check and
//! skip these checks). The anchor is the directory holding the daemon
//! executable: whoever can write there can replace the daemon itself, so the
//! checks start at the built-in plugins directory and go down, never to its
//! ancestors. Everything that decides what code runs (the plugin directory,
//! `plugin.toml`, the entry file, and for built-in plugins also the built-in
//! root and the directories a symlinked entry passes through) must be owned
//! by the current user or root and not writable by group or others.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use smithagents_plugin_host::{
    DylibRuntime, LoadError, PluginRuntime, WasmEngine, WasmLimits, WasmRuntime,
};
use smithagents_plugin_protocol::{HostMode, Manifest, ManifestError};

/// The longest a single call into a loaded plugin may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Larger manifests are refused.
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Shipped beside the daemon; may be native.
    Builtin,
    /// Placed by the user; never native.
    Installed,
}

/// Identifies a file on disk, so a replacement under the same name is noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId(u64, u64);

fn file_id(meta: &fs::Metadata) -> FileId {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FileId(meta.dev(), meta.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        FileId(0, 0)
    }
}

/// A plugin that passed discovery. Only [`discover`] makes one, so `origin`
/// cannot be forged.
#[derive(Debug, Clone)]
pub struct Discovered {
    manifest: Manifest,
    dir: PathBuf,
    origin: Origin,
    /// The entry file after resolving symlinks, as checked by `discover`.
    entry_path: PathBuf,
    entry_id: FileId,
    dir_id: FileId,
}

impl Discovered {
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn origin(&self) -> Origin {
        self.origin
    }

    /// The canonical path of the entry file that `discover` checked and
    /// `load_runtime` loads.
    pub fn entry_path(&self) -> &Path {
        &self.entry_path
    }

    pub fn into_manifest(self) -> Manifest {
        self.manifest
    }

    /// A copy claiming another origin, to test that `load_runtime` does not
    /// trust the field. Never use outside tests.
    #[cfg(feature = "test-util")]
    pub fn forged_origin_for_test(&self, origin: Origin) -> Discovered {
        Discovered {
            origin,
            ..self.clone()
        }
    }
}

/// A directory that holds no loadable plugin. `reason` is a plain sentence
/// for the user and carries no file contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub dir: PathBuf,
    pub reason: String,
}

/// `<directory of the running executable>/plugins`, with the executable's
/// symlinks resolved so the directory is the real one.
pub fn builtin_plugins_dir() -> Option<PathBuf> {
    let exe = fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    Some(exe.parent()?.join("plugins"))
}

#[derive(Default)]
struct Scan {
    found: Vec<Discovered>,
    rejected: Vec<Rejected>,
    /// Every directory name under the root, loadable or not.
    names: Vec<String>,
}

/// Scans both directories. A built-in plugin, even a broken one, keeps its
/// id from an installed plugin. Results are sorted by id (rejections by
/// directory).
pub fn discover(builtin: Option<&Path>, installed: &Path) -> (Vec<Discovered>, Vec<Rejected>) {
    let mut result = Scan::default();
    let mut reserved = Vec::new();
    if let Some(dir) = builtin {
        let mut scan = Scan::default();
        scan_root(dir, Origin::Builtin, &[], &mut scan);
        reserved = std::mem::take(&mut scan.names);
        result = scan;
    }
    if overlaps(builtin, installed) {
        result.rejected.push(Rejected {
            dir: installed.to_path_buf(),
            reason: "the installed plugins directory overlaps the built-in one".into(),
        });
    } else {
        let mut scan = Scan::default();
        scan_root(installed, Origin::Installed, &reserved, &mut scan);
        result.found.append(&mut scan.found);
        result.rejected.append(&mut scan.rejected);
    }
    result
        .found
        .sort_by(|a, b| a.manifest.id.as_ref().cmp(b.manifest.id.as_ref()));
    result.rejected.sort_by(|a, b| a.dir.cmp(&b.dir));
    (result.found, result.rejected)
}

fn overlaps(builtin: Option<&Path>, installed: &Path) -> bool {
    let (Some(b), Ok(i)) = (
        builtin.and_then(|b| fs::canonicalize(b).ok()),
        fs::canonicalize(installed),
    ) else {
        return false;
    };
    b.starts_with(&i) || i.starts_with(&b)
}

fn scan_root(root: &Path, origin: Origin, reserved: &[String], out: &mut Scan) {
    let reject_root = |out: &mut Scan, reason: String| {
        out.rejected.push(Rejected {
            dir: root.to_path_buf(),
            reason,
        })
    };
    let root_meta = match fs::metadata(root) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => return reject_root(out, "the plugins directory cannot be read".into()),
    };
    if !root_meta.is_dir() {
        return reject_root(out, "the plugins directory is not a directory".into());
    }
    if origin == Origin::Builtin {
        // Whoever can write here can add native code to the daemon.
        if let Err(reason) = trust(&root_meta, "the built-in plugins directory") {
            return reject_root(out, reason);
        }
    }
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return reject_root(out, "the plugins directory cannot be read".into()),
    };
    for entry in entries {
        let Ok(entry) = entry else {
            reject_root(out, "a directory entry cannot be read".into());
            continue;
        };
        let dir = entry.path();
        let meta = match fs::symlink_metadata(&dir) {
            Ok(meta) => meta,
            Err(_) => {
                out.rejected.push(Rejected {
                    dir,
                    reason: "the plugin directory cannot be inspected".into(),
                });
                continue;
            }
        };
        // Plain files beside the plugin directories are not plugins.
        if !meta.is_dir() && !meta.file_type().is_symlink() {
            continue;
        }
        if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
            out.names.push(name.to_string());
        }
        match check_dir(&dir, &meta, origin, reserved) {
            Ok(found) => out.found.push(found),
            Err(reason) => out.rejected.push(Rejected { dir, reason }),
        }
    }
}

fn check_dir(
    dir: &Path,
    meta: &fs::Metadata,
    origin: Origin,
    reserved: &[String],
) -> Result<Discovered, String> {
    if meta.file_type().is_symlink() {
        return Err("the plugin directory is a symbolic link".into());
    }
    trust(meta, "the plugin directory")?;
    let manifest = read_manifest(&dir.join("plugin.toml"))?;

    let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if dir_name != manifest.id.as_ref() {
        return Err(format!(
            "the directory name does not match the plugin id {}",
            manifest.id
        ));
    }
    if origin == Origin::Installed && reserved.iter().any(|n| n == manifest.id.as_ref()) {
        return Err("a built-in plugin has this id".into());
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
    let (entry_path, entry_id) = check_entry(dir, &manifest, origin)?;
    Ok(Discovered {
        manifest,
        dir: dir.to_path_buf(),
        origin,
        entry_path,
        entry_id,
        dir_id: file_id(meta),
    })
}

/// Opens `path` without following a symlink and without blocking on a pipe.
fn open_plain(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
        options.custom_flags(flags.bits() as i32);
    }
    options.open(path)
}

fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let unreadable = || "plugin.toml cannot be read".to_string();
    // Check and read through one handle, so the file cannot change in between.
    let file = match open_plain(path) {
        Ok(file) => file,
        Err(_) if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) => {
            return Err("plugin.toml is not a regular file".into());
        }
        Err(_) => return Err(unreadable()),
    };
    let meta = file.metadata().map_err(|_| unreadable())?;
    if !meta.is_file() {
        return Err("plugin.toml is not a regular file".into());
    }
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err("plugin.toml is larger than 256 KiB".into());
    }
    trust(&meta, "plugin.toml")?;
    let mut text = String::new();
    // The file may have grown since the check: read one byte past the limit.
    (&file)
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| unreadable())?;
    if text.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("plugin.toml is larger than 256 KiB".into());
    }
    Manifest::parse(&text).map_err(|e| match e {
        // Carries at most a line number.
        ManifestError::Toml { .. } => e.to_string(),
        // These carry only a number or a mode name.
        ManifestError::UnsupportedContract(_) | ManifestError::EntryRequired(_) => {
            format!("plugin.toml is not valid: {e}")
        }
        // The rest quote text from the file (keys, hosts, entry names).
        _ => "plugin.toml is not valid: a field has a value that is not allowed".to_string(),
    })
}

/// Resolves the entry file and checks it. Returns its canonical path.
fn check_entry(
    dir: &Path,
    manifest: &Manifest,
    origin: Origin,
) -> Result<(PathBuf, FileId), String> {
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
    trust(&meta, "the entry file")?;
    if origin == Origin::Builtin {
        // Native code: every directory the symlinks pass through counts too.
        let mut current = real_entry.parent();
        while let Some(parent) = current.filter(|p| *p != real_dir) {
            let meta = fs::metadata(parent)
                .map_err(|_| "the plugin directory cannot be read".to_string())?;
            trust(&meta, "a directory holding the entry file")?;
            current = parent.parent();
        }
    }
    Ok((real_entry, file_id(&meta)))
}

/// Why a file may not be trusted with code, if it may not.
#[derive(Debug, PartialEq, Eq)]
enum Distrust {
    Owner,
    Writable,
}

/// Owned by `euid` or root, and not writable by group or others.
#[cfg(unix)]
fn distrust_for(uid: u32, mode: u32, euid: u32) -> Option<Distrust> {
    if uid != euid && uid != 0 {
        Some(Distrust::Owner)
    } else if mode & 0o022 != 0 {
        Some(Distrust::Writable)
    } else {
        None
    }
}

fn trust(meta: &fs::Metadata, what: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let euid = rustix::process::geteuid().as_raw();
        match distrust_for(meta.uid(), meta.mode(), euid) {
            Some(Distrust::Owner) => {
                return Err(format!("{what} is not owned by the current user or root"));
            }
            Some(Distrust::Writable) => {
                return Err(format!("{what} is writable by other users"));
            }
            None => {}
        }
    }
    #[cfg(not(unix))]
    let _ = (meta, what);
    Ok(())
}

/// Builds the runtime for a discovered plugin. Refuses a native plugin that
/// is not built in, and re-checks the directory and entry file it is about
/// to load, since time has passed since `discover`.
pub async fn load_runtime(
    d: &Discovered,
    engine: &WasmEngine,
) -> Result<Arc<dyn PluginRuntime>, LoadError> {
    if d.manifest.host == HostMode::Dylib && d.origin != Origin::Builtin {
        return Err(LoadError::Refused {
            message: "native plugins can only be built in".into(),
        });
    }
    recheck(d).map_err(|message| LoadError::Refused { message })?;
    match d.manifest.host {
        HostMode::Dylib => Ok(Arc::new(DylibRuntime::load(&d.entry_path, CALL_TIMEOUT)?)),
        HostMode::Wasm => Ok(Arc::new(
            WasmRuntime::load(engine, &d.entry_path, WasmLimits::default()).await?,
        )),
        mode @ (HostMode::Process | HostMode::InProcess) => Err(LoadError::Refused {
            message: format!("host mode {mode} is not supported yet"),
        }),
    }
}

/// The plugin directory and entry file are still the ones `discover` checked.
fn recheck(d: &Discovered) -> Result<(), String> {
    let changed = || "the plugin changed on disk since it was discovered".to_string();
    let meta = fs::symlink_metadata(&d.dir).map_err(|_| changed())?;
    if meta.file_type().is_symlink() || !meta.is_dir() || file_id(&meta) != d.dir_id {
        return Err(changed());
    }
    trust(&meta, "the plugin directory")?;
    let (path, id) = check_entry(&d.dir, &d.manifest, d.origin)?;
    if path != d.entry_path || id != d.entry_id {
        return Err(changed());
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn only_the_current_user_or_root_may_own_code_and_nobody_else_may_write() {
        assert_eq!(distrust_for(501, 0o755, 501), None);
        assert_eq!(distrust_for(0, 0o644, 501), None);
        assert_eq!(distrust_for(502, 0o755, 501), Some(Distrust::Owner));
        assert_eq!(distrust_for(501, 0o775, 501), Some(Distrust::Writable));
        assert_eq!(distrust_for(501, 0o757, 501), Some(Distrust::Writable));
        assert_eq!(distrust_for(0, 0o777, 501), Some(Distrust::Writable));
        assert_eq!(distrust_for(502, 0o777, 501), Some(Distrust::Owner));
    }
}

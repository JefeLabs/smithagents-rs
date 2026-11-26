//! Packs SmithAgents plugins: builds each plugin crate and writes
//! `<out>/<id>/plugin.toml` beside the entry file, the layout the daemon
//! discovers.
//!
//! Builds run `$CARGO` (else `cargo`) with `--target-dir target/plugin-pack`,
//! apart from the workspace's own target directory, so a test that packs the
//! fixtures does not wait on the `cargo test` that started it.
//!
//! Packed files are never group- or world-writable: discovery refuses those.

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use smithagents_plugin_protocol::{HostMode, Manifest, ManifestError, CONTRACT_VERSION};

pub struct PackOptions {
    /// Build with the release profile.
    pub release: bool,
    pub out_dir: PathBuf,
    /// Also pack the test fixtures.
    pub fixtures: bool,
}

/// What [`pack`] wrote: one directory per plugin id under `dir`.
#[derive(Debug, Clone)]
pub struct Packed {
    pub dir: PathBuf,
    pub ids: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("cannot run cargo: {0}")]
    Spawn(io::Error),
    #[error("cargo build failed ({status}):\n{stderr}")]
    Build { status: String, stderr: String },
    #[error("plugin {id}: the packed manifest is invalid: {source}")]
    Manifest { id: String, source: ManifestError },
}

const WASM_TARGET: &str = "wasm32-wasip2";
/// Manifests and WebAssembly components.
const FILE_MODE: u32 = 0o644;
/// Native libraries.
const LIBRARY_MODE: u32 = 0o755;

/// One plugin this tool knows how to pack.
struct Spec {
    id: &'static str,
    name: &'static str,
    /// The Cargo package that builds the entry file.
    package: &'static str,
    /// The package's library name: the entry file is `<lib>.wasm` for
    /// `wasm`, and the platform's library file name (`lib<lib>.dylib` on
    /// macOS) for `dylib`.
    lib: &'static str,
    /// `Wasm` or `Dylib`.
    host: HostMode,
    /// How the manifest is written.
    manifest: ManifestSource,
    fixture: bool,
}

/// Where a plugin's manifest comes from.
#[derive(Clone, Copy)]
enum ManifestSource {
    /// The generated header followed by these parts.
    Generated(&'static [&'static str]),
    /// The plugin's own `plugin.toml`, whose `entry` is replaced with the
    /// file built for this platform: a library's file name differs by
    /// platform, and the source file can name only one.
    Source(&'static str),
}

/// The fixture plugin's declarations, with the resources every mode runs:
/// one per behaviour.
const FIXTURE_DECLS: &str = r#"hosts = ["fixture.invalid"]

[[secrets]]
key = "token"
label = "Token"
hosts = ["fixture.invalid"]

[[settings]]
key = "url"
label = "URL to fetch"
kind = "text"

[[settings]]
key = "ms"
label = "Milliseconds to sleep"
kind = "text"

[[resources]]
kind = "items"
label = "Three pages of two items"
interval_secs = 60

[[resources]]
kind = "http"
label = "GET the url setting"
interval_secs = 60

[[resources]]
kind = "post"
label = "POST the url setting"
interval_secs = 60

[[resources]]
kind = "auth"
label = "Fails with an auth error"
interval_secs = 60

[[resources]]
kind = "rate"
label = "Fails rate limited"
interval_secs = 60

[[resources]]
kind = "panic"
label = "Panics"
interval_secs = 60

[[resources]]
kind = "sleep"
label = "Sleeps for the ms setting"
interval_secs = 60

[[resources]]
kind = "huge"
label = "Too many items"
interval_secs = 60

[[resources]]
kind = "dupe"
label = "Duplicate ids"
interval_secs = 60
"#;

/// Behaviours only a WebAssembly host can stop.
const FIXTURE_WASM_ONLY: &str = r#"
[[resources]]
kind = "forever"
label = "Never done"
interval_secs = 60

[[resources]]
kind = "spin"
label = "Never returns"
interval_secs = 60

[[resources]]
kind = "bloat"
label = "Grows its memory without end"
interval_secs = 60
"#;

/// The behaviour only the native fixture has.
const FIXTURE_DYLIB_ONLY: &str = r#"
[[resources]]
kind = "stale-call"
label = "Calls the host under the previous call's number"
interval_secs = 60
"#;

const PLUGINS: &[Spec] = &[
    Spec {
        id: "fixture-dylib",
        name: "Fixture (native)",
        package: "smithagents-plugin-fixture-dylib",
        lib: "smithagents_plugin_fixture_dylib",
        host: HostMode::Dylib,
        manifest: ManifestSource::Generated(&[FIXTURE_DECLS, FIXTURE_DYLIB_ONLY]),
        fixture: true,
    },
    Spec {
        id: "fixture-wasm",
        name: "Fixture (WebAssembly)",
        package: "smithagents-plugin-fixture-wasm",
        lib: "smithagents_plugin_fixture_wasm",
        host: HostMode::Wasm,
        manifest: ManifestSource::Generated(&[FIXTURE_DECLS, FIXTURE_WASM_ONLY]),
        fixture: true,
    },
    Spec {
        id: "fixture-wasm-fs",
        name: "Fixture that reads files (WebAssembly)",
        package: "smithagents-plugin-fixture-wasm-fs",
        lib: "smithagents_plugin_fixture_wasm_fs",
        host: HostMode::Wasm,
        manifest: ManifestSource::Generated(&[FIXTURE_DECLS, FIXTURE_WASM_ONLY]),
        fixture: true,
    },
    Spec {
        id: "github",
        name: "GitHub",
        package: "smithagents-plugin-github-dylib",
        lib: "smithagents_plugin_github_dylib",
        host: HostMode::Dylib,
        manifest: ManifestSource::Source(include_str!("../../../plugins/github/plugin.toml")),
        fixture: false,
    },
];

/// The workspace this tool was built in.
pub fn workspace_root() -> PathBuf {
    let tool = Path::new(env!("CARGO_MANIFEST_DIR"));
    tool.parent()
        .and_then(Path::parent)
        .expect("tools/plugins sits two levels below the workspace root")
        .to_path_buf()
}

/// Where builds go: apart from the workspace's own target directory.
fn build_dir(root: &Path) -> PathBuf {
    root.join("target").join("plugin-pack")
}

/// Builds the plugins and writes `<out>/<id>/plugin.toml` and the entry file
/// for each. Files are replaced atomically, so packing while another process
/// reads the same directory is safe.
pub fn pack(opts: &PackOptions) -> Result<Packed, PackError> {
    let specs: Vec<&Spec> = PLUGINS
        .iter()
        .filter(|s| opts.fixtures || !s.fixture)
        .collect();
    let root = workspace_root();

    let packages = |host: HostMode| -> Vec<&str> {
        specs
            .iter()
            .filter(|s| s.host == host)
            .map(|s| s.package)
            .collect()
    };
    let wasm = packages(HostMode::Wasm);
    if !wasm.is_empty() {
        let mut cmd = cargo_build(&root, opts.release);
        cmd.args(["--target", WASM_TARGET]);
        run(cmd, &wasm)?;
    }
    let native = packages(HostMode::Dylib);
    if !native.is_empty() {
        run(cargo_build(&root, opts.release), &native)?;
    }

    let mut ids = Vec::new();
    for spec in specs {
        let (entry, out_dir, mode) = match spec.host {
            HostMode::Wasm => (
                format!("{}.wasm", spec.lib),
                build_dir(&root).join(WASM_TARGET),
                FILE_MODE,
            ),
            _ => (library_file(spec.lib), build_dir(&root), LIBRARY_MODE),
        };
        let artifact = out_dir.join(profile(opts.release)).join(&entry);
        let manifest = manifest_text(spec, &entry);
        let parsed = Manifest::parse(&manifest).map_err(|source| PackError::Manifest {
            id: spec.id.to_string(),
            source,
        })?;
        if parsed.entry.as_deref() != Some(entry.as_str()) {
            return Err(PackError::Manifest {
                id: spec.id.to_string(),
                source: ManifestError::BadEntry(entry),
            });
        }

        let dir = opts.out_dir.join(spec.id);
        fs::create_dir_all(&dir).map_err(io_at(&dir))?;
        let bytes = fs::read(&artifact).map_err(io_at(&artifact))?;
        write_atomic(&dir.join(&entry), &bytes, mode)?;
        write_atomic(&dir.join("plugin.toml"), manifest.as_bytes(), FILE_MODE)?;
        ids.push(spec.id.to_string());
    }
    Ok(Packed {
        dir: opts.out_dir.clone(),
        ids,
    })
}

/// The fixtures, packed once per process into `target/plugin-pack/fixtures`
/// of this workspace. For tests; panics if packing fails.
pub fn pack_fixtures_once() -> &'static Packed {
    static PACKED: OnceLock<Packed> = OnceLock::new();
    PACKED.get_or_init(|| {
        let opts = PackOptions {
            release: true,
            out_dir: build_dir(&workspace_root()).join("fixtures"),
            fixtures: true,
        };
        pack(&opts).unwrap_or_else(|e| panic!("cannot pack the plugin fixtures: {e}"))
    })
}

/// Builds the `cdylib` crate in `crate_dir`, which stands apart from this
/// workspace (its manifest has its own `[workspace]` table), and copies its
/// library, named from `lib`, into `out_dir`. Returns the copy's path.
///
/// The crate is built in release from a copy of its manifest and `src`
/// beside this workspace's `Cargo.lock`, offline, so it can use only the
/// crate versions the workspace already locks. For tests that need a native
/// library that is not a packed plugin.
pub fn pack_library(crate_dir: &Path, lib: &str, out_dir: &Path) -> Result<PathBuf, PackError> {
    let root = workspace_root();
    let name = crate_dir.file_name().unwrap_or(crate_dir.as_os_str());
    let copy = build_dir(&root).join("libraries").join(name);
    copy_tree(&crate_dir.join("src"), &copy.join("src"))?;
    copy_file(&crate_dir.join("Cargo.toml"), &copy.join("Cargo.toml"))?;
    copy_file(&root.join("Cargo.lock"), &copy.join("Cargo.lock"))?;

    let mut cmd = cargo_build(&root, true);
    cmd.arg("--manifest-path")
        .arg(copy.join("Cargo.toml"))
        .arg("--offline");
    run(cmd, &[])?;

    let file = library_file(lib);
    let artifact = build_dir(&root).join(profile(true)).join(&file);
    let bytes = fs::read(&artifact).map_err(io_at(&artifact))?;
    fs::create_dir_all(out_dir).map_err(io_at(out_dir))?;
    let packed = out_dir.join(file);
    write_atomic(&packed, &bytes, LIBRARY_MODE)?;
    Ok(packed)
}

fn manifest_text(spec: &Spec, entry: &str) -> String {
    match spec.manifest {
        ManifestSource::Generated(body) => format!(
            "id = \"{id}\"\nname = \"{name}\"\nversion = \"{version}\"\ncontract = {CONTRACT_VERSION}\nhost = \"{host}\"\nentry = \"{entry}\"\n{body}",
            id = spec.id,
            name = spec.name,
            version = env!("CARGO_PKG_VERSION"),
            host = spec.host,
            body = body.concat(),
        ),
        ManifestSource::Source(text) => with_entry(text, entry),
    }
}

/// `text` with its top-level `entry = ...` line naming `entry`. The line is
/// found before the first table, where the top-level keys end.
fn with_entry(text: &str, entry: &str) -> String {
    let mut in_header = true;
    let mut out = String::with_capacity(text.len() + entry.len());
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_header = false;
        }
        if in_header && trimmed.starts_with("entry") && trimmed[5..].trim_start().starts_with('=') {
            out.push_str(&format!("entry = \"{entry}\"\n"));
        } else {
            out.push_str(line);
        }
    }
    out
}

fn profile(release: bool) -> &'static str {
    if release {
        "release"
    } else {
        "debug"
    }
}

/// The file name the platform gives library `lib`: `lib<lib>.dylib` on
/// macOS, `lib<lib>.so` on Linux, `<lib>.dll` on Windows.
fn library_file(lib: &str) -> String {
    format!("{DLL_PREFIX}{lib}{DLL_SUFFIX}")
}

/// `$CARGO build` (else `cargo build`) into this workspace's pack directory.
fn cargo_build(root: &Path, release: bool) -> Command {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(root)
        .arg("build")
        .arg("--target-dir")
        .arg(build_dir(root));
    if release {
        cmd.arg("--release");
    }
    cmd
}

/// Runs a build of `packages`. Its output is kept and shown only if the
/// build fails.
fn run(mut cmd: Command, packages: &[&str]) -> Result<(), PackError> {
    for package in packages {
        cmd.args(["-p", package]);
    }
    let out = cmd.output().map_err(PackError::Spawn)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(PackError::Build {
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Copies the files under `from` into `to`, recursively.
fn copy_tree(from: &Path, to: &Path) -> Result<(), PackError> {
    fs::create_dir_all(to).map_err(io_at(to))?;
    for entry in fs::read_dir(from).map_err(io_at(from))? {
        let entry = entry.map_err(io_at(from))?;
        let path = entry.path();
        let target = to.join(entry.file_name());
        if entry.file_type().map_err(io_at(&path))?.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            copy_file(&path, &target)?;
        }
    }
    Ok(())
}

fn copy_file(from: &Path, to: &Path) -> Result<(), PackError> {
    let bytes = fs::read(from).map_err(io_at(from))?;
    write_atomic(to, &bytes, FILE_MODE)
}

/// Writes through a uniquely named file in the same directory, then renames
/// it over `path`: a reader sees the old file or the new one, never a part.
/// The file gets `mode` on Unix, whatever the umask.
fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), PackError> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let n = WRITES.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let written = fs::write(&tmp, bytes)
        .and_then(|()| set_mode(&tmp, mode))
        .map_err(io_at(&tmp))
        .and_then(|()| fs::rename(&tmp, path).map_err(io_at(path)));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

fn io_at(path: &Path) -> impl Fn(io::Error) -> PackError + '_ {
    move |source| PackError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod entry_tests {
    use super::with_entry;

    #[test]
    fn only_the_top_level_entry_line_is_replaced() {
        let text =
            "id = \"x\"\nentry = \"old.dylib\"\nhosts = []\n[[settings]]\nentry = \"kept\"\n";
        assert_eq!(
            with_entry(text, "new.so"),
            "id = \"x\"\nentry = \"new.so\"\nhosts = []\n[[settings]]\nentry = \"kept\"\n"
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn written_files_get_the_mode_asked_for_whatever_the_umask() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        // Neither mode is what the usual umasks give a new file.
        for mode in [0o600, 0o755] {
            write_atomic(&path, b"x", mode).unwrap();
            let got = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(got, mode, "{got:o}");
        }
    }
}

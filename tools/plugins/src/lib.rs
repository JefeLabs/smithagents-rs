//! Packs SmithAgents plugins: builds each plugin crate and writes
//! `<out>/<id>/plugin.toml` beside the entry file, the layout the daemon
//! discovers.
//!
//! Builds run `$CARGO` (else `cargo`) with `--target-dir target/plugin-pack`,
//! apart from the workspace's own target directory, so a test that packs the
//! fixtures does not wait on the `cargo test` that started it.

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

/// One plugin this tool knows how to pack.
struct Spec {
    id: &'static str,
    name: &'static str,
    /// The Cargo package that builds the entry file.
    package: &'static str,
    /// The package's library name: the entry file is `<lib>.wasm`.
    lib: &'static str,
    host: HostMode,
    /// The manifest after the generated header.
    body: &'static str,
    fixture: bool,
}

/// The fixture plugin's declarations: one resource per behaviour.
const FIXTURE_BODY: &str = r#"hosts = ["fixture.invalid"]

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

const PLUGINS: &[Spec] = &[
    Spec {
        id: "fixture-wasm",
        name: "Fixture (WebAssembly)",
        package: "smithagents-plugin-fixture-wasm",
        lib: "smithagents_plugin_fixture_wasm",
        host: HostMode::Wasm,
        body: FIXTURE_BODY,
        fixture: true,
    },
    Spec {
        id: "fixture-wasm-fs",
        name: "Fixture that reads files (WebAssembly)",
        package: "smithagents-plugin-fixture-wasm-fs",
        lib: "smithagents_plugin_fixture_wasm_fs",
        host: HostMode::Wasm,
        body: FIXTURE_BODY,
        fixture: true,
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
    let profile = if opts.release { "release" } else { "debug" };

    let wasm: Vec<&str> = specs
        .iter()
        .filter(|s| s.host == HostMode::Wasm)
        .map(|s| s.package)
        .collect();
    if !wasm.is_empty() {
        build(&root, &wasm, Some(WASM_TARGET), opts.release)?;
    }

    let mut ids = Vec::new();
    for spec in specs {
        let artifact = build_dir(&root)
            .join(WASM_TARGET)
            .join(profile)
            .join(format!("{}.wasm", spec.lib));
        let entry = format!("{}.wasm", spec.lib);
        let manifest = manifest_text(spec, &entry);
        Manifest::parse(&manifest).map_err(|source| PackError::Manifest {
            id: spec.id.to_string(),
            source,
        })?;

        let dir = opts.out_dir.join(spec.id);
        fs::create_dir_all(&dir).map_err(io_at(&dir))?;
        let bytes = fs::read(&artifact).map_err(io_at(&artifact))?;
        write_atomic(&dir.join(&entry), &bytes)?;
        write_atomic(&dir.join("plugin.toml"), manifest.as_bytes())?;
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

fn manifest_text(spec: &Spec, entry: &str) -> String {
    format!(
        "id = \"{id}\"\nname = \"{name}\"\nversion = \"{version}\"\ncontract = {CONTRACT_VERSION}\nhost = \"{host}\"\nentry = \"{entry}\"\n{body}",
        id = spec.id,
        name = spec.name,
        version = env!("CARGO_PKG_VERSION"),
        host = spec.host,
        body = spec.body,
    )
}

/// Runs `cargo build` for `packages`. Its output is kept and shown only if
/// the build fails.
fn build(
    root: &Path,
    packages: &[&str],
    target: Option<&str>,
    release: bool,
) -> Result<(), PackError> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(root)
        .arg("build")
        .arg("--target-dir")
        .arg(build_dir(root));
    for package in packages {
        cmd.args(["-p", package]);
    }
    if let Some(target) = target {
        cmd.args(["--target", target]);
    }
    if release {
        cmd.arg("--release");
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

/// Writes through a uniquely named file in the same directory, then renames
/// it over `path`: a reader sees the old file or the new one, never a part.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), PackError> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let n = WRITES.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()));
    fs::write(&tmp, bytes).map_err(io_at(&tmp))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        io_at(path)(e)
    })
}

fn io_at(path: &Path) -> impl Fn(io::Error) -> PackError + '_ {
    move |source| PackError::Io {
        path: path.to_path_buf(),
        source,
    }
}

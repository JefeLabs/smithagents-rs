//! Packing the fixtures: valid manifests beside real components, and packing
//! that is safe to repeat while another packer writes the same directory.

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::fs;
use std::path::Path;

use smithagents_plugin_protocol::{HostMode, Manifest, SettingKind};
use smithagents_plugins_tool::{pack, pack_fixtures_once, PackOptions};

const FIXTURES: [&str; 3] = ["fixture-dylib", "fixture-wasm", "fixture-wasm-fs"];

/// What every mode runs.
const SHARED: [&str; 9] = [
    "items", "http", "post", "auth", "rate", "panic", "sleep", "huge", "dupe",
];

/// Parses `<dir>/<id>/plugin.toml` and checks the fixture's declarations, its
/// mode-specific resources and its entry file.
fn check_fixture(dir: &Path, id: &str) {
    let dir = dir.join(id);
    let text = fs::read_to_string(dir.join("plugin.toml")).unwrap();
    let manifest = Manifest::parse(&text).unwrap();
    assert_eq!(manifest.id.as_ref(), id);
    assert_eq!(manifest.hosts, ["fixture.invalid"]);
    assert_eq!(manifest.secrets.len(), 1);
    assert_eq!(manifest.secrets[0].key, "token");
    assert_eq!(manifest.secrets[0].hosts, ["fixture.invalid"]);
    let settings: Vec<_> = manifest
        .settings
        .iter()
        .map(|s| (s.key.as_str(), s.kind))
        .collect();
    assert_eq!(
        settings,
        [("url", SettingKind::Text), ("ms", SettingKind::Text)]
    );
    let kinds: Vec<_> = manifest.resources.iter().map(|r| r.kind.as_str()).collect();
    let entry_name = manifest.entry.unwrap();
    let entry = fs::read(dir.join(&entry_name)).unwrap();
    if id == "fixture-dylib" {
        assert_eq!(manifest.host, HostMode::Dylib);
        assert_eq!(kinds, [&SHARED[..], &["stale-call"]].concat());
        assert_eq!(
            entry_name,
            format!("{DLL_PREFIX}smithagents_plugin_fixture_dylib{DLL_SUFFIX}")
        );
        assert!(!entry.starts_with(b"\0asm"), "the entry is WebAssembly");
    } else {
        assert_eq!(manifest.host, HostMode::Wasm);
        assert_eq!(kinds, [&SHARED[..], &["forever", "spin", "bloat"]].concat());
        assert!(entry.starts_with(b"\0asm"), "the entry is not WebAssembly");
    }
    for file in [dir.join("plugin.toml"), dir.join(&entry_name)] {
        assert_not_writable_by_others(&file);
    }
}

#[cfg(unix)]
fn assert_not_writable_by_others(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path).unwrap().permissions().mode();
    assert_eq!(mode & 0o022, 0, "{} has mode {mode:o}", path.display());
}

#[cfg(not(unix))]
fn assert_not_writable_by_others(_path: &Path) {}

#[test]
fn the_fixtures_are_packed_with_valid_manifests() {
    let packed = pack_fixtures_once();
    assert_eq!(packed.ids, [&FIXTURES[..], &["github"]].concat());
    for id in FIXTURES {
        check_fixture(&packed.dir, id);
    }
}

/// GitHub's own manifest, with the entry named for this platform, beside a
/// real library.
fn check_github(dir: &Path) {
    let dir = dir.join("github");
    let text = fs::read_to_string(dir.join("plugin.toml")).unwrap();
    let manifest = Manifest::parse(&text).unwrap();
    assert_eq!(manifest.id.as_ref(), "github");
    assert_eq!(manifest.host, HostMode::Dylib);
    assert_eq!(manifest.hosts, ["api.github.com"]);
    assert_eq!(manifest.tools[0].name, "gh");
    assert_eq!(manifest.resources.len(), 3);
    let entry = format!("{DLL_PREFIX}smithagents_plugin_github_dylib{DLL_SUFFIX}");
    assert_eq!(manifest.entry.as_deref(), Some(entry.as_str()));
    // Everything but the entry line is the source file's text.
    let source = include_str!("../../../plugins/github/plugin.toml");
    let without_entry = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|l| !l.starts_with("entry = "))
            .map(str::to_string)
            .collect()
    };
    assert_eq!(without_entry(&text), without_entry(source));
    assert!(!fs::read(dir.join(&entry)).unwrap().starts_with(b"\0asm"));
    for file in [dir.join("plugin.toml"), dir.join(&entry)] {
        assert_not_writable_by_others(&file);
    }
}

#[test]
fn github_is_packed_with_its_own_manifest() {
    check_github(&pack_fixtures_once().dir);
}

#[cfg(unix)]
#[test]
fn folders_others_can_write_are_tightened() {
    use std::os::unix::fs::PermissionsExt;

    // What a umask of 002 (or an earlier, laxer run) leaves: discovery
    // refuses a plugins folder, or a plugin's folder, that others can write.
    let out = tempfile::tempdir().unwrap();
    let root = out.path().join("plugins");
    for dir in [root.clone(), root.join("fixture-dylib")] {
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
    }

    pack(&PackOptions {
        release: true,
        out_dir: root.clone(),
        fixtures: true,
    })
    .unwrap();

    assert_not_writable_by_others(&root);
    for id in FIXTURES {
        assert_not_writable_by_others(&root.join(id));
    }
    // Nothing else of the folder's mode changes.
    let mode = fs::metadata(&root).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
}

#[test]
fn concurrent_packs_into_one_directory_both_succeed() {
    let out = tempfile::tempdir().unwrap();
    let packers: Vec<_> = (0..2)
        .map(|_| {
            let out_dir = out.path().to_path_buf();
            std::thread::spawn(move || {
                pack(&PackOptions {
                    release: true,
                    out_dir,
                    fixtures: true,
                })
            })
        })
        .collect();
    for packer in packers {
        packer.join().unwrap().unwrap();
    }
    for id in FIXTURES {
        check_fixture(out.path(), id);
    }
    check_github(out.path());
    // Only the packed files remain: no temporary files.
    let library = format!("{DLL_PREFIX}smithagents_plugin_fixture_dylib{DLL_SUFFIX}");
    for (id, entry) in [
        ("fixture-dylib", library.as_str()),
        ("fixture-wasm", "smithagents_plugin_fixture_wasm.wasm"),
    ] {
        let mut names: Vec<_> = fs::read_dir(out.path().join(id))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        let mut expected = vec![entry, "plugin.toml"];
        expected.sort();
        assert_eq!(names, expected);
    }
}

#[test]
fn without_fixtures_only_the_real_plugins_are_packed() {
    let out = tempfile::tempdir().unwrap();
    let packed = pack(&PackOptions {
        release: false,
        out_dir: out.path().to_path_buf(),
        fixtures: false,
    })
    .unwrap();
    assert_eq!(packed.ids, ["github"]);
    check_github(out.path());
}

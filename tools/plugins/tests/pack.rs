//! Packing the fixtures: valid manifests beside real components, and packing
//! that is safe to repeat while another packer writes the same directory.

use std::fs;
use std::path::Path;

use smithagents_plugin_protocol::{HostMode, Manifest, SettingKind};
use smithagents_plugins_tool::{pack, pack_fixtures_once, PackOptions};

const BEHAVIOURS: [&str; 12] = [
    "items", "http", "post", "auth", "rate", "panic", "sleep", "huge", "dupe", "forever", "spin",
    "bloat",
];

/// Parses `<dir>/<id>/plugin.toml` and checks it describes a wasm fixture
/// whose entry file is a WebAssembly binary.
fn check_fixture(dir: &Path, id: &str) {
    let dir = dir.join(id);
    let text = fs::read_to_string(dir.join("plugin.toml")).unwrap();
    let manifest = Manifest::parse(&text).unwrap();
    assert_eq!(manifest.id.as_ref(), id);
    assert_eq!(manifest.host, HostMode::Wasm);
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
    assert_eq!(kinds, BEHAVIOURS);
    let entry = fs::read(dir.join(manifest.entry.unwrap())).unwrap();
    assert!(entry.starts_with(b"\0asm"), "the entry is not WebAssembly");
}

#[test]
fn the_fixtures_are_packed_with_valid_manifests() {
    let packed = pack_fixtures_once();
    assert_eq!(packed.ids, ["fixture-wasm", "fixture-wasm-fs"]);
    for id in &packed.ids {
        check_fixture(&packed.dir, id);
    }
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
    for id in ["fixture-wasm", "fixture-wasm-fs"] {
        check_fixture(out.path(), id);
    }
    // Only the packed directories remain: no temporary files.
    let mut names: Vec<_> = fs::read_dir(out.path().join("fixture-wasm"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["plugin.toml", "smithagents_plugin_fixture_wasm.wasm"]
    );
}

#[test]
fn without_fixtures_nothing_is_packed_yet() {
    let out = tempfile::tempdir().unwrap();
    let packed = pack(&PackOptions {
        release: false,
        out_dir: out.path().to_path_buf(),
        fixtures: false,
    })
    .unwrap();
    assert!(packed.ids.is_empty());
}

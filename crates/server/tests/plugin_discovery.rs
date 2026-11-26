//! Which plugin directories may load, and that loading builds the right runtime.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use smithagents_plugin_host::{LoadError, WasmEngine};
use smithagents_plugin_protocol::HostMode;
use smithagents_plugins_tool::pack_fixtures_once;
use smithagents_server::plugins::{discover, load_runtime, Origin};

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Copies a packed fixture to `root/<id>`, rewriting its id, with explicit modes.
fn install_fixture(root: &Path, fixture: &str, id: &str) -> PathBuf {
    let src = pack_fixtures_once().dir.join(fixture);
    let dst = root.join(id);
    fs::create_dir_all(&dst).unwrap();
    chmod(root, 0o755);
    for entry in fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_name() == "plugin.toml" {
            let text = fs::read_to_string(entry.path()).unwrap();
            let text = text.replacen(&format!("id = \"{fixture}\""), &format!("id = \"{id}\""), 1);
            fs::write(&to, text).unwrap();
        } else {
            fs::copy(entry.path(), &to).unwrap();
        }
        chmod(&to, 0o644);
    }
    chmod(&dst, 0o755);
    dst
}

/// A minimal wasm-mode plugin directory with a dummy entry (never loaded).
fn plain_plugin(root: &Path, id: &str, extra: &str) -> PathBuf {
    let dir = root.join(id);
    fs::create_dir_all(&dir).unwrap();
    chmod(root, 0o755);
    fs::write(
        dir.join("plugin.toml"),
        format!(
            "id = \"{id}\"\nname = \"X\"\nversion = \"0.1.0\"\ncontract = 1\nhost = \"wasm\"\nentry = \"p.wasm\"\n{extra}"
        ),
    )
    .unwrap();
    fs::write(dir.join("p.wasm"), b"x").unwrap();
    chmod(&dir.join("p.wasm"), 0o644);
    chmod(&dir.join("plugin.toml"), 0o644);
    chmod(&dir, 0o755);
    dir
}

fn reason_for(root: &Path, dir: &Path) -> String {
    let (found, rejected) = discover(None, root);
    assert!(found.is_empty(), "unexpected plugins: {found:?}");
    rejected
        .into_iter()
        .find(|r| r.dir == dir)
        .unwrap_or_else(|| panic!("{dir:?} was not rejected"))
        .reason
}

#[test]
fn missing_and_empty_directories_yield_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (found, rejected) = discover(Some(&tmp.path().join("nope")), &tmp.path().join("nada"));
    assert!(found.is_empty() && rejected.is_empty());
    let (builtin, installed) = (tmp.path().join("b"), tmp.path().join("i"));
    fs::create_dir(&builtin).unwrap();
    fs::create_dir(&installed).unwrap();
    chmod(&builtin, 0o755);
    let (found, rejected) = discover(Some(&builtin), &installed);
    assert!(found.is_empty() && rejected.is_empty());
}

#[tokio::test]
async fn builtin_dylib_and_installed_wasm_are_discovered_and_load() {
    let tmp = tempfile::tempdir().unwrap();
    let builtin = tmp.path().join("builtin");
    let installed = tmp.path().join("installed");
    install_fixture(&builtin, "fixture-dylib", "fixture-dylib");
    install_fixture(&installed, "fixture-wasm", "fixture-wasm");
    let (found, rejected) = discover(Some(&builtin), &installed);
    assert!(rejected.is_empty(), "{rejected:?}");
    let ids: Vec<_> = found.iter().map(|d| d.manifest().id.to_string()).collect();
    assert_eq!(ids, ["fixture-dylib", "fixture-wasm"]);
    assert_eq!(found[0].origin(), Origin::Builtin);
    assert_eq!(found[1].origin(), Origin::Installed);

    let engine = WasmEngine::new(None).unwrap();
    let dylib = load_runtime(&found[0], &engine).await.unwrap();
    assert_eq!(dylib.mode(), HostMode::Dylib);
    let wasm = load_runtime(&found[1], &engine).await.unwrap();
    assert_eq!(wasm.mode(), HostMode::Wasm);
}

#[cfg(feature = "test-util")]
#[tokio::test]
async fn load_runtime_refuses_an_installed_dylib_even_if_discover_was_bypassed() {
    let tmp = tempfile::tempdir().unwrap();
    install_fixture(tmp.path(), "fixture-dylib", "fixture-dylib");
    let (found, _) = discover(Some(tmp.path()), Path::new("/nonexistent"));
    let d = found[0].forged_origin_for_test(Origin::Installed);
    let engine = WasmEngine::new(None).unwrap();
    let err = load_runtime(&d, &engine).await.err().unwrap();
    assert!(matches!(err, LoadError::Refused { .. }), "{err:?}");
}

#[test]
fn installed_dylib_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    install_fixture(tmp.path(), "fixture-dylib", "fixture-dylib");
    let (found, rejected) = discover(None, tmp.path());
    assert!(found.is_empty());
    assert_eq!(rejected[0].reason, "native plugins can only be built in");
}

#[test]
fn process_and_in_process_are_rejected() {
    for mode in ["process", "in-process"] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("p-one");
        fs::create_dir(&dir).unwrap();
        fs::write(
            dir.join("plugin.toml"),
            format!("id = \"p-one\"\nname = \"X\"\nversion = \"0.1.0\"\ncontract = 1\nhost = \"{mode}\"\n"),
        )
        .unwrap();
        chmod(&dir.join("plugin.toml"), 0o644);
        chmod(&dir, 0o755);
        assert_eq!(
            reason_for(tmp.path(), &dir),
            format!("host mode {mode} is not supported yet")
        );
    }
}

#[test]
fn directory_name_must_equal_the_id() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = plain_plugin(tmp.path(), "right-name", "");
    let moved = tmp.path().join("wrong-name");
    fs::rename(&dir, &moved).unwrap();
    assert_eq!(
        reason_for(tmp.path(), &moved),
        "the directory name does not match the plugin id right-name"
    );
}

#[test]
fn contract_must_be_supported() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = plain_plugin(tmp.path(), "future", "");
    let text = fs::read_to_string(dir.join("plugin.toml")).unwrap();
    fs::write(
        dir.join("plugin.toml"),
        text.replace("contract = 1", "contract = 99"),
    )
    .unwrap();
    let reason = reason_for(tmp.path(), &dir);
    assert!(reason.contains("unsupported contract 99"), "{reason}");
}

#[test]
fn invalid_manifest_rejects_only_its_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = tmp.path().join("bad-one");
    fs::create_dir(&bad).unwrap();
    fs::write(bad.join("plugin.toml"), "not toml [").unwrap();
    chmod(&bad.join("plugin.toml"), 0o644);
    chmod(&bad, 0o755);
    let noman = tmp.path().join("no-manifest");
    fs::create_dir(&noman).unwrap();
    chmod(&noman, 0o755);
    plain_plugin(tmp.path(), "good-one", "");
    let (found, rejected) = discover(None, tmp.path());
    assert_eq!(found.len(), 1);
    assert_eq!(rejected.len(), 2);
    let by_dir = |d: &Path| rejected.iter().find(|r| r.dir == d).unwrap().reason.clone();
    assert_eq!(
        by_dir(&bad),
        "plugin.toml is not valid TOML for a plugin manifest (line 1)"
    );
    assert_eq!(by_dir(&noman), "plugin.toml cannot be read");
}

#[test]
fn builtin_id_wins_over_installed() {
    let tmp = tempfile::tempdir().unwrap();
    let builtin = tmp.path().join("b");
    let installed = tmp.path().join("i");
    plain_plugin(&builtin, "same-id", "");
    let dup = plain_plugin(&installed, "same-id", "");
    let (found, rejected) = discover(Some(&builtin), &installed);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].origin(), Origin::Builtin);
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].dir, dup);
    assert_eq!(rejected[0].reason, "a built-in plugin has this id");
}

#[test]
fn entry_must_exist_be_a_file_and_stay_inside() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = plain_plugin(tmp.path(), "no-entry", "");
    fs::remove_file(dir.join("p.wasm")).unwrap();
    assert_eq!(
        reason_for(tmp.path(), &dir),
        "the entry file does not exist"
    );

    let tmp = tempfile::tempdir().unwrap();
    let dir = plain_plugin(tmp.path(), "dir-entry", "");
    fs::remove_file(dir.join("p.wasm")).unwrap();
    fs::create_dir(dir.join("p.wasm")).unwrap();
    assert_eq!(
        reason_for(tmp.path(), &dir),
        "the entry file is not a regular file"
    );

    // A symlink pointing out of the plugin directory.
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside.wasm");
    fs::write(&outside, b"x").unwrap();
    chmod(&outside, 0o644);
    let root = tmp.path().join("root");
    let dir = plain_plugin(&root, "escape", "");
    fs::remove_file(dir.join("p.wasm")).unwrap();
    symlink(&outside, dir.join("p.wasm")).unwrap();
    assert_eq!(
        reason_for(&root, &dir),
        "the entry file is outside the plugin directory"
    );

    // A symlink that stays inside is fine.
    let tmp = tempfile::tempdir().unwrap();
    let dir = plain_plugin(tmp.path(), "inside", "");
    fs::write(dir.join("real.bin"), b"x").unwrap();
    chmod(&dir.join("real.bin"), 0o644);
    fs::remove_file(dir.join("p.wasm")).unwrap();
    symlink("real.bin", dir.join("p.wasm")).unwrap();
    let (found, rejected) = discover(None, tmp.path());
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(found.len(), 1);
}

#[test]
fn entry_writable_by_others_is_rejected() {
    for mode in [0o664, 0o646] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = plain_plugin(tmp.path(), "loose-entry", "");
        chmod(&dir.join("p.wasm"), mode);
        assert_eq!(
            reason_for(tmp.path(), &dir),
            "the entry file is writable by other users"
        );
    }
}

#[test]
fn plugin_directory_writable_by_others_is_rejected() {
    for mode in [0o775, 0o757] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = plain_plugin(tmp.path(), "loose-dir", "");
        chmod(&dir, mode);
        assert_eq!(
            reason_for(tmp.path(), &dir),
            "the plugin directory is writable by other users"
        );
    }
}

#[test]
fn plugin_directory_symlink_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("elsewhere");
    plain_plugin(&real, "linked", "");
    let root = tmp.path().join("root");
    fs::create_dir(&root).unwrap();
    let link = root.join("linked");
    symlink(real.join("linked"), &link).unwrap();
    assert_eq!(
        reason_for(&root, &link),
        "the plugin directory is a symbolic link"
    );
}

#[test]
fn manifest_must_be_a_small_regular_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = plain_plugin(tmp.path(), "linked-manifest", "");
    let real = tmp.path().join("real.toml");
    fs::rename(dir.join("plugin.toml"), &real).unwrap();
    symlink(&real, dir.join("plugin.toml")).unwrap();
    assert_eq!(
        reason_for(tmp.path(), &dir),
        "plugin.toml is not a regular file"
    );

    let tmp = tempfile::tempdir().unwrap();
    let dir = plain_plugin(tmp.path(), "big-manifest", "");
    let mut text = fs::read_to_string(dir.join("plugin.toml")).unwrap();
    text.push_str(&"# padding\n".repeat(30_000));
    fs::write(dir.join("plugin.toml"), text).unwrap();
    assert_eq!(
        reason_for(tmp.path(), &dir),
        "plugin.toml is larger than 256 KiB"
    );
}

#[test]
fn results_are_sorted_by_id() {
    let tmp = tempfile::tempdir().unwrap();
    for id in ["zeta-one", "alpha-one", "mid-one"] {
        plain_plugin(tmp.path(), id, "");
    }
    let (found, _) = discover(None, tmp.path());
    let ids: Vec<_> = found.iter().map(|d| d.manifest().id.to_string()).collect();
    assert_eq!(ids, ["alpha-one", "mid-one", "zeta-one"]);
}

const SECRET: &str = "ghp_DISTINCTIVE_TOKEN_123";

#[test]
fn manifest_errors_never_quote_the_file() {
    // A syntax error on a line holding the secret, and a wrong-type value.
    for (id, body) in [
        ("syntax-err", format!("name = \"{SECRET}\" oops\n")),
        ("wrong-type", format!("version = {{ x = \"{SECRET}\" }}\n")),
        ("bad-field", format!("extra = \"{SECRET}\"\n")),
        ("bad-entry", format!("entry = \"{SECRET}/x\"\n")),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = plain_plugin(tmp.path(), id, "");
        let text = fs::read_to_string(dir.join("plugin.toml")).unwrap();
        let text = if id == "bad-entry" {
            text.replace("entry = \"p.wasm\"\n", &body)
        } else if id == "wrong-type" {
            text.replace("version = \"0.1.0\"\n", &body)
        } else {
            format!("{text}{body}")
        };
        fs::write(dir.join("plugin.toml"), text).unwrap();
        let reason = reason_for(tmp.path(), &dir);
        assert!(!reason.contains(SECRET), "{id}: {reason}");
        assert!(!reason.contains('\n'), "{id}: {reason}");
        assert!(
            reason.starts_with("plugin.toml is not valid"),
            "{id}: {reason}"
        );
    }
}

#[test]
fn world_writable_builtin_root_rejects_the_builtin_side() {
    let tmp = tempfile::tempdir().unwrap();
    let builtin = tmp.path().join("b");
    plain_plugin(&builtin, "one-plugin", "");
    chmod(&builtin, 0o777);
    let (found, rejected) = discover(Some(&builtin), &tmp.path().join("none"));
    assert!(found.is_empty());
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].dir, builtin);
    assert_eq!(
        rejected[0].reason,
        "the built-in plugins directory is writable by other users"
    );
}

#[test]
fn manifest_writable_by_others_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = plain_plugin(tmp.path(), "loose-toml", "");
    chmod(&dir.join("plugin.toml"), 0o664);
    assert_eq!(
        reason_for(tmp.path(), &dir),
        "plugin.toml is writable by other users"
    );
}

#[test]
fn builtin_directory_between_a_symlinked_entry_and_its_target_must_be_trusted() {
    let tmp = tempfile::tempdir().unwrap();
    let builtin = tmp.path().join("b");
    let dir = plain_plugin(&builtin, "linked-in", "");
    let sub = dir.join("sub");
    fs::create_dir(&sub).unwrap();
    fs::rename(dir.join("p.wasm"), sub.join("real.bin")).unwrap();
    fs::remove_file(dir.join("p.wasm")).ok();
    symlink("sub/real.bin", dir.join("p.wasm")).unwrap();
    chmod(&sub, 0o777);
    let (found, rejected) = discover(Some(&builtin), &tmp.path().join("none"));
    assert!(found.is_empty());
    assert_eq!(
        rejected[0].reason,
        "a directory holding the entry file is writable by other users"
    );
    chmod(&sub, 0o755);
    let (found, rejected) = discover(Some(&builtin), &tmp.path().join("none"));
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(found.len(), 1);
}

#[tokio::test]
async fn a_directory_swapped_after_discovery_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    install_fixture(tmp.path(), "fixture-wasm", "fixture-wasm");
    let (found, _) = discover(None, tmp.path());
    fs::rename(
        tmp.path().join("fixture-wasm"),
        tmp.path().join("moved-away"),
    )
    .unwrap();
    install_fixture(tmp.path(), "fixture-wasm", "fixture-wasm");
    let engine = WasmEngine::new(None).unwrap();
    let err = load_runtime(&found[0], &engine).await.err().unwrap();
    assert!(matches!(err, LoadError::Refused { .. }), "{err:?}");
}

#[tokio::test]
async fn an_entry_made_writable_after_discovery_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = install_fixture(tmp.path(), "fixture-wasm", "fixture-wasm");
    let (found, _) = discover(None, tmp.path());
    let entry = found[0].manifest().entry.clone().unwrap();
    chmod(&dir.join(entry), 0o666);
    let engine = WasmEngine::new(None).unwrap();
    let err = load_runtime(&found[0], &engine).await.err().unwrap();
    assert!(matches!(err, LoadError::Refused { .. }), "{err:?}");
}

#[test]
fn overlapping_roots_skip_the_installed_side() {
    let tmp = tempfile::tempdir().unwrap();
    let builtin = tmp.path().join("b");
    plain_plugin(&builtin, "one-plugin", "");
    for installed in [
        builtin.clone(),
        builtin.join("one-plugin"),
        tmp.path().to_path_buf(),
    ] {
        let (found, rejected) = discover(Some(&builtin), &installed);
        assert_eq!(found.len(), 1, "{installed:?}");
        assert_eq!(found[0].origin(), Origin::Builtin);
        assert!(
            rejected.iter().any(|r| r.dir == installed
                && r.reason == "the installed plugins directory overlaps the built-in one"),
            "{rejected:?}"
        );
    }
}

#[test]
fn a_rejected_builtin_still_reserves_its_id() {
    let tmp = tempfile::tempdir().unwrap();
    let builtin = tmp.path().join("b");
    let installed = tmp.path().join("i");
    let broken = plain_plugin(&builtin, "taken-id", "");
    fs::write(broken.join("plugin.toml"), "garbage [").unwrap();
    let dup = plain_plugin(&installed, "taken-id", "");
    let (found, rejected) = discover(Some(&builtin), &installed);
    assert!(found.is_empty());
    let r = rejected.iter().find(|r| r.dir == dup).unwrap();
    assert_eq!(r.reason, "a built-in plugin has this id");
}

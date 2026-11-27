//! Loading native plugins: the packed fixture answers calls, and files that
//! are not a plugin built against this interface are refused with a readable
//! error.

use std::path::{Path, PathBuf};

use serde_json::json;
use smithagents_plugin_loader::{builtin_dir, load, load_unchecked, DylibLoadError, LoadedPlugin};
use smithagents_plugin_protocol::dylib::{BoxedDylibHost, DylibHost, DYLIB_ENTRY_SYMBOL};
use smithagents_plugin_protocol::{HostReply, Manifest, PluginCall, PluginReply, SyncRequest};
use smithagents_plugins_tool::{pack_fixtures_once, pack_library, workspace_root, BuiltinScratch};

/// A host that refuses every call: these tests make none.
struct NoHost;

impl DylibHost for NoHost {
    extern "C" fn call(&self, _input: stabby::vec::Vec<u8>) -> stabby::vec::Vec<u8> {
        let reply = HostReply::BadCall {
            message: "this test has no host".into(),
        };
        stabby::vec::Vec::from(serde_json::to_vec(&reply).unwrap().as_slice())
    }
}

fn host() -> BoxedDylibHost {
    stabby::boxed::Box::new(NoHost).into()
}

/// The packed `fixture-dylib` library, as its manifest names it: where the
/// packer wrote it, which is outside the plugins folder of this test.
fn fixture() -> PathBuf {
    let dir = pack_fixtures_once().dir.join("fixture-dylib");
    let manifest = std::fs::read_to_string(dir.join("plugin.toml")).unwrap();
    let manifest = Manifest::parse(&manifest).unwrap();
    dir.join(manifest.entry.unwrap())
}

/// A copy of [`fixture`] inside this test executable's plugins folder, the
/// only place [`load`] opens a library from.
fn builtin_fixture(scratch: &BuiltinScratch) -> PathBuf {
    let from = fixture();
    let to = scratch.path().join(from.file_name().unwrap());
    std::fs::copy(&from, &to).unwrap();
    to
}

/// [`load_unchecked`] for a file a test made itself, wherever it is.
fn load_anywhere(path: &Path) -> Result<LoadedPlugin, DylibLoadError> {
    // SAFETY: every path given here is a file one of these tests wrote or a
    // library built from this workspace against this plugin interface.
    #[expect(
        unsafe_code,
        reason = "these tests load libraries they built themselves"
    )]
    unsafe {
        load_unchecked(path, host())
    }
}

/// One of the test libraries beside this file, built and packed into `out`.
fn test_library(name: &str, lib: &str, out: &Path) -> PathBuf {
    let dir = workspace_root()
        .join("crates/plugin-loader/tests")
        .join(name);
    pack_library(&dir, lib, out).unwrap_or_else(|e| panic!("cannot pack {name}: {e}"))
}

fn sync(plugin: &LoadedPlugin, call: u64, resource: &str) -> PluginReply {
    let call = PluginCall::Sync {
        call,
        request: SyncRequest {
            resource: resource.into(),
            cursor: None,
            settings: json!({}),
        },
    };
    let reply = plugin.call(serde_json::to_vec(&call).unwrap());
    serde_json::from_slice(&reply).unwrap()
}

/// The refusal for `path`. Its message is one line and names the file.
fn refused(path: &Path) -> DylibLoadError {
    let Err(err) = load_anywhere(path) else {
        panic!("{} was loaded", path.display());
    };
    let message = err.to_string();
    assert!(!message.contains('\n'), "{message}");
    let file = path.file_name().unwrap().to_str().unwrap();
    assert!(message.contains(file), "{message}");
    err
}

#[test]
fn the_packed_fixture_answers_a_sync_call() {
    let scratch = BuiltinScratch::new("loader");
    let plugin = load(&builtin_fixture(&scratch), host()).unwrap();
    match sync(&plugin, 1, "items") {
        PluginReply::Sync(Ok(page)) => {
            let ids: Vec<_> = page.upserts.iter().map(|i| i.id.as_str()).collect();
            assert_eq!(ids, ["item-1", "item-2"]);
        }
        other => panic!("expected a page, got {other:?}"),
    }
}

#[test]
fn the_fixture_answers_a_panic_and_unreadable_input_and_keeps_serving() {
    let scratch = BuiltinScratch::new("loader");
    let plugin = load(&builtin_fixture(&scratch), host()).unwrap();
    match sync(&plugin, 1, "panic") {
        PluginReply::Panicked { message } => {
            assert!(message.starts_with("fixture panic"), "{message}")
        }
        other => panic!("expected Panicked, got {other:?}"),
    }
    let reply: PluginReply = serde_json::from_slice(&plugin.call(b"{".to_vec())).unwrap();
    assert!(matches!(reply, PluginReply::BadCall { .. }), "{reply:?}");
    assert!(matches!(
        sync(&plugin, 2, "items"),
        PluginReply::Sync(Ok(_))
    ));
}

#[test]
fn a_missing_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let err = refused(&dir.path().join("libabsent.dylib"));
    assert!(matches!(err, DylibLoadError::Open { .. }), "{err:?}");
    // The system's reason follows libloading's own "dlopen failed".
    let message = err.to_string();
    assert!(!message.ends_with("dlopen failed"), "{message}");
}

#[test]
fn a_file_that_is_not_a_library_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("libtext.dylib");
    std::fs::write(&path, "not a library").unwrap();
    let err = refused(&path);
    assert!(matches!(err, DylibLoadError::Open { .. }), "{err:?}");
    assert!(err.to_string().starts_with("cannot load "), "{err}");
}

#[test]
fn a_library_without_the_entry_symbol_is_refused() {
    let out = tempfile::tempdir().unwrap();
    let path = test_library("no-entry", "smithagents_test_no_entry", out.path());
    let err = refused(&path);
    assert!(matches!(err, DylibLoadError::NoEntry { .. }), "{err:?}");
    let symbol = std::str::from_utf8(DYLIB_ENTRY_SYMBOL).unwrap();
    assert!(err.to_string().contains(symbol), "{err}");
}

#[test]
fn an_entry_with_another_signature_is_refused_as_a_mismatch() {
    let out = tempfile::tempdir().unwrap();
    let path = test_library("wrong-sig", "smithagents_test_wrong_sig", out.path());
    let err = refused(&path);
    assert!(matches!(err, DylibLoadError::Mismatch { .. }), "{err:?}");
    assert!(err.to_string().contains("mismatch"), "{err}");
}

#[test]
fn a_library_outside_the_plugins_folder_is_not_opened() {
    // Opening a library runs its code in this process, so the folder is
    // checked before anything is opened, and by the loader itself: no caller
    // has to remember to.
    let outside = fixture();
    assert!(!outside.starts_with(builtin_dir().unwrap()));
    let Err(err) = load(&outside, host()) else {
        panic!("a library outside the plugins folder was loaded");
    };
    assert!(matches!(err, DylibLoadError::NotBuiltin { .. }), "{err:?}");
    let message = err.to_string();
    assert!(!message.contains('\n'), "{message}");
    let file = outside.file_name().unwrap().to_str().unwrap();
    assert!(message.contains(file), "{message}");
}

#[cfg(unix)]
#[test]
fn a_link_inside_the_plugins_folder_to_a_library_outside_is_not_opened() {
    let scratch = BuiltinScratch::new("loader-link");
    let link = scratch.path().join("liblinked.dylib");
    std::os::unix::fs::symlink(fixture(), &link).unwrap();
    let Err(err) = load(&link, host()) else {
        panic!("a link to a library outside the plugins folder was followed");
    };
    assert!(matches!(err, DylibLoadError::NotBuiltin { .. }), "{err:?}");
}

#[test]
fn a_missing_file_in_the_plugins_folder_is_refused_as_before() {
    let scratch = BuiltinScratch::new("loader-missing");
    let Err(err) = load(&scratch.path().join("libabsent.dylib"), host()) else {
        panic!("a missing library was loaded");
    };
    assert!(matches!(err, DylibLoadError::Open { .. }), "{err:?}");
}

//! Crate-graph smoke test for the native UI stack.
//!
//! Building and opening a window is not a test (it needs a desktop session).
//! Instead this checks, from `cargo metadata` alone, that the workspace
//! resolves exactly one GPUI core (`gpui-pre`, the snapshot `gpui-kit`
//! pins) and that the `host-desktop` binary target exists.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn metadata() -> Value {
    let workspace_manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--manifest-path",
        ])
        .arg(&workspace_manifest)
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata emits JSON")
}

fn packages(meta: &Value) -> &[Value] {
    meta["packages"].as_array().expect("packages array")
}

#[test]
fn graph_resolves_a_single_gpui_core() {
    let meta = metadata();
    let versions: Vec<&str> = packages(&meta)
        .iter()
        .filter(|p| p["name"] == "gpui-pre")
        .map(|p| p["version"].as_str().unwrap_or("?"))
        .collect();
    assert_eq!(
        versions.len(),
        1,
        "expected exactly one gpui-pre in the graph, found {versions:?}"
    );
}

/// `gpui-fast` (an alternative GPUI engine) shows up in `Cargo.lock` and in
/// `cargo metadata` as an optional dependency of `gpui-kit`, but is built only
/// when `gpui-kit`'s `gpui-fast` feature is on. Keep it off: one engine.
#[test]
fn gpui_kit_stays_on_the_upstream_engine() {
    let meta = metadata();
    let kit_id = packages(&meta)
        .iter()
        .find(|p| p["name"] == "gpui-kit")
        .and_then(|p| p["id"].as_str())
        .expect("gpui-kit is in the graph");
    let node = meta["resolve"]["nodes"]
        .as_array()
        .expect("resolve nodes")
        .iter()
        .find(|n| n["id"] == kit_id)
        .expect("gpui-kit has a resolve node");
    let features: Vec<&str> = node["features"]
        .as_array()
        .expect("features array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        !features.contains(&"gpui-fast"),
        "gpui-kit's gpui-fast feature is on: {features:?}"
    );
}

#[test]
fn host_desktop_binary_target_exists() {
    let meta = metadata();
    let host = packages(&meta)
        .iter()
        .find(|p| p["name"] == "host-desktop")
        .expect("host-desktop package is a workspace member");
    let has_bin = host["targets"]
        .as_array()
        .expect("targets array")
        .iter()
        .any(|t| {
            t["name"] == "host-desktop"
                && t["kind"]
                    .as_array()
                    .is_some_and(|k| k.iter().any(|k| k == "bin"))
        });
    assert!(
        has_bin,
        "host-desktop has no binary target named host-desktop"
    );
}

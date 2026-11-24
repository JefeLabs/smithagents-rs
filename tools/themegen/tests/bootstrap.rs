use std::path::Path;
use std::process::Command;

use smithagents_theme::tokens::ThemeSet;
use themegen::bootstrap::render_bootstrap;

#[test]
fn bootstrap_basics() {
    let js = render_bootstrap(ThemeSet::embedded());
    assert!(js.contains("__smithagentsApplyTheme"));
    assert!(js.contains("smith-light"));
    assert_eq!(js, render_bootstrap(ThemeSet::embedded()));
    for kw in ["import ", "export "] {
        assert!(!js.contains(kw), "script contains `{kw}`");
    }
}

#[test]
fn bootstrap_behaves_in_node() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("bootstrap.js");
    std::fs::write(&script, render_bootstrap(ThemeSet::embedded())).unwrap();
    let check = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/bootstrap_check.mjs");
    let out = Command::new("node")
        .arg(&check)
        .arg(&script)
        .output()
        .expect("node must be installed to run bootstrap_check.mjs");
    assert!(
        out.status.success(),
        "node check failed ({}):\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

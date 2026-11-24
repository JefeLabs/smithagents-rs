use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn repo_has_no_literal_colors() {
    let findings = themegen::lint::lint_repo(&root()).unwrap();
    assert!(findings.is_empty(), "{findings:#?}");
}

#[test]
fn generated_files_are_current() {
    let r = root();
    themegen::check(&r.join("design/tokens"), &r.join("design/generated")).unwrap();
}

use std::fs;
use std::path::{Path, PathBuf};

use themegen::{check, generate, GenError};

fn repo_tokens() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../design/tokens")
}

fn copy_tokens(dst: &Path) {
    fs::create_dir_all(dst.join("themes")).unwrap();
    let src = repo_tokens();
    for f in ["base.json", "agent-palette.json"] {
        fs::copy(src.join(f), dst.join(f)).unwrap();
    }
    for e in fs::read_dir(src.join("themes")).unwrap() {
        let p = e.unwrap().path();
        fs::copy(&p, dst.join("themes").join(p.file_name().unwrap())).unwrap();
    }
}

#[test]
fn generate_then_check_is_ok() {
    let d = tempfile::tempdir().unwrap();
    let tokens = d.path().join("tokens");
    let out = d.path().join("out");
    copy_tokens(&tokens);
    let written = generate(&tokens, &out).unwrap();
    assert_eq!(written.len(), 2);
    assert!(out.join("theme.css").exists());
    assert!(out.join("theme-bootstrap.js").exists());
    check(&tokens, &out).unwrap();
}

#[test]
fn edited_token_is_drift() {
    let d = tempfile::tempdir().unwrap();
    let tokens = d.path().join("tokens");
    let out = d.path().join("out");
    copy_tokens(&tokens);
    generate(&tokens, &out).unwrap();
    let p = tokens.join("themes/smith-light.json");
    let s = fs::read_to_string(&p).unwrap();
    fs::write(&p, s.replace("\"l\": 0.985", "\"l\": 0.98")).unwrap();
    match check(&tokens, &out) {
        Err(GenError::Drift { path }) => assert!(path.ends_with("theme.css"), "{path:?}"),
        other => panic!("expected drift, got {other:?}"),
    }
}

#[test]
fn missing_file_is_drift() {
    let d = tempfile::tempdir().unwrap();
    let tokens = d.path().join("tokens");
    copy_tokens(&tokens);
    let out = d.path().join("out");
    fs::create_dir_all(&out).unwrap();
    assert!(matches!(check(&tokens, &out), Err(GenError::Drift { .. })));
}

#[test]
fn invalid_tokens_write_nothing() {
    let d = tempfile::tempdir().unwrap();
    let tokens = d.path().join("tokens");
    let out = d.path().join("out");
    copy_tokens(&tokens);
    let p = tokens.join("themes/smith-dark.json");
    let s = fs::read_to_string(&p).unwrap();
    let stripped: String = s
        .lines()
        .filter(|l| !l.contains("\"border\""))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&p, stripped).unwrap();
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("theme.css"), b"sentinel").unwrap();

    let err = generate(&tokens, &out).unwrap_err();
    match &err {
        GenError::Invalid(errs) => assert!(!errs.is_empty()),
        other => panic!("expected Invalid, got {other:?}"),
    }
    let msg = err.to_string();
    assert!(
        msg.contains("smith-dark") && msg.contains("border"),
        "{msg}"
    );
    assert_eq!(fs::read(out.join("theme.css")).unwrap(), b"sentinel");
    let names: Vec<_> = fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "unexpected files: {names:?}");
}

#[test]
fn third_theme_appears_in_outputs() {
    let d = tempfile::tempdir().unwrap();
    let tokens = d.path().join("tokens");
    let out = d.path().join("out");
    copy_tokens(&tokens);
    let s = fs::read_to_string(tokens.join("themes/smith-dark.json")).unwrap();
    let s = s
        .replace("\"id\": \"smith-dark\"", "\"id\": \"smith-night\"")
        .replace("\"name\": \"Smith Dark\"", "\"name\": \"Smith Night\"");
    fs::write(tokens.join("themes/smith-night.json"), s).unwrap();
    generate(&tokens, &out).unwrap();
    let css = fs::read_to_string(out.join("theme.css")).unwrap();
    assert!(css.contains("[data-theme=\"smith-night\"]"));
    let js = fs::read_to_string(out.join("theme-bootstrap.js")).unwrap();
    assert!(js.contains("smith-night"));
}

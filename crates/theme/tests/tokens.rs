use std::fs;
use std::path::{Path, PathBuf};

use smithagents_theme::tokens::{Appearance, ThemeSet, TokenError, COLOR_TOKENS};

fn tokens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../design/tokens")
}

fn copy_tokens(dst: &Path) {
    let src = tokens_dir();
    fs::create_dir_all(dst.join("themes")).unwrap();
    fs::copy(src.join("base.json"), dst.join("base.json")).unwrap();
    fs::copy(
        src.join("agent-palette.json"),
        dst.join("agent-palette.json"),
    )
    .unwrap();
    for e in fs::read_dir(src.join("themes")).unwrap() {
        let p = e.unwrap().path();
        fs::copy(&p, dst.join("themes").join(p.file_name().unwrap())).unwrap();
    }
}

#[test]
fn embedded_has_both_themes_with_all_tokens() {
    let set = ThemeSet::embedded();
    let ids: Vec<&str> = set.themes.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["smith-dark", "smith-light"]);
    assert_eq!(
        set.theme("smith-dark").unwrap().appearance,
        Appearance::Dark
    );
    assert_eq!(
        set.theme("smith-light").unwrap().appearance,
        Appearance::Light
    );
    for t in &set.themes {
        for name in COLOR_TOKENS {
            assert!(t.colors.contains_key(*name), "{} missing {name}", t.id);
        }
    }
    assert_eq!(set.base.system_pair.dark, "smith-dark");
    assert!(set.theme("nope").is_none());
}

#[test]
fn agent_hues_match_palette() {
    let set = ThemeSet::embedded();
    assert_eq!(set.agent_hues, [190.0, 220.0, 250.0, 280.0, 310.0, 340.0]);
    let core: Vec<f64> = smithagents_core::palette::palette()
        .iter()
        .map(|c| f64::from(c.h))
        .collect();
    assert_eq!(set.agent_hues, core);
    assert_eq!(set.reserved_hue_ranges["red"], (350.0, 50.0));
    assert_eq!(set.reserved_hue_ranges.len(), 3);
}

#[test]
fn load_dir_reads_themes_in_filename_order() {
    let dir = tempfile::tempdir().unwrap();
    copy_tokens(dir.path());
    let text = fs::read_to_string(dir.path().join("themes/smith-dark.json")).unwrap();
    let text = text
        .replace("\"smith-dark\"", "\"zzz\"")
        .replace("\"Smith Dark\"", "\"Zzz\"");
    fs::write(dir.path().join("themes/zzz.json"), text).unwrap();
    let set = ThemeSet::load_dir(dir.path()).unwrap();
    let ids: Vec<&str> = set.themes.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["smith-dark", "smith-light", "zzz"]);
}

#[test]
fn malformed_theme_is_parse_error_with_path() {
    let dir = tempfile::tempdir().unwrap();
    copy_tokens(dir.path());
    fs::write(dir.path().join("themes/bad.json"), "{ nope").unwrap();
    let err = ThemeSet::load_dir(dir.path()).unwrap_err();
    assert!(matches!(err, TokenError::Parse { .. }));
    assert!(err.to_string().contains("bad.json"), "{err}");
}

#[test]
fn unknown_field_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    copy_tokens(dir.path());
    let p = dir.path().join("themes/smith-dark.json");
    let text = fs::read_to_string(&p)
        .unwrap()
        .replace("\"agent\"", "\"agnet\"");
    fs::write(&p, text).unwrap();
    assert!(matches!(
        ThemeSet::load_dir(dir.path()),
        Err(TokenError::Parse { .. })
    ));
}

#[test]
fn missing_base_is_io_error() {
    let dir = tempfile::tempdir().unwrap();
    copy_tokens(dir.path());
    fs::remove_file(dir.path().join("base.json")).unwrap();
    assert!(matches!(
        ThemeSet::load_dir(dir.path()),
        Err(TokenError::Io { .. })
    ));
}

#[test]
fn embedded_equals_design_tokens_dir() {
    assert_eq!(
        ThemeSet::embedded(),
        &ThemeSet::load_dir(&tokens_dir()).unwrap()
    );
}

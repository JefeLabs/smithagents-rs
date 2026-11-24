//! Embeds every `design/tokens/themes/*.json` file into the crate.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let tokens = manifest.join("../../design/tokens");
    let themes = tokens.join("themes");
    println!("cargo:rerun-if-changed={}", tokens.display());
    println!("cargo:rerun-if-changed={}", themes.display());

    let mut files: Vec<PathBuf> = fs::read_dir(&themes)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", themes.display()))
        .map(|e| e.expect("directory entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort_by_key(|p| p.file_name().map(|n| n.to_owned()));

    let mut out = String::new();
    let lit = |p: &PathBuf| format!("{:?}", p.canonicalize().expect("canonical path"));
    for (name, path) in [
        ("BASE_JSON", tokens.join("base.json")),
        ("PALETTE_JSON", tokens.join("agent-palette.json")),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
        writeln!(
            out,
            "pub const {name}: &str = include_str!({});",
            lit(&path)
        )
        .unwrap();
    }
    out.push_str("pub const THEME_FILES: &[(&str, &str)] = &[\n");
    for p in &files {
        println!("cargo:rerun-if-changed={}", p.display());
        let name = p.file_name().unwrap().to_string_lossy();
        writeln!(out, "    ({name:?}, include_str!({})),", lit(p)).unwrap();
    }
    out.push_str("];\n");
    let dest = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("embedded_themes.rs");
    fs::write(dest, out).expect("write embedded_themes.rs");
}

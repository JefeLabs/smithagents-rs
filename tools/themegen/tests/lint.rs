use std::fs;
use std::path::Path;

use themegen::lint::{lint_repo, lint_text};

fn flagged(s: &str) -> bool {
    !lint_text(s).is_empty()
}

#[test]
fn flags_literal_colors() {
    for s in [
        "color: #fff;",
        "let c = \"#1A2B3C\";",
        "rgb(0xff0000)",
        "hsla(0.5, 1.0, 0.5, 1.0)",
        "oklch(0.7 0.1 200)",
        "rgb(12 34 56)",
    ] {
        assert!(flagged(s), "should flag: {s}");
    }
}

#[test]
fn reports_line_and_snippet() {
    let hits = lint_text("ok\ncolor: #abc;\n");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, 2);
}

#[test]
fn ignores_non_literals() {
    for s in [
        "rgb(var(--x))",
        "// #fff in a comment",
        "// see issue #123",
        "let r = r#\"raw\"#;",
        "&#123;",
        "url(#gradient)",
        "url(#abcdef0123456)",
        "format!(\"oklch({} {} {})\")",
        "let c = 1; // #fff theme-lint: allow",
        "/* #fff */",
        " * #fff",
        "<!-- #fff -->",
    ] {
        assert!(!flagged(s), "should not flag: {s}");
    }
}

fn write(root: &Path, rel: &str, body: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

#[test]
fn lint_repo_walks_and_skips() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let bad = b"a\nb\nconst x = '#ff0000';\n";
    write(root, "src/App.tsx", bad);
    for skipped in [
        "target/a.rs",
        "node_modules/a.js",
        ".worktrees/w/a.rs",
        "design/tokens/a.css",
        "design/generated/a.css",
        "x/tests/a.rs",
        "x/fixtures/a.rs",
        "ignored.css",
    ] {
        write(root, skipped, bad);
    }
    write(root, "src/readme.md", bad);
    write(root, "src/bin.rs", &[0xff, 0xfe, 0x00, 0x23, 0x66]);
    write(root, ".gitignore", b"ignored.css\n");

    let findings = lint_repo(root).unwrap();
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].path, root.join("src/App.tsx"));
    assert_eq!(findings[0].line, 3);
}

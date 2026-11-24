//! Literal-color lint: flags hard-coded colors in source files so that all
//! color flows through the design tokens.
//!
//! Flagged: hex color literals (3, 4, 6 or 8 digits after a hash) and color
//! function calls (rgb, rgba, hsl, hsla, hwb, lab, lch, oklab, oklch, color)
//! whose first argument is a number. Calls that start with a variable or a
//! format placeholder are fine.
//!
//! Known limits: named colors (such as a plain color keyword) and bare
//! numeric `0xRRGGBB` literals are not detected.
//!
//! A line is skipped when it is comment-only or contains the allow marker
//! (`theme-lint: allow`).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use ignore::WalkBuilder;
use regex::Regex;

const EXTENSIONS: &[&str] = &[
    "rs", "css", "scss", "js", "mjs", "cjs", "ts", "tsx", "jsx", "html", "svg",
];
const SKIP_DIR_NAMES: &[&str] = &["target", "node_modules", "tests", "fixtures"];
const SKIP_REL_DIRS: &[&str] = &["design/tokens", "design/generated"];
const ALLOW_MARKER: &str = "theme-lint: allow";

static HEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(^|[^\w&#])#([0-9a-fA-F]{3,4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})\b").unwrap()
});
static FUNC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(rgb|rgba|hsl|hsla|hwb|lab|lch|oklab|oklch|color)\(\s*[-+.\d]").unwrap()
});

/// One offending line in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
}

/// Returns `(1-based line, offending snippet)` for each literal color in `text`.
pub fn lint_text(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim_start();
        if ["//", "/*", "*", "<!--"].iter().any(|p| t.starts_with(p)) || line.contains(ALLOW_MARKER)
        {
            continue;
        }
        let hit = HEX
            .find(line)
            .map(|m| m.as_str().trim_start().to_string())
            .or_else(|| FUNC.find(line).map(|m| m.as_str().to_string()));
        if let Some(snippet) = hit {
            out.push((i + 1, snippet));
        }
    }
    out
}

/// Walks `root` (honoring `.gitignore`, skipping hidden and build dirs) and
/// lints every scanned source file. Non-UTF-8 files are skipped.
pub fn lint_repo(root: &Path) -> io::Result<Vec<Finding>> {
    let walk_root = root.to_path_buf();
    let walker = WalkBuilder::new(root)
        .require_git(false)
        .filter_entry(move |e| {
            if !e.file_type().is_some_and(|t| t.is_dir()) {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            if SKIP_DIR_NAMES.iter().any(|n| *n == name) {
                return false;
            }
            match e.path().strip_prefix(&walk_root) {
                Ok(rel) => !SKIP_REL_DIRS.iter().any(|d| rel == Path::new(d)),
                Err(_) => true,
            }
        })
        .build();

    let mut findings = Vec::new();
    for entry in walker {
        let entry = entry.map_err(io::Error::other)?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let scanned = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXTENSIONS.contains(&e));
        if !scanned {
            continue;
        }
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            // Invalid UTF-8: not a text source file, skip silently.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => continue,
            Err(e) => return Err(io::Error::new(e.kind(), format!("{}: {e}", path.display()))),
        };
        for (line, text) in lint_text(&text) {
            findings.push(Finding {
                path: path.to_path_buf(),
                line,
                text,
            });
        }
    }
    findings.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    Ok(findings)
}

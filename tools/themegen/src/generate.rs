//! Generation and drift checking of the committed theme files.

use std::fs;
use std::path::{Path, PathBuf};

use smithagents_theme::tokens::{ThemeSet, TokenError};
use smithagents_theme::validate::{validate, ValidationError};

use crate::bootstrap::render_bootstrap;
use crate::css::render_css;

const CSS_NAME: &str = "theme.css";
const BOOTSTRAP_NAME: &str = "theme-bootstrap.js";

#[derive(Debug, thiserror::Error)]
pub enum GenError {
    #[error(transparent)]
    Tokens(#[from] TokenError),
    #[error("invalid tokens:\n{}", .0.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n"))]
    Invalid(Vec<ValidationError>),
    #[error("io error at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("generated file out of date: {}", path.display())]
    Drift { path: PathBuf },
}

fn render(tokens_dir: &Path) -> Result<[(&'static str, String); 2], GenError> {
    let set = ThemeSet::load_dir(tokens_dir)?;
    validate(&set).map_err(GenError::Invalid)?;
    Ok([
        (CSS_NAME, render_css(&set)),
        (BOOTSTRAP_NAME, render_bootstrap(&set)),
    ])
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> GenError + '_ {
    move |source| GenError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Validates the tokens and writes the generated files into `out_dir`.
/// Nothing is written when validation fails; files are written to temporary
/// names and renamed into place.
pub fn generate(tokens_dir: &Path, out_dir: &Path) -> Result<Vec<PathBuf>, GenError> {
    let files = render(tokens_dir)?;
    fs::create_dir_all(out_dir).map_err(io_err(out_dir))?;
    let mut staged = Vec::new();
    for (name, content) in &files {
        let tmp = out_dir.join(format!(".{name}.tmp"));
        if let Err(e) = fs::write(&tmp, content) {
            for (t, _) in &staged {
                let _ = fs::remove_file(t);
            }
            return Err(io_err(&tmp)(e));
        }
        staged.push((tmp, out_dir.join(name)));
    }
    let mut written = Vec::new();
    for (tmp, dest) in staged {
        fs::rename(&tmp, &dest).map_err(io_err(&dest))?;
        written.push(dest);
    }
    Ok(written)
}

/// Verifies that the files in `out_dir` match what `generate` would write.
pub fn check(tokens_dir: &Path, out_dir: &Path) -> Result<(), GenError> {
    for (name, content) in render(tokens_dir)? {
        let path = out_dir.join(name);
        match fs::read(&path) {
            Ok(existing) if existing == content.as_bytes() => {}
            _ => return Err(GenError::Drift { path }),
        }
    }
    Ok(())
}

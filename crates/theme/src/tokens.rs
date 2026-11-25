//! Design token files and their loader.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::color::Oklch;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded_themes.rs"));
}

/// Color tokens every theme must define, in canonical order.
pub const COLOR_TOKENS: &[&str] = &[
    "background",
    "foreground",
    "card",
    "card-foreground",
    "card-2",
    "card-2-foreground",
    "center-pane",
    "center-pane-foreground",
    "popover",
    "popover-foreground",
    "primary",
    "primary-foreground",
    "secondary",
    "secondary-foreground",
    "muted",
    "muted-foreground",
    "accent",
    "accent-foreground",
    "destructive",
    "border",
    "input",
    "ring",
    "selection",
    "status-ok",
    "status-warn",
    "status-error",
    "status-info",
];

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("cannot read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse {}: {source}", path.display())]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Dark,
    Light,
}

/// Lightness and chroma applied to agent hues.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPresentation {
    pub l: f64,
    pub c: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeFile {
    pub id: String,
    pub name: String,
    pub appearance: Appearance,
    pub colors: BTreeMap<String, Oklch>,
    pub agent: AgentPresentation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemPair {
    pub dark: String,
    pub light: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontToken {
    pub id: String,
    pub label: String,
    /// CSS font stack.
    pub sans: String,
    /// CSS font stack.
    pub mono: String,
    /// Family name for GPUI (`.SystemUIFont` for the system font, to be verified).
    pub native_sans: String,
    pub native_mono: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextScale {
    pub min: f64,
    pub max: f64,
    pub default: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseTokens {
    pub system_pair: SystemPair,
    pub radius_px: f64,
    pub spacing_rem: f64,
    pub text_scale: TextScale,
    pub text_sizes_rem: BTreeMap<String, f64>,
    pub fonts: Vec<FontToken>,
}

/// Only the parts of `agent-palette.json` this crate needs; the file is shared
/// with `smithagents-core`, so unknown fields are allowed.
#[derive(Deserialize)]
struct PaletteFile {
    reserved_hue_ranges: BTreeMap<String, (f64, f64)>,
    colors: Vec<PaletteColor>,
}

#[derive(Deserialize)]
struct PaletteColor {
    h: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThemeSet {
    pub base: BaseTokens,
    pub themes: Vec<ThemeFile>,
    pub agent_hues: Vec<f64>,
    pub reserved_hue_ranges: BTreeMap<String, (f64, f64)>,
}

fn parse<T: serde::de::DeserializeOwned>(path: &Path, text: &str) -> Result<T, TokenError> {
    serde_json::from_str(text).map_err(|source| TokenError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn read(path: &Path) -> Result<String, TokenError> {
    std::fs::read_to_string(path).map_err(|source| TokenError::Io {
        path: path.to_path_buf(),
        source,
    })
}

impl ThemeSet {
    fn assemble(
        base: (&Path, &str),
        palette: (&Path, &str),
        themes: Vec<(PathBuf, String)>,
    ) -> Result<ThemeSet, TokenError> {
        let base = parse(base.0, base.1)?;
        let palette: PaletteFile = parse(palette.0, palette.1)?;
        let themes = themes
            .iter()
            .map(|(p, t)| parse(p, t))
            .collect::<Result<Vec<ThemeFile>, _>>()?;
        Ok(ThemeSet {
            base,
            themes,
            agent_hues: palette.colors.iter().map(|c| c.h).collect(),
            reserved_hue_ranges: palette.reserved_hue_ranges,
        })
    }

    /// Loads `base.json`, `agent-palette.json` and `themes/*.json` (sorted by
    /// filename) from `tokens_dir`.
    pub fn load_dir(tokens_dir: &Path) -> Result<ThemeSet, TokenError> {
        let base_path = tokens_dir.join("base.json");
        let palette_path = tokens_dir.join("agent-palette.json");
        let base = read(&base_path)?;
        let palette = read(&palette_path)?;
        let themes_dir = tokens_dir.join("themes");
        let io = |source| TokenError::Io {
            path: themes_dir.clone(),
            source,
        };
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(&themes_dir).map_err(io)? {
            let path = entry.map_err(io)?.path();
            if path.extension().is_some_and(|e| e == "json") {
                paths.push(path);
            }
        }
        paths.sort_by_key(|p| p.file_name().map(|n| n.to_owned()));
        let themes = paths
            .into_iter()
            .map(|p| read(&p).map(|t| (p, t)))
            .collect::<Result<Vec<_>, _>>()?;
        Self::assemble((&base_path, &base), (&palette_path, &palette), themes)
    }

    /// The token set embedded at build time. Panics, naming the file, if a
    /// shipped file is malformed.
    pub fn embedded() -> &'static ThemeSet {
        static SET: OnceLock<ThemeSet> = OnceLock::new();
        SET.get_or_init(|| {
            let themes = embedded::THEME_FILES
                .iter()
                .map(|(name, text)| (PathBuf::from(format!("themes/{name}")), (*text).to_owned()))
                .collect();
            Self::assemble(
                (Path::new("base.json"), embedded::BASE_JSON),
                (Path::new("agent-palette.json"), embedded::PALETTE_JSON),
                themes,
            )
            .unwrap_or_else(|e| panic!("embedded design tokens are malformed: {e}"))
        })
    }

    pub fn theme(&self, id: &str) -> Option<&ThemeFile> {
        self.themes.iter().find(|t| t.id == id)
    }
}

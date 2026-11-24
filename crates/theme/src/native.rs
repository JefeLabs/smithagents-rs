//! Native (sRGB) view of a theme, for the GPUI shell.

use std::collections::BTreeMap;

use crate::color::{Oklch, Srgb};
use crate::tokens::{Appearance, ThemeSet};

#[derive(Debug, Clone, PartialEq)]
pub struct NativeTheme {
    pub id: String,
    pub name: String,
    pub appearance: Appearance,
    pub colors: BTreeMap<String, Srgb>,
    pub radius_px: f64,
    pub agent_l: f64,
    pub agent_c: f64,
}

impl NativeTheme {
    /// The agent color for `hue` (degrees) at this theme's lightness/chroma.
    /// Falls back to a neutral gray of the same lightness if the combination
    /// is outside the sRGB gamut (a validated set never is).
    pub fn agent_color(&self, hue: f64) -> Srgb {
        Oklch {
            l: self.agent_l,
            c: self.agent_c,
            h: hue,
        }
        .to_srgb()
        .unwrap_or_else(|_| {
            Oklch {
                l: self.agent_l,
                c: 0.0,
                h: 0.0,
            }
            .to_srgb()
            .expect("achromatic colors are in gamut")
        })
    }
}

impl ThemeSet {
    /// Converts theme `id` to sRGB. Requires a validated set; panics if a
    /// token is out of gamut. Returns `None` for an unknown id.
    pub fn native(&self, id: &str) -> Option<NativeTheme> {
        let theme = self.theme(id)?;
        let colors = theme
            .colors
            .iter()
            .map(|(name, c)| {
                let rgb = c
                    .to_srgb()
                    .unwrap_or_else(|e| panic!("theme {id} token {name}: {e}"));
                (name.clone(), rgb)
            })
            .collect();
        Some(NativeTheme {
            id: theme.id.clone(),
            name: theme.name.clone(),
            appearance: theme.appearance,
            colors,
            radius_px: self.base.radius_px,
            agent_l: theme.agent.l,
            agent_c: theme.agent.c,
        })
    }
}

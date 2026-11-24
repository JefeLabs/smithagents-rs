//! Agent color palette, loaded from the shared design tokens.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const TOKENS_JSON: &str = include_str!("../../../design/tokens/agent-palette.json");

/// An OKLCH color.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AgentColor {
    pub l: f32,
    pub c: f32,
    pub h: f32,
}

#[derive(Deserialize)]
struct Tokens {
    reserved_hue_ranges: BTreeMap<String, (f32, f32)>,
    colors: Vec<AgentColor>,
}

struct Loaded {
    colors: Vec<AgentColor>,
    reserved: Vec<(f32, f32)>,
}

fn tokens() -> &'static Loaded {
    static TOKENS: OnceLock<Loaded> = OnceLock::new();
    TOKENS.get_or_init(|| {
        let t: Tokens =
            serde_json::from_str(TOKENS_JSON).expect("design/tokens/agent-palette.json is valid");
        Loaded {
            colors: t.colors,
            reserved: t.reserved_hue_ranges.into_values().collect(),
        }
    })
}

/// The agent color palette, in assignment order.
pub fn palette() -> &'static [AgentColor] {
    &tokens().colors
}

/// Hue ranges (degrees) reserved for status colors. A range with
/// `start > end` wraps through 0.
pub fn reserved_hue_ranges() -> &'static [(f32, f32)] {
    &tokens().reserved
}

/// First palette color not in `open`; when exhausted, cycles by `open.len()`.
pub fn next_color(open: &[AgentColor]) -> AgentColor {
    let p = palette();
    p.iter()
        .find(|c| !open.contains(c))
        .copied()
        .unwrap_or_else(|| p[open.len() % p.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_range(h: f32, (start, end): (f32, f32)) -> bool {
        if start > end {
            h >= start || h <= end
        } else {
            h >= start && h <= end
        }
    }

    #[test]
    fn palette_hues_avoid_reserved_ranges() {
        assert!(!reserved_hue_ranges().is_empty());
        for color in palette() {
            for range in reserved_hue_ranges() {
                assert!(!in_range(color.h, *range), "hue {} in {:?}", color.h, range);
            }
        }
    }

    #[test]
    fn palette_has_equal_lightness_and_chroma() {
        let p = palette();
        assert_eq!(p.len(), 6);
        assert!(p.iter().all(|c| c.l == p[0].l && c.c == p[0].c));
    }

    #[test]
    fn next_color_first_unused() {
        let p = palette();
        assert_eq!(next_color(&[p[0]]), p[1]);
        assert_eq!(next_color(&[]), p[0]);
    }

    #[test]
    fn next_color_wraps_when_exhausted() {
        let p = palette();
        assert_eq!(next_color(p), p[0]);
        let mut seven = p.to_vec();
        seven.push(p[0]);
        assert_eq!(next_color(&seven), p[1]);
    }
}

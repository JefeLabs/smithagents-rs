//! OKLCH to sRGB conversion and WCAG 2.x contrast.

use serde::{Deserialize, Serialize};

/// A color in OKLCH. `h` is in degrees.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Oklch {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

/// An 8-bit sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Srgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// The OKLCH color cannot be represented in sRGB.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[error("oklch color {} is outside the sRGB gamut", .color.css())]
pub struct OutOfGamut {
    pub color: Oklch,
}

const GAMUT_EPSILON: f64 = 0.001;

fn gamma_encode(x: f64) -> f64 {
    if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

fn gamma_decode(x: f64) -> f64 {
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

fn quantize(x: f64) -> u8 {
    (gamma_encode(x.clamp(0.0, 1.0)) * 255.0).round() as u8
}

impl Oklch {
    /// Convert to sRGB through OKLab and linear sRGB.
    pub fn to_srgb(self) -> Result<Srgb, OutOfGamut> {
        let (sin, cos) = self.h.to_radians().sin_cos();
        let a = self.c * cos;
        let b = self.c * sin;

        let l_ = self.l + 0.396_337_777_4 * a + 0.215_803_757_3 * b;
        let m_ = self.l - 0.105_561_345_8 * a - 0.063_854_172_8 * b;
        let s_ = self.l - 0.089_484_177_5 * a - 1.291_485_548_0 * b;
        let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);

        let r = 4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s;
        let g = -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s;
        let bl = -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s;

        let range = -GAMUT_EPSILON..=1.0 + GAMUT_EPSILON;
        if ![r, g, bl].iter().all(|v| range.contains(v)) {
            return Err(OutOfGamut { color: self });
        }
        Ok(Srgb {
            r: quantize(r),
            g: quantize(g),
            b: quantize(bl),
        })
    }

    /// CSS `oklch(L C H)` with at most 4 decimals per number.
    pub fn css(self) -> String {
        format!(
            "oklch({} {} {})",
            fmt_num(self.l),
            fmt_num(self.c),
            fmt_num(self.h)
        )
    }
}

/// Number formatting shared with `css()`: at most 4 decimals, trailing zeros trimmed.
pub fn fmt_num(x: f64) -> String {
    let s = format!("{x:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        "0".to_string()
    } else {
        s.to_string()
    }
}

impl Srgb {
    /// Lowercase `#rrggbb`.
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// WCAG 2.x relative luminance.
    pub fn relative_luminance(self) -> f64 {
        let ch = |v: u8| gamma_decode(f64::from(v) / 255.0);
        0.2126 * ch(self.r) + 0.7152 * ch(self.g) + 0.0722 * ch(self.b)
    }
}

/// WCAG 2.x contrast ratio, symmetric, from 1.0 to 21.0.
pub fn contrast_ratio(a: Srgb, b: Srgb) -> f64 {
    let (la, lb) = (a.relative_luminance(), b.relative_luminance());
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

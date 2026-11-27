//! Boxes drawn over a screenshot, kept as fractions of the image so they do
//! not depend on the size the image is shown at.

use serde::{Deserialize, Serialize};

use super::text::{MAX_BOXES, MAX_COMMENT_CHARS, MIN_BOX_POINTS};

/// A rectangle as fractions of the image, each 0..=1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FracRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl FracRect {
    /// The box between any two corners, clamped into the image.
    pub fn from_corners(a: (f32, f32), b: (f32, f32)) -> FracRect {
        let (ax, ay) = (a.0.clamp(0.0, 1.0), a.1.clamp(0.0, 1.0));
        let (bx, by) = (b.0.clamp(0.0, 1.0), b.1.clamp(0.0, 1.0));
        FracRect {
            x: ax.min(bx),
            y: ay.min(by),
            w: (ax - bx).abs(),
            h: (ay - by).abs(),
        }
    }

    /// The box kept inside the image; `None` when a field is not finite or
    /// the width or height is nothing once clamped.
    pub fn sanitized(self) -> Option<FracRect> {
        if ![self.x, self.y, self.w, self.h]
            .iter()
            .all(|v| v.is_finite())
        {
            return None;
        }
        let x = self.x.clamp(0.0, 1.0);
        let y = self.y.clamp(0.0, 1.0);
        let w = self.w.min(1.0 - x);
        let h = self.h.min(1.0 - y);
        (w > 0.0 && h > 0.0).then_some(FracRect { x, y, w, h })
    }
}

/// One box and the comment typed for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    #[serde(flatten)]
    pub rect: FracRect,
    pub comment: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxError {
    TooMany,
    CommentTooLong,
}

/// Whether a dragged box is at least [`MIN_BOX_POINTS`] each way.
pub fn big_enough(width_points: f32, height_points: f32) -> bool {
    width_points >= MIN_BOX_POINTS && height_points >= MIN_BOX_POINTS
}

/// Trimmed text; `Ok(None)` when nothing is left; `Err(CommentTooLong)` above
/// 500 characters (chars, not bytes).
pub fn clean_comment(text: &str) -> Result<Option<String>, BoxError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    // `nth` stops at the limit, so a huge paste is not counted to the end.
    if text.chars().nth(MAX_COMMENT_CHARS).is_some() {
        return Err(BoxError::CommentTooLong);
    }
    Ok(Some(text.to_string()))
}

/// Appends a box and returns its number (its 1-based position); `Err(TooMany)` at 20.
pub fn add_box(
    boxes: &mut Vec<Annotation>,
    rect: FracRect,
    comment: String,
) -> Result<usize, BoxError> {
    if boxes.len() >= MAX_BOXES {
        return Err(BoxError::TooMany);
    }
    boxes.push(Annotation { rect, comment });
    Ok(boxes.len())
}

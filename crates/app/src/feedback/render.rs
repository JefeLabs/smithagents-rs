//! Drawing the annotated PNG, thumbnails, and decoding pictures. No fonts:
//! badge numbers use a 3x5 bitmap per digit.

use std::io::Cursor;

use image::imageops::{self, FilterType};
use image::{ImageError, ImageFormat, ImageReader, Limits, Rgba, RgbaImage};
use smithagents_capture::Screenshot;

use super::annotation::Annotation;
use super::text::THUMBNAIL_MAX_WIDTH;

// A PNG cannot follow the theme, so the box color is fixed: a red near the
// light theme's `destructive`, with white for the badge number.
pub const BOX_RGB: [u8; 3] = [213, 57, 57];
pub const BADGE_TEXT_RGB: [u8; 3] = [255, 255, 255];

/// The largest a picture may decode to (width, height and memory), so a small
/// file cannot ask for gigabytes.
const MAX_DECODE_SIDE: u32 = 16_384;
const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// One drawing unit: 1 for images up to 1799 pixels wide, growing with width.
pub fn stroke_unit(image_width: u32) -> u32 {
    let unit = (u64::from(image_width) + 600) / 1200;
    u32::try_from(unit.max(1)).unwrap_or(u32::MAX)
}

/// Rows of each digit's 3x5 bitmap, the left cell in the highest of 3 bits.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// Fills the rectangle at (x, y) of size (w, h), clipped to the image.
fn fill(image: &mut RgbaImage, x: i64, y: i64, w: i64, h: i64, rgb: [u8; 3]) {
    let x0 = x.clamp(0, i64::from(image.width()));
    let x1 = x.saturating_add(w).clamp(0, i64::from(image.width()));
    let y0 = y.clamp(0, i64::from(image.height()));
    let y1 = y.saturating_add(h).clamp(0, i64::from(image.height()));
    let pixel = Rgba([rgb[0], rgb[1], rgb[2], 255]);
    for py in y0..y1 {
        for px in x0..x1 {
            image.put_pixel(px as u32, py as u32, pixel);
        }
    }
}

/// An outline `thickness` pixels wide, inside the rectangle.
fn outline(image: &mut RgbaImage, x: i64, y: i64, w: i64, h: i64, thickness: i64) {
    let t = thickness.min(w / 2 + w % 2).min(h / 2 + h % 2);
    fill(image, x, y, w, t, BOX_RGB);
    fill(image, x, y + h - t, w, t, BOX_RGB);
    fill(image, x, y, t, h, BOX_RGB);
    fill(image, x + w - t, y, t, h, BOX_RGB);
}

/// A filled badge at (x, y) showing `number`; cells are `cell` pixels.
fn badge(image: &mut RgbaImage, x: i64, y: i64, number: usize, cell: i64) {
    let digits: Vec<usize> = number
        .to_string()
        .bytes()
        .map(|b| usize::from(b - b'0'))
        .collect();
    let count = digits.len() as i64;
    // Two cells of padding each side; three cells per digit; one between digits.
    fill(
        image,
        x,
        y,
        (count * 3 + (count - 1) + 4) * cell,
        (5 + 4) * cell,
        BOX_RGB,
    );
    for (i, digit) in digits.into_iter().enumerate() {
        let left = x + (2 + i as i64 * 4) * cell;
        for (row, bits) in DIGITS[digit].iter().enumerate() {
            for col in 0..3 {
                if bits & (0b100 >> col) != 0 {
                    fill(
                        image,
                        left + col * cell,
                        y + (2 + row as i64) * cell,
                        cell,
                        cell,
                        BADGE_TEXT_RGB,
                    );
                }
            }
        }
    }
}

/// A copy of `original` with each box outlined and numbered by its position.
pub fn annotate(original: &RgbaImage, boxes: &[Annotation]) -> RgbaImage {
    let mut out = original.clone();
    let unit = i64::from(stroke_unit(out.width()));
    let (width, height) = (f64::from(out.width()), f64::from(out.height()));
    for (index, annotation) in boxes.iter().enumerate() {
        let r = annotation.rect;
        // Float casts saturate and turn NaN into 0, and every edge is clipped
        // to the image, so bad numbers cannot overflow later sums.
        let edge = |fraction: f32, size: f64| {
            ((f64::from(fraction) * size).round() as i64).clamp(0, size as i64)
        };
        let (left, top) = (edge(r.x, width), edge(r.y, height));
        let (right, bottom) = (edge(r.x + r.w, width), edge(r.y + r.h, height));
        let (w, h) = ((right - left).max(0), (bottom - top).max(0));
        outline(&mut out, left, top, w, h, 3 * unit);
        badge(&mut out, left, top, index + 1, 3 * unit);
    }
    out
}

/// At most 320 pixels wide, never enlarged.
pub fn thumbnail(image: &RgbaImage) -> RgbaImage {
    let (width, height) = image.dimensions();
    if width <= THUMBNAIL_MAX_WIDTH {
        return image.clone();
    }
    let target = u64::from(THUMBNAIL_MAX_WIDTH);
    let scaled = (u64::from(height) * target * 2 + u64::from(width)) / (2 * u64::from(width));
    imageops::resize(
        image,
        THUMBNAIL_MAX_WIDTH,
        scaled.max(1) as u32,
        FilterType::Triangle,
    )
}

pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, ImageError> {
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Png)?;
    Ok(bytes.into_inner())
}

/// Decodes png, jpeg, gif (first frame) or webp, refusing a picture whose
/// size is beyond [`MAX_DECODE_SIDE`] or [`MAX_DECODE_ALLOC`] before decoding it.
pub fn decode(bytes: &[u8]) -> Result<RgbaImage, ImageError> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODE_SIDE);
    limits.max_image_height = Some(MAX_DECODE_SIDE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    Ok(reader.decode()?.into_rgba8())
}

/// `None` when the buffer's length is not exactly width x height x 4.
pub fn image_from_screenshot(shot: Screenshot) -> Option<RgbaImage> {
    let expected = (shot.width as usize)
        .checked_mul(shot.height as usize)?
        .checked_mul(4)?;
    if shot.rgba.len() != expected {
        return None;
    }
    RgbaImage::from_raw(shot.width, shot.height, shot.rgba)
}

use smithagents_app::feedback::annotation::{
    add_box, big_enough, clean_comment, Annotation, BoxError, FracRect,
};
use smithagents_app::feedback::files::{admit, kind_from_name, sniff, FileKind, Refusal, Usage};
use smithagents_app::feedback::render::{
    annotate, decode, encode_png, image_from_screenshot, stroke_unit, thumbnail, BADGE_TEXT_RGB,
    BOX_RGB,
};
use smithagents_app::feedback::text::*;
use smithagents_capture::{test_pattern, Screenshot};

fn rect(x: f32, y: f32, w: f32, h: f32) -> FracRect {
    FracRect { x, y, w, h }
}

fn note(rect: FracRect) -> Annotation {
    Annotation {
        rect,
        comment: String::new(),
    }
}

fn close(a: FracRect, b: FracRect) -> bool {
    (a.x - b.x).abs() < 1e-6
        && (a.y - b.y).abs() < 1e-6
        && (a.w - b.w).abs() < 1e-6
        && (a.h - b.h).abs() < 1e-6
}

fn white(width: u32, height: u32) -> image::RgbaImage {
    image::RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 255]))
}

fn px(img: &image::RgbaImage, x: u32, y: u32) -> [u8; 3] {
    let p = img.get_pixel(x, y).0;
    [p[0], p[1], p[2]]
}

const WHITE: [u8; 3] = [255, 255, 255];

#[test]
fn corners_in_any_order_give_the_same_clamped_box() {
    let want = rect(0.25, 0.25, 0.25, 0.25);
    assert_eq!(FracRect::from_corners((0.5, 0.5), (0.25, 0.25)), want);
    assert_eq!(FracRect::from_corners((0.25, 0.25), (0.5, 0.5)), want);
    let clamped = FracRect::from_corners((-0.2, 0.1), (1.4, 0.3));
    assert!(close(clamped, rect(0.0, 0.1, 1.0, 0.2)), "{clamped:?}");
}

#[test]
fn a_box_with_bad_numbers_is_dropped_and_an_overhanging_one_is_clamped() {
    assert_eq!(rect(f32::NAN, 0.1, 0.1, 0.1).sanitized(), None);
    assert_eq!(rect(0.1, 0.1, f32::INFINITY, 0.1).sanitized(), None);
    let got = rect(0.9, 0.9, 0.5, 0.5).sanitized().unwrap();
    assert!(close(got, rect(0.9, 0.9, 0.1, 0.1)), "{got:?}");
    assert_eq!(rect(0.2, 0.2, 0.0, 0.3).sanitized(), None);
    assert_eq!(rect(0.2, 0.2, -0.3, 0.3).sanitized(), None);
    assert_eq!(rect(1.5, 0.2, 0.3, 0.3).sanitized(), None);
}

#[test]
fn a_box_must_be_eight_points_each_way() {
    assert!(big_enough(8.0, 8.0));
    assert!(!big_enough(7.9, 40.0));
    assert!(!big_enough(40.0, 7.9));
    assert!(!big_enough(f32::NAN, 40.0));
}

#[test]
fn comments_are_trimmed_and_capped_at_500_characters() {
    assert_eq!(clean_comment("  hi \n"), Ok(Some("hi".to_string())));
    assert_eq!(clean_comment("   "), Ok(None));
    assert!(matches!(clean_comment(&"é".repeat(500)), Ok(Some(_))));
    assert_eq!(
        clean_comment(&"é".repeat(501)),
        Err(BoxError::CommentTooLong)
    );
}

#[test]
fn the_twenty_first_box_is_refused_and_numbers_follow_position() {
    let mut boxes = Vec::new();
    for want in 1..=20 {
        assert_eq!(
            add_box(&mut boxes, rect(0.1, 0.1, 0.1, 0.1), String::new()),
            Ok(want)
        );
    }
    assert_eq!(
        add_box(&mut boxes, rect(0.1, 0.1, 0.1, 0.1), String::new()),
        Err(BoxError::TooMany)
    );
    assert_eq!(boxes.len(), 20);
    boxes.remove(0);
    assert_eq!(
        add_box(&mut boxes, rect(0.1, 0.1, 0.1, 0.1), String::new()),
        Ok(20)
    );
}

#[test]
fn stroke_unit_grows_with_the_image() {
    for (width, want) in [
        (100, 1),
        (1200, 1),
        (1799, 1),
        (1800, 2),
        (2400, 2),
        (5120, 4),
    ] {
        assert_eq!(stroke_unit(width), want, "width {width}");
    }
    assert!(stroke_unit(u32::MAX) > 1);
}

#[test]
fn annotate_draws_a_three_pixel_outline_and_a_numbered_badge() {
    let original = white(1200, 800);
    let out = annotate(&original, &[note(rect(0.25, 0.25, 0.5, 0.5))]);
    for (x, y) in [(300, 400), (302, 400), (899, 400), (600, 200), (600, 599)] {
        assert_eq!(px(&out, x, y), BOX_RGB, "({x},{y})");
    }
    for (x, y) in [(303, 400), (600, 400), (900, 400), (299, 400)] {
        assert_eq!(px(&out, x, y), WHITE, "({x},{y})");
    }
    assert_eq!(px(&out, 320, 226), BOX_RGB);
    assert_eq!(px(&out, 321, 240), WHITE);
    assert_eq!(px(&out, 310, 207), BADGE_TEXT_RGB);
    assert_eq!(px(&out, 307, 207), BOX_RGB);
    assert_eq!(original, white(1200, 800));
}

#[test]
fn the_outline_scales_with_the_image() {
    let out = annotate(&white(2400, 1600), &[note(rect(0.25, 0.25, 0.5, 0.5))]);
    assert_eq!(px(&out, 605, 1000), BOX_RGB);
    assert_eq!(px(&out, 606, 1000), WHITE);
}

#[test]
fn box_twelve_has_a_two_digit_badge_and_edges_clip() {
    let mut boxes: Vec<Annotation> = (0..11)
        .map(|i| note(rect(0.3 + 0.05 * i as f32 / 2.0, 0.0, 0.02, 0.02)))
        .collect();
    boxes.push(note(rect(0.0, 0.5, 0.2, 0.2)));
    let out = annotate(&white(1200, 800), &boxes);
    assert_eq!(px(&out, 32, 426), BOX_RGB);

    let edge = [note(rect(0.95, 0.95, 0.05, 0.05))];
    annotate(&white(1200, 800), &edge);
    let many: Vec<Annotation> = (0..20).map(|_| note(rect(0.0, 0.0, 1.0, 1.0))).collect();
    annotate(&white(10, 10), &many);
}

#[test]
fn hostile_boxes_and_sizes_do_not_panic() {
    let hostile = [
        note(rect(f32::NAN, f32::INFINITY, -5.0, 1e30)),
        note(rect(-3.0, -3.0, 10.0, 10.0)),
        note(rect(0.5, 0.5, 0.0, 0.0)),
    ];
    annotate(&white(50, 40), &hostile);
    annotate(&image::RgbaImage::new(0, 0), &hostile);
    annotate(&white(1, 1), &hostile);
}

#[test]
fn thumbnails_are_at_most_320_wide_and_never_enlarged() {
    for ((w, h), want) in [
        ((2400, 1520), (320, 203)),
        ((2400, 1512), (320, 202)),
        ((200, 100), (200, 100)),
        ((5000, 3), (320, 1)),
    ] {
        let t = thumbnail(&white(w, h));
        assert_eq!(t.dimensions(), want, "{w}x{h}");
    }
}

#[test]
fn png_round_trip_keeps_the_pixels() {
    let annotated = annotate(&white(200, 120), &[note(rect(0.1, 0.1, 0.5, 0.5))]);
    let bytes = encode_png(&annotated).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(decode(&bytes).unwrap(), annotated);
}

#[test]
fn decoding_refuses_garbage_and_truncated_files_without_panicking() {
    assert!(decode(b"").is_err());
    assert!(decode(b"hello world, not an image").is_err());
    let bytes = encode_png(&white(64, 64)).unwrap();
    assert!(decode(&bytes[..bytes.len() / 2]).is_err());
}

/// CRC-32 (IEEE) of `bytes`, as a PNG chunk needs.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
    out
}

/// A PNG that only claims a size: a valid header (8 bits per sample, of
/// `color_type`), a few bytes of image data and the end.
fn png_claiming(width: u32, height: u32, color_type: u8) -> Vec<u8> {
    let mut header = width.to_be_bytes().to_vec();
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, color_type, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(chunk(b"IHDR", &header));
    png.extend(chunk(
        b"IDAT",
        &[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
    ));
    png.extend(chunk(b"IEND", &[]));
    png
}

#[test]
fn a_picture_too_big_to_draw_is_refused_before_it_is_decoded() {
    const GRAY: u8 = 0;
    const RGBA: u8 = 6;
    // 8-bit gray at the largest side: 256 MiB as gray, but 1 GiB once it is
    // RGBA for drawing.
    let gray = png_claiming(16_384, 16_384, GRAY);
    assert!(
        matches!(decode(&gray), Err(image::ImageError::Limits(_))),
        "{:?}",
        decode(&gray).map(|image| image.dimensions())
    );
    // 10000 x 10000 gray: within the side limit, 400 MB as RGBA.
    let wide = png_claiming(10_000, 10_000, GRAY);
    assert!(matches!(decode(&wide), Err(image::ImageError::Limits(_))));
    // Far beyond any side limit.
    let huge = png_claiming(100_000, 100_000, RGBA);
    assert!(matches!(decode(&huge), Err(image::ImageError::Limits(_))));
    // A real picture of a real size still decodes.
    let bytes = encode_png(&white(64, 48)).unwrap();
    assert_eq!(decode(&bytes).unwrap().dimensions(), (64, 48));
}

#[test]
fn a_screenshot_with_a_short_buffer_is_none() {
    let short = Screenshot {
        width: 4,
        height: 4,
        rgba: vec![0; 60],
    };
    assert!(image_from_screenshot(short).is_none());
    let long = Screenshot {
        width: 4,
        height: 4,
        rgba: vec![0; 68],
    };
    assert!(image_from_screenshot(long).is_none());
    let ok = image_from_screenshot(test_pattern(8, 4)).unwrap();
    assert_eq!(ok.dimensions(), (8, 4));
}

#[test]
fn sniffing_reads_the_leading_bytes() {
    assert_eq!(sniff(b"\x89PNG\r\n\x1a\n\0\0\0\0"), Some(FileKind::Png));
    assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(FileKind::Jpeg));
    assert_eq!(sniff(b"GIF87a\0\0"), Some(FileKind::Gif));
    assert_eq!(sniff(b"GIF89a\0\0"), Some(FileKind::Gif));
    assert_eq!(sniff(b"RIFF\x01\x02\x03\x04WEBP"), Some(FileKind::Webp));
    assert_eq!(sniff(b"\0\0\0\x18ftypmp42"), Some(FileKind::Mp4));
    assert_eq!(sniff(b"abcdftypisom"), Some(FileKind::Mp4));
    assert_eq!(sniff(b"hello world!"), None);
    assert_eq!(sniff(b""), None);
    assert_eq!(sniff(b"\x89PN"), None);
    assert_eq!(sniff(b"RIFF\x01\x02\x03\x04WAVE"), None);
}

#[test]
fn names_map_to_kinds_in_any_case() {
    assert_eq!(kind_from_name("a.PNG"), Some(FileKind::Png));
    assert_eq!(kind_from_name("a.jpg"), Some(FileKind::Jpeg));
    assert_eq!(kind_from_name("a.JPEG"), Some(FileKind::Jpeg));
    assert_eq!(kind_from_name("a.gif"), Some(FileKind::Gif));
    assert_eq!(kind_from_name("a.webp"), Some(FileKind::Webp));
    assert_eq!(kind_from_name("a.Mp4"), Some(FileKind::Mp4));
    for name in ["a.txt", "png", "a.", "", ".png.bak"] {
        assert_eq!(kind_from_name(name), None, "{name:?}");
    }
    assert_eq!(FileKind::Jpeg.extension(), "jpg");
    assert!(FileKind::Mp4.is_video());
    assert!(!FileKind::Png.is_video());
}

#[test]
fn admission_checks_type_then_size_then_total() {
    let none = Usage::default();
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\0";
    let jpeg = [0xFF, 0xD8, 0xFF, 0xE0];
    let mp4 = b"\0\0\0\x18ftypmp42";
    assert_eq!(
        admit("x.png", b"just text!!!", 10, none),
        Err(Refusal::Contents)
    );
    assert_eq!(admit("x.png", &jpeg, 10, none), Err(Refusal::Contents));
    assert_eq!(admit("x.txt", png, 10, none), Err(Refusal::Extension));
    assert_eq!(
        admit("x.png", png, MAX_IMAGE_BYTES, none),
        Ok(FileKind::Png)
    );
    assert_eq!(
        admit("x.png", png, MAX_IMAGE_BYTES + 1, none),
        Err(Refusal::ImageTooLarge)
    );
    assert_eq!(
        admit("x.mp4", mp4, MAX_VIDEO_BYTES, none),
        Ok(FileKind::Mp4)
    );
    assert_eq!(
        admit("x.mp4", mp4, MAX_VIDEO_BYTES + 1, none),
        Err(Refusal::VideoTooLarge)
    );
    let mb = 1024 * 1024;
    let busy = Usage {
        count: 2,
        bytes: 70 * mb,
    };
    assert_eq!(
        admit("x.mp4", mp4, 40 * mb, busy),
        Err(Refusal::TotalTooLarge)
    );
    assert_eq!(admit("x.mp4", mp4, 30 * mb, busy), Ok(FileKind::Mp4));
    let full = Usage {
        count: 10,
        bytes: 0,
    };
    assert_eq!(admit("x.txt", b"", 0, full), Err(Refusal::TooMany));
    let huge = Usage {
        count: 0,
        bytes: u64::MAX,
    };
    assert_eq!(admit("x.png", png, 10, huge), Err(Refusal::TotalTooLarge));
}

#[test]
fn each_refusal_names_the_file() {
    let n = "big.jpg";
    let table = [
        (
            Refusal::Extension,
            "big.jpg was not added: only png, jpg, gif, webp and mp4 files can be attached.",
        ),
        (
            Refusal::Contents,
            "big.jpg was not added: its contents are not a png, jpg, gif, webp or mp4 file.",
        ),
        (
            Refusal::ImageTooLarge,
            "big.jpg was not added: an image can be at most 10 MB.",
        ),
        (
            Refusal::VideoTooLarge,
            "big.jpg was not added: a video can be at most 50 MB.",
        ),
        (
            Refusal::TotalTooLarge,
            "big.jpg was not added: the draft would be larger than 100 MB to send (a screenshot counts twice).",
        ),
        (
            Refusal::TooMany,
            "big.jpg was not added: a draft holds at most 10 attachments.",
        ),
        (
            Refusal::Unreadable,
            "big.jpg was not added: it could not be read.",
        ),
    ];
    for (refusal, want) in table {
        assert_eq!(refusal.sentence(n), want);
    }
}

#[test]
fn the_texts_and_limits_match_the_design() {
    assert_eq!(HOLD.as_secs(), 3);
    assert_eq!(
        (
            MIN_BOX_POINTS,
            MAX_BOXES,
            MAX_COMMENT_CHARS,
            THUMBNAIL_MAX_WIDTH,
            MAX_ATTACHMENTS
        ),
        (8.0, 20, 500, 320, 10)
    );
    assert_eq!(
        (MAX_IMAGE_BYTES, MAX_VIDEO_BYTES, MAX_TOTAL_BYTES),
        (10 << 20, 50 << 20, 100 << 20)
    );
    assert_eq!(
        capture_failed_text("boom"),
        "Could not capture the window: boom."
    );
    assert_eq!(
        picker_failed_text("x"),
        "Could not open the file picker: x."
    );
    assert_eq!(
        save_failed_text("x"),
        "Could not save the feedback draft: x."
    );
    assert_eq!(
        missing_files_text(1),
        "1 attachment was removed because its file is missing."
    );
    assert_eq!(
        missing_files_text(3),
        "3 attachments were removed because their files are missing."
    );
    assert_eq!(
        annotator_title(2, 3),
        "Screenshot 2 of 3 · Drag a box over what you mean"
    );
    assert_eq!(card_title(1, 1), "Screenshot 1 · 1 comment");
    assert_eq!(card_title(2, 0), "Screenshot 2 · 0 comments");
    assert_eq!(
        discard_description(1),
        "1 attachment and its comments are deleted. This cannot be undone."
    );
    assert_eq!(
        discard_description(4),
        "4 attachments and their comments are deleted. This cannot be undone."
    );
    assert_eq!(
        annotator_discard_description(1),
        "Its 1 comment is deleted."
    );
    assert_eq!(
        annotator_discard_description(3),
        "Its 3 comments are deleted."
    );
    assert_eq!(MAX_NOTICES, 20);
    assert_eq!(
        more_files_text(1),
        "1 more file was not added: the draft is full."
    );
    assert_eq!(
        more_files_text(5),
        "5 more files were not added: the draft is full."
    );
    assert_eq!(more_notices_text(3), "…and 3 more.");
    assert_eq!(
        draw_failed_text("x"),
        "Could not draw the boxes on the screenshot: x."
    );
    assert_eq!(removed_attachments_text(0, 0), None);
    assert_eq!(removed_attachments_text(2, 0), Some(missing_files_text(2)));
    assert_eq!(
        removed_attachments_text(0, 1).as_deref(),
        Some("1 attachment was removed because a draft holds at most 10.")
    );
    assert_eq!(
        removed_attachments_text(1, 2).as_deref(),
        Some(
            "3 attachments were removed because their files are missing or a draft holds at \
             most 10."
        )
    );
}

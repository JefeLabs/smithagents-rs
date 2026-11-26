//! The `Feedback` state entity: the capture sequence against a fake
//! capturer (two frame hops, run by hand), files added through a fake
//! picker, boxes saved as they change, Done drawing the annotated image, and
//! the draft on disk across a restart. No screen, no dialog, no real home.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use common::feedback::{act, capture_now, file_names, png, record_events, run_hop, FeedbackSetup};
use common::ui::open_shell_options;
use futures::FutureExt as _;
use gpui_kit::{AppContext as _, Entity, TestAppContext};
use smithagents_app::feedback::annotation::{Annotation, FracRect};
use smithagents_app::feedback::draft::{AttachmentId, AttachmentKind, Draft};
use smithagents_app::feedback::files::Refusal;
use smithagents_app::feedback::render::{decode, BOX_RGB};
use smithagents_app::feedback::state::{Feedback, FeedbackEvent, FeedbackOptions};
use smithagents_app::feedback::text::{
    capture_failed_text, more_files_text, more_notices_text, save_failed_text,
    CAPTURE_FRAMES_MISSED, CAPTURE_PICTURE_TIMEOUT, CAPTURE_TOO_SLOW, DRAFT_FULL_TEXT,
    DRAFT_NOT_SET_ASIDE, MAX_COMMENT_CHARS, MAX_IMAGE_BYTES, PERMISSION_TEXT, UNSUPPORTED_TEXT,
    WINDOW_NOT_FOUND_TEXT,
};
use smithagents_app::shell::ShellOptions;
use smithagents_capture::{CaptureError, FakeCapturer, Screenshot};

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const JPEG_START: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F', b'I', b'F', 0, 1,
];
const MP4_START: &[u8] = &[
    0, 0, 0, 0x18, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm',
];

fn draft(cx: &TestAppContext, feedback: &Entity<Feedback>) -> Draft {
    cx.read(|cx| feedback.read(cx).draft().clone())
}

fn notices(cx: &TestAppContext, feedback: &Entity<Feedback>) -> Vec<String> {
    cx.read(|cx| feedback.read(cx).notices().to_vec())
}

fn annotating(cx: &TestAppContext, feedback: &Entity<Feedback>) -> Option<AttachmentId> {
    cx.read(|cx| feedback.read(cx).annotating().cloned())
}

fn note(x: f32, y: f32, w: f32, h: f32, comment: &str) -> Annotation {
    Annotation {
        rect: FracRect { x, y, w, h },
        comment: comment.to_string(),
    }
}

fn pixel(image: &image::RgbaImage, x: u32, y: u32) -> [u8; 3] {
    let p = image.get_pixel(x, y).0;
    [p[0], p[1], p[2]]
}

/// `Feedback::load_image` for `id`, run to its end.
fn loaded_image(
    cx: &mut TestAppContext,
    window: gpui_kit::WindowHandle<gpui_kit::base::Root>,
    feedback: &Entity<Feedback>,
    id: &AttachmentId,
) -> Arc<gpui_kit::RenderImage> {
    let task = act(cx, window, feedback, |f, _, cx| f.load_image(id, cx));
    task.now_or_never()
        .expect("the image is decoded once parked")
        .expect("the stored image decodes")
}

/// A file of `len` bytes that starts with `head`.
fn sized_file(setup: &FeedbackSetup, name: &str, head: &[u8], len: u64) -> std::path::PathBuf {
    let path = setup.picked_file(name, head);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(len)
        .unwrap();
    path
}

#[gpui_kit::test]
fn capture_waits_two_frames_then_asks_the_capturer_once(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    let events = record_events(cx, &feedback);

    act(cx, window, &feedback, |f, window, cx| f.capture(window, cx));
    assert!(cx.read(|cx| feedback.read(cx).capturing()));
    assert_eq!(setup.hops.pending(), 1);
    assert!(setup.capturer.calls().is_empty());

    run_hop(cx, window, &setup);
    assert_eq!(setup.hops.pending(), 1, "the first hop asks for a second");
    assert!(setup.capturer.calls().is_empty());

    run_hop(cx, window, &setup);
    let calls = setup.capturer.calls();
    assert_eq!(calls.len(), 1);
    let scale = cx
        .update_window(window.into(), |_, window, _| window.scale_factor())
        .unwrap();
    assert_eq!(calls[0].pid, std::process::id());
    assert_eq!(calls[0].frame.width, 1200.0);
    assert_eq!(calls[0].frame.height, 800.0);
    assert_eq!(calls[0].scale, scale);
    assert!(!cx.read(|cx| feedback.read(cx).capturing()));

    let draft = draft(cx, &feedback);
    assert_eq!(draft.attachments.len(), 1);
    let shot = &draft.attachments[0];
    assert_eq!(shot.kind, AttachmentKind::Screenshot);
    assert_eq!(shot.stored_name, "screenshot-1.png");
    assert_eq!(shot.original_name, "screenshot-1.png");
    assert_eq!(shot.width, Some(2400));
    assert_eq!(shot.height, Some(1512));
    assert!(shot.boxes.is_empty());
    assert!(!shot.rendered);
    let bytes = fs::read(setup.draft_dir().join("screenshot-1.png")).unwrap();
    assert!(bytes.starts_with(PNG_SIGNATURE));
    assert_eq!(shot.size, bytes.len() as u64);
    assert_eq!(setup.store().load().draft, draft);
    assert_eq!(
        *events.borrow(),
        vec![FeedbackEvent::OpenAnnotator(shot.id.clone())]
    );
    assert_eq!(annotating(cx, &feedback), Some(shot.id.clone()));
    assert!(notices(cx, &feedback).is_empty());
}

#[gpui_kit::test]
fn each_capture_error_has_its_sentence_and_adds_nothing(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    let events = record_events(cx, &feedback);
    let cases = [
        (CaptureError::PermissionDenied, PERMISSION_TEXT.to_string()),
        (
            CaptureError::WindowNotFound,
            WINDOW_NOT_FOUND_TEXT.to_string(),
        ),
        (CaptureError::Unsupported, UNSUPPORTED_TEXT.to_string()),
        (
            CaptureError::Failed("boom".into()),
            "Could not capture the window: boom.".to_string(),
        ),
    ];
    for (error, sentence) in cases {
        setup.capturer.set(Err(error.clone()));
        events.borrow_mut().clear();
        capture_now(cx, window, &feedback, &setup);
        assert_eq!(notices(cx, &feedback), vec![sentence], "{error:?}");
        assert!(draft(cx, &feedback).is_empty(), "{error:?}");
        assert!(
            !file_names(&setup.draft_dir())
                .iter()
                .any(|name| name.starts_with("screenshot-")),
            "{error:?}"
        );
        assert_eq!(
            *events.borrow(),
            vec![FeedbackEvent::RevealPane],
            "{error:?}"
        );
        assert!(!cx.read(|cx| feedback.read(cx).capturing()));
        assert_eq!(annotating(cx, &feedback), None);
    }
    assert_eq!(setup.capturer.calls().len(), 4);
}

#[gpui_kit::test]
fn a_single_color_capture_is_reported_as_empty(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    setup.capturer.set(Ok(Screenshot {
        width: 64,
        height: 64,
        rgba: [30, 30, 30, 255].repeat(64 * 64),
    }));
    let (window, _, feedback) = setup.open(cx);
    capture_now(cx, window, &feedback, &setup);
    assert_eq!(
        notices(cx, &feedback),
        vec!["Could not capture the window: the capture came back empty.".to_string()]
    );
    assert!(draft(cx, &feedback).is_empty());
}

#[gpui_kit::test]
fn a_capture_with_a_short_buffer_is_reported_not_a_panic(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    setup.capturer.set(Ok(Screenshot {
        width: 64,
        height: 64,
        rgba: vec![7; 100],
    }));
    let (window, _, feedback) = setup.open(cx);
    capture_now(cx, window, &feedback, &setup);
    assert_eq!(
        notices(cx, &feedback),
        vec!["Could not capture the window: the capture has the wrong size.".to_string()]
    );
    assert!(draft(cx, &feedback).is_empty());
}

#[gpui_kit::test]
fn a_second_capture_while_one_runs_is_ignored(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    act(cx, window, &feedback, |f, window, cx| f.capture(window, cx));
    act(cx, window, &feedback, |f, window, cx| f.capture(window, cx));
    assert_eq!(setup.hops.pending(), 1);
    run_hop(cx, window, &setup);
    act(cx, window, &feedback, |f, window, cx| f.capture(window, cx));
    assert_eq!(setup.hops.pending(), 1);
    run_hop(cx, window, &setup);
    assert_eq!(setup.hops.pending(), 0);
    assert_eq!(setup.capturer.calls().len(), 1);
    assert_eq!(draft(cx, &feedback).attachments.len(), 1);

    // While the annotator is open, a capture is ignored too.
    act(cx, window, &feedback, |f, window, cx| f.capture(window, cx));
    assert_eq!(setup.hops.pending(), 0);
}

#[gpui_kit::test]
fn the_eleventh_attachment_is_refused_before_capturing(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let bytes = png(4, 4);
    let paths = (1..=10)
        .map(|n| setup.picked_file(&format!("good-{n}.png"), &bytes))
        .collect();
    setup.pick(paths);
    let (window, _, feedback) = setup.open(cx);
    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx)
    });
    assert_eq!(draft(cx, &feedback).attachments.len(), 10);
    let events = record_events(cx, &feedback);

    act(cx, window, &feedback, |f, window, cx| f.capture(window, cx));
    assert_eq!(setup.hops.pending(), 0);
    assert!(setup.capturer.calls().is_empty());
    assert!(!cx.read(|cx| feedback.read(cx).capturing()));
    assert_eq!(notices(cx, &feedback), vec![DRAFT_FULL_TEXT.to_string()]);
    assert_eq!(*events.borrow(), vec![FeedbackEvent::RevealPane]);
}

#[gpui_kit::test]
fn picked_files_are_checked_one_by_one(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let good = png(4, 4);
    let folder = setup.home.path().join("picked").join("folder.png");
    fs::create_dir_all(&folder).unwrap();
    let mut clip = MP4_START.to_vec();
    clip.extend_from_slice(&[1; 500]);
    setup.pick(vec![
        setup.picked_file("good.png", &good),
        setup.picked_file("notes.txt", b"some notes"),
        setup.picked_file("fake.png", b"this is text, not a picture"),
        sized_file(&setup, "big.jpg", JPEG_START, MAX_IMAGE_BYTES + 1),
        setup.picked_file("clip.mp4", &clip),
        setup.picked_file("empty.gif", b""),
        folder,
    ]);
    let (window, _, feedback) = setup.open(cx);
    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx)
    });
    assert!(!cx.read(|cx| feedback.read(cx).busy()));

    let draft = draft(cx, &feedback);
    let names: Vec<(&str, AttachmentKind, &str)> = draft
        .attachments
        .iter()
        .map(|a| (a.stored_name.as_str(), a.kind, a.original_name.as_str()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("image-1.png", AttachmentKind::Image, "good.png"),
            ("clip-1.mp4", AttachmentKind::Video, "clip.mp4"),
        ]
    );
    assert_eq!(
        notices(cx, &feedback),
        vec![
            Refusal::Extension.sentence("notes.txt"),
            Refusal::Contents.sentence("fake.png"),
            Refusal::ImageTooLarge.sentence("big.jpg"),
            Refusal::Contents.sentence("empty.gif"),
            Refusal::Unreadable.sentence("folder.png"),
        ]
    );
    let dir = setup.draft_dir();
    assert_eq!(fs::read(dir.join("image-1.png")).unwrap(), good);
    assert_eq!(fs::read(dir.join("clip-1.mp4")).unwrap(), clip);
    assert_eq!(draft.attachments[0].size, good.len() as u64);
    assert_eq!(draft.attachments[1].size, clip.len() as u64);
    assert!(dir.join("image-1.thumb.png").is_file());
    assert!(!dir.join("clip-1.thumb.png").exists());
    let (image_id, clip_id) = (&draft.attachments[0].id, &draft.attachments[1].id);
    assert!(cx
        .read(|cx| feedback.read(cx).thumbnail(image_id))
        .is_some());
    assert!(cx.read(|cx| feedback.read(cx).thumbnail(clip_id)).is_none());
    assert_eq!(setup.store().load().draft, draft);
}

#[gpui_kit::test]
fn two_files_with_the_same_name_get_different_stored_names(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    setup.pick(vec![
        setup.picked_file("a/photo.jpg", JPEG_START),
        setup.picked_file("b/photo.jpg", JPEG_START),
    ]);
    let (window, _, feedback) = setup.open(cx);
    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx)
    });
    let draft = draft(cx, &feedback);
    let names: Vec<(&str, &str)> = draft
        .attachments
        .iter()
        .map(|a| (a.stored_name.as_str(), a.original_name.as_str()))
        .collect();
    assert_eq!(
        names,
        vec![("image-1.jpg", "photo.jpg"), ("image-2.jpg", "photo.jpg")]
    );
    // Not a picture that decodes: kept, without a thumbnail.
    assert!(notices(cx, &feedback).is_empty());
    let id = &draft.attachments[0].id;
    assert!(cx.read(|cx| feedback.read(cx).thumbnail(id)).is_none());
}

#[gpui_kit::test]
fn the_total_limit_counts_files_from_the_same_pick(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let forty_mb = 40 * 1024 * 1024;
    setup.pick(vec![
        sized_file(&setup, "a.mp4", MP4_START, forty_mb),
        sized_file(&setup, "b.mp4", MP4_START, forty_mb),
        sized_file(&setup, "c.mp4", MP4_START, forty_mb),
    ]);
    let (window, _, feedback) = setup.open(cx);
    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx)
    });
    let stored: Vec<String> = draft(cx, &feedback)
        .attachments
        .iter()
        .map(|a| a.stored_name.clone())
        .collect();
    assert_eq!(stored, vec!["clip-1.mp4", "clip-2.mp4"]);
    assert_eq!(
        notices(cx, &feedback),
        vec![Refusal::TotalTooLarge.sentence("c.mp4")]
    );
}

#[gpui_kit::test]
fn a_cancelled_or_failed_picker(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    *setup.picked.borrow_mut() = Ok(None);
    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx)
    });
    assert!(draft(cx, &feedback).is_empty());
    assert!(notices(cx, &feedback).is_empty());
    assert!(!setup.draft_dir().exists());

    *setup.picked.borrow_mut() = Err("no display".into());
    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx)
    });
    assert!(draft(cx, &feedback).is_empty());
    assert_eq!(
        notices(cx, &feedback),
        vec!["Could not open the file picker: no display.".to_string()]
    );
}

#[gpui_kit::test]
fn boxes_are_saved_as_they_change_and_done_draws_the_annotated_image(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    capture_now(cx, window, &feedback, &setup);
    let id = annotating(cx, &feedback).expect("the capture opens the annotator");
    let boxes = vec![note(0.25, 0.25, 0.25, 0.25, "the toolbar")];
    let given = boxes.clone();
    act(cx, window, &feedback, |f, _, cx| {
        f.set_boxes(&id, given, cx)
    });

    let on_disk = setup.store().load().draft;
    assert_eq!(on_disk.attachments[0].boxes, boxes);
    assert!(!on_disk.attachments[0].rendered);
    let dir = setup.draft_dir();
    assert!(!dir.join("screenshot-1.annotated.png").exists());

    let events = record_events(cx, &feedback);
    act(cx, window, &feedback, |f, _, cx| f.finish_annotating(cx));
    let annotated = decode(&fs::read(dir.join("screenshot-1.annotated.png")).unwrap()).unwrap();
    assert_eq!(annotated.dimensions(), (2400, 1512));
    assert_eq!(pixel(&annotated, 600, 378), BOX_RGB);
    let thumb = decode(&fs::read(dir.join("screenshot-1.thumb.png")).unwrap()).unwrap();
    assert_eq!(thumb.dimensions(), (320, 202));
    let on_disk = setup.store().load().draft;
    assert!(on_disk.attachments[0].rendered);
    assert_eq!(on_disk.attachments[0].boxes, boxes);
    assert_eq!(*events.borrow(), vec![FeedbackEvent::CloseAnnotator]);
    assert_eq!(annotating(cx, &feedback), None);
    assert!(cx.read(|cx| feedback.read(cx).thumbnail(&id)).is_some());
    assert!(!cx.read(|cx| feedback.read(cx).busy()));
}

#[gpui_kit::test]
fn removing_and_discarding_delete_the_files(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    setup.pick(vec![setup.picked_file("good.png", &png(4, 4))]);
    let (window, _, feedback) = setup.open(cx);
    capture_now(cx, window, &feedback, &setup);
    let shot = annotating(cx, &feedback).unwrap();
    let boxes = vec![note(0.25, 0.25, 0.25, 0.25, "the toolbar")];
    act(cx, window, &feedback, |f, _, cx| {
        f.set_boxes(&shot, boxes, cx)
    });
    act(cx, window, &feedback, |f, _, cx| f.finish_annotating(cx));
    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx)
    });
    let image = draft(cx, &feedback).attachments[1].id.clone();
    let dir = setup.draft_dir();
    assert_eq!(
        file_names(&dir),
        vec![
            "draft.json",
            "image-1.png",
            "image-1.thumb.png",
            "screenshot-1.annotated.png",
            "screenshot-1.png",
            "screenshot-1.thumb.png",
        ]
    );
    let shot_thumb = cx.read(|cx| feedback.read(cx).thumbnail(&shot)).unwrap();
    let image_thumb = cx.read(|cx| feedback.read(cx).thumbnail(&image)).unwrap();
    assert!(act(cx, window, &feedback, |f, _, _| f.take_retired_images()).is_empty());

    act(cx, window, &feedback, |f, _, cx| f.remove(&shot, cx));
    assert_eq!(
        file_names(&dir),
        vec!["draft.json", "image-1.png", "image-1.thumb.png"]
    );
    let on_disk = setup.store().load().draft;
    assert_eq!(on_disk.attachments.len(), 1);
    assert_eq!(on_disk.attachments[0].id, image);
    assert!(cx.read(|cx| feedback.read(cx).thumbnail(&shot)).is_none());
    // The shell's Feedback pane takes each retired thumbnail as it is
    // retired and releases it: nothing but this test holds it any more.
    assert!(act(cx, window, &feedback, |f, _, _| f.take_retired_images()).is_empty());
    assert_eq!(Arc::strong_count(&shot_thumb), 1);
    assert_eq!(Arc::strong_count(&image_thumb), 2, "still the image's");

    act(cx, window, &feedback, |f, _, cx| f.discard_all(cx));
    assert!(!dir.exists(), "no draft directory is left");
    assert!(draft(cx, &feedback).is_empty());
    assert!(act(cx, window, &feedback, |f, _, _| f.take_retired_images()).is_empty());
    assert_eq!(Arc::strong_count(&image_thumb), 1);
    assert!(setup.store().load().draft.is_empty());
}

#[gpui_kit::test]
fn discarding_the_annotated_screenshot_removes_it(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    capture_now(cx, window, &feedback, &setup);
    let events = record_events(cx, &feedback);
    act(cx, window, &feedback, |f, _, cx| f.discard_annotated(cx));
    assert!(draft(cx, &feedback).is_empty());
    assert!(!setup.draft_dir().join("screenshot-1.png").exists());
    assert!(setup.store().load().draft.is_empty());
    assert_eq!(*events.borrow(), vec![FeedbackEvent::CloseAnnotator]);
    assert_eq!(annotating(cx, &feedback), None);
}

#[gpui_kit::test]
fn a_failed_save_is_said_and_the_draft_stays_on_screen(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    fs::write(setup.home.path().join("feedback"), b"in the way").unwrap();
    let (window, _, feedback) = setup.open(cx);
    let events = record_events(cx, &feedback);
    capture_now(cx, window, &feedback, &setup);
    let said = notices(cx, &feedback);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].starts_with("Could not save the feedback draft: ") && said[0].ends_with('.'),
        "{said:?}"
    );
    assert!(!cx.read(|cx| feedback.read(cx).capturing()));
    assert_eq!(*events.borrow(), vec![FeedbackEvent::RevealPane]);
}

#[cfg(unix)]
#[gpui_kit::test]
fn a_failed_save_keeps_the_boxes_on_screen_and_the_old_draft_on_disk(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;

    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    capture_now(cx, window, &feedback, &setup);
    let id = annotating(cx, &feedback).unwrap();
    let dir = setup.draft_dir();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let boxes = vec![note(0.25, 0.25, 0.25, 0.25, "the toolbar")];
    let given = boxes.clone();
    act(cx, window, &feedback, |f, _, cx| {
        f.set_boxes(&id, given, cx)
    });
    let said = notices(cx, &feedback);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("Could not save the feedback draft: "));
    assert_eq!(draft(cx, &feedback).attachments[0].boxes, boxes);
    assert!(setup.store().load().draft.attachments[0].boxes.is_empty());
}

#[gpui_kit::test]
fn a_restart_restores_the_draft_and_redraws_what_was_not_drawn(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    capture_now(cx, window, &feedback, &setup);
    let id = annotating(cx, &feedback).unwrap();
    let boxes = vec![note(0.25, 0.25, 0.25, 0.25, "the toolbar")];
    let given = boxes.clone();
    act(cx, window, &feedback, |f, _, cx| {
        f.set_boxes(&id, given, cx)
    });
    let dir = setup.draft_dir();
    assert!(!dir.join("screenshot-1.annotated.png").exists());

    let (_, _, again) = setup.open(cx);
    let restored = draft(cx, &again);
    assert_eq!(restored.attachments.len(), 1);
    assert_eq!(restored.attachments[0].id, id);
    assert_eq!(restored.attachments[0].boxes, boxes);
    assert!(restored.attachments[0].rendered);
    let annotated = decode(&fs::read(dir.join("screenshot-1.annotated.png")).unwrap()).unwrap();
    assert_eq!(pixel(&annotated, 600, 378), BOX_RGB);
    assert!(cx.read(|cx| again.read(cx).thumbnail(&id)).is_some());
    assert!(setup.store().load().draft.attachments[0].rendered);
    assert!(notices(cx, &again).is_empty());
}

#[gpui_kit::test]
fn thumbnails_on_disk_are_shown_after_a_restart(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    setup.pick(vec![setup.picked_file("good.png", &png(4, 4))]);
    let (window, _, feedback) = setup.open(cx);
    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx)
    });
    let id = draft(cx, &feedback).attachments[0].id.clone();

    let (_, _, again) = setup.open(cx);
    assert!(cx.read(|cx| again.read(cx).thumbnail(&id)).is_some());
    let image = loaded_image(cx, window, &again, &id);
    assert_eq!(
        (image.size(0).width.0, image.size(0).height.0),
        (4, 4),
        "load_image decodes the stored original"
    );
}

#[gpui_kit::test]
fn without_a_store_the_draft_is_kept_in_memory(cx: &mut TestAppContext) {
    let capturer = Arc::new(FakeCapturer::new(Ok(smithagents_capture::test_pattern(
        640, 400,
    ))));
    let setup = FeedbackSetup::new();
    let hops = setup.options().feedback.unwrap().frame_hop;
    let options = ShellOptions {
        feedback: Some(FeedbackOptions {
            store: None,
            capturer: Some(capturer.clone()),
            picker: None,
            frame_hop: hops,
            picture_timeout: None,
        }),
        ..Default::default()
    };
    let (window, shell) = open_shell_options(cx, options, (1200., 800.));
    let feedback = cx.read(|cx| shell.read(cx).feedback().clone());
    assert!(cx.read(|cx| feedback.read(cx).store().is_none()));
    capture_now(cx, window, &feedback, &setup);
    let id = annotating(cx, &feedback).expect("the capture is kept");
    act(cx, window, &feedback, |f, _, cx| {
        f.set_boxes(&id, vec![note(0.25, 0.25, 0.25, 0.25, "x")], cx)
    });
    act(cx, window, &feedback, |f, _, cx| f.finish_annotating(cx));
    assert!(cx.read(|cx| feedback.read(cx).thumbnail(&id)).is_some());
    let image = loaded_image(cx, window, &feedback, &id);
    assert_eq!(image.size(0).width.0, 640);
    assert!(notices(cx, &feedback).is_empty());
    assert_eq!(capturer.calls().len(), 1);
    // The picker defaults to "cancelled".
    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx)
    });
    assert_eq!(draft(cx, &feedback).attachments.len(), 1);
    assert!(
        file_names(setup.home.path()).is_empty(),
        "nothing is written"
    );
}

#[gpui_kit::test]
fn the_default_options_answer_unsupported_and_pick_nothing(cx: &mut TestAppContext) {
    let (window, shell) = open_shell_options(cx, ShellOptions::default(), (1200., 800.));
    let feedback = cx.read(|cx| shell.read(cx).feedback().clone());
    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx)
    });
    assert!(draft(cx, &feedback).is_empty());
    assert!(notices(cx, &feedback).is_empty());
    assert!(!cx.did_prompt_for_paths());
    // The default hop is the window's next frame, which the test platform
    // runs only when asked.
    act(cx, window, &feedback, |f, window, cx| f.capture(window, cx));
    for _ in 0..2 {
        cx.update_window(window.into(), |_, window, cx| {
            window.simulate_next_frame(cx)
        })
        .unwrap();
        cx.run_until_parked();
    }
    assert_eq!(notices(cx, &feedback), vec![UNSUPPORTED_TEXT.to_string()]);
    assert!(!cx.read(|cx| feedback.read(cx).capturing()));
}

fn busy(cx: &TestAppContext, feedback: &Entity<Feedback>) -> bool {
    cx.read(|cx| feedback.read(cx).busy())
}

fn set_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Picks `count` good PNGs named `good-<n>.png` and adds them.
fn add_good_files(
    cx: &mut TestAppContext,
    window: gpui_kit::WindowHandle<gpui_kit::base::Root>,
    feedback: &Entity<Feedback>,
    setup: &FeedbackSetup,
    count: usize,
) {
    let bytes = png(4, 4);
    let paths = (1..=count)
        .map(|n| setup.picked_file(&format!("good-{n}.png"), &bytes))
        .collect();
    setup.pick(paths);
    act(cx, window, feedback, |f, window, cx| {
        f.add_files(window, cx)
    });
}

#[gpui_kit::test]
fn a_capture_that_lands_on_a_draft_a_pick_filled_adds_nothing(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    add_good_files(cx, window, &feedback, &setup, 9);
    let events = record_events(cx, &feedback);

    // The capture waits for its frames while a pick fills the draft.
    act(cx, window, &feedback, |f, window, cx| f.capture(window, cx));
    setup.pick(vec![setup.picked_file("last.png", &png(4, 4))]);
    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx)
    });
    assert_eq!(draft(cx, &feedback).attachments.len(), 10);
    run_hop(cx, window, &setup);
    run_hop(cx, window, &setup);

    assert_eq!(setup.capturer.calls().len(), 1);
    assert_eq!(notices(cx, &feedback), vec![DRAFT_FULL_TEXT.to_string()]);
    assert_eq!(*events.borrow(), vec![FeedbackEvent::RevealPane]);
    assert!(!file_names(&setup.draft_dir())
        .iter()
        .any(|name| name.starts_with("screenshot-")));
    assert_eq!(draft(cx, &feedback).attachments.len(), 10);
    assert!(!cx.read(|cx| feedback.read(cx).capturing()));
}

#[gpui_kit::test]
fn removing_a_screenshot_while_done_draws_it_leaves_no_file(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    capture_now(cx, window, &feedback, &setup);
    let id = annotating(cx, &feedback).unwrap();
    let events = record_events(cx, &feedback);

    // Without parking in between: the drawing has not run yet.
    act(cx, window, &feedback, |f, _, cx| {
        f.set_boxes(&id, vec![note(0.25, 0.25, 0.25, 0.25, "the toolbar")], cx);
        f.finish_annotating(cx);
        f.remove(&id, cx);
    });

    assert!(
        file_names(&setup.draft_dir()).is_empty(),
        "{:?}",
        file_names(&setup.draft_dir())
    );
    assert_eq!(*events.borrow(), vec![FeedbackEvent::CloseAnnotator]);
    assert!(!busy(cx, &feedback));
    assert_eq!(annotating(cx, &feedback), None);
    assert!(draft(cx, &feedback).is_empty());
}

#[gpui_kit::test]
fn discarding_while_files_are_added_leaves_no_draft_directory(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    add_good_files(cx, window, &feedback, &setup, 1);
    assert!(setup.draft_dir().exists());

    setup.pick(vec![
        setup.picked_file("a.png", &png(4, 4)),
        setup.picked_file("b.png", &png(4, 4)),
    ]);
    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx);
        f.discard_all(cx);
    });

    assert!(!setup.draft_dir().exists());
    assert!(draft(cx, &feedback).is_empty());
    assert!(!busy(cx, &feedback));
}

#[gpui_kit::test]
fn removing_an_attachment_sweeps_files_the_draft_does_not_name(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    add_good_files(cx, window, &feedback, &setup, 2);
    let dir = setup.draft_dir();
    fs::write(dir.join("image-9.png"), b"left behind").unwrap();
    fs::write(dir.join("screenshot-4.annotated.png"), b"left behind").unwrap();
    let first = draft(cx, &feedback).attachments[0].id.clone();

    act(cx, window, &feedback, |f, _, cx| f.remove(&first, cx));

    assert_eq!(
        file_names(&dir),
        vec!["draft.json", "image-2.png", "image-2.thumb.png"]
    );
    let second = draft(cx, &feedback).attachments[0].id.clone();
    act(cx, window, &feedback, |f, _, cx| f.remove(&second, cx));
    assert!(!dir.exists(), "an empty draft leaves no directory");
}

#[gpui_kit::test]
fn remove_saves_the_draft_before_it_deletes_the_files(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    add_good_files(cx, window, &feedback, &setup, 2);
    let dir = setup.draft_dir();
    let first = draft(cx, &feedback).attachments[0].id.clone();
    // The draft cannot be saved: a folder is in its place.
    fs::remove_file(dir.join("draft.json")).unwrap();
    fs::create_dir_all(dir.join("draft.json").join("in-the-way")).unwrap();

    act(cx, window, &feedback, |f, _, cx| f.remove(&first, cx));

    let said = notices(cx, &feedback);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("Could not save the feedback draft: "));
    assert_eq!(draft(cx, &feedback).attachments.len(), 1);
    // What the saved draft still names is still there.
    assert!(dir.join("image-1.png").is_file());
    assert!(dir.join("image-1.thumb.png").is_file());
}

#[gpui_kit::test]
fn set_boxes_keeps_only_what_loads_back(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    capture_now(cx, window, &feedback, &setup);
    let id = annotating(cx, &feedback).unwrap();
    let long = "x".repeat(MAX_COMMENT_CHARS + 100);
    let mut boxes = vec![
        note(f32::NAN, 0.1, 0.2, 0.2, "not a number"),
        note(0.1, 0.1, 0.0, 0.2, "no width"),
        note(0.9, 0.1, 0.5, 0.2, "hangs over"),
        note(0.1, 0.1, 0.2, 0.2, &long),
    ];
    boxes.extend((0..25).map(|n| note(0.1, 0.1, 0.2, 0.2, &format!("box {n}"))));

    act(cx, window, &feedback, |f, _, cx| {
        f.set_boxes(&id, boxes, cx)
    });

    let held = draft(cx, &feedback).attachments[0].boxes.clone();
    assert_eq!(held.len(), 20);
    assert_eq!(held[0].comment, "hangs over");
    assert!((held[0].rect.w - 0.1).abs() < 1e-6, "{:?}", held[0].rect);
    assert_eq!(held[1].comment.chars().count(), MAX_COMMENT_CHARS);
    assert_eq!(held[19].comment, "box 17");
    let loaded = setup.store().load();
    assert!(loaded.notices.is_empty(), "{:?}", loaded.notices);
    assert_eq!(loaded.draft.attachments[0].boxes, held);
}

#[cfg(unix)]
#[gpui_kit::test]
fn a_draft_that_could_not_be_set_aside_is_never_saved_over(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let dir = setup.draft_dir();
    fs::create_dir_all(&dir).unwrap();
    let newer = r#"{"version":2,"attachments":[]}"#;
    fs::write(dir.join("draft.json"), newer).unwrap();
    let parent = setup.home.path().join("feedback");
    set_mode(&parent, 0o500);

    let (window, _, feedback) = setup.open(cx);
    let at_start = notices(cx, &feedback);
    capture_now(cx, window, &feedback, &setup);
    let after_capture = notices(cx, &feedback);
    set_mode(&parent, 0o700);

    let sentence = save_failed_text(DRAFT_NOT_SET_ASIDE);
    assert_eq!(at_start, vec![sentence.clone()]);
    assert_eq!(after_capture, vec![sentence]);
    assert!(draft(cx, &feedback).is_empty());
    assert_eq!(file_names(&dir), vec!["draft.json"]);
    assert_eq!(fs::read_to_string(dir.join("draft.json")).unwrap(), newer);
}

#[gpui_kit::test]
fn a_pick_stops_once_the_draft_is_full(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let bytes = png(4, 4);
    let mut paths: Vec<_> = (1..=9)
        .map(|n| setup.picked_file(&format!("good-{n}.png"), &bytes))
        .collect();
    paths.push(setup.picked_file("notes.txt", b"words"));
    paths.push(setup.picked_file("good-10.png", &bytes));
    // Never looked at: neither would be added anyway.
    paths.push(setup.picked_file("more.txt", b"words"));
    paths.push(setup.picked_file("good-11.png", &bytes));
    setup.pick(paths);
    let (window, _, feedback) = setup.open(cx);

    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx)
    });

    assert_eq!(draft(cx, &feedback).attachments.len(), 10);
    assert_eq!(
        notices(cx, &feedback),
        vec![Refusal::Extension.sentence("notes.txt"), more_files_text(2)]
    );
    assert!(!busy(cx, &feedback));

    // Already full: one sentence for the whole pick.
    setup.pick(vec![
        setup.picked_file("x.png", &bytes),
        setup.picked_file("y.png", &bytes),
    ]);
    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx)
    });
    assert_eq!(notices(cx, &feedback), vec![more_files_text(2)]);
}

#[gpui_kit::test]
fn at_most_twenty_notices_are_kept(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let paths = (1..=25)
        .map(|n| setup.picked_file(&format!("notes-{n}.txt"), b"words"))
        .collect();
    setup.pick(paths);
    let (window, _, feedback) = setup.open(cx);

    act(cx, window, &feedback, |f, window, cx| {
        f.add_files(window, cx)
    });

    let said = notices(cx, &feedback);
    assert_eq!(said.len(), 21);
    for (n, sentence) in said.iter().take(20).enumerate() {
        assert_eq!(
            *sentence,
            Refusal::Extension.sentence(&format!("notes-{}.txt", n + 1))
        );
    }
    assert_eq!(said[20], more_notices_text(5));
}

#[gpui_kit::test]
fn a_capture_whose_frames_never_come_gives_up_after_two_seconds(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    let events = record_events(cx, &feedback);

    act(cx, window, &feedback, |f, window, cx| f.capture(window, cx));
    cx.executor().advance_clock(Duration::from_millis(1990));
    cx.run_until_parked();
    assert!(cx.read(|cx| feedback.read(cx).capturing()), "still waiting");
    cx.executor().advance_clock(Duration::from_millis(10));
    cx.run_until_parked();

    assert!(!cx.read(|cx| feedback.read(cx).capturing()));
    assert_eq!(
        notices(cx, &feedback),
        vec![capture_failed_text(CAPTURE_FRAMES_MISSED)]
    );
    assert_eq!(*events.borrow(), vec![FeedbackEvent::RevealPane]);
    // Frames that come late take no picture.
    run_hop(cx, window, &setup);
    run_hop(cx, window, &setup);
    assert!(setup.capturer.calls().is_empty());
    assert!(draft(cx, &feedback).is_empty());

    // Capture works again.
    capture_now(cx, window, &feedback, &setup);
    assert_eq!(setup.capturer.calls().len(), 1);
    assert_eq!(draft(cx, &feedback).attachments.len(), 1);
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    assert!(
        annotating(cx, &feedback).is_some(),
        "a capture that ran is not undone"
    );
    assert!(notices(cx, &feedback).is_empty());
}

#[gpui_kit::test]
fn a_screenshot_that_does_not_decode_says_it_could_not_be_drawn(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    capture_now(cx, window, &feedback, &setup);
    let id = annotating(cx, &feedback).unwrap();
    fs::write(setup.draft_dir().join("screenshot-1.png"), b"not a picture").unwrap();
    let events = record_events(cx, &feedback);

    act(cx, window, &feedback, |f, _, cx| {
        f.set_boxes(&id, vec![note(0.25, 0.25, 0.25, 0.25, "the toolbar")], cx)
    });
    act(cx, window, &feedback, |f, _, cx| f.finish_annotating(cx));

    let said = notices(cx, &feedback);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].starts_with("Could not draw the boxes on the screenshot: "),
        "{said:?}"
    );
    assert_eq!(*events.borrow(), vec![FeedbackEvent::CloseAnnotator]);
    assert!(!busy(cx, &feedback));
}

#[gpui_kit::test]
fn removing_the_last_attachment_keeps_a_typed_main_idea(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    // A draft with a main idea (the next step types one) and one picture.
    let store = setup.store();
    store.write_file("image-1.png", &png(4, 4)).unwrap();
    let mut saved = Draft {
        main_idea: "Let the zoom follow the pointer".to_string(),
        ..Draft::default()
    };
    saved
        .attachments
        .push(smithagents_app::feedback::draft::Attachment {
            id: AttachmentId::new(),
            kind: AttachmentKind::Image,
            original_name: "a.png".to_string(),
            stored_name: "image-1.png".to_string(),
            size: 4,
            width: None,
            height: None,
            boxes: Vec::new(),
            rendered: false,
        });
    store.save(&saved).unwrap();
    let (window, _, feedback) = setup.open(cx);
    let id = draft(cx, &feedback).attachments[0].id.clone();

    act(cx, window, &feedback, |f, _, cx| f.remove(&id, cx));

    assert_eq!(file_names(&setup.draft_dir()), vec!["draft.json"]);
    let loaded = setup.store().load();
    assert_eq!(loaded.draft.main_idea, saved.main_idea);
    assert!(loaded.draft.attachments.is_empty());
}

#[gpui_kit::test]
fn a_picture_that_takes_too_long_is_dropped(cx: &mut TestAppContext) {
    assert_eq!(CAPTURE_PICTURE_TIMEOUT, Duration::from_secs(10));
    let setup = FeedbackSetup::new();
    // The test clock runs every ready job before it moves: a picture cannot
    // be made late with it, so the limit is nothing at all here.
    let mut options = setup.options();
    options.feedback.as_mut().unwrap().picture_timeout = Some(Duration::ZERO);
    let (window, shell) = open_shell_options(cx, options, (1200., 800.));
    let feedback = cx.read(|cx| shell.read(cx).feedback().clone());
    let events = record_events(cx, &feedback);

    // Whether the picture's job ran or was dropped unstarted, its result is
    // never used.
    capture_now(cx, window, &feedback, &setup);

    assert!(!cx.read(|cx| feedback.read(cx).capturing()));
    assert_eq!(
        notices(cx, &feedback),
        vec![capture_failed_text(CAPTURE_TOO_SLOW)]
    );
    assert_eq!(*events.borrow(), vec![FeedbackEvent::RevealPane]);
    assert!(draft(cx, &feedback).is_empty());
    assert_eq!(annotating(cx, &feedback), None);
    assert!(file_names(&setup.draft_dir()).is_empty(), "nothing written");

    // Capture is not stuck: the next one runs (and is late again).
    events.borrow_mut().clear();
    capture_now(cx, window, &feedback, &setup);
    assert!(!cx.read(|cx| feedback.read(cx).capturing()));
    assert_eq!(*events.borrow(), vec![FeedbackEvent::RevealPane]);
    assert!(draft(cx, &feedback).is_empty());
}

#[gpui_kit::test]
fn a_picture_in_time_is_kept_whatever_the_clock_does_later(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = setup.open(cx);
    capture_now(cx, window, &feedback, &setup);
    cx.executor().advance_clock(CAPTURE_PICTURE_TIMEOUT * 2);
    cx.run_until_parked();
    assert_eq!(draft(cx, &feedback).attachments.len(), 1);
    assert!(annotating(cx, &feedback).is_some());
    assert!(notices(cx, &feedback).is_empty());
}

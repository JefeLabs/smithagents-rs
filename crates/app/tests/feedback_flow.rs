//! The feedback feature from end to end, through the shell as a person uses
//! it: a 3-second hold on the rail's Feedback button, two boxes drawn and
//! commented in the annotator, Done, a file added, a restart on the same
//! home, the screenshot reopened, and Discard. The capturer is a fake, the
//! frame hops run by hand, the picker answers what the test put in
//! [`FeedbackSetup::picked`], and the draft lives in a temporary home, so
//! nothing reaches the screen, a dialog or the real home. The second test
//! checks that the shell's default options reach none of them either.
//!
//! The window is 1200x800, so the fake's 2400x1512 picture fills the
//! annotator's area below its 44-point bar exactly: a point (x, y) there is
//! the pixel (2x, 2(y - 44)) of the picture.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use common::feedback::{png, run_hop, FeedbackSetup};
use common::ui::open_shell_options;
use gpui_kit::base::{test_support, Root};
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::test::{TestEventExt as _, TestWindowExt as _};
use gpui_kit::{
    point, px, AppContext as _, ElementId, Entity, MouseButton, Pixels, Point, Role,
    TestAppContext, WindowHandle,
};
use smithagents_app::feedback::draft::{AttachmentId, Draft};
use smithagents_app::feedback::render::{decode, BOX_RGB};
use smithagents_app::feedback::state::Feedback;
use smithagents_app::feedback::text::{EMPTY_TEXT, UNSUPPORTED_TEXT};
use smithagents_app::shell::{Shell, ShellOptions};
use smithagents_app::views::feedback::{
    badge_id, card_id, ANNOTATOR_DONE_ID, ANNOTATOR_ID, DISCARD_CONFIRM_ID, FEEDBACK_ADD_FILES_ID,
    FEEDBACK_DISCARD_ID, FEEDBACK_EMPTY_ID, FEEDBACK_LIST_ID, FEEDBACK_NOTICES_ID, HOLD_RING_ID,
};
use smithagents_app::views::rail::RailEntry;

const SCREENSHOT_CARD: &str = "Screenshot 1 · 2 comments";

fn frame(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

/// Runs `f` on the window after a fresh frame.
fn with_window<R>(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App) -> R,
) -> R {
    let mut out = None;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        out = Some(f(window, cx));
    })
    .unwrap();
    out.unwrap()
}

fn click(cx: &mut TestAppContext, window: WindowHandle<Root>, id: impl Into<ElementId>) {
    let id = id.into();
    with_window(cx, window, |window, cx| window.click(id, cx));
    frame(cx, window);
}

fn press_key(cx: &mut TestAppContext, window: WindowHandle<Root>, key: &str) {
    with_window(cx, window, |window, cx| window.press(key, cx));
    frame(cx, window);
}

fn type_text(cx: &mut TestAppContext, window: WindowHandle<Root>, text: &str) {
    with_window(cx, window, |window, cx| window.input(text, cx));
    frame(cx, window);
}

fn at(x: f32, y: f32) -> Point<Pixels> {
    point(px(x), px(y))
}

/// A left-button drag from `from` to `to`, with moves in between.
fn drag(cx: &mut TestAppContext, window: WindowHandle<Root>, from: (f32, f32), to: (f32, f32)) {
    with_window(cx, window, |window, cx| {
        window.drag(at(from.0, from.1), at(to.0, to.1), cx)
    });
    frame(cx, window);
}

/// The center of the rail's Feedback button.
fn feedback_button(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Point<Pixels> {
    with_window(cx, window, |window, _| {
        window
            .find(RailEntry::Feedback.element_id())
            .bounds()
            .center()
    })
}

/// Moves onto the Feedback button and presses the left button there.
fn press_feedback(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Point<Pixels> {
    let on_button = feedback_button(cx, window);
    with_window(cx, window, |window, cx| {
        window.pointer_move(on_button, None, cx);
        window.pointer_down(on_button, MouseButton::Left, cx);
    });
    frame(cx, window);
    on_button
}

/// Releases the left button at `at`.
fn release_at(cx: &mut TestAppContext, window: WindowHandle<Root>, at: Point<Pixels>) {
    with_window(cx, window, |window, cx| {
        window.pointer_up(at, MouseButton::Left, cx)
    });
    frame(cx, window);
}

/// Moves the test clock on by `ms` milliseconds, running what falls due.
fn advance(cx: &mut TestAppContext, window: WindowHandle<Root>, ms: u64) {
    cx.executor().advance_clock(Duration::from_millis(ms));
    frame(cx, window);
}

fn found(cx: &mut TestAppContext, window: WindowHandle<Root>, id: impl Into<ElementId>) -> bool {
    let id = id.into();
    with_window(cx, window, |window, _| window.try_find(id).is_some())
}

fn rail_active(cx: &mut TestAppContext, shell: &Entity<Shell>) -> Option<RailEntry> {
    shell.read_with(cx, |shell, cx| shell.rail_active(cx))
}

fn dock_open(cx: &mut TestAppContext, shell: &Entity<Shell>) -> bool {
    let dock = shell.read_with(cx, |shell, _| shell.dock().clone());
    dock.read_with(cx, |dock, _| dock.is_dock_open(DockPlacement::Right))
}

fn annotator_open(cx: &mut TestAppContext, shell: &Entity<Shell>) -> bool {
    shell.read_with(cx, |shell, _| shell.annotator().is_some())
}

fn draft(cx: &TestAppContext, feedback: &Entity<Feedback>) -> Draft {
    cx.read(|cx| feedback.read(cx).draft().clone())
}

/// The accessible name of the element `id`, or `None` when it is not drawn.
fn label_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    id: impl Into<ElementId>,
) -> Option<String> {
    let id = id.into();
    with_window(cx, window, |window, _| {
        window
            .try_find(id)
            .map(|element| element.label().unwrap_or_default().to_owned())
    })
}

/// The notices block's name, after checking that it is a status.
fn notices(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Option<String> {
    with_window(cx, window, |window, _| {
        window.try_find(FEEDBACK_NOTICES_ID).map(|notices| {
            assert_eq!(notices.role(), Some(Role::Status));
            notices.label().unwrap_or_default().to_owned()
        })
    })
}

/// The cards' names, top to bottom (none when the list is not drawn).
fn cards(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Vec<String> {
    with_window(cx, window, |window, _| {
        if window.try_find(FEEDBACK_LIST_ID).is_none() {
            return Vec::new();
        }
        window
            .within(FEEDBACK_LIST_ID)
            .elements()
            .into_iter()
            .filter(|element| {
                matches!(element.path().last(), Some(ElementId::Name(name))
                    if name.starts_with("feedback-card-")
                        && !name.starts_with("feedback-card-remove-"))
            })
            .map(|card| card.label().unwrap_or_default().to_owned())
            .collect()
    })
}

/// The text painted inside the card of `id`.
fn card_texts(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    id: &AttachmentId,
) -> Vec<String> {
    let card = card_id(id);
    with_window(cx, window, |window, _| {
        test_support::texts(window)
            .into_iter()
            .filter(|text| text.path().contains(&card))
            .map(|text| text.text().to_owned())
            .collect()
    })
}

/// Checks the pane: the screenshot's card with both comments, then each of
/// `files`' cards, and no notice.
#[track_caller]
fn assert_cards(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    shot: &AttachmentId,
    files: &[&str],
) {
    let expected: Vec<&str> = std::iter::once(SCREENSHOT_CARD)
        .chain(files.iter().copied())
        .collect();
    assert_eq!(cards(cx, window), expected);
    let texts = card_texts(cx, window, shot);
    for comment in ["the toolbar", "this label"] {
        assert!(
            texts.iter().any(|text| text == comment),
            "{comment:?} in {texts:?}"
        );
    }
    assert_eq!(notices(cx, window), None);
}

/// Every file under `dir`, as paths relative to it with `/` between parts,
/// sorted.
fn files_under(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                let parts: Vec<_> = relative
                    .iter()
                    .map(|part| part.to_string_lossy().into_owned())
                    .collect();
                out.push(parts.join("/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn rgb_at(image: &image::RgbaImage, x: u32, y: u32) -> [u8; 3] {
    let p = image.get_pixel(x, y).0;
    [p[0], p[1], p[2]]
}

/// Closes the window, so a second shell can open on the same home.
fn close(cx: &mut TestAppContext, window: WindowHandle<Root>, shell: Entity<Shell>) {
    drop(shell);
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn the_whole_path_from_hold_to_a_restored_draft(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, feedback) = setup.open(cx);
    frame(cx, window);
    let before = (rail_active(cx, &shell), dock_open(cx, &shell));

    // 1. A 3-second hold takes the picture and opens the annotator.
    let on_button = press_feedback(cx, window);
    advance(cx, window, 3010);
    run_hop(cx, window, &setup);
    run_hop(cx, window, &setup);
    frame(cx, window);
    release_at(cx, window, on_button);
    assert_eq!(setup.capturer.calls().len(), 1);
    assert!(found(cx, window, ANNOTATOR_ID), "the annotator is open");
    assert!(!found(cx, window, HOLD_RING_ID), "the ring is gone");
    assert_eq!(
        (rail_active(cx, &shell), dock_open(cx, &shell)),
        before,
        "the dock did not toggle"
    );

    // 2. Two boxes, each with its comment.
    drag(cx, window, (300., 233.), (600., 422.));
    type_text(cx, window, "the toolbar");
    press_key(cx, window, "enter");
    drag(cx, window, (700., 500.), (900., 650.));
    type_text(cx, window, "this label");
    press_key(cx, window, "enter");
    assert!(found(cx, window, badge_id(1)));
    assert!(found(cx, window, badge_id(2)));

    // 3. Done lands on the Feedback pane, on the screenshot's card.
    click(cx, window, ANNOTATOR_DONE_ID);
    assert!(!found(cx, window, ANNOTATOR_ID), "the annotator is gone");
    assert!(!annotator_open(cx, &shell));
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    let shot = draft(cx, &feedback).attachments[0].id.clone();
    assert_cards(cx, window, &shot, &[]);

    // 4. On disk: the draft and the screenshot's three pictures, nothing else.
    let dir = setup.draft_dir();
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("draft.json")).unwrap()).unwrap();
    assert_eq!(json["version"], 1);
    let attachments = json["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0]["kind"], "screenshot");
    assert_eq!(attachments[0]["boxes"].as_array().unwrap().len(), 2);
    assert_eq!(attachments[0]["rendered"], true);
    assert!(dir.join("screenshot-1.png").is_file());
    let annotated = decode(&fs::read(dir.join("screenshot-1.annotated.png")).unwrap()).unwrap();
    assert_eq!(annotated.dimensions(), (2400, 1512));
    assert_eq!(rgb_at(&annotated, 600, 378), BOX_RGB, "the first box");
    assert_eq!(rgb_at(&annotated, 1400, 912), BOX_RGB, "the second box");
    let thumb = decode(&fs::read(dir.join("screenshot-1.thumb.png")).unwrap()).unwrap();
    assert_eq!(thumb.dimensions(), (320, 202));
    assert_eq!(
        files_under(setup.home.path()),
        [
            "feedback/draft/draft.json",
            "feedback/draft/screenshot-1.annotated.png",
            "feedback/draft/screenshot-1.png",
            "feedback/draft/screenshot-1.thumb.png",
        ]
    );

    // 5. A file joins the screenshot.
    setup.pick(vec![setup.picked_file("good.png", &png(64, 48))]);
    click(cx, window, FEEDBACK_ADD_FILES_ID);
    assert_cards(cx, window, &shot, &["good.png"]);

    // 6. A restart on the same home finds the same draft.
    close(cx, window, shell);
    let (window, shell, feedback) = setup.open(cx);
    frame(cx, window);
    click(cx, window, RailEntry::Feedback.element_id());
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    assert_cards(cx, window, &shot, &["good.png"]);
    assert_eq!(setup.capturer.calls().len(), 1, "no new capture");

    // 7. The screenshot reopens with its boxes; Escape with no comment open
    // changes nothing.
    click(cx, window, card_id(&shot));
    assert!(found(cx, window, ANNOTATOR_ID));
    assert!(found(cx, window, badge_id(1)));
    assert!(found(cx, window, badge_id(2)));
    let reopened = draft(cx, &feedback);
    press_key(cx, window, "escape");
    assert!(found(cx, window, ANNOTATOR_ID), "still open");
    assert!(found(cx, window, badge_id(1)));
    assert!(found(cx, window, badge_id(2)));
    assert_eq!(draft(cx, &feedback), reopened);
    click(cx, window, ANNOTATOR_DONE_ID);
    assert!(!annotator_open(cx, &shell));
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    assert_cards(cx, window, &shot, &["good.png"]);

    // 8. Discard empties the pane and deletes the draft's directory.
    click(cx, window, FEEDBACK_DISCARD_ID);
    click(cx, window, DISCARD_CONFIRM_ID);
    assert_eq!(
        label_of(cx, window, FEEDBACK_EMPTY_ID).as_deref(),
        Some(EMPTY_TEXT)
    );
    assert_eq!(cards(cx, window), Vec::<String>::new());
    assert!(!setup.home.path().join("feedback").join("draft").exists());
}

#[gpui_kit::test]
fn the_default_options_never_reach_the_screen_or_a_dialog(cx: &mut TestAppContext) {
    let (window, shell) = open_shell_options(cx, ShellOptions::default(), (1200., 800.));
    frame(cx, window);
    let feedback = shell.read_with(cx, |shell, _| shell.feedback().clone());

    // A full hold. The default frame hop is the window's next frame, which
    // the test platform runs only when asked.
    let on_button = press_feedback(cx, window);
    advance(cx, window, 3010);
    for _ in 0..2 {
        cx.update_window(window.into(), |_, window, cx| {
            window.simulate_next_frame(cx)
        })
        .unwrap();
        frame(cx, window);
    }
    release_at(cx, window, on_button);
    assert!(!annotator_open(cx, &shell));
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    assert_eq!(notices(cx, window).as_deref(), Some(UNSUPPORTED_TEXT));

    // "Add files…" asks no one and adds nothing.
    click(cx, window, FEEDBACK_ADD_FILES_ID);
    assert!(!cx.did_prompt_for_paths(), "no file dialog");
    assert!(draft(cx, &feedback).is_empty());
    assert_eq!(cards(cx, window), Vec::<String>::new());
    assert_eq!(notices(cx, window).as_deref(), Some(UNSUPPORTED_TEXT));
    assert_eq!(
        label_of(cx, window, FEEDBACK_EMPTY_ID).as_deref(),
        Some(EMPTY_TEXT)
    );
}

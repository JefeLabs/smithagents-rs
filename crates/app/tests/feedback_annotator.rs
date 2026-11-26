//! The annotator: a full-window overlay showing the screenshot, where a drag
//! draws a box and a comment is typed for it. The window is 1200x800, so the
//! fake's 2400x1512 picture fills the area below the 44-point bar exactly
//! (x 0..1200, y 44..800): a point (x, y) there is the fraction
//! (x / 1200, (y - 44) / 756) of the picture. Captures are taken without the
//! hold (the fake capturer, frame hops run by hand), drags and keys go
//! through the test window's events, and the draft lives in a temporary
//! home, so nothing reaches the screen, a dialog or the real home.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::sync::Arc;

use common::feedback::{act, capture_now, FeedbackSetup};
use common::ui::{dark_native, open_shell_options, painted_at, token_background};
use gpui_kit::base::{test_support, Root};
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::{ElementSnapshot, TestWindowExt as _};
use gpui_kit::{
    point, px, size, AppContext as _, Bounds, ElementId, Entity, Pixels, Point, RenderImage, Role,
    TestAppContext, WindowHandle,
};
use smithagents_app::feedback::annotation::{Annotation, FracRect};
use smithagents_app::feedback::draft::{AttachmentId, Draft};
use smithagents_app::feedback::state::Feedback;
use smithagents_app::shell::Shell;
use smithagents_app::views::feedback::{
    badge_id, card_id, Annotator, ANNOTATOR_COMMENT_ERROR_ID, ANNOTATOR_COMMENT_ID,
    ANNOTATOR_CONFIRM_DISCARD_ID, ANNOTATOR_DELETE_ID, ANNOTATOR_DISCARD_ID,
    ANNOTATOR_DISCARD_TITLE_ID, ANNOTATOR_DONE_ID, ANNOTATOR_ID, ANNOTATOR_KEEP_ID,
    ANNOTATOR_TITLE_ID,
};
use smithagents_app::views::rail::RailEntry;
use smithagents_capture::test_pattern;

const FIRST_TITLE: &str = "Screenshot 1 of 1 · Drag a box over what you mean";

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

fn press(cx: &mut TestAppContext, window: WindowHandle<Root>, key: &str) {
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

/// Draws a box from `from` to `to`, types `comment` and saves it with Enter.
fn comment_box(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    from: (f32, f32),
    to: (f32, f32),
    comment: &str,
) {
    drag(cx, window, from, to);
    type_text(cx, window, comment);
    press(cx, window, "enter");
}

/// Opens the shell on `setup` in a 1200x800 window and captures without the
/// hold: the annotator is open on the new screenshot, its picture decoded.
fn open(
    cx: &mut TestAppContext,
    setup: &FeedbackSetup,
) -> (WindowHandle<Root>, Entity<Shell>, Entity<Feedback>) {
    let (window, shell, feedback) = setup.open(cx);
    frame(cx, window);
    capture_now(cx, window, &feedback, setup);
    frame(cx, window);
    (window, shell, feedback)
}

/// Closes the window, so a second shell can open on the same home.
fn close(cx: &mut TestAppContext, window: WindowHandle<Root>, shell: Entity<Shell>) {
    drop(shell);
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
}

fn annotator(cx: &mut TestAppContext, shell: &Entity<Shell>) -> Option<Entity<Annotator>> {
    shell.read_with(cx, |shell, _| shell.annotator().cloned())
}

fn boxes(cx: &mut TestAppContext, shell: &Entity<Shell>) -> Vec<Annotation> {
    annotator(cx, shell)
        .expect("the annotator is open")
        .read_with(cx, |annotator, _| annotator.boxes().to_vec())
}

fn image(cx: &mut TestAppContext, shell: &Entity<Shell>) -> Option<Arc<RenderImage>> {
    annotator(cx, shell)
        .expect("the annotator is open")
        .read_with(cx, |annotator, _| annotator.image())
}

fn draft(cx: &TestAppContext, feedback: &Entity<Feedback>) -> Draft {
    cx.read(|cx| feedback.read(cx).draft().clone())
}

fn annotating(cx: &TestAppContext, feedback: &Entity<Feedback>) -> AttachmentId {
    cx.read(|cx| feedback.read(cx).annotating().cloned())
        .expect("the feedback is being annotated")
}

fn rail_active(cx: &mut TestAppContext, shell: &Entity<Shell>) -> Option<RailEntry> {
    shell.read_with(cx, |shell, cx| shell.rail_active(cx))
}

fn dialog_open(cx: &mut TestAppContext, window: WindowHandle<Root>) -> bool {
    with_window(cx, window, |window, cx| window.has_active_dialog(cx))
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

fn title(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Option<String> {
    label_of(cx, window, ANNOTATOR_TITLE_ID)
}

/// The comment's text field (the element with a placeholder inside
/// `ANNOTATOR_COMMENT_ID`), or `None` while no comment is open.
fn comment_field(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Option<ElementSnapshot> {
    with_window(cx, window, |window, _| {
        window.try_find(ANNOTATOR_COMMENT_ID)?;
        let field = window
            .within(ANNOTATOR_COMMENT_ID)
            .elements()
            .into_iter()
            .find(|element| element.placeholder().is_some());
        Some(field.expect("the comment has a text field"))
    })
}

fn bounds_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    id: impl Into<ElementId>,
) -> Bounds<Pixels> {
    let id = id.into();
    with_window(cx, window, |window, _| window.find(id).bounds())
}

#[track_caller]
fn assert_near(actual: Point<Pixels>, expected: (f32, f32), tolerance: f32) {
    let (x, y) = (f32::from(actual.x), f32::from(actual.y));
    assert!(
        (x - expected.0).abs() <= tolerance && (y - expected.1).abs() <= tolerance,
        "{actual:?} is not within {tolerance} of {expected:?}"
    );
}

#[track_caller]
fn assert_rect(rect: FracRect, expected: [f32; 4]) {
    let actual = [rect.x, rect.y, rect.w, rect.h];
    assert!(
        actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| (actual - expected).abs() <= 1e-4),
        "{actual:?} is not within 1e-4 of {expected:?}"
    );
}

fn note(rect: [f32; 4], comment: &str) -> Annotation {
    Annotation {
        rect: FracRect {
            x: rect[0],
            y: rect[1],
            w: rect[2],
            h: rect[3],
        },
        comment: comment.to_string(),
    }
}

#[gpui_kit::test]
fn the_annotator_covers_the_whole_window(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, feedback) = setup.open(cx);
    frame(cx, window);
    // Feedback on screen, so a click that reached the rail would open Settings.
    click(cx, window, RailEntry::Feedback.element_id());
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    capture_now(cx, window, &feedback, &setup);
    frame(cx, window);

    let bounds = bounds_of(cx, window, ANNOTATOR_ID);
    assert_eq!(bounds.origin, at(0., 0.));
    assert_eq!(bounds.size, size(px(1200.), px(800.)));
    assert_eq!(title(cx, window).as_deref(), Some(FIRST_TITLE));

    click(cx, window, RailEntry::Settings.element_id());
    assert_eq!(rail_active(cx, &shell), Some(RailEntry::Feedback));
    assert!(annotator(cx, &shell).is_some());

    let image = image(cx, &shell).expect("the picture is decoded");
    let drawn = image.size(0);
    assert_eq!((drawn.width.0, drawn.height.0), (2400, 1512));
}

#[gpui_kit::test]
fn dragging_draws_a_box_in_image_fractions_and_enter_saves_it(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, _) = open(cx, &setup);

    drag(cx, window, (300., 233.), (600., 422.));
    let field = comment_field(cx, window).expect("the comment field opens");
    assert_eq!(field.placeholder(), Some("Enter saves · Esc drops"));
    assert_eq!(field.focused(), Some(true));

    type_text(cx, window, "the toolbar");
    press(cx, window, "enter");

    let saved = boxes(cx, &shell);
    assert_eq!(saved.len(), 1);
    assert_rect(saved[0].rect, [0.25, 0.25, 0.25, 0.25]);
    assert_eq!(saved[0].comment, "the toolbar");
    assert_near(bounds_of(cx, window, badge_id(1)).origin, (300., 233.), 1.);
    assert_eq!(label_of(cx, window, badge_id(1)).as_deref(), Some("1"));
    assert!(comment_field(cx, window).is_none(), "the field is gone");
    assert_eq!(setup.store().load().draft.attachments[0].boxes, saved);
}

#[gpui_kit::test]
fn a_letterboxed_image_clamps_the_drag_to_the_picture(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    // Drawn 1200x400 at y 222 in the 1200x756 area.
    setup.capturer.set(Ok(test_pattern(2400, 800)));
    let (window, shell, _) = open(cx, &setup);

    // Starts above the picture.
    comment_box(cx, window, (300., 100.), (600., 322.), "the header");

    let saved = boxes(cx, &shell);
    assert_eq!(saved.len(), 1);
    assert_rect(saved[0].rect, [0.25, 0.0, 0.25, 0.25]);
    assert_near(bounds_of(cx, window, badge_id(1)).origin, (300., 222.), 1.);
}

#[gpui_kit::test]
fn a_drag_smaller_than_eight_points_is_ignored(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, _) = open(cx, &setup);

    drag(cx, window, (300., 233.), (307., 300.));
    assert!(comment_field(cx, window).is_none());
    assert!(boxes(cx, &shell).is_empty());

    drag(cx, window, (300., 233.), (308., 241.));
    assert!(comment_field(cx, window).is_some(), "8x8 is big enough");
}

#[gpui_kit::test]
fn escape_drops_a_new_box_and_an_empty_comment_drops_it_too(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, _) = open(cx, &setup);

    drag(cx, window, (300., 233.), (600., 422.));
    type_text(cx, window, "never mind");
    press(cx, window, "escape");
    assert!(comment_field(cx, window).is_none());
    assert!(boxes(cx, &shell).is_empty());

    drag(cx, window, (300., 233.), (600., 422.));
    press(cx, window, "enter");
    assert!(comment_field(cx, window).is_none());
    assert!(boxes(cx, &shell).is_empty());

    drag(cx, window, (300., 233.), (600., 422.));
    type_text(cx, window, "   ");
    press(cx, window, "enter");
    assert!(comment_field(cx, window).is_none());
    assert!(boxes(cx, &shell).is_empty());
    assert!(setup.store().load().draft.attachments[0].boxes.is_empty());

    // With no comment open, Escape does nothing.
    press(cx, window, "escape");
    assert!(annotator(cx, &shell).is_some());
}

#[gpui_kit::test]
fn shift_enter_adds_a_line_and_enter_saves_both(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, _) = open(cx, &setup);

    drag(cx, window, (300., 233.), (600., 422.));
    type_text(cx, window, "the");
    press(cx, window, "shift-enter");
    assert!(comment_field(cx, window).is_some(), "still open");
    type_text(cx, window, "toolbar");
    press(cx, window, "enter");

    let saved = boxes(cx, &shell);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].comment, "the\ntoolbar");
}

#[gpui_kit::test]
fn a_press_while_a_comment_is_open_is_ignored(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, _) = open(cx, &setup);

    drag(cx, window, (300., 233.), (600., 422.));
    drag(cx, window, (700., 500.), (900., 650.));
    let field = comment_field(cx, window).expect("the first comment is still open");
    assert_eq!(field.focused(), Some(true));
    type_text(cx, window, "the toolbar");
    press(cx, window, "enter");

    let saved = boxes(cx, &shell);
    assert_eq!(saved.len(), 1);
    assert_rect(saved[0].rect, [0.25, 0.25, 0.25, 0.25]);
}

#[gpui_kit::test]
fn clicking_a_badge_edits_its_comment(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, _) = open(cx, &setup);
    comment_box(cx, window, (300., 233.), (600., 422.), "the toolbar");

    click(cx, window, badge_id(1));
    let field = comment_field(cx, window).expect("the comment opens");
    assert_eq!(field.value(), Some("the toolbar"));
    assert_eq!(field.focused(), Some(true));
    press(cx, window, "cmd-a");
    type_text(cx, window, "the sidebar");
    press(cx, window, "enter");
    let saved = boxes(cx, &shell);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].comment, "the sidebar");
    assert_rect(saved[0].rect, [0.25, 0.25, 0.25, 0.25]);
    assert_eq!(setup.store().load().draft.attachments[0].boxes, saved);

    click(cx, window, badge_id(1));
    type_text(cx, window, "x");
    press(cx, window, "escape");
    assert!(comment_field(cx, window).is_none());
    assert_eq!(boxes(cx, &shell)[0].comment, "the sidebar");
}

#[gpui_kit::test]
fn delete_removes_the_box_and_renumbers(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, _) = open(cx, &setup);
    comment_box(cx, window, (100., 100.), (200., 200.), "one");
    comment_box(cx, window, (300., 300.), (400., 400.), "two");
    comment_box(cx, window, (500., 500.), (600., 600.), "three");
    assert_eq!(boxes(cx, &shell).len(), 3);

    click(cx, window, badge_id(2));
    assert_eq!(
        label_of(cx, window, ANNOTATOR_DELETE_ID).as_deref(),
        Some("Delete")
    );
    click(cx, window, ANNOTATOR_DELETE_ID);

    let comments: Vec<String> = boxes(cx, &shell)
        .into_iter()
        .map(|annotation| annotation.comment)
        .collect();
    assert_eq!(comments, ["one", "three"]);
    assert!(comment_field(cx, window).is_none());
    assert_near(bounds_of(cx, window, badge_id(2)).origin, (500., 500.), 1.);
    assert_eq!(label_of(cx, window, badge_id(3)), None);
    assert_eq!(setup.store().load().draft.attachments[0].boxes.len(), 2);
}

#[gpui_kit::test]
fn the_twenty_first_box_is_refused(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, feedback) = open(cx, &setup);
    let id = annotating(cx, &feedback);
    // A row of small boxes along the top, away from the drag below.
    let twenty: Vec<Annotation> = (0..20)
        .map(|i| note([0.04 * i as f32, 0.0, 0.03, 0.03], &format!("box {i}")))
        .collect();
    act(cx, window, &feedback, |f, _, cx| {
        f.set_boxes(&id, twenty.clone(), cx)
    });
    act(cx, window, &feedback, |f, _, cx| f.finish_annotating(cx));
    frame(cx, window);
    assert!(annotator(cx, &shell).is_none());
    act(cx, window, &feedback, |f, _, cx| f.open_annotator(&id, cx));
    frame(cx, window);
    assert_eq!(boxes(cx, &shell), twenty);

    drag(cx, window, (600., 600.), (700., 700.));
    assert!(comment_field(cx, window).is_none());
    assert_eq!(
        title(cx, window).as_deref(),
        Some("A screenshot can have at most 20 boxes.")
    );
    assert_eq!(boxes(cx, &shell), twenty);
}

#[gpui_kit::test]
fn a_comment_over_500_characters_is_not_saved(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, _) = open(cx, &setup);

    drag(cx, window, (300., 233.), (600., 422.));
    type_text(cx, window, &"a".repeat(501));
    press(cx, window, "enter");
    with_window(cx, window, |window, _| {
        let error = window.find(ANNOTATOR_COMMENT_ERROR_ID);
        assert_eq!(error.role(), Some(Role::Status));
        assert_eq!(
            error.label(),
            Some("A comment can be at most 500 characters.")
        );
    });
    assert!(comment_field(cx, window).is_some(), "still open");
    assert!(boxes(cx, &shell).is_empty());

    press(cx, window, "backspace");
    press(cx, window, "enter");
    let saved = boxes(cx, &shell);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].comment.chars().count(), 500);
    assert!(comment_field(cx, window).is_none());
    assert_eq!(label_of(cx, window, ANNOTATOR_COMMENT_ERROR_ID), None);
}

#[gpui_kit::test]
fn boxes_keep_their_place_in_a_window_of_another_size(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, _) = open(cx, &setup);
    comment_box(cx, window, (300., 233.), (600., 422.), "the toolbar");
    let saved = boxes(cx, &shell);
    click(cx, window, ANNOTATOR_DONE_ID);
    frame(cx, window);
    let id = setup.store().load().draft.attachments[0].id.clone();
    close(cx, window, shell);

    // The image area is 900x567, below the bar.
    let (window, shell) = open_shell_options(cx, setup.options(), (900., 611.));
    frame(cx, window);
    click(cx, window, RailEntry::Feedback.element_id());
    click(cx, window, card_id(&id));
    frame(cx, window);

    assert_near(bounds_of(cx, window, badge_id(1)).origin, (225., 186.), 1.);
    assert_eq!(boxes(cx, &shell), saved);
}

#[gpui_kit::test]
fn a_resized_window_keeps_boxes_on_the_picture(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, _) = open(cx, &setup);
    comment_box(cx, window, (300., 233.), (600., 422.), "the toolbar");

    // The image area becomes 900x567; the badge follows the box.
    cx.simulate_window_resize(window.into(), size(px(900.), px(611.)));
    frame(cx, window);
    assert_near(bounds_of(cx, window, badge_id(1)).origin, (225., 186.), 1.);

    // A drag at the new size is converted through the new picture.
    comment_box(cx, window, (450., 327.5), (675., 469.25), "the label");
    let saved = boxes(cx, &shell);
    assert_eq!(saved.len(), 2);
    assert_rect(saved[0].rect, [0.25, 0.25, 0.25, 0.25]);
    assert_rect(saved[1].rect, [0.5, 0.5, 0.25, 0.25]);
}

#[gpui_kit::test]
fn boxes_and_badges_use_the_destructive_token(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, _) = open(cx, &setup);
    comment_box(cx, window, (300., 233.), (600., 422.), "the toolbar");
    let destructive = token_background(&dark_native(), "destructive");

    with_window(cx, window, |window, _| {
        let badge = window.find(badge_id(1)).bounds();
        assert_eq!(painted_at(window, badge.center()), destructive);
        // The box's outline: borders are painted as quads of the box's
        // bounds, filled with the border's color and masked to their edge.
        let scale = window.scale_factor();
        let expected = [300. * scale, 233. * scale, 300. * scale, 189. * scale];
        let outlines = window
            .painted_quads()
            .into_iter()
            .filter(|quad| {
                let bounds = [
                    quad.bounds.origin.x.0,
                    quad.bounds.origin.y.0,
                    quad.bounds.size.width.0,
                    quad.bounds.size.height.0,
                ];
                quad.background == destructive
                    && bounds
                        .iter()
                        .zip(expected)
                        .all(|(actual, expected)| (actual - expected).abs() <= scale)
            })
            .count();
        assert_eq!(
            outlines, 4,
            "the box's four edges, in the destructive token"
        );
    });
}

#[gpui_kit::test]
fn done_returns_to_the_pane_and_releases_the_image(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, _) = open(cx, &setup);
    comment_box(cx, window, (300., 233.), (600., 422.), "the toolbar");
    let image = image(cx, &shell).expect("the picture is decoded");
    let weak = Arc::downgrade(&image);
    assert!(with_window(cx, window, |window, _| window.has_image_atlas_entry(&image)));

    click(cx, window, ANNOTATOR_DONE_ID);
    frame(cx, window);

    assert!(annotator(cx, &shell).is_none());
    assert!(
        !with_window(cx, window, |window, _| window.has_image_atlas_entry(&image)),
        "the picture left the window's atlas"
    );
    drop(image);
    assert!(weak.upgrade().is_none(), "nothing holds the picture");
    assert!(setup
        .draft_dir()
        .join("screenshot-1.annotated.png")
        .is_file());
    // The focus is back in the shell (on the Feedback pane): its shortcut works.
    let before = rail_active(cx, &shell);
    press(cx, window, "cmd-alt-f");
    assert_ne!(rail_active(cx, &shell), before);
}

#[gpui_kit::test]
fn done_saves_a_comment_still_being_typed(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, feedback) = open(cx, &setup);

    drag(cx, window, (300., 233.), (600., 422.));
    type_text(cx, window, "the toolbar");
    click(cx, window, ANNOTATOR_DONE_ID);
    frame(cx, window);

    assert!(annotator(cx, &shell).is_none());
    let shot = &draft(cx, &feedback).attachments[0];
    assert_eq!(shot.boxes.len(), 1);
    assert_eq!(shot.boxes[0].comment, "the toolbar");
}

#[gpui_kit::test]
fn removing_the_screenshot_underneath_closes_and_releases(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, feedback) = open(cx, &setup);
    let id = annotating(cx, &feedback);
    let image = image(cx, &shell).expect("the picture is decoded");
    let weak = Arc::downgrade(&image);

    act(cx, window, &feedback, |f, _, cx| f.remove(&id, cx));
    frame(cx, window);

    assert!(annotator(cx, &shell).is_none());
    assert!(!with_window(cx, window, |window, _| window.has_image_atlas_entry(&image)));
    drop(image);
    assert!(weak.upgrade().is_none());
}

#[gpui_kit::test]
fn discard_without_boxes_is_immediate_and_with_boxes_asks(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, feedback) = open(cx, &setup);
    let image = image(cx, &shell).expect("the picture is decoded");

    click(cx, window, ANNOTATOR_DISCARD_ID);
    assert!(!dialog_open(cx, window), "nothing to ask about");
    assert!(annotator(cx, &shell).is_none());
    assert!(draft(cx, &feedback).is_empty());
    assert!(!with_window(cx, window, |window, _| window.has_image_atlas_entry(&image)));

    capture_now(cx, window, &feedback, &setup);
    frame(cx, window);
    comment_box(cx, window, (300., 233.), (600., 422.), "the toolbar");
    let dir = setup.draft_dir();
    assert!(dir.join("screenshot-1.png").is_file());

    click(cx, window, ANNOTATOR_DISCARD_ID);
    assert!(dialog_open(cx, window));
    with_window(cx, window, |window, _| {
        assert_eq!(
            window.find(ANNOTATOR_DISCARD_TITLE_ID).label(),
            Some("Discard this screenshot?")
        );
        assert!(
            test_support::texts(window)
                .iter()
                .any(|text| text.text() == "Its 1 comment is deleted."),
            "the dialog says what goes"
        );
        assert_eq!(window.find(ANNOTATOR_KEEP_ID).focused(), Some(true));
        assert_eq!(window.find(ANNOTATOR_KEEP_ID).label(), Some("Keep"));
        assert_eq!(
            window.find(ANNOTATOR_CONFIRM_DISCARD_ID).label(),
            Some("Discard")
        );
    });
    click(cx, window, ANNOTATOR_KEEP_ID);
    assert!(!dialog_open(cx, window));
    assert!(annotator(cx, &shell).is_some());
    assert_eq!(boxes(cx, &shell).len(), 1);
    assert_eq!(draft(cx, &feedback).attachments.len(), 1);
    assert!(dir.join("screenshot-1.png").is_file());

    click(cx, window, ANNOTATOR_DISCARD_ID);
    click(cx, window, ANNOTATOR_CONFIRM_DISCARD_ID);
    assert!(!dialog_open(cx, window));
    assert!(annotator(cx, &shell).is_none());
    assert!(draft(cx, &feedback).is_empty());
    assert!(!dir.join("screenshot-1.png").exists());
    assert!(setup.store().load().draft.is_empty());
}

#[gpui_kit::test]
fn the_second_screenshot_says_two_of_two(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, feedback) = open(cx, &setup);
    let first = annotating(cx, &feedback);
    comment_box(cx, window, (300., 233.), (600., 422.), "the toolbar");
    click(cx, window, ANNOTATOR_DONE_ID);
    frame(cx, window);

    capture_now(cx, window, &feedback, &setup);
    frame(cx, window);
    assert_eq!(
        title(cx, window).as_deref(),
        Some("Screenshot 2 of 2 · Drag a box over what you mean")
    );
    assert_eq!(label_of(cx, window, badge_id(1)), None);
    click(cx, window, ANNOTATOR_DONE_ID);
    frame(cx, window);

    assert_eq!(
        rail_active(cx, &shell),
        Some(RailEntry::Feedback),
        "Done shows the pane"
    );
    click(cx, window, card_id(&first));
    frame(cx, window);
    assert!(annotator(cx, &shell).is_some());
    assert_eq!(
        title(cx, window).as_deref(),
        Some("Screenshot 1 of 2 · Drag a box over what you mean")
    );
    assert_near(bounds_of(cx, window, badge_id(1)).origin, (300., 233.), 1.);
}

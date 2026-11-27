//! The Feedback pane in the right dock: its help row, the notices, one card
//! per attachment, and the Add files, Capture, Discard and Submit buttons.
//! The pane is opened by clicking the rail entry. The capturer is a fake and
//! its frame hops run by hand, the picker answers what the test put in
//! [`FeedbackSetup::picked`], and the draft lives in a temporary home, so
//! nothing reaches the screen, a dialog or the real home.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use common::feedback::{act, capture_now, png, run_hop, FeedbackSetup};
use common::ui::{dark_native, painted_at, token_background};
use gpui_kit::base::{test_support, Root};
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, ElementId, Entity, RenderImage, Role, TestAppContext, WindowHandle,
};
use smithagents_app::feedback::annotation::{Annotation, FracRect};
use smithagents_app::feedback::draft::{AttachmentId, Draft};
use smithagents_app::feedback::files::Refusal;
use smithagents_app::feedback::state::Feedback;
use smithagents_app::feedback::text::{
    discard_description, BROKEN_DRAFT_TEXT, EMPTY_TEXT, HELP_LINES, HELP_TITLE, PERMISSION_TEXT,
};
use smithagents_app::shell::{Shell, FEEDBACK_PANEL};
use smithagents_app::view_models::NOT_CONNECTED_TEXT;
use smithagents_app::views::feedback::{
    card_id, card_remove_id, comment_badge_id, DISCARD_CANCEL_ID, DISCARD_CONFIRM_ID,
    DISCARD_DIALOG_TITLE_ID, FEEDBACK_ADD_FILES_ID, FEEDBACK_CAPTURE_ID, FEEDBACK_DISCARD_ID,
    FEEDBACK_EMPTY_ID, FEEDBACK_HELP_ID, FEEDBACK_HELP_TEXT_ID, FEEDBACK_LIST_ID,
    FEEDBACK_NOTICES_ID, FEEDBACK_SUBMIT_ID, FEEDBACK_TITLE_ID,
};
use smithagents_app::views::rail::RailEntry;
use smithagents_capture::CaptureError;

const MP4_START: &[u8] = &[
    0, 0, 0, 0x18, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm',
];

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

/// Opens the shell on `setup` and shows the Feedback pane with a click on
/// the rail entry.
fn open(
    cx: &mut TestAppContext,
    setup: &FeedbackSetup,
) -> (WindowHandle<Root>, Entity<Shell>, Entity<Feedback>) {
    let (window, shell, feedback) = setup.open(cx);
    frame(cx, window);
    click(cx, window, RailEntry::Feedback.element_id());
    assert_eq!(
        shell.read_with(cx, |shell, cx| shell.rail_active(cx)),
        Some(RailEntry::Feedback)
    );
    (window, shell, feedback)
}

/// Closes the window, so a second shell can open on the same home.
fn close(cx: &mut TestAppContext, window: WindowHandle<Root>, shell: Entity<Shell>) {
    drop(shell);
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
}

fn draft(cx: &TestAppContext, feedback: &Entity<Feedback>) -> Draft {
    cx.read(|cx| feedback.read(cx).draft().clone())
}

fn dock_open(cx: &mut TestAppContext, shell: &Entity<Shell>) -> bool {
    let dock = shell.read_with(cx, |shell, _| shell.dock().clone());
    dock.read_with(cx, |dock, _| dock.is_dock_open(DockPlacement::Right))
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

fn note(comment: &str) -> Annotation {
    Annotation {
        rect: FracRect {
            x: 0.25,
            y: 0.25,
            w: 0.25,
            h: 0.25,
        },
        comment: comment.to_string(),
    }
}

/// Captures (without the hold), saves `comments` as boxes and presses Done:
/// the screenshot's id.
fn screenshot(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    feedback: &Entity<Feedback>,
    setup: &FeedbackSetup,
    comments: &[&str],
) -> AttachmentId {
    capture_now(cx, window, feedback, setup);
    let id = cx
        .read(|cx| feedback.read(cx).annotating().cloned())
        .expect("the capture opened the annotator");
    let boxes: Vec<Annotation> = comments.iter().map(|comment| note(comment)).collect();
    act(cx, window, feedback, |f, _, cx| f.set_boxes(&id, boxes, cx));
    act(cx, window, feedback, |f, _, cx| f.finish_annotating(cx));
    frame(cx, window);
    id
}

/// The picker answers `paths`; then "Add files…".
fn add_files(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    setup: &FeedbackSetup,
    paths: Vec<PathBuf>,
) {
    setup.pick(paths);
    click(cx, window, FEEDBACK_ADD_FILES_ID);
}

fn in_atlas(cx: &mut TestAppContext, window: WindowHandle<Root>, image: &Arc<RenderImage>) -> bool {
    with_window(cx, window, |window, _| window.has_image_atlas_entry(image))
}

#[gpui_kit::test]
fn an_empty_pane_says_how_to_start(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, _) = open(cx, &setup);

    assert_eq!(
        label_of(cx, window, FEEDBACK_TITLE_ID).as_deref(),
        Some("FEEDBACK")
    );
    assert_eq!(
        label_of(cx, window, FEEDBACK_EMPTY_ID).as_deref(),
        Some(EMPTY_TEXT)
    );
    assert_eq!(label_of(cx, window, FEEDBACK_LIST_ID), None);
    assert_eq!(notices(cx, window), None);
    for (id, label) in [
        (FEEDBACK_ADD_FILES_ID, "Add files…"),
        (FEEDBACK_CAPTURE_ID, "Capture"),
        (FEEDBACK_DISCARD_ID, "Discard"),
        (FEEDBACK_SUBMIT_ID, "Submit…"),
    ] {
        assert_eq!(label_of(cx, window, id).as_deref(), Some(label), "{id}");
    }
}

#[gpui_kit::test]
fn the_help_row_starts_collapsed_and_shows_three_sentences(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, _) = open(cx, &setup);

    with_window(cx, window, |window, _| {
        let header = window.find(FEEDBACK_HELP_ID);
        assert_eq!(header.label(), Some(HELP_TITLE));
        assert_eq!(header.expanded(), Some(false));
        assert!(window.try_find(FEEDBACK_HELP_TEXT_ID).is_none());
    });

    click(cx, window, FEEDBACK_HELP_ID);
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(FEEDBACK_HELP_ID).expanded(), Some(true));
        let help = window.within(FEEDBACK_HELP_TEXT_ID);
        let tops: Vec<_> = HELP_LINES
            .iter()
            .map(|line| help.find_by_label(line).bounds().top())
            .collect();
        assert!(
            tops.windows(2).all(|pair| pair[0] < pair[1]),
            "in order: {tops:?}"
        );
    });

    click(cx, window, FEEDBACK_HELP_ID);
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(FEEDBACK_HELP_ID).expanded(), Some(false));
        assert!(window.try_find(FEEDBACK_HELP_TEXT_ID).is_none());
    });
}

#[gpui_kit::test]
fn submit_needs_the_daemon(cx: &mut TestAppContext) {
    // The pane runs offline here: Submit is disabled and says why, and a
    // click changes nothing. (`feedback_submit.rs` covers it connected.)
    let setup = FeedbackSetup::new();
    let (window, shell, feedback) = open(cx, &setup);
    add_files(
        cx,
        window,
        &setup,
        vec![setup.picked_file("good.png", &png(64, 48))],
    );
    let before = draft(cx, &feedback);

    with_window(cx, window, |window, _| {
        let submit = window.find(FEEDBACK_SUBMIT_ID);
        assert_eq!(submit.disabled(), Some(true));
        assert_eq!(submit.description(), Some(NOT_CONNECTED_TEXT));
    });

    click(cx, window, FEEDBACK_SUBMIT_ID);
    assert!(!dialog_open(cx, window));
    assert_eq!(draft(cx, &feedback), before);
    assert_eq!(notices(cx, window), None);
    assert!(dock_open(cx, &shell));
    assert_eq!(cards(cx, window), ["good.png"]);
}

#[gpui_kit::test]
fn capture_takes_the_pane_as_it_is_without_the_hold(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, shell, feedback) = open(cx, &setup);
    let capture_disabled = |cx: &mut TestAppContext| {
        with_window(cx, window, |window, _| {
            window.find(FEEDBACK_CAPTURE_ID).disabled()
        })
    };
    assert_ne!(capture_disabled(cx), Some(true));

    click(cx, window, FEEDBACK_CAPTURE_ID);
    assert!(cx.read(|cx| feedback.read(cx).capturing()));
    assert_eq!(capture_disabled(cx), Some(true));
    assert!(dock_open(cx, &shell));
    assert_eq!(setup.hops.pending(), 1);

    run_hop(cx, window, &setup);
    assert!(dock_open(cx, &shell));
    assert_eq!(capture_disabled(cx), Some(true));
    assert!(setup.capturer.calls().is_empty());

    run_hop(cx, window, &setup);
    frame(cx, window);
    assert_eq!(setup.capturer.calls().len(), 1);
    assert!(dock_open(cx, &shell));
    assert!(!cx.read(|cx| feedback.read(cx).capturing()));
    assert_ne!(capture_disabled(cx), Some(true));
    assert_eq!(draft(cx, &feedback).attachments.len(), 1);
}

#[gpui_kit::test]
fn a_refused_permission_shows_its_sentence_and_add_files_still_works(cx: &mut TestAppContext) {
    // Ruling: macOS reports the permission as missing until the app restarts.
    assert_eq!(
        PERMISSION_TEXT,
        "SmithAgents needs Screen Recording permission to capture its window. Allow it in \
         System Settings → Privacy & Security → Screen Recording, then quit and reopen \
         SmithAgents."
    );
    let setup = FeedbackSetup::new();
    setup.capturer.set(Err(CaptureError::PermissionDenied));
    let (window, _, feedback) = open(cx, &setup);

    click(cx, window, FEEDBACK_CAPTURE_ID);
    run_hop(cx, window, &setup);
    run_hop(cx, window, &setup);
    frame(cx, window);
    assert_eq!(notices(cx, window).as_deref(), Some(PERMISSION_TEXT));
    assert!(draft(cx, &feedback).is_empty());
    assert_eq!(
        label_of(cx, window, FEEDBACK_EMPTY_ID).as_deref(),
        Some(EMPTY_TEXT)
    );

    add_files(
        cx,
        window,
        &setup,
        vec![setup.picked_file("good.png", &png(64, 48))],
    );
    assert_eq!(cards(cx, window), ["good.png"]);
    assert_eq!(notices(cx, window), None);
}

#[gpui_kit::test]
fn add_files_lists_cards_and_names_each_refused_file(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, _) = open(cx, &setup);

    add_files(
        cx,
        window,
        &setup,
        vec![
            setup.picked_file("good.png", &png(64, 48)),
            setup.picked_file("notes.txt", b"just words"),
            setup.picked_file("clip.mp4", MP4_START),
        ],
    );

    assert_eq!(cards(cx, window), ["good.png", "clip.mp4"]);
    assert_eq!(
        notices(cx, window),
        Some(Refusal::Extension.sentence("notes.txt"))
    );
    assert_eq!(label_of(cx, window, FEEDBACK_EMPTY_ID), None);
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(FEEDBACK_LIST_ID).role(), Some(Role::List));
    });
}

#[gpui_kit::test]
fn a_screenshot_card_counts_its_comments(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = open(cx, &setup);

    let first = screenshot(cx, window, &feedback, &setup, &[]);
    assert_eq!(cards(cx, window), ["Screenshot 1 · 0 comments"]);

    act(cx, window, &feedback, |f, _, cx| {
        f.open_annotator(&first, cx)
    });
    act(cx, window, &feedback, |f, _, cx| {
        f.set_boxes(&first, vec![note("the toolbar")], cx)
    });
    act(cx, window, &feedback, |f, _, cx| f.finish_annotating(cx));
    frame(cx, window);
    assert_eq!(cards(cx, window), ["Screenshot 1 · 1 comment"]);
    assert!(
        card_texts(cx, window, &first).contains(&"the toolbar".to_string()),
        "{:?}",
        card_texts(cx, window, &first)
    );

    screenshot(cx, window, &feedback, &setup, &["one", "two", "three"]);
    assert_eq!(
        cards(cx, window),
        ["Screenshot 1 · 1 comment", "Screenshot 2 · 3 comments"]
    );
}

#[gpui_kit::test]
fn comment_badges_use_the_destructive_token(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = open(cx, &setup);
    let id = screenshot(
        cx,
        window,
        &feedback,
        &setup,
        &["the toolbar", "this label"],
    );

    with_window(cx, window, |window, _| {
        let badge = window.within(card_id(&id)).find(comment_badge_id(1));
        assert_eq!(
            painted_at(window, badge.bounds().center()),
            token_background(&dark_native(), "destructive")
        );
        let second = window.within(card_id(&id)).find(comment_badge_id(2));
        assert!(
            second.bounds().top() > badge.bounds().top(),
            "one line each"
        );
    });
}

#[gpui_kit::test]
fn removing_a_card_renumbers_the_rest(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = open(cx, &setup);
    let first = screenshot(cx, window, &feedback, &setup, &[]);
    screenshot(cx, window, &feedback, &setup, &[]);
    assert_eq!(
        cards(cx, window),
        ["Screenshot 1 · 0 comments", "Screenshot 2 · 0 comments"]
    );
    assert_eq!(
        label_of(cx, window, card_remove_id(&first)).as_deref(),
        Some("Remove")
    );

    click(cx, window, card_remove_id(&first));

    assert_eq!(cards(cx, window), ["Screenshot 1 · 0 comments"]);
    let dir = setup.draft_dir();
    assert!(dir.join("screenshot-2.png").is_file());
    assert!(!dir.join("screenshot-1.png").exists());
    assert_eq!(cx.read(|cx| feedback.read(cx).annotating().cloned()), None);
}

#[gpui_kit::test]
fn clicking_a_screenshot_card_reopens_the_annotator(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = open(cx, &setup);
    add_files(
        cx,
        window,
        &setup,
        vec![setup.picked_file("good.png", &png(64, 48))],
    );
    let file = draft(cx, &feedback).attachments[0].id.clone();
    let shot = screenshot(cx, window, &feedback, &setup, &["the toolbar"]);
    let annotating =
        |cx: &mut TestAppContext| cx.read(|cx| feedback.read(cx).annotating().cloned());
    assert_eq!(annotating(cx), None);

    click(cx, window, card_id(&file));
    assert_eq!(annotating(cx), None, "a file card opens nothing");

    click(cx, window, card_id(&shot));
    assert_eq!(annotating(cx), Some(shot));
}

#[gpui_kit::test]
fn discard_asks_first_when_the_draft_is_not_empty(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, _) = open(cx, &setup);
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(FEEDBACK_DISCARD_ID).disabled(), Some(true));
    });
    click(cx, window, FEEDBACK_DISCARD_ID);
    assert!(!dialog_open(cx, window), "nothing to discard");

    add_files(
        cx,
        window,
        &setup,
        vec![
            setup.picked_file("good.png", &png(64, 48)),
            setup.picked_file("clip.mp4", MP4_START),
        ],
    );
    assert_eq!(cards(cx, window), ["good.png", "clip.mp4"]);
    with_window(cx, window, |window, _| {
        assert_ne!(window.find(FEEDBACK_DISCARD_ID).disabled(), Some(true));
    });

    click(cx, window, FEEDBACK_DISCARD_ID);
    assert!(dialog_open(cx, window));
    with_window(cx, window, |window, _| {
        assert_eq!(
            window.find(DISCARD_DIALOG_TITLE_ID).label(),
            Some("Discard this feedback?")
        );
        let description = discard_description(2);
        assert!(
            test_support::texts(window)
                .iter()
                .any(|text| text.text() == description),
            "the dialog says what goes"
        );
        assert_eq!(window.find(DISCARD_CANCEL_ID).focused(), Some(true));
        assert_eq!(window.find(DISCARD_CANCEL_ID).label(), Some("Cancel"));
        assert_eq!(window.find(DISCARD_CONFIRM_ID).label(), Some("Discard"));
    });
    click(cx, window, DISCARD_CANCEL_ID);
    assert!(!dialog_open(cx, window));
    assert_eq!(cards(cx, window), ["good.png", "clip.mp4"]);

    // Enter does what the focused Cancel does.
    click(cx, window, FEEDBACK_DISCARD_ID);
    with_window(cx, window, |window, cx| window.press("enter", cx));
    frame(cx, window);
    assert!(!dialog_open(cx, window));
    assert_eq!(cards(cx, window), ["good.png", "clip.mp4"]);

    click(cx, window, FEEDBACK_DISCARD_ID);
    click(cx, window, DISCARD_CONFIRM_ID);
    assert!(!dialog_open(cx, window));
    assert_eq!(
        label_of(cx, window, FEEDBACK_EMPTY_ID).as_deref(),
        Some(EMPTY_TEXT)
    );
    assert_eq!(cards(cx, window), Vec::<String>::new());
    assert!(!setup.draft_dir().exists());
}

#[gpui_kit::test]
fn a_broken_draft_is_announced_once(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    fs::create_dir_all(setup.draft_dir()).unwrap();
    fs::write(setup.draft_dir().join("draft.json"), "{").unwrap();

    let (window, shell, _) = open(cx, &setup);
    assert_eq!(notices(cx, window).as_deref(), Some(BROKEN_DRAFT_TEXT));
    assert_eq!(
        label_of(cx, window, FEEDBACK_EMPTY_ID).as_deref(),
        Some(EMPTY_TEXT)
    );
    assert_eq!(cards(cx, window), Vec::<String>::new());

    close(cx, window, shell);
    let (window, _, _) = open(cx, &setup);
    assert_eq!(notices(cx, window), None, "said once");
    assert_eq!(
        label_of(cx, window, FEEDBACK_EMPTY_ID).as_deref(),
        Some(EMPTY_TEXT)
    );
}

#[gpui_kit::test]
fn thumbnails_leave_the_atlas_with_their_cards(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = open(cx, &setup);
    let thumbnail = |cx: &mut TestAppContext, id: &AttachmentId| {
        cx.read(|cx| feedback.read(cx).thumbnail(id))
            .expect("a thumbnail")
    };

    // Removed.
    add_files(
        cx,
        window,
        &setup,
        vec![setup.picked_file("good.png", &png(64, 48))],
    );
    let file = draft(cx, &feedback).attachments[0].id.clone();
    let shown = thumbnail(cx, &file);
    assert!(in_atlas(cx, window, &shown), "drawn");
    click(cx, window, card_remove_id(&file));
    assert!(!in_atlas(cx, window, &shown), "released with its card");

    // Replaced: Done draws a new one.
    let shot = screenshot(cx, window, &feedback, &setup, &[]);
    let before = thumbnail(cx, &shot);
    assert!(in_atlas(cx, window, &before));
    act(cx, window, &feedback, |f, _, cx| {
        f.open_annotator(&shot, cx)
    });
    act(cx, window, &feedback, |f, _, cx| {
        f.set_boxes(&shot, vec![note("the toolbar")], cx)
    });
    act(cx, window, &feedback, |f, _, cx| f.finish_annotating(cx));
    frame(cx, window);
    let after = thumbnail(cx, &shot);
    assert!(!Arc::ptr_eq(&before, &after));
    assert!(in_atlas(cx, window, &after));
    assert!(!in_atlas(cx, window, &before), "the old one is released");

    // Discarded.
    act(cx, window, &feedback, |f, _, cx| f.discard_all(cx));
    frame(cx, window);
    assert!(!in_atlas(cx, window, &after));
}

#[gpui_kit::test]
fn enter_on_a_focused_screenshot_card_opens_the_annotator(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, feedback) = open(cx, &setup);
    let shot = screenshot(cx, window, &feedback, &setup, &["the toolbar"]);
    assert_eq!(cx.read(|cx| feedback.read(cx).annotating().cloned()), None);

    let card_focused = |cx: &mut TestAppContext| {
        with_window(cx, window, |window, _| {
            window.find(card_id(&shot)).focused()
        }) == Some(true)
    };
    for _ in 0..60 {
        if card_focused(cx) {
            break;
        }
        with_window(cx, window, |window, cx| window.press("tab", cx));
        frame(cx, window);
    }
    assert!(card_focused(cx), "Tab reaches the screenshot's card");

    with_window(cx, window, |window, cx| window.press("enter", cx));
    frame(cx, window);
    assert_eq!(
        cx.read(|cx| feedback.read(cx).annotating().cloned()),
        Some(shot)
    );
}

#[gpui_kit::test]
fn many_notices_scroll_and_leave_the_cards_and_buttons_in_the_pane(cx: &mut TestAppContext) {
    let setup = FeedbackSetup::new();
    let (window, _, _) = open(cx, &setup);
    let mut paths: Vec<PathBuf> = vec![setup.picked_file("good.png", &png(64, 48))];
    paths.extend(
        (1..=30).map(|n| setup.picked_file(&format!("meeting-notes-{n}.txt"), b"just words")),
    );
    add_files(cx, window, &setup, paths);
    assert!(notices(cx, window).is_some());

    with_window(cx, window, |window, _| {
        let pane = window.find(FEEDBACK_PANEL).bounds();
        let notices = window.find(FEEDBACK_NOTICES_ID).bounds();
        let list = window.find(FEEDBACK_LIST_ID).bounds();
        let add = window.find(FEEDBACK_ADD_FILES_ID).bounds();
        let submit = window.find(FEEDBACK_SUBMIT_ID).bounds();
        assert!(
            f32::from(notices.size.height) <= 160.5,
            "the notices are at most 10 rem tall: {notices:?}"
        );
        assert!(notices.bottom() <= list.top(), "{notices:?} {list:?}");
        assert!(
            f32::from(list.size.height) >= 100.,
            "the cards keep their room: {list:?}"
        );
        for button in [add, submit] {
            assert!(
                button.bottom() <= pane.bottom() && button.top() >= list.bottom(),
                "the buttons stay in the pane: {button:?} in {pane:?}"
            );
        }
    });
    assert_eq!(cards(cx, window), ["good.png"]);
}

use std::fs;
use std::path::Path;

use smithagents_app::feedback::annotation::{Annotation, FracRect};
use smithagents_app::feedback::draft::{
    annotated_name, thumb_name, Attachment, AttachmentId, AttachmentKind, Draft, DraftStore,
};
use smithagents_app::feedback::files::{FileKind, Usage};
use smithagents_app::feedback::text::BROKEN_DRAFT_TEXT;

fn note(x: f32, y: f32, w: f32, h: f32) -> Annotation {
    Annotation {
        rect: FracRect { x, y, w, h },
        comment: "the toolbar".to_string(),
    }
}

fn shot(name: &str, boxes: Vec<Annotation>) -> Attachment {
    Attachment {
        id: AttachmentId::new(),
        kind: AttachmentKind::Screenshot,
        original_name: name.to_string(),
        stored_name: name.to_string(),
        size: 100,
        width: Some(2400),
        height: Some(1512),
        boxes,
        rendered: true,
    }
}

fn plain(kind: AttachmentKind, original: &str, stored: &str, size: u64) -> Attachment {
    Attachment {
        id: AttachmentId::new(),
        kind,
        original_name: original.to_string(),
        stored_name: stored.to_string(),
        size,
        width: None,
        height: None,
        boxes: Vec::new(),
        rendered: false,
    }
}

fn draft_of(attachments: Vec<Attachment>) -> Draft {
    Draft {
        attachments,
        ..Draft::default()
    }
}

/// Writes the attachments' files (and the annotated image of each
/// screenshot marked rendered) so that `load` keeps them.
fn write_files(store: &DraftStore, draft: &Draft) {
    for a in &draft.attachments {
        store.write_file(&a.stored_name, b"bytes").unwrap();
        if a.kind == AttachmentKind::Screenshot && a.rendered {
            store
                .write_file(&annotated_name(&a.stored_name), b"bytes")
                .unwrap();
        }
    }
}

fn three() -> Draft {
    draft_of(vec![
        shot("screenshot-1.png", vec![note(0.25, 0.25, 0.25, 0.25)]),
        plain(AttachmentKind::Image, "a.jpg", "image-1.jpg", 10),
        plain(AttachmentKind::Video, "demo.mp4", "clip-1.mp4", 20),
    ])
}

fn broken_dirs(home: &Path) -> Vec<std::path::PathBuf> {
    let mut found: Vec<_> = fs::read_dir(home.join("feedback"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("draft.broken-")
        })
        .collect();
    found.sort();
    found
}

fn write_json(store: &DraftStore, text: &str) {
    fs::create_dir_all(store.dir()).unwrap();
    fs::write(store.dir().join("draft.json"), text).unwrap();
}

#[test]
fn the_draft_lives_under_feedback_draft_in_the_home() {
    let home = tempfile::tempdir().unwrap();
    let store = DraftStore::new(home.path());
    assert_eq!(store.dir(), home.path().join("feedback/draft"));
    store.save(&Draft::default()).unwrap();
    assert!(store.dir().join("draft.json").is_file());
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&store.dir()), 0o700);
    assert_eq!(mode(&store.dir().join("draft.json")), 0o600);
}

#[test]
fn version_one_json_has_the_documented_shape() {
    let home = tempfile::tempdir().unwrap();
    let store = DraftStore::new(home.path());
    let draft = draft_of(vec![
        shot("screenshot-1.png", vec![note(0.25, 0.25, 0.25, 0.25)]),
        plain(AttachmentKind::Video, "demo.mp4", "clip-1.mp4", 1_048_576),
    ]);
    store.save(&draft).unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(store.dir().join("draft.json")).unwrap()).unwrap();
    let a0 = draft.attachments[0].id.as_str();
    let a1 = draft.attachments[1].id.as_str();
    let expected = serde_json::json!({
        "version": 1, "main_idea": "", "kind": null,
        "attachments": [
            { "id": a0, "kind": "screenshot", "original_name": "screenshot-1.png",
              "stored_name": "screenshot-1.png", "size": 100,
              "width": 2400, "height": 1512, "rendered": true,
              "boxes": [ { "x": 0.25, "y": 0.25, "w": 0.25, "h": 0.25, "comment": "the toolbar" } ] },
            { "id": a1, "kind": "video", "original_name": "demo.mp4",
              "stored_name": "clip-1.mp4", "size": 1_048_576 }
        ]
    });
    assert_eq!(json, expected);
    let video = json["attachments"][1].as_object().unwrap();
    for key in ["width", "height", "boxes", "rendered"] {
        assert!(!video.contains_key(key), "{key}");
    }
}

#[test]
fn save_then_load_is_the_same_draft() {
    let home = tempfile::tempdir().unwrap();
    let store = DraftStore::new(home.path());
    let empty = store.load();
    assert_eq!(empty.draft, Draft::default());
    assert!(empty.notices.is_empty());

    let draft = three();
    write_files(&store, &draft);
    store.save(&draft).unwrap();
    let loaded = store.load();
    assert_eq!(loaded.draft, draft);
    assert!(loaded.notices.is_empty());
}

#[test]
fn a_broken_draft_is_set_aside_and_named() {
    for text in [
        "{ not json".to_string(),
        r#"{"version":2,"main_idea":"","kind":null,"attachments":[]}"#.to_string(),
        String::new(),
        "x".repeat(5 * 1024 * 1024),
    ] {
        let home = tempfile::tempdir().unwrap();
        let store = DraftStore::new(home.path());
        write_json(&store, &text);
        fs::write(store.dir().join("screenshot-1.png"), b"png").unwrap();
        let loaded = store.load();
        assert_eq!(loaded.draft, Draft::default());
        assert_eq!(loaded.notices, vec![BROKEN_DRAFT_TEXT.to_string()]);
        let aside = broken_dirs(home.path());
        assert_eq!(aside.len(), 1);
        assert!(aside[0].join("draft.json").is_file());
        assert!(aside[0].join("screenshot-1.png").is_file());
        assert!(!store.dir().join("draft.json").exists());

        write_json(&store, "{ not json");
        let again = store.load();
        assert_eq!(again.notices, vec![BROKEN_DRAFT_TEXT.to_string()]);
        assert_eq!(broken_dirs(home.path()).len(), 2);
    }
}

#[test]
fn attachments_whose_files_are_gone_are_dropped_with_a_sentence() {
    let home = tempfile::tempdir().unwrap();
    let store = DraftStore::new(home.path());
    let draft = three();
    write_files(&store, &draft);
    store.save(&draft).unwrap();
    fs::remove_file(store.path_of("image-1.jpg")).unwrap();
    let loaded = store.load();
    assert_eq!(loaded.draft.attachments.len(), 2);
    assert_eq!(loaded.draft.attachments[0], draft.attachments[0]);
    assert_eq!(loaded.draft.attachments[1], draft.attachments[2]);
    assert_eq!(
        loaded.notices,
        vec!["1 attachment was removed because its file is missing.".to_string()]
    );
    let on_disk: serde_json::Value =
        serde_json::from_slice(&fs::read(store.dir().join("draft.json")).unwrap()).unwrap();
    assert_eq!(on_disk["attachments"].as_array().unwrap().len(), 2);
    assert!(store.load().notices.is_empty());

    fs::remove_file(store.path_of("clip-1.mp4")).unwrap();
    fs::remove_file(store.path_of("screenshot-1.png")).unwrap();
    let loaded = store.load();
    assert!(loaded.draft.attachments.is_empty());
    assert_eq!(
        loaded.notices,
        vec!["2 attachments were removed because their files are missing.".to_string()]
    );
}

#[test]
fn a_stored_name_that_leaves_the_directory_is_dropped_and_never_touched() {
    let home = tempfile::tempdir().unwrap();
    fs::write(home.path().join("outside.png"), b"x").unwrap();
    let store = DraftStore::new(home.path());
    let names = [
        "../../outside.png",
        "/etc/hosts",
        ".hidden.png",
        "sub/x.png",
        "..\\outside.png",
        "",
        "..",
        "a\0b.png",
    ];
    let draft = draft_of(
        names
            .iter()
            .map(|n| plain(AttachmentKind::Image, "x.png", n, 1))
            .collect(),
    );
    store.save(&draft).unwrap();
    fs::create_dir_all(store.dir().join("sub")).unwrap();
    fs::write(store.dir().join("sub/x.png"), b"x").unwrap();
    fs::write(store.dir().join(".hidden.png"), b"x").unwrap();
    let loaded = store.load();
    assert!(loaded.draft.attachments.is_empty());
    assert_eq!(loaded.notices.len(), 1);
    assert!(home.path().join("outside.png").exists());
    store.remove_file("../../outside.png");
    assert!(home.path().join("outside.png").exists());
    assert!(store.write_file("../escape.png", b"x").is_err());
    assert!(!store.dir().parent().unwrap().join("escape.png").exists());
    store.clear().unwrap();
    assert!(home.path().join("outside.png").exists());
    assert!(!store.dir().exists());
}

#[test]
fn a_directory_or_symlink_in_place_of_a_file_is_dropped() {
    let home = tempfile::tempdir().unwrap();
    let store = DraftStore::new(home.path());
    fs::write(home.path().join("target.png"), b"x").unwrap();
    let draft = draft_of(vec![
        plain(AttachmentKind::Image, "d.png", "image-1.png", 1),
        plain(AttachmentKind::Image, "l.png", "image-2.png", 1),
    ]);
    store.save(&draft).unwrap();
    fs::create_dir(store.path_of("image-1.png")).unwrap();
    std::os::unix::fs::symlink(home.path().join("target.png"), store.path_of("image-2.png"))
        .unwrap();
    let loaded = store.load();
    assert!(loaded.draft.attachments.is_empty());
    assert_eq!(loaded.notices.len(), 1);
}

#[test]
fn bad_boxes_are_cleaned_and_the_screenshot_is_redrawn() {
    let home = tempfile::tempdir().unwrap();
    let store = DraftStore::new(home.path());
    let mut boxes = vec![note(0.8, 0.8, 0.9, 0.9), note(-1.0, 0.1, 0.0, 0.2)];
    boxes.extend((0..50).map(|_| note(0.1, 0.1, 0.2, 0.2)));
    let draft = draft_of(vec![shot("screenshot-1.png", boxes)]);
    write_files(&store, &draft);
    store.save(&draft).unwrap();
    let loaded = store.load();
    let s = &loaded.draft.attachments[0];
    assert_eq!(s.boxes.len(), 20);
    for b in &s.boxes {
        let r = b.rect;
        assert!((0.0..=1.0).contains(&r.x) && (0.0..=1.0).contains(&r.y));
        assert!(r.x + r.w <= 1.0 + 1e-6 && r.y + r.h <= 1.0 + 1e-6);
    }
    assert!(!s.rendered);
    assert!(loaded.notices.is_empty());
    let again = store.load();
    assert_eq!(again.draft, loaded.draft);
}

#[test]
fn a_screenshot_without_its_annotated_file_is_marked_for_redrawing() {
    let home = tempfile::tempdir().unwrap();
    let store = DraftStore::new(home.path());
    let draft = draft_of(vec![shot(
        "screenshot-1.png",
        vec![note(0.1, 0.1, 0.2, 0.2)],
    )]);
    store.write_file("screenshot-1.png", b"x").unwrap();
    store.save(&draft).unwrap();
    let loaded = store.load();
    assert!(!loaded.draft.attachments[0].rendered);
    assert!(loaded.notices.is_empty());
}

#[test]
fn names_count_up_per_prefix_and_never_repeat() {
    let empty = Draft::default();
    assert_eq!(
        empty.next_stored_name(AttachmentKind::Screenshot, FileKind::Png),
        "screenshot-1.png"
    );
    assert_eq!(
        empty.next_stored_name(AttachmentKind::Image, FileKind::Jpeg),
        "image-1.jpg"
    );
    assert_eq!(
        empty.next_stored_name(AttachmentKind::Video, FileKind::Mp4),
        "clip-1.mp4"
    );
    let held = draft_of(vec![
        shot("screenshot-1.png", vec![]),
        shot("screenshot-3.png", vec![]),
    ]);
    assert_eq!(
        held.next_stored_name(AttachmentKind::Screenshot, FileKind::Png),
        "screenshot-4.png"
    );
    assert_eq!(
        held.next_stored_name(AttachmentKind::Image, FileKind::Png),
        "image-1.png"
    );
    assert_eq!(
        annotated_name("screenshot-1.png"),
        "screenshot-1.annotated.png"
    );
    assert_eq!(thumb_name("image-2.jpg"), "image-2.thumb.png");
}

#[test]
fn screenshot_numbers_follow_position() {
    let a = shot("screenshot-1.png", vec![]);
    let image = plain(AttachmentKind::Image, "i.png", "image-1.png", 40);
    let b = shot("screenshot-2.png", vec![]);
    let (ia, ii, ib) = (a.id.clone(), image.id.clone(), b.id.clone());
    let draft = draft_of(vec![a, image, b]);
    assert_eq!(draft.screenshot_number(&ia), Some(1));
    assert_eq!(draft.screenshot_number(&ib), Some(2));
    assert_eq!(draft.screenshot_number(&ii), None);
    assert_eq!(draft.screenshot_number(&AttachmentId::new()), None);
    assert_eq!(draft.screenshot_count(), 2);
    assert_eq!(draft.get(&ii).unwrap().stored_name, "image-1.png");
    assert_eq!(
        draft.usage(),
        Usage {
            count: 3,
            bytes: 240
        }
    );
    assert!(!draft.is_empty());
    assert!(Draft::default().is_empty());
}

#[test]
fn remove_file_takes_its_annotated_and_thumb_files() {
    let home = tempfile::tempdir().unwrap();
    let store = DraftStore::new(home.path());
    for n in [
        "screenshot-1.png",
        "screenshot-1.annotated.png",
        "screenshot-1.thumb.png",
        "screenshot-2.png",
    ] {
        store.write_file(n, b"x").unwrap();
    }
    store.remove_file("screenshot-1.png");
    assert!(!store.path_of("screenshot-1.png").exists());
    assert!(!store.path_of("screenshot-1.annotated.png").exists());
    assert!(!store.path_of("screenshot-1.thumb.png").exists());
    assert!(store.path_of("screenshot-2.png").exists());
    store.remove_file("screenshot-9.png");
}

#[test]
fn a_failed_save_leaves_the_old_draft_intact() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let store = DraftStore::new(home.path());
    let a = draft_of(vec![shot("screenshot-1.png", vec![])]);
    store.save(&a).unwrap();
    fs::set_permissions(store.dir(), fs::Permissions::from_mode(0o500)).unwrap();
    let b = draft_of(vec![
        shot("screenshot-1.png", vec![]),
        shot("screenshot-2.png", vec![]),
    ]);
    let result = store.save(&b);
    fs::set_permissions(store.dir(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    let text = fs::read_to_string(store.dir().join("draft.json")).unwrap();
    assert_eq!(serde_json::from_str::<Draft>(&text).unwrap(), a);
    let leftovers: Vec<_> = fs::read_dir(store.dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

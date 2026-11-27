use chrono::{NaiveDate, TimeZone, Utc};
use smithagents_core::feedback::{
    AttachmentKind, FeedbackAttachment, FeedbackBox, FeedbackKind, SubmissionId,
};
use smithagents_server::feedback::issue::*;

fn date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 26).unwrap()
}

fn shot(comments: &[&str]) -> FeedbackAttachment {
    FeedbackAttachment {
        kind: AttachmentKind::Screenshot,
        original_name: "screen.png".into(),
        boxes: comments
            .iter()
            .map(|c| FeedbackBox {
                x: 0.1,
                y: 0.1,
                w: 0.2,
                h: 0.2,
                comment: (*c).into(),
            })
            .collect(),
    }
}

fn file(kind: AttachmentKind, name: &str) -> FeedbackAttachment {
    FeedbackAttachment {
        kind,
        original_name: name.into(),
        boxes: vec![],
    }
}

fn names_for(a: &[FeedbackAttachment], kinds: &[FileKind]) -> Vec<Names> {
    let pairs: Vec<_> = a
        .iter()
        .map(|x| x.kind)
        .zip(kinds.iter().copied())
        .collect();
    file_names(&pairs)
}

fn render(title: &str, idea: &str, a: &[FeedbackAttachment], kinds: &[FileKind]) -> String {
    let names = names_for(a, kinds);
    render_issue(&IssueInput {
        title,
        main_idea: idea,
        kind: FeedbackKind::Bug,
        created: Utc.with_ymd_and_hms(2026, 11, 26, 14, 15, 0).unwrap(),
        app_version: "0.1.0",
        platform: "macos",
        attachments: a,
        names: &names,
    })
}

#[test]
fn the_title_is_the_first_line() {
    assert_eq!(
        title_of("  \n## Fix the zoom\nmore").as_deref(),
        Some("Fix the zoom")
    );
    assert_eq!(title_of("\n \t"), None);
    assert_eq!(title_of(""), None);
    assert_eq!(title_of("###"), None);
    assert_eq!(
        title_of(&"é".repeat(300)).unwrap().chars().count(),
        TITLE_MAX_CHARS
    );
    let t = title_of("a\u{202e}b").unwrap();
    assert!(!t.contains('\u{202e}'));
    assert!(!title_of("a\u{0}b\u{7}c")
        .unwrap()
        .chars()
        .any(char::is_control));
    // enormous input does not panic and stays bounded
    assert!(title_of(&"x".repeat(5_000_000)).unwrap().chars().count() <= TITLE_MAX_CHARS);
}

#[test]
fn slugs_and_names() {
    assert_eq!(issue_slug("Fix the Zoom!"), "fix-the-zoom");
    assert_eq!(issue_slug("日本語"), "feedback");
    assert_eq!(issue_slug(""), "feedback");
    assert_eq!(with_suffix("x", 1), "x");
    assert_eq!(with_suffix("x", 3), "x-3");
    assert_eq!(
        folder_path(date(), "fix-the-zoom"),
        "docs/issues/2026-11-26-fix-the-zoom"
    );
    assert_eq!(
        branch_name(date(), "fix-the-zoom", false),
        "feedback/2026-11-26-fix-the-zoom"
    );
    assert_eq!(
        branch_name(date(), "fix-the-zoom", true),
        "idea/fix-the-zoom"
    );
}

#[test]
fn hostile_slugs_never_reach_a_path() {
    let hostile = [
        "../../etc/passwd",
        "..",
        ".hidden",
        "a/b\\c",
        "x\0y\n",
        "\u{202e}rtl",
        "   ",
        "---",
        "ÀÉ ünï",
        &"a".repeat(100_000),
    ];
    for h in hostile {
        let s = issue_slug(h);
        assert!(!s.is_empty() && s.len() <= 48, "{h:?} -> {s:?}");
        assert!(
            s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "{s:?}"
        );
        assert!(!s.starts_with('-'));
        // even a slug that did not come from issue_slug cannot escape the folder
        for p in [
            folder_path(date(), h),
            branch_name(date(), h, false),
            branch_name(date(), h, true),
        ] {
            assert!(!p.contains(".."), "{p:?}");
            assert!(
                !p.contains('\\') && !p.contains('\0') && !p.contains('\n'),
                "{p:?}"
            );
            assert!(
                p.split('/')
                    .all(|seg| !seg.is_empty() && !seg.starts_with('.')),
                "{p:?}"
            );
        }
    }
}

#[test]
fn file_names_count_per_kind() {
    let a = [
        file(AttachmentKind::Screenshot, "a"),
        file(AttachmentKind::Image, "b"),
        file(AttachmentKind::Screenshot, "c"),
        file(AttachmentKind::Video, "d"),
        file(AttachmentKind::Image, "e"),
    ];
    let n = names_for(
        &a,
        &[
            FileKind::Png,
            FileKind::Jpeg,
            FileKind::Png,
            FileKind::Mp4,
            FileKind::Webp,
        ],
    );
    assert_eq!(n[0].file, "screenshot-1.png");
    assert_eq!(
        n[0].annotated.as_deref(),
        Some("screenshot-1.annotated.png")
    );
    assert_eq!(n[1].file, "image-1.jpg");
    assert_eq!(n[1].annotated, None);
    assert_eq!(n[2].file, "screenshot-2.png");
    assert_eq!(
        n[2].annotated.as_deref(),
        Some("screenshot-2.annotated.png")
    );
    assert_eq!(n[3].file, "clip-1.mp4");
    assert_eq!(n[4].file, "image-2.webp");
    assert_eq!(FileKind::Gif.extension(), "gif");
}

#[test]
fn issue_md_is_rendered_exactly() {
    let a = [
        shot(&["the toolbar", "this label"]),
        file(AttachmentKind::Image, "photo.jpg"),
        file(AttachmentKind::Video, "demo.mp4"),
    ];
    let got = render(
        "Fix the zoom",
        "Fix the zoom\n\nThe page is too big.",
        &a,
        &[FileKind::Png, FileKind::Jpeg, FileKind::Mp4],
    );
    let want = "---\n\
title: \"Fix the zoom\"\n\
kind: bug\n\
created: 2026-11-26T14:15:00Z\n\
app_version: 0.1.0\n\
platform: macos\n\
---\n\
\n\
# Fix the zoom\n\
\n\
The page is too big.\n\
\n\
## Screenshot 1\n\
\n\
![Screenshot 1](screenshot-1.annotated.png)\n\
\n\
1. the toolbar\n\
2. this label\n\
\n\
Original: [screenshot-1.png](screenshot-1.png)\n\
\n\
## photo.jpg\n\
\n\
![photo.jpg](image-1.jpg)\n\
\n\
## demo.mp4\n\
\n\
[demo.mp4](clip-1.mp4)\n";
    assert_eq!(got, want);
}

#[test]
fn a_title_only_idea_leaves_out_the_body() {
    let got = render("T", "T", &[], &[]);
    assert_eq!(
        got,
        "---\ntitle: \"T\"\nkind: bug\ncreated: 2026-11-26T14:15:00Z\napp_version: 0.1.0\nplatform: macos\n---\n\n# T\n"
    );
}

#[test]
fn hostile_names_and_comments_cannot_break_the_file() {
    let a = [
        file(AttachmentKind::Image, "../../etc/passwd\n# pwn](http://x)"),
        shot(&[
            "a\n## b",
            "![x](http://evil/p.png) <script>alert(1)</script>",
            "> q\n---",
        ]),
        file(AttachmentKind::Image, ""),
        file(AttachmentKind::Video, "<img src=x onerror=y>.mp4"),
    ];
    let got = render(
        "x\"\ny: z",
        "x\"\ny: z\nbody",
        &a,
        &[FileKind::Png, FileKind::Png, FileKind::Gif, FileKind::Mp4],
    );
    // Everything up to the last `/` is dropped (D-M8).
    assert!(got.contains("\n## x\n"), "{got}");
    assert!(!got.lines().any(|l| l.starts_with("# pwn")));
    assert!(got.contains("\n1. a \\#\\# b\n"), "{got}");
    assert!(!got.lines().any(|l| l.starts_with("## b")));
    // The comment's image link is text (the image itself is now named `x`).
    assert!(!got.contains("![x](http"), "{got}");
    assert!(got.contains("\n2. \\!\\[x\\]\\(http"), "{got}");
    let unescaped = got.replace("\\<", "");
    assert!(
        !unescaped.contains("<script") && !unescaped.contains("<img"),
        "{got}"
    );
    assert!(got.contains("## attachment\n"));
    // front matter: exactly five keys, and the title parses back
    let mut lines = got.lines();
    assert_eq!(lines.next(), Some("---"));
    let fm: Vec<&str> = lines.by_ref().take_while(|l| *l != "---").collect();
    assert_eq!(fm.len(), 5, "{fm:?}");
    let title = fm[0].strip_prefix("title: ").unwrap();
    assert_eq!(serde_json::from_str::<String>(title).unwrap(), "x\"\ny: z");
    assert!(fm[1..].iter().all(|l| !l.starts_with("y:")));
    // the single H1 is the cleaned, escaped title
    assert_eq!(got.lines().filter(|l| l.starts_with("# ")).count(), 1);
}

#[test]
fn hostile_version_and_platform_stay_on_their_lines() {
    let names = names_for(&[], &[]);
    let got = render_issue(&IssueInput {
        title: "T",
        main_idea: "T",
        kind: FeedbackKind::Enhancement,
        created: Utc.with_ymd_and_hms(2026, 11, 26, 14, 15, 0).unwrap(),
        app_version: "1\"\nkind: x\n---\n# h",
        platform: "\u{2028}evil: 1",
        attachments: &[],
        names: &names,
    });
    let fm: Vec<&str> = got.lines().skip(1).take_while(|l| *l != "---").collect();
    assert_eq!(fm.len(), 5, "{fm:?}");
    assert!(!got.contains('\u{2028}'));
    assert!(fm[1].starts_with("kind: enhancement"));
}

#[test]
fn non_ascii_and_huge_input_do_not_panic() {
    let big = "日本語é".repeat(50_000);
    let a = [file(AttachmentKind::Image, &big), shot(&[&big])];
    let out = render(&big, &big, &a, &[FileKind::Png, FileKind::Png]);
    assert!(out.ends_with('\n'));
}

fn seed_input<'a>(a: &'a [FeedbackAttachment], names: &'a [Names]) -> IssueInput<'a> {
    IssueInput {
        title: "Fix the zoom",
        main_idea: "Fix the zoom\n\nThe page is too big.",
        kind: FeedbackKind::Bug,
        created: Utc.with_ymd_and_hms(2026, 11, 26, 14, 15, 0).unwrap(),
        app_version: "0.1.0",
        platform: "macos",
        attachments: a,
        names,
    }
}

#[test]
fn the_seed_names_the_folder_and_the_branch() {
    let a = [shot(&["the toolbar", "this label"])];
    let n = names_for(&a, &[FileKind::Png]);
    let s = seed_text(
        &seed_input(&a, &n),
        "docs/issues/2026-11-26-fix-the-zoom",
        "feedback/2026-11-26-fix-the-zoom",
        "0123abc",
        10_000,
    );
    assert!(
        s.starts_with("Fix the zoom\n\nThe page is too big.\n\nKind: bug\n"),
        "{s}"
    );
    assert!(
        s.contains("Kind: bug\nComments on the screenshots:\n- Screenshot 1, box 1: the toolbar\n")
    );
    assert!(s.contains("- Screenshot 1, box 2: this label"));
    assert!(s.ends_with(
        "Feedback files: `docs/issues/2026-11-26-fix-the-zoom/` on branch `feedback/2026-11-26-fix-the-zoom` (commit 0123abc)."
    ));

    let long = "long comment ".repeat(10);
    let many: Vec<&str> = (0..40).map(|_| long.as_str()).collect();
    let a = [shot(&many)];
    let n = names_for(&a, &[FileKind::Png]);
    let s = seed_text(&seed_input(&a, &n), "f", "b", "c", 200);
    let tail = "\n\nFeedback files: `f/` on branch `b` (commit c).";
    assert!(s.ends_with(tail));
    assert!(s.len() <= 200 + tail.len(), "{} {s}", s.len());
    assert!(s.contains("- (more comments in issue.md)"));

    let none = [file(AttachmentKind::Image, "p.png")];
    let n = names_for(&none, &[FileKind::Png]);
    let s = seed_text(&seed_input(&none, &n), "f", "b", "c", 200);
    assert!(!s.contains("Comments on the screenshots"));
    assert!(s.contains("Kind: bug\n\nFeedback files:"));
}

#[test]
fn only_github_remotes_are_parsed() {
    let ok = ("o".to_string(), "r".to_string());
    for u in [
        "https://github.com/o/r",
        "https://github.com/o/r.git",
        "https://github.com/o/r/",
        "https://user:token@github.com/o/r.git",
        "https://GitHub.com/o/r",
        "git@github.com:o/r.git",
        "ssh://git@github.com/o/r.git",
        "https://user:LEAK-token@github.com/o/r.git",
    ] {
        assert_eq!(github_repo(u), Some(ok.clone()), "{u}");
    }
    for u in [
        "https://gitlab.com/o/r",
        "https://github.com.evil.example/o/r",
        "https://evil.example/github.com/o/r",
        "https://github.com@evil.example/o/r",
        "https://github.com:8443/o/r",
        "/tmp/bare.git",
        "",
        "https://github.com/o",
        "https://github.com/o/r/extra",
        "https://github.com/../r",
        "https://github.com/o/r?x=1",
        "https://github.com/o/r r",
        "https://github.com/o/\nr",
        "git@github.com:o",
        "file:///github.com/o/r",
        "http://github.com/o/r",
    ] {
        assert_eq!(github_repo(u), None, "{u:?}");
    }
}

const BASE: &str = "https://github.com/o/r/blob/0123abc/docs/issues/2026-11-26-fix-the-zoom/";

#[test]
fn the_issue_body_links_to_the_commit() {
    let a = [shot(&["c"])];
    let n = names_for(&a, &[FileKind::Png]);
    let mut md = render_issue(&seed_input(&a, &n));
    md.push_str(
        "\n[x](other.png) [y](https://example.com/screenshot-1.png) [z](screenshot-1.png.evil)\n",
    );
    let id = SubmissionId(uuid::Uuid::nil());
    let m = marker(id);
    assert_eq!(
        m,
        "<!-- smithagents-feedback:00000000-0000-0000-0000-000000000000 -->"
    );
    let files = vec![
        "screenshot-1.png".to_string(),
        "screenshot-1.annotated.png".to_string(),
    ];
    let body = issue_body(&md, &files, BASE, Some(&m));
    assert!(!body.contains("title:") && !body.starts_with("---"));
    assert!(body.starts_with("# Fix the zoom"));
    assert!(body.contains(&format!(
        "![Screenshot 1]({BASE}screenshot-1.annotated.png?raw=true)"
    )));
    assert!(body.contains(&format!(
        "[screenshot-1.png]({BASE}screenshot-1.png?raw=true)"
    )));
    assert!(body.contains("[x](other.png)"));
    assert!(body.contains("[y](https://example.com/screenshot-1.png)"));
    assert!(body.contains("[z](screenshot-1.png.evil)"));
    assert!(body.ends_with(&format!("\n\n{m}")));

    let big = format!("# t\n{}", "é".repeat(100_000));
    let body = issue_body(&big, &files, BASE, Some(&m));
    assert_eq!(body.chars().count(), 60_000);
    assert!(body.ends_with(&m));
    let body = issue_body(&big, &files, BASE, None);
    assert_eq!(body.chars().count(), 60_000);
}

#[test]
fn permalinks_percent_encode() {
    assert_eq!(
        permalink_base("o", "r", "0123abc", "docs/issues/d-x"),
        "https://github.com/o/r/blob/0123abc/docs/issues/d-x/"
    );
    let b = permalink_base("o", "r", "c", "docs/a b/é");
    assert_eq!(b, "https://github.com/o/r/blob/c/docs/a%20b/%C3%A9/");
    let body = issue_body("[a](a b.png)", &["a b.png".to_string()], "https://h/", None);
    assert_eq!(body, "[a](https://h/a%20b.png?raw=true)");
}

#[test]
fn the_not_done_section_is_found() {
    assert_eq!(
        not_done_section("# E\n\n## Not done\n\nThe project did not start.\n\n## Other").as_deref(),
        Some("The project did not start.")
    );
    assert_eq!(not_done_section("# E\n\n## Done\n\nAll.\n"), None);
    assert_eq!(not_done_section("## Not done\n\n## Next"), None);
    let long = format!("## not done\n{}", "é".repeat(2000));
    assert_eq!(not_done_section(&long).unwrap().chars().count(), 500);
    assert_eq!(
        not_done_section("## Not done\nA\nB\n").as_deref(),
        Some("A B")
    );
}

#[test]
fn mentions_are_neutralised_only_where_a_mention_starts() {
    let z = '\u{200b}';
    for (text, want) in [
        ("@name", format!("@{z}name")),
        ("(@name)", format!("(@{z}name)")),
        ("cc @org/team", format!("cc @{z}org/team")),
        ("line\n@name", format!("line\n@{z}name")),
        ("x, @a and @b", format!("x, @{z}a and @{z}b")),
        ("\u{e9}@name", format!("\u{e9}@{z}name")),
        // D-M3: only a letter, digit or `_` before it keeps an `@` as typed.
        ("x.@name", format!("x.@{z}name")),
        ("x-@name", format!("x-@{z}name")),
        ("pkg/@name", format!("pkg/@{z}name")),
        ("@@name", format!("@@{z}name")),
    ] {
        assert_eq!(no_mentions(text), want, "{text:?}");
    }
    // Not where GitHub reads a mention: kept as typed.
    for kept in [
        "a@b.com",
        "photo@2x.png",
        "x_@y",
        "X9@y",
        "@ y",
        "@-y",
        "@",
        "@\u{e9}",
        "",
    ] {
        assert_eq!(no_mentions(kept), kept, "{kept:?}");
    }
}

#[test]
fn issue_md_keeps_mentions_as_written() {
    let attachments = [FeedbackAttachment {
        kind: AttachmentKind::Image,
        original_name: "@team.png".into(),
        boxes: vec![],
    }];
    let names = file_names(&[(AttachmentKind::Image, FileKind::Png)]);
    let md = render_issue(&IssueInput {
        title: "@octocat the zoom",
        main_idea: "@octocat the zoom\n\nping @hubot",
        kind: FeedbackKind::Bug,
        created: Utc.with_ymd_and_hms(2026, 11, 26, 14, 15, 0).unwrap(),
        app_version: "0.1.0",
        platform: "macos",
        attachments: &attachments,
        names: &names,
    });
    assert!(md.contains("# @octocat the zoom\n"), "{md}");
    assert!(md.contains("## @team.png\n"), "{md}");
    assert!(md.contains("ping @hubot"), "{md}");
    assert!(!md.contains('\u{200b}'), "{md}");
}

#[test]
fn a_pull_request_body_is_the_issue_then_the_explainer() {
    let base = permalink_base("o", "r", "abc123", "docs/issues/2026-11-26-x");
    let files = vec!["issue.md".to_owned(), "diagram-1.png".to_owned()];
    let issue = "---\ntitle: \"X\"\n---\n\n# X\n\nBody.\n";
    let explainer = "# Explainer\n\n![d](diagram-1.png) and [other](elsewhere.png)\n";

    let body = pull_request_body(issue, Some(explainer), &files, &base);

    assert_eq!(
        body,
        "# X\n\nBody.\n\n---\n\n# Explainer\n\n![d](https://github.com/o/r/blob/abc123/docs/issues/2026-11-26-x/diagram-1.png?raw=true) and [other](elsewhere.png)\n"
    );
    assert!(!body.contains("smithagents-feedback:"), "no marker");
    // Without an explainer, the issue alone.
    assert_eq!(
        pull_request_body(issue, None, &files, &base),
        "# X\n\nBody.\n"
    );
    // Together never longer than an issue's body may be.
    let long = "x".repeat(50_000);
    let body = pull_request_body(&long, Some(&long), &files, &base);
    assert_eq!(body.chars().count(), 60_000);
}

#[test]
fn a_whole_comment_arrives_whatever_its_script() {
    // A-I4: capped at 500 characters, never cut by bytes.
    for comment in ["漢".repeat(500), "\u{1f600}".repeat(500)] {
        assert!(comment.len() > 1024);
        let got = render("T", "T", &[shot(&[&comment])], &[FileKind::Png]);
        assert!(
            got.contains(&format!("\n1. {comment}\n")),
            "{}",
            comment.len()
        );
    }
    // Longer than 500 characters: the first 500.
    let long = "漢".repeat(600);
    let got = render("T", "T", &[shot(&[&long])], &[FileKind::Png]);
    assert!(
        got.contains(&format!("\n1. {}\n", "漢".repeat(500))),
        "{got}"
    );
    assert!(!got.contains(&"漢".repeat(501)));
}

#[test]
fn a_file_name_loses_all_of_its_folders() {
    // D-M8: a folder named after a person never reaches a heading.
    for (original, shown) in [
        ("C:\\Users\\Some One\\Desktop\\shot.png", "shot.png"),
        ("/home/some one/pictures/my shot.png", "my shot.png"),
        ("plain name.png", "plain name.png"),
    ] {
        let got = render(
            "T",
            "T",
            &[file(AttachmentKind::Image, original)],
            &[FileKind::Png],
        );
        assert!(
            got.contains(&format!("\n## {shown}\n")),
            "{original}: {got}"
        );
        assert!(
            !got.contains("Some One") && !got.contains("some one"),
            "{got}"
        );
    }
}

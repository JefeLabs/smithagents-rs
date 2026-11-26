mod common;
use common::*;
use serde_json::json;

const FIRST: &str = "https://api.github.com/repos/octo-org/widgets/pulls?state=all&sort=updated&direction=desc&per_page=100";
const PAGE2: &str = "https://api.github.com/repositories/1/pulls?page=2";

fn s() -> serde_json::Value {
    settings(&["octo-org/widgets"])
}

#[test]
fn lists_pull_requests_with_merged_closed_and_draft_states() {
    let host = Scripted::new().ok(FIRST, fixture("pulls_widgets.json"));
    let page = sync(&host, "pull_request", None, s()).unwrap();
    assert!(page.done && !page.snapshot_complete);
    assert_eq!(page.upserts.len(), 3);
    let merged = &page.upserts[0];
    assert_eq!(merged.id, "octo-org/widgets#9");
    assert_eq!(
        merged.url.as_deref(),
        Some("https://github.com/octo-org/widgets/pull/9")
    );
    assert_eq!(merged.fields["state"], json!("closed"));
    assert_eq!(merged.fields["merged_at"], json!("2026-03-01T12:00:00Z"));
    assert_eq!(merged.fields["draft"], json!(false));
    assert_eq!(merged.fields["requested_reviewers"], json!(["hubot"]));
    assert_eq!(merged.fields["head_ref"], json!("spin-fast"));
    assert_eq!(merged.fields["base_ref"], json!("main"));
    assert_eq!(merged.fields["author"], json!("mona"));
    assert_eq!(merged.fields["labels"], json!(["perf"]));
    assert_eq!(merged.fields["repository"], json!("octo-org/widgets"));
    let draft = &page.upserts[1];
    assert_eq!(draft.fields["draft"], json!(true));
    assert_eq!(draft.fields["state"], json!("open"));
    let closed = &page.upserts[2];
    assert_eq!(closed.fields["state"], json!("closed"));
    assert_eq!(closed.fields["merged_at"], json!(null));
    assert_eq!(
        cursor_json(&page)["marks"]["octo-org/widgets"],
        json!("2026-03-01T12:00:00Z")
    );
    assert_all_standard(&host);
}

#[test]
fn stops_after_the_mark_and_does_not_follow_the_next_page() {
    let host = Scripted::new().on(
        FIRST,
        200,
        &[(
            "Link",
            "<https://api.github.com/repositories/1/pulls?page=2>; rel=\"next\"",
        )],
        fixture("pulls_widgets.json"),
    );
    let cursor = r#"{"v":1,"repo":0,"next":null,"marks":{"octo-org/widgets":"2026-02-25T09:00:00Z"},"pending":null}"#;
    // The signature is part of the cursor; a hand-written one restarts the
    // position but keeps the mark, which is what this test needs.
    let page = sync(&host, "pull_request", Some(cursor), s()).unwrap();
    // #9 is newer; #8 is equal to the mark and read again; #7 is older: stop.
    let ids: Vec<_> = page.upserts.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["octo-org/widgets#9", "octo-org/widgets#8"]);
    assert!(page.done);
    assert_eq!(host.urls(), [FIRST]);
}

#[test]
fn pages_on_until_the_mark_or_the_last_page() {
    let host = Scripted::new()
        .on(
            FIRST,
            200,
            &[(
                "Link",
                "<https://api.github.com/repositories/1/pulls?page=2>; rel=\"next\"",
            )],
            fixture("pulls_widgets.json"),
        )
        .ok(PAGE2, b"[]".to_vec());
    let a = sync(&host, "pull_request", None, s()).unwrap();
    assert!(!a.done);
    let b = sync(&host, "pull_request", a.next_cursor.as_deref(), s()).unwrap();
    assert!(b.done && b.upserts.is_empty());
    // The newest timestamp seen on the first page became the mark.
    assert_eq!(
        cursor_json(&b)["marks"]["octo-org/widgets"],
        json!("2026-03-01T12:00:00Z")
    );
    assert_eq!(host.urls(), [FIRST, PAGE2]);
}

#[test]
fn an_unchanged_repository_keeps_its_mark() {
    let host = Scripted::new().ok(FIRST, fixture("pulls_widgets.json"));
    let a = sync(&host, "pull_request", None, s()).unwrap();
    let b = sync(&host, "pull_request", a.next_cursor.as_deref(), s()).unwrap();
    // Only the entry equal to the mark is read again.
    assert_eq!(b.upserts.len(), 1);
    assert_eq!(
        cursor_json(&b)["marks"]["octo-org/widgets"],
        json!("2026-03-01T12:00:00Z")
    );
}

fn pr(n: u64, updated: Option<&str>) -> serde_json::Value {
    let mut v = json!({"number": n, "title": format!("pr {n}"), "state": "open",
                       "html_url": format!("https://github.com/octo-org/widgets/pull/{n}")});
    if let Some(u) = updated {
        v["updated_at"] = json!(u);
    }
    v
}

#[test]
fn a_mid_repository_cursor_carries_pending_and_no_committed_mark() {
    let host = Scripted::new().on(
        FIRST,
        200,
        &[(
            "Link",
            "<https://api.github.com/repositories/1/pulls?page=2>; rel=\"next\"",
        )],
        fixture("pulls_widgets.json"),
    );
    let a = sync(&host, "pull_request", None, s()).unwrap();
    let c = cursor_json(&a);
    assert_eq!(c["pending"], json!("2026-03-01T12:00:00Z"));
    assert!(c["marks"].get("octo-org/widgets").is_none());
    assert_eq!(c["next"], json!(PAGE2));
}

#[test]
fn reaching_the_mark_on_page_two_commits_the_pending_from_page_one() {
    let old = json!([pr(3, Some("2026-01-01T00:00:00Z"))]);
    let mark = r#"{"v":1,"repo":0,"next":null,"marks":{"octo-org/widgets":"2026-02-01T00:00:00Z"},"pending":null}"#;
    let host = Scripted::new()
        .on(
            FIRST,
            200,
            &[(
                "Link",
                "<https://api.github.com/repositories/1/pulls?page=2>; rel=\"next\"",
            )],
            fixture("pulls_widgets.json"),
        )
        .on(
            PAGE2,
            200,
            &[(
                "Link",
                "<https://api.github.com/repositories/1/pulls?page=3>; rel=\"next\"",
            )],
            serde_json::to_vec(&old).unwrap(),
        );
    let a = sync(&host, "pull_request", Some(mark), s()).unwrap();
    assert!(!a.done);
    assert_eq!(cursor_json(&a)["pending"], json!("2026-03-01T12:00:00Z"));
    let b = sync(&host, "pull_request", a.next_cursor.as_deref(), s()).unwrap();
    assert!(b.done && b.upserts.is_empty(), "page 3 is not requested");
    assert_eq!(host.urls(), [FIRST, PAGE2]);
    assert_eq!(
        cursor_json(&b)["marks"]["octo-org/widgets"],
        json!("2026-03-01T12:00:00Z")
    );
}

#[test]
fn an_entry_without_a_usable_updated_at_is_kept_and_moves_nothing() {
    let body = json!([
        pr(5, None),
        pr(4, Some("garbage")),
        pr(3, Some("2026-02-02T00:00:00Z")),
        pr(2, Some("2026-01-01T00:00:00Z"))
    ]);
    let mark = r#"{"v":1,"repo":0,"next":null,"marks":{"octo-org/widgets":"2026-02-01T00:00:00Z"},"pending":null}"#;
    let host = Scripted::new().ok_json(FIRST, &body);
    let page = sync(&host, "pull_request", Some(mark), s()).unwrap();
    let ids: Vec<_> = page.upserts.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "octo-org/widgets#5",
            "octo-org/widgets#4",
            "octo-org/widgets#3"
        ]
    );
    assert_eq!(
        cursor_json(&page)["marks"]["octo-org/widgets"],
        json!("2026-02-02T00:00:00Z")
    );
}

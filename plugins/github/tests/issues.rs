mod common;
use common::*;
use serde_json::json;

const FIRST: &str = "https://api.github.com/repos/octo-org/widgets/issues?state=all&sort=updated&direction=desc&per_page=100";
const SINCE: &str = "https://api.github.com/repos/octo-org/widgets/issues?state=all&sort=updated&direction=desc&per_page=100&since=2026-01-07T09:00:00Z";
const PAGE2: &str = "https://api.github.com/repositories/1/issues?page=2";

fn s() -> serde_json::Value {
    settings(&["octo-org/widgets"])
}

fn two_pages() -> Scripted {
    Scripted::new()
        .on(
            FIRST,
            200,
            &[("Link", "<https://api.github.com/repositories/1/issues?page=2>; rel=\"next\", <https://api.github.com/repositories/1/issues?page=2>; rel=\"last\"")],
            fixture("issues_page1.json"),
        )
        .ok(PAGE2, fixture("issues_page2.json"))
        .ok(SINCE, b"[]".to_vec())
}

#[test]
fn follows_link_pages_skips_pull_requests_and_sends_since_on_the_next_run() {
    let host = two_pages();
    let p1 = sync(&host, "issue", None, s()).unwrap();
    assert!(!p1.done && !p1.snapshot_complete);
    assert_eq!(p1.upserts.len(), 1, "the pull request entry is skipped");
    let issue = &p1.upserts[0];
    assert_eq!(issue.id, "octo-org/widgets#1");
    assert_eq!(issue.title, "Widget does not spin");
    assert_eq!(
        issue.url.as_deref(),
        Some("https://github.com/octo-org/widgets/issues/1")
    );
    assert_eq!(issue.version.as_deref(), Some("2026-01-05T09:00:00Z"));
    assert_eq!(issue.fields["state"], json!("closed"));
    assert_eq!(issue.fields["state_reason"], json!("completed"));
    assert_eq!(issue.fields["author"], json!("mona"));
    assert_eq!(issue.fields["labels"], json!(["bug", "help wanted"]));
    assert_eq!(issue.fields["assignees"], json!(["hubot"]));
    assert_eq!(issue.fields["comments"], json!(3));
    assert_eq!(issue.fields["closed_at"], json!("2026-01-05T09:00:00Z"));
    assert_eq!(issue.fields["repository"], json!("octo-org/widgets"));
    assert!(!serde_json::to_string(&issue.fields)
        .unwrap()
        .contains("secret-looking"));

    // Mid-repository the newest timestamp is carried as pending, not committed.
    let mid = cursor_json(&p1);
    assert_eq!(mid["pending"], json!("2026-01-06T09:00:00Z"));
    assert!(mid["marks"].get("octo-org/widgets").is_none());
    assert_eq!(mid["next"], json!(PAGE2));

    let p2 = sync(&host, "issue", p1.next_cursor.as_deref(), s()).unwrap();
    assert!(p2.done && !p2.snapshot_complete);
    assert_eq!(p2.upserts[0].id, "octo-org/widgets#3");
    assert_eq!(host.urls(), [FIRST, PAGE2]);

    // The mark is the newest updated_at seen, including the skipped pull request's.
    assert_eq!(
        cursor_json(&p2)["marks"]["octo-org/widgets"],
        json!("2026-01-07T09:00:00Z")
    );

    let p3 = sync(&host, "issue", p2.next_cursor.as_deref(), s()).unwrap();
    assert_eq!(host.urls().last().unwrap(), SINCE);
    assert!(p3.upserts.is_empty() && p3.done);
    assert_all_standard(&host);
}

#[test]
fn a_next_link_on_another_host_is_not_followed() {
    for link in [
        "<https://evil.example/issues?page=2>; rel=\"next\"",
        "<http://api.github.com/issues?page=2>; rel=\"next\"",
        "<https://api.github.com.evil.example/x>; rel=\"next\"",
        "<https://api.github.com/issues?page=2>; rel=\"prev\"",
        "garbage",
    ] {
        let host = Scripted::new().on(FIRST, 200, &[("Link", link)], fixture("issues_page2.json"));
        let page = sync(&host, "issue", None, s()).unwrap();
        assert!(page.done, "{link}");
        assert_eq!(host.urls(), [FIRST]);
    }
}

#[test]
fn walks_repositories_in_sorted_order_and_finishes_after_the_last() {
    let g = "https://api.github.com/repos/octo-org/gadgets/issues?state=all&sort=updated&direction=desc&per_page=100";
    let host = Scripted::new()
        .ok(g, fixture("issues_page2.json"))
        .ok(FIRST, fixture("issues_page2.json"));
    let both = settings(&["octo-org/widgets", "octo-org/gadgets"]);
    let a = sync(&host, "issue", None, both.clone()).unwrap();
    assert!(!a.done);
    let b = sync(&host, "issue", a.next_cursor.as_deref(), both.clone()).unwrap();
    assert!(b.done);
    assert_eq!(host.urls(), [g, FIRST]);
    let marks = &cursor_json(&b)["marks"];
    assert_eq!(marks["octo-org/gadgets"], json!("2026-01-07T09:00:00Z"));
    assert_eq!(marks["octo-org/widgets"], json!("2026-01-07T09:00:00Z"));
}

#[test]
fn an_unparsable_cursor_restarts_from_the_beginning() {
    for bad in [
        "not json",
        "{}",
        "{\"v\":2,\"repo\":0,\"next\":null,\"marks\":{},\"pending\":null}",
        "[1]",
    ] {
        let host = Scripted::new().ok(FIRST, fixture("issues_page2.json"));
        let page = sync(&host, "issue", Some(bad), s()).unwrap();
        assert_eq!(host.urls(), [FIRST], "{bad}");
        assert_eq!(page.upserts.len(), 1);
    }
}

#[test]
fn a_repository_removed_from_settings_loses_its_mark() {
    let g = "https://api.github.com/repos/octo-org/gadgets/issues?state=all&sort=updated&direction=desc&per_page=100";
    let host = Scripted::new()
        .ok(g, fixture("issues_page2.json"))
        .ok(FIRST, fixture("issues_page2.json"))
        .ok(SINCE, b"[]".to_vec());
    let both = settings(&["octo-org/widgets", "octo-org/gadgets"]);
    let a = sync(&host, "issue", None, both.clone()).unwrap();
    let b = sync(&host, "issue", a.next_cursor.as_deref(), both).unwrap();
    assert!(cursor_json(&b)["marks"].get("octo-org/gadgets").is_some());
    let c = sync(&host, "issue", b.next_cursor.as_deref(), s()).unwrap();
    let marks = &cursor_json(&c)["marks"];
    assert!(marks.get("octo-org/gadgets").is_none());
    assert!(marks.get("octo-org/widgets").is_some());
    assert_eq!(host.urls().last().unwrap(), SINCE);
}

#[test]
fn a_hostile_next_url_in_the_cursor_is_ignored() {
    let host = Scripted::new().ok(FIRST, fixture("issues_page2.json"));
    let cursor = r#"{"v":1,"repo":0,"next":"https://evil.example/x","marks":{},"pending":null}"#;
    sync(&host, "issue", Some(cursor), s()).unwrap();
    assert_eq!(host.urls(), [FIRST]);
}

#[test]
fn a_hundred_long_issues_stay_within_the_contract_and_bodies_are_not_stored() {
    let long = "x".repeat(10_000);
    let entries: Vec<_> = (1..=100)
        .map(|n| {
            json!({
                "number": n,
                "title": format!("{n} {long}"),
                "html_url": format!("https://github.com/octo-org/widgets/issues/{n}"),
                "state": "open",
                "user": {"login": "mona"},
                "labels": [{"name": "bug"}],
                "assignees": [],
                "comments": 0,
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-02T00:00:00Z",
                "closed_at": null,
                "body": long,
            })
        })
        .collect();
    let host = Scripted::new().ok_json(FIRST, &json!(entries));
    let page = sync(&host, "issue", None, s()).unwrap();
    assert_eq!(page.upserts.len(), 100);
    assert!(page.upserts.iter().all(|i| i.title.chars().count() == 512));
    assert!(page
        .upserts
        .iter()
        .all(|i| !serde_json::to_string(&i.fields).unwrap().contains("xxxx")));
}

#[test]
fn a_bad_html_url_is_dropped_not_passed_on() {
    let body = json!([{"number": 1, "title": "t", "html_url": "javascript:alert(1)", "state": "open", "updated_at": "2026-01-02T00:00:00Z"}]);
    let host = Scripted::new().ok_json(FIRST, &body);
    // validate() inside `sync` would fail if the URL were passed on.
    let page = sync(&host, "issue", None, s()).unwrap();
    assert!(page.upserts[0].url.is_none());
}

#[test]
fn fifty_repositories_and_a_long_next_url_fit_the_cursor() {
    let names: Vec<String> = (0..50)
        .map(|i| format!("octo-org/widgets-{i:02}"))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    // Fill every mark by walking the whole list, then check the size again.
    let mut host = Scripted::new();
    for n in &names {
        host = host.ok(
            &format!("https://api.github.com/repos/{n}/issues?state=all&sort=updated&direction=desc&per_page=100"),
            fixture("issues_page2.json"),
        );
    }
    let mut cursor: Option<String> = None;
    loop {
        let page = sync(&host, "issue", cursor.as_deref(), settings(&refs)).unwrap();
        cursor = page.next_cursor;
        if page.done {
            break;
        }
    }
    assert_eq!(
        cursor_json_str(cursor.as_deref().unwrap())["marks"]
            .as_object()
            .unwrap()
            .len(),
        50
    );
    let done_cursor = cursor.unwrap();
    assert!(done_cursor.len() < 4096);

    // All 50 repositories are marked; one more page with a long next URL on
    // top of that still fits.
    let first = format!(
        "https://api.github.com/repos/{}/issues?state=all&sort=updated&direction=desc&per_page=100&since=2026-01-07T09:00:00Z",
        names[0]
    );
    let next = format!(
        "https://api.github.com/repositories/1/issues?page=2&x={}",
        "a".repeat(400)
    );
    let host = Scripted::new().on(
        &first,
        200,
        &[("Link", &format!("<{next}>; rel=\"next\""))],
        fixture("issues_page2.json"),
    );
    let page = sync(&host, "issue", Some(&done_cursor), settings(&refs)).unwrap();
    let c = cursor_json(&page);
    assert_eq!(c["marks"].as_object().unwrap().len(), 50);
    assert!(c["pending"].is_string() && c["next"].is_string());
    assert!(page.next_cursor.unwrap().len() < 4096);
}

fn cursor_json_str(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap()
}

#[test]
fn a_cursor_that_would_overflow_is_an_error_not_an_invalid_page() {
    let next = format!(
        "https://api.github.com/repositories/1/issues?page=2&x={}",
        "a".repeat(4200)
    );
    let host = Scripted::new().on(
        FIRST,
        200,
        &[("Link", &format!("<{next}>; rel=\"next\""))],
        fixture("issues_page2.json"),
    );
    let err = sync(&host, "issue", None, s()).unwrap_err();
    assert!(
        matches!(err, smithagents_plugin_protocol::PluginError::Other { .. }),
        "{err:?}"
    );
}

fn entry(n: u64, updated: &str) -> serde_json::Value {
    json!({"number": n, "title": format!("issue {n}"), "state": "open",
           "html_url": format!("https://github.com/octo-org/widgets/issues/{n}"),
           "updated_at": updated, "user": {"login": "mona"}})
}

fn ids(page: &smithagents_plugin_protocol::SyncPage) -> Vec<&str> {
    page.upserts.iter().map(|i| i.id.as_str()).collect()
}

#[test]
fn a_page_shifted_by_one_between_pages_loses_nothing() {
    // Between page 1 and page 2 issue 4 was updated, so it moved up and page 2
    // repeats the boundary entry. Newest first, nothing is skipped.
    let page1 = json!([
        entry(5, "2026-01-09T00:00:00Z"),
        entry(4, "2026-01-08T00:00:00Z")
    ]);
    let page2 = json!([
        entry(4, "2026-01-08T00:00:00Z"),
        entry(3, "2026-01-07T00:00:00Z")
    ]);
    let host = Scripted::new()
        .on(
            FIRST,
            200,
            &[(
                "Link",
                "<https://api.github.com/repositories/1/issues?page=2>; rel=\"next\"",
            )],
            serde_json::to_vec(&page1).unwrap(),
        )
        .ok_json(PAGE2, &page2);
    let a = sync(&host, "issue", None, s()).unwrap();
    let b = sync(&host, "issue", a.next_cursor.as_deref(), s()).unwrap();
    let mut all: Vec<&str> = ids(&a).into_iter().chain(ids(&b)).collect();
    all.sort();
    all.dedup();
    assert_eq!(
        all,
        [
            "octo-org/widgets#3",
            "octo-org/widgets#4",
            "octo-org/widgets#5"
        ]
    );
    assert!(b.done);
    assert_eq!(
        cursor_json(&b)["marks"]["octo-org/widgets"],
        json!("2026-01-09T00:00:00Z")
    );
}

#[test]
fn does_not_stop_at_the_mark_it_follows_links_to_the_end() {
    let cursor = r#"{"v":1,"repo":0,"next":null,"marks":{"octo-org/widgets":"2026-01-07T09:00:00Z"},"pending":null}"#;
    let host = Scripted::new()
        .on(
            SINCE,
            200,
            &[(
                "Link",
                "<https://api.github.com/repositories/1/issues?page=2>; rel=\"next\"",
            )],
            fixture("issues_page2.json"),
        )
        .ok(PAGE2, b"[]".to_vec());
    let a = sync(&host, "issue", Some(cursor), s()).unwrap();
    assert!(!a.done);
    let b = sync(&host, "issue", a.next_cursor.as_deref(), s()).unwrap();
    assert!(b.done);
    assert_eq!(host.urls(), [SINCE, PAGE2]);
}

#[test]
fn a_response_cannot_make_an_invalid_page() {
    // 600 entries: only 500 are taken.
    let many: Vec<_> = (1..=600)
        .map(|n| entry(n, "2026-01-02T00:00:00Z"))
        .collect();
    let host = Scripted::new().ok_json(FIRST, &json!(many));
    assert_eq!(sync(&host, "issue", None, s()).unwrap().upserts.len(), 500);

    // The same number twice in one response: one item.
    let dup = json!([
        entry(1, "2026-01-02T00:00:00Z"),
        entry(1, "2026-01-03T00:00:00Z"),
        entry(2, "2026-01-01T00:00:00Z")
    ]);
    let host = Scripted::new().ok_json(FIRST, &dup);
    assert_eq!(
        ids(&sync(&host, "issue", None, s()).unwrap()),
        ["octo-org/widgets#1", "octo-org/widgets#2"]
    );

    // Nested values are not copied and long strings are capped at 1 KiB.
    let mut odd = entry(1, "2026-01-02T00:00:00Z");
    odd["state"] = json!({"deep": [1, 2, 3]});
    odd["comments"] = json!([1]);
    odd["state_reason"] = json!("y".repeat(5000));
    odd["user"] = json!({"login": "z".repeat(5000)});
    let host = Scripted::new().ok_json(FIRST, &json!([odd]));
    let page = sync(&host, "issue", None, s()).unwrap();
    let f = &page.upserts[0].fields;
    assert_eq!(f["state"], json!(null));
    assert_eq!(f["comments"], json!(null));
    assert_eq!(f["state_reason"].as_str().unwrap().len(), 1024);
    assert_eq!(f["author"].as_str().unwrap().len(), 1024);

    // An item whose fields still exceed 64 KiB is dropped; the others stay.
    let mut big = entry(1, "2026-01-02T00:00:00Z");
    big["labels"] = json!((0..100)
        .map(|i| json!({"name": format!("{i}{}", "l".repeat(1000))}))
        .collect::<Vec<_>>());
    big["assignees"] = json!((0..100)
        .map(|_| json!({"login": "a".repeat(1000)}))
        .collect::<Vec<_>>());
    let host = Scripted::new().ok_json(FIRST, &json!([big, entry(2, "2026-01-01T00:00:00Z")]));
    let page = sync(&host, "issue", None, s()).unwrap();
    assert_eq!(ids(&page), ["octo-org/widgets#2"]);
}

#[test]
fn an_implausible_updated_at_does_not_move_the_mark_but_the_item_is_kept() {
    let date = [("Date", "Thu, 26 Nov 2026 10:00:00 GMT")];
    let future = json!([
        entry(1, "2999-01-01T00:00:00Z"),
        entry(2, "2026-11-25T00:00:00Z")
    ]);
    let host = Scripted::new().on(FIRST, 200, &date, serde_json::to_vec(&future).unwrap());
    let page = sync(&host, "issue", None, s()).unwrap();
    assert_eq!(page.upserts.len(), 2);
    assert_eq!(
        cursor_json(&page)["marks"]["octo-org/widgets"],
        json!("2026-11-25T00:00:00Z")
    );

    let only_bad = json!([entry(1, "2999-01-01T00:00:00Z"), entry(2, "not a time")]);
    let host = Scripted::new().on(FIRST, 200, &date, serde_json::to_vec(&only_bad).unwrap());
    let page = sync(&host, "issue", None, s()).unwrap();
    assert_eq!(page.upserts.len(), 2);
    assert!(cursor_json(&page)["marks"]
        .get("octo-org/widgets")
        .is_none());
}

#[test]
fn repository_names_are_deduplicated_ignoring_case_keeping_the_first_spelling() {
    let url = "https://api.github.com/repos/octo-org/Widgets/issues?state=all&sort=updated&direction=desc&per_page=100";
    let host2 = Scripted::new().ok(url, b"[]".to_vec());
    let page = sync(
        &host2,
        "issue",
        None,
        settings(&["octo-org/Widgets", "octo-org/widgets"]),
    )
    .unwrap();
    assert!(page.done, "one repository, not two");
    assert_eq!(host2.urls(), [url]);
}

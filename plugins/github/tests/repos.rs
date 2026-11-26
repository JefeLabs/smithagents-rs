mod common;
use common::*;
use serde_json::json;
use smithagents_plugin_protocol::Manifest;

const GADGETS: &str = "https://api.github.com/repos/octo-org/gadgets";
const WIDGETS: &str = "https://api.github.com/repos/octo-org/widgets";

#[test]
fn lists_one_repository_per_call_and_completes_the_snapshot_on_the_last() {
    let host = Scripted::new()
        .ok(GADGETS, fixture("repo_gadgets.json"))
        .ok(WIDGETS, fixture("repo_widgets.json"));
    let s = settings(&["octo-org/widgets", "octo-org/gadgets"]);

    let first = sync(&host, "repository", None, s.clone()).unwrap();
    assert_eq!(host.urls(), [GADGETS]);
    assert!(!first.done && !first.snapshot_complete);
    assert_eq!(first.upserts.len(), 1);
    let item = &first.upserts[0];
    assert_eq!(item.id, "octo-org/gadgets");
    assert_eq!(
        item.url.as_deref(),
        Some("https://github.com/octo-org/gadgets")
    );
    assert_eq!(item.fields["private"], json!(true));
    assert_eq!(item.fields["archived"], json!(true));
    assert_eq!(item.fields["default_branch"], json!("trunk"));
    assert_eq!(item.fields["description"], json!(null));

    let second = sync(&host, "repository", first.next_cursor.as_deref(), s.clone()).unwrap();
    assert_eq!(host.urls(), [GADGETS, WIDGETS]);
    assert!(second.done && second.snapshot_complete);
    let item = &second.upserts[0];
    assert_eq!(item.id, "octo-org/widgets");
    assert_eq!(item.fields["description"], json!("Reusable widgets"));
    assert_eq!(item.fields["open_issues_count"], json!(4));
    assert_eq!(item.fields["stargazers_count"], json!(12));
    assert_eq!(item.fields["pushed_at"], json!("2026-03-02T10:00:00Z"));
    assert_all_standard(&host);

    // After `done` the next call begins a fresh listing at the first repository.
    let third = sync(&host, "repository", second.next_cursor.as_deref(), s).unwrap();
    assert_eq!(host.urls().last().unwrap(), GADGETS);
    assert_eq!(third.upserts[0].id, "octo-org/gadgets");
    assert!(!third.snapshot_complete);
}

#[test]
fn a_single_repository_is_a_complete_snapshot_at_once() {
    let host = Scripted::new().ok(WIDGETS, fixture("repo_widgets.json"));
    let page = sync(&host, "repository", None, settings(&["octo-org/widgets"])).unwrap();
    assert!(page.done && page.snapshot_complete);
}

#[test]
fn a_stale_cursor_after_the_list_changed_restarts_and_never_claims_a_snapshot_midway() {
    let aardvark = "https://api.github.com/repos/octo-org/aardvark";
    let host = Scripted::new()
        .ok(GADGETS, fixture("repo_gadgets.json"))
        .ok(aardvark, fixture("repo_widgets.json"));
    let first = sync(
        &host,
        "repository",
        None,
        settings(&["octo-org/widgets", "octo-org/gadgets"]),
    )
    .unwrap();
    // The list changed between calls: the saved position no longer means
    // anything, so the listing restarts at the new first repository.
    let three = settings(&["octo-org/widgets", "octo-org/gadgets", "octo-org/aardvark"]);
    let page = sync(&host, "repository", first.next_cursor.as_deref(), three).unwrap();
    assert_eq!(host.urls(), [GADGETS, aardvark]);
    assert!(!page.done && !page.snapshot_complete);
}

#[test]
fn an_empty_list_is_an_empty_done_page() {
    let host = Scripted::new();
    let page = sync(&host, "repository", None, settings(&[])).unwrap();
    assert!(page.upserts.is_empty() && page.done);
    assert!(
        page.snapshot_complete,
        "the user listed nothing: a complete empty listing"
    );
    assert!(host.requests().is_empty());
    for resource in ["issue", "pull_request"] {
        let page = sync(&host, resource, None, settings(&[])).unwrap();
        assert!(page.done && !page.snapshot_complete, "{resource}");
    }
}

#[test]
fn the_manifest_parses() {
    let m = Manifest::parse(include_str!("../plugin.toml")).unwrap();
    assert_eq!(m.id.to_string(), "github");
    assert_eq!(m.hosts, ["api.github.com"]);
    assert_eq!(m.tools[0].name, "gh");
    let secret = &m.secrets[0];
    assert_eq!(secret.key, "token");
    assert_eq!(secret.from_tool.as_ref().unwrap().args, ["auth", "token"]);
    let kinds: Vec<_> = m
        .resources
        .iter()
        .map(|r| (r.kind.as_str(), r.interval_secs))
        .collect();
    assert_eq!(
        kinds,
        [("repository", 3600), ("issue", 300), ("pull_request", 300)]
    );
}

#[test]
fn a_long_description_is_capped_and_nested_values_are_not_copied() {
    let body = json!({"html_url": "https://github.com/octo-org/widgets",
                      "description": "d".repeat(5000), "default_branch": {"x": 1},
                      "stargazers_count": [1], "private": false});
    let host = Scripted::new().ok_json(WIDGETS, &body);
    let page = sync(&host, "repository", None, settings(&["octo-org/widgets"])).unwrap();
    let f = &page.upserts[0].fields;
    assert_eq!(f["description"].as_str().unwrap().len(), 2048);
    assert_eq!(f["default_branch"], json!(null));
    assert_eq!(f["stargazers_count"], json!(null));
}

//! Plugin configuration, sync state and resource items in the store.

use chrono::{DateTime, Utc};
use serde_json::json;
use smithagents_plugin_protocol::{Item, SyncPage};
use smithagents_server::store::{PluginConfig, Store, StoreError, SyncState};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};

fn time(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn item(id: &str, title: &str, updated: Option<&str>) -> Item {
    Item {
        id: id.into(),
        title: title.into(),
        url: Some(format!("https://example.com/{id}")),
        updated_at: updated.map(time),
        version: Some("1".into()),
        fields: json!({"n": id}),
    }
}

fn page(upserts: Vec<Item>, deletes: &[&str], cursor: Option<&str>) -> SyncPage {
    SyncPage {
        upserts,
        deletes: deletes.iter().map(|s| (*s).into()).collect(),
        next_cursor: cursor.map(Into::into),
        done: false,
        snapshot_complete: false,
    }
}

const AT: &str = "2026-11-26T10:00:00Z";

async fn ids(store: &Store, plugin: &str, resource: &str) -> Vec<String> {
    let mut out = vec![];
    let mut after: Option<String> = None;
    loop {
        let p = store
            .list_items(plugin, resource, 200, after.as_deref(), None)
            .await
            .unwrap();
        out.extend(p.items.into_iter().map(|i| i.id));
        match p.next {
            Some(n) => after = Some(n),
            None => return out,
        }
    }
}

#[tokio::test]
async fn config_round_trips_and_is_absent_by_default() {
    let store = Store::open_in_memory().await.unwrap();
    assert!(store.plugin_config("github").await.unwrap().is_none());
    let cfg = PluginConfig {
        enabled: true,
        settings: json!({"org": "acme"}),
    };
    store.set_plugin_config("github", &cfg).await.unwrap();
    let got = store.plugin_config("github").await.unwrap().unwrap();
    assert!(got.enabled);
    assert_eq!(got.settings, json!({"org": "acme"}));
    store
        .set_plugin_config(
            "github",
            &PluginConfig {
                enabled: false,
                settings: json!({}),
            },
        )
        .await
        .unwrap();
    assert!(
        !store
            .plugin_config("github")
            .await
            .unwrap()
            .unwrap()
            .enabled
    );
}

#[tokio::test]
async fn sync_state_defaults_and_round_trips() {
    let store = Store::open_in_memory().await.unwrap();
    let d = store.sync_state("p", "issues").await.unwrap();
    assert_eq!(d, SyncState::default());
    assert_eq!(d.failures, 0);
    let s = SyncState {
        cursor: Some("c".into()),
        last_run_at: Some(time(AT)),
        last_ok_at: Some(time("2026-11-26T09:00:00Z")),
        last_error: Some("boom".into()),
        next_due_at: Some(time("2026-11-26T11:00:00Z")),
        failures: 3,
        listing_id: Some("listing-1".into()),
        listing_from_start: true,
    };
    store.save_sync_state("p", "issues", &s).await.unwrap();
    assert_eq!(store.sync_state("p", "issues").await.unwrap(), s);
    store
        .save_sync_state("p", "issues", &SyncState::default())
        .await
        .unwrap();
    assert_eq!(
        store.sync_state("p", "issues").await.unwrap(),
        SyncState::default()
    );
}

#[tokio::test]
async fn three_pages_read_back_in_order_across_two_list_pages() {
    let store = Store::open_in_memory().await.unwrap();
    let t = |n: u32| format!("2026-11-{n:02}T00:00:00+02:00");
    let a = vec![
        item("a", "A", Some(&t(5))),
        item("b", "B", Some(&t(7))),
        item("n1", "N1", None),
    ];
    let b = vec![
        item("c", "C", Some(&t(7))),
        item("d", "D", Some(&t(2))),
        item("n0", "N0", None),
    ];
    let c = vec![item("e", "E", Some(&t(9)))];
    for (p, cur) in [(a, "1"), (b, "2"), (c, "3")] {
        store
            .apply_page(
                "p",
                "r",
                "run1",
                &page(p, &[], Some(cur)),
                time(AT),
                None,
                false,
            )
            .await
            .unwrap();
    }
    assert_eq!(
        store.sync_state("p", "r").await.unwrap().cursor.as_deref(),
        Some("3")
    );
    assert_eq!(store.count_items("p", "r").await.unwrap(), 7);

    let first = store.list_items("p", "r", 4, None, None).await.unwrap();
    assert_eq!(first.items.len(), 4);
    let next = first.next.clone().expect("more items");
    let second = store
        .list_items("p", "r", 4, Some(&next), None)
        .await
        .unwrap();
    assert_eq!(second.items.len(), 3);
    assert!(second.next.is_none());
    let got: Vec<_> = first
        .items
        .iter()
        .chain(&second.items)
        .map(|i| i.id.as_str())
        .collect();
    // Newest first (by instant, not by text offset), ties by id, nulls last.
    assert_eq!(got, ["e", "b", "c", "a", "d", "n0", "n1"]);
    assert_eq!(
        first.items[0].updated_at,
        Some(time("2026-11-09T00:00:00+02:00"))
    );

    // Paging by one never repeats or skips.
    let mut seen = vec![];
    let mut after: Option<String> = None;
    loop {
        let p = store
            .list_items("p", "r", 1, after.as_deref(), None)
            .await
            .unwrap();
        seen.extend(p.items.into_iter().map(|i| i.id));
        match p.next {
            Some(n) => after = Some(n),
            None => break,
        }
    }
    assert_eq!(seen, got);
}

#[tokio::test]
async fn a_page_stores_its_listing_with_its_cursor() {
    let store = Store::open_in_memory().await.unwrap();
    let listing = |s: SyncState| (s.cursor, s.listing_id, s.listing_from_start);
    store
        .apply_page(
            "p",
            "r",
            "l1",
            &page(vec![item("a", "A", None)], &[], Some("1")),
            time(AT),
            Some("l1"),
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        listing(store.sync_state("p", "r").await.unwrap()),
        (Some("1".into()), Some("l1".into()), true)
    );
    store
        .apply_page(
            "p",
            "r",
            "l1",
            &page(vec![], &[], Some("2")),
            time(AT),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        listing(store.sync_state("p", "r").await.unwrap()),
        (Some("2".into()), None, false)
    );
    // The rest of the state is left alone.
    let saved = SyncState {
        failures: 2,
        last_error: Some("e".into()),
        ..store.sync_state("p", "r").await.unwrap()
    };
    store.save_sync_state("p", "r", &saved).await.unwrap();
    store
        .apply_page(
            "p",
            "r",
            "l2",
            &page(vec![], &[], Some("3")),
            time(AT),
            Some("l2"),
            false,
        )
        .await
        .unwrap();
    let after = store.sync_state("p", "r").await.unwrap();
    assert_eq!(
        (after.failures, after.last_error.as_deref()),
        (2, Some("e"))
    );
    assert_eq!(listing(after), (Some("3".into()), Some("l2".into()), false));
}

#[tokio::test]
async fn limit_is_clamped() {
    let store = Store::open_in_memory().await.unwrap();
    let items: Vec<_> = (0..3).map(|i| item(&format!("i{i}"), "T", None)).collect();
    store
        .apply_page(
            "p",
            "r",
            "run",
            &page(items, &[], None),
            time(AT),
            None,
            false,
        )
        .await
        .unwrap();
    let p = store.list_items("p", "r", 0, None, None).await.unwrap();
    assert_eq!(p.items.len(), 1);
    assert!(p.next.is_some());
    let p = store
        .list_items("p", "r", 100_000, None, None)
        .await
        .unwrap();
    assert_eq!(p.items.len(), 3);
}

#[tokio::test]
async fn reapplying_an_identical_page_changes_nothing_but_the_run() {
    let store = Store::open_in_memory().await.unwrap();
    let p = page(
        vec![item("a", "A", Some(AT)), item("b", "B", None)],
        &[],
        Some("c"),
    );
    assert_eq!(
        store
            .apply_page("p", "r", "run1", &p, time(AT), None, false)
            .await
            .unwrap(),
        (2, 0)
    );
    assert_eq!(
        store
            .apply_page("p", "r", "run2", &p, time(AT), None, false)
            .await
            .unwrap(),
        (0, 0)
    );
    // The unchanged rows carry run2 now, so run2 leaves them alone.
    assert_eq!(store.delete_unseen("p", "r", "run2").await.unwrap(), 0);
    assert_eq!(store.count_items("p", "r").await.unwrap(), 2);
}

#[tokio::test]
async fn each_changed_column_counts_once() {
    let store = Store::open_in_memory().await.unwrap();
    let base = item("a", "A", Some(AT));
    store
        .apply_page(
            "p",
            "r",
            "r1",
            &page(vec![base.clone()], &[], None),
            time(AT),
            None,
            false,
        )
        .await
        .unwrap();
    let variants: Vec<Item> = vec![
        Item {
            title: "A2".into(),
            ..base.clone()
        },
        Item {
            url: None,
            ..base.clone()
        },
        Item {
            updated_at: None,
            ..base.clone()
        },
        Item {
            version: Some("2".into()),
            ..base.clone()
        },
        Item {
            fields: json!({"n": "other"}),
            ..base.clone()
        },
    ];
    for (i, v) in variants.into_iter().enumerate() {
        let run = format!("v{i}");
        assert_eq!(
            store
                .apply_page(
                    "p",
                    "r",
                    &run,
                    &page(vec![v], &[], None),
                    time(AT),
                    None,
                    false
                )
                .await
                .unwrap(),
            (1, 0),
            "variant {i}"
        );
        // Put it back so each variant differs from the stored row in one column.
        store
            .apply_page(
                "p",
                "r",
                &format!("b{i}"),
                &page(vec![base.clone()], &[], None),
                time(AT),
                None,
                false,
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn deletes_remove_rows_and_count_only_existing() {
    let store = Store::open_in_memory().await.unwrap();
    store
        .apply_page(
            "p",
            "r",
            "r1",
            &page(vec![item("a", "A", None), item("b", "B", None)], &[], None),
            time(AT),
            None,
            false,
        )
        .await
        .unwrap();
    let counts = store
        .apply_page(
            "p",
            "r",
            "r2",
            &page(vec![], &["a", "missing"], None),
            time(AT),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(counts, (0, 1));
    assert_eq!(ids(&store, "p", "r").await, ["b"]);
}

#[tokio::test]
async fn delete_unseen_removes_exactly_the_untouched_rows() {
    let store = Store::open_in_memory().await.unwrap();
    store
        .apply_page(
            "p",
            "r",
            "old",
            &page(
                vec![
                    item("a", "A", None),
                    item("b", "B", None),
                    item("c", "C", None),
                ],
                &[],
                None,
            ),
            time(AT),
            None,
            false,
        )
        .await
        .unwrap();
    // `a` is unchanged, `b` changed, `c` is not mentioned.
    store
        .apply_page(
            "p",
            "r",
            "new",
            &page(vec![item("a", "A", None), item("b", "B2", None)], &[], None),
            time(AT),
            None,
            false,
        )
        .await
        .unwrap();
    store
        .apply_page(
            "q",
            "r",
            "old",
            &page(vec![item("z", "Z", None)], &[], None),
            time(AT),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(store.delete_unseen("p", "r", "new").await.unwrap(), 1);
    assert_eq!(ids(&store, "p", "r").await, ["a", "b"]);
    assert_eq!(ids(&store, "q", "r").await, ["z"]);
}

/// The failing statement is a trigger, created through a second connection to
/// the same file, that aborts the insert of the item `boom`. It fires on the
/// second upsert of the page every time, so the rollback is deterministic.
#[tokio::test]
async fn a_failing_statement_rolls_the_whole_page_back() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("s.db");
    let store = Store::open(&db).await.unwrap();
    store
        .apply_page(
            "p",
            "r",
            "r0",
            &page(vec![item("keep", "K", None)], &[], Some("c0")),
            time(AT),
            None,
            false,
        )
        .await
        .unwrap();

    let mut conn = SqliteConnectOptions::new()
        .filename(&db)
        .connect()
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER boom BEFORE INSERT ON resource_items WHEN NEW.item_id = 'boom' \
         BEGIN SELECT RAISE(ABORT, 'boom'); END",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    conn.close().await.unwrap();

    let bad = page(
        vec![item("fresh", "F", None), item("boom", "B", None)],
        &["keep"],
        Some("c1"),
    );
    let err = store
        .apply_page("p", "r", "r1", &bad, time(AT), None, false)
        .await;
    assert!(matches!(err, Err(StoreError::Sqlx(_))), "{err:?}");
    assert_eq!(ids(&store, "p", "r").await, ["keep"]);
    assert_eq!(
        store.sync_state("p", "r").await.unwrap().cursor.as_deref(),
        Some("c0")
    );
}

/// The last statement of the page, the cursor write, fails: the trigger aborts
/// every insert into and update of `resource_sync`. Everything before it
/// (the upserts and the delete of `keep`) must be undone with it.
#[tokio::test]
async fn a_failing_cursor_write_rolls_back_the_upserts_and_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("s.db");
    let store = Store::open(&db).await.unwrap();
    store
        .apply_page(
            "p",
            "r",
            "r0",
            &page(vec![item("keep", "K", None)], &[], Some("c0")),
            time(AT),
            Some("r0"),
            true,
        )
        .await
        .unwrap();

    let mut conn = SqliteConnectOptions::new()
        .filename(&db)
        .connect()
        .await
        .unwrap();
    for sql in [
        "CREATE TRIGGER no_cursor_insert BEFORE INSERT ON resource_sync \
         BEGIN SELECT RAISE(ABORT, 'no cursor'); END",
        "CREATE TRIGGER no_cursor_update BEFORE UPDATE ON resource_sync \
         BEGIN SELECT RAISE(ABORT, 'no cursor'); END",
    ] {
        sqlx::query(sql).execute(&mut conn).await.unwrap();
    }
    conn.close().await.unwrap();

    let bad = page(
        vec![item("fresh", "F", None), item("keep", "K2", None)],
        &["keep"],
        Some("c1"),
    );
    let before = store.sync_state("p", "r").await.unwrap();
    let err = store
        .apply_page("p", "r", "r1", &bad, time(AT), Some("r1"), false)
        .await;
    assert!(matches!(err, Err(StoreError::Sqlx(_))), "{err:?}");
    assert_eq!(store.sync_state("p", "r").await.unwrap(), before);
    assert_eq!(before.listing_id.as_deref(), Some("r0"));
    let page = store.list_items("p", "r", 10, None, None).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, "keep");
    assert_eq!(page.items[0].title, "K");
    assert_eq!(
        store.sync_state("p", "r").await.unwrap().cursor.as_deref(),
        Some("c0")
    );
}

#[tokio::test]
async fn q_escapes_like_wildcards_and_ignores_case() {
    let store = Store::open_in_memory().await.unwrap();
    store
        .apply_page(
            "p",
            "r",
            "r1",
            &page(
                vec![
                    item("1", "Up 100% faster", None),
                    item("2", "Up 1000 faster", None),
                    item("3", "snake_case name", None),
                    item("4", "snakeXcase name", None),
                    item("5", "back\\slash", None),
                    item("6", "Hello World", None),
                ],
                &[],
                None,
            ),
            time(AT),
            None,
            false,
        )
        .await
        .unwrap();
    let q = |q: &'static str| {
        let store = store.clone();
        async move {
            store
                .list_items("p", "r", 50, None, Some(q))
                .await
                .unwrap()
                .items
                .into_iter()
                .map(|i| i.id)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(q("100%").await, ["1"]);
    assert_eq!(q("e_c").await, ["3"]);
    assert_eq!(q("k\\s").await, ["5"]);
    assert_eq!(q("hello w").await, ["6"]);
    assert_eq!(q("%").await, ["1"]);
}

#[tokio::test]
async fn q_applies_across_pages() {
    let store = Store::open_in_memory().await.unwrap();
    let items: Vec<_> = (0..5)
        .map(|i| item(&format!("m{i}"), "match", None))
        .chain((0..3).map(|i| item(&format!("x{i}"), "other", None)))
        .collect();
    store
        .apply_page(
            "p",
            "r",
            "r",
            &page(items, &[], None),
            time(AT),
            None,
            false,
        )
        .await
        .unwrap();
    let a = store
        .list_items("p", "r", 3, None, Some("MATCH"))
        .await
        .unwrap();
    assert_eq!(a.items.len(), 3);
    let b = store
        .list_items("p", "r", 3, a.next.as_deref(), Some("MATCH"))
        .await
        .unwrap();
    assert_eq!(b.items.len(), 2);
    assert!(b.next.is_none());
}

#[tokio::test]
async fn a_malformed_cursor_is_an_error_not_a_panic() {
    let store = Store::open_in_memory().await.unwrap();
    for bad in ["", "zz", "abc", "00", "7b7d", "5b5d", "ff"] {
        let r = store.list_items("p", "r", 10, Some(bad), None).await;
        assert!(matches!(r, Err(StoreError::BadCursor)), "{bad:?}: {r:?}");
    }
}

#[tokio::test]
async fn plugins_with_the_same_resource_kind_do_not_mix() {
    let store = Store::open_in_memory().await.unwrap();
    for (plugin, id) in [("one", "a"), ("two", "b")] {
        store
            .apply_page(
                plugin,
                "issues",
                "r",
                &page(vec![item(id, "T", None)], &[], Some(id)),
                time(AT),
                None,
                false,
            )
            .await
            .unwrap();
    }
    assert_eq!(ids(&store, "one", "issues").await, ["a"]);
    assert_eq!(ids(&store, "two", "issues").await, ["b"]);
    assert_eq!(store.count_items("one", "issues").await.unwrap(), 1);
    assert_eq!(
        store
            .sync_state("two", "issues")
            .await
            .unwrap()
            .cursor
            .as_deref(),
        Some("b")
    );
    assert_eq!(
        store.delete_unseen("one", "issues", "other").await.unwrap(),
        1
    );
    assert_eq!(ids(&store, "two", "issues").await, ["b"]);
}

#[tokio::test]
async fn delete_resource_clears_items_and_state() {
    let store = Store::open_in_memory().await.unwrap();
    for res in ["issues", "prs"] {
        store
            .apply_page(
                "p",
                res,
                "r",
                &page(vec![item("a", "A", None)], &[], Some("c")),
                time(AT),
                None,
                false,
            )
            .await
            .unwrap();
    }
    assert_eq!(store.known_resources("p").await.unwrap(), ["issues", "prs"]);
    assert_eq!(store.delete_resource("p", "issues").await.unwrap(), 1);
    assert_eq!(store.count_items("p", "issues").await.unwrap(), 0);
    assert_eq!(
        store.sync_state("p", "issues").await.unwrap(),
        SyncState::default()
    );
    assert_eq!(store.known_resources("p").await.unwrap(), ["prs"]);
    assert_eq!(store.count_items("p", "prs").await.unwrap(), 1);
}

#[tokio::test]
async fn known_resources_includes_state_only_and_item_only_kinds_once() {
    let store = Store::open_in_memory().await.unwrap();
    store
        .save_sync_state("p", "b", &SyncState::default())
        .await
        .unwrap();
    store
        .apply_page(
            "p",
            "a",
            "r",
            &page(vec![item("x", "X", None)], &[], None),
            time(AT),
            None,
            false,
        )
        .await
        .unwrap();
    store
        .apply_page(
            "p",
            "b",
            "r",
            &page(vec![item("x", "X", None)], &[], None),
            time(AT),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(store.known_resources("p").await.unwrap(), ["a", "b"]);
    assert!(store.known_resources("none").await.unwrap().is_empty());
}

#[tokio::test]
async fn migrating_from_a_database_at_0002_keeps_existing_rows() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("s.db");
    {
        let store = Store::open(&db).await.unwrap();
        store
            .insert_repo(&smithagents_core::agent::Repo {
                id: "2b7e9c41-5d3a-4f68-8a1c-7e4b0d9f2c35".parse().unwrap(),
                path: "/tmp/r".into(),
                default_branch: "main".into(),
                created_at: time(AT),
            })
            .await
            .unwrap();
    }
    // Make it look like a database that never ran 0003.
    let mut conn = SqliteConnectOptions::new()
        .filename(&db)
        .connect()
        .await
        .unwrap();
    for sql in [
        "DROP TABLE plugin_config",
        "DROP TABLE resource_sync",
        "DROP TABLE resource_items",
        "DELETE FROM _sqlx_migrations WHERE version = 3",
    ] {
        sqlx::query(sql).execute(&mut conn).await.unwrap();
    }
    conn.close().await.unwrap();

    let store = Store::open(&db).await.unwrap();
    assert_eq!(store.list_repos().await.unwrap().len(), 1);
    store
        .set_plugin_config(
            "p",
            &PluginConfig {
                enabled: true,
                settings: json!({}),
            },
        )
        .await
        .unwrap();
    assert!(store.plugin_config("p").await.unwrap().is_some());
}

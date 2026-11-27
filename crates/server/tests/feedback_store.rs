//! Migration 0004 and the feedback columns and table in the store.

use chrono::{DateTime, Utc};
use smithagents_core::agent::Repo;
use smithagents_core::feedback::{
    AttachmentKind, FeedbackAttachment, FeedbackKind, FeedbackOrigin, FeedbackRequest, IdeaPackage,
    SendAs, SubmissionId,
};
use smithagents_core::ids::{IntentId, ItemId, RepoId};
use smithagents_core::intent::{
    Content, Intent, IntentFilter, IntentKind, IntentState, IntentVersion, Item, ItemStatus, Phase,
    Seed, SeedSource, VersionAuthor,
};
use smithagents_server::store::{Store, StoreError, Submission};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;

fn time(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn repo(path: &str) -> Repo {
    Repo {
        id: RepoId::new(),
        path: path.into(),
        default_branch: "main".into(),
        created_at: Utc::now(),
    }
}

fn content(fixed: bool) -> Content {
    Content {
        title: "T".into(),
        kind: IntentKind::Bug,
        current_behavior: String::new(),
        questions: vec![],
        preconditions: vec![],
        criteria: vec![Item {
            id: ItemId::new(),
            text: "c".into(),
            status: ItemStatus::Accepted,
            edited: false,
            intent_ref: None,
            fixed,
        }],
        stories: vec![],
        depends_on: vec![],
    }
}

fn intent(repo_id: RepoId, slug: &str, fixed: bool) -> Intent {
    let now = Utc::now();
    Intent {
        id: IntentId::new(),
        repo_id,
        slug: slug.into(),
        state: IntentState::Draft,
        phase: Phase::Review,
        seed: Seed {
            source: SeedSource::Text,
            text: "seed".into(),
            kind: None,
        },
        content: content(fixed),
        agent_id: None,
        created_at: now,
        updated_at: now,
        queued_at: None,
        sent_at: None,
        resolved_at: None,
        last_start_error: None,
        base_branch: None,
        origin: None,
    }
}

fn first_version(i: &Intent) -> IntentVersion {
    IntentVersion {
        intent_id: i.id,
        version: 1,
        author: VersionAuthor::Seed,
        content: i.content.clone(),
        at: Utc::now(),
    }
}

fn origin(pr: Option<&str>) -> FeedbackOrigin {
    FeedbackOrigin {
        folder: "docs/issues/2026-11-26-x".into(),
        branch: "feedback/x".into(),
        commit: "abc123".into(),
        idea: Some(IdeaPackage {
            explainer: true,
            diagrams: false,
            demo_video: true,
            pr_url: pr.map(Into::into),
            not_done: None,
            implementation_intent_id: None,
            pr_commit: None,
            pr_pushed: false,
        }),
    }
}

#[tokio::test]
async fn a_database_from_0003_migrates_and_keeps_its_intents() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("old.db");
    let r = repo("/repos/old");
    let i = intent(r.id, "old", false);
    {
        let opts = SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePool::connect_with(opts).await.unwrap();
        let mut m = sqlx::migrate!("./migrations");
        m.migrations = m
            .migrations
            .iter()
            .filter(|x| x.version <= 3)
            .cloned()
            .collect::<Vec<_>>()
            .into();
        m.run(&pool).await.unwrap();
        sqlx::query("INSERT INTO repos (id, path, default_branch, created_at) VALUES (?, ?, ?, ?)")
            .bind(r.id.to_string())
            .bind("/repos/old")
            .bind("main")
            .bind("2026-11-26T10:00:00Z")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO intents (id, repo_id, slug, state, phase_json, seed_json, \
             content_json, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(i.id.to_string())
        .bind(r.id.to_string())
        .bind("old")
        .bind("draft")
        .bind(serde_json::to_string(&i.phase).unwrap())
        .bind(serde_json::to_string(&i.seed).unwrap())
        // an item stored before `fixed` existed has no such key
        .bind(
            serde_json::to_string(&i.content)
                .unwrap()
                .replace("\"fixed\":false,", ""),
        )
        .bind("2026-11-26T10:00:00Z")
        .bind("2026-11-26T10:00:00Z")
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }
    let store = Store::open(&db).await.unwrap();
    let got = store.get_intent(i.id).await.unwrap().unwrap();
    assert_eq!(got.slug, "old");
    assert_eq!(got.base_branch, None);
    assert_eq!(got.origin, None);
    assert!(!got.content.criteria[0].fixed);
}

#[tokio::test]
async fn base_branch_and_origin_round_trip() {
    let store = Store::open_in_memory().await.unwrap();
    let r = repo("/repos/a");
    store.insert_repo(&r).await.unwrap();
    let mut i = intent(r.id, "one", false);
    i.base_branch = Some("feedback/x".into());
    i.origin = Some(origin(None));
    store.insert_intent(&i, &first_version(&i)).await.unwrap();
    assert_eq!(store.get_intent(i.id).await.unwrap().unwrap(), i);

    i.base_branch = Some("idea/y".into());
    i.origin = Some(origin(Some(
        "https://github.com/smithagents-test-invalid/widgets/pull/1",
    )));
    store.save_intent(&i).await.unwrap();
    let listed = store.list_intents(IntentFilter::default()).await.unwrap();
    assert_eq!(listed, vec![i.clone()]);

    i.base_branch = None;
    i.origin = None;
    store.save_intent(&i).await.unwrap();
    let got = store.get_intent(i.id).await.unwrap().unwrap();
    assert_eq!((got.base_branch, got.origin), (None, None));
}

#[tokio::test]
async fn a_fixed_item_round_trips_in_content_json() {
    let store = Store::open_in_memory().await.unwrap();
    let r = repo("/repos/a");
    store.insert_repo(&r).await.unwrap();
    let i = intent(r.id, "fixed", true);
    store.insert_intent(&i, &first_version(&i)).await.unwrap();
    let got = store.get_intent(i.id).await.unwrap().unwrap();
    assert!(got.content.criteria[0].fixed);
    assert_eq!(got, i);
}

fn submission(repo_id: RepoId) -> Submission {
    let id = SubmissionId::new();
    Submission {
        id,
        repo_id,
        request: FeedbackRequest {
            submission_id: id,
            main_idea: "Fix the zoom".into(),
            kind: FeedbackKind::Bug,
            repo_id,
            send_as: SendAs::GithubIssue,
            idea: None,
            app_version: "0.1.0".into(),
            platform: "macos".into(),
            attachments: vec![FeedbackAttachment {
                kind: AttachmentKind::Screenshot,
                original_name: "a.png".into(),
                boxes: vec![],
            }],
            public_acknowledged: false,
        },
        title: "Fix the zoom".into(),
        branch: "feedback/fix-the-zoom".into(),
        commit: "abc1234".into(),
        folder: "docs/issues/2026-11-26-fix-the-zoom".into(),
        pushed: false,
        intent_id: None,
        issue_url: None,
        created_at: time("2026-11-26T10:00:00Z"),
        updated_at: time("2026-11-26T10:00:00Z"),
    }
}

#[tokio::test]
async fn a_submission_round_trips_and_its_id_is_unique() {
    let store = Store::open_in_memory().await.unwrap();
    let r = repo("/repos/a");
    store.insert_repo(&r).await.unwrap();
    let i = intent(r.id, "made", false);
    store.insert_intent(&i, &first_version(&i)).await.unwrap();

    let mut s = submission(r.id);
    assert_eq!(store.get_submission(s.id).await.unwrap(), None);
    store.insert_submission(&s).await.unwrap();
    assert_eq!(store.get_submission(s.id).await.unwrap(), Some(s.clone()));

    s.pushed = true;
    s.intent_id = Some(i.id);
    s.issue_url = Some("https://github.com/smithagents-test-invalid/widgets/issues/1".into());
    s.updated_at = time("2026-11-26T10:05:00Z");
    // fields the save does not own stay as inserted
    let mut tampered = s.clone();
    tampered.title = "other".into();
    store.save_submission(&tampered).await.unwrap();
    assert_eq!(store.get_submission(s.id).await.unwrap(), Some(s.clone()));

    let err = store.insert_submission(&s).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "got {err:?}");
    assert_eq!(
        store.get_submission(SubmissionId::new()).await.unwrap(),
        None
    );
}

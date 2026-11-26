//! The feedback routes: for now the meta repository setting.
#![cfg(unix)]

mod common;

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use common::feedback::FakeTools;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use smithagents_core::agent::Repo;
use smithagents_core::ids::RepoId;
use smithagents_plugin_host::StaticSecrets;
use smithagents_server::api::{router, AppState};
use smithagents_server::events::EventBus;
use smithagents_server::feedback::FeedbackService;
use smithagents_server::harness::fake::FakeHarness;
use smithagents_server::intents::IntentService;
use smithagents_server::paths::Paths;
use smithagents_server::plugins::{embedded_plugins, ManagerConfig, PluginManager};
use smithagents_server::queue::{Scheduler, SchedulerHandle};
use smithagents_server::service::AgentService;
use smithagents_server::store::Store;
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "test-token-123";

struct Fx {
    tmp: TempDir,
    state: AppState,
    _sched: SchedulerHandle,
}

async fn fixture() -> Fx {
    let tmp = TempDir::new().unwrap();
    let store = Store::open_in_memory().await.unwrap();
    let bus = EventBus::new(1024);
    let paths = Paths {
        home: tmp.path().join("home"),
    };
    paths.ensure_dirs().unwrap();
    let harness = FakeHarness::scripted(vec![]);
    let service = AgentService::new(
        store.clone(),
        bus.clone(),
        Arc::new(harness.clone()),
        paths.clone(),
        Duration::from_secs(3600),
    );
    let intents = IntentService::new(
        store.clone(),
        bus.clone(),
        Arc::new(harness),
        service.clone(),
        paths.clone(),
    );
    let sched = Scheduler::start(intents.clone(), service.clone(), store.clone(), bus.clone());
    let fake = FakeTools::new(&tmp.path().join("fakes"), true, true);
    let plugins = PluginManager::start(
        store.clone(),
        bus.clone(),
        Arc::new(StaticSecrets(Default::default())),
        embedded_plugins(),
        ManagerConfig {
            tools: fake.finder(),
            ..ManagerConfig::default()
        },
    )
    .await
    .unwrap();
    let feedback = FeedbackService::new(
        store.clone(),
        intents.clone(),
        service.clone(),
        plugins.clone(),
        paths,
    );
    let state = AppState {
        service,
        intents,
        feedback,
        bus,
        store,
        plugins,
        token: Arc::from(TOKEN),
        instance_id: Arc::from("0123456789abcdef"),
        ws_clients: Arc::new(AtomicUsize::new(0)),
        started_at: Instant::now(),
    };
    Fx {
        tmp,
        state,
        _sched: sched,
    }
}

impl Fx {
    async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.call_with(method, uri, body, Some(TOKEN)).await
    }

    async fn call_with(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let req = req
            .body(match body {
                Some(v) => Body::from(v.to_string()),
                None => Body::empty(),
            })
            .unwrap();
        let resp = router(self.state.clone()).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn add_repo(&self, name: &str) -> Repo {
        let dir = self.tmp.path().join(name);
        common::init_repo(&dir);
        let (status, body) = self
            .call(Method::POST, "/v1/repos", Some(json!({ "path": dir })))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        serde_json::from_value(body).unwrap()
    }
}

#[tokio::test]
async fn the_meta_repository_can_be_read_set_and_cleared() {
    let fx = fixture().await;
    let repo = fx.add_repo("one").await;

    let (status, body) = fx.call(Method::GET, "/v1/settings/feedback", None).await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, json!({"meta_repo_id": null}))
    );

    let put = json!({ "meta_repo_id": repo.id });
    let (status, body) = fx
        .call(Method::PUT, "/v1/settings/feedback", Some(put.clone()))
        .await;
    assert_eq!((status, &body), (StatusCode::OK, &put));
    let (_, body) = fx.call(Method::GET, "/v1/settings/feedback", None).await;
    assert_eq!(body, put);

    let (status, body) = fx
        .call(
            Method::PUT,
            "/v1/settings/feedback",
            Some(json!({"meta_repo_id": null})),
        )
        .await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, json!({"meta_repo_id": null}))
    );
    let (_, body) = fx.call(Method::GET, "/v1/settings/feedback", None).await;
    assert_eq!(body, json!({"meta_repo_id": null}));
}

#[tokio::test]
async fn an_unknown_meta_repository_is_refused() {
    let fx = fixture().await;
    let repo = fx.add_repo("one").await;
    let put = json!({ "meta_repo_id": repo.id });
    fx.call(Method::PUT, "/v1/settings/feedback", Some(put.clone()))
        .await;

    let (status, body) = fx
        .call(
            Method::PUT,
            "/v1/settings/feedback",
            Some(json!({ "meta_repo_id": RepoId::new() })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "repo_not_found");
    let (_, body) = fx.call(Method::GET, "/v1/settings/feedback", None).await;
    assert_eq!(body, put);
}

#[tokio::test]
async fn a_stored_repository_that_is_gone_reads_as_none() {
    let fx = fixture().await;
    fx.state
        .store
        .set_setting(
            "feedback",
            &smithagents_core::feedback::FeedbackSettings {
                meta_repo_id: Some(RepoId::new()),
            },
        )
        .await
        .unwrap();
    let (status, body) = fx.call(Method::GET, "/v1/settings/feedback", None).await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, json!({"meta_repo_id": null}))
    );
}

#[tokio::test]
async fn the_route_needs_the_token() {
    let fx = fixture().await;
    for method in [Method::GET, Method::PUT] {
        let (status, _) = fx
            .call_with(method, "/v1/settings/feedback", Some(json!({})), None)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

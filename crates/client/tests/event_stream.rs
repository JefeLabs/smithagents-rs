//! The client's event stream against a scripted local WebSocket server: a kind
//! this version does not know is skipped without ending the stream, and the
//! intent kinds decode into typed variants.

use std::time::Duration;

use chrono::Utc;
use futures::{SinkExt, StreamExt};
use smithagents_client::Client;
use smithagents_core::daemon::DaemonInfo;
use smithagents_core::event::{Event, EventKind};
use smithagents_core::ids::{IntentId, RepoId};
use smithagents_core::intent::{Content, Intent, IntentKind, IntentState, Phase, Seed, SeedSource};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

fn intent() -> Intent {
    let now = Utc::now();
    Intent {
        id: IntentId::new(),
        repo_id: RepoId::new(),
        slug: "t".into(),
        state: IntentState::Draft,
        phase: Phase::Questions,
        seed: Seed {
            source: SeedSource::Text,
            text: "seed".into(),
        },
        content: Content {
            title: "T".into(),
            kind: IntentKind::Task,
            current_behavior: String::new(),
            questions: vec![],
            preconditions: vec![],
            criteria: vec![],
            stories: vec![],
            depends_on: vec![],
        },
        agent_id: None,
        created_at: now,
        updated_at: now,
        queued_at: None,
        sent_at: None,
        resolved_at: None,
        last_start_error: None,
    }
}

#[tokio::test]
async fn unknown_kind_is_skipped_and_intent_kinds_decode() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let created = intent();
    let updated = intent();
    let at = Utc::now();
    let frames = vec![
        serde_json::json!({"seq": 1, "at": at, "kind": {"type": "from_the_future", "data": {}}}),
        serde_json::to_value(Event {
            seq: 2,
            at,
            kind: EventKind::IntentCreated(created.clone()),
        })
        .unwrap(),
        serde_json::json!({"seq": 3, "at": at, "kind": {"type": "also_new", "data": [1]}}),
        serde_json::to_value(Event {
            seq: 4,
            at,
            kind: EventKind::IntentUpdated(updated.clone()),
        })
        .unwrap(),
    ];
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        for frame in frames {
            ws.send(Message::text(frame.to_string())).await.unwrap();
        }
        ws.close(None).await.unwrap();
    });

    let client = Client::new(&DaemonInfo {
        pid: std::process::id(),
        port,
        token: "t".into(),
        version: "0".into(),
    });
    let mut events = client.events(None).await.unwrap();
    let mut seen = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(e) = events.next().await {
            seen.push(e);
        }
    })
    .await
    .expect("the stream ends when the server closes");
    let seqs: Vec<u64> = seen.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [2, 4]);
    assert_eq!(seen[0].kind, EventKind::IntentCreated(created));
    assert_eq!(seen[1].kind, EventKind::IntentUpdated(updated));
}

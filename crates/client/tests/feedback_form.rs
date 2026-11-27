//! The feedback calls against a scripted local HTTP server: what the upload
//! looks like on the wire (the request part first, each file named by its
//! part and never by its own name) and how answers decode.

use std::time::Duration;

use serde_json::{json, Value};
use smithagents_client::{Client, ClientError, FeedbackFile};
use smithagents_core::daemon::DaemonInfo;
use smithagents_core::feedback::{
    AttachmentKind, FeedbackAttachment, FeedbackKind, FeedbackOutcome, FeedbackRequest,
    FeedbackTargets, SendAs, SubmissionId,
};
use smithagents_core::ids::RepoId;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const TOKEN: &str = "test-token";
const PATIENCE: Duration = Duration::from_secs(20);

fn client(port: u16) -> Client {
    Client::new(&DaemonInfo {
        pid: std::process::id(),
        port,
        token: TOKEN.into(),
        version: "0".into(),
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Answers one request with `status` and `body`; yields the request's head
/// (lower-cased) and body (none without a `Content-Length`).
async fn answer_once(status: u16, body: Value) -> (u16, JoinHandle<(String, Vec<u8>)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        let mut buf = [0u8; 64 * 1024];
        let head_end = loop {
            if let Some(at) = find(&raw, b"\r\n\r\n") {
                break at + 4;
            }
            let n = tcp.read(&mut buf).await.unwrap();
            assert!(n > 0, "the request ended inside its head");
            raw.extend_from_slice(&buf[..n]);
        };
        let head = String::from_utf8_lossy(&raw[..head_end]).to_lowercase();
        let length: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .map_or(0, |v| v.trim().parse().unwrap());
        while raw.len() < head_end + length {
            let n = tcp.read(&mut buf).await.unwrap();
            assert!(n > 0, "the request ended inside its body");
            raw.extend_from_slice(&buf[..n]);
        }
        let text = body.to_string();
        let answer = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        );
        tcp.write_all(answer.as_bytes()).await.unwrap();
        (head, raw[head_end..].to_vec())
    });
    (port, task)
}

fn request() -> FeedbackRequest {
    FeedbackRequest {
        submission_id: SubmissionId::new(),
        main_idea: "Fix the zoom".into(),
        kind: FeedbackKind::Bug,
        repo_id: RepoId::new(),
        send_as: SendAs::Intent,
        idea: None,
        app_version: "0.1.0".into(),
        platform: "macos".into(),
        attachments: vec![FeedbackAttachment {
            kind: AttachmentKind::Screenshot,
            original_name: "Screen Shot.png".into(),
            boxes: vec![],
        }],
        public_acknowledged: true,
    }
}

fn outcome(req: &FeedbackRequest) -> FeedbackOutcome {
    FeedbackOutcome {
        submission_id: req.submission_id,
        repo_id: req.repo_id,
        branch: "feedback/2026-11-26-fix-the-zoom".into(),
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        folder: "docs/issues/2026-11-26-fix-the-zoom".into(),
        pushed: false,
        intent: None,
        issue_url: None,
        failed: None,
    }
}

#[tokio::test]
async fn an_upload_sends_the_request_first_and_names_files_by_their_part() {
    let tmp = tempfile::tempdir().unwrap();
    let shot = tmp.path().join("My Private Shot.png");
    let annotated = tmp.path().join("annotated copy.png");
    std::fs::write(&shot, b"\x89PNG\r\n\x1a\nfirst file").unwrap();
    std::fs::write(&annotated, b"\x89PNG\r\n\x1a\nsecond file").unwrap();
    let req = request();
    let expected = outcome(&req);
    let (port, server) = answer_once(200, serde_json::to_value(&expected).unwrap()).await;

    let got = tokio::time::timeout(
        PATIENCE,
        client(port).submit_feedback(
            &req,
            &[
                FeedbackFile {
                    part: "file-1".into(),
                    path: shot.clone(),
                },
                FeedbackFile {
                    part: "annotated-1".into(),
                    path: annotated.clone(),
                },
            ],
        ),
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(got, expected);
    let (head, body) = server.await.unwrap();
    assert!(head.starts_with("post /v1/feedback http/1.1\r\n"), "{head}");
    assert!(
        head.contains(&format!("authorization: bearer {TOKEN}")),
        "{head}"
    );
    assert!(
        head.contains("content-type: multipart/form-data; boundary="),
        "{head}"
    );
    // Every part's length is known: the files are streamed with their size.
    assert!(head.contains("content-length:"), "{head}");
    let text = String::from_utf8_lossy(&body);
    let request_at = text.find("name=\"request\"").expect("a request part");
    let file_at = text
        .find("name=\"file-1\"; filename=\"file-1\"")
        .expect("file-1 named by its part");
    let annotated_at = text
        .find("name=\"annotated-1\"; filename=\"annotated-1\"")
        .expect("annotated-1 named by its part");
    assert!(request_at < file_at && file_at < annotated_at, "{text}");
    let request_part = &text[request_at..file_at];
    assert!(
        request_part.contains("content-type: application/json")
            || request_part.contains("Content-Type: application/json"),
        "{request_part}"
    );
    let json_start = request_part.find("\r\n\r\n").unwrap() + 4;
    let json_end = request_part[json_start..].find("\r\n--").unwrap() + json_start;
    let sent: FeedbackRequest = serde_json::from_str(&request_part[json_start..json_end]).unwrap();
    assert_eq!(sent, req);
    assert!(
        text.contains("first file") && text.contains("second file"),
        "{text}"
    );
    // The local file names and folder stay on this machine.
    for local in [
        "My Private Shot",
        "annotated copy",
        tmp.path().to_str().unwrap(),
    ] {
        assert!(
            !text.contains(local) && !head.contains(&local.to_lowercase()),
            "{local}"
        );
    }
}

#[tokio::test]
async fn a_file_that_cannot_be_opened_is_an_error_before_anything_is_sent() {
    // Nothing listens on this port: a request sent would be an Http error.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let tmp = tempfile::tempdir().unwrap();
    let gone = tmp.path().join("gone.png");

    let err = client(port)
        .submit_feedback(
            &request(),
            &[FeedbackFile {
                part: "file-1".into(),
                path: gone.clone(),
            }],
        )
        .await
        .unwrap_err();

    // The error says which file, and why.
    assert!(
        matches!(
            &err,
            ClientError::File { path, source }
                if *path == gone && source.kind() == std::io::ErrorKind::NotFound
        ),
        "{err:?}"
    );
    let text = err.to_string();
    assert!(
        text.starts_with(&format!("could not open {} to upload: ", gone.display())),
        "{text}"
    );
}

#[tokio::test]
async fn a_retry_posts_to_the_submission_and_decodes_its_outcome() {
    let req = request();
    let mut expected = outcome(&req);
    expected.pushed = true;
    expected.issue_url =
        Some("https://github.com/smithagents-test-invalid/widgets/issues/7".into());
    let (port, server) = answer_once(200, serde_json::to_value(&expected).unwrap()).await;

    let got = tokio::time::timeout(PATIENCE, client(port).retry_feedback(req.submission_id))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(got, expected);
    let (head, body) = server.await.unwrap();
    let line = format!("post /v1/feedback/{}/retry http/1.1\r\n", req.submission_id);
    assert!(head.starts_with(&line), "{head}");
    assert!(body.is_empty());

    let refusal = json!({"error": "submission_not_found", "message": "no such submission"});
    let (port, _server) = answer_once(404, refusal).await;
    let err = client(port)
        .retry_feedback(SubmissionId::new())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Api { status: 404, code, .. } if code == "submission_not_found"),
        "{err:?}"
    );
}

#[tokio::test]
async fn targets_and_refusals_decode() {
    let targets = json!({
        "repos": [{"repo_id": RepoId::new(), "has_github_remote": true, "is_public": null}],
        "meta_repo_id": null,
        "gh": {"ready": true, "reason": null},
        "agent_browser": {"ready": false, "reason": "not installed"},
    });
    let (port, server) = answer_once(200, targets.clone()).await;
    let got = client(port).feedback_targets().await.unwrap();
    assert_eq!(
        got,
        serde_json::from_value::<FeedbackTargets>(targets).unwrap()
    );
    let (head, _) = server.await.unwrap();
    assert!(
        head.starts_with("get /v1/feedback/targets http/1.1\r\n"),
        "{head}"
    );

    let refusal =
        json!({"error": "payload_too_large", "message": "the submission is larger than 100 MB"});
    let (port, _server) = answer_once(413, refusal).await;
    let err = client(port)
        .submit_feedback(&request(), &[])
        .await
        .unwrap_err();
    match err {
        ClientError::Api {
            status,
            code,
            message,
        } => assert_eq!(
            (status, code.as_str(), message.as_str()),
            (
                413,
                "payload_too_large",
                "the submission is larger than 100 MB"
            )
        ),
        other => panic!("{other:?}"),
    }
}

/// An idea intent as the daemon answers one, with a pull request.
fn idea_intent() -> smithagents_core::intent::Intent {
    use smithagents_core::feedback::{FeedbackOrigin, IdeaPackage};
    use smithagents_core::intent::{
        Content, Intent, IntentKind, IntentState, Phase, Seed, SeedSource,
    };
    let at = chrono::Utc::now();
    Intent {
        id: smithagents_core::ids::IntentId::new(),
        repo_id: RepoId::new(),
        slug: "fix-the-zoom".into(),
        state: IntentState::Resolved,
        phase: Phase::Review,
        seed: Seed {
            source: SeedSource::Text,
            text: "Fix the zoom".into(),
            kind: Some(IntentKind::Bug),
        },
        content: Content {
            title: "Fix the zoom".into(),
            kind: IntentKind::Bug,
            current_behavior: String::new(),
            questions: vec![],
            preconditions: vec![],
            criteria: vec![],
            stories: vec![],
            depends_on: vec![],
        },
        agent_id: None,
        created_at: at,
        updated_at: at,
        queued_at: None,
        sent_at: Some(at),
        resolved_at: Some(at),
        last_start_error: None,
        base_branch: Some("idea/fix-the-zoom".into()),
        origin: Some(FeedbackOrigin {
            folder: "docs/issues/2026-11-26-fix-the-zoom".into(),
            branch: "idea/fix-the-zoom".into(),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            idea: Some(IdeaPackage {
                explainer: true,
                diagrams: false,
                demo_video: false,
                pr_url: Some("https://github.com/smithagents-test-invalid/widgets/pull/8".into()),
                not_done: None,
                implementation_intent_id: None,
                pr_commit: Some("89abcdef0123456789abcdef0123456789abcdef".into()),
                pr_pushed: true,
            }),
        }),
    }
}

#[tokio::test]
async fn the_idea_actions_post_to_the_intent_and_decode_it() {
    let intent = idea_intent();
    for route in ["pull-request", "implementation"] {
        let status = if route == "implementation" { 201 } else { 200 };
        let (port, server) = answer_once(status, serde_json::to_value(&intent).unwrap()).await;
        let c = client(port);
        let got = tokio::time::timeout(PATIENCE, async {
            if route == "pull-request" {
                c.open_pull_request(intent.id, true).await
            } else {
                c.create_implementation_intent(intent.id, true).await
            }
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(got, intent, "{route}");
        let (head, body) = server.await.unwrap();
        let line = format!("post /v1/intents/{}/{route} http/1.1\r\n", intent.id);
        assert!(head.starts_with(&line), "{head}");
        assert!(
            head.contains(&format!("authorization: bearer {TOKEN}")),
            "{head}"
        );
        // The user's acknowledgement goes along, on both routes.
        let sent: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sent, json!({"public_acknowledged": true}), "{route}");
        assert!(head.contains("content-type: application/json"), "{head}");
    }

    // A failed step decodes with its own code.
    let refusal = json!({"error": "push_rejected", "message": "git could not push to origin: the remote rejected the branch"});
    let (port, _server) = answer_once(502, refusal).await;
    let err = client(port)
        .open_pull_request(intent.id, false)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Api { status: 502, code, .. } if code == "push_rejected"),
        "{err:?}"
    );
}

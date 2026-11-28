//! Submit through the link against the real `smithagentsd`: the intent a
//! submission creates is in the app's model when the answer arrives (the
//! link hands it to the UI as `Action::IntentUpserted` before answering).
//! No window is opened.
//!
//! The daemon finds its tools through `PATH` first: the fake `gh` and
//! `agent-browser` from `crates/server/tests/bin` are copied into a
//! directory that leads the daemon's `PATH` (checked before it starts), so a
//! real `gh` on this machine is never run. `claude` is
//! `crates/server/tests/bin/fake-claude`. Nothing leaves the machine: the
//! repository is a local one without a remote.

mod common;

use std::fs;
use std::future::Future;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::feedback::png;
use common::{daemon_bin, fake_claude, git, init_repo, TestHome};
use smithagents_app::link::{DaemonLink, LinkConfig};
use smithagents_app::model::{Action, Connection, ShellModel};
use smithagents_client::{FeedbackFile, SpawnOptions};
use smithagents_core::agent::AddSourceControl;
use smithagents_core::feedback::{
    AttachmentKind, FeedbackAttachment, FeedbackBox, FeedbackKind, FeedbackRequest, SendAs,
    SubmissionId,
};
use tokio::sync::mpsc::Receiver;

const WAIT: Duration = Duration::from_secs(60);

/// The transcript fake-claude prints for a chat turn.
const HELLO_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../server/tests/fixtures/claude_stream_hello.jsonl"
);
const FAKE_TOOLS: [(&str, &str); 2] = [
    (
        concat!(env!("CARGO_MANIFEST_DIR"), "/../server/tests/bin/fake-gh"),
        "gh",
    ),
    (
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../server/tests/bin/fake-agent-browser"
        ),
        "agent-browser",
    ),
];

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// `<scratch>/tools/bin` with the fake `gh` and `agent-browser` (the fake
/// `gh` keeps its logs in `<scratch>/tools/gh`).
fn fake_tools(scratch: &Path) -> PathBuf {
    let bin = scratch.join("tools").join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(scratch.join("tools").join("gh")).unwrap();
    for (source, name) in FAKE_TOOLS {
        let to = bin.join(name);
        fs::copy(source, &to).unwrap();
        fs::set_permissions(&to, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(to.is_file(), "the fake {name} is in place");
    }
    bin
}

fn config(home: &TestHome, tools: &Path) -> LinkConfig {
    let mut config = LinkConfig::new(
        home.path().to_owned(),
        daemon_bin().to_owned(),
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![tools.to_owned()];
    dirs.extend(std::env::split_paths(&path));
    config.spawn = SpawnOptions {
        args: vec![
            "--claude-bin".into(),
            fake_claude().into(),
            "--idle-timeout-secs".into(),
            "120".into(),
        ],
        env: vec![
            ("PATH".into(), std::env::join_paths(dirs).unwrap()),
            ("FAKE_CLAUDE_FIXTURE".into(), HELLO_FIXTURE.into()),
        ],
    };
    config.backoff_min = Duration::from_millis(50);
    config.backoff_max = Duration::from_millis(200);
    config
}

/// Receives actions, applying each, until the link says it is connected.
async fn until_connected(rx: &mut Receiver<Action>, model: &mut ShellModel) {
    within("the connection", async {
        loop {
            let action = rx.recv().await.expect("link closed");
            let connected = action == Action::ConnectionChanged(Connection::Connected);
            model.apply(action);
            if connected {
                break;
            }
        }
    })
    .await;
}

#[tokio::test]
async fn the_created_intent_is_in_the_model_when_the_answer_arrives() {
    // `home` outlives the link: its drop stops the daemon, on every exit.
    let home = TestHome::new();
    let tools = fake_tools(home.scratch());
    let repo_dir = home.scratch().join("repo");
    init_repo(&repo_dir);
    git(&repo_dir, &["config", "user.name", "Test User"]);
    git(&repo_dir, &["config", "user.email", "test@example.invalid"]);
    let shots = home.scratch().join("shots");
    fs::create_dir_all(&shots).unwrap();
    let (shot, annotated) = (shots.join("shot.png"), shots.join("shot.annotated.png"));
    fs::write(&shot, png(32, 24)).unwrap();
    fs::write(&annotated, png(32, 24)).unwrap();

    let (link, mut rx) = DaemonLink::start(config(&home, &tools));
    let mut model = ShellModel::new();
    until_connected(&mut rx, &mut model).await;
    let repo = within(
        "the repository",
        link.add_repo(&repo_dir, AddSourceControl::Git),
    )
    .await
    .unwrap();

    let request = FeedbackRequest {
        submission_id: SubmissionId::new(),
        main_idea: "Fix the zoom\n\nThe page is too big.".into(),
        kind: FeedbackKind::Bug,
        repo_id: repo.id,
        send_as: SendAs::Intent,
        idea: None,
        app_version: "0.1.0".into(),
        platform: std::env::consts::OS.into(),
        attachments: vec![FeedbackAttachment {
            kind: AttachmentKind::Screenshot,
            original_name: "screenshot-1.png".into(),
            boxes: vec![FeedbackBox {
                x: 0.25,
                y: 0.25,
                w: 0.25,
                h: 0.25,
                comment: "the toolbar".into(),
            }],
        }],
        public_acknowledged: false,
    };
    let files = vec![
        FeedbackFile {
            part: "file-1".into(),
            path: shot,
        },
        FeedbackFile {
            part: "annotated-1".into(),
            path: annotated,
        },
    ];
    let outcome = within("the submission", link.submit_feedback(request, files))
        .await
        .unwrap();
    assert_eq!(outcome.failed, None, "{outcome:?}");
    let intent = outcome.intent.expect("the outcome carries the intent");

    // Only what the link queued before answering.
    let mut upserted = false;
    while let Ok(action) = rx.try_recv() {
        if matches!(&action, Action::IntentUpserted(i) if i.id == intent.id) {
            upserted = true;
        }
        model.apply(action);
    }
    assert!(upserted, "the link handed the intent over before answering");
    assert!(
        model.intent(intent.id).is_some(),
        "the model has the intent"
    );

    drop(link);
}

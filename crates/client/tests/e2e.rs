//! End to end: the real `smithagentsd`, found or started through
//! `ensure_daemon_with`, running turns with `crates/server/tests/bin/fake-claude`
//! standing in for `claude`.
//!
//! A daemon's environment is fixed when it starts, so what fake-claude does on
//! a given turn is switched through its control directory
//! (`FAKE_CLAUDE_CONTROL_DIR`), which it reads again on every run.
//!
//! `real_claude_smoke` drives the real CLI instead and is ignored by default.

mod common;

use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::{Stream, StreamExt};
use serde_json::Value;
use smithagents_client::{ensure_daemon_with, Client, SpawnOptions};
use smithagents_core::agent::{AddSourceControl, Agent, AgentState, Role};
use smithagents_core::daemon::read_lockfile;
use smithagents_core::event::{Event, EventKind};
use smithagents_core::ids::AgentId;
use smithagents_core::palette::palette;
use smithagents_core::VERSION;

use common::{
    claude_fixture, claude_tool_fixture, daemon_bin, fake_claude, git, init_repo, pid_alive,
    TestHome,
};

/// Bounds each wait: a hang is a failure, not a stuck run.
const PATIENCE: Duration = Duration::from_secs(30);
/// Bounds each test as a whole.
const TEST_DEADLINE: Duration = Duration::from_secs(60);
/// Interval of the polls on state no event reports (the lockfile, processes).
const POLL: Duration = Duration::from_millis(25);
/// `--idle-timeout-secs` for the tests about idle exit.
const IDLE_TIMEOUT_SECS: &str = "1";
/// How soon after it answered an idle daemon must be gone, lockfile and process.
const IDLE_EXIT_WITHIN: Duration = Duration::from_secs(4);
/// How long the daemon must stay up after its last client left while a turn
/// runs: past the 1 s idle timeout and its 100 ms sampling, by when a daemon
/// that did not count the running turn would have removed its lockfile.
const OUTLIVE_CLIENT_FOR: Duration = Duration::from_millis(1500);

/// Bounds a whole test body, on top of the finer `within` bounds inside it.
/// Tests resolve `daemon_bin()` before this, so a cold build of the daemon
/// never counts against the bound.
async fn bounded<T>(limit: Duration, test: impl Future<Output = T>) -> T {
    tokio::time::timeout(limit, test)
        .await
        .unwrap_or_else(|_| panic!("the test did not finish within {limit:?}"))
}

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(PATIENCE, fut)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish within {PATIENCE:?}"))
}

/// What fake-claude replays: the fixture's session id and its reply.
struct Fixture {
    session_id: String,
    reply: String,
}

/// Reads the session id and the streamed reply out of the fixture, so the
/// tests do not hard-code either.
fn fixture() -> Fixture {
    let raw = std::fs::read_to_string(claude_fixture()).unwrap();
    let mut session_id = None;
    let mut reply = String::new();
    let mut result = None;
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).unwrap();
        match v["type"].as_str() {
            Some("system") if v["subtype"] == "init" => {
                session_id = v["session_id"].as_str().map(str::to_owned);
            }
            Some("stream_event") if v["event"]["delta"]["type"] == "text_delta" => {
                reply.push_str(v["event"]["delta"]["text"].as_str().unwrap());
            }
            Some("result") => result = v["result"].as_str().map(str::to_owned),
            _ => {}
        }
    }
    let session_id = session_id.expect("the fixture starts a session");
    assert!(!reply.is_empty(), "the fixture streams no text");
    assert_eq!(
        result.as_deref(),
        Some(reply.as_str()),
        "the fixture's result differs from its streamed text"
    );
    Fixture { session_id, reply }
}

/// fake-claude's side of one test: the argv and stdin of its latest run, and
/// the control directory that decides what its next run does.
struct FakeClaude {
    fixture: PathBuf,
    args_file: PathBuf,
    stdin_file: PathBuf,
    control_dir: PathBuf,
}

impl FakeClaude {
    /// Replays the hello fixture on every run.
    fn new(scratch: &Path) -> Self {
        Self::replaying(scratch, claude_fixture())
    }

    fn replaying(scratch: &Path, fixture: PathBuf) -> Self {
        let control_dir = scratch.join("fake-claude-control");
        std::fs::create_dir(&control_dir).unwrap();
        FakeClaude {
            fixture,
            args_file: scratch.join("fake-claude-args"),
            stdin_file: scratch.join("fake-claude-stdin"),
            control_dir,
        }
    }

    /// Options for a daemon that runs fake-claude as `claude`, replaying the
    /// fixture, with `args` added to its command line and `env` to its
    /// environment.
    fn daemon_options(&self, args: &[&str], env: &[(&str, &str)]) -> SpawnOptions {
        let mut options = SpawnOptions {
            args: vec!["--claude-bin".into(), fake_claude().into()],
            env: vec![
                ("FAKE_CLAUDE_FIXTURE".into(), self.fixture.clone().into()),
                (
                    "FAKE_CLAUDE_ARGS_FILE".into(),
                    self.args_file.clone().into(),
                ),
                (
                    "FAKE_CLAUDE_STDIN_FILE".into(),
                    self.stdin_file.clone().into(),
                ),
                (
                    "FAKE_CLAUDE_CONTROL_DIR".into(),
                    self.control_dir.clone().into(),
                ),
            ],
        };
        options.args.extend(args.iter().map(OsString::from));
        options
            .env
            .extend(env.iter().map(|&(k, v)| (k.into(), v.into())));
        options
    }

    /// Later runs exit with `code` once they have replayed the fixture.
    fn exit_with(&self, code: i32) {
        std::fs::write(self.control_dir.join("exit"), format!("{code}\n")).unwrap();
    }

    /// Later runs exit 0 again.
    fn exit_normally(&self) {
        std::fs::remove_file(self.control_dir.join("exit")).unwrap();
    }

    /// The argv of the latest run (the args file holds one entry per line).
    fn last_args(&self) -> Vec<String> {
        std::fs::read_to_string(&self.args_file)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The prompt of the latest run: everything it read on stdin. Also checks
    /// that the prompt stayed off the command line, where `-p` is a flag.
    fn last_prompt(&self) -> String {
        let prompt = std::fs::read_to_string(&self.stdin_file).unwrap();
        let args = self.last_args();
        assert!(args.iter().any(|arg| arg == "-p"), "{args:?}");
        assert!(
            !args.iter().any(|arg| arg == &prompt),
            "the prompt is in argv: {args:?}"
        );
        prompt
    }
}

/// The argument after `flag` in `args`, if `flag` is there.
fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let at = args.iter().position(|arg| arg == flag)?;
    args.get(at + 1).map(String::as_str)
}

/// `ensure_daemon_with` for this crate's version, panicking with the daemon log.
async fn ensure(home: &TestHome, bin: &Path, options: &SpawnOptions) -> Client {
    within(
        "ensure_daemon_with",
        ensure_daemon_with(home.path(), bin, VERSION, options),
    )
    .await
    .unwrap_or_else(|e| {
        panic!(
            "ensure_daemon_with failed: {e}; daemon log:\n{}",
            home.log()
        )
    })
}

/// The pid in `home`'s lockfile, if there is one.
fn lockfile_pid(home: &TestHome) -> Option<u32> {
    read_lockfile(home.path()).unwrap().map(|info| info.pid)
}

/// Creates a git repo at `dir` on `main` with one commit, registers it and
/// creates an agent on it.
async fn new_agent(client: &Client, dir: &Path) -> Agent {
    init_repo(dir);
    let repo = client.add_repo(dir, AddSourceControl::Git).await.unwrap();
    client.create_agent(repo.id).await.unwrap()
}

/// Consumes `events` until one reports agent `id` in a state `wanted`
/// accepts, and returns the agent as of that event.
async fn next_state(
    events: &mut (impl Stream<Item = Event> + Unpin),
    id: AgentId,
    wanted: impl Fn(AgentState) -> bool,
) -> Agent {
    while let Some(event) = events.next().await {
        match event.kind {
            EventKind::AgentUpdated(agent) if agent.id == id && wanted(agent.state) => {
                return agent
            }
            _ => {}
        }
    }
    panic!("the event stream ended first");
}

/// Consumes `events` until agent `id` ends its turn, `Done` or `Dead`.
async fn turn_end(events: &mut (impl Stream<Item = Event> + Unpin), id: AgentId) -> Agent {
    next_state(events, id, |state| {
        matches!(state, AgentState::Done | AgentState::Dead)
    })
    .await
}

/// The agent's messages as (role, text), oldest first.
async fn chat(client: &Client, id: AgentId) -> Vec<(Role, String)> {
    client
        .messages(id, 0)
        .await
        .unwrap()
        .into_iter()
        .map(|m| (m.role, m.text))
        .collect()
}

fn said(role: Role, text: &str) -> (Role, String) {
    (role, text.to_owned())
}

#[tokio::test]
async fn chat_flow_end_to_end() {
    let bin = daemon_bin();
    let home = TestHome::new();
    bounded(TEST_DEADLINE, async {
        let fx = fixture();
        let fake = FakeClaude::new(home.scratch());
        let client = ensure(&home, bin, &fake.daemon_options(&[], &[])).await;

        let repo_dir = home.scratch().join("repo");
        let agent = new_agent(&client, &repo_dir).await;
        assert_eq!(agent.name, "Mercury");
        assert_eq!(agent.color, palette()[0]);
        assert_eq!(agent.state, AgentState::Idle);
        let branch = agent.branch.as_deref().unwrap();
        assert!(branch.starts_with("agent/mercury-"), "{branch}");
        assert!(
            agent.worktree_path.is_dir(),
            "no worktree at {}",
            agent.worktree_path.display()
        );
        assert_eq!(
            git(&agent.worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"]),
            branch
        );
        // The user's checkout stays where it was.
        assert_eq!(
            git(&repo_dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "main"
        );

        let mut events = client.events(None).await.unwrap();
        client.send_message(agent.id, "hello").await.unwrap();
        let first = within("the first turn", turn_end(&mut events, agent.id)).await;
        let messages = chat(&client, agent.id).await;
        assert_eq!(
            first.state,
            AgentState::Done,
            "chat: {messages:?}; daemon log:\n{}",
            home.log()
        );
        assert_eq!(
            messages,
            [said(Role::User, "hello"), said(Role::Assistant, &fx.reply)]
        );
        assert_eq!(
            first.harness_session_id.as_deref(),
            Some(fx.session_id.as_str())
        );
        assert_eq!(fake.last_prompt(), "hello");
        let args = fake.last_args();
        assert_eq!(arg_after(&args, "--resume"), None, "{args:?}");

        client.send_message(agent.id, "again").await.unwrap();
        let second = within("the second turn", turn_end(&mut events, agent.id)).await;
        let messages = chat(&client, agent.id).await;
        assert_eq!(
            second.state,
            AgentState::Done,
            "chat: {messages:?}; daemon log:\n{}",
            home.log()
        );
        // The second turn resumed the session the first one started.
        assert_eq!(fake.last_prompt(), "again");
        let args = fake.last_args();
        assert_eq!(
            arg_after(&args, "--resume"),
            Some(fx.session_id.as_str()),
            "{args:?}"
        );
        assert_eq!(
            messages,
            [
                said(Role::User, "hello"),
                said(Role::Assistant, &fx.reply),
                said(Role::User, "again"),
                said(Role::Assistant, &fx.reply),
            ]
        );

        client.close_agent(agent.id).await.unwrap();
        assert_eq!(client.list_agents(false).await.unwrap(), vec![]);
        let all = client.list_agents(true).await.unwrap();
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!((all[0].id, all[0].name.as_str()), (agent.id, "Mercury"));
        assert!(all[0].closed_at.is_some(), "{all:?}");
    })
    .await;
}

#[tokio::test]
async fn tool_using_turn_is_saved_in_the_order_it_happened() {
    let bin = daemon_bin();
    let home = TestHome::new();
    bounded(TEST_DEADLINE, async {
        let fake = FakeClaude::replaying(home.scratch(), claude_tool_fixture());
        let client = ensure(&home, bin, &fake.daemon_options(&[], &[])).await;
        let agent = new_agent(&client, &home.scratch().join("repo")).await;

        let mut events = client.events(None).await.unwrap();
        client.send_message(agent.id, "hello").await.unwrap();
        // Collect the live deltas on the way to the end of the turn.
        let mut deltas = String::new();
        let end = within("the turn", async {
            while let Some(event) = events.next().await {
                match event.kind {
                    EventKind::TurnDelta { agent_id, text } if agent_id == agent.id => {
                        deltas.push_str(&text);
                    }
                    EventKind::AgentUpdated(a)
                        if a.id == agent.id
                            && matches!(a.state, AgentState::Done | AgentState::Dead) =>
                    {
                        return a;
                    }
                    _ => {}
                }
            }
            panic!("the event stream ended first");
        })
        .await;
        let messages = chat(&client, agent.id).await;
        assert_eq!(
            end.state,
            AgentState::Done,
            "chat: {messages:?}; daemon log:\n{}",
            home.log()
        );
        // Text before the tool call, the call, then the two closing text
        // blocks as paragraphs of one message.
        assert_eq!(
            messages,
            [
                said(Role::User, "hello"),
                said(Role::Assistant, "I'll check the file."),
                said(Role::Tool, "Read: README.md"),
                said(
                    Role::Assistant,
                    "The file is a short README.\n\nIt has one heading."
                ),
            ]
        );
        // Live deltas are passed on as claude streamed them.
        assert_eq!(
            deltas,
            "I'll check the file.The file is a short README.It has one heading."
        );
    })
    .await;
}

#[tokio::test]
async fn daemon_outlives_client_while_agent_working() {
    let bin = daemon_bin();
    let home = TestHome::new();
    bounded(TEST_DEADLINE, async {
        let fx = fixture();
        let fake = FakeClaude::new(home.scratch());
        // With a short idle timeout, a daemon that did not count the running
        // turn would exit while no client is connected below.
        let options = fake.daemon_options(
            &["--idle-timeout-secs", IDLE_TIMEOUT_SECS],
            &[("FAKE_CLAUDE_SLEEP", "3")],
        );
        let client = ensure(&home, bin, &options).await;
        // Until the turn runs, this connection is what keeps the daemon up.
        let mut events = client.events(None).await.unwrap();
        let pid = client.health().await.unwrap().pid;
        assert_eq!(lockfile_pid(&home), Some(pid));
        let agent = new_agent(&client, &home.scratch().join("repo")).await;

        client.send_message(agent.id, "hello").await.unwrap();
        within(
            "the turn to start",
            next_state(&mut events, agent.id, |state| state == AgentState::Working),
        )
        .await;
        assert_eq!(client.health().await.unwrap().running_agents, 1);

        // fake-claude now sleeps for 3 s before it exits; meanwhile no client
        // is connected at all.
        drop(events);
        drop(client);
        let left = Instant::now();
        while left.elapsed() < OUTLIVE_CLIENT_FOR {
            assert!(
                pid_alive(pid),
                "the daemon exited while its agent worked; log:\n{}",
                home.log()
            );
            assert_eq!(
                lockfile_pid(&home),
                Some(pid),
                "the daemon began shutting down while its agent worked; log:\n{}",
                home.log()
            );
            tokio::time::sleep(POLL).await;
        }

        let client = ensure(&home, bin, &options).await;
        let mut events = client.events(None).await.unwrap();
        assert_eq!(
            client.health().await.unwrap().pid,
            pid,
            "a new daemon was started"
        );
        // The turn may have ended before this subscription.
        let now = client
            .list_agents(false)
            .await
            .unwrap()
            .into_iter()
            .find(|a| a.id == agent.id)
            .expect("the agent is still open");
        let end = match now.state {
            AgentState::Done | AgentState::Dead => now,
            _ => within("the turn", turn_end(&mut events, agent.id)).await,
        };
        let messages = chat(&client, agent.id).await;
        assert_eq!(
            end.state,
            AgentState::Done,
            "chat: {messages:?}; daemon log:\n{}",
            home.log()
        );
        assert_eq!(
            messages,
            [said(Role::User, "hello"), said(Role::Assistant, &fx.reply)]
        );
        assert_eq!(client.health().await.unwrap().running_agents, 0);
    })
    .await;
}

#[tokio::test]
async fn idle_daemon_exits_after_timeout() {
    let bin = daemon_bin();
    let home = TestHome::new();
    bounded(TEST_DEADLINE, async {
        let options = SpawnOptions {
            args: vec!["--idle-timeout-secs".into(), IDLE_TIMEOUT_SECS.into()],
            env: vec![],
        };
        let asked = Instant::now();
        let client = ensure(&home, bin, &options).await;
        let answered = Instant::now();
        let pid = client.health().await.unwrap().pid;
        assert_eq!(lockfile_pid(&home), Some(pid));
        // Not even a pooled HTTP connection stays open.
        drop(client);

        let deadline = answered + IDLE_EXIT_WITHIN;
        // The sleep runs on the runtime, which lets tokio reap the exited daemon.
        while lockfile_pid(&home).is_some() || pid_alive(pid) {
            assert!(
                Instant::now() < deadline,
                "the idle daemon was still there {IDLE_EXIT_WITHIN:?} after it answered \
                 (lockfile pid {:?}, alive {}); log:\n{}",
                lockfile_pid(&home),
                pid_alive(pid),
                home.log()
            );
            tokio::time::sleep(POLL).await;
        }
        // It could not have been idle for 1 s before it was asked to start.
        assert!(
            asked.elapsed() >= Duration::from_secs(1),
            "exited {:?} after the start, before its idle timeout",
            asked.elapsed()
        );
        assert!(
            home.log().contains("idle, shutting down"),
            "daemon log:\n{}",
            home.log()
        );
    })
    .await;
}

#[tokio::test]
async fn failed_turn_marks_agent_dead_then_resume_works() {
    let bin = daemon_bin();
    let home = TestHome::new();
    bounded(TEST_DEADLINE, async {
        let fx = fixture();
        let fake = FakeClaude::new(home.scratch());
        let client = ensure(&home, bin, &fake.daemon_options(&[], &[])).await;
        let agent = new_agent(&client, &home.scratch().join("repo")).await;
        let mut events = client.events(None).await.unwrap();

        // The first run replays the whole reply, then exits 3.
        fake.exit_with(3);
        client.send_message(agent.id, "hello").await.unwrap();
        let dead = within("the failing turn", turn_end(&mut events, agent.id)).await;
        let messages = chat(&client, agent.id).await;
        assert_eq!(
            dead.state,
            AgentState::Dead,
            "chat: {messages:?}; daemon log:\n{}",
            home.log()
        );
        // The session id survives the failure, so the next turn can resume it.
        assert_eq!(
            dead.harness_session_id.as_deref(),
            Some(fx.session_id.as_str())
        );
        assert_eq!(arg_after(&fake.last_args(), "--resume"), None);
        // The reply is kept; the failure is recorded after it.
        assert_eq!(messages.len(), 3, "{messages:?}");
        assert_eq!(
            messages[..2],
            [said(Role::User, "hello"), said(Role::Assistant, &fx.reply)]
        );
        let (role, failure) = &messages[2];
        assert_eq!(*role, Role::Assistant);
        assert!(failure.starts_with("Agent process failed:"), "{failure}");
        assert!(failure.contains("exit status: 3"), "{failure}");

        fake.exit_normally();
        client.send_message(agent.id, "again").await.unwrap();
        let back = within("the resumed turn", turn_end(&mut events, agent.id)).await;
        let messages = chat(&client, agent.id).await;
        assert_eq!(
            back.state,
            AgentState::Done,
            "chat: {messages:?}; daemon log:\n{}",
            home.log()
        );
        assert_eq!(fake.last_prompt(), "again");
        let args = fake.last_args();
        assert_eq!(
            arg_after(&args, "--resume"),
            Some(fx.session_id.as_str()),
            "{args:?}"
        );
        assert_eq!(
            messages[3..],
            [said(Role::User, "again"), said(Role::Assistant, &fx.reply)]
        );
    })
    .await;
}

/// Bounds the smoke run as a whole; a real turn can take a while.
const SMOKE_DEADLINE: Duration = Duration::from_secs(300);

/// Manual smoke against the real `claude` CLI, which must be on `PATH` and
/// logged in; it makes one small real API call. Run it from a plain terminal
/// (the daemon and the CLI inherit this process's environment) with
///
/// `cargo test -p smithagents-client --test e2e -- --ignored real_claude_smoke --nocapture`
///
/// The temporary home and repo are deleted afterwards; the CLI's own session
/// record under `~/.claude/projects/` (named after the worktree path) stays.
#[tokio::test]
#[ignore = "calls the real claude CLI, which must be installed and logged in"]
async fn real_claude_smoke() {
    let bin = daemon_bin();
    // The repo lives in a temporary directory of its own, apart from the home
    // (declared first, so the daemon is stopped before it is deleted).
    let repo_tmp = tempfile::tempdir().unwrap();
    let home = TestHome::new();
    bounded(SMOKE_DEADLINE, async {
        println!("home: {}", home.path().display());
        println!("repo: {}", repo_tmp.path().display());
        let client = ensure(&home, bin, &SpawnOptions::default()).await;
        let mut events = client.events(None).await.unwrap();
        let agent = new_agent(&client, &repo_tmp.path().join("repo")).await;
        // Starts with "- ": the CLI must get it as the prompt, not as an option.
        client
            .send_message(agent.id, "- reply with the word pong")
            .await
            .unwrap();
        let end = turn_end(&mut events, agent.id).await;
        let messages = chat(&client, agent.id).await;

        println!("agent: {} on {:?}", agent.name, agent.branch);
        println!("worktree: {}", agent.worktree_path.display());
        println!("state: {:?}", end.state);
        for (role, text) in &messages {
            println!("{role:?}: {text}");
        }
        assert_eq!(end.state, AgentState::Done, "daemon log:\n{}", home.log());
        assert!(
            messages.iter().any(
                |(role, text)| *role == Role::Assistant && text.to_lowercase().contains("pong")
            ),
            "{messages:?}"
        );
        let branch = agent.branch.as_deref().unwrap();
        assert!(branch.starts_with("agent/mercury-"), "{branch}");
        assert_eq!(
            git(&agent.worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"]),
            branch
        );
    })
    .await;
}

/// Polls intent `id` until `wanted` accepts it (a failed pass ends the wait
/// with a panic, since the smoke test never retries).
async fn poll_intent(
    client: &Client,
    id: smithagents_core::ids::IntentId,
    what: &str,
    wanted: impl Fn(&smithagents_core::intent::Intent) -> bool,
) -> smithagents_core::intent::Intent {
    let deadline = Instant::now() + Duration::from_secs(150);
    loop {
        let intent = client.get_intent(id).await.unwrap();
        if let smithagents_core::intent::Phase::Failed { pass, message } = &intent.phase {
            panic!("pass {pass} failed while waiting for {what}: {message}");
        }
        if wanted(&intent) {
            return intent;
        }
        assert!(Instant::now() < deadline, "{what} did not happen in time");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Manual smoke of the polish passes against the real `claude` CLI (see
/// `real_claude_smoke` for the setup). It makes one short chat turn and two
/// small structured calls (polish passes 1 and 2, on the model named by
/// `SMITHAGENTS_POLISH_MODEL=haiku`). It does not Send, so no agent runs.
///
/// `cargo test -p smithagents-client --test e2e -- --ignored real_claude_polish_smoke --nocapture`
#[tokio::test]
#[ignore = "calls the real claude CLI, which must be installed and logged in"]
async fn real_claude_polish_smoke() {
    use smithagents_core::intent::{
        AnswerInput, CreateIntentRequest, IntentState, ItemStatus, Phase, SeedInput,
    };

    let bin = daemon_bin();
    let repo_tmp = tempfile::tempdir().unwrap();
    let home = TestHome::new();
    bounded(SMOKE_DEADLINE, async {
        let options = SpawnOptions {
            args: Vec::new(),
            env: vec![("SMITHAGENTS_POLISH_MODEL".into(), "haiku".into())],
        };
        let client = ensure(&home, bin, &options).await;
        let mut events = client.events(None).await.unwrap();
        let repo_dir = repo_tmp.path().join("repo");
        init_repo(&repo_dir);
        std::fs::write(
            repo_dir.join("greet.py"),
            "def greet(name):\n    return \"hello \" + name\n",
        )
        .unwrap();
        git(&repo_dir, &["add", "greet.py"]);
        git(&repo_dir, &["commit", "-m", "add greet"]);
        let repo = client
            .add_repo(&repo_dir, AddSourceControl::Git)
            .await
            .unwrap();
        let agent = client.create_agent(repo.id).await.unwrap();

        client
            .send_message(agent.id, "Reply with the single word: ok")
            .await
            .unwrap();
        let end = turn_end(&mut events, agent.id).await;
        assert_eq!(end.state, AgentState::Done, "daemon log:\n{}", home.log());

        let created = client
            .create_intent(CreateIntentRequest {
                kind: None,
                repo_id: None,
                seed: SeedInput::Chat {
                    agent_id: agent.id,
                    from_seq: None,
                    to_seq: None,
                },
            })
            .await
            .unwrap();
        let pass1 = poll_intent(&client, created.id, "pass 1", |i| {
            !matches!(i.phase, Phase::Polishing { pass: 1 })
        })
        .await;
        println!("title: {}", pass1.content.title);
        for q in &pass1.content.questions {
            println!("question: {} ({} options)", q.text, q.options.len());
        }
        assert!(!pass1.content.title.trim().is_empty());
        assert!(pass1.content.questions.len() <= 5);
        for q in &pass1.content.questions {
            assert!(q.options.len() >= 2, "{q:?}");
        }
        if pass1.phase == Phase::Questions {
            for q in &pass1.content.questions {
                client
                    .answer_question(created.id, q.id, AnswerInput::Skip)
                    .await
                    .unwrap();
            }
        }
        let done = poll_intent(&client, created.id, "pass 2", |i| {
            matches!(i.phase, Phase::Review) || i.state == IntentState::Complete
        })
        .await;
        println!("{:#?}", done.content);
        for item in done
            .content
            .preconditions
            .iter()
            .chain(&done.content.criteria)
            .chain(&done.content.stories)
            .chain(&done.content.depends_on)
        {
            assert_eq!(item.status, ItemStatus::Pending, "{item:?}");
        }
    })
    .await;
}

# Daemon Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A headless local daemon (`smithagentsd`) and a Rust client crate that create agents with chat sessions through the Claude Code CLI, in per-agent git worktrees, with token-authenticated HTTP/WebSocket access and a find-or-start lifecycle.

**Architecture:** One Cargo workspace with three crates: `core` (shared domain types and pure allocation logic), `server` (store, git, harness adapter, event bus, agent service, API, daemon lifecycle), `client` (typed API client plus `ensure_daemon`). The daemon is always a separate process; everything, native and web clients alike, talks to it over loopback HTTP/WS with a per-start token. Intents, polish and execution come in Plan 2; UI comes in Plan 3.

**Tech Stack:** Rust (stable, edition 2021), tokio, axum (HTTP + WebSocket), sqlx (SQLite + migrations), serde, uuid, chrono, thiserror, tracing, fs2 (instance lock), subtle (constant-time compare), rand, reqwest + tokio-tungstenite (client), tempfile (tests).

**Spec:** `docs/superpowers/specs/2026-11-23-smithagents-architecture-design.md` (sections 4.3, 4.4 Agents, 6 Version 1). Roadmap: `docs/superpowers/plans/2026-11-24-v1-roadmap.md`.

## Global Constraints

- Daemon is a separate process, the same binary and API a remote host will use.
- Listens on 127.0.0.1 only, random port; a secret token generated at daemon start is required on every request.
- Lockfile `<home>/run/daemon.json` holds pid, port, token, version; owner-only permissions (0600).
- Idle daemon (no WebSocket clients and no running agents) exits after 10 minutes (default); closing a client never stops it while agents work.
- State root is `~/.smithagents/`; env override `SMITHAGENTS_HOME`. Naming is `smithagents`, never `singularity`.
- v1 harness is the Claude Code headless CLI only, behind a `Harness` trait; prompt permissions are bypassed (agents run unattended in their worktrees).
- Every chat is a harness session id and every message resumes it.
- Agent names come from the pool "Planets" (Mercury, Venus, Earth, Mars, Jupiter, Saturn, Uranus, Neptune): first name with no open agent.
- Agent colors come from an equal-saturation palette excluding red, yellow and green, allocated first unused, wrapping. The palette lives in `design/tokens/agent-palette.json` (OKLCH), the single source of truth shared later with the UI themes.
- Each agent gets its own git worktree off the chosen repository; the user's main checkout is never touched; a GitHub remote is not required.
- Activity states: working, quiet, done, blocked, waiting, stopped, dead (plus `idle` for an agent that has not run yet). This plan produces idle, working, quiet, done and dead; blocked, waiting and stopped need harness signals and arrive later.
- macOS (Apple silicon) is the tested platform; code stays portable.

## Review Focus

1. Two daemons started at once: exactly one survives, the other's caller connects to it (tests in Tasks 10 and 11).
2. The harness process exits non-zero, prints malformed JSON, or emits unknown event types mid-turn: unknown types ignored, failure marks the agent dead, the error is persisted, the session id is kept for resume (Tasks 5, 6, 8).
3. A second message while a turn runs is rejected with 409 `turn_in_progress` and spawns no second process (Tasks 8, 9).
4. Repository paths with spaces and unicode, non-git directories, bare repos, detached HEAD, and an existing branch name give clear typed errors (Task 4).
5. Missing or wrong token gives 401 without leaking timing; lockfile is 0600; a megabyte-long stream-json line does not break parsing (Tasks 5, 9, 10).

---

### Task 1: Workspace scaffold

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `crates/core/{Cargo.toml,src/lib.rs}`, `crates/server/{Cargo.toml,src/lib.rs}`, `crates/client/{Cargo.toml,src/lib.rs}`
- Test: `crates/core/tests/smoke.rs`

**Interfaces:**
- Produces: workspace members `smithagents-core`, `smithagents-server`, `smithagents-client`; `smithagents_core::VERSION: &str` (the crate version).

- [ ] **Step 1: Write the failing test** in `crates/core/tests/smoke.rs`:

```rust
#[test]
fn version_is_non_empty() {
    assert!(!smithagents_core::VERSION.is_empty());
}
```

- [ ] **Step 2: Run** `cargo test -p smithagents-core` — Expected: FAIL (crate does not exist yet).
- [ ] **Step 3: Create the workspace.** Root `Cargo.toml` with `[workspace] members = ["crates/*"]`, `resolver = "2"`, and `[workspace.dependencies]` for every library in the Tech Stack line (pin current versions), `[workspace.lints.rust] unsafe_code = "forbid"`. `rust-toolchain.toml` pins `stable`. `.gitignore` lists `/target`. `core/src/lib.rs` exports `pub const VERSION: &str = env!("CARGO_PKG_VERSION");`. Server and client libs are empty for now.
- [ ] **Step 4: Run** `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --check` — Expected: PASS.
- [ ] **Step 5: Commit** `git add -A && git commit -m "chore: cargo workspace scaffold"`

---

### Task 2: Core domain types, name pool and color palette

**Files:**
- Create: `design/tokens/agent-palette.json`, `crates/core/src/{ids.rs,agent.rs,palette.rs,event.rs,daemon.rs}`
- Modify: `crates/core/src/lib.rs` (re-export modules)
- Test: `crates/core/src/agent.rs` and `crates/core/src/palette.rs` (`#[cfg(test)]` modules)

**Interfaces:**
- Produces (all `Serialize + Deserialize + Clone + Debug + PartialEq` unless noted):
  - `ids`: `AgentId(pub Uuid)`, `RepoId(pub Uuid)`, `MessageId(pub Uuid)` newtypes with `new()`, `Display`, `FromStr`, `Copy`, `Eq`, `Hash`.
  - `agent`: `AgentState { Idle, Working, Quiet, Done, Blocked, Waiting, Stopped, Dead }` (snake_case serde); `Repo { id: RepoId, path: PathBuf, default_branch: String, created_at: DateTime<Utc> }`; `Agent { id: AgentId, name: String, color: AgentColor, repo_id: RepoId, worktree_path: PathBuf, branch: String, harness_session_id: Option<String>, state: AgentState, created_at: DateTime<Utc>, closed_at: Option<DateTime<Utc>> }`; `Role { User, Assistant, Tool }`; `ChatMessage { id: MessageId, agent_id: AgentId, seq: u64, role: Role, text: String, created_at: DateTime<Utc> }`.
  - `palette`: `AgentColor { l: f32, c: f32, h: f32 }` (`Copy`); `palette() -> &'static [AgentColor]` parsed once (`OnceLock`) from `include_str!("../../../design/tokens/agent-palette.json")`; `RESERVED_HUES` read from the same file via `reserved_hue_ranges() -> &'static [(f32, f32)]`.
  - `agent`: `PLANETS: [&str; 8]`; `next_agent_name(pool: &[&str], open: &HashSet<String>) -> String` (first unused; when all are open, appends ` 2`, ` 3`, … at the first suffix level that has a free name); `next_color(open: &[AgentColor]) -> AgentColor` (first palette color not in `open`; when exhausted, `palette()[open.len() % palette().len()]`).
  - `event`: `Event { seq: u64, at: DateTime<Utc>, kind: EventKind }`; `EventKind { AgentCreated(Agent), AgentUpdated(Agent), AgentClosed(AgentId), MessageAppended(ChatMessage), TurnDelta { agent_id: AgentId, text: String } }`.
  - `daemon`: `DaemonInfo { pid: u32, port: u16, token: String, version: String }` (serde, `Clone`, `Debug`, `PartialEq`); `lockfile_path(home: &Path) -> PathBuf` (`<home>/run/daemon.json`); `read_lockfile(home: &Path) -> io::Result<Option<DaemonInfo>>` (`Ok(None)` when the file is absent). Writing the lockfile is the server's job (Task 10).

- [ ] **Step 1: Create `design/tokens/agent-palette.json`** with `reserved_hue_ranges: { red: [350, 50], yellow: [80, 115], green: [120, 170] }` (red wraps through 0) and six colors, all `l: 0.72, c: 0.12`, hues 190, 220, 250, 280, 310, 340 named cyan, azure, blue, violet, purple, magenta.
- [ ] **Step 2: Write the failing tests:**

```rust
// agent.rs
#[test] fn next_agent_name_takes_first_unused() { /* open {Mercury, Venus} -> "Earth" */ }
#[test] fn next_agent_name_fills_gaps() { /* open {Venus} -> "Mercury" */ }
#[test] fn next_agent_name_exhausted_appends_suffix() { /* all 8 open -> "Mercury 2" */ }
#[test] fn agent_serde_round_trip() { /* Agent -> json -> Agent == original */ }
// palette.rs
#[test] fn palette_hues_avoid_reserved_ranges() { /* every h outside red (wrapping), yellow, green */ }
#[test] fn palette_has_equal_lightness_and_chroma() { /* all l equal and all c equal */ }
#[test] fn next_color_first_unused() { /* open [palette[0]] -> palette[1] */ }
#[test] fn next_color_wraps_when_exhausted() { /* open = whole palette (6) -> palette[0]; open len 7 -> palette[1] */ }
```

- [ ] **Step 3: Run** `cargo test -p smithagents-core` — Expected: FAIL (items not defined).
- [ ] **Step 4: Implement** the interfaces above in the listed files. Hue range check must handle the wrapping red range (`start > end` means `h >= start || h <= end`).
- [ ] **Step 5: Run** `cargo test -p smithagents-core` — Expected: PASS.
- [ ] **Step 6: Commit** `git add -A && git commit -m "feat(core): domain types, planet names, agent color palette"`

---

### Task 3: SQLite store

**Files:**
- Create: `crates/server/src/store.rs`, `crates/server/migrations/0001_init.sql`
- Modify: `crates/server/src/lib.rs` (`pub mod store;`), `crates/server/Cargo.toml`
- Test: `crates/server/src/store.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: Task 2 types.
- Produces: `Store` (`Clone`) with
  - `async fn open(path: &Path) -> Result<Store, StoreError>` (creates the file, runs migrations), `async fn open_in_memory() -> Result<Store, StoreError>`
  - `insert_repo(&self, &Repo) -> Result<(), StoreError>`, `get_repo(RepoId) -> Result<Option<Repo>, _>`, `list_repos() -> Result<Vec<Repo>, _>`, `repo_by_path(&Path) -> Result<Option<Repo>, _>`
  - `insert_agent(&self, &Agent)`, `get_agent(AgentId) -> Result<Option<Agent>, _>`, `list_agents(include_closed: bool) -> Result<Vec<Agent>, _>`, `update_agent_state(AgentId, AgentState)`, `set_harness_session(AgentId, &str)`, `close_agent(AgentId, DateTime<Utc>)`
  - `append_message(&self, AgentId, Role, &str) -> Result<ChatMessage, _>` (assigns the next per-agent `seq` starting at 1, atomically), `list_messages(AgentId, after_seq: u64, limit: u32) -> Result<Vec<ChatMessage>, _>`
  - `StoreError { Sqlx(sqlx::Error), Migrate(..), DuplicateRepoPath }`
- Schema: `repos(id PK, path UNIQUE, default_branch, created_at)`, `agents(id PK, name, color_json, repo_id FK, worktree_path, branch, harness_session_id NULL, state, created_at, closed_at NULL)`, `messages(id PK, agent_id FK, seq, role, text, created_at, UNIQUE(agent_id, seq))`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[tokio::test] async fn state_persists_across_reopen() { /* tempfile db: insert repo+agent, drop, reopen, get_agent equals */ }
#[tokio::test] async fn message_seq_is_monotonic_per_agent() { /* two agents interleaved: each gets 1,2,3 */ }
#[tokio::test] async fn concurrent_appends_get_unique_seqs() { /* 20 tasks append to one agent: seqs are exactly 1..=20 */ }
#[tokio::test] async fn closed_agents_hidden_unless_requested() { /* close_agent -> list_agents(false) excludes, (true) includes */ }
#[tokio::test] async fn duplicate_repo_path_is_rejected() { /* second insert_repo same path -> StoreError::DuplicateRepoPath */ }
```

- [ ] **Step 2: Run** `cargo test -p smithagents-server store::` — Expected: FAIL.
- [ ] **Step 3: Implement** `Store` over `sqlx::SqlitePool` with `sqlx::migrate!()`; store `AgentColor` as JSON text; assign `seq` inside one transaction (`SELECT COALESCE(MAX(seq),0)+1`), relying on the `UNIQUE(agent_id, seq)` constraint plus `BEGIN IMMEDIATE` for concurrency; enable WAL.
- [ ] **Step 4: Run** `cargo test -p smithagents-server store::` — Expected: PASS.
- [ ] **Step 5: Commit** `git add -A && git commit -m "feat(server): sqlite store for repos, agents, messages"`

---

### Task 4: Repositories and worktrees (git CLI)

**Files:**
- Create: `crates/server/src/git.rs`
- Modify: `crates/server/src/lib.rs`
- Test: `crates/server/src/git.rs` (`#[cfg(test)]`, uses `tempfile` and real `git`)

**Interfaces:**
- Produces:
  - `RepoInfo { root: PathBuf, default_branch: String }`
  - `async fn validate_repo(path: &Path) -> Result<RepoInfo, GitError>` (resolves to the repo root via `git rev-parse --show-toplevel`; default branch from `origin/HEAD` if present, else the current branch)
  - `async fn create_worktree(repo_root: &Path, worktrees_dir: &Path, branch: &str, base: &str) -> Result<PathBuf, GitError>` (new branch `branch` from `base`, directory `worktrees_dir/<branch with '/' replaced by '-'>`)
  - `async fn remove_worktree(repo_root: &Path, worktree: &Path) -> Result<(), GitError>`
  - `GitError { NotAGitRepo, BareRepo, NoDefaultBranch, BranchExists(String), Command { stderr: String }, Io(std::io::Error) }`
- Approach: shell out to `git` with `tokio::process::Command`, arguments as separate args (never through a shell), so paths with spaces and unicode need no escaping.

- [ ] **Step 1: Write the failing tests** (each builds a throwaway repo via `git init -b main` plus one commit):

```rust
#[tokio::test] async fn validate_repo_returns_root_and_default_branch() { /* "main" */ }
#[tokio::test] async fn validate_repo_handles_spaces_and_unicode_paths() { /* dir "répo é 1" works */ }
#[tokio::test] async fn validate_repo_rejects_non_git_dir() { /* GitError::NotAGitRepo */ }
#[tokio::test] async fn validate_repo_rejects_bare_repo() { /* git init --bare -> GitError::BareRepo */ }
#[tokio::test] async fn validate_repo_detached_head_without_main_errors() { /* GitError::NoDefaultBranch */ }
#[tokio::test] async fn create_worktree_checks_out_new_branch_from_base() { /* dir exists, HEAD branch == new branch */ }
#[tokio::test] async fn create_worktree_existing_branch_errors() { /* GitError::BranchExists("agent/x") */ }
#[tokio::test] async fn remove_worktree_deletes_directory() { /* path no longer exists */ }
```

- [ ] **Step 2: Run** `cargo test -p smithagents-server git::` — Expected: FAIL.
- [ ] **Step 3: Implement** the functions above; map stderr patterns ("not a git repository", "is bare", "already exists") to the typed errors.
- [ ] **Step 4: Run** `cargo test -p smithagents-server git::` — Expected: PASS.
- [ ] **Step 5: Commit** `git add -A && git commit -m "feat(server): git repo validation and worktrees"`

---

### Task 5: Harness trait, fake harness and Claude stream-json parser

**Files:**
- Create: `crates/server/src/harness/{mod.rs,fake.rs,claude_parser.rs}`, `crates/server/tests/fixtures/claude_stream_hello.jsonl`
- Modify: `crates/server/src/lib.rs`, `crates/server/Cargo.toml` (feature `test-util` gating `fake`)
- Test: `crates/server/src/harness/claude_parser.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `TurnRequest { prompt: String, cwd: PathBuf, resume_session_id: Option<String>, model: Option<String>, system_prompt_append: Option<String> }`
  - `HarnessEvent { SessionStarted { session_id: String }, AssistantText { text: String }, ToolUse { name: String, summary: String }, TurnCompleted { session_id: String, is_error: bool, result_text: Option<String> }, Failed { message: String } }`
  - `#[async_trait] trait Harness: Send + Sync { async fn run_turn(&self, req: TurnRequest) -> Result<BoxStream<'static, HarnessEvent>, HarnessError>; }`; `HarnessError { Spawn(std::io::Error) }`
  - `FakeHarness::scripted(turns: Vec<Vec<HarnessEvent>>) -> FakeHarness` (each `run_turn` pops the next script and records the `TurnRequest`; `requests() -> Vec<TurnRequest>`; an optional per-event delay `with_delay(Duration)`), available under feature `test-util`.
  - `claude_parser::parse_line(line: &str) -> Result<Option<HarnessEvent>, ParseError>`: `Ok(None)` for valid JSON of an unknown or uninteresting type; `Err(ParseError::InvalidJson)` for non-JSON.

- [ ] **Step 1: Capture a real fixture.** Run `claude --help` and confirm the flags `-p`, `--output-format stream-json`, `--verbose`, `--include-partial-messages`, `--resume`, `--dangerously-skip-permissions`, `--append-system-prompt`, `--model` exist (adjust Task 6 if names differ). Then run `claude -p "say hi" --output-format stream-json --verbose --include-partial-messages > crates/server/tests/fixtures/claude_stream_hello.jsonl`. If `claude` is unavailable, hand-write a fixture from the documented stream-json shapes (a `system` init line with `session_id`, `stream_event` text deltas, an `assistant` message, a `result` line) and note that in the commit message.
- [ ] **Step 2: Write the failing tests:**

```rust
#[test] fn parses_system_init_to_session_started() { /* init line -> SessionStarted{session_id} */ }
#[test] fn parses_text_delta_to_assistant_text() { /* stream_event content_block_delta text_delta -> AssistantText */ }
#[test] fn parses_tool_use_block_to_tool_use() { /* name and short summary */ }
#[test] fn parses_result_success() { /* is_error false -> TurnCompleted{is_error:false, result_text:Some(..)} */ }
#[test] fn parses_result_error() { /* is_error true -> TurnCompleted{is_error:true} */ }
#[test] fn unknown_event_type_returns_none() { /* {"type":"brand_new"} -> Ok(None) */ }
#[test] fn invalid_json_returns_err() { /* "not json" -> Err(InvalidJson) */ }
#[test] fn parser_handles_megabyte_line() { /* 1 MiB text_delta line parses */ }
#[test] fn fixture_parses_to_started_then_text_then_completed() { /* all lines of the fixture, in order */ }
```

- [ ] **Step 3: Run** `cargo test -p smithagents-server harness::` — Expected: FAIL.
- [ ] **Step 4: Implement** `Harness`, `HarnessEvent`, `FakeHarness` and `parse_line` (serde_json into a loose `Value`, match on `type` and `subtype`; never panic on missing fields, return `Ok(None)` instead).
- [ ] **Step 5: Run** `cargo test -p smithagents-server --features test-util harness::` — Expected: PASS.
- [ ] **Step 6: Commit** `git add -A && git commit -m "feat(server): harness trait, fake harness, claude stream parser"`

---

### Task 6: Claude Code process harness

**Files:**
- Create: `crates/server/src/harness/claude.rs`, `crates/server/tests/bin/fake-claude` (executable `sh` script)
- Modify: `crates/server/src/harness/mod.rs`
- Test: `crates/server/tests/claude_harness.rs`

**Interfaces:**
- Consumes: Task 5 (`Harness`, `TurnRequest`, `HarnessEvent`, `parse_line`).
- Produces: `ClaudeCodeHarness { bin: PathBuf }` implementing `Harness`. Spawns `<bin> -p <prompt> --output-format stream-json --verbose --include-partial-messages --dangerously-skip-permissions` plus `--resume <id>`, `--model <m>`, `--append-system-prompt <s>` when set, with `current_dir(cwd)` and `kill_on_drop(true)`. Reads stdout line by line (no line-length cap), maps lines with `parse_line`, skips `Ok(None)` and logs `Err`, and ends the stream with `Failed { message }` (including the last stderr lines) when the process exits non-zero.
- `fake-claude` script: prints the file named by `$FAKE_CLAUDE_FIXTURE`, writes its argv (one per line) to `$FAKE_CLAUDE_ARGS_FILE`, exits with `${FAKE_CLAUDE_EXIT:-0}`, and if `$FAKE_CLAUDE_SLEEP` is set, sleeps that many seconds before exiting.

- [ ] **Step 1: Write the failing tests** in `crates/server/tests/claude_harness.rs` (set env on the child via a helper that wraps the script):

```rust
#[tokio::test] async fn args_include_stream_json_and_bypass_permissions() { /* args file contains each flag */ }
#[tokio::test] async fn resume_flag_present_only_when_session_given() { /* with Some("abc") contains "--resume\nabc"; with None it does not */ }
#[tokio::test] async fn streams_events_from_fixture() { /* collected events == [SessionStarted, AssistantText.., TurnCompleted] */ }
#[tokio::test] async fn nonzero_exit_yields_failed_event() { /* FAKE_CLAUDE_EXIT=3 -> last event Failed */ }
#[tokio::test] async fn malformed_line_is_skipped_not_fatal() { /* fixture with one bad line still completes */ }
#[tokio::test] async fn dropping_stream_kills_process() { /* FAKE_CLAUDE_SLEEP=30; drop stream; process gone within 2 s */ }
```

- [ ] **Step 2: Run** `cargo test -p smithagents-server --test claude_harness` — Expected: FAIL.
- [ ] **Step 3: Implement** `ClaudeCodeHarness::run_turn` returning a stream built from a spawned task and an `mpsc` channel (dropping the stream drops the receiver, which ends the task, which drops the child and kills it).
- [ ] **Step 4: Run** the same command — Expected: PASS.
- [ ] **Step 5: Commit** `git add -A && git commit -m "feat(server): claude code process harness"`

---

### Task 7: Event bus

**Files:**
- Create: `crates/server/src/events.rs`
- Modify: `crates/server/src/lib.rs`
- Test: `crates/server/src/events.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: Task 2 `Event`, `EventKind`.
- Produces: `EventBus` (`Clone`) with `EventBus::new(capacity: usize)`, `publish(&self, EventKind) -> Event` (assigns the next `seq` starting at 1, stamps `at`), `subscribe(&self, since: Option<u64>) -> Result<Subscription, BusError>` where `Subscription { backlog: Vec<Event>, live: broadcast::Receiver<Event> }` (backlog holds retained events with `seq > since`, with no gap or duplicate against `live`), `latest_seq(&self) -> u64`; `BusError::TooOld { oldest_retained: u64 }`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[tokio::test] async fn seq_is_monotonic_from_one() { /* three publishes -> 1,2,3 */ }
#[tokio::test] async fn subscribe_since_replays_backlog_then_live() { /* since=1 after 3 events -> backlog [2,3]; next publish arrives on live as 4 */ }
#[tokio::test] async fn subscribe_since_too_old_errors() { /* capacity 2, publish 5, since=1 -> TooOld{oldest_retained:4} */ }
#[tokio::test] async fn slow_subscriber_does_not_block_publisher() { /* never-polled receiver; 10_000 publishes finish within 1 s */ }
#[tokio::test] async fn no_gap_between_backlog_and_live_under_concurrent_publish() { /* publisher task running while subscribing; union is contiguous */ }
```

- [ ] **Step 2: Run** `cargo test -p smithagents-server events::` — Expected: FAIL.
- [ ] **Step 3: Implement** with a mutex-guarded ring buffer (`VecDeque<Event>`) and a `tokio::sync::broadcast` channel; subscribe takes the lock so the backlog snapshot and the receiver creation are atomic against `publish`.
- [ ] **Step 4: Run** `cargo test -p smithagents-server events::` — Expected: PASS.
- [ ] **Step 5: Commit** `git add -A && git commit -m "feat(server): resumable event bus"`

---

### Task 8: Agent service

**Files:**
- Create: `crates/server/src/service.rs`, `crates/server/src/paths.rs`
- Modify: `crates/server/src/lib.rs`
- Test: `crates/server/src/service.rs` (`#[cfg(test)]`, uses `FakeHarness`, in-memory store, temp git repo)

**Interfaces:**
- Consumes: Tasks 2–7.
- Produces:
  - `Paths { home: PathBuf }` with `run_dir()`, `db_path()`, `worktrees_dir()`, `Paths::from_env() -> Paths` (`SMITHAGENTS_HOME`, else `~/.smithagents`).
  - `AgentService` (`Clone`) built by `AgentService::new(store: Store, bus: EventBus, harness: Arc<dyn Harness>, paths: Paths, quiet_after: Duration)`:
    - `async fn add_repo(&self, path: &Path) -> Result<Repo, ServiceError>`
    - `async fn create_agent(&self, repo_id: RepoId) -> Result<Agent, ServiceError>`: name via `next_agent_name(PLANETS, open names)`, color via `next_color(open colors)`, branch `agent/<lowercase-name-with-spaces-as-dashes>-<first 8 hex of uuid>`, worktree from `create_worktree(repo, worktrees_dir/<repo dir name>, branch, repo.default_branch)`, state `Idle`; publishes `AgentCreated`.
    - `async fn close_agent(&self, id: AgentId) -> Result<(), ServiceError>`: sets `closed_at` (frees name and color), publishes `AgentClosed`; does not delete the worktree.
    - `async fn send_message(&self, id: AgentId, text: String) -> Result<ChatMessage, ServiceError>`: rejects with `TurnInProgress` while a turn runs; persists the user message (publishes `MessageAppended`); sets `Working`; spawns the turn task.
    - `fn running_agent_count(&self) -> usize` (agents in `Working` or `Quiet`).
    - `ServiceError { RepoNotFound, AgentNotFound, AgentClosed, TurnInProgress, Git(GitError), Store(StoreError) }`
  - Turn behavior: run the harness in the agent's worktree with `resume_session_id = agent.harness_session_id`; on `SessionStarted` persist the id; on `AssistantText` publish `TurnDelta` and accumulate; on `ToolUse` append a `Tool` message with the summary; no event for `quiet_after` moves the agent to `Quiet` (any later event returns it to `Working`); on `TurnCompleted` append one `Assistant` message with the accumulated text (or `result_text` when no deltas arrived), state `Done`; on `Failed` or an early stream end append an `Assistant`-role error message prefixed `Agent process failed:`, state `Dead`, session id kept. A `Dead` agent may be messaged again and resumes its session. Every state change publishes `AgentUpdated`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[tokio::test] async fn create_agent_assigns_planet_names_in_order() { /* Mercury, Venus, Earth */ }
#[tokio::test] async fn create_agent_assigns_distinct_palette_colors() { /* first three differ, equal to palette[0..3] */ }
#[tokio::test] async fn closing_agent_frees_name_and_color() { /* close Mercury; next create is Mercury with palette[0] */ }
#[tokio::test] async fn create_agent_makes_worktree_on_agent_branch() { /* worktree dir exists; branch starts with "agent/mercury-" */ }
#[tokio::test] async fn send_message_persists_and_streams_reply() { /* messages: user then assistant with full text; TurnDelta events seen; state Done */ }
#[tokio::test] async fn second_send_while_running_is_turn_in_progress() { /* FakeHarness with delay; second call -> TurnInProgress; harness requests().len()==1 */ }
#[tokio::test] async fn session_id_is_saved_and_reused_on_next_turn() { /* 2nd request.resume_session_id == Some(id from 1st) */ }
#[tokio::test] async fn harness_failure_marks_agent_dead_and_keeps_session_id() { /* Failed -> state Dead, error message persisted, session id kept */ }
#[tokio::test] async fn dead_agent_can_be_messaged_again_and_resumes() { /* after Dead, send again -> Working then Done, request carries session id */ }
#[tokio::test] async fn silent_turn_becomes_quiet_then_working_on_next_event() { /* quiet_after=50ms, delay 200ms before first event */ }
#[tokio::test] async fn closed_agent_rejects_messages() { /* ServiceError::AgentClosed */ }
```

- [ ] **Step 2: Run** `cargo test -p smithagents-server --features test-util service::` — Expected: FAIL.
- [ ] **Step 3: Implement** `AgentService` with a `HashSet<AgentId>` of running turns behind a mutex (insert before spawning, remove in a drop guard so a panicking task cannot leave an agent stuck).
- [ ] **Step 4: Run** the same command — Expected: PASS.
- [ ] **Step 5: Commit** `git add -A && git commit -m "feat(server): agent service with turn orchestration"`

---

### Task 9: HTTP and WebSocket API

**Files:**
- Create: `crates/server/src/api.rs`
- Modify: `crates/server/src/lib.rs`
- Test: `crates/server/tests/api.rs`

**Interfaces:**
- Consumes: Task 8 `AgentService`, Task 7 `EventBus`, Task 2 types.
- Produces: `pub fn router(state: AppState) -> axum::Router` with `AppState { service: AgentService, bus: EventBus, store: Store, token: Arc<str>, ws_clients: Arc<AtomicUsize>, started_at: Instant }`. Every route requires `Authorization: Bearer <token>`, compared in constant time (`subtle`). Errors are JSON `{ "error": "<code>", "message": "<text>" }`.
  - `GET /v1/health` → `{ version, pid, running_agents, ws_clients }`
  - `POST /v1/repos` `{ path }` → `Repo`; errors `400 not_a_git_repo`, `400 bare_repo`, `409 repo_exists`
  - `GET /v1/repos` → `[Repo]`
  - `POST /v1/agents` `{ repo_id }` → `Agent`; `404 repo_not_found`
  - `GET /v1/agents?include_closed=false` → `[Agent]`
  - `POST /v1/agents/{id}/close` → `204`
  - `POST /v1/agents/{id}/messages` `{ text }` → `202 ChatMessage` (the persisted user message); `409 turn_in_progress`, `404 agent_not_found`, `409 agent_closed`
  - `GET /v1/agents/{id}/messages?after=0&limit=200` → `[ChatMessage]`
  - `GET /v1/events?since=<seq>` WebSocket: sends the backlog then live `Event` JSON text frames; `410 events_too_old` (plain HTTP, before the upgrade) when `since` predates the retained window; increments `ws_clients` while connected.

- [ ] **Step 1: Write the failing tests** (build the router in-process; use `tower::ServiceExt::oneshot` for HTTP and a real listener plus `tokio-tungstenite` for WebSocket):

```rust
#[tokio::test] async fn missing_token_is_401() {}
#[tokio::test] async fn wrong_token_is_401() {}
#[tokio::test] async fn health_returns_version_and_counts() {}
#[tokio::test] async fn add_repo_rejects_non_git_dir() { /* 400 not_a_git_repo */ }
#[tokio::test] async fn create_agent_returns_name_and_color() { /* name "Mercury", color == palette[0] */ }
#[tokio::test] async fn send_message_second_call_is_409_turn_in_progress() {}
#[tokio::test] async fn messages_after_seq_pages() { /* after=2 returns seq 3.. */ }
#[tokio::test] async fn events_ws_replays_since_then_streams_live() {}
#[tokio::test] async fn events_since_too_old_is_410() {}
#[tokio::test] async fn ws_clients_count_tracks_connections() { /* 1 while open, 0 after close */ }
```

- [ ] **Step 2: Run** `cargo test -p smithagents-server --features test-util --test api` — Expected: FAIL.
- [ ] **Step 3: Implement** the router with an axum middleware layer for auth and a single `ApiError` type implementing `IntoResponse` that maps every `ServiceError` variant to the status and code above.
- [ ] **Step 4: Run** the same command — Expected: PASS.
- [ ] **Step 5: Commit** `git add -A && git commit -m "feat(server): token-authenticated http and websocket api"`

---

### Task 10: Daemon binary, lockfile and idle exit

**Files:**
- Create: `crates/server/src/lifecycle.rs`, `crates/server/src/bin/smithagentsd.rs`
- Modify: `crates/server/src/lib.rs`, `crates/server/Cargo.toml` (`[[bin]] name = "smithagentsd"`)
- Test: `crates/server/src/lifecycle.rs` (`#[cfg(test)]`), `crates/server/tests/daemon_process.rs`

**Interfaces:**
- Consumes: Tasks 8–9; `Paths`.
- Produces:
  - `fn write_lockfile(paths: &Paths, info: &DaemonInfo) -> io::Result<()>` (`DaemonInfo` and `read_lockfile` come from `smithagents_core::daemon`, Task 2; atomic: temp file then rename, mode 0600, creates `run/` with 0700), `fn remove_lockfile(paths: &Paths)`
  - `fn acquire_instance_lock(paths: &Paths) -> Result<InstanceLock, LockError>`: exclusive `fs2` try-lock on `run/daemon.lock`, held for the process lifetime; `LockError::AlreadyRunning`.
  - `IdleMonitor::new(timeout: Duration, ws_clients: Arc<AtomicUsize>, running: impl Fn() -> usize)` with `async fn wait_idle_exit(self)`: resolves once there are zero WebSocket clients and zero running agents continuously for `timeout`; any activity resets the timer.
  - Binary `smithagentsd [--home <dir>] [--claude-bin <path>] [--idle-timeout-secs <n>]` (defaults: `SMITHAGENTS_HOME`/`~/.smithagents`, `claude` on PATH, 600). Startup order: take the instance lock (exit code 3 with a message if already running) → open store → bind `127.0.0.1:0` → generate a 32-byte hex token with `rand` → write the lockfile → serve → on idle exit or SIGTERM remove the lockfile and exit 0.

- [ ] **Step 1: Write the failing tests:**

```rust
// lifecycle.rs
#[test] fn lockfile_is_0600_and_dir_is_0700() { /* check unix modes after write_lockfile */ }
#[test] fn lockfile_write_is_atomic_round_trip() { /* read_lockfile == written info */ }
#[test] fn second_instance_lock_fails_already_running() {}
#[tokio::test] async fn idle_monitor_exits_after_timeout_with_no_clients_or_agents() { /* timeout 200ms */ }
#[tokio::test] async fn idle_monitor_waits_while_ws_client_connected() {}
#[tokio::test] async fn idle_monitor_waits_while_agent_running() {}
#[tokio::test] async fn idle_monitor_resets_after_activity() {}
// tests/daemon_process.rs  (runs the real binary with a temp home)
#[tokio::test] async fn daemon_writes_lockfile_and_serves_health() { /* token from lockfile authenticates GET /v1/health */ }
#[tokio::test] async fn concurrent_start_single_instance() { /* spawn two daemons at once: exactly one stays running, the other exits with code 3 */ }
#[tokio::test] async fn daemon_exits_and_removes_lockfile_when_idle() { /* --idle-timeout-secs 1 */ }
```

- [ ] **Step 2: Run** `cargo test -p smithagents-server lifecycle:: --test daemon_process` — Expected: FAIL.
- [ ] **Step 3: Implement** `lifecycle.rs` and the binary (`tokio::select!` over the server future, `IdleMonitor::wait_idle_exit`, and a SIGTERM/ctrl-c signal).
- [ ] **Step 4: Run** the same command — Expected: PASS.
- [ ] **Step 5: Commit** `git add -A && git commit -m "feat(server): smithagentsd binary, lockfile, idle exit"`

---

### Task 11: Client crate and find-or-start

**Files:**
- Create: `crates/client/src/{lib.rs,api.rs,ensure.rs}`
- Modify: `crates/client/Cargo.toml`
- Test: `crates/client/tests/ensure.rs`

**Interfaces:**
- Consumes: Task 10 (the `smithagentsd` binary), Task 9 routes, Task 2 types including `core::daemon::{DaemonInfo, lockfile_path, read_lockfile}`. The client depends on `smithagents-core`, not on `server`.
- Produces:
  - `Client::new(info: &DaemonInfo) -> Client`; methods `health() -> Result<Health, ClientError>`, `add_repo(&Path) -> Result<Repo, _>`, `list_repos()`, `create_agent(RepoId) -> Result<Agent, _>`, `list_agents(include_closed: bool)`, `close_agent(AgentId)`, `send_message(AgentId, &str) -> Result<ChatMessage, _>`, `messages(AgentId, after_seq: u64) -> Result<Vec<ChatMessage>, _>`, `events(since: Option<u64>) -> Result<impl Stream<Item = Event>, _>`.
  - `ClientError { Http(reqwest::Error), Api { status: u16, code: String, message: String }, Ws(..) }`
  - `async fn ensure_daemon(home: &Path, daemon_bin: &Path, expected_version: &str) -> Result<Client, EnsureError>`: read lockfile → if present and `health()` succeeds: matching version returns the client, different version returns `EnsureError::VersionMismatch { running, expected }` (the caller asks the user before restarting); if the lockfile is stale (unreachable or pid dead): delete it and start; start = spawn `daemon_bin --home <home>` detached in its own session (`setsid`/`process_group(0)`) with stdio to `<home>/run/daemon.log`, then poll the lockfile every 50 ms up to 5 s; concurrent callers all end up on the one daemon that won the instance lock (a loser process exits with code 3, and the caller re-reads the lockfile). `EnsureError { VersionMismatch{..}, StartTimeout, Io(..) }`.

- [ ] **Step 1: Write the failing tests** in `crates/client/tests/ensure.rs` (temp home, the built `smithagentsd` via `env!("CARGO_BIN_EXE_smithagentsd")` path helper):

```rust
#[tokio::test] async fn ensure_starts_daemon_when_none_running() {}
#[tokio::test] async fn ensure_reuses_running_daemon() { /* same pid on second call */ }
#[tokio::test] async fn ensure_replaces_stale_lockfile() { /* lockfile with dead pid/port -> new daemon */ }
#[tokio::test] async fn ensure_reports_version_mismatch() { /* expected_version "0.0.0-other" */ }
#[tokio::test] async fn ensure_concurrent_calls_share_one_daemon() { /* 8 parallel calls, one pid */ }
#[tokio::test] async fn events_stream_yields_agent_created() { /* subscribe, create agent, receive AgentCreated */ }
```

- [ ] **Step 2: Run** `cargo test -p smithagents-client` — Expected: FAIL.
- [ ] **Step 3: Implement** `Client` (reqwest with the bearer header, `tokio-tungstenite` for events) and `ensure_daemon`.
- [ ] **Step 4: Run** `cargo test --workspace --features smithagents-server/test-util` — Expected: PASS.
- [ ] **Step 5: Commit** `git add -A && git commit -m "feat(client): typed client and ensure_daemon"`

---

### Task 12: End-to-end test with a fake claude

**Files:**
- Create: `crates/client/tests/e2e.rs`
- Test: same file

**Interfaces:**
- Consumes: everything above, with `crates/server/tests/bin/fake-claude` standing in for `claude` (`--claude-bin`).

- [ ] **Step 1: Write the failing tests:**

```rust
#[tokio::test] async fn chat_flow_end_to_end() {
    // temp home + temp git repo; ensure_daemon(claude-bin = fake-claude)
    // add_repo -> create_agent: name "Mercury", color == palette[0], worktree dir exists, branch checked out
    // send_message("hello") -> wait on events until Mercury state is Done
    // messages(): [User "hello", Assistant <fixture text>]
    // send_message("again") -> args file shows "--resume <session id from the fixture>"
    // close_agent -> list_agents(false) is empty; list_agents(true) has Mercury
}
#[tokio::test] async fn daemon_outlives_client_while_agent_working() {
    // FAKE_CLAUDE_SLEEP=3; send_message; drop the client and its event stream; daemon pid still alive after 1 s;
    // a fresh ensure_daemon returns the same pid and the agent reaches Done
}
#[tokio::test] async fn idle_daemon_exits_after_timeout() {
    // start with --idle-timeout-secs 1, no clients: lockfile gone and process exited within 4 s
}
#[tokio::test] async fn failed_turn_marks_agent_dead_then_resume_works() {
    // FAKE_CLAUDE_EXIT=3 first run -> Dead with "Agent process failed:" message; next run exit 0 -> Done, resumed with the saved session id
}
```

- [ ] **Step 2: Run** `cargo test -p smithagents-client --test e2e` — Expected: FAIL until the fake-claude wiring is complete; fix wiring only (no production changes expected).
- [ ] **Step 3: Run the full gate** `cargo test --workspace --features smithagents-server/test-util && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check` — Expected: PASS.
- [ ] **Step 4: Manual smoke with the real CLI** (needs `claude` logged in): start `smithagentsd --home /tmp/sa-smoke`, use a scratch script through the client crate to create an agent on a throwaway repo and send "reply with the word pong"; confirm an assistant message containing "pong" and a worktree on `agent/mercury-…`.
- [ ] **Step 5: Commit** `git add -A && git commit -m "test: end-to-end chat flow against fake claude"`

---

## Self-Review

- **Spec coverage:** daemon as separate process, loopback, token, lockfile (Tasks 9–11); idle exit and outliving the client (Tasks 10, 12); Claude Code CLI harness with resume (Tasks 5, 6, 8); names, colors, worktrees (Tasks 2, 4, 8); activity states idle/working/quiet/done/dead (Task 8; the rest deferred as noted). Not here by design: intents, polish, execution, queue (Plan 2), UI and AI SDK stream endpoint (Plan 3), theme system (Plan T; only the palette file is shared).
- **Type consistency:** `Agent`, `ChatMessage`, `Event`, `EventKind`, `AgentColor`, `DaemonInfo` are defined once in `core` (Task 2, `DaemonInfo` moved in Task 11) and used unchanged by later tasks; `ServiceError` variants map one-to-one to API error codes in Task 9.
- **Open verification items** (flagged in steps, not guessed): exact Claude CLI flag names and stream-json shapes (Task 5, step 1).

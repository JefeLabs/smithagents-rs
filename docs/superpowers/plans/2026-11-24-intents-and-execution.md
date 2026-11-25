# Intents and Execution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The daemon owns intents end to end: seed one from a chat, run the two polish passes (steering questions, then proposals), review items, mark it Complete, Send it to a new agent in its own worktree with a composed brief, hold it in a dependency-aware queue, and let the user mark it Resolved, all through the HTTP/WebSocket API and the client crate.

**Architecture:** New domain types in `smithagents-core`; two new SQLite tables plus a settings table in migration 0002; a second `Harness` capability, `run_structured`, that calls the Claude CLI once with a JSON schema and returns validated JSON (used by polish only; chat turns are unchanged); an `IntentService` beside `AgentService` that serializes work per intent; a `Scheduler` that starts queued intents when their dependencies are Resolved and a slot is free; REST routes and two new event kinds. No UI here: Plan 3b builds the intent UI on this API.

**Tech Stack:** Rust (stable, edition 2021), the Plan 1 stack (tokio, axum, sqlx/SQLite, serde, thiserror, tracing), Claude Code CLI 2.1.x in `--print` mode with `--output-format json --json-schema`.

**Spec:** `docs/superpowers/specs/2026-11-23-smithagents-architecture-design.md` (sections 4.4 Intents, Review workflow, Steering, Queue, Agents; section 6 "Version 1", items Chat to intent, Polish, Review semantics, Execute, Dependencies, Intent states). Roadmap: `docs/superpowers/plans/2026-11-24-v1-roadmap.md`. Builds on: `docs/superpowers/plans/2026-11-24-daemon-foundation.md` (code in `crates/core`, `crates/server`, `crates/client`).

## Verified facts (2026-11-24)

- `claude -p --output-format json --json-schema '<schema>' --model haiku` with the prompt on stdin returns one JSON object whose `structured_output` field holds the schema-conforming value (also echoed as a string in `result`), plus `is_error`, `session_id`, `total_cost_usd`, `subtype`. One real call cost about 0.007 USD (Claude Code 2.1.331). VERIFIED by running it.
- Flags available in this CLI: `--tools <list>` (built-in tool allowlist, `""` for none), `--allowedTools`, `--disallowedTools`, `--permission-mode`, `--max-budget-usd`, `--no-session-persistence`, `--effort`, `--add-dir`. VERIFIED from `claude --help`.
- Plan 1 provides: `AgentService::{create_agent, send_message}`, `git::create_worktree` returning `GitError::BranchExists`, `EventBus` with `subscribe(since)`, `Store` on SQLite with migration `0001_init.sql` (never edit it), `FakeHarness` (feature `test-util`), `tests/bin/fake-claude`, the real-CLI test `real_claude_smoke`.

## Decisions (my calls; the spec leaves them to the builder)

- **State model.** `IntentState { Draft, Complete, Queued, Sent, Resolved }` plus a `Phase` that only matters in Draft: `Polishing{pass}`, `Questions`, `Review`, `Failed{message}`. The UI filter "Draft and Polishing" is `state == Draft`. Draft and Complete flip freely until Send (changing a decision can reopen a Complete intent).
- **Kinds.** `Enhancement`, `Bug`, `Task` (the spec shows only "Enhancement" as an example).
- **Versions** are append-only snapshots written at: seed creation (v1), pass 1 result, pass 2 result, Send. Edits and review decisions mutate the current content only.
- **Questions:** at most 5, each with 2 to 5 options and always skippable; a question with fewer than 2 options is dropped; zero questions after pass 1 goes straight to pass 2. **Items** (preconditions, acceptance criteria, stories, depends-on) start `Pending`; Confirm, Edit then accept, and Skip map to `Accepted`, `Accepted(edited)`, `Dropped`.
- **Complete** means: pass 2 finished, no pending items in any of the four sections, and every question answered or skipped.
- **Polish runs read-only** by passing `--tools "Read,Grep,Glob"` with `cwd` = the repository root and permissions bypassed (a read-only tool set is the guard); it records no session on disk (`--no-session-persistence`).
- **Dependencies** only within one repository, never self or cyclic; "Resolved" means landed on the default branch, so a dependent's worktree branches from the default branch (spec).
- **Execute:** the fixed instructions go in the agent's system prompt (persisted on the agent row so every resumed turn gets them); the composed brief is the agent's first user message (visible in its chat).
- **Queue limit** lives in a small `settings` table (`agent_defaults.queue_limit`, default none); the Settings UI for it is Plan 3b. The limit counts every agent with a turn in flight (Working or Quiet), however it was started: intent agents (including one whose intent is already Resolved) and agents the user messages directly. Only intent starts wait for a slot; manual agents are never held back, but they occupy slots while they reply (spec: "never held back but still count").
- **Not in v1 (recorded):** deleting or archiving intents, user-added items, the Resume action and auto-nudge, auto-resolve from merged PRs, stacked branches.

## Global Constraints

- Daemon API: loopback only, bearer token on every request, errors as `{error, message}` JSON; new event kinds are additive and tagged `{"type","data"}` in snake_case so older clients skip them.
- Agents run unattended in their own worktrees with permissions bypassed; the user's main checkout is never written to. Polish may read the repository but never writes.
- An intent binds to exactly one repository, changeable until Send.
- Each polish pass is recorded as a new intent version. Skip on a question means proceed with no answer and let polish assume; Confirm accepts as is, Edit changes then accepts, Skip drops an item; counters read "n of m accepted"; all four sections without pending items makes the intent Complete.
- Send on a Complete intent creates a new agent (next name, next color), a worktree on a branch named from the slug, and starts the harness with the brief; an intent with unresolved Depends-on waits as Queued and starts when all are Resolved.
- Never edit `migrations/0001_init.sql`; add `0002_intents.sql`. Naming is `smithagents`, never `singularity`. `unsafe_code = "forbid"`.

## Review Focus

1. Polish output that is unusable: invalid JSON, schema-violating, more than 5 questions, questions with one option, dependency candidates that do not exist, are self, cross-repo or cyclic; CLI crash, timeout, empty stdout; daemon restart mid-polish. Expected: normalized or dropped items, else `Failed` with a message and a working Retry; never stuck Polishing (Tasks 4, 6).
2. Answering the same question twice, answering after pass 2 started, two clients answering the last question at once, retry while polishing. Expected: exactly one pass 2 run; later calls get a clear 409 (Task 6).
3. Dependencies: a cycle attempt, a dependency Resolved while the daemon was down, three queued intents with limit 1, two intents racing for the last slot, an intent whose agent died. Expected: no cycle, no double start, queued intents start in order after restart (Tasks 7, 9).
4. Send twice, Send on a non-Complete intent, branch name already exists, repository path moved so worktree creation fails. Expected: typed errors, the intent stays Complete, no orphan agent or worktree (Task 9).
5. Seeds: a very large transcript, tool-only transcript, unicode, an invalid selection range, transcript text that tries to instruct the polish model. Expected: size cap with an omission marker, clear 400 for empty or bad seeds, output constrained by the schema and read-only tools (Tasks 5, 4).
6. Slugs from empty, emoji-only, very long and colliding titles; branch names that git rejects. Expected: a valid unique slug and branch every time (Tasks 1, 9).

---

### Task 1: Intent domain types in `smithagents-core`

**Files:**
- Create: `crates/core/src/intent.rs`
- Modify: `crates/core/src/{lib.rs,ids.rs,agent.rs,event.rs}`
- Test: unit tests inside `crates/core/src/intent.rs` and `event.rs` (follow Plan 1 style)

**Interfaces:**
- Produces (`smithagents_core::intent`), all `Debug, Clone, PartialEq, Serialize, Deserialize`; enums use `#[serde(rename_all = "snake_case")]`:
  - ids (in `ids.rs` via the existing `id_type!`): `IntentId`, `QuestionId`, `ItemId`
  - `enum IntentKind { Enhancement, Bug, Task }`, `enum IntentState { Draft, Complete, Queued, Sent, Resolved }`
  - `enum Phase { Polishing { pass: u8 }, Questions, Review, Failed { pass: u8, message: String } }` (tagged `{"kind": ...}`; `pass` records which pass to re-run)
  - `enum SeedSource { Chat { agent_id: AgentId, session_id: Option<String>, from_seq: Option<u64>, to_seq: Option<u64> }, Text }`, `struct Seed { pub source: SeedSource, pub text: String }`
  - `struct QuestionOption { pub id: String, pub label: String }`, `enum Answer { Unanswered, Option { option_id: String }, Skipped }`, `struct Question { pub id: QuestionId, pub text: String, pub options: Vec<QuestionOption>, pub answer: Answer }`
  - `enum ItemStatus { Pending, Accepted, Dropped }`, `struct Item { pub id: ItemId, pub text: String, pub status: ItemStatus, pub edited: bool, pub intent_ref: Option<IntentId> }` (`intent_ref` is set only on depends-on items)
  - `struct Content { pub title: String, pub kind: IntentKind, pub current_behavior: String, pub questions: Vec<Question>, pub preconditions: Vec<Item>, pub criteria: Vec<Item>, pub stories: Vec<Item>, pub depends_on: Vec<Item> }`
  - `struct Counters { pub questions: (u32, u32), pub preconditions: (u32, u32), pub criteria: (u32, u32), pub stories: (u32, u32), pub depends_on: (u32, u32) }` with `Content::counters()` giving `(accepted-or-answered, total)`; `Content::pending_count()`; `Content::questions_open()` (number of `Unanswered`)
  - `struct Intent { pub id: IntentId, pub repo_id: RepoId, pub slug: String, pub state: IntentState, pub phase: Phase, pub seed: Seed, pub content: Content, pub agent_id: Option<AgentId>, pub created_at, pub updated_at, pub queued_at: Option<DateTime<Utc>>, pub sent_at: Option<..>, pub resolved_at: Option<..>, pub last_start_error: Option<String> }` (`last_start_error` is set by the queue when a start attempt fails and cleared on success)
  - `enum VersionAuthor { Seed, Polish1, Polish2, Send }`, `struct IntentVersion { pub intent_id: IntentId, pub version: u32, pub author: VersionAuthor, pub content: Content, pub at: DateTime<Utc> }`
  - `fn slugify(title: &str) -> String`: lowercase ASCII kebab-case, non-alphanumerics collapse to one `-`, trimmed, at most 48 chars cut on a `-` boundary where possible, empty result becomes `intent`.
  - `Intent::is_complete_content(&self) -> bool` (phase is `Review`, `pending_count() == 0`, `questions_open() == 0`).
- Also: `Agent` gains `#[serde(default)] pub intent_id: Option<IntentId>` (compatible with clients that do not know it); `EventKind` gains `IntentCreated(Intent)` and `IntentUpdated(Intent)`; `EventKind::TYPES` grows to 7 and `type_name` covers both, keeping Plan 1's test that the two agree.

- [ ] **Step 1: Write failing tests:** JSON shape of `Phase::Failed` and `Answer::Option` (assert the exact strings), `Content::counters()` over a mixed content (2 of 3 criteria accepted, 1 dropped, 0 pending), `is_complete_content` false while any item is Pending or any question Unanswered and true once all are decided in `Review`, `slugify` cases (`"Add dark-mode toggle!"` → `add-dark-mode-toggle`; `"  "` → `intent`; emoji-only → `intent`; a 200-character title stays within 48 and does not end in `-`; unicode letters are dropped, digits kept; `"A  B--C"` → `a-b-c`), event round trip for the two new kinds including `type_name`, and an `Agent` JSON without `intent_id` still deserializes.
- [ ] **Step 2: Run `cargo test -p smithagents-core`.** Expected: FAIL.
- [ ] **Step 3: Implement** to the interfaces.
- [ ] **Step 4: Run `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`.** Expected: PASS, clean (existing server and client code compiles with the new optional field; fix constructors that build `Agent` literals).
- [ ] **Step 5: Commit** `feat(core): intent domain types, slugify, intent events`.

---

### Task 2: Store: migration 0002 and intent queries

**Files:**
- Create: `crates/server/migrations/0002_intents.sql`
- Modify: `crates/server/src/store.rs`
- Test: `crates/server/src/store.rs` unit tests (in-memory store, as in Plan 1)

**Interfaces:**
- Migration: `intents(id TEXT PK, repo_id TEXT NOT NULL REFERENCES repos(id), slug TEXT NOT NULL, state TEXT NOT NULL, phase_json TEXT NOT NULL, seed_json TEXT NOT NULL, content_json TEXT NOT NULL, agent_id TEXT REFERENCES agents(id), created_at, updated_at, queued_at, sent_at, resolved_at, UNIQUE(repo_id, slug))`; `intent_versions(intent_id TEXT NOT NULL REFERENCES intents(id), version INTEGER NOT NULL, author TEXT NOT NULL, content_json TEXT NOT NULL, at TEXT NOT NULL, PRIMARY KEY (intent_id, version))`; `settings(key TEXT PRIMARY KEY, value_json TEXT NOT NULL)`; `ALTER TABLE agents ADD COLUMN intent_id TEXT` and `ALTER TABLE agents ADD COLUMN system_prompt_append TEXT`.
- Produces on `Store`: `insert_intent(&Intent, first_version: &IntentVersion)` (one transaction), `get_intent(IntentId) -> Option<Intent>`, `list_intents(filter: IntentFilter { state: Option<IntentState>, repo_id: Option<RepoId> })` (newest `updated_at` first), `save_intent(&Intent) -> Result<(), StoreError>` (full row update, bumps nothing itself), `append_version(&IntentVersion)` (version = max + 1 computed in the same transaction; returns the number), `list_versions(IntentId)`, `slug_exists(RepoId, &str) -> bool`, `intents_in_phase_polishing() -> Vec<Intent>`, `queued_intents() -> Vec<Intent>` (ordered by `queued_at`), `get_setting<T>(key) -> Option<T>`, `set_setting<T>(key, &T)`; `insert_agent` and `get_agent`/`list_agents` carry `intent_id` and `system_prompt_append` (new `Agent` field `intent_id` from Task 1; the append text is exposed through `Store::agent_system_prompt_append(AgentId) -> Option<String>` and set by `set_agent_system_prompt_append`).

- [ ] **Step 1: Write failing tests:** round trip of an intent with questions and items; `UNIQUE(repo_id, slug)` violation maps to a typed `StoreError::Conflict`; `append_version` numbers 1, 2, 3 under two concurrent calls without a gap or duplicate; `list_intents` filters by state and repo and orders by update time; `queued_intents` ordered by `queued_at`; settings round trip and missing key is `None`; an existing 0001 database file migrates to 0002 without touching old rows (open a database created by 0001 only: copy a fixture or run migrations stepwise in the test); agents written before the migration read back with `intent_id: None`.
- [ ] **Step 2: Run `cargo test -p smithagents-server store`.** Expected: FAIL.
- [ ] **Step 3: Implement.** Keep all new SQL in `store.rs`.
- [ ] **Step 4: Run `cargo test -p smithagents-server`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(server): intents, versions and settings in the store`.

---

### Task 3: Structured harness runs (`run_structured`)

**Files:**
- Modify: `crates/server/src/harness/{mod.rs,claude.rs,fake.rs}`, `crates/server/tests/bin/fake-claude`
- Create: `crates/server/src/harness/structured.rs` (parsing of the CLI's JSON result)
- Test: `crates/server/tests/claude_harness.rs`, `crates/server/src/harness/structured.rs` unit tests

**Interfaces:**
- Produces:
  - `struct StructuredRequest { pub prompt: String, pub cwd: PathBuf, pub schema: serde_json::Value, pub model: Option<String>, pub system_prompt_append: Option<String>, pub allowed_tools: Vec<String>, pub timeout: Duration }`
  - `struct StructuredResult { pub value: serde_json::Value, pub cost_usd: Option<f64> }`
  - `HarnessError` gains `Timeout`, `Failed(String)` (the CLI reported `is_error` or a non-zero exit with no output), `InvalidOutput(String)` (no parseable JSON or no `structured_output`)
  - `Harness` gains `async fn run_structured(&self, req: StructuredRequest) -> Result<StructuredResult, HarnessError>`.
  - `ClaudeCodeHarness::run_structured`: spawns `claude -p --output-format json --json-schema <schema JSON> --dangerously-skip-permissions --no-session-persistence --tools <allowed_tools joined by commas>` plus `--model`/`--append-system-prompt` when set; prompt on stdin (as in Plan 1: never in argv); `cwd` as given; kills the process group on timeout; takes the last JSON object on stdout; `is_error: true` becomes `Failed(result text)`; success requires `structured_output` present (a string `result` alone is `InvalidOutput`). `structured::parse_cli_result(stdout: &str) -> Result<StructuredResult, HarnessError>` is the pure part.
  - `FakeHarness::script_structured(Vec<Result<serde_json::Value, HarnessError>>)` queue (one per call, in order) and `structured_calls() -> Vec<StructuredRequest>` for assertions.
  - `fake-claude` learns one more env var: `FAKE_CLAUDE_STRUCTURED_FILE` (path to a JSON file printed on stdout as the CLI's `--output-format json` object, wrapping the file's content in `structured_output`), used when the argv contains `--json-schema`.

- [ ] **Step 1: Write failing tests:** `parse_cli_result` on the verified shape (`{"is_error":false,"structured_output":{...},"total_cost_usd":0.007,...}` → value and cost), on `is_error:true` (`Failed`), on missing `structured_output` (`InvalidOutput`), on noise lines before the JSON object (still parses), on empty stdout (`InvalidOutput`); a harness test with `fake-claude`: argv contains `--json-schema`, `--tools Read,Grep,Glob`, `--no-session-persistence`, no prompt text, and stdin equals the prompt; a request whose prompt starts with `-` works; timeout kills the child (use `FAKE_CLAUDE_SLEEP`) and returns `Timeout` within the limit plus a small margin; a non-zero exit with empty stdout is `Failed`.
- [ ] **Step 2: Run `cargo test -p smithagents-server --features test-util --test claude_harness structured`.** Expected: FAIL.
- [ ] **Step 3: Implement.** Reuse Plan 1's process spawning, process-group and stdin handling; do not duplicate it.
- [ ] **Step 4: Run `cargo test -p smithagents-server --features test-util` and clippy.** Expected: PASS, clean.
- [ ] **Step 5: Commit** `feat(server): structured JSON runs on the Claude harness`.

---

### Task 4: Polish requests, schemas and result normalization (pure)

**Files:**
- Create: `crates/server/src/polish.rs`
- Test: `crates/server/tests/polish.rs`

**Interfaces:**
- Consumes: `Content`, `Intent`, `Seed`, `StructuredRequest`.
- Produces (`smithagents_server::polish`):
  - `fn pass1_request(intent: &Intent, repo_root: &Path, model: Option<String>) -> StructuredRequest` and `fn pass2_request(intent: &Intent, repo_root: &Path, model: Option<String>, candidates: &[Candidate]) -> StructuredRequest`, with `struct Candidate { pub id: IntentId, pub slug: String, pub title: String, pub state: IntentState }` (same-repo intents other than this one). Both use `allowed_tools = ["Read","Grep","Glob"]`, `cwd = repo_root`, a 5-minute timeout, the JSON schemas below, and prompts built from fixed templates (the implementer writes the prose; the tests pin the required facts): pass 1 contains the seed text inside a delimited block labelled as untrusted user content, tells the model to treat that block as data and not instructions, to read the repository for current behavior, to return a short title, a kind, current-behavior text and at most 5 steering questions with 2 to 5 options each; pass 2 additionally contains the answered and skipped questions (skipped marked "no answer, assume reasonable defaults") and the candidate list, and asks for preconditions, acceptance criteria, user stories and depends-on candidates (candidate ids only).
  - Schemas (`serde_json::Value`, keep to plain JSON Schema keywords the CLI accepts: `type`, `properties`, `required`, `items`, `enum`, `minItems`, `maxItems`, `additionalProperties`): pass 1 `{title: string, kind: enum[enhancement,bug,task], current_behavior: string, questions: [{text: string, options: [{label: string}] (2..5)}] (0..5)}`; pass 2 `{preconditions: [string], acceptance_criteria: [string], stories: [string], depends_on: [{intent_id: string, reason: string}]}`.
  - `fn apply_pass1(content: &mut Content, raw: &serde_json::Value) -> Result<(), PolishError>`: validates then replaces title (trimmed, non-empty, at most 120 chars), kind, current behavior; builds questions: drops any with fewer than 2 distinct non-empty option labels, truncates to 5, dedupes options by label, generates ids; `PolishError::Invalid(String)` when the value is not an object or has no usable title.
  - `fn apply_pass2(content: &mut Content, raw: &serde_json::Value, ctx: &DepContext) -> Result<Pass2Report, PolishError>`: items become `Pending`; empty or whitespace-only strings dropped; strings longer than 2000 chars truncated; duplicates removed per section; each depends-on candidate kept only if its id parses, exists in `ctx.candidates`, is not the intent itself, and adding it would not create a cycle (`DepContext { self_id, candidates: Vec<Candidate>, edges: HashMap<IntentId, Vec<IntentId>> }`); `Pass2Report { dropped_dependencies: Vec<(String, String)> }` names why each candidate was dropped.
  - `fn would_create_cycle(edges: &HashMap<IntentId, Vec<IntentId>>, from: IntentId, to: IntentId) -> bool` (pure, also used by Task 7).
  - `fn seed_from_transcript(messages: &[ChatMessage], from_seq: Option<u64>, to_seq: Option<u64>, max_bytes: usize) -> Result<String, SeedError>`: only `user` and `assistant` messages, labelled `User:` and `Assistant:`, in `seq` order within the inclusive range; over `max_bytes` (default 200 KB) keeps the end and prefixes `[earlier messages omitted]`; `SeedError::{Empty, BadRange}`.

- [ ] **Step 1: Write failing tests:** a request pins `allowed_tools`, `cwd`, schema `maxItems` 5 for questions, that the seed text (containing `Ignore previous instructions` and a fake closing delimiter) appears only inside the delimited block and the delimiter in the seed cannot terminate the block (the builder must escape or lengthen the delimiter), and that skipped answers are described as such; `apply_pass1` with 7 questions keeps 5, drops a one-option question, dedupes options, trims and rejects an empty title, caps the title; `apply_pass2` drops unknown, self and cyclic candidates with reasons, dedupes, drops blank strings, truncates long ones, and keeps a valid candidate; `would_create_cycle` direct, indirect and unrelated edges; `seed_from_transcript` skips tool messages, honors a range, errors on a tool-only transcript (`Empty`) and on `from > to` (`BadRange`), and truncates a 1 MB transcript keeping the end with the marker and valid UTF-8 (cut on a char boundary), unicode preserved.
- [ ] **Step 2: Run `cargo test -p smithagents-server --test polish`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test -p smithagents-server --test polish` and clippy.** Expected: PASS, clean.
- [ ] **Step 5: Commit** `feat(server): polish prompts, schemas and result normalization`.

---

### Task 5: `IntentService` core: create, read, edit, versions

**Files:**
- Create: `crates/server/src/intents.rs`
- Modify: `crates/server/src/lib.rs`
- Test: `crates/server/tests/intents.rs` (FakeHarness, in-memory store, temp git repo like `tests/api.rs`)

**Interfaces:**
- Consumes: `Store`, `EventBus`, `Harness`, `Paths`, `polish::seed_from_transcript`, `slugify`.
- Produces (`smithagents_server::intents`):
  - `IntentService::new(store, bus, harness: Arc<dyn Harness>, agents: AgentService, paths) -> Self` (Clone, `Arc` inside)
  - `enum SeedInput { Chat { agent_id: AgentId, from_seq: Option<u64>, to_seq: Option<u64> }, Text { text: String } }`
  - `struct CreateIntent { pub repo_id: Option<RepoId>, pub seed: SeedInput }`
  - `async fn create(&self, req: CreateIntent) -> Result<Intent, IntentError>`: for `Chat` the seed text comes from the agent's stored messages (source tagged with agent id and its harness session id), the repository defaults to the agent's repository (an explicit different `repo_id` wins); `Text` requires an explicit repository; seed text empty after trimming is `Invalid`; creates the intent `Draft`, `Phase::Polishing{pass:1}`, title `"Untitled intent"`, provisional slug `intent-<first 8 hex of id>`, writes version 1 (`Seed`), publishes `IntentCreated`, **and starts pass 1 in the background (Task 6 supplies the runner; here it is invoked through a `PolishRunner` handle that defaults to a no-op in this task's tests)**.
  - `async fn get(&self, IntentId) -> Result<Intent, IntentError>`, `async fn list(&self, IntentFilter) -> Result<Vec<Intent>, IntentError>`, `async fn versions(&self, IntentId) -> Result<Vec<IntentVersion>, IntentError>`
  - `struct Patch { pub title: Option<String>, pub kind: Option<IntentKind>, pub current_behavior: Option<String>, pub repo_id: Option<RepoId> }` and `async fn update(&self, id, Patch) -> Result<Intent, IntentError>`: allowed only in `Draft` (not while `Phase::Polishing`) and `Complete`; title trimmed, non-empty, at most 120; changing the repository drops all depends-on items (different repository) and returns them to nothing; publishes `IntentUpdated`.
  - `enum IntentError { NotFound, RepoNotFound, AgentNotFound, InvalidState { expected: &'static str, actual: IntentState }, Polishing, Invalid(String), DependencyCycle, DependencyCrossRepo, Service(ServiceError), Store(StoreError) }` with `kind()`/message used by the API.
  - A per-intent serialization primitive (`reserve(id) -> Reservation`, same drop-guard pattern as `AgentService`) used by every mutating method in Tasks 6, 7 and 9; a second concurrent mutation of one intent returns `IntentError::Polishing` only when a polish job holds it, otherwise waits its turn on a `tokio::sync::Mutex` (document the choice).

- [ ] **Step 1: Write failing tests:** create from a chat with two messages tags the source with agent id and session id and defaults to the agent's repository; create from an unknown agent is `AgentNotFound`; a tool-only chat is `Invalid`; text seed without repository is `Invalid`; explicit repository override works; versions list has exactly the seed version; `update` while polishing is `Polishing`; update title and kind works and publishes `IntentUpdated` (check the event bus); a blank title is `Invalid`; changing repository clears depends-on; unknown intent is `NotFound`; list filters by state.
- [ ] **Step 2: Run `cargo test -p smithagents-server --features test-util --test intents`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the test target and clippy.** Expected: PASS, clean.
- [ ] **Step 5: Commit** `feat(server): intent service core`.

---

### Task 6: Polish orchestration (pass 1, questions, pass 2, retry, recovery)

**Files:**
- Modify: `crates/server/src/intents.rs`
- Test: `crates/server/tests/intents.rs` (extend)

**Interfaces:**
- Produces on `IntentService`:
  - `PolishRunner` becomes real: after `create` (and on `retry_polish`) a background task (spawned, its reservation released by a drop guard, panics caught) runs pass 1: `harness.run_structured(polish::pass1_request(..))`, `apply_pass1`, writes version `Polish1`, sets `Phase::Questions` when questions exist else immediately starts pass 2; publishes `IntentUpdated` at each step; slug is recomputed from the new title with `slugify` and made unique per repository (`-2`, `-3` suffixes) **only while the intent has never been Sent**.
  - `async fn answer(&self, id, question_id: QuestionId, answer: AnswerInput) -> Result<Intent, IntentError>` with `enum AnswerInput { Option(String), Skip }`: valid only in `Phase::Questions`; the option id must exist; a question already decided is `InvalidState`-style conflict (`IntentError::Invalid("question already answered")`, HTTP 409 in Task 10); when no question is `Unanswered` it sets `Phase::Polishing{pass:2}` **in the same store write as the answer** and starts pass 2 exactly once.
  - Pass 2: builds `DepContext` from same-repo intents (candidates exclude Resolved? no: include all states; the edges come from confirmed depends-on items of other intents), `harness.run_structured(pass2_request(..))`, `apply_pass2`, version `Polish2`, `Phase::Review`; the intent stays `Draft` until Task 7 completes it; if there is nothing to review (all four sections empty) it becomes `Complete` at once.
  - `async fn retry_polish(&self, id) -> Result<Intent, IntentError>`: only from `Phase::Failed`; re-runs the pass recorded in `Failed.pass`; the message stays user-readable.
  - Failures (`HarnessError::{Timeout, Failed, InvalidOutput, Spawn}`, `PolishError`, panic) set `Phase::Failed{pass, message}`; messages never include prompt text or tokens.
  - `async fn recover(&self) -> Result<u64, IntentError>`: at daemon start every intent in `Phase::Polishing` becomes `Failed{message: "polish was interrupted by a daemon restart; retry"}`; called from the daemon startup path (Task 10 wires it).

- [ ] **Step 1: Write failing tests with `FakeHarness::script_structured`:** happy path to `Phase::Questions` with three questions, answer two and skip one, pass 2 runs once and the intent reaches `Phase::Review` with pending items; slug follows the title and a colliding title gets `-2`; zero questions skips straight to pass 2; harness `Timeout`, `InvalidOutput` and a panic-inducing script each give `Failed` and `retry_polish` then succeeds; answering the same question twice is a conflict; two concurrent answers to the last two questions produce exactly one pass 2 call (assert `structured_calls().len()`); `answer` after pass 2 started is a conflict; `retry_polish` while polishing is `Polishing`; `recover()` converts a stored polishing intent to `Failed`; the pass 2 prompt contains the skipped-question marker and only same-repository candidates; every step emits `IntentUpdated` with increasing `updated_at`.
- [ ] **Step 2: Run `cargo test -p smithagents-server --features test-util --test intents`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the test target 20 times** (`for i in $(seq 20); do cargo test -p smithagents-server --features test-util --test intents -q || break; done`) to expose races; then clippy. Expected: PASS every time, clean.
- [ ] **Step 5: Commit** `feat(server): polish orchestration with questions, retry and restart recovery`.

---

### Task 7: Review semantics and completion

**Files:**
- Modify: `crates/server/src/intents.rs`
- Test: `crates/server/tests/intents.rs` (extend)

**Interfaces:**
- Produces on `IntentService`: `async fn review(&self, id, item_id: ItemId, action: ReviewAction) -> Result<Intent, IntentError>` with `enum ReviewAction { Accept, Edit(String), Drop }`.
  - Valid only in `Draft` with `Phase::Review`, or `Complete` (reopening: a changed decision recomputes state). `Accept` sets `Accepted`; `Edit(text)` trims, rejects empty, caps 2000 chars, sets text, `edited = true`, `Accepted`; `Drop` sets `Dropped`. Decisions can be changed until Send.
  - After every change: `state = Complete` iff `Intent::is_complete_content`, else `Draft`; publishes `IntentUpdated`; no new version.
  - Accepting a depends-on item re-checks the dependency at that moment: the referenced intent must still exist and be in the same repository, and `would_create_cycle` must be false against current confirmed edges; otherwise `IntentError::DependencyCycle` or `DependencyCrossRepo` and the item stays Pending. Editing a depends-on item's text changes only its label, never `intent_ref`.
  - `Edit` on a question item is not applicable; questions use `answer`.

- [ ] **Step 1: Write failing tests:** accept, edit and drop each give the expected status, `edited` and counters ("1 of 3 accepted"); the last pending item moves `Draft` to `Complete`; dropping every item also completes; changing a decided item from Accept to Drop on a Complete intent keeps it Complete; changing the repository of a Complete intent removes its depends-on items and the state is recomputed (still Complete when nothing is pending); an empty edit is `Invalid`; reviewing during `Polishing` or `Questions` phase is `InvalidState`; a cycle on accepting a depends-on item (A depends on B confirmed, now B tries to accept A) is `DependencyCycle`; a stale reference to an intent in another repository is `DependencyCrossRepo`; a Sent intent rejects review with `InvalidState`.
- [ ] **Step 2: Run `cargo test -p smithagents-server --features test-util --test intents review`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the test target and clippy.** Expected: PASS, clean.
- [ ] **Step 5: Commit** `feat(server): intent review semantics and completion`.

---

### Task 8: Brief composition and intent-aware agent creation

**Files:**
- Create: `crates/server/src/brief.rs`
- Modify: `crates/server/src/service.rs` (agent creation options, per-turn system prompt), `crates/server/src/git.rs` (branch-name helper)
- Test: `crates/server/tests/brief.rs`, `crates/server/src/service.rs` tests

**Interfaces:**
- Produces:
  - `brief::compose(intent: &Intent, deps: &[DepSummary]) -> Brief { pub system_append: String, pub first_message: String }` with `struct DepSummary { pub slug: String, pub title: String, pub current_behavior: String }`. `first_message` is plain Markdown with sections in this order, each omitted when empty: title and kind, **Current behavior**, **Preconditions** (accepted items), **Acceptance criteria** (accepted items, numbered), **User stories**, **Steering answers** (question text with the chosen option label; skipped questions listed as "no answer; use your judgment"), **Depends on** (each dependency's title and first 300 chars of its current behavior). Dropped and pending items never appear. `system_append` carries the fixed instructions: work only in this worktree, satisfy every acceptance criterion, commit your work, and open the pull request when done **if the repository has a remote** (otherwise leave the commits on the branch and say so), and the default resume wording from the spec.
  - `git::branch_name_for_intent(slug: &str) -> String` = `intent/<slug>`.
  - `AgentService::create_agent_with(&self, repo_id: RepoId, opts: CreateAgentOptions) -> Result<Agent, ServiceError>` where `struct CreateAgentOptions { pub branch: Option<String>, pub intent_id: Option<IntentId>, pub system_prompt_append: Option<String> }`; `create_agent(repo_id)` becomes `create_agent_with(repo_id, Default::default())`. A given branch that exists is `ServiceError::Git(GitError::BranchExists)` (no silent rename inside the service). The system prompt append is saved on the agent row and passed as `TurnRequest.system_prompt_append` on **every** turn (first and resumed).
  - `AgentService::start_with_message(&self, id: AgentId, text: String) -> Result<ChatMessage, ServiceError>` is just the existing `send_message`; no new method.

- [ ] **Step 1: Write failing tests:** `compose` golden test on a fixture intent (title, two accepted criteria, one dropped, one pending, one skipped question, one dependency) comparing the exact `first_message` text and checking the dropped and pending texts are absent; `system_append` mentions the worktree, the criteria, committing and the remote-conditional PR instruction; a unicode and Markdown-heavy criterion is preserved; `create_agent_with` stores the branch name, the intent id and the append text (read back through the store); a resumed turn (second `send_message`) passes the same `system_prompt_append` (assert via `FakeHarness` recorded requests); an existing branch gives `BranchExists` and creates no agent row or worktree directory.
- [ ] **Step 2: Run `cargo test -p smithagents-server --features test-util brief service`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the full server tests and clippy.** Expected: PASS, clean; Plan 1 tests unchanged.
- [ ] **Step 5: Commit** `feat(server): brief composition and intent-aware agent creation`.

---

### Task 9: Send, queue scheduler, Resolve and queue limit

**Files:**
- Create: `crates/server/src/queue.rs`
- Modify: `crates/server/src/intents.rs`, `crates/server/src/lifecycle.rs` (daemon wiring points listed in Task 10)
- Test: `crates/server/tests/queue.rs`

**Interfaces:**
- Produces on `IntentService`: `async fn send(&self, id) -> Result<Intent, IntentError>`: requires `Complete`; stamps a `Send` version; if every depends-on item is Accepted and its target Resolved, starts now, else sets `Queued` (`queued_at = now`) and returns; publishes `IntentUpdated`. `async fn resolve(&self, id) -> Result<Intent, IntentError>`: requires `Sent` (any agent state); sets `Resolved`, `resolved_at`, publishes, then `scheduler.kick()`.
- Produces (`smithagents_server::queue`): `struct Scheduler` with `Scheduler::start(intents: IntentService, agents: AgentService, store: Store, bus: EventBus) -> SchedulerHandle` (a background task) and `SchedulerHandle::kick()`. The loop runs on startup, on `kick()`, and on every `AgentUpdated` bus event; each run holds a mutex so runs never overlap and does: list `Queued` intents oldest first; for each whose dependencies are all Resolved and while `running_count < limit`, **start it** (`IntentService::start(id)`, below). `running_count` = intents in `Sent` whose agent state is `Working` or `Quiet`; `limit` = `settings agent_defaults.queue_limit` (none means unlimited). A start failure leaves the intent `Queued`, records the error in `Intent.last_start_error` (cleared on success) and does not stop the pass.
- `IntentService::start(&self, id) -> Result<Intent, IntentError>` (internal, also used by `send` for the immediate case): in this order: branch `intent/<slug>` (on `BranchExists` retry once with `-<first 6 hex of intent id>` appended), `agents.create_agent_with(repo, CreateAgentOptions{ branch, intent_id, system_prompt_append })`, `agents.send_message(agent.id, brief.first_message)`, then save the intent as `Sent` with `agent_id`, `sent_at`, publish. If saving the intent fails after the agent exists, close the agent and remove nothing else (the worktree is kept, as for any closed agent) and return the store error. If agent creation fails nothing else changed and the intent keeps its previous state with `last_start_error` set.
- `queue_limit` get/set: `IntentService::agent_defaults()`/`set_agent_defaults(AgentDefaults { queue_limit: Option<u32> })` (0 means none; reject values above 64) stored under key `agent_defaults`; setting it kicks the scheduler.

- [ ] **Step 1: Write failing tests (FakeHarness, temp git repo, real worktrees):** Send on a Complete intent without dependencies creates an agent named Mercury on branch `intent/<slug>` whose first stored message is the brief and whose intent id is set, intent `Sent`; Send on Draft is `InvalidState`; second Send is `InvalidState` and creates no second agent; Send with an unresolved dependency gives `Queued` and no agent; `resolve` of the dependency starts the dependent without any other call (scheduler observes it); with limit 1 and three queued intents (all dependencies met) only one starts, the next starts once the first agent leaves `Working` or `Quiet` (the running count only counts those states), and they start in `queued_at` order; manually created agents count toward the limit while they reply but are never blocked by it; two concurrent `kick()` calls start each intent once; a branch collision retries once with the suffix and succeeds; worktree creation failure (repository moved) leaves the intent `Complete` or `Queued` with `last_start_error` set and no agent row; `resolve` on a non-Sent intent is `InvalidState`; restart recovery: construct a fresh service and scheduler over the same store with a Queued intent whose dependency is already Resolved and observe it start; `set_agent_defaults` rejects 100.
- [ ] **Step 2: Run `cargo test -p smithagents-server --features test-util --test queue`.** Expected: FAIL.
- [ ] **Step 3: Implement.** Keep scheduling decisions in pure functions where possible (`fn next_startable(queued: &[Intent], resolved: &HashSet<IntentId>, running: usize, limit: Option<u32>) -> Vec<IntentId>`) and unit test that function directly.
- [ ] **Step 4: Run the test target 20 times, then `cargo test -p smithagents-server --features test-util` and clippy.** Expected: PASS every run, clean.
- [ ] **Step 5: Commit** `feat(server): send, dependency-aware queue scheduler, resolve and queue limit`.

---

### Task 10: API routes, events on the wire, daemon wiring

**Files:**
- Modify: `crates/server/src/api.rs`, `crates/server/src/bin/smithagentsd.rs`, `crates/server/src/lifecycle.rs` (as needed to construct `IntentService` and `Scheduler`, call `recover()` once at startup before serving)
- Test: `crates/server/tests/api.rs` (extend)

**Interfaces:**
- Produces routes (all behind the bearer middleware, mutating handlers inside `tokio::spawn` like Plan 1):

| Method and path | Body | Result |
|---|---|---|
| `POST /v1/intents` | `{"repo_id"?, "seed": {"chat": {"agent_id","from_seq"?,"to_seq"?}} \| {"text": "..."}}` | 201 `Intent` |
| `GET /v1/intents?state=&repo_id=` | | `Intent[]` |
| `GET /v1/intents/{id}` | | `Intent` |
| `GET /v1/intents/{id}/versions` | | `IntentVersion[]` |
| `PATCH /v1/intents/{id}` | `{"title"?,"kind"?,"current_behavior"?,"repo_id"?}` | `Intent` |
| `POST /v1/intents/{id}/questions/{qid}/answer` | `{"option_id"}` or `{"skip": true}` | `Intent` |
| `POST /v1/intents/{id}/polish/retry` | | `Intent` |
| `POST /v1/intents/{id}/items/{iid}/accept` \| `drop` \| `edit` | `edit`: `{"text"}` | `Intent` |
| `POST /v1/intents/{id}/send` | | `Intent` |
| `POST /v1/intents/{id}/resolve` | | `Intent` |
| `GET /v1/settings/agent-defaults`, `PUT /v1/settings/agent-defaults` | `{"queue_limit": n \| null}` | `AgentDefaults` |

  - Error mapping (`{error, message}`): `NotFound` 404 `intent_not_found`; `RepoNotFound` 404 `repo_not_found`; `AgentNotFound` 404 `agent_not_found`; `InvalidState` 409 `invalid_state`; `Polishing` 409 `polishing`; `Invalid` 400 `invalid`, except "question already answered" which is 409 `already_answered`; `DependencyCycle` 409 `dependency_cycle`; `DependencyCrossRepo` 409 `dependency_cross_repo`; service/git errors as in Plan 1.
  - `IntentCreated` and `IntentUpdated` flow through `/v1/events` unchanged; `Health` is unchanged.
  - Startup order in `smithagentsd`: open store, `IntentService::recover()`, start `Scheduler`, serve.

- [ ] **Step 1: Write failing API tests** (FakeHarness through the test router, as in the existing `tests/api.rs`): each route's success shape; unknown ids 404; bad uuid in the path 400; all 409 codes above; missing token 401 on a new route; body over the limit rejected; events: creating an intent publishes `intent_created` and each step an `intent_updated` over the WebSocket; an old client that does not know the kinds can skip them (reuse the client's skip rule); the whole loop through HTTP only: create agent, send a chat message, create intent from the chat, answer, review, send, resolve, ending with the dependent intent starting.
- [ ] **Step 2: Run `cargo test -p smithagents-server --features test-util --test api intents`.** Expected: FAIL.
- [ ] **Step 3: Implement the routes and the daemon wiring.**
- [ ] **Step 4: Run `cargo test -p smithagents-server --features test-util` and clippy.** Expected: PASS, clean.
- [ ] **Step 5: Commit** `feat(server): intent and settings API, scheduler and recovery wired into the daemon`.

---

### Task 11: Client crate

**Files:**
- Modify: `crates/client/src/api.rs`, `crates/client/src/lib.rs`
- Test: `crates/client/tests/intents.rs`, `crates/client/tests/e2e.rs`

**Interfaces:**
- Produces on `Client` (async, typed, same error type as Plan 1): `create_intent(CreateIntentRequest) -> Intent`, `list_intents(IntentFilter) -> Vec<Intent>`, `get_intent(id)`, `intent_versions(id)`, `update_intent(id, Patch)`, `answer_question(id, QuestionId, AnswerInput)`, `retry_polish(id)`, `accept_item(id, ItemId)`, `drop_item(id, ItemId)`, `edit_item(id, ItemId, &str)`, `send_intent(id)`, `resolve_intent(id)`, `agent_defaults()`, `set_agent_defaults(AgentDefaults)`. Request and response types live in `smithagents-core` (`intent` module) so the shell and the daemon share them; add `CreateIntentRequest`, `Patch`, `AnswerInput`, `AgentDefaults` there if Task 5/9 defined them in the server crate (move, do not duplicate). The event stream already skips unknown kinds; it now decodes the two new ones.

- [ ] **Step 1: Write failing tests** against the real daemon binary with `fake-claude` (the helper from Plan 1's client tests; extend the daemon launch environment so `fake-claude` serves structured results through `FAKE_CLAUDE_STRUCTURED_FILE`, with one file for pass 1 and one for pass 2 chosen by a small control file the test updates between calls): the full loop through the client (chat message, `create_intent` from the chat, wait for `Phase::Questions` via the event stream, answer and skip, wait for `Review`, accept all, `send_intent`, observe `Sent` and the agent's brief message, `resolve_intent`); typed errors surface (`already_answered`, `invalid_state`); an unknown event kind in the stream is skipped without ending it.
- [ ] **Step 2: Run `cargo test -p smithagents-client --test intents`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test --workspace` and clippy.** Expected: PASS, clean, no leftover `smithagentsd` processes.
- [ ] **Step 5: Commit** `feat(client): intent and settings API`.

---

### Task 12: Real-CLI smoke test, acceptance and roadmap update

**Files:**
- Modify: `crates/client/tests/e2e.rs` (an `#[ignore]` test), `docs/superpowers/plans/2026-11-24-v1-roadmap.md`
- Create: `docs/superpowers/acceptance/intents-and-execution.md`

**Interfaces:**
- `real_claude_polish_smoke` (ignored, run with `cargo test -p smithagents-client --test e2e -- --ignored real_claude_polish_smoke --nocapture`): in a temp git repository containing a tiny file, create an agent, send one short message, create an intent from the chat, wait for pass 1 with the real CLI (model `haiku` to bound cost; read the model from an env var `SMITHAGENTS_POLISH_MODEL` that the daemon passes to polish requests, default none), assert a non-empty title and 0 to 5 questions each with at least 2 options, skip all questions, wait for pass 2, assert `Review` or `Complete` and every item `Pending`, then stop. It does **not** Send (that would run a full agent). Expected cost: two small calls (the verified single structured call cost about 0.007 USD). Add the env var plumbing in the daemon's polish request builder in this task, with a test that the model reaches `StructuredRequest.model`.
- Acceptance doc: the checklist below with a results column.

- [ ] **Step 1: Write the ignored test and the model plumbing test.** Run the plumbing test (not ignored). Expected: FAIL then PASS after implementing.
- [ ] **Step 2: Run the ignored real-CLI test once.** Expected: PASS; if the real model returns something the normalizer rejects (for example options as plain strings), fix the schema or normalizer and add that output as a regression fixture test in `tests/polish.rs`.
- [ ] **Step 3: Run the acceptance checklist and record results:** daemon restart during pass 1 shows Failed with a Retry that works; a chat with 3 messages produces an intent whose seed shows all three; skipping every question still reaches pass 2; two intents with one depending on the other: Send both, the dependent shows Queued, Resolve the first, the dependent starts on its own and its agent's first message contains the dependency summary; queue limit 1 holds a second ready intent until the first agent is Done; Send with the repository moved reports an error and leaves the intent Complete; killing the daemon while an intent is Queued and restarting it starts it when ready; `cargo test --workspace` and clippy clean.
- [ ] **Step 4: Update the roadmap Plan 2 row** to this plan's file name and add the API summary (route table) as the contract Plan 3b builds on; list the follow-ups (delete or archive, user-added items, Resume and auto-nudge, auto-resolve, stacked branches, polish progress streaming, cost tracking).
- [ ] **Step 5: Commit** `test: real-CLI polish smoke test, intents acceptance results and roadmap update`.

---

## Self-review notes

- **Spec coverage (v1 steps 3 to 7):** send a chat to create an intent and start polish (Tasks 5, 6); steering questions up to 5 with options and Skip, "n of m answered" (Tasks 1, 6); polish attempt after questions (Task 6); confirm, edit or skip each proposed precondition, criterion, story and depends-on with counters (Task 7); Send to a new agent with name, color, worktree, branch from slug, composed brief, fixed instructions (Tasks 8, 9); Queued while waiting on dependencies, optional limit, manual agents bypass it (Task 9); Sent with agent sub-state (derived: the intent carries `agent_id`, the agent carries state), Resolved by the user (Task 9); intent versions per pass (Tasks 2, 5, 6); source ticket references and the GitHub plugin are out of v1.
- **Plan 3b contract:** the route table in Task 10 and the types in `smithagents-core::intent`. The agent list carries `intent_id` so the UI can draw the matching colored border on intent cards.
- **Known limits to say out loud:** polish reads the repository's default checkout, which can differ from the branch an agent last worked on; the polish prompt includes repository content read by the model, so a malicious repository can still influence proposals (output is schema-constrained and Send needs the user's review); costs are not tracked.

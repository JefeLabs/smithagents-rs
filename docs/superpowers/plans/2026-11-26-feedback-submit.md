# Feedback Submit (feedback tool, steps 3 to 5) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Submit in the Feedback pane turns the draft into files committed under `docs/issues/` on a new branch of a chosen repository, and then into an intent (optionally an idea intent whose agent prepares an explainer, diagrams and a demo video) or a GitHub issue; an idea intent can open a draft pull request and start an implementation intent.

**Architecture:** The daemon does all the work. A new `feedback` module owns an upload stage (multipart parts streamed to a private temp directory), a pure issue-folder writer, a git step (temporary worktree from the default branch, commit, push) and a GitHub step (`gh` run through the plugin tool runner). Every submission is a row keyed by a submission id the app makes, so a repeated or retried call continues where it stopped and never repeats a finished step. The `feedback` built-in plugin is a manifest compiled into the daemon (host mode `in-process`, zero resources, two tools); it only reports and runs tools. The app adds a Submit screen to the Feedback pane and two buttons to the intent detail; views talk to the daemon through a new `FeedbackCommands` trait.

**Tech Stack:** Rust stable, axum 0.8.9 (new feature `multipart`), reqwest 0.13.5 (new features `multipart`, `stream`), sqlx/SQLite (migration `0004`), tokio, `gpui-kit = "=0.11.0"`, the daemon's fake harness and `fake-claude`, fake `gh` and `agent-browser` scripts, local bare git repositories.

**Spec:** `docs/superpowers/specs/2026-11-26-feedback-tool-design.md` (plan-split items 3, 4 and 5). Plugin system as built: `docs/plugins.md`. GPUI facts: `docs/superpowers/research/2026-11-26-gpui-capture-intent-research.md` ("research D4" and so on). `REG/` means `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`.

**Built after:** `docs/superpowers/plans/2026-11-26-intent-tool.md` and `docs/superpowers/plans/2026-11-26-feedback-capture.md`, on top of their branch. Their Interfaces blocks are consumed by exact name (summary under "Consumed from the two earlier plans"). If the code on the branch differs from a name used here, the code wins.

## Verified facts (2026-11-26, read from committed source at the pinned versions)

- `HostMode::InProcess` (`"in-process"`) parses, and `Manifest::parse` accepts a manifest with no `hosts`, `secrets`, `settings` and `resources` (`crates/plugin-protocol/src/manifest.rs:58-68,393-520`). Discovery refuses `in-process` for every directory (`crates/server/src/plugins/discovery.rs`, `check_dir`). `ToolDecl::is_valid_name("agent-browser")` and `is_valid_spec("npm:agent-browser")` hold by the rules at `manifest.rs:197-213`.
- The manager takes `Vec<LoadedPlugin { manifest, origin, runtime }>`, keeps the first of two equal ids, and creates a built-in plugin's stored configuration as enabled (`crates/server/src/plugins/manager.rs:82-87,398-410,716-733`). Tools are checked only before a sync run (`manager/run.rs:144-152`), so a plugin with no resources never gets `needs_setup` today. `HostServices::new` works with zero hosts (`crates/plugin-host/src/services.rs:49-77`).
- The command runner `run_command` and `Stdout::FirstLine` are `pub(crate)` in `crates/plugin-host/src/secrets.rs:326-415`: no shell, stdin closed, stderr discarded, every `SMITHAGENTS_*` variable removed, own process group, a timeout, 64 KiB of output at most, the first line returned. `tool_command` always runs from `/` (`tools.rs:229-237`). There is no public way to run a declared tool with arguments.
- `crates/server/src/git.rs`: `git_command` pins `LC_ALL=C`, sets `GIT_TERMINAL_PROMPT=0` and removes `SMITHAGENTS_SECRET_*`; `create_worktree(repo_root, worktrees_dir, branch, base)` runs `git worktree add -b <branch> -- <dir> <base>`; `remove_worktree`. `GitError::Command { stderr }` carries git's stderr and `ApiError` prints it (`api.rs:186-205`): new code must not use that variant for anything that reaches a response.
- `AgentService::create_agent_with` always branches from `repo.default_branch` (`crates/server/src/service.rs:229-231`); `IntentService::launch_agent` builds `CreateAgentOptions` (`crates/server/src/intents.rs:917-941`). `apply_pass2` replaces `criteria` and `depends_on` whole (`crates/server/src/polish.rs:405-463`); pass 1 touches neither.
- Settings are a key and JSON value table (`Store::get_setting`/`set_setting`, `store.rs:629-655`); agent defaults use the key `agent_defaults` (`intents.rs:73`). Intents are one row with fixed columns (`migrations/0002_intents.sql`).
- The router has one body limit of 1 MiB for every route (`api.rs:50,109`); mutating handlers wrap the service call in `detached` (`api.rs:235-252`).
- axum 0.8.9: feature `multipart = ["dep:multer"]` (`REG/axum-0.8.9/Cargo.toml:113`, multer `3.0.0`); `Multipart` is a `FromRequest` extractor that reads through `req.with_limited_body()`, so it obeys `DefaultBodyLimit`; `Multipart::next_field()`, `Field::{name, file_name, content_type}`, `Field::chunk() -> Result<Option<Bytes>, MultipartError>` for streaming; `MultipartError::status()` is 413 for a body-limit error (`REG/axum-0.8.9/src/extract/multipart.rs:64-110,150-232,247-260`). `multer` is not in `Cargo.lock` and not in the local registry; its dependencies (`encoding_rs`, `httparse`, `memchr`, `mime`, `spin`) are locked already.
- reqwest 0.13.5: feature `multipart` (`mime_guess`, `futures-util`, both locked) gives `multipart::{Form, Part}`; with feature `stream` (`tokio-util`, locked) `Part::file(path)` streams a file with its length (`REG/reqwest-0.13.5/src/async_impl/multipart.rs:112-120,275-296`). The workspace enables only `json` today.
- gpui-component 0.11.0 has `Checkbox::new(id).label(..).checked(bool).on_click(|&bool, window, cx|)` (`REG/gpui-component-0.11.0/src/checkbox.rs:40-125`) and `Radio::new(id).label(..).checked(bool).disabled(bool).on_click(..)` (`radio.rs:41-128`). The research does not cover them.
- `ensure_daemon_with(home, bin, version, &SpawnOptions { args, env })` passes extra environment to the daemon (`crates/client/src/ensure.rs:52-58,238`).
- NOT verified: that `agent-browser` is the npm package's name and that it can record video (no network here). Task 1 checks first.
- NOT verified: whether `ElementSnapshot::checked()` reports a `Checkbox` or `Radio` in headless tests. Task 9 checks first.

## Decisions made here (the decisions file leaves them to the plan)

- **The Submit "dialog" is a screen of the Feedback pane,** not a modal: it holds a text area, pickers and a busy state, and "Back" returns to the draft. Confirmations ("Open pull request…") use `open_alert_dialog` as the intent tool does.
- **Built-in registration:** `plugins/embedded.rs` returns `LoadedPlugin`s built from manifests compiled in with `include_str!`, with a runtime that never syncs. They are listed first, so the manager's "first id wins" rule keeps a directory named `feedback` from replacing it.
- **Status of a plugin without resources** is derived from a tool check when the status is read (the manager's 60 s cache), never from a run.
- **Tools are run through the manager:** `PluginManager::run_tool` runs only a tool that a built-in plugin's manifest declares, in a given directory.
- **`meta_repo_id` is a setting** (key `feedback`), so it needs no column. Migration `0004` adds `intents.base_branch`, `intents.origin_json` and the table `feedback_submissions`.
- **Fixed items.** `Item` gains `fixed: bool`. Pass 2 keeps every fixed criterion and dependency, first and unchanged. Idea criteria and the implementation intent's dependency are fixed and accepted.
- **Upload shape:** part `request` (JSON, first, at most 256 KiB), then `file-<i>` and, for screenshots, `annotated-<i>` (`i` is the attachment's 1-based position). The daemon chooses every file name; the client's file names are never used as paths.
- **The 100 MB total counts every file part,** annotated images included. The route's body limit is that total plus 1 MiB for the JSON and the boundaries.
- **Dates are UTC.** Slug: `slugify(title)` from `smithagents_core::intent`, or `feedback` when the title has no ASCII letter or digit.
- **The agent's branch is not the feedback branch.** Send creates `intent/<slug>` from `base_branch`, as today. "Open pull request" pushes the agent's branch to `origin` under the name `idea/<slug>`. The implementation intent's base is the idea agent's branch.
- **The issue body** is `issue.md` without its front matter, with links to the folder's files rewritten to `https://github.com/<owner>/<repo>/blob/<commit>/<folder>/<file>?raw=true`, and a last line `<!-- smithagents-feedback:<submission id> -->`, which a retry searches for before creating a second issue.
- **Only `github.com` counts as a GitHub remote.** The URL is read with `git config --get remote.origin.url` (not `git remote get-url`, which applies `insteadOf` rewrites; tests rely on the difference to keep pushes local).
- **The commit is made with `--no-verify`** and the user's own git identity and signing settings. No identity is invented.
- **Submitting with no attachment is allowed by the daemon;** the app's Submit button needs at least one.

Exact sentences (app constants in `crates/app/src/feedback/text.rs`; `{}` is filled in):

| Constant | Text |
|---|---|
| `SUBMIT_EMPTY_TOOLTIP` | `Add a screenshot or a file first` |
| `MAIN_IDEA_LABEL`, `MAIN_IDEA_HINT` | `Main idea`; `The first line is the title.` |
| `SEND_AS_INTENT`, `SEND_AS_ISSUE` | `An intent to work on now`; `A GitHub issue` |
| `NO_REMOTE_REASON` | `This repository has no GitHub remote.` |
| `GH_MISSING_REASON` | `Install the GitHub CLI (gh) to create issues.` |
| `GH_SIGNED_OUT_REASON` | `Sign in first: gh auth login` |
| `PUBLIC_WARNING` | `This repository is public. The screenshots will be visible to anyone.` |
| `MAYBE_PUBLIC_WARNING` | `This repository may be public. The screenshots will be visible to anyone.` |
| `IDEA_LABEL`; parts | `Prepare an idea PR for teammates`; `Demo video`, `Diagrams`, `Explainer doc` |
| `DEMO_HELP`, `DEMO_MISSING_REASON` | `Only works for projects a browser can show.`; `Install agent-browser to record a demo video.` |
| `IDEA_NEEDS_PART` | `Choose at least one part.` |
| `summary_intent(n)` | `Creates the issue folder with {n} on a new branch, and an intent to review and send.` |
| `summary_issue(n)` | `Creates the issue folder with {n}, pushes the branch to origin and opens a GitHub issue.` |
| `summary_idea(n, parts)` | `Creates the issue folder with {n}. An agent adds {parts} on an idea branch, and you open the pull request.` (`parts`: `an explainer doc`, `diagrams`, `a demo video`, joined with `, ` and a final ` and `) |
| `{n}` | `1 screenshot`, `2 screenshots`, `1 file`, `2 screenshots and 1 file` (screenshots first; files are images and videos) |
| `result_intent(title, branch, sha7)` | `Created the intent “{title}”. Files: {branch} ({sha7}).` |
| `result_issue(url, branch, sha7)` | `Created {url}. Files: {branch} ({sha7}).` |
| `partial_text(branch, sha7, message)` | `The files are committed on {branch} ({sha7}), but: {message}` |
| `NOTHING_CREATED_TEXT` | `Nothing was created. Submit again.` |
| `PR_TITLE`, `PR_DESCRIPTION` | `Open a pull request?`; `Pushes the idea branch to origin and opens a draft pull request. If the repository is public, the screenshots will be visible to anyone.` |
| Buttons | `Back`, `Create intent`, `Create issue`, `Retry`, `Start over`, `Open pull request…`, `Open pull request`, `Create implementation intent`, `Use for intents and feedback` |
| `META_TAG` | `Intents and feedback` |

## Global Constraints

- Naming is `smithagents`. `unsafe_code = "forbid"` stays in every crate.
- No literal colors outside token files (`cargo themegen check`). Exact pin `gpui-kit = "=0.11.0"`.
- Migrations `0001` to `0003` are never edited; this plan adds `0004_feedback.sql` once, in Task 2.
- `Cargo.lock` may gain `multer` (through axum's `multipart` feature) and nothing may change version; if `git diff Cargo.lock` shows a changed version, stop and report.
- No secret, token, repository URL with credentials, or output of `git` or `gh` reaches a log, an error, an event or an API response. Messages about a failed command are fixed sentences plus an exit status at most.
- Screenshots and files leave the machine only at Submit, and a push happens only for "A GitHub issue" and "Open pull request", to `origin` of the repository shown. The daemon supplies no credential to git and never passes `--force`.
- The user's working tree and checked-out branch are never touched: files are written only in a temporary worktree under `<home>/tmp/feedback/`.
- Limits, verbatim from the design: 10 attachments; 10 MB per image; 50 MB per video; 100 MB per submission (binary megabytes: `10 * 1024 * 1024` and so on); `demo.mp4` at most 50 MB. From the capture plan: at most 20 boxes per screenshot, a comment at most 500 characters.
- Tests never run a real `gh`, `mise` or `agent-browser`, never run an `#[ignore]` test, never push to a real remote (only to a bare repository the test creates), never capture the screen and never launch the app. `gh` and `agent-browser` in tests are fake executables on an injected `ToolFinder` path or first on the test daemon's `PATH`. Polish uses the fake harness or `fake-claude`.
- Every daemon call from a view goes through `FeedbackCommands`, `IntentCommands` or `AgentCommands`. No optimistic updates. Every string a view test asserts is a `Label` with the id given in the task or a button's label.
- The repository is public: no personal names, emails, tokens or private repository names in code, tests or docs. Test remotes use `https://github.com/smithagents-test-invalid/widgets.git`.
- Gate for every task: `cargo test --workspace --features smithagents-server/test-util`, `cargo clippy --workspace --all-targets --features smithagents-server/test-util -- -D warnings`, `cargo fmt --all --check`, `cargo themegen check`, and `pgrep -fl smithagentsd` empty afterwards.
- Commits end with the trailer `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>`.

## Review Focus

1. **A hostile or oversized upload.** A part that is not what the request says (text sent as a screenshot, a JPEG as the annotated PNG), a part named `../../x`, a part sent twice, `request` not first, 11 attachments, a box outside 0..1, an `original_name` with a path and a newline, an image of 10 MB plus one byte, a total over 100 MB. Expected: 400 or 413 with a sentence naming the attachment by number; no branch, no row and no file left under `<home>/tmp/feedback/`; a name from the client is never a path. (Tests in Tasks 4 and 6.)
2. **A failure after the commit.** The push is refused, `gh` fails or times out, or the intent cannot be created. Expected: the answer names the branch and commit and says which step failed; the draft stays; Retry repeats only that step and does not commit, push or create twice. (Tests in Tasks 6, 8 and 9.)
3. **A secret or command output leaking.** `gh` and git print text on failure; the remote URL may hold a token; the daemon's environment holds `SMITHAGENTS_*` secrets. Expected: none of it appears in a response, an event or the daemon log, and `gh` does not see `SMITHAGENTS_*`. (Tests in Tasks 1, 5, 8 and 12.)
4. **A repository that changed under the tool.** The stored default branch was renamed or deleted, has new commits, has no `origin`, or `origin` is not GitHub. Expected: 409 `default_branch_missing` with nothing created; the branch starts from the default branch's tip of that moment; the intent path works without a remote; the issue path is refused before anything is written. (Tests in Tasks 5, 6 and 8.)
5. **A double submit.** Submit pressed twice, the same request sent twice at once, or sent again after a timeout. Expected: one branch, one commit, one intent or issue; the second answer equals the first. (Tests in Tasks 6, 8 and 9.)

## File Structure

```
crates/plugin-host/src/tools.rs               ToolFinder::run, ToolRun, ToolRunError (public tool runner)
crates/server/src/plugins/embedded.rs         embedded_plugins(), the never-syncing runtime
crates/server/src/plugins/embedded/feedback.toml   the feedback manifest
crates/server/src/plugins/manager{.rs,/status.rs,/tools.rs}   status without resources; tool_state, run_tool
crates/core/src/feedback.rs                   wire types: request, outcome, targets, settings, origin
crates/core/src/intent.rs                     Item.fixed; Intent.base_branch, Intent.origin
crates/server/migrations/0004_feedback.sql    two intent columns, feedback_submissions
crates/server/src/store.rs                    the new columns; Submission rows
crates/server/src/feedback/mod.rs             FeedbackService, FeedbackError, settings, targets, submit, retry
crates/server/src/feedback/issue.rs           pure: title, slug, names, issue.md, seed text, issue body, GitHub URL parsing
crates/server/src/feedback/upload.rs          UploadLimits, sniffing, request checks, staging parts on disk
crates/server/src/feedback/repo.rs            placement, temporary worktree, commit, push
crates/server/src/feedback/github.rs          gh readiness, visibility, labels, issue, pull request
crates/server/src/feedback/idea.rs            fixed criteria, pull request, implementation intent
crates/server/src/git.rs                      branch_exists, path_in_branch, origin_url, commit_paths, push_branch, show_file, ...
crates/server/src/{api.rs,paths.rs,service.rs,intents.rs,polish.rs,brief.rs,bin/smithagentsd.rs}
crates/server/tests/bin/{fake-gh,fake-agent-browser}; tests/common/feedback.rs; tests/{feedback_plugin,feedback_store,feedback_issue,feedback_repo,feedback_api,feedback_github,feedback_idea}.rs
crates/client/src/api.rs                      feedback methods; crates/client/tests/feedback.rs (end to end)
crates/app/src/link.rs                        FeedbackCommands; model.rs: feedback settings
crates/app/src/feedback/{text.rs,submit_vm.rs,state.rs}   sentences, pure submit view model, upload list and pending submission
crates/app/src/views/feedback/{mod.rs,submit.rs}          screens, the Submit screen
crates/app/src/views/intents/actions.rs       Open pull request, Create implementation intent
crates/app/src/views/settings/repositories.rs the meta repository control
crates/app/tests/{feedback_submit_vm,feedback_submit,idea_actions,meta_repo}.rs; tests/common/feedback_submit.rs
docs/plugins.md, docs/feedback.md, the design and the roadmap
```

## Consumed from the two earlier plans

- Intent tool: `IntentKind::{NewFeature, Enhancement, Bug, ..}` with `wire_name`; `CreateIntentRequest { repo_id, kind, seed }`; `Seed.kind`; `Action::{IntentsLoaded, IntentUpserted, AgentDefaultsLoaded}`; `ShellModel::{intents, intent, agent, repos}`; `IntentCommands`, `IntentAnswer`; `ShellState::{intent_commands, reveal, now, command_timeout, last_repo_id, set_last_repo_id}`; `RailEntry::Intents`; `Shell::show_rail_entry`; `IntentsPane::{open_screen, run, busy, error_line}`, `Screen::Detail(IntentId)`, `Busy`; `detail_actions`; `intent_vm::{default_repo, intent_error_text, kind_label}`; `views::intents::actions` ids (`SEND_ID`, `RESOLVE_ID`, ..); `tests/common/intents.rs` (`FakeIntents`, `intent`, `item`, `model_with`, `fixed_clock`).
- Feedback capture: `Feedback` entity (`draft`, `store`, `notices`, `busy`, `discard_all`), `Draft { main_idea, kind: Option<String>, attachments }`, `Attachment { kind, original_name, stored_name, boxes, .. }`, `AttachmentKind`, `annotated_name`, `DraftStore::{dir, path_of}`, `FeedbackPane`, `FEEDBACK_SUBMIT_ID`, `SUBMIT_TOOLTIP`, `card_title`, `tests/common/feedback.rs::FeedbackSetup`.

---

### Task 1: The `feedback` built-in plugin and a public tool runner

**Files:**
- Create: `crates/server/src/plugins/embedded.rs`, `crates/server/src/plugins/embedded/feedback.toml`, `crates/server/tests/feedback_plugin.rs`, `crates/server/tests/bin/{fake-gh,fake-agent-browser}`, `crates/server/tests/common/feedback.rs`
- Modify: `crates/plugin-host/src/{tools.rs,lib.rs}`, `crates/server/src/plugins/{mod.rs,manager.rs,manager/status.rs,manager/tools.rs}`, `crates/server/src/api.rs:254-306` (`From<ManagerError>`), `crates/server/src/bin/smithagentsd.rs:272-320`, `crates/server/tests/common/mod.rs`
- Test: also `crates/plugin-host/tests/tools.rs`

**Interfaces:**
- Produces:

```rust
// plugin-host/src/tools.rs
pub struct ToolRun<'a> { pub args: &'a [String], pub cwd: &'a Path, pub env: &'a [(&'a str, &'a str)], pub timeout: Duration }
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolRunError { NotFound, NotStarted, Timeout, TooMuchOutput, Failed { status: Option<i32> } }   // carries no output
impl ToolFinder {
    /// Runs `<name> <args>` in `cwd` through `run_command`; Ok is the first line it printed, trimmed (None when empty).
    pub async fn run(&self, name: &str, run: ToolRun<'_>) -> Result<Option<String>, ToolRunError>;
}
// server/src/plugins/embedded.rs
pub const FEEDBACK_PLUGIN_ID: &str = "feedback";
pub fn embedded_plugins() -> Vec<LoadedPlugin>;          // origin Builtin; today only feedback
// server/src/plugins/manager.rs
pub enum ManagerError { /* existing */ Tool(ToolRunError) }
impl PluginManager {
    /// A fresh check of one declared tool of a built-in plugin. UnknownPlugin, NotAllowed (installed plugin), UnknownTool.
    pub async fn tool_state(&self, plugin: &str, tool: &str) -> Result<ToolState, ManagerError>;
    /// Runs a declared tool of a built-in plugin. Same refusals; a run failure is ManagerError::Tool.
    pub async fn run_tool(&self, plugin: &str, tool: &str, run: ToolRun<'_>) -> Result<Option<String>, ManagerError>;
}
// tests/common/feedback.rs
pub struct FakeTools { pub bin: PathBuf, pub dir: PathBuf }   // bin holds `gh` and/or `agent-browser`; dir is FAKE_GH_DIR
impl FakeTools { pub fn new(root: &Path, gh: bool, agent_browser: bool) -> Self; pub fn finder(&self) -> ToolFinder;
    pub fn calls(&self) -> Vec<String>; pub fn set(&self, file: &str, text: &str); pub fn read(&self, file: &str) -> Option<String>; }
```

`feedback.toml`, exactly:

```toml
id = "feedback"
name = "Feedback"
version = "0.1.0"
contract = 1
host = "in-process"

[[tools]]
name = "gh"
label = "GitHub CLI"
install_url = "https://cli.github.com"
install = { mise = "gh", brew = "gh" }

[[tools]]
name = "agent-browser"
label = "agent-browser"
install = { mise = "npm:agent-browser" }
```

Rules: the embedded runtime's `mode()` is `HostMode::InProcess` and its `sync` answers `CallError::Contract { message: "this plugin has nothing to sync" }`. `start_plugins` builds `embedded_plugins()` first and appends the discovered ones. For a plugin with no resources, `status::status` calls `tools::missing_tools` (60 s cache) while the plugin is enabled, loaded and not failed: a non-empty answer gives `NeedsSetup { tools, message: setup_message(..) }`, an empty one `Ready`; `tools::install` stores no `Problem` for such a plugin and only forgets the cached check. Plugins with resources behave exactly as before. `ApiError` maps `ManagerError::Tool` to 502 `tool_failed` with the error's own sentence. `ToolFinder::run` sets `cwd` and the given variables on the command and otherwise is `run_command(.., Stdout::FirstLine)`; `NotFound` when `find(name)` is `None`.

`fake-gh` (a `sh` script, mode 0755; `FAKE_GH_DIR` names its control directory): appends `"$*"` as one line to `calls.log` and its environment (`env`) to `env.log`; when the file `fail` holds exactly `"$1 $2"` it prints `LEAK-gh-stdout` to stdout and `LEAK-gh-stderr` to stderr and exits 1; when `hang` holds `"$1 $2"` it sleeps 600 s. Otherwise: `auth status` exits 1 if the file `signed-out` exists, else 0; `repo view` prints the file `visibility` or `PRIVATE`; `label list` prints the file `label` or `true`; `issue list` and `pr list` print the file `existing` or nothing; `issue create` copies the file named by `--body-file=` to `issue-body.md` and prints `https://github.com/smithagents-test-invalid/widgets/issues/7`; `pr create` copies it to `pr-body.md` and prints `https://github.com/smithagents-test-invalid/widgets/pull/8`. `fake-agent-browser` exits 0. `FakeTools::new` copies the chosen scripts into `<root>/bin` (0755) and creates `<root>/gh`.

- [ ] **Step 1: Verify first: `agent-browser`.** If the network is available, confirm on the npm registry page that a package named `agent-browser` exists and that its documentation describes recording a video. Record the answer in the commit message. If it cannot be confirmed, keep the tool declaration as written and leave the Follow-ups line about it in place; nothing else in this plan depends on the answer (the daemon never runs `agent-browser`).
- [ ] **Step 2: Write the failing tests.**
  - `plugin-host/tests/tools.rs`: `run_returns_the_first_line_and_runs_in_the_given_directory` (a script `pwd; echo second` in a temp finder dir, `cwd` a second temp dir: `Ok(Some(<that dir, canonical>))`). `run_strips_smithagents_variables_and_passes_the_given_ones` (Review Focus 3; process env `SMITHAGENTS_SECRET_RUN_TEST__X=LEAK-secret`, `env: &[("GH_PROMPT_DISABLED", "1")]`, script `env > "$OUT"`: the file has `GH_PROMPT_DISABLED=1` and no line starting with `SMITHAGENTS_`). `run_errors_carry_no_output` (script `echo LEAK-out; echo LEAK-err >&2; exit 3` → `Err(Failed { status: Some(3) })` whose `to_string()` has no `LEAK`; an unknown name → `NotFound`; `sleep 5` with a 100 ms timeout → `Timeout`).
  - `server/tests/feedback_plugin.rs` (manager started with `embedded_plugins()` and `ManagerConfig { tools: fake.finder(), ..Default::default() }`): `the_feedback_manifest_has_two_tools_and_no_resources` (`Manifest::parse` of the embedded text: id `feedback`, host `InProcess`, tools `gh` and `agent-browser`, `install["mise"] == "npm:agent-browser"`, no resources, hosts, secrets or settings). `it_is_listed_as_a_ready_built_in_plugin` (both fakes: `get("feedback")` has `builtin: true`, `enabled: true`, `host: "in-process"`, `resources: []`, state `Ready`). `missing_tools_are_advice` (no fakes: state `NeedsSetup` with tools `["gh", "agent-browser"]` in that order and message `needs GitHub CLI (gh): not installed; needs agent-browser (agent-browser): not installed`; only `gh`: one missing tool `agent-browser`). `the_state_follows_the_tools_without_a_restart` (start without `gh`, copy it in, advance the paused clock 61 s: `Ready`). `an_embedded_id_wins_over_a_directory_plugin` (list `embedded_plugins()` followed by an `Origin::Installed` plugin with id `feedback`: `list()` has one `feedback`, `builtin: true`). `run_tool_runs_only_declared_tools_of_built_in_plugins` (`run_tool("feedback", "gh", args ["auth", "status"])` is `Ok` and `calls()` is `["auth status"]`; `"mise"` → `UnknownTool`; plugin `"nope"` → `UnknownPlugin`; an installed fixture plugin → `NotAllowed`; with `fail` = `auth status` → `Err(ManagerError::Tool(Failed { status: Some(1) }))`). `tool_state_reports_a_missing_tool`. `plugins_with_resources_are_unchanged` (the existing `plugin_manager.rs` and `plugin_api.rs` suites pass untouched).
- [ ] **Step 3: Run them.** `cargo test -p smithagents-plugin-host --test tools && cargo test -p smithagents-server --features test-util --test feedback_plugin`. Expected: compile errors (`ToolRun`, `embedded_plugins`).
- [ ] **Step 4: Implement** as specified.
- [ ] **Step 5: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 6: Commit.** `feat(plugins): the feedback built-in plugin and a runner for declared tools`

---

### Task 2: Core types, migration 0004 and the store

**Files:**
- Create: `crates/core/src/feedback.rs`, `crates/server/migrations/0004_feedback.sql`, `crates/server/tests/feedback_store.rs`
- Modify: `crates/core/src/{lib.rs,intent.rs:86-95,172-188}`, `crates/server/src/store.rs:135-160,223-250,503-535` (row mapping, insert, update), every `Intent { .. }` and `Item { .. }` literal the compiler lists

**Interfaces:**
- Produces:

```rust
// core/src/intent.rs
pub struct Item { /* existing */ #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub fixed: bool }
pub struct Intent { /* existing */
    #[serde(default, skip_serializing_if = "Option::is_none")] pub base_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub origin: Option<FeedbackOrigin> }
// core/src/feedback.rs (all Serialize + Deserialize; enums snake_case)
pub struct SubmissionId(pub Uuid);                           // transparent; Display, FromStr, new()
pub enum FeedbackKind { NewFeature, Enhancement, Bug }       // intent_kind() -> IntentKind of the same name
pub enum SendAs { Intent, GithubIssue }                      // "intent" | "github_issue"
pub enum AttachmentKind { Screenshot, Image, Video }         pub struct IdeaOptions { pub explainer: bool, pub diagrams: bool, pub demo_video: bool }
pub struct FeedbackBox { pub x: f32, pub y: f32, pub w: f32, pub h: f32, pub comment: String }
pub struct FeedbackAttachment { pub kind: AttachmentKind, pub original_name: String, #[serde(default)] pub boxes: Vec<FeedbackBox> }
pub struct FeedbackRequest { pub submission_id: SubmissionId, pub main_idea: String, pub kind: FeedbackKind, pub repo_id: RepoId,
    pub send_as: SendAs, #[serde(default)] pub idea: Option<IdeaOptions>, pub app_version: String, pub platform: String,
    #[serde(default)] pub attachments: Vec<FeedbackAttachment> }
pub enum FeedbackStep { CreateIntent, Push, CreateIssue }
pub struct FeedbackFailure { pub step: FeedbackStep, pub code: String, pub message: String }
pub struct FeedbackOutcome { pub submission_id: SubmissionId, pub repo_id: RepoId, pub branch: String, pub commit: String, pub folder: String,
    pub pushed: bool, pub intent: Option<Intent>, pub issue_url: Option<String>, pub failed: Option<FeedbackFailure> }
pub struct IdeaPackage { pub explainer: bool, pub diagrams: bool, pub demo_video: bool,
    pub pr_url: Option<String>, pub not_done: Option<String>, pub implementation_intent_id: Option<IntentId> }
pub struct FeedbackOrigin { pub folder: String, pub branch: String, pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub idea: Option<IdeaPackage> }
pub struct FeedbackSettings { pub meta_repo_id: Option<RepoId> }                    // Default: None
pub fn effective_meta_repo(stored: Option<RepoId>, repos: &[Repo]) -> Option<RepoId>;
pub struct ToolReadiness { pub ready: bool, pub reason: Option<String> }
pub struct FeedbackTarget { pub repo_id: RepoId, pub has_github_remote: bool, pub is_public: Option<bool> }
pub struct FeedbackTargets { pub repos: Vec<FeedbackTarget>, pub meta_repo_id: Option<RepoId>, pub gh: ToolReadiness, pub agent_browser: ToolReadiness }
// server/src/store.rs
pub struct Submission { pub id: SubmissionId, pub repo_id: RepoId, pub request: FeedbackRequest, pub title: String, pub branch: String,
    pub commit: String, pub folder: String, pub pushed: bool, pub intent_id: Option<IntentId>, pub issue_url: Option<String>,
    pub created_at: DateTime<Utc>, pub updated_at: DateTime<Utc> }
impl Store {
    pub async fn insert_submission(&self, s: &Submission) -> Result<(), StoreError>;   // a second insert of an id is StoreError::Conflict
    pub async fn get_submission(&self, id: SubmissionId) -> Result<Option<Submission>, StoreError>;
    pub async fn save_submission(&self, s: &Submission) -> Result<(), StoreError>;     // pushed, intent_id, issue_url, updated_at
}
```

`0004_feedback.sql`: `ALTER TABLE intents ADD COLUMN base_branch TEXT;` `ALTER TABLE intents ADD COLUMN origin_json TEXT;` and `CREATE TABLE feedback_submissions (id TEXT PRIMARY KEY NOT NULL, repo_id TEXT NOT NULL REFERENCES repos(id), request_json TEXT NOT NULL, title TEXT NOT NULL, branch TEXT NOT NULL, commit_sha TEXT NOT NULL, folder TEXT NOT NULL, pushed INTEGER NOT NULL DEFAULT 0, intent_id TEXT REFERENCES intents(id), issue_url TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);`. `effective_meta_repo`: `stored` while a repository with that id exists; else the only repository when there is exactly one; else `None`.

- [ ] **Step 1: Write the failing tests.**
  - core (unit tests in `feedback.rs` and `intent.rs`): `old_intent_and_item_json_still_load` (an intent JSON without `base_branch`, `origin` and without `fixed` on its items parses with `None`, `None`, `false`, and serializes back without those keys). `the_request_has_the_documented_wire_shape` (a request with one screenshot and `idea: Some(..)` serializes with `"send_as":"github_issue"`, `"kind":"new_feature"`, `"kind":"screenshot"` on the attachment; `{"submission_id":..,"main_idea":"x","kind":"bug","repo_id":..,"send_as":"intent","app_version":"0.1.0","platform":"macos"}` parses with no idea and no attachments; `"kind":"feature"` is an error). `feedback_kinds_map_to_intent_kinds`. `the_meta_repository_is_implied_only_for_a_single_repository` (stored and present → it; stored but gone, two repositories → `None`; nothing stored, one repository → that one; none → `None`).
  - `server/tests/feedback_store.rs`: `a_database_from_0003_migrates_and_keeps_its_intents` (in the style of `store.rs:1646`: run migrations up to 3, insert an intent row, run all: `get_intent` returns it with `base_branch: None`, `origin: None`). `base_branch_and_origin_round_trip` (insert, `save_intent` with other values, `list_intents`). `a_fixed_item_round_trips_in_content_json`. `a_submission_round_trips_and_its_id_is_unique` (insert, get, `save_submission` with `pushed: true`, `issue_url`; a second insert of the id is `StoreError::Conflict`; an unknown id is `None`).
- [ ] **Step 2: Run them.** `cargo test -p smithagents-core feedback && cargo test -p smithagents-server --features test-util --test feedback_store`. Expected: compile errors.
- [ ] **Step 3: Implement.** The compiler lists the literals to extend with `fixed: false`, `base_branch: None`, `origin: None`.
- [ ] **Step 4: Run the tests.** Expected: PASS, with every existing suite green. Gate.
- [ ] **Step 5: Commit.** `feat(core): feedback wire types; intents carry a base branch and an origin (migration 0004)`

---

### Task 3: The meta repository setting: service, route, client and the Settings control

**Files:**
- Create: `crates/server/src/feedback/mod.rs`, `crates/app/tests/{meta_repo.rs,common/feedback_submit.rs}`
- Modify: `crates/server/src/{lib.rs,api.rs:54-65,91-94,1035-1050}`, `crates/server/src/bin/smithagentsd.rs:223-234`, the `AppState { .. }` literals in `crates/server/tests/{api.rs,plugin_api.rs}` and any other the compiler lists, `crates/client/src/api.rs:290-305`, `crates/app/src/{link.rs,model.rs,shell.rs,shell_state.rs}`, `crates/app/src/views/settings/repositories.rs:230-348`, `crates/app/src/view_models.rs:689-712`, `crates/app/tests/{link.rs,common/mod.rs}`
- Test: `crates/server/tests/feedback_api.rs` (created here), `crates/client/tests/intents.rs`

**Interfaces:**
- Consumes: Task 2 (`FeedbackSettings`, `effective_meta_repo`).
- Produces:

```rust
// server/src/feedback/mod.rs
#[derive(Clone)] pub struct FeedbackService;
impl FeedbackService {
    pub fn new(store: Store, intents: IntentService, agents: AgentService, plugins: PluginManager, paths: Paths) -> Self;
    pub async fn settings(&self) -> Result<FeedbackSettings, FeedbackError>;              // a stored id whose repository is gone reads as None
    pub async fn set_settings(&self, settings: FeedbackSettings) -> Result<FeedbackSettings, FeedbackError>;
}
pub enum FeedbackError { /* grows in later tasks */ RepoNotFound, Store(StoreError) }
impl FeedbackError { pub fn code(&self) -> &'static str; pub fn status(&self) -> StatusCode; }   // ApiError::from uses both and to_string()
pub struct AppState { /* existing */ pub feedback: FeedbackService }
// routes: GET and PUT /v1/settings/feedback, body and answer {"meta_repo_id": "<uuid>" | null}
// client
impl Client { pub async fn feedback_settings(&self) -> Result<FeedbackSettings, ClientError>;
              pub async fn set_feedback_settings(&self, settings: &FeedbackSettings) -> Result<FeedbackSettings, ClientError>; }
// app: link.rs
pub trait FeedbackCommands {
    fn set_feedback_settings(&self, settings: FeedbackSettings) -> LocalBoxFuture<'static, Result<FeedbackSettings, LinkError>>;
}   // impl for DaemonLink; later tasks add methods
// app: model.rs / shell.rs / shell_state.rs
Action::FeedbackSettingsLoaded(FeedbackSettings);   impl ShellModel { pub fn feedback_settings(&self) -> Option<&FeedbackSettings>; }
pub struct ShellOptions { /* existing */ pub feedback_commands: Option<Rc<dyn FeedbackCommands>> }
impl ShellState { pub fn feedback_commands(&self) -> Option<&dyn FeedbackCommands>; }       // injected, else the link, else None
// app: view_models.rs and settings/repositories.rs
pub struct RepoRowVm { /* existing */ pub is_meta: bool, pub can_set_meta: bool }
pub fn repo_rows(repos: &[Repo], settings: Option<&FeedbackSettings>) -> Vec<RepoRowVm>;
pub fn repo_meta_tag_id(id: RepoId) -> ElementId;    // "repo-meta-<uuid>"; a Tag reading META_TAG
pub fn repo_use_meta_id(id: RepoId) -> ElementId;    // "repo-use-meta-<uuid>"; button "Use for intents and feedback"
pub const REPO_META_ERROR_ID: &str = "repo-meta-error";   // Role::Status
// tests/common/feedback_submit.rs
pub struct FakeFeedback { pub calls: RefCell<Vec<FeedbackCall>>, .. }   // records, then answers the next queued result or waits for `answer`, like FakeIntents
pub enum FeedbackCall { SetSettings(FeedbackSettings) /* later tasks add variants */ }
```

Rules: the setting is stored under the key `feedback`. `PUT` with an unknown repository answers 404 `repo_not_found`; `null` clears it. The link fetches the settings at connect and sends `FeedbackSettingsLoaded` after `AgentDefaultsLoaded` and before `ConnectionChanged(Connected)`; `set_feedback_settings` sends it again with the answer. `is_meta` is `effective_meta_repo(stored, repos) == Some(id)`; `can_set_meta` is `!is_meta` and there are two or more repositories and the settings are loaded. A row shows the tag when `is_meta`, the button when `can_set_meta`; the button runs the command with the pane's command timeout, shows loading, and a failure shows under the list in `REPO_META_ERROR_ID` (`repo_error_text`, or `DAEMON_SILENT_TEXT`).

- [ ] **Step 1: Write the failing tests.**
  - `server/tests/feedback_api.rs` (a fixture like `tests/api.rs:43-92`, plus `FeedbackService` and a manager from `embedded_plugins()` with `FakeTools`): `the_meta_repository_can_be_read_set_and_cleared` (`GET` → `{"meta_repo_id":null}`; `PUT` an added repository → 200 with its id, and `GET` agrees; `PUT null` → `null`). `an_unknown_meta_repository_is_refused` (404, `"error":"repo_not_found"`, the stored value unchanged). `the_route_needs_the_token` (401).
  - `client/tests/intents.rs`: in `full_loop_through_the_client`, `set_feedback_settings` then `feedback_settings` return the repository.
  - `app/tests/link.rs`: update `intents_and_defaults_arrive_before_connected` to expect `Snapshot`, `IntentsLoaded`, `AgentDefaultsLoaded`, `FeedbackSettingsLoaded`, `ConnectionChanged(Connected)`.
  - `app/tests/meta_repo.rs` (Settings pane opened as in `settings_pane.rs`, with `FakeFeedback`): `a_single_repository_is_the_meta_repository_without_a_button` (the tag is found with the label "Intents and feedback"; no `repo_use_meta_id`). `the_chosen_repository_is_tagged_and_the_others_offer_the_button` (two repositories, the second stored: the tag on the second, the button "Use for intents and feedback" on the first). `no_repository_is_tagged_until_one_is_chosen` (two repositories, nothing stored: two buttons, no tag). `choosing_puts_the_setting_and_waits_for_the_daemon` (click the first row's button: one `FeedbackCall::SetSettings` with its id; the tag does not move until `FeedbackSettingsLoaded` is dispatched). `a_failure_is_shown_under_the_list` (queue an API error 404 `repo_not_found` "repo not found": `REPO_META_ERROR_ID` reads "Repo not found"). `nothing_can_be_chosen_before_the_settings_load` (no button).
- [ ] **Step 2: Run them.** `cargo test -p smithagents-server --features test-util --test feedback_api && cargo test -p smithagents-app --test meta_repo --test link`. Expected: compile errors.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(feedback): a meta repository per Space, set in Settings → Source repositories`

---

### Task 4: The issue folder writer (pure, no git, no I/O)

**Files:**
- Create: `crates/server/src/feedback/issue.rs`, `crates/server/tests/feedback_issue.rs`
- Modify: `crates/server/src/feedback/mod.rs` (`pub mod issue;`)

**Interfaces:**
- Consumes: Task 2 types; `smithagents_core::intent::slugify`; `crate::plugins::clean_message`.
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum FileKind { Png, Jpeg, Gif, Webp, Mp4 }   // extension(): "png" "jpg" "gif" "webp" "mp4"
pub const TITLE_MAX_CHARS: usize = 120;      pub const MAIN_IDEA_MAX_CHARS: usize = 20_000;
pub fn title_of(main_idea: &str) -> Option<String>;      // first non-blank line, leading '#' marks and spaces removed, cleaned, cut to 120 chars
pub fn issue_slug(title: &str) -> String;                // slugify(title); "feedback" when the title has no ASCII letter or digit
pub fn with_suffix(slug: &str, n: u32) -> String;        // n == 1: slug; else "<slug>-<n>"
pub fn folder_path(date: NaiveDate, slug: &str) -> String;                 // "docs/issues/2026-11-26-<slug>"
pub fn branch_name(date: NaiveDate, slug: &str, idea: bool) -> String;     // "feedback/2026-11-26-<slug>" | "idea/<slug>"
pub struct Names { pub file: String, pub annotated: Option<String> }
pub fn file_names(attachments: &[(AttachmentKind, FileKind)]) -> Vec<Names>;
pub struct IssueInput<'a> { pub title: &'a str, pub main_idea: &'a str, pub kind: FeedbackKind, pub created: DateTime<Utc>,
    pub app_version: &'a str, pub platform: &'a str, pub attachments: &'a [FeedbackAttachment], pub names: &'a [Names] }
pub fn render_issue(input: &IssueInput<'_>) -> String;
pub fn seed_text(input: &IssueInput<'_>, folder: &str, branch: &str, commit: &str, max_bytes: usize) -> String;
pub fn github_repo(remote_url: &str) -> Option<(String, String)>;          // (owner, repository) for github.com only
pub fn permalink_base(owner: &str, repo: &str, commit: &str, folder: &str) -> String;   // "https://github.com/<o>/<r>/blob/<commit>/<folder>/"
pub fn issue_body(issue_md: &str, files: &[String], permalink_base: &str, marker: Option<&str>) -> String;
pub fn marker(id: SubmissionId) -> String;               // "<!-- smithagents-feedback:<uuid> -->"
pub fn not_done_section(explainer_md: &str) -> Option<String>;             // the text under a "Not done" heading, cleaned, at most 500 chars
```

Rules:
- Names: `screenshot-<n>.png` with `screenshot-<n>.annotated.png`; `image-<n>.<ext>`; `clip-<n>.mp4`; `<n>` counts per prefix from 1 in attachment order.
- `issue.md`, exactly: a front matter block `---`, `title: <the title as a JSON string>`, `kind: <new_feature|enhancement|bug>`, `created: <RFC 3339, seconds, Z>`, `app_version: <v>`, `platform: <p>`, `---`; a blank line; `# <title>`; a blank line; the main idea without its first non-blank line, trimmed (left out when empty); then one section per attachment, each preceded by a blank line. Screenshot `k` (its position among screenshots): `## Screenshot k`, blank, `![Screenshot k](screenshot-<n>.annotated.png)`, blank, one line `<i>. <comment>` per box (the comment cleaned to one line), blank, `Original: [screenshot-<n>.png](screenshot-<n>.png)`. Image: `## <cleaned original name>`, blank, `![<cleaned original name>](image-<n>.<ext>)`. Video: `## <cleaned original name>`, blank, `[<cleaned original name>](clip-<n>.mp4)`. "Cleaned" is `clean_message`, then `[`, `]`, `(`, `)`, `` ` `` and `#` removed, then only the part after the last `/` or `\`, trimmed, `attachment` when empty. The file ends with one newline.
- `seed_text`: the main idea; a blank line; `Kind: <wire name>`; when any box exists, `Comments on the screenshots:` and one line `- Screenshot k, box i: <comment>` each; a blank line; ``Feedback files: `<folder>/` on branch `<branch>` (commit <commit>).``. When the text would exceed `max_bytes`, comment lines are dropped from the end and replaced by one line `- (more comments in issue.md)`.
- `github_repo` accepts `https://github.com/o/r`, `https://github.com/o/r.git`, `https://user:token@github.com/o/r.git`, `git@github.com:o/r.git`, `ssh://git@github.com/o/r.git`; the host must be exactly `github.com` (any case); owner and repository must match `[A-Za-z0-9_.-]+`. Anything else is `None`.
- `issue_body`: the text after the front matter block; every `](<name>)` whose `<name>` is exactly one of `files` becomes `](<permalink_base><name>?raw=true)`; nothing else is changed; when `marker` is given, a blank line and the marker are appended. Cut to 60,000 characters on a character boundary, the marker kept.

- [ ] **Step 1: Write the failing tests** in `tests/feedback_issue.rs`:
  - `the_title_is_the_first_line`: `"  \n## Fix the zoom\nmore"` → `"Fix the zoom"`; `"\n \t"` → `None`; a 300-character line of `é` → 120 characters; `"a\u{202e}b"` has no bidi mark.
  - `slugs_and_names`: `issue_slug("Fix the Zoom!")` is `fix-the-zoom`; of `"日本語"` is `feedback`; `with_suffix("x", 1)` is `x`, `(.., 3)` is `x-3`; `folder_path(2026-11-26, "fix-the-zoom")` is `docs/issues/2026-11-26-fix-the-zoom`; `branch_name(.., false)` is `feedback/2026-11-26-fix-the-zoom` and `(.., true)` is `idea/fix-the-zoom`.
  - `file_names_count_per_kind`: screenshot, JPEG image, screenshot, MP4, WebP image → `screenshot-1.png` (+ `.annotated.png`), `image-1.jpg`, `screenshot-2.png`, `clip-1.mp4`, `image-2.webp`.
  - `issue_md_is_rendered_exactly`: title "Fix the zoom", main idea `"Fix the zoom\n\nThe page is too big."`, kind bug, created 2026-11-26T14:15:00Z, version `0.1.0`, platform `macos`, one screenshot with comments "the toolbar" and "this label", one image `photo.jpg`, one video `demo.mp4` → the whole string equals the expected text written out in the test (front matter first, `title: "Fix the zoom"`, `kind: bug`, `created: 2026-11-26T14:15:00Z`, then `# Fix the zoom`, `The page is too big.`, `## Screenshot 1`, `![Screenshot 1](screenshot-1.annotated.png)`, `1. the toolbar`, `2. this label`, `Original: [screenshot-1.png](screenshot-1.png)`, `## photo.jpg`, `![photo.jpg](image-1.jpg)`, `## demo.mp4`, `[demo.mp4](clip-1.mp4)`).
  - `hostile_names_and_comments_cannot_break_the_file` (Review Focus 1): an image whose `original_name` is `"../../etc/passwd\n# pwn](http://x)"` renders the heading `## passwd  pwnhttp://x` on one line and no line starts with `# pwn`; a comment `"a\n## b"` is one list line; a title `"x\"\ny: z"` gives a `title:` line that parses back as that JSON string and the front matter still has exactly five keys.
  - `the_seed_names_the_folder_and_the_branch`: ends with ``Feedback files: `docs/issues/2026-11-26-fix-the-zoom/` on branch `feedback/2026-11-26-fix-the-zoom` (commit 0123abc).``, contains `Kind: bug` and `- Screenshot 1, box 2: this label`; with `max_bytes` 200 and forty long comments it is at most 200 bytes plus the last two lines and contains `- (more comments in issue.md)`.
  - `only_github_remotes_are_parsed`: each accepted form above → `("o", "r")`; `https://user:LEAK-token@github.com/o/r.git` → `("o", "r")`; `https://gitlab.com/o/r`, `https://github.com.evil.example/o/r`, `/tmp/bare.git`, `""`, `https://github.com/o` → `None`.
  - `the_issue_body_links_to_the_commit`: with files `["screenshot-1.png", "screenshot-1.annotated.png"]` and base `https://github.com/o/r/blob/0123abc/docs/issues/2026-11-26-fix-the-zoom/`: the body has no front matter, contains `![Screenshot 1](https://github.com/o/r/blob/0123abc/docs/issues/2026-11-26-fix-the-zoom/screenshot-1.annotated.png?raw=true)`, leaves a link `[x](other.png)` and a link `[y](https://example.com/screenshot-1.png)` alone, and ends with `<!-- smithagents-feedback:<uuid> -->`; a 100,000-character body is cut to 60,000 and still ends with the marker.
  - `the_not_done_section_is_found`: `"# E\n\n## Not done\n\nThe project did not start.\n\n## Other"` → `Some("The project did not start.")`; no such heading → `None`.
- [ ] **Step 2: Run them.** `cargo test -p smithagents-server --features test-util --test feedback_issue`. Expected: compile errors.
- [ ] **Step 3: Implement `issue.rs`.**
- [ ] **Step 4: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(feedback): render the issue folder's names, issue.md, seed text and issue body`

---

### Task 5: Git side: place, commit in a temporary worktree, push

**Files:**
- Create: `crates/server/src/feedback/repo.rs`, `crates/server/tests/feedback_repo.rs`
- Modify: `crates/server/src/git.rs` (new functions below `remove_worktree`), `crates/server/src/paths.rs:44-52`, `crates/server/src/feedback/mod.rs`

**Interfaces:**
- Consumes: Task 4 (`issue_slug`, `with_suffix`, `folder_path`, `branch_name`); `git::{create_worktree, remove_worktree}`.
- Produces:

```rust
// git.rs (all through git_command; none returns or logs git's output)
pub async fn branch_exists(repo_root: &Path, branch: &str) -> Result<bool, GitError>;
pub async fn path_in_branch(repo_root: &Path, branch: &str, path: &str) -> Result<bool, GitError>;   // git cat-file -e <branch>:<path>
pub async fn origin_url(repo_root: &Path) -> Result<Option<String>, GitError>;       // git config --get remote.origin.url
pub async fn rev_parse(repo_root: &Path, rev: &str) -> Result<Option<String>, GitError>;   // None when it names no commit
pub async fn prune_worktrees(repo_root: &Path) -> Result<(), GitError>;
pub async fn delete_branch(repo_root: &Path, branch: &str) -> Result<(), GitError>;  // git branch -D
pub enum CommitError { IdentityMissing, Failed { status: Option<i32> }, Io(std::io::Error) }
/// `git add -- <paths>`, `git commit --no-verify -m <message>`, then the new commit's id.
pub async fn commit_paths(worktree: &Path, paths: &[&str], message: &str) -> Result<String, CommitError>;
pub enum PushError { NoRemote, Auth, Rejected, Timeout, Failed { status: Option<i32> } }
/// `git push origin refs/heads/<local>:refs/heads/<remote_branch>`, never forced, at most `timeout`.
pub async fn push_branch(repo_root: &Path, local: &str, remote_branch: &str, timeout: Duration) -> Result<(), PushError>;
pub async fn show_file(repo_root: &Path, rev: &str, path: &str, max_bytes: usize) -> Result<Option<String>, GitError>;  // None when absent
pub async fn list_folder(repo_root: &Path, rev: &str, folder: &str) -> Result<Vec<String>, GitError>;   // file names directly in the folder
// paths.rs
impl Paths { pub fn feedback_tmp_dir(&self) -> PathBuf; }     // <home>/tmp/feedback
// feedback/repo.rs
pub const PUSH_TIMEOUT: Duration = Duration::from_secs(120);
pub struct Placement { pub slug: String, pub folder: String, pub branch: String }
/// The first suffix 1..=99 whose folder is absent from the default branch and whose branch does not exist.
pub async fn place(repo: &Repo, title: &str, date: NaiveDate, idea: bool) -> Result<Placement, FeedbackError>;
pub enum FileSource { Text(String), Staged(PathBuf) }
pub struct FolderFile { pub name: String, pub source: FileSource }
/// Creates `placement.branch` from the default branch in `<scratch>/wt`, writes the folder, commits
/// `docs(issues): <title>`, removes the worktree. On any failure the worktree and the new branch are removed.
pub async fn commit_folder(repo: &Repo, placement: &Placement, files: &[FolderFile], title: &str, scratch: &Path) -> Result<String, FeedbackError>;
pub async fn push(repo: &Repo, local: &str, remote_branch: &str) -> Result<(), FeedbackFailure>;   // step Push, codes below
// FeedbackError gains: DefaultBranchMissing(String), GitIdentityMissing, Git { step: &'static str, status: Option<i32> }, NoFreeName, Io
```

Rules: `place` and `commit_folder` first check `branch_exists(default_branch)`; if not, `DefaultBranchMissing` (409 `default_branch_missing`, "the repository's default branch {name} does not exist locally"). `commit_folder` runs `prune_worktrees` first, moves `Staged` files into the folder (rename, falling back to copy), and writes files with mode 0644. Errors: `GitIdentityMissing` (409 `git_identity_missing`, "git has no user.name and user.email set for this repository") when git's message says so (matched on `Please tell me who you are`, never shown); `Git { step, status }` (500 `git_failed`, "git could not {step} (exit status {n})" or "... (no exit status)") with step `create the worktree` or `commit the files`; `NoFreeName` (409 `no_free_name`, "99 issue folders with this name already exist"). Push failures, matched on git's C-locale message and never shown: `push_no_remote` "git could not push: the repository has no remote named origin"; `push_auth` "git could not push to origin: authentication failed"; `push_rejected` "git could not push to origin: the remote rejected the branch"; `push_timeout` "git did not finish pushing within 120 seconds"; `push_failed` "git could not push to origin (exit status {n})".

- [ ] **Step 1: Write the failing tests** in `tests/feedback_repo.rs` (repositories from `common::init_repo`, with `git config user.name Test` and `user.email test@example.com` set in the repository; a bare `origin` made with `git init --bare`):
  - `the_folder_is_committed_on_a_new_branch_from_the_default_branch`: files `issue.md` (text) and `screenshot-1.png` (staged bytes); title "Fix the zoom" → the returned id equals `git rev-parse feedback/2026-11-26-fix-the-zoom`; its parent is `main`'s tip; `git show --name-only` lists exactly `docs/issues/2026-11-26-fix-the-zoom/issue.md` and `.../screenshot-1.png`; the subject is `docs(issues): Fix the zoom`; `git status --porcelain` in the user's checkout is empty and `git branch --show-current` is still `main`; `git worktree list` has one line; the scratch directory has no `wt` left.
  - `a_clash_gets_the_next_suffix`: with the folder already on `main` → slug `fix-the-zoom-2`; with a branch `feedback/2026-11-26-fix-the-zoom-2` as well → `-3`; with `idea: true` and a branch `idea/fix-the-zoom` → branch `idea/fix-the-zoom-2`, folder `docs/issues/2026-11-26-fix-the-zoom-2`.
  - `the_branch_starts_from_the_default_branch_as_it_is_now` (Review Focus 4): add a commit to `main` after `add_repo`-time: the new commit's parent is the new tip. `a_missing_default_branch_is_refused_and_nothing_is_created` (Review Focus 4: `git branch -m main trunk` → `Err(DefaultBranchMissing("main"))`, `git branch --list 'feedback/*'` is empty).
  - `a_failed_commit_leaves_no_branch_and_no_worktree`: unset the identity with `HOME` and `XDG_CONFIG_HOME` pointing at an empty temp dir for this test's repository (use `git config user.useConfigOnly true` and no name) → `Err(GitIdentityMissing)`; the branch does not exist; `git worktree list` has one line; a second attempt after setting the identity succeeds with the same name.
  - `a_pre_commit_hook_is_not_run`: a `pre-commit` hook that exits 1 → the commit succeeds.
  - `push_sends_the_branch_to_origin`: `origin` is the bare repository → `Ok`; `git --git-dir <bare> rev-parse feedback/..` equals the commit; a second `push` is `Ok`.
  - `push_failures_are_named_without_gits_words` (Review Focus 3, 4): no remote → code `push_no_remote`; `origin` set to `/nonexistent/LEAK-git-path` → code `push_failed` or `push_no_remote`, and in both cases the message contains neither `LEAK` nor `fatal`; a remote whose `pre-receive` hook exits 1 → `push_rejected`.
  - `origin_url_reads_the_configured_url_not_the_rewritten_one`: remote URL `https://github.com/smithagents-test-invalid/widgets.git` with `url.<bare>.insteadOf` set to it → `origin_url` is the GitHub URL, and `push` still lands in the bare repository.
  - `show_file_reads_a_file_from_a_branch_and_is_none_when_absent`; `list_folder_names_the_files_of_one_folder` (the two committed names, no subfolder entries, empty for a folder that does not exist).
- [ ] **Step 2: Run them.** `cargo test -p smithagents-server --features test-util --test feedback_repo`. Expected: compile errors.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(feedback): commit the issue folder in a temporary worktree and push it`

---

### Task 6: `GET /v1/feedback/targets`, `POST /v1/feedback` (the intent path) and the client

**Files:**
- Create: `crates/server/src/feedback/{upload.rs,github.rs}`
- Modify: `Cargo.toml:16,29` (axum features `["ws", "multipart"]`; reqwest features `["json", "multipart", "stream"]`), `crates/server/src/feedback/mod.rs`, `crates/server/src/api.rs:67-123` (routes, the route's own body limit), `crates/server/src/intents.rs:380-484` (`create_with`), `crates/server/src/bin/smithagentsd.rs` (remove leftovers at start), `crates/client/src/api.rs`, `crates/client/src/lib.rs`
- Test: `crates/server/tests/feedback_api.rs`, unit tests in `upload.rs`

**Interfaces:**
- Consumes: Tasks 1 to 5.
- Produces:

```rust
// feedback/upload.rs
#[derive(Debug, Clone, Copy)] pub struct UploadLimits { pub attachments: usize, pub image_bytes: u64, pub video_bytes: u64, pub total_bytes: u64, pub request_bytes: u64 }
impl Default for UploadLimits { /* 10, 10 MiB, 50 MiB, 100 MiB, 256 KiB */ }
impl UploadLimits { pub fn body_limit(&self) -> usize; }      // total_bytes + 1 MiB
pub fn sniff(leading: &[u8]) -> Option<FileKind>;             // PNG, JPEG, GIF87a/89a, RIFF..WEBP, ftyp at 4
pub fn check_request(request: &FeedbackRequest, limits: &UploadLimits) -> Result<String /* title */, FeedbackError>;
pub struct Staged { pub request: FeedbackRequest, pub title: String, pub files: Vec<StagedFile>, pub dir: StagingDir }
pub struct StagedFile { pub attachment: usize, pub annotated: bool, pub kind: FileKind, pub path: PathBuf, pub size: u64 }
pub struct StagingDir;                                        // removes its directory when dropped
pub async fn stage(multipart: Multipart, root: &Path, limits: &UploadLimits) -> Result<Staged, FeedbackError>;
// feedback/mod.rs
impl FeedbackService {
    pub fn limits(&self) -> UploadLimits;
    pub async fn targets(&self) -> Result<FeedbackTargets, FeedbackError>;
    pub async fn submit(&self, staged: Staged) -> Result<FeedbackOutcome, FeedbackError>;
    pub async fn remove_leftovers(&self);                     // deletes everything under feedback_tmp_dir()
    // test-util only:
    pub fn with_limits(self, limits: UploadLimits) -> Self;
    pub fn with_clock(self, clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>) -> Self;
    pub fn fail_next_intent_for_test(&self, error: IntentError);   // the next intent step fails with it, once, before create_with is called
}
// FeedbackError gains: Invalid(String) 400 "invalid"; UnsupportedFile(String) 400 "unsupported_file"; TooLarge(String) 413 "payload_too_large";
//   ToolMissing(String) 409 "tool_missing"; and Intent(IntentError), which keeps the intent service's own code and status
// intents.rs
#[derive(Debug, Clone, Default)] pub struct CreateExtras { pub base_branch: Option<String>, pub origin: Option<FeedbackOrigin>,
    pub fixed_criteria: Vec<String>, pub fixed_dependency: Option<IntentId> }
impl IntentService { pub async fn create_with(&self, req: CreateIntentRequest, extras: CreateExtras) -> Result<Intent, IntentError>; }
// client
pub struct FeedbackFile { pub part: String, pub path: PathBuf }     // part: "file-<i>" | "annotated-<i>"
impl Client {
    pub async fn feedback_targets(&self) -> Result<FeedbackTargets, ClientError>;
    pub async fn submit_feedback(&self, request: &FeedbackRequest, files: &[FeedbackFile]) -> Result<FeedbackOutcome, ClientError>;
}
```

Rules:
- **Upload.** `stage` reads parts in order with `Multipart::next_field` and `Field::chunk`, writing each file part to `<root>/<submission id>-<random>/upload/<part name>` as it arrives (never whole in memory). The first part must be `request` (JSON, at most `request_bytes`); `check_request` runs before any file is read. Allowed part names are exactly `file-<i>` for every attachment and `annotated-<i>` for every screenshot, each at most once; when the stream ends every expected part must have arrived. A part's type is `sniff` of its first 12 bytes: a screenshot's `file` and `annotated` must be PNG; an image PNG, JPEG, GIF or WebP; a video MP4. Sizes are counted while streaming and the part is abandoned as soon as it passes its limit (`image_bytes` for screenshots, annotated images and images; `video_bytes` for videos; `total_bytes` for the sum of all file parts). The staging root is created with `ensure_private_real_dir`.
- **`check_request` sentences** (`Invalid`): "the main idea is empty"; "the main idea must be at most 20000 characters"; "at most 10 attachments can be sent"; "attachment {i} has more than 20 boxes"; "attachment {i}: a comment is longer than 500 characters"; "attachment {i}: a box is outside the image" (a field not finite or outside 0..=1, or `x + w` or `y + h` above 1.0001); "attachment {i}: only a screenshot can have boxes"; "the app version is not valid" and "the platform is not valid" (each must match `[A-Za-z0-9._+-]{1,32}`); "choose at least one part of the idea package"; "an idea package needs an intent, not a GitHub issue". Part errors: "the request part must come first"; "unexpected part {name}" (the name cleaned and cut to 40 characters); "part {name} was sent twice"; "attachment {i} was not sent"; `UnsupportedFile`: "attachment {i} is not a png, jpg, gif, webp or mp4 file", "screenshot {i} is not a png file"; `TooLarge`: "attachment {i} is larger than 10 MB", "attachment {i} is larger than 50 MB", "the submission is larger than 100 MB", "the request part is larger than 256 KiB". A size is printed from the limit in use: `{n} MB` when it is a whole number of MiB, else `{n} bytes`.
- **Handler.** `POST /v1/feedback` takes `State` and `Multipart`; the route carries `.layer(DefaultBodyLimit::max(state.feedback.limits().body_limit()))`; every other route keeps 1 MiB. A `MultipartError` whose `status()` is 413 is `TooLarge("the submission is larger than 100 MB")`; any other is `Invalid("the upload is not valid multipart form data")`. Staging runs in the handler (a dropped request drops `StagingDir`, which removes the files); the rest runs in `detached(async move { svc.submit(staged).await })`. The answer is 200 with a `FeedbackOutcome`.
- **`submit`**, under one service-wide `tokio::sync::Mutex` (one submission at a time): (1) if a `Submission` with this id exists, the uploaded files are dropped and the stored submission is continued as in step 5; (2) the repository must exist (404 `repo_not_found`); `demo_video` needs `agent-browser` ready (409 `tool_missing`, "agent-browser is not installed"); (3) `place`, build `FolderFile`s (`issue.md` from `render_issue`, then each attachment's files under the names of `file_names`), `commit_folder`; (4) `insert_submission`; (5) for `SendAs::Intent`, when `intent_id` is `None`: `intents.create_with(CreateIntentRequest { repo_id: Some(repo), kind: Some(request.kind.intent_kind()), seed: SeedInput::Text(seed_text(.., MAX_SEED_BYTES)) }, CreateExtras { base_branch: Some(branch), origin: Some(FeedbackOrigin { folder, branch, commit, idea: None }), ..Default::default() })`, then `save_submission`; a failure becomes `failed: Some(FeedbackFailure { step: CreateIntent, code: <the IntentError's code>, message: <its text> })` and the answer is still 200. `created` in `issue.md` and the date in the names come from the service clock. Until Task 10, `idea` is checked (the two sentences below and the `agent-browser` rule) and otherwise ignored. `create_with` stores `base_branch` and `origin` on the new intent; `create(req)` is `create_with(req, CreateExtras::default())`. (`fixed_criteria` and `fixed_dependency` are used from Task 10 on.)
- **Logs** name the submission id, the step and an exit status; never a title, a comment, a file name, a URL or command output.
- **`targets`:** one `FeedbackTarget` per repository in `list_repos` order; `has_github_remote` is `github_repo(origin_url(..))` being `Some`; `is_public` is `None` in this task (Task 8 fills it); `meta_repo_id` is `effective_meta_repo`; `gh` and `agent_browser` come from `PluginManager::tool_state` (`Ready` → `ready: true`; otherwise `ready: false` with reason `not installed`, `could not read its version` or `too old`). Task 8 adds the signed-in check for `gh`.
- **Client.** `submit_feedback` builds a `reqwest::multipart::Form`: `Part::text(json).mime_str("application/json")` named `request` first, then `Part::file(path)` for each file, named by `part` and with `.file_name(part)` (the local file name never leaves the machine). No request timeout.

- [ ] **Step 1: Verify first: the per-route body limit.** Write `the_feedback_route_has_its_own_body_limit`: with default limits, a 2 MiB body on `POST /v1/feedback` gets past the body limit (it fails later with 400 `invalid`, not 413), and a 2 MiB JSON body on `POST /v1/repos` still answers 413. If the route-level `DefaultBodyLimit` does not override the router's, apply `DefaultBodyLimit::max(1 MiB)` to a sub-router holding every other route and merge the feedback route with its own limit; the test stays the same.
- [ ] **Step 2: Write the remaining failing tests** in `tests/feedback_api.rs`. Helpers in `tests/common/feedback.rs`: `multipart(parts: &[(&str, &[u8])]) -> (String, Vec<u8>)` (content type and body), `png(bytes: usize)`, `jpeg()`, `mp4(bytes: usize)`, `request_json(repo, send_as) -> serde_json::Value`. The fixture uses `with_clock(2026-11-26T14:15:00Z)`, a repository with a git identity, and both fake tools.
  - `default_limits_are_the_designs` (unit): 10; `10 * 1024 * 1024`; `50 * 1024 * 1024`; `100 * 1024 * 1024`; `256 * 1024`; `body_limit()` is the total plus `1024 * 1024`.
  - `targets_list_repositories_tools_and_the_meta_repository`: two repositories, one with `origin` `https://github.com/smithagents-test-invalid/widgets.git`, one without a remote → `has_github_remote` true and false; `gh.ready` and `agent_browser.ready` true; `meta_repo_id` null until set; with only `gh` in the fake bin, `agent_browser` is `{"ready":false,"reason":"not installed"}`. `targets_never_show_a_remote_url` (Review Focus 3: `origin` `https://user:LEAK-token@github.com/o/r.git` → the response text has no `LEAK` and no `user:`).
  - `an_intent_submission_commits_the_folder_and_creates_the_intent`: one screenshot (two boxes), one JPEG image; main idea `"Fix the zoom\n\nThe page is too big."`, kind `bug` → 200; `branch == "feedback/2026-11-26-fix-the-zoom"`, `folder == "docs/issues/2026-11-26-fix-the-zoom"`, `commit` equals `git rev-parse` of the branch, `pushed == false`, `issue_url == null`, `failed == null`; `git show <branch>:<folder>/issue.md` equals `render_issue`'s text for the same input; the tree has `issue.md`, `screenshot-1.png`, `screenshot-1.annotated.png`, `image-1.jpg` with the uploaded bytes; `intent.content.kind == "bug"`, `intent.base_branch == branch`, `intent.origin == {folder, branch, commit}`, `intent.seed.text` ends with the `Feedback files:` line; `GET /v1/intents/{id}` returns it; the user's checkout is clean and on `main`; `<home>/tmp/feedback` is empty; `gh` was not called (`calls()` is empty).
  - `a_submission_without_attachments_is_allowed`.
  - `hostile_and_oversized_uploads_are_refused_and_leave_nothing` (Review Focus 1; limits `image_bytes: 1024, video_bytes: 4096, total_bytes: 6000`). Each row asserts the status, the code, the exact message, and afterwards: no `feedback/*` branch, `get_submission` is `None`, `<home>/tmp/feedback` is empty. Rows: text bytes as `file-1` of a screenshot → 400 `unsupported_file` "screenshot 1 is not a png file"; a JPEG as `annotated-1` → the same; text as an image → 400 `unsupported_file` "attachment 1 is not a png, jpg, gif, webp or mp4 file"; a part named `../../x` → 400 `invalid` "unexpected part ../../x"; `file-1` twice → "part file-1 was sent twice"; `file-1` before `request` → "the request part must come first"; a screenshot with no `annotated-1` → "attachment 1 was not sent"; 11 attachments → "at most 10 attachments can be sent"; a box with `x: 2.0` → "attachment 1: a box is outside the image"; 21 boxes; a 501-character comment; a blank main idea → "the main idea is empty"; `platform: "mac os; rm"` → "the platform is not valid"; a PNG of 1025 bytes → 413 `payload_too_large` "attachment 1 is larger than 1024 bytes"; an MP4 of 4097 bytes → "attachment 1 is larger than 4096 bytes"; three parts summing over 6000 → "the submission is larger than 6000 bytes"; a body that is not multipart → 400 `invalid` "the upload is not valid multipart form data"; an unknown `repo_id` → 404 `repo_not_found`.
  - `a_name_from_the_client_is_never_a_path` (Review Focus 1): `original_name` `"../../../outside.png"` and a part `filename` of `"/etc/passwd"` → 200; the committed tree has only the daemon's names; no file named `outside.png` exists anywhere under the temp root or the repository.
  - `the_same_submission_twice_makes_one_branch_and_one_intent` (Review Focus 5): post the same body twice in sequence → the two answers are equal as JSON (same branch, commit, intent id); `git branch --list 'feedback/*'` has one line; `list_intents` has one. Then two posts of a new id at once (`tokio::join!`) → equal answers, one branch, one intent.
  - `a_failed_intent_step_keeps_the_commit_and_says_so` (Review Focus 2): call `fail_next_intent_for_test(IntentError::Invalid("boom".into()))`, then post → 200 with `branch`, `commit` set, `intent == null`, `failed == {"step":"create_intent","code":"invalid","message":"boom"}`; posting the same submission again (hook cleared) returns the same branch and commit with the intent set, and the branch still has one commit above `main`.
  - `a_renamed_default_branch_is_refused` (Review Focus 4): 409 `default_branch_missing`, "the repository's default branch main does not exist locally", nothing created. `a_repository_without_a_remote_takes_the_intent_path` (Review Focus 4: 200).
  - `a_demo_video_needs_agent_browser`: `idea.demo_video: true` with no `agent-browser` in the fake bin → 409 `tool_missing` "agent-browser is not installed", nothing created.
  - `leftovers_are_removed_at_start`: create `<home>/tmp/feedback/x/upload/file-1`, call `remove_leftovers` → the directory is empty.
  - `the_routes_need_the_token` (401 for both).
- [ ] **Step 3: Run them.** `cargo test -p smithagents-server --features test-util --test feedback_api`. Expected: compile errors, then failures.
- [ ] **Step 4: Implement.** `git diff Cargo.lock` must show `multer` added and no version changed.
- [ ] **Step 5: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 6: Commit.** `feat(feedback): POST /v1/feedback commits the issue folder and creates an intent; GET /v1/feedback/targets`

---

### Task 7: Send starts from the intent's base branch; the brief points at the evidence

**Files:**
- Modify: `crates/server/src/service.rs:57-63,210-231`, `crates/server/src/intents.rs:912-941`, `crates/server/src/brief.rs:56-106`
- Test: `crates/server/tests/{intents.rs,brief.rs}`, unit tests in `service.rs`

**Interfaces:**
- Consumes: Task 2 (`Intent.base_branch`, `Intent.origin`), Task 5 (`git::branch_exists`).
- Produces:

```rust
pub struct CreateAgentOptions { /* existing */ pub base: Option<String> }   // None: the repository's default branch
pub const EVIDENCE_TITLE: &str = "Evidence";                                 // brief.rs
```

Rules: `create_agent_with` passes `opts.base` (else `repo.default_branch`) to `git::create_worktree`. `launch_agent` sets `base: intent.base_branch.clone()`; when the base branch is set and `branch_exists` is false, the start fails with `IntentError::Invalid("the base branch {name} no longer exists, so the intent cannot start from it")` before any worktree is made (it lands in `last_start_error` like every start error). The agent's own branch stays `intent/<slug>`. `brief::compose` adds, after "Current behavior", for an intent whose `origin` is set and has no `idea`: `## Evidence`, blank, ``The feedback files are in `{folder}/` on this branch. Read `issue.md` and look at its images before you start. Leave the folder in place: the pull request carries it.``

- [ ] **Step 1: Write the failing tests.**
  - `service.rs` unit: `an_agent_can_start_from_another_branch` (a branch `feedback/x` with a commit that adds `docs/issues/x/issue.md`; `create_agent_with(.., CreateAgentOptions { base: Some("feedback/x".into()), branch: Some("intent/y".into()), ..Default::default() })` → the worktree has the file and `git merge-base --is-ancestor feedback/x intent/y` succeeds; without `base`, the file is absent).
  - `tests/intents.rs`: `send_starts_the_agent_from_the_base_branch` (an intent created with `create_with(.., CreateExtras { base_branch: Some("feedback/x".into()), .. })`, polished with the fake harness and reviewed to Complete; `send` → Sent; the agent's `branch` is `intent/<slug>` and its worktree contains `docs/issues/x/issue.md`). `a_missing_base_branch_is_a_start_error` (delete the branch before `send` → `Err` with code `invalid`; the intent is still Complete with `last_start_error == "could not start the agent: the base branch feedback/x no longer exists, so the intent cannot start from it"`; no agent and no `intent/*` branch exist). `an_intent_without_a_base_branch_starts_from_the_default_branch` (the existing behavior, asserted).
  - `tests/brief.rs`: `a_feedback_intent_points_at_its_folder` (the first message contains the Evidence section with the folder; an intent without `origin` has no `## Evidence`).
- [ ] **Step 2: Run them.** `cargo test -p smithagents-server --features test-util --test intents --test brief service`. Expected: compile errors.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(server): an intent's agent starts from its base branch and is told where the evidence is`

---

### Task 8: The GitHub issue path: readiness, push, `gh issue create`, labels, partial failure and Retry

**Files:**
- Modify: `crates/server/src/feedback/{mod.rs,github.rs}`, `crates/server/src/api.rs` (one route), `crates/client/src/api.rs`
- Create: `crates/server/tests/feedback_github.rs`

**Interfaces:**
- Consumes: Task 1 (`run_tool`, `FakeTools`), Task 4 (`github_repo`, `issue_body`, `permalink_base`, `marker`), Task 5 (`push`, `show_file`), Task 6.
- Produces:

```rust
// feedback/github.rs
pub const GH_TIMEOUT: Duration = Duration::from_secs(60);
pub const VISIBILITY_TIMEOUT: Duration = Duration::from_secs(5);
pub const GH_ENV: [(&str, &str); 3] = [("GH_PROMPT_DISABLED", "1"), ("GH_NO_UPDATE_NOTIFIER", "1"), ("NO_COLOR", "1")];
pub async fn gh_readiness(plugins: &PluginManager) -> ToolReadiness;     // not installed | "not signed in" (gh auth status failed) | ready
pub async fn is_public(plugins: &PluginManager, repo: &Repo) -> Option<bool>;   // gh repo view --json visibility --jq .visibility
pub fn label_for(kind: FeedbackKind) -> &'static str;                    // "enhancement" | "enhancement" | "bug"
pub async fn create_issue(plugins: &PluginManager, repo: &Repo, title: &str, body_file: &Path, label: &str, marker: &str) -> Result<String, FeedbackFailure>;
// feedback/mod.rs
impl FeedbackService { pub async fn retry(&self, id: SubmissionId) -> Result<FeedbackOutcome, FeedbackError>;
    pub fn with_gh_timeout_for_test(self, timeout: Duration) -> Self; }   // test-util only; replaces GH_TIMEOUT
// FeedbackError gains: SubmissionNotFound 404 "submission_not_found" ("no such submission"); NoGithubRemote 409 "no_github_remote"
//   ("the repository's origin is not a GitHub repository"); GhNotReady(String) 409 "gh_not_ready"
// route: POST /v1/feedback/{submission_id}/retry (no body) -> 200 FeedbackOutcome
impl Client { pub async fn retry_feedback(&self, id: SubmissionId) -> Result<FeedbackOutcome, ClientError>; }
```

Rules:
- **Before anything is written,** a `github_issue` submission needs `github_repo(origin_url)` (else `NoGithubRemote`) and `gh_readiness().ready` (else `GhNotReady("GitHub CLI (gh) is not installed")` or `GhNotReady("GitHub CLI (gh) is not signed in; run: gh auth login")`).
- **After the commit,** in order, each skipped when its result is already stored: (a) `push(repo, branch, branch)`, then `pushed = true` saved; (b) the issue. A failure stops the sequence and is returned as `failed` with the branch and commit; nothing is undone.
- **The issue.** The body is `issue_body(show_file(commit, "<folder>/issue.md"), <the folder's file names>, permalink_base(owner, repo, commit, folder), Some(marker(id)))`, written to `<scratch>/body.md`. First `gh issue list --state=all --search=<submission uuid> in:body --json=url --jq=.[0].url // ""`: a first line that is an issue URL is used and nothing is created. Then the label: `gh label list --search=<label> --json=name --jq=any(.[]; .name == "<label>")`; only a first line `true` adds `--label=<label>`; a failure of this check means no label. Then `gh issue create --title=<title> --body-file=<path> [--label=<label>]`. All three run through `plugins.run_tool("feedback", "gh", ToolRun { cwd: &repo.path, env: &GH_ENV, timeout: GH_TIMEOUT, .. })`. The answer must be `https://github.com/<owner>/<repo>/issues/<digits>` for the same owner and repository, else `gh_bad_answer`.
- **Failure codes and sentences** (step `create_issue`): `gh_not_ready` (the two sentences above); `gh_failed` "gh could not create the issue (exit status {n})"; `gh_timeout` "gh did not finish within 60 seconds"; `gh_bad_answer` "gh did not print the issue's address".
- **`retry`** loads the submission (404 when unknown) and runs the unfinished steps of its `send_as` (the intent step of Task 6, or push and issue here) under the same mutex; a complete submission is returned as it is.
- **`targets`** now fills `gh` from `gh_readiness` and, for repositories with a GitHub remote while `gh` is ready, `is_public` from `is_public` (first line `PUBLIC` → `Some(true)`; `PRIVATE` or `INTERNAL` → `Some(false)`; anything else or a failure → `None`), all repositories checked at once, each answer cached in the service for 10 minutes.

- [ ] **Step 1: Write the failing tests** in `tests/feedback_github.rs` (the fixture of Task 6; the repository's `origin` is `https://github.com/smithagents-test-invalid/widgets.git` with `url.<bare>.insteadOf` pointing at a bare repository in the temp dir):
  - `an_issue_submission_pushes_and_creates_the_issue`: kind `new_feature`, one screenshot → 200, `pushed == true`, `issue_url == "https://github.com/smithagents-test-invalid/widgets/issues/7"`, `intent == null`, `failed == null`; the bare repository has the branch at `commit`; `calls()` is, in order, `auth status`, a line starting with `issue list`, a line starting with `label list --search=enhancement`, and a line starting with `issue create --title=Fix the zoom --body-file=` and ending with `--label=enhancement`; `issue-body.md` has no front matter, contains `https://github.com/smithagents-test-invalid/widgets/blob/<commit>/docs/issues/2026-11-26-fix-the-zoom/screenshot-1.annotated.png?raw=true` and ends with the marker of the submission id; `list_intents` is empty.
  - `labels_follow_the_kind_and_only_when_they_exist`: `bug` → `--label=bug`; `enhancement` → `--label=enhancement`; with the control file `label` holding `false` → the `issue create` line has no `--label`.
  - `a_title_that_looks_like_a_flag_stays_a_title`: main idea `--web` → the call line is `issue create --title=--web --body-file=…`.
  - `the_issue_path_is_refused_before_anything_is_written` (Review Focus 4): a repository without `origin` → 409 `no_github_remote`; `origin` `https://gitlab.com/o/r.git` → the same; `signed-out` present → 409 `gh_not_ready` "GitHub CLI (gh) is not signed in; run: gh auth login"; no `gh` in the bin → "GitHub CLI (gh) is not installed". In each case: no branch, no submission row, the bare repository has no branch.
  - `a_failed_push_keeps_the_commit_and_retry_pushes_and_creates` (Review Focus 2): the bare repository's `pre-receive` hook exits 1 → 200 with `branch` and `commit`, `pushed == false`, `failed == {"step":"push","code":"push_rejected","message":"git could not push to origin: the remote rejected the branch"}`, and `calls()` has no `issue create`. Remove the hook; `POST /v1/feedback/{id}/retry` → `pushed == true`, the issue URL set, `failed == null`; the branch still has exactly one commit above `main`.
  - `a_failed_gh_keeps_the_push_and_retry_only_creates_the_issue` (Review Focus 2): `fail` = `issue create` → `pushed == true`, `failed.step == "create_issue"`, `failed.code == "gh_failed"`, message "gh could not create the issue (exit status 1)". Record the bare repository's reflog size; clear `fail`; retry → the URL is set and `calls()` gained exactly one `issue create`; a second retry returns the same outcome and adds no `issue create`.
  - `a_hanging_gh_times_out` (a real process ignores tokio's paused clock, so the fixture uses `with_gh_timeout_for_test(Duration::from_millis(300))`; `hang` = `issue create`) → `failed.code == "gh_timeout"`, and no `gh` process of the test is left running.
  - `an_issue_that_already_exists_is_not_created_again` (Review Focus 5): `fail` = `issue create` for the first call; then set `existing` to `https://github.com/smithagents-test-invalid/widgets/issues/7` and retry → that URL, and no second `issue create` line.
  - `a_wrong_answer_from_gh_is_refused`: a fake that prints `https://example.com/x` for `issue create` (control file `issue-url`) → `gh_bad_answer`.
  - `nothing_gh_or_git_printed_reaches_a_response_or_an_event` (Review Focus 3): with `fail` = `issue create`, and separately with `origin` rewritten to `/nonexistent/LEAK-git-path`, collect every response body and every event published on the bus as text: none contains `LEAK`; `env.log` has no line starting with `SMITHAGENTS_` although the test process has `SMITHAGENTS_SECRET_FEEDBACK__X=LEAK-secret` set.
  - `retry_of_an_unknown_submission_is_404`. `retry_finishes_a_failed_intent_step` (the hook of Task 6, then retry → the intent is set).
  - `targets_report_visibility_and_sign_in`: `visibility` holding `PUBLIC` → `is_public == true`; `PRIVATE` → `false`; `fail` = `repo view` → `null`; a repository without a GitHub remote → `null` and no `repo view` call for it; `signed-out` → `gh == {"ready":false,"reason":"not signed in"}` and no `repo view` call at all; a second `targets` call within 10 minutes adds no `repo view` line.
- [ ] **Step 2: Run them.** `cargo test -p smithagents-server --features test-util --test feedback_github`. Expected: compile errors.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(feedback): send feedback as a GitHub issue with gh; partial failures can be retried`

---

### Task 9: The Submit screen in the app

**Files:**
- Create: `crates/app/src/feedback/submit_vm.rs`, `crates/app/src/views/feedback/submit.rs`, `crates/app/tests/{feedback_submit_vm.rs,feedback_submit.rs}`
- Modify: `crates/app/src/feedback/{mod.rs,text.rs,state.rs}`, `crates/app/src/views/feedback/mod.rs`, `crates/app/src/{link.rs,shell.rs,shell_state.rs}`, `crates/app/tests/common/feedback_submit.rs`, `crates/app/tests/feedback_pane.rs` (`submit_is_disabled_and_says_why` is replaced)

**Interfaces:**
- Consumes: Tasks 2, 3, 6, 8 (client methods); the capture plan's `Feedback`, `FeedbackPane`, `DraftStore`; the intent tool's `IntentsPane::open_screen`, `Screen::Detail`, `Shell::show_rail_entry`, `intent_vm::default_repo`.
- Produces:

```rust
// link.rs
pub trait FeedbackCommands { /* Task 3 */
    fn feedback_targets(&self) -> LocalBoxFuture<'static, Result<FeedbackTargets, LinkError>>;
    fn submit_feedback(&self, request: FeedbackRequest, files: Vec<FeedbackFile>) -> LocalBoxFuture<'static, Result<FeedbackOutcome, LinkError>>;
    fn retry_feedback(&self, id: SubmissionId) -> LocalBoxFuture<'static, Result<FeedbackOutcome, LinkError>>;
}   // DaemonLink sends Action::IntentUpserted(intent) for an outcome that carries one, before returning
// shell_state.rs / shell.rs
pub enum ShellEvent { /* existing */ OpenIntent(IntentId) }
impl ShellState { pub fn open_intent(&mut self, id: IntentId, cx: &mut Context<Self>); }   // the shell shows RailEntry::Intents and opens Screen::Detail(id)
// feedback/state.rs
pub const SUBMIT_TIMEOUT: Duration = Duration::from_secs(600);
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pending { pub submission_id: SubmissionId, pub outcome: Option<FeedbackOutcome> }   // <draft dir>/submission.json
impl Feedback {
    pub fn set_main_idea(&mut self, text: String, cx: &mut Context<Self>);     // saved to draft.json
    pub fn set_kind(&mut self, kind: FeedbackKind, cx: &mut Context<Self>);    // Draft.kind = the wire name
    pub fn kind(&self) -> FeedbackKind;                                        // Draft.kind parsed; NewFeature when absent or unknown
    pub fn pending(&self) -> Option<&Pending>;
    pub fn begin_submission(&mut self, cx: &mut Context<Self>) -> SubmissionId; // the pending id, or a new one written to submission.json
    pub fn record_outcome(&mut self, outcome: FeedbackOutcome, cx: &mut Context<Self>);
    pub fn forget_submission(&mut self, cx: &mut Context<Self>);
    pub fn upload(&self) -> Option<(Vec<FeedbackAttachment>, Vec<FeedbackFile>)>;   // None while a screenshot is not rendered or the draft is in memory only
    pub fn result(&self) -> Option<&str>;                                      // the last success sentence; cleared by the next change to the draft
    pub fn finish_submission(&mut self, result: String, cx: &mut Context<Self>);    // discard_all, then result
}
// feedback/submit_vm.rs (pure)
pub fn attachments_phrase(draft: &Draft) -> String;
pub fn issue_reason(target: Option<&FeedbackTarget>, gh: &ToolReadiness) -> Option<&'static str>;   // None: the choice is available
pub fn public_warning(send_as: SendAs, target: Option<&FeedbackTarget>) -> Option<&'static str>;
pub fn summary(send_as: SendAs, idea: Option<IdeaOptions>, draft: &Draft) -> String;
pub fn confirm_label(send_as: SendAs) -> &'static str;                         // "Create intent" | "Create issue"
pub fn starting_repo(repos: &[Repo], last_used: Option<RepoId>, meta: Option<RepoId>) -> Option<RepoId>;
pub fn can_confirm(main_idea: &str, repo: Option<RepoId>, send_as: SendAs, issue_reason: Option<&str>, idea: Option<IdeaOptions>) -> bool;
pub fn submit_error_text(error: &LinkError) -> String;                         // intent_error_text's rules; unknown codes show the daemon's sentence
pub fn result_text(outcome: &FeedbackOutcome) -> Option<String>;               // None unless complete
// views/feedback/mod.rs and submit.rs
pub enum FeedbackScreen { Draft, Submit }
impl FeedbackPane { pub fn screen(&self) -> &FeedbackScreen; }
pub const FEEDBACK_RESULT_ID: &str = "feedback-result";             // Role::Status, on the Draft screen
pub const SUBMIT_BACK_ID: &str = "feedback-submit-back";            pub const SUBMIT_IDEA_ID: &str = "feedback-main-idea";
pub fn submit_kind_id(kind: FeedbackKind) -> ElementId;             // "feedback-kind-<wire name>"; labels New feature, Enhancement, Bug
pub const SUBMIT_REPO_ID: &str = "feedback-repo";                   pub fn submit_repo_item_id(id: RepoId) -> ElementId;   // "feedback-repo-<uuid>"
pub const SEND_INTENT_ID: &str = "feedback-send-intent";            pub const SEND_ISSUE_ID: &str = "feedback-send-issue";
pub const SEND_ISSUE_REASON_ID: &str = "feedback-send-issue-reason";
pub const SUBMIT_SUMMARY_ID: &str = "feedback-submit-summary";      pub const PUBLIC_WARNING_ID: &str = "feedback-public-warning";
pub const SUBMIT_ERROR_ID: &str = "feedback-submit-error";          // Role::Status
pub const SUBMIT_CONFIRM_ID: &str = "feedback-submit-confirm";      pub const SUBMIT_RETRY_ID: &str = "feedback-submit-retry";
pub const SUBMIT_START_OVER_ID: &str = "feedback-submit-start-over";
// tests/common/feedback_submit.rs
pub enum FeedbackCall { SetSettings(..), Targets, Submit(FeedbackRequest, Vec<FeedbackFile>), Retry(SubmissionId) }
pub fn targets(repos: &[(RepoId, bool, Option<bool>)], gh: bool, agent_browser: bool) -> FeedbackTargets;
pub fn outcome_with_intent(id: SubmissionId, intent: Intent) -> FeedbackOutcome;   // branch "feedback/2026-11-26-fix-the-zoom", commit "0123abc4567"
```

Behavior:
- **Pane.** "Submit…" is enabled when the draft has an attachment, `Feedback::busy()` is false and `state.feedback_commands()` is `Some`; otherwise disabled with the tooltip `SUBMIT_EMPTY_TOOLTIP` (empty draft) or `NOT_CONNECTED_TEXT`. It opens `FeedbackScreen::Submit` and requests `feedback_targets()`. The Draft screen shows `Feedback::result()` in `FEEDBACK_RESULT_ID` above the cards.
- **Screen**, top to bottom, in one scrolling column: "Back"; the attachments read-only (thumbnail, the card title, the numbered comments; no remove button, not clickable); `MAIN_IDEA_LABEL` with a `Textarea` (`TextareaState::new(window, cx).auto_grow(3, 10)`, accessible name "Main idea", id `SUBMIT_IDEA_ID`, starting from `draft.main_idea`, saved with `set_main_idea` on every change) and `MAIN_IDEA_HINT`; Kind as three buttons, the chosen one `selected` (starting from `Feedback::kind()`); Repository: a dropdown button (research D4) labelled with `repo_title`, starting on `starting_repo(repos, last_repo_id, effective meta repository)`, reading `NEED_REPO_TEXT` and opening Settings when there is no repository; "Send it as": two `Radio`s (`SEND_AS_INTENT`, `SEND_AS_ISSUE`), the second disabled with `issue_reason` shown in `SEND_ISSUE_REASON_ID` (while the targets are loading it is disabled with no reason); `summary` in `SUBMIT_SUMMARY_ID`; `public_warning` in `PUBLIC_WARNING_ID` (the `status-warn` token); the error line; the confirm button (`confirm_label`, primary), disabled unless `can_confirm`.
- **`issue_reason`:** no target or `!has_github_remote` → `NO_REMOTE_REASON`; `gh.reason == "not signed in"` → `GH_SIGNED_OUT_REASON`; `!gh.ready` → `GH_MISSING_REASON`; else `None`. When the chosen repository changes and the issue choice becomes unavailable, the choice returns to intent. **`public_warning`:** only for `GithubIssue`: `is_public == Some(true)` → `PUBLIC_WARNING`; `None` → `MAYBE_PUBLIC_WARNING`; `Some(false)` → nothing.
- **Confirm.** One command at a time: the button shows loading and every control on the screen is disabled. It calls `begin_submission`, builds `FeedbackRequest { submission_id, main_idea, kind, repo_id, send_as, idea: None, app_version: smithagents_core::VERSION, platform: std::env::consts::OS, attachments }` and the files from `upload()` (part `file-<i>` = `store.path_of(stored_name)`, `annotated-<i>` = `path_of(annotated_name(..))`), and waits with `answered(command, timer(SUBMIT_TIMEOUT))`.
- **Answers.** Complete (`failed == None`): `set_last_repo_id(repo)`, `finish_submission(result_text)` (the draft and `submission.json` are deleted), the screen returns to Draft; for an intent, `state.open_intent(intent.id)`. Partial (`failed` set): `record_outcome`; the screen shows `partial_text(branch, first 7 characters of the commit, failed.message)` in the error line and, in place of the confirm button, "Retry" and "Start over"; every other control stays disabled. An error answer: the error line shows `submit_error_text`, the draft and the pending id stay; a timeout shows `DAEMON_SILENT_TEXT`.
- **Pending.** Opening the screen while `pending().outcome` holds a failure shows the partial state at once. "Retry" calls `retry_feedback(id)` with the same answer handling; an API error `submission_not_found` forgets the pending id and shows `NOTHING_CREATED_TEXT`. "Start over" forgets the pending id (the branch stays) and re-enables the form.

- [ ] **Step 1: Verify first: `Radio` in headless tests.** In `feedback_submit.rs`, open the screen and check that `find(SEND_ISSUE_ID)` reports `checked()` and `disabled()` and that `window.click(SEND_ISSUE_ID, cx)` selects it. If `checked()` is `None`, draw the two choices as outline `Button`s with `.selected(bool)` (as the Kind row does) and assert `selected()`; the ids and labels stay.
- [ ] **Step 2: Write the failing tests.**
  - `tests/feedback_submit_vm.rs`: `attachment_phrases` ("1 screenshot", "2 screenshots", "1 file", "2 screenshots and 1 file", "3 files"). `the_issue_choice_says_why_it_is_unavailable` (the three reasons, and `None`). `the_public_warning_depends_on_what_is_known` (the three cases for `GithubIssue`; always `None` for `Intent`). `summaries` (`summary_intent` and `summary_issue` exactly as in the table). `the_starting_repository` (last used while it exists; else the meta repository; else `None`). `confirm_needs_a_main_idea_and_a_repository` (blank or whitespace-only text → false; no repository → false; issue with a reason → false). `result_sentences` (`Created the intent “Fix the zoom”. Files: feedback/2026-11-26-fix-the-zoom (0123abc).`; `Created https://github.com/o/r/issues/7. Files: feedback/2026-11-26-fix-the-zoom (0123abc).`; a partial outcome → `None`). `error_sentences` (`default_branch_missing` with the daemon's message → its first letter capitalized; `NotConnected` → `NOT_CONNECTED_TEXT`).
  - `tests/feedback_submit.rs` (shell from `FeedbackSetup` plus `FakeFeedback`, `FakeIntents`, two repositories; the draft holds one captured screenshot with one box "the toolbar", made through `Feedback::capture` and `finish_annotating`):
    - `submit_is_enabled_only_with_an_attachment`: empty draft → disabled, description `Add a screenshot or a file first`; with the screenshot → enabled; a click opens the Submit screen and records `FeedbackCall::Targets`.
    - `the_screen_shows_the_draft_read_only`: the card title "Screenshot 1 · 1 comment" and "the toolbar" are found; no `card_remove_id`; the three kind buttons have the labels "New feature", "Enhancement", "Bug"; the confirm button reads "Create intent" and is disabled.
    - `the_main_idea_and_kind_are_kept_in_the_draft`: type "Fix the zoom", choose Bug, press Back, reopen → the text and the kind are there; `draft.json` on disk has `"main_idea":"Fix the zoom"` and `"kind":"bug"`.
    - `the_repository_starts_on_the_last_used_then_the_meta_repository`.
    - `the_issue_choice_is_disabled_with_its_reason`: targets without a GitHub remote → `SEND_ISSUE_ID` disabled, `SEND_ISSUE_REASON_ID` reads "This repository has no GitHub remote."; `gh` not ready → "Install the GitHub CLI (gh) to create issues."; choosing the other repository (with a remote, `gh` ready) enables it.
    - `choosing_an_issue_changes_the_button_the_summary_and_the_warning`: confirm reads "Create issue"; the summary reads "Creates the issue folder with 1 screenshot, pushes the branch to origin and opens a GitHub issue."; `is_public: Some(true)` → `PUBLIC_WARNING`; `None` → `MAYBE_PUBLIC_WARNING`; `Some(false)` → no `PUBLIC_WARNING_ID`.
    - `create_intent_sends_the_draft_and_opens_the_intent`: type "Fix the zoom", click confirm → one `FeedbackCall::Submit` whose request has `send_as: Intent`, `kind: NewFeature`, `idea: None`, `platform` equal to `std::env::consts::OS`, one attachment `{ kind: Screenshot, boxes: [one box, comment "the toolbar"] }`, and whose files are `file-1` → `<draft dir>/screenshot-1.png` and `annotated-1` → `<draft dir>/screenshot-1.annotated.png`; before the answer every control is disabled and the button shows loading. Answer with `outcome_with_intent` → the draft directory is gone, the Intents pane is shown with `Screen::Detail(intent.id)`, `last_repo_id` is the repository; back on the Feedback pane `FEEDBACK_RESULT_ID` reads `Created the intent “Fix the zoom”. Files: feedback/2026-11-26-fix-the-zoom (0123abc).` and the pane shows `EMPTY_TEXT`.
    - `a_second_press_sends_nothing` (Review Focus 5): two clicks before the answer → one `Submit` call.
    - `a_refusal_keeps_the_draft_and_shows_the_daemons_sentence`: queue an API error 409 `default_branch_missing` "the repository's default branch main does not exist locally" → `SUBMIT_ERROR_ID` reads "The repository's default branch main does not exist locally"; the card is still there; a second confirm sends the same `submission_id`.
    - `a_partial_failure_offers_retry_and_start_over` (Review Focus 2): answer with an outcome whose `failed` is `{ step: Push, code: "push_rejected", message: "git could not push to origin: the remote rejected the branch" }` → the error line reads `The files are committed on feedback/2026-11-26-fix-the-zoom (0123abc), but: git could not push to origin: the remote rejected the branch`; "Retry" and "Start over" are found and the confirm button is not; the draft directory still has its files and `submission.json`. Click Retry → one `FeedbackCall::Retry` with the same id; answer complete → the result line shows and the draft is gone.
    - `a_pending_submission_survives_a_restart`: after the partial failure, open a second shell on the same home and press "Submit…" → the partial state shows without a new `Submit` call.
    - `retry_of_a_submission_the_daemon_never_saw_starts_over`: Retry answered with 404 `submission_not_found` → the line reads `Nothing was created. Submit again.`, the form is enabled, and the next confirm sends a different `submission_id`.
    - `a_silent_daemon_frees_the_screen`: no answer; advance the clock past `SUBMIT_TIMEOUT` → "The daemon did not answer", the controls are enabled, the pending id is kept.
- [ ] **Step 3: Run them.** `cargo test -p smithagents-app --test feedback_submit_vm --test feedback_submit --test feedback_pane`. Expected: compile errors, then failures.
- [ ] **Step 4: Implement.**
- [ ] **Step 5: Run the tests.** Expected: PASS, with `feedback_flow` and the intent tool's suites green. Gate.
- [ ] **Step 6: Commit.** `feat(feedback): the Submit screen creates an intent or a GitHub issue from the draft`

---

### Task 10: The idea package: an idea intent with fixed criteria, its brief, and the checkboxes

**Files:**
- Create: `crates/server/src/feedback/idea.rs`, `crates/server/tests/feedback_idea.rs`
- Modify: `crates/server/src/{polish.rs:405-463,brief.rs,intents.rs:380-484}`, `crates/server/src/feedback/mod.rs`, `crates/app/src/views/feedback/submit.rs`, `crates/app/src/feedback/submit_vm.rs`, `crates/app/tests/{feedback_submit.rs,feedback_submit_vm.rs}`
- Test: also `crates/server/tests/{polish.rs,brief.rs}`

**Interfaces:**
- Consumes: Task 6 (`CreateExtras`, `submit`), Task 7 (the brief), Task 9 (the screen).
- Produces:

```rust
// feedback/idea.rs
pub const CRITERION_EXPLAINER: &str = "`explainer.md` exists in the issue folder and covers what, why, how it could work and open questions.";
pub const CRITERION_DIAGRAMS: &str = "`explainer.md` has at least one Mermaid diagram of the idea.";
pub const CRITERION_DEMO: &str = "`demo.mp4` (at most 50 MB) shows the bug or the idea in a browser, and `demo.md` lists the steps taken.";
pub const CRITERION_SCOPE: &str = "Only files inside the issue folder are changed, and nothing is implemented.";
pub fn fixed_criteria(options: IdeaOptions) -> Vec<String>;      // explainer, diagrams, demo (each when ticked), then scope
// brief.rs
pub const IDEA_SYSTEM_APPEND: &str = "Work only in this worktree. Prepare the idea package the intent describes and nothing else: do not implement the idea, do not push, and do not open a pull request. Commit your work inside the issue folder and stop.";
pub fn idea_instructions(folder: &str, idea: &IdeaPackage) -> String;
// app: views/feedback/submit.rs
pub const IDEA_CHECK_ID: &str = "feedback-idea";               pub const IDEA_EXPLAINER_ID: &str = "feedback-idea-explainer";
pub const IDEA_DIAGRAMS_ID: &str = "feedback-idea-diagrams";   pub const IDEA_DEMO_ID: &str = "feedback-idea-demo";
pub const IDEA_DEMO_HELP_ID: &str = "feedback-idea-demo-help";
```

Rules:
- **Daemon.** A submission with `idea: Some(options)` uses `place(.., idea: true)` (branch `idea/<slug>`) and creates the intent with `CreateExtras { base_branch, origin: Some(FeedbackOrigin { .., idea: Some(IdeaPackage { explainer, diagrams, demo_video, pr_url: None, not_done: None, implementation_intent_id: None }) }), fixed_criteria: fixed_criteria(options), .. }`. `create_with` puts each fixed criterion in `content.criteria` as an `Item { status: Accepted, fixed: true, edited: false, .. }`, in order, and a fixed dependency in `content.depends_on` as an accepted fixed item whose `intent_ref` is the target and whose text is the target's title.
- **Polish.** `apply_pass2` keeps fixed items: the new `criteria` are the old fixed criteria (same ids, same status) followed by the model's criteria whose text differs from every fixed one; `depends_on` likewise, by `intent_ref`. Pass 1 is unchanged, so it can still ask questions.
- **Brief.** For an intent whose `origin.idea` is set, `compose` uses `IDEA_SYSTEM_APPEND` as the system append and adds `idea_instructions` in place of the Evidence section:

```
## Idea package

This intent prepares an idea for teammates. Do not implement it.

1. Read `{folder}/issue.md` and look at its images.
2. Write `{folder}/explainer.md` with the sections "What", "Why", "How it could work" and "Open questions".
3. Add diagrams to `explainer.md` as Mermaid code blocks. If a Mermaid renderer is installed, also save one rendered image per diagram in the folder and link it.
4. Run the project, reproduce the bug or walk through the idea in a browser with `agent-browser`, and save the recording as `{folder}/demo.mp4` (at most 50 MB). Write the steps you took in `{folder}/demo.md`.
5. If you cannot produce a part, say why under a "Not done" heading in `explainer.md` and go on with the rest.
6. Commit only files inside `{folder}/`. Change nothing else. Do not push and do not open a pull request. Then stop.
```

  Line 3 is present only with `diagrams`, line 4 only with `demo_video`; the remaining lines are renumbered from 1.
- **App.** Under "An intent to work on now", a `Checkbox` `IDEA_LABEL`; while it is ticked, three more, indented: "Demo video", "Diagrams", "Explainer doc". Ticking the parent ticks "Explainer doc". The boxes are shown only while the intent choice is selected and are not remembered. "Demo video" is disabled with `DEMO_MISSING_REASON` in `IDEA_DEMO_HELP_ID` when `agent_browser.ready` is false; when ready the help reads `DEMO_HELP`. With the parent ticked and no part ticked, the summary reads `IDEA_NEEDS_PART` and confirm is disabled. The request carries `idea: Some(IdeaOptions { .. })` when the parent is ticked.

- [ ] **Step 1: Write the failing tests.**
  - `tests/polish.rs`: `pass_two_keeps_fixed_criteria_first` (content with fixed accepted criteria A and B; raw output with criteria `["x", A's text, "y"]` → A, B (same ids, Accepted, fixed), then x and y Pending). `pass_two_keeps_a_fixed_dependency` (a fixed accepted dependency on intent T; raw output proposing T again and a candidate U → T once, fixed and accepted, then U pending). `content_without_fixed_items_is_replaced_as_before`.
  - `tests/brief.rs`: `an_idea_brief_tells_the_agent_to_prepare_and_stop` (all three parts: `system_append == IDEA_SYSTEM_APPEND`; the first message contains the block above with the folder filled in, six numbered lines, and no `## Evidence`). `unticked_parts_are_left_out_and_lines_renumbered` (explainer only: four lines numbered 1 to 4, no `agent-browser`, no `Mermaid`). `a_plain_intent_keeps_the_usual_append`.
  - `tests/feedback_idea.rs`: `fixed_criteria_follow_the_ticked_parts` (all → the four constants in order; demo only → `[CRITERION_DEMO, CRITERION_SCOPE]`). `an_idea_submission_makes_an_idea_intent` (`idea: {explainer: true, diagrams: true, demo_video: false}` → `branch == "idea/fix-the-zoom"`, folder `docs/issues/2026-11-26-fix-the-zoom`; the intent's `origin.idea` has those flags; its criteria are `CRITERION_EXPLAINER`, `CRITERION_DIAGRAMS`, `CRITERION_SCOPE`, each Accepted and fixed; after the fake harness's pass 1 (one question) and pass 2 (two criteria) the three fixed ones are still first and accepted, the two new ones pending, and the question is there). `an_idea_needs_a_part_and_an_intent` (all false → 400 `invalid` "choose at least one part of the idea package"; `send_as: github_issue` with `idea` → 400 `invalid` "an idea package needs an intent, not a GitHub issue"). `the_same_idea_twice_makes_one_branch` (Review Focus 5). `a_failed_idea_intent_step_can_be_retried` (Review Focus 2: the hook of Task 6, then retry → the intent exists with its fixed criteria, and `idea/fix-the-zoom` has one commit above `main`).
  - `app/tests/feedback_submit_vm.rs`: `idea_summaries` (explainer and demo → "Creates the issue folder with 1 screenshot. An agent adds an explainer doc and a demo video on an idea branch, and you open the pull request."; all three → "an explainer doc, diagrams and a demo video"; none → `IDEA_NEEDS_PART` and `can_confirm` false).
  - `app/tests/feedback_submit.rs`: `the_idea_boxes_appear_under_the_intent_choice` (not found until `IDEA_CHECK_ID` is clicked; then the three are found and "Explainer doc" is checked; choosing "A GitHub issue" hides all four). `demo_video_needs_agent_browser` (not ready → `IDEA_DEMO_ID` disabled and `IDEA_DEMO_HELP_ID` reads "Install agent-browser to record a demo video."; ready → enabled and the help reads "Only works for projects a browser can show."). `an_idea_request_carries_the_ticked_parts` (tick the parent and "Diagrams", confirm → the `Submit` call has `idea: Some(IdeaOptions { explainer: true, diagrams: true, demo_video: false })` and `send_as: Intent`). `no_part_means_no_confirm` (untick "Explainer doc" → the summary reads "Choose at least one part." and confirm is disabled).
- [ ] **Step 2: Run them.** `cargo test -p smithagents-server --features test-util --test polish --test brief --test feedback_idea && cargo test -p smithagents-app --test feedback_submit --test feedback_submit_vm`. Expected: compile errors, then failures.
- [ ] **Step 3: Implement.** If Task 9 fell back from `Radio` to buttons, apply the same check to `Checkbox` (`checked()`), with the fallback of an outline `Button` with `.selected(bool)` and a `Check` icon.
- [ ] **Step 4: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(feedback): an idea package is an idea intent with fixed criteria and its own brief`

---

### Task 11: "Open pull request" and "Create implementation intent" on an idea intent

**Files:**
- Modify: `crates/server/src/feedback/{idea.rs,github.rs,mod.rs}`, `crates/server/src/api.rs` (two routes), `crates/client/src/api.rs`, `crates/app/src/link.rs`, `crates/app/src/views/intents/actions.rs`, `crates/app/src/intent_vm.rs`, `crates/app/tests/common/feedback_submit.rs`
- Create: `crates/app/tests/idea_actions.rs`
- Test: `crates/server/tests/feedback_idea.rs`

**Interfaces:**
- Consumes: Tasks 5, 8, 10; the intent tool's `detail_actions`, `IntentsPane::run`, `Busy`, the resolve dialog's pattern.
- Produces:

```rust
// feedback/mod.rs (bodies in idea.rs)
impl FeedbackService {
    pub async fn open_pull_request(&self, intent: IntentId) -> Result<Intent, FeedbackError>;
    pub async fn create_implementation_intent(&self, intent: IntentId) -> Result<Intent, FeedbackError>;
}
// FeedbackError gains: NotAnIdea 409 "not_an_idea" ("this intent is not an idea package"); NotReady(String) 409 "invalid_state";
//   Step(FeedbackFailure) 502 with the failure's own code and message
// routes: POST /v1/intents/{id}/pull-request -> 200 Intent;  POST /v1/intents/{id}/implementation -> 201 Intent
impl Client { pub async fn open_pull_request(&self, id: IntentId) -> Result<Intent, ClientError>;
              pub async fn create_implementation_intent(&self, id: IntentId) -> Result<Intent, ClientError>; }
// app
pub trait FeedbackCommands { /* earlier */
    fn open_pull_request(&self, id: IntentId) -> IntentAnswer;                 // the link upserts the answer
    fn create_implementation_intent(&self, id: IntentId) -> IntentAnswer;      // the link upserts the answer
}
pub(crate) enum Busy { /* existing */ PullRequest, Implementation }
pub fn idea_actions_shown(intent: &Intent, agent: Option<AgentState>) -> bool;   // intent_vm.rs
pub const OPEN_PR_ID: &str = "intent-open-pr";                  // label "Open pull request…"
pub const OPEN_PR_TITLE_ID: &str = "intent-open-pr-title";      // "Open a pull request?"
pub const OPEN_PR_CANCEL_ID: &str = "intent-open-pr-cancel";    // "Cancel", focused when the dialog opens
pub const OPEN_PR_CONFIRM_ID: &str = "intent-open-pr-confirm";  // "Open pull request"
pub const PR_URL_ID: &str = "intent-pr-url";                    pub const NOT_DONE_ID: &str = "intent-not-done";   // Role::Status
pub const IMPLEMENT_ID: &str = "intent-implement";              // label "Create implementation intent"
```

Rules:
- **`open_pull_request`** (under the service mutex): the intent must have `origin.idea` (`NotAnIdea`); a stored `pr_url` is returned as it is. It must be Sent or Resolved with an `agent_id` (`NotReady("the idea intent has not been sent yet")`), and its agent must have no turn in flight (`AgentService::turn_in_flight`; `NotReady("the agent is still working")`). `github_repo(origin_url)` and `gh_readiness` as in Task 8. Then: `head = rev_parse(agent.branch)`; `push(repo, &agent.branch, &origin.branch)` (the agent's branch goes to `origin` as `idea/<slug>`); the body is `issue_body` of `<folder>/issue.md` read at `head`, then, when `<folder>/explainer.md` exists at `head`, a line `---` and its `issue_body` (both with links rewritten to `head`; files = `git::list_folder(head, folder)`), no marker; `gh pr list --head=<idea branch> --state=all --json=url --jq=.[0].url // ""` first, and its URL is used when it is a pull request URL; else `gh pr create --draft --base=<repo.default_branch> --head=<idea branch> --title=<intent title> --body-file=<path>`. The answer must be `https://github.com/<owner>/<repo>/pull/<digits>`. Then `origin.idea.pr_url` and `not_done` (`not_done_section` of the explainer) are saved and `IntentUpdated` is published. Failure codes: the push codes of Task 5; `gh_failed` "gh could not open the pull request (exit status {n})"; `gh_timeout`; `gh_bad_answer` "gh did not print the pull request's address".
- **`create_implementation_intent`**: the same idea and state checks (no `gh` needed). A stored `implementation_intent_id` whose intent exists is returned. Else `create_with(CreateIntentRequest { repo_id: Some(repo), kind: Some(<the idea intent's kind>), seed: SeedInput::Text(text) }, CreateExtras { base_branch: Some(agent.branch), origin: Some(FeedbackOrigin { folder, branch: agent.branch, commit: head, idea: None }), fixed_criteria: vec![], fixed_dependency: Some(idea intent id) })`, where `text` is ``Implement the idea “{title}”.\n\nThe idea package is in `{folder}/` on this branch: read `issue.md` and `explainer.md` first.\n\n{the idea intent's seed text}``, cut to `MAX_SEED_BYTES`. The new id is saved on the idea intent.
- **App.** `idea_actions_shown`: `origin.idea` is set and the intent is Resolved, or Sent with its agent in state `Done`. The bar of such an intent (also for a Resolved one, which otherwise has no bar) adds: when `not_done` is set, a `Role::Status` line `Not done: {text}` (id `NOT_DONE_ID`); when `pr_url` is `None`, "Open pull request…", which opens an alert dialog (`PR_TITLE`, `PR_DESCRIPTION`, Cancel focused, Enter cancels) and on confirm runs `open_pull_request` under `Busy::PullRequest`; when `pr_url` is set, a selectable `Label` with the URL (id `PR_URL_ID`); and "Create implementation intent", which runs under `Busy::Implementation` and on success opens `Screen::Detail(new id)`. The commands come from `state.feedback_commands()`; without one the two buttons are disabled. Errors use the pane's error line with `intent_error_text` (unknown codes show the daemon's sentence).

- [ ] **Step 1: Write the failing tests.**
  - `tests/feedback_idea.rs` (an idea intent taken to Sent with the fake harness; a commit adding `<folder>/explainer.md` with a `## Not done` section and a link `![d](diagram-1.png)` is made in the agent's worktree by the test; `origin` rewritten to the bare repository):
    - `a_pull_request_pushes_the_agents_branch_as_the_idea_branch`: 200; `origin.idea.pr_url == "https://github.com/smithagents-test-invalid/widgets/pull/8"`; the bare repository's `idea/fix-the-zoom` equals the agent branch's head and has no `intent/*` branch; the `pr create` call line starts with `pr create --draft --base=main --head=idea/fix-the-zoom --title=`; `pr-body.md` contains the issue text, a `---` line, the explainer text and `blob/<head>/docs/issues/2026-11-26-fix-the-zoom/diagram-1.png?raw=true`; `origin.idea.not_done` is the section's text; an `IntentUpdated` event was published.
    - `a_second_request_opens_no_second_pull_request` (Review Focus 5): the answer is equal and `calls()` has one `pr create`. `an_existing_pull_request_is_reused` (`existing` set to a pull request URL before the first call → that URL, no `pr create`).
    - `only_a_sent_idea_intent_can_open_one`: a plain intent → 409 `not_an_idea`; an idea intent still Complete → 409 `invalid_state` "the idea intent has not been sent yet"; with a turn in flight → "the agent is still working".
    - `a_failed_push_or_gh_changes_nothing` (Review Focus 2, 3): the `pre-receive` hook → 502 `push_rejected`, `pr_url` still `None`; `fail` = `pr create` → 502 `gh_failed` "gh could not open the pull request (exit status 1)", the response has no `LEAK`, and a later call (failure cleared) succeeds with one more `pr create`.
    - `a_repository_without_a_github_remote_cannot_open_one` (Review Focus 4: 409 `no_github_remote`, nothing pushed).
    - `an_implementation_intent_depends_on_the_idea_and_starts_from_its_branch`: 201; `base_branch` is the idea agent's branch; `content.kind` is the idea intent's; `depends_on` has one item, Accepted and fixed, with `intent_ref` the idea intent; the seed starts with `Implement the idea “`; after pass 2 the dependency is still there; a second request returns the same intent (200 or 201 with the same id) and `list_intents` has two intents; after resolving the idea intent and reviewing, `send` starts an agent whose worktree contains `explainer.md`.
  - `app/tests/idea_actions.rs` (intents from `common::intents`, with `origin.idea` set through a helper `idea_intent(state, pr_url)`; `FakeFeedback` extended with `FeedbackCall::{OpenPullRequest(IntentId), Implement(IntentId)}`):
    - `a_resolved_idea_intent_offers_both_buttons` (also Sent with the agent `Done`; a Sent idea intent whose agent is `Working`, and a resolved plain intent, show neither).
    - `open_pull_request_asks_first`: click → the dialog title reads "Open a pull request?" and its text is `PR_DESCRIPTION`; Cancel has focus; `enter` closes it with no call; confirm → one `FeedbackCall::OpenPullRequest`; the button shows loading; dispatch the answer with `pr_url` → `PR_URL_ID` reads the URL and `OPEN_PR_ID` is gone.
    - `a_second_press_while_busy_sends_nothing` (Review Focus 5).
    - `a_refused_pull_request_shows_the_daemons_sentence` (queue 409 `gh_not_ready` "GitHub CLI (gh) is not signed in; run: gh auth login" → the error line reads that sentence capitalized; the button is enabled again).
    - `the_not_done_line_is_repeated` (`not_done: Some("The project did not start.")` → `NOT_DONE_ID` reads "Not done: The project did not start.").
    - `create_implementation_intent_opens_the_new_intent` (one `FeedbackCall::Implement`; after the answer the screen is `Detail(new id)`).
- [ ] **Step 2: Run them.** `cargo test -p smithagents-server --features test-util --test feedback_idea && cargo test -p smithagents-app --test idea_actions --test intent_actions`. Expected: compile errors, then failures.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests.** Expected: PASS; the intent tool's `intent_actions` tests pass unchanged. Gate.
- [ ] **Step 5: Commit.** `feat(feedback): open a draft pull request for an idea and start its implementation intent`

---

### Task 12: End-to-end tests against the real daemon; documentation

**Files:**
- Create: `crates/client/tests/feedback.rs`
- Modify: `crates/client/tests/common/mod.rs` (helpers), `docs/plugins.md`, `docs/feedback.md`, `docs/superpowers/specs/2026-11-26-feedback-tool-design.md`, `docs/superpowers/plans/2026-11-24-v1-roadmap.md`

**Interfaces:**
- Consumes: everything above; `common::{daemon_bin, fake_claude, claude_fixture, TestHome, init_repo, git}`; `ensure_daemon_with` and `SpawnOptions`; the `fake-claude` controls used in `crates/client/tests/intents.rs:51-98`.
- Produces (test helpers): `common::fake_tools(scratch: &Path) -> (PathBuf /* bin */, PathBuf /* FAKE_GH_DIR */)` (copies `crates/server/tests/bin/{fake-gh,fake-agent-browser}` to `<scratch>/bin/{gh,agent-browser}`, mode 0755); `common::github_repo_with_local_origin(scratch: &Path) -> (PathBuf /* repository */, PathBuf /* bare */)` (`init_repo`, a git identity, `git init --bare`, remote `origin` = `https://github.com/smithagents-test-invalid/widgets.git`, and `url.<bare path>.insteadOf` set to that URL in the repository's own config).

The daemon is started with `SpawnOptions { args: ["--claude-bin", fake_claude()], env: [("PATH", "<bin>:<the test's own PATH>"), ("FAKE_GH_DIR", <dir>), ("SMITHAGENTS_SECRET_FEEDBACK__TOKEN", "LEAK-secret"), fake-claude's controls] }`, so the fakes are found before any real tool and `gh` cannot see the secret. Files to upload are written by the test (a real 4×4 PNG twice, a JPEG).

- [ ] **Step 1: Write the tests** in `crates/client/tests/feedback.rs` (`#[tokio::test]`, the `within` helper of `tests/plugins.rs`):
  - `feedback_becomes_an_intent_whose_agent_has_the_files`: `client.plugin("feedback")` is built in, enabled, `in-process`, state `ready`, no resources. `feedback_targets()` lists the repository with `has_github_remote: true`. `submit_feedback` (send as intent, kind bug, one screenshot with a box, one image) → `branch` starts with `feedback/`, `failed` is `None`, the intent is in Draft with kind `bug`; the repository's branch has `issue.md`, `screenshot-1.png`, `screenshot-1.annotated.png`, `image-1.jpg`; the checkout is clean on `main`; the bare repository has no branch. Drive the intent to Complete as `full_loop_through_the_client` does, `send_intent` → the agent's worktree (from `list_agents`) contains `<folder>/issue.md`.
  - `feedback_becomes_a_github_issue_and_a_failed_push_is_retried`: with a `pre-receive` hook that exits 1 in the bare repository, `submit_feedback` (send as GitHub issue) → `pushed == false`, `failed.code == "push_rejected"`; remove the hook; `retry_feedback(id)` → `issue_url` ends with `/issues/7` and the bare repository has the branch at `commit`; `calls.log` has exactly one `issue create`; submitting the same request again returns the same outcome and adds none.
  - `an_idea_goes_from_feedback_to_a_draft_pull_request`: `submit_feedback` with `idea: { explainer: true, diagrams: false, demo_video: true }` → branch `idea/<slug>`, the intent's criteria start with the three fixed ones; drive it to Sent; wait until its agent's state is `Done`; `open_pull_request(id)` → `pr_url` ends with `/pull/8`, the bare repository has `idea/<slug>`; `create_implementation_intent(id)` → an intent whose `base_branch` is the idea agent's branch and whose `depends_on` names the idea intent.
  - `nothing_leaks_and_nothing_is_left_behind` (Review Focus 3; run at the end of each test above through a shared helper): `home.log()` contains none of `LEAK`, `smithagents-test-invalid`, the bearer token, or a comment text of the upload; `env.log` has no line starting with `SMITHAGENTS_`; `<home>/tmp/feedback` is empty; after `TestHome` drops, `pgrep -fl smithagentsd` shows no daemon of this home.
- [ ] **Step 2: Run them.** `cargo test -p smithagents-client --test feedback`. Expected: PASS (every piece exists; a failure is a defect in an earlier task: fix it there).
- [ ] **Step 3: Update `docs/plugins.md`.** Host modes table: `in-process` becomes "A plugin compiled into the daemon, registered from a manifest embedded in the binary. No directory can ask for it." / "Built-in plugins inside the daemon, and tests." "Command-line tools": a paragraph that a built-in plugin's daemon code runs a declared tool through the manager (`run_tool`: the plugin's own tools only, in a given directory, same environment rules), and that a plugin without resources shows `needs_setup` as advice from a check made when its status is read. A new section "The feedback plugin" (its manifest; that it syncs nothing; what `gh` and `agent-browser` are used for; the routes `GET /v1/feedback/targets`, `POST /v1/feedback`, `POST /v1/feedback/{id}/retry`, `GET|PUT /v1/settings/feedback`, `POST /v1/intents/{id}/pull-request`, `POST /v1/intents/{id}/implementation` with their error codes; the limits; that nothing is pushed except for an issue or a pull request). "Limits": the upload limits. Error codes: `tool_failed` (502).
- [ ] **Step 4: Update the other documents.** `docs/feedback.md`: replace "Submit arrives with the next update" with how to submit (main idea, kind, repository, the two ways to send, the idea package, Retry, what is pushed and when, the public-repository warning). The design: under "Decisions from the builder check", the **Kind** paragraph becomes "New feature, Enhancement and Bug map to the intent kinds of the same names (the intent tool added `new_feature`). `issue.md` keeps the kind the owner chose."; step 3 of "How it works" says "the kind is passed through"; "The pull request is for the idea only" gains one sentence: the agent works on `intent/<slug>` branched from `idea/<slug>`, and "Open pull request" pushes that work to `origin` as `idea/<slug>`; the Submit dialog is described as a screen of the pane. The roadmap: a row for the feedback tool (capture; submit, GitHub issue and idea package) with Status `Built: 2026-11-26-feedback-capture.md, 2026-11-26-feedback-submit.md`, and the six routes added to the API contract table.
- [ ] **Step 5: Run the gate** for the whole workspace. `cargo build -p host-desktop` compiles (do not run it).
- [ ] **Step 6: Commit.** `test(feedback): submit, issue and idea paths against the real daemon; docs`

---

## Manual check (a person, after the build; not automated)

With a scratch `SMITHAGENTS_HOME` and a throwaway private GitHub repository of the tester's own: (1) the Feedback plugin shows `needs_setup` naming a missing tool and `ready` once `gh` and `agent-browser` are installed; (2) Submit as an intent creates the branch and opens the intent, and the working tree is untouched; (3) Submit as an issue pushes with the tester's own git credentials, the issue shows its images, and its label is right; (4) with the network off, the push fails with a plain sentence and Retry works once it is back; (5) an idea package: the agent writes `explainer.md` and, when `agent-browser` really records, `demo.mp4`; "Open pull request" opens a draft; (6) the public-repository warning shows for a public repository. Record what `agent-browser` actually produced: the recording was never run by anyone before this check.

## Follow-ups

- **`agent-browser`**: if Task 1 could not confirm the package name and video recording, confirm both before relying on Demo video; the manifest's `install.mise` spec is the only place to change.
- **Feedback about SmithAgents itself**: the design disables Demo video there; this plan offers it whenever the tool is ready and says what it is for. A rule that knows the app's own repository can replace the helper text.
- **GitHub Enterprise and other hosts**: only `github.com` is a GitHub remote; a remote other than `origin` cannot be chosen.
- **Cleaning up**: feedback and idea branches, and the rows of `feedback_submissions`, are never deleted; "Start over" leaves its branch behind.
- **Signed commits and hooks**: the commit uses the user's signing settings (a signing program that needs a prompt fails the commit) and skips hooks with `--no-verify`.
- **The daemon on another machine**: uploads already travel as bytes; the upload has no progress bar and no resume.
- **Visibility** is cached for 10 minutes and asked only for `github.com`; a repository made public in between is reported as private until the cache ends.
- **Idea package extras**: rendering Mermaid to images in the daemon; checking `demo.mp4` against 50 MB before the pull request; a pull request for a plain feedback intent.
- **The app**: a plugin screen that shows `needs_setup` and installs a tool with one click (today: the API); remembering the idea checkboxes; a size check in the app that counts annotated images before uploading.
- **Fixed items** (corrected 2026-11-26; this note first said the review routes could still drop a fixed criterion): the review routes refuse to edit or drop a fixed item (400 `invalid`, "this item is fixed: it cannot be edited or dropped"), and polish keeps fixed items first and unchanged. Nothing is left to follow up here.
- **Search delay**: the marker search before `gh issue create` depends on GitHub's index; an issue created seconds before a retry may not be found, and a second one is then created.

## Self-review

- **Spec coverage:** built-in plugin with two tools, zero resources, advice-only `needs_setup` (Task 1); meta repository (Task 3); `GET /v1/feedback/targets` (Tasks 6, 8); `POST /v1/feedback`, multipart, limits, checks by content, temp directory, bearer token, `detached` (Task 6); folder, `issue.md`, names, slug clash, branch names, commit message, untouched working tree (Tasks 4, 5); intent path with kind passed through, text seed, `base_branch`, opening the intent (Tasks 6, 7, 9); GitHub issue path, labels, permanent links, partial failure, Retry (Task 8); public-repository warning (Tasks 9, 11); idea intent, fixed criteria kept by polish, the brief, tool gating (Task 10); "Open pull request" and "Create implementation intent" (Task 11); Submit screen, result line, cleared draft (Task 9); tests without network or real tools (all, Task 12); documentation and the design's kind line (Task 12). Decisions 1 to 12 map to Tasks 1; 3; 6 and 8; 6; 4 and 5; 6, 7 and 9; 8; 9 and 11; 10; 11; 9 and 10; all.
- **Type consistency:** names in each Produces block were checked against later Consumes and test steps: `ToolRun`, `run_tool`, `FakeTools`; `FeedbackRequest`, `FeedbackOutcome`, `FeedbackFailure`, `FeedbackStep`, `FeedbackOrigin`, `IdeaPackage`, `IdeaOptions`, `SubmissionId`, `Submission`; `FeedbackService::{settings, set_settings, targets, submit, retry, open_pull_request, create_implementation_intent}`; `CreateExtras`, `create_with`; `Placement`, `FolderFile`, `commit_folder`, `push`; `issue_body`, `marker`, `github_repo`; `FeedbackCommands`, `FakeFeedback`, `FeedbackCall`; `Pending`, `begin_submission`, `finish_submission`; `Busy::{PullRequest, Implementation}`.
- **Review Focus:** 1 → `hostile_names_and_comments_cannot_break_the_file` (Task 4), `hostile_and_oversized_uploads_are_refused_and_leave_nothing`, `a_name_from_the_client_is_never_a_path` (Task 6). 2 → `a_failed_intent_step_keeps_the_commit_and_says_so` (Task 6), `a_failed_push_keeps_the_commit_and_retry_pushes_and_creates`, `a_failed_gh_keeps_the_push_and_retry_only_creates_the_issue` (Task 8), `a_partial_failure_offers_retry_and_start_over` (Task 9), `a_failed_push_or_gh_changes_nothing` (Task 11). 3 → `run_strips_smithagents_variables_and_passes_the_given_ones`, `run_errors_carry_no_output` (Task 1), `push_failures_are_named_without_gits_words` (Task 5), `targets_never_show_a_remote_url` (Task 6), `nothing_gh_or_git_printed_reaches_a_response_or_an_event` (Task 8), `nothing_leaks_and_nothing_is_left_behind` (Task 12). 4 → `a_missing_default_branch_is_refused_and_nothing_is_created`, `the_branch_starts_from_the_default_branch_as_it_is_now` (Task 5), `a_renamed_default_branch_is_refused`, `a_repository_without_a_remote_takes_the_intent_path` (Task 6), `the_issue_path_is_refused_before_anything_is_written` (Task 8), `a_repository_without_a_github_remote_cannot_open_one` (Task 11). 5 → `the_same_submission_twice_makes_one_branch_and_one_intent` (Task 6), `an_issue_that_already_exists_is_not_created_again` (Task 8), `a_second_press_sends_nothing` (Task 9), `the_same_idea_twice_makes_one_branch` (Task 10), `a_second_request_opens_no_second_pull_request` (Task 11).
- **Marked "verify first":** the `agent-browser` package and its video recording (Task 1; nothing depends on the answer); the per-route body limit (Task 6; fallback: a sub-router); `Radio` and `Checkbox` state in headless tests (Tasks 9 and 10; fallback: buttons with `selected`).
- **Where the code differs from the decisions file:** the decision's "`needs_setup` as the plugin system already has it" needed a change, because tools are checked only before a sync run and this plugin never syncs (Task 1). "Run through the plugin tool runner" needed a public entry point, because the runner is crate-private and always runs from `/` (Task 1). `GitError::Command` carries git's stderr into API responses today, so the new git functions use their own error types (Task 5). `meta_repo_id` needs no column (the settings table is key and value), so migration `0004` holds only the intent columns and the submissions table (Task 2).

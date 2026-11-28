# Folder Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A repository of the Workspace may have no source control: it is a plain folder, the default for a new one, and an agent works in it directly, one at a time.

**Architecture:** `Repo` gains `source_control: Option<SourceControl>` and `Agent.branch` becomes optional; everything that runs git asks that field first. An in-place agent's working folder is the repository's own path, and the agent service adds a per-repository claim beside its per-agent reservation so only one turn runs in a plain folder. The queue learns which plain folders are held and skips their intents.

**Tech Stack:** Rust stable, edition 2021; tokio, axum, sqlx/SQLite (bundled, with JSON functions and `DROP COLUMN`), serde, thiserror; GPUI through `gpui-kit` for the app.

**Spec:** `docs/superpowers/specs/2026-11-27-folder-mode-design.md`. Read it before any task; every rule below comes from it. Context: `docs/superpowers/specs/2026-11-23-smithagents-architecture-design.md`, "Owner decisions of 2026-11-27".

## Global Constraints

- Wire: `"source_control": null` for a plain folder, `{"git": {"default_branch": "<name>"}}` otherwise. `PROTOCOL` is `2`.
- The add choices are spelled `none`, `git`, `new_git`. A request without the field means `none`.
- A new git repository gets the branch `main` and one empty first commit with the message `Initial commit`.
- Error codes, exactly: `folder_busy`, `repo_in_use`, `repo_overlaps`, `repo_has_feedback_intents`, `needs_source_control`, `git_not_installed`, `git_identity_missing`, `folder_not_empty`, `not_repo_root`.
- The daemon runs no git for a repository without source control, and never tells its agent to commit, branch, push or open a pull request.
- A repository that exists before the migration has source control after it.
- Tests never run a real `gh`, `claude`, `mise` or `agent-browser`, never capture the screen, never launch the desktop app. `claude` is `crates/server/tests/bin/fake-claude`. Real `git` is used, as the existing tests do.
- The repository is public: no personal names, emails, tokens or home paths in code, tests, fixtures or docs.
- `unsafe` stays forbidden. No new dependency.
- The grouping is called a Workspace in new comments, docs and UI text, never a "Space".
- Gate, from the worktree root, all clean before a task is done: `cargo test --workspace --no-fail-fast`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`, `cargo themegen check`, and no test `smithagentsd` left running (`pgrep -fl smithagentsd`).
- One commit per task, message in the style of `git log` (`feat(server): …`), ending with the co-author trailer the session gives.

## Review Focus

1. **The old default is gone.** `POST /v1/repos` without `source_control` now adds a plain folder. Every existing test, fixture and caller that adds a git repository and then expects a worktree must say `git`; one that is missed fails or, worse, passes while testing the wrong mode.
2. **A claim that is never released.** A turn that fails, panics, times out or is cut off by a daemon restart must free its folder. A leaked claim blocks the folder and its queue for good.
3. **A folder that is a git repository, added with no source control.** No git process may run for it and its agent must be told not to commit, even though git would work there.
4. **The same folder spelled differently.** A link to it, a trailing slash, `..`, or another letter case on a case-insensitive disk must count as the same folder in the duplicate and overlap checks.
5. **A real database from before the migration.** Repositories, open and closed agents with their branches, and a default branch containing a quote must all read back.

---

### Task 1: The record

The types change shape and every user of them keeps compiling, with behaviour unchanged: after this task every repository is still a git repository.

**Files:**
- Modify: `crates/core/src/agent.rs` (`Repo`, `Agent`, new `SourceControl`), `crates/core/src/lib.rs` (`PROTOCOL`), `crates/core/src/feedback.rs` (`effective_meta_repo`), `crates/core/src/event.rs` (JSON fixtures in its tests)
- Create: `crates/server/migrations/0005_source_control.sql`
- Modify: `crates/server/src/store.rs` (`repo_from_row`, `insert_repo`, `agent_from_row`, `insert_agent_with_append`, new `set_repo_source_control`)
- Modify, to keep compiling: every read of `repo.default_branch` and `agent.branch` in `crates/server/src` (`service.rs`, `intents.rs`, `feedback/*.rs`), `crates/app/src` (`view_models.rs`, `views/settings/repositories.rs`) and the tests and fixtures of `core`, `server`, `client`, `app`
- Test: `crates/core/src/agent.rs`, `crates/core/src/feedback.rs`, `crates/server/src/store.rs`, `crates/server/tests/feedback_store.rs`

**Interfaces:**
- Produces:
  ```rust
  // core::agent
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum SourceControl { Git { default_branch: String } }

  pub struct Repo { pub id: RepoId, pub path: PathBuf,
                    pub source_control: Option<SourceControl>, pub created_at: DateTime<Utc> }
  impl Repo {
      pub fn default_branch(&self) -> Option<&str>;   // None for a plain folder
      pub fn is_plain(&self) -> bool;
  }
  pub struct Agent { /* as before, except */ pub branch: Option<String> }
  impl Agent { pub fn works_in_place(&self) -> bool; } // branch is None

  // core
  pub const PROTOCOL: u32 = 2;

  // server::store
  pub async fn set_repo_source_control(&self, id: RepoId, to: Option<&SourceControl>)
      -> Result<(), StoreError>;

  // server::service
  ServiceError::NeedsSourceControl   // 409 "needs_source_control",
                                     // "this repository has no source control"
  ```
- Storage: `repos.source_control` is the JSON of `SourceControl`, `NULL` for a plain folder. `agents.branch` stays `NOT NULL`; the empty string means no branch (a branch name is never empty).

- [ ] **Step 1: Write the failing tests**

  ```rust
  // crates/core/src/agent.rs
  #[test]
  fn a_repository_says_its_source_control_on_the_wire() {
      let plain = repo(None);
      let git = repo(Some(SourceControl::Git { default_branch: "main".into() }));
      assert_eq!(serde_json::to_value(&plain).unwrap()["source_control"], Value::Null);
      assert_eq!(serde_json::to_value(&git).unwrap()["source_control"],
                 json!({"git": {"default_branch": "main"}}));
      assert_eq!((plain.default_branch(), plain.is_plain()), (None, true));
      assert_eq!((git.default_branch(), git.is_plain()), (Some("main"), false));
      // and each reads back equal to itself
  }

  #[test]
  fn an_agent_that_works_in_place_has_no_branch() {
      let json = serde_json::to_value(agent(None)).unwrap();
      assert_eq!(json["branch"], Value::Null);
      assert!(agent(None).works_in_place() && !agent(Some("agent/x")).works_in_place());
  }

  // crates/core/src/feedback.rs
  #[test]
  fn the_meta_repository_is_never_a_plain_folder() {
      // plain, git_a, git_b: three repositories
      assert_eq!(effective_meta_repo(Some(plain.id), &[plain.clone(), git_a.clone()]), Some(git_a.id));
      assert_eq!(effective_meta_repo(None, &[plain.clone()]), None);
      assert_eq!(effective_meta_repo(Some(plain.id), &[plain.clone(), git_a.clone(), git_b.clone()]), None);
      assert_eq!(effective_meta_repo(Some(git_b.id), &[plain, git_a, git_b.clone()]), Some(git_b.id));
  }

  // crates/server/tests/feedback_store.rs, in the style of
  // `a_database_from_0003_migrates_and_keeps_its_intents`
  #[tokio::test]
  async fn a_database_from_0004_migrates_its_repositories_to_git() {
      // a database at 0004 with a repository whose default branch is `it's-trunk`,
      // an open agent on `agent/earth-1` and a closed one on `intent/x`
      assert_eq!(repos[0].source_control,
                 Some(SourceControl::Git { default_branch: "it's-trunk".into() }));
      assert_eq!(open.branch.as_deref(), Some("agent/earth-1"));
      assert_eq!(closed.branch.as_deref(), Some("intent/x"));
  }

  // crates/server/src/store.rs
  #[tokio::test]
  async fn a_plain_repository_and_its_agent_round_trip() {
      // insert a Repo with source_control None and an Agent with branch None
      assert_eq!(store.get_repo(id).await.unwrap().unwrap().source_control, None);
      assert_eq!(store.get_agent(agent_id).await.unwrap().unwrap().branch, None);
      store.set_repo_source_control(id, Some(&git("main"))).await.unwrap();
      assert_eq!(store.get_repo(id).await.unwrap().unwrap().default_branch(), Some("main"));
  }
  ```

- [ ] **Step 2: Run them and see them fail to compile** — `cargo test -p smithagents-core 2>&1 | head -30`. Expected: `cannot find type SourceControl`.

- [ ] **Step 3: Change the types** in `core::agent` and `core::lib` as under Interfaces. `effective_meta_repo` keeps its signature and considers only repositories that are not plain, both for the stored id and for "the only repository".

- [ ] **Step 4: Write the migration**, exactly:

  ```sql
  ALTER TABLE repos ADD COLUMN source_control TEXT;
  UPDATE repos SET source_control =
      json_object('git', json_object('default_branch', default_branch));
  ALTER TABLE repos DROP COLUMN default_branch;
  ```

- [ ] **Step 5: Update the store.** Rows read and write `source_control` as JSON text; an agent's `None` branch is stored as `''` and read back as `None`. Add `set_repo_source_control`.

- [ ] **Step 6: Make everything compile, behaviour unchanged.** `AgentService::add_repo` still records `Git` from `validate_repo`. Where code needs a default branch or an agent's branch and there is none, it returns `ServiceError::NeedsSourceControl` (or the caller's own error wrapping it); no `unwrap` or `expect`. `RepoRowVm.default_branch` becomes `Option<String>`.

- [ ] **Step 7: Run the gate.** Expected: everything passes, and the four tests above pass.

- [ ] **Step 8: Commit** — `feat(core): a repository's source control is optional on the record`.

---

### Task 2: Adding a repository three ways, and switching

**Files:**
- Modify: `crates/core/src/agent.rs` (`AddSourceControl`), `crates/server/src/git.rs`, `crates/server/src/service.rs`, `crates/server/src/api.rs`, `crates/server/src/bin/smithagentsd.rs` (`--git-bin`), `crates/client/src/api.rs`
- Modify: every existing caller and test that adds a git repository (see Review Focus 1): `crates/server/tests/*.rs` and `tests/common`, the `service.rs` and `intents.rs` test fixtures, `crates/client/tests/*.rs` and `tests/common`, `crates/app/src/link.rs`, `crates/app/tests/*.rs` and `tests/common`
- Test: `crates/server/src/service.rs` (unit tests), `crates/server/tests/api.rs`, `crates/server/tests/daemon_process.rs`

**Interfaces:**
- Consumes: Task 1's `SourceControl`, `Repo`, `set_repo_source_control`, `ServiceError::NeedsSourceControl`.
- Produces:
  ```rust
  // core::agent
  #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum AddSourceControl { #[default] None, Git, NewGit }

  // server::git
  pub fn set_program(program: PathBuf);          // once, at daemon start; default "git"
  pub async fn is_installed() -> bool;           // `<program> --version` can be started
  pub async fn init_repo(path: &Path) -> Result<RepoInfo, GitError>;
  GitError::{NotInstalled, IdentityMissing, FolderNotEmpty(PathBuf), NotRepoRoot}

  // server::service
  pub async fn add_repo(&self, path: &Path, choice: AddSourceControl) -> Result<Repo, ServiceError>;
  pub async fn set_source_control(&self, id: RepoId, on: bool) -> Result<Repo, ServiceError>;
  ServiceError::{RepoInUse, RepoOverlaps, FeedbackIntentsOpen}

  // client
  pub async fn add_repo(&self, path: &Path, choice: AddSourceControl) -> Result<Repo, ClientError>;
  pub async fn set_source_control(&self, id: RepoId, on: bool) -> Result<Repo, ClientError>;
  pub struct Health { /* … */ #[serde(default)] pub git: bool }
  ```
- Routes: `POST /v1/repos` body `{ "path", "source_control"?: "none" | "git" | "new_git" }`; `PATCH /v1/repos/{id}` body `{ "source_control": "none" | "git" }`, answering the `Repo`; `GET /v1/health` gains `"git": <bool>`.
- Daemon flag: `--git-bin <path>` beside `--claude-bin`, default `git`.

| Error | Status | Code |
|---|---|---|
| `ServiceError::RepoInUse` | 409 | `repo_in_use` |
| `ServiceError::RepoOverlaps` | 409 | `repo_overlaps` |
| `ServiceError::FeedbackIntentsOpen` | 409 | `repo_has_feedback_intents` |
| `GitError::NotInstalled` | 400 | `git_not_installed` |
| `GitError::IdentityMissing` | 400 | `git_identity_missing` |
| `GitError::FolderNotEmpty` | 409 | `folder_not_empty` |
| `GitError::NotRepoRoot` | 400 | `not_repo_root` |

- [ ] **Step 1: Write the failing tests** (unit tests in `service.rs`, beside `existing_branch_is_branch_exists_and_leaves_nothing_behind`; the fixture's own repository is added with `AddSourceControl::Git`):

  ```rust
  #[tokio::test]
  async fn a_plain_folder_is_added_as_it_is() {
      // `notes/` is not a repository; `my-repo/docs/` is a folder inside one
      let notes = fx.svc.add_repo(&notes_dir, AddSourceControl::None).await.unwrap();
      assert_eq!(notes.source_control, None);
      assert_eq!(notes.path, std::fs::canonicalize(&notes_dir).unwrap());
      let gone = fx.svc.add_repo(&tmp.join("nowhere"), AddSourceControl::None).await.unwrap_err();
      assert!(matches!(gone, ServiceError::Git(GitError::PathNotFound(_))));
  }

  #[tokio::test]
  async fn a_new_git_repository_needs_an_empty_folder() {
      // a folder that holds one file, added with NewGit:
      assert!(matches!(err, ServiceError::Git(GitError::FolderNotEmpty(_))));
      // the file is still there and the folder has no `.git`
  }

  #[tokio::test]
  async fn a_plain_folder_may_not_overlap_another_repository() {
      // x/ is a plain repository; every folder below exists
      for path in [x.join("sub"), x.parent().unwrap().to_owned(), link_to_x, x.join("sub").join(".."),
                   fx.paths.home.clone(), fx.paths.home.join("worktrees"), fx.repo_dir.join("docs")] {
          let err = fx.svc.add_repo(&path, AddSourceControl::None).await.unwrap_err();
          assert!(matches!(err, ServiceError::RepoOverlaps | ServiceError::Store(StoreError::DuplicateRepoPath)), "{path:?}: {err:?}");
      }
      // where the disk ignores letter case (the upper-cased path of x/ names the same folder),
      // that spelling is refused too
      // a git repository inside another git repository is still added, as before
  }

  #[tokio::test]
  async fn source_control_is_switched_only_with_no_open_agent() {
      let agent = fx.svc.create_agent(fx.repo.id).await.unwrap();
      assert!(matches!(fx.svc.set_source_control(fx.repo.id, false).await, Err(ServiceError::RepoInUse)));
      fx.svc.close_agent(agent.id).await.unwrap();
      assert_eq!(fx.svc.set_source_control(fx.repo.id, false).await.unwrap().source_control, None);
      assert_eq!(fx.svc.set_source_control(fx.repo.id, true).await.unwrap().default_branch(), Some("main"));
      // the closed agent's worktree folder and branch are still there
  }

  #[tokio::test]
  async fn a_folder_inside_a_git_repository_cannot_be_turned_into_one() {
      // docs/ inside the fixture's repository, added first as plain is refused by the
      // overlap rule; so: a plain repository at other-repo/docs, other-repo not added
      assert!(matches!(err, ServiceError::Git(GitError::NotRepoRoot)));
  }

  #[tokio::test]
  async fn source_control_stays_on_while_a_feedback_intent_is_unresolved() {
      // an intent of fx.repo with `origin: Some(..)`, state Complete
      assert!(matches!(fx.svc.set_source_control(fx.repo.id, false).await, Err(ServiceError::FeedbackIntentsOpen)));
      // the same intent Resolved: the switch succeeds
  }
  ```

  In `crates/server/tests/api.rs`:

  ```rust
  #[tokio::test]
  async fn a_repository_is_added_plain_unless_the_request_says_git() {
      // POST {"path": <a git repository>} -> 200, body["source_control"] == null
      // POST {"path": <another>, "source_control": "git"} -> body["source_control"]["git"]["default_branch"] == "main"
      // POST {"path": .., "source_control": "svn"} -> 400 bad_request
      // PATCH /v1/repos/{id} {"source_control": "git"} on the first -> 200 with git
      // GET /v1/health -> body["git"] == true
  }
  ```

  In `crates/server/tests/daemon_process.rs`:

  ```rust
  #[tokio::test]
  async fn a_daemon_without_git_starts_and_says_so() {
      // spawned with ["--git-bin", "<scratch>/no-such-git"]
      // GET /v1/health -> "git": false
      // POST /v1/repos {"path": <folder>} -> 200, plain
      // POST /v1/repos {"path": <folder2>, "source_control": "git"} -> 400 git_not_installed
      // POST /v1/repos {"path": <folder3>, "source_control": "new_git"} -> 400 git_not_installed,
      //   and <folder3> was not created
  }

  // The next two give the daemon a HOME of the test's own, so that git's identity is the
  // test's and not this machine's: `Daemon::spawn` gains extra environment for that.
  #[tokio::test]
  async fn a_new_git_repository_starts_with_one_commit_on_main() {
      // HOME holds a .gitconfig with a neutral name and email
      // POST {"path": <scratch>/fresh, "source_control": "new_git"} -> 200,
      //   body["source_control"]["git"]["default_branch"] == "main"
      // `git rev-list --count main` in it is "1"; `git log -1 --format=%s` is "Initial commit"
      // POST /v1/agents for it -> 200: a worktree can branch from that commit
  }

  #[tokio::test]
  async fn a_new_repository_without_a_git_identity_is_refused_and_leaves_no_folder() {
      // HOME holds a .gitconfig with only `[user] useConfigOnly = true`
      // POST {"path": <scratch>/fresh, "source_control": "new_git"} -> 400 git_identity_missing
      // <scratch>/fresh does not exist afterwards
  }
  ```

- [ ] **Step 2: Run them and see them fail** — `cargo test -p smithagents-server --lib a_plain_folder 2>&1 | head -30`. Expected: `add_repo` takes one argument.

- [ ] **Step 3: git.** `set_program` stores the program in a `OnceLock` that `git_command` and `isolated_command` read. A spawn that fails with "not found" becomes `GitError::NotInstalled` in `validate_repo`, `init_repo` and `is_installed`. `init_repo`: the path is missing or an empty folder, else `FolderNotEmpty`; create it; `init -b main`; an empty commit `Initial commit`, made with the user's own git configuration (it is their repository and their first commit, so the identity is theirs); when git says it has no identity (`IDENTITY_MISSING`, as `commit_paths` detects it), remove what was created and return `IdentityMissing`.

- [ ] **Step 4: `add_repo` and `set_source_control`**, by the spec's sections "Adding a repository" and "Turning source control on or off". Paths are compared after `std::fs::canonicalize`. A plain folder overlaps when it equals, contains or lies inside another repository's path or `paths.home`. Turning on requires `validate_repo(path).root == path`, else `NotRepoRoot`. Turning off is refused while an intent of the repository has an `origin` and is not `Resolved` (`store.list_intents` with the repository's id).

- [ ] **Step 5: Routes, the flag, health and the client**, as under Interfaces. `main` calls `git::set_program` before anything runs git.

- [ ] **Step 6: Update every caller that means a git repository** to pass `AddSourceControl::Git` (Review Focus 1). `DaemonLink::add_repo` takes the choice and passes it on; the app still passes `Git` until Task 6.

- [ ] **Step 7: Run the gate.** Expected: everything passes.

- [ ] **Step 8: Commit** — `feat(server): add a repository as a plain folder, an existing or a new git repository`.

---

### Task 3: An agent in place, one at a time

**Files:**
- Modify: `crates/server/src/service.rs`
- Test: `crates/server/src/service.rs` (unit tests), `crates/server/tests/daemon_process.rs`

**Interfaces:**
- Consumes: Task 1's `Agent::works_in_place`, Task 2's `add_repo`.
- Produces:
  ```rust
  ServiceError::FolderBusy      // 409 "folder_busy", "Another agent is working in this folder."
  impl AgentService {
      /// A turn is in flight among the in-place agents of `repo`.
      pub fn folder_busy(&self, repo: RepoId) -> bool;
  }
  ```

- [ ] **Step 1: Write the failing tests**

  ```rust
  #[tokio::test]
  async fn an_agent_on_a_plain_repository_works_in_the_folder_itself() {
      let agent = fx.svc.create_agent(plain.id).await.unwrap();
      assert_eq!(agent.worktree_path, plain.path);
      assert_eq!(agent.branch, None);
      assert!(!fx.paths.repo_worktrees_dir(&plain).exists());
      // one turn: the harness was asked to run with cwd == plain.path
      // options that name a branch or a base on a plain repository:
      assert!(matches!(err, ServiceError::NeedsSourceControl));
  }

  #[tokio::test]
  async fn one_agent_works_in_a_plain_folder_at_a_time() {
      // FakeHarness::scripted(..).with_delay(500 ms); two agents a, b on `plain`,
      // and c on the git repository
      fx.svc.send_message(a.id, "go".into()).await.unwrap();
      assert!(fx.svc.folder_busy(plain.id));
      assert!(matches!(fx.svc.send_message(b.id, "go".into()).await, Err(ServiceError::FolderBusy)));
      assert!(fx.messages(b.id).await.is_empty(), "the refused message is not kept");
      fx.svc.send_message(c.id, "go".into()).await.unwrap();      // another repository is free
      assert!(matches!(fx.svc.close_agent(a.id).await, Err(ServiceError::TurnInProgress)));
      // when a's turn has ended:
      assert!(!fx.svc.folder_busy(plain.id));
      fx.svc.send_message(b.id, "go".into()).await.unwrap();
  }

  #[tokio::test]
  async fn a_turn_that_fails_frees_its_folder() {
      // a's turn ends with HarnessEvent::Failed; then with a harness whose run_turn returns Err;
      // then with one that panics: after each, folder_busy(plain.id) is false and b may send
  }

  #[tokio::test]
  async fn two_agents_on_a_git_repository_still_work_at_once() {
      // unchanged behaviour: both sends accepted while both turns run; folder_busy is false
  }
  ```

  In `crates/server/tests/daemon_process.rs`:

  ```rust
  #[tokio::test]
  async fn a_plain_folder_is_worked_in_without_git() {
      // daemon with ["--git-bin", "<scratch>/no-such-git"]; a folder that IS a git repository
      // (made by the test with the real git) is added with no source control
      // create an agent, send one message, wait for the turn to end (fake-claude):
      // the agent ends Done, its worktree_path is the folder, and the folder's
      // `git status --porcelain` and `git log` are as before the turn
  }
  ```

- [ ] **Step 2: Run them and see them fail** — `cargo test -p smithagents-server --lib plain 2>&1 | head -30`.

- [ ] **Step 3: Create in place.** In `create_agent_with`, a plain repository takes no git path at all: the folder must be a directory (else `GitError::PathNotFound`), `worktree_path` is the repository's path, `branch` is `None`, and `opts.branch` or `opts.base` being set is `NeedsSourceControl`. The failed-insert cleanup has nothing to remove.

- [ ] **Step 4: The claim.** The reservation map holds, beside the per-agent slots, the set of repositories with an in-place turn in flight. `send_message` takes the agent's slot, loads the agent, and for an agent that works in place claims its repository, answering `FolderBusy` and keeping no message when it is taken. The `Reservation` releases both when it is dropped or released, so every way a turn ends frees the folder.

- [ ] **Step 5: Run the gate.** Expected: everything passes.

- [ ] **Step 6: Commit** — `feat(server): an agent on a plain folder works in place, one at a time`.

---

### Task 4: The brief, intents and the queue

**Files:**
- Modify: `crates/server/src/brief.rs`, `crates/server/src/intents.rs`, `crates/server/src/queue.rs`
- Test: `crates/server/tests/brief.rs`, `crates/server/src/queue.rs` (unit tests), `crates/server/tests/intents.rs`, `crates/server/tests/queue.rs`

**Interfaces:**
- Consumes: Task 3's `folder_busy` and in-place creation.
- Produces:
  ```rust
  // brief
  pub const PLAIN_SYSTEM_APPEND: &str;        // text below
  pub fn compose(intent: &Intent, deps: &[DepSummary], in_place: bool) -> Brief;

  // queue
  pub fn next_startable(queued: &[Intent], resolved: &HashSet<IntentId>, running: usize,
                        limit: Option<u32>, held: &HashSet<RepoId>) -> Vec<IntentId>;

  // intents
  impl IntentService {
      /// Plain repositories the queue may not start an intent on now.
      pub(crate) async fn held_folders(&self) -> Result<HashSet<RepoId>, IntentError>;
  }
  IntentError::NeedsSourceControl   // 409 "needs_source_control",
                                    // "This intent needs a repository with source control."
  ```
- Exact texts:
  - `PLAIN_SYSTEM_APPEND`: `Work only in this folder. Satisfy every acceptance criterion in the intent you are given by changing the files here directly. This folder is not under source control for this work: do not create commits or branches, do not push, and do not open a pull request, even if you find a git repository here. When you are done, say what you changed.`
  - After "Depends on" for an in-place brief, in place of the rebase instruction: `The work of these dependencies was done in this same folder and is already here.`
- A plain repository is held when a turn is in flight in it (`folder_busy`), or when one of its intents is `Sent` and that intent's agent is not closed.

- [ ] **Step 1: Write the failing tests**

  ```rust
  // crates/server/tests/brief.rs
  #[test]
  fn an_in_place_brief_asks_for_no_commit_and_no_pull_request() {
      let brief = compose(&intent_with_one_dependency(), &deps, true);
      assert_eq!(brief.system_append, PLAIN_SYSTEM_APPEND);
      assert!(brief.first_message.contains("The work of these dependencies was done in this same folder and is already here."));
      for word in ["rebase", "worktree", "open the pull request when you are done"] {
          assert!(!brief.first_message.contains(word) && !brief.system_append.contains(word), "{word}");
      }
      // compose(.., false) is byte for byte what it was before this task
  }

  // crates/server/src/queue.rs
  #[test]
  fn an_intent_on_a_held_folder_waits_and_takes_no_slot() {
      // queued, oldest first: a (repo P), b (repo P), c (repo G); nothing running, limit 1
      assert_eq!(next_startable(&queued, &resolved, 0, Some(1), &HashSet::from([p])), vec![c.id]);
      assert_eq!(next_startable(&queued, &resolved, 0, Some(1), &HashSet::new()), vec![a.id]);
  }

  // crates/server/tests/intents.rs
  #[tokio::test]
  async fn an_intent_on_a_plain_repository_starts_in_place() {
      // the intent's agent: branch None, worktree_path == the folder; its system prompt append
      // is PLAIN_SYSTEM_APPEND; the intent is Sent
      // an intent of that repository that carries a base_branch: start fails with
      // IntentError::NeedsSourceControl, last_start_error is set, and no agent is left
  }

  #[tokio::test]
  async fn send_on_a_held_folder_queues() {
      // a is Sent on the plain repository and its turn has ended; b is sent:
      assert_eq!(b.state, IntentState::Queued);
      assert_eq!(b.last_start_error, None);
  }

  // crates/server/tests/queue.rs
  #[tokio::test]
  async fn intents_on_one_plain_folder_run_one_after_the_other() {
      // queue limit 2; a and b on the plain repository, c on a git repository, sent in that order
      // a and c are Sent; b stays Queued while a's turn runs and after it ends
      // resolve a: b becomes Sent
  }

  #[tokio::test]
  async fn closing_the_agent_frees_the_folder_for_the_next_intent() {
      // as above, but a's agent is closed instead of a being resolved: b becomes Sent
  }
  ```

- [ ] **Step 2: Run them and see them fail** — `cargo test -p smithagents-server --test brief 2>&1 | head -20`. Expected: `compose` takes two arguments.

- [ ] **Step 3: The brief.** `compose` chooses the append and the sentence after "Depends on" by `in_place`. An idea intent is never in place (Feedback does not target a plain repository); `compose` with `in_place` and an idea intent uses the plain append all the same.

- [ ] **Step 4: Starting.** `launch_agent` on a plain repository skips `checked_base_branch` and the branch names, passes `branch: None, base: None`, and composes with `in_place: true`. An intent with a `base_branch` or an `origin` on a plain repository fails with `NeedsSourceControl` before any agent is made.

- [ ] **Step 5: The queue.** `next_startable` skips an intent whose repository is in `held`, without counting it against the free slots. `held_folders` computes the set as under Interfaces. The scheduler's loop and `send_now` compute it afresh before each decision, as they do `running`. Resolving an intent and closing an agent already wake the scheduler; check both with the two queue tests.

- [ ] **Step 6: Run the gate.** Expected: everything passes.

- [ ] **Step 7: Commit** — `feat(server): intents on a plain folder start in place and take turns`.

---

### Task 5: Feedback and the META repository

**Files:**
- Modify: `crates/server/src/feedback/mod.rs`, `crates/server/src/feedback/idea.rs`, `docs/feedback.md`
- Test: `crates/server/tests/feedback_api.rs`

**Interfaces:**
- Consumes: Task 1's `effective_meta_repo` and `Repo::is_plain`.
- Produces: no new type. A feedback call that names a plain repository is refused with status 409, code `needs_source_control`, message `Feedback needs a repository with source control.`

- [ ] **Step 1: Write the failing tests**

  ```rust
  #[tokio::test]
  async fn plain_repositories_are_not_feedback_targets() {
      // a Workspace with one plain and one git repository
      // GET /v1/feedback/targets: repos lists only the git one, and meta_repo_id is it
      // with only the plain repository: repos is empty and meta_repo_id is null
  }

  #[tokio::test]
  async fn a_submission_to_a_plain_repository_is_refused_before_anything_is_written() {
      // POST /v1/feedback naming the plain repository -> 409 needs_source_control,
      // "Feedback needs a repository with source control."
      // nothing was written into the folder, and no submission is recorded
  }

  #[tokio::test]
  async fn the_meta_repository_cannot_be_a_plain_folder() {
      // setting the META repository to the plain one -> 409 needs_source_control
      // the META repository's source control is turned off (PATCH): targets then
      // answer meta_repo_id null; turned on again: it is the META repository again
  }
  ```

- [ ] **Step 2: Run them and see them fail** — `cargo test -p smithagents-server --test feedback_api plain 2>&1 | head -20`.

- [ ] **Step 3: Refuse and filter.** `targets` lists repositories that are not plain. Submit, retry, the idea actions and the META setting refuse a plain repository with the error above, before any file or record is written. The work done for each repository when the daemon starts (removing feedback leftovers) skips plain repositories.

- [ ] **Step 4: Document it** in `docs/feedback.md`: Feedback and the META repository need a repository with source control, and what the Submit screen says when there is none.

- [ ] **Step 5: Run the gate.** Expected: everything passes.

- [ ] **Step 6: Commit** — `feat(server): feedback and the META repository need source control`.

---

### Task 6: The app

**Files:**
- Modify: `crates/app/src/link.rs`, `crates/app/src/model.rs`, `crates/app/src/view_models.rs`, `crates/app/src/views/settings/repositories.rs`, `crates/app/src/views/settings/mod.rs` (comments), `crates/app/src/views/agents/close.rs`, `crates/app/src/views/intents/actions.rs`, `crates/app/src/intent_vm.rs`, `crates/app/src/feedback/text.rs`, `crates/app/src/feedback/submit_vm.rs`
- Test: `crates/app/tests/view_models.rs`, `crates/app/tests/settings_pane.rs`, `crates/app/tests/meta_repo.rs`, `crates/app/tests/agents_pane.rs`, `crates/app/tests/intent_actions.rs`, `crates/app/tests/feedback_submit_vm.rs`, `crates/app/tests/link.rs`

**Interfaces:**
- Consumes: Task 2's client calls and `Health.git`, the codes of Tasks 2 to 5.
- Produces:
  ```rust
  // link
  pub fn add_repo(&self, path: &Path, choice: AddSourceControl) -> impl Future<Output = Result<Repo, LinkError>>;
  pub fn set_source_control(&self, id: RepoId, on: bool) -> impl Future<Output = Result<Repo, LinkError>>;
  // model
  Action::RepoUpdated(Repo)        // replaces the repository of that id

  // view_models
  pub struct RepoRowVm { /* … */ pub source_control: String,   // the line shown, below
                         pub has_source_control: bool }
  // these two replace the `default_branch: Option<String>` Task 1 left on the row
  ```
- Exact texts:
  - Row line: `Source control: None`, or `Source control: Git, default branch <name>`.
  - Add choices, in this order, the first selected: `A folder, no source control`, `An existing git repository`, `A new git repository`.
  - Row buttons: `Turn on source control`, `Turn off source control`. Turning it off for the META repository asks first: `Intents and feedback use this repository. With source control off, this Workspace has no repository for them until you choose another. Turn it off?`
  - Git choices disabled: `git is not installed`.
  - Closing an in-place agent: `Its chat tab closes and it leaves the list. The folder is not touched.`
  - Resolved hint for an intent on a plain repository: `This means the work is finished in the folder, so intents that depend on it can start.`
  - Submit screen with no source-controlled repository: `Feedback needs a repository with source control.`

  | Code | Sentence |
  |---|---|
  | `folder_busy` | `Another agent is working in this folder.` |
  | `repo_in_use` | `Close this repository's agents first.` |
  | `repo_overlaps` | `That folder overlaps another repository of this Workspace.` |
  | `repo_has_feedback_intents` | `Resolve this repository's feedback intents first.` |
  | `needs_source_control` | `That needs a repository with source control.` |
  | `git_not_installed` | `git is not installed.` |
  | `git_identity_missing` | `git has no name and email to commit with. Set them, then try again.` |
  | `folder_not_empty` | `A new repository needs a folder that is empty or does not exist yet.` |
  | `not_repo_root` | `That folder is inside a git repository. Add the repository's own folder.` |

- [ ] **Step 1: Write the failing tests**

  ```rust
  // crates/app/tests/view_models.rs
  #[test]
  fn a_repository_row_says_its_source_control() {
      // rows of [plain, git("main"), git("trunk")] with the second stored as META
      assert_eq!(row(plain).source_control, "Source control: None");
      assert_eq!(row(git_main).source_control, "Source control: Git, default branch main");
      assert!(!row(plain).can_set_meta && !row(plain).is_meta && !row(plain).has_source_control);
      assert!(row(git_trunk).can_set_meta);
      // with [plain, git("main")] only: nothing to choose, and git_main is META
  }

  #[test]
  fn each_new_refusal_has_its_sentence() {
      // for every row of the table above: repo_error_text(&api(409, code, "x")) == sentence
  }

  // crates/app/tests/settings_pane.rs
  #[gpui_kit::test]
  fn adding_a_repository_asks_which_of_the_three_it_is(cx: &mut TestAppContext) {
      // the three choices are there in order and the first is selected
      // adding with each choice calls the link with None, Git and NewGit
      // with Health.git false the two git choices are disabled and say "git is not installed"
  }

  #[gpui_kit::test]
  fn a_row_turns_source_control_on_and_off(cx: &mut TestAppContext) {
      // a plain row shows "Turn on source control"; pressing it calls set_source_control(id, true),
      // and the row then reads "Source control: Git, default branch main"
      // the META row asks the question above before turning it off
      // a refusal (repo_in_use) shows its sentence and the row is unchanged
  }

  // crates/app/tests/agents_pane.rs
  #[gpui_kit::test]
  fn closing_an_in_place_agent_does_not_speak_of_a_worktree(cx: &mut TestAppContext) {
      // an agent with branch None: the dialog says "… The folder is not touched."
      // an agent with a branch: the dialog says "… Its worktree stays on disk." as before
  }

  // crates/app/tests/intent_actions.rs: the Resolved hint by repository kind
  // crates/app/tests/feedback_submit_vm.rs: no source-controlled repository -> the sentence
  // crates/app/tests/link.rs, against the real daemon:
  #[tokio::test]
  async fn a_plain_folder_is_added_and_then_given_source_control() {
      // add a folder that is a git repository with None -> Repo.source_control None
      // set_source_control(id, true) -> Git with its default branch
  }
  ```

- [ ] **Step 2: Run them and see them fail** — `cargo test -p smithagents-app --test view_models a_repository_row 2>&1 | head -20`.

- [ ] **Step 3: The link and the model.** `add_repo` passes the choice; `set_source_control` dispatches `Action::RepoUpdated` with the answer. The model keeps `Health.git` from the connect-time snapshot.

- [ ] **Step 4: View models and sentences**, as under Interfaces. `can_set_meta` and the "there is a choice" count use only repositories with source control.

- [ ] **Step 5: The Settings section.** The add form gains the three-way choice above its path field; the row gains its line and its button. The section keeps the title `Source repositories`. Comments that say "Space scope" say "Workspace scope".

- [ ] **Step 6: The other texts**: the close dialog, the Resolved hint, the Submit screen, and `folder_busy` wherever a failed send is shown.

- [ ] **Step 7: Run the gate.** Expected: everything passes.

- [ ] **Step 8: Commit** — `feat(app): choose a repository's source control when adding it and on its row`.

---

## After the last task

- The whole-branch review reads the diff against the spec's "Worth a test" list and the Review Focus above.
- Nothing is pushed and no pull request is opened until the owner asks.

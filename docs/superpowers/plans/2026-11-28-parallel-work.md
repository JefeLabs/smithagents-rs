# Parallel work: who builds what, where, and in what order

Read this before starting or merging any part of the roadmap. It says which
parts can run side by side, what each one may touch, the numbers that must not
collide, and the order things merge in. The roadmap itself, with the reasons,
is in `docs/superpowers/specs/2026-11-23-smithagents-architecture-design.md`,
"Roadmap after folder mode (2026-11-28)".

Coordinator: the session that keeps this file up to date. On 2026-11-28 that
is the session named `smithagents-rs-51`. A session that takes a track writes
its name in the board below (in its own branch, or by telling the coordinator)
before it starts. When no coordinator session is running, this file is the
coordination: follow it, and record what you took.

## The board

Last updated 2026-11-28.

| Track | Part | State | Branch | Worktree | Taken by | Waits for |
|---|---|---|---|---|---|---|
| Folder mode | source control optional per repository | built; last review of the fixes running | `feat/folder-mode` | `.worktrees/workspaces` | `smithagents-rs-51` | the owner: pull request and merge |
| D1 | plugin typed data and tools in the daemon | design approved, plan written (`plans/2026-11-28-plugin-typed-data-and-tools.md`, not committed yet) | `feat/plugin-typed-data` | `.worktrees/plugin-typed-data` | nobody yet | folder mode merged, or stacked on it |
| C1 | resource and usage limits | facts gathered, no design | `design/resource-limits` | `.worktrees/resource-limits` | nobody yet | a design the owner approves |
| B1 | several Workspaces and remote runtimes | decisions recorded, no design | `design/remote-runtimes` | `.worktrees/remote-runtimes` | nobody yet | a design the owner approves |
| A1 | secrets service and the agent environment | decisions recorded, no design | `design/secrets-service` | `.worktrees/secrets-service` | nobody yet | D1 merged |
| D2 to D7, A2, A3, B2, B3 | see the roadmap | not started | | | | the part before each |

Branch and worktree names are reserved, not made: make them when the work
starts, from `main` as it is then.

## What may run side by side

- **Now:** D1's build, C1's design and build, B1's design. They touch
  different parts of the code (below).
- **Not with D1:** anything in chain A, and anything else that edits plugin
  manifests, the plugin host or `docs/plugins.md`. D1's first task renames
  `[[tools]]` to `[[commands]]` and `from_tool` to `from_command` across all
  of them.
- **At most two builds at once on one machine.** The full check uses every
  core for about six minutes, and timing-sensitive tests and the known
  `plugin-protocol` crash show up more often under load. Designs cost nothing
  and can always run.

## Who touches what

| Area | Folder mode | D1 | C1 | B1 | A1 |
|---|---|---|---|---|---|
| `crates/core` types on the wire | `Repo`, `Agent` | plugin status types | usage on a turn, limits in agent defaults | health, connection info | secret and environment declarations |
| `crates/server/src/store.rs` and migrations | `repos`, `agents` | plugin item values | turn usage | | |
| `crates/server/src/api.rs` routes | `/v1/repos` | `/v1/plugins/...`, catalog | usage and limits routes | listener, TLS | secrets and environment routes |
| `crates/server/src/queue.rs`, `intents.rs` | the held-folder rule | | the limit gates | | |
| `crates/server/src/harness/claude.rs` | the working-folder check | | reading usage and the rate-limit line | | the agent's environment |
| `crates/plugin-protocol`, `crates/plugin-host`, `plugins/` | | all of D1 | | | secret sources (after D1's rename) |
| `crates/client` | repository calls | plugin calls | usage calls | connecting to a remote daemon | |
| `crates/app` Settings pane | "Source repositories" | rename only | "Agent defaults" | a Workspace switcher, start page | a secrets section |
| `crates/app` elsewhere | intent and agent texts | | status bar, intent card reason | shell, link | |
| `docs/plugins.md` | the feedback API reference | most of it | | | the secrets part |

Two tracks that share a cell will conflict in that file. That is expected and
is settled by the merge order: the later one rebases.

## Numbers that must not collide

Take a number here before you write the file that uses it.

| Thing | In use | Reserved | Rule |
|---|---|---|---|
| Database migrations (`crates/server/migrations`) | `0001` to `0004` on `main`; `0005_source_control` (folder mode) | `0006` for D1; `0007` for C1 | A migration's number is final once it is on `main`. An unmerged branch renames its file if another branch merged first; a development database made from the unmerged branch is then thrown away. |
| `smithagents_core::PROTOCOL` | `1` on `main`; `2` (folder mode) | none | It goes up by one, at merge time, in every branch that changes what goes over the wire incompatibly: rebase, then take `main`'s number plus one. |
| Plugin contract version | `1` | `2` for D3 (actions) | D1 keeps it at 1 on purpose. |
| Error codes | the nine of folder mode (see its plan) | | A code is never reused with another meaning. Search `status_and_code` before adding one. |

## The order things merge in

1. Folder mode merges first. Everything else is based on it.
2. D1 before anything in chain A.
3. Otherwise first ready, first merged. Before its last full check a branch
   rebases onto `main`; the check that counts is the one after the rebase.
4. A branch is merged with a merge commit pinned to the commit that was
   checked, and only when the owner says so.

A branch may be started before folder mode is merged by basing it on
`feat/folder-mode` and rebasing onto `main` once that has merged. Say so on
the board.

## Rules every track follows

- One implementer per worktree at a time. Reviewers only read.
- The full check, judged by `cargo test`'s exit status and never by adding up
  pass counts: `cargo test --workspace --no-fail-fast`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --all --check`, `cargo themegen check`, and no test
  `smithagentsd` left running. Before starting one, look for another
  (`pgrep -fl 'cargo test --workspace'`) and wait for it.
- Known flakes, not yours to fix in passing: the `smithagents-plugin-protocol`
  lib tests rarely abort inside `stabby` under load; wall-clock tests in
  `crates/server/tests/claude_harness.rs`.
- Tests never run a real `gh`, `claude`, `mise` or `agent-browser`, never
  capture the screen, never open a native dialog, never launch the app.
- The repository is public: no names, emails, tokens or home paths in
  anything committed.
- The grouping is a Workspace, never a "Space". A plugin's command-line
  program is a "command" once D1 has merged.
- Nothing is pushed, merged or installed unless the owner asks.
- A part is built only from a design the owner approved and a plan written
  from it.

## Open with the owner

- Folder mode: open the pull request and merge.
- Who builds D1, and whether it starts now, stacked on folder mode, or after
  the merge.
- The plugin plan and its design are not committed: they sit untracked in the
  root checkout.
- Folder mode's queue rule: an intent holds its plain folder until it is
  resolved or its agent is closed (built), or only while an agent is
  mid-turn.
- C1: limits set by the owner (the working assumption) or worked out by the
  daemon; the token budget per day or per week (the owner asked for either).

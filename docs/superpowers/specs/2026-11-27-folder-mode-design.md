# Folder mode: source control is optional per repository (Design)

Status: approved by the owner 2026-11-27 ("yes continue"); the question under
"Asked of the owner" was not answered, so the designer's pick stands.
Plan: `docs/superpowers/plans/2026-11-27-folder-mode.md`.
Part 1 of the order in the architecture design
(`2026-11-23-smithagents-architecture-design.md`, "Owner decisions of
2026-11-27").

Legend: **[decided]** = stated by the owner. **[my call]** = proposed by the
designer; the owner should check these (collected near the end).

## Intent

An agent's turn needs a folder to work in and nothing else. Today SmithAgents
still refuses a folder that is not a git repository, and gives every agent a
git worktree and a branch. So it cannot be used on notes, data, a scratch
project, or a machine without git.

Source control becomes something a repository of the Workspace has or does
not have. A repository without it is a plain folder, and that is the default.
[decided] Turning it on gives what exists today. Later parts let a provider
plugin supply the host (GitHub first); this part only makes "none" possible.

## Goals

- Any folder can be added as a repository of the Workspace.
- A newly added repository has no source control until the owner chooses it.
  [decided: "the default mode is folder mode"]
- On a repository without source control, everything that does not need git
  works: chat with an agent, polishing an intent, starting an intent's agent,
  the queue and dependencies.
- Turning source control on for a folder that is a git repository gives
  today's behaviour: a worktree and a branch per agent, commits, the pull
  request.
- A new git repository can be made from the app. [decided, from the owner's
  outline: "new local source-controlled folder"]
- Repositories that exist before the upgrade keep source control, with
  nothing for the owner to do.
- The daemon and the app run on a machine where git is not installed.

## Non-goals

- Cloning from a provider. It arrives with hosts through plugins (part 4).
- Undo or snapshots of a plain folder.
- Two agents working in the same plain folder at once.
- Stopping a running turn. A turn that hangs holds its folder, and the
  queue behind it, until it ends. Nothing cancels a turn today in a git
  repository either; here one hung turn now blocks more.
- Feedback into a plain folder, or a plain META repository.
- Hosts as plugins, the secrets and sign-in services, several Workspaces,
  remote runtimes: parts 2 to 6.
- Other version-control systems.
- Renaming the `Repo` type, its table or its routes.

## Anti-goals

- A repository that had source control before the upgrade working in place
  after it.
- The daemon telling an agent to commit, branch, push or open a pull request
  in a repository without source control, even when that folder happens to
  be a git repository.
- The daemon running `git` for a repository without source control.
- Two agents changing the same plain folder at the same time.
- A control that needs source control shown on a plain repository, where it
  can only fail.

## Constraints

- The shapes of `Repo` and `Agent` change on the wire, so `PROTOCOL` goes
  from 1 to 2. The app and the daemon ship together, and a daemon of the
  other protocol is already reported by the version banner.
- The store changes through a new migration; existing rows must come through
  it as described under "Upgrade".
- The project's standing rules apply: tests never run a real `gh`, `claude`,
  `mise` or `agent-browser`; no personal data in anything committed;
  `unsafe` stays forbidden.

## Approaches

Three ways to let an agent work in a folder that has no source control:

| | Agents at once | Undo | Needs git | Size |
|---|---|---|---|---|
| A. In place, nothing else | one per folder | none | no | small |
| B. In place, a snapshot before each turn | one per folder | per turn | no | medium |
| C. A hidden repository in the daemon's home | several | yes | yes | large |

- **A** runs the agent in the folder itself.
- **B** adds a copy of the folder before each turn. It brings snapshot
  storage and cleanup, large folders such as `node_modules`, and a restore
  screen.
- **C** gives plain folders worktrees without a `.git` in them. It needs git,
  which defeats the purpose, and nothing merges an agent's worktree back into
  the live folder.

**Chosen: A.** [decided: the owner agreed to "an agent in a plain folder
works in place, one at a time, with no snapshots"] It is the smallest thing
that meets the intent, and B can be added later without changing the record.

## The design

### The record

```rust
// core::agent
pub struct Repo {
    pub id: RepoId,
    pub path: PathBuf,
    /// `None`: a plain folder.
    pub source_control: Option<SourceControl>,
    pub created_at: DateTime<Utc>,
}

#[serde(rename_all = "snake_case")]
pub enum SourceControl {
    Git { default_branch: String },
}
```

- The name and the `Option` shape are [decided]. `default_branch` moves from
  `Repo` into `Git`, because a plain folder has none. [my call]
- On the wire: `"source_control": null`, or
  `{"git": {"default_branch": "main"}}`.
- The host (`host: Option<PluginId>` in the shape the owner agreed to) is
  added by part 4, with a default, when something uses it. [my call]
- `Agent.branch` becomes `Option<String>`: `None` for an agent made on a
  plain repository. `Agent.worktree_path` keeps its name and holds the folder
  the agent works in, which for a plain repository is the repository's own
  path. [my call: no rename, to keep the change small]
- An agent keeps the way of working it was created with for its whole life.

### Adding a repository

`POST /v1/repos` takes `path` and a `source_control` choice with three
values. [the three come from the owner's outline; the names are my call]

| Choice | The path must be | What the daemon does |
|---|---|---|
| `none` (the default when absent) | an existing folder | records it as it is; runs no git |
| `git` | inside a git repository that is not bare and has a default branch | what `add_repo` does today: records the repository's root and default branch |
| `new_git` | missing, or an empty folder | creates the folder, `git init` with the branch `main`, and one empty first commit, so that worktrees have something to branch from |

- `none` keeps the path it was given (made absolute, links resolved); it does
  not look for a repository root above it.
- A path that is already a repository of the Workspace is refused, as today.
- A plain repository may not overlap another repository of the Workspace:
  adding it is refused when its folder contains, or lies inside, another
  repository's folder or the daemon's home. Otherwise two agents could
  change the same files through two repositories. Two git repositories may
  still nest, as today. [my call]
- `new_git` fails with a sentence of its own when git has no identity for the
  first commit, and removes the folder it created. [my call]
- `git` and `new_git` on a machine without git answer `git_not_installed`
  with a sentence that says so.

### Turning source control on or off

`PATCH /v1/repos/{id}` with a `source_control` of `none` or `git`. [my call]

- `none` to `git`: the folder must be the root of a git repository, checked
  as in "Adding". A plain folder that is a subfolder of one is refused: the
  owner adds the root instead.
- `git` to `none`: refused when the folder would then overlap another
  repository (see "Adding"), and while the repository has an intent made by
  Feedback that is not Resolved: such an intent starts from a branch and its
  files are on one. [my call]
- Turning it off for the META repository is allowed and clears the META
  choice; the question the app asks first says so. [my call]
- Both directions are refused with 409 `repo_in_use` while the repository has
  an agent that is not closed. [decided]
- Worktrees and branches of closed agents stay where they are.

### An agent on a plain repository

- Creating it runs no git. Its working folder is the repository's path. If
  that folder is gone, creation fails with the existing "no directory at …"
  error.
- **One at a time.** [decided] At most one turn is in flight among the agents
  of one plain repository. Several agents may be open on it. [my call]
  - Sending a message to an agent while another agent of the same plain
    repository is working answers 409 `folder_busy`: "Another agent is
    working in this folder."
  - Send on a plain repository whose folder is not free queues the intent,
    as Send does today when the queue is full. `folder_busy` is answered
    only to a chat message. [my call]
  - The queue starts an intent on a plain repository only when the folder is
    free. The intent stays Queued meanwhile, with no start error recorded.
    For the queue, "free" means more than "no turn in flight": see "Asked of
    the owner". [my pick until answered]
  - Polish runs do not count: they only read.
  - Closing an agent stays refused while its turn runs, as today, so a turn
    always ends before its folder is free.
- A folder that disappears later fails the turn with the existing "no
  working folder at …" message.

### What the agent is told

The system-prompt append for an agent on a plain repository, in place of
today's "Work only in this worktree … Commit your work, and open the pull
request": [my call on the wording]

> Work only in this folder. Satisfy every acceptance criterion in the intent
> you are given by changing the files here directly. This folder is not under
> source control for this work: do not create commits or branches, do not
> push, and do not open a pull request, even if you find a git repository
> here. When you are done, say what you changed.

The sentence that follows "Depends on", in place of the instruction to fetch
and rebase:

> The work of these dependencies was done in this same folder and is already
> here.

### Intents

- An intent on a plain repository has no base branch. Only Feedback sets
  one, Feedback does not target a plain repository, and source control
  cannot be turned off while such an intent is unresolved, so none should
  exist. A start that meets one all the same fails with a sentence and runs
  no git.
- "Open pull request" and anything else that needs a branch is not offered.
- Dependencies work unchanged: an intent waits until the ones it depends on
  are marked Resolved, and nothing checks a merge. The hint shown with
  Resolved ("the work has landed on the default branch") gets a wording for
  a plain repository that does not mention a branch.

### Feedback and the META repository

- Only repositories with source control are Feedback targets: the tool works
  by committing files. [decided] A submission that names a plain repository
  is refused.
- The META repository must have source control. [decided, "at first"]
  - A stored META id that points at a plain repository counts as not set.
  - "With a single repository it is that one" counts only repositories with
    source control.
  - "Use for intents and feedback" is offered only on those rows.
  - A Workspace with no repository under source control has no META
    repository. That is a normal state, since a new repository has none.
- When no repository has source control, the Submit screen says: "Feedback
  needs a repository with source control."

### The app

- **Settings, "Source repositories".** "Add repository" asks which of the
  three it is: a folder with no source control (selected first), an existing
  git repository, or a new git repository. Each row shows "Source control:
  None" or "Source control: Git, default branch main", and has the control
  that turns it on or off.
- The section keeps its title. [my call; the owner calls a plain folder a
  repository of the Workspace too]
- The daemon does not run git until something asks for it: on a Mac
  without the developer tools, running git opens the system's install
  dialog. So the git choices are always offered, and choosing one where git
  cannot run answers "git is not installed."
- Closing an agent on a plain repository says: "Its chat tab closes and it
  leaves the list. The folder is not touched."
- Nothing shows a branch, a worktree or a pull request for a plain repository
  or its agents.
- Each new refusal (`folder_busy`, `repo_in_use`, `git_not_installed`, the
  META one) has its own sentence, like the existing repository errors.
- A queued intent on a plain repository says why it waits when no
  dependency holds it. While another intent of the repository is Sent and
  its agent is still open (the one sent first, by its title):
  `Waiting for "<that intent's title>" to be resolved or its agent closed. One intent works in this folder at a time.`
  Else, while an agent that works in place there is Working or Quiet:
  `Waiting: another agent is working in this folder.` Otherwise, and on a
  git repository, the slot sentence stays.

### Upgrade

- The migration gives every existing repository `Git` with the default
  branch it has stored, and every existing agent keeps its branch.
- Nothing at startup may fail because git is missing. A repository with
  source control on such a machine reports the problem when it is used.

## Left to the builder

- How the `repos` table changes (SQLite needs a rebuild to drop a NOT NULL
  column), and how `source_control` is stored.
- Where the claim "a turn is in flight in this repository" lives, beside the
  per-agent reservation that exists.
- The exact error-code names beyond those written here, kept in the style of
  the existing ones, and the HTTP status of each.
- Where the "can run git" fact is reported and when it is refreshed.
- The layout of the three-way choice and of the row's control, and the
  wording of sentences not quoted here, in the app's existing voice.
- The tests, with the cases under "Worth a test".

## Worth a test

Cases the design implies that are easy to miss:

- A folder that is a git repository, added with `none`: no git process runs
  for it, at creation or during a turn, and the agent is not told to commit.
- Two agents on one plain repository: the second message is refused while the
  first turn runs, and accepted once it ends, however it ends: done, failed,
  or the daemon restarted. Closing the first agent is refused meanwhile.
- A plain folder inside another repository's folder, around one, or inside
  the daemon's home is refused, on adding and on switching.
- Source control cannot be turned off while an unresolved Feedback intent
  exists, and turning it off for the META repository leaves no META.
- Two queued intents on one plain repository with a queue limit of two or
  more: they run one after the other, and an intent on another repository is
  not held back by them.
- A database from before the migration: its repositories and agents read
  back with source control and their branches.
- Switching with an open agent is refused both ways, and allowed after the
  agent is closed.
- The META repository: a plain repository is never picked, also when it is
  the only repository.
- The daemon with no `git` on its `PATH`: it starts, a plain repository can
  be added and used, and the git choices answer `git_not_installed`.

## Decisions from the builder check [my calls]

A second reader planned from this document and asked six questions. Five are
settled above:

1. Git-dependent intents when source control is turned off: the switch is
   refused while an unresolved Feedback intent exists.
2. Send on a busy folder queues; it is not refused.
3. A plain repository may not overlap another repository or the daemon's
   home.
4. Turning source control off for the META repository clears META instead of
   being refused, so the last such repository is not locked in.
5. Closing an agent mid-turn stays refused; stopping a turn is a non-goal.

## Asked of the owner, not yet answered: my pick stands until changed

**When is a plain folder free for the next queued intent?**

- **My pick: when the earlier intent is finished with it.** An intent holds
  its folder from the start of its agent until the intent is Resolved or its
  agent is closed. Queued intents on that folder wait. Work in the only copy
  of the files is never interleaved, but the queue on a plain folder moves
  only when the owner resolves an intent.
- **The other way: as soon as no turn is running.** The next intent starts
  when the earlier agent ends a turn, also when it ended with a question or
  half-done. The queue moves by itself, and a second agent can build on
  unfinished work, with no undo.

A chat message is the same either way: refused only while a turn runs.

## Suggested split into plans

1. The record: core types, migration, protocol number, `POST` and `PATCH`,
   the client.
2. Agents in place: creation, one at a time, the brief, intents and the
   queue.
3. Feedback and META restrictions.
4. The app: the add flow, rows, sentences.

## Calls the designer made (owner to verify)

1. `default_branch` lives inside `SourceControl::Git`; the host field waits
   for part 4.
2. `Agent.branch` is optional; `worktree_path` keeps its name.
3. The three add choices are named `none`, `git` and `new_git`; `new_git`
   makes the branch `main` and one empty first commit.
4. Switching is a `PATCH` on the repository, and a subfolder of a git
   repository cannot be turned into one.
5. Several agents may be open on a plain repository; only one works at a
   time. A queued intent waits silently for a busy folder.
10. The five decisions from the builder check, and the pick under "Asked of
    the owner".
6. Polish runs are not counted as work in the folder.
7. The wording given to the agent, and the sentences quoted for the app.
8. The Settings section keeps the title "Source repositories".
9. The git choices are always offered; where git cannot run, choosing one
   says so, and the daemon runs no git before then.

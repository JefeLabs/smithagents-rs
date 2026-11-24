# SmithAgents: Context Console Architecture (Design)

Status: draft for approval (revision 2). Date: 2026-11-23.
Predecessor: "Intent IDE" v0.20 (pure TypeScript). This is a Rust rewrite.

Legend: **[decided]** = stated by the owner. **[my call]** = proposed by the
designer; owner should check these (collected in section 9).

## 1. Intent and why

A "context console": the user turns a rough input (a chat message, a Jira or
Linear ticket, a GitHub issue) into an **intent**, then sends agents to
complete it. An intent is the agent's worker contract: preconditions,
acceptance criteria, user stories. On input, the agent polishes it and asks up
to 5 steering questions, each answered by picking from options. Everything the
user sees on screen (issues, PRs, builds, vulnerabilities, logs) is also
available to agents as context.

Why rewrite: the TypeScript app keeps its control plane inside the desktop
process. The agent queue only advances while the app is open (routines already run in
the workspace, per the routine editor's note), and a lost workspace connection
stalls work. Each added integration also adds concurrent
sync, polling and caching load in a single event loop. The rewrite moves the
control plane into a Rust daemon that runs wherever the work runs, with thin
clients attached.

## 2. Goals, non-goals, anti-goals

Goals
- Daemon-owned queue, routines, agents and context sync that keep advancing
  with every client closed (for remote daemons).
- A fast native desktop shell (left, bottom, right panes) with a rich web
  center for chat, terminal and graphs.
- Plugins for context sources, releasable individually, isolated from the daemon.
- UI composed on demand by agents (A2UI) from a styled component set.
- Two-way sync with source tickets, gated by explicit user approval.

Non-goals
- Not a general code editor or full IDE.
- Not building an agent harness; agents run through existing headless CLIs or
  a custom SDK behind an adapter.
- No multi-tenant SaaS; single user, one daemon per Space.
- v1: no browser host, no mobile/tablet host, no Rust A2UI renderer for panes.

Anti-goals (failure even if it "works")
- Work stalls because a laptop slept or the app closed (remote daemons).
- A misbehaving plugin takes down the daemon or other plugins.
- Any plugin write to Jira/GitHub/etc. happens without user approval.
- Credentials persisted by the daemon or exposed to the UI.
- Pane and center state drifting out of sync (daemon is the single source of truth).

## 3. Constraints

- As much Rust as possible; web tech only in the center pane's tabs. [decided]
- Left, right and bottom panes are native, built with GPUI (gpui-component
  docking). [decided]
- Center pane holds tabs rendered by web views: chat, terminal (xterm.js),
  flow (react-flow), more later. [decided]
- Daemon is always a separate process: on the local machine (started by the
  desktop app) or standalone on Coder, RunPod or any SSH host. [owner
  proposed local-as-separate-process to keep behavior consistent with remote;
  this replaces the earlier "embedded" idea]
- Agents are invoked via headless CLI or custom SDK. [decided]
- Desktop uses native Chromium (CEF) with CDP, not Tauri, so the remote backend
  can integrate with it. [decided]
- Web hosts (browser, Coder web) and mobile/tablet (Tauri) are wanted but
  deferred. Their panes would need a React implementation because GPUI has no
  browser target. [my call: desktop-first]

## 4. Architecture

### 4.1 Layout

```
┌ Left ───────┐┌ Center: web tabs ───────────────┐┌ Right ────────┐┌─┐
│ Spaces      ││ [Chat] [Terminal] [Flow] [+]    ││ Settings      ││▣│ tool
│ Agents      ││  Chat     AI Elements + A2UI    ││ Item detail   ││▣│ icon
│ Queue       ││  Terminal xterm.js <-> daemon   ││ Integration   ││▣│ rail
│ Routines    ││  Flow     react-flow            ││ config        ││▣│
│ (full       │├─────────────────────────────────┴┴───────────────┤│▣│
│  height)    ││ Bottom: view for the pressed tool icon            ││▣│
└─────────────┘└───────────────────────────────────────────────────┘└─┘
```

- Left pane: full height. Space switcher (with connection and "browser
  signed out" status), personal dashboards (e.g. "Start of Day"; rendered in
  a center tab, a natural home for agent-composed UI [my call]), agents (named
  sessions with status dot), agent queue, routines (scheduled / on demand).
- Bottom pane: under center and right. Shows the view for the tool icon
  pressed in the icon rail (far right). Views: agent sessions, GitHub
  repositories, GitHub issues, GitHub pull requests, Jira tickets, Confluence
  pages, Nexus published artifacts, AWS pipelines and builds, Snyk
  vulnerabilities, Datadog application logs. Every integration has settings.
  [decided]
- Right pane: shows the view chosen in the icon rail's top two groups
  [decided, from screenshots]:
  - Space-context group: Notifications, Settings (incl. per-Space
    integrations), Intents (list with state filter, create "+"; selecting
    one opens the intent editor), Feedback, Terminal.
  - Agent/worktree-context group (follows the agent selected on the left):
    Active intent, Folder tree, Changed files, Browser, Terminal.
  - Terminal and Browser buttons focus or open a center tab (a shell in the
    Space, a shell in the agent's worktree, a browser tab).
  - A third rail group lower down selects the bottom pane view (below).
- Center tab strip holds dashboards, Browser and terminal tabs (several per
  Space) among others; a connection banner ("Reconnecting to <space>") sits
  above the center. [from screenshots]
- Settings (right pane, collapsible sections) [owner: "suggestions"]:
  - Space scope: Agent defaults (default provider with per-provider model and thinking
    level, agent name pool, queue limit, resume message and auto-nudge),
    Source repositories (repos the Space has, add from its organizations),
    Integrations (each bottom-pane tool's per-Space setup).
  - App scope: Secrets (agent tokens from the Keeper vault), Sign-ins (how
    often the app checks), Appearance (theme, text size, font), Updates.
- Sign-ins: the Space card shows "n signed out"; a popover lists Coder,
  Keeper, Jira and Browser with Signed in/out, Sign in, Check now, Sign out
  and "checked Ns ago". [from screenshot]
- All panes collapsible. [decided]
- gpui-component's dock may need adjustment to get "left full height, bottom
  under center + right"; verify in the first spike. [my call]

### 4.2 Cargo workspace (monorepo)

```
crates/
  core/              domain types (intent, session, resource, view schema,
                     pending change), client trait, state store
  plugin-protocol/   versioned plugin contract + plugin SDK
  server/            daemon (lib + bin): queue, scheduler, store, plugin host,
                     agent runner, browser manager, PTY service, API
  app/               GPUI shell: panes, icon rail, generic view renderer,
                     tab strip, center-tab host interface
  host-desktop/      binary: window, local daemon launcher, CEF center tabs,
                     bridge; bundles the `server` binary as a sidecar
web/                 the one TypeScript package: center-tab web app
                     (chat, terminal, flow); talks to the daemon directly
plugins/
  github/ jira/ confluence/ nexus/ aws-pipelines/ snyk/ datadog/ ...
                     each: own version, plugin.toml, binary
deferred hosts:
  host-web, host-web-coder, host-tauri (mobile/tablet)
dev harness (in scope):
  host-web-mock      web/ app against a fake daemon, for UI work and tests
```
[my call] on names; the host split (app, core, hosts, server) is [decided].

### 4.3 Runtime topologies

1. Desktop local: `host-desktop` starts (or finds) a local `server` process.
   The GPUI panes and the web tabs both connect to it over loopback HTTP and
   WebSocket, the same transport a remote daemon uses through a tunnel.
2. Desktop + remote: `server` on a Coder workspace, RunPod pod or SSH host.
   The desktop connects through an SSH tunnel to the daemon port. Panes use the
   client API; web tabs use the same tunneled WebSocket/HTTP endpoints.
3. Mock: `host-web-mock` for the web app.

Native panes and web tabs are both clients of the daemon and subscribe to the
same change feed. A small host-local event bus coordinates UI actions (for
example "open this issue in a center tab").

### 4.4 Control plane (`server`)

- **Intents**: filterable states in Intent IDE are Draft and Polishing,
  Complete and Queued, Sent, Resolved; Sent shows a sub-state (e.g. "done").
  Finer transitions are unknown until the Intent IDE source is seen. Intents
  have a slug, a kind (e.g. Enhancement), title, current-behavior text and an
  outline. Versioned; the app owns the intent and source tickets are input
  references. Intents have dependencies ("Depends on"); an intent starts only
  when all dependencies are Resolved, as in Intent IDE.
- **Review workflow**: agent-proposed preconditions, acceptance criteria and
  user stories arrive as "new" items the user accepts or edits (counters like
  "0 of 12 accepted"). This is the same proposal-and-approval pattern as
  plugin write-backs; one pending-change mechanism serves both. [my call]
- **Steering**: up to 5 questions with options, generated by the agent and
  shown in the native intent editor in the right pane (question card, option
  chips with keyboard shortcuts, Skip, "n of m answered"), then folded into
  the intent version. These are typed data rendered by a native question-card
  component, not generic A2UI. [from screenshots; typed-vs-A2UI is my call]
- **Queue**: lives in the daemon. A concurrency limit counts only agents
  actively replying; Blocked agents (waiting on the user) don't hold slots;
  agents the user starts manually bypass the limit (Intent IDE behavior).
- **Agents**: long-lived named sessions with harness (CLI or SDK adapter),
  model and thinking level. Each session records intent, branch/worktree, PR,
  message count, last activity. Details from the Agent defaults screenshot:
  - Unattended [decided]: agents run unattended in their own worktrees. Only
    plugin writes need approval; an agent's own actions (git commit and push,
    opening PRs, shell commands, driving a browser over CDP) are not gated.
    Consequences: agents receive the secrets they need at start (resolved by
    the provider plugin, held in the agent's environment, never persisted by
    the daemon); each agent works in its own git worktree; the blast radius
    is the Space's host, so a Coder workspace or pod is the isolation unit.
    The daemon still audits what agents did (session log, commits, PRs) but
    does not block it.
  - Providers seen: Claude Code (e.g. Opus 5.5, 1M context) and GitHub
    Copilot (Claude Opus 5.5), each with its own model and thinking level;
    one is the Space's default provider. A custom SDK adapter is still
    unspecified.
  - Naming: agents take the first unused name from a name pool (default
    "Planets": Mercury through Neptune); an intent sent to the Space takes
    the first name with no open agent.
  - Color [decided]: each agent created when an intent is sent gets a color
    from a palette of hues that share the same saturation and exclude red,
    yellow and green. Those three are reserved for status (error, warning or
    signed-out, healthy or connected), so an agent color is never mistaken for
    a state. The color marks the agent everywhere it appears: the left-pane
    accent bar, its center chat tab (a left border in the agent's color), the
    chat's message composer (the same left border, so tab and input read as
    one agent's space), its Sessions row, each intent card in the Intents
    list whose intent is assigned to that agent (the same left border;
    unassigned cards have none), and anything else the agent is attached to. The chat tab is a web view, so the daemon supplies the agent color as
    session data and the web app applies it from the shared tokens. [where it appears is from the screenshots; the rest is my call]
    Allocation mirrors naming: first palette color not used by an open agent,
    wrapping when exhausted. The palette lives in the shared design tokens
    (same values native and web), checked for contrast on both themes. Color
    is never the only identifier; the name is always shown. [my call]
  - Session identity: every chat is a session id, and every message resumes
    that session (harness resume is a hard requirement).
  - Activity states: working, quiet (reply running but silent), done, blocked
    (waiting on the user), waiting, stopped, dead (process died).
  - Queue limit ("Agents working at once", blank = no limit): an agent counts
    while its reply runs (working or quiet), not when done, blocked or dead.
    Agents started with New, a routine or Ask are never held back but still
    count.
  - Resume: a Sessions-row action that sends a configurable message (default
    "continue; when done, pretty commit and open the PR") to a stuck, waiting
    or stopped agent, restarting a dead agent's process in its folder first.
    Optional auto-nudge: send it to an agent quiet for N minutes (default 15),
    once per stuck reply, with a notification. An agent's process stops itself
    after 30 idle minutes, so nudges come before that.
  - Composer: mode (Agent or Ask), provider, model and thinking level are
    chosen per message.
- **Routines** [decided: prompts a user can call on demand or run on a
  schedule]: a name and a prompt "sent to a new agent each run" (every run is
  a fresh agent session). Trigger is on demand only, or at times of day (one
  or more times, chosen weekdays, an explicit timezone, e.g. America/New_York).
  Runs execute in the Space's workspace via the daemon scheduler even with no
  client open, as long as the workspace is running. Left pane lists each with
  its schedule ("Weekdays · 11:50", "On demand"); the editor has Save, Cancel
  and Delete. Agent, model and thinking level per routine follow Agent
  defaults. [from screenshot; per-routine agent settings are my call]
- **Store**: normalized resources plus a change feed.
- **Screen context**: the shell reports which resources and views are visible
  or focused; the daemon exposes that to agents.
- **Center-tab services**: the daemon exposes (a) an AI SDK UI-compatible
  message stream so Vercel components work against it, carrying A2UI messages;
  (b) PTYs over WebSocket for xterm.js, running where the daemon runs;
  (c) graph data for react-flow. [my call]

### 4.5 Plugin system

- **Hybrid hosting** [decided: in-process or out-of-process per plugin]. A
  plugin is one Rust trait implementation plus serializable message types, all
  defined in `plugin-protocol`. The same plugin can run either way:
  - In-process: the plugin crate is linked into the daemon and runs on its own
    supervised task (restart on panic, call timeouts and sync budgets). This
    protects against panics and slow calls, not against memory or CPU hogs or
    malice.
  - Out-of-process: the plugin ships as its own binary that serves the same
    trait over a versioned stdio/socket protocol (`plugin-protocol::serve`).
    The daemon supervises the process (restart with backoff, resource limits,
    log capture). Use this for flaky, heavy, third-party or non-Rust plugins;
    TypeScript plugins are allowed only here, as a temporary bridge.
  - `plugin.toml` declares `host = "in-process" | "process"`. Moving a plugin
    between modes is a manifest and build change, not a rewrite. Default
    guidance [my call]: first-party, simple, trusted plugins in-process;
    everything else out-of-process. Process isolation contains crashes but is
    not a security sandbox; an OS sandbox is separate work if third parties
    ever write plugins.
- `plugin.toml` manifest: name, version, supported protocol range, resources
  with freshness mode, **settings** (form schema rendered per Space),
  **auth** requirements (secret-backed token, OAuth, browser session; feeds
  the sign-in status model), **action prompts** (below), **views** (table columns, filters, sort, row actions,
  or a streaming-tail kind), required capabilities (hosts, secret keys),
  proposable writes. The GPUI shell renders views generically from the
  manifest; a new integration needs no UI code. [my call]
- Row actions: show detail in the right pane, open in a center tab, attach to
  intent (adds the item as agent context). [my call]
- **Action prompts** [decided: each integration can have associated action
  prompts that act on a row or on list(s), and can be applied to dashboards
  when applicable]. A plugin declares prompts in its manifest, each with: id
  and label; scope (row, selection, or whole list); the resource or view it
  applies to; a prompt template with bindings to the row/list data (e.g.
  `{{row.title}}`); optional default agent/harness; and its outcome (open
  chat, start an intent, or propose a write). They appear as menu items on
  rows and lists in the bottom pane, and on any dashboard tile bound to a
  matching view. Running one starts or targets an agent session with the
  bound data attached as context. Plugin-shipped prompts are the defaults;
  users may add their own per Space. [user-defined prompts are my call]
  Prompts whose outcome is a plugin write go through the approval flow; what
  the agent then does on its own is not gated.
- **One prompt concept** [my call]: a prompt is a template plus agent
  settings. Invoked on demand with no data it is a routine; invoked by a
  schedule it is a scheduled routine; bound to a row or list it is an action
  prompt. A single prompt store and runner in the daemon serves all three, and
  a routine can be promoted to an action prompt (or the reverse) by adding or
  removing a data binding.
- Freshness modes [my call]: records (poll/webhook): GitHub, Jira, Confluence,
  Nexus, AWS pipelines/builds, Snyk; query + live tail: Datadog logs; live:
  agent sessions (native).
- **Writes are proposals only**: a plugin emits a staged change (diff + source
  version). The daemon executes it only after user approval. Staged changes
  appear as rows in the bottom pane. If the source changed upstream since the
  proposal, the approval shows a conflict. Every approved write is audited.
- Secrets: a provider plugin (Keeper first, as in Intent IDE) resolves secrets
  at agent/plugin start. The daemon does not persist them. Resolution failure
  is surfaced in the session. [my call]
- Isolation: crash, restart with backoff, rate-limit handling and sync budgets
  per plugin.
- Release: out-of-process plugins release individually: per-plugin tags
  (`github-v0.3.1`), path-filtered CI, per-package release tooling, binaries
  per target (macOS arm64/x86_64, Linux x86_64/aarch64), and macOS signing and
  notarization per binary. In-process plugins are compiled into the daemon and
  release with it, not individually. The daemon supports protocol N and N-1
  for out-of-process plugins.

### 4.6 Browser

- **Daemon-hosted headless Chromium**: runs where the daemon runs. Agents drive
  it over CDP at any time. A center tab can show its CDP screencast with
  watch/take-over/hand-back; viewing it from a remote client is inherently
  streamed.
- **Client-local Chromium (CEF) on desktop**: renders the center tabs at
  native resolution, not streamed. CDP is bound to loopback and exposed to the
  remote daemon only through an authenticated reverse tunnel the user opts into
  per session, so agents can attach to the user's browser. [decided: native
  Chromium with CDP and remote backend integration; [my call] on the opt-in
  tunnel mechanism.]

### 4.7 UI tiers

- **Native (GPUI)**: left, right, bottom panes, icon rails, tab strip. Tables,
  streaming logs, intent editor (steering question cards, accept/review
  lists, dependency picker), folder tree and changed files. A Rust A2UI
  renderer for panes is deferred.
- **Web (center tabs)**: chat (Vercel AI Elements; official A2UI React
  renderer for agent-composed UI such as dashboards, forms, approval cards), terminal
  (xterm.js), flow (react-flow), and agent-authored React in a sandboxed tab
  with a capability-restricted postMessage bridge.
- **Theming** [decided: build the shell with themes in mind, coordinated
  with Tailwind theme management]. One token source of truth feeds both
  tiers:
  - Source: `design/tokens/` holds one file per theme (semantic tokens in
    OKLCH: surface levels, text, border, accent, focus ring, selection,
    status colors for ok/warn/error/info, radius, spacing, font families,
    text-size scale) plus `agent-palette.json` (the equal-saturation agent
    colors, excluding red, yellow and green).
  - Generator (`tools/themegen`, run as an xtask) emits: Tailwind v4
    CSS-first theme (`@theme` variables plus one `[data-theme="<id>"]` block
    per theme) for `web/`, and the GPUI theme data for the native panes
    (mapped onto gpui-component's theme format; verify when building).
  - Tailwind side: utilities consume CSS variables, so a new theme is a new
    token file with no component changes. Variable names follow the shadcn
    contract (`--background`, `--foreground`, `--card`, `--primary`,
    `--muted`, `--border`, `--ring`, `--radius`) so Vercel AI Elements, which
    builds on shadcn, picks up the theme unchanged [my call; verify].
  - Runtime: the shell owns the current theme, text scale and font (Settings
    → Appearance, app scope, with a "System" option mapping to a light/dark
    pair). On change it updates the GPUI theme and sends the new values to
    every CEF view over the host bridge (sets `data-theme` and CSS
    variables). New views receive the current theme before first paint, so
    they never flash the wrong theme.
  - Rule: no hard-coded colors in either tier; every color is a token. A
    check in CI fails on literal colors outside the token files. [my call]
  - Agent colors come from the token palette (OKLCH); web uses `oklch()`
    directly and native converts to sRGB.
- **Dashboards** (e.g. "Start of Day") are the main agent-composed UI [my
  read of the screenshot]: a versioned declarative document of tiles (stat
  tiles, tables, callouts) bound to store queries over plugin resources
  (Jira tickets, GitHub PRs/issues). Controls seen: Filter, Layout order,
  Undo last change (prompt edits are versioned), "Plan my day" (a prompt).
  Dashboards can be composed from any supplied integration's views and
  resources [decided]. Each tile has an "Ask" menu: the action prompts of its
  bound integration that apply to the tile's scope (row or list), each
  opening an agent chat with the tile's data attached as context. A tile whose plugin lacks auth or secrets shows
  an inline error naming the fix (e.g. sign in to Keeper in Settings).

## 5. Approaches considered

Plugin boundary
- A. Compiled-in crates: simplest; no isolation, rebuild per integration.
- B. Out-of-process plugins: isolation, independent release; costs a
  supervisor, a versioned protocol and per-binary distribution.
- Hybrid: one plugin trait, hosted in-process or out-of-process per plugin.
  **Chosen (owner).**
- C. WASM components: strong sandbox; weak SDKs for AWS/Datadog. Rejected for now.

UI toolkit and split
- iced everywhere in Rust (wasm for web hosts): web reach from one codebase;
  weaker widgets. Rejected once the owner placed rich content in the web center.
- GPUI panes + web center. **Chosen.** Cost: no browser host without a React
  re-implementation of the panes.
- Tauri/React everywhere: contradicts "as much Rust as possible". Rejected.

Browser
- Webview-only (wry/Tauri): no consistent CDP. Rejected for desktop.
- Remote headless only: not native resolution. Kept for unattended agent work.
- Both behind one abstraction. **Chosen.**

## 6. Version 1 scope and staging

### Version 1 [decided]

The owner wants to be able to:
1. Create an agent, which opens a tab containing a chat.
2. Do normal research or ideation with that chat.
3. Send the current chat to create an intent, which starts the intent
   polish process.
4. Go through the steering questions (up to 5, options, Skip).
5. After the questions are completed, a polish attempt runs.
6. Confirm, edit or skip each proposed precondition, acceptance criterion,
   story and depends-on entry.
7. Send the completed intent to an agent to execute the job.

Definition of done for v1: that loop works end to end on the desktop app,
from a chat to an agent executing the intent in its own worktree, with the
intent reaching Sent and then Resolved. Everything below is [my call]
unless marked otherwise.

- **Out of v1**: routines, dashboards, bottom-pane integration tools and
  all plugins, the browser tab, terminal tabs, write-back approval,
  auto-resolve from merged PRs, the Resume action and auto-nudge, remote
  daemons, browser and mobile hosts. The architecture stays compatible with all of them: the daemon API
  and the client/daemon split are used from day one so remote and web are
  later additions, not rewrites.
- **Harness**: the Claude Code headless CLI only, behind the harness adapter
  trait. Copilot and any custom SDK come later.
- **Where agents run in v1** (assuming no remote daemon in v1; see open
  questions): on the owner's machine, in per-agent git worktrees, with the
  user's existing credentials (git, `gh`) and Claude Code's permission
  prompts bypassed inside the worktree, as the unattended decision implies.
  No secrets provider and no sandbox in v1. The blast radius is therefore the
  owner's laptop; this is the main thing to check before approving.
- **Platform**: macOS (Apple silicon first) only in v1. Linux and Windows
  later; remote Linux daemons are separate (see After v1).
- **Theming in v1** [decided]: the shell is built on the token system from
  section 4.7 from the first screen. v1 ships two themes (a dark and a light)
  plus "System", live switching that updates native panes and every CEF view
  together, and Settings → Appearance (theme, text size, font). Adding a theme
  must need only a new token file.
- **Center web views in v1** [decided]: CEF, so the riskiest integration is
  proven first rather than after the product exists. The center-tab host
  interface still hides the engine from `app`, so a different engine remains
  swappable.
- **First milestone: the CEF-in-GPUI proof** [my call]. Before building
  features, show on macOS that a CEF view embedded in the GPUI window can:
  sit in the center region beside native left, right and bottom panes; resize
  and collapse with them; render the chat web app at native resolution; take
  keyboard focus and IME input; and coexist with native popovers and menus
  from the panes. It should also load xterm.js and react-flow tabs, and
  expose CDP on loopback. If embedding as a child view fails, the allowed
  fallbacks are, in order: CEF offscreen rendering into a GPUI texture, then
  a separate borderless native window docked to the center region. Each
  fallback is judged on input fidelity (focus, IME, scrolling) and overlay
  behavior, and the owner is told before switching.
- **Repositories**: an intent binds to exactly one source repository, chosen
  when the intent is created (default: the last used; changeable in the
  editor until Send). A GitHub remote is not required: without one, the
  agent leaves commits on its branch and skips the PR. The "New agent"
  research chat runs in its own scratch worktree off the chosen repository
  (it can read the code; the user's main checkout is never touched). The
  polish job is a tool-using run with read-only access to that repository so
  current-behavior text and proposed criteria reflect the real code. "Ask"
  mode (read-only Q&A) is deferred.
- **Dependencies in v1**: an intent's worktree branches from the repository's
  default branch. Resolved is set by the user and is documented in the UI as
  "landed on the default branch", so dependents build on it. Stacked
  branches are deferred.
- **Surfaces**: left pane (agents list with status dots, "New agent", agent
  queue, Space card minimal); center chat tabs (web); right pane Intents
  list and intent editor with a Send action; icon rail with the Intents and
  Settings entries (Agent defaults, Source repositories and Appearance only). No bottom
  pane content.
- **Daemon**: a separate `server` process, the same binary and API a remote
  host will use, started by the desktop app on the local machine. The app is
  only a client. Lifecycle: on launch the app finds a running local daemon
  through a lockfile under `~/.smithagents/run/` (pid, port, token, version)
  or starts one; on a version mismatch it asks before restarting it. Closing
  the app does not stop the daemon while agents are working; an idle daemon
  with no clients and no running agents exits after a grace period (10
  minutes). Shipped as a sidecar binary inside the app bundle. It stores agents, chat
  sessions (harness session ids, resume), intents and their versions. Everything,
  native panes and CEF tabs alike, uses one transport: HTTP and WebSocket on
  127.0.0.1 only, on a random port, with a secret token generated at daemon
  start and required on every request. The token lives in the lockfile with
  owner-only permissions. This matters because anything that can reach the
  port can start agents that run with permissions bypassed.
- **Chat to intent**: "Send to intent" takes the chat transcript (or the
  selected part) as the intent's seed input, tagged with its source (chat,
  session id), and creates an intent in Draft and Polishing.
- **Polish is a daemon job, not the chat agent**: a structured-output run
  against the harness, separate from the user-visible agent, in two passes.
  Pass 1 reads the seed and produces a cleaned title, kind, current-behavior
  text and up to 5 steering questions (question text, options, skippable).
  Pass 2 runs after the questions are completed (answered or skipped) and
  produces proposed preconditions, acceptance criteria, user stories and
  depends-on candidates (drawn from existing intents). Each pass is recorded
  as a new intent version.
- **Review semantics**: for questions, Skip means proceed with no answer and
  let the polish pass assume. For proposed items, Confirm accepts as is, Edit
  changes then accepts, Skip drops the item. Counters show "n of m accepted".
  When all four sections have no pending items the intent becomes Complete.
- **Execute (step 7)**: Send on a Complete intent:
  - Creates a new agent (next name, next color), a git worktree off the
    Space's repository on a branch named from the intent slug, and starts the
    harness with a brief composed from the intent (title, current behavior,
    preconditions, acceptance criteria, stories, answered steering questions,
    summaries of its dependencies) plus fixed instructions: work in this
    worktree, satisfy the acceptance criteria, commit, and open the PR when
    done (matching Intent IDE's default resume message). The agent's chat tab
    opens with its colored left border, and the intent card gets the same
    border.
  - Source repository: Settings has Source repositories. v1 supports adding
    a local git repository path or a clone URL (cloned on the daemon host).
  - Dependencies: an intent with unresolved Depends-on entries waits as
    Queued and starts when all of them are Resolved. A minimal agent queue
    exists; the "Agents working at once" limit is optional and defaults to no
    limit (Agent defaults).
  - Running state: Sent, with the agent's activity sub-state (working, quiet,
    done, blocked, dead). You can keep chatting with the running agent like
    any chat. Agents run unattended per the decided approval scope.
  - Resolved: set by the user with a button in v1 (the agent finishing only
    makes it "Sent, done"); automatic resolution from a merged PR arrives with
    the GitHub plugin.
- **Intent states in v1**: Draft and Polishing, Complete, Queued (only while
  waiting on dependencies or a slot), Sent, Resolved.

### After v1 (in order) [my call]
v1 already ships `core`, `server` (store, agent runner, minimal queue,
polish jobs), `app`, `host-desktop` and `web/`. UI-to-daemon traffic uses the
real network API from day one, and all file and PTY access stays daemon-side.
1. Remote daemons: SSH tunnel, daemon install/upgrade on Coder, RunPod and
   SSH hosts, per-Space connection and sign-in status. This is the main
   reason for the rewrite, so it comes first.
2. Plugin system: `plugin-protocol`, plugin host, secrets provider (Keeper),
   GitHub plugin first, bottom-pane generic views, settings and auth
   manifests, write-back approval, auto-resolve from merged PRs.
3. Remaining integrations, action prompts, routines, scheduled runs, Resume
   and auto-nudge, personal dashboards, Ask mode.
4. Browser tab and agent browser (CEF with CDP, daemon headless Chromium),
   terminal tabs, react-flow tab.
5. Later: browser hosts (`host-web`, `host-web-coder`), mobile/tablet
   (`host-tauri`), hosted relay and push. `host-web-mock` is a dev harness
   built whenever the web app needs it, not a v1 requirement.

## 7. Risks to spike early

- Embedding CEF inside a GPUI window: z-order with pane popovers/menus
  overlapping the center, resizing, macOS specifics. gpui-component's webview
  was experimental when last documented; current state unverified.
- gpui-component dock supporting "left full height, bottom under center+right".
- GPUI pre-1.0 API churn (tracks Zed).
- CEF binary size and update story.
- Reverse-tunnel security model for exposing desktop CDP.
- Streaming logs: backpressure from daemon to a virtualized GPUI list.

## 8. Left to the builder

Wire format and API shape, store engine, A2UI spec version, bridge schema
between native panes and web tabs, design token set, exact intent state
transitions, plugin install/update UX, test strategy.

## 9. Calls the designer made (owner to verify)

1. GPUI + web center; web hosts and mobile deferred (desktop-first). Owner
   has not answered whether browser access is needed in v1.
2. [Decided: hybrid] One plugin trait, hosted in-process or out-of-process
   per plugin; default guidance is in-process for simple trusted first-party
   plugins and out-of-process for the rest.
3. Freshness modes per integration; views declared in the plugin manifest.
4. Secrets via a provider plugin resolved at start, bootstrap credential from
   the host environment, never stored.
5. Intent state machine and queue/dependency semantics from Intent IDE.
6. Reverse-tunnel opt-in for desktop CEF CDP.
7. Space = one daemon; client may connect to several; intents persist on the
   host volume.
8. The local daemon is a separate process that outlives the app while agents
   are working and exits after 10 idle minutes (replaces the earlier
   "embedded daemon stops when the app quits"). Long-lived unattended work on
   other hosts is still the remote daemon's job.
9. v1 web/mobile reach, relay and push are deferred.
10. Write-back v1: comments, status transitions, PR links, description diffs;
    upstream change blocks approval with a conflict view.
11. v1 connects to existing hosts and installs/upgrades the daemon over SSH;
    creating Coder/RunPod hosts is a later plugin.
12. Daemon exposes AI SDK UI stream, PTY and graph endpoints for center tabs.
13. Crate and package names.
14. Intent editor and steering cards are native typed UI in the right pane,
    not A2UI; A2UI is for agent-composed center content.
15. Rail Terminal/Browser buttons open center tabs (Space shell, worktree
    shell, browser); Folder tree and Changed files are native right-pane views
    fed by the daemon from the agent's worktree.
16. Personal dashboards render in center tabs.
17. Plugins declare per-Space settings schemas and auth requirements in the
    manifest; the daemon tracks per-Space sign-in state (Coder is a
    client-side connection; Keeper, Jira and Browser are daemon-side) and
    checks it on the interval set in Settings → Sign-ins.
18. Dashboards are versioned declarative documents with data bindings, edited
    by prompt, with an "Ask" action per tile.
20. Action prompts are plugin-shipped data with user-defined additions per
    Space; applicability to a dashboard tile is decided by matching the
    prompt's declared scope and resource to the tile's bound view.
21. Routines, scheduled runs and action prompts share one prompt store and
    runner in the daemon.
26. Theme tokens are OKLCH in `design/tokens/`, generated into a Tailwind v4
    theme (shadcn-compatible variable names) and GPUI theme data; v1 themes
    are one dark, one light, plus System; a CI check bans literal colors.
25. No Intent IDE source or data: any Intent IDE behavior not visible in the
    screenshots (exact state transitions, edge cases) is the builder's to
    design, guided by the intent and this document. No migration tooling.
24. Consequences of "plugin writes only" approval: agents hold their secrets
    in their environment for the session; one worktree per agent; isolation
    is at the host level; the daemon audits agent actions without gating them.
23. Agent colors are allocated like names (first unused, wrapping) from an
    equal-saturation palette in the design tokens, never red/yellow/green.
22. Space settings are stored on the daemon host as a human-editable
    `~/.smithagents/spaces/<org>/<space>/space.yaml` [decided: `smithagents`
    naming, not `singularity`], so queue limits, routines and nudges work with
    no client open. Intent IDE stores its equivalent on the client machine
    under a different root, one reason its queue is tied to the open app. The
    desktop caches a copy. All app-owned state, config and plugin installs
    live under `~/.smithagents/` (env override for the root is my call).
19. The settings screenshot was a rotated photo; section names and text were
    read sideways and may contain misreadings.

### Open questions (owner only)

- (Resolved: the Intent IDE source is not available. Behavior is specified
  from the owner's screenshots and statements only. No data import or
  side-by-side operation is planned; SmithAgents starts with an empty store.)
- Custom SDK: what it is (not needed for v1, which uses the Claude Code CLI).
- Does v1 include a remote daemon (Coder/SSH/RunPod), or is v1 local-only
  with remote as the first milestone after? Draft assumes local-only.
- (Resolved: v1 uses CEF for the center web views, to prove early that
  CEF embedded in GPUI works. See the Platform and first-milestone bullets.)
- (Resolved: v1 is desktop only, no browser host.)

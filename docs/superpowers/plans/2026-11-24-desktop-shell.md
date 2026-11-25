# Desktop Shell (chat-only) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A macOS desktop app where you create an agent, a center tab opens with its chat (web app, streamed from the daemon), agents show with status and color in a native left pane, and Settings offers Appearance and Source repositories, all themed live from the Plan T tokens.

**Architecture:** The app is only a client of `smithagentsd`. A new daemon endpoint streams a turn in the AI SDK UI message-stream format so the web chat (`useChat`) works unchanged. A GPUI shell (`crates/app`, `crates/host-desktop`) keeps its logic in a GPUI-free `ShellModel` driven by daemon events. Center tabs sit behind a `CenterTabHost` trait (`crates/center-host`) so the web engine is swappable; the real CEF engine is the last task and is gated on Plan 0's go/no-go, so everything before it is built and tested without CEF.

**Tech Stack:** Rust stable; `gpui-kit` (gpui-component on the `gpui-pre` snapshot), tokio, axum, `smithagents-client`/`-theme`; web: Vite, React 19, TypeScript, Tailwind v4, Vercel AI SDK UI (`ai`, `@ai-sdk/react`), shadcn/ui, vitest; CEF via the `cef` crate (Task 16 only).

**Spec:** `docs/superpowers/specs/2026-11-23-smithagents-architecture-design.md` (sections 4.1 Layout, 4.3, 4.4 Agents and Color, 4.7 UI tiers and Theming, 6 Version 1). Roadmap: `docs/superpowers/plans/2026-11-24-v1-roadmap.md`. Theme contract: `docs/superpowers/plans/2026-11-24-theme-system.md` (bridge contract section). Research this plan rests on: see "Verified facts" below.

## Scope and splits

This is **Plan 3a, the chat-only shell**. Plan 3b (intent list, intent editor with question cards, accept/review lists, Send, the Intents rail entry) follows Plan 2. The roadmap row for Plan 3 is split accordingly. Two spec items are deferred out of 3a with reasons:

- **Settings → Agent defaults:** nothing in the daemon stores or uses a default provider, model, name pool or queue limit yet (Plan 1 has one hard-coded harness). It lands with Plan 2's queue.
- **Bottom pane:** the v1 spec says "No bottom pane content", so 3a has no bottom dock at all. That removes the "bottom under center and right" docking risk from v1.

## Verified facts (2026-11-24, from primary sources; re-check when the pin moves)

- `gpui` on crates.io is stale (0.2.2). `gpui-component` (repo now `longbridge/gpui-kit`) 0.11.0 requires the `gpui-pre` 0.3.14 snapshot (zed@f126950) with exact `=` pins and releases weekly with API breaks. The umbrella crate is `gpui-kit`. Pin exact versions and upgrade deliberately.
- `gpui_component::dock::DockArea` has `DockPlacement { Center, Left, Bottom, Right }`, collapsing, sizing and serializable layout. `Theme::change`, `Theme::update(cx, |t| t.apply_config(&Rc<ThemeConfig>))` and `ThemeRegistry::load_themes_from_str` switch themes at runtime. Theme JSON is `ThemeSet { themes: [ThemeConfig { name, mode, colors: {"background": …, "primary.background": …} }] }` with hex or Tailwind-name values.
- `gpui::Window` implements `HasWindowHandle`/`HasDisplayHandle` (raw-window-handle 0.6). A native child view paints above all GPUI content in its bounds, including popovers, unless the `gpui-fast` fork is used or the page is rendered offscreen into a GPUI element.
- `cef` crate 156.x supports macOS arm64. `WindowInfo::set_as_child` exists but no example exercises it. `multi_threaded_message_loop` is unsupported on macOS; use `external_message_pump` with `do_message_loop_work`. The bundle needs helper apps (`bundle-cef-app`).
- AI SDK UI message stream: SSE, header `x-vercel-ai-ui-message-stream: v1`, chunks `start`, `start-step`, `text-start|delta|end {id,delta}`, `finish-step`, `finish`, `error {errorText}`, ending with `data: [DONE]`. `useChat` takes `DefaultChatTransport({api, headers, prepareSendMessagesRequest})`. Versions: `ai` 7, `@ai-sdk/react` 4.
- Vercel AI Elements is being deprecated at the end of Nov 2026 in favor of shadcn's `ai-elements` patterns (`npx shadcn@latest add …`). Existing copied components keep working.
- A built Vite page does not load from `file://` in Chromium (absolute asset paths; module scripts are blocked from origin null): serve it from a custom scheme. Verified 2026-11-24.
- Unverified until built: CEF child-NSView end to end, `tool-output-error` in `ai` 7, whether `shadcn init` preserves existing CSS variables (we avoid it, see Task 4).

## Global Constraints

- The app is only a client of the daemon; the daemon is a separate process the app finds or starts through the lockfile (`smithagents_client::ensure_daemon`); closing the app never stops it while agents work.
- Every daemon request carries the token; the token is delivered to a page only by the host (document-start injection), never in a URL outside dev mode.
- macOS Apple silicon only in v1. Naming is `smithagents`, never `singularity`.
- Native panes: left and right. The left pane is never hidden: it switches between Expanded and a compact strip; the right pane hides. Icon rail far right; center holds web tabs only. Web tech only in center tabs.
- No literal colors in `crates/app`, `crates/center-host`, `crates/host-desktop` or `web/` (`cargo themegen lint` stays clean); every color is a token. Agent colors render with the theme's agent `l` and `c` and the daemon's **hue only** (Plan T finding 1).
- An agent's name is always shown next to its color (color is never the only identifier), except in the compact left pane, where an agent shows its color, status dot and initial, with the full name and state in the tooltip and in the accessibility label.
- Theme changes reach native panes and every open web tab together; a new tab gets the current theme before first paint.
- Rust `unsafe_code = "forbid"` stays in force for every crate except where Task 16 isolates unavoidable FFI in its own crate with a comment per `unsafe` block.

## Review Focus

1. Sending while a turn runs returns 409 `turn_in_progress` before any stream bytes; the web composer shows it and recovers (Tasks 3, 6).
2. Tab closed or page reloaded mid-turn: the daemon turn continues, reopening shows the finished reply from history, no second turn starts (Tasks 3, 6).
3. Daemon restart, events `410 events_too_old`, or lagged subscriber: the model resyncs from a fresh snapshot; agents the daemon marked Dead show as Dead; the "Reconnecting" banner appears and clears (Tasks 8, 9).
4. Agent output containing markdown links, raw HTML or `</script>`: rendered inert; a link click never navigates the token-holding page; it opens in the system browser (Tasks 6, 11).
5. Theme: a tab opened after a theme change starts themed; a system-appearance flip with "System" selected updates every tab once; an unknown saved theme falls back (Tasks 10, 15).
6. Adding a repository with a missing path, a file instead of a directory, a non-git directory, or unicode and spaces in the path shows the daemon's error text, never a blank failure (Task 14).

---

### Task 1: Daemon CORS for browser clients

**Files:**
- Modify: `crates/server/src/api.rs` (router layers), `Cargo.toml` (workspace dep `tower-http` with feature `cors`)
- Test: `crates/server/tests/api.rs`

**Interfaces:**
- Produces: every route answers `OPTIONS` preflight **without** a token (status 200 from tower-http, not 204) with `access-control-allow-origin: *`, allowed headers `authorization, content-type`, allowed methods `GET, POST, PUT, PATCH, OPTIONS`, `access-control-max-age`; all other requests are unchanged: a request without a valid token still gets 401 (the 401 response also carries the CORS headers so a browser can read it).

- [ ] **Step 1: Write failing tests** in `tests/api.rs`: preflight `OPTIONS /v1/agents` with `Origin`, `Access-Control-Request-Method: POST`, `Access-Control-Request-Headers: authorization,content-type` and no token returns 200 with the allow headers; `GET /v1/health` without token is still 401 and includes `access-control-allow-origin`; with the token it works and includes it; a wrong token on the WebSocket path is still rejected.
- [ ] **Step 2: Run `cargo test -p smithagents-server --test api cors`.** Expected: FAIL.
- [ ] **Step 3: Add the CORS layer outside the auth middleware** (layer order so preflight never reaches auth). Origin `*` is acceptable because auth is a bearer token, not cookies; note the Host-header allow-list follow-up from Plan 1 in a code comment.
- [ ] **Step 4: Run `cargo test -p smithagents-server`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(server): CORS for browser clients`.

---

### Task 2: UI message stream encoder and golden fixture

**Files:**
- Create: `crates/server/src/ui_stream.rs`, `crates/server/tests/fixtures/ui_stream_golden.sse` (generated, committed)
- Modify: `crates/server/src/lib.rs`
- Test: `crates/server/tests/ui_stream.rs`

**Interfaces:**
- Produces (`smithagents_server::ui_stream`): `enum Chunk { Start{message_id:String}, StartStep, TextStart{id:String}, TextDelta{id:String,delta:String}, TextEnd{id:String}, FinishStep, Finish, Error{error_text:String} }`; `Chunk::to_sse(&self) -> String` (`data: {json}\n\n`, JSON `type` kebab-case per the verified list, field names `messageId`, `id`, `delta`, `errorText`); `const DONE_SSE: &str = "data: [DONE]\n\n"`; `const STREAM_HEADER: (&str, &str) = ("x-vercel-ai-ui-message-stream", "v1")`; `fn golden_transcript() -> String` building the canonical stream (Start, StartStep, TextStart t1, two TextDeltas including one with a quote, newline and non-ASCII character, TextEnd, FinishStep, Finish, then DONE) used by the test and the web contract test.

- [ ] **Step 1: Write failing tests:** each `Chunk` variant serializes to the exact line from the verified protocol (assert the full JSON string); text with `"`, `\n`, `\\` and `é😀` round-trips through `serde_json`; `golden_transcript()` equals the committed fixture (when `UPDATE_GOLDEN=1` the test rewrites the file instead).
- [ ] **Step 2: Run `cargo test -p smithagents-server --test ui_stream`.** Expected: FAIL.
- [ ] **Step 3: Implement** with `serde_json::json!`; no extra dependencies.
- [ ] **Step 4: Generate the fixture** with `UPDATE_GOLDEN=1 cargo test -p smithagents-server --test ui_stream`, then run without the flag. Expected: PASS.
- [ ] **Step 5: Commit** `feat(server): AI SDK UI message stream encoder`.

---

### Task 3: `POST /v1/agents/{id}/chat` streaming endpoint

**Files:**
- Modify: `crates/server/src/api.rs` (route and handler), `crates/server/src/service.rs` (small helper if needed to subscribe before sending)
- Test: `crates/server/tests/api.rs` (uses `FakeHarness`, `test-util`)

**Interfaces:**
- Consumes: `AgentService::send_message`, `EventBus::subscribe(None)`, `Chunk`, `STREAM_HEADER`, `AgentState`.
- Produces: `POST /v1/agents/{id}/chat` with JSON body `{"text": "<message>"}` (our own body; the web transport reduces `useChat`'s payload to it). Behavior: subscribe to the bus **before** calling `send_message`; if `send_message` fails return the normal JSON `{error,message}` with its status (404 unknown agent, 409 `turn_in_progress`, 400 empty text) and no stream. On success respond `200`, `content-type: text/event-stream`, `cache-control: no-cache`, header `x-vercel-ai-ui-message-stream: v1`, then stream: `Start{new uuid}`, `StartStep`; for each `TurnDelta` of this agent: `TextStart{t1}` once then `TextDelta`; on `AgentUpdated` of this agent with state `Done` or `Dead`: `TextEnd` if open, `Error{"The agent stopped unexpectedly."}` first when `Dead`, `FinishStep`, `Finish`, `[DONE]`, close. A lagged subscriber ends the stream with an `Error{"The stream fell behind; the full reply will appear when the agent finishes."}` chunk and the same finish sequence. SSE keep-alive comment every 15 s. Dropping the connection never cancels the turn.

- [ ] **Step 1: Write failing tests** with `FakeHarness` scripts: (a) normal turn: collected body equals `start, start-step, text-start, ≥1 text-delta, text-end, finish-step, finish, [DONE]` and the concatenated deltas equal the fake reply; (b) second `POST /chat` while a turn runs: status 409 `turn_in_progress`, body JSON, no `text/event-stream`; (c) unknown agent 404; empty `text` 400; missing token 401; (d) a failing fake harness (non-zero exit after result): stream contains an `error` chunk then `finish`, agent ends Dead; (e) the client drops the connection after the first delta: the turn still finishes (agent reaches `Done`, the assistant message is in `GET messages`) and a later `POST /chat` starts a new turn normally; (f) no deltas (empty reply): stream has no `text-start` and still ends with `finish` and `[DONE]`.
- [ ] **Step 2: Run `cargo test -p smithagents-server --features test-util --test api chat`.** Expected: FAIL.
- [ ] **Step 3: Implement the handler** with an `axum::response::Sse`-free manual `Body::from_stream` (the protocol is raw `data:` lines) so the exact bytes come from `Chunk::to_sse`; run the stream task inside `tokio::spawn`-safe boundaries per Plan 1's handler rules.
- [ ] **Step 4: Run `cargo test -p smithagents-server --features test-util` and `cargo clippy --workspace --all-targets -- -D warnings`.** Expected: PASS, clean.
- [ ] **Step 5: Commit** `feat(server): streaming chat endpoint in the AI SDK UI message-stream format`.

---

### Task 4: `web/` scaffold with the Plan T theme

**Files:**
- Create: `web/package.json`, `web/vite.config.ts`, `web/tsconfig.json`, `web/index.html`, `web/src/main.tsx`, `web/src/styles.css`, `web/components.json`, `web/vitest.config.ts`, `web/.gitignore`, `web/README.md`
- Test: `web/src/theme.test.ts`

**Interfaces:**
- Produces: a Vite + React 19 + TypeScript project (pin the versions `npm view` reports today; record them in `package.json` exactly) with Tailwind v4 via `@tailwindcss/vite`, vitest + `@testing-library/react` + jsdom, and these rules:
  - `src/styles.css` starts with `@import "tailwindcss";` then `@import "../../design/generated/theme.css";` (no copy; the generated file stays the single source).
  - A Vite plugin (`transformIndexHtml`) inlines `design/generated/theme-bootstrap.js` as the first `<script>` in `<head>`, before any stylesheet.
  - shadcn is configured by a hand-written `components.json` (`cssVariables: true`, css path `src/styles.css`) and **`shadcn init` is not run**, because it may rewrite our variables; components are added with `npx shadcn@latest add <name>` and the diff reviewed.
  - `npm run build` outputs `web/dist`; `npm test` runs vitest; `npm run lint:colors` is `cargo themegen lint` (so web code is covered by the same check).

- [ ] **Step 1: Write failing test** `theme.test.ts`: after importing the bootstrap script text into jsdom with `window.__SMITH_THEME__ = {theme:"smith-light",appearance:"light",text_scale:1.2,font:"system"}`, `document.documentElement` has `data-theme="smith-light"` and `--text-scale` `1.2`; with no host state it applies the media default; `styles.css` compiled through Tailwind contains `--color-background` (a build-output assertion run after `vite build`, skipped in the unit run).
- [ ] **Step 2: Run `npm test`.** Expected: FAIL.
- [ ] **Step 3: Scaffold**; `npm run build` must succeed and `web/dist/index.html` must contain the inlined bootstrap before the first `<link>`/`<style>`.
- [ ] **Step 4: Run `npm test`, `npm run build`, and from the repo root `cargo themegen lint`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(web): Vite React Tailwind scaffold wired to the theme tokens`.

---

### Task 5: Host bridge, API client and history conversion

**Files:**
- Create: `web/src/host.ts`, `web/src/api.ts`, `web/src/history.ts`
- Test: `web/src/host.test.ts`, `web/src/api.test.ts`, `web/src/history.test.ts`

**Interfaces:**
- Produces:
  - `type HostConfig = { baseUrl: string; token: string; agentId: string }`; `getHostConfig(): HostConfig` reads `window.__SMITH_HOST__` (host-injected at document start, same moment as `__SMITH_THEME__`); in dev mode only (`import.meta.env.DEV`) it falls back to query params `?daemon=&token=&agent=`; in a production build the fallback is absent; a missing config throws `HostConfigError` with a message the UI shows.
  - `api.ts`: `getAgent(cfg): Promise<AgentInfo>` (from `GET /v1/agents`, picks the id; `AgentInfo {id,name,color:{h:number},state}`), `getMessages(cfg): Promise<ChatMessage[]>` (`GET /v1/agents/{id}/messages`), both sending `Authorization: Bearer`, throwing `ApiError {status, code, message}` from the daemon's `{error,message}` body.
  - `history.ts`: `toUiMessages(messages: ChatMessage[]): UIMessage[]` mapping `user` → user message and `assistant` → assistant message with one text part, **skipping `tool` messages** (tool activity display is deferred); ordering by `seq`; consecutive assistant messages stay separate.

- [ ] **Step 1: Write failing tests:** config from `window` object; dev fallback only when `DEV`; production mode with query params present and no injected config throws; `getMessages` sends the bearer header (assert via a fake `fetch`); a 401/409/404 body becomes `ApiError` with `code`; `toUiMessages` skips tool rows, keeps order by `seq`, preserves text with markdown and non-ASCII exactly.
- [ ] **Step 2: Run `npm test`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `npm test`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(web): host bridge, API client, history conversion`.

---

### Task 6: Chat view and wire-contract test

**Files:**
- Create: `web/src/Chat.tsx`, `web/src/transport.ts`, `web/src/Markdown.tsx` (or the shadcn `Message` pattern if it renders markdown safely), plus any `web/src/components/ai-elements/*` added through `npx shadcn@latest add` (names verified at install time; if a needed pattern is missing, use plain components; do not block on it)
- Modify: `web/src/main.tsx`
- Test: `web/src/Chat.test.tsx`, `web/src/contract.test.ts`

**Interfaces:**
- Produces:
  - `makeTransport(cfg: HostConfig): DefaultChatTransport` posting to `${baseUrl}/v1/agents/${agentId}/chat` with `headers: () => ({Authorization: "Bearer " + token})` and `prepareSendMessagesRequest` returning `{ body: { text: <last user message text> } }`.
  - `<Chat cfg initialMessages agent />`: message list, composer (disabled while streaming and while the page waits for a running turn to end; Enter sends, Shift+Enter newline), error banner (409 shows "This agent is still working on the last message"; other daemon rejections show the daemon message; a dropped connection shows "Lost the connection to the daemon."; a stream error chunk shows its text), a poll that resyncs history when a turn the page is not streaming ends, the agent's name in a header, and the agent color as a left border on the root through the generated utilities: root sets `style={{"--agent-h": agent.color.h}}` and class `border-l-4 border-agent`.
  - Markdown rendering is inert: no raw HTML, links rendered as `<a href rel="noreferrer noopener" target="_blank">`, and a click handler on the root calls `event.preventDefault()` on any anchor click and invokes `window.__smithagentsOpenExternal?.(href)` (a host-provided function; absent in a plain browser, where it falls back to `window.open`) so the token-holding page never navigates.
  - Links are http/https only, images are never rendered, and the built page carries a CSP.
  - `main.tsx` loads config, fetches agent and history, and renders `<Chat>`; any failure renders a readable error panel (not a blank page).
- `contract.test.ts` feeds `crates/server/tests/fixtures/ui_stream_golden.sse` (Task 2) through the `ai` package's own SSE/UI-message-stream parser (`readUIMessageStream` or `parseJsonEventStream`; use whichever `ai` 7 exports, check its types) and asserts it yields one assistant message whose text equals the golden deltas joined, with no parse errors. This is the guard that the Rust encoder and the SDK agree.

- [ ] **Step 1: Write failing tests:** `Chat.test.tsx` with a fake `fetch` returning an SSE body: history renders; sending shows the streamed text incrementally and re-enables the composer after `[DONE]`; a 409 JSON response shows the "still working" banner, polls until the running turn ends, then shows its messages, clears the banner and re-enables the composer with the draft kept; the root has `--agent-h` and the border class; an assistant message containing `[x](https://example.com)`, `<script>alert(1)</script>` and `</script>` renders no `script` element, the anchor click is default-prevented and calls the external-open hook with the href; an unreadable config renders the error panel. `contract.test.ts` as above.
- [ ] **Step 2: Run `npm test`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `npm test`, `npm run build`, `cargo themegen lint`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(web): chat view on useChat with the daemon stream`.

---

### Task 7: App crates scaffold and GPUI spike window

**Files:**
- Create: `crates/app/Cargo.toml`, `crates/app/src/lib.rs`, `crates/host-desktop/Cargo.toml`, `crates/host-desktop/src/main.rs`
- Modify: `Cargo.toml` (workspace dependency `gpui-kit = "=0.11.0"` plus whatever it needs for `gpui_platform`; record the resolved `gpui-pre` version in the commit message), `rust-toolchain.toml` only if the pinned gpui needs a newer stable (state the reason)
- Test: `crates/app/tests/smoke.rs`

**Interfaces:**
- Produces: `host-desktop` binary that opens one window titled "SmithAgents" through `gpui_kit::application()` / `open_window`, shows a `Root` with a text label, and exits cleanly when the window closes. `app` exposes `pub fn run(config: AppConfig) -> !`-style entry used by `host-desktop` later (a stub taking no config now).
- This task **verifies the pinned stack compiles and runs on this machine**; the verified facts above are paraphrase-grade, so record what actually works.

- [ ] **Step 1: Write the smoke test** `smoke.rs`: `cargo build -p host-desktop` is not a test; instead assert the crate graph resolves a single `gpui-pre` version (parse `cargo metadata`: exactly one package named `gpui-pre`), and that `host-desktop`'s binary target exists.
- [ ] **Step 2: Run `cargo test -p smithagents-app --test smoke`.** Expected: FAIL.
- [ ] **Step 3: Scaffold, build, and run `cargo run -p host-desktop`**; confirm the window opens and closes (manual; record macOS version and Xcode/Metal observations in the commit body). If the `runtime_shaders` feature is needed to avoid a full Xcode build, use it and say so.
- [ ] **Step 4: Run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`.** Expected: PASS, clean (the workspace `unsafe_code = "forbid"` applies).
- [ ] **Step 5: Commit** `feat(app): GPUI scaffold and spike window on the pinned gpui-kit`.

---

### Task 8: `ShellModel` and event reducer (GPUI-free)

**Files:**
- Create: `crates/app/src/model.rs`
- Test: `crates/app/tests/model.rs`

**Interfaces:**
- Consumes: `smithagents_core::{Agent, AgentId, AgentState, Repo, RepoId, Event, EventKind}`.
- Produces (`smithagents_app::model`):
  - `enum Connection { Connecting, Connected, Reconnecting }`
  - `struct ShellModel { … }` with `ShellModel::new()`, accessors `agents() -> Vec<&Agent>` (open agents sorted by `created_at`, closed ones excluded), `repos() -> &[Repo]`, `selected() -> Option<AgentId>`, `tabs() -> &[AgentId]`, `active_tab() -> Option<AgentId>`, `connection() -> Connection`, `last_seq() -> Option<u64>`.
  - `enum Action { Snapshot{agents:Vec<Agent>, repos:Vec<Repo>, latest_seq:u64}, Event(Event), ConnectionChanged(Connection), SelectAgent(AgentId), OpenTab(AgentId), CloseTab(AgentId), RepoAdded(Repo) }` and `ShellModel::apply(&mut self, Action) -> Vec<Effect>` where `enum Effect { OpenCenterTab(AgentId), CloseCenterTab(AgentId), FocusCenterTab(AgentId), UpdateTabAccent(AgentId) }`.
  - Rules: `Snapshot` replaces agents and repos, sets `last_seq`, and closes tabs for agents no longer present; `Event` with `seq <= last_seq` is ignored (duplicate after reconnect); `AgentCreated`/`AgentUpdated` upsert; `AgentClosed` removes the agent and closes its tab (Effect `CloseCenterTab`); `OpenTab` of an existing tab focuses it (no duplicate tab); selecting an agent selects but does not open; closing the active tab activates the previous tab or none; `MessageAppended` and `TurnDelta` are ignored by the model (chat content belongs to the web tab); an `AgentUpdated` for an agent with an open tab emits `UpdateTabAccent` only when its color changed (never in v1, but defined).

- [ ] **Step 1: Write failing tests** for each rule above, including: duplicate event ignored; snapshot after restart drops agents and tabs that vanished; agent marked `Dead` by the daemon stays listed with state Dead; opening the same agent twice yields one tab and a `FocusCenterTab` effect; a closed agent's tab closes and the next tab activates.
- [ ] **Step 2: Run `cargo test -p smithagents-app --test model`.** Expected: FAIL.
- [ ] **Step 3: Implement** (plain structs, no gpui dependency in this module).
- [ ] **Step 4: Run `cargo test -p smithagents-app`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(app): shell model and daemon-event reducer`.

---

### Task 9: `DaemonLink` (find or start the daemon, snapshot, events, commands)

**Files:**
- Create: `crates/app/src/link.rs`
- Test: `crates/app/tests/link.rs`

**Interfaces:**
- Consumes: `smithagents_client::{ensure_daemon, Client, ClientError}`, `Action`, `Connection`.
- Produces (`smithagents_app::link`):
  - `struct LinkConfig { home: PathBuf, daemon_bin: PathBuf, version: String }`
  - `struct DaemonLink` with `DaemonLink::start(config: LinkConfig) -> (DaemonLink, tokio::sync::mpsc::Receiver<Action>)` running its own tokio runtime on a dedicated thread (GPUI owns the main thread and its own executor; the app bridges through the channel).
  - Behavior: `ensure_daemon`; on success emit `ConnectionChanged(Connected)` after a `Snapshot` built from `health`, `list_repos`, `list_agents(false)`; then `events(Some(latest_seq))` and forward each as `Action::Event`. On stream end or error: `Reconnecting`, exponential backoff 0.5 s to 10 s, `ensure_daemon` again, take a **fresh snapshot** (never trust `since` across a restart), then resubscribe. On `410`/`BusError` equivalents: fresh snapshot. If `ensure_daemon` keeps failing, stay `Reconnecting` and surface the error text through `DaemonLink::last_error() -> Option<String>`.
  - Commands (async, return the daemon's typed error): `create_agent(repo_id)`, `add_repo(path)`, `close_agent(id)`; results arrive back as events or `Action::RepoAdded` for repos (repos have no event).
  - `DaemonLink::connection_info() -> Option<(u16, String)>` (port, token) for the center-tab bootstrap in Task 11; the token never gets logged (use the redacting `DaemonInfo` Debug).

- [ ] **Step 1: Write failing tests** using the real daemon binary built once (same approach as `crates/client/tests/common`; copy the minimal `daemon_bin()` helper, with a temp `SMITHAGENTS_HOME` and `--idle-timeout` small): first connection yields `Snapshot` then `Connected`; creating an agent through a second client produces an `Event(AgentCreated)`; killing the daemon process yields `Reconnecting`, then after restart a **new Snapshot** and `Connected` (agents the old daemon marked Dead appear as Dead); `add_repo` with a missing path returns the typed `path_not_found` error; stopping the link (drop) ends the thread without hanging the test.
- [ ] **Step 2: Run `cargo test -p smithagents-app --test link`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test -p smithagents-app --test link`.** Expected: PASS, no leftover `smithagentsd` processes.
- [ ] **Step 5: Commit** `feat(app): daemon link with snapshot, event stream and reconnect`.

---

### Task 10: App settings and the theme adapter

**Files:**
- Create: `crates/app/src/settings.rs`, `crates/app/src/theme_adapter.rs`
- Test: `crates/app/tests/settings.rs`, `crates/app/tests/theme_adapter.rs`

**Interfaces:**
- Consumes: `smithagents_theme::{ThemeSet, ThemeController, AppearanceSettings, ThemeState, NativeTheme, Appearance}`.
- Produces:
  - `AppSettingsStore::new(home: &Path)`, `load() -> AppearanceSettings` (file `<home>/app-settings.json`; a missing, empty or corrupt file returns the default and never errors), `save(&AppearanceSettings) -> io::Result<()>` (atomic write via temp file and rename, mode 0600, home dir created 0700).
  - `theme_adapter::theme_set_json(set: &ThemeSet) -> String`: a gpui-component `ThemeSet` JSON string with one `ThemeConfig` per Smith theme (`name` = theme id, `mode` = appearance), mapping our tokens to gpui-component keys. Mapping (verify each key against `crates/component/src/theme/schema.rs` of the pinned version and adjust names, not intent): `background`←background, `foreground`←foreground, `border`←border, `input.border`←input, `ring`←ring, `selection.background`←selection, `accent.background`←accent, `accent.foreground`←accent-foreground, `primary.background`←primary, `primary.foreground`←primary-foreground, `secondary.background`←secondary, `secondary.foreground`←secondary-foreground, `muted.background`←muted, `muted.foreground`←muted-foreground, `popover.background`←popover, `popover.foreground`←popover-foreground, `list.background`←card, `sidebar.background`←card, `title_bar.background`/`tab_bar.background`/`status_bar.background`←card-2 (gpui-component has no foreground keys for these), `tab.active.background`←center-pane, `tab.active.foreground`←center-pane-foreground, `list.hover.background`←muted (the framework clamps the active row to a faint alpha, so hover uses the weaker `muted` and `list.active.border`←ring marks selection), `list.active.background`←selection, `danger.background`←status-error, `success.background`←status-ok, `warning.background`←status-warn, `info.background`←status-info; values are `#rrggbb` from `NativeTheme`. Region surfaces (single mapping, `theme_adapter::region_tokens`): center pane and active tab = `center-pane`/`center-pane-foreground`; left pane and right activity pane = `card`; title bar, icon rail, tab strip and bottom bar = `card-2`. Our own views paint from `region_tokens` via `region_paint`: left pane, right (Settings) panel, icon rail (icons use the rail foreground), center content, tab strip controls, and the app-drawn title bar (`views::title_bar`, gpui-component `TitleBar` over a transparent native title bar with `app_owns_titlebar_drag`; surface `card-2`, text `card-2-foreground`). `Region::BottomBar` (card-2) paints the status bar (Task 12). The theme is changed only inside the shell state, from a state its `ThemeController` resolved (`ShellState::set_appearance` / `set_system_appearance`, Task 14), which updates gpui-component's theme and the native theme together; Task 15 observes `ShellEvent::ThemeChanged` and reads `ShellState::theme_state()`, and never applies a theme itself.
  - `fn apply_theme_state(state: &ThemeState, cx: &mut gpui::App)`: looks up the registered config by `state.theme` and applies it (`Theme::update`/`apply_config`), sets font and text scale; unknown id falls back to the system pair.
  - `fn agent_accent(native: &NativeTheme, hue: f32) -> Rgb` wrapping `NativeTheme::agent_color`.
  - A system-appearance observer that calls `ThemeController::set_system_appearance` and, when it returns `Some(state)`, pushes it (Task 15 consumes this).

- [ ] **Step 1: Write failing tests:** settings round trip; corrupt JSON and `{"theme":"gone","text_scale":null}` load to sane values; file mode is 0600 and the write leaves no temp file; `theme_set_json` parses as JSON, has two themes with the right `mode`, every key listed above present in each, values match `NativeTheme` hex for `background` and `primary.background`, and a third theme file (via `load_dir` in a tempdir) adds a third config without code changes; `agent_accent` for the light theme at hue 190 equals the Plan T value.
- [ ] **Step 2: Run `cargo test -p smithagents-app --test settings --test theme_adapter`.** Expected: FAIL.
- [ ] **Step 3: Implement** (`apply_theme_state` needs a gpui context and is verified manually in Task 12).
- [ ] **Step 4: Run `cargo test -p smithagents-app`, clippy, `cargo themegen lint`.** Expected: PASS, clean.
- [ ] **Step 5: Commit** `feat(app): app settings store and gpui-component theme adapter`.

---

### Task 11: Center tab host interface

**Files:**
- Create: `crates/center-host/Cargo.toml` (package `smithagents-center-host`, deps `raw-window-handle` 0.6, `serde`, `serde_json`, `smithagents-theme`, `url`), `crates/center-host/src/lib.rs`
- Test: `crates/center-host/tests/center_host.rs`

**Interfaces:**
- Produces (`smithagents_center_host`, no gpui dependency):
  - `struct TabId(pub u64)`; `enum TabKind { Chat{ agent_id: String } }` (Terminal and Flow are later); `struct TabSpec { id: TabId, kind: TabKind, title: String }`; `struct PixelRect { x: f64, y: f64, w: f64, h: f64, scale: f64 }`.
  - `trait CenterTabHost { fn attach(&mut self, window: &dyn raw_window_handle::HasWindowHandle) -> Result<(), HostError>; fn open(&mut self, spec: TabSpec, boot: PageBoot) -> Result<(), HostError>; fn close(&mut self, id: TabId); fn show(&mut self, id: TabId); fn set_bounds(&mut self, rect: PixelRect); fn push_theme(&mut self, state: &ThemeState); fn open_external(&mut self, url: &str); }` (`show` makes one tab visible and hides the others).
  - `struct PageBoot { page_url: String, host_script: String }` and `fn page_boot(spec: &TabSpec, daemon_port: u16, token: &str, theme: &ThemeState, page_origin: &str) -> PageBoot`: `page_url` is `<page_origin>/index.html`, where the product passes the custom-scheme origin `smithagents://app` (its scheme handler serves `web/dist`, see Task 16) and dev may pass `http://localhost:5173`; never `file://`; `host_script` is JavaScript to run **at document start** that sets `window.__SMITH_THEME__ = <ThemeState JSON>` and `window.__SMITH_HOST__ = {"baseUrl":"http://127.0.0.1:<port>","token":"<token>","agentId":"<uuid>"}` with `baseUrl` having no trailing slash or path, and all values emitted through JSON serialization with `<`, `\u2028`, `\u2029` escaped. It is injected only through embedder APIs, never as an inline script (the page's CSP blocks it).
  - `fn navigation_allowed(page_url: &str, target: &str) -> bool`: true only for same-origin, same-document navigation (same `smithagents://app/index.html` page, or the same dev origin when `page_url` is `http://localhost:5173`); everything else is false and the host must call `open_external` for `http(s)` targets.
  - `struct RecordingHost` implementing the trait for tests of later tasks (records calls in a `Vec<HostCall>`).

- [ ] **Step 1: Write failing tests:** `page_boot` output contains the exact `ThemeState` JSON and an `agentId` with a quote/backslash/`</script>` in it stays valid JavaScript (evaluate the script in `node` with a stub `window` or parse with a JSON round trip of the extracted literals; assert no raw `</script>`); the token appears only in `host_script`, never in `page_url`; the emitted `baseUrl` matches the pattern `web/src/host.ts` accepts (`http://127.0.0.1:<port>` or `localhost`, no trailing slash, no path); `navigation_allowed`: `smithagents://app/index.html` with a different query/hash → true, `smithagents://app/other.html` → false, `smithagents://evil/index.html` → false, `https://example.com` → false, `file:///etc/passwd` → false, `javascript:alert(1)` → false, `http://localhost:5173/` allowed only when the page URL is that origin; `RecordingHost` records an open-then-show-then-push_theme sequence.
- [ ] **Step 2: Run `cargo test -p smithagents-center-host`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test -p smithagents-center-host`, clippy, `cargo themegen lint`.** Expected: PASS, clean.
- [ ] **Step 5: Commit** `feat(center-host): center tab host trait, page boot script, navigation policy`.

---

### Task 12: Shell window: dock layout, icon rail, tab strip, banner

**Files:**
- Create: `crates/app/src/shell.rs`, `crates/app/src/views/{mod.rs,rail.rs,tabs.rs,banner.rs}`, `crates/app/src/view_models.rs`
- Modify: `crates/app/src/lib.rs`, `crates/host-desktop/src/main.rs`
- Test: `crates/app/tests/view_models.rs`

**Interfaces:**
- Produces:
  - `Shell` (GPUI view): a **left** pane (full height), **center** (tab strip over a content area that reserves the rectangle for the center host), a **right** dock, no bottom dock; the icon rail on the far right with one entry for now, Settings (an `enum RailEntry { Settings }` so Plan 3b adds Intents without rework). The left pane is never hidden: it has two modes, `enum LeftPaneMode { Expanded, Compact }`. Expanded is resizable; Compact is a fixed narrow strip (about 56 px, not resizable). `cmd-b` and the strip's toggle switch between them, and the pane's view gets the mode. The right dock hides and shows (rail, `cmd-alt-b`). The connection banner "Reconnecting to the local daemon…" sits above the center and shows when `Connection != Connected`. The layout persists through `DockAreaState` in `<home>/app-layout.json`, loaded leniently with the same rules as the settings file. It holds the left mode and expanded width; an older file whose left dock is closed loads as Compact.
  - Status bar (owner decision 2026-11-25): full window width at the very bottom, `STATUS_BAR_HEIGHT` = 24 px (in `layout_store.rs`), painted from `Region::BottomBar` (`card-2`). Vertical order: title bar, main row (left pane, dock area, rail), status bar. Center zone: one single-line status message (`StatusMessage { text, kind: Info | Error }`, whitespace normalised; chosen by the pure `status_to_show`: the connection status while not connected, else the transient message set with `ShellState::set_status`; `Info` clears after 5 s, `Error` stays until replaced or `clear_status`, and an `Error` set while disconnected is cleared on reconnect; failures caused by the connection set no status; an error shows a leading `status-error` dot, text stays in the foreground token and reads "<what failed>: <text>"). Right zone: `v<VERSION>`. Left zone reserved. The bar is a labelled region ("Status bar"); the message is a status node (this GPUI cannot mark a live region, so changes are not announced).
  - Update chip (owner decision 2026-11-25): a pill at the top right of the title bar (`accent` / `accent-foreground`), only for an `UpdateState` other than `UpToDate` (`Available`: "Update available"; `Installing`: "Installing…", disabled; `Failed`: "Update failed, retry"; `RestartRequired`: "Restart to update", persistent, no action yet). It takes the press so it neither drags nor zooms the window. Accessible label "<label>: v<version>".
  - Update seam (`crates/app/src/update.rs`): `enum UpdateState { UpToDate, Available, Installing, Failed, RestartRequired }` and an object-safe `trait Updater { fn check(&self) -> LocalBoxFuture<UpdateState>; fn install(&self, version: String) -> LocalBoxFuture<Result<(), String>> }`, injected through `ShellOptions`. Implementations bound their own timeouts, are cancel-safe and verify the signature before replacing anything. Production runs `NoUpdater` (always `UpToDate`; install errors "updates are not available in this build"), so the chip never appears in production builds until a real updater exists (release channel, app bundle, signature verification). `ShellState::check_for_updates()` runs once at startup and can be called again; clicking the chip installs once (success: `RestartRequired` and a transient Info "Update v<version> installed. Restart SmithAgents to finish."; failure: `Failed` and an Error "Update failed: <message>").
  - `view_models.rs` (pure, tested): `fn banner_text(&Connection, Option<&str>) -> Option<String>`, `fn tab_title(&Agent) -> String` (the agent name), `fn center_rect(…) -> PixelRect` helper converting GPUI bounds and scale factor into `PixelRect`.
  - The center area reports its bounds every layout through a custom element so `CenterTabHost::set_bounds` follows resizes and collapses.
- Manual verification (no automated GPUI view test is assumed to work on `gpui-pre`; if `gpui::test` support exists, add one smoke test): window opens with the left pane and the right dock; compacting the left pane or hiding the right dock resizes the center; reopening the app restores the layout; the banner shows when the daemon is killed.

- [ ] **Step 1: Write failing tests** for the pure view-model functions (banner text for Connecting/Reconnecting with and without an error string, `None` when Connected; tab title is the name; `center_rect` multiplies by scale).
- [ ] **Step 2: Run `cargo test -p smithagents-app --test view_models`.** Expected: FAIL.
- [ ] **Step 3: Implement the view models, then the shell views, and run `cargo run -p host-desktop`** to perform the manual checks above; record the results in the commit body.
- [ ] **Step 4: Run `cargo test --workspace`, clippy, `cargo themegen lint`.** Expected: PASS, clean (colors only through `cx.theme()` tokens).
- [ ] **Step 5: Commit** `feat(app): shell window with docks, icon rail, tab strip and connection banner`.

---

### Task 13: Agents pane and the New agent flow

**Files:**
- Create: `crates/app/src/views/agents/{mod.rs,new_agent.rs,close.rs}`, `crates/app/src/app_state.rs`
- Modify: `crates/app/src/view_models.rs`, `crates/app/src/shell.rs`
- Test: extend `crates/app/tests/view_models.rs`; headless `crates/app/tests/agents_pane.rs`

**Interfaces:**
- Produces:
  - Pure view models: `struct AgentRowVm { id, name, state_label: &'static str, state_kind: StateKind, accent_hue: f32, selected: bool }` and `fn agent_rows(model: &ShellModel) -> Vec<AgentRowVm>`; `enum StateKind { Idle, Working, Quiet, Done, Dead }` with `fn state_label` giving "idle", "working", "quiet", "done", "stopped unexpectedly" (Dead).
  - Native pane: header with a "New agent" button; rows with a left accent bar (color from `agent_accent` with the row's hue), the name, and a status dot.
  - Status dot: a status token plus a shape, so state never rests on color alone (pure `state_dot(StateKind) -> DotStyle { token, filled, dimmed }`, used by rows and markers):
    - idle: a hollow ring in `muted-foreground`;
    - working: a filled `status-info` dot (static, no pulse);
    - quiet: the working dot at 50% opacity (the turn runs but is silent);
    - done: a filled `status-ok` dot;
    - dead: a filled `status-error` dot.
  - Single click selects; double-click opens the agent's tab. On the focused row or marker (listbox keys): Space selects, Enter opens, Up/Down move the selection, the focus and the scroll position (the new selection scrolls into view).
  - A context action "Close agent" calls `close_agent` after a confirmation. The confirmation is a modal dialog whose default action is Cancel: it opens with the focus on Cancel; Enter (the dialog's Confirm) and Escape dismiss it without closing; only its danger "Close agent" button closes. A close in flight is sent once.
  - A create or close the daemon does not answer within the command timeout (60 s, injectable), or that is waiting when the connection drops, is given up with the notification "The daemon did not answer", so the New agent button never stays disabled.
  - Compact rendering (`LeftPaneMode::Compact`):
    - One marker per agent with its color, its status dot and the initial of its name. The full name is in a tooltip, so color is never the only identifier.
    - Markers are keyboard-focusable. Each one's accessibility label is "<name>, <state>".
    - Initials may collide (Mercury and Mars); the tooltip and the label tell them apart.
    - The New agent button is icon-only.
    - Selecting and opening work as in the expanded pane.
    - Keep `track_focus` on the pane's root in both modes.
    - The strip has about 55 px of content width.
  - New agent flow: if no repositories exist, the button opens the Settings pane's Source repositories section (Task 14) instead of failing; with exactly one repository it creates immediately; with several it shows a repository picker popover (default: last used, stored as `last_repo_id` in `<home>/app-state.json`, loaded leniently). Daemon errors show in a transient notification with the daemon's message.
- [ ] **Step 1: Write failing tests:** rows sorted by creation, `selected` follows the model, Dead shows "stopped unexpectedly", the accent hue passes through unchanged, an empty model yields no rows; picker logic as a pure function `fn new_agent_plan(repos: &[Repo], last_used: Option<RepoId>) -> NewAgentPlan { NeedRepo | Create(RepoId) | Pick{ repos, preselected } }` with cases (0 repos, 1 repo, 2 repos with and without a valid last-used id, last-used id no longer present).
- [ ] **Step 2: Run `cargo test -p smithagents-app --test view_models`.** Expected: FAIL.
- [ ] **Step 3: Implement the view models, then the pane;** run the app against a real daemon and create two agents, check names (Mercury, Venus), distinct colors and the open-tab effect (manual).
- [ ] **Step 4: Run `cargo test -p smithagents-app`, clippy, `cargo themegen lint`.** Expected: PASS, clean.
- [ ] **Step 5: Commit** `feat(app): agents pane and new-agent flow`.

---

### Task 14: Settings pane (Appearance and Source repositories)

**Files:**
- Create: `crates/app/src/views/settings/{mod,appearance,repositories}.rs` (replaces `views/placeholder.rs`), `crates/app/src/appearance.rs` (`AppearanceRuntime`: saved settings + `ThemeController` + store)
- Modify: `crates/app/src/view_models.rs`, `crates/app/src/shell.rs`, `crates/app/src/shell_state.rs`, `crates/app/src/theme_adapter.rs`, `crates/app/src/link.rs`, `crates/app/src/app_state.rs`, `crates/app/src/lib.rs`, `crates/app/src/views/agents/new_agent.rs`
- Test: extend `crates/app/tests/{view_models,settings,app_state}.rs`; create `crates/app/tests/settings_pane.rs`

**Interfaces:**
- Produces:
  - **Appearance** section: theme choice (System plus each theme's `name` from `ThemeSet`), text size (a stepped control over `base.text_scale` bounds in 0.05 steps), font choice (`base.fonts` labels). Changing any control builds a new `AppearanceSettings` and calls `ShellState::set_appearance`, which runs `ThemeController::update_settings` (the controller is owned by the shell state), saves changed settings through `AppSettingsStore` (a failed save is an Error status; the change still applies), applies a `Some(state)` to gpui-component and the native panes together (`theme_adapter::apply_framework_theme` + the state's record), and emits `ShellEvent::ThemeChanged`; `ShellState::theme_state()` is what Task 15 pushes to every web tab. "System" follows the OS live through `ShellOptions::system_appearance` (`OsAppearance` in the app, a fake in tests).
  - **Source repositories** section: the list of daemon repos (name, path, default branch), an "Add repository" field taking a local path (with a "Browse…" button on GPUI's native folder picker, `App::prompt_for_paths`, which fills the field) that calls `add_repo` through the commands seam (`AgentCommands::add_repo`); the repo appears through the model (`Action::RepoAdded`); failure shows the daemon's `message` inline under the field. Clone-by-URL is **not** in 3a (the daemon has no clone endpoint; recorded as a follow-up for Plan 2's source-repo work); a URL gets an inline "not supported yet" line. Both sections collapse; the state is kept in `app-state.json` (`collapsed_settings_sections`). The New agent flow without a repository calls `ShellState::request_settings_focus(SettingsSection::Repositories)`: the section expands, scrolls into view and its path field takes the focus.
  - Pure view models: `fn theme_options(&ThemeSet, &ThemeChoice) -> Vec<ThemeOptionVm {choice, label, selected}>`, `fn text_scale_steps(&TextScale) -> Vec<f64>`, `nearest_step`, `stepped_text_scale`, `text_scale_label`, `font_options`, `repo_rows`, `fn repo_error_text(&LinkError) -> String` (our own sentence for each known add code (`path_not_found`, `not_a_git_repo`, `bare_repo`, `no_default_branch`, `repo_exists`); otherwise the daemon's `message` without its internal `git: `/`store: ` prefix; a generic line for transport errors), `validate_repo_path_input`, `repo_matches_sent_path` (a repository the daemon finished after the add was given up clears the stale line).
- [ ] **Step 1: Write failing tests:** theme options list System first then the two themes, with `selected` following a stale saved id (falls to System); text steps stay within `[min,max]` and include the default; `repo_error_text` for `path_not_found`, `not_a_git_repo`-style daemon codes (use the real codes from `crates/server/src/api.rs`) returns the daemon message, and for a connection error returns the generic line.
- [ ] **Step 2: Run `cargo test -p smithagents-app --test view_models`.** Expected: FAIL.
- [ ] **Step 3: Implement the view models, then the section views;** manual: add a repo with spaces and unicode in the path, a missing path (error text shown), a plain directory (error shown), switch the theme live and confirm native panes change.
- [ ] **Step 4: Run `cargo test -p smithagents-app`, clippy, `cargo themegen lint`.** Expected: PASS, clean.
- [ ] **Step 5: Commit** `feat(app): settings pane with appearance and source repositories`.

---

### Task 15: Tab lifecycle wiring and live theme push

**Files:**
- Create: `crates/app/src/tabs.rs`
- Modify: `crates/app/src/shell.rs`, `crates/host-desktop/src/main.rs`
- Test: `crates/app/tests/tabs.rs`

**Interfaces:**
- Consumes: `ShellModel` `Effect`s, `CenterTabHost`, `page_boot`, `ShellState::theme_state()` and `ShellEvent::ThemeChanged` (the `ThemeController` is owned by the shell state since Task 14), `DaemonLink::connection_info`.
- Produces: `TabManager<H: CenterTabHost>` with `new(host, page_origin, theme: ThemeState)`, `handle(&mut self, effect: Effect, agent: Option<&Agent>, conn: Option<(u16,String)>) -> Result<(), HostError>` (open → `host.open(spec, page_boot(…))` using the **current** theme state, `show`; focus → `show`; close → `close`), `set_theme(&mut self, state: ThemeState)` (stores it and calls `push_theme` once per open tab), and `on_bounds(rect)`. A daemon port or token change after a reconnect re-opens the open tabs with a new `PageBoot` (the old page has a stale token). When `conn` is `None` (daemon not connected yet) opening a tab returns a clear error that the shell shows as a notification.
- Wiring in `host-desktop`: build `LinkConfig` (home from `SMITHAGENTS_HOME` or `~/.smithagents`, `daemon_bin` next to the executable in the bundle or `target/<profile>/smithagentsd` in dev, version from `CARGO_PKG_VERSION`), start the link, feed `Action`s into the model, run `Effect`s through the manager, and use `PlaceholderHost` (a host that draws nothing and logs `open` calls, defined in this task) until Task 16.

- [ ] **Step 1: Write failing tests with `RecordingHost`:** opening a tab after the theme was changed to light passes the light `ThemeState` in its boot script; a theme change with 3 open tabs yields exactly 3 `push_theme` calls; a system-appearance flip with "System" selected yields one push per tab, and none when an explicit theme is selected; reconnect with a new token re-opens tabs with the new token and never leaves the old token in `host_script`; opening with no connection returns the error and records no `open`; closing the last tab leaves no visible tab.
- [ ] **Step 2: Run `cargo test -p smithagents-app --test tabs`.** Expected: FAIL.
- [ ] **Step 3: Implement and wire;** run `cargo run -p host-desktop` with the placeholder host and confirm the log shows the right `page_url`, and that the logged `host_script` (REDACTED: the token replaced by `<redacted>`, never the token itself) contains the live port.
- [ ] **Step 4: Run `cargo test --workspace`, clippy, fmt, `cargo themegen check`, `cargo themegen lint`.** Expected: PASS, clean.
- [ ] **Step 5: Commit** `feat(app): tab lifecycle, boot scripts and live theme push`.

---

### Task 16: CEF engine, bundle and sidecar (gated on Plan 0)

**Gate:** do not start until Plan 0 records a go/no-go. Read its verdict and chosen embedding (child NSView, offscreen into a GPUI element, or docked borderless window) and write the first step of this task against that choice; if the verdict is no-go, stop and re-plan this task with the owner.

**Files:**
- Create: `crates/cef-host/` (package `smithagents-cef-host`, implements `CenterTabHost`; the only crate allowed `unsafe`, with `#![allow(unsafe_code)]` scoped to it and a `// SAFETY:` comment on every block), `crates/host-desktop/bundle/` (bundle metadata, helper process binary, `Info.plist` template), `scripts/bundle-macos.sh`
- Modify: `crates/host-desktop/src/main.rs` (swap `PlaceholderHost` for `CefHost`), `Cargo.toml` (workspace dep `cef` pinned to the version Plan 0 proved, `unsafe_code = "forbid"` lint overridden only in `cef-host`)
- Test: `crates/cef-host/tests/policy.rs` (pure parts), manual acceptance in Task 17

**Interfaces (the contract the Plan 0 spike should already satisfy):**
- `CefHost` implements the Task 11 trait. Requirements: `open` creates a browser at `page_url` and registers `host_script` to run at document start in the page's main frame (renderer-process context-created hook; verify in the spike); a request handler denies navigation when `navigation_allowed` is false and forwards `http(s)` targets to `open_external` (system browser through `open`), covering `target=_blank`, `window.open` and redirects; `push_theme` calls the page's `window.__smithagentsApplyTheme(<state JSON>)`; `set_bounds`/`show` keep exactly one tab visible at the rect; CDP (remote debugging port) is OFF by default in the product and enabled only in debug builds or when an environment flag is set, because CDP has no authentication and the chat page holds the daemon token; when enabled it is bound to 127.0.0.1 through `remote_debugging_port`, with the port chosen at startup (random free port) and logged; `external_message_pump` scheduled through `on_schedule_message_pump_work` (no fixed polling loop); `window.__smithagentsOpenExternal` is exposed to pages; the host re-validates its targets with `external_target` and routes the page's `window.open` fallback through the popup handler; register the `smithagents` scheme as standard, secure, CORS-enabled and fetch-enabled, with a scheme handler serving `web/dist` with correct MIME types (`text/javascript` for module scripts) and path confinement to the web root; keep Vite `base: "/"`; verify the page's CSP `'self'` matches the scheme origin; verify Chromium's local-network access checks do not block or prompt on `fetch` to `http://127.0.0.1:<port>` from that origin (prefer a CEF switch; if the daemon must send `Access-Control-Allow-Private-Network`, only for the app's origin); `push_theme` sets `window.__SMITH_THEME__` and calls `window.__smithagentsApplyTheme` only if it exists (a tab still loading picks the state up from the global); `scripts/bundle-macos.sh` runs `npm ci && npm run test:build` in `web/` before bundling.
- Bundle: `scripts/bundle-macos.sh` produces `SmithAgents.app` with the CEF framework and helper apps (via `bundle-cef-app`), `web/dist` under `Contents/Resources/web`, and `smithagentsd` as a sidecar next to the main executable; ad-hoc signed for local use (notarization is a release task, out of v1 scope for this plan).

- [ ] **Step 1: Write failing tests** for the pure parts: the request-policy function (reuse `navigation_allowed`; add `fn external_target(url) -> Option<String>` returning only `http`/`https` URLs, never `file`, `javascript` or `data`), the bounds math for the chosen embedding, and the CDP port chooser (loopback address only, port nonzero). Run `cargo test -p smithagents-cef-host`. Expected: FAIL.
- [ ] **Step 2: Implement `CefHost` per the Plan 0 spike code.**
- [ ] **Step 3: Build the bundle** with `scripts/bundle-macos.sh` and launch `SmithAgents.app`.
- [ ] **Step 4: Run `cargo test --workspace`, clippy, fmt, themegen checks.** Expected: PASS, clean.
- [ ] **Step 5: Commit** `feat: CEF center tabs, macOS bundle with daemon sidecar`.

---

### Task 17: End-to-end acceptance and roadmap update

**Files:**
- Create: `docs/superpowers/acceptance/desktop-shell.md` (the checklist below, with a results column)
- Modify: `docs/superpowers/plans/2026-11-24-v1-roadmap.md` (Plan 3 row: 3a this plan, 3b after Plan 2)

- [ ] **Step 1: Run the acceptance checklist on the bundled app** and record results: fresh launch with no daemon starts one (lockfile appears); New agent with no repository opens Source repositories; adding a repository then New agent creates "Mercury" with a colored accent and opens its chat tab with the matching left border; sending "hello" streams a reply and the agent ends Done; a second message sent during a turn shows the "still working" banner, then the running turn's messages appear when it ends, the banner clears and the draft is kept; closing and reopening the tab shows the full history; killing the daemon shows "Reconnecting" and recovers with the agents intact (Dead if mid-turn); switching Dark/Light/System changes native panes and the open chat tab together with no flash on a newly opened tab; text size and font changes apply to both; a markdown link in an agent reply opens the system browser and the chat page does not navigate; compacting and expanding the left pane, hiding the right pane and resizing either keeps the chat tab aligned and sharp (Retina), and the left pane is never hidden; typing with an IME in the composer works; closing the app leaves the daemon running while an agent works and it exits after the idle period when nothing runs. The status bar spans the full window width at the bottom (24 px, under the left pane and the rail) in every layout; its center shows "Agent <name> created" after New agent and "Reconnecting to the local daemon…" while the daemon is down, and a long message is truncated without reaching the version; the version shows at the right, and in a production build no update chip appears at the top right of the title bar.
- [ ] **Step 2: Fix or ticket every failure;** failures that block a checklist line are fixed before commit.
- [ ] **Step 3: Update the roadmap row and commit** `docs: desktop shell acceptance results, roadmap split into 3a and 3b`.

---

## Self-review notes

- **Spec coverage (v1 steps 1 and 2):** create an agent that opens a chat tab (Tasks 3, 6, 13, 15, 16); research chat with resume (daemon, Plan 1). Left pane with status dots, "New agent", agent colors and accent bar (13); center chat tabs with the colored left border (6, 15); right pane Settings, icon rail (12, 14); theming live across native and web, System option (10, 14, 15); daemon find-or-start and client-only app (9, 15); CEF center tabs (16). Intents, steering, review, Send, the Intents rail entry, Agent defaults: deferred to Plans 2 and 3b (scope section).
- **Not in this plan, by design:** bottom pane, terminal and flow tabs, browser tab, remote daemons, clone-by-URL.
- **Open risks that Task 7 and Plan 0 must settle:** the `gpui-pre`/gpui-component weekly pin churn; CEF child view versus offscreen rendering and overlays above the center; document-start script injection in CEF; AI SDK 7 parser exports for the contract test; whether `gpui::test` support works for headless view tests.
- **Follow-ups recorded:** Host-header allow-list for the CORS origin (Plan 1 follow-up still open); the chat page does not live-update for turns it did not start (it resyncs when the turn ends); tool-call display in chat, `tool-output-error` handling, notarization; real update detection and install (release channel, signed bundle, signature verification); announcing status messages to assistive technology once GPUI can mark a live region.

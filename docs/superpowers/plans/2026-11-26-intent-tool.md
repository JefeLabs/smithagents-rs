# Intent Tool Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The desktop app gets a native Intents pane in the right dock (list, create, review, Send, Mark resolved) and a Settings → Agent defaults section, on top of the daemon's existing intent API, which gains three intent kinds and a `kind` on create.

**Architecture:** The rail and the right dock are reworked once so several panes share the dock: each pane is a panel of one tab group, and only the pane the rail selected is `visible()`. Intents reach the app the way agents do: the link loads them at connect and forwards `IntentCreated`/`IntentUpdated`; `ShellModel` stores them; pure functions in `intent_vm.rs` derive every label, counter and sentence; the views only draw view models and send commands through a new `IntentCommands` trait. Nothing is updated optimistically: a view changes when the daemon's answer or event reaches the model.

**Tech Stack:** Rust stable, `gpui-kit = "=0.11.0"` (GPUI `gpui-pre 0.3.14`, `gpui-component 0.11.0`, `gpui-kit-assets 0.11.0`), `chrono` 0.4 (already in the workspace), tokio, axum, the daemon's fake harness and `crates/server/tests/bin/fake-claude` for tests.

**Spec:** `docs/superpowers/specs/2026-11-23-smithagents-architecture-design.md` §4.2 (right pane and rail), §4.4 (intents, review, steering), §6 (Chat to intent through Intent states in v1), decision 14. Roadmap row 3b and "Plan 2 API contract": `docs/superpowers/plans/2026-11-24-v1-roadmap.md`. Verified GPUI facts: `docs/superpowers/research/2026-11-26-gpui-capture-intent-research.md` (sections A, B4, D; cited below as "research A1" and so on). `REG/` means `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`.

## Scope

In this plan: the Intents rail entry and pane (list with filter, create screen, detail with header, markdown body, Questions, Preconditions, Acceptance criteria, Stories, Depends on, actions), the app's intent commands and model, Settings → Agent defaults, and two small daemon changes (three new kinds; `kind` on create, kept by polish).

Not in this plan: see Follow-ups at the end.

## Verified facts (2026-11-26, read from source at the pinned versions)

- All ten icons this plan and the feedback plan need exist in the pinned Lucide catalog as `REG/gpui-kit-assets-0.11.0/assets/icons/{lightbulb,list-filter,plus,arrow-left,check,message-square-plus,camera,paperclip,trash,x}.svg`. Only `plus`, `arrow-left` and `check` are in the default bundle (`default-icons.txt`). `gpui_kit::assets::icon_assets!(pub Name, [Lightbulb, ...])` embeds chosen icons from that catalog (`REG/gpui-kit-assets-0.11.0/src/lib.rs:71-91`; composition example `examples/extra_assets.rs`). The crate ships `LICENSE-LUCIDE`.
- There are two `IconName` types: `gpui_kit::component::IconName` (the default subset) and `gpui_kit::assets::IconName` (the full catalog); the first converts into the second (`REG/gpui-component-0.11.0/src/icon.rs:16-30`). `Icon::new` takes either (research D3).
- `BasePanel::visible(&self, cx: &App) -> bool` defaults to `true` (`REG/gpui-base-0.11.0/src/dock/panel.rs:25-30`); `DockArea::select_panel(&mut self, panel: PanelId, window, cx)` (`dock_area.rs:563-582`); a tab group with one visible panel whose `title_bar()` is false draws no tab bar (research A1, proved by probe).
- `TextareaState = InputBaseState<TextareaMode>` (`REG/gpui-base-0.11.0/src/input/textarea/mod.rs:10`), so it has the same `value()`, `set_value`, `focus`, `placeholder` and `InputEvent` as `InputState` (research B4); `Textarea::new(&Entity<TextareaState>)` (`REG/gpui-component-0.11.0/src/input/textarea.rs:70`).
- `gpui_kit::component::spinner::Spinner::new()` with `.color(Hsla)` and `Sizable` (`REG/gpui-component-0.11.0/src/spinner.rs:10-60`). The repo's buttons already show it through `Button::loading(bool)` (`crates/app/src/views/settings/repositories.rs:306`).
- A focused row or button is activated by Enter and Space as a GPUI keyboard click (`crates/app/src/views/agents/mod.rs:140-143`).
- The daemon refuses a text seed without `repo_id` ("a repository is required for a text seed", `crates/server/src/intents.rs:416-419`). It has no default repository.
- The daemon accepts a queue limit up to 64 (`QUEUE_LIMIT_MAX`, `crates/server/src/intents.rs:75`); `0` and `null` both mean no limit.
- `Agent` (`crates/core/src/agent.rs:39-53`) carries no last message; the app's model holds no chat content (`crates/app/src/model.rs:1-4`).
- NOT verified: whether a key binding on `alt-f` wins over text entry in a focused textarea (on macOS ⌥F types "ƒ"). Task 6 checks first.
- NOT verified: whether `TextView::markdown` exposes its text to `find_by_label` in headless tests. Task 7 checks first.

## Decisions (made for this plan; the owner's are in the task text where they apply)

- **Icons come from the pinned catalog through `icon_assets!`,** not from SVG files copied into the repo. They are the same Lucide files, and no second copy can drift. The Lucide license text is copied to `crates/app/licenses/LICENSE-LUCIDE`.
- **The create screen sends a `repo_id`.** The daemon has no default repository, so the app chooses one: the last repository used (`ShellState::last_repo_id`) while it still exists, else the oldest. The screen names it; with two or more repositories the name is a menu.
- **The chosen kind is stored on the seed** (`Seed.kind`), so pass 1 and a retried pass 1 can keep it. `seed_json` is free-form JSON, so no migration is needed.
- **Intent commands are a second trait,** `IntentCommands`, next to `AgentCommands`. The existing fakes in `crates/app/tests` keep compiling.
- **Initial loads are their own actions** (`Action::IntentsLoaded`, `Action::AgentDefaultsLoaded`), sent by the link between `Snapshot` and `Connected`. `Action::Snapshot` keeps its shape.
- **One command at a time per pane.** While a command is in flight every command control in the pane is disabled and the pressed one shows the button spinner.
- **Errors are one inline status line per screen** (the pattern of `REPO_PATH_ERROR_ID`), not notifications.
- **"Enter never triggers Send or resolve"** is read as: no key binding and no input's Enter dispatches Send or resolve, the pane never moves focus onto Send, and Enter in the resolve dialog cancels. A Send button the user tabbed to still takes Enter as a keyboard click, like every button in the app.
- **Meta line separator** is `" | "`, as in the old app.
- **Agent defaults offers 1 to 16.** A stored limit above 16 (set through the API) is shown as its number and stays until the user picks another.

## Global Constraints

- Naming is `smithagents`. `unsafe_code = "forbid"` stays.
- No literal colors outside token files (`cargo themegen check`; rules in research A4). Colors come from `region_paint`, `token_paint` and `cx.theme()`.
- Exact pin `gpui-kit = "=0.11.0"`. No new GPUI-related dependency versions. `Cargo.lock` gains no package.
- Tests spend no money: never run `#[ignore]` real-CLI tests. Polish in tests uses the daemon's fake harness or `fake-claude`, as the existing intent tests do.
- Do not launch the desktop app in tests or gates. Do not install system software. View tests use the headless harness of research A2.
- Migrations `0001` to `0003` are never edited (add `0004` only if a schema change is needed; this plan needs none).
- Every daemon call from a view goes through `IntentCommands` or `AgentCommands`; a view never holds a `Client`.
- No optimistic updates: a view changes when the daemon's answer or event reaches `ShellModel`.
- Every control is reachable by keyboard and has an accessible name. Sizes are rem-based (`rems(..)`, `text_sm()` and the like) except hairlines and dots.
- Every string a view test asserts is drawn as a `Label` with the element id given in the task, or is a button's label (plain text is not in the accessibility tree; `crates/app/src/views/settings/repositories.rs:241`).
- The repository is public: no personal names, emails, tokens or private repository names in code, tests or docs.
- Gate for every task: `cargo test --workspace --features smithagents-server/test-util`, `cargo clippy --workspace --all-targets --features smithagents-server/test-util -- -D warnings`, `cargo fmt --all --check`, `cargo themegen check`, and `pgrep -fl smithagentsd` empty afterwards.
- Commits end with the trailer `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>`.

## Review Focus

1. **An older copy of an intent arrives after a newer one** (a command's answer lands after the event of a later change, or events replay after a reconnect). Expected: the model keeps the copy with the later `updated_at`; the view never steps back. (Test in Task 3.)
2. **The daemon goes silent or the connection drops while a control is busy; the list is reloaded after a reconnect.** Expected: the control is enabled again, the screen says "The daemon did not answer", and a detail whose intent is no longer in the list returns to the list. (Tests in Task 5.)
3. **Someone else changed the intent first, or the user presses twice.** A second press while busy, or a daemon answer of 409 `polishing`, `invalid_state` or `already_answered`. Expected: one request per press, a plain sentence for the refusal, and the view shows the daemon's state. (Tests in Tasks 4, 8 and 9.)
4. **Body text nobody designed for.** `current_behavior` that is empty, has no heading, is one 10,000-character line, is only a code fence, uses CRLF or non-ASCII text. Expected: cards and the detail draw without panicking, previews are cut at 200 characters on a character boundary, and an empty body shows the seed text. (Tests in Tasks 4 and 7.)
5. **Typing in the create box.** Whitespace-only text, and ⌥B pressed while the box has focus (macOS types "∫"). Expected: the kind rows stay disabled for blank text; ⌥B creates a Bug and adds no character to the text. (Tests in Task 6.)

## File Structure

```
crates/core/src/intent.rs                 IntentKind::{NewFeature,Refactor,Maintenance}, ALL, wire_name; Seed.kind; CreateIntentRequest.kind
crates/server/src/{intents.rs,polish.rs,brief.rs}   kind on create, kept by pass 1; schema and prompt list six kinds
crates/app/src/assets.rs                  AppIcons (ten Lucide icons) and AppAssets (icons, then the Kit's defaults)
crates/app/licenses/LICENSE-LUCIDE        copy of the catalog's license
crates/app/src/views/rail.rs              RailEntry::{Settings, Intents}, as_str/parse
crates/app/src/shell.rs                   right dock with several panels; show/select a rail entry; ToggleIntentsPane
crates/app/src/shell_state.rs             right_entry, intent_commands, clock; ShellEvent::Reveal(RailEntry)
crates/app/src/app_state.rs               remembers the right pane ("right_pane")
crates/app/src/link.rs                    IntentCommands; intent methods on DaemonLink; initial loads
crates/app/src/model.rs                   intents and agent defaults in ShellModel
crates/app/src/intent_vm.rs               pure derivations: labels, counters, cards, sections, sentences
crates/app/src/views/intents/mod.rs       IntentsPane: panel, screens, the command runner, the error line
crates/app/src/views/intents/list.rs      header, filter menu, cards, empty states
crates/app/src/views/intents/create.rs    the create screen
crates/app/src/views/intents/detail.rs    header, body, polishing and failed states
crates/app/src/views/intents/sections.rs  Questions, the three item sections, Depends on
crates/app/src/views/intents/actions.rs   Send, queued reason, Open chat, Mark resolved, start error
crates/app/src/views/settings/agent_defaults.rs   the Agent defaults section
crates/app/tests/common/intents.rs        FakeIntents, intent builders, a fixed clock
crates/app/tests/{assets,rail,intent_model,intent_vm,intents_pane,intent_create,intent_detail,intent_review,intent_actions,agent_defaults,intent_link}.rs
```

---

### Task 1: Rail and right dock take several panes; one icon source

**Files:**
- Create: `crates/app/src/assets.rs`, `crates/app/licenses/LICENSE-LUCIDE`, `crates/app/src/views/intents/mod.rs` (skeleton pane), `crates/app/tests/{assets.rs,rail.rs}`
- Modify: `crates/app/src/lib.rs:11,65` (module, `.with_assets`), `crates/app/src/views/{mod.rs,rail.rs}`, `crates/app/src/shell.rs:69-147,349-363,399-405,492-512,649-657,779-792`, `crates/app/src/shell_state.rs:92-113,350-355`, `crates/app/src/app_state.rs`, `crates/app/src/views/settings/mod.rs:252-265`
- Test: also `crates/app/tests/{menu_and_keys.rs:115-180,app_state.rs,shell_ui.rs}` (update)

**Interfaces:**
- Produces:

```rust
// assets.rs
gpui_kit::assets::icon_assets!(pub AppIcons, [Lightbulb, ListFilter, Plus, ArrowLeft, Check,
    MessageSquarePlus, Camera, Paperclip, Trash, X]);
pub struct AppAssets;                       // impl AssetSource: AppIcons first, then gpui_kit::assets::Assets

// views/rail.rs
pub enum RailEntry { Settings, Intents }    // ALL = [Settings, Intents] (top to bottom)
impl RailEntry {
    pub fn label(self) -> &'static str;                  // "Settings" | "Intents"
    pub fn element_id(self) -> ElementId;                // "rail-settings" | "rail-intents"
    pub fn icon(self) -> gpui_kit::assets::IconName;     // Settings | Lightbulb
    pub fn placement(self) -> DockPlacement;             // Right for both
    pub fn as_str(self) -> &'static str;                 // "settings" | "intents" (persisted)
    pub fn parse(text: &str) -> Option<Self>;
}

// shell.rs
pub const INTENTS_PANEL: &str = "Intents";  // persisted panel name; never change it once shipped
// action ToggleIntentsPane, bound to "cmd-alt-i" in KEY_CONTEXT; View menu item "Toggle Intents" after "Toggle Settings"
impl Shell {
    pub fn rail_active(&self, cx: &App) -> Option<RailEntry>;   // the right entry while the right dock is open, else None
    pub fn select_rail_entry(&mut self, entry: RailEntry, window: &mut Window, cx: &mut App);  // on screen: close; else show
    pub fn show_rail_entry(&mut self, entry: RailEntry, window: &mut Window, cx: &mut App);    // open or switch; never closes
}

// shell_state.rs
pub enum ShellEvent { LeftPaneChanged, Reveal(RailEntry), FocusSettings(SettingsSection), ThemeChanged, TabOpenFailed(String) }
impl ShellState {
    pub fn right_entry(&self) -> RailEntry;                                   // default Settings
    pub fn set_right_entry(&mut self, entry: RailEntry, cx: &mut Context<Self>);   // saves app-state.json, notifies
    pub fn reveal(&mut self, entry: RailEntry, cx: &mut Context<Self>);            // emits ShellEvent::Reveal(entry)
}

// app_state.rs
pub struct AppState { pub last_repo_id: Option<RepoId>, pub collapsed_sections: Vec<SettingsSection>, pub right_pane: Option<RailEntry> }
// JSON key "right_pane": "settings" | "intents"; an unknown or missing value loads as None

// views/intents/mod.rs
pub const INTENTS_TITLE_ID: &str = "intents-title";
pub struct IntentsPane;   // BasePanel (panel_name INTENTS_PANEL, closable false, zoomable false, visible), Panel (title_bar false, inner_padding false), EventEmitter<PanelEvent>, Focusable, Render
impl IntentsPane { pub fn new(state: Entity<ShellState>, window: &mut Window, cx: &mut Context<Self>) -> Self; }
```

How it works: `Shell::new` builds every right pane and adds each to one tab group (`DockLayout::tabs().panel_view(panel_handle(settings), cx).panel_view(panel_handle(intents), cx)`), keeping `right_panels: Vec<(RailEntry, PanelId)>` (`PanelId::from(entity.entity_id())`). Each pane's `BasePanel::visible` returns `self.state.read(cx).right_entry() == RailEntry::<Own>`, so one panel is visible and no tab bar is drawn. `show_rail_entry` sets the state's right entry, calls `DockArea::select_panel` with that entry's `PanelId`, opens the dock if it is closed, refits it, and focuses the pane. `ShellEvent::Reveal(entry)` calls `show_rail_entry`. `request_settings_focus` emits `Reveal(RailEntry::Settings)` then `FocusSettings`. The right entry starts from `AppState.right_pane`.

**How a later plan adds a rail entry and pane (for example Feedback):** (1) add the variant to `RailEntry`, to `ALL` at its place, and to `label`, `element_id`, `icon`, `placement`, `as_str`; (2) write the pane with the same five impls as `IntentsPane`, its `visible` comparing `right_entry()` with its variant, and a `pub const <NAME>_PANEL`; (3) in `Shell::new`, create it and add one `.panel_view(..)` and one `right_panels` entry; (4) its icon is already in `AppIcons` (add to the list if not); (5) optionally an action, a key in `init`, a menu item in `app_menus` and an `.on_action` in `render`, with the two `menu_and_keys.rs` tests updated. Nothing else in the shell changes.

- [ ] **Step 1: Write the failing tests.**
  - `tests/assets.rs` `the_app_source_serves_every_bundled_icon`: for each of the ten names, `AppAssets.load(&IconName::<Name>.path())` is `Some` and its bytes contain `<svg`; `IconName::Settings.path()` (a default icon) also loads; `AppAssets.load("icons/no-such-icon.svg")` is `Ok(None)`.
  - `tests/rail.rs` (headless, `open_shell`): `the_rail_lists_settings_then_intents` (`RailEntry::ALL == [Settings, Intents]`; both buttons are found; the Intents button's label is "Intents" and it sits below Settings). `selecting_intents_switches_the_right_pane_without_a_tab_bar` (click `rail-intents`: `rail_active == Some(Intents)`, `INTENTS_TITLE_ID` is found, `SETTINGS_TITLE_ID` is not, `find_all_by_role(Role::Tab)` is empty, the right dock is open and its width unchanged). `selecting_the_entry_on_screen_closes_the_dock` (click `rail-intents` twice: dock closed, `rail_active == None`; click `rail-settings`: dock open on Settings). `the_right_pane_is_remembered` (with an `AppStateStore` in a tempdir: select Intents, drop the window, reopen with the same store: `rail_active == Some(Intents)`). `revealing_settings_from_intents_switches_back` (with Intents shown, `state.request_settings_focus(SettingsSection::Repositories, cx)`: Settings is shown). `cmd_alt_i_toggles_intents`.
  - `tests/app_state.rs`: `right_pane` round-trips as `"intents"`; `"right_pane": "nope"` and a missing key load as `None` without losing `last_repo_id`.
  - `tests/menu_and_keys.rs`: the View menu is exactly "Toggle Agents Pane", "Toggle Settings", "Toggle Intents"; `ToggleIntentsPane` is bound to `cmd-alt-i`.
- [ ] **Step 2: Run them.** `cargo test -p smithagents-app --test assets --test rail --test app_state --test menu_and_keys`. Expected: compile errors (`AppAssets`, `RailEntry::Intents` do not exist).
- [ ] **Step 3: Implement** as specified. The skeleton pane draws only its heading "Intents" (`Role::Heading`, id `INTENTS_TITLE_ID`) on `Region::RightPane`, like `SettingsPane::render`. Copy `REG/gpui-kit-assets-0.11.0/LICENSE-LUCIDE` to `crates/app/licenses/LICENSE-LUCIDE` and name it in the doc comment of `assets.rs`.
- [ ] **Step 4: Run the tests.** Expected: PASS, and the tests research A1 item 7 lists still pass unchanged (`shell_ui.rs:198-223`, `agents_pane.rs:610`, `settings_pane.rs:954`). Run the gate.
- [ ] **Step 5: Commit.** `feat(app): the rail and right dock take several panes; one icon source`

---

### Task 2: Daemon: three new kinds, and `kind` on create kept by polish

**Files:**
- Modify: `crates/core/src/intent.rs:9-15,49-53,263-267`, `crates/server/src/polish.rs:102-108,138,203,297-302`, `crates/server/src/brief.rs:34-40`, `crates/server/src/intents.rs:386-470,1123-1140`, `docs/superpowers/plans/2026-11-24-v1-roadmap.md` (API contract table)
- Test: `crates/core/src/intent.rs` (unit), `crates/server/tests/{polish.rs,intents.rs,api.rs,brief.rs}`, `crates/client/tests/intents.rs`

**Interfaces:**
- Produces:

```rust
pub enum IntentKind { NewFeature, Enhancement, Bug, Refactor, Maintenance, Task }
// serde snake_case: "new_feature" | "enhancement" | "bug" | "refactor" | "maintenance" | "task"
impl IntentKind {
    pub const ALL: [IntentKind; 6];                    // in the order above
    pub fn wire_name(self) -> &'static str;            // the serde name
    pub fn from_wire(name: &str) -> Option<IntentKind>;
}
pub struct Seed { pub source: SeedSource, pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub kind: Option<IntentKind> }   // the kind chosen at create
pub struct CreateIntentRequest { pub repo_id: Option<RepoId>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub kind: Option<IntentKind>, pub seed: SeedInput }
```

Behavior: `create` stores `req.kind` in `seed.kind` and starts `content.kind` at it (`Task` when absent). Pass 1 still cleans the title and text; when `seed.kind` is `Some(k)` the saved `content.kind` is `k` whatever the model returned, and the prompt's kind line reads `- kind: the user chose {wire_name}; return exactly that`. Otherwise the line reads `- kind: one of new_feature, enhancement, bug, refactor, maintenance or task`. The pass-1 schema enum is built from `IntentKind::ALL`. `polish.rs::kind_name`, the `apply_pass1` match and `brief.rs::kind_label` use `wire_name`/`from_wire` (the brief prints the wire name with `_` as a space: "new feature"). A later `PATCH` of `kind` still works and does not change `seed.kind`. The client needs no new method: `create_intent` already sends the whole request.

- [ ] **Step 1: Write the failing tests.**
  - core `kinds_have_stable_wire_names`: each of `ALL` serializes to its name above and `from_wire(wire_name(k)) == Some(k)`; `from_wire("feature")` is `None`. `old_seed_and_request_json_still_load`: `{"source":"text","text":"s"}` parses to a `Seed` with `kind: None` and serializes back without a `kind` key; `{"repo_id":null,"seed":{"text":"hi"}}` parses with `kind: None`; a request with `kind: Some(Refactor)` serializes with `"kind":"refactor"`.
  - `server/tests/polish.rs`: the schema's kind enum equals `["new_feature","enhancement","bug","refactor","maintenance","task"]` (replaces the assertion at line 75); `apply_pass1` accepts `"refactor"` and rejects `"feature"` with `PolishError::Invalid`; `pass1_request` for a seed with `kind: Some(Bug)` contains `the user chose bug`.
  - `server/tests/intents.rs` `create_with_a_kind_survives_pass_one` (fake harness scripted to return `"kind": "enhancement"`): create with `kind: Some(Maintenance)`; the created intent has `content.kind == Maintenance`; after pass 1 it is still `Maintenance` and the title is the polished one; after a scripted pass-1 failure and `retry_polish` it is still `Maintenance`. `create_without_a_kind_takes_the_polished_kind` (the existing behavior, asserted). `a_stored_task_intent_still_loads` (save an intent whose `content_json` has `"kind":"task"` and whose `seed_json` has no `kind`; `get` returns it).
  - `server/tests/api.rs`: `POST /v1/intents` with `"kind":"new_feature"` answers 201 with `content.kind == "new_feature"`; with `"kind":"feature"` answers 400 `invalid`.
  - `client/tests/intents.rs`: in `full_loop_through_the_client`, create with `kind: Some(IntentKind::Refactor)` and assert it after pass 1 (the fixture's pass 1 says `enhancement`).
- [ ] **Step 2: Run them.** `cargo test -p smithagents-core intent && cargo test -p smithagents-server --features test-util --test polish --test intents --test api`. Expected: compile errors (`IntentKind::NewFeature`, `Seed.kind`).
- [ ] **Step 3: Implement.** The compiler lists the `Seed { .. }` and `CreateIntentRequest { .. }` literals to extend with `kind: None` (11 and 21 sites, mostly tests).
- [ ] **Step 4: Update the roadmap's API contract table**: the `POST /v1/intents` body becomes `{"repo_id"?, "kind"?, "seed": ...}`, and one sentence under the table: kinds are `new_feature`, `enhancement`, `bug`, `refactor`, `maintenance`, `task`; a `kind` given at create is kept by polish.
- [ ] **Step 5: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 6: Commit.** `feat(server): new_feature, refactor and maintenance kinds; create takes a kind that polish keeps`

---

### Task 3: App link and model: intent commands, initial load, events

**Files:**
- Create: `crates/app/tests/common/intents.rs`, `crates/app/tests/intent_model.rs`
- Modify: `crates/app/src/link.rs:255-320,447-468`, `crates/app/src/model.rs:24-44,57-67,118-221`, `crates/app/src/shell.rs:173-225,264-338`, `crates/app/src/shell_state.rs:115-164,278-285`, `crates/app/tests/common/mod.rs` (`pub mod intents;`), `crates/app/Cargo.toml` (`chrono.workspace = true` moves from dev-dependencies to dependencies)
- Test: `crates/app/tests/{intent_model.rs,link.rs}`

**Interfaces:**
- Consumes: Task 2's core types.
- Produces:

```rust
// model.rs
pub enum Action { /* existing variants */ IntentsLoaded(Vec<Intent>), IntentUpserted(Intent), AgentDefaultsLoaded(AgentDefaults) }
impl ShellModel {
    pub fn intents(&self) -> Vec<&Intent>;                 // newest updated_at first; ties by id
    pub fn intent(&self, id: IntentId) -> Option<&Intent>;
    pub fn intents_loaded(&self) -> bool;                  // an IntentsLoaded was applied since the app started
    pub fn agent_defaults(&self) -> Option<&AgentDefaults>;
}

// link.rs
pub type IntentAnswer = LocalBoxFuture<'static, Result<Intent, LinkError>>;
pub trait IntentCommands {
    fn create_intent(&self, req: CreateIntentRequest) -> IntentAnswer;
    fn update_intent(&self, id: IntentId, patch: Patch) -> IntentAnswer;
    fn answer_question(&self, id: IntentId, question: QuestionId, answer: AnswerInput) -> IntentAnswer;
    fn retry_polish(&self, id: IntentId) -> IntentAnswer;
    fn review_item(&self, id: IntentId, item: ItemId, action: ReviewAction) -> IntentAnswer;   // accept_item | edit_item | drop_item
    fn send_intent(&self, id: IntentId) -> IntentAnswer;
    fn resolve_intent(&self, id: IntentId) -> IntentAnswer;
    fn set_agent_defaults(&self, defaults: AgentDefaults) -> LocalBoxFuture<'static, Result<AgentDefaults, LinkError>>;
}
impl IntentCommands for DaemonLink { .. }   // inherent methods of the same names return `impl Future + Send + 'static`, like create_agent

// shell.rs / shell_state.rs
pub type Clock = Rc<dyn Fn() -> chrono::DateTime<chrono::FixedOffset>>;   // defined in shell_state.rs
pub struct ShellOptions { /* existing */ pub intent_commands: Option<Rc<dyn IntentCommands>>, pub clock: Option<Clock> }
impl ShellState {
    pub fn intent_commands(&self) -> Option<&dyn IntentCommands>;   // injected, else the link, else None
    pub fn now(&self) -> chrono::DateTime<chrono::FixedOffset>;     // the injected clock, else chrono::Local::now().fixed_offset()
}

// tests/common/intents.rs
pub struct FakeIntents { pub calls: RefCell<Vec<IntentCall>>, .. }   // each method records its call, then answers the next queued result or waits for `answer`
pub enum IntentCall { Create(CreateIntentRequest), Update(IntentId, Patch), Answer(IntentId, QuestionId, AnswerInput), Retry(IntentId), Review(IntentId, ItemId, ReviewAction), Send(IntentId), Resolve(IntentId), SetDefaults(AgentDefaults) }
impl FakeIntents { pub fn queue(&self, result: Result<Intent, LinkError>); pub fn answer(&self, result: Result<Intent, LinkError>); pub fn api_error(status: u16, code: &str, message: &str) -> LinkError; }
pub fn intent(slug: &str, state: IntentState, phase: Phase) -> Intent;   // repo fixed per test, times "2026-11-26T14:15:00Z"
pub fn item(text: &str, status: ItemStatus) -> Item;
pub fn question(text: &str, options: &[&str], answer: Answer) -> Question;   // option ids "o1", "o2", ..
pub fn fixed_clock(rfc3339: &str) -> Clock;
pub fn model_with(intents: Vec<Intent>, agents: Vec<Agent>, repos: Vec<Repo>) -> ShellModel;   // connected, loaded
```

Model rules: `IntentsLoaded` replaces the whole list and sets loaded. `IntentUpserted`, `IntentCreated` and `IntentUpdated` insert the intent, or replace the stored copy unless the stored `updated_at` is later; an identical copy changes nothing (no revision bump). `AgentDefaultsLoaded` replaces the defaults. The event arm at `model.rs:216-217` no longer ignores the two intent events.

Link rules: inside the session's `fetch` (`link.rs:447-453`), after `list_agents`, also `list_intents(IntentFilter::default())` and `agent_defaults()`; after `Action::Snapshot` and before `ConnectionChanged(Connected)` send `IntentsLoaded` then `AgentDefaultsLoaded`. Every intent command sends `Action::IntentUpserted(answer.clone())` from the link's runtime before returning (the pattern of `add_repo`, `link.rs:269-279`); `set_agent_defaults` sends `AgentDefaultsLoaded`.

- [ ] **Step 1: Write the failing tests.**
  - `tests/intent_model.rs`: `loaded_intents_are_listed_newest_first` (three intents with `updated_at` 10:00, 12:00, 11:00 list as 12, 11, 10; `intents_loaded()` turns true; revision bumps once). `an_intent_event_inserts_then_replaces` (`IntentCreated` adds; `IntentUpdated` with a later `updated_at` replaces; the same event again returns no change and the same revision). `an_older_copy_never_replaces_a_newer_one` (Review Focus 1: upsert `updated_at` 12:00 with title "new", then `IntentUpserted` with 11:00 and title "old": `intent(id).content.title == "new"` and the revision is unchanged; an event with seq above `last_seq` carrying the 11:00 copy also leaves "new"). `a_reload_replaces_the_list` (after `IntentsLoaded(vec![a])`, `IntentsLoaded(vec![b])` leaves only `b`). `agent_defaults_are_kept` (`None` before; `Some(queue_limit: Some(3))` after).
  - `tests/link.rs` (real `smithagentsd` with `fake-claude`, as the file already does): `intents_and_defaults_arrive_before_connected` (create one intent through `client_for(&home)` first; a new link's actions are `Snapshot`, `IntentsLoaded` holding it, `AgentDefaultsLoaded`, `ConnectionChanged(Connected)` in that order). `an_intent_command_answers_and_upserts` (`link.create_intent` with a text seed in an `init_repo` repository answers an intent in `Draft`, and the receiver yields `IntentUpserted` with the same id). `intent_commands_need_a_connection` (a link whose daemon binary does not exist: `send_intent` answers `LinkError::NotConnected`).
- [ ] **Step 2: Run them.** `cargo test -p smithagents-app --test intent_model --test link`. Expected: compile errors.
- [ ] **Step 3: Implement** the actions, the model rules, the trait, the link methods, the two `ShellOptions` fields and their `ShellState` accessors, and `tests/common/intents.rs`.
- [ ] **Step 4: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(app): intents in the link and the model`

---

### Task 4: View models: every label, counter and sentence as pure functions

**Files:**
- Create: `crates/app/src/intent_vm.rs`, `crates/app/tests/intent_vm.rs`
- Modify: `crates/app/src/lib.rs` (`pub mod intent_vm;`), `crates/app/src/view_models.rs:795-806` (`one_line` and `capitalized` become `pub(crate)`)

**Interfaces:**
- Consumes: `ShellModel::{intents, intent, agent, repos}`, `view_models::{state_kind, state_label, state_dot, DotStyle, NOT_CONNECTED_TEXT, DAEMON_UNREACHABLE_TEXT}`.
- Produces (all in `intent_vm.rs`, no GPUI types):

```rust
pub fn kind_label(kind: IntentKind) -> &'static str;   // "New feature" | "Enhancement" | "Bug" | "Refactor" | "Maintenance" | "Task"
pub struct KindChoice { pub kind: IntentKind, pub title: &'static str, pub description: &'static str, pub key: &'static str, pub shortcut: &'static str }
pub const KIND_CHOICES: [KindChoice; 5];               // table below; Task is not offered
pub fn intent_state_label(intent: &Intent) -> &'static str;
pub enum IntentFilterChoice { All, DraftAndPolishing, CompleteAndQueued, Sent, Resolved }   // ALL, label(), as_str(), matches(&Intent)
pub struct BodyPreview { pub heading: Option<String>, pub line: Option<String> }
pub fn body_preview(markdown: &str) -> BodyPreview;
pub fn created_text(at: DateTime<Utc>, offset: FixedOffset) -> String;      // "Nov 26, 2026, 6:15 AM"
pub fn age_text(since: DateTime<Utc>, now: DateTime<Utc>) -> String;        // "now" | "5m" | "4h" | "3d"
pub fn intent_dot(intent: &Intent, agent: Option<AgentState>) -> DotStyle;
pub struct IntentCardVm { pub id: IntentId, pub slug: String, pub title: String, pub meta: String, pub preview: BodyPreview,
    pub dot: DotStyle, pub accent_hue: Option<f32>, pub a11y_label: String }
pub fn intent_cards(model: &ShellModel, filter: IntentFilterChoice, now: DateTime<FixedOffset>) -> Vec<IntentCardVm>;
pub fn list_empty_text(model: &ShellModel, filter: IntentFilterChoice) -> Option<&'static str>;
pub enum SectionKind { Questions, Preconditions, Criteria, Stories, DependsOn }   // ALL in this order; title(); as_str()
pub struct SectionVm { pub kind: SectionKind, pub title: &'static str, pub counter: String, pub status: String, pub pending: u32, pub skipped: u32, pub starts_open: bool }
pub fn section_vm(content: &Content, kind: SectionKind) -> SectionVm;
pub fn section_items(content: &Content, kind: SectionKind) -> &[Item];     // empty for Questions
pub fn answer_text(question: &Question) -> Option<String>;                 // "→ <label>" | "→ Skipped" | None
pub fn polishing_text(pass: u8) -> String;                                 // "Polishing… (pass 1 of 2)"
pub fn body_text(intent: &Intent) -> &str;                                 // current_behavior, or seed.text when that is blank
pub fn header_editable(intent: &Intent) -> bool;                           // Draft not polishing, or Complete
pub struct DependencyVm { pub item_id: ItemId, pub slug: Option<String>, pub title: String, pub status: ItemStatus }
pub fn dependency_vm(model: &ShellModel, item: &Item) -> DependencyVm;     // slug and title of intent_ref when loaded; else slug None, title = item.text
pub fn queued_reason(model: &ShellModel, intent: &Intent) -> String;
pub fn default_repo(repos: &[Repo], last_used: Option<RepoId>) -> Option<RepoId>;   // last_used while it exists, else the oldest by created_at
pub fn intent_error_text(error: &LinkError) -> String;
pub const QUEUE_LIMIT_CHOICES: std::ops::RangeInclusive<u32> = 1..=16;
pub fn queue_limit_label(limit: Option<u32>) -> String;                    // "No limit" | "3"
```

Exact values:

| Kind | title | description | key | shortcut |
|---|---|---|---|---|
| NewFeature | New feature | a new capability | `alt-f` | ⌥F |
| Enhancement | Enhancement | extend an existing capability | `alt-e` | ⌥E |
| Bug | Bug | a capability that misbehaves | `alt-b` | ⌥B |
| Refactor | Refactor | restructure, same behavior | `alt-r` | ⌥R |
| Maintenance | Maintenance | upgrades, config, housekeeping | `alt-m` | ⌥M |

- State labels: Draft with `Polishing` → "Polishing"; `Questions` → "Questions"; `Review` → "Review"; `Failed` → "Polish failed"; Complete → "Complete"; Queued → "Queued"; Sent → "Sent"; Resolved → "Resolved".
- Filter labels and membership: "All" (every intent); "Draft and polishing" (Draft); "Complete and queued" (Complete, Queued); "Sent"; "Resolved". `as_str`: `all`, `draft`, `complete`, `sent`, `resolved`.
- Meta line: parts joined with `" | "`: `created_text(created_at)`; the state label; for Sent, when `agent_id` is an agent in the model, `state_label(state_kind(agent.state))`; `age_text` since `sent_at` for Sent (when set), else since `updated_at`. Example: `Nov 26, 2026, 6:15 AM | Sent | done | 4h`.
- `age_text`: under 60 s "now"; under 60 min "{n}m"; under 24 h "{n}h"; else "{n}d"; a `since` after `now` is "now".
- `body_preview`: lines are split on `\n` with a trailing `\r` removed; code-fence lines (starting with three backticks) are skipped. The heading is the first line that starts with one to six `#` and a space, without the marks. `line` is the first non-empty line after the heading; with no heading, the first non-empty line. Both are made plain: `**`, `__` and backticks removed, a leading `- `, `* `, `> ` or `<digits>. ` removed, `[text](url)` becomes `text`; then trimmed and cut to 200 characters.
- Dots (`DotStyle { token, filled, dimmed }`): Polishing `status-info`, filled; Questions and Review `status-warn`, filled; Failed `status-error`, filled; Complete `status-ok`, hollow; Queued `muted-foreground`, filled, dimmed; Sent `state_dot(state_kind(agent))`, or `muted-foreground` hollow without an agent; Resolved `status-ok`, filled, dimmed.
- `accent_hue`: `Some(agent.color.h)` for a Sent intent whose agent is in the model, else `None`. `a11y_label`: "{title}, {state label}".
- `list_empty_text`: not connected and never loaded → `NOT_CONNECTED_TEXT`; no intents → "No intents yet. Press + to describe what you want."; intents but none match → "No intents match this filter."; else `None`.
- Section titles: "Questions", "Preconditions", "Acceptance criteria", "Stories", "Depends on". Counters: Questions "{answered or skipped} of {all} answered"; item sections "{accepted} of {all minus dropped} accepted"; Depends on "{accepted} picked". Status: Questions with any unanswered "{n} to answer"; an item section or Depends on with pending items "{n} to review"; a section with no items "none"; Depends on with no pending and no accepted items "none"; otherwise "✓ done". `skipped` is the dropped count (0 for Questions). `starts_open` is `pending > 0`.
- `queued_reason`: accepted depends-on items whose `intent_ref` is not Resolved in the model give "Waiting for {slugs joined with ", "} to be resolved." (an intent not in the model reads "another intent"); with none, "Waiting for a free agent slot."
- `intent_error_text`: `NotConnected` → `NOT_CONNECTED_TEXT`; `Stopped` → "The daemon link has stopped"; transport errors → `DAEMON_UNREACHABLE_TEXT`; API codes: `polishing` → "The intent is still being polished."; `invalid_state` → "The intent changed, so this is no longer possible."; `already_answered` → "That question is already answered."; `dependency_cycle` → "Those intents would depend on each other."; `dependency_cross_repo` → "A dependency must be in the same repository."; `intent_not_found` → "That intent no longer exists."; `repo_not_found` → "That repository no longer exists."; any other code → the daemon's message on one line, first letter capitalized, or "The daemon could not do that." when blank.

- [ ] **Step 1: Write the failing tests** in `tests/intent_vm.rs`, one test per bullet above, asserting the exact strings: `kind_labels_and_choices` (the table, in order; `KIND_CHOICES` has no Task). `state_labels` (all eight). `filter_membership` (one intent per state against each choice). `meta_line_for_a_sent_intent` (created 2026-11-26T14:15:00Z, offset −08:00, sent 4 h before now, agent `Done` → `Nov 26, 2026, 6:15 AM | Sent | done | 4h`; the same intent with its agent absent → `Nov 26, 2026, 6:15 AM | Sent | 4h`; a Draft in Review updated 5 min ago → `... | Review | 5m`). `age_steps` (59 s "now", 60 s "1m", 59 min "59m", 60 min "1h", 23 h "23h", 24 h "1d", future "now"). `body_preview_reads_the_first_heading_and_line` (`"## What happens\n\nIn the **app**, `x` is [wrong](http://e).\n"` → heading "What happens", line "In the app, x is wrong."). `body_preview_survives_odd_markdown` (Review Focus 4: `""` → both `None`; `"just text"` → heading `None`, line "just text"; a fence-only body → both `None`; CRLF input gives no `\r`; a 10,000-character line of "é" gives a 200-character line; `"# Only a heading"` → line `None`). `dots_by_state` (each row of the dot list). `cards_are_filtered_and_ordered` (order follows `model.intents()`; `accent_hue` only for Sent with an agent). `empty_texts` (the four cases). `section_counters_and_status` (Questions 1 answered, 1 skipped, 1 open → "2 of 3 answered", "1 to answer", `starts_open`; criteria accepted, accepted, dropped → "2 of 2 accepted", "✓ done", `skipped == 1`, closed; stories empty → "0 of 0 accepted", "none"; depends-on with one pending → "0 picked", "1 to review"; with one dropped only → "0 picked", "none"; with one accepted → "1 picked", "✓ done"). `answers` ("→ Yes", "→ Skipped", `None`). `polishing_text(2) == "Polishing… (pass 2 of 2)"`. `body_falls_back_to_the_seed`. `header_is_editable_only_in_draft_and_complete` (false while Polishing, in Queued, Sent, Resolved). `queued_reasons` (two unresolved dependencies `a-one`, `b-two` → "Waiting for a-one, b-two to be resolved."; one resolved and none pending → "Waiting for a free agent slot."; an unknown `intent_ref` → "Waiting for another intent to be resolved."). `default_repo_prefers_the_last_used` (and `None` with no repositories). `error_sentences` (Review Focus 3: each code above, an unknown code with message `"store:  bad  thing"` → "Store: bad thing", a blank message, `NotConnected`). `queue_limit_labels`.
- [ ] **Step 2: Run them.** `cargo test -p smithagents-app --test intent_vm`. Expected: compile errors.
- [ ] **Step 3: Implement `intent_vm.rs`.** Format the date with chrono's `%b %-d, %Y, %-I:%M %p` after `with_timezone(&offset)`.
- [ ] **Step 4: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(app): intent view models`

---

### Task 5: Intents pane: list, cards, filter, empty states, and the command runner

**Files:**
- Create: `crates/app/src/views/intents/list.rs`, `crates/app/tests/intents_pane.rs`
- Modify: `crates/app/src/views/intents/mod.rs`, `crates/app/src/shell.rs:117-129` (call `views::intents::init`)

**Interfaces:**
- Consumes: Task 1 (`IntentsPane`, `AppIcons`), Task 3 (`IntentCommands`, `ShellState::{intent_commands, now, command_timeout}`), Task 4.
- Produces:

```rust
// views/intents/mod.rs
pub const KEY_CONTEXT: &str = "IntentsPane";
pub const INTENTS_FILTER_ID: &str = "intents-filter";     pub const INTENTS_NEW_ID: &str = "intents-new";
pub const INTENT_LIST_ID: &str = "intent-list";           pub const INTENTS_EMPTY_ID: &str = "intents-empty";
pub const INTENT_ERROR_ID: &str = "intent-error";         // the screen's one error line, Role::Status
pub const CARD_DOT_ID: &str = "dot"; pub const CARD_ACCENT_ID: &str = "accent"; pub const CARD_SLUG_ID: &str = "slug";
pub const CARD_TITLE_ID: &str = "title"; pub const CARD_META_ID: &str = "meta"; pub const CARD_HEADING_ID: &str = "heading"; pub const CARD_LINE_ID: &str = "line";
pub fn intent_card_id(id: IntentId) -> ElementId;         // "intent-card-<uuid>"
pub fn filter_item_id(choice: IntentFilterChoice) -> ElementId;   // "intents-filter-<as_str>"
pub fn init(cx: &mut App);                                // the pane's key bindings (Task 6 adds the create keys)

pub enum Screen { List, Create, Detail(IntentId) }
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Busy { Create, Title, Kind, Retry, Question(QuestionId), Item(ItemId), Send, Resolve }
impl IntentsPane {
    pub fn screen(&self) -> &Screen;
    pub fn filter(&self) -> IntentFilterChoice;
    pub(crate) fn open_screen(&mut self, screen: Screen, window: &mut Window, cx: &mut Context<Self>);   // clears the error line
    pub(crate) fn busy(&self) -> Option<Busy>;
    /// Sends one command. Does nothing while another is in flight. Clears the error line, marks `key` busy,
    /// waits with `answered(command, timer(command_timeout))`; then clears busy and either calls `done` with
    /// the answer or sets the error line (`intent_error_text`, or DAEMON_SILENT_TEXT on a timeout).
    pub(crate) fn run(&mut self, key: Busy, command: IntentAnswer,
        done: impl FnOnce(&mut Self, Intent, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window, cx: &mut Context<Self>);
    pub(crate) fn error_line(&self, paint: &Paint) -> Option<AnyElement>;   // id INTENT_ERROR_ID
}
```

Layout (photo of the list; theme tokens, not the photo's colors): the heading "Intents" with two icon buttons at the right: filter (`IconName::ListFilter`, tooltip and accessible name "Filter") and new (`IconName::Plus`, "New intent"). The filter button opens a dropdown menu (research D4) labelled "Show" with the five choices, the current one checked; each entry is a `PopupMenuItem::element` with its own named node, as `picker_menu` does (`crates/app/src/views/agents/new_agent.rs:192-230`). The list is a `Role::List` column in a `flex_1().min_h_0().overflow_y_scroll().track_scroll(..)` container (research D2). A card is a focusable `Role::Button` row (tab stop, focus ring, hover; `aria_label` = the view model's `a11y_label`), in the style of `expanded_row` (`crates/app/src/views/agents/mod.rs:398-432`): a 3 px left bar in the agent's accent when `accent_hue` is set (`agent_accent`), else in the card's border color; the status dot drawn like the agents pane's `status_dot`; the slug in a small monospace `Tag` (research D3); the title on one truncated line; the meta line muted and truncated; the preview heading in semibold and the preview line, each truncated, each left out when `None`. Clicking a card, or Enter on it, opens `Screen::Detail`. The filter lives in the pane and resets when the app restarts.

Pane state changes: the pane observes `ShellState`. When the connection drops while a command is busy, it gives the command up with `DAEMON_SILENT_TEXT`. When `Screen::Detail(id)` names an intent the model no longer has after a load, it goes back to `Screen::List`. A 60 s timer re-renders the list so ages advance.

- [ ] **Step 1: Write the failing tests** in `tests/intents_pane.rs`. A `Setup` helper opens the shell with `model_with(..)`, a `FakeIntents`, `fixed_clock("2026-11-26T10:15:00-08:00")`, clicks `rail-intents`, and offers `click`, `press`, `label_of(id)` like `settings_pane.rs:118-210`.
  - `a_card_shows_slug_title_meta_and_preview`: one Sent intent (slug `browser-pane-zoom-bounds`, title "Fix Browser page drawn too big", body `"## What happens\nThe page is drawn in the wrong place."`, created 2026-11-26T14:15:00Z, sent 4 h ago, agent `Done`): within its card, `slug` reads the slug, `title` the title, `meta` `Nov 26, 2026, 6:15 AM | Sent | done | 4h`, `heading` "What happens", `line` "The page is drawn in the wrong place."; the quad at the accent bar is painted in the agent's accent (`quad_at`), and the dot in `status-ok`.
  - `cards_are_newest_first_and_follow_events`: two intents; dispatch `Action::Event` with `IntentUpdated` for the older one carrying a later `updated_at` and a new title: it moves to the top and shows the new title.
  - `the_filter_narrows_the_list`: Draft, Complete, Sent, Resolved intents; choose "Sent" from the menu: only the Sent card is found and the Sent entry is checked on reopening; choose "Resolved" then "All".
  - `empty_states`: no intents → `INTENTS_EMPTY_ID` reads "No intents yet. Press + to describe what you want."; a filter with no match → "No intents match this filter."; a disconnected, never loaded model → "Not connected to the daemon".
  - `a_card_opens_its_detail_by_click_and_by_enter` (`pane.screen()` is `Detail(id)`); `the_plus_button_opens_the_create_screen`.
  - `a_silent_daemon_frees_the_control_and_says_so` (Review Focus 2; `command_timeout` 5 s): call `pane.run(Busy::Send, fake.send_intent(id), ..)` with no queued answer; `busy()` is `Some(Busy::Send)`; a second `run` records no second call; after `advance_clock(6 s)` `busy()` is `None` and `INTENT_ERROR_ID` reads "The daemon did not answer". The same with `Action::ConnectionChanged(Reconnecting)` instead of the clock.
  - `a_reconnect_that_no_longer_has_the_open_intent_returns_to_the_list` (Review Focus 2): open a detail, dispatch `IntentsLoaded(vec![])`: the screen is `List`.
  - `a_refused_command_shows_the_daemons_sentence`: queue `api_error(409, "invalid_state", "..")`: the line reads "The intent changed, so this is no longer possible."; opening another screen clears it.
- [ ] **Step 2: Run them.** `cargo test -p smithagents-app --test intents_pane`. Expected: compile errors, then failures (the pane draws only its heading).
- [ ] **Step 3: Implement** `list.rs`, the screen switch, `run`, the error line and a `Paint` struct like the settings pane's (`crates/app/src/views/settings/mod.rs:357-391`) with `status-ok` added. `Screen::Create` and `Screen::Detail` draw a back button ("Intents", `IconName::ArrowLeft`) and nothing else until Tasks 6 and 7.
- [ ] **Step 4: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(app): the Intents pane lists intents with a filter`

---

### Task 6: Create screen

**Files:**
- Create: `crates/app/src/views/intents/create.rs`, `crates/app/tests/intent_create.rs`
- Modify: `crates/app/src/views/intents/mod.rs` (the textarea entity, `init`)

**Interfaces:**
- Consumes: Task 5 (`run`, `open_screen`, `Busy::Create`), Task 4 (`KIND_CHOICES`, `default_repo`), `ShellState::{last_repo_id, set_last_repo_id, request_settings_focus}`, `view_models::{repo_title, NEED_REPO_TEXT}`.
- Produces:

```rust
pub const CREATE_KEY_CONTEXT: &str = "IntentCreate";
pub const CREATE_INPUT_ID: &str = "intent-create-text";    // accessible name "Describe what you want"
pub const CREATE_CLOSE_ID: &str = "intent-create-close";   // IconName::X, "Close"
pub const CREATE_REPO_ID: &str = "intent-create-repo";
pub fn kind_row_id(kind: IntentKind) -> ElementId;         // "intent-kind-<wire_name>"
// actions (namespace smithagents_intents): ChooseNewFeature, ChooseEnhancement, ChooseBug, ChooseRefactor, ChooseMaintenance
// bound in views::intents::init to KindChoice::key, in CREATE_KEY_CONTEXT only
```

Behavior: a multi-line `Textarea` (`TextareaState::new(window, cx).auto_grow(3, 12)`, placeholder "Describe what you want…") that takes focus when the screen opens; Enter inserts a line. Under it a muted line "In {repo_title}" (id `CREATE_REPO_ID`); with two or more repositories it is a small ghost button with a dropdown of the repositories, the chosen one checked. With no repository the line reads `NEED_REPO_TEXT` and is a button that calls `request_settings_focus(SettingsSection::Repositories)`. Then five rows from `KIND_CHOICES`, each a `Role::Button` showing the title, the description and the shortcut, with accessible name "{title}, {description}". The rows are disabled while the trimmed text is empty, while there is no repository, and while a command is busy. Choosing a row, or its ⌥ key while focus is inside the screen, runs `create_intent(CreateIntentRequest { repo_id: Some(repo), kind: Some(kind), seed: SeedInput::Text(text) })` with the text as typed. On success: remember the repository (`set_last_repo_id`), clear the textarea, open `Screen::Detail(new id)`. On failure the text stays and the error line shows under the rows. The close button and Escape (the input's `Escape` action propagates, research B4) return to the list and keep the text for the next visit. The paperclip of the old app is not drawn.

- [ ] **Step 1: Verify first: do the ⌥ keys beat text entry?** Write `option_b_creates_a_bug_and_types_nothing` (Review Focus 5): open the create screen, `window.input("Fix the zoom", cx)`, `window.press("alt-b", cx)`; assert one `IntentCall::Create` whose request has `kind: Some(Bug)` and `seed == SeedInput::Text("Fix the zoom")`. If the key reaches the textarea instead (the text gains a character or no call is made), drop the key bindings and handle the five keys in a `capture_key_down` listener on the screen's root that matches `modifiers.alt` and the key letter and stops propagation; the test stays the same.
- [ ] **Step 2: Write the remaining failing tests** in `tests/intent_create.rs`: `the_screen_shows_five_kinds_with_descriptions_and_shortcuts` (rows in the table's order with accessible names "New feature, a new capability" and so on; no row for Task; no paperclip: `try_find` of an attach control is `None`). `rows_are_disabled_until_there_is_text` (Review Focus 5: empty, and after typing three spaces and a newline, every row is `disabled`; clicking one records no call; after typing "x" they are enabled). `choosing_a_kind_creates_and_opens_the_detail` (click the Refactor row with text "Tidy the store": the call carries `Refactor`, the model's only repository and the text; answer with an intent: the screen is `Detail(id)`, the textarea is empty, `last_repo_id` is that repository). `the_row_shows_the_spinner_while_creating` (before the answer: all rows disabled, `busy() == Some(Busy::Create)`). `a_failed_create_keeps_the_text` (queue `api_error(400, "invalid", "seed text must be at most 204800 bytes")`: the error line reads "Seed text must be at most 204800 bytes", the text is still there, the screen is still `Create`). `shortcuts_work_only_on_the_create_screen` (on the list, `alt-b` records no call). `escape_and_close_return_to_the_list_and_keep_the_text`. `the_repository_is_named_and_can_be_changed` (two repositories, the second last used: the line reads "In {its folder name}"; pick the other from the menu; the create call carries it). `no_repository_points_to_settings` (the line reads "Add a source repository first", rows disabled; clicking it shows Settings).
- [ ] **Step 3: Run them.** `cargo test -p smithagents-app --test intent_create`. Expected: FAIL (the screen is empty).
- [ ] **Step 4: Implement** `create.rs`.
- [ ] **Step 5: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 6: Commit.** `feat(app): create an intent from text and a kind`

---

### Task 7: Detail: header, markdown body, polishing and failed states

**Files:**
- Create: `crates/app/src/views/intents/detail.rs`, `crates/app/tests/intent_detail.rs`
- Modify: `crates/app/src/views/intents/mod.rs`

**Interfaces:**
- Consumes: Task 5 (`run`, `Busy::{Title, Kind, Retry}`), Task 4 (`intent_state_label`, `kind_label`, `KIND_CHOICES`, `header_editable`, `body_text`, `polishing_text`).
- Produces:

```rust
pub const DETAIL_BACK_ID: &str = "intent-back";            // label "Intents", IconName::ArrowLeft
pub const DETAIL_STATE_ID: &str = "intent-state";          pub const DETAIL_SLUG_ID: &str = "intent-slug";
pub const DETAIL_KIND_ID: &str = "intent-kind";            pub fn kind_item_id(kind: IntentKind) -> ElementId;   // "intent-kind-item-<wire_name>"
pub const DETAIL_TITLE_ID: &str = "intent-title";          pub const DETAIL_TITLE_INPUT_ID: &str = "intent-title-input";
pub const DETAIL_SCROLL_ID: &str = "intent-scroll";        pub const DETAIL_BODY_ID: &str = "intent-body";
pub const POLISHING_ID: &str = "intent-polishing";         pub const POLISH_FAILED_ID: &str = "intent-polish-failed";
pub const RETRY_ID: &str = "intent-retry";                 // label "Try again"
impl IntentsPane {
    /// The detail's parts below the body, in order; Tasks 8 to 10 fill them. Empty while the phase is Polishing.
    pub(crate) fn detail_sections(&self, intent: &Intent, paint: &Paint, cx: &Context<Self>) -> Vec<AnyElement>;
    pub(crate) fn detail_actions(&self, intent: &Intent, paint: &Paint, cx: &Context<Self>) -> Option<AnyElement>;
}
```

Layout (photos of the detail): a fixed header: the back button at the left and the state label at the right; then the slug in a monospace `Tag` and the kind chip; then the title. Under the header one scrolling column (`DETAIL_SCROLL_ID`) holds the body, then `detail_sections`; `detail_actions` is fixed at the bottom with the error line above it.
- Kind chip: when `header_editable`, a small outline button showing `kind_label` whose dropdown lists the five `KIND_CHOICES` titles (the current one checked; none checked for Task); choosing another runs `update_intent(id, Patch { kind: Some(k), ..Default::default() })` under `Busy::Kind`. Otherwise a read-only `Tag` with the label.
- Title: when `header_editable`, a `Role::Button` (accessible name "Edit title: {title}"); click or Enter swaps it for a single-line `Input` holding the title, focused. `InputEvent::PressEnter` with a non-blank trimmed text different from the title runs `update_intent` with `title` under `Busy::Title` and the input closes when the answer arrives; with the same or blank text it just closes. Escape closes it unchanged. Otherwise the title is a `Label`.
- Body: `TextView::markdown(DETAIL_BODY_ID, body_text(intent)).selectable(true)` inside a wrapper `div` with id `DETAIL_BODY_ID` and `.test_support()` (research D1); read-only.
- Phase `Polishing { pass }`: a row with `Spinner::new().small()` and a `Label` `polishing_text(pass)` (id `POLISHING_ID`); no sections and no actions. Phase `Failed { message, .. }`: a `Role::Status` block (id `POLISH_FAILED_ID`) reading "Polish failed: {message}" and a "Try again" button that runs `retry_polish` under `Busy::Retry`; sections that already have items are still drawn.

- [ ] **Step 1: Verify first: what a test can see of `TextView::markdown`.** In `the_body_is_drawn_from_the_markdown`, open a detail with body `"## What happens\n\nThe page is **wrong**.\n"`, `cx.run_until_parked()`, render, and try `find_by_label("What happens")`. If it is found, assert it and that `find_by_label("## What happens")` is not. If not, assert instead that the wrapper `DETAIL_BODY_ID` has a height greater than 0 and that `pane.body_source(cx)` equals the markdown (add the accessor `pub fn body_source(&self, cx: &App) -> Option<String>`, which returns the text handed to the view for the open detail).
- [ ] **Step 2: Write the remaining failing tests** in `tests/intent_detail.rs`: `the_header_shows_state_slug_kind_and_title` (a Sent Bug: `DETAIL_STATE_ID` "Sent", slug, kind "Bug", title; the kind is not a button and the title is not editable). `an_old_task_kind_reads_task`. `the_kind_menu_patches_the_kind` (a Draft in Review, kind Bug: the menu lists New feature, Enhancement, Bug, Refactor, Maintenance with Bug checked; choose Refactor: one `IntentCall::Update` with `kind: Some(Refactor)` and nothing else; the chip still reads "Bug" until `IntentUpserted` with the answer is dispatched, then "Refactor"). `enter_saves_the_title_and_escape_cancels` (click the title, clear and type "New title", Enter: one `Update` with `title: Some("New title")`; again with Escape: no call and the old title shows; Enter with only spaces: no call). `the_title_and_kind_are_fixed_while_polishing_and_after_send`. `polishing_shows_the_pass_and_no_review_controls` (`Polishing { pass: 1 }` with a blank body: `POLISHING_ID` reads "Polishing… (pass 1 of 2)"; `try_find(SEND_ID)` and any section header are `None`). `an_empty_body_shows_the_seed` (Review Focus 4: the body source is the seed text). `a_failed_polish_offers_try_again` (`Failed { pass: 2, message: "the harness timed out" }`: the block reads "Polish failed: the harness timed out"; click "Try again": one `IntentCall::Retry`; dispatch the answer in `Polishing { pass: 2 }`: the block is gone and "Polishing… (pass 2 of 2)" shows). `back_returns_to_the_list`. `a_very_long_body_scrolls` (Review Focus 4: a 10,000-character single line: the frame renders and the pane's root is no wider than the dock).
- [ ] **Step 3: Run them.** `cargo test -p smithagents-app --test intent_detail`. Expected: FAIL.
- [ ] **Step 4: Implement** `detail.rs`; `detail_sections` and `detail_actions` return nothing yet.
- [ ] **Step 5: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 6: Commit.** `feat(app): intent detail with header editing, the body and polish states`

---

### Task 8: Detail: sections and Questions

**Files:**
- Create: `crates/app/src/views/intents/sections.rs`, `crates/app/tests/intent_review.rs`
- Modify: `crates/app/src/views/intents/{mod.rs,detail.rs}`

**Interfaces:**
- Consumes: Task 7 (`detail_sections`), Task 4 (`SectionKind`, `section_vm`, `answer_text`), Task 5 (`run`, `Busy::Question`).
- Produces:

```rust
pub fn section_header_id(kind: SectionKind) -> ElementId;    // "intent-section-<as_str>"; a button with aria_expanded
pub fn section_counter_id(kind: SectionKind) -> ElementId;   // "intent-counter-<as_str>"
pub fn section_status_id(kind: SectionKind) -> ElementId;    // "intent-status-<as_str>"
pub fn question_card_id(id: QuestionId) -> ElementId;        // "intent-question-<uuid>"
pub fn question_option_id(question: QuestionId, option: &str) -> ElementId;   // "intent-option-<uuid>-<option id>"
pub fn question_skip_id(question: QuestionId) -> ElementId;  // "intent-question-skip-<uuid>"; label "Skip"
pub const QUESTION_ANSWER_ID: &str = "answer";               // inside a question card
impl IntentsPane {
    /// One collapsible section: header (chevron, title, counter, status at the right), then `content` while open.
    pub(crate) fn section(&self, intent: &Intent, kind: SectionKind, content: AnyElement, paint: &Paint, cx: &Context<Self>) -> AnyElement;
    pub(crate) fn section_open(&self, intent: IntentId, kind: SectionKind, vm: &SectionVm) -> bool;   // the user's toggle, else vm.starts_open
}
```

Behavior: all five sections are drawn in `SectionKind::ALL` order, hand-made like the Settings sections (`crates/app/src/views/settings/mod.rs:203-249`): the header is a full-width ghost button with a chevron, the title in semibold, the counter muted beside it, and the status at the right ("✓ done" and "none" in the `status-ok` token, the others in `status-warn`). The user's toggles are kept in a map keyed by intent and section for the pane's lifetime. Questions: one outlined card per question with its text; an unanswered one shows a button per option (label = the option's label) and "Skip"; an answered or skipped one shows `answer_text` in the `QUESTION_ANSWER_ID` label and no buttons. An option runs `answer_question(id, qid, AnswerInput::Option(option_id))`, Skip runs `AnswerInput::Skip`, both under `Busy::Question(qid)`. Option buttons are drawn only in phase `Questions`. Task 9 fills the other four sections; until then they show their header only.

- [ ] **Step 1: Write the failing tests** in `tests/intent_review.rs`: `section_headers_show_counters_and_status` (an intent in Questions with 2 questions, one answered: Questions reads "1 of 2 answered" and "1 to answer" and is expanded; Preconditions reads "0 of 0 accepted" and "none" and is collapsed; Depends on reads "0 picked" and "none"). `a_done_section_starts_collapsed_and_the_toggle_is_remembered` (all answered: collapsed, status "✓ done"; click the header: expanded, `expanded == Some(true)`; open the list and the same detail again: still expanded). `choosing_an_option_answers_the_question` (click the option "Yes" of the first question: one `IntentCall::Answer(id, qid, AnswerInput::Option("o1"))`; the buttons stay until the answer is dispatched; then the card reads "→ Yes" and has no buttons, and the counter reads "2 of 2 answered"). `skip_shows_skipped` ("→ Skipped"). `options_are_disabled_while_another_answer_is_in_flight` (after clicking an option of question 1, question 2's buttons are `disabled` and clicking one records no call). `an_already_answered_question_says_so` (Review Focus 3: queue `api_error(409, "already_answered", "..")`: the error line reads "That question is already answered."; dispatching the daemon's copy with the answer shows it). `questions_are_read_only_after_the_questions_phase` (a Sent intent: no option or Skip button is found; the answers show).
- [ ] **Step 2: Run them.** `cargo test -p smithagents-app --test intent_review`. Expected: FAIL.
- [ ] **Step 3: Implement** `sections.rs` (the section frame and Questions) and return the sections from `detail_sections`.
- [ ] **Step 4: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(app): intent sections and steering questions`

---

### Task 9: Detail: Preconditions, Acceptance criteria, Stories and Depends on

**Files:**
- Modify: `crates/app/src/views/intents/sections.rs`, `crates/app/tests/intent_review.rs`

**Interfaces:**
- Consumes: Task 8 (`section`), Task 4 (`section_items`, `dependency_vm`), Task 5 (`run`, `Busy::Item`).
- Produces:

```rust
pub fn item_card_id(id: ItemId) -> ElementId;        // "intent-item-<uuid>"
pub fn item_confirm_id(id: ItemId) -> ElementId;     // "intent-item-confirm-<uuid>"; label "Confirm"
pub fn item_edit_id(id: ItemId) -> ElementId;        // "intent-item-edit-<uuid>"; label "Edit"
pub fn item_skip_id(id: ItemId) -> ElementId;        // "intent-item-skip-<uuid>"; label "Skip"
pub fn item_input_id(id: ItemId) -> ElementId;       // "intent-item-input-<uuid>"
pub fn skipped_line_id(kind: SectionKind) -> ElementId;   // "intent-skipped-<as_str>"
pub const ITEM_TEXT_ID: &str = "text";               // inside an item card
```

Behavior: review controls are drawn when the intent is a Draft in phase `Review` or is Complete (the states the daemon's review routes accept); elsewhere items are read-only.
- Preconditions, Acceptance criteria, Stories: one outlined card per item that is not dropped, in the daemon's order. An Accepted card has its outline in the `status-ok` token and no buttons. A Pending card has "Confirm" (`ReviewAction::Accept`), "Edit" and "Skip" (`ReviewAction::Drop`). Edit swaps the text for a single-line `Input` holding it, focused: Enter with non-blank text runs `ReviewAction::Edit(trimmed text)` (the daemon edits and accepts); Escape, or Enter with blank text, closes the input unchanged. One item is edited at a time. All three run under `Busy::Item(item id)`. Dropped items are not drawn; when the section has any, a muted line (id `skipped_line_id`) reads "{n} skipped".
- Depends on: one card per item that is not dropped. Its text is the dependency's slug in a monospace `Tag` and its title (`dependency_vm`); when the intent is not loaded, the item's own text. A Pending card has "Confirm" and "Skip" only; an Accepted one has the `status-ok` outline. The "{n} skipped" line applies too.

- [ ] **Step 1: Write the failing tests** (added to `tests/intent_review.rs`): `a_pending_item_offers_confirm_edit_and_skip` (a Review intent with criteria Pending, Accepted, Dropped: the Pending card has the three buttons; the Accepted card has none and its border quad is painted in `status-ok`; the Dropped item's text is not found; the skipped line reads "1 skipped"; the counter reads "1 of 2 accepted" and the status "1 to review"). `confirm_accepts_the_item` (one `IntentCall::Review(id, item, ReviewAction::Accept)`; the buttons stay until the answer is dispatched; then the section reads "2 of 2 accepted", "✓ done"). `skip_drops_the_item` (`ReviewAction::Drop`; after the answer the card is gone and the line reads "2 skipped"). `edit_saves_on_enter_as_edit_and_accept` (click Edit: the input holds the item's text and has focus; clear, type "Better wording", Enter: `ReviewAction::Edit("Better wording")`). `escape_and_blank_text_cancel_an_edit` (no call; the old text shows). `a_second_press_while_busy_sends_nothing` (Review Focus 3: click Confirm on item A, then Confirm on item B and Skip on A before the answer: exactly one call; A's Confirm shows loading and every other control is `disabled`). `a_refusal_leaves_the_item_pending` (Review Focus 3: queue `api_error(409, "polishing", "..")`: the error line reads "The intent is still being polished." and the card still has its buttons). `items_are_read_only_once_sent` (no Confirm, Edit or Skip found; accepted outlines still drawn). `a_complete_intent_can_still_be_reviewed` (Complete with all accepted: no buttons, but a Pending item arriving through an update shows them). `a_dependency_shows_the_other_intents_slug_and_title` (the candidate's `intent_ref` is a loaded intent `browser-pane-zoom-bounds` / "Fix Browser page drawn too big": the card shows both; it has Confirm and Skip and no Edit; the header reads "0 picked", "1 to review"). `confirming_a_dependency_picks_it` ("1 picked", "✓ done"). `a_dependency_cycle_is_explained` (queue `api_error(409, "dependency_cycle", "..")`: "Those intents would depend on each other."). `a_dependency_on_an_unknown_intent_shows_its_own_text`.
- [ ] **Step 2: Run them.** `cargo test -p smithagents-app --test intent_review`. Expected: the new tests FAIL.
- [ ] **Step 3: Implement** the item cards and the Depends on cards in `sections.rs`.
- [ ] **Step 4: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(app): confirm, edit and skip proposed items and dependencies`

---

### Task 10: Actions: Send, Queued reason, Open chat, Mark resolved, start error

**Files:**
- Create: `crates/app/src/views/intents/actions.rs`, `crates/app/tests/intent_actions.rs`
- Modify: `crates/app/src/views/intents/{mod.rs,detail.rs}`

**Interfaces:**
- Consumes: Task 7 (`detail_actions`), Task 4 (`queued_reason`), Task 5 (`run`, `Busy::{Send, Resolve}`), `Action::OpenTab`, the confirm-dialog pattern of `crates/app/src/views/agents/close.rs:127-170`.
- Produces:

```rust
pub const SEND_ID: &str = "intent-send";                      // label "Send", primary
pub const QUEUED_REASON_ID: &str = "intent-queued-reason";
pub const OPEN_CHAT_ID: &str = "intent-open-chat";            // label "Open chat"
pub const RESOLVE_ID: &str = "intent-resolve";                // label "Mark resolved…"
pub const RESOLVE_TITLE_ID: &str = "intent-resolve-title";    // "Mark as resolved?"
pub const RESOLVE_CANCEL_ID: &str = "intent-resolve-cancel";  // "Cancel", focused when the dialog opens
pub const RESOLVE_CONFIRM_ID: &str = "intent-resolve-confirm";   // "Mark resolved"
pub const START_ERROR_ID: &str = "intent-start-error";        // Role::Status
pub const RESOLVE_DESCRIPTION: &str = "This means the work has landed on the default branch, so intents that depend on it can start.";
```

Behavior by state: Draft and Resolved: no bar. Complete: a primary "Send" button that runs `send_intent` under `Busy::Send`. Queued: a muted `Label` with `queued_reason`. Sent: "Open chat" and "Mark resolved…". "Open chat" dispatches `Action::OpenTab(agent_id)` (which also selects the agent on the left); it is disabled, with the tooltip "The agent was closed", when the model has no such agent. "Mark resolved…" opens an alert dialog titled "Mark as resolved?" with `RESOLVE_DESCRIPTION`; Cancel is focused and is what Enter does (`on_ok` only dismisses); "Mark resolved" runs `resolve_intent` under `Busy::Resolve`. When `last_start_error` is set, a `Role::Status` line above the bar reads "The agent could not start: {error}" in the `destructive` token, in every state. No key binding dispatches Send or resolve, and the pane never moves focus onto either button.

- [ ] **Step 1: Write the failing tests** in `tests/intent_actions.rs`: `a_complete_intent_offers_send` (one "Send" button; a Draft in Review, a Queued and a Resolved intent have none). `send_waits_for_the_daemon` (click: one `IntentCall::Send`; the button shows loading and the state label still reads "Complete"; dispatch the answer in Sent with an agent: the label reads "Sent" and the bar shows "Open chat" and "Mark resolved…"). `a_queued_intent_says_what_it_waits_for` (an accepted dependency on the unresolved intent `repos-integration-pane`: `QUEUED_REASON_ID` reads "Waiting for repos-integration-pane to be resolved."; with no unresolved dependency: "Waiting for a free agent slot."). `open_chat_selects_the_agent_and_opens_its_tab` (with `fake_connection()`: after the click `model.selected()` and `model.active_tab()` are the intent's agent). `open_chat_is_disabled_when_the_agent_is_gone`. `mark_resolved_asks_first` (click: the dialog's title reads "Mark as resolved?" and its text is `RESOLVE_DESCRIPTION`; no call yet; Cancel has focus). `enter_in_the_dialog_cancels` (press `enter`: the dialog closes and no `Resolve` call was recorded). `confirming_resolves` (click `RESOLVE_CONFIRM_ID`: one `IntentCall::Resolve`; after the answer the state reads "Resolved" and the bar is gone). `enter_never_sends` (a Complete intent: press `enter` with focus on the pane, on an item card, and inside the title input after opening it: no `Send` call in any case). `a_start_error_is_shown_above_the_bar` (a Complete intent with `last_start_error: Some("no directory at /moved/repo")`: `START_ERROR_ID` reads "The agent could not start: no directory at /moved/repo", and its bounds are above the Send button's). `a_refused_send_is_explained` (queue `api_error(409, "invalid_state", "..")`: the error line shows and Send is enabled again).
- [ ] **Step 2: Run them.** `cargo test -p smithagents-app --test intent_actions`. Expected: FAIL.
- [ ] **Step 3: Implement** `actions.rs` and return it from `detail_actions`.
- [ ] **Step 4: Run the tests.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(app): send an intent, open its chat and mark it resolved`

---

### Task 11: Settings → Agent defaults

**Files:**
- Create: `crates/app/src/views/settings/agent_defaults.rs`, `crates/app/tests/agent_defaults.rs`
- Modify: `crates/app/src/view_models.rs:517-560` (`SettingsSection`), `crates/app/src/views/settings/mod.rs:1-27,76-104,304-317`, `crates/app/tests/{settings_pane.rs,app_state.rs,view_models.rs}` where they list the sections

**Interfaces:**
- Consumes: Task 3 (`ShellModel::agent_defaults`, `IntentCommands::set_agent_defaults`, `ShellState::intent_commands`), Task 4 (`QUEUE_LIMIT_CHOICES`, `queue_limit_label`, `intent_error_text`).
- Produces:

```rust
pub enum SettingsSection { Appearance, AgentDefaults, Repositories }   // ALL in this order: [SettingsSection; 3]
// title "Agent defaults", as_str "agent_defaults"
pub const QUEUE_LIMIT_LABEL: &str = "Agents working at once";
pub const QUEUE_LIMIT_ID: &str = "queue-limit";             // dropdown button; its label is queue_limit_label(current)
pub fn queue_limit_item_id(limit: Option<u32>) -> ElementId;   // "queue-limit-none" | "queue-limit-<n>"
pub const QUEUE_LIMIT_HELP: &str = "Every agent that is replying counts. Agents you start yourself never wait.";
pub const QUEUE_LIMIT_ERROR_ID: &str = "queue-limit-error";  // Role::Status
```

Behavior: the section sits between Appearance and Source repositories (Repositories stays last, which `focus_section`'s scrolling relies on). It shows the caption `QUEUE_LIMIT_LABEL`, a small outline dropdown button whose menu lists "No limit" then "1" to "16" with the current value checked, and `QUEUE_LIMIT_HELP` in muted text. Choosing a value runs `set_agent_defaults(AgentDefaults { queue_limit })` (`None` for "No limit") with the same timeout rule as other commands; the button shows loading and is disabled until the answer; its label changes when `AgentDefaultsLoaded` reaches the model. A failure shows under it in `QUEUE_LIMIT_ERROR_ID` (`intent_error_text`, or `DAEMON_SILENT_TEXT`). While the model has no defaults the button is disabled and the help line is replaced by `NOT_CONNECTED_TEXT`. A stored limit above 16 is shown as its number with no entry checked.

- [ ] **Step 1: Write the failing tests.** `tests/view_models.rs`: `SettingsSection::ALL` is Appearance, AgentDefaults, Repositories; `parse("agent_defaults")` round-trips; titles. `tests/agent_defaults.rs`: `the_section_shows_the_current_limit` (defaults `None` → the button reads "No limit"; `Some(3)` → "3"; the caption and the help text are found). `the_menu_offers_no_limit_and_one_to_sixteen` (17 entries; the current one checked). `choosing_a_number_puts_it` (choose "4": one `IntentCall::SetDefaults(AgentDefaults { queue_limit: Some(4) })`; the button still reads "No limit" and is disabled until `AgentDefaultsLoaded(Some(4))` is dispatched; then "4"). `choosing_no_limit_puts_null`. `a_failure_is_shown_under_the_control` (queue `api_error(400, "invalid", "queue limit must be at most 64")`: the line reads "Queue limit must be at most 64" and the button is enabled). `nothing_can_be_chosen_before_the_defaults_load` (disabled; "Not connected to the daemon"). `a_larger_stored_limit_is_shown_as_is` (`Some(40)` → "40", no entry checked). `the_section_collapses_and_is_remembered` (as the other sections: `collapsed_settings_sections` holds "agent_defaults").
- [ ] **Step 2: Run them.** `cargo test -p smithagents-app --test agent_defaults --test view_models --test settings_pane`. Expected: compile errors, then failures.
- [ ] **Step 3: Implement** the section, and update the module doc of `settings/mod.rs` (its last paragraph says Agent defaults wait for Plan 2).
- [ ] **Step 4: Run the tests.** Expected: PASS, including the existing `settings_pane.rs` tests that scroll to Source repositories. Gate.
- [ ] **Step 5: Commit.** `feat(app): Settings has Agent defaults with the limit on agents working at once`

---

### Task 12: End-to-end check against a real daemon; roadmap and spec

**Files:**
- Create: `crates/app/tests/intent_link.rs`
- Modify: `docs/superpowers/plans/2026-11-24-v1-roadmap.md` (row 3b), `docs/superpowers/specs/2026-11-23-smithagents-architecture-design.md` (§4.4 Intents: the kinds), `docs/superpowers/acceptance/intents-and-execution.md` is not touched

**Interfaces:**
- Consumes: everything above; `tests/common::{daemon_bin, fake_claude, TestHome, init_repo}`; the `fake-claude` controls `FAKE_CLAUDE_STRUCTURED_FILE`, `FAKE_CLAUDE_CONTROL_DIR` and `FAKE_CLAUDE_FIXTURE`, used as in `crates/client/tests/intents.rs:51-98`.

The test drives the real `smithagentsd` (started by `DaemonLink` with `--claude-bin fake-claude`) through the link, applies every received `Action` to a `ShellModel`, and asserts what the pane would show through the Task 4 functions. No window is opened and no real CLI runs.

- [ ] **Step 1: Write the test** `create_review_send_through_the_link` (`#[tokio::test]`, helpers `within`, `until` as in `tests/link.rs`):
  1. Write `pass1.json` (title "Add dark mode", kind `enhancement`, current behavior `"## Today\nOnly a light theme exists."`, one question "Follow the system theme?" with options Yes and No) and `pass2.json` (one precondition, two acceptance criteria, one story, no dependencies); point the control directory's `structured` file at pass 1; set `FAKE_CLAUDE_FIXTURE` to `crates/server/tests/fixtures/claude_stream_hello.jsonl`. Create a git repository with `init_repo`.
  2. Start the link; wait for `IntentsLoaded(vec![])`, `AgentDefaultsLoaded`, `Connected`. Add the repository with `link.add_repo`.
  3. `link.create_intent(CreateIntentRequest { repo_id: Some(repo), kind: Some(IntentKind::Refactor), seed: SeedInput::Text("dark mode please".into()) })`. Apply actions until `intent_state_label` is "Questions". Assert: `kind_label(content.kind) == "Refactor"`, the title is "Add dark mode", `body_preview` gives heading "Today" and line "Only a light theme exists.", `section_vm(Questions)` reads "0 of 1 answered" and "1 to answer".
  4. Point `structured` at pass 2. `link.answer_question(.., AnswerInput::Option(first option id))`. Apply until the label is "Review". Assert `answer_text` of the question is "→ Yes"; Acceptance criteria reads "0 of 2 accepted", "2 to review".
  5. `review_item` with `Accept` for the precondition and the first criterion, `Edit("Dark colors apply in every pane")` for the second, `Drop` for the story. Apply until the label is "Complete". Assert: criteria "2 of 2 accepted", "✓ done"; stories "0 of 0 accepted", "✓ done", `skipped == 1`; the edited text is in the model.
  6. `link.send_intent(id)`. Apply until the label is "Sent". Assert: `agent_id` is set and that agent is in the model with `intent_id == Some(id)`; `intent_cards(.., IntentFilterChoice::Sent, now)` has one card whose `accent_hue` is the agent's hue and whose meta line contains ` | Sent | `.
  7. `link.resolve_intent(id)`. Apply until the label is "Resolved".
  8. Drop the link; `TestHome`'s drop stops the daemon.
- [ ] **Step 2: Run it.** `cargo test -p smithagents-app --test intent_link`. Expected: PASS (every piece exists by now; a failure here is a defect in an earlier task: fix it there). Then `pgrep -fl smithagentsd`: empty.
- [ ] **Step 3: Update the docs.** Roadmap row 3b: Delivers gains "create screen with five kinds", Status becomes `Built: 2026-11-26-intent-tool.md`. Spec §4.4 Intents bullet: replace "a kind (e.g. Enhancement)" with "a kind (New feature, Enhancement, Bug, Refactor or Maintenance; Task for intents that were not given one)".
- [ ] **Step 4: Run the gate** for the whole workspace.
- [ ] **Step 5: Commit.** `test(app): intent flow against the real daemon; docs: Plan 3b is built`

---

## Follow-ups (not in this plan)

From the decisions:
- The paperclip and attachments on the create screen.
- "Send to intent" from a chat (the daemon's chat seed exists; the web chat has no button yet).
- Version history (`GET /v1/intents/{id}/versions` is unused by the app).
- Deleting or archiving intents (the daemon has no route).
- Editing the body (`PATCH current_behavior` exists; the pane shows it read-only).
- Drag-to-reorder.

Left out because the daemon or the app's model lacks the data:
- The agent's last message on a card: `Agent` carries none and the model keeps no chat content. It needs a field on `Agent` or a per-agent summary event.
- The old app's "—" column on the meta line: its meaning is unknown, so it is not drawn.
- Polish progress beyond "pass n of 2": the daemon streams none.
- The live reason a Queued intent waits: the app derives it from dependencies in its own list. The daemon does not say whether the limit or a dependency holds an intent, so a dependency that is satisfied while the limit is full reads "a free agent slot" only by elimination.
- A daemon-side default repository: the app chooses the repository itself and always sends `repo_id`.

Found while planning:
- A repository picker inside the detail (the daemon accepts `PATCH repo_id`).
- Stories in the old app have a disclosure triangle; the daemon's story is one text, so cards have nothing to disclose.
- A stored queue limit above 16 can only be set through the API; the daemon's maximum is 64.
- The filter and the open sections are not remembered across restarts.
- Keyboard shortcuts for question options (the spec mentions them for option chips).

Found during execution:
- The app does not show agents either when the intent list fails to load at connect: the snapshot, the intents and the agent defaults are one load round, and the round fails as a whole.
- The agent's last message is not on cards, because neither the daemon nor the app holds it (see above).
- The ⌥-key shortcuts on the create screen are untested with non-US keyboard layouts and input methods.

## Self-review notes

- Spec coverage: rail entry and right pane (Task 1); list with state filter and "+" (Task 5); create (Task 6); editor with header, body and polish states (Task 7); question cards with Skip and "n of m answered" (Task 8); accept, edit, skip with "n of m accepted" and Depends on (Task 9); Send, Queued, Sent with the agent's activity and color, Resolved by button (Tasks 4, 5, 10); Agent defaults (Task 11); kinds (Task 2). Option-chip keyboard shortcuts from §4.4 are a Follow-up.
- Every Review Focus line names its tests: 1 → Task 3; 2 → Task 5; 3 → Tasks 4, 8, 9; 4 → Tasks 4, 7; 5 → Task 6.
- Names used across tasks are defined once: `Busy`, `Screen`, `run`, `IntentAnswer`, `IntentCall`, `SectionKind`, `SectionVm`, `intent_state_label`, `detail_sections`, `detail_actions`.

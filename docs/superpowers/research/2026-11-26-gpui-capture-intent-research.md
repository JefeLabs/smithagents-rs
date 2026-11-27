# Feedback tool (step 1) and intent tool: verified research

Date: 2026-11-26. Repo: `smithagents-rs` at `179f4ad` (code as of `edb7a73`; read-only; `Cargo.lock` sha1 `95abda24…` unchanged after all probes).

Pinned stack (from `Cargo.lock`): `gpui-kit 0.11.0` → GPUI is **`gpui-pre 0.3.14`** (not `gpui-fast`; the `gpui-fast` feature is off, `gpui-kit-0.11.0/src/lib.rs:100-103`), `gpui-base 0.11.0`, `gpui-component 0.11.0`, `gpui-kit-assets 0.11.0`, `image 0.25.10`, `objc2 0.6.5`, `objc2-core-graphics 0.3.2`, `windows 0.62.2`. Sources read under `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/` (abbreviated `REG/` below).

Probes (all in the scratch dir; a copy of the workspace with two extra members, its own lock and target dir):

- Workspace copy: `<scratch>/feedback-research/ws/`
- `ws/crates/capture-probe/` — xcap listing/capture/monitor-crop, permission preflight, PNG/thumbnail/boxes/digits. `cargo check` OK; its unit test (pure image work, no capture) passes.
- `ws/crates/gpui-probe/` — image, annotator (drag boxes, badges, anchored comment field), hold button with ring, file picker, overlay, extra icons, intent widgets (markdown, accordion, tag, dropdown, popover, alert), two-panel right dock. `cargo check` OK with zero warnings; `tests/probe.rs` has 3 headless tests (test platform, no screen, no capture), **all pass**.
- Lock diff of the copy vs the repo: `feedback-research/a.txt` (repo) / `b.txt` (copy).

---

## A. The app as built

### A1. Rail, right dock and Settings wiring; what a second right-dock entry touches

How it is wired today:

- `RailEntry` (`crates/app/src/views/rail.rs:24-59`): `enum RailEntry { Settings }`, `ALL: &[RailEntry]` (top to bottom), `label()`, `element_id()` (`"rail-settings"`), `icon()` (`IconName::Settings`), `placement()` (always `DockPlacement::Right`). `rail(active: Option<RailEntry>, on_select: impl Fn(RailEntry,&mut Window,&mut App)+Clone+'static, native: Option<&NativeTheme>, cx:&App) -> impl IntoElement` (`rail.rs:63-95`) draws one `Button::new(entry.element_id()).icon(..).ghost().small().selected(active==Some(entry)).tooltip(..).accessibility_label(..).on_click(..)` per entry. It is a free function, so it holds no state (a hold timer has to live in `Shell` or a new entity).
- Shell (`crates/app/src/shell.rs`):
  - constants: `KEY_CONTEXT = "SmithAgentsShell"` (:92), `SETTINGS_PANEL = "Settings"` (:94, "Never change it once shipped").
  - actions (:69-89) `ToggleAgentsPane, ToggleSettingsPane, CloseActiveTab, NextTab, PreviousTab, Quit`; `init` binds `cmd-alt-b` → `ToggleSettingsPane` in `KEY_CONTEXT` (:117-129) and calls `views::settings::init`.
  - `app_menus()` (:135-147): View menu = "Toggle Agents Pane", "Toggle Settings".
  - `Shell::new` (:264-475): creates `SettingsPane` (:349), builds the right dock as `DockLayout::tabs().panel_view(panel_handle(settings), cx)` (:351-356), `set_dock_size`, `set_locked(true)`; a saved closed right dock is toggled closed (:360-362). Subscriptions: `DockEvent::LayoutChanged` → save + notify "rail's selection follows the right dock" (:378-385); `ShellEvent::RevealSettings` → `if self.rail_active(cx) != Some(RailEntry::Settings) { select_rail_entry(Settings) }` (:399-405); `on_focus_lost` gives focus back to the shell (:439-442).
  - `rail_active(&self, cx) -> Option<RailEntry>` (:492-497) = **first entry whose `placement()` dock is open**. With two entries both on `Right` this always answers the first one. Must change.
  - `select_rail_entry(&mut self, entry, window, cx)` (:501-512): toggles the entry's dock (same entry on screen → close; else open). With two entries in one dock it must instead switch panels when the dock is open on the other entry.
  - `on_toggle_settings` (:649-657); render registers `.on_action(cx.listener(Self::on_toggle_settings))` (:785) and calls `rail(self.rail_active(cx), on_select, …)` (:779).
- `ShellState` (`crates/app/src/shell_state.rs`): `ShellEvent { LeftPaneChanged, RevealSettings, FocusSettings(SettingsSection), ThemeChanged, TabOpenFailed(String) }` (:92-113); `request_settings_focus` emits `RevealSettings` + `FocusSettings` (:350-355). Settings section collapse state lives in `AppState.collapsed_sections` (`app_state.rs:22-34`).
- `SettingsPane` (`crates/app/src/views/settings/mod.rs`): struct :76-104; implements `BasePanel` (`panel_name() -> SETTINGS_PANEL`, `closable=false`, `zoomable=false`, :252-265), `Panel` (`tab_name`, `title`, `zoom_control=None`, `inner_padding=false`, **`title_bar=false`** — the pane draws its own heading, :267-288), `EventEmitter<PanelEvent>`, `Focusable`, `Render` (root `v_flex().id(SETTINGS_PANEL).test_support().track_focus(..).size_full().bg(paint.surface)`, :298-353). Colors come from a `Paint` struct built from `region_paint(native, Region::RightPane)` and `token_paint` (:357-391).
- Layout persistence (`crates/app/src/layout_store.rs`): `LAYOUT_VERSION = 1` (:35); `LayoutStore::{load, save, load_window, save_window}` (:95-156) write `<home>/app-layout.json` and `<home>/app-window.json`; only the right dock's **size and open state** are read back (`saved_dock`, :420-433). The panel tree is never restored ("The panel tree is fixed", :6-10). `with_right_preferred` clones the dumped right dock (:360-370), `with_left_pane` writes the left pane as a fake `left_dock` (:337-355).

What the dock can do (verified by probe `right_dock::two_panels_one_visible_no_tab_bar_and_select_panel_switches`, passes):

- Two panels in the right dock's tab group: `DockLayout::tabs().panel_view(panel_handle(settings), cx).panel_view(panel_handle(feedback), cx)` (`REG/gpui-base-0.11.0/src/dock/layout/builder.rs:51-123`, `.active_index(ix)` exists).
- A tab group draws a **tab bar whenever more than one panel is `visible()`** (`REG/gpui-component-0.11.0/src/dock/tab_panel.rs:766-793`); with exactly one visible panel whose `title_bar()` is false it draws nothing. `BasePanel::visible(&self, cx) -> bool` (`REG/gpui-base-0.11.0/src/dock/panel.rs:25-30`) hides a panel but keeps its tab. Probe result: one visible → 0 `Role::Tab` elements and only that panel rendered; control with both visible → 2 tabs.
- Switching: `DockArea::select_panel(&mut self, panel: PanelId, window, cx)` (`REG/gpui-base-0.11.0/src/dock/dock_area.rs:563-582`); `PanelId::from(entity.entity_id())` (`layout/node.rs:33`). Changing the shared "which panel is shown" state alone does not re-render the dock: call `select_panel` (which commits) or `dock.update(cx, |_, cx| cx.notify())`.
- Reading the active panel: `dock.layout(DockPlacement::Right)` → `PaneTree::root().kind()` → `PaneRef::Tabs { panels, active_ix }` (`dock_area.rs:189-194`, `layout/node.rs:57-100`).
- Persisted shape after the change (probe output): right dock children `["Settings", "Feedback"]`, `info: Tabs { active_index: 1 }`. Nothing in the app reads that back today; remembering the last right entry needs either reading this info (like `saved_left_pane` reads the Agents info) or a field in `app-state.json`.

Checklist for a second right-dock rail entry with its own pane:

1. `RailEntry::Feedback` (+ `ALL` order: Settings, then Feedback), `label`, `element_id` (`"rail-feedback"`), `icon`, `placement` (`rail.rs:24-59`).
2. Hold support: `rail()` (`rail.rs:63-95`) needs per-entry mouse down/up/hover callbacks and progress, or the Feedback button rendered from an entity.
3. `FeedbackPane` implementing `BasePanel` (+ `visible()` driven by shared state), `Panel` (`title_bar=false`, `inner_padding=false`), `EventEmitter<PanelEvent>`, `Focusable`, `Render`; a new `const FEEDBACK_PANEL: &str = "Feedback"` next to `SETTINGS_PANEL` (`shell.rs:94`).
4. `Shell::new` dock construction (`shell.rs:349-363`), `rail_active` (:492-497), `select_rail_entry` (:501-512), `RevealSettings` handler (:399-405, compares with `rail_active`).
5. Keyboard/menu: a new action (e.g. `ToggleFeedbackPane`), its binding in `init` (:117-125), menu item in `app_menus` (:141-144), `.on_action` in `render` (:784-792). Tests that pin these exactly: `tests/menu_and_keys.rs:115-157` (exact menu list) and `:160-180` (key per action).
6. State: a `ShellEvent::RevealFeedback`-style event if other views open the pane; draft state probably its own entity/store, injected through `ShellOptions` (`shell.rs:173-225`) like `layout`/`app_state`/`settings`.
7. Tests touching the rail/right dock that must keep passing: `shell_ui.rs:198-223` (rail click toggles dock, `rail_active`), `shell_ui.rs:1044-1049` (rail button position), `shell_ui.rs:1188-1192`, `agents_pane.rs:610`, `settings_pane.rs:954`, and the layout fixtures with a lone "Settings" panel (`shell_ui.rs:420-430, 695-705, 741-751, 958-968, 1060-1072`; size/open only, so they stay valid).
8. Icons: see D3 — the feedback/lightbulb icons are **not** in the bundled 101 icons, so `run()`'s asset source (`lib.rs:65`) must change.

### A2. How views are tested

- Harness: `#[gpui_kit::test] fn name(cx: &mut TestAppContext)`; test files import Kit names one by one (a glob import would shadow `#[test]`, `shell_ui.rs:4-5`). `tests/common/ui.rs` opens the real `Shell` headless: `open_shell(cx, model)` (:71-83) and `open_shell_options(cx, ShellOptions, (w,h))` (:144-169), which calls `gpui_kit::init`, `smithagents_app::shell::init`, `gpui_kit::open_window(..)` and returns `(WindowHandle<Root>, Entity<Shell>)`. Fakes are injected through `ShellOptions` (`commands`, `system_appearance`, `connection_info`, `link_failure`, stores pointing at a `tempfile` home).
- `gpui_kit::test` (`REG/gpui-kit-0.11.0/src/test.rs`): `TestWindowExt` on `Window` (:33-75): `find`, `try_find`, `find_all`, `within(id)`, `render_frame`, `click`, `click_at`, `click_with_options`, `right_click`, `double_click`, `hover`, `scroll`, `drag(from,to)` (8 intermediate moves, :393-411), `drag_to`, `press("cmd-a")`, `input("text")`. `TestEventExt` (`test/events.rs:12-30`): `pointer_move(pos, pressed_button)`, `pointer_down`, `pointer_up`, `change_modifiers`, `key_down`, `key_up`. `TestQueryExt` (`test/query.rs:7-18`): `find_by_label`, `find_all_by_role(Role)`. `TestAppContextExt::wait_for` (:572-617).
- `ElementSnapshot` accessors (`REG/gpui-base-0.11.0/src/test_support.rs:76-125`): `role, path, checked, selected, expanded, value, bounds, visible, focused, disabled, label`.
- Element ids: an element is findable only if it has `.id(X)` **and** `.test_support()` (`TestSupportExt`, `REG/gpui-base-0.11.0/src/observe.rs:20-38`; call it before `track_focus`). The probe's first failure proved it ("missing ElementId … Registered paths: <none>"). gpui-component `Button`s are observable by their id without extra calls (probe `find(HOLD_ID)` works).
- Time and async: `cx.executor().advance_clock(d)` then `cx.run_until_parked()` (settings_pane.rs:793-813). The native picker: `cx.did_prompt_for_paths()` and `cx.simulate_path_prompt_response(|options| Some(vec![..]))` (settings_pane.rs:849-878; `REG/gpui-pre-0.3.14/src/app/test_context.rs:419-430`).
- No pixel snapshots ("does not inspect rendered pixels", gpui-kit `test.rs:9`); colors are checked through the last frame's quads: `quad_at` / `painted_at` (`tests/common/ui.rs:171-194`, `window.painted_quads()`).
- How a click is simulated: `window.click(id, cx)` = render, `MouseMoveEvent` to the element's center, then `MouseDownEvent` and `MouseUpEvent` through `window.dispatch_event`, rendering after each (`test.rs:236-256`, `174-233`).
- Representative test to imitate (`tests/settings_pane.rs:849-878`, `browse_fills_the_field_with_the_chosen_folder`); helpers `Setup::open`, `frame`, `with_window`, `click`, `press` are at `settings_pane.rs:118-210`, `563-600`:

```rust
#[gpui_kit::test]
fn browse_fills_the_field_with_the_chosen_folder(cx: &mut TestAppContext) {
    let setup = Setup::new();
    let (window, _, _) = setup.open(cx);
    click(cx, window, BROWSE_REPO_ID);           // with_window(.. window.click(id, cx)); frame(cx, window)
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(options.directories && !options.files && !options.multiple);
        Some(vec![PathBuf::from("/Users/me/My Projects/ünïcode app")])
    });
    frame(cx, window);                            // cx.run_until_parked(); window.render_frame(cx)
    assert_eq!(path_value(cx, window), "/Users/me/My Projects/ünïcode app");
    ...
}
```

### A3. Per-user data directory and the atomic write helper

- Home: `smithagents_core::daemon::home_from_env() -> PathBuf` = `SMITHAGENTS_HOME` when set and non-empty, else `~/.smithagents` (`crates/core/src/daemon.rs:32-46`). `host-desktop` makes it absolute and passes it as `LinkConfig.home` (`crates/host-desktop/src/main.rs:16-22`; `LinkConfig.home` at `crates/app/src/link.rs:31-32`). The app keeps its own files in the **same home as the daemon** (`lib.rs:44-46`); stores are built from it in `open_main_window` (`lib.rs:97-99`): `LayoutStore::new(&home)` → `app-layout.json`/`app-window.json`, `AppStateStore::new(&home)` → `app-state.json`, `AppSettingsStore::new(&home)` → `app-settings.json`. There is no other "app data dir" function; the spec's `<app data>/feedback/draft/` maps to `<home>/feedback/draft/`. The `Shell` does not keep the home path itself; a draft store would come in through a new `ShellOptions` field (tests pass `None` or a tempdir).
- Atomic write (`crates/app/src/atomic_file.rs`, module is private `mod atomic_file;` in `lib.rs:11`):

```rust
pub(crate) fn write_private(dir: &Path, file_name: &str, bytes: &[u8]) -> io::Result<()>
```
  Creates a missing `dir` recursively with mode 0700 (an existing dir is left alone), writes a fresh temp file `.{file_name}.{pid}.{nanos:x}.{n}.tmp` with `create_new` and mode 0600, `sync_all`, then `rename` over `dir/file_name`; removes the temp on failure (:11-65). It does not fsync the directory. It is **unix-only** (`std::os::unix::fs::{DirBuilderExt, OpenOptionsExt}`, :6), consistent with "v1 is macOS only" (`shell.rs:113-115`). Stores read leniently (missing/corrupt → default, `app_state.rs:57-117`).

### A4. Theme tokens, regions, and the literal-color rule

- Tokens in both `design/tokens/themes/smith-dark.json` and `smith-light.json`: `background, foreground, card, card-foreground, card-2, card-2-foreground, center-pane, center-pane-foreground, popover, popover-foreground, primary, primary-foreground, secondary, secondary-foreground, muted, muted-foreground, accent, accent-foreground, destructive, border, input, ring, selection, status-ok, status-warn, status-error, status-info` (OKLCH `{l,c,h}`; converted to `Srgb {r,g,b: u8}` by `ThemeSet::native`, `crates/theme/src/native.rs:8-67`, `color.rs:15-19`).
- Regions (`crates/app/src/theme_adapter.rs:74-99`): `Center` → `center-pane`; `LeftPane | RightPane` → `card`; `TitleBar | Rail | TabStrip | BottomBar` → `card-2` (each with its `-foreground`). Helpers: `region_paint(native, Region, cx) -> (Hsla, Hsla)` (:125), `token_paint(native, "token", cx) -> Hsla` (:268-272), `srgb_to_hsla` (:276-285, "the one place an Srgb becomes an Hsla"). gpui-component's theme keys are filled from tokens by `COLOR_KEY_MAP` (:32-70), e.g. `primary.background←primary`, `danger.background←status-error`, `popover.*←popover*`, `list.active.background←selection`; so `cx.theme().primary`, `.danger`, `.border`, `.popover` follow the tokens. Danger for errors: the settings pane uses `token_paint(.., "destructive")` (`settings/mod.rs:382`); status dots use `status-*` (`view_models.rs:310-323`).
- The rule: `tools/themegen/src/lint.rs` flags, in `.rs/.css/.scss/.js/.mjs/.cjs/.ts/.tsx/.jsx/.html/.svg` files outside `target`, `node_modules`, any `tests` or `fixtures` dir, `design/tokens`, `design/generated` (:22-27): **hex literals** `#rgb/#rgba/#rrggbb/#rrggbbaa` (:29-31) and **color function calls whose first argument is a number**: `rgb|rgba|hsl|hsla|hwb|lab|lch|oklab|oklch|color(` followed by digit/sign/dot (:32-34). Comment-only lines and lines containing `theme-lint: allow` are skipped (:47-51). It is case-sensitive, so `image::Rgba([..])` and `Hsla { .. }` struct literals are not caught, but `gpui::rgb(0x…)`/`hsla(0.5, …)` are. Enforced by the test `repo_has_no_literal_colors` (`tools/themegen/tests/repo.rs:8-11`, run by `cargo test -p themegen`; there is no CI config in the repo). Convention: "nothing here writes a literal color" (`views/mod.rs:1-3`). So box/badge colors in the annotated PNG should be passed in from a token (`native.colors["status-error"]` etc.), not written in code.

### A5. Window creation and bounds/scale from app code

- `smithagents_app::run` (`crates/app/src/lib.rs:59-85`): `gpui_kit::application().with_assets(gpui_kit::assets::Assets).with_quit_mode(QuitMode::LastWindowClosed)`. `open_main_window` (:87-143) restores a saved window when its display is attached and the title strip is visible (`restored_window_bounds`, `layout_store.rs:225-256`), else `WindowBounds::Windowed(Bounds::centered(None, size(px(1200.), px(760.)), cx))` (:109-115). `main_window_options(window_bounds, display_id)` (:150-165): `window_min_size: Some(MIN_WINDOW_SIZE)` = 640×400 (`layout_store.rs:83-86`), `titlebar: TitlebarOptions { title: Some("SmithAgents"), ..TitleBar::title_bar_options() }` (app-drawn title bar), `app_owns_titlebar_drag: true`. `gpui_kit::open_window` wraps the view in `gpui_base::Root` (`REG/gpui-kit-0.11.0/src/lib.rs:181-196`). `WINDOW_TITLE = "SmithAgents"` (`lib.rs:40`).
- From app code (GPUI `Window`, `REG/gpui-pre-0.3.14/src/window.rs`): `window.bounds() -> Bounds<Pixels>` (:3243, the frame incl. title bar, in points), `window.window_bounds() -> WindowBounds` (:2885), `window.viewport_size() -> Size<Pixels>` (:3320, content), `window.scale_factor() -> f32` (:3494), `window.display(cx) -> Option<Rc<dyn PlatformDisplay>>` (:3479; `PlatformDisplay::{id, uuid, bounds, visible_bounds}`, `REG/gpui-pre-0.3.14/src/platform.rs:675-705`), `window.mouse_position()` (:3775), `cx.observe_window_bounds(window, ..)` (used at `shell.rs:426`). `Window: HasWindowHandle` (:8112-8116).
- macOS specifics that matter for capture: `bounds()` is computed as x = frame.x − screen.x and y flipped against the window's screen (`REG/gpui-pre-macos-0.3.14/src/window.rs:984-1005`), so it equals CoreGraphics global coordinates **only on the primary display**; `PlatformDisplay::bounds()` on macOS always has origin (0,0) (`REG/gpui-pre-macos-0.3.14/src/display.rs:114-125`), so GPUI cannot give a display's global origin. The raw handle is `AppKitWindowHandle(ns_view)` only (`gpui-pre-macos window.rs:2586-2593`); no window number.

---

## B. GPUI / gpui-component at the pinned versions

### B1. Showing an in-memory image

Two sources, both accepted by `img(source: impl Into<ImageSource>)` (`REG/gpui-pre-0.3.14/src/elements/img.rs:196-204`; `ImageSource` :38-47, `From<Arc<RenderImage>>` :103, `From<Arc<Image>>` :109, `From<PathBuf>` :97):

1. `Arc<RenderImage>` (decoded pixels, no asset cache): `RenderImage::new(data: impl Into<SmallVec<[image::Frame; 1]>>)` (`REG/gpui-pre-0.3.14/src/assets.rs:58-68`). Frames must be **BGRA** ("A cached and processed image, in BGRA format", :42). `RenderImage::size(frame) -> Size<DevicePixels>` (:76-85). Needs a direct `image = "0.25"` dependency (GPUI does not re-export `image`; unifies with the locked 0.25.10). Compiled and exercised in the probe:

```rust
pub fn render_image_from_rgba(mut rgba: image::RgbaImage) -> Arc<RenderImage> {
    for pixel in rgba.chunks_exact_mut(4) { pixel.swap(0, 2); }      // RGBA -> BGRA
    Arc::new(RenderImage::new(vec![image::Frame::new(rgba)]))
}
```
2. `Arc<Image>` (encoded bytes): `Image::from_bytes(ImageFormat::Png, bytes)` (`platform.rs:3534-3541`; its id is a hash of all bytes). Decoded asynchronously through `window.use_asset::<ImageDecoder>` (`img.rs:525-536`, `576-590`): the first frame has no data (the `with_loading` replacement shows after `LOADING_DELAY`); in tests run `cx.run_until_parked()`.

Lifetime/cache concerns (large images):
- `paint_image` uploads each `RenderImage` into the window's sprite atlas keyed by its id, and it stays there until **`cx.drop_image(image, Some(window))`** (`REG/gpui-pre-0.3.14/src/app.rs:3324-3334`) or `window.drop_image(image)` (`window.rs:5669-5680`). Replace/discard of a screenshot must call it. An `Arc<Image>` additionally stays in the App asset cache until `image.remove_asset(cx)` (`platform.rs:3562-3565`).
- Atlas textures are clamped to 1024..=16384 px per side (`REG/gpui-pre-apple-0.3.14/src/metal_atlas.rs:139-150`), so 3000×2000 (or a 5K Retina window, 5120×2880) fits; it costs ~24 MB CPU (the `RenderImage`) + ~24 MB GPU. For pane thumbnails use small images (downscale with `image::imageops::resize`, or write a thumbnail PNG and `img(path)`); `img(PathBuf)` decodes the full file.
- `RenderImage.scale_factor` is `pub(crate)` and 1.0 (`assets.rs:47`), so the intrinsic size is the pixel size; give the element an explicit size.
- Fit with aspect ratio: `img(..).size_full().object_fit(ObjectFit::Contain)` (`StyledImage`, img.rs:143-176; `Contain` is the default, :132-140). The drawn rectangle is public math: `ObjectFit::Contain.get_bounds(bounds: Bounds<Pixels>, image_size: Size<DevicePixels>) -> Bounds<Pixels>` (`REG/gpui-pre-0.3.14/src/style.rs:42-75`); the annotator uses it to map the pointer to image fractions (probe test proves a 1600×600 image letterboxed into 800×600 maps (200,225)–(400,300) to x=0.25, y=0.25, w=0.25, h=0.25).
- Not usable for capture in release: `Window::render_to_image() -> anyhow::Result<image::RgbaImage>` renders GPUI's own scene (`window.rs:3251-3258`) but is `#[cfg(any(test, feature = "test-support"))]`, and it would not include native child views (a future web engine in the center).

### B2. Mouse handling, bounds, drawing

- Listeners (`REG/gpui-pre-0.3.14/src/elements/div.rs`): `on_mouse_down(MouseButton, Fn(&MouseDownEvent,&mut Window,&mut App))` (:893), `on_mouse_move(Fn(&MouseMoveEvent,..))` (:1030), `on_mouse_up(MouseButton, Fn(&MouseUpEvent,..))` (:954), `on_mouse_up_out(MouseButton, ..)` (:1017), `capture_any_mouse_down/up` (:930, :967), `on_hover(Fn(&bool,..))` (:1708), `occlude()` / `block_mouse_except_scroll()` (:1297, :1313). Bubble listeners fire **only while the element's hitbox is hovered** (`Interactivity::on_mouse_down/up/move`, div.rs:126-140, 210-224, 303-313); `on_mouse_up_out` fires in capture phase when not hovered (:283-297). Events carry `position: Point<Pixels>` in window coordinates; `MouseMoveEvent.pressed_button: Option<MouseButton>` detects a missed mouse-up (pattern already in `shell.rs:593-597`). Wrap with `cx.listener(|this, ev, window, cx| ..)` (`REG/gpui-pre-0.3.14/src/app/context.rs:241-249`). For a window-wide listener during paint there is `window.on_mouse_event::<E>(..)` (`window.rs:5948-5962`).
- Element bounds: `canvas(prepaint: FnOnce(Bounds<Pixels>,&mut Window,&mut App)->T, paint: FnOnce(Bounds<Pixels>,T,&mut Window,&mut App))` (`elements/canvas.rs:10-19`), styled with `.absolute().inset_0()`; store the prepaint bounds in an `Rc<Cell<Option<Bounds<Pixels>>>>` and read it in the mouse listeners (events always follow a paint).
- Drawing boxes in `paint`: `window.paint_quad(fill(bounds, color))` and `window.paint_quad(outline(bounds, color, BorderStyle::Solid))` (`window.rs:8372-8395`; `quad(..)` with widths/radii at :8353). Badges: absolutely positioned divs; inside a container sized to the drawn image they can be placed with `relative(fraction)` (`geometry.rs:3813`) → `.absolute().left(relative(x)).top(relative(y))`.
- Compiled + tested shape (gpui-probe `Annotator`, `src/lib.rs`):

```rust
div().id(ANNOTATOR_ID).test_support().key_context("Annotator").track_focus(&self.focus)
    .on_action(cx.listener(Self::drop_comment))                 // Escape, see B4
    .relative().size_full()
    .child(img(self.image.clone()).id(SHOT_ID).size_full().object_fit(ObjectFit::Contain))
    .child(canvas(move |bounds, _, _| { frame.set(Some(bounds)); bounds },
                  move |_, bounds, window, _| {
                      let image = ObjectFit::Contain.get_bounds(bounds, image_size);
                      for r in &rects { let b = /* fractions -> Bounds */;
                          window.paint_quad(fill(b, color.opacity(0.12)));
                          window.paint_quad(outline(b, color, BorderStyle::Solid)); }
                  }).absolute().inset_0())
    .child(/* div at the image rect */ .children(badges /* .left(relative(x)).top(relative(y)) */))
    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
    .on_mouse_move(cx.listener(Self::on_move))                  // ends the drag if pressed_button != Left
    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_up))
```
  Test `dragging_draws_a_box_in_image_fractions_and_enter_saves_its_comment` passes (drag via `window.drag`).

### B3. Press-and-hold, timer, cancel, ring

- Timer: `cx.spawn_in(window, async move |this, cx| { cx.background_executor().timer(HOLD).await; let _ = this.update_in(cx, |this, window, cx| this.complete(window, cx)); })` (`Context::spawn_in`, context.rs:649-656; `BackgroundExecutor::timer(Duration) -> Task<()>`, `REG/gpui-pre-0.3.14/src/executor.rs:200-206`). **Dropping the `Task` cancels it** (repo relies on this, `shell_state.rs:589-600`). Elapsed time: `cx.background_executor().now()` (executor.rs:192-194, returns `web_time::Instant` = `std::time::Instant` on native) — it follows `advance_clock` in tests.
- Button-like element: gpui-component `Button` implements `InteractiveElement` (`REG/gpui-component-0.11.0/src/button/button.rs:633-637`) and does not stop left mouse-down propagation (:887-901), so `Button::new(id).on_mouse_down(Left, ..).on_mouse_up(Left, ..).on_mouse_up_out(Left, ..)` work. Cancel on leaving: `Button::on_hover(Fn(&bool,..))` (inherent, :477-480). `on_click` still fires on the release after a completed hold, so the view must swallow it with a flag.
- Ring: `gpui_kit::component::progress::ProgressCircle::new(id).value(0.0..=100.0)` with children (`REG/gpui-component-0.11.0/src/progress/progress_circle.rs:28-71`, `ParentElement` :136-140); it eases toward each new value over the theme's `duration_normal` = 180 ms (:157-169; `theme/motion.rs:28`), so it trails slightly. Repaint while holding with a ~33 ms timer loop that calls `cx.notify()` (proved) or `window.request_animation_frame()` from render (`window.rs:3096-3099`; no frame loop in tests). `AnimationExt::with_animation(id, Animation::new(d), |el, t| ..)` exists (`elements/animation.rs:82-100`) but under reduce-motion a oneshot renders its end state immediately (:74-80), which would show a full ring — so drive the ring from elapsed time, not from an animation.
- "Hide the ring, capture next frame": `window.on_next_frame(cb)` callbacks run at the **start** of the next frame, before it draws (`window.rs:3076-3083`, `2045-2060`). Hop twice (`on_next_frame(|w, _| w.on_next_frame(capture))`) so the capture follows a frame drawn without the ring.
- Compiled + tested (`Hold` in gpui-probe; test `a_release_before_three_seconds_is_a_click_and_a_full_hold_is_not` passes): 1.5 s → progress 0.5; release before 3 s = cancel + one click; 2.99 s not completed, 3.01 s completed; the release after completion is not a click; moving off the button while pressed cancels.

### B4. Text input

- Single line: `cx.new(|cx| InputState::new(window, cx).placeholder("…"))` (`InputState = InputBaseState<InputMode>`, `REG/gpui-base-0.11.0/src/input/input/mod.rs:10`; `new` `base/state.rs:13578`; `placeholder` :1114) and element `gpui_kit::component::input::Input::new(&state)` (`REG/gpui-component-0.11.0/src/input/input.rs:241`; `.small()`, `.aria_label()`, `.disabled()`, `.cleanable()`, `.id()`). Read: `state.read(cx).value() -> SharedString` (:1683). Write: `state.update(cx, |s, cx| s.set_value("", window, cx))` (:1312). Focus: `state.update(cx, |s, cx| s.focus(window, cx))` (:1766). Events: `cx.subscribe_in(&state, window, |this, _, ev: &InputEvent, window, cx| ..)` with `InputEvent { Change, PressEnter { secondary, shift }, Focus, Blur }` (`base/state.rs:141-146`) — repo example `settings/mod.rs:130-142`.
- Escape: there is no Escape event. The input binds `escape` → action `Escape` in context `"Input"` (`base/state.rs:215`); its handler ends with `cx.propagate()` unless `clean_on_escape` is set (:2600-2640), so a parent handles `.on_action(cx.listener(|this, _: &gpui_kit::component::input::Escape, window, cx| ..))`. Proved by the annotator test (Escape drops the unsaved box).
- Multi-line: `TextareaState::new(window, cx).auto_grow(min, max)` / `.rows(n)` (`base/state.rs:13826-13880`) with `Textarea::new(&state)` (`REG/gpui-component-0.11.0/src/input/textarea.rs:70`); `submit_on_enter(bool)` exists but is `#[doc(hidden)]` (:1530).
- Anchoring: `deferred(anchored().position(point).snap_to_window().child(..))` (`elements/anchored.rs:39-100`, `deferred.rs:7`); compiled in the annotator. Since the annotator fills the window, an absolutely positioned div at the box's computed position also works.
- Tests: `window.input("text", cx)` types into the focused input; `window.press("enter"|"escape", cx)`.

### B5. Native open-file dialog

`cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Add".into()) }) -> oneshot::Receiver<anyhow::Result<Option<Vec<PathBuf>>>>` (`REG/gpui-pre-0.3.14/src/app.rs:2139-2144`; options struct `platform.rs:3025-3034`). **There is no extension filter** in this GPUI: the macOS impl sets only `canChooseDirectories/Files`, `allowsMultipleSelection`, `canCreateDirectories`, `resolvesAliases(false)` and the prompt (`REG/gpui-pre-macos-0.3.14/src/platform.rs:868-918`). `Ok(Ok(None))` = cancelled; `Ok(Err(e))` = the picker failed; `Err(Canceled)` = receiver gone. Filter by extension afterwards (and check content type when reading). Repo pattern: `settings/repositories.rs:187-218`. Compiled: `add_files()` in gpui-probe.

### B6. Full-window overlay

- `gpui_base::Root` draws the app view, then plugin layers absolutely over it (`REG/gpui-base-0.11.0/src/root.rs:275-306`); gpui-component registers its `WindowState` (dialogs, sheets, notifications) as such a plugin (`REG/gpui-component-0.11.0/src/root.rs:728`), which is why `window.push_notification` works in the shell (`views/center.rs:39-54`).
- Simplest overlay covering every pane: render it from `Shell` as the last child of a `.relative()` root, `div().absolute().inset_0().occlude().child(annotator)` (compiled: `with_overlay` in gpui-probe). It covers the title bar, left pane, dock and rail; focus it so Escape/Enter reach it (note `Shell` takes focus back on focus loss, `shell.rs:439-442`).
- Alternatives that exist: `window.open_dialog(cx, |dialog, window, cx| ..)`, `open_alert_dialog`, `open_sheet_at(Placement, ..)` (`REG/gpui-component-0.11.0/src/window_ext.rs:13-118`); dialogs have their own chrome/width, not a natural fit for a full-window annotator.
- Caveat for later: the center chat tabs belong to a `CenterTabHost` (native web views attached to the window); a native view would sit above any GPUI overlay and the host has no "hide all" call (`crates/center-host/src/lib.rs:53-67`). Today's `PlaceholderHost` draws nothing, so step 1 is unaffected.

---

## C. Capture

### C1. xcap 0.9.10

- Newest on crates.io: **0.9.10** (sparse index `index.crates.io/xc/ap/xcap`, last five: 0.9.6–0.9.10, none yanked). License **Apache-2.0** (`REG/xcap-0.9.10/Cargo.toml`); workspace is MIT — compatible.
- Features: `image = ["image/default"]`, `wgc` (Windows Graphics Capture); **no default features**. Always depends on `image 0.25` (`default-features=false, features=["png"]`), `log`, `scopeguard`, `thiserror 2`. macOS: `objc2 0.6.4`, `objc2-app-kit`, `objc2-av-foundation`, `objc2-core-foundation`, `objc2-core-graphics`, `objc2-core-media`, `objc2-core-video`, `objc2-foundation` (all 0.3.2), `dispatch2 0.3.1`. Windows: `windows 0.62` (+ `widestring`). Linux: `ashpd 0.14`, `libwayshot-xcap`, `pipewire 0.10`, `xcb 1.7`, `pollster 1`, `rand 0.10`, `url`, `percent-encoding`, `serde`.
- Public API (`REG/xcap-0.9.10/src/lib.rs`, `window.rs`, `monitor.rs`): `pub use image;` and

```rust
impl Window {
    pub fn all() -> XCapResult<Vec<Window>>;            // "sorted by z coordinate" (macOS: front to back)
    pub fn id(&self) -> XCapResult<u32>;               // macOS: kCGWindowNumber
    pub fn pid(&self) -> XCapResult<u32>;
    pub fn app_name(&self) -> XCapResult<String>;
    pub fn title(&self) -> XCapResult<String>;
    pub fn current_monitor(&self) -> XCapResult<Monitor>;
    pub fn x(&self) -> XCapResult<i32>;  pub fn y(&self) -> XCapResult<i32>;  pub fn z(&self) -> XCapResult<i32>;
    pub fn width(&self) -> XCapResult<u32>;  pub fn height(&self) -> XCapResult<u32>;
    pub fn is_minimized(&self) -> XCapResult<bool>;  pub fn is_maximized(&self) -> XCapResult<bool>;
    pub fn is_focused(&self) -> XCapResult<bool>;
    pub fn capture_image(&self) -> XCapResult<image::RgbaImage>;
}
impl Monitor {
    pub fn all() -> XCapResult<Vec<Monitor>>;  pub fn from_point(x: i32, y: i32) -> XCapResult<Monitor>;
    pub fn id/name/friendly_name/x/y/width/height/rotation/scale_factor/frequency/is_primary/is_builtin(&self);
    pub fn capture_image(&self) -> XCapResult<image::RgbaImage>;
    pub fn capture_region(&self, x: u32, y: u32, width: u32, height: u32) -> XCapResult<image::RgbaImage>;
    pub fn video_recorder(&self) -> XCapResult<(VideoRecorder, Receiver<Frame>)>;
}
pub enum XCapError { NotSupported, Error(String), StdSyncPoisonError(String), InvalidCaptureRegion(String), /* platform variants */ }
pub type XCapResult<T> = Result<T, XCapError>;
```
- macOS implementation facts (`src/macos/impl_window.rs`, `capture.rs`): listing uses `CGWindowListCopyWindowInfo(OnScreenOnly | ExcludeDesktopElements)`; a window is skipped if it has **no `kCGWindowName`**, is the "StatusIndicator", or has `kCGWindowSharingState == 0` (`get_window_id`, :94-116). Every accessor re-lists all windows (`get_window_cf_dictionary`, :118-152). `x/y/width/height` are the CG window bounds in **points** (global display coordinates, top-left of the primary display), despite the "pixel" doc comments. `capture_image` calls `CGWindowListCreateImage(rect, OptionIncludingWindow, id, Default)` and converts BGRA→RGBA into an `image::RgbaImage` (`capture.rs:10-46`) — at the display's backing scale (unverified at runtime, see Unverified). `Monitor::capture_region` takes a region relative to the monitor origin in points and rejects regions outside it (`impl_monitor.rs:211-242`). xcap logs through `log`, which the app does not bridge (`host-desktop/src/main.rs:43-47`).
- `ImplWindow` is `unsafe impl Send` (`impl_window.rs:23-24`), so an `xcap::Window` can move to `cx.background_spawn`.
- Lock impact (probe: adding `xcap = "=0.9.10"` + `image 0.25` + `objc2-core-graphics 0.3.2` to the workspace copy; diff of the copy's lock vs the repo's): **nothing removed or re-versioned**; `image` stays 0.25.10, `objc2` 0.6.5, `objc2-core-graphics/foundation/app-kit/core-video` 0.3.2, `windows` 0.62.2, `dispatch2` 0.3.1. 37 packages added. New on macOS: `xcap 0.9.10`, `objc2-av-foundation 0.3.2`, `objc2-core-media 0.3.2`, `objc2-core-audio 0.3.2`, `objc2-core-audio-types 0.3.2`. The rest are Linux/Windows-only: `ashpd 0.14.3` (second copy; repo has 0.13.13 via gpui), `pipewire 0.10.1`, `pipewire-sys`, `libspa`, `libspa-sys`, `bindgen 0.72.1`, `clang-sys`, `libwayshot-xcap 0.3.3`, `gbm`, `gbm-sys`, `drm` 0.14.1 and 0.15.0, `drm-ffi`, `drm-sys`, `drm-fourcc`, `gl`, `wayland-server 0.31.14`, `system-deps`, `cfg-expr`, `version-compare`, `widestring 1.2.1`, plus second copies of `annotate-snippets 0.11.5`, `nom 7.1.3`, `pollster 1.0.3`, `rustix 0.38.44`, `linux-raw-sys 0.4.15/0.9.4`, `shlex 1.3.0`, `target-lexicon 0.12.16`; and `cexpr`, `minimal-lexical`, `cookie-factory`. A Linux build therefore needs bindgen/libclang and the PipeWire/libspa development packages.
- Compile probe (`ws/crates/capture-probe/src/lib.rs`, `cargo check` OK, no warnings): `xcap::Window::all()`, `pid()`, `is_minimized()`, `width()/height()`, `id()`, `title()`, `x()/y()`, `capture_image()`, `xcap::Monitor::from_point`, `x()/y()`, `scale_factor()`, `capture_region(..)`, `objc2_core_graphics::CGPreflightScreenCaptureAccess()` and `CGRequestScreenCaptureAccess()`. Never run.

### C2. Identifying the app's own window

- Window number from the raw handle: GPUI gives only `AppKitWindowHandle { ns_view }` (A5). Turning that into a `CGWindowID` needs an Objective-C message on a raw pointer (`[[view window] windowNumber]`), i.e. `unsafe`, which the workspace forbids (`[workspace.lints.rust] unsafe_code = "forbid"`, root `Cargo.toml`; `raw-window-handle =0.6.2` is a dependency of the app already, `crates/app/Cargo.toml`). So: not without unsafe.
- What works without unsafe (compiled in `find_own_window`): filter `Window::all()` by `pid() == std::process::id()`, not minimized, and size within ~2 points of GPUI's `window.bounds().size` (both are the frame in points), and take the first (frontmost). The title "SmithAgents" is set on the NSWindow (`lib.rs:158-161`) and can be a tie-breaker. Do not match on origin: GPUI's `bounds().origin` equals CG global only on the primary display (A5).
- Fallback by cropping: GPUI cannot give a display's global origin (A5), so crop with xcap's own numbers: `Monitor::from_point(win.x(), win.y())`, then `capture_region(win.x() - mon.x(), win.y() - mon.y(), win.width(), win.height())` (region in points; output in pixels). If the window is not in the list at all, the crop fallback has no reliable global rectangle from GPUI.
- Capture off the main thread: `cx.background_spawn(async move { capture(..) })`, then decode/encode there too.

### C3. Without Screen Recording permission (macOS)

What the source establishes:
- `Window::all()` calls `CGPreflightScreenCaptureAccess()` and only **logs a warning** when it is false, then continues (`impl_window.rs:161-168`). There is no xcap API to check or request permission.
- The check exists in the OS and is safe to call from Rust without `unsafe`: `objc2_core_graphics::CGPreflightScreenCaptureAccess() -> bool` (no prompt) and `CGRequestScreenCaptureAccess() -> bool` (prompts), both plain `pub extern "C-unwind" fn` in `REG/objc2-core-graphics-0.3.2/src/generated/CGWindow.rs:285-297`, feature `CGWindow`; `objc2-core-graphics 0.3.2` is already in the lock. Compiled in the probe.
- `CGWindowListCreateImage` is marked `#[deprecated = "Please use ScreenCaptureKit instead."]` (`CGWindow.rs:239-247`); Rust links it regardless of the macOS 15 SDK "obsoleted" availability error that C/Swift builds hit (the probe built on this machine).
- Because listing skips windows without `kCGWindowName`, and macOS omits other processes' window names without permission, an unpermitted process should see mostly its own windows in the list (inference, see Unverified).

From issues/forums (not source):
- xcap #123 (closed 2024-05-29): maintainer: "Without screenshot permission, you can only take screenshots of the desktop" — a capture showed only the background; a re-signed/downloaded build needed its permission removed and re-added.
- xcap #138 (open): no way in xcap to check before the prompt; the answer given is `CGPreflightScreenCaptureAccess` / `CGRequestScreenCaptureAccess`.
- xcap #160 (open, 2024-10-21): macOS 15.1 started warning users about apps using the CG capture APIs; ScreenCaptureKit support offered as a bounty; no fix.
- Apple forum thread 849969 (Oct 2026): DTS quotes WWDC 2019 "Apps can freely record the contents of their own windows, the menu bar and the desktop background image"; a developer reports `CGWindowListCreateImage` returning a **gray image** for other windows from **macOS 27 beta 4** (worked through macOS 26).
- Search results also report that these CG calls make macOS add the app to the Screen Recording list / show a prompt on macOS 15.

### C4. `image` and drawing badges without fonts

- `image 0.25.10` is already in the tree through `gpui-pre` (formats bmp, dds, exr, ff, gif, hdr, ico, jpeg, png, pnm, qoi, rayon, tga, tiff, webp: `REG/gpui-pre-0.3.14/Cargo.toml:380-397`); xcap re-exports the same crate (`xcap::image`). Adding `image = { version = "0.25", default-features = false, features = ["png"] }` adds nothing to the lock.
- Encode/decode: `RgbaImage::write_to(&mut Cursor<Vec<u8>>, ImageFormat::Png)`, `image::load_from_memory_with_format(bytes, ImageFormat::Png)?.into_rgba8()`; content sniffing: `image::guess_format(bytes)` for png/jpeg/gif/webp (mp4: check the `ftyp` box at byte 4 yourself).
- Resize: `imageops::thumbnail(&img, w, h)` (fast) or `imageops::resize(&img, w, h, FilterType::Triangle)` (`REG/image-0.25.10/src/imageops/sample.rs:613, 964`).
- Rectangles: no draw API in `image` (that is `imageproc`). Filled rect = `imageops::overlay(&mut img, &RgbaImage::from_pixel(w, h, color), x, y)` (alpha-blends and clips, negative coordinates OK; `imageops/mod.rs:223`); outline = four filled strips.
- Digits: a 3×5 bitmap font for 0–9 (10 × 5 bytes) drawn as scaled filled cells; badge = filled square + centered number. Implemented and **tested** in `capture-probe` (`draw_number`, `draw_badge`, `outline_rect`, `fill_rect`, `thumbnail`, `encode_png`, `decode_png`; test `draw_encode_decode_thumbnail_without_capturing` passes, including clipping at the edges).

---

## D. Intent tool

### D1. Markdown

- Exists: `gpui_kit::component::text::{TextView, TextViewStyle, markdown}`. `TextView::markdown(id: impl Into<ElementId>, text: impl Into<SharedString>) -> TextView` (`REG/gpui-component-0.11.0/src/text/compat.rs:68-77`) and `markdown(source)` keyed by caller location (:519-524). Parser: the `markdown 1.0.0` crate (always on, `gpui-component` and `gpui-base` Cargo.toml; in the lock). `pulldown-cmark` is **not** in the lock.
- Needs only a window: the element keeps a `TextViewState` per id via `window.use_keyed_state("{id}/state")` (`REG/gpui-base-0.11.0/src/text/text_view.rs:850-868`); for explicit control `cx.new(|cx| TextViewState::markdown(text, cx))` + `TextView::new(&state)` and `state.update(cx, |s, cx| s.set_text(..))` (`text/state.rs:192, 352-389`; appends parse incrementally, parsing runs on the background executor, `state.rs:230`, small replacements parse synchronously).
- Options (`compat.rs:94-270`): `.selectable(bool)` (default true; I-beam, drag selection, copy as plain text or source via `.selection_format(SelectionFormat::Source)`), `.scrollable(bool)` (default false = grows to fit content; true = own virtualized scroll, needs a fixed-height parent, `text_view.rs:463-476`), `.max_lines(n)`, `.style(TextViewStyle)`, `.inline_code_style(StyleRefinement)`, `.list_style(..)`, `.blockquote_style(..)`, `.on_link_click(..)`, `.on_task_toggle(..)`.
- Theme: defaults derive from gpui-component's `Theme` (foreground, muted foreground, link, selection, code background = `muted`, border, inline code background = `accent`), installed on every `Theme::update`/`sync_base` (`text/mod.rs:43-80`, `theme/mod.rs:477-496`); the app's `apply_theme_state` uses `Theme::update` (`theme_adapter.rs:397`), so it follows the Smith tokens through `COLOR_KEY_MAP`. With `inherit_text_color` it follows the container's text color (`text_view.rs:893-905`). `TextViewStyle` fields: `paragraph_gap`, `heading_base_font_size`, `heading_font_size`, `highlight_theme`, `code_block`, `table*`, `inline_code`, `is_dark` (`text/style.rs:1-60`).
- Compiled shape (gpui-probe `intent::brief`): a `v_flex().id(..).flex_1().min_h_0().overflow_y_scroll().track_scroll(&scroll).child(TextView::markdown("brief", BRIEF).selectable(true).text_color(cx.theme().foreground))`. Rendering of headings/lists not visually verified.

### D2. Scrolling and long lists

- Repo pattern (both panes): a stored `ScrollHandle` field + `.id(..).overflow_y_scroll().track_scroll(&self.scroll)` on a `flex_1().min_h_0()` column (`views/agents/mod.rs:193, 218, 566-577`; `views/settings/mod.rs:80, 148, 342-351`). The offset lives in the handle, so it survives re-renders; programmatic: `scroll.scroll_to_item(ix)` (`agents/mod.rs:378`), `scroll.scroll_to_top_of_item(ix)` (`settings/mod.rs:186`).
- Virtualized options exist but the repo uses none: GPUI `uniform_list(id, count, |range, window, cx| -> Vec<R>)` (uniform heights, `REG/gpui-pre-0.3.14/src/elements/uniform_list.rs:22`), GPUI `list(ListState, ..)` (variable heights, `elements/list.rs:24`), gpui-component `v_virtual_list(view: Entity<V>, id, item_sizes: Rc<Vec<Size<Pixels>>>, |&mut V, Range<usize>, &mut Window, &mut Context<V>| -> Vec<R>)` (`REG/gpui-base-0.11.0/src/virtual_list.rs:139-152`, re-exported `gpui-component/src/lib.rs:122`), and the delegate-based `component::list` module.

### D3. Collapsible sections, chips, icon buttons, icon names

- `Accordion::new(id).multiple(bool).bordered(bool).item(|item: AccordionItem| item.title(el).icon(..).open(bool).child(..)).on_toggle_click(|open: &[usize], window, cx| ..)` (`REG/gpui-component-0.11.0/src/accordion.rs:32-90, 180-240`; item `ParentElement` :261). Open state is controlled by the caller (`open(bool)`); store it yourself.
- `Collapsible::new().open(bool).child(always).content(shown_when_open).motion_id(id)` (`collapsible.rs:20-50`). The Settings pane's sections are hand-made (header `Button` with chevron + `aria_expanded`, `settings/mod.rs:203-249`) — the existing pattern to copy.
- Chips: `Tag::{new, primary, secondary, danger, success, warning, info, custom(color, fg, border), color(ColorName)}` + `.outline()`, `.rounded_full()`, `Sizable` (`.small()`), `Styled`, `ParentElement` (`tag.rs:124-233`). Monospace: `.font_family(cx.theme().mono_font_family.clone())` (`theme/mod.rs:196`). `Badge` is a count/dot overlay on a child, not a chip (`badge.rs:32-90`). Status dot: a `div().size(px(8.)).rounded_full().bg(..)`; the repo's dot styling is `state_dot(kind) -> DotStyle { token, filled, dimmed }` (`view_models.rs:310-323`).
- Icon-only button: `Button::new(id).icon(Icon::new(name)).ghost().small().tooltip("Filter").accessibility_label("Filter")` (`button.rs:404-420`; "if the Button has no label, the button will be in Icon Button mode").
- Icons: the app registers only `gpui_kit::assets::Assets` (`lib.rs:65`) = the **101 default icons** (`REG/gpui-kit-assets-0.11.0/default-icons.txt`). In it: `Plus`, `ArrowLeft`, `Check`, `ChevronDown/Up/Left/Right`, `ChevronsUpDown`, `Settings`, `Info`, `CircleAlert`, `TriangleAlert`, `Close`, `Inbox`, `Bell`, `FileText`, `Search`, `SortAscending/Descending`… **Not in it**: `Lightbulb`, `Funnel`/`ListFilter` (filter), `Triangle`, `Camera`, `MessageSquareWarning`, `Trash` (there is no `Trash2` in this catalog). These exist in the full catalog `gpui_kit::assets::IconName` and must be bundled with `gpui_kit::assets::icon_assets!(pub ExtraIcons, [Lightbulb, Funnel, ListFilter, MessageSquareWarning, Camera, Trash, X]);` composed with `Assets` in an app `AssetSource` passed to `.with_assets(..)` (`REG/gpui-kit-assets-0.11.0/src/lib.rs:71-91`, `examples/extra_assets.rs`, README). Without that they draw blank. Compiled: `ExtraIcons`, `AppAssets` and `Icon::new(gpui_kit::assets::IconName::Lightbulb)` in gpui-probe.

### D4. Popover, dropdown, confirm dialog

- Dropdown menu on a button: `use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem}; Button::new(..).dropdown_menu(|menu, window, cx| menu.label("…").item(PopupMenuItem::new("Drafts").checked(true).on_click(..)))` and `dropdown_menu_with_anchor(Anchor, ..)` (`REG/gpui-component-0.11.0/src/menu/dropdown_menu.rs:12-30`; `PopupMenu::{label, item, separator, menu_with_check, ..}` `menu/popup_menu.rs:119-748`). Repo example: New agent picker (`views/agents/new_agent.rs:180, 192-230`).
- Popover: `Popover::new(id).trigger(button /* Selectable */).anchor(..).content(|state, window, cx| el).open(bool).on_open_change(..).overlay_closable(bool)` (`popover.rs:136-270`).
- Confirm: `window.open_alert_dialog(cx, |alert, window, cx| alert.title(..).description(..).on_ok(|_, _, _| true).footer(DialogFooter::new().child(..)))` (`window_ext.rs:101-103`; `dialog/alert_dialog.rs:81-280`); repo example with a focused safe default: `views/agents/close.rs:127-170`. All compiled in `gpui-probe::intent`.

### D5. Events → models → views, and command failures

- Path: `DaemonLink::start(config) -> (DaemonLink, mpsc::Receiver<Action>)` runs a tokio thread (`link.rs:171-209`); each session sends `Action::Snapshot { agents, repos, latest_seq }` (lists agents and repos only), `ConnectionChanged(Connected)`, then `Action::Event(event)` per daemon event (`link.rs:418-485`). `ShellState::new` pumps the receiver on GPUI's foreground executor in batches (`shell_state.rs:873-889`) → `dispatch_all` → `ShellModel::apply` → `cx.notify()` only if `model.revision()` changed (`shell_state.rs:366-399`). Panes `cx.observe_in(&state, ..)` and re-render (`agents/mod.rs:209-236`, `settings/mod.rs:122-124`).
- `ShellModel` (`model.rs:24-64`): `Action { Snapshot, Event, ConnectionChanged, SelectAgent, OpenTab, CloseTab, RevertOpen, RepoAdded }`. **`EventKind::IntentCreated(_)` and `IntentUpdated(_)` are ignored** (return `false`, nothing stored) together with `MessageAppended`, `TurnDelta`, `PluginUpdated`, `ResourceSynced` (`model.rs:213-219`). Intents are not in the snapshot either.
- Agents list: `agent_rows(model) -> Vec<AgentRowVm>` (`view_models.rs:332-370`), rendered by `AgentsPane::render` (`agents/mod.rs:550-…`).
- Commands: views call the `AgentCommands` trait (`create_agent`, `close_agent`, `add_repo`; `link.rs:299-306`) obtained from `state.agent_commands()` (`shell_state.rs:280-285`), with `answered(command, cx.background_executor().timer(timeout))` (`views/mod.rs:17-25`) inside `cx.spawn_in`. **There are no intent commands in `AgentCommands`/`DaemonLink` yet** (the client has them, D6), so the intent tool must add trait methods (and fakes in tests).
- Failure sentences: `command_error_text(&LinkError)` (`view_models.rs:504-513`: `NOT_CONNECTED_TEXT` "Not connected to the daemon", "The daemon link has stopped", else the daemon's `message`), `DAEMON_SILENT_TEXT` "The daemon did not answer" on timeout (:477); shown as a notification (`notify_unreachable` / `notify_failure` / `notify_link_error`, `agents/mod.rs:728-790`) or as a `Role::Status` line under a field (`settings/repositories.rs:270-283`, with `repo_error_text` `view_models.rs:733-745`).

### D6. Client intent methods and core types

`smithagents_client::Client` (`crates/client/src/api.rs`), all `async fn …(&self, …) -> Result<_, ClientError>`:

| method | signature | route |
|---|---|---|
| `create_intent` | `(req: CreateIntentRequest) -> Intent` | POST /v1/intents (:192) |
| `list_intents` | `(filter: IntentFilter) -> Vec<Intent>` | GET /v1/intents?state=&repo_id= (:198) |
| `get_intent` | `(id: IntentId) -> Intent` | GET /v1/intents/{id} (:220) |
| `intent_versions` | `(id: IntentId) -> Vec<IntentVersion>` | (:225) |
| `update_intent` | `(id: IntentId, patch: Patch) -> Intent` | PATCH (:233) |
| `answer_question` | `(id: IntentId, question: QuestionId, answer: AnswerInput) -> Intent` | (:242) |
| `retry_polish` | `(id: IntentId) -> Intent` | (:252) |
| `accept_item` / `drop_item` | `(id: IntentId, item: ItemId) -> Intent` | (:257, :262) |
| `edit_item` | `(id: IntentId, item: ItemId, text: &str) -> Intent` (edits and accepts) | (:269) |
| `send_intent` / `resolve_intent` | `(id: IntentId) -> Intent` | (:280, :285) |
| `agent_defaults` | `() -> AgentDefaults` | GET /v1/settings/agent-defaults (:290) |
| `set_agent_defaults` | `(defaults: AgentDefaults) -> AgentDefaults` | PUT (:295) |

`ClientError { Http(reqwest::Error), Api { status: u16, code: String, message: String }, Ws(tungstenite::Error) }` (:59-75).

Core types (`crates/core/src/intent.rs`; serde `snake_case`):
- `enum IntentKind { Enhancement, Bug, Task }` (:11); `enum IntentState { Draft, Complete, Queued, Sent, Resolved }` (:19); `enum Phase { Polishing { pass: u8 }, Questions, Review, Failed { pass: u8, message: String } }` (tag `kind`, :30).
- `enum SeedSource { Chat { agent_id, session_id: Option<String>, from_seq: Option<u64>, to_seq: Option<u64> }, Text }`; `struct Seed { source: SeedSource, text: String }` (:39-53).
- Questions: `struct QuestionOption { id: String, label: String }`; `enum Answer { Unanswered, Option { option_id: String }, Skipped }`; `struct Question { id: QuestionId, text: String, options: Vec<QuestionOption>, answer: Answer }` (:56-76).
- Items: `enum ItemStatus { Pending, Accepted, Dropped }`; `struct Item { id: ItemId, text: String, status: ItemStatus, edited: bool, intent_ref: Option<IntentId> /* depends-on only */ }` (:79-93).
- `struct Content { title, kind: IntentKind, current_behavior: String, questions: Vec<Question>, preconditions: Vec<Item>, criteria: Vec<Item>, stories: Vec<Item>, depends_on: Vec<Item> }` — the **four item sections** are `preconditions`, `criteria`, `stories`, `depends_on` (:96-106); helpers `counters() -> Counters` (`(done,total)` per section, :109-150), `pending_count()`, `questions_open()`.
- `struct Intent { id: IntentId, repo_id: RepoId, slug: String, state: IntentState, phase: Phase, seed: Seed, content: Content, agent_id: Option<AgentId>, created_at, updated_at, queued_at: Option<_>, sent_at: Option<_>, resolved_at: Option<_>, last_start_error: Option<String> }` (:166-182); `is_complete_content()` (:184-190).
- Versions: `enum VersionAuthor { Seed, Polish1, Polish2, Send }`; `struct IntentVersion { intent_id, version: u32, author, content: Content, at }` (:194-208).
- Inputs: `struct IntentFilter { state: Option<IntentState>, repo_id: Option<RepoId> }` (:246); `enum SeedInput { Chat { agent_id, from_seq, to_seq }, Text(String) }`; `struct CreateIntentRequest { repo_id: Option<RepoId>, seed: SeedInput }` (:254-268); `struct Patch { title, kind, current_behavior, repo_id: all Option }` (:271-282); `enum AnswerInput { Option(String), Skip }` (wire `{"option_id"}` / `{"skip":true}`, :284-318); `enum ReviewAction { Accept, Edit(String), Drop }` (:322); `struct AgentDefaults { queue_limit: Option<u32> }` (:329). `slugify(&str) -> String` (:216).

---

## Unverified

- Runtime capture on this Mac (macOS 26, Darwin 25.6): whether `xcap::Window::all()` lists the app's own window without Screen Recording permission, whether `capture_image()` of its own window works without permission and without a prompt, and whether the result is at Retina scale. Not run by instruction. Apple's stated rule (WWDC 2019, quoted by DTS) is that apps may record their own windows; reports say the CG calls still add the app to the Screen Recording list / prompt on macOS 15+.
- Future macOS: the Oct 2026 forum report of `CGWindowListCreateImage` returning gray images on macOS 27 betas concerns other processes' windows; its effect on capturing one's own window is unknown. xcap has no ScreenCaptureKit path (#160 open).
- That macOS omits `kCGWindowName` for other apps' windows without permission (so xcap's list shrinks to own windows) is from general knowledge, not proved here.
- Thread-safety of calling xcap capture from GPUI's background executor (`Send` is declared by xcap; CoreGraphics thread-safety not checked).
- Whether the window frame size reported by xcap equals `window.bounds().size` exactly (both are the frame in points per source; not measured) — keep a tolerance.
- Visual quality: how `TextView::markdown` renders headings/lists, how `ProgressCircle` looks at rail size, letterboxed image sampling — compile/headless only, no pixels inspected.
- Whether two `on_next_frame` hops are enough for the window server to have the ring-free frame before `CGWindowListCreateImage` reads it (a short timer may be needed).
- Windows/Linux builds of xcap were not compiled (only the lock resolution and tree were checked); the app crate itself is unix-only today (`atomic_file.rs:6`).

Sources consulted on the web: [xcap issues search](https://github.com/nashaofu/xcap/issues?q=is%3Aissue+macos+permission), [xcap #123](https://github.com/nashaofu/xcap/issues/123), [xcap #138](https://github.com/nashaofu/xcap/issues/138), [xcap #160](https://github.com/nashaofu/xcap/issues/160), [Apple forums 849969](https://developer.apple.com/forums/thread/849969), [WebKit bug 277564 (obsoleted in macOS 15 SDK)](https://bugs.webkit.org/show_bug.cgi?id=277564), [Apple forums 760483](https://developer.apple.com/forums/thread/760483), [crates.io sparse index for xcap](https://index.crates.io/xc/ap/xcap).

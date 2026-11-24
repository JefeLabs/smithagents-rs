# CEF-in-GPUI Proof (Plan 0) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. This is a **spike**: its product is a go/no-go decision backed by measurements, plus throwaway code that Plan 3a Task 16 reuses. Tasks end with recorded evidence, not feature polish.

**Goal:** Decide, with evidence, how a CEF (Chromium) view is embedded in a GPUI macOS window beside native left and right panes, and hand Plan 3a Task 16 a working embedding plus the exact pins and API fit.

**Architecture:** A standalone spike workspace (`spikes/cef-gpui/`, outside the main workspace so `cargo test --workspace` never needs cmake or the CEF download) with a GPUI window (left pane, center rectangle, right pane, collapsible) and three candidate embeddings tried in order of preference: **A** CEF as a child NSView in a `gpui-fast` composition surface (keeps GPUI popovers above the page); **B** CEF off-screen rendering drawn as a GPUI image element (the `gpui-cef` approach ported onto the `gpui-pre` snapshot); **C** a borderless native window docked to the center rectangle. A Node CDP probe and a manual checklist score each against the spec's go/no-go list.

**Tech Stack:** Rust, `gpui-kit =0.11.0` with feature `gpui-fast` (gpui-pre 0.3.14 base, zed@f126950), `cef =156.7.0` (CEF/Chromium 156), cmake + Ninja (CEF wrapper build), Node 22+ (CDP probe, esbuild fixtures), `@xterm/xterm`, `@xyflow/react`.

**Spec:** `docs/superpowers/specs/2026-11-23-smithagents-architecture-design.md` (section 6 "First milestone: the CEF-in-GPUI proof", section 7 Risks, section 4.6 Browser). Roadmap: `docs/superpowers/plans/2026-11-24-v1-roadmap.md` (go/no-go checklist). Consumer: `docs/superpowers/plans/2026-11-24-desktop-shell.md` (Task 11 `CenterTabHost`, Task 16 CEF engine).

## Go/no-go checklist (verbatim from the spec, with gates)

A CEF view embedded in the GPUI window on macOS must:

| # | Requirement | How it is checked | Gate |
|---|---|---|---|
| 1 | Sit in the center beside native left, right and bottom panes | Visual plus CDP `innerWidth/innerHeight` equals the center rect | must |
| 2 | Resize and collapse with them | Drag and collapse; probe reads size after each change | must |
| 3 | Render at native resolution | CDP `devicePixelRatio` equals GPUI scale factor (2 on Retina); `Page.captureScreenshot` at 2x pixel width; text not blurry (manual) | must |
| 4 | Take keyboard focus and IME input | Manual script: type, Cmd+C/V/A/Z, Japanese Hiragana and Chinese Pinyin composition in a textarea, dead key é, emoji picker | must |
| 5 | Coexist with native popovers and menus | GPUI popover, tooltip and dropdown overlapping the center rect are visible above the page and clickable; app-menu items work while the page has focus | must |
| 6 | Load xterm.js and react-flow pages | Fixtures render and respond to input; probe checks DOM state | must |
| 7 | Expose CDP on loopback | `curl http://127.0.0.1:<port>/json/version` works; not reachable on the LAN address | must |
| 8 | Stable | 10-minute soak with resize, theme-switch script and tab visibility toggles: no crash, no helper left after quit, relaunch works | must |
| 9 | Fast enough | Page-side `requestAnimationFrame` rate with a CSS animation: at least 55 fps idle, at least 30 fps while the window is being resized; idle CPU under 5% of one core; typing feels immediate (manual) | should |

Fallback order from the spec (the owner is told before switching): A, then B, then C. A path **passes** when every "must" row passes and "should" rows are recorded; **partial** when a must row fails but has a named fix inside the time box; **fail** otherwise.

## Global Constraints

- The spike lives in `spikes/cef-gpui/` with its own `[workspace]` table and lockfile; nothing under `crates/` or the root `Cargo.toml` changes in this plan.
- `cargo themegen lint` scans the spike's source and HTML: keep literal colors out of `.rs` files (use `// theme-lint: allow` with a reason where a test needs one) and put colored fixture pages under `fixtures/` (a skipped directory name).
- Pins: `gpui-kit =0.11.0` (feature `gpui-fast`), `cef =156.7.0`. Record the resolved `gpui-pre`/`gpui-fast` versions in the report. Upgrading is out of scope.
- Code taken from `bokuweb/gpui-cef` (MIT OR Apache-2.0) keeps its license headers and is credited in `spikes/cef-gpui/NOTICE`.
- CDP is bound to 127.0.0.1 on a random port chosen at startup; the spike never opens it on another interface.
- macOS Apple silicon only. Naming is `smithagents`, never `singularity`.
- Time boxes (the owner is told when one expires): Task 1 half a day, Task 3 one day, Task 4 two days, Task 5 two days, Task 6 one day. Stop and report at the box rather than extending silently.

## Review Focus

1. Retina and display changes: dragging the window between a 2x and a 1x display (or changing scaling) updates `devicePixelRatio` and keeps text sharp (Tasks 4, 5).
2. Overlays: a GPUI popover, tooltip and dropdown that extend over the center rectangle are above the page, receive clicks, and hide nothing wrongly when the page scrolls under them (Tasks 4, 5).
3. IME and shortcuts: Pinyin and Japanese composition show a candidate window at the caret and commit once; Cmd+C/V/A/Z reach the page; Cmd+Q and Cmd+W reach the app; no key types twice (Tasks 4, 5).
4. Lifecycle: quitting during page load, closing the window, force-quit and relaunch leave no helper process and no profile-lock error; a second instance is refused cleanly (Tasks 1, 4, 5).
5. Message pump: heavy page (react-flow dragging) does not starve GPUI frames, and an idle app does not spin the CPU (Tasks 4, 5).
6. CDP exposure: the debugging port is loopback only; the report states plainly that any local process can drive the browser (and read anything injected into it), which the spec accepts for v1 (Task 7).

---

### Task 1: Toolchain, spike workspace and a bundled CEF that loads a page

**Owner prerequisite (a system change, so the owner does it or approves it):** this machine has no `cmake` or `ninja` (checked 2026-11-24, macOS 26.6.2, Command Line Tools only). Run `brew install cmake ninja`. If the CEF wrapper build fails with `'atomic' file not found` (stale CLT libc++), set `CXXFLAGS="-nostdinc++ -isystem $(xcrun --show-sdk-path)/usr/include/c++/v1"` as `gpui-cef`'s `justfile` does. Optionally pre-install the CEF distribution once with `cargo run -p export-cef-dir -- --force $HOME/.local/share/cef` from a `cef-rs` checkout and export `CEF_PATH` (about 135 MB download).

**Files:**
- Create: `spikes/cef-gpui/Cargo.toml` (own `[workspace]`, members `app`, `helper`, `xtask`), `spikes/cef-gpui/app/{Cargo.toml,src/main.rs}`, `spikes/cef-gpui/helper/{Cargo.toml,src/main.rs}`, `spikes/cef-gpui/xtask/{Cargo.toml,src/main.rs}`, `spikes/cef-gpui/README.md`, `spikes/cef-gpui/NOTICE`, `spikes/cef-gpui/.gitignore`

**Interfaces:**
- Produces: `cargo run -p xtask` builds `target/bundle/cef-gpui-spike.app` using `cef::build_util::mac::build_bundle` (main executable, helper apps, `Chromium Embedded Framework.framework`); `open` of the bundle starts the app. The app initializes CEF with the verified sequence: `LibraryLoader::new(&exe, false).load()`, `cef::api_hash(CEF_API_VERSION_LAST, 0)`, `execute_process` (exit when not -1), then `initialize` with `Settings{ external_message_pump: 1, multi_threaded_message_loop: 0, remote_debugging_port: <random free port, logged>, root_cache_path: <temp dir>, ..Default::default() }`. This task uses CEF's own windowed browser (cefsimple style) only to prove the toolchain; GPUI arrives in Task 3.

- [ ] **Step 1: Verify tools.** Run `cmake --version`, `ninja --version`, `node --version` (22 or newer), `xcode-select -p`; record outputs in `spikes/cef-gpui/README.md`. Expected: all present.
- [ ] **Step 2: Create the workspace and the bundle task**, modeled on `cef-rs/examples/cefsimple` and `gpui-cef`'s `xtask` (read both; copy only what is needed, keep headers, add NOTICE).
- [ ] **Step 3: Build and launch the bundle.** Run `cargo run -p xtask && open target/bundle/cef-gpui-spike.app`. Expected: a window showing a local fixture page (`fixtures/hello.html`).
- [ ] **Step 4: Prove CDP and lifecycle.** Run `curl -s http://127.0.0.1:<logged port>/json/version` (expect JSON with `webSocketDebuggerUrl`), confirm the port is not reachable on the machine's LAN address, quit, and run `pgrep -fl "cef-gpui"` (expect nothing), then launch again (expect no profile-lock error).
- [ ] **Step 5: Record and commit.** Write measured build time, bundle size and any workarounds in the README. Commit `spike(cef): spike workspace, bundle task and a CEF window that loads a page`.

---

### Task 2: Fixtures and the CDP probe

**Files:**
- Create: `spikes/cef-gpui/fixtures/{hello.html,env.html,fps.html,ime.html,xterm.html,flow.html}`, `spikes/cef-gpui/web/{package.json,build.mjs,xterm-entry.ts,flow-entry.tsx}`, `spikes/cef-gpui/probe/probe.mjs`, `spikes/cef-gpui/probe/README.md`
- Test: `spikes/cef-gpui/probe/probe.test.mjs` (node:test, runs the probe's pure functions against canned CDP replies)

**Interfaces:**
- Produces: fixtures built by `npm run build` (esbuild bundles `@xterm/xterm` and `@xyflow/react` into `fixtures/dist/`, gitignored). `env.html` shows `devicePixelRatio`, `innerWidth`, `innerHeight`. `fps.html` runs a CSS animation and a `requestAnimationFrame` counter exposed as `window.__fps()`. `ime.html` has a textarea logging `compositionstart/update/end`, `keydown` and `input` into `window.__log`. `xterm.html` mounts a terminal that echoes typed text; `flow.html` mounts a react-flow canvas with three draggable nodes.
- `probe.mjs`: `node probe/probe.mjs --port <p> --scenario <name> --out results/<path>-<scenario>.json` (Node's global `WebSocket`, no dependencies). It fetches `/json/list`, attaches to the first `page` target, runs `Runtime.evaluate` expressions, and writes JSON `{path, scenario, at, dpr, innerWidth, innerHeight, fps, xtermOk, flowOk, notes}`; scenarios: `env` (size and dpr), `fps` (sample `__fps()` for 3 s), `xterm` (dispatch input through `Input.dispatchKeyEvent`, assert terminal buffer text), `flow` (assert 3 nodes in the DOM and that a synthetic drag moved one), `screenshot` (`Page.captureScreenshot`, report pixel width). Exported pure functions: `parseVersionJson`, `pickPageTarget(list)`, `summarize(results)`.

- [ ] **Step 1: Write failing probe tests** for the pure functions: `pickPageTarget` skips `service_worker` and `iframe` targets and returns the first `page`; it returns `null` for an empty list; `parseVersionJson` extracts `webSocketDebuggerUrl`; `summarize` marks a result failed when `dpr` differs from `expectedDpr` or `innerWidth` differs from `expectedWidth` by more than 1 CSS pixel.
- [ ] **Step 2: Run `node --test spikes/cef-gpui/probe`.** Expected: FAIL.
- [ ] **Step 3: Implement the probe and the fixtures.**
- [ ] **Step 4: Run the tests and the probe against the Task 1 window** (`--scenario env` and `fps`). Expected: PASS and a results file showing the plain-CEF baseline (this is the control for Tasks 4 and 5).
- [ ] **Step 5: Commit** `spike(cef): fixtures and CDP probe with baseline results`.

---

### Task 3: GPUI host window with the layout (no CEF yet)

**Files:**
- Create: `spikes/cef-gpui/app/src/{layout.rs,panes.rs}`; modify `app/src/main.rs`, `app/Cargo.toml` (`gpui-kit = { version = "=0.11.0", features = ["gpui-fast"] }`; note the `extern crate gpui_kit as gpui;` requirement for macros)
- Test: `spikes/cef-gpui/app/tests/layout.rs`

**Interfaces:**
- Produces: a GPUI window with a collapsible left pane, a center rectangle reserved for the web view, a collapsible right pane, and an icon rail on the right edge, all through `DockArea` if it expresses this (otherwise a hand-made flex layout; record which). Left pane contains: a button "Popover" opening a gpui-component popover whose content extends over the center rectangle, a dropdown (select) and a tooltip target. The app menu (`cx.set_menus`) has a "Spike" menu with one action that logs. `layout::center_rect(window_bounds, left_w, right_w, rail_w) -> Bounds` (pure) and `layout::to_device_pixels(bounds, scale) -> DeviceRect` used by every path. Window composition is enabled: `window.enable_window_composition()` succeeds (log the result, because it is the precondition for Path A).

- [ ] **Step 1: Write failing tests** for the pure layout math: center rect equals window minus pane widths and rail; collapsing a pane gives its width to the center; a window narrower than both panes' minimums yields a center rect of at least the declared minimum and never negative; device-pixel conversion at scale 2 doubles every field and rounds to integers consistently (no 1-pixel gaps between adjacent rects).
- [ ] **Step 2: Run `cargo test -p app` in the spike workspace.** Expected: FAIL.
- [ ] **Step 3: Implement the layout and the window**; run it (`cargo run -p app`). Verify manually: panes collapse and expand, the popover overlaps the center rectangle, `enable_window_composition` result is logged as OK or the exact error.
- [ ] **Step 4: Run the tests** (expected PASS) and record in the README: which dock mechanism was used, any gpui-fast behavior notes (the retained-mode rule: state changes outside tracked entities need `cx.notify()`).
- [ ] **Step 5: Commit** `spike(cef): GPUI host window with panes, popover targets and composition enabled`.

---

### Task 4: Path A, CEF as a child NSView in a composition surface

**Files:**
- Create: `spikes/cef-gpui/app/src/cef_child/{mod.rs,nsapp.rs,pump.rs,handlers.rs}`
- Modify: `app/src/main.rs` (feature or CLI flag `--path a`), `helper/src/main.rs`
- Test: `spikes/cef-gpui/app/tests/bounds.rs` (pure bounds and coordinate math), probe results under `spikes/cef-gpui/results/`

**Interfaces:**
- Produces:
  - `nsapp::adopt_cef_app_protocol()`: after GPUI creates its NSApp and before `cef::initialize`, add `isHandlingSendEvent`/`setHandlingSendEvent:` to the `GPUIApplication` class, wrap `sendEvent:` setting the flag for the duration (restoring the previous value for nesting), and register `CrAppProtocol`, `CrAppControlProtocol`, `CefAppProtocol` at runtime. Port from `gpui-cef`'s `platform/cef/nsapp.rs` (credit in NOTICE); this is the only `unsafe`/Objective-C code, isolated here, each block commented.
  - `pump::start()`: a `CFRunLoopTimer` (60 Hz fallback, common modes) plus a thread-safe `CFRunLoopSource` signalled from `BrowserProcessHandler::on_schedule_message_pump_work(delay_ms)`; calls `cef::do_message_loop_work()` outside any GPUI borrow; never on GPUI's executor. Port from `gpui-cef`'s `pump.rs`.
  - `cef_child::open(window, composition, url) -> ChildView`: `let c = window.enable_window_composition()?; let s = c.create_native_surface()?;` get the surface NSView through `platform_surface()?.platform_handle()?` (downcast `usize`), create the browser with `WindowInfo::default().set_as_child(parent_view, &rect)` through `browser_host_create_browser`, and on each layout call `surface.platform_surface()?.set_bounds(device_rect)`, `set_visible`, and `browser_host.was_resized()`/`notify_screen_info_changed()` when size or scale changed. Page-side `window.__smithagentsOpenExternal` is not part of the spike.
  - Command line switches in `on_before_command_line_processing`: `remote-debugging-port=<port>`, `no-startup-window`, `noerrdialogs`, `use-mock-keychain`; `KeyboardHandler` consumes keys CEF would bounce through `NSMenu.performKeyEquivalent:`.

- [ ] **Step 1: Write failing bounds tests:** the rect passed to CEF is in the surface's local coordinates (origin 0,0, bottom-up flip for AppKit handled in one function with tests at scale 1 and 2); a resize from 800x600 to 801x601 at scale 2 produces consistent device pixel sizes; zero or negative sizes are clamped to 1x1 (CEF refuses 0x0).
- [ ] **Step 2: Run `cargo test -p app --test bounds`.** Expected: FAIL.
- [ ] **Step 3: Implement the NSApp retrofit, the pump and the child view; open `fixtures/env.html`.** First milestone: the page appears inside the center rectangle. If CEF refuses `parent_view` or the view never shows, record the exact failure and the attempts, then go to Task 5 (this is the "partial/fail" branch).
- [ ] **Step 4: Run the checklist rows 1 to 8 in order**, recording evidence in `results/path-a/`: probe scenarios `env`, `fps`, `xterm`, `flow`, `screenshot` after launch, after resize, after collapsing each pane, and after dragging to a second display if available; manual script for rows 3 (sharpness), 4 (IME, shortcuts, emoji), 5 (popover, tooltip, dropdown above the page; app-menu item while the page has focus; screenshot via `screencapture -l <window id>` saved as evidence, or a human note if Screen Recording permission is unavailable), 8 (10-minute soak, quit, `pgrep`, relaunch). Row 9: fps and CPU (`top -pid`, 30 s average idle).
- [ ] **Step 5: Write `results/path-a/SUMMARY.md`** (each row pass/fail/partial with evidence links, plus a verdict) and commit `spike(cef): path A child NSView in a composition surface, results`.

---

### Task 5: Path B, off-screen rendering into a GPUI image element (run when Path A is not a full pass)

**Run condition:** any "must" row of Path A is not pass. Also run it if the owner wants a comparison. Record the condition in the summary.

**Files:**
- Create: `spikes/cef-gpui/app/src/cef_osr/{mod.rs,element.rs,input.rs,ime.rs}` (reuse `nsapp.rs` and `pump.rs` from Task 4)
- Test: `spikes/cef-gpui/app/tests/osr_input.rs` (pure input translation), results under `spikes/cef-gpui/results/path-b/`

**Interfaces:**
- Produces: `cef_osr::open(window, cx, url) -> OsrView` creating the browser with `windowless_rendering_enabled: 1` (`WindowInfo::set_as_windowless`, `Settings.windowless_rendering_enabled: 1`), `RenderHandler::{view_rect, screen_info (device_scale_factor from window.scale_factor()), on_paint, on_popup_show, on_popup_size, on_ime_composition_range_changed}`; paint copies the BGRA buffer into `gpui::RenderImage::new(vec![Frame])` and draws it with `gpui::img(ImageSource::Render(..)).object_fit(Fill)` inside a custom element whose prepaint stores bounds and scale and whose paint calls `window.handle_input(focus_handle, ElementInputHandler::new(..))`. Input: GPUI mouse, scroll and key events become `send_mouse_move_event`, `send_mouse_click_event`, `send_mouse_wheel_event`, `send_key_event`; IME through an `EntityInputHandler` mapping to `ime_set_composition`, `ime_commit_text`, `ime_cancel_composition`, `ime_finish_composing_text`; focus through `set_focus`. Port from `gpui-cef`'s `platform/cef/{mod,input,ime,handlers}.rs` against the `gpui-pre`/`gpui-fast` API (credit in NOTICE). Only the CPU BGRA path is built; the accelerated IOSurface path is out of scope unless frame cost (row 9) fails, because stock `surface()` is NV12-only and the objc2 type differs.
- Pure functions (tested): `input::mouse_event(pos, element_bounds, modifiers) -> MouseEventFields` (window to element-local coordinates, never negative), `input::scroll_deltas(raw, is_pixels) -> (i32, i32)` with a stated sign convention, `input::key_event(...)` mapping of printable keys held until AppKit decides `insertText` vs marked text.

- [ ] **Step 1: Write failing input tests:** mouse position subtracts element bounds and is scale-correct; scroll delta sign for natural scrolling matches the page's expected direction (assert against the documented convention in the test name); printable key down is held, then released as a CHAR event on `insertText` and dropped on `setMarkedText`.
- [ ] **Step 2: Run `cargo test -p app --test osr_input`.** Expected: FAIL.
- [ ] **Step 3: Implement the OSR path** and open `fixtures/env.html` in the center rectangle.
- [ ] **Step 4: Run the same checklist as Task 4 into `results/path-b/`**, and add: paint cost (time per `on_paint` copy at 2x full-pane size, 60 s average), CPU while scrolling `flow.html`, and perceived typing latency in `xterm.html` against Path A (or plain CEF from Task 2).
- [ ] **Step 5: Write `results/path-b/SUMMARY.md`** and commit `spike(cef): path B off-screen rendering into a GPUI element, results`.

---

### Task 6: Path C, borderless window docked to the center (run only when A and B both fail a must row)

**Files:**
- Create: `spikes/cef-gpui/app/src/cef_window/{mod.rs,sync.rs}`
- Test: `spikes/cef-gpui/app/tests/window_sync.rs` (pure geometry), results under `spikes/cef-gpui/results/path-c/`

**Interfaces:**
- Produces: a CEF windowed browser inside a borderless `NSWindow` added as a child window of the GPUI window (`addChildWindow:ordered:`), positioned in screen coordinates from the center rectangle on every layout, move, resize, collapse, Spaces change and display change; hidden when the GPUI window is minimized or covered. `sync::screen_rect(window_frame, center_rect, screen_height) -> ScreenRect` (pure; AppKit's bottom-left origin conversion). Overlays above the page are impossible in this path (the page is a separate window); the summary must say which overlay behaviors were lost and the UI workaround (pane popovers confined to the panes, or opened as their own windows).

- [ ] **Step 1: Write failing geometry tests** (retina, window on a second display with negative x, window moved between screens).
- [ ] **Step 2: Run `cargo test -p app --test window_sync`.** Expected: FAIL.
- [ ] **Step 3: Implement, run checklist rows 1, 2, 3, 4, 6, 7, 8 and record row 5 as the expected loss.**
- [ ] **Step 4: Write `results/path-c/SUMMARY.md`** and commit `spike(cef): path C docked borderless window, results`.

---

### Task 7: Decision report and handoff

**Files:**
- Create: `docs/superpowers/spikes/2026-11-cef-gpui-proof.md`
- Modify: `docs/superpowers/plans/2026-11-24-v1-roadmap.md` (Plan 0 status and verdict), `docs/superpowers/plans/2026-11-24-desktop-shell.md` (Task 11 and Task 16 text if the findings change them)

**Interfaces:**
- Produces the report with these sections, in order: **Verdict** (go with path X, or no-go) in the first two lines; **Checklist table** filled for each path that was run (pass, partial, fail with a one-line reason and a link to evidence); **Pins and prerequisites** (exact resolved versions of `gpui-kit`, `gpui-pre` or `gpui-fast`, `cef`, CEF/Chromium build, macOS version, tools, CEF download and extracted size, wrapper build time, workarounds such as the libc++ flags); **Embedding recipe** (the minimal sequence of calls that works, with file paths in the spike to lift into `crates/cef-host`); **Fit with `CenterTabHost`** (the Task 11 trait takes `attach(&dyn HasWindowHandle)`: state whether the chosen path needs a composition surface or a GPUI element instead, and give the exact signature changes Task 11 must make before Task 16); **Document-start script injection** (which CEF hook ran the `host_script` before page scripts and the evidence: a page that logs `window.__SMITH_THEME__` in its first inline script); **Navigation and popups** (how `window.open`, `target=_blank` and redirects are intercepted); **Performance numbers**; **Known issues and risks** (CDP exposure: any local process can drive the browser and read anything injected, accepted by the spec for v1; bundle and code-signing notes; the weekly gpui-kit pin churn); **Recommendations for Plan 3a Task 16** (what to productionize, what to delete).

- [ ] **Step 1: Assemble the report from the three `SUMMARY.md` files**; every claim links to a results file or a recorded manual check.
- [ ] **Step 2: Cross-check against the checklist:** a "go" verdict requires every "must" row to pass on the chosen path; otherwise the verdict is "go with caveats" listing the failing rows and the proposed product-level mitigation, or "no-go" with the recommended re-plan (for example a different engine behind `CenterTabHost`, which the architecture allows).
- [ ] **Step 3: Update the roadmap Plan 0 row** with the verdict and report path, and edit Plan 3a Tasks 11 and 16 where the report requires it.
- [ ] **Step 4: Tell the owner the verdict before any fallback beyond Path A is adopted** (spec requirement); the report is the artifact for that conversation.
- [ ] **Step 5: Commit** `docs: CEF-in-GPUI proof report and roadmap update`.

---

## Self-review notes

- **Spec coverage:** every row of the spec's milestone list maps to a checklist row and a probe or manual step (center beside panes, resize and collapse, native resolution, keyboard focus and IME, coexistence with popovers and menus, xterm.js and react-flow, CDP on loopback); the fallback order A, B, C matches the spec's order of offscreen rendering then docked window (Path A is the spec's primary child-view attempt, and the `gpui-fast` composition surface is the means to keep overlays above it). The spec's "bottom pane" is not exercised because v1 has none; the layout math supports one later.
- **Known unknowns this plan exists to resolve:** whether CEF accepts a composition surface's NSView as `parent_view` (no one has shown it); whether `gpui-fast` composition and CEF's own compositor coexist without flicker; IME fidelity in either path (the `gpui-cef` README says live IME was not confirmed by a human); the OSR copy cost at Retina sizes; whether document-start injection works in the chosen path.
- **Not in scope:** the product bundle and signing (Plan 3a Task 16), the daemon sidecar, theme push, terminal and flow as real tabs, accelerated OSR, other platforms.
- **Owner touchpoints:** `brew install cmake ninja` in Task 1; Screen Recording permission for the overlay screenshot check (a human note substitutes); the fallback notification in Task 7.

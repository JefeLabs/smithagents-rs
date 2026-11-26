# Feedback Capture (feedback tool, step 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Holding the Feedback rail entry for 3 seconds (or pressing Capture) takes a picture of the SmithAgents window, opens a full-window annotator for numbered boxes with comments, and keeps the result, with files added by hand, as a draft on disk that survives a restart. Submit is shown and disabled.

**Architecture:** A new crate `crates/capture` holds a small `Capturer` trait with the `xcap` library behind it and a fake for tests. Everything else lives in the app crate: a pure `feedback` module (annotation model, PNG drawing, file checks, the draft store), one `Feedback` state entity that owns the draft and runs the capture sequence against the trait, and three views (the hold button on the rail, the Feedback pane in the right dock, the annotator overlay drawn by the shell above every pane). The real capturer, the native file picker and the draft directory are injected through `ShellOptions`, so no test reaches the screen, a dialog or the real home.

**Tech Stack:** Rust (edition 2021), `gpui-kit = "=0.11.0"` (GPUI `gpui-pre 0.3.14`, `gpui-component 0.11.0`), `xcap = "=0.9.10"`, `image 0.25` (already in the lock at 0.25.10), `objc2-core-graphics 0.3.2` (already in the lock; macOS only), `serde`/`serde_json`, `uuid`.

**Spec:** `docs/superpowers/specs/2026-11-26-feedback-tool-design.md`, plan-split item 1 only. Verified API shapes: `docs/superpowers/research/2026-11-26-gpui-capture-intent-research.md` sections A, B, C (cited below as "research B3" and so on). Read both before starting a task.

**Built after:** `docs/superpowers/plans/2026-11-26-intent-tool.md`, on top of its branch. Its Task 1 makes the rail hold several right-dock entries and bundles the icons this plan uses; its Interfaces block and its paragraph "How a later plan adds a rail entry and pane" are the contract summarized under "Consumed from the intent-tool plan" below. If the code on the branch differs from that summary, the code wins.

## Scope

In: `crates/capture`; `RailEntry::Feedback` with the hold; the annotator; the Feedback pane; the draft in `<home>/feedback/draft/`; a short user note. Out (see Follow-ups): Submit and its dialog, the daemon route, the meta repository, GitHub, the idea package, recording video, drawing tools other than boxes. No daemon, client or core change. Nothing leaves the machine.

## Facts this plan relies on

- Rail, dock, `ShellOptions` and the test harness: research A1, A2. An element is findable in tests only with `.id(..)` and `.test_support()`; a gpui-component `Button` by its id alone. A `Button`'s `.tooltip(text)` is its accessibility description, read in tests with `ElementSnapshot::description()` (`gpui-base-0.11.0/src/test_support.rs:129`, `gpui-component-0.11.0/src/button/button.rs:787-788`).
- The app's data directory is the daemon's home (`LinkConfig.home`), so `<app data>/feedback/draft/` is `<home>/feedback/draft/`; `crate::atomic_file::write_private(dir, file_name, bytes)` creates the directory (0700) and writes atomically (0600) (research A3).
- Hold timer, ring, cancel, and "the release after a completed hold still fires `on_click`": research B3. Drag, canvas bounds, badges, Escape as the `gpui_kit::component::input::Escape` action handled by the parent: research B2, B4. Both are proved by the probe's headless tests; the probe source (`feedback-research/ws/crates/{capture-probe,gpui-probe}` in the research scratch directory, if still there) is a reference, not code to copy whole.
- Image display, fit math and release: research B1. `App::drop_image(image, None)` skips the window being updated (`gpui-pre-0.3.14/src/app.rs:3321-3334`), so every release passes `Some(window)`.
- xcap's API, own-window matching by pid and size, the safe `CGPreflightScreenCaptureAccess()` / `CGRequestScreenCaptureAccess()`: research C1 to C3. The probe compiled them; **nothing was ever run** (see Manual check).
- The color check (`tools/themegen/src/lint.rs`) flags hex literals and lower-case color function calls whose first argument is a number, in `.rs` files outside directories named `tests`. A `[u8; 3]` constant and `image::Rgba([..])` are not flagged.

## Decisions made here (the design leaves them to the builder)

- **No capture without permission, but ask once.** On macOS, when the preflight says no, `XcapCapturer` calls `CGRequestScreenCaptureAccess()` once per run (this shows the system prompt and lists the app in System Settings) and returns `PermissionDenied` without capturing.
- **No monitor-crop fallback.** When no window of our process matches, the answer is `WindowNotFound` (research C2: GPUI gives no reliable global rectangle). The design's "falling back to the window's screen rectangle" is dropped.
- **Leaving the button cancels without a click.** A release on the button before 3 seconds is a click (opens the pane). Moving off the button cancels the hold and does nothing else.
- **A capture is saved at once.** The screenshot joins the draft when the capture succeeds, and each saved box rewrites `draft.json`, so a crash in the annotator loses nothing. Done draws the annotated image. A `rendered` flag per screenshot says whether the annotated image matches the boxes; the store redraws at start when it does not.
- **Discard in the annotator removes that screenshot** (new or reopened), with a confirm only when it has boxes.
- **Sizes** are binary: 10 MB = `10 * 1024 * 1024` bytes. The draft's total counts each attachment's original file only.
- **A file must pass both checks and they must agree:** its extension and its leading bytes name the same type.
- **Stored names:** `screenshot-<n>.png`, `screenshot-<n>.annotated.png`, `image-<n>.<ext>`, `clip-<n>.mp4`, thumbnails `<stem>.thumb.png`. `<n>` is one more than the highest number of that prefix in the draft. The number shown on a card is the screenshot's position among the draft's screenshots.
- **Key and menu:** action `ToggleFeedbackPane`, `cmd-alt-f`, View menu item "Toggle Feedback" between "Toggle Settings" and "Toggle Intents" (the rail's order).
- **Colors on screen** are tokens: boxes and badge fill `destructive`, badge number `background` (through `token_paint`). **Colors in the PNG** are two named constants in `feedback/render.rs`; `annotate` takes no color argument.

Exact sentences (constants in `crates/app/src/feedback/text.rs`; `{}` is filled in):

| Constant | Text |
|---|---|
| `PERMISSION_TEXT` | `SmithAgents needs Screen Recording permission to capture its window. Allow it in System Settings → Privacy & Security → Screen Recording, then quit and reopen SmithAgents.` |
| `WINDOW_NOT_FOUND_TEXT` | `Could not find the SmithAgents window to capture. Try again.` |
| `UNSUPPORTED_TEXT` | `Capturing the window is not supported on this system. You can still add files.` |
| `capture_failed_text(m)` | `Could not capture the window: {m}.` |
| `DRAFT_FULL_TEXT` | `A draft holds at most 10 attachments. Remove one first.` |
| `Refusal::Extension` | `{name} was not added: only png, jpg, gif, webp and mp4 files can be attached.` |
| `Refusal::Contents` | `{name} was not added: its contents are not a png, jpg, gif, webp or mp4 file.` |
| `Refusal::ImageTooLarge` | `{name} was not added: an image can be at most 10 MB.` |
| `Refusal::VideoTooLarge` | `{name} was not added: a video can be at most 50 MB.` |
| `Refusal::TotalTooLarge` | `{name} was not added: the draft would be larger than 100 MB.` |
| `Refusal::TooMany` | `{name} was not added: a draft holds at most 10 attachments.` |
| `Refusal::Unreadable` | `{name} was not added: it could not be read.` |
| `picker_failed_text(e)` | `Could not open the file picker: {e}.` |
| `BROKEN_DRAFT_TEXT` | `The saved feedback draft could not be read and was set aside. This is a new, empty draft.` |
| `missing_files_text(n)` | `1 attachment was removed because its file is missing.` / `{n} attachments were removed because their files are missing.` |
| `save_failed_text(e)` | `Could not save the feedback draft: {e}.` |
| `TOO_MANY_BOXES_TEXT` | `A screenshot can have at most 20 boxes.` |
| `COMMENT_TOO_LONG_TEXT` | `A comment can be at most 500 characters.` |
| `SUBMIT_TOOLTIP` | `Submitting arrives with the next update` |
| `COMMENT_HINT` | `Enter saves · Esc drops` |
| `annotator_title(n, m)` | `Screenshot {n} of {m} · Drag a box over what you mean` |
| `card_title(n, k)` | `Screenshot {n} · {k} comments` (`1 comment` when k is 1) |
| `EMPTY_TEXT` | `No feedback yet. Hold the Feedback button for 3 seconds, or press Capture.` |
| `HELP_TITLE`, `HELP_LINES` | `How to submit feedback`; `Hold the Feedback button for 3 seconds, or press Capture, to take a picture of this window.` / `Drag a box over what you mean and type a comment. Add as many boxes as you need.` / `Add more screenshots or files. The draft is kept until you discard it.` |
| Discard dialog | title `Discard this feedback?`, description `{n} attachments and their comments are deleted. This cannot be undone.` (`1 attachment and its comments are deleted. …`), buttons `Cancel`, `Discard` |
| Annotator discard dialog | title `Discard this screenshot?`, description `Its {k} comments are deleted.` (`Its 1 comment is deleted.`), buttons `Keep`, `Discard` |

## Global Constraints

- Naming is `smithagents`. The new package is `smithagents-capture` in `crates/capture`.
- `unsafe_code = "forbid"` stays in every crate, including `crates/capture` (`[lints] workspace = true`). No `unsafe` of ours anywhere.
- No literal colors outside token files; `cargo themegen check` and `cargo test -p themegen` stay green without any change to `tools/themegen`.
- Exact pins: `gpui-kit = "=0.11.0"` (unchanged), `xcap = "=0.9.10"`. Adding xcap may add packages to `Cargo.lock` but must not change the version of any package already there (research C1 saw 37 additions and no change); if `git diff Cargo.lock` shows a changed version, stop and report.
- No test and no gate captures the screen, lists windows, asks for a permission, opens a file dialog or launches the desktop app. `XcapCapturer` and the native picker are constructed only in `crates/app/src/lib.rs::open_main_window`. Do not install system software.
- Tests keep every file under a `tempfile` directory; none touches `~/.smithagents`.
- Limits, verbatim from the design: 3 seconds hold; boxes at least 8×8 points; at most 20 boxes per screenshot; a comment at most 500 characters; thumbnails at most 320 px wide; 10 attachments; 10 MB per image; 50 MB per video; 100 MB in total.
- Gate for every task: `cargo test --workspace --features smithagents-server/test-util`, `cargo clippy --workspace --all-targets --features smithagents-server/test-util -- -D warnings`, `cargo fmt --all --check`, `cargo themegen check`, and `pgrep -fl smithagentsd` empty afterwards.
- Commits end with the trailer `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>`. The repository is public: no personal names, emails, tokens or private repository names in code, tests or docs.

## Review Focus

1. **A capture that is not a usable picture.** Permission refused, no matching window, a blank or single-color image, a pixel buffer whose length does not match its size. Expected: one sentence in the pane, nothing added to the draft, no panic, "Add files…" still works. (Tests in Tasks 1 and 4.)
2. **The app stops while the annotator is open.** Expected: after a restart the screenshot and every box saved so far are in the pane, and the annotated image shows those boxes. (Tests in Tasks 3 and 8.)
3. **A file that is not what its name says, or too big.** A text file named `.png`, an empty file, a folder, an image of 10 MB plus one byte, an eleventh attachment, two files with the same name. Expected: each refused file gets its own sentence naming it; accepted files in the same pick are still added; same-named files get different stored names. (Tests in Tasks 2 and 4.)
4. **A `draft.json` that was damaged or edited by hand.** Invalid JSON, `version` 2, a `stored_name` such as `../../x`, box numbers outside 0..1 or not finite, 50 boxes on one screenshot. Expected: the draft is set aside, or the bad entry is dropped or clamped; the app never reads or deletes a path outside the draft directory. (Tests in Task 3.)
5. **The disk refuses a write.** Expected: the pane says `Could not save the feedback draft: …`, what is on screen stays, and the previous `draft.json` is intact. (Tests in Tasks 3 and 4.)

## File Structure

```
crates/capture/                  package smithagents-capture
  src/lib.rs                     Frame, WindowTarget, Screenshot, CaptureError, Capturer, pick_window, checked
  src/xcap_capturer.rs           XcapCapturer: the only code that touches xcap and the permission calls
  src/fake.rs                    FakeCapturer, test_pattern
  tests/{matching.rs,manual.rs}
crates/app/src/feedback/         no GPUI views in here
  text.rs                        every sentence and limit above
  annotation.rs                  FracRect, Annotation, box rules (pure)
  render.rs                      annotated PNG, digit bitmap, thumbnail, PNG codec, the two PNG colors (pure)
  files.rs                       FileKind, sniffing, admission and refusals (pure)
  draft.rs                       Draft, Attachment, DraftStore
  state.rs                       Feedback entity, FeedbackOptions, FeedbackEvent, the capture sequence
crates/app/src/views/feedback/   mod.rs FeedbackPane; hold.rs HoldButton; annotator.rs Annotator
crates/app/tests/                feedback_{model,draft,state,rail,pane,annotator,flow}.rs; common/feedback.rs (shared setup)
docs/feedback.md                 the user note
```

Modified: root `Cargo.toml`, `crates/app/Cargo.toml`, `crates/app/src/{lib.rs,shell.rs}`, `crates/app/src/views/{mod.rs,rail.rs}`, `crates/app/tests/{menu_and_keys.rs,rail.rs,app_state.rs,shell_ui.rs,common/mod.rs}`.

## Consumed from the intent-tool plan (Task 1)

- `RailEntry { Settings, Intents }` with `ALL`, `label`, `element_id`, `icon`, `placement`, `as_str`, `parse` (`views/rail.rs`).
- `Shell::rail_active(&self, cx: &App) -> Option<RailEntry>`; `Shell::select_rail_entry(&mut self, entry: RailEntry, window: &mut Window, cx: &mut App)` (on screen: close; else show); `Shell::show_rail_entry(&mut self, entry: RailEntry, window: &mut Window, cx: &mut App)` (open or switch; never closes).
- `ShellState::right_entry(&self) -> RailEntry`; `ShellEvent::Reveal(RailEntry)`; `AppState.right_pane` persisted as the entry's `as_str`.
- A right pane is a `BasePanel` (its `visible` is `state.right_entry() == <its entry>`, not closable, not zoomable) + `Panel` (`title_bar` false, `inner_padding` false) + `EventEmitter<PanelEvent>` + `Focusable` + `Render`, added in `Shell::new` with one `.panel_view(..)` and one `right_panels` entry, next to a `pub const <NAME>_PANEL`. `IntentsPane` is the example.
- `crate::assets::AppIcons` already bundles `MessageSquarePlus`, `Camera`, `Paperclip`, `Trash`, `X`.
- View menu after that task: "Toggle Agents Pane", "Toggle Settings", "Toggle Intents" (`cmd-alt-i`).

---

### Task 1: The capture crate

**Files:**
- Create: `crates/capture/{Cargo.toml,src/lib.rs,src/xcap_capturer.rs,src/fake.rs,tests/matching.rs,tests/manual.rs}`
- Modify: `Cargo.toml` (workspace dependencies `xcap = "=0.9.10"`, `image = { version = "0.25", default-features = false, features = ["png", "jpeg", "gif", "webp"] }`)

`crates/capture/Cargo.toml`: dependencies `xcap.workspace = true`, `thiserror.workspace = true`; `[target.'cfg(target_os = "macos")'.dependencies] objc2-core-graphics = { version = "0.3.2", default-features = false, features = ["std", "CGWindow"] }`; dev-dependency `image.workspace = true` (the manual test writes a PNG).

**Interfaces (Produces):**

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame { pub x: f32, pub y: f32, pub width: f32, pub height: f32 }      // screen points
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowTarget { pub pid: u32, pub frame: Frame, pub scale: f32 }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screenshot { pub width: u32, pub height: u32, pub rgba: Vec<u8> }      // 4 bytes per pixel, rows top to bottom
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError { PermissionDenied, WindowNotFound, Unsupported, Failed(String) }
pub trait Capturer: Send + Sync {
    fn capture_window(&self, target: &WindowTarget) -> Result<Screenshot, CaptureError>;
}

pub const SIZE_TOLERANCE_POINTS: f32 = 2.0;
pub const EMPTY_CAPTURE: &str = "the capture came back empty";
pub const WRONG_SIZE_CAPTURE: &str = "the capture has the wrong size";
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo { pub pid: u32, pub width: u32, pub height: u32, pub minimized: bool }
/// Index of the window to capture: same pid, not minimized, width and height each within
/// SIZE_TOLERANCE_POINTS of the target's frame; the smallest |dw| + |dh| wins, the earliest on a tie.
pub fn pick_window(windows: &[WindowInfo], target: &WindowTarget) -> Option<usize>;
/// Failed(WRONG_SIZE_CAPTURE) when rgba.len() != width * height * 4; Failed(EMPTY_CAPTURE) when
/// width or height is 0 or every pixel equals the first one; otherwise the screenshot unchanged.
pub fn checked(shot: Screenshot) -> Result<Screenshot, CaptureError>;

pub struct XcapCapturer;      impl XcapCapturer { pub fn new() -> Self; }      // impl Capturer, Default
pub struct FakeCapturer;      // impl Capturer; interior Mutex
impl FakeCapturer {
    pub fn new(answer: Result<Screenshot, CaptureError>) -> Self;
    pub fn set(&self, answer: Result<Screenshot, CaptureError>);
    pub fn calls(&self) -> Vec<WindowTarget>;                                   // every target asked for, in order
}
/// A width × height picture with no two neighbouring pixels equal (never "empty").
pub fn test_pattern(width: u32, height: u32) -> Screenshot;
```

`XcapCapturer::capture_window`, in this order (research C1 to C3; keep it one short function, it has no automated test): on macOS, if `objc2_core_graphics::CGPreflightScreenCaptureAccess()` is false, call `CGRequestScreenCaptureAccess()` unless this process already did (an `AtomicBool`), and return `PermissionDenied`; list `xcap::Window::all()`; build one `WindowInfo` per window from `pid()`, `width()`, `height()`, `is_minimized()`, skipping a window whose accessor fails; `pick_window`, `None` is `WindowNotFound`; `capture_image()`; return `Screenshot { width, height, rgba: image.into_raw() }`. `xcap::XCapError::NotSupported` maps to `Unsupported`; every other xcap error to `Failed(error.to_string())`. It does not call `checked` (the app does, so the fake goes through the same check).

- [ ] **Step 1: Write the failing tests** in `tests/matching.rs` (plain `#[test]`, no xcap call). With `target(pid 42, 1200.0 × 760.0)`:
  - `picks_the_window_of_this_process_with_the_matching_size`: `[{7,1200,760}, {42,1200,760}, {42,300,200}]` → `Some(1)`.
  - `the_closest_size_wins_and_the_first_on_a_tie`: `[{42,1201,761}, {42,1200,760}, {42,1200,760}]` → `Some(1)`.
  - `two_points_off_matches_and_three_does_not`: `[{42,1202,758}]` → `Some(0)`; `[{42,1203,760}]` → `None`.
  - `a_minimized_window_never_matches`: `[{42,1200,760, minimized}]` → `None`; an empty list → `None`.
  - `an_empty_or_single_color_capture_is_refused`: `checked` of 0×0, of 4×0, and of a 4×4 picture where every pixel is `[128,128,128,255]` is `Err(Failed(EMPTY_CAPTURE.into()))`; the same 4×4 with one pixel changed is `Ok`.
  - `a_buffer_of_the_wrong_length_is_refused`: `Screenshot { width: 4, height: 4, rgba: vec![0; 60] }` → `Err(Failed(WRONG_SIZE_CAPTURE.into()))`.
  - `the_fake_records_each_target_and_answers_what_it_was_given`: two calls → `calls()` has both targets in order; after `set(Err(PermissionDenied))` the next call is that error; `checked(test_pattern(2400, 1512))` is `Ok` with `rgba.len() == 2400 * 1512 * 4`; `Failed("x".into()).to_string() == "x"`.
- [ ] **Step 2: Write `tests/manual.rs`**: one test `real_capture_of_a_running_app_window`, marked `#[ignore = "manual: captures the screen and may show the Screen Recording prompt; see docs/superpowers/plans/2026-11-26-feedback-capture.md, Manual check"]`. It reads `SMITHAGENTS_CAPTURE_PID` and `SMITHAGENTS_CAPTURE_SIZE` (`1200x760`) from the environment (panics with a how-to message when unset), calls `XcapCapturer::new().capture_window`, runs `checked`, writes the PNG into a `tempfile` directory it keeps, and prints the path and the pixel size.
- [ ] **Step 3: Run.** `cargo test -p smithagents-capture`. Expected: compile errors.
- [ ] **Step 4: Implement** the crate.
- [ ] **Step 5: Run.** `cargo test -p smithagents-capture`: PASS with 1 ignored. `git diff Cargo.lock` shows additions only. Gate.
- [ ] **Step 6: Commit.** `feat(capture): window capture behind a trait, with xcap and a fake`

---

### Task 2: Annotation model, annotated PNG and file checks (pure)

**Files:**
- Create: `crates/app/src/feedback/{mod.rs,text.rs,annotation.rs,render.rs,files.rs}`, `crates/app/tests/feedback_model.rs`
- Modify: `crates/app/src/lib.rs` (`pub mod feedback;`), `crates/app/Cargo.toml` (`image.workspace = true`, `smithagents-capture = { path = "../capture" }`)

**Interfaces:**
- Consumes: `smithagents_capture::Screenshot`.
- Produces (all under `smithagents_app::feedback`):

```rust
// text.rs: the sentences in the table above, plus
pub const HOLD: Duration = Duration::from_secs(3);
pub const MIN_BOX_POINTS: f32 = 8.0;      pub const MAX_BOXES: usize = 20;      pub const MAX_COMMENT_CHARS: usize = 500;
pub const THUMBNAIL_MAX_WIDTH: u32 = 320; pub const MAX_ATTACHMENTS: usize = 10;
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;  pub const MAX_VIDEO_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 100 * 1024 * 1024;

// annotation.rs
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FracRect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }           // fractions of the image, 0..=1
impl FracRect {
    pub fn from_corners(a: (f32, f32), b: (f32, f32)) -> FracRect;                // any two corners; clamped to 0..=1
    pub fn sanitized(self) -> Option<FracRect>;   // None when a field is not finite or w or h is 0 after clamping into the image
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation { #[serde(flatten)] pub rect: FracRect, pub comment: String }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum BoxError { TooMany, CommentTooLong }
pub fn big_enough(width_points: f32, height_points: f32) -> bool;                 // both >= MIN_BOX_POINTS
/// Trimmed text; Ok(None) when nothing is left; Err(CommentTooLong) above 500 characters (chars, not bytes).
pub fn clean_comment(text: &str) -> Result<Option<String>, BoxError>;
/// Appends and returns the new box's number (its 1-based position); Err(TooMany) at 20.
pub fn add_box(boxes: &mut Vec<Annotation>, rect: FracRect, comment: String) -> Result<usize, BoxError>;

// render.rs
pub const BOX_RGB: [u8; 3];  pub const BADGE_TEXT_RGB: [u8; 3];
pub fn stroke_unit(image_width: u32) -> u32;                                      // ((width + 600) / 1200).max(1)
pub fn annotate(original: &image::RgbaImage, boxes: &[Annotation]) -> image::RgbaImage;
pub fn thumbnail(image: &image::RgbaImage) -> image::RgbaImage;                   // never wider than 320, never enlarged
pub fn encode_png(image: &image::RgbaImage) -> Result<Vec<u8>, image::ImageError>;
pub fn decode(bytes: &[u8]) -> Result<image::RgbaImage, image::ImageError>;       // png, jpeg, gif (first frame), webp
pub fn image_from_screenshot(shot: Screenshot) -> Option<image::RgbaImage>;       // None on a wrong-length buffer

// files.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum FileKind { Png, Jpeg, Gif, Webp, Mp4 }
impl FileKind { pub fn extension(self) -> &'static str;  pub fn is_video(self) -> bool; }   // "png" "jpg" "gif" "webp" "mp4"
pub fn sniff(leading: &[u8]) -> Option<FileKind>;                                  // needs at most the first 12 bytes
pub fn kind_from_name(file_name: &str) -> Option<FileKind>;                        // extension, any case; "jpeg" and "jpg" are Jpeg
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal { Extension, Contents, ImageTooLarge, VideoTooLarge, TotalTooLarge, TooMany, Unreadable }
impl Refusal { pub fn sentence(self, file_name: &str) -> String; }
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)] pub struct Usage { pub count: usize, pub bytes: u64 }
/// Checks in this order: TooMany, Extension, Contents (no known type, or not the extension's type),
/// the size limit of its kind, TotalTooLarge.
pub fn admit(file_name: &str, leading: &[u8], size: u64, usage: Usage) -> Result<FileKind, Refusal>;
```

Drawing rules for `annotate` (research C4; the probe's `outline_rect`, `draw_number` and `draw_badge` show a working shape): with `u = stroke_unit(width)`, a box's pixel rectangle is its fractions times the image size, rounded, clipped to the image; its outline is `3 * u` pixels thick, drawn inside the rectangle in `BOX_RGB`; its badge has its top-left corner at the rectangle's top-left corner, is filled with `BOX_RGB`, and shows the box's number in `BADGE_TEXT_RGB` using a 3×5 bitmap per digit with cells of `3 * u` pixels, one cell between digits and two cells of padding on every side (so a one-digit badge at `u = 1` is 21×27 pixels). Boxes are drawn in order; anything outside the image is clipped. `BOX_RGB` and `BADGE_TEXT_RGB` are defined once here as `[u8; 3]` constants, with a comment that a PNG cannot follow the theme (pick a red near the light theme's `destructive`, and white). Sniffing: PNG `89 50 4E 47 0D 0A 1A 0A`; JPEG `FF D8 FF`; GIF `GIF87a` or `GIF89a`; WebP `RIFF` at 0 and `WEBP` at 8; MP4 `ftyp` at 4.

- [ ] **Step 1: Write the failing tests** in `tests/feedback_model.rs`:
  - `corners_in_any_order_give_the_same_clamped_box`: `from_corners((0.5, 0.5), (0.25, 0.25))` and the reverse both equal `{0.25, 0.25, 0.25, 0.25}`; `from_corners((-0.2, 0.1), (1.4, 0.3))` is `{0.0, 0.1, 1.0, 0.2}`.
  - `a_box_with_bad_numbers_is_dropped_and_an_overhanging_one_is_clamped`: `sanitized` of a rect with `x = NaN` is `None`; of `{0.9, 0.9, 0.5, 0.5}` is `{0.9, 0.9, 0.1, 0.1}` (within 1e-6); of `{0.2, 0.2, 0.0, 0.3}` is `None`.
  - `a_box_must_be_eight_points_each_way`: `big_enough(8.0, 8.0)`, `!big_enough(7.9, 40.0)`, `!big_enough(40.0, 7.9)`.
  - `comments_are_trimmed_and_capped_at_500_characters`: `"  hi \n"` → `Ok(Some("hi"))`; `"   "` → `Ok(None)`; 500 × `é` → `Ok(Some(..))`; 501 × `é` → `Err(CommentTooLong)`.
  - `the_twenty_first_box_is_refused_and_numbers_follow_position`: 20 `add_box` calls return 1..=20; the 21st is `Err(TooMany)` and the list still has 20; after `boxes.remove(0)` a new `add_box` returns 20.
  - `stroke_unit_grows_with_the_image`: 100 → 1, 1200 → 1, 1799 → 1, 1800 → 2, 2400 → 2, 5120 → 4.
  - `annotate_draws_a_three_pixel_outline_and_a_numbered_badge`: on a white 1200×800 image with one box `{0.25, 0.25, 0.5, 0.5}` (pixels x 300..900, y 200..600): `(300,400)`, `(302,400)`, `(899,400)`, `(600,200)`, `(600,599)` are `BOX_RGB`; `(303,400)`, `(600,400)`, `(900,400)`, `(299,400)` are white; badge: `(320,226)` is `BOX_RGB` and `(321,240)` is white; digit 1 (top row `010`): `(310,207)` is `BADGE_TEXT_RGB` and `(307,207)` is `BOX_RGB`. The original image is unchanged.
  - `the_outline_scales_with_the_image`: on 2400×1600 with the same box, `(605,1000)` is `BOX_RGB` and `(606,1000)` is white.
  - `box_twelve_has_a_two_digit_badge_and_edges_clip`: eleven small boxes in the top half, then the twelfth at `{0.0, 0.5, 0.2, 0.2}` on 1200×800 has `BOX_RGB` at `(32, 426)` (a one-digit badge would end at x = 20); a box `{0.95, 0.95, 0.05, 0.05}` and twenty boxes on a 10×10 image do not panic.
  - `thumbnails_are_at_most_320_wide_and_never_enlarged`: 2400×1520 → 320×203; 2400×1512 → 320×202; 200×100 → 200×100; 5000×3 → 320×1.
  - `png_round_trip_keeps_the_pixels`: `decode(&encode_png(&annotated))` equals `annotated`; the bytes start with the PNG signature.
  - `a_screenshot_with_a_short_buffer_is_none`: `image_from_screenshot` of a 4×4 with 60 bytes is `None`; of `test_pattern(8, 4)` is an 8×4 image.
  - `sniffing_reads_the_leading_bytes`: each signature above → its kind; `ftyp` at offset 4 with any first four bytes → `Mp4`; `b"hello world!"`, an empty slice and a 3-byte PNG prefix → `None`.
  - `names_map_to_kinds_in_any_case`: `a.PNG`, `a.jpg`, `a.JPEG`, `a.gif`, `a.webp`, `a.Mp4` → their kinds; `a.txt`, `png`, `a.` → `None`.
  - `admission_checks_type_then_size_then_total`: text bytes named `x.png` → `Contents`; JPEG bytes named `x.png` → `Contents`; `x.txt` → `Extension`; a PNG of `MAX_IMAGE_BYTES` is `Ok(Png)` and of `MAX_IMAGE_BYTES + 1` is `ImageTooLarge`; an MP4 of `MAX_VIDEO_BYTES` is `Ok(Mp4)` and one byte more `VideoTooLarge`; an MP4 of 40 MB with `Usage { count: 2, bytes: 70 MB }` → `TotalTooLarge`; anything with `Usage { count: 10, .. }` → `TooMany`.
  - `each_refusal_names_the_file`: `Refusal::ImageTooLarge.sentence("big.jpg") == "big.jpg was not added: an image can be at most 10 MB."`, and the other six against the table.
- [ ] **Step 2: Run.** `cargo test -p smithagents-app --test feedback_model`. Expected: compile errors.
- [ ] **Step 3: Implement** the four files.
- [ ] **Step 4: Run.** Expected: PASS. `cargo test -p themegen` passes (the two PNG constants are not flagged). Gate.
- [ ] **Step 5: Commit.** `feat(feedback): annotation model, annotated PNG drawing and file checks`

---

### Task 3: The draft store

**Files:**
- Create: `crates/app/src/feedback/draft.rs`, `crates/app/tests/feedback_draft.rs`
- Modify: `crates/app/Cargo.toml` (`uuid.workspace = true`), `crates/app/src/feedback/mod.rs`

**Interfaces:**
- Consumes: `Annotation`, `FracRect::sanitized`, `FileKind`, `Usage`, `MAX_BOXES`, `BROKEN_DRAFT_TEXT`, `missing_files_text`; `crate::atomic_file::write_private`.
- Produces:

```rust
pub const DRAFT_VERSION: u32 = 1;
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)] #[serde(transparent)]
pub struct AttachmentId(String);          impl AttachmentId { pub fn new() -> Self /* uuid v4 */; pub fn as_str(&self) -> &str; }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum AttachmentKind { Screenshot, Image, Video }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: AttachmentId, pub kind: AttachmentKind,
    pub original_name: String, pub stored_name: String, pub size: u64,
    // Screenshots only (omitted from the JSON otherwise; default when absent):
    pub width: Option<u32>, pub height: Option<u32>, pub boxes: Vec<Annotation>, pub rendered: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Draft { pub version: u32, pub main_idea: String, pub kind: Option<String>, pub attachments: Vec<Attachment> }
impl Default for Draft { /* version 1, "", None, [] */ }
impl Draft {
    pub fn is_empty(&self) -> bool;                    pub fn usage(&self) -> Usage;
    pub fn get(&self, id: &AttachmentId) -> Option<&Attachment>;
    pub fn screenshot_count(&self) -> usize;
    pub fn screenshot_number(&self, id: &AttachmentId) -> Option<usize>;   // 1-based position among screenshots
    pub fn next_stored_name(&self, kind: AttachmentKind, file: FileKind) -> String;
}
pub fn annotated_name(stored_name: &str) -> String;    // "screenshot-1.png" -> "screenshot-1.annotated.png"
pub fn thumb_name(stored_name: &str) -> String;        // "image-2.jpg" -> "image-2.thumb.png"

#[derive(Debug, Clone)] pub struct DraftStore { /* home */ }
pub struct Loaded { pub draft: Draft, pub notices: Vec<String> }
impl DraftStore {
    pub fn new(home: &Path) -> Self;
    pub fn dir(&self) -> PathBuf;                                   // <home>/feedback/draft
    pub fn path_of(&self, stored_name: &str) -> PathBuf;
    pub fn load(&self) -> Loaded;                                   // never fails
    pub fn save(&self, draft: &Draft) -> io::Result<()>;            // draft.json, atomic
    pub fn write_file(&self, stored_name: &str, bytes: &[u8]) -> io::Result<()>;   // atomic
    pub fn remove_file(&self, stored_name: &str);                   // also its annotated and thumb files; errors logged
    pub fn clear(&self) -> io::Result<()>;                          // removes the draft directory
}
```

`draft.json` (version 1; `main_idea` and `kind` stay `""` and `null` in this step):

```json
{ "version": 1, "main_idea": "", "kind": null, "attachments": [
  { "id": "…", "kind": "screenshot", "original_name": "screenshot-1.png", "stored_name": "screenshot-1.png", "size": 48213,
    "width": 2400, "height": 1512, "rendered": true, "boxes": [ { "x": 0.25, "y": 0.25, "w": 0.25, "h": 0.25, "comment": "the toolbar" } ] },
  { "id": "…", "kind": "video", "original_name": "demo.mp4", "stored_name": "clip-1.mp4", "size": 1048576 } ] }
```

`load` rules, in order: no `draft.json` → an empty draft, no notice. Text that is not a `Draft`, or `version != 1` → rename the whole directory to `<home>/feedback/draft.broken-<unix seconds>` (append `-2`, `-3` when taken; if the rename fails, leave it and log), answer an empty draft with `BROKEN_DRAFT_TEXT`. Otherwise, per attachment: drop it when `stored_name` is not a plain file name (empty, contains `/` or `\`, starts with `.`, or differs from its own `Path::file_name`) or when that file is not a regular file in the directory; for a screenshot, replace each box by `sanitized()` (dropping `None`), keep the first 20, and set `rendered = false` when anything changed or the annotated file is missing. One `missing_files_text(n)` notice for the dropped ones, and the cleaned draft is saved back. Files in the directory that no attachment names are left alone.

- [ ] **Step 1: Write the failing tests** in `tests/feedback_draft.rs`, each with a `tempfile::tempdir()` home:
  - `the_draft_lives_under_feedback_draft_in_the_home`: `dir() == home.join("feedback/draft")`; after `save`, `draft.json` exists there; on unix the directory mode is `0o700` and the file `0o600`.
  - `version_one_json_has_the_documented_shape`: a draft with one screenshot (one box) and one video serializes to exactly the keys shown above (compare as `serde_json::Value`); the video object has no `width`, `height`, `boxes` or `rendered` key; `main_idea` is `""` and `kind` is `null`.
  - `save_then_load_is_the_same_draft`: with the attachment files written, `load()` returns the saved draft and no notice; with no `draft.json` at all it returns the empty draft and no notice.
  - `a_broken_draft_is_set_aside_and_named`: `draft.json` holding `{ not json` plus a file `screenshot-1.png` → `load()` is the empty draft with `notices == [BROKEN_DRAFT_TEXT]`; exactly one sibling directory starts with `draft.broken-` and contains both files; `dir()` no longer has `draft.json`. The same for a valid file with `"version": 2`. A second broken draft in the same second lands in a different directory.
  - `attachments_whose_files_are_gone_are_dropped_with_a_sentence`: three attachments, one file deleted → two remain in order, `notices == ["1 attachment was removed because its file is missing."]`, and `draft.json` on disk now has two; with two deleted the sentence is `2 attachments were removed because their files are missing.`
  - `a_stored_name_that_leaves_the_directory_is_dropped_and_never_touched`: write `outside.png` in the home and a draft naming `../../outside.png`, `/etc/hosts`, `.hidden.png` and `sub/x.png` → all four dropped; `outside.png` still exists after `load()` and after `clear()`.
  - `bad_boxes_are_cleaned_and_the_screenshot_is_redrawn`: a screenshot with a `NaN` box, an overhanging box and 50 valid boxes → at most 20 boxes, all within 0..=1, `rendered == false`.
  - `a_screenshot_without_its_annotated_file_is_marked_for_redrawing`: `rendered: true` in the JSON, no `screenshot-1.annotated.png` → `rendered == false` after `load()`.
  - `names_count_up_per_prefix_and_never_repeat`: an empty draft gives `screenshot-1.png`, `image-1.jpg`, `clip-1.mp4`; a draft holding `screenshot-1.png` and `screenshot-3.png` gives `screenshot-4.png`; `annotated_name` and `thumb_name` as in the comments above.
  - `screenshot_numbers_follow_position`: screenshots A, image, screenshot B → `screenshot_number(B) == Some(2)`, of the image `None`, `screenshot_count() == 2`; `usage()` sums `size` and counts three.
  - `a_failed_save_leaves_the_old_draft_intact`: save draft A; make the directory read-only (unix `0o500`; restore the mode at the end of the test); saving B is `Err`; `draft.json` still parses as A and no `.tmp` file is left.
- [ ] **Step 2: Run.** `cargo test -p smithagents-app --test feedback_draft`. Expected: compile errors.
- [ ] **Step 3: Implement** `draft.rs`.
- [ ] **Step 4: Run.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(feedback): the draft on disk, with recovery from a broken or incomplete one`

---

### Task 4: The `Feedback` state and the capture sequence

**Files:**
- Create: `crates/app/src/feedback/state.rs`, `crates/app/tests/feedback_state.rs`, `crates/app/tests/common/feedback.rs`
- Modify: `crates/app/src/shell.rs` (`ShellOptions.feedback`, the entity, `Shell::feedback()`), `crates/app/src/lib.rs` (the new field as `None` for now), `crates/app/tests/common/mod.rs`

**Interfaces:**
- Consumes: Tasks 1 to 3.
- Produces:

```rust
/// Asks the user for files. Ok(None) is "cancelled"; Err carries the reason the picker failed.
pub type FilePicker = Rc<dyn Fn(&mut App) -> LocalBoxFuture<'static, Result<Option<Vec<PathBuf>>, String>>>;
/// Runs the callback at the start of the next frame (research B3). The default is `window.on_next_frame`.
pub type FrameHop = Rc<dyn Fn(&mut Window, Box<dyn FnOnce(&mut Window, &mut App)>)>;

#[derive(Default)]
pub struct FeedbackOptions {
    pub store: Option<DraftStore>,               // None: the draft is kept in memory only
    pub capturer: Option<Arc<dyn Capturer>>,     // None: every capture answers CaptureError::Unsupported
    pub picker: Option<FilePicker>,              // None: answers Ok(None)
    pub frame_hop: Option<FrameHop>,             // None: window.on_next_frame
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedbackEvent { RevealPane, OpenAnnotator(AttachmentId), CloseAnnotator }

pub struct Feedback;   // impl EventEmitter<FeedbackEvent>
impl Feedback {
    pub fn new(options: FeedbackOptions, cx: &mut Context<Self>) -> Self;       // loads the draft; its notices become notices()
    pub fn draft(&self) -> &Draft;
    pub fn store(&self) -> Option<&DraftStore>;
    pub fn notices(&self) -> &[String];                                         // newest last; replaced by the next action that reports
    pub fn capturing(&self) -> bool;
    pub fn busy(&self) -> bool;                                                 // image work or a file copy is running
    pub fn annotating(&self) -> Option<&AttachmentId>;
    pub fn capture(&mut self, window: &mut Window, cx: &mut Context<Self>);
    pub fn add_files(&mut self, window: &mut Window, cx: &mut Context<Self>);
    pub fn remove(&mut self, id: &AttachmentId, cx: &mut Context<Self>);
    pub fn discard_all(&mut self, cx: &mut Context<Self>);
    pub fn open_annotator(&mut self, id: &AttachmentId, cx: &mut Context<Self>);            // screenshots only
    pub fn set_boxes(&mut self, id: &AttachmentId, boxes: Vec<Annotation>, cx: &mut Context<Self>);   // saves; rendered = false
    pub fn finish_annotating(&mut self, cx: &mut Context<Self>);                // Done
    pub fn discard_annotated(&mut self, cx: &mut Context<Self>);                // removes the screenshot being annotated
    pub fn thumbnail(&self, id: &AttachmentId) -> Option<Arc<RenderImage>>;
    pub fn load_image(&self, id: &AttachmentId, cx: &mut Context<Self>) -> Task<Option<Arc<RenderImage>>>;  // the stored original, decoded off the UI thread
    pub fn take_retired_images(&mut self) -> Vec<Arc<RenderImage>>;             // thumbnails no longer shown; the pane drops them
}
// shell.rs
pub struct ShellOptions { /* … */ pub feedback: Option<FeedbackOptions> }       // None = FeedbackOptions::default()
impl Shell { pub fn feedback(&self) -> &Entity<Feedback>; }
// tests/common/feedback.rs
pub struct FeedbackSetup { pub home: tempfile::TempDir, pub capturer: Arc<FakeCapturer>, pub picked: Rc<RefCell<Result<Option<Vec<PathBuf>>, String>>>, pub hops: Rc<ManualHops> }
impl FeedbackSetup { pub fn new() -> Self; pub fn options(&self) -> ShellOptions; pub fn store(&self) -> DraftStore; }
impl ManualHops { pub fn pending(&self) -> usize; pub fn run_next(&self, window: &mut Window, cx: &mut App); }   // one queued callback
```

Behaviour:
- `capture`: does nothing while `capturing()` or `annotating()`; with 10 attachments sets `notices = [DRAFT_FULL_TEXT]` and emits `RevealPane` without calling the capturer. Otherwise it sets `capturing`, notifies, and hops **two** frames through the `FrameHop` (hop, and inside it hop again). After the second hop it builds `WindowTarget { pid: std::process::id(), frame: window.bounds() as a Frame, scale: window.scale_factor() }` and runs, on `cx.background_spawn`: `capturer.capture_window(&target)`, `checked`, `image_from_screenshot`, `encode_png`. Back on the UI thread: on success, when the PNG is larger than `MAX_IMAGE_BYTES` it is refused with `Refusal::ImageTooLarge.sentence("The screenshot")`; otherwise write `next_stored_name(Screenshot, Png)`, push the attachment (`original_name` equal to the stored name, `width`, `height`, no boxes, `rendered: false`), save, emit `OpenAnnotator(id)`. On error: `PermissionDenied` → `PERMISSION_TEXT`, `WindowNotFound` → `WINDOW_NOT_FOUND_TEXT`, `Unsupported` → `UNSUPPORTED_TEXT`, `Failed(m)` → `capture_failed_text(m)`; emit `RevealPane`. Either way `capturing` ends.
- `add_files`: awaits the picker; for each path in order, off the UI thread: file name (lossy), metadata (a folder or an error is `Unreadable`), the first 12 bytes, `admit` against the usage **including files accepted earlier in the same pick**, then read and `write_file` under `next_stored_name`; an image also gets its thumbnail file (a decode failure only means no thumbnail). One notice per refused file; `Err(e)` from the picker is `picker_failed_text(e)`.
- Every change saves `draft.json`; a failed save or write adds `save_failed_text(e)` and keeps the in-memory draft.
- `finish_annotating`: when the screenshot is not `rendered`, decode the original, `annotate`, write the annotated file and the thumbnail (off the UI thread), set `rendered = true`, save; then emit `CloseAnnotator`. At start, every screenshot that is not `rendered` goes through the same redraw, and every thumbnail file is decoded into `thumbnail()`.
- `remove`, `discard_all` and `discard_annotated` delete the files (`remove_file` / `clear`), save, and move the thumbnails they held to the retired list.

- [ ] **Step 1: Verify first** (a throwaway test, not committed): in a `#[gpui_kit::test]`, call `window.on_next_frame(..)` and check whether `window.render_frame(cx)` plus `cx.run_until_parked()` runs it on the test platform. Record the answer in the commit message. The plan does not depend on it: tests always inject `ManualHops`, and production uses `on_next_frame`.
- [ ] **Step 2: Write `tests/common/feedback.rs`** and the failing tests in `tests/feedback_state.rs`. Each opens the shell with `open_shell_options(cx, setup.options(), (1200., 800.))`, calls the `Feedback` entity inside `cx.update_window`, and runs `cx.run_until_parked()` after each step. The fake capturer starts with `Ok(test_pattern(2400, 1512))`.
  - `capture_waits_two_frames_then_asks_the_capturer_once`: after `capture`: `capturing()`, `hops.pending() == 1`, `calls()` empty. After `run_next`: `pending() == 1`, `calls()` still empty. After the second `run_next` and parking: `calls().len() == 1` with `pid == std::process::id()`, `frame.width == 1200.0`, `frame.height == 800.0` and `scale` equal to the window's; `!capturing()`; the draft has one attachment `{ kind: Screenshot, stored_name: "screenshot-1.png", width: Some(2400), height: Some(1512), boxes: [], rendered: false }`; the file exists in `setup.store().dir()` and starts with the PNG signature; `setup.store().load().draft` equals it; the events seen are exactly `[OpenAnnotator(id)]`.
  - `each_capture_error_has_its_sentence_and_adds_nothing`: for `PermissionDenied`, `WindowNotFound`, `Unsupported`, `Failed("boom".into())` → `notices()` is `[PERMISSION_TEXT]`, `[WINDOW_NOT_FOUND_TEXT]`, `[UNSUPPORTED_TEXT]`, `["Could not capture the window: boom."]`; the draft is empty; the directory has no `screenshot-` file; the event is `RevealPane`.
  - `a_single_color_capture_is_reported_as_empty`: a 64×64 screenshot of one color → `["Could not capture the window: the capture came back empty."]`.
  - `a_capture_with_a_short_buffer_is_reported_not_a_panic`: `Screenshot { width: 64, height: 64, rgba: vec![7; 100] }` → `["Could not capture the window: the capture has the wrong size."]`.
  - `a_second_capture_while_one_runs_is_ignored`: two `capture` calls, then both hops → `calls().len() == 1`.
  - `the_eleventh_attachment_is_refused_before_capturing`: ten added files, then `capture` → `hops.pending() == 0`, `calls()` empty, `notices() == [DRAFT_FULL_TEXT]`.
  - `picked_files_are_checked_one_by_one`: the picker answers `good.png` (a real 4×4 PNG), `notes.txt`, `fake.png` (text), `big.jpg` (JPEG signature, grown to `MAX_IMAGE_BYTES + 1` bytes with `File::set_len`), `clip.mp4` (`ftyp` at 4), `empty.gif` (0 bytes), and a folder named `folder.png` → the draft has `image-1.png` (kind `Image`, `original_name` "good.png") then `clip-1.mp4` (kind `Video`); `notices()` equals, in order, the `Extension` sentence for `notes.txt`, `Contents` for `fake.png`, `ImageTooLarge` for `big.jpg`, `Contents` for `empty.gif`, `Unreadable` for `folder.png`; `image-1.thumb.png` exists and `thumbnail(id)` is `Some`; the video has no thumbnail.
  - `two_files_with_the_same_name_get_different_stored_names`: `a/photo.jpg` and `b/photo.jpg` → `image-1.jpg`, `image-2.jpg`, both `original_name` "photo.jpg".
  - `the_total_limit_counts_files_from_the_same_pick`: three MP4 files of 40 MB each → two accepted, the third refused with the `TotalTooLarge` sentence.
  - `a_cancelled_or_failed_picker`: `Ok(None)` → nothing changes and no notice; `Err("no display".into())` → `["Could not open the file picker: no display."]`.
  - `boxes_are_saved_as_they_change_and_done_draws_the_annotated_image`: capture, `set_boxes(id, [ {0.25,0.25,0.25,0.25,"the toolbar"} ])` → `load()` from disk has the box and `rendered == false`, and no annotated file exists; `finish_annotating` → `screenshot-1.annotated.png` decodes to 2400×1512 with `BOX_RGB` at `(600, 378)`; `screenshot-1.thumb.png` is 320×202; `rendered == true` on disk; the event is `CloseAnnotator`; `thumbnail(id)` is `Some`.
  - `removing_and_discarding_delete_the_files`: `remove(id)` deletes the stored, annotated and thumb files and rewrites `draft.json`; `discard_all` leaves no draft directory; `take_retired_images()` returns the thumbnails once, then is empty.
  - `discarding_the_annotated_screenshot_removes_it`: capture, `discard_annotated` → the draft is empty, the file is gone, the event is `CloseAnnotator`.
  - `a_failed_save_is_said_and_the_draft_stays_on_screen`: make `<home>/feedback` a regular file before the first capture → `notices()` has one line starting with `Could not save the feedback draft: ` and ending with `.`; nothing panics.
  - `a_restart_restores_the_draft_and_redraws_what_was_not_drawn`: capture, `set_boxes` with one box, then build a second shell on the same home without pressing Done → its draft has the screenshot and the box, and after parking `screenshot-1.annotated.png` exists with `BOX_RGB` at `(600, 378)`.
- [ ] **Step 3: Run.** `cargo test -p smithagents-app --test feedback_state`. Expected: compile errors.
- [ ] **Step 4: Implement** `state.rs`, the `ShellOptions` field and `Shell::feedback()`.
- [ ] **Step 5: Run.** Expected: PASS; every earlier app test still passes. Gate.
- [ ] **Step 6: Commit.** `feat(feedback): the draft state and the capture sequence against the Capturer trait`

---

### Task 5: The rail entry and the 3-second hold

**Files:**
- Create: `crates/app/src/views/feedback/{mod.rs,hold.rs}`, `crates/app/tests/feedback_rail.rs`
- Modify: `crates/app/src/views/{mod.rs,rail.rs}`, `crates/app/src/shell.rs`, `crates/app/tests/{menu_and_keys.rs,rail.rs,app_state.rs,shell_ui.rs}`

**Interfaces:**
- Consumes: Task 4; the intent-tool plan's rail (see "Consumed from the intent-tool plan").
- Produces:

```rust
// rail.rs
RailEntry::Feedback          // ALL = [Settings, Feedback, Intents]; label "Feedback"; element id "rail-feedback";
                             // icon IconName::MessageSquarePlus; placement DockPlacement::Right; as_str "feedback"
/// As after the intent-tool plan, plus `feedback`: drawn in place of the plain button for RailEntry::Feedback.
pub fn rail(active: Option<RailEntry>, on_select: impl Fn(RailEntry, &mut Window, &mut App) + Clone + 'static,
            feedback: AnyElement, native: Option<&NativeTheme>, cx: &App) -> impl IntoElement;
// shell.rs
pub const FEEDBACK_PANEL: &str = "Feedback";       // persisted panel name; never change it once shipped
ToggleFeedbackPane                                  // action; "cmd-alt-f" in KEY_CONTEXT; menu item "Toggle Feedback"
impl Shell { pub fn hold_button(&self) -> &Entity<HoldButton>; }
// views/feedback/hold.rs
pub const HOLD_RING_ID: &str = "rail-feedback-ring";
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum HoldEvent { Clicked, Completed }
pub struct HoldButton;       // impl Render, EventEmitter<HoldEvent>
impl HoldButton {
    pub fn new(cx: &mut Context<Self>) -> Self;
    pub fn set_selected(&mut self, selected: bool, cx: &mut Context<Self>);
    pub fn progress(&self, cx: &App) -> f32;       // 0.0 when not holding; elapsed / 3 s, at most 1.0
}
// views/feedback/mod.rs
pub const FEEDBACK_TITLE_ID: &str = "feedback-title";
pub struct FeedbackPane;     // the same five impls as IntentsPane; panel_name FEEDBACK_PANEL; visible when right_entry() == Feedback
impl FeedbackPane { pub fn new(state: Entity<ShellState>, feedback: Entity<Feedback>, window: &mut Window, cx: &mut Context<Self>) -> Self; }
```

`HoldButton` renders the same `Button` the rail draws for other entries (id `RailEntry::Feedback.element_id()`, ghost, small, tooltip and accessibility label "Feedback", `selected`), plus, only while holding, a `ProgressCircle::new(HOLD_RING_ID).value(progress * 100.)` laid over the icon. Rules (research B3): left mouse-down starts a 3-second timer task (`cx.background_executor().timer(HOLD)`) and a ~33 ms repaint loop, and records `cx.background_executor().now()`; progress comes from elapsed time, never from an animation. Mouse-up on the button, mouse-up outside, hover leaving (`on_hover(false)`) and a mouse move without the left button all cancel by dropping both tasks. When the timer ends: stop holding (the ring is no longer rendered), notify, emit `Completed`, and remember to swallow the next `on_click`. `on_click` emits `Clicked` unless it is being swallowed. A new mouse-down clears the swallow flag.

Shell: build `HoldButton` and `FeedbackPane` (a pane with only its heading for now: the text and accessibility label "FEEDBACK", role `Heading`, id `FEEDBACK_TITLE_ID`, styled like `SETTINGS_TITLE_ID`'s), add the pane to the right dock by the intent-tool plan's five steps, and pass the button to `rail`. `HoldEvent::Clicked` → `select_rail_entry(RailEntry::Feedback)`; `HoldEvent::Completed` → `feedback.capture(window, cx)`; `FeedbackEvent::RevealPane` → `show_rail_entry(RailEntry::Feedback)`; `ToggleFeedbackPane` → `select_rail_entry(RailEntry::Feedback)`. Keep `set_selected` in step with `rail_active`.

- [ ] **Step 1: Write the failing tests** in `tests/feedback_rail.rs` (shell opened from `FeedbackSetup`; pointer events through `TestEventExt`: `pointer_move`, `pointer_down`, `pointer_up`; time through `cx.executor().advance_clock`):
  - `feedback_sits_directly_under_settings`: `RailEntry::ALL == [Settings, Feedback, Intents]`; the `rail-feedback` button's top is at or below the `rail-settings` button's bottom and its bottom at or above the `rail-intents` button's top; its accessibility label is "Feedback"; `find_all_by_role(Role::Tab)` is empty with the pane open.
  - `a_click_opens_the_feedback_pane_and_a_second_click_closes_it`: after one click `rail_active == Some(RailEntry::Feedback)`, the right dock is open and `FEEDBACK_TITLE_ID` has the label "FEEDBACK"; after the second the dock is closed. With Settings showing, a click on Feedback switches to it and the Settings heading is no longer found.
  - `the_ring_fills_over_three_seconds`: press; at 1500 ms `progress` is within 0.02 of 0.5 and `HOLD_RING_ID` is found; before the press and after a release it is not found and `progress == 0.0`.
  - `a_release_just_before_three_seconds_is_a_click`: press, advance 2990 ms, release → the Feedback pane is open, `calls()` is empty, `hops.pending() == 0`; advancing another 3 s changes nothing.
  - `holding_three_seconds_captures_once_and_the_release_is_not_a_click`: press, advance 2990 ms (`hops.pending() == 0`), advance 20 ms → `hops.pending() == 1` and `HOLD_RING_ID` is not found; at each of the two hops the ring is still not found and, before the second, `calls()` is empty; after it `calls().len() == 1`; then release → `rail_active` is what it was before the press (the pane did not toggle).
  - `leaving_the_button_cancels_and_is_not_a_click`: press, `pointer_move` to the window's center with the left button pressed, advance 6 s → no hop, no call, ring gone; releasing there leaves the dock as it was.
  - `a_failed_capture_after_a_hold_opens_the_pane`: the fake answers `PermissionDenied`; hold and run both hops → `rail_active == Some(RailEntry::Feedback)`.
  - `the_shortcut_toggles_the_pane`: `cmd-alt-f` opens it, again closes it.
  - `the_feedback_pane_is_remembered`: with an `AppStateStore` in the tempdir, select Feedback, reopen → `rail_active == Some(RailEntry::Feedback)`.
  - `reveal_settings_still_wins_over_feedback`: with Feedback showing, `request_settings_focus(SettingsSection::Repositories)` → `rail_active == Some(RailEntry::Settings)`.
- [ ] **Step 2: Update the pinned tests.** `tests/menu_and_keys.rs`: the View menu is exactly "Toggle Agents Pane", "Toggle Settings", "Toggle Feedback", "Toggle Intents", and `ToggleFeedbackPane` is bound to `cmd-alt-f`. `tests/rail.rs`: `the_rail_lists_settings_then_intents` expects `[Settings, Feedback, Intents]`. `tests/app_state.rs`: `right_pane` round-trips as `"feedback"`. `tests/shell_ui.rs`: adjust only assertions that pin rail positions.
- [ ] **Step 3: Run.** `cargo test -p smithagents-app --test feedback_rail --test menu_and_keys --test rail --test app_state`. Expected: compile errors.
- [ ] **Step 4: Implement.**
- [ ] **Step 5: Run.** Expected: PASS, with `shell_ui`, `settings_pane` and `agents_pane` green. Gate.
- [ ] **Step 6: Commit.** `feat(feedback): Feedback rail entry with a 3-second press-and-hold`

---

### Task 6: The Feedback pane

**Files:**
- Modify: `crates/app/src/views/feedback/mod.rs`
- Create: `crates/app/tests/feedback_pane.rs`

**Interfaces:**
- Consumes: Tasks 4 and 5.
- Produces (element ids, all in `views::feedback`):

```rust
pub const FEEDBACK_HELP_ID: &str = "feedback-help";               // header button, aria_expanded
pub const FEEDBACK_HELP_TEXT_ID: &str = "feedback-help-text";
pub const FEEDBACK_EMPTY_ID: &str = "feedback-empty";
pub const FEEDBACK_NOTICES_ID: &str = "feedback-notices";         // Role::Status; label = the notices joined by "\n"
pub const FEEDBACK_LIST_ID: &str = "feedback-attachments";        // Role::List
pub const FEEDBACK_ADD_FILES_ID: &str = "feedback-add-files";     // label "Add files…"
pub const FEEDBACK_CAPTURE_ID: &str = "feedback-capture";         // label "Capture"
pub const FEEDBACK_DISCARD_ID: &str = "feedback-discard";         // label "Discard"
pub const FEEDBACK_SUBMIT_ID: &str = "feedback-submit";           // label "Submit…"
pub const DISCARD_DIALOG_TITLE_ID: &str = "feedback-discard-title";
pub const DISCARD_CANCEL_ID: &str = "feedback-discard-cancel";
pub const DISCARD_CONFIRM_ID: &str = "feedback-discard-confirm";
pub fn card_id(id: &AttachmentId) -> ElementId;                    // "feedback-card-<id>"; the card's accessibility label is its title
pub fn card_remove_id(id: &AttachmentId) -> ElementId;             // "feedback-card-remove-<id>"; label "Remove"
```

Layout, top to bottom (the Settings pane's patterns: a `Paint` from `region_paint(native, Region::RightPane)` and `token_paint`, a stored `ScrollHandle`, a hand-made collapsible header as in `SettingsPane::section`): the heading; the help row, collapsed at start and not remembered (three `HELP_LINES` when expanded); the notices block in the `destructive` token when there are notices; the scrolling list of cards or `EMPTY_TEXT`; a button row. A card shows the thumbnail (`img(Arc<RenderImage>)` at most 320 points wide; the `Paperclip` icon when there is none), the title (`card_title(n, k)` for a screenshot, `original_name` otherwise), one line per comment with its number in a badge drawn with the `destructive` token, and a remove button (`Trash` icon). Clicking a screenshot card (not its remove button) calls `open_annotator`. "Capture" (`Camera` icon) calls `Feedback::capture` and is disabled while `capturing()`. "Add files…" calls `add_files`. "Discard" is disabled on an empty draft; otherwise it opens `window.open_alert_dialog` (the pattern in `views/agents/close.rs`, Cancel focused first) and calls `discard_all` only on Discard. "Submit…" is always `.disabled(true)` with `.tooltip(SUBMIT_TOOLTIP)`. The pane observes `Feedback` with `cx.observe_in(.., window, ..)` and there drops every `take_retired_images()` entry with `cx.drop_image(image, Some(window))`.

- [ ] **Step 1: Write the failing tests** in `tests/feedback_pane.rs` (pane opened by clicking the rail entry; helpers `frame`, `with_window`, `click` as in `tests/settings_pane.rs`):
  - `an_empty_pane_says_how_to_start`: `FEEDBACK_TITLE_ID` label "FEEDBACK"; `FEEDBACK_EMPTY_ID` label equals `EMPTY_TEXT`; `FEEDBACK_LIST_ID` and `FEEDBACK_NOTICES_ID` are not found; the four buttons have the labels "Add files…", "Capture", "Discard", "Submit…".
  - `the_help_row_starts_collapsed_and_shows_three_sentences`: `expanded() == Some(false)` and no `FEEDBACK_HELP_TEXT_ID`; after a click `Some(true)` and the three `HELP_LINES` are found by label, in order; its label is `HELP_TITLE`.
  - `submit_is_disabled_and_says_why`: `disabled() == Some(true)`, `description() == Some("Submitting arrives with the next update")`; clicking it changes nothing.
  - `capture_takes_the_pane_as_it_is_without_the_hold`: click `FEEDBACK_CAPTURE_ID`, run both hops → `calls().len() == 1`, the dock was open the whole time, and while `capturing()` the button's `disabled()` is `Some(true)`.
  - `a_refused_permission_shows_its_sentence_and_add_files_still_works`: the fake answers `PermissionDenied`; after Capture `FEEDBACK_NOTICES_ID` has role `Status` and label `PERMISSION_TEXT`; then "Add files…" with `good.png` adds a card and the notices are gone.
  - `add_files_lists_cards_and_names_each_refused_file`: the picker answers `good.png`, `notes.txt`, `clip.mp4` → two cards in order with labels "good.png" and "clip.mp4"; the notices label is the `Extension` sentence for `notes.txt`.
  - `a_screenshot_card_counts_its_comments`: capture and Done with no box → a card labeled "Screenshot 1 · 0 comments"; after `set_boxes` with one box and Done → "Screenshot 1 · 1 comment" and the text "the toolbar" is found inside the card; a second screenshot with three boxes → "Screenshot 2 · 3 comments".
  - `comment_badges_use_the_destructive_token`: the quad painted at the first comment badge's center equals `token_background(&dark_native(), "destructive")` (`painted_at`, `tests/common/ui.rs`).
  - `removing_a_card_renumbers_the_rest`: two screenshots; click the first card's remove button → one card, labeled "Screenshot 1 · 0 comments"; its file `screenshot-2.png` still exists and `screenshot-1.png` does not.
  - `clicking_a_screenshot_card_reopens_the_annotator`: the `Feedback` entity reports `annotating() == Some(id)`; clicking a file card does nothing.
  - `discard_asks_first_when_the_draft_is_not_empty`: with an empty draft "Discard" is disabled. With two attachments: click → `DISCARD_DIALOG_TITLE_ID` label "Discard this feedback?"; `DISCARD_CANCEL_ID` keeps both cards; click again and `DISCARD_CONFIRM_ID` → `FEEDBACK_EMPTY_ID` is back and the draft directory is gone.
  - `a_broken_draft_is_announced_once`: start the shell on a home whose `draft.json` is `{` → the notices label is `BROKEN_DRAFT_TEXT` and the pane is otherwise empty.
- [ ] **Step 2: Run.** `cargo test -p smithagents-app --test feedback_pane`. Expected: FAIL (ids not found).
- [ ] **Step 3: Implement** the pane.
- [ ] **Step 4: Run.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(feedback): the Feedback pane with cards, Add files, Capture and Discard`

---

### Task 7: The annotator overlay

**Files:**
- Create: `crates/app/src/views/feedback/annotator.rs`, `crates/app/tests/feedback_annotator.rs`
- Modify: `crates/app/src/shell.rs` (owns `Option<Entity<Annotator>>`, draws the overlay), `crates/app/src/views/feedback/mod.rs`

**Interfaces:**
- Consumes: Tasks 2, 4, 5.
- Produces:

```rust
pub const ANNOTATOR_ID: &str = "feedback-annotator";
pub const ANNOTATOR_TITLE_ID: &str = "feedback-annotator-title";     // label = annotator_title(n, m), or a limit sentence (below)
pub const ANNOTATOR_DONE_ID: &str = "feedback-annotator-done";       // label "Done"
pub const ANNOTATOR_DISCARD_ID: &str = "feedback-annotator-discard"; // label "Discard"
pub const ANNOTATOR_COMMENT_ID: &str = "feedback-annotator-comment"; // the input; placeholder COMMENT_HINT
pub const ANNOTATOR_COMMENT_ERROR_ID: &str = "feedback-annotator-comment-error";   // Role::Status
pub const ANNOTATOR_DELETE_ID: &str = "feedback-annotator-delete";   // label "Delete"
pub const ANNOTATOR_KEEP_ID: &str = "feedback-annotator-keep";       // confirm dialog
pub const ANNOTATOR_CONFIRM_DISCARD_ID: &str = "feedback-annotator-confirm-discard";
pub const ANNOTATOR_BAR_HEIGHT: f32 = 44.;
pub fn badge_id(number: usize) -> ElementId;                          // "feedback-badge-<n>"; label = the number
pub struct Annotator;         // impl Render, Focusable
impl Annotator {
    pub fn new(feedback: Entity<Feedback>, id: AttachmentId, window: &mut Window, cx: &mut Context<Self>) -> Self;
    pub fn image(&self) -> Option<Arc<RenderImage>>;                  // None until the stored image is decoded
    pub fn boxes(&self) -> &[Annotation];
    pub fn release(&mut self, window: &mut Window, cx: &mut Context<Self>);   // drops the image from the atlas
}
impl Shell { pub fn annotator(&self) -> Option<&Entity<Annotator>>; }
```

Shell: on `FeedbackEvent::OpenAnnotator(id)` create the `Annotator` and focus it; on `CloseAnnotator` call `release`, drop the entity and focus the shell. The shell's root becomes `.relative()` and, while an annotator exists, its last child is `div().absolute().inset_0().occlude().child(annotator)` (research B6), so it covers the title bar, the panes, the dock and the rail.

Annotator (research B1, B2, B4; the probe's `Annotator` is the working shape): a bar of `ANNOTATOR_BAR_HEIGHT` with the title, "Done" and "Discard"; below it the image with `ObjectFit::Contain`, a canvas that records the area's bounds and paints each box (a faint fill and an outline in the `destructive` token), and badges placed with `relative()` fractions inside a container sized to the drawn image (badge fill `destructive`, number `background`). The image comes from `Feedback::load_image`. Pointer rules: a left press on the image area starts a drag unless the comment panel is open (then the press is ignored); the drag ends on mouse-up, mouse-up outside, or a move without the left button; both corners are converted with `ObjectFit::Contain.get_bounds` and `FracRect::from_corners` (so they clamp to the image); a box whose drawn size fails `big_enough` is ignored; with 20 boxes the drag is ignored and the title shows `TOO_MANY_BOXES_TEXT` until the next action. A finished drag opens the comment panel anchored under the box (`deferred(anchored().position(..).snap_to_window()..)`), focused, with the hint and a "Delete" button. Enter: `clean_comment`; `Ok(Some(text))` saves (a new box is appended; an edit replaces the comment) through `Feedback::set_boxes`; `Ok(None)` drops the box (also when editing); `Err(CommentTooLong)` keeps the panel open and shows `COMMENT_TOO_LONG_TEXT`. Escape (the `gpui_kit::component::input::Escape` action on the annotator's root) drops a new box or cancels an edit, leaving the old comment; with no panel open it does nothing. Clicking a badge opens the panel on that box with its comment; "Delete" removes it and the later boxes renumber. "Done" calls `finish_annotating`. "Discard" calls `discard_annotated` at once when there are no boxes; otherwise it asks (`open_alert_dialog`, "Keep" focused first).

**Verify first (comment input):** the decision asks for a multi-line input. Try `TextareaState::new(window, cx).auto_grow(1, 4)` with `Textarea::new(&state)` and check in a test that plain Enter emits `InputEvent::PressEnter` without inserting a newline, shift-Enter inserts one, and Escape still reaches the parent's `Escape` handler (`submit_on_enter` exists but is `#[doc(hidden)]`; research B4). If any of the three fails, use the single-line `InputState` + `Input` the probe proved, and note the gap in the commit message and under Follow-ups.

- [ ] **Step 1: Write the failing tests** in `tests/feedback_annotator.rs`. Window 1200×800; the fake's 2400×1512 picture fills the image area exactly (x 0..1200, y 44..800). `open(cx)` = hold-free helper: `Feedback::capture`, both hops, park, frame.
  - `the_annotator_covers_the_whole_window`: `ANNOTATOR_ID` bounds are 1200×800 at the origin; the title label is "Screenshot 1 of 1 · Drag a box over what you mean"; a click where the rail's Settings button is does not open Settings; `annotator.image()` is `Some` with size 2400×1512.
  - `dragging_draws_a_box_in_image_fractions_and_enter_saves_it`: `window.drag((300, 233), (600, 422))` → `ANNOTATOR_COMMENT_ID` is found, focused, with placeholder "Enter saves · Esc drops"; `window.input("the toolbar")`, `press("enter")` → `boxes()` has one box within 1e-4 of `{0.25, 0.25, 0.25, 0.25}` with that comment; `badge_id(1)` is found with its top-left at (300, 233) within 1 point; the comment field is gone; `draft.json` on disk has the box.
  - `a_letterboxed_image_clamps_the_drag_to_the_picture`: a 2400×800 capture is drawn 1200×400 at y 222; `drag((300, 100), (600, 322))` → `{0.25, 0.0, 0.25, 0.25}`.
  - `a_drag_smaller_than_eight_points_is_ignored`: `drag((300, 233), (307, 300))` → no comment field, no box; `drag((300, 233), (308, 241))` → the field opens.
  - `escape_drops_a_new_box_and_an_empty_comment_drops_it_too`: drag, `press("escape")` → no box; drag, `press("enter")` with nothing typed → no box; drag, type three spaces, Enter → no box.
  - `clicking_a_badge_edits_its_comment`: after one saved box, click `badge_id(1)` → the field's value is "the toolbar"; select all, type "the sidebar", Enter → the comment is "the sidebar"; click the badge, type "x", Escape → still "the sidebar".
  - `delete_removes_the_box_and_renumbers`: three boxes; click `badge_id(2)`, click `ANNOTATOR_DELETE_ID` → two boxes, the former third is found as `badge_id(2)`, `badge_id(3)` is not found.
  - `the_twenty_first_box_is_refused`: with 20 boxes set through `set_boxes` and the annotator reopened, a drag opens no field and the title label is "A screenshot can have at most 20 boxes."
  - `a_comment_over_500_characters_is_not_saved`: type 501 characters, Enter → `ANNOTATOR_COMMENT_ERROR_ID` has the label "A comment can be at most 500 characters." and the field is still open; deleting one character and Enter saves a 500-character comment.
  - `boxes_keep_their_place_in_a_window_of_another_size`: save the box above at 1200×800 and press Done; open a second shell on the same home at 900×611 (image area 900×567) and click the card → `badge_id(1)` is at (225, 186) within 1 point and `boxes()` is unchanged.
  - `boxes_and_badges_use_the_destructive_token`: the quad painted at the badge's center equals `token_background(&dark_native(), "destructive")`.
  - `done_returns_to_the_pane_and_releases_the_image`: keep `Arc::downgrade(&annotator.image().unwrap())`; click `ANNOTATOR_DONE_ID`, park, frame → `shell.annotator()` is `None`, the weak reference no longer upgrades, the shell has the focus (`cmd-alt-f` toggles the pane), and `screenshot-1.annotated.png` exists.
  - `discard_without_boxes_is_immediate_and_with_boxes_asks`: no boxes: click `ANNOTATOR_DISCARD_ID` → the annotator closes and the draft is empty. One box: click → a dialog titled "Discard this screenshot?" with the description "Its 1 comment is deleted."; `ANNOTATOR_KEEP_ID` keeps everything; again with `ANNOTATOR_CONFIRM_DISCARD_ID` → closed, draft empty, files gone.
  - `the_second_screenshot_says_two_of_two`: capture, Done, capture → the title is "Screenshot 2 of 2 · Drag a box over what you mean"; reopening the first card says "Screenshot 1 of 2 · …" and shows its saved badges.
- [ ] **Step 2: Run.** `cargo test -p smithagents-app --test feedback_annotator`. Expected: compile errors.
- [ ] **Step 3: Run the "verify first" check** for the comment input and choose the input.
- [ ] **Step 4: Implement** the annotator and the shell overlay.
- [ ] **Step 5: Run.** Expected: PASS. Gate.
- [ ] **Step 6: Commit.** `feat(feedback): full-window annotator with numbered boxes and comments`

---

### Task 8: Wiring, the end-to-end test and the user note

**Files:**
- Modify: `crates/app/src/lib.rs` (`open_main_window` builds the real options), `crates/app/src/feedback/state.rs` (`native_file_picker`)
- Create: `crates/app/tests/feedback_flow.rs`, `docs/feedback.md`

**Interfaces:**
- Consumes: everything above.
- Produces:

```rust
/// The system's open-file dialog: files only, several, prompt "Add" (research B5). Used only by lib.rs.
pub fn native_file_picker() -> FilePicker;
// lib.rs, in open_main_window:
feedback: Some(FeedbackOptions {
    store: Some(DraftStore::new(&link.home)),
    capturer: Some(Arc::new(smithagents_capture::XcapCapturer::new())),
    picker: Some(native_file_picker()),
    frame_hop: None,
}),
```

`native_file_picker` wraps `cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Add".into()) })`: `Ok(Ok(paths))` → `Ok(paths)`; `Ok(Err(e))` → `Err(e.to_string())`; a dropped receiver → `Ok(None)`.

- [ ] **Step 1: Write the failing test** `the_whole_path_from_hold_to_a_restored_draft` in `tests/feedback_flow.rs`, with `FeedbackSetup` on one tempdir home, in this order, asserting after each step:
  1. Press the `rail-feedback` button, advance 3010 ms, run both hops, release → `calls().len() == 1`; `ANNOTATOR_ID` is found; `HOLD_RING_ID` is not; the dock did not toggle.
  2. `drag((300, 233), (600, 422))`, type "the toolbar", Enter; `drag((700, 500), (900, 650))`, type "this label", Enter → `badge_id(1)` and `badge_id(2)` are found.
  3. Click Done → the annotator is gone, the Feedback pane is open (the shell shows it on `CloseAnnotator`), and it has one card labeled "Screenshot 1 · 2 comments" containing both comment texts.
  4. On disk under `<home>/feedback/draft/`: `draft.json` (version 1, one screenshot, two boxes, `rendered: true`), `screenshot-1.png`, `screenshot-1.annotated.png` (2400×1512, `BOX_RGB` at `(600, 378)` and at `(1400, 912)`), `screenshot-1.thumb.png` (320×202). No file exists anywhere else under the home except what the shell's other stores write.
  5. "Add files…" with `good.png` → a second card "good.png".
  6. Close the window (`window.remove_window()`), open a new shell on the same home → after clicking the rail entry the pane shows the same two cards in the same order with the same labels and comments, with no notice; `calls().len()` is still 1.
  7. Click the screenshot card → the annotator shows `badge_id(1)` and `badge_id(2)`; Escape with no panel open changes nothing; Done.
  8. Discard, confirm → the pane shows `EMPTY_TEXT` and `<home>/feedback/draft` does not exist.
  And `the_default_options_never_reach_the_screen_or_a_dialog`: a shell from `ShellOptions::default()`: a full hold shows `UNSUPPORTED_TEXT`; "Add files…" changes nothing and `cx.did_prompt_for_paths()` is false.
- [ ] **Step 2: Run.** `cargo test -p smithagents-app --test feedback_flow`. Expected: FAIL at step 3 (the pane is not selected after Done) or earlier.
- [ ] **Step 3: Implement** the remaining wiring: on `CloseAnnotator` the shell calls `show_rail_entry(RailEntry::Feedback)`; `native_file_picker`; the options in `open_main_window`. `crates/host-desktop` needs no change.
- [ ] **Step 4: Write `docs/feedback.md`** (under 40 lines, for a user): what the Feedback entry does on a click and on a 3-second hold; how to draw a box, save and drop a comment, edit and delete; what "Add files…" accepts and the limits; that the draft lives in `<home>/feedback/draft/` and nothing leaves the machine; the Screen Recording permission on macOS and where to allow it; that Submit arrives with the next update.
- [ ] **Step 5: Run.** Expected: PASS. Gate, in full. `cargo build -p host-desktop` compiles (do not run it).
- [ ] **Step 6: Commit.** `feat(feedback): wire the real capturer, picker and draft directory; end-to-end test and user note`

---

## Manual check (a person, after the build; none of this is automated)

Real capture of the app's own window was never run: the research compiled the calls and stopped there. xcap's macOS backend uses `CGWindowListCreateImage`, which Apple has deprecated in favor of ScreenCaptureKit, and there is one report of gray images from it on a macOS 27 beta. Automated tests prove everything above the `Capturer` trait with a fake; they prove nothing about what follows. Run `cargo run -p host-desktop` with `SMITHAGENTS_HOME` pointing at a scratch directory and check, writing the result into the pull request:

1. **Permission, first run.** Hold the Feedback entry for 3 seconds on a machine where SmithAgents has no Screen Recording permission. Expected: the system prompt appears once, the pane shows the permission sentence, nothing is added. Note the macOS version and whether macOS shows its own extra warning.
2. **Permission granted.** Allow it in System Settings, restart the app if macOS asks, hold again. Expected: the annotator opens on a picture of the window. If it still says permission is missing after a rebuild, remove and re-add the app in the list (research C3, xcap #123) and note that.
3. **The picture itself.** It shows the whole window including the title bar, at Retina sharpness (pixel size about `scale ×` the window's size in points), not gray, not another window, and **the ring is not in it**. If the ring shows, two frames are not enough: add a short timer after the second hop in `Feedback::capture` and re-check (research "Unverified").
4. **Other displays and states.** Repeat on a second display, with the window fullscreen, and with a second SmithAgents window open if that is possible. `WINDOW_NOT_FOUND_TEXT` on any of these means the size match (2 points) is too strict there; record the sizes from the log.
5. **The hold's feel.** The ring is visible at rail size and fills smoothly; 3 seconds feels deliberate and not broken; releasing early opens the pane; sliding off the button cancels; the tooltip does not cover the ring.
6. **The annotator.** A 5K capture opens without a long freeze; boxes follow the pointer; the comment panel stays inside the window near the edges; badges are readable in the light and the dark theme; the annotated PNG's badges are readable at full size and in the pane's thumbnail.
7. **The picker.** "Add files…" opens the system dialog; choosing a `.txt` and a `.png` together adds one card and one sentence.
8. **Restart.** Quit with the annotator open after saving two boxes; start again; the card has both comments and its thumbnail shows both boxes.
9. **The ignored test** (optional, same permission): `SMITHAGENTS_CAPTURE_PID=<pid> SMITHAGENTS_CAPTURE_SIZE=1200x760 cargo test -p smithagents-capture --test manual -- --ignored --nocapture`, then open the printed PNG.

Not checked by anyone in this plan: Windows and Linux. The app crate is unix-only today (`atomic_file.rs`), and a Linux build of xcap needs libclang and the PipeWire development packages (research C1), which this plan does not install.

## Follow-ups

- **Submit** and its dialog (main idea, kind, repository, "Send it as"); `main_idea` and `kind` are already in `draft.json`. The 100 MB total must then count the annotated images too, and the daemon checks sizes and types again by content.
- **The daemon route** `POST /v1/feedback`, `GET /v1/feedback/targets`, the meta repository setting, the `feedback` built-in plugin with its tool requirements.
- **GitHub** (push, `gh issue create`, retry, the public-repository warning) and **the idea package** (through the intent tool, `agent-browser`, "Open pull request").
- **Recording video** inside the tool, and **more drawing tools**: arrows, free-hand, text, blur, crop; moving and resizing a saved box.
- **Replacing xcap's deprecated macOS backend** with ScreenCaptureKit behind the same `Capturer` trait, and deciding then whether capturing our own window needs the permission at all (Apple says an app may record its own windows; this plan refuses without it, to be safe).
- A monitor-crop fallback when the window is not in the list; matching on more than size when several of our windows have the same size.
- Remembering whether the help row is expanded; a keyboard way to start a capture from anywhere (today: the pane's Capture button).
- Windows and Linux: an `atomic_file` that is not unix-only, and a build check of xcap there. Capture on Linux and Windows has never been built or run (xcap's own table says window capture does not work under Wayland).
- A real `CenterTabHost` (native web views) would draw above the annotator overlay: the overlay must hide or cover the native views when one exists.
- The window cannot be moved while annotating (the overlay covers the title bar).
- Boxes cannot be drawn from the keyboard (badges can be reached and edited; a new box needs the pointer).
- A screenshot whose PNG is over 10 MB is refused: measure the size on a 5K display, and retry with stronger compression before refusing.
- The screenshot write and every draft save fsync on the UI thread; move them off it if a large write stutters.

## Self-review

- **Spec coverage (plan-split item 1):** capture crate (Task 1); rail entry under Settings, click, 3-second hold, ring, early release (Task 5); pane with cards, "Add files…", Capture, Discard, disabled "Submit…", help row, title "FEEDBACK" (Task 6); annotator bar, drag, anchored comment field, badges, edit, delete, Done, Discard (Task 7); draft with `draft.json`, fractions, restart (Tasks 3, 4, 8); annotated images and thumbnails (Tasks 2, 4); limits (Tasks 2, 4). No task adds a network call or a daemon route. Decisions 1 to 10 of the decisions file map to Tasks 1; 1; 1, 4, 6; 4, 5; 7; 2, 4, 6; 3; 2, 4; 2, 6, 7; Follow-ups.
- **Type consistency:** names in each Produces block were checked against every later Consumes and test step (`Capturer`, `checked`, `test_pattern`; `Annotation`, `FracRect`, `admit`, `Refusal::sentence`; `Draft`, `AttachmentId`, `DraftStore`; `Feedback`, `FeedbackOptions`, `FeedbackEvent`, `FrameHop`, `FilePicker`; `HoldButton`, `HOLD_RING_ID`, `ToggleFeedbackPane`).
- **Review Focus:** each of the five lines names its tasks; the tests are `an_empty_or_single_color_capture_is_refused`, `a_buffer_of_the_wrong_length_is_refused`, `each_capture_error_has_its_sentence_and_adds_nothing` (1); `a_screenshot_without_its_annotated_file_is_marked_for_redrawing`, `a_restart_restores_the_draft_and_redraws_what_was_not_drawn` (2); `admission_checks_type_then_size_then_total`, `picked_files_are_checked_one_by_one`, `two_files_with_the_same_name_get_different_stored_names` (3); `a_broken_draft_is_set_aside_and_named`, `a_stored_name_that_leaves_the_directory_is_dropped_and_never_touched`, `bad_boxes_are_cleaned_and_the_screenshot_is_redrawn` (4); `a_failed_save_leaves_the_old_draft_intact`, `a_failed_save_is_said_and_the_draft_stays_on_screen` (5).
- **Marked "verify first":** whether next-frame callbacks run on the test platform (Task 4, step 1; nothing depends on the answer); the multi-line comment input's Enter and Escape behaviour (Task 7; fallback: the single-line input the probe proved).

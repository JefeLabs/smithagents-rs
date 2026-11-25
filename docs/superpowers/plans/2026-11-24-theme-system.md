# Theme System Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One token source of truth in `design/tokens/` that generates a Tailwind v4 theme for `web/`, gives native panes resolved sRGB theme data, and defines the shell's live theme state, with checks that fail on a low-contrast theme or a hard-coded color.

**Architecture:** A pure-Rust library crate `smithagents-theme` owns the token model, OKLCH-to-sRGB math, validation (gamut and contrast), native theme data and the `ThemeState`/`ThemeController` the shell will own. A build tool `tools/themegen` renders the CSS and a tiny page-side bootstrap script from the same token files and hosts the literal-color lint and the generated-files drift check. Nothing here depends on GPUI, CEF or the daemon; Plans 0 and 3 consume the crate, the generated files and the bridge contract below.

**Tech Stack:** Rust (stable, edition 2021), serde/serde_json, thiserror, regex and ignore (lint), tempfile (tests), Node (one behavioural test of the generated JS; Node is required by `web/` anyway).

**Spec:** `docs/superpowers/specs/2026-11-23-smithagents-architecture-design.md` (sections 4.7 Theming, 6 Version 1 "Theming in v1", section 4.4 Color). Roadmap: `docs/superpowers/plans/2026-11-24-v1-roadmap.md`.

## Global Constraints

- Token files are OKLCH; one file per theme in `design/tokens/themes/`; `design/tokens/agent-palette.json` (Plan 1) is the agent palette and is not changed by this plan.
- Generated Tailwind output is CSS-first Tailwind v4: an `@theme` block plus one `[data-theme="<id>"]` block per theme.
- Variable names follow the shadcn contract: `--background`, `--foreground`, `--card`, `--primary`, `--muted`, `--border`, `--ring`, `--radius` (and their `-foreground` pairs).
- v1 ships exactly two themes, one dark (`smith-dark`) and one light (`smith-light`), plus the "System" choice that maps to that pair.
- Adding a theme must need only a new token file: no Rust, CSS or TypeScript edits.
- No hard-coded colors in any tier; a check fails on literal colors outside the token and generated files.
- Agent colors come from the token palette; web uses `oklch()` directly, native converts to sRGB; color is never the only identifier (the name is always shown).
- Red, yellow and green hues are reserved for status; agent colors never use them.
- New views receive the current theme before first paint, so they never flash the wrong theme.
- Rust stable, edition 2021, `unsafe_code = "forbid"` (workspace lint). Naming is `smithagents`, never `singularity`.

## Findings that shaped this plan

1. **The Plan 1 agent palette fails contrast on a light theme.** `agent-palette.json` is L 0.72, C 0.12. Against a near-white background that is 2.3 to 2.6:1, below the 3:1 floor for non-text marks (the agent's accent bar and tab border). Raising the contrast by lowering L at the same chroma pushes cyan and azure out of the sRGB gamut. A scan found L 0.62, C 0.10 is in gamut for all six hues and reaches at least 3.3:1 on the light theme. So each theme file carries its own agent presentation `l` and `c` (equal across all agent hues within a theme, preserving "equal saturation"), and the daemon-supplied color contributes only its **hue**. The palette file and the daemon API stay unchanged. Plan 3 must render agent colors as `oklch(theme.agent.l theme.agent.c <hue>)`, never with the daemon's raw `l` and `c`.
2. **"info" must not look like an agent color.** Agent hues include blue and cyan, and the spec reserves only red, yellow and green for status. `status-info` is therefore near-neutral (chroma at most 0.05), and `primary` is neutral too, so only agents and the three reserved status hues carry hue in the UI.
3. **GPUI theme data.** The spec says the generator emits it "mapped onto gpui-component's theme format; verify when building". GPUI is not a dependency of this plan and gpui-component's format is unverified. This plan instead exposes `NativeTheme` (resolved sRGB per token) from the theme crate, computed from the same token files; Plan 3 writes the thin adapter into gpui-component once that version is verified. Runtime conversion is needed anyway for daemon-supplied agent hues.

## Bridge contract (consumed by Plan 3's host)

- **State** pushed to every view, as JSON: `{"theme":"smith-dark","appearance":"dark","text_scale":1.0,"font":"system"}` (`theme` is always a concrete theme id, never "system").
- **Before first paint:** the host injects `window.__SMITH_THEME__ = <state>` at document start (CEF document-start script). Pages include `design/generated/theme-bootstrap.js` synchronously in `<head>`, before any stylesheet or module script.
- **Live changes:** the host calls `window.__smithagentsApplyTheme(<state>)` in every view.
- **No host** (plain browser, dev harness): the bootstrap picks the dark or light theme from `prefers-color-scheme`.
- **Agent color in web:** a chat view sets `--agent-h` (degrees, from the daemon's session data) on its root; the generated `--color-agent` utility (`border-agent`, `bg-agent`) resolves with the theme's agent `l` and `c`.

## Review Focus

1. A token file with a missing token, an out-of-gamut color, a bad id or low contrast fails with an error naming the theme and token, and `generate` writes nothing (Tasks 3, 8).
2. A third theme dropped in as a file only: appears in `ThemeSet`, CSS, bootstrap and System resolution without any code change (Tasks 2, 5, 6).
3. Persisted settings gone stale: unknown theme id (a removed theme), unknown font, NaN or out-of-range text scale give a sane themed state, never an error or an unthemed view (Tasks 4, 6).
4. A page with no host state, or a host state naming an unknown theme, still gets a theme (Task 6).
5. Lint false positives and negatives: issue refs like `#123` in comments, `r#"..."#`, `rgb(var(--x))`, HTML entities, and files under `target/`, `.worktrees/`, `node_modules/` or non-UTF-8 must not break or spam; `rgb(0x…)`, `hsla(…)` and `#abc` in code must be caught (Task 7).

---

### Task 1: Theme crate scaffold and OKLCH color math

**Files:**
- Create: `crates/theme/Cargo.toml` (package `smithagents-theme`, workspace lints, deps `serde`, `serde_json`, `thiserror`; dev-deps `tempfile`)
- Create: `crates/theme/src/lib.rs` (declares modules as later tasks add them)
- Create: `crates/theme/src/color.rs`
- Test: `crates/theme/tests/color.rs`

**Interfaces:**
- Produces (all in `smithagents_theme::color`):
  - `struct Oklch { pub l: f64, pub c: f64, pub h: f64 }` (Debug, Clone, Copy, PartialEq, Serialize, Deserialize; `h` in degrees)
  - `struct Srgb { pub r: u8, pub g: u8, pub b: u8 }` (Debug, Clone, Copy, PartialEq, Eq, Hash)
  - `struct OutOfGamut { pub color: Oklch }` (thiserror Error, PartialEq)
  - `Oklch::to_srgb(self) -> Result<Srgb, OutOfGamut>`: converts through OKLab and linear sRGB with Ottosson's matrices, then sRGB gamma; any linear channel outside `[-0.001, 1.001]` is out of gamut; in-gamut values are clamped then rounded to u8.
  - `Oklch::css(self) -> String`: `oklch(0.72 0.12 190)`, each number at most 4 decimals with trailing zeros trimmed (`0.7` not `0.7000`, `190` not `190.0`).
  - `Srgb::hex(self) -> String` (`#rrggbb`, lowercase), `Srgb::relative_luminance(self) -> f64` (WCAG 2.x)
  - `fn contrast_ratio(a: Srgb, b: Srgb) -> f64` (WCAG 2.x, symmetric, 1.0 to 21.0)

- [ ] **Step 1: Write failing tests in `crates/theme/tests/color.rs`.** Cases: `oklch(1 0 0)` converts to `#ffffff` and `oklch(0 0 0)` to `#000000`; `oklch(0.6279 0.2577 29.23)` converts to a value within 2 per channel of `#ff0000`; `oklch(0.7 0.4 150)` is `Err(OutOfGamut)`; `css()` of `(0.72, 0.12, 190.0)` is `oklch(0.72 0.12 190)` and of `(0.5, 0.0, 0.0)` is `oklch(0.5 0 0)`; `contrast_ratio(black, white)` is within 0.01 of 21.0 and equals `contrast_ratio(white, black)`; equal colors give 1.0.
- [ ] **Step 2: Run `cargo test -p smithagents-theme --test color`.** Expected: FAIL (crate missing). Add `crates/theme` to the workspace by changing `members` in `Cargo.toml` to `["crates/*", "tools/*"]` (the glob matches the new crate; `tools/` arrives in Task 5, an unmatched glob is fine).
- [ ] **Step 3: Implement `color.rs` to the signatures above.** Use f64 throughout; quantize last.
- [ ] **Step 4: Run `cargo test -p smithagents-theme` and `cargo clippy -p smithagents-theme --all-targets -- -D warnings`.** Expected: PASS, clean.
- [ ] **Step 5: Commit** `feat(theme): OKLCH to sRGB conversion and WCAG contrast`.

---

### Task 2: Token files and loader

**Files:**
- Create: `design/tokens/base.json`, `design/tokens/themes/smith-dark.json`, `design/tokens/themes/smith-light.json`, `design/tokens/README.md` (how to add a theme: copy a file, change `id`, `name`, values, run `cargo test -p smithagents-theme` and `cargo themegen generate`)
- Create: `crates/theme/build.rs` (scans `design/tokens/themes/*.json` and writes `$OUT_DIR/embedded_themes.rs` with one `include_str!` per file, sorted by filename; `cargo:rerun-if-changed=../../design/tokens`)
- Create: `crates/theme/src/tokens.rs`
- Test: `crates/theme/tests/tokens.rs`

**Interfaces:**
- Consumes: `Oklch` from Task 1.
- Produces (`smithagents_theme::tokens`):
  - `enum Appearance { Dark, Light }` (serde lowercase, Copy, Eq)
  - `struct AgentPresentation { pub l: f64, pub c: f64 }`
  - `struct ThemeFile { pub id: String, pub name: String, pub appearance: Appearance, pub colors: BTreeMap<String, Oklch>, pub agent: AgentPresentation }`
  - `struct SystemPair { pub dark: String, pub light: String }`
  - `struct FontToken { pub id: String, pub label: String, pub sans: String, pub mono: String, pub native_sans: String, pub native_mono: String }` (`sans`/`mono` are CSS stacks; `native_*` are family names for GPUI; the value `.SystemUIFont` for the system font is to be verified in Plan 3)
  - `struct TextScale { pub min: f64, pub max: f64, pub default: f64 }`
  - `struct BaseTokens { pub system_pair: SystemPair, pub radius_px: f64, pub spacing_rem: f64, pub text_scale: TextScale, pub text_sizes_rem: BTreeMap<String, f64>, pub fonts: Vec<FontToken> }`
  - `struct ThemeSet { pub base: BaseTokens, pub themes: Vec<ThemeFile>, pub agent_hues: Vec<f64> }` with `ThemeSet::load_dir(tokens_dir: &Path) -> Result<ThemeSet, TokenError>` (reads `base.json`, `themes/*.json` sorted by filename, and hues from `agent-palette.json`), `ThemeSet::embedded() -> &'static ThemeSet` (parsed once from the build-script-embedded files, panics with the file name if a shipped file is malformed), `ThemeSet::theme(&self, id: &str) -> Option<&ThemeFile>`
  - `const COLOR_TOKENS: &[&str]` (required, in this order): `background foreground card card-foreground card-2 card-2-foreground center-pane center-pane-foreground popover popover-foreground primary primary-foreground secondary secondary-foreground muted muted-foreground accent accent-foreground destructive border input ring selection status-ok status-warn status-error status-info`
  - `enum TokenError` (thiserror): `Io { path, source }`, `Parse { path, source }`
- Token values, to be used verbatim (L C H; the validation in Task 3 enforces them, so a typo here fails loudly):

| token | smith-dark | smith-light |
|---|---|---|
| background | .18 .01 260 | .985 .003 260 |
| foreground | .96 .005 260 | .2 .01 260 |
| card / popover | .22 .012 260 / .25 .014 260 | 1 0 0 / 1 0 0 |
| card-foreground, popover-foreground | .96 .005 260 | .2 .01 260 |
| card-2 / card-2-foreground (second card surface, owner decision 2026-11-24) | .26 .013 260 / .96 .005 260 | .97 .004 260 / .2 .01 260 |
| center-pane / center-pane-foreground (center pane and active tab; starts equal to background) | .18 .01 260 / .96 .005 260 | .985 .003 260 / .2 .01 260 |
| primary / primary-foreground | .92 .01 260 / .2 .01 260 | .22 .01 260 / .98 .003 260 |
| secondary / -foreground | .28 .012 260 / .96 .005 260 | .96 .006 260 / .25 .01 260 |
| muted / muted-foreground | .26 .01 260 / .72 .015 260 | .955 .006 260 / .5 .02 260 |
| accent / accent-foreground | .3 .015 260 / .96 .005 260 | .94 .008 260 / .22 .01 260 |
| destructive | .7 .19 25 | .577 .22 27 |
| border / input | .32 .012 260 / .36 .012 260 | .9 .006 260 / .88 .006 260 |
| ring / selection | .65 .05 260 / .4 .06 260 | .6 .04 260 / .88 .05 260 |
| status-ok / warn | .75 .17 150 / .82 .15 95 | .55 .15 150 / .6 .12 90 |
| status-error / info | .7 .19 25 / .72 .03 250 | .577 .22 27 / .55 .03 250 |
| agent l, c | .72, .12 | .62, .10 |

  `base.json`: `system_pair` `{dark: "smith-dark", light: "smith-light"}`, `radius_px` 8, `spacing_rem` 0.25, `text_scale` `{min 0.85, max 1.5, default 1.0}`, `text_sizes_rem` xs 0.75, sm 0.875, base 1, lg 1.125, xl 1.25, `fonts`: `system` ("System", sans `ui-sans-serif, system-ui, sans-serif`, mono `ui-monospace, SFMono-Regular, Menlo, monospace`, native `.SystemUIFont` / `Menlo`) and `mono` ("Monospace", both stacks the mono stack, native both `Menlo`).

- [ ] **Step 1: Write failing tests in `crates/theme/tests/tokens.rs`.** `embedded()` has exactly ids `smith-dark` (Dark) and `smith-light` (Light) and every `COLOR_TOKENS` name is present in both; `agent_hues` equals `[190, 220, 250, 280, 310, 340]`; `load_dir` on a tempdir copy of `design/tokens` plus a third theme file (`ids` differs only) yields 3 themes in filename order; a malformed theme file gives `TokenError::Parse` whose message contains the file path; a missing `base.json` gives `TokenError::Io`; a dev-dependency test (add `smithagents-core` as a dev-dependency) asserts the theme crate's `agent_hues` equal the hues of `smithagents_core::palette::palette()`.
- [ ] **Step 2: Run `cargo test -p smithagents-theme --test tokens`.** Expected: FAIL.
- [ ] **Step 3: Write the JSON files from the table, then `build.rs` and `tokens.rs`.** Serde derive with `deny_unknown_fields` on theme and base structs so a typo in a token file is an error, not silence.
- [ ] **Step 4: Run `cargo test -p smithagents-theme`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(theme): token files and loader`.

---

### Task 3: Validation (gamut, contrast, status rules)

**Files:**
- Create: `crates/theme/src/validate.rs`
- Test: `crates/theme/tests/validate.rs`

**Interfaces:**
- Consumes: `ThemeSet`, `ThemeFile`, `COLOR_TOKENS`, `Appearance`, `Oklch::to_srgb`, `contrast_ratio`.
- Produces (`smithagents_theme::validate`):
  - `fn validate(set: &ThemeSet) -> Result<(), Vec<ValidationError>>` (collects every error, does not stop at the first)
  - `enum ValidationError` (thiserror, each message names theme and token): `BadThemeId { id }` (must match `^[a-z][a-z0-9-]*$` and not be `system`), `DuplicateThemeId { id }`, `MissingToken { theme, token }`, `UnknownToken { theme, token }`, `OutOfGamut { theme, token }`, `LowContrast { theme, foreground, background, ratio: f64, min: f64 }`, `StatusHueOutsideReservedRange { theme, token }`, `InfoTooSaturated { theme }`, `AgentOutOfGamut { theme, hue: f64 }`, `AgentLowContrast { theme, hue: f64, against: String, ratio: f64 }`, `MissingAppearance { appearance: Appearance }` (a set needs at least one dark and one light theme), `BadSystemPair { which: Appearance, id: String }` (must name an existing theme of that appearance), `BadFont`/`BadTextScale` for empty fonts or `min <= default <= max` violated.
  - `pub const CONTRAST_RULES: &[(&str, &str, f64)]`: `(foreground token, background token, minimum)`: text pairs at 4.5: `foreground/background`, `card-foreground/card`, `popover-foreground/popover`, `primary-foreground/primary`, `secondary-foreground/secondary`, `accent-foreground/accent`, `muted-foreground/background`, `muted-foreground/muted`, `muted-foreground/card`, `card-2-foreground/card-2`, `center-pane-foreground/center-pane`, `muted-foreground/card-2`, `muted-foreground/center-pane`; non-text marks at 3.0 against `background`, `card`, `card-2` and `center-pane`: `destructive`, `ring`, `status-ok`, `status-warn`, `status-error`, `status-info`.
- Rules beyond contrast: reserved hue ranges are read from the same `agent-palette.json` (`reserved_hue_ranges`; a range with start > end wraps through 0, as in Plan 1's `palette.rs`), so extend `ThemeSet` loading to keep `reserved_hue_ranges: BTreeMap<String,(f64,f64)>`; `status-error` hue must be in `red`, `status-warn` in `yellow`, `status-ok` in `green`; `status-info` chroma at most 0.05; every agent hue rendered at the theme's `agent.l`/`agent.c` must be in gamut and reach 3.0:1 against `background`, `card`, `card-2` and `center-pane`; `card` and `card-2` must convert to different sRGB colors (`IndistinctSurfaces`).

- [ ] **Step 1: Write failing tests in `crates/theme/tests/validate.rs`.** Shipped `ThemeSet::embedded()` validates `Ok`. Each of these mutations of a clone yields exactly the named error (assert with `matches!`): remove `border` from one theme (`MissingToken`); add token `bogus` (`UnknownToken`); set `background` to `Oklch{l:0.7,c:0.4,h:150.0}` (`OutOfGamut`); set dark `foreground` to L 0.5 (`LowContrast` for `foreground`/`background`); set `status-error` hue to 250 (`StatusHueOutsideReservedRange`); set `status-info` chroma 0.2 (`InfoTooSaturated`); set light `agent` to the Plan 1 values `l .72 c .12` (`AgentLowContrast`, one per failing hue); id `Smith Dark` (`BadThemeId`); id `system` (`BadThemeId`); two files with one id (`DuplicateThemeId`); remove the only light theme (`MissingAppearance`); `system_pair.light` naming a dark theme (`BadSystemPair`). A set with two mutations reports both errors.
- [ ] **Step 2: Run `cargo test -p smithagents-theme --test validate`.** Expected: FAIL.
- [ ] **Step 3: Implement `validate.rs`.** Contrast is computed on quantized `Srgb` values (what users see), not on floats.
- [ ] **Step 4: Run `cargo test -p smithagents-theme`.** Expected: PASS. If a shipped token fails, fix the token file value, not the rule.
- [ ] **Step 5: Commit** `feat(theme): token validation with contrast and status-hue rules`.

---

### Task 4: Native theme data and theme state

**Files:**
- Create: `crates/theme/src/native.rs`, `crates/theme/src/state.rs`
- Test: `crates/theme/tests/native.rs`, `crates/theme/tests/state.rs`

**Interfaces:**
- Consumes: `ThemeSet`, `Srgb`, `Appearance`, `FontToken`, `TextScale`.
- Produces (`smithagents_theme::native`): `struct NativeTheme { pub id: String, pub name: String, pub appearance: Appearance, pub colors: BTreeMap<String, Srgb>, pub radius_px: f64, pub agent_l: f64, pub agent_c: f64 }`; `ThemeSet::native(&self, id: &str) -> Option<NativeTheme>` (requires a validated set; converts every token, returns `None` for an unknown id, panics only if called on an out-of-gamut set); `NativeTheme::agent_color(&self, hue: f64) -> Srgb`.
- Produces (`smithagents_theme::state`):
  - `enum ThemeChoice { System, Id(String) }` serializing as a plain string, `"system"` or the id (implement via `From<String>`/`Into<String>` serde conversion).
  - `struct AppearanceSettings { pub theme: ThemeChoice, pub text_scale: f64, pub font: String }` (Serialize, Deserialize, Clone, PartialEq; what the shell persists).
  - `struct ThemeState { pub theme: String, pub appearance: Appearance, pub text_scale: f64, pub font: String }` (serde with exactly the field names in the bridge contract; what is pushed to views).
  - `struct ThemeController` with `ThemeController::new(set: ThemeSet, settings: AppearanceSettings, system: Appearance) -> Self`, `state(&self) -> ThemeState`, `update_settings(&mut self, settings: AppearanceSettings) -> Option<ThemeState>`, `set_system_appearance(&mut self, system: Appearance) -> Option<ThemeState>`. The update methods return `Some(new state)` only when the state differs from before, so the shell broadcasts exactly on change.
  - Resolution rules: `System` follows the system appearance through `system_pair`; an unknown theme id resolves like `System`; an unknown font id becomes the first font; `text_scale` NaN or infinite becomes the default and finite values clamp to `[min, max]`; `appearance` in the state is the resolved theme's.

- [ ] **Step 1: Write failing tests.** `native.rs`: for both shipped themes, `native(id)` has every `COLOR_TOKENS` entry; `smith-light` `background` has relative luminance above 0.9 and `smith-dark` `background` below 0.05; `agent_color(190.0)` of the light theme equals `Oklch{l:0.62,c:0.10,h:190.0}.to_srgb()`; `native("nope")` is `None`. `state.rs`: `System` with system Dark gives `smith-dark`, flipping system to Light returns `Some` with `smith-light` and `appearance` Light; an explicit `Id("smith-dark")` ignores a system flip (`None`); `update_settings` with identical settings returns `None`; stale input (`Id("deleted-theme")`, font `"comic"`, `text_scale` `f64::NAN`, then `9.0`, then `0.1`) gives the System-resolved theme, font `system`, scale 1.0, then 1.5, then 0.85; serde round trip of `ThemeState` equals the exact JSON `{"theme":"smith-dark","appearance":"dark","text_scale":1.0,"font":"system"}`; `ThemeChoice` serializes as `"system"` and `"smith-dark"`; a third theme (loaded via `load_dir`) selected by id is honored.
- [ ] **Step 2: Run the two test files.** Expected: FAIL.
- [ ] **Step 3: Implement `native.rs` and `state.rs`.**
- [ ] **Step 4: Run `cargo test -p smithagents-theme` and clippy `-D warnings`.** Expected: PASS, clean.
- [ ] **Step 5: Commit** `feat(theme): native theme data and shell theme state`.

---

### Task 5: Tailwind CSS generator

**Files:**
- Create: `tools/themegen/Cargo.toml` (package `themegen`, bin + lib, workspace lints; deps `smithagents-theme`, `regex`, `ignore`; dev-deps `tempfile`), `tools/themegen/src/lib.rs`, `tools/themegen/src/css.rs`, `.cargo/config.toml` (alias `themegen = "run -p themegen --"`)
- Modify: `Cargo.toml` (add `regex` and `ignore` to `[workspace.dependencies]` at the versions `cargo add` resolves)
- Test: `tools/themegen/tests/css.rs`

**Interfaces:**
- Consumes: `ThemeSet`, `COLOR_TOKENS`, `Oklch::css`.
- Produces: `themegen::css::render_css(set: &ThemeSet) -> String`, deterministic (stable ordering, `\n` endings, trailing newline). Output layout, in order:
  1. A `/* GENERATED by themegen from design/tokens. Do not edit. */` header.
  2. `@theme inline { … }` mapping every color token to Tailwind: `--color-<token>: var(--<token>);` (so `bg-background`, `text-muted-foreground`, `border-border`, `ring-ring` utilities exist), `--color-agent: oklch(var(--agent-l) var(--agent-c) var(--agent-h, 250));`, radius scale `--radius-sm/md/lg/xl` as `calc(var(--radius) - 4px)`, `- 2px`, `var(--radius)`, `+ 4px`, `--spacing: <spacing_rem>rem;`, `--text-<name>: <rem>rem;` per text size, `--font-sans: var(--font-sans);` and `--font-mono: var(--font-mono);`.
  3. The dark pair theme's block uses the selector `:root, [data-theme="<dark pair id>"]`, so a page with no `data-theme` attribute renders dark.
  4. One block per theme: `[data-theme="<id>"] { color-scheme: <appearance>; --<token>: oklch(…); … --agent-l: …; --agent-c: …; --radius: <px>px; }`, tokens in `COLOR_TOKENS` order.
  5. One `[data-font="<id>"] { --font-sans: …; --font-mono: …; }` block per font.
  6. `html { font-size: calc(16px * var(--text-scale, 1)); }`.

- [ ] **Step 1: Write failing tests in `tools/themegen/tests/css.rs`.** Render the embedded set and assert: exactly one `[data-theme="smith-dark"]` and one `[data-theme="smith-light"]` block; contains `--background: oklch(0.18 0.01 260);` and `--color-background: var(--background);` and `color-scheme: light;`; `--color-agent:` line present; every `COLOR_TOKENS` name appears as `--<token>:` inside each theme block; the dark block's selector includes `:root`; rendering twice is byte-identical; rendering a set loaded with a third theme file (tempdir) has three theme blocks and the new id, with no code change.
- [ ] **Step 2: Run `cargo test -p themegen --test css`.** Expected: FAIL.
- [ ] **Step 3: Implement `css.rs`; add the alias and workspace deps.**
- [ ] **Step 4: Run `cargo test -p themegen --test css`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(themegen): Tailwind v4 theme generator`.

---

### Task 6: Page-side bootstrap script

**Files:**
- Create: `tools/themegen/src/bootstrap.rs`
- Test: `tools/themegen/tests/bootstrap.rs`, `tools/themegen/tests/bootstrap_check.mjs`

**Interfaces:**
- Consumes: `ThemeSet` (system pair, theme ids and appearances, fonts, text-scale bounds).
- Produces: `themegen::bootstrap::render_bootstrap(set: &ThemeSet) -> String`: one self-contained classic script (no modules, no dependencies, ES2015 at most), deterministic. Behavior:
  - Embeds the known theme ids with appearances, the font ids, the system pair and the text-scale bounds as a JSON literal.
  - Defines `window.__smithagentsApplyTheme(state)`: validates `state` against the embedded data (unknown theme id falls back to the system pair for the current `prefers-color-scheme`; unknown font to the first; non-finite or out-of-range text scale to default or clamped), then on `document.documentElement` sets attributes `data-theme` and `data-font`, the inline property `--text-scale`, and `style.colorScheme`.
  - Runs immediately: applies `window.__SMITH_THEME__` if present, otherwise applies a state derived from `matchMedia('(prefers-color-scheme: dark)')` (and from the dark pair if `matchMedia` is missing).
  - Never throws: a malformed state object is treated as absent.

- [ ] **Step 1: Write failing tests.** `bootstrap.rs` (Rust): output contains `__smithagentsApplyTheme` and `smith-light`, is deterministic, and contains no `import `/`export ` keywords. `bootstrap_check.mjs` run by a second Rust test through `node` (the Rust test writes the rendered script to a tempdir and runs `node tools/themegen/tests/bootstrap_check.mjs <script>`, failing, not skipping, when `node` is absent or exits non-zero): load the script with `vm.runInNewContext` against a stub `window`, `document.documentElement` (records attributes and style properties) and `matchMedia`; assert (a) `__SMITH_THEME__` `{theme:"smith-light",…}` sets `data-theme` to `smith-light`; (b) no host state and `matchMedia` dark gives `smith-dark`, light gives `smith-light`; (c) unknown theme id `ghost` with dark media gives `smith-dark`; (d) text scale `NaN`, `99`, `0.1` give `1`, `1.5`, `0.85`; (e) calling `__smithagentsApplyTheme` later changes attributes; (f) `__SMITH_THEME__ = "garbage"` does not throw and applies the media default; (g) no `matchMedia` at all does not throw.
- [ ] **Step 2: Run `cargo test -p themegen --test bootstrap`.** Expected: FAIL.
- [ ] **Step 3: Implement `bootstrap.rs`** (a JS template with the data literal spliced in via `serde_json`).
- [ ] **Step 4: Run `cargo test -p themegen`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(themegen): page-side theme bootstrap`.

---

### Task 7: Literal-color lint

**Files:**
- Create: `tools/themegen/src/lint.rs`
- Test: `tools/themegen/tests/lint.rs`

**Interfaces:**
- Produces: `struct Finding { pub path: PathBuf, pub line: usize, pub text: String }`; `themegen::lint::lint_text(text: &str) -> Vec<(usize, String)>` (1-based line, offending snippet); `themegen::lint::lint_repo(root: &Path) -> std::io::Result<Vec<Finding>>`.
- Rules:
  - Scanned extensions: `rs css scss js mjs cjs ts tsx jsx html svg`. The walk uses the `ignore` crate (honors `.gitignore`, skips hidden directories such as `.worktrees/`), and additionally skips `target`, `node_modules`, `design/tokens`, `design/generated`, and any directory named `tests` or `fixtures`. Files that are not valid UTF-8 are skipped silently.
  - A line is skipped if it is a comment-only line (trimmed starts with `//`, `/*`, `*`, `<!--`) or contains `theme-lint: allow`.
  - Flagged: hex colors matching `(^|[^\w&#])#([0-9a-fA-F]{3,4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})\b`, and color functions `\b(rgb|rgba|hsl|hsla|hwb|lab|lch|oklab|oklch|color)\(\s*[-+.\d]` (so `rgb(var(--x))` and Rust format strings `oklch({} …)` are fine, while `rgb(0xff0000)` and `hsla(0.5, …)` are caught).
  - Known limits, stated in the module docs: named colors (`red`, `white`) and bare `0xRRGGBB` literals are not detected.

- [ ] **Step 1: Write failing tests in `tools/themegen/tests/lint.rs`** (fixtures built in a tempdir; this file is under `tests/`, so it is itself exempt). `lint_text` flags `color: #fff;`, `"#1A2B3C"`, `rgb(0xff0000)`, `hsla(0.5, 1.0, 0.5, 1.0)`, `oklch(0.7 0.1 200)`, `rgb(12 34 56)`; does not flag `rgb(var(--x))`, `// #fff in a comment`, `see issue #123` inside a `//` line, `r#"raw"#`, `&#123;`, `url(#gradient)` (id longer than 8 hex chars or not hex), `format!("oklch({} {} {})")`, and a line ending `// theme-lint: allow`. `lint_repo` on a tempdir: reports a `.tsx` file with a hex color with correct path and line; ignores the same content under `target/`, `node_modules/`, `.worktrees/` (a hidden dir), `design/tokens/`, `design/generated/`, `x/tests/`; a binary file named `a.rs` (invalid UTF-8) is skipped without error; a `.gitignore`d file is skipped.
- [ ] **Step 2: Run `cargo test -p themegen --test lint`.** Expected: FAIL.
- [ ] **Step 3: Implement `lint.rs`.**
- [ ] **Step 4: Run `cargo test -p themegen --test lint`.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(themegen): literal color lint`.

---

### Task 8: CLI, generated files and repo checks

**Files:**
- Create: `tools/themegen/src/main.rs`, `design/generated/theme.css`, `design/generated/theme-bootstrap.js` (produced by the tool, committed)
- Modify: `tools/themegen/src/lib.rs` (add `pub fn generate(tokens_dir: &Path, out_dir: &Path) -> Result<Vec<PathBuf>, GenError>` and `pub fn check(tokens_dir: &Path, out_dir: &Path) -> Result<(), GenError>`)
- Test: `tools/themegen/tests/generate.rs`, `tools/themegen/tests/repo.rs`

**Interfaces:**
- `generate`: loads with `ThemeSet::load_dir`, runs `validate`, and only if valid renders both files and writes them (write to temp names then rename, so a failure never leaves half-written outputs). Returns the written paths. `GenError` (thiserror): `Tokens(TokenError)`, `Invalid(Vec<ValidationError>)` (message lists every error, one per line), `Io`, `Drift { path }`.
- `check`: validates, renders in memory, compares byte-for-byte with the files in `out_dir`; any difference or missing file is `Drift`.
- CLI: `cargo themegen generate`, `cargo themegen check`, `cargo themegen lint`; each takes optional `--root <dir>` (default: the workspace root, found from `CARGO_MANIFEST_DIR/../..`). `lint` prints `path:line: snippet` per finding and exits 1 when any exist; `check` exits 1 on drift or invalid tokens. Exit 2 and a usage line on an unknown command.

- [ ] **Step 1: Write failing tests.** `generate.rs`: in a tempdir copy of `design/tokens`, `generate` writes both files and a following `check` is `Ok`; editing one token value makes `check` return `Drift` for `theme.css`; a token file with a missing `border` makes `generate` return `Invalid` naming that theme and token **and leaves `out_dir` without any new or modified file** (pre-seed `out_dir` with a sentinel `theme.css` and assert its bytes unchanged); adding a third theme file and generating yields `[data-theme="<new id>"]` in the CSS and the id in the bootstrap. `repo.rs` (runs against the real repo root): `repo_has_no_literal_colors` (`lint_repo(root)` is empty) and `generated_files_are_current` (`check(design/tokens, design/generated)` is `Ok`).
- [ ] **Step 2: Run `cargo test -p themegen`.** Expected: FAIL (`repo.rs` fails until the generated files exist).
- [ ] **Step 3: Implement `generate`/`check` and `main.rs`; run `cargo themegen generate`.** If `lint` reports findings in existing Plan 1 code, fix them to use tokens or, only for a genuine non-color match, add `theme-lint: allow` with a reason.
- [ ] **Step 4: Run the full gate.** `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`, `cargo themegen check`, `cargo themegen lint`. Expected: all pass, and the Plan 1 tests still pass (194 passed, 1 ignored before this plan).
- [ ] **Step 5: Commit** `feat(themegen): CLI, generated theme files, drift and lint checks`.

---

## Self-review notes

- **Spec coverage:** token files in OKLCH with the listed token classes (surface levels via background/card/popover, text, border, accent, ring, selection, status, radius, spacing, fonts, text-size scale): Task 2. Generator with `@theme` plus per-theme `[data-theme]` blocks and shadcn names: Task 5. GPUI theme data: Task 4 (see finding 3). ThemeState pushed to native and every CEF view, delivered before first paint: Task 4 (state) and Task 6 (page side); the CEF injection itself is Plan 3's host work and is specified in the bridge contract. System option: Task 4. Agent colors from tokens, contrast checked on both themes: Tasks 3 and 4. CI check for literal colors: Tasks 7 and 8; there is no CI service configured yet, so the checks run under `cargo test --workspace`, which any CI will run. Adding a theme with only a file: Tasks 2, 5, 6, 8 tests.
- **Left for Plan 3:** CEF document-start injection, Settings → Appearance UI, persistence of `AppearanceSettings`, detecting the macOS system appearance, the gpui-component adapter, and the web app importing `design/generated/theme.css`.
- **Deviation to confirm:** per-theme agent `l`/`c` (finding 1) means the daemon's agent `l`/`c` are informational; only `h` is used by clients.

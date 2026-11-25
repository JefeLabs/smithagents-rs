# Design tokens

- `base.json`: radius, spacing, text scale, fonts, and the system dark/light pair.
- `agent-palette.json`: agent hues and reserved status hue ranges (shared with `smithagents-core`).
- `themes/*.json`: one file per theme.

## Adding a theme

1. Copy `themes/smith-dark.json` (or `smith-light.json`) to `themes/<new-id>.json`.
2. Change `id`, `name`, and the color values (OKLCH `l`, `c`, `h`). Every required color token must be present; unknown fields are rejected.
3. Run `cargo test -p smithagents-theme` to check it loads and passes validation.
4. Run `cargo themegen generate` to regenerate the derived outputs.

## Surfaces

Required surface tokens, each with a `-foreground` pair: `background`, `card` (first card surface), `card-2` (second card surface, a step apart from `card`; the validator rejects identical colors), `center-pane` (center pane content and the active tab; may equal `background`), `popover`.

Default region mapping (one place: `region_tokens` in `crates/app/src/theme_adapter.rs`):

| region | surface |
|---|---|
| center pane, active center tab | `center-pane` |
| left pane, right activity pane | `card` |
| title bar (app-drawn), icon rail, tab strip, status bar | `card-2` |

The status bar runs the full width of the window's bottom (`Region::BottomBar`). The tab strip bar and inactive tabs use `card-2`; the active tab uses `center-pane`. The update chip in the title bar is a `primary` pill (`primary`/`primary-foreground`); the validator requires `primary` at 3:1 against `card-2`.

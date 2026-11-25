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

Required surface tokens, each with a `-foreground` pair: `background`, `card` (first card surface), `card-2` (second card surface, a step apart from `card`; the validator rejects identical colors), `center` (center pane content and the active tab; may equal `background`), `popover`.

Default region mapping (one place: `region_tokens` in `crates/app/src/theme_adapter.rs`):

| region | surface |
|---|---|
| center pane, active center tab | `center` |
| left pane, right activity pane | `card` |
| title bar, icon rail, tab strip, bottom bar | `card-2` |

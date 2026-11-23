# Unified module icons

156 module icons (150 core modules + 5 native external crates, plus
one `placeholder` icon), one SVG per icon in `modules/<name>.svg`,
combined into `modules-sprite.svg` (156 symbols) by
`scripts/build-module-icons-sprite.py` (deterministic, re-runnable).
Use them with `<svg class="icon"><use href="#module-icon-billing"/></svg>`.
New modules registered in `src/modules/mod.rs` need an icon file here;
until one exists the accessor falls back to `module-icon-placeholder`.

## Style — multi-colour IconPark

The icons are [IconPark](https://github.com/bytedance/IconPark)
(ByteDance, Apache 2.0 — see `LICENSE.icon-park`) multi-color theme
icons, plus 5 custom-drawn gap icons in the same visual language:

- 48x48 `viewBox`; stroke width 4, round caps and joins.
- Four fixed fills per icon (see Palette below). Fills are fixed at
  generation time — they do not follow CSS variables.
- Every file: `<title>` in English as first child, then the drawing.
- No gradients. **No Gemini-style sparkles/stars anywhere.** AI
  concepts use brain / neural-network nodes / circuit motifs (see below).

## Palette — one palette for light and dark

IconPark's default multi-color palette uses `#333` outlines, which
vanish on dark backgrounds. This set uses a single palette chosen to
read on both the light theme (`--bg: #f4f6f8`) and Editorial Dark
(`--bg: #141210`): the outline is slate `#64748B` instead of `#333`
(~4.8:1 on light, ~3.9:1 on dark). There is no second dark sprite;
the same icons ship in both themes.

| Fill | Value | Role |
|---|---|---|
| 1 — outline | `#64748B` (slate-500) | Strokes, outlines, neutral structure |
| 2 — main | `#2F88FF` (blue) | Main colour, primary meaning |
| 3 — light | `#FFFFFF` (white) | Inner detail on coloured shapes |
| 4 — accent | `#43CCF8` (light blue) | Secondary colour |

Exception: `tenant-destroy` uses danger red `#B3261E` for its X badge —
the only red in the set, kept from the old icon because a destructive
action must read as destructive.

## Provenance

- 151 icons are generated from IconPark 1.4.2 (`@icon-park/svg`,
  pinned) with theme `multi-color` and the palette above; see
  `docs/integration/MULTICOLOR_ICON_SETS.md` §8 for the full
  module → IconPark name mapping.
- 5 icons are custom-drawn in the same style (48 grid, stroke 4, same
  palette): `load-balancer` (arrow splitting into server bars),
  `vault` (safe with dial), `tenant-destroy` (building + red X badge),
  `site-builder` (browser window + layout blocks), `placeholder`
  (circle + "?").
- Licence: Apache 2.0 (`LICENSE.icon-park`). Keep the notice with the
  redistributed files; no attribution in the UI is required.
- The static SVGs in `modules/` are the source of truth; the
  generation step (Node + `@icon-park/svg`, see the proposal doc §4.1)
  is not shipped in the repo.

## Grayscale mode — retired

The old flat icons followed CSS variables (`--icon-*`) and a parent
could set `data-icon-theme="grayscale"` to recolour them. Multi-colour
icons carry fixed fills, so that mechanism no longer applies to module
icons; the variables remain defined in `assets/css/icons.css` but
nothing reads them. Do not fake grayscale with a CSS `filter` on
colour icons.

## AI family (no sparkles)

- `ai`: brain (IconPark `brain`).
- `ai-support`: robot (IconPark `robot`), not a star.
- `advisor`: help (IconPark `help`); never a plain circle.
- `mcp`: data server (IconPark `data-server`); never a star.

## Animation (opt-in, off by default)

See `ANIMATION.md` and `assets/css/icons.css`. Only the icons that
declare a `data-animation` in their icon file animate, each with a
stated reason in `ANIMATION.md`. Triggers live on a parent:
`data-icon-animation="spin|wiggle|pulse|bounce"` (explicit choice),
parent class `.icon-animated` (runs each icon's declared animation),
or `:hover`. `prefers-reduced-motion` disables all of it.

## Rebuilding the sprite

```sh
python3 scripts/build-module-icons-sprite.py
```

Then render the sprite on light and dark backgrounds and inspect the
PNGs before reporting done.

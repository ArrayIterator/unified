# Unified module icons

150 module icons (145 core modules + 5 native external crates) plus one
`placeholder` icon, one SVG per icon in `modules/<name>.svg`, combined
into `modules-sprite.svg` (151 symbols) by
`scripts/build-module-icons-sprite.py` (deterministic, re-runnable).
Use them with `<svg class="icon"><use href="#module-icon-billing"/></svg>`.
New modules registered in `src/modules/mod.rs` need an icon file here;
until one exists the accessor falls back to `module-icon-placeholder`.

## Style — flat solid fills, tracker language

The project tracker artifact has no SVG icon set at all — its whole
visual language is flat solid colour (one semantic colour per meaning),
fully rounded geometry, generous whitespace, no linework, no decoration.
These icons translate that language into SVG:

- **Fill-only silhouettes. No `stroke` attribute anywhere** — not on
  the `<svg>` root, not on any shape. Every shape carries an explicit
  `fill="var(--icon-*, #hex)"`.
- 24x24 `viewBox`; drawings stay inside the 3–21 range so nothing clips
  at small sizes; corners fully rounded (`rx` on rects, round joins in
  path data).
- Every file: `<title>` in English as first child, then the drawing.
- **Max 2 flat colours per icon**: one dominant semantic colour + one
  supporting colour (or a white knockout). Restraint is the style —
  no decorative linework, no gradients, no third colour.
- **No Gemini-style sparkles/stars anywhere.** AI concepts use
  brain / neural-network nodes / circuit motifs (see below).

## Palette (CSS custom properties)

Colors are written as `var(--name, fallback)` so the host page can
retheme them. The fallback is the color-mode default, so icons also
render correctly standalone (no CSS loaded). Values are the tracker
artifact's own semantic colours:

| Variable | Color mode (default) | Role |
|---|---|---|
| `--icon-ink` | `#64748B` (slate) | Neutral structure, default colour |
| `--icon-primary` | `#2563EB` (blue) | Main shape, primary meaning |
| `--icon-accent` | `#D97706` (amber) | Highlight / in-progress warmth |
| `--icon-success` | `#16A34A` (green) | Positive, done, live |
| `--icon-danger` | `#B3261E` (red) | Alert, failed, destructive |
| `--icon-teal` | `#0F766E` (teal) | Tracker accent, maintenance/mail |
| `--icon-violet` | `#7C3AED` (violet) | Pending, special states |
| `--icon-soft` | `#E2E8F0` (slate-200) | Light fills, secondary shapes |

White (`#FFFFFF`) is the knockout colour for details cut out of a
solid shape (e.g. a white check on a green badge).

## Grayscale mode

A parent sets `data-icon-theme="grayscale"` (`.icon-grayscale` class is
an alias). The variables are redefined to a stepped gray ramp so icons
keep their structure and stay distinguishable — do not fake grayscale
with a CSS `filter: grayscale()` on color icons; the variables are the
mechanism:

| Variable | Grayscale value |
|---|---|
| `--icon-ink` | `#475569` |
| `--icon-primary` | `#334155` |
| `--icon-accent` | `#64748B` |
| `--icon-success` | `#525252` |
| `--icon-danger` | `#0F172A` |
| `--icon-teal` | `#57534E` |
| `--icon-violet` | `#737373` |
| `--icon-soft` | `#A8A29E` |

Both modes are rendered and inspected before any report of done
(see `~/workspace/iconfix/` render scripts).

## AI family (no sparkles)

- `ai`: brain outline + neural-network nodes (dots connected by lines).
  Never a 4-point star/sparkle.
- `ai-support`: headset + a small brain/node mark, not a star.
- `advisor`: person + lightbulb (or compass); never a plain circle.
- `mcp`: plug / connected nodes; never a star.

## Weak metaphors that must read at 24px

These rendered as blobs or bare shapes in the first round and must be
redrawn, not just recolored: `advisor` (person + lightbulb),
`apikeys` (key with teeth + keyring), `approvals` (checkmark badge),
`bi` (bar chart + trend line), `calendar` (grid + marked date),
`cards` (stacked cards + chip). Any other icon that reads as a plain
circle, rectangle, or dot is a failure — add the distinguishing detail.

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

Then render both modes and inspect the PNGs before reporting done:
`~/workspace/iconfix/render_grid.py --mode color|grayscale`.

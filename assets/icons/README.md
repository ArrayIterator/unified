# Unified icons

## Layout

```
assets/icons/
  modules/            156 module icons (one per module id)
  modules-sprite.svg  built from modules/ (156 symbols)
  submodules/         582 sub-module icons (one per Rust sub-module name)
  submodules-sprite.svg  built from submodules/ (582 symbols)
  brands/             14 general brand icons (real marks + lettermarks)
  brands-sprite.svg   built from brands/ (14 symbols)
  flags/              country flags, 4x3 + 1x1 (flag-icons, MIT)
  ICONS.md            preview: modules + payments + brands
  SUBMODULES.md       preview: sub-module icons
  README.md           this file (layout, style, provenance)
  LICENSE.flat-color-icons  MIT-or-Good-Boy licence for the module icons
  LICENSE.tabler        MIT licence for the sub-module icons (Tabler)
```

Module icons are used with
`<svg class="icon"><use href="#module-icon-billing"/></svg>`.
Sub-module icons use `#submodule-icon-<name>`; brand icons use
`#brand-<slug>`. New modules registered in `src/modules/mod.rs`
need an icon file in `modules/`; until one exists the accessor
falls back to `module-icon-placeholder`.

Payment provider / card-scheme / method logos do NOT live here —
they live with the payments module in
`src/modules/payments/logos/` (see its README), and email provider
logos live in `src/modules/email_providers/logos/`. Demo copies of
all three logo sets live in `demo/assets/logos/`.

## Style — Icons8 Flat Color Icons (multi-tone, fixed palette)

The module icons are
[Icons8 Flat Color Icons](https://github.com/icons8/flat-color-icons)
(312 free icons, MIT-or-Good-Boy — see `LICENSE.flat-color-icons`),
plus 14 custom-drawn gap icons in the same visual language:

- 48x48 `viewBox`; flat fills, no strokes. Every shape carries its
  own explicit fill, so an icon renders identically on light and dark
  backgrounds — there is no second dark sprite and no `currentColor`.
- Every vendored file is a verbatim upstream copy (only the XML
  prolog, the SVG 1.1 DOCTYPE line and the Adobe Illustrator generator
  comment were stripped). Custom files carry no `<title>` (the module
  id is the label); the drawing is the whole file.
- A few source icons rely on the default black fill for small details
  (clock hands in `scheduler`/`timetracker`, the car swoosh stroke in
  `external-fleet`) — that is the original design, not a missing fill.
  The sprite builder therefore puts NO `fill`/`stroke` attributes on
  the `<symbol>` (a symbol-level `fill="none"` would hide those
  details).
- 15 of the 143 vendored icons are single-fill shapes (one solid
  colour, e.g. `like`, `bookmark`, `download`); the set as a whole is
  multi-tone and every icon is a solid colour shape, not an outline.
- No gradients. **No Gemini-style sparkles/stars anywhere.** AI
  concepts use automation / support / circuit motifs (see below).

The sub-module icons are still Tabler 2.11.0 outlines (see
`LICENSE.tabler`): 24x24 `viewBox`, stroke width 2, `currentColor` —
they inherit the surrounding text colour.

## Custom-drawn icons

13 module gaps had no correct Flat Color match, so they were drawn in
the Flat Color style (48 grid, flat fills, no strokes except the
megaphone sound arcs):

- `load-balancer` (arrow splitting into 3 server bars),
- `tenant-destroy` (building + red X badge),
- `site-builder` (browser window + layout blocks),
- `placeholder` (grey circle + "?"),
- `calendar` (blue-header calendar — the red-header `calendar` from
  the set belongs to `appointments`),
- `geo` (map pin), `email-providers` (envelope),
- `logs` (terminal window), `notifications` (bell + red badge),
- `social-marketing` (megaphone), `presets` (sliders),
- `network-tools` (RJ45 plug), `tickets` (ticket).

`maintenance` was fixed by a better vendored pick instead of a custom
draw: `support` (a wrench — exact for maintenance).

The sub-module `handlers_balancers` reuses the `load-balancer`
drawing (same concept). No other sub-module needed a custom drawing:
all 582 sub-module names map to Tabler icons (186 distinct icons;
names sharing a concept share an icon — see
`docs/integration/SUBMODULE_ICONS.md` for the definition and the
full mapping).

## Provenance

- Module icons: 143 vendored from Icons8 Flat Color Icons +
  14 custom-drawn. Full module → Flat Color name mapping:
  `docs/integration/SVGREPO_ICON_SETS.md` (§5 + §10 execution record).
  The previous sets (IconPark, then ColorTicons/Tabler) were removed
  entirely; see `docs/integration/MULTICOLOR_ICON_SETS.md` and
  `docs/integration/COLORTICONS_MAPPING.md` (both marked superseded).
- Sub-module icons: 582 names mapped to 186 Tabler icons (+ 1
  custom reuse). Full mapping:
  `docs/integration/SUBMODULE_ICONS.md`.
- Brand icons: 11 real marks from
  [simple-icons](https://github.com/simple-icons/simple-icons)
  (CC0) + 3 lettermark fallbacks (Microsoft, Slack, Amazon —
  removed from simple-icons 16.x). See `brands/README.md`.
- Payment logos: 11 real simple-icons marks + 14 lettermarks.
  See `src/modules/payments/logos/README.md`.
- Licences: Flat Color Icons MIT-or-Good-Boy
  (`LICENSE.flat-color-icons` — the repo has a `LICENSE.md`, no file
  literally named `LICENSE`); Tabler MIT for the sub-module icons
  (`LICENSE.tabler`); simple-icons CC0 (brand marks are trademarks of
  their owners, used only to identify their services; no endorsement
  implied); flag-icons MIT (`flags/LICENSE.flag-icons`).
- The static SVGs are the source of truth; the vendor scripts
  (`/tmp/vendor_flat_color.py`, `/tmp/vendor_submodules.py`,
  `/tmp/fetch_brand_marks.py`) are not shipped in the repo.

## Colour — one fixed palette for light and dark

Module icons carry their own fixed fills, so one sprite serves both
themes: an icon looks the same on a light background and on Editorial
Dark. Verify any new icon renders on both backgrounds before
reporting done. (The sub-module icons are the exception: Tabler
outlines in `currentColor`, so they follow the surrounding text
colour.)

## Grayscale mode — retired

The old flat icons followed CSS variables (`--icon-*`) and a parent
could set `data-icon-theme="grayscale"` to recolour them. Tabler
icons inherit `color` instead, so grayscale is just a grey text
colour on the parent — no special mechanism, and no CSS `filter`
on icons. The `--icon-*` variables remain defined in
`assets/css/icons.css` but nothing reads them.

## AI family (no sparkles)

- `ai`: automation (`automatic`).
- `ai-support`: online support (`online_support`), not a star.
- `advisor`: assistant (`assistant`); never a plain circle.
- `mcp`: circuit (`circuit`); never a star.

## Animation (opt-in, off by default)

See `ANIMATION.md` and `assets/css/icons.css`. Only the icons that
declare a `data-animation` in their icon file animate, each with a
stated reason in `ANIMATION.md`. Triggers live on a parent:
`data-icon-animation="spin|wiggle|pulse|bounce"` (explicit choice),
parent class `.icon-animated` (runs each icon's declared animation),
or `:hover`. `prefers-reduced-motion` disables all of it.

## Rebuilding the sprites

```sh
python3 scripts/build-module-icons-sprite.py
python3 scripts/build-submodule-icons-sprite.py
python3 scripts/build-provider-logos-sprite.py --src src/modules/payments/logos --glob 'payment-*.svg'
python3 scripts/build-provider-logos-sprite.py --src assets/icons/brands --glob 'brand-*.svg' --prefix '' --out assets/icons/brands-sprite.svg
```

Then render the sprites on light and dark backgrounds and inspect
the PNGs before reporting done.

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
  LICENSE.colorticons MIT licence for the module + sub-module icons
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

## Style — Tabler outlines (single colour, inherits text colour)

The module and sub-module icons are
[Tabler 2.11.0](https://github.com/tabler/tabler-icons) outline
icons, vendored from the
[ColorTicons](https://github.com/weilanwl/ColorTicons) fork (MIT —
see `LICENSE.colorticons`), plus 4 custom-drawn gap icons in the
same visual language:

- 24x24 `viewBox`; stroke width 2, round caps and joins.
- Single-colour strokes: `stroke="currentColor" fill="none"`.
  Icons inherit the surrounding text colour, so they adapt to
  light and dark themes automatically — there is no second dark
  sprite. To recolour an icon, set CSS `color` on its parent.
- Every vendored file is a verbatim upstream copy (only the 6
  `data-animation` attributes were added, see below). Custom files
  carry an English `<title>` as first child, then the drawing.
- No gradients. **No Gemini-style sparkles/stars anywhere.** AI
  concepts use brain / neural-network nodes / circuit motifs (see below).

## Custom-drawn icons

4 module gaps had no correct Tabler match, so they were drawn in
the Tabler style (24 grid, stroke 2, currentColor):

- `load-balancer` (arrow splitting into 3 server bars),
- `vault` (safe with dial),
- `tenant-destroy` (building + X badge),
- `workflows` (flow diagram: 3 boxes + connectors).

The sub-module `handlers_balancers` reuses the `load-balancer`
drawing (same concept). No other sub-module needed a custom drawing:
all 582 sub-module names map to Tabler icons (186 distinct icons;
names sharing a concept share an icon — see
`docs/integration/SUBMODULE_ICONS.md` for the definition and the
full mapping).

## Provenance

- Module icons: 152 vendored from ColorTicons (Tabler 2.11.0) +
  4 custom-drawn. Full module → Tabler name mapping:
  `docs/integration/COLORTICONS_MAPPING.md`.
- Sub-module icons: 582 names mapped to 186 Tabler icons (+ 1
  custom reuse). Full mapping:
  `docs/integration/SUBMODULE_ICONS.md`.
- Brand icons: 11 real marks from
  [simple-icons](https://github.com/simple-icons/simple-icons)
  (CC0) + 3 lettermark fallbacks (Microsoft, Slack, Amazon —
  removed from simple-icons 16.x). See `brands/README.md`.
- Payment logos: 11 real simple-icons marks + 14 lettermarks.
  See `src/modules/payments/logos/README.md`.
- Licences: ColorTicons/Tabler MIT (`LICENSE.colorticons`);
  simple-icons CC0 (brand marks are trademarks of their owners,
  used only to identify their services; no endorsement implied);
  flag-icons MIT (`flags/LICENSE.flag-icons`).
- The static SVGs are the source of truth; the vendor scripts
  (`/tmp/vendor_colorticons.py`, `/tmp/vendor_submodules.py`,
  `/tmp/fetch_brand_marks.py`) are not shipped in the repo.

## Colour — one set for light and dark

There is no palette to maintain: strokes are `currentColor`, so an
icon is whatever colour its surrounding text is. On a light
background the icons render in the page text colour; on Editorial
Dark they render in the dark theme's text colour. Verify any new
icon renders on both backgrounds before reporting done.

## Grayscale mode — retired

The old flat icons followed CSS variables (`--icon-*`) and a parent
could set `data-icon-theme="grayscale"` to recolour them. Tabler
icons inherit `color` instead, so grayscale is just a grey text
colour on the parent — no special mechanism, and no CSS `filter`
on icons. The `--icon-*` variables remain defined in
`assets/css/icons.css` but nothing reads them.

## AI family (no sparkles)

- `ai`: brain (Tabler `brain`).
- `ai-support`: robot (Tabler `robot`), not a star.
- `advisor`: help (Tabler `help`); never a plain circle.
- `mcp`: plug (Tabler `plug`); never a star.

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

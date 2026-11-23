# Multi-Colour Icon Sets — Research & Replacement Proposal

Date: 2026-11-23
Status: IMPLEMENTED 2026-11-23 — all 156 module icons replaced with
multi-colour IconPark icons (151 vendored) + 5 custom-drawn gap icons.
See §8 for the implementation record. Nothing committed or pushed yet
(parent does the push).
Context: the 156 module icons in `assets/icons/modules/*.svg` (built into
`assets/icons/modules-sprite.svg`) are flat fill-only vectors, mostly a single
colour via `var(--icon-*, #hex)`. The user finds them ugly and wants
MULTI-COLOUR icons instead. This document researches open-source sets in other
people's public repositories and proposes a replacement plan.

## 0. Honest headline

**No MIT-licensed, general-purpose, genuinely multi-colour icon set covering
156 admin/ERP module concepts exists.**

The only MIT multi-colour sets found are:

- **Emoji sets** (Fluent Emoji — MIT, verified): pictorial, weak on abstract
  admin concepts.
- **Brand/dev-logo sets** (Devicon — MIT, verified): multi-colour, but only
  technology brand logos — useless for module concepts.

Everything else that is genuinely multi-colour and general-purpose is
Apache 2.0 (IconPark — the only real candidate), CC-BY 4.0 / CC-BY-SA 4.0
(emoji sets), or CC0 (3dicons — PNG only, tiny catalogue). The two-tone MIT
sets (Phosphor Duotone, Ant Design TwoTone) are MIT but NOT multi-colour —
they are two opacities of one colour family.

Every license claim below was verified by reading the actual LICENSE file (or
the license section of the README where no LICENSE file exists) in the source
repository. Second-hand claims are flagged as such and were NOT trusted.

## 1. Licence truth table

| Set | Licence (verified) | MIT? | Commercial use | Modification | What it costs vs MIT |
|---|---|---|---|---|---|
| IconPark (ByteDance) | Apache 2.0 | No | Yes | Yes | Keep the Apache notice + state changes; includes an express patent grant (MIT has none — Apache is stronger protection here). No UI attribution required. |
| Fluent Emoji (Microsoft) | MIT | **Yes** | Yes | Yes | Nothing — the MIT terms only (keep the notice). |
| Twemoji (jdecked/twemoji) | Code MIT, graphics CC-BY 4.0 | No (art) | Yes | Yes | Attribution required for the graphics (a credit line in README / About / footer). |
| Noto Emoji (Google) | Split: root LICENSE file is OFL-1.1; README says fonts OFL-1.1, tools + most image resources Apache 2.0, flags public domain; 3D PNG licence unstated | No | Yes (images, under Apache 2.0 per README) | Yes | Messy split licence; the 2D SVG images are Apache 2.0 only per a README sentence, not a LICENSE file. |
| OpenMoji | Graphics CC-BY-SA 4.0, code LGPL-3.0 | No | Yes | Yes | Attribution + **ShareAlike**: any modified/redrawn glyph must be shared under CC-BY-SA 4.0. Bad fit for a proprietary product that will customise icons. |
| 3dicons (realvjy) | CC0 1.0 (public domain) | No (CC0 — more permissive than MIT in practice) | Yes | Yes | Nothing, but PNG only — cannot enter the SVG sprite. |
| Phosphor Icons | MIT | **Yes** | Yes | Yes | Nothing — but duotone is two tones of one colour, NOT multi-colour. |
| Ant Design Icons | MIT | **Yes** | Yes | Yes | Nothing — but TwoTone is two tones of one colour, NOT multi-colour. |
| Devicon | MIT | **Yes** | Yes | Yes | Nothing — but brand logos only; ~0 module concepts covered. |
| Hugeicons (free set) | MIT | **Yes** | Yes | Yes | Nothing — but the free set is ONE monochrome stroke style; duotone/twotone styles are Pro (paid). |
| Solar Icons (480 Design) | CC-BY 4.0 claimed by Iconify + Figma page; **no LICENSE file in the 480 Design repo**; npm `solar-icon-set` ships a full GPL-3.0 file (third-party package) | No | Yes (if the CC-BY claim holds) | Yes (with credit) | **Provenance problem**: the licence cannot be verified at the source repo. Also duotone only, not multi-colour. Do not adopt. |
| SVG Nexus "3D Vector Icons" | MIT claimed by the SVG Nexus aggregator page | Unverified | Claimed yes | Claimed yes | No source repository / LICENSE file found to verify. Visible catalogue is heavily AI-concept themed. Do not adopt without per-icon upstream verification. |
| Reicon | MIT label on its own LICENSE file | Labelled MIT, provenance muddy | Claimed yes | Claimed yes | README credits base elements from Solar Icons (CC-BY 4.0) and Zappicon (commercial licence) — derived artwork likely still carries those obligations. Also monochrome. Rejected. |
| Pepicons Pop! | CC-BY 4.0 | No | Yes | Yes | Icons are drawn in `currentColor` — single-colour, NOT multi-colour. Rejected as a multi-colour candidate. |

Sets excluded as monochrome (not multi-colour), even though MIT:
Fluent UI System Icons, Tabler, Heroicons, Lucide (ISC), Iconoir, Feather,
Bootstrap Icons, Material Symbols (Apache 2.0), Carbon (Apache 2.0),
Octicons, Remix Icon (Apache 2.0), Boxicons, Eva, Radix, CSS.gg, MingCute
(Apache 2.0), Entypo (CC-BY-SA 4.0), Game-icons (CC-BY 3.0),
Font Awesome Free (CC-BY 4.0; duotone is Pro), Simple Icons (CC0 — brand
marks, already used for the mail logos).

## 2. Ranked candidates (best first)

### #1 — IconPark (ByteDance) — Apache 2.0 — RECOMMENDED

- Source: https://github.com/bytedance/IconPark
- Licence: Apache 2.0 — verified by reading the LICENSE file in full
  (Copyright 2019-present Bytedance Inc.; GitHub license field `apache-2.0`).
  Commercial use and modification allowed. Keep the licence notice with the
  redistributed files and state changes; no attribution in the UI.
- Size: 2,658 Outline icons, 1,947 Solid, 1,944 TwoTone. One source
  generates 4 themes: outline, filled, two-tone, **multi-color**.
- Multi-color theme: 4 fill colours, default
  `['#333', '#2F88FF', '#FFF', '#43CCF8']` — the fill array is passed at
  generation time, so the palette is chosen once when the SVGs are generated
  and is then fixed in the shipped files.
- Format: SVG, 48x48 grid, stroke width 4.
- Vendoring: npm `@icon-park/svg` — the JS API returns an SVG string per
  theme (`Home({ theme: 'multi-color', fill: [...] })`). A small Node script
  generates static SVG files for the chosen names. The raw `source/` SVGs in
  the repo are single-colour outlines — multi-color is applied by the JS API,
  so the npm generation route is the correct one. (Iconify splits IconPark
  into 4 sets, but the multi-color theme is NOT available as an Iconify set —
  it only exists through the IconPark JS API.)
- Caveat: the repo is **ARCHIVED** (public archive, 9,032 stars, last commit
  8dc132da) — no new icons or maintenance ever. Acceptable for a frozen
  vendor: 2,658 icons is a complete catalogue for this purpose. Pin the
  version and vendor the generated files.
- Coverage estimate: **~125-135 of 156 modules** have a direct or near icon;
  **~20-30 need custom drawing** or a loose metaphor (see §3).

### #2 — Fluent Emoji (Microsoft) — MIT — the only MIT multi-colour option

- Source: https://github.com/microsoft/fluentui-emoji
- Licence: MIT — verified by reading the LICENSE file in full (Copyright (c)
  Microsoft Corporation). Commercial use and modification allowed; keep the
  notice.
- Size: ~1,538 glyphs, SVG (flat style) + PNG (3D style), styles 3D / flat /
  high-contrast.
- Format: SVG available (flat style); 3D style is PNG-heavy. Assets are filed
  per glyph under `assets/<Name>/`.
- Vendoring: copy the needed glyphs from the repo; keep the MIT LICENSE file
  with them.
- Coverage estimate: **~45-60 of 156 modules** have a usable emoji metaphor;
  **~95+ would still need custom drawing**. Emoji are pictorial — they cover
  concrete things (bank, map, brain, books, receipt, calendar) and fail on
  abstract admin concepts (see §3). At 16-20 px UI size, emoji glyphs are
  muddy, and the 3D style reads as stickers, not UI icons.
- Verdict: the only true MIT multi-colour general set — but it cannot carry
  the system. Best kept as a supplement for a handful of pictorial concepts
  if the user insists on having an MIT piece in the mix.

### #3 — Twemoji (jdecked/twemoji, maintained fork of twitter/twemoji) — graphics CC-BY 4.0

- Source: https://github.com/jdecked/twemoji
- Licence: verified at the repo root — LICENSE (code, MIT) + LICENSE-GRAPHICS
  (graphics, CC-BY 4.0); README states it explicitly. Commercial use and
  modification allowed; **attribution required** for the graphics (a credit
  line in README / About / footer is enough).
- Size: ~1,900 emoji, flat colour SVG under `assets/svg/`, Unicode 17.0.
- Coverage estimate: same shape as Fluent Emoji, **~45-60 of 156** usable;
  ~95+ custom.
- Verdict: same coverage problem as Fluent Emoji, plus an attribution
  obligation, and NOT MIT for the artwork. Only worth considering if the flat
  Twemoji style is preferred over Fluent's flat style.

### #4 — Noto Emoji (Google) — split licence (Apache 2.0 images per README / OFL-1.1 fonts)

- Source: https://github.com/googlefonts/noto-emoji
- Licence: GitHub license field OFL-1.1; the root LICENSE file is the SIL Open
  Font License. README licence section: "Emoji fonts (under the fonts
  subdirectory) are under the SIL Open Font License, version 1.1. Tools and
  most image resources are under the Apache license, version 2.0. Flag images
  under third_party/region-flags are in the public domain." The 3D PNGs have
  no licence file of their own.
- Size: 1,923 emoji, flat colour SVG under `2D/` + 3D PNGs.
- Coverage estimate: same emoji shape, **~45-60 of 156** usable.
- Verdict: below Twemoji — the licence split is messy and the 3D PNG terms
  are unstated. Not recommended.

### #5 — OpenMoji — CC-BY-SA 4.0 — not recommended

- Source: https://github.com/hfg-gmuend/openmoji
- Licence: verified — graphics CC-BY-SA 4.0, code LGPL-3.0 (README + LICENSE
  files). Commercial use and modification allowed, but **ShareAlike binds
  derivatives**: a recoloured or redrawn glyph must go out under CC-BY-SA 4.0.
  Using unchanged glyphs does not put the app under that licence (per their
  FAQ), but any customisation of the artwork triggers it.
- Size: ~4,000-4,500 glyphs including non-Unicode extras; color/ + black/
  SVG + PNG; npm `openmoji`.
- Coverage estimate: same emoji shape, **~45-60 of 156** usable.
- Verdict: the ShareAlike term is a bad fit for a proprietary product that
  will customise icons. Last among the emoji options.

### #6 — 3dicons (realvjy) — CC0 — niche use only

- Source: https://github.com/realvjy/3dicons (site 3dicons.co)
- Licence: CC0 1.0 — verified (GitHub license field CC0-1.0; site states CC0
  for V1). Public domain: no attribution, commercial OK, modify OK.
- Size: 122 icons V1 (1,464 renders) / 200 icons; Blender-rendered 3D icons
  in 4 styles (color, clay, gradient, premium) and 3 camera angles.
- Format: **PNG only** (plus BLEND / glTF / C4D / OBJ source files, Figma) —
  **no SVG**, so it cannot enter the SVG sprite system.
- Coverage estimate: **~15-25 usable concepts at most**.
- Verdict: zero licence burden, but PNG-only and a tiny catalogue. Use case
  is hero / marketing art, not module icons.

### #7 — Phosphor Duotone / Ant Design TwoTone — MIT, but NOT multi-colour (the MIT compromise)

- Phosphor Icons (https://github.com/phosphor-icons/core): MIT verified
  (LICENSE file at root; README "MIT (c) Phosphor Icons"). 1,512 icons x 6
  weights (thin, light, regular, bold, fill, duotone) = 9,072 SVGs; assets
  under `assets/<weight>/<kebab-name>-<weight>.svg`; npm
  `@phosphor-icons/core` 2.1.1 (1.4 MB tarball); viewBox 0 0 256 256.
  Duotone = a second `<path opacity="0.2">`, both paths `currentColor` —
  two tones of ONE colour, not multi-colour.
- Ant Design Icons (https://github.com/ant-design/ant-design-icons): MIT
  verified (LICENSE file at root; README links the MIT License). 1,870 icons
  (840 distinct x 3 styles: Outlined, Filled, TwoTone). TwoTone = primary
  form + secondary pass at lower opacity, themeable via the `twoToneColor`
  prop; viewBox 1024x1024; npm `@ant-design/icons-svg`.
- Coverage estimate: Phosphor **~140-150 of 156**, Ant Design **~120-135 of
  156** — the best coverage of any MIT set, because both are general-purpose
  UI icon libraries.
- Verdict: if the user accepts **two-tone** (one colour + a lighter pass of
  the same colour) instead of full multi-colour, Phosphor Duotone is the best
  MIT answer: huge coverage, MIT, and a big visual upgrade over the current
  flat single-colour icons. It is NOT what the user asked for (multi-colour),
  so it is the fallback, not the recommendation.

### #8 — Devicon — MIT, multi-colour, but brands only

- Source: https://github.com/devicons/devicon
- Licence: MIT verified (LICENSE file at root). README brand-policy caveat:
  brand marks remain the property of their owners; use per each brand's
  policy.
- Size: 800+ tech logos (1,623 SVGs indexed), variants original (full colour)
  / plain / line, wordmark optional; SVG viewBox 0 0 128 128; `icons/` dir
  per technology.
- Coverage estimate: **~0 module concepts**. Multi-colour, MIT, and useless
  for this purpose — except as a source of real brand marks (the same role
  simple-icons already plays for the mail logos).
- Verdict: not a module-icon candidate.

### Rejected / not ranked

- **Solar Icons (480 Design)**: duotone only (second colour at 20%
  opacity), NOT multi-colour; and the licence cannot be verified at the
  source — the 480 Design GitHub repo has NO license file, the CC-BY 4.0
  claim comes from Iconify metadata and the Figma page (which blocked
  verification), and the npm package `solar-icon-set` ships a full GPL-3.0
  license file (third-party package, not official). Do not adopt.
- **Reicon**: MIT label on its own LICENSE file, but the README credits base
  elements from Solar Icons (CC-BY 4.0) and Zappicon (commercial licence) —
  derived artwork likely still carries those obligations. Also monochrome
  (Iconify: Palette Colorless). Rejected.
- **Pepicons Pop!**: CC-BY 4.0, but icons are drawn in `currentColor` —
  single-colour, not multi-colour. Rejected.
- **Hugeicons free set**: MIT verified (LICENSE.md at root; README: "The
  free icons (@hugeicons/core-free-icons) and all source code in this
  repository are released under the MIT License"), but the free set is ONE
  style only — Stroke Rounded, monochrome. The 10 styles (Stroke, Solid,
  Bulk, Duotone, Twotone, 54,000-60,000+ icons) are Pro (paid, $99/yr; Pro
  icons may not be redistributed in open-source code). Not a multi-colour
  answer.
- **Streamline / Lordicon**: coloured styles are paid / not open source.
  Not candidates.
- **SVG Nexus "3D Vector Icons"**: MIT claimed by the aggregator page
  ("The files are published under the MIT Open Source License (MIT)"), 1,692
  icons, SVG — but no source repository or LICENSE file was found to verify
  the claim, and the visible catalogue is heavily AI-concept themed
  ("AI Autonomous Reasoning Loop", "AI Bimodal Perception Unit"). Do not
  adopt without per-icon upstream verification.

## 3. Coverage spot-check — the hard concepts

Legend: Y = direct icon exists, ~ = usable approximation exists,
N = not found in the spot-check (may exist under a different name — confirm
during the mapping step).

| Hard concept | IconPark | Fluent Emoji (MIT) | Twemoji / Noto / OpenMoji | Phosphor Duotone (MIT) |
|---|---|---|---|---|
| Load balancer | N — `balance` / `balance-one` / `balance-two` are weighing scales (the SAME trap as the old icon). Nearest: `branch` (splitting arrows) / `server` — custom or composite | N | N | ~ (`arrows-split` / `network` family — check in mapping) |
| Vault / safe | N — the "Safe" category list (alarm, balance-two, bug, fire-extinguisher, key, keyhole, protect, protection, shield, umbrella) contains no safe/vault box. Nearest: `bank`, `lock` — or custom | ~ (lock, bank) | ~ (lock, bank) | Y (`vault` exists in Phosphor) |
| Tenant destroy | N — custom (building + X, in IconPark style) | N | N | N — custom |
| Geofence / map | ~ (`fence-one` / `fence-two` + `local` / `local-pin` / `pushpin` / `bullet-map`) | ~ (map, pin) | ~ (map, pin) | ~ (`map-pin`, `fence` — check in mapping) |
| AI / neural network | Y (`brain`, `blockchain`) | Y (brain) | Y (brain) | Y (`brain`) |
| Site builder | N — custom. Nearest: `browser`, `web-page`, `layout` (unconfirmed) | N | N | ~ (`browser`, `layout`) |
| Licensing | ~ (`certificate`, `seal`) | ~ (scroll) | ~ (scroll) | Y (`certificate`, `seal`) |
| Billing / invoice | Y (`bill`, `bank-card`) | Y (receipt, card) | Y (receipt, card) | Y (`receipt`, `invoice`) |
| HR attendance | ~ (`clock`, `calendar`, `alarm-clock`) | Y (clock, calendar) | Y (clock, calendar) | Y (`clock`, `calendar-check`) |
| Exams | ~ (`blackboard`, `book` — an icon literally named `exam` was not confirmed in the spot-check) | ~ (graduation cap, memo) | ~ (graduation cap, memo) | ~ (`exam` — check in mapping) |
| Library | ~ (`bookshelf`, `book-open` — an icon literally named `library` was not confirmed) | Y (books) | Y (books) | Y (`books`) |

Other concepts NOT covered by any general set (custom drawing needed in
every option): webhook, entitlements, saas-packages, mcp, vcs, whitelabel,
spreadsheets, statuspage, okrs, plm, esg, kyc, api-manager. (IconPark has
`api` / `plug` / `link` family members that may approximate webhook — confirm
in the mapping step; none was verified in the spot-check.)

Coverage estimates (state as estimates, from the spot-checks above):

- IconPark multi-color: **~125-135 of 156** direct/near; ~20-30 custom.
- Fluent Emoji / Twemoji / Noto / OpenMoji: **~45-60 of 156** usable
  metaphors; ~95+ custom.
- 3dicons: **~15-25** usable concepts.
- Phosphor Duotone (MIT, two-tone): **~140-150 of 156**.
- Ant Design TwoTone (MIT, two-tone): **~120-135 of 156**.

## 4. Practical integration notes

### 4.1 IconPark (recommended)

- **Generation**: run a small Node script with `@icon-park/svg`:
  `IconName({ theme: 'multi-color', fill: ['#333', '#2F88FF', '#FFF', '#43CCF8'] })`
  returns the SVG string per icon. Emit one static file per module, named to
  the existing kebab module id, into `assets/icons/modules/`.
- **Palette decision (do before generating)**: the default palette uses
  `#333` outlines. On a dark background `#333` strokes disappear and white
  fills glow. Options: (a) ship one palette tested on both light and dark
  (pick outlines that survive both, e.g. a mid grey-blue instead of `#333`);
  (b) generate a second dark sprite with light outlines and switch sprites
  with the theme. Decide before building — this is the one real design
  decision in the plan.
- **Sprite system**: the existing builder
  (`scripts/build-module-icons-sprite.py`) scans
  `assets/icons/modules/*.svg` + module-owned `icon.svg` files and emits one
  `<symbol id="module-icon-<name>" viewBox="0 0 24 24">` per icon, with
  `symbol fill="none"` and per-module `data-animation`. Changes needed:
  - viewBox becomes `0 0 48 48` (IconPark grid).
  - The flat-fill-only style rules in `assets/icons/README.md` must be
    updated: allow `stroke` (IconPark icons use stroke + fill), drop the
    "max 2 flat colours" cap, drop the "drawings inside 3-21" geometry rule
    (IconPark uses the 48 grid with stroke width 4).
  - `symbol fill="none"` still works: each shape carries its own fill/stroke.
  - Per-symbol `data-animation` (spin/wiggle/pulse/bounce) is unaffected.
  - The HBS/React usage (`<use href="#module-icon-<name>">`) is unchanged.
- **Theming**: multi-colour fills are FIXED at generation time — they do not
  follow CSS variables the way the current `var(--icon-*, #hex)` fills do.
  The icons will look the same in light and dark mode (that is the point of
  multi-colour), but dark-mode contrast must be checked per icon (see the
  palette decision above).
- **Custom gap icons**: draw the ~20-30 missing concepts in the same
  IconPark visual language (48x48 grid, stroke width 4, same 4-colour
  palette) so the set stays consistent. The current custom load-balancer
  concept (arrow splitting into server bars) should be redrawn in this style
  rather than kept as-is.
- **Licence compliance**: keep the Apache-2.0 LICENSE + NOTICE files in
  `assets/icons/` (or `assets/icons/LICENSES/`), keep the notice with the
  redistributed files, and record the change in the sprite. No attribution
  in the UI is required.
- **Weight**: IconPark multi-color SVGs are heavier than the current flat
  files (multiple paths + stroke attributes). The sprite will grow; measure
  the built sprite size before shipping and confirm it stays acceptable for
  the dashboard (the current sprite is 156 symbols; the generated one will
  be larger per symbol).
- **Module-owned overrides**: the current mechanism (module-owned
  `src/modules/*/icon.svg` feeding the sprite) stays — a module can still
  ship its own multi-colour icon; it must follow the same 48-grid + palette
  rules.

### 4.2 Fluent Emoji (MIT supplement, if wanted)

- Copy the needed glyphs from `assets/<Name>/` in the repo (flat style SVG;
  3D style is PNG). Keep the MIT LICENSE file with them.
- Does NOT fit the sprite system as-is: emoji SVGs use their own viewBox
  and fixed colours, and the flat style is a different visual language from
  the rest of the UI. If used at all, use it for a handful of pictorial
  concepts only — do not mix emoji and IconPark styles in the same nav.
- At 16-20 px the glyphs lose detail; test legibility at the actual sizes
  used (sidebar 20 px, cards 24 px).

### 4.3 Phosphor Duotone / Ant Design TwoTone (MIT fallback)

- Vendoring: Phosphor via npm `@phosphor-icons/core` (1.4 MB tarball,
  `assets/<weight>/<kebab-name>-<weight>.svg`, viewBox 0 0 256 256) or copy
  from the repo; Ant via npm `@ant-design/icons-svg` (viewBox 1024x1024).
- Sprite integration: both are `currentColor`-based, so they fit the existing
  sprite system almost unchanged — only the viewBox in the symbol changes
  (256 or 1024 instead of 24), and the two-tone effect comes from the
  second path's `opacity="0.2"`.
- Theming: duotone follows `currentColor`, so it themes with the UI like the
  current icons do. Dark mode works for free.
- Cost: it is NOT multi-colour. If the user wants colour, this does not
  deliver it.

## 5. Recommendation

**Adopt IconPark multi-color (Apache 2.0) as the module icon system, with
custom-drawn gap icons in the same visual language.**

Why, in one paragraph: it is the only general-purpose, genuinely
multi-colour, commercially usable set with deep coverage (~125-135 of 156
modules), and Apache 2.0 is as permissive as MIT for this use — commercial
use and modification allowed, no UI attribution, plus an express patent grant
MIT does not have. The MIT alternative (Fluent Emoji) would leave ~95
modules to custom drawing and would look like stickers at UI sizes. The
two-tone MIT sets are a real fallback if the user prefers MIT purity over
full colour, but they are not what was asked for.

If the user wants MIT purity AND multi-colour, the honest answer is that the
combination does not exist for this domain — the closest MIT multi-colour
artefacts are emoji (Fluent Emoji) and brand logos (Devicon), neither of
which can carry 156 admin modules.

## 6. Next steps (if the user approves)

1. **User decision**: IconPark multi-color (recommended) vs Phosphor Duotone
   (MIT, two-tone compromise) vs Fluent Emoji supplement only.
2. **Palette decision**: one palette for both themes, or a second dark sprite
   with light outlines. Decide before generating.
3. **Mapping table**: build the 156-name mapping (IconPark icon name, or
   "custom") and review it before any drawing — this is the step where the
   coverage estimates above become exact.
4. **Generate**: Node script with `@icon-park/svg` (pinned version),
   multi-color theme, fixed fill palette; emit static SVGs into
   `assets/icons/modules/` under the existing kebab module ids.
5. **Custom-draw** the ~20-30 gap icons in IconPark style (48 grid, stroke
   4, same palette), including load balancer, vault, tenant destroy,
   site builder, webhook, and the other concepts in §3 with no direct icon.
6. **Update the sprite builder + style rules**: viewBox 48, allow stroke,
   drop the 2-colour cap, update `assets/icons/README.md`; regenerate
   `assets/icons/modules-sprite.svg`; verify the symbol count (156).
7. **Verify**: render the sprite on light + dark (Default + Editorial Dark),
   check the demo pages and the HBS/React dashboards at real sizes, measure
   the sprite size, and regenerate `assets/icons/ICONS.md` (the preview).
8. **Licence files**: place the Apache-2.0 LICENSE + NOTICE in
   `assets/icons/` and record the provenance in the README.
9. **Commit + push** (parent does the push; this task does not).

## 7. Sources verified (2026-11-23)

- IconPark LICENSE: https://github.com/bytedance/IconPark/blob/master/LICENSE
  (Apache 2.0, Copyright 2019-present Bytedance Inc.; repo archived).
- Fluent Emoji LICENSE: https://github.com/microsoft/fluentui-emoji/blob/main/LICENSE
  (MIT, Copyright (c) Microsoft Corporation).
- Twemoji: https://github.com/jdecked/twemoji — LICENSE (MIT, code) +
  LICENSE-GRAPHICS (CC-BY 4.0, graphics); README states both.
- Noto Emoji: https://github.com/googlefonts/noto-emoji — GitHub license
  OFL-1.1; README licence section (fonts OFL-1.1, tools + most image
  resources Apache 2.0, flags public domain).
- OpenMoji: https://github.com/hfg-gmuend/openmoji — README + LICENSE.txt
  (graphics CC-BY-SA 4.0, code LGPL-3.0).
- Phosphor: https://github.com/phosphor-icons/core — LICENSE file at root,
  README "MIT (c) Phosphor Icons".
- Ant Design Icons: https://github.com/ant-design/ant-design-icons —
  LICENSE file at root, README "MIT License".
- Devicon: https://github.com/devicons/devicon — LICENSE file at root (MIT).
- Hugeicons: https://github.com/hugeicons/hugeicons — LICENSE.md at root;
  README (free icons + all source code MIT; free set = Stroke Rounded only).
- Solar: https://github.com/480-Design/Solar-Icon-Set — no LICENSE file in
  the repo; CC-BY 4.0 claim from Iconify metadata + Figma page; npm
  `solar-icon-set` ships GPL-3.0 (third-party).
- 3dicons: https://github.com/realvjy/3dicons — GitHub license CC0-1.0.
- SVG Nexus 3D Vector Icons: https://www.svgnexus.com/en/icons/3d-svg-icons
  — MIT claimed by the aggregator; no source repo / LICENSE file verified.
- Reicon: https://github.com/dqev/reicon — LICENSE file exists (MIT label);
  README credits Solar Icons (CC-BY 4.0) and Zappicon (commercial licence)
  as base elements.
- IconPark coverage spot-checks: Iconify IconPark collections
  (icon-sets.iconify.design/icon-park/ — Outline 2,658, Solid 1,947,
  TwoTone 1,944); IconPark package docs (@icon-park/svg, @icon-park/react,
  @icon-park/vue); fyne-iconpark Safe-category list; Dart icon-park
  `iconsList` sample.

## 8. Implementation record (2026-11-23)

Decision: IconPark multi-color (Apache 2.0) adopted as planned in §5.
151 icons vendored from `@icon-park/svg` 1.4.2 (pinned), theme
`multi-color`; 5 custom-drawn in the same visual language. No icon
was changed in the mail logos or the flags.

### 8.1 Palette decision — ONE palette for light and dark

Option (a) from §4.1: a single palette chosen to read on both the
light theme (`--bg: #f4f6f8`) and Editorial Dark (`--bg: #141210`).
IconPark's default `#333` outline was replaced with slate `#64748B`
(~4.8:1 on light, ~3.9:1 on dark); the other three fills are the
IconPark defaults. No second dark sprite is generated.

| Fill | Value | Role |
|---|---|---|
| 1 — outline | `#64748B` | Strokes, outlines, neutral structure |
| 2 — main | `#2F88FF` | Main colour, primary meaning |
| 3 — light | `#FFFFFF` | Inner detail on coloured shapes |
| 4 — accent | `#43CCF8` | Secondary colour |

Exception: `tenant-destroy` uses danger red `#B3261E` for its X badge
(the only red in the set — a destructive action must read as destructive).
Rendered grids on both backgrounds were inspected at 48 px and 16 px:
all 156 icons render, outlines stay visible on dark, no blanks.

### 8.2 Custom-drawn gap icons (5)

| Module | Design (48 grid, stroke 4, same palette) |
|---|---|
| `load-balancer` | Down arrow splitting from one node into three server bars |
| `vault` | Safe box with blue door and dial |
| `tenant-destroy` | Building + red X badge (only red in the set) |
| `site-builder` | Browser window with layout blocks |
| `placeholder` | Circle with a "?" |

### 8.3 Mapping — module id → IconPark name (or CUSTOM)

| Module | IconPark icon | Module | IconPark icon |
|---|---|---|---|
| `academic` | school | `admissions` | add-user |
| `advisor` | help | `affiliates` | link-one |
| `ai` | brain | `ai-support` | robot |
| `analytics` | chart-line | `announcements` | announcement |
| `api-manager` | api | `apikeys` | keyhole |
| `app-sync` | sync | `appointments` | appointment |
| `approvals` | stamp | `assets` | box |
| `audit` | audit | `auditors` | people-search |
| `auth-manager` | fingerprint | `backup` | copy |
| `bank-transfer` | bank-transfer | `bi` | chart-pie |
| `billing` | bill | `cache` | refresh |
| `calendar` | calendar | `cards` | bank-card |
| `certificates` | certificate | `chat` | messages |
| `cloudflare` | shield-add | `clusters` | network-drive |
| `community` | group | `company` | building-one |
| `compliance` | protect | `content` | file-text |
| `contracts` | agreement | `crm` | customer |
| `curriculum` | book | `dashboards` | dashboard |
| `delivery-notes` | express-delivery | `departments` | headset |
| `documents` | document-folder | `domain-verification` | check-correct |
| `domains` | globe | `downloader` | download |
| `elearning` | monitor-two | `email-providers` | send-email |
| `email-templates` | mail-edit | `entitlements` | key |
| `esg` | leaf | `events` | ticket |
| `exams` | notebook-and-pen | `expenses` | expenses |
| `extensions` | puzzle | `external-bi` | data |
| `external-esignature` | doc-detail | `external-fleet` | car |
| `external-iot` | wifi | `external-whatsapp` | message-one |
| `field-service` | toolkit | `files` | folder |
| `forms` | form | `forums` | comments |
| `frontend` | browser | `gamification` | game |
| `geo` | map-two | `holidays` | sun |
| `hosting` | server | `hr` | people |
| `i18n` | translate | `import-export` | import-and-export |
| `inbox` | inbox | `incidents` | alarm |
| `knowledgebase` | book-open | `kyc` | id-card |
| `library` | bookshelf | `licensing` | seal |
| `load-balancer` | CUSTOM | `logistics` | truck |
| `logs` | record | `loyalty` | gift |
| `mail` | mail | `maintenance` | repair |
| `manufacturing` | factory-building | `marketing` | broadcast |
| `mcp` | data-server | `meetings` | video |
| `network` | network-tree | `network-tools` | tool |
| `newsletter` | mail-package | `notifications` | remind |
| `okrs` | target | `panels` | monitor |
| `payments` | wallet | `phone` | phone |
| `placeholder` | CUSTOM | `planner` | notebook |
| `plm` | cube | `portals` | gate |
| `pos` | shopping-cart | `presets` | setting-one |
| `pricing` | tag | `pricing-tables` | table-file |
| `printing` | printer | `privacy` | personal-privacy |
| `procurement` | shopping-bag | `products` | commodity |
| `projects` | folder-block | `properties` | home |
| `providers` | link | `quality` | badge |
| `records` | collection-records | `recruitment` | briefcase |
| `reference-data` | data-sheet | `referral` | people-plus |
| `rental` | coupon | `risk` | attention |
| `roles` | user-business | `routes` | road |
| `saas` | link-cloud | `saas-domains` | earth |
| `saas-packages` | gift-box | `saved-views` | bookmark |
| `scheduler` | time | `search` | search |
| `security` | shield | `seo` | seo |
| `service-monitor` | radar | `settings` | setting |
| `sign` | edit | `site-builder` | CUSTOM |
| `sms-marketing` | message | `social-marketing` | share |
| `software` | code | `spreadsheets` | table |
| `statuspage` | monitor-one | `storage` | cloud-storage |
| `storage-manager` | database-setting | `store` | shop |
| `studio` | paint | `supervision` | eyes |
| `surveys` | clipboard | `tasks` | check |
| `tax` | calculator | `tenant-destroy` | CUSTOM |
| `tenants` | building-two | `theme` | theme |
| `tickets` | tickets-one | `timeline` | timeline |
| `timetracker` | timer | `todos` | list |
| `users` | user | `vault` | CUSTOM |
| `vcs` | branch | `web3` | blockchain |
| `webhooks` | plug | `webinars` | video-conference |
| `whitelabel` | magic | `workflows` | mindmap-list |

Notes on the loose metaphors: `whitelabel` → `magic` (rebrand like magic);
`departments` → `headset` (support agents); `holidays` → `sun`;
`saas` → `link-cloud`; `risk` → `attention`; `rental` → `coupon`;
`projects` → `folder-block`; `workflows` → `mindmap-list`;
`mcp` → `data-server`; `vcs` → `branch`; `plm` → `cube`.
Every IconPark name is used once — no two modules share an icon.

### 8.4 Files changed

- `assets/icons/modules/*.svg` — all 156 replaced (151 vendored, 5 custom).
- `assets/icons/modules-sprite.svg` — rebuilt, 156 symbols, viewBox 48.
- `assets/icons/ICONS.md` — regenerated from disk.
- `assets/icons/README.md` — multi-colour style, palette, provenance,
  grayscale mode retired.
- `assets/icons/LICENSE.icon-park` — Apache 2.0 licence from the
  IconPark repo (raw.githubusercontent.com/bytedance/IconPark/master/LICENSE).
- `scripts/build-module-icons-sprite.py` — viewBox 24 → 48, multi-colour
  rules; `fill="none"` on symbols kept (some IconPark paths carry no fill).
- `demo/assets/sprite.js` — regenerated from the new sprite.
- This doc — status + this section.

### 8.5 Verification

- Sprite rebuilds clean: `python3 scripts/build-module-icons-sprite.py` →
  156 symbols, 0 module-owned; every module id has a matching symbol,
  no extras; all symbols viewBox `0 0 48 48`.
- Lint on all 156 module files: well-formed XML, viewBox `0 0 48 48`,
  `<title>` present, no `<text>`, no old `var(--icon-*)` fills, no `24`
  viewBox remnant — zero problems.
- Render grids (resvg) at 48 px and 16 px on light `#f4f6f8` and dark
  `#141210`: all 156 render on both backgrounds, outlines visible on dark,
  no blanks. The 5 custom icons were rendered at 240 px and inspected
  individually.
- The 6 `data-animation` declarations from the old icons were carried over
  (`app-sync`/`backup` spin, `downloader` bounce, `geo`/`service-monitor`
  pulse, `notifications` wiggle) and are present on the built symbols.
- Greps on touched files: no Indonesian strings, no tnode, no WordPress,
  no TrayDigita (one pre-existing "WordPress-plugin model" comment in the
  builder docstring was reworded to "the icon belongs to the module").
- `npm run check` exit 0 (no React code touched; `node --check` on the
  regenerated `demo/assets/sprite.js` also passes).
- Sprite size: 114,130 bytes (was 94,553 — multi-colour paths are heavier,
  still one file, no runtime dependency).

### 8.6 What was NOT done

- Mail logos (`src/modules/email_providers/logos/`) untouched — already real
  brand marks. Flags (`assets/icons/flags/`) untouched.
- The 17 snake_case `fn id()` modules still cannot render icons in-app
  (needs the kebab unification decision); their new icons exist in the
  preview but the app cannot address them yet.
- No commit, no push (parent does that).
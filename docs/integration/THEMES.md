# Themes

Unified has two theme tracks that share one theme switcher:

- **Handlebars track** — server-rendered pages. The embedded default
  theme lives in `templates/default/`; disk themes live in `themes/<id>/`
  with a `theme.yaml` manifest. The engine is `src/themes/`; the HTTP
  admin API is `src/modules/theme/`.
- **React track** — the dashboard SPA under `/react/`, built by Vite
  from `assets/react/` into per-theme bundles under
  `assets/react/dist/<theme_id>/`. A React theme is a disk theme whose
  manifest requires `react` and whose `react/` directory holds the
  theme's components (see below).

The switcher is one source of truth: `localStorage` key
`unified-theme` (`light`/`dark`, OS fallback) plus the `.dark` class
on `<html>`, shared by both tracks. There is no second theme system.

## Theme rule: built on a UI framework, named after it

Themes are built on a UI framework, never from zero, and each theme
is named after its framework:

| Theme id    | Framework   | Status        |
| ----------- | ----------- | ------------- |
| `default`   | none (embedded HBS) | built-in fallback |
| `editorial` | none (hand-built HBS) | available, not default |
| `untitled`  | Untitled UI React (free, MIT) | the React track's theme |
| `chakra`    | Chakra UI v3 (MIT) | phase 2 — not built yet |

`default` and `editorial` stay as-is: they are HBS-only themes and
have no React bundle. `untitled` is the theme the React app is built
on; `chakra` will become the default theme in phase 2 (task 376) and
`editorial` stays available.

## `theme.yaml` manifest

The manifest is YAML, parsed into `ThemeManifest`
(`src/themes/engine/manifest.rs`). The admin HTTP module parses the
same shape (`src/modules/theme/validation.rs` delegates structural
validation to the engine) — there is exactly one manifest shape.

```yaml
name: Untitled
version: 1.0.0
description: ...
screenshot: screenshot.png
directories: [templates, partials, assets, react]
templates: [layout, dashboard, home, error, ...]
template_files: [...]
# Capability requirements (singular key `require`, a LIST):
require: [react]
# UI framework the theme is built on:
framework: untitled
# Theme-relative JS/CSS the theme loads (see Assets below):
assets:
  js: []
  css: [untitled.css]
```

### `require` — a capability list, not booleans

- The key is `require` (singular), a list of capability names.
  Per-feature booleans (`react: true/false`, `maps: ...`) were
  rejected: they explode combinatorially and need a struct change
  per capability. A list is extensible (add a string, no struct
  change) and matches the module pattern (`Module::requires()`).
- The engine validates each name against a known capability set
  (`KNOWN_THEME_REQUIREMENTS` in `manifest.rs`; today exactly one:
  `"react"`, satisfied when the theme's React bundle is built).
- Fail-closed, two ways: an **unknown** name is a validation error
  (`invalid-requirement`); an **unsatisfied** requirement blocks
  theme activation with a clear error (`set_active_theme` refuses,
  HTTP 400) — a theme never silently renders broken.
- **No versioned requires** (`require: {react: "19.x"}` was rejected).
  Versions live in each theme's `package.json` (npm pins them at
  build time, e.g. `react ^19.3.1`). Version ranges would turn the
  manifest into a package resolver; multi-version React runtimes on
  one page break hooks.

### `framework`

A plain string naming the UI framework (`untitled`, `chakra`, empty
for the HBS-only themes). It is metadata + documentation: the engine
does not install or resolve frameworks at runtime — the build does.

### `assets` — theme-relative only

- `assets.js` / `assets.css` are paths **relative to the theme's
  own `assets/` directory**, resolved through the existing `theme:`
  mechanism (`asset_url` helper). Paths are validated
  (`validate_theme_asset_path`): no leading `/`, no `\`, no `:`, no
  `..`/empty segments, `.js` must end `.js`, `.css` must end `.css`.
- A declared asset file that does not exist is a validation error
  (`missing-asset`). Unknown manifest keys warn
  (`unknown-manifest-field`); `require`/`framework`/`assets` are
  known keys.
- **No external URLs from the manifest in this round.** An allowlist
  for external asset hosts is a future step if one is ever needed.
- At render time the engine injects the active theme's assets into
  the template context as `theme_styles` / `theme_scripts` (resolved
  URLs), plus the full `active_theme` metadata object,
  `theme_requires_react` (bool) and `theme_framework` (string).
  Layouts render them after their own styles/scripts blocks. The
  existing `theme` template variable is the light/dark scheme — do
  not collide with it; use `active_theme` for theme metadata.
- The embedded default theme and `editorial` get empty lists.

## Per-theme package manager

Each theme carries its **own `package.json` inside the theme dir**
(npm-native), declaring its framework + libs. The root
`package.json` keeps **only** the shared toolchain + scripts (Vite,
TypeScript, Tailwind, check/build scripts) and shared deps
(`react`, `react-dom`, `react-router-dom`, `leaflet`, `grapesjs`).
Per-theme framework deps live in the theme's own `package.json`,
never in the root. The root declares npm workspaces
(`"workspaces": ["themes/*"]`) so one `npm install` resolves them.

Build-time (Vite reads each theme's `package.json`/manifest and
builds per-theme bundles):

- `npm run build:react` — typecheck + build the default theme
  (`untitled`) into `assets/react/dist/untitled/`.
- `npm run build:react:all` — typecheck once, then `vite build`
  per theme dir under `themes/` with a `react/` subdir
  (`scripts/build-react-all.mjs`, `UNIFIED_THEME` per theme),
  output `assets/react/dist/<theme>/`.
- `vite.config.ts`: `UNIFIED_THEME` env (default `untitled`),
  `@theme` alias → `themes/<theme>/react`, `outDir: dist/<theme>`,
  `base: "/react/"`, root stays `assets/react`.

The backend (Rust) **never resolves or installs JS packages at
runtime** — it serves SSR HTML + the API + static built assets
only. A theme whose bundle is not built cannot be activated
(`require: [react]` is unsatisfied) and `/react` answers 404
naming `npm run build:react:all`.

## React serving (`src/http/react.rs`)

`GET /react` and `GET /react/{path}` resolve the bundle from the
active theme through a server-wide `ThemeEngine` (created in
`run_server` with `with_react_dist_dir(assets/react/dist)` and
subscribed to `theme.activated` like the frontend/dashboards
engines, so it tracks activations without a restart):

1. The active theme's own bundle, when it requires `react` and
   that bundle is built.
2. Otherwise the `untitled` bundle — the React app is built on the
   Untitled component library, so Untitled is the React track's
   theme whatever HBS theme the server-rendered pages use.
3. `None` when neither bundle exists → 404 naming the build
   command. A theme whose bundle was deleted after activation falls
   through to Untitled rather than 404ing.

Inside the bundle: the file when it exists, the bundle's
`index.html` for any other path (SPA fallback), 400 on path
traversal (canonicalization is the second check). Hashed Vite
assets are immutable for a year; `index.html` is `no-cache`.

## Component-library contract

Every React theme must expose the same component API surface, so
page code never imports the vendored primitives directly:

- `themes/<id>/react/components/ui.tsx` — `Card`, `StatCard`,
  `Badge`, `Skeleton`, `Alert`, `EmptyState`. (`Alert` is hand-built
  from utility tokens: the Untitled UI free tier has no alerts
  component — alerts is PRO-gated.)
- `themes/<id>/react/components/controls.tsx` — `Button`, `Input`,
  `TextArea`, `Select`, `Checkbox`, `Switch`, `Tabs` (+ `TabPanel`),
  `Table` + `TableCard` (compound), `Pagination`, `Dialog`,
  `DatePicker` (ISO `YYYY-MM-DD` strings in/out), `Tooltip`,
  `Toaster` + `toast` (sonner).

Pages import from `@theme/components/ui` and
`@theme/components/controls`. The `@theme` alias resolves to the
default theme (`untitled`) for typechecking; Vite overrides it per
theme at build time. Props are plain React props (`onClick`, string
`onChange`, ISO date strings) — React Aria conventions (`onPress`,
`DateValue`) are adapted inside the library. A new theme copies
the Untitled library and adapts its tokens; the app typechecks
against the default theme.

### Architecture principle

The theme owns the design tokens and component styles — components
consume the theme, never a hard-coded look. Data is separate:
components get data from the API (`/api/v1/*`) or from JSON
embedded in the DOM on server-rendered pages. The theme never
carries data; data never carries styling.

### What the theme library does NOT cover (intentional)

- Row/card click targets (a whole list row or file card is a
  button), the drawer backdrop, icon-only tree controls
  (expand/collapse, rename/move/delete glyphs), the Grid/List
  `aria-pressed` segmented toggle, inline text links, and the
  hidden `<input type="file">` stay native — they are not UI
  controls, and the theme `Button` has no icon-only variant.
- `DatePicker` has no time support: `datetime` fields are a
  `DatePicker` + a time `Input`, recombined to the same ISO string
  the old `datetime-local` input produced.
- The vendored Untitled `Input` has no `step` prop (React Aria):
  the one number field is controlled (`value`/`onChange` → `Number`)
  with no native form validation, so decimal entry is unaffected.
  The theme `Button` has no `title` prop: tooltips on disabled
  buttons live on a wrapper `<span title=...>`.

## Activation

- `set_active_theme` validates first (structure + `require` +
  declared assets), then switches. `THEME_ACTIVATED_EVENT`
  (`theme.activated`) is emitted by the theme module on activation;
  every engine (frontend, dashboards, React) follows it.
- The default theme id is `default` (embedded). No production code
  defaults to `editorial` — only tests activate it.
- Activation of a disk theme whose manifest cannot be parsed, or
  whose required templates/assets are missing, is refused with the
  validation problems listed (module `validate_theme` delegates
  structural validation to the engine, then adds its stricter admin
  checks: semver version, non-empty description, screenshot
  extension).

## Adding a new theme

1. Create `themes/<id>/` with `theme.yaml` (`require`,
   `framework`, `assets`), `package.json` (framework deps),
   `templates/` (the 16 required HBS templates, copied from
   `templates/default/` and adapted), `partials/`, `assets/`
   (theme CSS), and `react/` (vendored framework components +
   `theme.css` + `components/ui.tsx` + `components/controls.tsx`
   implementing the contract above).
2. `npm install` (workspace picks the theme up), `npm run check`,
   `npm run build:react:all` — the bundle must appear at
   `assets/react/dist/<id>/`.
3. Activate via the theme admin API; the engine refuses activation
   while the bundle is missing.
4. Record the theme in the mapping table at the top of this doc.

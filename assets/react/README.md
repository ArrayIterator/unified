# React track

React + Tailwind frontend, built by Vite and served by the Rust server
under the `/react/` mount. It runs alongside the Handlebars + TypeScript
track; neither track may break the other.

## Layout

- `assets/react/index.html` — Vite app root (HTML entry).
- `assets/react/src/main.tsx` — entry point; mounts `<App />` inside
  `BrowserRouter basename="/react"`.
- `assets/react/src/App.tsx` — dashboard shell (header + sidebar +
  theme toggle + sidebar collapse) with the route table.
- `assets/react/src/api.ts` — API client: standard-envelope fetch
  wrapper (`credentials: include`, cached CSRF token with retry-once
  on a CSRF 403), login (sets the `vault` session cookie), and one
  typed wrapper per endpoint used. Every endpoint is registered in
  the Rust route tables; the file cites the source per call.
- `assets/react/src/sprite.ts` — fetches the module-icon sprite
  (`/assets/icons/modules-sprite.svg`, served as a static asset) once
  and injects it, so pages reuse the Handlebars track's symbols.
- `assets/react/src/components/` — `ui.tsx` (Card, StatCard, Badge,
  Skeleton, Alert, EmptyState), `ModuleIcon.tsx` (sprite symbol, or a
  neutral initials tile when a module has no icon).
- `assets/react/src/pages/` — `DashboardPage.tsx` (user-overview stat
  cards, system-status / tasks / notifications widgets, module grid
  with search), `LoginPage.tsx` (email/password + TOTP/recovery),
  `AdminPage.tsx` (admin overview counts + system, modules table,
  users table, honest links to classic-only sections), and
  `ModuleDetailPage.tsx` (extension detail + enable/disable).
- `assets/react/src/hooks.ts` — `useLoad` data-loading hook
  (loading / ready / error + reload).
- `assets/react/src/theme.ts` — theme handling: `.dark` class on
  `<html>` (Tailwind class-based dark mode), `data-theme` attribute and
  `theme-*` classes for parity with the Handlebars track, persisted in
  localStorage under the shared `unified-theme` key.
- `assets/react/src/index.css` — Tailwind v4 entry. Design tokens mirror
  `assets/css/dashboard.css` (brand green `#0b6b3a`, gold `#c9a227`,
  semantic surface/text/border tokens, light + dark values) via CSS
  variables mapped through `@theme inline`, so both tracks look like
  one product. The editorial theme variant is not mirrored here.
- `assets/react/dist/` — Vite build output. **Committed** (releases ship
  it); do not add it to `.gitignore`.
- `vite.config.ts` — React + Tailwind plugins, `base: "/react/"` (the
  server worker relies on this — do not change it), sourcemaps off,
  minify on (esbuild default).
- `tsconfig.react.json` — strict, `jsx: react-jsx`, bundler module
  resolution. Extends the root tsconfig but overrides `module: none`
  (the root config only covers `assets/js/**`, which Vite never sees).

## Routes (all under `/react/`)

- `/` — dashboard overview (stat cards, system status, tasks,
  notifications, module grid with search/filter).
- `/login` — sign-in (POST `/api/v1/auth-manager/jwt/issue`).
- `/admin` — admin overview (counts, system, modules table, users
  table; classic-only sections link to `/admin/*`).
- `/modules/:id` — module detail (GET `/api/v1/extensions/{id}`,
  enable/disable via the matching POST endpoints).
- Anything else renders an honest not-found page.

## Commands

- `npm run build:react` — typecheck (`tsc -p tsconfig.react.json`)
  **then** `vite build`. Vite itself does not typecheck (esbuild strips
  types), so the tsc step runs first and fails the build on type errors.
- `npm run dev:react` — Vite dev server (development only).
- `npm run check` — typechecks both tracks: root `tsc --noEmit`
  (Handlebars TS under `assets/js/`) plus the React tsconfig.

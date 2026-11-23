# Unified demo — static HTML preview (phase 1)

A clickable, backend-free preview of the Unified app, built from the real
default theme (`assets/css/dashboard.css`) and the Editorial Dark variant
(`themes/editorial/`). Open `index.html` in a browser, or serve this folder
with any static server. No build step, no network dependencies.

## Pages (phase 1)

This demo shows the **system site** (control plane) plus one **tenant site**
example, so the two contexts are never confused:

| File | Page |
| --- | --- |
| `index.html` | **Home (system site)** — public front page: pages, latest posts, market strip, ticker |
| `dashboard.html` | **Dashboard (system site)** — stats, market strip, news ticker, module grid, activity |
| `tenant.html` | **Tenant demo** — a tenant site (Koperasi Kita) with its own pages and services |
| `login.html` | Sign in — email/password, TOTP toggle, links to register/dashboard |
| `register.html` | Create an account — name, email, company, plan, password |
| `admin.html` | Admin dashboard — users table, roles, system status |
| `modules.html` | Module list — client-side search, enable/disable toggles |
| `module.html` | Module detail — Billing as the example: capabilities, settings, invoices |
| `settings.html` | Settings — General (WordPress General parity: title, tagline, URLs, admin email, membership, default role, 12 languages, full IANA timezone, date/time formats with live preview, week start) / Theme / Mail (13-provider picker grid with logos + dynamic per-provider fields + Test connection) / Security tabs |
| `store.html` | Store — product cards, demo cart counter |

System vs tenant: the system site is the control plane (tenants,
subscriptions, domains, billing, licensing). A tenant site serves only that
tenant's services; tenant administration lives on the system dashboard.

All data is inline demo data. Forms never submit; buttons show a toast.
The login/register forms redirect to the dashboard after a short toast.

## Structure

```
demo/
  index.html  dashboard.html  tenant.html  login.html  register.html
  admin.html  modules.html  module.html  settings.html  store.html
  README.md            <- this file
  assets/
    demo.css           <- the only stylesheet (tokens mirror the real theme)
    demo.js            <- the only script (shell + interactions)
    sprite.js          <- real icon sprite as a JS string (file:// safe)
    modules-sprite.svg <- the copied real sprite (source for sprite.js)
    logos/             <- copied provider logos (src/modules/email_providers/logos/)
```

## How it stays extensible

- **One shell, no copies.** Pages ship only their `<main>` content plus
  `<body data-page="..." data-layout="app|auth">`. `demo.js` renders the
  topbar, sidebar, footer and toast around it (`renderShell()`), so the
  header/sidebar/footer exist in exactly one place.
- **One stylesheet, one script.** Never add per-page `<style>` or `<script>`
  blocks; add component rules to `demo.css` and behaviour to `demo.js`.
- **Theme switcher.** `html.theme-dark` = Editorial Dark (dark canvas,
  serif headlines, gold accents); default = light theme. Choice persists in
  `localStorage` key `unified-demo-theme`. The switcher is demo-only and
  touches no real theme code.
- **Icons.** Use kebab-case module ids:
  `<svg class="module-icon"><use href="#module-icon-billing"/></svg>`.
  The sprite is injected by `demo.js` from `sprite.js`, so icons work from
  `file://` as well as from a static server.

## Adding a page (phase 2 recipe)

1. Copy any existing page. Keep the `<head>` block verbatim (stylesheet,
   theme bootstrap snippet, `sprite.js` + `demo.js` at the end of `<body>`).
2. Set `<body data-page="your-page" data-layout="app">` — `app` gets the
   sidebar, `auth` gets a centred layout with no sidebar — and write only
   the `<main class="main">` content.
3. If the page needs top-level navigation, add one entry to `NAV` in
   `demo.js` (and to `SIDEBAR_MODULES` for a module shortcut). Do not copy
   header/sidebar markup into the page.
4. Use relative links only, English demo data, and `data-demo-form` on
   forms. Buttons that only demonstrate an action get `data-toast="..."`.
   Toggles use `<button class="switch" aria-pressed="..." data-label="...">`.
   Tabs use `data-tab` / `data-tab-panel` pairs (see `settings.html`).

## Refreshing the icon sprite

1. Copy the new sprite: `cp ../assets/icons/modules-sprite.svg assets/`.
2. Regenerate `assets/sprite.js` from it (same JSON-string format — a
   one-liner with Python `json.dumps` on the SVG file).
3. No page changes are needed; icons are addressed by kebab-case id.

## Verify

```sh
node --check demo/assets/demo.js
cd demo && python3 -m http.server 8130
# then curl every page and asset, expect HTTP 200
```

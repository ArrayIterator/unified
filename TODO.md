# TODO

## Phase 0 — Scaffolding

- [x] Repository scaffolding: README, TODO, .gitignore, LICENSE
- [ ] Initial backend crate (Actix-web) and project layout
- [ ] Initial frontend app (React + Tailwind, Dark / Light mode)
- [ ] Development and release build documentation

## Phase 1 — Core API and auth

- [ ] JSON API standard: success `{ "data": ... }`, error
  `{ "error": { "message": ..., "code": ..., "metadata": {} } }`,
  with optional `trace` in debug / development mode only
- [ ] Argon2 password hashing (with migration path for legacy hashes)
- [ ] Stateless session tokenizer (signed, tamper-proof)
- [ ] TOTP two-factor authentication
- [ ] CSRF protection
- [ ] In-memory session flash messages
- [ ] Database layer: Diesel with PostgreSQL by default

## Phase 2 — Listeners, TLS, and monitoring

- [ ] Socket and TCP port listeners, HTTP and HTTPS (80 / 443, validated)
- [ ] Let's Encrypt ACME, configurable from the dashboard
- [ ] SSL swapping from the dashboard and CLI, custom SSL with validation
      and renewal
- [ ] Service Monitor & Management core module (health, server info,
      reload with dry-run)
- [ ] WebSocket support where needed

## Phase 3 — Core modules

- [ ] Security Management
- [ ] User Management
- [ ] Modules / Extensions Manager (dynamic modules, full interconnection)
- [ ] Content Management (posts, categories, pages, site builder)
- [ ] Settings
- [ ] SEO Manager
- [ ] API Manager (public / client API, versioning, endpoint registry)
- [ ] Auth Manager (SSO, OAuth server and login, social login, biometric
      login, JWT manager, submodule settings injection)
- [ ] Translations / i18n Manager (dynamic translations)
- [ ] Theme Management (templating engine, embedded and dynamic themes,
      strict checking, screenshots)

## Phase 4 — Licensing, downloads, and storefront

- [ ] Licensing Management (issue, validate, suspend, revoke)
- [ ] File Downloader (secure, resumable downloads)
- [ ] Software and asset management
- [ ] Themes and plugins storefront

## Phase 5 — Dashboard

- [ ] React dashboard: modern Tailwind UI, easy navigation, Dark / Light mode
- [ ] IcoMoon icons (for example Entypo and Material Icons)
- [ ] SEO-friendly frontend rendering

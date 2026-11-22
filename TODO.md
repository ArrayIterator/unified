# TODO

## Phase 0 — Scaffolding

- [x] Repository scaffolding: README, TODO, .gitignore, LICENSE
- [x] Initial backend crate (Actix-web) and project layout
      (crate layout, core engine, config, CLI/HTTP/auth/db skeletons;
      module bodies land per phase below)
- [ ] Initial frontend app (React + Tailwind, Dark / Light mode)
      (server-rendered Handlebars dashboard shell done in this batch —
      see Phase 5; the React app is still open)
- [ ] Development and release build documentation

## Phase 1 — Core API and auth

- [x] JSON API standard: success `{ "data": ... }`, error
  `{ "error": { "message": ..., "code": ..., "metadata": {} } }`,
  with optional `trace` in debug / development mode only
  (implemented in `src/core/json.rs` and `src/core/error.rs`)
- [ ] Argon2 password hashing (with migration path for legacy hashes)
- [ ] Stateless session tokenizer (signed, tamper-proof)
- [ ] TOTP two-factor authentication
- [ ] CSRF protection
- [ ] In-memory session flash messages
  (auth files exist under `src/auth/` — password, session, TOTP, CSRF,
  flash — but their contents were not verified in this batch)
- [x] Database layer: Diesel with PostgreSQL by default
  (connection pool in `src/db/mod.rs`, migrations embedded from
  `migrations/`, first migration `2026-11-22-000001_core` present)

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
- [x] Theme Management (templating engine, embedded and dynamic themes,
      strict checking, screenshots)
      (done in this batch: Handlebars engine in `src/themes/engine.rs`,
      embedded default theme in `templates/default/`, dashboard shell;
      the Theme module HTTP surface lives in `src/modules/theme.rs`)

## Phase 4 — Licensing, downloads, and storefront

- [ ] Licensing Management (issue, validate, suspend, revoke)
- [ ] File Downloader (secure, resumable downloads)
- [ ] Software and asset management
- [ ] Themes and plugins storefront

## Phase 5 — Dashboard

- [ ] React dashboard: modern Tailwind UI, easy navigation, Dark / Light mode
      (server-rendered Handlebars dashboard shell done in this batch:
      `templates/default/dashboard.hbs`, `assets/css/dashboard.css`,
      `assets/js/dashboard.js` — sidebar, stat cards, module grid fed
      live from `/api/v1/extensions`, activity feed with honest empty
      states, dark/light toggle persisted in localStorage; the React app
      itself is still open)
- [ ] IcoMoon icons (for example Entypo and Material Icons)
- [ ] SEO-friendly frontend rendering

## Phase 6 — Expanded core modules

All modules below are specified and registered in `src/modules/mod.rs`.
Implementation status is per module owner; only the theme shell items
in this list were verified in this batch.

- [ ] Roles Manager (static default + dynamic roles, role factory, hookable)
- [ ] Company Management (multi-site, multi-company, immutable site basis)
- [ ] Domain / Subdomain Manager (interconnected with SSL manager)
- [ ] Network Manager (GEO submodule, DNS verifier / DNS checker library
      wired into Domain, Mail, Security)
- [ ] GEO Location API (GeoLite / MaxMind-compatible DB, download from
      MaxMind or custom URL, global Geo info variable, ASN check, IP-range
      settings overrides for IPv4 and IPv6, settings override wins by
      default)
- [ ] Application Connection & Sync (desktop app integration)
- [ ] Chat (realtime chat, hook-interconnected with Tickets and
      Knowledgebase)
- [ ] AI Support (chat connectors, knowledgebase-grounded answers,
      handoff to human)
- [ ] Knowledgebase (articles suggested by Chat and AI Support)
- [ ] Store / Marketplace (cart, checkout, orders)
- [ ] Billing / Invoicing (invoices from orders and licenses)
- [ ] Support Tickets (created from Chat, AI Support, CRM)
- [ ] Notifications (in-app and email, driven by hook events)
- [ ] Audit Log (append-only, with Geo and network context)
- [ ] Scheduler / Queue (scheduled jobs and background queues)
- [ ] File Manager (uploads and assets)
- [ ] CRM Contacts / Leads
- [ ] Mail Manager (SMTP settings, mail connectors, default mailer)
- [ ] Email Template Builder (GrapeJS editor page done in this batch:
      `assets/email-builder.html` posts template JSON to
      `/api/v1/email-templates`; the Email Templates module endpoints
      themselves are a follow-up)
- [ ] Site Builder
- [ ] SaaS Multi-Site (with integrated Site Builder)
- [x] Theme shell and dashboard assets (this batch: theme engine,
      default theme templates, dashboard CSS/JS, email-builder page)
- [ ] Pagination and filtering on every core API list endpoint
      (shared `PaginationParams` implemented in `src/core/pagination.rs`;
      per-module adoption is tracked with each module above)
- [ ] Every core module fully hookable and dynamic-hookable by code
      (hook bus implemented in `src/core/hooks.rs`; per-module hook
      registration is tracked with each module above)

## Phase 7 — ERP wiring and release

- [ ] ERP interconnection flows verified end to end through hooks only
      (no direct module calls): Chat → Ticket → Knowledgebase,
      Order → License → Download → Invoice,
      Company → Domain → SSL → Site, Geo → Security → Audit,
      AI Support → Knowledgebase → Ticket,
      Domain check → DNS verifier → SSL issue → Site publish
- [ ] Server daemon and CLI: Reload, Stop, Pause, Maintenance, Restart,
      Start, Status, Monitor; config tested before reload
      (`unified -t`, `unified -t -f /path/to/config.yaml`); ratatui
      monitor TUI with active connections, server info, network,
      process, disk, and app usage
- [ ] Full `cargo build` / `cargo test` pass and release packaging
      (not run in this batch: no cargo toolchain is available in the
      build VM and crates.io is blocked)

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

## Review-and-Adapt Round (2026-11-22)

The reference implementation was reviewed file by file (read-only) and the
stronger ideas were adapted into Unified. Full per-file verdicts live in
`docs/REVIEW.md`. Nothing was copied file-by-file.

- [x] Session tokenizer reviewed — skipped, Unified is equal or better
      (same nested-HMAC layout; Unified fixes the same-second expiry flake
      and has no panic paths)
- [x] Flash messages reviewed — skipped, Unified is equal or better
      (real per-kind cap, true consume-on-read, working periodic cleanup;
      the reference has a silent-drop bug and a dead background recycler)
- [x] SSL storage / SNI reviewed — adapted: separate `auto_created` map so
      eviction never drops a live certificate; fixed `remove()` deleting
      configured defaults; added `certificate_names()`
- [x] ACME reviewed — adapted: multi-provider (Let's Encrypt, Google,
      ZeroSSL, BuyPass, Custom) with EAB, challenge failure handling with
      authorization deactivation, token cleanup on every exit path,
      SAN-preserving renewal with a clamped 7–90 day window
- [x] Downloader reviewed — rewritten: validated status transitions,
      exponential-backoff retries honouring `Retry-After`, true resumability
      (ETag / `If-Range`, `Content-Range` totals), atomic `.part` +
      checksum + rename finalisation, streamed (not in-memory) checksum
      verification, sha512, `POST /jobs/{id}/retry`
- [x] Cache layer added (was missing): `src/core/cache.rs` facade with
      filesystem adapter (sha256 sharding, atomic writes) and Redis adapter
      (`SETEX`, prefix-scoped SCAN clear), plus `src/modules/cache.rs`
      HTTP module; `redis = "1.0"` is the only new dependency
- [x] i18n reviewed — adapted: corrected CLDR plural rules, gettext-style
      lookup with `\x04` msgctxt separator, minimal pure-std `.po` parser,
      locale negotiation, ISO 4217 currency table
- [x] IDNA / domain / Geo reviewed — adapted: full IP classification
      (bogon, CGNAT, documentation, benchmarking, reserved), IPv4-mapped
      IPv6 normalization, CIDR range checks that reject non-aligned ranges,
      `normalize_domain_input`, DNS verifier with `IpChoice`, CNAME and PTR
      lookups
- [x] System / process stats reviewed — adapted: tiered sampling cadence
      in the monitor TUI, per-process stats with sort toggle, disk kind +
      I/O rates, network packets/errors, shared 1 s snapshot cache in the
      service monitor, new `GET /api/v1/service-monitor/processes`
- [x] Hooks / middleware / routes / assets / themes reviewed — adapted:
      hook priorities, once-handlers, unregister, re-entrancy guard; new
      middleware trait/manager/dispatcher (modules can now add middleware);
      route metadata + catalog; new asset registry with deterministic
      ordering, cycle detection, and SHA-256 fingerprinting; template-name
      mapping and 5 MiB template size cap in the theme engine; JSON 404
      default service; dashboard assets now actually served
- [x] JSON helpers and database query builder reviewed — query builder
      skipped entirely (string-interpolated SQL, unbound placeholders,
      must never replace Diesel); envelope kept exactly; adapted only error
      classification (`From<serde_json::Error>`, new
      `From<diesel::result::Error>`) in `src/core/error.rs`
- [ ] Follow-ups from the review (not implemented): corrected absolute
      max-age check for sessions; cookie-lifetime margin over the renew
      window; flash survival across token rotation; cache stampede
      protection; reconnect-on-error wrapper for the Redis adapter
- [ ] `cargo check` / `cargo build` for this round (not run here: no
      toolchain in this VM and crates.io is blocked; compilation is
      unverified — see `docs/REVIEW.md` for the riskiest API surfaces)

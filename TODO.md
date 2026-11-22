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

## Software-Selling Round (2026-11-22)

Enterprise software manager, storefront, and third-party VCS
integrations, wired together through hooks only. The three builders
delivered `src/modules/software.rs` (2,664 lines, 35+ routes under
`/api/v1/software`), `src/modules/store.rs` (3,242 lines, 29 routes
under `/api/v1/store`), and `src/modules/vcs.rs` (2,847 lines, 23
routes under `/api/v1/vcs`); the wiring pass registered VCS,
connected the ERP flows, and added the persistence tables.

- [x] VCS module registered in `src/modules/mod.rs` (`pub mod vcs;`,
      `vcs::register(registry)` after `downloader::register`) — 37
      modules total, still in dependency order
- [x] Refund revokes real licenses: licensing listens to
      `store.order.refunded` and revokes the emitted `license_ids`
      (in-memory store, plus the database row when a pool is attached
      and the license is not in memory); each revocation emits
      `license.revoked`. The store still only suspends its own mirror
      and revokes its entitlements.
- [x] Release published refreshes the store catalog: the software
      module enriches `software.release.published` with the version,
      changelog, requirements, and software slug/name, and the store
      listens and refreshes the matching catalog entry (latest version
      only moves forward, changelog and WordPress / PHP compatibility
      updated). No catalog entry for the slug means no-op, not an error.
- [x] VCS import creates a software draft: the software module
      listens to `vcs.release.imported` and creates a draft version
      (status draft, never published); re-importing the same tag is an
      idempotent no-op, and drafts for unknown software records are
      skipped. Publishing stays manual through the release publish
      endpoint. The software module also answers
      `vcs.software.validate`, so a VCS connection can only point at
      a software record that exists.
- [x] Order → license → entitlement → billing chain verified after
      the store changes: checkout still emits `order.created` first
      (licensing and billing listen to it), then
      `store.order.created`. Billing reads only `id`, `user_id`,
      `total`, `currency`, `items`, and `company_id`, so the new
      `subtotal` / `discount_total` / `coupon_code` fields do not
      break it, and no `store.order.completed` listener is needed:
      the draft invoice is created on `order.created` and finalised
      by paying it.
- [x] Download signing secret moved to config:
      `store.download_signing_secret` in `src/core/config.rs` (at
      least 32 characters when set, validated) and
      `config.example.yaml`. With it set, signed links survive
      restarts; without it, a random per-process secret is generated
      once with a logged warning. Changing the value requires a
      restart to take effect.
- [x] Vendor ownership verified: the software module validates
      `vendor_company_id` through the `company.exists` hook
      (`{"company_id": ...}` in, `{"exists": bool}` out) and the
      company handler matches that shape exactly — no fix was needed.
      A non-UUID vendor id is rejected with 400, an unknown company
      with 404.
- [x] `settings.merge` connected: the settings module now listens to
      `settings.merge` and stores the merged flags under
      `vcs.<provider>` (token / webhook flags only — the VCS module
      never puts a credential in the merge payload, so tokens stay in
      its write-only in-memory store). The `vcs_provider_settings`
      table reserves `*_encrypted` columns for the database side.
- [x] Persistence tables added in
      `migrations/2026-11-22-000002_software_selling/` (20 tables)
      and mirrored in `src/db/schema.rs`: `software_taxonomy`,
      `software`, `software_versions`, `software_releases` (added
      beyond the builders' list — `release_files.release_id` needs
      the referential target), `release_files`, `software_tags`,
      `software_tag_assignments`, `store_products`,
      `store_pricing_tiers`, `store_coupons`, `store_orders`,
      `store_order_items`, `store_entitlements`, `store_downloads`,
      `store_catalog`, `store_issued_licenses`, `vcs_connections`,
      `vcs_version_drafts`, `vcs_sync_history`,
      `vcs_provider_settings`. No table name collided with the
      000001 core migration, so no renames were needed.
      `src/db/models.rs` was left untouched: it does not model these
      tables yet (per the wiring brief, models follow the read-path
      migration).

### Still in-memory only (not swapped in this pass)

- [ ] The software, store, and VCS modules still serve reads and
      writes from their DashMap in-memory stores. The new tables are
      the persistence target; swapping the stores for Diesel queries
      is a larger migration and is deliberately not attempted here.
      The licensing module is the exception: it already reads and
      writes the `licenses` / `license_activations` tables when a
      database pool is attached.
- [ ] `vcs_provider_settings.token_encrypted` /
      `webhook_secret_encrypted` reserve the encrypted-at-rest shape,
      but no encryption helper exists yet — encrypting values before
      insert is still open work. Tokens currently live only in the
      VCS module's write-only in-memory store.
- [ ] Money is `DOUBLE PRECISION` in the new tables to mirror the
      f64 values used by the in-memory store. `NUMERIC` would be the
      better long-term choice; switching needs a data migration.

### Follow-ups from the builders (not implemented)

- [ ] Licensing dedupe on repeat purchase: buying the same licensed
      product twice for the same owner does not issue a second license
      (`license_exists_for_product` skips it), so the repeat order
      gets no new license and no new entitlement. Decide whether a
      repeat purchase should extend, renew, or re-issue.
- [ ] Trial does not defer payment: a subscription with trial days
      records `trial_until` on the order, but checkout still takes
      payment immediately. Trial-then-charge needs a billing-side
      deferral.
- [ ] `remove_from_cart` removes all tiers: deleting a cart item by
      product without a tier removes every tier of that product from
      the cart.
- [ ] Catalog release / file ids are validated only against store
      products: `create_catalog_entry` checks `product_id` against
      the store, but `release_id` / `file_id` are free-form strings
      with no check against the software module's releases or files.
- [ ] Invoice line items stay gross while the invoice total is net:
      billing builds invoice items from the order's `line_total`
      values and the invoice `total` from the order total (after the
      coupon discount), so the item lines can sum to more than the
      invoice total. Allocate the discount across lines if the
      invoice must reconcile.
- [ ] Redis-less cache note: the market widgets' 60 s negative-cache
      backoff uses the shared cache facade (filesystem adapter when
      Redis is disabled); no store-specific cache work was needed.
- [ ] `cargo check` / `cargo build` for this round (not run here: no
      toolchain in this VM and crates.io is blocked; compilation is
      unverified — see the wiring report for the riskiest API
      surfaces).

---

## Payment security re-check (builder B3, 2026-11-22) — DO NOT EDIT ABOVE

What was checked: the whole payment surface — `payments.rs`
(webhooks, intents, charges, refunds, voids, payouts,
subscriptions, payment methods), `bank_transfer.rs` (proof
upload, verification, approval/rejection), store checkout
(totals, coupons, quantities, signed downloads), billing
(invoice totals, credit notes). What was fixed is listed below;
what remains open is listed after it. Compilation was NOT
verified (`cargo check` never ran here: no toolchain, crates.io
blocked); see the wiring report for the riskiest API surfaces.

### Fixed

- Webhooks: Stripe timestamp window added (300 s, rejects stale or
  unparseable `t=`); event-id fallback is now a SHA-256 of the raw
  body instead of a fresh UUID (duplicates now dedupe); webhook
  event ids older than the TTL are pruned (`prune_webhook_events`).
  HyperPay / 2Checkout webhooks no longer trust a fetched id's
  existence alone — a `webhook_outcome` trait method re-fetches the
  payment/order server-side and maps the real status, closing the
  forgery hole (an attacker with a real checkout/order id could
  otherwise forge a capture). Refund webhooks now use a
  disposition (`Completed` / `Partial` / `Pending` / `Failed`):
  `refund.failed` never marks anything refunded, a pending refund
  moves the charge to `refund_pending`, a failed refund restores
  `refund_pending` to `captured`. The capture branch rejects
  currency mismatches, underpaid amounts (`partial_capture`), and
  already-terminal transactions.
- Refund gating (`refund_transaction`): only a completed
  `Refunded` / `PartiallyRefunded` provider result marks the
  original `refunded` / `partially_refunded` and emits
  `payment.refunded` (now with an explicit `status` field). A
  `Pending` result records the refund as `pending`, moves the
  original to `refund_pending`, and emits nothing. Any other
  result records nothing and returns an error. Over-refund guard:
  a refund may not exceed the charge minus all earlier refunds
  linked through the new `PaymentTransaction.original_transaction_id`
  field (completed and pending both reserve money); an absent
  amount refunds the remainder, never the full original amount.
  New endpoint `POST /api/v1/payments/transactions/{id}/complete-refund`
  (`CompleteRefundRequest { status: "refunded" | "failed", reason }`)
  is the refund-completion path: on `refunded` it recomputes the
  original's status from cumulative completed refunds and emits
  `payment.refunded`; on `failed` it restores `refund_pending` to
  `captured` (or `partially_refunded` when other refunds completed).
  `refund_pending` is an accepted refund source status.
- Store-first refunds: `on_store_order_refunded` now forwards
  `provider_refund_id`, `transaction_id`, `amount_minor`,
  `currency`, plus `"status": "refunded"` and
  `"fully_refunded": true`, so the billing listener's status gate
  and redelivery dedupe work on that path.
- Amount tampering: `check_pending_checkout_amount` compares a
  client-supplied amount/currency against the store order total
  recorded in `payment.initiated` (major to minor units,
  zero-decimal aware); applied in `create_intent`,
  `create_charge`, and payments `pay_order` (422 on mismatch).
  Orders with no recorded checkout (direct API charges) pass —
  this is a second line of defence, not a substitute for API auth.
- `capture_intent`: the optional amount is now validated
  (`0 < amount <= intent.amount_minor`); previously it went to
  the adapter unchecked.
- Ownership (defence in depth, header identity — NOT
  authentication): `x-user-id` / `x-session-id` checks added in
  payments (`create_intent`, `confirm_intent`, `capture_intent`,
  `cancel_intent`, `get_intent`, `create_charge`, `pay_order`,
  `refund_transaction`, `complete_refund`, `void_transaction`,
  `get_transaction`, payment-method create/get/delete/set-default),
  store (order pay/refund/cancel/complete/update/delete), billing
  (invoice get/update/delete/pay/void/record-payment/create-credit-note),
  bank transfer (verification get/proof/verify/reject/cancel,
  plus a self-approval block). A caller with no identity headers
  matches only `guest`-owned records; operator calls must send
  `x-user-id` until the API-management layer provides a real
  admin identity. Behaviour change — flag loudly.
- Invoice gross-vs-net (TODO item above, now DONE):
  `invoice_from_order_value` allocates the order discount
  proportionally across lines (rounding remainder on the largest
  line) so lines reconcile to the net invoice total. The checkbox
  above can be checked off.
- Store entitlement loop closed (B10 follow-up): the store now
  answers `store.entitlement.check` (`{"product_id", "user_id"}` →
  `{"entitled": bool, "entitlements": [...]}`) from active
  entitlements (status `active`, not past `expires_at`) or an
  active issued license (matched by product name through the
  store product). The products module's
  `product.entitlement.check` and the meetings join check no
  longer fail closed with `no_entitlement_source`.
- Store catalog mirror (B10 follow-up): the store now listens to
  `product.listed` (create/refresh the listing, keyed by the
  products product id, published) and `product.delisted` (retire
  to `draft`, never delete — orders and entitlements keep their
  reference). Assumptions: tier data is NOT in the event payload
  (only `tier_count`), so tiers are never mirrored; the store
  kind is kept verbatim from the catalog (the catalog vocabulary
  is wider than the store's theme/plugin/software); `usage` /
  `per_seat` pricing models fall back to `one_time`.
- Subscription renewal loop (B4 follow-up): the scheduler arm for
  `payment.subscription.renew` emits
  `payment.subscription.renew_requested` with `via_scheduler:
  true`; the payments handler now checks that flag and spawns
  the actual charge (`renew_one`) instead of re-enqueuing, so
  the loop is closed. The scheduler's 30 s per-subscription
  dedupe was left in place.
- Also fixed while touching the code: `set_default_payment_method`
  returned the pre-update clone (stale `is_default`); it now
  re-reads the stored record.

### Still open (not fixed here)

- NO global authentication / authorization on `/api/v1` (only
  hardening middleware). The header-identity ownership checks
  above are defence in depth, not auth. Real auth + capability
  checks belong to the API-management / auth layer (flagged
  CRUCIAL by the user 2026-11-22 10:21); API Management must
  manage allowed public access.
- No provider IP/host allowlist on webhook endpoints (no provider
  publishes a stable list the code can check today; signatures,
  timestamp window, and event-id dedupe are the defences in
  place). Rate limiting is the global middleware only.
- List endpoints are unscoped (store orders, billing invoices,
  payment transactions/methods, bank-transfer verifications,
  connections CRUD, subscriptions/payouts admin ops) — any
  caller can list everything. Needs roles / API management.
- Webhook amount cross-check is minor-units only: a provider
  sending a major-unit amount is not compared against the
  transaction total.
- Over-refund guard cannot see refunds completed via webhook:
  the webhook refund branch updates the original's status but
  records no refund transaction, so a partially-refunded charge
  (via webhook) followed by an API refund could over-refund.
  Fix sketch: record webhook refunds as refund transactions
  (kind `refund`, linked by `original_transaction_id`) with
  provider-refund-id dedupe.
- Split-bill completion does not move the order to paid: a
  split-part capture is below the order total, so the store
  captured listener correctly skips it, but no listener exists
  on `payment.split.completed` to mark the order paid when all
  parts land.
- Duplicate scheduler dispatch after the 30 s dedupe window
  could double-charge: `renew_one` does not check due-ness
  (same pre-existing property as the HTTP renew endpoints).
- `create_invoice` still accepts an explicit total override
  (pre-existing; the caller-supplied total wins over the
  computed one).

---

## Integrator Round (2026-11-22)

All build rounds merged into one registered, wired codebase. Builders
B1–B13 (permission fix, migrations + public API registry, payment
wiring, core wiring, registry + API merge, library checks, ticketing +
departments, live chat, meetings, products, notifications, MCP, AI)
and wave-2 builders W1–W4 (schema/migrations, registry final, TLS
leftovers, event gaps) all completed. Nothing was pushed; the parent
verifies and pushes.

### Final counts (verified by the integrator, by reading and counting)

- **206 files, 114 `.rs` files, 159,439 Rust lines.**
- **71 modules registered**: `pub mod` == register calls == module
  files in `src/modules/` (72 `.rs` files minus `mod.rs`). The four
  modules that were unregistered after the additions (ai, mcp,
  meetings, products) are now registered; departments was registered
  by its builder.
- **30 migrations** (`2026-11-22-000001_core` … `000030_ai`), no
  duplicate numbers. The three sketches that each claimed 000026
  (meetings, products, mcp, ai) were resolved to 000027–000030;
  000026 is departments.
- **160 tables**: `CREATE TABLE` count across all `up.sql` files ==
  `diesel::table!` blocks in `src/db/schema.rs` == half of the 320
  public structs in `src/db/models.rs` (one Row + one New struct per
  table). Every table has `created_at` + `updated_at`; money is
  `NUMERIC(19,4)` (products/mcp/ai money is integer minor units in
  code; the AI `estimated_cost_usd` field is a DOUBLE estimate, not
  money, and is commented as such).
- **30/30 migrations have a `down.sql`** (inverse of `up.sql`).
- **0 hits** for the forbidden reference-project name across
  `src/`, `templates/`, `assets/`, `docs/`, `*.md`, `*.toml`,
  `*.yaml`. **0 stub macros** (`todo!` / `unimplemented!`) in `src/`.
  **0 Indonesian prose hits** (word-boundary grep over `src/*.rs`).
- **Delimiter balance**: a tokenizer-aware check (raw strings, char
  literals, lifetimes, comments handled) reports no unbalanced file
  across all of `src/**/*.rs`. A naive counter flags many files —
  that is a lifetime/char-literal artifact, not a finding.

### Permission fix — verified by read-back

`permission_allowed` in `src/modules/roles.rs` (B1): the `"*"`
branch used to return `!map.is_empty()` — any role with any
permission at all was granted the wildcard. Now only a literal
`"*": ["*"]` grant returns true; `permission == "*"` against any
other map returns false. The dashboards workaround was reconciled
with the fix (direct call, no workaround left). Read back by the
integrator: the fixed function is in place as described.

### Snippets and wiring applied

- Config sections: security, api, routes, storage, load_balancer,
  gateway, records; `store.download_signing_secret`;
  `config.example.yaml` updated. `AppConfig.api` is read by the API
  Manager (`apply_config` seeds prefix/version defaults);
  `AppConfig.records.retention_days` is read by the records module
  (constant kept as fallback).
- AppContext fields: `storage_registry`, `routes` (shared
  `RouteRegistry`), `tls` + `set_tls()`. One shared `SslStorage`:
  server attaches it, the load balancer's `get_tls_status` reads
  `ctx.tls()` (config-only fallback when unattached), and
  certificates' `seed_from_config` takes `ctx.tls()` (falls back to
  building one when unattached). The dashboards theme engine gets
  `ctx.routes` attached (`set_route_registry`); routes and
  dashboards modules use the shared registry instead of building
  their own.
- Scheduler arms: `backup.run`, `cluster.health`, `cluster.sync`,
  `payment.subscription.renew`, `report.generate`,
  `newsletter.send`, `notification.digest` (+ `.send`),
  `meeting.start`, `announcement.publish_due`, `ticket.sla.check`
  (dispatches `ticket.sla.check_requested`; the tickets module
  handles it and emits `ticket.sla.breached`).
- `AUDITED_EVENTS`: ticket events (`ticket.created`,
  `ticket.updated`, `ticket.deleted`, `ticket.escalated`,
  `ticket.sla.breached`) and AI events (`ai.call.completed`,
  `ai.call.failed`, `ai.draft.created`, `ai.draft.approved`,
  `ai.draft.rejected`, `ai.provider.created`,
  `ai.provider.deleted`) added.
- LogsLayer wiring, `route_url` Handlebars helper + the 16
  hardcoded-path template fixes, maintenance 503 exemptions for
  `/status` and the status-page API, dashboard route mounting +
  redirect handler, reseller role seed (sixth default role:
  `reseller: [access, clients.view, clients.manage, payouts.view]`,
  `user: [access]`, `orders: [read]`, `licenses: [read]`).
- Payment wiring (B3): `payment.captured` listeners in store +
  billing move an approved payment to `paid` and update the invoice
  balance; `pending_verification` added to the store order status
  machine; refund handler gated on an actual `Refunded` status;
  `via_scheduler` loop guard on subscription renewal;
  `store.entitlement.check` answers products/meetings;
  `product.listed` / `product.delisted` store consumers.
  Payment security re-check: see the B3 section above.
- API merge: single API-key store in apikeys (api_manager
  delegates; SHA-256 hashing, 120-char name limit, scopes default
  `["*"]`); `app.dashboard_path` deprecated in favour of
  `routes.user_dashboard`; `RouteVersionSupport` + optional
  `Module::route_version_support()` (default All) instead of a
  RouteInfo field; `security.auth.failed` subscription verified
  present. Public API access control (B2): default-deny guard
  (`PublicApiAccessMiddleware`, priority 850), per-endpoint /
  per-version allow/deny, per-key scopes and rate limits, audit log;
  rules and audit now persist in `api_access_rules` /
  `api_access_audit` (load on init, save on change) — W4.
- Library checks (B6): optional `Module::requires_external_libraries()`
  trait method + 29-tool probe allowlist, local + SSH remote checks
  (password via one-time `SSHPASS`, never plaintext), module-list
  notices + re-check action, `library.check.failed` event. **Gap:
  no module declares `requires_external_libraries` yet, so checks
  return empty until owners adopt it.**
- Ticketing + departments (B7): tickets 586 → 2,524 lines (status
  workflow, department/product fields, replies vs staff-only
  internal notes, storage-referenced attachments, canned
  responses, SLA timers + breach alerts + escalation rules,
  merge/link incl. license links); departments new core module
  (1,296 lines). Tickets `RouteInfo` verified intact (31 entries)
  after B2's concurrent edits.
- Live chat (B8): chat.rs 1,450 → 3,788 lines; 30 routes under
  `/api/v1/chat/support`; department routing via the real
  `department.agents_requested` hook; chat → ticket escalation with
  transcript; offline messages → ticket. Fixed the old escalation
  bug (emitted `ticket.create_requested` with no `body`/`user_id`,
  so every old escalation failed validation).
- Meetings (B9): 2,962 lines, 31 routes; signalling over the
  WebSocket hub; paid rooms check `product.entitlement.check` at
  join. **Honest note: signalling + room management are real; media
  transport is browser WebRTC — no media server/SFU is built in
  this round (follow-up, not pretended otherwise).**
- Products (B10): 4,806 lines, 67 routes; lifecycle, 7 kinds,
  variants, bundles (DFS cycle rejection), tiers/per-seat/
  usage-based/regional pricing, tax in basis points, inventory,
  money in integer minor units only.
- Notifications (B11): notifications.rs 1,575 → 3,508 lines,
  announcements.rs 995 → 1,440 lines. Fixed two real bugs:
  per-category channel overrides were ignored by dispatch, and
  digests excluded broadcast notifications. Announcements were
  effectively invisible before (stored for literal `user_id:
  "all"`, which no real user listed); audience resolution is now
  server-side and fail-closed. Scheduled publishing fixed (a future
  `publish_at` used to fan out immediately). See the README
  notifications entry for the full list.
- MCP (B12): 2,480 lines; 12 read-only tools, 9 resources, 4
  prompts; JSON-RPC 2.0 on the MCP endpoint (deliberate), standard
  envelope elsewhere. Read-only enforcement: table/column
  allowlist, credential-column denylist, `SET TRANSACTION READ
  ONLY`, parameterized queries only. Auth via apikeys with per-key
  tool allowlists; sellable per-server/per-site via products.
  **W4 merged the context source: MCP now calls
  `ai::assemble_shared_context` instead of its own assembler.**
- AI (B13): 3,468 lines; Anthropic/OpenAI/OpenAI-compatible/local
  providers; credentials write-only, encrypted with an HMAC
  keystream (NOT AEAD — documented caveat, replace when an AEAD
  crate is added); drafts labelled `ai_generated: true`, no
  publish/send path; approve only flips status and emits an event
  with `requires_human_delivery: true`.
- Event gaps closed (W4): licensing expiry scan emits
  `license.expiring` (`license_id`, `license_key`,
  `days_remaining`, 30/14/7-day windows); service monitor emits
  `cluster.health.changed` on down transitions (notifications
  already subscribes); webhooks module listens to
  `notification.webhook` and delivers through its existing
  machinery (no matching endpoint is recorded, not silently
  dropped).

### Still open (not fixed here)

- **No global authentication / authorization on `/api/v1`** (only
  hardening middleware) — carried from B3. Header-identity
  ownership checks are defence in depth, not auth.
- **`license.expiring` has an emitter but no subscriber yet.**
  Licensing emits it (W4); the notifications module does not
  subscribe to it — its comment (notifications.rs ~2795) predates
  the emitter. Add a listener there with the same pattern as
  `ssl.expiring`.
- **Invoice gross-vs-net checkbox above is stale**: the item under
  "Follow-ups from the builders" (still gross) is DONE per the B3
  section (`invoice_from_order_value` allocates the discount
  proportionally). The checkbox was left untouched because the B3
  section says do not edit above it — check it off in a later pass
  that owns that section.
- Split-bill completion does not move the order to paid (B3 open);
  `create_invoice` still accepts an explicit total override (B3
  open); over-refund guard cannot see webhook-completed refunds
  (B3 open, fix sketch in the B3 section); list endpoints are
  unscoped (B3 open); no provider IP/host allowlist on webhook
  endpoints (B3 open); webhook amount cross-check is minor-units
  only (B3 open); duplicate scheduler dispatch after the 30 s
  dedupe window could double-charge (B3 open).
- **MCP writes are a future, gated feature.** The MVP is read-only;
  any write-capable tool must be explicitly enabled by an admin
  later.
- **No media server / SFU for meetings** (B9 honest note). Browser
  WebRTC only in this round.
- **No module declares `requires_external_libraries` yet** (B6 gap)
  — library checks return empty until owners adopt the trait
  method.
- **AI credential encryption is HMAC-keystream, not AEAD** (B13
  caveat) — replace when an AEAD crate is added.
- `viewer_id` / `viewer_role` are trusted query params until the
  auth extractor lands (B7). Ticket order/server links are stored
  unverified — no lookup hook exists (B7). SLA scans run through
  the `ticket.sla.check` scheduler kind (W2); confirm the job is
  actually scheduled, not just dispatchable.
- `mcp.context.*` hook handlers: no source module answers them yet,
  so MCP context answers report `available: false` with a gap note
  for those sources (B12).
- `notification.webhook` delivery now has a listener (W4), but
  retries are via the retry endpoint or re-emitting the event;
  `next_retry_at` is advisory; there is no background worker in the
  notifications module (B11).
- Licensing dedupe on repeat purchase, trial-does-not-defer-payment,
  `remove_from_cart` removes all tiers, catalog release/file ids
  validated only against store products (software-selling round
  follow-ups, unchanged).
- **Compilation is unverified everywhere**: `cargo check` / `cargo
  build` / `cargo test` never ran (no toolchain in this VM,
  crates.io blocked). No live HTTP render, no database run, no
  migration was ever executed. The riskiest integration points for
  the first compile: the Pingora gateway feature (11 uncertain API
  points, pin unverified), the 160-table Diesel schema/models
  (type/nullability mismatches would surface here first),
  `payments.rs` (10,021 lines — HMAC/webhook paths), the MCP ↔ AI
  context merge (W4), and the API access-rules persistence path
  (raw SQL in api_manager.rs).

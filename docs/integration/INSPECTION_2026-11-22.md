# Full Re-Inspection — Unified (2026-11-22)

**Scope verified:** 1,742 `.rs` files, **632,908 lines** (measured independently by 3 agents).
Correction: an earlier parent baseline of 1,265,816 lines was exactly 2x double-counted (632,908 x 2).
All findings are static, read-only inspection — no `cargo check`/build (VM OOM), no live server/DB, no push.
Seven areas audited in parallel; every finding below carries file:line evidence.

---

## CRITICAL

### C1 — Authentication is self-asserted headers across the whole API (security)
Any unauthenticated caller can send `X-User-Id: <anyone>` + `X-Role: admin` and pass every capability check.
There is no global auth middleware (`src/http/server.rs` registers only security-headers, tenant-resolution,
IP-filter, rate-limit, request-size) and `RouteInfo` (`crates/unified-core/src/modules.rs:75-84`) has no
auth/permission field. Per-module `caller()` just reads headers — e.g. `src/modules/notifications/helpers.rs:80-95`,
`src/modules/network_tools/helpers.rs:44-49`, `src/modules/chat/helpers.rs:32-36` (documented admission:
*"NOT A SECURITY BOUNDARY: this header is self-asserted, so any caller can claim any role, including `admin`"*).
Scale: **296 files** reference `x-user-id`/`x-role` across **131 of 137 modules**; **118 files** define their own
`fn caller()`. Only the `security` module verifies JWT (`src/modules/security/auth.rs:120+`).
**Fix:** verify the Auth Manager JWT/session in one middleware, inject a server-verified `Caller` into request
extensions, make `caller()` read that — never headers. Do not expose this API publicly until then.

### C2 — Fully unauthenticated mutating endpoints (security)
`src/modules/store/routes.rs:268` — `POST /api/v1/store/products` (and PUT/DELETE products, coupons, tiers,
orders pay/refund/complete/cancel, checkout) with no auth; `src/modules/store/products.rs:91-94` `create_product`
takes no `HttpRequest` at all. `src/modules/store/cart.rs:198-210` `checkout()` takes `user_id` **from the request
body**. `src/modules/mail/module.rs:289-290` — `PUT /settings` overwrites SMTP host/credentials, no auth.
`src/modules/audit/handlers.rs:20-60` — full audit log readable, no auth. 22 handler files total with zero auth
tokens by scan.
**Fix:** C1's middleware globally with an explicit public-route allowlist.

### C3 — Tenant isolation still essentially unguarded (security, re-audit)
Re-counted: **114 of 137 modules have ZERO tenant-guarded files; only 96 of 1,659 module files guarded.**
`with_tenant_connection` (the only sanctioned tenant-DB path, `crates/unified-core/src/tenancy.rs`) is used in
only **13 modules** (29 call sites). Billing, CRM, projects, store, chat, users, orders, tickets, files do not use
it. Worst: `src/modules/dashboards/service.rs:467-472` counts `users`/`roles`/`licenses`/`store_orders` directly
from `public` with no tenant scoping; `src/modules/files/types.rs:269-274` — `can_access_record(None, _) == true`.
Most DashMap stores are keyed by bare id with no tenant key. (Round 2: 125/138 unguarded — nominally improved by
~11 modules, same order of magnitude.)
**Fix:** enforce tenant scoping at the data-access layer (connection checkout sets search path; reject
tenant-module requests without a resolved tenant); partition in-memory stores by tenant id.

### C4 — Tenant middleware defaults to fail-OPEN (security)
`crates/unified-core/src/middleware.rs:561-563` — `TenantResolutionConfig::default()` sets `unknown_host_policy`
and `missing_host_policy` to `ControlPlane`; `unresolved()` (`:966-970`) passes the request through. Contradicts
the fail-closed contract in `tenancy.rs`; the tenants module itself registers no resolver
(`src/modules/tenants/module.rs:80`).
**Fix:** default both policies to `NotFound` (404); require explicit operator opt-in for `ControlPlane`.

### C5 — Idempotency is check-then-act: concurrent duplicates double-charge / double-refund (logic)
`src/modules/payments/handlers_charges.rs:42-43` — check `find_by_idempotency()` → `adapter.charge()` →
`record_transaction()`; two concurrent requests with the same key both pass. Same for refunds (`:317-318`),
split bills (`:375-376`), intents (`:203-205`), webhooks (`handlers_webhooks.rs:252`, event id inserted only
after success at `:~607`). Refund over-refund guard is also racy (`:303-321`).
**Fix:** claim the key atomically before the provider call (pending-marker entry / pending transaction row); same
for webhook event ids at receipt. Note: DashMap claims are per-process only — multi-instance needs the claim in
Postgres.

### C6 — Webhook partial refunds without an amount are recorded as FULL refunds (logic)
`src/modules/payments/handlers_webhooks.rs:139-145` — `event_amount_minor(event, kind).unwrap_or(original.amount_minor)`;
non-Stripe events without an amount fall through to the full original amount, charge marked `refunded`,
`PAYMENT_REFUNDED` emitted with the full amount (`:438-441`).
**Fix:** record amount-less refunds as `pending`/unquantified, or parse each provider's major-unit amount with
currency-aware conversion.

### C7 — Dunning configured but never enforced; renewal anchors to charge time (logic)
`src/modules/payments/handlers_subscriptions.rs:228-262` `renewal_failed` only sets `past_due` + emits
`payment.failed`; no failure counter or retry cap. `dunning_max_retries`/`dunning_retry_interval_days`/
`grace_period_days` are stored (`src/modules/products/handlers_products.rs:167-190`, copied at
`src/modules/store/subscriptions.rs:221-223`) but no renewal code reads them. `renew_due_subscriptions` (`:535-575`)
retries every due `past_due` subscription on every sweep, forever. `period_end`
(`src/modules/payments/wallets.rs:198-207`) anchors to `Utc::now()`, so late sweeps grant free days.
**Fix:** count consecutive failures, honour the dunning settings, cancel/expire when exhausted; anchor `next_end`
to `max(previous period end, now)`.

### C8 — `subscription.activated` / `subscription.expired` never emitted; saas provisioning + expiry dead (logic)
Listeners exist (`src/modules/saas_packages/module.rs:516-540`, `src/modules/entitlements/module.rs:481,506`) but
repo-wide grep finds only const definitions/registrations/tests. Store emits
`created/.renewed/.past_due/.trial_ended/.entitlement_changed/.cancelled`
(`src/modules/store/subscriptions.rs:237,269,387,411,464,834`) — never `activated`/`expired`. A purchase in grace
never expires (module doc admits it).
**Fix:** emit `activated` on activation, `expired` from an expiry sweep — or delete the listeners.

### C9 — Library fine-payment listeners on wrong event names (logic)
`src/modules/library/module.rs:317,325` registers `billing.payment.completed` and `billing.invoice.paid`, but
billing emits unprefixed `invoice.paid` (`src/modules/billing/handlers_invoices.rs:415-419`) and the store emits
`payment.completed` (`src/modules/store/orders.rs:171,283`). Nothing emits the `billing.*` names. Second defect:
`on_billing_paid` (`src/modules/library/events.rs:165-182`) reads top-level `fine_id`, but the payload nests
everything under `invoice`.
**Fix:** register for `invoice.paid` + `payment.completed` and resolve `fine_id` from the nested record.

### C10 — Licensing upgrade listeners dead (logic)
`src/modules/licensing/module.rs:79,85` listens for `store.order.upgraded` and `billing.plan.changed` →
`on_order_upgraded` (`src/modules/licensing/hooks.rs:456`); no emit of either name exists in the tree. The handler
doc cites the upgrade path as the way an expired license returns to active; only manual `update_license`
(`src/modules/licensing/handlers.rs:167-197`) can do it today.
**Fix:** emit both events from the store/billing flows with the payload the handler expects, or remove the
listeners.

### C11 — In-memory DashMap stores still authoritative (OPEN item c — STILL STANDS)
**784 DashMap store declarations (700 fields + 84 `OnceLock` statics) across 129 files.** Only 8 module groups
reference `DbPool` at all; two explicitly say business stores have no `DbPool`
(`src/modules/exams/module.rs:158`, `src/modules/library/module.rs:147`). No startup hydration found.
Representative: `src/modules/store.rs:62` ("in-memory maps stay authoritative until the database layer is
wired"), `src/modules/billing/state.rs:23-86`, `src/modules/elearning/state.rs:35-51`,
`src/modules/scheduler/state.rs:17-19`, `src/modules/web3/state.rs:23-37`. Migrations are labelled
"persistence targets only, not yet the read path".
**Fix:** land the shipped migrations as the read path per module (load at startup, write-through, or Diesel
queries), starting with money/lifecycle state; keep lock/dedup/rate-limit maps in memory by design.

### C12 — Modularity: closed provider registries everywhere (rule MUTLAK — FAR from compliant)
156 `pub enum` declarations, 385 `matches!(…)` occurrences, 339 `https://` in Rust. The pattern everywhere is
closed enum + 4–8 parallel `match` tables (`as_str`/`from_str`/`display_name`/`allowed_hosts`/`capabilities_for`/
`currencies_for` + per-provider adapter + dispatch arm). Adding one provider = code change in 5+ places; an admin
cannot do it.
- Payments, 12 variants: `src/modules/payments/providers.rs:14` (+ `allowed_hosts()` `:106`,
  `capabilities_for()` `:177`, `currencies_for()` `:333`); base URLs in `dispatch.rs:68-105`.
- Cloud/VPS, 17 variants: `src/modules/providers/types.rs:83` (+ `:205`, `:587`).
- Email, 9 variants: `src/modules/email_providers/provider_kinds.rs:14` (+ `:88`, `:162`). A real
  `register_factory` registry exists (`registry.rs:37,75`) but only the 9 built-ins are ever registered and there
  is no admin/config path.
- AI, 4 variants + baked models: `src/modules/ai/types.rs:16,67,77` (`claude-sonnet-4-20250514`, `gpt-4o-mini`
  as defaults).
- Storage, 5 variants in core: `crates/unified-core/src/storage.rs:54`.
- VCS, 3 variants: `src/modules/vcs/providers.rs:43,79`.
- Plus HIGH: notification channels only 3 (`src/modules/notifications/types.rs:114`, no SMS/push possible);
  Google/Facebook bespoke OAuth code paths (`src/modules/auth_manager/providers.rs:80`, `google.rs:47,49,51`);
  holiday provider kinds + 12-country Google calendar map in code (`src/modules/holidays/types.rs:39`,
  `state.rs:189,195-205`); Cloudflare cache levels/record types as consts
  (`src/modules/cloudflare/constants.rs:30,32`); reference-ID format fixed in core
  (`crates/unified-core/src/reference_id.rs:55,59` — 30 chars, segment sizes not configurable); Web3 chains closed
  enum (`src/modules/web3/types.rs:13`).
- MEDIUM: role/permission seed matrices in code (`src/modules/roles/seeds.rs:8`); 4 baked presets + closed wizard
  steps (`src/modules/presets/seeds.rs:66`, `wizard_steps.rs:18`); 35 currencies / 12 languages / CLDR plural
  rules as code (`src/modules/i18n/currencies.rs:19`, `languages.rs:20`, `locale.rs:104-110`); ~46 single-line
  string vocabularies (priority `"low"|"normal"|"high"|"urgent"` in 3 copies; project status/visibility/role;
  methodology lists; discount kinds; etc.); billing `period_days()` magic day-counts
  (`src/modules/billing/state.rs:340-346`).
- **Fix (pattern, per provider family):** one definition table (DB/config) — id, display name, base URL
  (admin-overridable), allowed hosts, capabilities, currencies, auth scheme — driving the existing trait
  registries; delete the enums. The `Custom`/`Generic`/`openai_compatible` escape hatches already prove the
  shape — promote them from exceptions to the rule.
- Sanctioned exceptions verified compliant (do NOT flag): paper sizes (`src/modules/printing/paper.rs:31-43`);
  card IIN/Luhn/CVV (`src/modules/cards/types.rs:171`); which brands are enabled is data
  (`cards/handlers.rs:407`).

### C13 — Auth/session/token hardening gaps (security, HIGH)
- **H1** `src/modules/auth_manager/state.rs:85,93-99` — signing key falls back to a per-process random secret when
  `secret_key`/`salt_key` unset: tokens don't survive restart/replica, and the `security` verifier refuses empty
  keys, so issuer and verifier disagree. Config enforces key length only in `production`
  (`crates/unified-core/src/config.rs:649-657`); `config.example.yaml:20` ships placeholder
  `"change-this-to-a-random-secret-string"`.
  **Fix:** fail startup unless both keys are set (≥32/16) in every mode; never fall back to ephemeral; reject the
  placeholder.
- **H2** Stateless JWT revocation is an in-memory `DashMap` (`auth_manager/state.rs:110-125`), lost on restart;
  logout only clears the cookie (`src/modules/frontend/module.rs:100-104`). `src/auth/session.rs`
  `SessionTokenizer.parse()` doc: *"Expiry is not checked here"* — any caller forgetting `is_expired()` accepts
  expired tokens.
  **Fix:** persist revocations (DB) or short-lived access + server-side refresh tokens; make `parse()` enforce
  expiry.
- **H3** Recovery codes stored as plain SHA-256, 12 chars / 32-char alphabet (~60 bits)
  (`src/modules/auth_manager/recovery.rs:65-70,117-127`). **Fix:** Argon2id or HMAC with server pepper.
- **H4** TOTP secrets stored in plaintext (`crates/unified-db-core/src/schema.rs:48`, `models.rs:41`;
  `src/modules/auth_manager/totp.rs:207-212`). **Fix:** encrypt at rest (AES-GCM, key in env/KMS).
- **H5** Legacy unpeppered API-key SHA-256 hashes still accepted as fallback
  (`src/modules/apikeys/service.rs:226-237`). **Fix:** sunset date + force-rotate.
- **H6** Cookie auth without CSRF outside the security module (`src/modules/dashboards/service.rs:436` mirrors
  Bearer into a cookie; `CsrfManager` only referenced by the security module).
  **Fix:** require CSRF token on all cookie-authenticated mutations.
- **H7** No password-reset backend exists: `/forgot-password` and `/reset-password` are GET-only pages
  (`src/modules/frontend/module.rs:176-177`); no POST handler/token/email flow anywhere.
  **Fix:** implement a proper reset flow or remove the pages.
- MEDIUM: JWT TTL caller-chosen up to 24h (`auth_manager/tokens.rs:205`); `security` `verify_jwt` never checks
  `iss` (auth_manager does, `tokens.rs:258-298`); `src/modules/files/types.rs:123-129` `disk_path()` has no
  containment check; network-tools `allow_private` is a client-supplied SSRF bypass flag
  (`src/modules/network_tools/tools_net.rs:190-205`, gated only by self-asserted role per C1).

### C14 — Entitlements vs licensing (OPEN item a — PARTIAL)
The named mechanism is **fixed**: the licensing gate now checks subscriptions
(`src/modules/licensing/validation.rs:379-435`, `has_subscription_entitlement` `:331-349`, answered by the store at
`src/modules/store/routes.rs:160-166`, `subscriptions.rs:502-545`); entitlements mirrors it
(`src/modules/entitlements/state.rs:278-324,427-463`). **Residual divergences (same direction — entitlements says
yes, gate says no):** (1) stored-row leg has no gate counterpart — `effective()` counts stored active rows
(`state.rs:178-193`) the gate never consults, and lapse matching (`state.rs:769`) is fragile (tenant-grant rows
get `source_id = tenant_id`, never the subscription id; `granted_via_for` never returns `"subscription"`); a
subscription-cancel can therefore never lapse such a row. (2) Grace asymmetry — `effective()` returns
`ACCESS_GRACE_READ_ONLY` (`state.rs:217-242`) and `enable_module` treats grace as enableable
(`tenant.rs:253-262`), while the gate has no stored-row grace branch, so a grace-state module can be enabled in
entitlements but 403 in provisioning (`src/modules/tenants/provisioning.rs:233`).
**Fix:** one shared `entitled_via_live_sources` function for gate and `effective()`; give stored subscription rows
`source_ref` = subscription id; decide grace once.

### C15 — Scheduler kinds with no listeners (OPEN item e — STILL STANDS)
`approvals.sweep_expiry` and `web3.poll` are in `KNOWN_KINDS` (`src/modules/scheduler/types.rs:136,140`) and
dispatch to emit-only arms (`src/modules/scheduler/service.rs:292-299,320-329`) that return `{"dispatched": true}`
despite doing nothing; module doc admits "no module in the tree listens to either event today"
(`src/modules/scheduler/module.rs:28-34`). Approvals registers exactly one hook (`module.rs:310-319`); Web3 has no
`register_hooks` at all. Real work exists only as HTTP paths.
**Fix:** register listeners (Approvals → existing `sweep_expiry`; Web3 → `poll_watched_addresses`) or remove both
kinds from `KNOWN_KINDS` / return an honest non-success status.

---

## HIGH (remaining)

### API consistency — mostly honoured, small number of real bypasses
Canonical envelope exists (`crates/unified-core/src/json.rs`: `success` → `{"data":…}`,
`error_json`/`to_json(debug)` → `{"error":{"message","code","metadata"}}` + `trace` only when debug) and one
pagination type (`pagination.rs`). No trace leak, no critical envelope breach. Counts: 704 list-like handlers; 660
accept `PaginationParams`, 44 without; `json::respond` 1,564 calls (972 with debug, 420 hardcoded `false`); 45/45
per-module `ok()`/`created()` helpers delegate correctly; exactly **1** direct `HttpResponse::Ok().json(...)` bypass.
- **H1** MCP JSON-RPC bypasses the envelope — `src/modules/mcp/handlers.rs:475` (only direct-JSON response in the
  tree; handler comment explicitly calls it an exception). Fix: ratify MCP as a formal carve-out.
- **H2** Gateway returns bare Pingora errors, no envelope — `src/gateway/proxy.rs:331,359,365,370,378`
  (`respond_error` 503/404/502/426). Fix: render the standard envelope or document the gateway as out-of-scope.
- **H3** `list_invoice_print_jobs` accepts pagination params but ignores them —
  `src/modules/billing/templates_handlers.rs:826` (returns the hook's full jobs list). Fix: paginate with
  `paginate()`/`PageResult`.
- **H4** Service-monitor lists have no pagination at all — `src/modules/service_monitor/system.rs:239`
  (`list_processes`, custom `ProcessesQuery`, limit default 10, no page/total) and `:382,437` (`list_services`,
  no query at all). Fix: accept `PaginationParams` + `PageResult`.
- **M1** `debug=false` hardcoded on a large minority of error paths (420× `json::respond`, 203×
  `error_response`, 12/18 `respond_created`, plus middleware/hardening and `error.rs:103`) — trace never leaks, but
  also never renders even in `development`. Fix: thread `ctx.is_debug()` through every call; delete the no-`debug`
  helper variants.
- **M2** Raw-JSON export downloads skip the envelope (self-documented) —
  `src/modules/analytics/handlers_runs.rs:220,241`, `src/modules/logs/handlers.rs:309,327`. Fix: ratify "file
  downloads are exempt".
- **M3** Per-parent bare arrays, no pagination — `src/modules/site_builder/handlers.rs:299,483` (`list_blocks`,
  `list_sections`), `src/modules/departments/handlers_departments.rs:358` (`list_agents`).
- **M4** Versioning registry exists but is entirely unused — all 47 module overrides return
  `RouteVersionSupport::All` (`crates/unified-core/src/modules.rs:98`); `resolve_public`
  (`handlers_endpoints.rs:739+`) is "resolution only… dispatching is out of scope", so
  `/public/{version}/{path}` never executes a handler; no `Deprecation`/`Sunset` headers. Fix: land one real
  non-`All` declaration + dispatch, or mark versioning unimplemented.
- LOW: two delete payload shapes (`{"deleted": "<id>"}` 320 hits vs `{"deleted": true, "id": …}`); manual envelope
  in `src/modules/bi/handlers.rs:469-471`; custom pagination dialects (`calendar/handlers.rs:169`).

### f64 money (OPEN item d — PARTIAL)
Core amounts are now integer minor units (`i64`) in Rust — billing/pricing/payments struct fields clean — but f64
money survives in three places: `src/modules/store/types.rs:247,262,272` `Coupon.discount_value: f64` (dual-meaning
percent-or-fixed, admitted in comment; converted at use in `pricing.rs:331-332`); `royalty_value: f64`
(`types.rs:904,935,955,1046,1141,1164`, converted in `digital.rs:648,650`); **JSON wire format is f64** —
`crates/unified-core/src/money.rs:37-60` `serde_major::serialize` does `serialize_f64(minor / 100.0)`, so
hook/order payloads carry money as f64 and are re-parsed with `as_f64()` (billing boundary reads:
`billing/state.rs:170,189,208,215,410`, `hooks_payments.rs:426,535`, `hooks_subscriptions.rs:78,82,322,614,715`,
`payments/adapter.rs:450`).
**Fix:** split discount/royalty into `*_percent_bp: i64` + `*_fixed_minor: i64`; switch the wire format to integer
minor units (or decimal string); keep `minor_to_major` only at display edges.

### Logic — medium
- **M1** Pagination `page` has no upper bound — u32 overflow in offset math:
  `crates/unified-core/src/pagination.rs:77` `((page - 1) * per_page) as i64` (page clamped only to ≥1);
  `?page=4294967295` panics in debug / wraps in release. Same pattern in ~20 hand-rolled helpers
  (`store/helpers.rs:73`, `geo/types.rs:104`, `timetracker/validation.rs:60`, `cards/service.rs:98`,
  `expenses/helpers.rs:239`, `gateway/manage.rs:490`). Fix: clamp page in `normalized()` + `saturating_mul`/
  `checked_mul` in the core helper, delete the copies.
- **M2** `HookBus::emit` has no panic isolation — `crates/unified-core/src/hooks.rs:324-360` invokes handlers
  directly in a loop; one panicking handler kills the whole emit and the HTTP request; `.expect("hook bus lock
  poisoned")` panics on poisoned locks. Fix: `catch_unwind` per handler + `unwrap_or_else(|e| e.into_inner())`.
- **M3** Money `f64 → i64` conversions saturate silently — `crates/unified-core/src/money.rs:53-58`
  (`(major * 100.0).round() as i64`; `1e17` → `i64::MAX`, no error). Same in `store/helpers.rs:186-192`,
  `billing/school_fees_types.rs:298-304`. Fix: range-check before the cast.
- **M4** Latvian plurals indexed in wrong form order — `src/modules/i18n/locale.rs:142-156` documents the swap,
  but `catalog.rs:238-247` stores `msgstr_plural` in file order with no swap while `translate_plural` (`:313`)
  indexes with the CLDR index. Fix: normalise form order at `load_po_catalog` for `lv` (audit `ga` too).
- **M5** ACME matrix over-claims wildcard support (client is HTTP-01-only, `order_certificate` correctly rejects
  `*.`); account-key file briefly world-readable (`src/http/acme.rs` — `fs::write` then `set_permissions(0o600)`).
  Fix: report `supports_wildcard: false`; create the key with `OpenOptionsExt::mode(0o600)` atomically.
- **LOW:** `.expect("…still present")` after drop-and-re-get can panic on concurrent delete
  (`store/orders.rs:279,309`, `billing/handlers_invoices.rs:423,456`); downloader 416 path finalises a truncated
  file without a checksum (`src/modules/downloader/engine.rs:178-192`); licensing upgrade accepts a past
  `expires_at` without status correction (`licensing/hooks.rs:518-524`); `mark_provision_failed` bypasses the
  tenant state machine by design (`tenants/provisioning.rs:540-570`).

### Dead code / stubs
- **HIGH:** Backup at-rest encryption not implemented — fail-closed (refuses to run) but any configured encryption
  mode = backups fail entirely (`src/modules/backup/execution.rs:210,215`). TOTP/session-tokenizer/flash helpers
  built but never called — `src/auth/flash.rs` (8 pub fns, 0 callers), `SessionTokenizer`/`SessionPayload` (0 uses
  outside `src/auth/`), `src/auth/totp.rs:48,61` `verify_at`/`current_code` never called. Network-tools scheduled
  checks never run — known gap, no emit of `network_tools.uptime.check_requested` / `cert.check_requested`
  (`src/modules/network_tools.rs:67`). JWT `iss`/`jti` never validated in the security module
  (`src/modules/security/auth.rs:57,61,120-180` — `#[allow(dead_code)]` on both fields; auth_manager does check
  `iss` at `tokens.rs:51`).
- **MEDIUM:** Load balancer state in-memory only, no persistence, no live proxy listeners
  (`src/modules/load_balancer/module.rs:490`, commented-out TLS block `:346+`). AI-support external chat connectors
  store config only (`src/modules/ai_support/types.rs:138`). Domain-verification retries have no scheduler wiring
  (`src/modules/domain_verification/handlers.rs:485`). ACME: 4 dead methods superseded by live variants
  (`src/http/acme.rs:379,536,650,1025`). `unified-core` cache: 7 pub methods never called
  (`cache.rs:155,167,193,1177,1184,1208,1267`).
- **LOW:** 20 `#[allow(dead_code)]` sites (mostly unread struct fields); 7 empty `register_hooks` no-ops; 2 real
  commented-out blocks (`load_balancer/module.rs:346-360`, `src/http/cert_swap.rs:35-36` — stale docs, wiring exists
  inline); 3 real `// TODO` comments total (backup encryption, network-tools gap, load-balancer persistence).
  Clean: 0 `todo!()`/`unimplemented!()`, 0 `unreachable!()` (1 hit is a comment), 0 FIXME/XXX/HACK, 0 `lorem`/
  `mock`, 0 fake data in production handlers, 0 `Ok(vec![])` stubs, 0 "coming soon"/501 handlers. Sampled
  money/auth/provisioning paths: every pub fn referenced.

### OPEN item (b) — table collisions: FIXED
Only one `CREATE TABLE` per name in the whole tree; duplicate scan over all migrations is empty. `certificates`
only in `crates/unified-core/migrations/2026-11-22-000005_certificates/up.sql:31` (SSL table); 000079 now creates
`elearning_certificates` (`000079_elearning/up.sql:199`, header comment `:25-27` explains the rename).
`pricing_tiers` only in `000041_pricing_marketplace/up.sql:84`; 000103 creates 16 tables but no bare
`pricing_tiers`. Single migration dir (`crates/unified-core/migrations/`, 108 migrations). No fix needed —
source-level only; migrations never ran against live PostgreSQL.

---

## SPLIT COMPLETENESS — VERDICT: NOT COMPLETE

Module split and Diesel split are done and verified, but **16 files still exceed 1500 lines (38,241 lines —
6.0% of the tree), 89 files exceed 1000**. Bands: >3000: 4 · >2000: 12 · >1500: 16 · >1000: 89.

### Verification of claimed-done splits
- **(a) Diesel stubs — CONFIRMED in substance, WRONG PATH in the claim.** `src/db/` does not exist. The 759-byte
  stubs (17 lines each, pure re-export shims) are at `crates/unified-core/src/db/schema.rs` and
  `crates/unified-core/src/db/models.rs`; `crates/unified-core/src/db/mod.rs` (195 lines) holds
  pool/migration/tenant-schema helpers + a test asserting one table + one model per db crate resolves through the
  shims.
- **(b) Module registration — CONFIRMED.** `src/modules/mod.rs`: **137 `pub mod`, 138 `::register()` calls**; the
  138th is `unified_calculator::register` (`mod.rs:264`) — the external workspace crate
  `crates/unified-calculator` (`Cargo.toml:100`, `pub fn register` at `crates/unified-calculator/src/module.rs:141`),
  resolved via the extern prelude, so no `mod` declaration is needed. Every pub mod has a backing file/dir
  (`frontend` is dir-only). Side note: `docs/integration/INTEGRATION_NOTES_INSPECTION104.md` says "136 +
  `unified_calculator`" — stale by one; true count is 137 + 1 = 138.
- **(c) `<name>.rs` + `<name>/` pattern — CONFIRMED.** All 137/137 modules have a subdir (spot-checked:
  timetracker 12 files/root 87 lines, documents 9/102, billing 13/87, notifications 8/81, tasks 6/81, ai 13/149).
  40 module roots exceed 100 lines (largest: hr 198, whitelabel 193, pricing_tables 189) — inspected samples are
  doc comments + `pub use` re-exports, i.e. thick-but-legitimate, not logic.

### Remaining oversized files (16) with split proposals
| # | Lines | File | Proposal |
|---|---|---|---|
| 1 | 3863 | `crates/unified-db-commerce/src/models.rs` (256 structs) | Thin root + `models/<group>.rs`: payments 36–474, products 475–829, tax+subscriptions 830–1177, pricing_tables/marketplace/saas_domains 1178–1554, pricing 1555–2018, cards 2019–2070, affiliates 2071–2281, loyalty 2282–2427, procurement 2428–2605, POS 2606–2839, digital_products 2840–3064, marketing 3065–3334, expenses 3335–3573, printing 3574–3750, saas_packages 3751–3863 (~14 files, largest ≈465 lines) |
| 2 | 3350 | `crates/unified-db-operations/src/models.rs` (226 structs) | Same pattern: clusters 36–206, backup 207–303, storage_targets 304–413, load_balancer 414–499, projects 500–764, project_tasks 765–1027, storage_buckets 1028–1197, health 1198–1304, risk 1305–1539, properties 1540–1680, logistics 1681–2033, storage_plans 2034–2273, assets 2274–2450, incidents 2451–2676, field_service 2677–2912, cluster_sync 2913–3041, manufacturing 3042–3350 |
| 3 | 3342 | `crates/unified-db-core/src/models.rs` (220 structs) | identity 34–248, software 249–471, store 472–797, VCS 798–938, API 939–1096, certificates 1097–1184, hosting 1185–1327, tasks 1328–1491, invoices 1492–1609, security/MCP/AI 1610–1874, timetracker 1875–2094, tracker_staff 2095–2294, ai_policies 2295–2506, licensing 2507–2676, domain_verification 2677–2814, whitelabel 2815–2976, entitlements 2977–3164, tenants 3165–3342 |
| 4 | 3266 | `crates/unified-db-platform/src/models.rs` (228 structs) | analytics 32–199, email 200–473, announcements 474–547, notifications 548–633, saved_views 634–694, meetings 695–821, events 822–1108, presets 1109–1210, BI+portals 1211–1381, forms 1382–1506, contracts 1507–1657, inbox 1658–1761, surveys 1762–1916, community 1917–2122, workflows 2123–2233, documents 2234–2442, webinars 2443–2643, quality 2644–2842, approvals 2843–3022, studio 3023–3191, calendar 3192–3266 |
| 5 | 2616 | `crates/unified-core/src/storage.rs` | Thin root (trait + registry + re-exports) + `storage/keys.rs`, `storage/validation.rs`, `storage/local.rs`, `storage/s3.rs` (S3+R2), `storage/drive.rs`, `storage/sftp.rs`, `storage/config.rs`, `storage/events.rs` |
| 6 | 2532 | `crates/unified-db-commerce/src/schema.rs` (128 tables) | Same domain groups as its `models.rs` under `schema/<group>.rs` + `schema/joins.rs` holding all `joinable!` + `allow_tables_to_appear_in_same_query!` groups (a join group must stay whole in one file). Trailing joins ~ll.2243–2532 |
| 7 | 2311 | `crates/unified-db-education/src/models.rs` (164 structs) | elearning 40–364, curriculum 365–700, admissions 701–976, academic 977–1147, academic_records 1148–1431, exams 1432–1753, school_fees 1754–1950, library 1951–2311 |
| 8 | 2213 | `crates/unified-db-platform/src/schema.rs` (114 tables) | Same groups as platform `models.rs` + `schema/joins.rs`; trailing joins ~1925–2213 |
| 9 | 2153 | `crates/unified-db-operations/src/schema.rs` (113 tables) | Same groups as operations `models.rs` + `schema/joins.rs`; trailing joins ~1929–2153 |
| 10 | 2125 | `crates/unified-core/src/hardening.rs` | Thin root + `hardening/settings.rs`, `hardening/state.rs`, `hardening/brute_force.rs`, `hardening/rate_limit.rs`, `hardening/reputation.rs`, `hardening/ip_lists.rs`, `hardening/helpers.rs`, `hardening/middleware.rs` (or `middleware/{headers,ip_filter,rate_limit,request_size}.rs`) |
| 11 | 2108 | `crates/unified-db-infra/src/models.rs` (152 structs) | providers 36–181, webhooks 182–248, jobs 249–317, maintenance/status 318–450, google 451–504, routes 505–553, cloudflare 554–598 + 989–1039, audit 599–694, departments 695–748, calculator 749–859, network_tools 860–988, vault 1040–1209, compliance 1210–1449, gamification 1450–1622, privacy 1623–1822, web3 1823–2041, sessions 2042–2108 |
| 12 | 2083 | `crates/unified-db-core/src/schema.rs` (110 tables) | Same groups as core `models.rs` + `schema/joins.rs`; trailing joins ~1847–2083 |
| 13 | 1647 | `src/modules/timetracker/service.rs` | Thin root + `service/timers.rs`, `service/payroll.rs` (week helpers + `compute_payroll` + `report_rows`), `service/activity.rs`, `service/loading.rs`, `service/hooks.rs`; move the ~492-line `mod tests` (ll.1156–1647, 30% of the file) to `service/tests.rs`. The `timetracker/` dir already exists with 12 files — this is the one file that missed the split |
| 14 | 1586 | `crates/unified-db-people/src/models.rs` (104 structs) | todos 36–221, hr 222–411, hr_payroll 412–597, supervision 598–934, okr 935–1064, recruitment 1065–1250, appointments 1251–1419, holidays 1420–1586 |
| 15 | 1533 | `src/themes/engine.rs` | Thin root + `engine/manifest.rs`, `engine/render.rs`, `engine/lifecycle.rs`, `engine/module_templates.rs` (or fold into existing `src/themes/module_templates.rs`, 597 lines), `engine/tests.rs` (tests ll.1255–1533, ~279 lines) |
| 16 | 1513 | `crates/unified-db-education/src/schema.rs` (82 tables) | Same groups as education `models.rs` + `schema/joins.rs`; trailing joins ~1308–1513 |

All 7 `models.rs` files are the same shape (repeated `Queryable` + `Insertable` struct pairs, zero `impl`/`fn`/`pub mod`) — mechanically splittable; tables and models are in the same domain order per crate (spot-checked on commerce). Just below the cap: `unified-db-infra/src/schema.rs` (1453, 76 tables) and `unified-db-people/src/schema.rs` (1008, 52 tables) — the DB split left **675 tables / 1,350 structs** across 7 crates, every crate's `models.rs` + `schema.rs` still a single flat file. Worst god-files under the cap (1001–1500 band, 73 files): `documents/state.rs` (1482, 53 fns), `ai/gating.rs` (1471, `evaluate_ai_gate` alone ~263 lines), `notifications/handlers.rs` (1464, 38 handlers), `cache.rs` (1450, 92 fns), `tasks/handlers.rs` (1418), `billing/state.rs` (1389), `logistics/shipments.rs` (1388), `email_providers/adapters.rs` (1368), `loyalty/handlers.rs` (1358), `tickets/handlers.rs` (1345), `store/events.rs` (1341). Dominant pattern: `handlers*.rs` / `state.rs` files that absorbed a whole concern.

---

## Verified clean (no finding)
- **Passwords:** Argon2id only — zero `bcrypt` in the tree; OWASP params (m=19456, t=2, p=1), `OsRng` salt, PHC strings, dummy-hash login for unknown accounts (`auth_manager/login.rs:52-56`), lockout. **JWT issue/verify (auth_manager):** HS256 pinned, `none` rejected, constant-time verify, exp/iat/issuer validated, `jti` revocation checked. **SQL:** zero `sql_query!` interpolations; all raw `diesel::sql_query` uses bound parameters; MCP read-only queries interpolate only allowlist-validated identifiers. **SSRF:** webhooks/downloader/network-tools resolve hostnames and require *every* resolved address public (incl. `::ffff:`-mapped IPv4), pin the client to validated addresses, re-validate every redirect. **Secrets in logs:** none found; PAN masked only, CVV in-memory only. **Mass assignment:** `UpdateUserInput` has no role/admin field; LIKE terms escaped. **Store order state machine** (`store/pricing.rs:89-111`), **billing invoice guards**, **licensing transitions**, **tenant lifecycle graph**, **admissions idempotency**, **cache stampede protection** (`cache.rs:1321-1395`), **geo bogon/override handling**, **i18n plurals** (modulo the Latvian bug), **downloader resume/checksum** (modulo the 416 gap), **hook bus ordering/re-entrancy** — all verified clean by the logic agent.
- **API:** no trace leak, no critical envelope breach; 45/45 per-module `ok()`/`created()` helpers correct; `GET /health` compliant; ~35 of the 44 non-paginated list handlers return bounded/static vocabularies (exempt).
- **Open items:** (b) table collisions fixed (above).

---

## What could not be verified
- **No compilation, no live behaviour:** no `cargo check`/build (OOM rule), no running server/DB, no live migration run, no penetration test, no concurrency reproduction (the idempotency/webhook races are code-reading findings, not reproduced). Whether the unauthenticated endpoints (C2) are actually reachable in production depends on deployment reverse-proxy rules (none found in the tree).
- **Reference project absent:** the reference inspection directory does not exist, so adapted SSL/ACME/downloader/cache/hooks/i18n/geo code was checked standalone only, never diffed against the reference.
- **Event inventory residue:** automated pass flagged 83 listeners with no matching emit and ~1,100 emitted names with no in-process listener; most are `*_requested` query hooks answered via `emit_with_results`/direct calls the regex missed, and notification-style emits by design. Unresolved candidates worth a const-resolved re-check: `apikey.created/.revoked/.rotated`, `error.occurred` + `logs.record`, `maintenance.check`, `search.index`, `tenant.destroyed.check`, `whitelabel.branding.resolve/.allowance.check/.domain.verified`, `saas_domains.alias.lookup`, `billing.reminders.run`, `storage.sync.completed`, `store.discount.applied/.failed`, `exams.exam.archived/.closed`, `community.post.replied`, `marketing.unsubscribe`, `newsletter.open/.click`. (Note: the saas_packages doc's OPEN notes about `billing.subscription.renew/cancel_requested` are **stale** — billing does listen, `billing/module.rs:220,228`.)
- **Dead-code detection is grep-heuristic** (no compiler): trait methods via dynamic dispatch, macro-generated call sites, and uses from outside the workspace could be missed; some "dead" pub fns may be intentional public API. Route reachability was not fully enumerated.
- **API:** filter correctness per handler was not audited (only presence); non-`list_*` list endpoints may exist unchecked; WebSocket `/ws` frames not checked; deprecation headers / live version routing not exercised.
- **Security:** whether the `role.check` hook has any listener in a default deployment (if none, capability checks fail closed — safe but the API would be unusable); whether the `saas_domains` resolver is actually answering `tenant.resolve_requested` in production; who calls `SessionTokenizer` in production and whether every caller checks `is_expired()`.
- **Modularity:** only ~40 of 137 modules opened in depth; admin-editability of some defaults (exams settings vocabularies) not traced; whether `register_factory` registries are reachable from config/plugins at runtime not confirmed.
- **Split:** proposed `models/`/`schema/` splits are structurally sound but unproven by a build; schema↔models order parity verified on commerce only; join-group membership per table not audited. The continuation coordinator was editing throughout — line numbers are as-read and may drift.

## Bottom line
1. **Do not expose the API publicly yet** — C1 (self-asserted auth) + C2 (unauthenticated mutations) + C3/C4 (tenant isolation) are systemic, not isolated bugs; they must be fixed before any deploy, ahead of everything else.
2. **Money-path races (C5–C7) and dead provisioning events (C8–C10)** are the next tier — real financial/logic risk.
3. **Modularity (C12) and DashMap-authoritative state (C11)** are architectural and will not be fixed by a single pass — they need the registry-table + DB-read-path programs.
4. **Split is NOT complete:** 16 files >1500 lines (38,241 lines) with concrete per-file proposals above; the 13 `unified-db-*` models/schema files dominate.
5. OPEN items status: (a) PARTIAL (named mechanism fixed, stored-row + grace residuals), (b) FIXED, (c) STILL STANDS, (d) PARTIAL (f64 at the edges/wire), (e) STILL STANDS.

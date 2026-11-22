# INTEGRATION NOTES — BATCH 15 (per-product selling + smart pricing)

Coordinator: batch-15 coordinator (parent task 2026-11-22 ~12:09 WIB).
Builders: B1 entitlements, B2 product subdomains, B3 pricing. All three
completed and verified by coordinator read-back. NOTHING PUSHED.
Remote head untouched (`7cf6853c`).

Per-builder notes (full detail, exact snippets — read these first):
- `INTEGRATION_NOTES_BATCH15_B1.md` (627 lines) — entitlements
- `INTEGRATION_NOTES_BATCH15_B2.md` (667 lines) — product subdomains
- `INTEGRATION_NOTES_BATCH15_B3.md` (885 lines) — pricing

## 1. What was built

| Item | Files | Rust lines | Routes | Migration | Tables |
|---|---|---|---|---|---|
| B1 entitlements (per-product feature limiting) | `src/modules/entitlements.rs` (new) | 3,635 | 20 | `000092_entitlements` | 6 |
| B2 product subdomains | `src/modules/saas_domains.rs` (+1,756; 2,142 → 3,898) | +1,756 | +9 | `000093_product_subdomains` | 2 |
| B3 pricing (smart pricing + discount/proration + reconciliation) | `src/modules/pricing.rs` (new) | 7,216 | 58 | `000097_pricing` | 16 |
| **Batch 15 total** | **2 new modules + 1 extension** | **12,607** | **87** | **3 migrations** | **24** |

Tree at batch-15 verification: 137 `.rs` files in `src/modules/`,
441,216 total Rust lines under `src/`, 106 migration dirs.

### B1 — per-product feature limiting (`entitlements.rs`)
- Entitlement model: union of active sources (`license`, `subscription`,
  `saas_package`, `manual_grant`, plus `owner_entitlement` for sellable
  core modules, mirroring `licensing::check_module_entitlement` exactly).
  Fail closed throughout; no entitlement record = not entitled.
- Dashboard: `GET /api/v1/entitlements/my-modules` — all modules with
  per-module state (entitled with source + expiry, or locked) + upgrade
  path per locked module (hook-resolved, null with reason when
  unanswerable — never fabricated).
- `POST /api/v1/entitlements/modules/{id}/enable` — server-side
  entitlement check first; not entitled → 403 `entitlement.required` +
  audit; fires `module.enabled` for the Extensions Manager.
- Package changes: `package-changes/apply|confirm` — module diff
  (newly entitled / shared / no-longer-entitled); modules used outside
  the package are NEVER silently disabled — they land in
  `requires_confirmation` and need an explicit confirm call.
- Lapse handling: lapse → read-only grace (configurable, default 14d;
  writes rejected `entitlement.grace_read_only`) → disabled. Tenant
  data is NEVER deleted by this flow (invariant documented in the
  module; the only DELETE is admin-only manual-grant row removal with
  history kept).
- Admin: grants CRUD, per-tenant list (paginated + filtered), history,
  denied-attempts, settings, sweep. AI: `ai/suggest` +
  `ai/capabilities` (suggestion only, never purchases).
- Capability check: own `require_capability` helper (mirrors the
  licensing pattern — `licensing.rs` itself has 0 `require_permission`
  hits, the helper name varies by module). 20 call sites = 20 routes.
- `available_as_product = false` (infrastructure).

### B2 — per-product subdomains (`saas_domains.rs` extension)
- `ProductSubdomain` records (`pending_verification | active |
  suspended | released`, `product_id`/`package_id` opaque,
  `custom_domain`, grace fields) in a new DashMap store; settings
  extended (`max_product_subdomains_per_tenant` 10,
  `max_product_subdomains_per_product` 1, grace days 30,
  `require_product_subdomain_approval`, AI suggestion suffixes).
- 9 new routes under `/api/v1/saas/domains/product-subdomains`
  (check, claim, list, get, verify, activate, suspend, release,
  expire-sweep). All 24 original routes intact (RouteInfo 24 → 33).
- Claims are entitlement-gated (3 sources, any positive entitles,
  else 403 with `metadata.code: entitlement.required`); availability
  check never leaks who owns a taken label.
- Product-scoped resolution via the existing
  `tenant.resolve_requested` contract (kind stays `subdomain` +
  `scope: "product"` fields); public helpers `require_product_scope` /
  `product_scope_for_host` / `resolve_product_host`. Grace-period
  release with a new blocked reason `released`. Whitelabel custom-domain
  attach via `whitelabel.domain.verified`. AI suggestion hook
  `saas_domains.ai.suggest_subdomain_requested` (availability-checked,
  suggestions only). 7 emitted + 8 consumed events (incl. a
  `site.provision_requested` listener, closing the packages module's
  open item).

### B3 — smart pricing calculator + discount/proration (`pricing.rs`)
- Recommendations (suggestions only, full line-by-line math, admin
  always confirms — the engine NEVER silently sets a price):
  cost-plus, tiered, bundle (sum of components minus bundle discount);
  market reference prices are human-entered (source + date mandatory,
  never scraped, never guessed); price versions
  `draft → pending_approval → approved → active` with a second-approver
  rule above the configurable threshold (default 10%).
- Discount engine: percent / fixed / buy-X-get-Y / volume; coupons
  with validity windows, global + per-customer limits; declared
  evaluation order, stackable vs exclusive, full evaluation trace;
  one entry point `pricing.discount.evaluate_requested` that never
  mutates an order.
- Proration: per-day/per-month basis, daily rate rounded first, every
  line shows period / daily rate / days used / remaining / credit /
  charge.
- Invoice reconciliation (the accuracy guard): per-component exact
  match (lines + discounts + proration + tax = total); mismatch BLOCKS
  issuing with the exact discrepancy; preview endpoint with
  `can_issue`; issued invoices immutable, corrections via credit notes
  with their own reconciliation.
- MONEY: integer scaled minor units throughout (1 major = 10,000,
  exactly NUMERIC(19,4)); basis points for all percents; half-up,
  per-line-then-sum rounding documented and enforced. `f64`: 0 in
  `pricing.rs`. 26 unit tests (arithmetic walked through by reading).
- Calculator conventions reused from `calculator.rs`, not rebuilt.

## 2. Registration (integrator)

- `src/modules/mod.rs`: add `pub mod entitlements;` and
  `pub mod pricing;` (exact registration + `Module` impl details are
  in the per-builder notes §1/§2). `saas_domains` is already registered
  (B2 is an extension, no new mod line).
- `src/db/schema.rs` / `src/db/models.rs`: Diesel tables for all 24
  new tables — exact snippets in per-builder notes (B1 §4, B2 §2,
  B3 §2). In-memory DashMap stores are authoritative this round;
  Diesel tables are persistence targets only (same pattern as the
  rest of the tree).
- Capabilities to seed: `entitlements:read`, `entitlements:manage`,
  `entitlements:grant`; `pricing:read`, `pricing:manage`,
  `pricing:approve`; product-subdomain capabilities per B2 notes §7.
- ApiAccessRule rows + README/TODO lines: per-builder notes.
- Scheduler kinds to add (see B1/B2/B3 notes): entitlement expiry
  sweep, product-subdomain expire sweep, price-approval reminders.
- Hook contracts: full emitted/consumed event lists in per-builder
  notes. MUST-WIRE items:
  - Billing MUST answer `pricing.invoice.validate_requested`
    (B3 notes §6.4 snippet) — until then reconciliation is
    HTTP-available but NOT enforced at invoice-issue time.
  - Extensions Manager has no `module.enabled` hook subscriber —
    B1 firing the event is a handoff, not activation.
  - Store coupon convergence: store keeps its own float coupons;
    pricing is the shared evaluator via hook (B3 notes §6).
  - `saas_packages.package.modules_requested` has no answerer
    (B2 OPEN 8); `saas_packages.rs` is a 479-line partial draft
    (no `Module` impl, no routes/hooks) — batch 14's follow-up.

## 3. Migration numbers (batch 15 + tree state)

- Batch 15 used (all verified unique at verification time):
  `000092_entitlements`, `000093_product_subdomains`,
  `000097_pricing`. Collision history: B1 was assigned 000091 but
  batch 14's `000091_library` landed first; B2 was assigned 000092
  but B1's `000092_entitlements` landed mid-task; B3 was assigned
  000093 but B2's `000093_product_subdomains` landed mid-task —
  each builder moved to the next free number and recorded it.
- Tree-wide duplicates at verification (106 migration dirs):
  **000056, 000070, 000075, 000077, 000080, 000090 (×2 each)** +
  `000100_calculator` out of sequence. B1's report claimed 000093
  (×2) — NOT reproduced by the coordinator's listing (only one
  `000093_product_subdomains` dir exists). 000068 / 000074 / 000081
  verified SINGLE (earlier claims stale).
- Other batches' migrations that landed during batch 15:
  000094 tenant_isolation (batch 17), 000095 calendar + 000098
  holidays (batch 16), 000096 saas_packages (batch 14),
  000099 cards (batch 18).

## 4. TENANT-SCOPED DATA ACCESS (standing rule, user 2026-11-22 12:13)

Every module resolves its data through the tenant context — tenant id
(uuidv7) + tenant schema (search_path) + package entitlements — and
NEVER queries the main/control-plane DB directly for tenant business
data. Control-plane tables (tenants, subscriptions, domains, billing,
licensing) stay in `public` and are accessed only through their own
modules/events. No tenant context → fail closed, never silently fall
back to `public`. No hard-coded tenant ids, schema names, or package
ids.

Per-builder status (coordinator-verified):
- **B1 (entitlements) — IMPLEMENTED against live plumbing.**
  `src/core/tenancy.rs` EXISTS (landed mid-task, batch 17:
  `TenantContext` extractor, `schema_name()`,
  `with_tenant_connection`, `run_tenant_migrations`,
  `ControlPlaneGuard`). B1 re-based onto it: every tenant endpoint
  resolves the tenant ONLY via
  `crate::core::tenancy::TenantContext::from_http_request`, mapped to
  403 `tenant.required` with no data when absent; every row carries
  uuidv7 `tenant_id`; the six tables are documented as tenant-schema
  tables; control-plane sources read only via licensing APIs/events
  (zero direct table references, grep-verified); the old
  `X-Company-Id` fallback was removed; `TenantScope` also reports
  the derived `schema_name`. B1 notes §2.
- **B2 (product subdomains) — recorded, plumbing claimed absent.**
  B2's notes §13 record: product-subdomain records are domain
  (control-plane) data in `public`; the module never reads tenant
  business data, scopes every handler by company, and fails closed.
  **COORDINATOR CORRECTION:** B2 wrote "batch 17 plumbing not on
  disk" — `src/core/tenancy.rs` (21,071 bytes, 27 `schema_name`
  hits) landed 05:14 UTC, before B2 finished. The integrator must
  re-check B2's handlers against `src/core/tenancy.rs` instead of
  treating B2 §13's "absent" note as current; the substance (scope
  by company, fail closed) is unchanged.
- **B3 (pricing) — recorded, same correction.** B3's notes §5:
  every record carries `tenant_id`; `caller()` resolves
  `TenantContext::from_request` first, all 58 handlers fail closed
  (403) with no tenant context; lists filtered, single-record access
  404s across tenants; settings per-tenant; hooks require
  `tenant_id` (uuidv7-validated) and emit it in every event. Currency
  registry is the one documented exception (global ISO reference
  data, endpoints still tenant-gated). B3's convergence point is
  `resolve_tenant()` in `pricing.rs`, following batch 16's calendar
  pattern. **Same correction as B2:** batch 17's `tenancy.rs` has
  since landed — the integrator should re-point B3's convergence
  from `resolve_tenant()` to `crate::core::tenancy` where it fits,
  and re-check B3 against it.
- Batch-17 defect carried forward (found by batch 17's own
  coordinator, confirmed by B1): migration 000094's CHECK
  `^tenant_[0-9a-f_]{36}$` does NOT match real `tenant_`+32-hex
  schema names — EVERY real tenant row would fail the CHECK; must
  become `^tenant_[0-9a-f]{32}$`. Also batch 17's B2 seam does not
  match B1's real API (assumed `TenantContext::new` /
  `context.schema()` / `create_tenant_schema` /
  `run_tenant_migrations` — none exist); fix is isolated to the
  seam functions.
- Naming collision warning (batch 17): two types named
  `TenantContext` (middleware vs tenancy — alias imports).

## 5. Coordinator verification (read-back; cargo check NEVER ran)

- Line counts match builder reports exactly (entitlements 3,635;
  saas_domains 3,898; pricing 7,216). Route counts: `.route(`
  20 / 33 (24 original + 9 new) / 58. Tables: CREATE = DROP in all
  three migrations (6/6, 2/2, 16/16 — B3's first "17 DROP" count
  was a comment line; actual DROP statements are 16).
- Forbidden reference-project name: 0 in all batch-15 module +
  migration files. (One mention found in B3's NOTES file line 805 —
  the reference inspection path inside a compliance sentence —
  rephrased by the coordinator; re-grepped 0.)
- Indonesian words: 0 (builders scanned; coordinator spot-checked).
- `todo!` / `unimplemented!`: 0. `f64` in `pricing.rs`: 0.
- Capability checks: B1 20 `require_capability` call sites = 20
  routes; B2 every new route capability-checked via
  `transition_product_subdomain`; B3 59 `require_permission` uses
  for 58 handlers (extra is the helper definition).
- Delimiters balanced in all three files (builders verified).
- **NOT verified: `cargo check` / `cargo build` NEVER ran anywhere
  in this project (no toolchain, crates.io blocked) — compilation
  is NOT verified for any batch-15 item, and no migration ever ran
  against live PostgreSQL. No endpoint was live-tested.**

## 6. Combined OPEN list (full lists in per-builder notes §OPEN)

1. No compiler run; no live migration run; no live HTTP test.
2. Billing stores `f64` money (pre-existing); B3's hook snippet
   converts via 4-decimal strings — full proof needs integer money
   in billing.
3. Billing has no `pricing.invoice.validate_requested` listener yet
   (integrator must add B3 §6.4 snippet).
4. Extensions Manager has no `module.enabled` subscriber (B1's
   enable is a handoff, not activation).
5. `saas_packages` and `tenants` unregistered, so their events
   don't fire yet; `saas_packages.rs` is a partial draft.
6. Only licensing entitlement is live — store/packages hooks have
   no answerers, so B2 claims fail closed 403 until one lands.
7. Middleware must gain product-scope fields in `TenantContext` +
   a `Released` blocked variant with export notice (safe blocked
   fallback today) — B2.
8. `site_id` gap: unprovisioned product subdomains resolve as
   `Unresolved` (pre-existing, shared with generic allocations).
9. Hook handlers can't emit events (no bus) — event-driven releases
   change state silently (B2).
10. Pre-existing alias-verification bug (handler reads a `verified`
    boolean the verification module never sends; left untouched).
11. Whitelabel attach ambiguous for multi-product tenants (no
    product id in payload).
12. In-memory stores authoritative this round; durable per-schema
    Diesel wiring still to land; a restart loses grants made since
    the last durable write (B1).
13. Owner-entitlement consequence: sellable core modules are
    implicitly entitled per licensing's rule, so per-product limiting
    bites hardest on non-core sellable modules (B1).
14. `mod.rs` declares only 72 of 137 module files (pre-existing,
    tree-wide; calculator/pricing_tables unwired too).
15. Batch-17 CHECK defect + seam mismatch (see §4) — integrator
    must-fix before any tenant row is written.
16. AI surfaces are hook-based / template drafts, not live LLM
    calls (B1/B3); the `AiCapability` /
    `register_ai_capabilities` / `run_ai_capability` API does NOT
    exist in `ai.rs` (B1 verified).
17. B2/B3 tenant-plumbing notes are stale (see §4 correction) —
    re-check against `src/core/tenancy.rs`.
18. Tree-wide migration duplicates (see §3) — integrator must
    renumber into a gapless sequence before any migration run.

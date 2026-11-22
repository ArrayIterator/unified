# Integration Notes — Batch 15, Builder B2: Per-Product Subdomains

- **Builder:** B2 (per-product subdomains)
- **Date:** 2026-11-22
- **Files changed:** `src/modules/saas_domains.rs` (2,142 → 3,898 lines, **+1,756**)
- **Files added:** `migrations/2026-11-22-000093_product_subdomains/{up,down}.sql`
  (2 tables; see §1)
- **Files NOT touched:** `src/modules/mod.rs`, `src/db/schema.rs`,
  `src/db/models.rs`, `README.md`, `TODO.md`, `LICENSE`, tracker, `MEMORY.md`.
  Nothing pushed to GitHub. No binary files.
- **Existing routes:** all 24 original `saas_domains` routes intact
  (RouteInfo 24 → 33; `configure_routes` `.route(` lines 24 → 33).
  Existing behaviour (reservations, aliases, allocations, resolve,
  settings) was extended, never rewritten.

## What was built

Each purchased product/package claims **its own subdomain**
(`<label>.<wildcard_base>`, e.g. `siakad.<base>` for SIAKAD and
`exams.<base>` for online exams). A product subdomain resolves to
(tenant, product/package scope) and only ever serves that product's
module surfaces. Claims are entitlement-gated server-side; release
starts a grace period first and frees the label only after the
grace ends.

### 1. Schema — migration `2026-11-22-000093_product_subdomains`

**Migration-number collision (recorded, not renumbered):** 000092 was
free when this task started, but `2026-11-22-000092_entitlements`
(a parallel batch 15 builder) landed while this migration was being
written. Per the brief, the other migration was NOT renumbered;
this one uses **000093** (next free; 000091 is `000091_library`,
000090 exists twice, 000100 is out of sequence).

`up.sql` creates 2 tables; `down.sql` drops 2 tables (CREATE = DROP = 2).

```sql
CREATE TABLE saas_product_subdomains (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id UUID NOT NULL REFERENCES companies (id) ON DELETE CASCADE,
    product_id VARCHAR(128) NOT NULL CHECK (char_length(product_id) BETWEEN 1 AND 128),
    package_id VARCHAR(128) CHECK (package_id IS NULL OR char_length(package_id) BETWEEN 1 AND 128),
    label VARCHAR(63) NOT NULL,
    host VARCHAR(253) NOT NULL,
    custom_domain VARCHAR(253),
    site_id UUID,  -- no FK on purpose; SaaS module owns site rows
    status VARCHAR(32) NOT NULL DEFAULT 'pending_verification'
        CHECK (status IN ('pending_verification', 'active', 'suspended', 'released')),
    claimed_by TEXT NOT NULL,
    claimed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    verified_at TIMESTAMPTZ,
    released_at TIMESTAMPTZ,
    grace_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE saas_product_subdomain_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_subdomain_id UUID NOT NULL REFERENCES saas_product_subdomains (id) ON DELETE CASCADE,
    event VARCHAR(64) NOT NULL CHECK (char_length(event) BETWEEN 1 AND 64),
    actor TEXT NOT NULL,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Partial unique indexes (live rows only; `released` rows are excluded
on purpose — the grace rule is computed from `grace_until` in the
module, see §3):
- `uq_saas_product_subdomains_host_live` on `(host)`
  `WHERE status IN ('pending_verification','active','suspended')`
- `uq_saas_product_subdomains_tenant_product_package_live` on
  `(company_id, product_id, package_id)` same status filter
  `AND package_id IS NOT NULL`
- `uq_saas_product_subdomains_tenant_product_live` on
  `(company_id, product_id)` same status filter
  `AND package_id IS NULL`
- plus lookup indexes on `company_id`, `status`, `product_id`,
  `label`, `custom_domain`, `grace_until`,
  and on the events table `(product_subdomain_id)`, `(event)`.

### 2. Models — exact additions to `src/db/schema.rs` / `src/db/models.rs`

**Integrator: add these; do not edit the migration to match.**
The in-memory DashMap store stays authoritative at runtime (same
pattern as the rest of this module); the tables are persistence
targets.

```rust
// schema.rs (Diesel table! blocks; add the tables to
// allow_tables_to_appear_in_same_query! alongside the other
// saas tables):
diesel::table! {
    saas_product_subdomains (id) {
        id -> Uuid,
        company_id -> Uuid,
        product_id -> Varchar,
        package_id -> Nullable<Varchar>,
        label -> Varchar,
        host -> Varchar,
        custom_domain -> Nullable<Varchar>,
        site_id -> Nullable<Uuid>,
        status -> Varchar,
        claimed_by -> Text,
        claimed_at -> Timestamptz,
        verified_at -> Nullable<Timestamptz>,
        released_at -> Nullable<Timestamptz>,
        grace_until -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    saas_product_subdomain_events (id) {
        id -> Uuid,
        product_subdomain_id -> Uuid,
        event -> Varchar,
        actor -> Text,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}
```

```rust
// models.rs (Queryable/Insertable pair, following the module's
// existing model conventions):
#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable)]
#[diesel(table_name = crate::db::schema::saas_product_subdomains)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProductSubdomainRow { /* same fields as the struct below */ }

#[derive(Debug, Clone, Serialize, Deserialize, Insertable)]
#[diesel(table_name = crate::db::schema::saas_product_subdomains)]
pub struct NewProductSubdomainRow { /* same fields minus id/timestamps */ }
```

The runtime record (already in `saas_domains.rs`):

```rust
pub struct ProductSubdomain {
    pub id: Uuid,
    pub company_id: Uuid,
    pub product_id: String,              // opaque id: products-module id or sellable module id
    pub package_id: Option<String>,      // None = standalone product claim
    pub label: String,
    pub host: String,                    // "<label>.<wildcard_base>"
    pub custom_domain: Option<String>,   // verified whitelabel domain, same product scope
    pub site_id: Option<Uuid>,
    pub status: String,                  // pending_verification | active | suspended | released
    pub claimed_by: String,              // X-User-Id actor (or "system:saas_packages" for event claims)
    pub claimed_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    pub grace_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
pub struct ProductScope {
    pub company_id: Uuid,
    pub product_id: String,
    pub package_id: Option<String>,
    pub product_subdomain_id: Uuid,
    pub host: String,
}
```

### 3. Settings — `SaasDomainsSettings` extended (admin-editable)

New fields (factory defaults in parentheses; all under the
`saas_domains.*` config keys, editable via `PUT /settings`):
- `max_product_subdomains_per_tenant: u32` (**10**) — live product
  subdomains per tenant across all products.
- `max_product_subdomains_per_product: u32` (**1**) — live product
  subdomains per tenant per product (and per package when named);
  the default of 1 enforces "one active product subdomain per
  (tenant, product/package)".
- `default_product_subdomain_grace_days: u32` (**30**) — days between
  release and label reclaim.
- `require_product_subdomain_approval: bool` (**false**) — when true,
  activation needs `saas_domains.manage`; otherwise the owning
  tenant may activate its own verified record.
- `product_subdomain_suggestion_suffixes: Vec<String>`
  (**`["", "-app", "-online", "-portal", "-hub", "-2", "-3"]`**) —
  suffixes the AI suggestion hook appends to a slugified product name.

Validation bounds are enforced in `SaasDomainsSettings::validate`
(quotas 1–1000 / 1–100, grace 0–365, suffixes must be empty or valid
label suffixes).

### 4. New routes (9) — all under `/api/v1/saas/domains/product-subdomains`

| Method | Path | Capability | Behaviour |
|---|---|---|---|
| POST | `/check` | any of `manage`/`read`/`self_assign` | Availability: `{label, host, available, status: available\|taken\|reserved\|invalid, reason}`. Never names the owner of a taken label. |
| POST | `/` | `authorize_company` (manage, or self_assign + own `X-Company-Id`) | Claim. Entitlement checked first (403 `entitlement.required`, see §6); label pattern + reserved list; global host uniqueness (all stores + `domains` table + hooks); per-tenant and per-product quotas; creates `pending_verification`. 201. |
| GET | `/` | `authorize_list` (admin sees all; others force-scoped to own company) | Paginated; filters `company_id`, `product_id`, `package_id`, `status`; search over host/label/product. |
| GET | `/{id}` | `authorize_company` | One record. |
| POST | `/{id}/verify` | `authorize_company` | Marks verified **only after** the domain-verification module confirms it: queries `domain.verification.status_requested` for the host (and custom domain) and requires `status == "verified"`; otherwise 422. Idempotent when already verified. Sets `verified_at`; emits `.verified`. |
| POST | `/{id}/activate` | admin-only when `require_product_subdomain_approval`, else `authorize_company` | `pending_verification` (requires `verified_at`) or `suspended` → `active`. Emits `.activated`. |
| POST | `/{id}/suspend` | `authorize_company` | `active`/`pending_verification` → `suspended`. Emits `.suspended`. |
| POST | `/{id}/release` | `authorize_company` | `active`/`suspended`/`pending_verification` → `released`, `released_at = now`, `grace_until = now + grace days`. Emits `.release_started` (**not** `.released`). |
| POST | `/expire` | `saas_domains.manage` | Admin sweep: finalizes released records past grace; emits `.released` per record; returns `{finalized, ids}`. The record stays `released` (audit trail); the label is free because the availability check treats past-grace releases as non-blocking. |

All mutations emit a lifecycle event carrying the actor and the
tenant/product scope (audit trail; see §7).

### 5. Middleware / resolver contract (for the integrator)

**What already works unchanged:** `resolve_host` /
`tenant.resolve_requested` keeps its order and its stable blocked
reason codes (`unverified_domain`, `suspended`). Product-subdomain
resolution is inserted **after** the main-host check and **before**
the generic allocation check. A product host returns kind
`"subdomain"` (the only kind `TenantResolutionMiddleware`
accepts) **plus** extra fields, so the middleware keeps working
with no code change:

```json
{
  "tenant_id": "<company uuid>", "company_id": "<company uuid>",
  "site_id": "<uuid or null>", "kind": "subdomain",
  "verified": true, "host": "<host>",
  "scope": "product",
  "product_id": "academic",
  "package_id": "online-exam-saas",
  "product_subdomain_id": "<uuid>",
  "custom_domain": false
}
```

Resolution outcomes for a product host:
- `active` + verified → tenant result above (scope fields included).
- `pending_verification` (or active but unverified) → blocked
  `unverified_domain`.
- `suspended` → blocked `suspended`.
- `released` inside grace → blocked with the **new** reason code
  **`released`** (see below).
- `released` past grace → no result (label free; falls through to
  the normal resolvers).
- A verified custom domain attached to the record resolves the same
  way (`"custom_domain": true`).

**Integrator steps (exact):**
1. **Product-scope enforcement.** The middleware today attaches
   `TenantContext { tenant_id, company_id, site_id, kind, host }`
   and drops the scope fields. Two options, in order of preference:
   (a) extend `TenantContext` with `product_id: Option<String>`,
   `package_id: Option<String>`, `product_subdomain_id: Option<Uuid>`
   and populate them from the hook result in
   `TenantResolutionMiddleware::resolve` (the `kind` match must
   keep accepting `"subdomain"`); then enforce per-request that the
   dispatched module is in scope (404 `product_scope.mismatch`
   otherwise). (b) Until (a) lands, every product-module handler
   must call the provided guard at its top (it is public in this
   module):
   ```rust
   match crate::modules::saas_domains::require_product_scope(&ctx, &req, "exams")? {
       Some(scope) => { /* request is on this product's subdomain; scope.company_id is the tenant */ }
       None => { /* main host or generic tenant host: no product restriction */ }
   }
   ```
   `require_product_scope` returns `Ok(None)` off product hosts,
   `Err(403)` on a blocked product host, and `Err(404,
   product_scope.mismatch)` when the host is scoped to a different
   product (404 deliberately never confirms which product the host
   does serve). Product match is `scope.product_id == module_id`,
   or package membership answered through
   `saas_packages.package.modules_requested` (`{"package_id"}` →
   `{"module_ids": [...]}`); when nothing answers, only a direct
   product match counts. Helper `product_scope_for_host(host)` and
   `resolve_product_host(ctx, host)` (emits `.resolve_scoped`) are
   also public.
2. **Control-plane paths.** Product subdomains must never serve
   control-plane paths: the middleware's existing
   `blocked_paths`/`DEFAULT_BLOCKED_PATHS` already blocks them on
   any resolved tenant host, and product hosts resolve as tenants,
   so they inherit that block. No new path list is needed; verify
   the block still applies after step 1(a).
3. **Released-grace notice.** The middleware maps unknown blocked
   reasons to `UnverifiedDomain` today, so a released-in-grace host
   is blocked and never serves tenant data (safe), but shows the
   generic blocked response instead of the required "released —
   export your data" notice. Integrator: add a `Released` variant
   to `BlockedReason` in `src/core/middleware.rs` for reason
   `"released"`, serve the export notice (data export runs on the
   main dashboard host, not on the released host), and keep
   refusing to serve the product.
4. **`site_id` gap (pre-existing, same as generic allocations).**
   The middleware treats a tenant answer with a missing/empty
   `site_id` as `Unresolved` (falls through to the control-plane
   policy). Product subdomains provisioned without a `site_id`
   therefore do not serve through the middleware until a site is
   provisioned (via `site.provisioned` / the allocation flow).
   Same limitation already applies to generic allocations; not
   introduced here.
5. **Main host untouched.** Main-host → control-plane behaviour is
   unchanged (checked before any product lookup).

### 6. Entitlement — fail-closed, three sources

`POST /product-subdomains` (and only it) verifies entitlement.
`check_product_entitlement` asks every source; **any** positive
answer entitles; nothing positive → **403** with
`metadata.code = "entitlement.required"`:
1. `product.entitlement.check` (`{"product_id","user_id"}` →
   `{"entitled": bool}`) — products module, backed by
   `store.entitlement.check`.
2. `saas_packages.entitlement.check_requested`
   (`{"company_id","module_id","allowed": false}` → `allowed`) —
   asked once for the product id and once for the package id.
3. Licensing direct check (`check_module_entitlement`) — only when
   the product id names a registered **sellable** module
   (`available_as_product() == true`); non-sellable/unknown ids
   never pass through this source, so a core-module id cannot be
   used to claim without a purchase.

**Live-today status (verified by reading the sources):** the
licensing path is live for sellable modules — `academic`
(SIAKAD) and `exams` (online exams) both return
`available_as_product() == true`. The products/store path
reports `entitled: false` with `no_entitlement_source` until the
store module answers `store.entitlement.check` (products.rs
documents that no handler exists today). The packages hook has
no answerer today (saas_packages' own OPEN note). Net effect,
by design: claims succeed today only via a real licensing
entitlement; all other claims fail closed with 403. That is the
specified behaviour, not a bug — but the integrator should know
claims will 403 until one of the other sources lands.

`product_id` / `package_id` are opaque validated strings
(1–128 chars, lowercase letters/digits/`-`/`_`); nothing
hard-codes product, package, tenant, or schema names.

### 7. Hook contracts

**Emitted** (all carry `product_subdomain_id`, `company_id`,
`product_id`, `package_id`, `label`, `host`, `status`, `actor`):
- `saas.product_subdomain.claimed`
- `saas.product_subdomain.verified`
- `saas.product_subdomain.activated`
- `saas.product_subdomain.suspended`
- `saas.product_subdomain.release_started` (release endpoint)
- `saas.product_subdomain.released` (grace-expiry sweep only)
- `saas.product_subdomain.resolve_scoped` (from
  `resolve_product_host` on an active product resolution)

**Consumed** (all handlers are defensive — payloads they cannot
scope safely are skipped, never guessed):
- `domain.verification.completed` — flips a matching product
  subdomain to verified. Accepts the real payload
  (`status: "verified"`) **and** `verified: true` / a
  `product_subdomain_id`. Note the pre-existing alias handler
  only reads the `verified` boolean, which the verification
  module does not send (see OPEN).
- `saas_packages.purchase.completed` — claim intent only when
  the payload carries `entitlement_verified: true` **and** a
  `subdomain` label (+ `company_id`, `product_id`/`module_id`).
  Idempotent (an existing live record is returned, never
  duplicated).
- `saas_packages.cancelled` — `{"company_id","package_id"}`
  starts release (grace) for that package's live product
  subdomains.
- `license.revoked` and `module.entitlement.denied` — suspend
  the affected live product subdomains. Payload spellings differ
  (`product` / `product_id` / `module_id` are all read); a
  payload without `company_id` is skipped.
- `site.provision_requested` — claim intent trusted when
  `source == "saas_packages"` (that module validates entitlements
  in its provisioning step before emitting). Answers
  `{"claimed", "product_subdomain_id" | "subdomain_pending",
  "reason"}`. This closes the packages module's open item
  ("the domains flow has no provision-request listener today").
- `whitelabel.domain.verified` — `{"company_id","domain"}`
  attaches the domain as `custom_domain` on the tenant's product
  subdomain, inheriting its product scope. Only `.verified` is
  listened to, never `.requested`. Attach happens only when the
  tenant holds exactly one live product subdomain (see OPEN).
- `saas_domains.ai.suggest_subdomain_requested` — see §8.
- `saas.product_subdomain.expire_grace` — scheduler sweep;
  answers `{"finalized", "ids"}`.

**Hook-handler limitation (same as the existing alias handler):**
hook handlers have no `AppContext` and no hook bus, so they can
mutate the stores but cannot re-emit events and cannot run the
cross-module checks (`domain_claimed`, `company_exists`,
entitlement hooks). Consequences, recorded honestly:
- Event-driven releases (package-cancelled) change state but
  cannot emit `.release_started`; the HTTP release endpoint and
  the expire sweep are the emitting paths.
- The hook-side grace sweep mutates silently; the HTTP
  `/expire` endpoint emits `.released`.
- Hook-side claims use a store-only conflict check (product
  store + reservations + aliases + allocations); the full
  cross-module check (`domains` table + hooks) runs on the HTTP
  claim path.

**Audit:** every mutation emits a lifecycle event with the actor.
Add these names to `AUDITED_EVENTS` in `src/modules/audit.rs`
(integrator; audit.rs was not edited here):
`saas.product_subdomain.claimed`, `.verified`, `.activated`,
`.suspended`, `.release_started`, `.released`.

### 8. AI wiring

Hook `saas_domains.ai.suggest_subdomain_requested`, payload
`{"product_name" | "product_id", "company_id"?, "count"?}`
(count clamped 1–10, default 5). Answered with
`{"product_name", "suggestions": [{"label","host","available": true}]}`
— availability-checked against the live store (label pattern,
reserved list, all stores). Suggestions only; nothing is
reserved. The AI module gates the call through its own
contracts before emitting; this handler never calls the AI
module back. No direct `ai.rs` dependency was added (the
`AiCapability` / `register_ai_capabilities` API the packages
module references did not exist in `ai.rs` when checked;
the hook-only shape avoids depending on unlanded API).

### 9. Whitelabel tie-in

A verified custom domain replaces the product subdomain for the
same product scope: `whitelabel.domain.verified` attaches it as
`custom_domain` on the record, and resolution (§5) serves it
with the same scope. The whitelabel flow itself is reused, not
rebuilt: this module never verifies a domain (verification stays
with the domain-verification module) and never touches
whitelabel branding. Events: emits `whitelabel.domain.requested`
is the whitelabel module's own emission (its domains flow);
this module listens to `whitelabel.domain.verified` only.

### 10. Capability seeds & API Management rows

No new capability names were introduced — the existing ones are
reused on every new route (verified per handler; activate /
suspend / release go through `transition_product_subdomain`,
which checks `caller_from` + `require_permission(manage)` when
approval is required, else `authorize_company`):
- `saas_domains.manage` — admin: settings, `/expire` sweep,
  activation when approval is required, acting for any tenant.
- `saas_domains.read` — list reads (force-scoped to own company
  for non-admins).
- `saas_domains.self_assign` — tenant acting on its own records
  (requires matching `X-Company-Id`).

The roles module already seeds these three (they predate this
round); no new seed rows are needed. If a role seed list is
regenerated, keep the three names exactly.

`ApiAccessRule` rows for the versioned public API registry
(`api_manager`; `public_access` is default-deny — all nine are
authenticated dashboard endpoints, **not** public):

| method | path | version | allowed | public_access |
|---|---|---|---|---|
| POST | `/api/v1/saas/domains/product-subdomains/check` | v1 | true | false |
| POST | `/api/v1/saas/domains/product-subdomains` | v1 | true | false |
| GET | `/api/v1/saas/domains/product-subdomains` | v1 | true | false |
| GET | `/api/v1/saas/domains/product-subdomains/{id}` | v1 | true | false |
| POST | `/api/v1/saas/domains/product-subdomains/{id}/verify` | v1 | true | false |
| POST | `/api/v1/saas/domains/product-subdomains/{id}/activate` | v1 | true | false |
| POST | `/api/v1/saas/domains/product-subdomains/{id}/suspend` | v1 | true | false |
| POST | `/api/v1/saas/domains/product-subdomains/{id}/release` | v1 | true | false |
| POST | `/api/v1/saas/domains/product-subdomains/expire` | v1 | true | false |

All nine are registered in `route_info()` (RouteInfo 24 → 33),
so the registry collects them; the rows above are the
access-control records the API manager owns.

### 11. README / TODO lines (for the integrator — not edited here)

README (SaaS Domains section, append):
> Per-product subdomains: each purchased product or package claims
> its own subdomain under `/api/v1/saas/domains/product-subdomains`
> (availability check, entitlement-gated claim, verify, activate,
> suspend, grace-period release, admin grace-expiry sweep).
> Resolution is product-scoped — a request on a product subdomain
> only serves that product; see `INTEGRATION_NOTES_BATCH15_B2.md`.

TODO (append):
> - [ ] Batch 15 B2: extend `TenantContext` with product-scope
>   fields and enforce scope in `TenantResolutionMiddleware`
>   (see notes §5 step 1).
> - [ ] Batch 15 B2: add `Released` blocked reason + export
>   notice for released-in-grace product subdomains (§5 step 3).
> - [ ] Batch 15 B2: add the seven `saas.product_subdomain.*`
>   events to `AUDITED_EVENTS` in `audit.rs` (§7).
> - [ ] Batch 15 B2: Diesel persistence layer for
>   `saas_product_subdomains` / `saas_product_subdomain_events`
>   (schema/models snippets in notes §2).

### 12. Convergence with batch 14 `saas_packages`

`src/modules/saas_packages.rs` **exists on disk but is a partial
draft**: 479 lines — header docs, settings, constants, and event
names only; no `Module` implementation, no routes, no
`register_hooks` body. Verified facts used by this round:
- Events (names as documented there):
  `saas_packages.purchase.completed`,
  `saas_packages.cancelled`,
  `saas_packages.entitlement.check_requested`
  (`{"company_id","module_id","allowed": false}`),
  `site.provision_requested`
  (`{"purchase_id","company_id","subdomain","package_id",
  "source": "saas_packages"}`).
- Its seed package names the `exams` module (exists, id `exams`)
  and `elearning` / `academic` (exist; `academic` id is
  `academic`); `saas_domains` and `presets` are event-only
  integrations there (neither registered in `mod.rs`).
- Its own OPEN notes record that the domains flow had no
  provision-request listener — **this round adds it** (§7).
- Convergence point for the integrator: when `saas_packages`
  lands its `Module` implementation, confirm (a) the payload
  shapes above are unchanged, (b)
  `saas_packages.entitlement.check_requested` gains an answerer
  (nobody answers it today — see §6), and (c) the
  `saas_packages.package.modules_requested` query used by
  `product_scope_allows` gains an answerer (no such event exists
  in `saas_packages.rs` today; until it does, package-scoped
  resolution only matches the product id directly).

### 13. Tenant-scoped data access (standing rule)

This section answers the standing rule broadcast during this
round. (The handoff named `INTEGRATION_NOTES_BATCH15_B3.md` —
the pricing builder's file. This session is builder B2, so the
rule is recorded here in the B2 notes; B3's file was not
written to.)

**How it applies to this module:**
- Product-subdomain records are **domain** records. Domains are
  explicitly listed as control-plane tables (tenants,
  subscriptions, domains, billing, licensing), so the migration
  tables in §1 live in the **`public`** schema as control-plane
  data. No tenant schema is created or referenced by this module.
- This module **never reads tenant business data**: it runs no
  Diesel query against any tenant table (no academic/exam/order
  rows). Its only Diesel read is the pre-existing `domain_claimed`
  check against the `domains` table — a control-plane table owned
  by the domains module. All other reads are the in-memory
  DashMap stores, keyed and filtered by `company_id`.
- **Tenant scoping on every path:** every handler requires
  `X-User-Id` + `X-Role`, and non-admin callers are force-scoped
  to their own company (`authorize_company` /
  `authorize_list`); a caller without a matching `X-Company-Id`
  gets 403. List reads never return another tenant's records.
- **Entitlement-scoped visibility:** the purchased package
  determines what a product subdomain may serve
  (`require_product_scope`, §5): a host scoped to `academic`
  serves only the academic surface; anything else is 404.
- **Fail closed, never fall back to `public`:** no tenant
  context (no `X-Company-Id` for a non-admin, no entitlement,
  unresolved/blocked host) → 403/404, never a silent fallback.
  No tenant ids, schema names, or package ids are hard-coded
  anywhere in this module (product/package ids are opaque
  validated strings; quotas, grace days, and suggestion
  suffixes are admin-editable settings).
- **Convergence point with batch 17's tenancy plumbing —
  NOT on disk yet (verified 2026-11-22 ~12:14):** there is no
  tenancy module, no `schema_name` helper, no `search_path`
  plumbing anywhere in the tree. The only tenant context that
  exists is `TenantContext` in `src/core/middleware.rs`
  (`tenant_id`, `company_id`, `site_id`, `kind`, `host` —
  no schema, no package entitlements). Nothing was invented
  here to fill the gap. When batch 17 lands:
  1. `require_product_scope` must consume the attached tenant
     context (tenant id uuidv7 + tenant schema + package
     entitlements) from request extensions instead of
     re-resolving the `Host` header itself, and must fail closed
     when no context is present on a product-subdomain host.
  2. `TenantContext` needs the product-scope fields
     (`product_id`, `package_id`, `product_subdomain_id`) —
     `resolve_host` already emits them in the hook result (§5);
     the middleware must carry them through.
  3. The product modules behind the scope (academic, exams)
     must resolve their business data through the tenant schema
     from that context; this module never touches their data,
     so no change is needed here beyond (1)–(2).

## Verification (done by read-back; no compiler available)

- `cargo check` has **never** run in this VM (no Rust toolchain,
  crates.io blocked) — compilation is **NOT** verified. This is
  stated here, not hidden.
- Lines: `saas_domains.rs` 2,142 → 3,898 (+1,756). Routes:
  RouteInfo 24 → 33; `configure_routes` 24 → 33 `.route(` lines.
  Tables: 2 created / 2 dropped. Migration `000093` is unique
  (one directory with that number).
- Forbidden reference-project name: case-insensitive grep for the
  forbidden name over the changed files = **0**. (Repo-wide, the
  only matching substring is a pre-existing false positive inside
  an HR org-chart identifier, untouched.)
- Indonesian words: word-boundary grep over a 70-word list in the
  changed files = **0**.
- `todo!` / `unimplemented!` in the changed files = **0**.
- Every new route has a capability check (verified per handler;
  activate/suspend/release via `transition_product_subdomain`).
- Existing routes: all 24 original RouteInfo entries and
  `configure_routes` entries still present, byte-identical paths.
- Brace/paren/bracket balance in `saas_domains.rs`: 713/713,
  2241/2241, 95/95.
- Deliberately **not** verified: live PostgreSQL execution of
  migration 000093, live HTTP behaviour, live hook delivery,
  `composer`/`vite` equivalents — none exist for this stack;
  needs a run with a toolchain.

## OPEN list

1. **No compiler run.** `cargo check` never ran here; the code is
   written to the file's existing patterns but compilation is
   unverified (see above).
2. **Middleware product-scope extension** (§5 steps 1–3):
   `TenantContext` drops the scope fields; the `Released`
   blocked reason has no middleware variant (safe blocked
   fallback today, no export notice); product-module handlers
   must call `require_product_scope` until the middleware
   enforces scope itself.
3. **`site_id` gap** (§5 step 4): product subdomains without a
   provisioned `site_id` resolve as `Unresolved` under the
   current middleware (pre-existing behaviour shared with
   generic allocations).
4. **Entitlement sources mostly unanswered** (§6): only the
   licensing path is live today; store entitlement and packages
   entitlement hooks have no answerers, so claims 403 until one
   lands. Fail-closed by design.
5. **Hook handlers cannot emit events** (§7): event-driven
   releases and hook-side grace sweeps change state silently;
   the HTTP endpoints are the emitting paths. Needs hook
   handlers to receive a bus/`AppContext` (integrator-level
   change, same as the existing alias-verification note).
6. **Pre-existing alias-verification bug (not mine, not fixed):**
   `apply_verification_completed` only reads a `verified`
   boolean from `domain.verification.completed`, but the
   verification module sends `status: "verified"` and no
   boolean — so the alias flip likely never fires. The product
   handler accepts both shapes; the alias handler was left
   untouched per the "keep existing behaviour intact" rule.
7. **Whitelabel attach is ambiguous for multi-product tenants:**
   `whitelabel.domain.verified` carries no product identifier,
   so the domain is attached only when the tenant holds exactly
   one live product subdomain. The whitelabel module must carry
   `product_subdomain_id` in its payload for the general case.
8. **`saas_packages.package.modules_requested` has no answerer:**
   package-scoped resolution currently only matches the product
   id directly (§5, §12).
9. **`saas_packages.rs` is a partial draft** (479 lines, no
   `Module` impl): convergence items in §12 must be re-confirmed
   when it lands.
10. **`saas_domains` (and `domain_verification`, `whitelabel`)
    are not declared in `src/modules/mod.rs`** (only 72 of 134
    module files are declared) — the module's own header already
    notes this; cross-module references here use hook events
    and string event constants only, except for the already-used
    `company` helpers and the `licensing` check (both modules
    are declared). Do not add a `crate::modules::
    domain_verification::` path: it does not resolve.
11. **Diesel persistence layer not wired:** schema/models
    snippets are in §2 for the integrator; the DashMap store is
    still authoritative and the migration has never run against
    live PostgreSQL.
12. **Batch 17 tenancy plumbing not on disk** (§13): convergence
    points recorded; nothing invented.
13. **Migration duplicates elsewhere (not mine, recorded for the
    integrator):** 000056, 000070, 000075, 000077, 000080,
    000090 (`000090_exams` vs `000090_school_fees`) each exist
    twice; 000100 is out of sequence. This round adds no new
    duplicate (000093 is unique).

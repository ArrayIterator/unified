# Integration Notes — Batch 8: SaaS Domain Management

Coordinator-verified by read-back. Nothing in this file is applied yet:
the integrator owns every step below. Batch 8 built 4 builders (B1–B4),
all complete. **Nothing was pushed.** Remote head is still `7cf6853c`.

## 1. What batch 8 delivered

| Builder | Files | Lines | Routes | Migration | Tables |
|---|---|---|---|---|---|
| B1 saas_domains | `src/modules/saas_domains.rs` (new) | 2,142 | 24 | `2026-11-22-000070_saas_domains` | 3 |
| B2 domain_verification | `src/modules/domain_verification.rs` (new) | 1,410 | 8 | `2026-11-22-000072_domain_verification` | 3 |
| B3 SSL (cert modes + Cloudflare + swap) | `src/modules/certificates.rs` (+1,434), `src/http/cert_swap.rs` (new, 271), `src/http/tls.rs` (+18 docs only), `src/http/mod.rs` (+1), `Cargo.toml` (+1: `arc-swap = "1"`) | +1,724 | +11 (16 → 27) | `2026-11-22-000073_domain_cert_modes` | 3 |
| B4 middleware | `src/core/middleware.rs` (+801), `src/http/server.rs` (+18) | +819 | 0 (middleware, already wired) | — | 0 |

**Batch totals: ~5,095 new/changed Rust lines, 43 new routes, 9 new tables,
3 migrations.** Tree at batch end: `src/modules/*.rs` 296,176 lines,
118 module files, 76 migration dirs (other rounds writing concurrently —
batch 7/9/10/11 counts are theirs, not mine).

Module ids: `saas_domains` (infra, `available_as_product = false`),
`domain_verification` (infra, `available_as_product = false`).
`certificates` is pre-existing (already registered). The middleware is
already wired into `server.rs` (priority 950, after security headers 1000,
before IP filter 900) — do NOT re-wire it.

## 2. Register the two new modules (`src/modules/mod.rs`)

```rust
pub mod saas_domains;
pub mod domain_verification;
```

And in the registration list (same place the other `register(...)` calls
live — check the file; the pattern is one `pub mod X;` + one
`X::register(registry);` per module):

```rust
saas_domains::register(registry);
domain_verification::register(registry);
```

`certificates` is already registered. `cert_swap` is declared in
`src/http/mod.rs` (`pub mod cert_swap;`) — no `mod.rs` change needed for it.

## 3. Diesel schema (`src/db/schema.rs`)

Add these `table!` blocks (column order/types taken from the up.sql files).
All ids are UUID, all timestamps are `Timestamptz`.

```rust
table! {
    saas_domain_reservations (id) {
        id -> Uuid,
        domain -> Varchar,
        company_id -> Uuid,
        reserved_by -> Text,
        reserved_until -> Timestamptz,
        status -> Varchar,
        site_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

table! {
    saas_domain_aliases (id) {
        id -> Uuid,
        domain -> Varchar,
        company_id -> Uuid,
        target_type -> Varchar,
        target_value -> Varchar,
        status -> Varchar,
        site_id -> Nullable<Uuid>,
        verified_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

table! {
    saas_subdomain_allocations (id) {
        id -> Uuid,
        label -> Varchar,
        hostname -> Varchar,
        company_id -> Uuid,
        site_id -> Nullable<Uuid>,
        status -> Varchar,
        approved_by -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

table! {
    domain_verification_settings (id) {
        id -> Uuid,
        version -> BigInt,
        record_type -> Text,
        host_template -> Text,
        value_template -> Text,
        token_length -> Integer,
        token_alphabet -> Text,
        verify_timeout_seconds -> Integer,
        max_attempts -> Integer,
        retry_delay_seconds -> Integer,
        token_ttl_seconds -> BigInt,
        rate_limit_window_seconds -> BigInt,
        updated_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

table! {
    domain_verification_tokens (id) {
        id -> Uuid,
        domain -> Text,
        alias_id -> Nullable<Uuid>,
        tenant_id -> Nullable<Uuid>,
        company_id -> Nullable<Uuid>,
        token_hash -> Text,
        record_type -> Text,
        host -> Text,
        expected_value -> Text,
        settings_version -> BigInt,
        status -> Text,
        issued_at -> Timestamptz,
        expires_at -> Timestamptz,
        verified_at -> Nullable<Timestamptz>,
    }
}

table! {
    domain_verification_attempts (id) {
        id -> Uuid,
        token_id -> Uuid,
        domain -> Text,
        alias_id -> Nullable<Uuid>,
        attempt_number -> Integer,
        result -> Text,
        record_found -> Nullable<Text>,
        looked_up -> Jsonb,
        error -> Nullable<Text>,
        checked_at -> Timestamptz,
    }
}

table! {
    domain_cert_modes (id) {
        id -> Uuid,
        domain -> Varchar,
        mode -> Varchar,
        provider -> Varchar,
        fingerprint_sha256 -> Varchar,
        installed_at -> Nullable<Timestamptz>,
        previous_fingerprint -> Nullable<Varchar>,
        status -> Varchar,
        last_error -> Nullable<Text>,
        updated_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

table! {
    cloudflare_detections (id) {
        id -> Uuid,
        domain -> Varchar,
        detected -> Bool,
        methods -> Jsonb,
        ssl_mode -> Nullable<Varchar>,
        end_to_end_encrypted -> Nullable<Bool>,
        flexible_warning -> Nullable<Text>,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

table! {
    cloudflare_ip_ranges (key) {
        key -> Varchar,
        v4 -> Jsonb,
        v6 -> Jsonb,
        fetched_at -> Nullable<Timestamptz>,
        source_v4 -> Varchar,
        source_v6 -> Varchar,
        fetch_error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

Add all nine table names to `allow_tables_to_appear_in_same_query!`
(only if the file uses it — check the existing list first).

## 4. Models (`src/db/models.rs`)

Add Queryable + Insertable structs mirroring the tables above
(`saas_domains.rs`, `domain_verification.rs`, `certificates.rs` hold the
in-memory shapes; follow the existing `Domain`/`NewDomain` pattern in the
file, including `#[diesel(table_name = ...)]`). Suggested struct names:
`SaasDomainReservation` / `NewSaasDomainReservation`,
`SaasDomainAlias` / `NewSaasDomainAlias`,
`SaasSubdomainAllocation` / `NewSaasSubdomainAllocation`,
`DomainVerificationSetting` / `NewDomainVerificationSetting`,
`DomainVerificationToken` / `NewDomainVerificationToken`,
`DomainVerificationAttempt` / `NewDomainVerificationAttempt`,
`DomainCertMode` / `NewDomainCertMode`,
`CloudflareDetection` / `NewCloudflareDetection`,
`CloudflareIpRanges` / `NewCloudflareIpRanges`.

**Persistence writers are still missing** (same as every other round):
all three modules keep DashMap stores authoritative and the migration
tables are persistence targets only. The integrator must add the
insert/update/select wiring (or record it as a follow-up) — do not
claim the tables are live until a writer exists.

## 5. Migration order and collisions (READ THIS FIRST)

Batch 8 numbers: `000070_saas_domains`, `000072_domain_verification`,
`000073_domain_cert_modes`. Full collision table on disk at batch end:

| Number | Directories on disk | Action for integrator |
|---|---|---|
| 000056 | `000056_community` (batch 4), `000056_storage_product_byo` (storage follow-up) | Renumber ONE (pre-existing, not batch 8) |
| 000068 | `000068_licensing_saas`, `000068_network_tools` (both batch 7) | Renumber ONE (pre-existing, not batch 8) |
| 000070 | `000070_saas_domains` (batch 8 B1), `000070_presets` (batch 9) | Renumber ONE — B1 took it first (batch 7 had already taken 000069, which is why B1 used 000070); batch 9 landed `000070_presets` later the same day |
| 000071 | `000071_whitelabel` (batch 9) | B3 avoided it (renumbered to 000073); nothing to do |
| 000100 | `000100_calculator` (batch 3) | Renumber into the gapless sequence (pre-existing) |

B2 was renumbered mid-round 000070 → 000072; B3 was renumbered
mid-round 000071 → 000073. Verified: 0 references to the old numbers
remain in B2/B3 files. Run the migrations in numeric order after
renumbering; none of the batch-8 migrations has ever run against a live
PostgreSQL.

## 6. Hook wiring the integrator must still do

Already answered (verified by read-back):
- `tenant.resolve_requested` — answered by `saas_domains`
  (`saas_domains.rs:1991`, `resolve_host_with`). Middleware (B4) emits it.
- `saas_domains.alias.lookup` — answered by `saas_domains`.
- `domain.verification.completed` — emitted by `domain_verification`,
  listened to by `saas_domains` (flips a pending alias to verified).
- `domain.verification.failed`, `domain.verification.token_issued`,
  `domain.verification.attempted`, `domain.verification.settings_updated`
  — emitted by `domain_verification`.
- `domain.verification.status_requested` — answered by
  `domain_verification` (no result when unknown).
- `ssl.swap` — completion payloads from `cert_swap` carry
  `{domain, fingerprint_sha256, previous_fingerprint_sha256,
  ssl_expires_at, swapped_at, swapped:true}`; `domains.rs` already
  listens and applies when `ssl_expires_at` is present — no change
  needed there. `ssl.cert_mode.changed`, `ssl.cloudflare.detected`,
  `ssl.swapped`, `ssl.store.swapped` — emitted by certificates/cert_swap.
- `saas_domains.reservations.expire` — answered by `saas_domains`
  (scheduler arm, see §7).
- `ssl.cloudflare.recheck` — answered by certificates (scheduler arm,
  see §7).

Still MISSING (fail-closed until wired — record, do not silently skip):
1. `domain.ownership.check` `{domain, user_id, owned:false}` — B3's
   tenant-scoped cert-mode/detection calls need a handler that sets
   `owned:true` for a tenant's verified domain. Natural home:
   `saas_domains` (it owns aliases + allocations) — add a handler
   there. Until wired, tenant calls 403; admin path works.
2. `cloudflare.zone.lookup` `{domain, found:false, zone_id:null,
   ssl_mode:null}` — B3's API detection method needs a handler in the
   `cloudflare` module that sets `found`/`zone_id`/`ssl_mode`
   (`flexible`/`full`/`full_strict`). Until wired, the API method
   honestly reports unknown.
3. `domain.lookup` / `saas.site.lookup` — B1's cross-module conflict
   check queries them; neither `domains.rs` nor `saas.rs` answers
   them today, so in-memory-only domains/sites rows are invisible to
   the check (the Diesel read of the `domains` table works). Add
   lookup answerers in `domains`/`saas`, or confirm the DB read
   suffices once domains persist.
4. `site.provisioned` — B1's reservation→provisioning handoff emits
   it but does NOT create the site row. Confirm a listener creates the
   site (or document that the caller then calls
   `POST /api/v1/saas/sites`).

## 7. Scheduler arms to add

- `saas_domains.reservations.expire` — releases past-due reservations
  (B1 answers; add the kind to the scheduler's known kinds + a
  schedule, e.g. hourly).
- `ssl.cloudflare.recheck` — re-runs Cloudflare detection for one
  domain (`{domain}`) or all mode-holding domains (B3 answers; add
  the kind + a schedule, e.g. daily).
- No scheduled retry exists for domain verification: verify is
  on-demand via `POST /api/v1/domains/verification/verify`. If
  "system retries on a schedule" is wanted, add a scheduler job that
  calls it (B2 OPEN #4).

## 8. CertStoreSwap server wiring (B3 — NOT yet wired)

B4 owns `server.rs` and did not wire this (B3's brief told it to
document, not to rewrite `server.rs`). In the server startup, next to
where the `SslStorage` is built (see `src/http/cert_swap.rs` header
for the exact snippet):

```rust
let swap = Arc::new(CertStoreSwap::new(storage.clone()));
let tls_config = cert_swap::server_config_from_swap(&swap)?;
cert_swap::register_cert_swap_hooks(&ctx.hooks, swap.clone());
```

Honest semantics (verified by reading `tls.rs`): the per-certificate
path is what changes live traffic — `SslStorage::resolve` reads the
DashMaps on every handshake, so `add`/`renew`/`swap_certificate` take
effect for NEW handshakes immediately, no restart (in-flight
connections keep their old cert). `ArcSwap` whole-store swap only
affects `ServerConfig`s built AFTER the swap; an already-built/running
listener keeps resolving through the old `Arc`.

## 9. Typed `[saas]` config section (B4 OPEN #2)

No typed `[saas]` section exists in `AppConfig` (`src/core/config.rs`).
`server.rs` currently builds the middleware config via
`TenantResolutionConfig::from_environment()` (env vars below). When the
integrator adds the typed section, plug it in at the single marked
construction site in `server.rs`.

## 10. Capabilities to seed (roles)

- `saas_domains.manage`, `saas_domains.read`, `saas_domains.self_assign`
  (B1; every endpoint requires `X-User-Id` + `X-Role`, `role.check`
  hook, fail-closed; non-admin callers are force-scoped to their own
  company on lists/reads).
- `domain_verification.manage`, `domain_verification.issue`,
  `domain_verification.verify`, `domain_verification.read` (B2).
- `certificates.view`, `certificates.manage`, `certificates.manage_own`
  (B3 — NEW routes only; the 16 pre-existing certificate routes have
  NO capability checks, predating the pattern. Adding checks to the
  old routes is a behaviour change for the integrator to decide —
  do not silently add them.)
- Tenant-scoped cert calls rely on `domain.ownership.check` (§6.1).

## 11. Audit events (`AUDITED_EVENTS` in `src/modules/audit.rs`)

Add: `saas_domains.reservation.created/released/expired/provisioned`,
`saas_domains.alias.created/updated/suspended/released`,
`saas_domains.subdomain.created/approved/rejected/suspended/released`,
`saas_domains.settings.updated`,
`domain.verification.completed/failed/token_issued/attempted/settings_updated`,
`ssl.cert_mode.changed`, `ssl.cloudflare.detected`,
`tenant.control_plane_blocked`.
(Check the existing list first — do not duplicate names that are
already there.)

## 12. Config keys (all overridable; defaults documented in code)

- `saas_domains.wildcard_base` (default empty = subdomain resolution
  disabled), `saas_domains.main_host` (default empty = first `app.tcp`
  listen host, else `localhost`), `saas_domains.reserved_labels`
  (default www, admin, api, mail, app, dashboard, static, cdn,
  support, status), `saas_domains.label_pattern` (DNS-label regex),
  `saas_domains.max_subdomains_per_tenant` (default 5),
  `saas_domains.require_subdomain_approval` (default false),
  `saas_domains.default_reservation_ttl_hours` (default 48),
  `saas_domains.alias_target_types` (default cname_subdomain,
  a_record, cname_server) — admin-editable via
  `GET/PUT /api/v1/saas/domains/settings`.
- `saas.main_host` (env `UNIFIED_SAAS_MAIN_HOST`),
  `saas.blocked_control_plane_paths` (env
  `UNIFIED_SAAS_BLOCKED_PATHS`, comma-sep.; default `/admin`,
  `/dashboard`, `/api/v1/admin`, `/dashboard/settings/domains`,
  `/dashboard/domains`, `/dashboard/verification`),
  `saas.exempt_paths` (env `UNIFIED_SAAS_EXEMPT_PATHS`; default
  `/health`, `/.well-known/acme-challenge/`),
  `saas.tenant_header` (env `UNIFIED_SAAS_TENANT_HEADER`; default
  `X-Unified-Tenant`), `saas.unknown_host_policy` (env
  `UNIFIED_SAAS_UNKNOWN_HOST_POLICY`; default `control_plane`),
  `saas.missing_host_policy` (env `UNIFIED_SAAS_MISSING_HOST_POLICY`;
  default `control_plane`).
- Certificates settings via `PUT /api/v1/certificates/settings`:
  `custom_min_validity_days` (default 7, range 0–365),
  `cf_detect_dns` / `cf_detect_probe` / `cf_detect_api` (default true
  each), `cf_ranges_v4_url` (default
  `https://www.cloudflare.com/ips-v4`), `cf_ranges_v6_url` (default
  `https://www.cloudflare.com/ips-v6`, both validated HTTPS).
  Cert-mode registry via `PUT /api/v1/certificates/cert-modes/definitions`.
- Domain-verification settings via
  `PUT /api/v1/domains/verification/settings`: `record_type`,
  `host_template`, `value_template`, `token_length`, `token_alphabet`,
  `verify_timeout_seconds`, `max_attempts`, `retry_delay_seconds`,
  `token_ttl_seconds`, `rate_limit_window_seconds` (defaults: TXT,
  `_unified-verify`, `unified-verify={token}`, 32, alnum, 10, 5, 10,
  604800, 3600).

## 13. README / TODO lines

- README: module count +2 (saas_domains, domain_verification;
  recount the whole tree at push time — other rounds are landing
  files concurrently), migrations +3, tables +9, routes +43.
  Cert-mode / Cloudflare detection / tenant-resolution middleware
  get one line each under their modules.
- TODO: add a "Batch 8 — SaaS Domain Management" section with the
  OPEN list (§15) and the hook-wiring checklist (§6).

## 14. Modularity-rule check (standing user rule: NOTHING hard-coded)

Checked by builders + coordinator read-back. Config-driven:
B1 settings struct + config keys (§12); B2 all verification behaviour
reads `current_settings()`; B3 cert-mode registry is seed data only
(behaviour driven by mode flags), Cloudflare ranges are fetched data
(no range list baked into code — until the first successful fetch the
DNS method honestly reports `ranges_unavailable`), detection methods
individually toggleable; B4 all tunables in `TenantResolutionConfig`
with env-var mapping.

Honest remaining hard-coded spots (contract/protocol constants, not
tunables — recorded, not hidden):
- B1: blocked reason codes `unverified_domain`/`suspended`
  (wire contract B4 matches exactly); status vocabularies
  (state machine + migration CHECKs); capability names; resolution
  ORDER (control plane → subdomain → alias → blocked) — only the
  rules inside each step are settings-driven.
- B2: status vocabulary `verified`/`pending`/`failed`/`unknown`
  (event contract); `value_prefix()` 12-char display truncation
  (cosmetic, `domain_verification.rs` ~line 500).
- B3: detection/range-fetch timeouts are a 10 s constant
  (`Duration::from_secs(10)` in `certificates.rs`); mode-history cap
  is a constant 20 entries (`record_cert_mode`); the internal
  `via:"cert_mode"` marker and Cloudflare header names
  (`cf-ray`, `cf-cache-status`, `server: cloudflare`) are protocol
  constants.
- B4: event names (`tenant.resolve_requested`,
  `tenant.control_plane_blocked`) and blocked-reason values are fixed
  by the hook contract.
- B2 gap: CNAME value templates are NOT shape-validated — an admin
  switching `record_type` to `cname` must set a CNAME-appropriate
  `value_template` (e.g. `"{token}.verify.example.net"`); validation
  only enforces the `{token}` placeholder. A TXT-style default
  template would not be a valid CNAME target.

## 15. OPEN list (consolidated — anything unfinished or unverified)

1. **`cargo check` / `cargo build` has NEVER run anywhere** — no
   toolchain in this VM, crates.io blocked. Compilation unverified
   for all four builders. Highest-risk uncompiled points (B3):
   the `async |url|` closure in `refresh_cf_ranges` and the
   or-pattern error binding `(Err(error), _) | (_, Err(error))`
   there.
2. Migrations never executed against live PostgreSQL; no endpoint
   live-tested; no live DNS/Cloudflare/range-fetch test was possible
   in this sandbox.
3. Integrator work (§2–§11): register 2 modules, schema/models for
   9 tables, persistence writers, migration renumbering (§5),
   4 missing hook handlers (§6), 2 scheduler arms (§7), CertStoreSwap
   server wiring (§8), typed `[saas]` config (§9), capability seeds
   (§10), audit events (§11), README/TODO (§13).
4. B1: settings are runtime-only (in-memory `RwLock`) — not persisted
   across restart; persist via the settings/config store if
   durability is required. Hook-path main-host fallback is weaker
   than the HTTP path (setting → `localhost` only; the full chain
   setting → first `app.tcp` listen host runs only in `resolve_host`
   / `/resolve`) — set `saas_domains.main_host` explicitly if the
   main host is neither configured nor `localhost`. No
   `saas_domains.alias.verified` event is emitted (HookContext
   carries no bus) — consumers should observe
   `domain.verification.completed` itself.
5. B4: unknown-host vs failed-tenant-host 404 tension — the
   middleware cannot distinguish the two (a hook non-answer is
   identical in both cases). Default policy sends both to control
   plane; `saas.unknown_host_policy=not_found` 404s ALL unknown
   hosts including main-host typos. Needs a contract refinement
   (e.g. B1 answering an explicit "known tenant host, resolution
   failed" result) to scope the 404 to tenant-only hosts. Also:
   invalid tenant-header config silently falls back to
   `X-Unified-Tenant` with no log line (add a startup log when the
   typed config lands).
6. B2: no scheduled retries; CNAME template shape not validated
   (§14); data lost on restart until a persistence writer lands.
7. B3: `domain.ownership.check` + `cloudflare.zone.lookup` have no
   handlers (§6); CertStoreSwap not wired (§8); Cloudflare ranges
   start empty until first fetch (DNS method reports
   `ranges_unavailable`); 16 pre-existing cert routes have no
   capability checks (§10).
8. B1 provision handoff emits `site.provisioned` but creates no site
   row (§6.4).
9. B4's report claimed B1's handler was "not present yet" — stale:
   `saas_domains.rs` (2,142 lines, 82,839 bytes) was verified on disk
   by the coordinator. End-to-end resolution still needs a live
   server to prove.
10. Note for the integrator: the user said "axum" in the original
    message — the project actually uses Actix-web 4 (decided
    earlier). ArcSwap applies the same; no Axum code exists or is
    needed.

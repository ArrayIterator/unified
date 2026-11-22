# Integration notes — Batch 7 (Network Tools + Licensing as a SaaS)

Coordinator-verified. Nothing in this batch was pushed. Remote head is still `7cf6853c`.

## Summary

| Item | Files | Lines | Migration | Routes | Tables |
|---|---|---|---|---|---|
| B1 Network Tools | `src/modules/network_tools.rs` (new) | 4,629 | `2026-11-22-000069_network_tools` | 33 (new module) | 5 |
| B2 Licensing SaaS | `src/modules/licensing.rs` (1,654 → 5,111, +3,457) | +3,457 | `2026-11-22-000068_licensing_saas` | 26 new (38 total in module) | 6 |

**Batch totals: 8,086 added lines, 59 new routes, 11 new tables, 2 migrations.**

Migration-number history (recorded because three rounds raced): 000066 was taken by
batch 6 (`000066_web3`), 000067 by batch 5 (`000067_sharing_auditor`). B2 took 000068
(after finding 000067 taken). B1 first created 000068, found B2's `000068_licensing_saas`
on disk, and renumbered to 000069 (verified free at write time). Current tail of the
migration sequence: `... 000065_privacy, 000066_web3, 000067_sharing_auditor,
000068_licensing_saas, 000069_network_tools, 000100_calculator`. The `000100_calculator`
entry is out of sequence (batch 3) — renumber at integration time. The storage round
also flagged a possible 000056 community/storage-product collision; batch 7 did not
touch it.

## 1. `src/modules/mod.rs` — registration lines

Add the module declaration (alphabetical position, after `network` is wrong —
`network_tools` sorts after `network` and before `newsletter`):

```rust
pub mod network_tools;
```

In `register_all`, in the Operations group after `network::register(registry);`:

```rust
network_tools::register(registry);
```

`licensing` is already registered; no mod.rs change is needed for B2.

## 2. `src/db/schema.rs` — `table!` blocks

Append all eleven blocks, plus the `joinable!` lines and `allow_tables_to_appear_in_same_query!`
entries for the new tables (following the existing file's style).

```rust
diesel::table! {
    network_tool_saved_checks (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        name -> Text,
        tool -> Text,
        target -> Text,
        parameters -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    network_tool_runs (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        tool -> Text,
        target -> Text,
        duration_ms -> BigInt,
        outcome -> Text,
        summary -> Jsonb,
        created_at -> Timestamptz,
    }
}
diesel::table! {
    network_tool_uptime_monitors (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        name -> Text,
        url -> Text,
        interval_seconds -> Int4,
        expected_status -> Int4,
        failure_threshold -> Int4,
        consecutive_failures -> Int4,
        enabled -> Bool,
        last_status -> Nullable<Text>,
        last_checked_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    network_tool_uptime_events (id) {
        id -> Uuid,
        monitor_id -> Uuid,
        status -> Text,
        http_status -> Nullable<Int4>,
        latency_ms -> Nullable<BigInt>,
        detail -> Jsonb,
        created_at -> Timestamptz,
    }
}
diesel::table! {
    network_tool_cert_watches (id) {
        id -> Uuid,
        user_id -> Text,
        company_id -> Nullable<Uuid>,
        domain -> Text,
        enabled -> Bool,
        last_days_remaining -> Nullable<Int4>,
        last_checked_at -> Nullable<Timestamptz>,
        alerted_thresholds -> Array<Int4>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    license_products (id) {
        id -> Uuid,
        product -> Varchar,
        display_name -> Nullable<Varchar>,
        owner_id -> Nullable<Uuid>,
        key_prefix -> Nullable<Varchar>,
        segments -> Int4,
        key_format -> Varchar,
        key_pattern -> Nullable<Varchar>,
        default_max_activations -> Int4,
        default_validity_days -> Nullable<Int8>,
        grace_period_days -> Int8,
        renewal_reminder_days -> Array<Int8>,
        offline_max_days -> Int8,
        allowed_distribution_channels -> Array<Text>,
        api_allowed_endpoints -> Array<Text>,
        api_rate_limit_per_minute -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    license_distributions (id) {
        id -> Uuid,
        product -> Varchar,
        provider -> Varchar,
        repo -> Varchar,
        git_ref -> Varchar,
        webhook_secret_hash -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    license_releases (id) {
        id -> Uuid,
        product_id -> Uuid,
        product -> Varchar,
        version -> Varchar,
        file_name -> Varchar,
        storage_target -> Nullable<Varchar>,
        storage_key -> Varchar,
        sha256_hex -> Varchar,
        size_bytes -> Int8,
        download_count -> Int8,
        created_at -> Timestamptz,
    }
}
diesel::table! {
    license_events (id) {
        id -> Uuid,
        license_id -> Uuid,
        event -> Varchar,
        actor -> Nullable<Varchar>,
        details -> Jsonb,
        prev_hash -> Varchar,
        hash -> Varchar,
        created_at -> Timestamptz,
    }
}
diesel::table! {
    license_offline_tokens (token_id) {
        token_id -> Uuid,
        license_id -> Uuid,
        product -> Varchar,
        expires_at -> Timestamptz,
        issued_at -> Timestamptz,
        revoked -> Bool,
        created_at -> Timestamptz,
    }
}
diesel::table! {
    license_activation_history (id) {
        id -> Uuid,
        license_id -> Uuid,
        action -> Varchar,
        actor -> Nullable<Varchar>,
        instance_id -> Varchar,
        hostname -> Nullable<Varchar>,
        created_at -> Timestamptz,
    }
}
diesel::joinable!(network_tool_uptime_events -> network_tool_uptime_monitors (monitor_id));
diesel::joinable!(license_products -> users (owner_id));
diesel::joinable!(license_releases -> license_products (product_id));
diesel::joinable!(license_events -> licenses (license_id));
diesel::joinable!(license_offline_tokens -> licenses (license_id));
diesel::joinable!(license_activation_history -> licenses (license_id));
```

## 3. `src/db/models.rs` — structs

The row structs currently live inside the module files. Move them here verbatim and
update the modules' imports to `crate::db::models::*`.

**Convention warning:** the network-tools structs below derive `Queryable + Selectable +
Insertable` on a single struct, while the rest of `models.rs` separates `Queryable`
structs from `New*` `Insertable` structs (see `License` / `NewLicense`). The integrator
should split them into the repo's usual two-struct form if the combined derive does
not fit; the field lists are authoritative either way. (`Value` = `serde_json::Value`,
already imported in `models.rs`; same Jsonb pattern as `Role.permissions`.)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable)]
#[diesel(table_name = crate::db::schema::network_tool_saved_checks)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NetworkToolSavedCheck {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub name: String,
    pub tool: String,
    pub target: String,
    pub parameters: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable)]
#[diesel(table_name = crate::db::schema::network_tool_runs)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NetworkToolRun {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub tool: String,
    pub target: String,
    pub duration_ms: i64,
    pub outcome: String,
    pub summary: Value,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable)]
#[diesel(table_name = crate::db::schema::network_tool_uptime_monitors)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NetworkToolUptimeMonitor {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub name: String,
    pub url: String,
    pub interval_seconds: i32,
    pub expected_status: i32,
    pub failure_threshold: i32,
    pub consecutive_failures: i32,
    pub enabled: bool,
    pub last_status: Option<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable)]
#[diesel(table_name = crate::db::schema::network_tool_uptime_events)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NetworkToolUptimeEvent {
    pub id: Uuid,
    pub monitor_id: Uuid,
    pub status: String,
    pub http_status: Option<i32>,
    pub latency_ms: Option<i64>,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable)]
#[diesel(table_name = crate::db::schema::network_tool_cert_watches)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NetworkToolCertWatch {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub domain: String,
    pub enabled: bool,
    pub last_days_remaining: Option<i32>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub alerted_thresholds: Vec<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_products)]
pub struct LicenseProduct {
    pub id: Uuid,
    pub product: String,
    pub display_name: Option<String>,
    pub owner_id: Option<Uuid>,
    pub key_prefix: Option<String>,
    pub segments: i32,
    pub key_format: String,
    pub key_pattern: Option<String>,
    pub default_max_activations: i32,
    pub default_validity_days: Option<i64>,
    pub grace_period_days: i64,
    pub renewal_reminder_days: Vec<i64>,
    pub offline_max_days: i64,
    pub allowed_distribution_channels: Vec<String>,
    pub api_allowed_endpoints: Vec<String>,
    pub api_rate_limit_per_minute: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_products)]
pub struct NewLicenseProduct {
    pub id: Uuid,
    pub product: String,
    pub display_name: Option<String>,
    pub owner_id: Option<Uuid>,
    pub key_prefix: Option<String>,
    pub segments: i32,
    pub key_format: String,
    pub key_pattern: Option<String>,
    pub default_max_activations: i32,
    pub default_validity_days: Option<i64>,
    pub grace_period_days: i64,
    pub renewal_reminder_days: Vec<i64>,
    pub offline_max_days: i64,
    pub allowed_distribution_channels: Vec<String>,
    pub api_allowed_endpoints: Vec<String>,
    pub api_rate_limit_per_minute: i64,
}
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_distributions)]
pub struct LicenseDistribution {
    pub id: Uuid,
    pub product: String,
    pub provider: String,
    pub repo: String,
    pub git_ref: String,
    pub webhook_secret_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_distributions)]
pub struct NewLicenseDistribution {
    pub id: Uuid,
    pub product: String,
    pub provider: String,
    pub repo: String,
    pub git_ref: String,
    pub webhook_secret_hash: Option<String>,
}
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_releases)]
pub struct LicenseRelease {
    pub id: Uuid,
    pub product_id: Uuid,
    pub product: String,
    pub version: String,
    pub file_name: String,
    pub storage_target: Option<String>,
    pub storage_key: String,
    pub sha256_hex: String,
    pub size_bytes: i64,
    pub download_count: i64,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_releases)]
pub struct NewLicenseRelease {
    pub id: Uuid,
    pub product_id: Uuid,
    pub product: String,
    pub version: String,
    pub file_name: String,
    pub storage_target: Option<String>,
    pub storage_key: String,
    pub sha256_hex: String,
    pub size_bytes: i64,
    pub download_count: i64,
}
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_events)]
pub struct LicenseEvent {
    pub id: Uuid,
    pub license_id: Uuid,
    pub event: String,
    pub actor: Option<String>,
    pub details: Value,
    pub prev_hash: String,
    pub hash: String,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_events)]
pub struct NewLicenseEvent {
    pub id: Uuid,
    pub license_id: Uuid,
    pub event: String,
    pub actor: Option<String>,
    pub details: Value,
    pub prev_hash: String,
    pub hash: String,
}
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_offline_tokens)]
pub struct LicenseOfflineToken {
    pub token_id: Uuid,
    pub license_id: Uuid,
    pub product: String,
    pub expires_at: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
    pub revoked: bool,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_offline_tokens)]
pub struct NewLicenseOfflineToken {
    pub token_id: Uuid,
    pub license_id: Uuid,
    pub product: String,
    pub expires_at: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
    pub revoked: bool,
}
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_activation_history)]
pub struct LicenseActivationHistory {
    pub id: Uuid,
    pub license_id: Uuid,
    pub action: String,
    pub actor: Option<String>,
    pub instance_id: String,
    pub hostname: Option<String>,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_activation_history)]
pub struct NewLicenseActivationHistory {
    pub id: Uuid,
    pub license_id: Uuid,
    pub action: String,
    pub actor: Option<String>,
    pub instance_id: String,
    pub hostname: Option<String>,
}
```

## 4. Diesel persistence wiring (both items are in-memory authoritative until this lands)

- **Network Tools:** the module's Diesel helpers reference `crate::db::schema::network_tool_*`
  tables that do not exist yet, so the module does **not compile** until §2 lands. After
  §2/§3, wire the helpers to the real tables (same dual DB/in-memory pattern as
  `licensing.rs`: DB authoritative when a pool is attached, DashMap fallback otherwise).
- **Licensing SaaS:** the migration ships the DDL, but no Diesel writers/readers exist
  yet for the six new tables — SaaS records (products, distributions, releases metadata,
  events, tokens, history) do not survive a restart until the integrator adds them.
  Release bytes uploaded with no storage target registered fall back to an in-memory
  blob and are lost on restart (the file endpoint returns 410 in that case).

## 5. Scheduler arms (integrator)

No network-tools job kind exists yet. Arm scheduler jobs that, per enabled monitor at
its interval, either emit `network_tools.uptime.check_requested` with
`{"monitor_id", "actor"}` or call `POST /api/v1/network-tools/uptime-monitors/{id}/check`.
Same for cert watches: emit `network_tools.cert.check_requested` (daily is enough) or
call `POST /api/v1/network-tools/cert-watches/check`. The licensing expiry scan already
has its hook (`license.expiry.check_requested`) and POST trigger; the new renewal
reminders and grace alerts ride on that scan — no new scheduler kind is needed for
licensing, but the scan must actually be scheduled for reminders to fire.

## 6. `AUDITED_EVENTS` (`src/modules/audit.rs`)

Add: `license.verify.failed`, `license.upgraded`, `license.renewal.reminder`,
`license.grace.started`, `license.anchored`, `license.download`,
`network_tools.run`, `network_tools.uptime.down`, `network_tools.cert.expiring`,
`network_tools.result.shared`.

## 7. API Management — public-access rules (`ApiAccessRule` rows)

The licensing public endpoints are authenticated but public-facing; create allow rules
(default-deny otherwise) for:
- `POST /api/v1/licensing/public/verify` (API key, scope `licensing:read`)
- `POST /api/v1/licensing/offline-verify` (signed offline token is the credential)
- `GET /api/v1/licensing/distribute/git/{product}/access` (API key, `licensing:read`)
- `POST /api/v1/licensing/distribute/git/{product}/webhook` (provider HMAC signature)
- `GET /api/v1/licensing/distribute/releases/{release_id}/download` (valid license key + instance)
- `GET /api/v1/licensing/distribute/releases/{release_id}/file` (HMAC signed link, 1 h expiry)

Network Tools has no public endpoints; all 33 routes are admin/operator authenticated.

## 8. Sharing registry (auditors module)

Register shareable kind `network_tools_run` with view `summary` in the auditors
module's `ShareRegistry`. No hook exists for that today, so until then link creation
fails closed with "not a registered shareable kind", by design. The network-tools
module already answers `sharing.allow_check` for kind `network_tools_run` (owner-only,
fail-closed) and emits `network_tools.result.shared`; its `POST /history/{id}/share`
endpoint returns the exact scope/mint body but does not mint tokens itself.

## 9. README.md / TODO.md lines

README: module count +1 (Network Tools; licensing is an extension, not a new module),
migrations +2 (000068, 000069), tables +11. Add a "Network Tools" list entry
(developer tools: DNS, ping, traceroute, port/SSL/WHOIS/HTTP checks, IP/Geo, subnet
calculator, webhook tester, uptime monitors, cert watches, blacklist checks) and extend
the Licensing entry (verification API, activation history/seats, renewal reminders +
grace periods, upgrades, git/API/file distribution, per-product key formats, offline
tokens, hash-chained event log; SDK planned, not built).

TODO: record the OPEN list below (§10) verbatim, plus the SDK plan (PHP,
JavaScript/TypeScript, Python, Rust; wraps `POST /public/verify` and the offline-token
flow) as future work.

## 10. OPEN list (headline — full detail is in the builders' reports)

1. **`cargo check` / `cargo build` has NEVER run anywhere** — no toolchain in this VM,
   crates.io blocked. Compilation is unverified for the whole batch. Compile-risk items:
   `x509-certificate` field access (`tbs_certificate.serial_number`, `subject`/`issuer`
   Display), `TokioResolver::builder(...).build()` + SOA lookup shape (copied from
   `network.rs`, uncompiled), Diesel `on_conflict(...).do_update()` (no other module
   uses it), `serde_json::Value` ↔ Jsonb mapping.
2. Integrator must: register `network_tools` in `mod.rs` (§1), add schema/models (§2/§3),
   wire Diesel persistence (§4), add scheduler arms (§5), add audit events (§6),
   create ApiAccessRule rows (§7), register the shareable kind (§8), update README/TODO (§9).
3. **New licensing tables are in-memory only** until Diesel writers/readers land (§4).
4. **`store.order.upgraded` and `billing.plan.changed` are not emitted anywhere yet**
   (verified by searching store/billing sources) — the `license.upgraded` handler is
   written against the documented payload shape (`license_id`/`license_ids`,
   `extend_days`, `new_max_activations`/`seats`, `new_product`, `expires_at`,
   `order_id`) and applies as soon as the emitters land.
5. **Pre-existing gap, unchanged:** the original 12 licensing routes still have no
   capability checks (adding them would change existing behaviour). All 26 new admin
   routes do have checks.
6. `role.check` must be answered by the roles module for the new admin capability
   checks to pass at all (fail-closed otherwise). Same trusted-query-param gap as the
   rest of the codebase (`viewer_id`/`viewer_role` are trusted query params until the
   auth extractor lands).
7. Offline-token revocation only affects records held in this process; tokens issued
   before a restart have no record and verify on signature + expiry alone. Git webhook
   secrets are held raw in memory only (required for HMAC verification) and never
   returned/serialized; only the SHA-256 hash is kept (and is the DB column). After a
   restart a distribution must be re-configured unless the integrator adds encrypted
   persistence.
8. Offline tokens are bearer credentials: replayable until expiry, single-product,
   revocation does NOT propagate to verifications already holding the token; offline
   verify does no DB lookup (counts returned as null). Signing secret is
   `store.download_signing_secret` when configured, else a random per-process secret
   (tokens/links die on restart) — same fallback pattern as the store module.
9. SSRF TOCTOU: DNS is re-resolved per hop for HTTP redirects, but a target whose DNS
   flips between the guard check and the connect (rebinding) is not re-validated at
   connect time; allowlist + `probe_private` gating is the primary defence.
10. Ping/traceroute depend on host binaries; the sandbox never executed them (no run
    against a live host). `delete_license` does not purge SaaS audit records
    (history/events/tokens) for the deleted license — intentional, documented as audit
    retention.
11. Migration 000069 must be re-checked at integration time: two other builders
    renumbered into 000066–000068 while B1 worked; if another 000069 lands first,
    renumber B1's migration (the only file referencing its own number is the line-165
    comment + down.sql header). Same caution for 000068.

## 11. Coordinator verification (by read-back, not by trusting reports)

- `network_tools.rs` 4,629 lines, `licensing.rs` 5,111 lines — both match the builders'
  counts. Braces/parens/brackets balanced (0/0/0) in both files.
- Greps over both batch files + both migrations: **0** forbidden reference-project
  names, **0** `todo!`/`unimplemented!`/`unreachable!` macros, **0** Indonesian words
  (word-boundary spot list).
- Routes: network-tools 33 `route_info` entries = 33 `.route(` entries;
  `require_capability` in all 33 handlers (34 call sites = 1 definition + 33 handlers;
  the 35th grep hit is a doc comment). Licensing: 38 `.route(` entries (12 existing +
  26 new); 21 `require_licensing_capability` occurrences (1 definition + 20 new-admin
  call sites); public endpoints carry API-key / HMAC / signed-link / offline-token /
  license validation instead.
- Migrations: `000068_licensing_saas` 6 CREATE / 6 DROP, `000069_network_tools` 5 CREATE /
  5 DROP; no number collisions (`000066_web3`, `000067_sharing_auditor` belong to other
  rounds). `mod.rs` contains no batch-7 registration (correct — integrator's job).
- Reuse seams confirmed on disk: `DnsVerifier` imported and used, `geo::lookup_geo`
  referenced, `SslStorage` consulted, `web3.rs` exists (licensing emits
  `license.anchored`, never calls it directly), `vcs::verify_webhook_signature` is
  private so the algorithm is replicated with a citation comment.
- **Not verified: compilation.** No toolchain, crates.io blocked. No endpoint has been
  live-tested. No migration has ever run against a live PostgreSQL.

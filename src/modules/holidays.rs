// Holidays module — global public-holiday synchronization.
//
// What this module does: it keeps a shared, cached reference set of
// public holidays per country and year, fetched from external
// providers, and answers whether a date is a holiday (or which days
// in a range are non-working) through the hook bus, so the calendar
// module and HR / academic / appointments / projects can consume
// holidays without ever calling this module directly. This module
// never writes into the calendar module or any other module: the
// hooks below are the only integration surface.
//
// Provider response shapes (from provider documentation, NOT
// live-verified — no outbound fetch has ever run from this
// workspace, so every shape below is documented, not proven):
//   - Nager.Date: GET {base}/api/v3/publicholidays/{year}/{country}
//     returns a JSON array of
//     {"date": "2026-01-01", "localName": "...", "name": "...",
//     "countryCode": "ID", "fixed": true, "global": true,
//     "counties": null, "launchYear": null,
//     "types": ["Public"]}. No API key. Free.
//   - Calendarific: GET {base}/api/v2/holidays?api_key={key}
//     &country={country}&year={year} returns
//     {"response": {"holidays": [{"name": "...",
//     "description": "...", "date": {"iso": "2026-01-01"},
//     "type": ["National holiday"]}]}}. Keyed.
//   - Abstract API: GET {base}/?api_key={key}&country={country}
//     &year={year} returns a JSON array of
//     {"date": "2026-01-01", "name": "...", ...}. Keyed. The parser
//     also accepts {"holidays": [...]} and a nested
//     {"date": {"iso": "..."}} date, because the documented shape
//     has varied across versions; unknown extra fields are ignored.
//   - Google Calendar public holidays: a public iCal feed per
//     country, GET
//     https://calendar.google.com/calendar/ical/{calendar_id}
//     /public/basic.ics where {calendar_id} is DATA in the provider
//     definition (`calendar_ids` map), never code. No key.
//   - Generic iCal: any public iCal feed URL supplied by the admin
//     on the sync config (`feed_url`). The feed host becomes the
//     only allowed host for that fetch.
//   iCal feeds are parsed by the hand-rolled parser in this file
//   (line unfolding + VEVENT DTSTART / DTEND / SUMMARY / UID):
//   there is no ical crate in Cargo.toml.
//
// Hook contracts (this module owns both; the calendar module is the
// first consumer, HR / academic / appointments / projects consume
// them the same way, through events only):
//   - `holidays.is_holiday` — request
//     {"date": "2026-12-25", "country": "ID",
//     "tenant_id": "<uuidv7>"} (tenant_id is REQUIRED; a request
//     without it gets no answer — fail closed, see Tenant scoping).
//     Answer: exactly one result object
//     {"date": "2026-12-25", "country": "ID",
//     "is_holiday": true, "name": "Christmas Day",
//     "source": "synced", "provider": "nager-date"} where
//     "source" is "synced" (fetched from a provider), "manual" (a
//     tenant override), or "not_configured" (the tenant has no
//     enabled sync config selecting that country; is_holiday is
//     false then). "provider" is the provider id that supplied the
//     holiday, or "manual" for an override, or null when
//     not_configured. A manual override WINS over a synced holiday
//     on the same date, including a "working_day" override that
//     cancels a synced holiday (is_holiday false, source manual).
//     NOTE (convergence gap, recorded 2026-11-22): the calendar
//     module (`src/modules/calendar.rs`, `non_working_days`) emits
//     this hook WITHOUT `tenant_id` today, so until it passes its
//     tenant id this handler answers nothing for calendar views
//     and the calendar falls back to weekend days only (its
//     documented behaviour when the hook has no answer). The HTTP
//     `GET /api/v1/holidays/is-holiday` endpoint always carries
//     tenant context and works fully.
//   - `holidays.non_working_days` — request
//     {"start": "2026-12-24", "end": "2026-12-26",
//     "country": "ID", "tenant_id": "<uuidv7>"} (tenant_id
//     REQUIRED; no tenant id means no answer). Answer: exactly one
//     result object {"start": ..., "end": ..., "country": "ID",
//     "in_scope": true, "dates": [{"date": "2026-12-25",
//     "name": "Christmas Day", "source": "synced",
//     "provider": "nager-date"}]}. The range is clamped to 366
//     days. When the country is not selected by any enabled config
//     of the tenant, the answer is {"dates": [], "in_scope": false}.
//   - `holidays.sync_requested` / `holidays.sync_all_requested` —
//     the scheduler-facing listener names for the `holidays.sync`
//     / `holidays.sync_all` job kinds (naming rule: `backup.run`
//     -> `backup.run_requested`). Payloads:
//     {"config_id": "<uuid>", "tenant_id": "<uuidv7>"} and
//     {"tenant_id": "<uuidv7>"}. The handler runs the sync in a
//     spawned task (or records a failed run when no async runtime
//     is available); a config whose tenant id does not match the
//     payload is ignored silently (no run, no leak).
//   Events emitted by this module:
//   - `holidays.provider.created/updated/deleted`,
//     `holidays.config.created/updated/deleted`,
//     `holidays.sync.started/finished/failed`,
//     `holidays.override.created/updated/deleted`
//     (payloads carry ids and tenant id, never a key).
//   - `notification.requested` on sync failure (same payload shape
//     the notifications module documents:
//     {"tenant_id", "title", "body", "kind", "category", "module",
//     "priority", "link"}), so a failed provider chain is logged
//     and alerted, never silently empty. Previously synced holidays
//     stay cached when a sync fails.
//
// Tenant scoping (standing rule — read before changing anything
// below). Sync configs, sync runs, and manual overrides are TENANT
// business data: every record carries `tenant_id` (uuidv7) and
// `company_id`, every lookup is filtered by the resolved tenant,
// and cross-tenant reads answer 404, never 403. The tenant context
// is resolved ONLY through `crate::core::tenancy::TenantContext`
// (`from_http_request`, which fails closed: 401 when the request
// did not resolve to a tenant, 403 when the resolved tenant id is
// not a valid UUID) — never from a client-supplied header, and
// never from a hard-coded tenant id, schema name, or package id.
// The schema name is derived ONLY by `tenancy::schema_name()` (via
// `schema_name_for_str()` for hook payloads); no `tenant_...` name
// is ever built inline in this file. NO TENANT CONTEXT MEANS FAIL
// CLOSED: every tenant-scoped handler rejects the request when
// there is no resolved tenant; nothing silently falls back to the
// `public` schema. Hook payloads carrying a tenant id that is not
// a valid UUID get no answer (fail closed, same rule). The provider
// registry and the country list are platform reference data
// (admin-editable definitions, no tenant id) — shared global lookup
// tables, readable from `public` by design (see `tenancy.rs`); they
// are the ONLY endpoints that do not require a tenant context, and
// their writes still require `holidays:manage`. The SYNCED
// holidays are shared reference data keyed by country+year (no
// tenant id), but a tenant only ever sees the countries its configs
// select, and its `holidays.is_holiday` / `holidays.non_working_days`
// answers are scoped to that tenant's countries and overrides. This
// module issues no SQL at all and never reads another module's
// tables: control-plane tables (tenants, subscriptions, domains,
// billing, licensing) live in `public` and are touched only through
// their own modules/events, never from here. Package entitlements
// are enforced by the licensing module through the `role.check`
// hook capabilities below, not by this module reading entitlement
// tables.
//
// CONVERGENCE POINT (batch 17 — tenancy plumbing EXISTS on disk,
// verified 2026-11-22 in `src/core/tenancy.rs`: `schema_name()`,
// `schema_name_for_str()`, `is_valid_schema_name()`,
// `TenantContext::from_http_request`,
// `with_tenant_connection`, `run_tenant_migrations`). This module
// already converges on it: `resolve_tenant` in this file is the
// single place the tenant context is resolved, it returns the
// tenancy `TenantContext` (tenant id as Uuid + schema name derived
// only by `schema_name()`), and hook payloads validate their
// tenant id through `schema_name_for_str()`. What is STILL open is
// only the Diesel persistence wiring: the in-memory DashMap store
// is authoritative and this module issues no SQL yet, so there is
// no `with_tenant_connection` call site to point at a tenant
// schema. When the database-backed implementation is wired in, it
// calls `tenancy::with_tenant_connection(pool, tenant_uuid, ...)`
// with the Uuid from `resolve_tenant`, and the migration tables
// (`holiday_providers`, `holiday_countries`,
// `holiday_sync_configs`, `holiday_sync_runs`, `holidays`,
// `holiday_overrides`, each already carrying a `tenant_id` column
// where the data is tenant-owned) are created inside the tenant
// schema by `tenancy::run_tenant_migrations` instead of `public`.
// No handler, no record shape, and no hook contract changes:
// `tenant_id` is already on every tenant-owned record and every
// filter. The migration SQL is deliberately schema-agnostic (no
// schema qualification): the migration runner sets `search_path`
// per tenant when it runs.
//
// Persistence: the live data lives in in-memory DashMap stores
// (the same precedent as the todos, library, calendar, and other
// modules: the in-memory store is authoritative). The migration
// `2026-11-22-000098_holidays` defines the persistence targets
// (`holiday_providers`, `holiday_countries`,
// `holiday_sync_configs`, `holiday_sync_runs`, `holidays`,
// `holiday_overrides`); the tables are not yet the read path and
// rows do not survive a restart until a database-backed
// implementation is wired in. No plaintext secret is stored in any
// table: provider API keys are stored only as `v1.` ciphertext
// (see the Credentials section).
//
// Credentials are write-only, following the ai.rs pattern exactly:
// an API key is accepted once, encrypted with a key derived from
// `app.secret_key`, and stored only as ciphertext (`v1.` HMAC
// keystream with an HMAC tag — not an AEAD cipher, because no AEAD
// crate is in Cargo.toml yet; when one is added, replace
// `encrypt_api_key` / `decrypt_api_key` and bump the version prefix
// so old values can be re-encrypted on next write). The encryption
// helpers in this file are a copy of the ai.rs `v1.` format, so a
// value encrypted here decrypts with the same secret there and
// vice versa. No response, log line, hook payload, or error message
// ever contains a key or a fragment of one; read views expose only
// `key_configured` and a short fingerprint prefix (first 12 hex
// characters of the SHA-256). The development fallback secret is
// the same string ai.rs uses, so the fallback behaviour is
// identical when `app.secret_key` is unset.
//
// SSRF guard (follows `src/modules/downloader.rs` exactly): every
// outbound fetch is https only, carries no embedded credentials,
// and its host must be in the provider's allowlist AND resolve only
// to public addresses (no private, link-local, loopback, or
// reserved addresses; a mixed DNS answer is refused). Every
// redirect target is re-validated with the same rules before it is
// followed (max 10 redirects). A DNS change between validation and
// fetch (TOCTOU rebinding) is still possible and is recorded as an
// open limitation, not a closed gap — same as the downloader.
//
// Rate limiting: each provider definition carries `min_interval_secs`
// (minimum seconds between requests) and `max_requests_per_minute`;
// both are enforced per provider before every fetch, a 429 honours
// the `Retry-After` header (capped at 60 seconds), and transient
// failures (408 / 429 / 5xx, connection errors, timeouts) retry
// with exponential backoff (max 3 attempts). The module never
// hammers a provider.
//
// Sync semantics: fetch -> validate -> store per country+year,
// cached, deduplicated by (country, date, name); a re-sync upserts
// in place and never duplicates. Last-sync state is recorded per
// (provider, country, year) in `holiday_sync_runs` (fetched_at,
// count, status, error). The fallback chain (ordered provider ids
// on the config) is tried in order on failure.
//
// Modularity rule: nothing is hard-coded except standard constants
// (ISO 3166-1 alpha-2 codes, HTTP status codes, the `v1.` key
// format prefix). Providers, countries, schedules, fallback chains,
// weekend handling (this module does not define weekends at all —
// the calendar owns them), and all limits below are data or
// configurable defaults, clearly marked CONFIG below.
//
// Routes live under `/api/v1/holidays` (27 routes; see
// `route_info` — the count there must equal the `.route(` count
// in `configure_routes`).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, ProductCategory, RouteInfo, RouteVersionSupport};
use crate::core::tenancy::{schema_name_for_str, TenantContext as TenancyContext};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::header::RETRY_AFTER;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

pub const EVENT_IS_HOLIDAY: &str = "holidays.is_holiday";
pub const EVENT_NON_WORKING_DAYS: &str = "holidays.non_working_days";
pub const EVENT_SYNC_REQUESTED: &str = "holidays.sync_requested";
pub const EVENT_SYNC_ALL_REQUESTED: &str = "holidays.sync_all_requested";
pub const EVENT_PROVIDER_CREATED: &str = "holidays.provider.created";
pub const EVENT_PROVIDER_UPDATED: &str = "holidays.provider.updated";
pub const EVENT_PROVIDER_DELETED: &str = "holidays.provider.deleted";
pub const EVENT_CONFIG_CREATED: &str = "holidays.config.created";
pub const EVENT_CONFIG_UPDATED: &str = "holidays.config.updated";
pub const EVENT_CONFIG_DELETED: &str = "holidays.config.deleted";
pub const EVENT_SYNC_STARTED: &str = "holidays.sync.started";
pub const EVENT_SYNC_FINISHED: &str = "holidays.sync.finished";
pub const EVENT_SYNC_FAILED: &str = "holidays.sync.failed";
pub const EVENT_OVERRIDE_CREATED: &str = "holidays.override.created";
pub const EVENT_OVERRIDE_UPDATED: &str = "holidays.override.updated";
pub const EVENT_OVERRIDE_DELETED: &str = "holidays.override.deleted";

// ---------------------------------------------------------------------------
// Capabilities (PERM_* const pattern, as in todos.rs).
// ---------------------------------------------------------------------------

pub const PERM_HOLIDAYS_READ: &str = "holidays:read";
pub const PERM_HOLIDAYS_MANAGE: &str = "holidays:manage";
pub const PERM_HOLIDAYS_SYNC: &str = "holidays:sync";

// ---------------------------------------------------------------------------
// CONFIG — configurable defaults (operator-overridable without code
// changes; the values below are the shipped defaults, not constants
// of the domain).
// ---------------------------------------------------------------------------

/// Default provider when a config does not name one explicitly.
/// (CONFIG: the seeded provider with this id; see `seed_providers`.)
const DEFAULT_PROVIDER_ID: &str = "nager-date";
/// Default sync schedule expression (interval seconds) applied when
/// a config omits `schedule`. (CONFIG: daily.)
const DEFAULT_SCHEDULE: &str = "86400";
/// Default years synced when a config omits `years`: the current
/// year and the next one. (CONFIG.)
const DEFAULT_YEARS_AHEAD: i32 = 1;
/// Hard bounds for a sync year. (CONFIG.)
const MIN_SYNC_YEAR: i32 = 2000;
const MAX_SYNC_YEAR: i32 = 2100;
/// Maximum years one config may sync. (CONFIG.)
const MAX_YEARS_PER_CONFIG: usize = 5;
/// Maximum countries one config may select. (CONFIG.)
const MAX_COUNTRIES_PER_CONFIG: usize = 60;
/// Maximum fallback providers on one config. (CONFIG.)
const MAX_FALLBACK_PROVIDERS: usize = 4;
/// Maximum days a `non_working_days` range may span. (CONFIG.)
const MAX_NON_WORKING_RANGE_DAYS: i64 = 366;
/// Maximum days a `non_working_days` hook range may span before the
/// handler clamps it. (CONFIG.)
const MAX_HOOK_RANGE_DAYS: i64 = 366;
/// Fetch timeout bounds in seconds. (CONFIG.)
const DEFAULT_FETCH_TIMEOUT_SECS: u64 = 30;
const MIN_FETCH_TIMEOUT_SECS: u64 = 5;
const MAX_FETCH_TIMEOUT_SECS: u64 = 120;
/// Maximum response body accepted from a provider. (CONFIG.)
const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
/// Maximum redirects followed per fetch. (CONFIG.)
const MAX_REDIRECTS: u32 = 10;
/// Longest `Retry-After` delay honoured from a provider. (CONFIG.)
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
/// Maximum fetch attempts per (provider, country, year), including
/// the first attempt. (CONFIG.)
const MAX_FETCH_ATTEMPTS: u32 = 3;
/// Base delay before the first retry; doubled per attempt, capped.
/// (CONFIG.)
const RETRY_BASE_DELAY: Duration = Duration::from_secs(1);
const RETRY_MAX_DELAY: Duration = Duration::from_secs(8);
/// Maximum rate-limit wait before a fetch gives up instead of
/// sleeping longer. (CONFIG.)
const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_secs(60);
/// Scheduler job kinds (the scheduler's `KNOWN_KINDS` does not
/// contain these yet — see the OPEN list in the module report — so
/// today `scheduler.enqueue` declines them and the sync is recorded
/// with `scheduler_status: "declined"` instead of pretending it was
/// scheduled; the sync itself still runs when triggered by HTTP).
const SCHEDULER_JOB_KIND_SYNC: &str = "holidays.sync";
const SCHEDULER_JOB_KIND_SYNC_ALL: &str = "holidays.sync_all";
/// User-Agent sent with provider requests.
const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"));
/// Development fallback encryption secret, identical to ai.rs so the
/// fallback behaviour matches when `app.secret_key` is unset.
const DEV_ENCRYPTION_SECRET: &str = "unified-ai-development-secret";

// ---------------------------------------------------------------------------
// Helpers: tenant, caller, capabilities, envelope.
// ---------------------------------------------------------------------------

/// Resolve the tenant context for a request. THE single convergence
/// point for batch-17 schema-per-tenant plumbing (see the module
/// header): this function returns the tenancy `TenantContext`
/// (tenant id as Uuid + schema name derived ONLY by
/// `tenancy::schema_name()`), and the Diesel persistence wiring —
/// when it lands — takes the Uuid from here into
/// `tenancy::with_tenant_connection`. Fail closed:
/// `from_http_request` answers 401 when the request did not resolve
/// to a tenant and 403 when the resolved tenant id is not a valid
/// UUID; no resolved tenant means the request is refused here,
/// never served from `public`.
fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}

/// The caller, from the headers every module in this codebase uses:
/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// the capability check runs against. (Same shape as `todos.rs` and
/// `calendar.rs`.) The tenant id, schema name, and company id come
/// from the resolved tenant context, never from a client header.
/// `schema_name` is carried (not just derived on demand) so the
/// future Diesel persistence layer takes it from exactly one place.
struct Caller {
    actor: String,
    role: String,
    tenant_id: String,
    /// Derived ONLY by `tenancy::schema_name()` (see
    /// `resolve_tenant`). Unread until the Diesel persistence layer
    /// lands and takes it into `tenancy::with_tenant_connection`.
    #[allow(dead_code)]
    schema_name: String,
    company_id: String,
}

/// Tenant-scoped caller: resolves the tenant first, then the actor
/// headers. Used by every tenant-owned endpoint (configs, runs,
/// holidays, overrides).
fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let tenant = resolve_tenant(req)?;
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller {
        actor,
        role,
        tenant_id: tenant.tenant_id.to_string(),
        schema_name: tenant.schema_name.clone(),
        company_id: tenant.company_id.clone().unwrap_or_default(),
    })
}

/// Platform caller: actor headers only, no tenant context. Used ONLY
/// by the provider registry and country list endpoints, which are
/// platform reference data (not tenant business data). A platform
/// caller can never read, create, or touch tenant-owned records.
struct PlatformCaller {
    actor: String,
    role: String,
}

fn platform_caller(req: &HttpRequest) -> UnifiedResult<PlatformCaller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(PlatformCaller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

/// Fail-closed ownership rule (same as todos.rs): a tenant-owned
/// record that belongs to another tenant answers 404, never 403, so
/// existence never leaks across tenants.
fn not_found_for_tenant(what: &str) -> UnifiedError {
    UnifiedError::not_found(format!("{what} not found"))
}

// ---------------------------------------------------------------------------
// Credential encryption (write-only storage) — same `v1.` format as
// ai.rs, copied so the two modules stay interoperable. See the module
// header: HMAC keystream with an HMAC tag, NOT an AEAD cipher.
// ---------------------------------------------------------------------------

type HmacSha256 = Hmac<Sha256>;

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex_of(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The encryption secret: `app.secret_key`, or the development
/// fallback when none is configured.
fn encryption_secret(ctx: &AppContext) -> String {
    let config = ctx.config.read().expect("config lock poisoned");
    if config.app.secret_key.trim().is_empty() {
        DEV_ENCRYPTION_SECRET.to_string()
    } else {
        config.app.secret_key.clone()
    }
}

/// Keystream for the XOR stream cipher: HMAC-SHA256 blocks keyed by
/// the SHA-256 of the secret, over `nonce || counter`.
fn keystream(key: &[u8], nonce: &[u8], len: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(len);
    let mut counter: u32 = 0;
    while out.len() < len {
        let mut message = nonce.to_vec();
        message.extend_from_slice(&counter.to_be_bytes());
        out.extend_from_slice(&hmac_sha256(key, &message));
        counter += 1;
    }
    out.truncate(len);
    out
}

/// Constant-time equality for authentication tags.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Encrypt an API key. Output format: `v1.<base64url(nonce ||
/// ciphertext || tag)>`. The tag covers nonce + ciphertext.
fn encrypt_api_key(secret: &str, plaintext: &str) -> String {
    let key = Sha256::digest(secret.as_bytes());
    let nonce: [u8; 16] = rand::random();
    let stream = keystream(&key, &nonce, plaintext.len());
    let ciphertext: Vec<u8> = plaintext
        .as_bytes()
        .iter()
        .zip(stream.iter())
        .map(|(plain, mask)| plain ^ mask)
        .collect();
    let mut tag_message = nonce.to_vec();
    tag_message.extend_from_slice(&ciphertext);
    let tag = hmac_sha256(&key, &tag_message);
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ciphertext);
    blob.extend_from_slice(&tag);
    format!(
        "v1.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(blob)
    )
}

/// Decrypt a value produced by `encrypt_api_key`. The error message
/// never includes any part of the stored value.
fn decrypt_api_key(secret: &str, stored: &str) -> UnifiedResult<String> {
    let encoded = stored
        .strip_prefix("v1.")
        .ok_or_else(|| UnifiedError::internal("Stored provider key has an unknown format"))?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| UnifiedError::internal("Stored provider key cannot be decoded"))?;
    if blob.len() < 16 + 32 {
        return Err(UnifiedError::internal(
            "Stored provider key cannot be decrypted",
        ));
    }
    let (nonce, rest) = blob.split_at(16);
    let (ciphertext, tag) = rest.split_at(rest.len() - 32);
    let key = Sha256::digest(secret.as_bytes());
    let mut tag_message = nonce.to_vec();
    tag_message.extend_from_slice(ciphertext);
    let expected = hmac_sha256(&key, &tag_message);
    if !constant_time_eq(&expected, tag) {
        return Err(UnifiedError::internal(
            "Stored provider key failed authentication",
        ));
    }
    let stream = keystream(&key, nonce, ciphertext.len());
    let plaintext: Vec<u8> = ciphertext
        .iter()
        .zip(stream.iter())
        .map(|(cipher, mask)| cipher ^ mask)
        .collect();
    String::from_utf8(plaintext)
        .map_err(|_| UnifiedError::internal("Stored provider key cannot be decrypted"))
}

/// Short fingerprint prefix of a key, for the masked read view. Only
/// the first 12 hex characters of the SHA-256 are exposed, never the
/// key itself.
fn key_fingerprint(plaintext: &str) -> String {
    sha256_hex_of(plaintext.as_bytes()).chars().take(12).collect()
}

// ---------------------------------------------------------------------------
// SSRF guard — follows `src/modules/downloader.rs` exactly, except
// provider fetches are https ONLY (the downloader also allows http;
// no holiday provider serves plain http, so the stricter rule
// applies here).
// ---------------------------------------------------------------------------

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => addr.is_loopback(),
        IpAddr::V6(addr) => addr.is_loopback(),
    }
}

/// True when the address is private, link-local, or otherwise
/// reserved: never a valid provider target. Loopback is handled
/// separately (never allowed for provider fetches).
fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => {
            addr.is_private()
                || addr.is_link_local()
                || addr.is_unspecified()
                || addr.is_broadcast()
                || addr.is_multicast()
                || addr.is_documentation()
                || addr.is_benchmarking()
                || addr.is_reserved()
        }
        IpAddr::V6(addr) => {
            addr.is_unspecified()
                || addr.is_multicast()
                || (addr.segments()[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (addr.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || addr.is_unique_local()
        }
    }
}

/// Resolve a hostname and require EVERY resolved address to be safe.
/// A mixed answer (one public, one private address) is refused: the
/// HTTP client picks among the resolved addresses, so any private
/// address in the answer could be the one fetched.
fn resolved_addresses_are_safe(url: &url::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        return !is_loopback(ip) && !is_forbidden_ip(ip);
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let Ok(addresses) = (host, port).to_socket_addrs() else {
        return false;
    };
    let mut any = false;
    for socket in addresses {
        any = true;
        let ip = socket.ip();
        if is_loopback(ip) || is_forbidden_ip(ip) {
            return false;
        }
    }
    any
}

/// Validate a provider fetch URL: https only, no embedded
/// credentials, host on the provider's allowlist, and no hostname
/// that resolves to a forbidden address. A DNS change between
/// validation and fetch (TOCTOU rebinding) is still possible and is
/// recorded as an open limitation, not a closed gap.
fn validate_provider_url(raw: &str, allowed_hosts: &[String]) -> UnifiedResult<()> {
    let url = url::Url::parse(raw.trim())
        .map_err(|_| UnifiedError::bad_request("Invalid provider URL"))?;
    if url.scheme() != "https" {
        return Err(UnifiedError::bad_request(
            "Provider URL must use https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Provider URL must not contain embedded credentials",
        ));
    }
    let Some(host) = url.host_str() else {
        return Err(UnifiedError::bad_request(
            "Provider URL must have a host",
        ));
    };
    if host.eq_ignore_ascii_case("localhost") {
        return Err(UnifiedError::bad_request(
            "Provider URL host \"localhost\" is not allowed",
        ));
    }
    if url.port().is_some_and(|port| port != 443) {
        return Err(UnifiedError::bad_request(
            "Provider URL must use the default https port (443)",
        ));
    }
    let host_lower = host.to_ascii_lowercase();
    if !allowed_hosts
        .iter()
        .any(|allowed| allowed.to_ascii_lowercase() == host_lower)
    {
        return Err(UnifiedError::bad_request(
            "Provider URL host is not on the provider allowlist",
        ));
    }
    if !resolved_addresses_are_safe(&url) {
        return Err(UnifiedError::bad_request(
            "Provider URL host resolves to a private, link-local, or reserved address; fetch refused",
        ));
    }
    Ok(())
}

/// Build an HTTP client whose redirect policy re-validates every
/// redirect target with the same SSRF rules as the original URL
/// (same as the downloader's `build_client`).
fn build_fetch_client(allowed_hosts: Arc<Vec<String>>) -> UnifiedResult<reqwest::Client> {
    let redirect = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() as u32 >= MAX_REDIRECTS {
            return attempt.stop();
        }
        match validate_provider_url(attempt.url().as_str(), &allowed_hosts) {
            Ok(()) => attempt.follow(),
            Err(_) => attempt.stop(),
        }
    });
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(DEFAULT_FETCH_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(10))
        .redirect(redirect)
        .tcp_nodelay(true)
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// Provider kind. The kind selects the request shape and the parser;
/// everything else about a provider (base URL, allowlist, rate
/// limits, key requirement, calendar ids) is DATA on the definition,
/// so adding a provider never requires a code change — except a
/// genuinely new wire shape, which adds one kind here and one parse
/// arm in `fetch_holidays_for_provider`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Nager.Date JSON API (free, no key).
    NagerDate,
    /// Calendarific JSON API (keyed).
    Calendarific,
    /// Abstract API Holidays JSON API (keyed).
    AbstractApi,
    /// Google Calendar public holiday iCal feed (no key; the per
    /// country calendar id is data on the definition).
    GoogleIcal,
    /// Any public iCal feed URL supplied by the admin (no key).
    GenericIcal,
}

impl ProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::NagerDate => "nager_date",
            ProviderKind::Calendarific => "calendarific",
            ProviderKind::AbstractApi => "abstract_api",
            ProviderKind::GoogleIcal => "google_ical",
            ProviderKind::GenericIcal => "generic_ical",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "nager_date" => Some(ProviderKind::NagerDate),
            "calendarific" => Some(ProviderKind::Calendarific),
            "abstract_api" => Some(ProviderKind::AbstractApi),
            "google_ical" => Some(ProviderKind::GoogleIcal),
            "generic_ical" => Some(ProviderKind::GenericIcal),
            _ => None,
        }
    }

    /// True when the kind needs an API key to answer at all.
    pub fn key_required(&self) -> bool {
        matches!(self, ProviderKind::Calendarific | ProviderKind::AbstractApi)
    }

    /// True when the kind serves iCal feeds instead of JSON.
    pub fn is_ical(&self) -> bool {
        matches!(self, ProviderKind::GoogleIcal | ProviderKind::GenericIcal)
    }
}

/// One provider definition in the registry. Platform reference
/// data: no tenant id. `api_key_encrypted` holds only `v1.`
/// ciphertext (never plaintext); `key_fingerprint` holds the first
/// 12 hex characters of the SHA-256 of the key, for the masked read
/// view. `calendar_ids` maps ISO alpha-2 country codes to public
/// Google Calendar ids (data, only used by `google_ical`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HolidayProvider {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    /// Exact hostnames this provider may be fetched from (no
    /// wildcards). A fetch to any other host is refused.
    pub allowed_hosts: Vec<String>,
    pub api_key_required: bool,
    pub api_key_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    /// Minimum seconds between requests to this provider.
    pub min_interval_secs: u64,
    /// Maximum requests per rolling minute to this provider.
    pub max_requests_per_minute: u32,
    /// Per-country public calendar ids (`google_ical` only).
    pub calendar_ids: HashMap<String, String>,
    pub enabled: bool,
    /// True for the providers seeded by this module. Built-in
    /// providers cannot be deleted (disable them instead) and their
    /// kind / base URL cannot change; everything else is editable.
    pub builtin: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl HolidayProvider {
    /// Masked public view: the key itself never appears, only
    /// whether one is configured and its fingerprint prefix.
    pub fn public_view(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "kind": self.kind.as_str(),
            "base_url": self.base_url,
            "allowed_hosts": self.allowed_hosts,
            "api_key_required": self.api_key_required,
            "key_configured": self.api_key_encrypted.is_some(),
            "key_fingerprint": self.key_fingerprint,
            "min_interval_secs": self.min_interval_secs,
            "max_requests_per_minute": self.max_requests_per_minute,
            "calendar_ids": self.calendar_ids,
            "enabled": self.enabled,
            "builtin": self.builtin,
            "created_at": self.created_at.to_rfc3339(),
            "updated_at": self.updated_at.to_rfc3339(),
        })
    }
}

/// One ISO 3166-1 alpha-2 country. Platform reference data: the
/// list is seeded from the `ISO_COUNTRIES` table below (data, not
/// code) and an admin can add or disable entries without a code
/// change. A disabled country cannot be selected by new configs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HolidayCountry {
    pub code: String,
    pub name: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One tenant's sync configuration: which provider (plus an ordered
/// fallback chain), which countries, which years, and on what
/// schedule. TENANT business data: carries `tenant_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HolidaySyncConfig {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub provider_id: String,
    /// Ordered fallback provider ids, tried in order when the
    /// primary provider fails for a (country, year).
    pub fallback_provider_ids: Vec<String>,
    /// ISO 3166-1 alpha-2 country codes this tenant syncs.
    pub countries: Vec<String>,
    /// Years this tenant syncs.
    pub years: Vec<i32>,
    /// Schedule expression: either an interval in seconds
    /// ("86400") or five cron-like fields ("0 2 * * *").
    /// Interpreted by the scheduler when it fires
    /// `holidays.sync`; stored here as data.
    pub schedule: String,
    /// Public iCal feed URL, only for `generic_ical` providers.
    /// Validated with the SSRF rules on save and on every fetch.
    pub feed_url: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl HolidaySyncConfig {
    pub fn public_view(&self) -> Value {
        json!({
            "id": self.id,
            "tenant_id": self.tenant_id,
            "company_id": self.company_id,
            "provider_id": self.provider_id,
            "fallback_provider_ids": self.fallback_provider_ids,
            "countries": self.countries,
            "years": self.years,
            "schedule": self.schedule,
            "feed_url": self.feed_url,
            "enabled": self.enabled,
            "created_at": self.created_at.to_rfc3339(),
            "updated_at": self.updated_at.to_rfc3339(),
        })
    }
}

/// Last-sync state for one (provider, country, year) attempt.
/// TENANT business data: carries `tenant_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HolidaySyncRun {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub config_id: Uuid,
    pub provider_id: String,
    pub country: String,
    pub year: i32,
    /// "success" | "failed" | "partial" (partial is only used on
    /// the per-config summary, never on a single run row).
    pub status: String,
    /// "manual" | "scheduled" | "sync_all".
    pub trigger: String,
    /// Holidays the provider returned for this (country, year).
    pub holidays_found: i64,
    /// Holidays upserted into the store (new or updated).
    pub holidays_stored: i64,
    /// Failure reason, when status is "failed". Never contains a
    /// key, a URL with a key, or a response body.
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub fetched_at: DateTime<Utc>,
}

impl HolidaySyncRun {
    pub fn public_view(&self) -> Value {
        json!({
            "id": self.id,
            "tenant_id": self.tenant_id,
            "company_id": self.company_id,
            "config_id": self.config_id,
            "provider_id": self.provider_id,
            "country": self.country,
            "year": self.year,
            "status": self.status,
            "trigger": self.trigger,
            "holidays_found": self.holidays_found,
            "holidays_stored": self.holidays_stored,
            "error": self.error,
            "started_at": self.started_at.to_rfc3339(),
            "finished_at": self.finished_at.to_rfc3339(),
            "fetched_at": self.fetched_at.to_rfc3339(),
        })
    }
}

/// One synced holiday. SHARED reference data keyed by
/// country+year: no tenant id. A tenant only ever sees the
/// countries its configs select (enforced on every read path).
/// Deduplicated by (country, date, name): a re-sync upserts in
/// place and never duplicates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Holiday {
    pub id: Uuid,
    pub country: String,
    pub date: NaiveDate,
    pub name: String,
    pub local_name: Option<String>,
    pub provider_id: String,
    /// Always "synced" for this struct (manual days live in
    /// `HolidayOverride`); kept so views and hook answers share one
    /// shape with overrides.
    pub source: String,
    pub global: bool,
    pub types: Vec<String>,
    pub counties: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Holiday {
    pub fn public_view(&self) -> Value {
        json!({
            "id": self.id,
            "country": self.country,
            "date": self.date.to_string(),
            "name": self.name,
            "local_name": self.local_name,
            "provider_id": self.provider_id,
            "source": self.source,
            "global": self.global,
            "types": self.types,
            "counties": self.counties,
            "created_at": self.created_at.to_rfc3339(),
            "updated_at": self.updated_at.to_rfc3339(),
        })
    }
}

/// One manual holiday / override. TENANT business data: carries
/// `tenant_id`. Overrides merge with synced holidays and WIN on the
/// same date: `day_off` adds a company-specific day off;
/// `working_day` cancels a synced holiday for this tenant (for
/// example a holiday moved to another date by local decree).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HolidayOverride {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub country: String,
    pub date: NaiveDate,
    pub name: String,
    /// "day_off" | "working_day".
    pub override_kind: String,
    pub note: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl HolidayOverride {
    pub fn public_view(&self) -> Value {
        json!({
            "id": self.id,
            "tenant_id": self.tenant_id,
            "company_id": self.company_id,
            "country": self.country,
            "date": self.date.to_string(),
            "name": self.name,
            "override_kind": self.override_kind,
            "note": self.note,
            "created_by": self.created_by,
            "created_at": self.created_at.to_rfc3339(),
            "updated_at": self.updated_at.to_rfc3339(),
        })
    }
}

/// One holiday as fetched from a provider, before storage.
#[derive(Debug, Clone)]
struct FetchedHoliday {
    date: NaiveDate,
    name: String,
    local_name: Option<String>,
    global: bool,
    types: Vec<String>,
    counties: Vec<String>,
}

/// Per-provider rate-limit state (in-memory only).
#[derive(Debug, Default)]
struct RateState {
    last_request: Option<Instant>,
    /// Request start instants inside the rolling one-minute window.
    window: VecDeque<Instant>,
}

/// Summary of one config sync, returned by the sync endpoints.
#[derive(Debug, Clone, Serialize)]
struct SyncSummary {
    config_id: Uuid,
    tenant_id: String,
    trigger: String,
    runs: Vec<Value>,
    attempted: usize,
    succeeded: usize,
    failed: usize,
    holidays_found: i64,
    holidays_stored: i64,
    scheduler_status: String,
}

// ---------------------------------------------------------------------------
// Request DTOs.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateProviderRequest {
    id: String,
    name: String,
    kind: String,
    base_url: String,
    allowed_hosts: Vec<String>,
    api_key_required: Option<bool>,
    /// Write-only: accepted once, encrypted, never returned.
    api_key: Option<String>,
    min_interval_secs: Option<u64>,
    max_requests_per_minute: Option<u32>,
    calendar_ids: Option<HashMap<String, String>>,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct UpdateProviderRequest {
    name: Option<String>,
    allowed_hosts: Option<Vec<String>>,
    /// Write-only rotation: present + non-empty replaces the stored
    /// key; absent leaves it unchanged; empty string clears it.
    api_key: Option<String>,
    min_interval_secs: Option<u64>,
    max_requests_per_minute: Option<u32>,
    calendar_ids: Option<HashMap<String, String>>,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct TestProviderRequest {
    country: Option<String>,
    year: Option<i32>,
    /// Feed URL for a `generic_ical` test (validated with the SSRF
    /// rules; never stored by this endpoint).
    feed_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateCountryRequest {
    code: String,
    name: String,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct UpdateCountryRequest {
    name: Option<String>,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CreateConfigRequest {
    provider_id: Option<String>,
    fallback_provider_ids: Option<Vec<String>>,
    countries: Vec<String>,
    years: Option<Vec<i32>>,
    schedule: Option<String>,
    feed_url: Option<String>,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct UpdateConfigRequest {
    provider_id: Option<String>,
    fallback_provider_ids: Option<Vec<String>>,
    countries: Option<Vec<String>>,
    years: Option<Vec<i32>>,
    schedule: Option<String>,
    /// Present + non-empty replaces the feed URL; empty string
    /// clears it; absent leaves it unchanged.
    feed_url: Option<String>,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CreateOverrideRequest {
    country: String,
    date: String,
    name: String,
    override_kind: Option<String>,
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateOverrideRequest {
    name: Option<String>,
    override_kind: Option<String>,
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IsHolidayQuery {
    date: String,
    country: String,
}

#[derive(Debug, Deserialize)]
struct NonWorkingDaysQuery {
    start: String,
    end: String,
    country: String,
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct HolidaysState {
    /// Provider registry, keyed by provider id. Platform data.
    pub providers: Arc<DashMap<String, HolidayProvider>>,
    /// Country list, keyed by upper-case alpha-2 code. Platform data.
    pub countries: Arc<DashMap<String, HolidayCountry>>,
    /// Sync configs, keyed by id. Tenant data.
    pub configs: Arc<DashMap<Uuid, HolidaySyncConfig>>,
    /// Sync runs, keyed by id. Tenant data.
    pub runs: Arc<DashMap<Uuid, HolidaySyncRun>>,
    /// Synced holidays, keyed by "{country}|{date}|{name}".
    /// Shared reference data.
    pub holidays: Arc<DashMap<String, Holiday>>,
    /// Manual overrides, keyed by id. Tenant data.
    pub overrides: Arc<DashMap<Uuid, HolidayOverride>>,
    /// Per-provider rate-limit state, keyed by provider id.
    rate_state: Arc<DashMap<String, Arc<Mutex<RateState>>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
    /// Encryption secret captured at `init` (a clone of
    /// `app.secret_key`, the same value `encryption_secret` reads
    /// from config) so scheduled syncs running in spawned tasks can
    /// decrypt provider keys without an `AppContext` at hand.
    encryption_secret: Arc<RwLock<Option<String>>>,
}

impl Default for HolidaysState {
    fn default() -> Self {
        Self {
            providers: Arc::new(DashMap::new()),
            countries: Arc::new(DashMap::new()),
            configs: Arc::new(DashMap::new()),
            runs: Arc::new(DashMap::new()),
            holidays: Arc::new(DashMap::new()),
            overrides: Arc::new(DashMap::new()),
            rate_state: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
            encryption_secret: Arc::new(RwLock::new(None)),
        }
    }
}

impl HolidaysState {
    fn new() -> Self {
        Self::default()
    }

    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn secret(&self) -> String {
        self.encryption_secret
            .read()
            .expect("encryption secret lock poisoned")
            .clone()
            .unwrap_or_else(|| DEV_ENCRYPTION_SECRET.to_string())
    }

    /// Seed the provider registry and the country list on first
    /// init. Idempotent: only inserts when the key is absent, so a
    /// restart never overwrites an admin's edits.
    fn seed_defaults(&self) {
        let now = Utc::now();
        for provider in seed_providers(now) {
            self.providers.entry(provider.id.clone()).or_insert(provider);
        }
        for (code, name) in ISO_COUNTRIES {
            let code = code.to_string();
            self.countries.entry(code.clone()).or_insert(HolidayCountry {
                code,
                name: name.to_string(),
                enabled: true,
                created_at: now,
                updated_at: now,
            });
        }
    }

    /// Countries an enabled config of this tenant selects.
    fn tenant_countries(&self, tenant_id: &str) -> HashSet<String> {
        let mut out = HashSet::new();
        for config in self.configs.iter() {
            if config.tenant_id == tenant_id && config.enabled {
                for country in &config.countries {
                    out.insert(country.clone());
                }
            }
        }
        out
    }

    /// The manual override (if any) this tenant has for one
    /// (country, date). At most one override per (tenant, country,
    /// date, kind) exists; a `working_day` override and a `day_off`
    /// override cannot coexist for the same date — creation refuses
    /// the second one — so the first match found is the answer.
    fn tenant_override(&self, tenant_id: &str, country: &str, date: NaiveDate) -> Option<HolidayOverride> {
        self.overrides
            .iter()
            .find(|entry| {
                entry.tenant_id == tenant_id && entry.country == country && entry.date == date
            })
            .map(|entry| entry.clone())
    }

    /// The synced holiday (if any) for one (country, date). When
    /// several synced holidays share a date, the first by name wins
    /// for the single-day answer; the range answer lists all of them.
    fn synced_on(&self, country: &str, date: NaiveDate) -> Option<Holiday> {
        let mut found: Vec<Holiday> = self
            .holidays
            .iter()
            .filter(|entry| entry.country == country && entry.date == date)
            .map(|entry| entry.clone())
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found.into_iter().next()
    }
}

/// Seeded provider definitions. DATA, not behaviour: an admin can
/// add, edit, disable, or (for non-built-in entries) delete any of
/// these through the endpoints, and a new provider never needs a
/// code change. Base URLs, hosts, rate limits, and calendar ids are
/// from provider documentation, NOT live-verified.
fn seed_providers(now: DateTime<Utc>) -> Vec<HolidayProvider> {
    let mut google_calendar_ids: HashMap<String, String> = HashMap::new();
    // Public Google Calendar holiday ids, from public documentation,
    // NOT live-verified. Pattern:
    // en.<country>#holiday@group.v.calendar.google.com. Admins add
    // more countries through PATCH /providers/google-ical.
    for (country, calendar_id) in [
        ("US", "en.usa#holiday@group.v.calendar.google.com"),
        ("GB", "en.uk#holiday@group.v.calendar.google.com"),
        ("ID", "en.indonesia#holiday@group.v.calendar.google.com"),
        ("DE", "en.german#holiday@group.v.calendar.google.com"),
        ("FR", "en.french#holiday@group.v.calendar.google.com"),
        ("JP", "en.japanese#holiday@group.v.calendar.google.com"),
        ("AU", "en.australian#holiday@group.v.calendar.google.com"),
        ("CA", "en.canadian#holiday@group.v.calendar.google.com"),
        ("IN", "en.indian#holiday@group.v.calendar.google.com"),
        ("SG", "en.singapore#holiday@group.v.calendar.google.com"),
        ("MY", "en.malaysia#holiday@group.v.calendar.google.com"),
        ("NL", "en.dutch#holiday@group.v.calendar.google.com"),
    ] {
        google_calendar_ids.insert(country.to_string(), calendar_id.to_string());
    }
    vec![
        HolidayProvider {
            id: "nager-date".to_string(),
            name: "Nager.Date".to_string(),
            kind: ProviderKind::NagerDate,
            base_url: "https://date.nager.at".to_string(),
            allowed_hosts: vec!["date.nager.at".to_string()],
            api_key_required: false,
            api_key_encrypted: None,
            key_fingerprint: None,
            min_interval_secs: 1,
            max_requests_per_minute: 60,
            calendar_ids: HashMap::new(),
            enabled: true,
            builtin: true,
            created_at: now,
            updated_at: now,
        },
        HolidayProvider {
            id: "calendarific".to_string(),
            name: "Calendarific".to_string(),
            kind: ProviderKind::Calendarific,
            base_url: "https://calendarific.com".to_string(),
            allowed_hosts: vec!["calendarific.com".to_string()],
            api_key_required: true,
            api_key_encrypted: None,
            key_fingerprint: None,
            min_interval_secs: 1,
            max_requests_per_minute: 60,
            calendar_ids: HashMap::new(),
            enabled: true,
            builtin: true,
            created_at: now,
            updated_at: now,
        },
        HolidayProvider {
            id: "abstract-api".to_string(),
            name: "Abstract API Holidays".to_string(),
            kind: ProviderKind::AbstractApi,
            base_url: "https://holidays.abstractapi.com/v1".to_string(),
            allowed_hosts: vec!["holidays.abstractapi.com".to_string()],
            api_key_required: true,
            api_key_encrypted: None,
            key_fingerprint: None,
            min_interval_secs: 1,
            max_requests_per_minute: 60,
            calendar_ids: HashMap::new(),
            enabled: true,
            builtin: true,
            created_at: now,
            updated_at: now,
        },
        HolidayProvider {
            id: "google-ical".to_string(),
            name: "Google Calendar Public Holidays".to_string(),
            kind: ProviderKind::GoogleIcal,
            base_url: "https://calendar.google.com".to_string(),
            allowed_hosts: vec!["calendar.google.com".to_string()],
            api_key_required: false,
            api_key_encrypted: None,
            key_fingerprint: None,
            min_interval_secs: 2,
            max_requests_per_minute: 30,
            calendar_ids: google_calendar_ids,
            enabled: true,
            builtin: true,
            created_at: now,
            updated_at: now,
        },
        HolidayProvider {
            id: "generic-ical".to_string(),
            name: "Generic iCal Feed".to_string(),
            kind: ProviderKind::GenericIcal,
            base_url: String::new(),
            allowed_hosts: Vec::new(),
            api_key_required: false,
            api_key_encrypted: None,
            key_fingerprint: None,
            min_interval_secs: 5,
            max_requests_per_minute: 12,
            calendar_ids: HashMap::new(),
            enabled: true,
            builtin: true,
            created_at: now,
            updated_at: now,
        },
    ]
}

/// ISO 3166-1 alpha-2 country list (code, English short name).
/// DATA: the full standard list, seeded as reference data. An admin
/// can add or disable entries through the countries endpoints; no
/// code change is needed to support a new country.
const ISO_COUNTRIES: &[(&str, &str)] = &[
    ("AD", "Andorra"), ("AE", "United Arab Emirates"), ("AF", "Afghanistan"),
    ("AG", "Antigua and Barbuda"), ("AI", "Anguilla"), ("AL", "Albania"),
    ("AM", "Armenia"), ("AO", "Angola"), ("AQ", "Antarctica"),
    ("AR", "Argentina"), ("AS", "American Samoa"), ("AT", "Austria"),
    ("AU", "Australia"), ("AW", "Aruba"), ("AX", "Aland Islands"),
    ("AZ", "Azerbaijan"), ("BA", "Bosnia and Herzegovina"), ("BB", "Barbados"),
    ("BD", "Bangladesh"), ("BE", "Belgium"), ("BF", "Burkina Faso"),
    ("BG", "Bulgaria"), ("BH", "Bahrain"), ("BI", "Burundi"),
    ("BJ", "Benin"), ("BL", "Saint Barthelemy"), ("BM", "Bermuda"),
    ("BN", "Brunei Darussalam"), ("BO", "Bolivia"),
    ("BQ", "Bonaire Sint Eustatius and Saba"), ("BR", "Brazil"),
    ("BS", "Bahamas"), ("BT", "Bhutan"), ("BV", "Bouvet Island"),
    ("BW", "Botswana"), ("BY", "Belarus"), ("BZ", "Belize"),
    ("CA", "Canada"), ("CC", "Cocos (Keeling) Islands"),
    ("CD", "Congo Democratic Republic of the"), ("CF", "Central African Republic"),
    ("CG", "Congo"), ("CH", "Switzerland"), ("CI", "Cote d'Ivoire"),
    ("CK", "Cook Islands"), ("CL", "Chile"), ("CM", "Cameroon"),
    ("CN", "China"), ("CO", "Colombia"), ("CR", "Costa Rica"),
    ("CU", "Cuba"), ("CV", "Cabo Verde"), ("CW", "Curacao"),
    ("CX", "Christmas Island"), ("CY", "Cyprus"), ("CZ", "Czechia"),
    ("DE", "Germany"), ("DJ", "Djibouti"), ("DK", "Denmark"),
    ("DM", "Dominica"), ("DO", "Dominican Republic"), ("DZ", "Algeria"),
    ("EC", "Ecuador"), ("EE", "Estonia"), ("EG", "Egypt"),
    ("EH", "Western Sahara"), ("ER", "Eritrea"), ("ES", "Spain"),
    ("ET", "Ethiopia"), ("FI", "Finland"), ("FJ", "Fiji"),
    ("FK", "Falkland Islands"), ("FM", "Micronesia"), ("FO", "Faroe Islands"),
    ("FR", "France"), ("GA", "Gabon"), ("GB", "United Kingdom"),
    ("GD", "Grenada"), ("GE", "Georgia"), ("GF", "French Guiana"),
    ("GG", "Guernsey"), ("GH", "Ghana"), ("GI", "Gibraltar"),
    ("GL", "Greenland"), ("GM", "Gambia"), ("GN", "Guinea"),
    ("GP", "Guadeloupe"), ("GQ", "Equatorial Guinea"), ("GR", "Greece"),
    ("GS", "South Georgia and the South Sandwich Islands"), ("GT", "Guatemala"),
    ("GU", "Guam"), ("GW", "Guinea-Bissau"), ("GY", "Guyana"),
    ("HK", "Hong Kong"), ("HM", "Heard Island and McDonald Islands"),
    ("HN", "Honduras"), ("HR", "Croatia"), ("HT", "Haiti"),
    ("HU", "Hungary"), ("ID", "Indonesia"), ("IE", "Ireland"),
    ("IL", "Israel"), ("IM", "Isle of Man"), ("IN", "India"),
    ("IO", "British Indian Ocean Territory"), ("IQ", "Iraq"), ("IR", "Iran"),
    ("IS", "Iceland"), ("IT", "Italy"), ("JE", "Jersey"),
    ("JM", "Jamaica"), ("JO", "Jordan"), ("JP", "Japan"),
    ("KE", "Kenya"), ("KG", "Kyrgyzstan"), ("KH", "Cambodia"),
    ("KI", "Kiribati"), ("KM", "Comoros"), ("KN", "Saint Kitts and Nevis"),
    ("KP", "Korea Democratic People's Republic of"), ("KR", "Korea Republic of"),
    ("KW", "Kuwait"), ("KY", "Cayman Islands"), ("KZ", "Kazakhstan"),
    ("LA", "Lao People's Democratic Republic"), ("LB", "Lebanon"),
    ("LC", "Saint Lucia"), ("LI", "Liechtenstein"), ("LK", "Sri Lanka"),
    ("LR", "Liberia"), ("LS", "Lesotho"), ("LT", "Lithuania"),
    ("LU", "Luxembourg"), ("LV", "Latvia"), ("LY", "Libya"),
    ("MA", "Morocco"), ("MC", "Monaco"), ("MD", "Moldova"),
    ("ME", "Montenegro"), ("MF", "Saint Martin (French part)"),
    ("MG", "Madagascar"), ("MH", "Marshall Islands"), ("MK", "North Macedonia"),
    ("ML", "Mali"), ("MM", "Myanmar"), ("MN", "Mongolia"),
    ("MO", "Macao"), ("MP", "Northern Mariana Islands"), ("MQ", "Martinique"),
    ("MR", "Mauritania"), ("MS", "Montserrat"), ("MT", "Malta"),
    ("MU", "Mauritius"), ("MV", "Maldives"), ("MW", "Malawi"),
    ("MX", "Mexico"), ("MY", "Malaysia"), ("MZ", "Mozambique"),
    ("NA", "Namibia"), ("NC", "New Caledonia"), ("NE", "Niger"),
    ("NF", "Norfolk Island"), ("NG", "Nigeria"), ("NI", "Nicaragua"),
    ("NL", "Netherlands"), ("NO", "Norway"), ("NP", "Nepal"),
    ("NR", "Nauru"), ("NU", "Niue"), ("NZ", "New Zealand"),
    ("OM", "Oman"), ("PA", "Panama"), ("PE", "Peru"),
    ("PF", "French Polynesia"), ("PG", "Papua New Guinea"), ("PH", "Philippines"),
    ("PK", "Pakistan"), ("PL", "Poland"), ("PM", "Saint Pierre and Miquelon"),
    ("PN", "Pitcairn"), ("PR", "Puerto Rico"), ("PS", "Palestine State of"),
    ("PT", "Portugal"), ("PW", "Palau"), ("PY", "Paraguay"),
    ("QA", "Qatar"), ("RE", "Reunion"), ("RO", "Romania"),
    ("RS", "Serbia"), ("RU", "Russian Federation"), ("RW", "Rwanda"),
    ("SA", "Saudi Arabia"), ("SB", "Solomon Islands"), ("SC", "Seychelles"),
    ("SD", "Sudan"), ("SE", "Sweden"), ("SG", "Singapore"),
    ("SH", "Saint Helena Ascension and Tristan da Cunha"), ("SI", "Slovenia"),
    ("SJ", "Svalbard and Jan Mayen"), ("SK", "Slovakia"), ("SL", "Sierra Leone"),
    ("SM", "San Marino"), ("SN", "Senegal"), ("SO", "Somalia"),
    ("SR", "Suriname"), ("SS", "South Sudan"), ("ST", "Sao Tome and Principe"),
    ("SV", "El Salvador"), ("SX", "Sint Maarten (Dutch part)"),
    ("SY", "Syrian Arab Republic"), ("SZ", "Eswatini"),
    ("TC", "Turks and Caicos Islands"), ("TD", "Chad"),
    ("TF", "French Southern Territories"), ("TG", "Togo"), ("TH", "Thailand"),
    ("TJ", "Tajikistan"), ("TK", "Tokelau"), ("TL", "Timor-Leste"),
    ("TM", "Turkmenistan"), ("TN", "Tunisia"), ("TO", "Tonga"),
    ("TR", "Turkiye"), ("TT", "Trinidad and Tobago"), ("TV", "Tuvalu"),
    ("TW", "Taiwan"), ("TZ", "Tanzania"), ("UA", "Ukraine"),
    ("UG", "Uganda"), ("UM", "United States Minor Outlying Islands"),
    ("US", "United States"), ("UY", "Uruguay"), ("UZ", "Uzbekistan"),
    ("VA", "Holy See"), ("VC", "Saint Vincent and the Grenadines"),
    ("VE", "Venezuela"), ("VG", "Virgin Islands British"), ("VI", "Virgin Islands U.S."),
    ("VN", "Viet Nam"), ("VU", "Vanuatu"), ("WF", "Wallis and Futuna"),
    ("WS", "Samoa"), ("YE", "Yemen"), ("YT", "Mayotte"),
    ("ZA", "South Africa"), ("ZM", "Zambia"), ("ZW", "Zimbabwe"),
];

// ---------------------------------------------------------------------------
// Validation helpers.
// ---------------------------------------------------------------------------

/// Normalize an ISO 3166-1 alpha-2 code: trim, upper-case, exactly
/// two ASCII letters.
fn normalize_country_code(raw: &str) -> UnifiedResult<String> {
    let code = raw.trim().to_ascii_uppercase();
    if code.len() != 2 || !code.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(format!(
            "Country code must be an ISO 3166-1 alpha-2 code such as \"ID\", got \"{raw}\""
        )));
    }
    Ok(code)
}

/// Parse a YYYY-MM-DD date.
fn parse_date(raw: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").map_err(|_| {
        UnifiedError::bad_request(format!("Date must be YYYY-MM-DD, got \"{raw}\""))
    })
}

/// Validate a provider id slug: lower-case letters, digits, hyphens.
fn validate_provider_id(raw: &str) -> UnifiedResult<String> {
    let id = raw.trim().to_ascii_lowercase();
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(UnifiedError::bad_request(
            "Provider id must be 1-64 characters of lower-case letters, digits, or hyphens",
        ));
    }
    Ok(id)
}

/// Validate a schedule expression: either an interval in seconds
/// (a plain number, 60..=2592000) or five cron-like fields where
/// each field is `*`, `*/N`, or a plain number. The scheduler owns
/// the real interpretation; this only rejects nonsense early.
fn validate_schedule(raw: &str) -> UnifiedResult<String> {
    let schedule = raw.trim().to_string();
    if schedule.is_empty() {
        return Err(UnifiedError::bad_request("Schedule must not be empty"));
    }
    if let Ok(seconds) = schedule.parse::<u64>() {
        if (60..=2_592_000).contains(&seconds) {
            return Ok(schedule);
        }
        return Err(UnifiedError::bad_request(
            "Interval schedule must be between 60 and 2592000 seconds",
        ));
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(UnifiedError::bad_request(
            "Schedule must be an interval in seconds or five cron-like fields (minute hour day month weekday)",
        ));
    }
    for field in fields {
        if field == "*" {
            continue;
        }
        if let Some(step) = field.strip_prefix("*/") {
            if step.parse::<u32>().is_ok_and(|value| value >= 1) {
                continue;
            }
            return Err(UnifiedError::bad_request(format!(
                "Invalid schedule step \"{field}\""
            )));
        }
        if field.parse::<u32>().is_err() {
            return Err(UnifiedError::bad_request(format!(
                "Invalid schedule field \"{field}\""
            )));
        }
    }
    Ok(schedule)
}

/// Validate a list of years: 1..=MAX_YEARS_PER_CONFIG entries, each
/// within MIN_SYNC_YEAR..=MAX_SYNC_YEAR, de-duplicated, sorted.
fn validate_years(years: &[i32]) -> UnifiedResult<Vec<i32>> {
    if years.is_empty() || years.len() > MAX_YEARS_PER_CONFIG {
        return Err(UnifiedError::bad_request(format!(
            "Years must list between 1 and {MAX_YEARS_PER_CONFIG} years"
        )));
    }
    let mut unique: Vec<i32> = Vec::new();
    for year in years {
        if !(MIN_SYNC_YEAR..=MAX_SYNC_YEAR).contains(year) {
            return Err(UnifiedError::bad_request(format!(
                "Year must be between {MIN_SYNC_YEAR} and {MAX_SYNC_YEAR}, got {year}"
            )));
        }
        if !unique.contains(year) {
            unique.push(*year);
        }
    }
    unique.sort_unstable();
    Ok(unique)
}

/// Default years for a new config: the current year and the next
/// DEFAULT_YEARS_AHEAD years.
fn default_years() -> Vec<i32> {
    let current = Utc::now().year();
    (current..=current + DEFAULT_YEARS_AHEAD).collect()
}

/// Validate a config's country list: non-empty, at most
/// MAX_COUNTRIES_PER_CONFIG, each a known, enabled country.
fn validate_config_countries(
    state: &HolidaysState,
    countries: &[String],
) -> UnifiedResult<Vec<String>> {
    if countries.is_empty() || countries.len() > MAX_COUNTRIES_PER_CONFIG {
        return Err(UnifiedError::bad_request(format!(
            "Countries must list between 1 and {MAX_COUNTRIES_PER_CONFIG} countries"
        )));
    }
    let mut unique: Vec<String> = Vec::new();
    for raw in countries {
        let code = normalize_country_code(raw)?;
        let Some(country) = state.countries.get(&code) else {
            return Err(UnifiedError::bad_request(format!(
                "Unknown country code \"{code}\""
            )));
        };
        if !country.enabled {
            return Err(UnifiedError::bad_request(format!(
                "Country \"{code}\" is disabled"
            )));
        }
        if !unique.contains(&code) {
            unique.push(code);
        }
    }
    unique.sort();
    Ok(unique)
}

/// Validate a provider reference on a config: the provider must
/// exist and be enabled.
fn load_enabled_provider(state: &HolidaysState, provider_id: &str) -> UnifiedResult<HolidayProvider> {
    let Some(provider) = state.providers.get(provider_id) else {
        return Err(UnifiedError::bad_request(format!(
            "Unknown provider \"{provider_id}\""
        )));
    };
    if !provider.enabled {
        return Err(UnifiedError::bad_request(format!(
            "Provider \"{provider_id}\" is disabled"
        )));
    }
    Ok(provider.clone())
}

/// Validate a fallback chain: at most MAX_FALLBACK_PROVIDERS ids,
/// each an existing enabled provider, distinct from the primary and
/// from each other.
fn validate_fallback_chain(
    state: &HolidaysState,
    primary_id: &str,
    fallback_ids: &[String],
) -> UnifiedResult<Vec<String>> {
    if fallback_ids.len() > MAX_FALLBACK_PROVIDERS {
        return Err(UnifiedError::bad_request(format!(
            "Fallback chain must list at most {MAX_FALLBACK_PROVIDERS} providers"
        )));
    }
    let mut unique: Vec<String> = Vec::new();
    for raw in fallback_ids {
        let id = raw.trim().to_string();
        if id == primary_id {
            return Err(UnifiedError::bad_request(
                "Fallback chain must not repeat the primary provider",
            ));
        }
        if unique.contains(&id) {
            return Err(UnifiedError::bad_request(format!(
                "Fallback chain repeats provider \"{id}\""
            )));
        }
        load_enabled_provider(state, &id)?;
        unique.push(id);
    }
    Ok(unique)
}

/// Emit a failure alert through `notification.requested`. The payload
/// never contains a key, a URL, or a response body — only ids, the
/// country/year, and the short failure reason.
fn notify_sync_failure(
    bus: &HookBus,
    tenant_id: &str,
    config_id: Uuid,
    summary: &str,
    failures: &[String],
) {
    let body = if failures.is_empty() {
        summary.to_string()
    } else {
        format!("{summary}: {}", failures.join("; "))
    };
    bus.emit(
        "notification.requested",
        json!({
            "tenant_id": tenant_id,
            "title": "Holiday sync failed",
            "body": body,
            "kind": "sync_failed",
            "category": "holidays",
            "module": "holidays",
            "priority": "high",
            "link": "/api/v1/holidays/sync-runs",
            "config_id": config_id.to_string(),
        }),
    );
}

/// Enqueue a scheduler job through `scheduler.enqueue` and return
/// the scheduler outcome honestly: "queued" when the scheduler
/// accepted the kind, "declined" otherwise (today the
/// `holidays.sync` / `holidays.sync_all` kinds are not in the
/// scheduler's `KNOWN_KINDS`, so the answer is "declined" — same
/// shape as the todos / events / calendar modules record).
fn enqueue_scheduler_job(bus: &HookBus, kind: &str, payload: Value) -> String {
    let answer = bus.emit("scheduler.enqueue", json!({ "kind": kind, "payload": payload }));
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            "queued".to_string()
        }
        _ => "declined".to_string(),
    }
}

// ---------------------------------------------------------------------------
// iCal parsing (hand-rolled: no ical crate is available).
// ---------------------------------------------------------------------------

/// One parsed VEVENT: the date the holiday falls on and its name.
#[derive(Debug, Clone)]
struct IcsEvent {
    date: NaiveDate,
    summary: String,
}

/// Unfold iCal lines: a line starting with a space or tab continues
/// the previous line (RFC 5545 section 3.1).
fn unfold_ics_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if (line.starts_with(' ') || line.starts_with('\t')) && !out.is_empty() {
            let previous = out.pop().expect("unfold checked non-empty");
            out.push(format!("{previous}{}", line[1..].to_string()));
        } else {
            out.push(line.to_string());
        }
    }
    out
}

/// Unescape an iCal TEXT value: `\\` -> `\`, `\n` / `\N` -> newline,
/// `\,` -> `,`, `\;` -> `;`.
fn ics_unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(char) = chars.next() {
        if char != '\\' {
            out.push(char);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Parse an iCal date value: `YYYYMMDD`, `YYYYMMDDTHHMMSSZ`, or
/// `YYYYMMDDTHHMMSS` (a floating local time — the date part is what
/// a holiday needs). Returns None when the first 8 digits are not a
/// valid calendar date.
fn parse_ics_date(value: &str) -> Option<NaiveDate> {
    let digits: String = value.chars().take(8).collect();
    if digits.len() != 8 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    NaiveDate::parse_from_str(&digits, "%Y%m%d").ok()
}

/// Split one unfolded iCal content line into (name, value): the name
/// is the part before the first `;` or `:`, the value is the part
/// after the first `:`.
fn split_ics_line(line: &str) -> Option<(String, String)> {
    let colon = line.find(':')?;
    let name_part = &line[..colon];
    let name = name_part.split(';').next().unwrap_or("").to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    Some((name, line[colon + 1..].to_string()))
}

/// Parse an iCal feed into its VEVENTs. Events without a parseable
/// DTSTART or without a SUMMARY are skipped (counted by the caller
/// as found-but-unusable through the returned skip count); a feed
/// with no VEVENT at all parses to an empty list, which the sync
/// records honestly as zero holidays found.
fn parse_ics(text: &str) -> (Vec<IcsEvent>, usize) {
    let mut events: Vec<IcsEvent> = Vec::new();
    let mut skipped: usize = 0;
    let mut in_event = false;
    let mut current_date: Option<NaiveDate> = None;
    let mut current_summary: Option<String> = None;
    for line in unfold_ics_lines(text) {
        let Some((name, value)) = split_ics_line(&line) else {
            continue;
        };
        match name.as_str() {
            "BEGIN" if value.eq_ignore_ascii_case("VEVENT") => {
                in_event = true;
                current_date = None;
                current_summary = None;
            }
            "END" if value.eq_ignore_ascii_case("VEVENT") => {
                if in_event {
                    match (current_date, current_summary.take()) {
                        (Some(date), Some(summary)) if !summary.trim().is_empty() => {
                            events.push(IcsEvent {
                                date,
                                summary: ics_unescape(summary.trim()),
                            });
                        }
                        _ => skipped += 1,
                    }
                }
                in_event = false;
                current_date = None;
                current_summary = None;
            }
            "DTSTART" if in_event => {
                if current_date.is_none() {
                    current_date = parse_ics_date(value.trim());
                }
            }
            "SUMMARY" if in_event => {
                if current_summary.is_none() {
                    current_summary = Some(value);
                }
            }
            _ => {}
        }
    }
    (events, skipped)
}

// ---------------------------------------------------------------------------
// Provider response parsing (shapes from provider documentation, NOT
// live-verified — see the module header).
// ---------------------------------------------------------------------------

fn text_field(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn string_list(value: &Value, field: &str) -> Vec<String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a Nager.Date response body: a JSON array of
/// {"date", "localName", "name", "countryCode", "fixed", "global",
/// "counties", "types"}.
fn parse_nager_response(body: &str, country: &str) -> UnifiedResult<Vec<FetchedHoliday>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| UnifiedError::internal("Nager.Date returned an unreadable response"))?;
    let items = value
        .as_array()
        .ok_or_else(|| UnifiedError::internal("Nager.Date response was not a JSON array"))?;
    let mut out = Vec::new();
    for item in items {
        let Some(date_text) = text_field(item, "date") else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(&date_text, "%Y-%m-%d") else {
            continue;
        };
        let name = text_field(item, "localName")
            .or_else(|| text_field(item, "name"))
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        out.push(FetchedHoliday {
            date,
            name,
            local_name: text_field(item, "name"),
            global: item.get("global").and_then(Value::as_bool).unwrap_or(true),
            types: string_list(item, "types"),
            counties: string_list(item, "counties"),
        });
    }
    let _ = country;
    Ok(out)
}

/// Parse a Calendarific response body:
/// {"response": {"holidays": [{"name", "description",
/// "date": {"iso": "2026-01-01"}, "type": [...] }]}}.
fn parse_calendarific_response(body: &str) -> UnifiedResult<Vec<FetchedHoliday>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| UnifiedError::internal("Calendarific returned an unreadable response"))?;
    let holidays = value
        .get("response")
        .and_then(|response| response.get("holidays"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            UnifiedError::internal("Calendarific response had no response.holidays array")
        })?;
    let mut out = Vec::new();
    for item in holidays {
        let date_text = item
            .get("date")
            .and_then(|date| date.get("iso"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| text_field(item, "date"));
        let Some(date_text) = date_text else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(date_text.trim(), "%Y-%m-%d") else {
            continue;
        };
        let Some(name) = text_field(item, "name") else {
            continue;
        };
        out.push(FetchedHoliday {
            date,
            name,
            local_name: text_field(item, "description"),
            global: true,
            types: string_list(item, "type"),
            counties: Vec::new(),
        });
    }
    Ok(out)
}

/// Parse an Abstract API response body: a JSON array of
/// {"date": "2026-01-01", "name": ...} (also accepts
/// {"holidays": [...]} and a nested {"date": {"iso": ...}} date).
fn parse_abstract_response(body: &str) -> UnifiedResult<Vec<FetchedHoliday>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| UnifiedError::internal("Abstract API returned an unreadable response"))?;
    let items: &Vec<Value> = match value.as_array() {
        Some(items) => items,
        None => value
            .get("holidays")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                UnifiedError::internal("Abstract API response was not a JSON array")
            })?,
    };
    let mut out = Vec::new();
    for item in items {
        let date_text = text_field(item, "date").or_else(|| {
            item.get("date")
                .and_then(|date| date.get("iso"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let Some(date_text) = date_text else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(date_text.trim(), "%Y-%m-%d") else {
            continue;
        };
        let Some(name) = text_field(item, "name") else {
            continue;
        };
        out.push(FetchedHoliday {
            date,
            name,
            local_name: None,
            global: true,
            types: string_list(item, "type"),
            counties: Vec::new(),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Fetching: rate limits, retries, SSRF-safe client.
// ---------------------------------------------------------------------------

/// Wait until the provider's rate limits allow one more request.
/// Returns an error when the wait would exceed
/// MAX_RATE_LIMIT_WAIT instead of sleeping indefinitely.
async fn wait_for_rate_limit(state: &HolidaysState, provider: &HolidayProvider) -> UnifiedResult<()> {
    let entry = state
        .rate_state
        .entry(provider.id.clone())
        .or_insert_with(|| Arc::new(Mutex::new(RateState::default())));
    let guard = entry.value().clone();
    let wait = {
        let mut rate = guard.lock().expect("rate limit lock poisoned");
        let now = Instant::now();
        let mut wait = Duration::from_secs(0);
        if provider.min_interval_secs > 0 {
            if let Some(last) = rate.last_request {
                let gap = Duration::from_secs(provider.min_interval_secs);
                if let Some(elapsed) = now.checked_duration_since(last) {
                    if elapsed < gap {
                        wait = wait.max(gap - elapsed);
                    }
                }
            }
        }
        let window_start = now.checked_sub(Duration::from_secs(60)).unwrap_or(now);
        rate.window.retain(|instant| *instant >= window_start);
        if rate.window.len() as u32 >= provider.max_requests_per_minute {
            if let Some(oldest) = rate.window.front() {
                let until = *oldest + Duration::from_secs(60);
                if until > now {
                    wait = wait.max(until - now);
                }
            }
        }
        if wait > MAX_RATE_LIMIT_WAIT {
            return Err(UnifiedError::internal(format!(
                "Rate limit wait for provider \"{}\" would exceed {} seconds; try again later",
                provider.id,
                MAX_RATE_LIMIT_WAIT.as_secs()
            )));
        }
        // Reserve the slot now, so concurrent fetches serialize
        // instead of all sleeping the same short wait and then
        // firing together.
        rate.last_request = Some(now + wait);
        rate.window.push_back(now + wait);
        wait
    };
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
    Ok(())
}

/// Parse a `Retry-After` header value: seconds, or nothing (HTTP
/// dates are not parsed — no httpdate crate — and fall back to the
/// exponential backoff). Capped at MAX_RETRY_AFTER.
fn parse_retry_after(value: &str) -> Option<Duration> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|seconds| Duration::from_secs(seconds).min(MAX_RETRY_AFTER))
}

/// One HTTP GET with retries: 429 honours `Retry-After`, 408 / 5xx
/// and transport errors retry with exponential backoff, other 4xx
/// fail fast. The returned body is capped at MAX_RESPONSE_BYTES.
/// The URL is never logged and never appears in an error message
/// (it may carry an API key as a query parameter).
async fn fetch_with_retries(
    state: &HolidaysState,
    provider: &HolidayProvider,
    url: &str,
) -> UnifiedResult<Vec<u8>> {
    let allowed_hosts: Arc<Vec<String>> = Arc::new(provider.allowed_hosts.clone());
    let client = build_fetch_client(allowed_hosts)?;
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        wait_for_rate_limit(state, provider).await?;
        let response = client.get(url).send().await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                if attempt < MAX_FETCH_ATTEMPTS {
                    let backoff = RETRY_BASE_DELAY
                        .mul_f64(2_f64.powi((attempt - 1) as i32))
                        .min(RETRY_MAX_DELAY);
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                return Err(UnifiedError::internal(format!(
                    "Provider \"{}\" request failed: {error}",
                    provider.id
                )));
            }
        };
        let status = response.status();
        if status.is_success() {
            let bytes = response
                .bytes()
                .await
                .map_err(|error| {
                    UnifiedError::internal(format!(
                        "Provider \"{}\" response could not be read: {error}",
                        provider.id
                    ))
                })?
                .to_vec();
            if bytes.len() > MAX_RESPONSE_BYTES {
                return Err(UnifiedError::internal(format!(
                    "Provider \"{}\" response exceeded the {} byte limit",
                    provider.id, MAX_RESPONSE_BYTES
                )));
            }
            return Ok(bytes);
        }
        if status.as_u16() == 429 {
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_retry_after);
            if attempt < MAX_FETCH_ATTEMPTS {
                let backoff = retry_after.unwrap_or_else(|| {
                    RETRY_BASE_DELAY
                        .mul_f64(2_f64.powi((attempt - 1) as i32))
                        .min(RETRY_MAX_DELAY)
                });
                tokio::time::sleep(backoff).await;
                continue;
            }
            return Err(UnifiedError::internal(format!(
                "Provider \"{}\" rate-limited the request (429)",
                provider.id
            )));
        }
        if status.as_u16() == 408 || status.is_server_error() {
            if attempt < MAX_FETCH_ATTEMPTS {
                let backoff = RETRY_BASE_DELAY
                    .mul_f64(2_f64.powi((attempt - 1) as i32))
                    .min(RETRY_MAX_DELAY);
                tokio::time::sleep(backoff).await;
                continue;
            }
            return Err(UnifiedError::internal(format!(
                "Provider \"{}\" answered with status {status}",
                provider.id
            )));
        }
        return Err(UnifiedError::internal(format!(
            "Provider \"{}\" answered with status {status}",
            provider.id
        )));
    }
}

/// Build the fetch URL for one (provider, country, year) and the
/// allowlist that fetch must satisfy. For `generic_ical` the URL is
/// the config's `feed_url` and the allowlist is that URL's single
/// host; for every other kind the URL is built from the provider's
/// `base_url` and the allowlist is the provider definition's. The
/// API key (when the kind needs one) is decrypted here and appended
/// as a query parameter over https only; it is never logged.
fn build_fetch_url(
    provider: &HolidayProvider,
    config: &HolidaySyncConfig,
    country: &str,
    year: i32,
    secret: &str,
) -> UnifiedResult<(String, Vec<String>)> {
    match provider.kind {
        ProviderKind::NagerDate => {
            let base = provider.base_url.trim_end_matches('/');
            Ok((
                format!("{base}/api/v3/publicholidays/{year}/{country}"),
                provider.allowed_hosts.clone(),
            ))
        }
        ProviderKind::Calendarific => {
            let key = provider_key(provider, secret)?;
            let base = provider.base_url.trim_end_matches('/');
            let mut url = url::Url::parse(&format!("{base}/api/v2/holidays"))
                .map_err(|_| UnifiedError::internal("Calendarific base URL is invalid"))?;
            url.query_pairs_mut()
                .append_pair("api_key", &key)
                .append_pair("country", country)
                .append_pair("year", &year.to_string());
            Ok((url.to_string(), provider.allowed_hosts.clone()))
        }
        ProviderKind::AbstractApi => {
            let key = provider_key(provider, secret)?;
            let base = provider.base_url.trim_end_matches('/');
            let mut url = url::Url::parse(base)
                .map_err(|_| UnifiedError::internal("Abstract API base URL is invalid"))?;
            url.query_pairs_mut()
                .append_pair("api_key", &key)
                .append_pair("country", country)
                .append_pair("year", &year.to_string());
            Ok((url.to_string(), provider.allowed_hosts.clone()))
        }
        ProviderKind::GoogleIcal => {
            let Some(calendar_id) = provider.calendar_ids.get(country) else {
                return Err(UnifiedError::bad_request(format!(
                    "Provider \"google-ical\" has no public calendar id configured for country \"{country}\""
                )));
            };
            let base = provider.base_url.trim_end_matches('/');
            Ok((
                format!("{base}/calendar/ical/{calendar_id}/public/basic.ics"),
                provider.allowed_hosts.clone(),
            ))
        }
        ProviderKind::GenericIcal => {
            let Some(feed_url) = config.feed_url.as_deref() else {
                return Err(UnifiedError::bad_request(
                    "Generic iCal provider needs a feed_url on the sync config",
                ));
            };
            let parsed = url::Url::parse(feed_url.trim())
                .map_err(|_| UnifiedError::bad_request("Sync config feed_url is invalid"))?;
            let Some(host) = parsed.host_str() else {
                return Err(UnifiedError::bad_request(
                    "Sync config feed_url must have a host",
                ));
            };
            Ok((feed_url.trim().to_string(), vec![host.to_string()]))
        }
    }
}

/// Decrypt the stored key for a keyed provider. Fails closed when
/// the provider needs a key and none is stored.
fn provider_key(provider: &HolidayProvider, secret: &str) -> UnifiedResult<String> {
    if !provider.kind.key_required() {
        return Ok(String::new());
    }
    let Some(stored) = provider.api_key_encrypted.as_deref() else {
        return Err(UnifiedError::bad_request(format!(
            "Provider \"{}\" needs an API key and none is configured",
            provider.id
        )));
    };
    decrypt_api_key(secret, stored)
}

/// Fetch and parse the holidays for one (provider, country, year).
/// The URL is validated with the SSRF rules (https only, allowlist
/// host, safe DNS) before the fetch, and every redirect target is
/// re-validated by the fetch client.
async fn fetch_holidays_for_provider(
    state: &HolidaysState,
    provider: &HolidayProvider,
    config: &HolidaySyncConfig,
    country: &str,
    year: i32,
    secret: &str,
) -> UnifiedResult<Vec<FetchedHoliday>> {
    let (url, allowed_hosts) = build_fetch_url(provider, config, country, year, secret)?;
    validate_provider_url(&url, &allowed_hosts)?;
    let bytes = fetch_with_retries(state, provider, &url).await?;
    match provider.kind {
        ProviderKind::NagerDate => {
            let text = String::from_utf8(bytes).map_err(|_| {
                UnifiedError::internal("Nager.Date response was not valid UTF-8")
            })?;
            parse_nager_response(&text, country)
        }
        ProviderKind::Calendarific => {
            let text = String::from_utf8(bytes).map_err(|_| {
                UnifiedError::internal("Calendarific response was not valid UTF-8")
            })?;
            parse_calendarific_response(&text)
        }
        ProviderKind::AbstractApi => {
            let text = String::from_utf8(bytes).map_err(|_| {
                UnifiedError::internal("Abstract API response was not valid UTF-8")
            })?;
            parse_abstract_response(&text)
        }
        ProviderKind::GoogleIcal | ProviderKind::GenericIcal => {
            let text = String::from_utf8(bytes).map_err(|_| {
                UnifiedError::internal("iCal feed was not valid UTF-8")
            })?;
            let (events, _skipped) = parse_ics(&text);
            Ok(events
                .into_iter()
                .map(|event| FetchedHoliday {
                    date: event.date,
                    name: event.summary,
                    local_name: None,
                    global: true,
                    types: Vec::new(),
                    counties: Vec::new(),
                })
                .collect())
        }
    }
}

// ---------------------------------------------------------------------------
// Sync engine.
// ---------------------------------------------------------------------------

/// Upsert fetched holidays for one (country, year) into the shared
/// store, deduplicated by (country, date, name). Returns how many
/// rows were inserted or updated. A re-sync never duplicates.
fn upsert_holidays(
    state: &HolidaysState,
    provider_id: &str,
    country: &str,
    fetched: &[FetchedHoliday],
) -> i64 {
    let now = Utc::now();
    let mut stored: i64 = 0;
    for item in fetched {
        if item.name.trim().is_empty() {
            continue;
        }
        let key = format!("{}|{}|{}", country, item.date, item.name.trim());
        let name = item.name.trim().to_string();
        match state.holidays.get_mut(&key) {
            Some(mut existing) => {
                existing.local_name = item.local_name.clone().or_else(|| existing.local_name.clone());
                existing.provider_id = provider_id.to_string();
                existing.source = "synced".to_string();
                existing.global = item.global;
                existing.types = item.types.clone();
                existing.counties = item.counties.clone();
                existing.updated_at = now;
                stored += 1;
            }
            None => {
                state.holidays.insert(
                    key,
                    Holiday {
                        id: Uuid::new_v4(),
                        country: country.to_string(),
                        date: item.date,
                        name,
                        local_name: item.local_name.clone(),
                        provider_id: provider_id.to_string(),
                        source: "synced".to_string(),
                        global: item.global,
                        types: item.types.clone(),
                        counties: item.counties.clone(),
                        created_at: now,
                        updated_at: now,
                    },
                );
                stored += 1;
            }
        }
    }
    stored
}

/// Sync one (country, year) for one config, trying the primary
/// provider first and then the fallback chain in order. Records one
/// run row per provider attempt and returns the successful
/// provider's (found, stored) counts, or the last failure.
async fn sync_country_year(
    state: &HolidaysState,
    bus: &HookBus,
    secret: &str,
    config: &HolidaySyncConfig,
    country: &str,
    year: i32,
    trigger: &str,
) -> Result<(String, i64, i64), (String, String)> {
    let mut chain: Vec<String> = vec![config.provider_id.clone()];
    chain.extend(config.fallback_provider_ids.iter().cloned());
    let mut last_error: Option<(String, String)> = None;
    for provider_id in chain {
        let Some(provider) = state.providers.get(&provider_id).map(|entry| entry.clone()) else {
            let started = Utc::now();
            let run_id = Uuid::new_v4();
            state.runs.insert(
                run_id,
                HolidaySyncRun {
                    id: run_id,
                    tenant_id: config.tenant_id.clone(),
                    company_id: config.company_id.clone(),
                    config_id: config.id,
                    provider_id: provider_id.clone(),
                    country: country.to_string(),
                    year,
                    status: "failed".to_string(),
                    trigger: trigger.to_string(),
                    holidays_found: 0,
                    holidays_stored: 0,
                    error: Some(format!("Provider \"{provider_id}\" no longer exists")),
                    started_at: started,
                    finished_at: started,
                    fetched_at: started,
                },
            );
            last_error = Some((
                provider_id.clone(),
                format!("Provider \"{provider_id}\" no longer exists"),
            ));
            continue;
        };
        if !provider.enabled {
            let started = Utc::now();
            let run_id = Uuid::new_v4();
            state.runs.insert(
                run_id,
                HolidaySyncRun {
                    id: run_id,
                    tenant_id: config.tenant_id.clone(),
                    company_id: config.company_id.clone(),
                    config_id: config.id,
                    provider_id: provider_id.clone(),
                    country: country.to_string(),
                    year,
                    status: "failed".to_string(),
                    trigger: trigger.to_string(),
                    holidays_found: 0,
                    holidays_stored: 0,
                    error: Some(format!("Provider \"{provider_id}\" is disabled")),
                    started_at: started,
                    finished_at: started,
                    fetched_at: started,
                },
            );
            last_error = Some((
                provider_id.clone(),
                format!("Provider \"{provider_id}\" is disabled"),
            ));
            continue;
        }
        let started = Utc::now();
        let outcome = fetch_holidays_for_provider(state, &provider, config, country, year, secret).await;
        let finished = Utc::now();
        match outcome {
            Ok(fetched) => {
                let found = fetched.len() as i64;
                let stored = upsert_holidays(state, &provider.id, country, &fetched);
                let run_id = Uuid::new_v4();
                state.runs.insert(
                    run_id,
                    HolidaySyncRun {
                        id: run_id,
                        tenant_id: config.tenant_id.clone(),
                        company_id: config.company_id.clone(),
                        config_id: config.id,
                        provider_id: provider.id.clone(),
                        country: country.to_string(),
                        year,
                        status: "success".to_string(),
                        trigger: trigger.to_string(),
                        holidays_found: found,
                        holidays_stored: stored,
                        error: None,
                        started_at: started,
                        finished_at: finished,
                        fetched_at: finished,
                    },
                );
                return Ok((provider.id.clone(), found, stored));
            }
            Err(error) => {
                let message = error_message_without_secrets(&error);
                let run_id = Uuid::new_v4();
                state.runs.insert(
                    run_id,
                    HolidaySyncRun {
                        id: run_id,
                        tenant_id: config.tenant_id.clone(),
                        company_id: config.company_id.clone(),
                        config_id: config.id,
                        provider_id: provider.id.clone(),
                        country: country.to_string(),
                        year,
                        status: "failed".to_string(),
                        trigger: trigger.to_string(),
                        holidays_found: 0,
                        holidays_stored: 0,
                        error: Some(message.clone()),
                        started_at: started,
                        finished_at: finished,
                        fetched_at: finished,
                    },
                );
                last_error = Some((provider.id.clone(), message));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| ("unknown".to_string(), "Sync failed for an unknown reason".to_string())))
}

/// The error message of a sync failure, with any accidental secret
/// material stripped: messages built in this module never contain a
/// key or a URL, but defence in depth truncates anything suspiciously
/// long instead of storing it verbatim.
fn error_message_without_secrets(error: &UnifiedError) -> String {
    let message = error.to_string();
    if message.len() > 500 {
        message.chars().take(500).collect()
    } else {
        message
    }
}

/// Run a full sync for one config: every selected country crossed
/// with every selected year. Returns a summary; failures are
/// collected and alerted once per config (never silently empty).
/// Previously synced holidays stay cached when a sync fails.
async fn run_sync_for_config(
    state: &HolidaysState,
    bus: &HookBus,
    secret: &str,
    config: &HolidaySyncConfig,
    trigger: &str,
) -> SyncSummary {
    bus.emit(
        EVENT_SYNC_STARTED,
        json!({
            "config_id": config.id,
            "tenant_id": config.tenant_id,
            "trigger": trigger,
        }),
    );
    let mut runs: Vec<Value> = Vec::new();
    let mut attempted: usize = 0;
    let mut succeeded: usize = 0;
    let mut failed: usize = 0;
    let mut holidays_found: i64 = 0;
    let mut holidays_stored: i64 = 0;
    let mut failures: Vec<String> = Vec::new();
    for country in &config.countries {
        for year in &config.years {
            attempted += 1;
            match sync_country_year(state, bus, secret, config, country, *year, trigger).await {
                Ok((provider_id, found, stored)) => {
                    succeeded += 1;
                    holidays_found += found;
                    holidays_stored += stored;
                    runs.push(json!({
                        "country": country,
                        "year": year,
                        "provider_id": provider_id,
                        "status": "success",
                        "holidays_found": found,
                        "holidays_stored": stored,
                    }));
                }
                Err((provider_id, message)) => {
                    failed += 1;
                    failures.push(format!("{country}/{year} via {provider_id}: {message}"));
                    runs.push(json!({
                        "country": country,
                        "year": year,
                        "provider_id": provider_id,
                        "status": "failed",
                        "error": message,
                    }));
                }
            }
        }
    }
    let status = if failed == 0 {
        "finished"
    } else if succeeded == 0 {
        "failed"
    } else {
        "partial"
    };
    if failed > 0 {
        notify_sync_failure(
            bus,
            &config.tenant_id,
            config.id,
            &format!(
                "Holiday sync for config {} finished with status {status} ({succeeded}/{attempted} country-years synced)",
                config.id
            ),
            &failures,
        );
        bus.emit(
            EVENT_SYNC_FAILED,
            json!({
                "config_id": config.id,
                "tenant_id": config.tenant_id,
                "trigger": trigger,
                "status": status,
                "failures": failures,
            }),
        );
    }
    bus.emit(
        EVENT_SYNC_FINISHED,
        json!({
            "config_id": config.id,
            "tenant_id": config.tenant_id,
            "trigger": trigger,
            "status": status,
            "attempted": attempted,
            "succeeded": succeeded,
            "failed": failed,
            "holidays_found": holidays_found,
            "holidays_stored": holidays_stored,
        }),
    );
    SyncSummary {
        config_id: config.id,
        tenant_id: config.tenant_id.clone(),
        trigger: trigger.to_string(),
        runs,
        attempted,
        succeeded,
        failed,
        holidays_found,
        holidays_stored,
        scheduler_status: "manual".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Hook handlers.
// ---------------------------------------------------------------------------

/// Answer one `holidays.is_holiday` request. The request MUST carry
/// `tenant_id`: without it this handler adds no result (fail closed
/// — the caller falls back to weekends only, as the calendar module
/// documents). The answer is scoped to the tenant's enabled configs:
/// a country the tenant never selected answers
/// `source: "not_configured"`. A manual override wins over a synced
/// holiday on the same date.
fn on_is_holiday_requested(state: Arc<HolidaysState>, ctx: &mut HookContext) {
    let Some(date_text) = ctx.get("date").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(country_raw) = ctx.get("country").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(tenant_id) = ctx
        .get("tenant_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    // The tenant id must be a valid UUID (the tenancy schema name
    // derives from it): an unparseable id gets no answer, fail closed.
    if schema_name_for_str(&tenant_id).is_err() {
        return;
    }
    let Ok(date) = NaiveDate::parse_from_str(date_text.trim(), "%Y-%m-%d") else {
        return;
    };
    let Ok(country) = normalize_country_code(&country_raw) else {
        return;
    };
    let answer = answer_is_holiday(&state, &tenant_id, &country, date);
    ctx.add_result(answer);
}

/// Shared single-day answer logic for the hook and the HTTP endpoint.
fn answer_is_holiday(
    state: &HolidaysState,
    tenant_id: &str,
    country: &str,
    date: NaiveDate,
) -> Value {
    let selected = state.tenant_countries(tenant_id);
    if !selected.contains(country) {
        return json!({
            "date": date.to_string(),
            "country": country,
            "is_holiday": false,
            "name": Value::Null,
            "source": "not_configured",
            "provider": Value::Null,
        });
    }
    if let Some(override_entry) = state.tenant_override(tenant_id, country, date) {
        if override_entry.override_kind == "working_day" {
            return json!({
                "date": date.to_string(),
                "country": country,
                "is_holiday": false,
                "name": Value::Null,
                "source": "manual",
                "provider": "manual",
            });
        }
        return json!({
            "date": date.to_string(),
            "country": country,
            "is_holiday": true,
            "name": override_entry.name,
            "source": "manual",
            "provider": "manual",
        });
    }
    if let Some(holiday) = state.synced_on(country, date) {
        return json!({
            "date": date.to_string(),
            "country": country,
            "is_holiday": true,
            "name": holiday.name,
            "source": "synced",
            "provider": holiday.provider_id,
        });
    }
    json!({
        "date": date.to_string(),
        "country": country,
        "is_holiday": false,
        "name": Value::Null,
        "source": "synced",
        "provider": Value::Null,
    })
}

/// Answer one `holidays.non_working_days` request. Same tenant
/// scoping as `on_is_holiday_requested`: no `tenant_id` means no
/// result. The range is clamped to MAX_HOOK_RANGE_DAYS. Overrides
/// win over synced holidays on the same date; a `working_day`
/// override removes that date from the answer.
fn on_non_working_days_requested(state: Arc<HolidaysState>, ctx: &mut HookContext) {
    let Some(start_text) = ctx.get("start").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(end_text) = ctx.get("end").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(country_raw) = ctx.get("country").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(tenant_id) = ctx
        .get("tenant_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    // The tenant id must be a valid UUID (the tenancy schema name
    // derives from it): an unparseable id gets no answer, fail closed.
    if schema_name_for_str(&tenant_id).is_err() {
        return;
    }
    let Ok(start) = NaiveDate::parse_from_str(start_text.trim(), "%Y-%m-%d") else {
        return;
    };
    let Ok(end) = NaiveDate::parse_from_str(end_text.trim(), "%Y-%m-%d") else {
        return;
    };
    let Ok(country) = normalize_country_code(&country_raw) else {
        return;
    };
    let (start, end) = if end < start { (end, start) } else { (start, end) };
    let span = end.signed_duration_since(start).num_days();
    let end = if span > MAX_HOOK_RANGE_DAYS {
        start + chrono::Duration::days(MAX_HOOK_RANGE_DAYS)
    } else {
        end
    };
    let selected = state.tenant_countries(&tenant_id);
    if !selected.contains(&country) {
        ctx.add_result(json!({
            "start": start.to_string(),
            "end": end.to_string(),
            "country": country,
            "in_scope": false,
            "dates": [],
        }));
        return;
    }
    let mut dates: Vec<Value> = Vec::new();
    let mut day = start;
    while day <= end {
        if let Some(override_entry) = state.tenant_override(&tenant_id, &country, day) {
            if override_entry.override_kind != "working_day" {
                dates.push(json!({
                    "date": day.to_string(),
                    "name": override_entry.name,
                    "source": "manual",
                    "provider": "manual",
                }));
            }
        } else {
            let mut day_holidays: Vec<Holiday> = state
                .holidays
                .iter()
                .filter(|entry| entry.country == country && entry.date == day)
                .map(|entry| entry.clone())
                .collect();
            day_holidays.sort_by(|a, b| a.name.cmp(&b.name));
            for holiday in day_holidays {
                dates.push(json!({
                    "date": day.to_string(),
                    "name": holiday.name,
                    "source": "synced",
                    "provider": holiday.provider_id,
                }));
            }
        }
        day += chrono::Duration::days(1);
    }
    ctx.add_result(json!({
        "start": start.to_string(),
        "end": end.to_string(),
        "country": country,
        "in_scope": true,
        "dates": dates,
    }));
}

/// Scheduler listener for `holidays.sync`: payload
/// {"config_id", "tenant_id"}. Runs the sync in a spawned task so
/// the synchronous hook bus never blocks on network I/O. When no
/// async runtime is available, the request is recorded as a failed
/// run instead of being silently dropped. A config whose tenant id
/// does not match the payload is ignored (no run, no leak).
fn on_sync_requested(state: Arc<HolidaysState>, ctx: &mut HookContext) {
    let Some(config_id_text) = ctx.get("config_id").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(tenant_id) = ctx
        .get("tenant_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    // The tenant id must be a valid UUID (the tenancy schema name
    // derives from it): an unparseable id gets no answer, fail closed.
    if schema_name_for_str(&tenant_id).is_err() {
        return;
    }
    let Ok(config_id) = Uuid::parse_str(&config_id_text) else {
        return;
    };
    let Some(config) = state.configs.get(&config_id).map(|entry| entry.clone()) else {
        return;
    };
    if config.tenant_id != tenant_id || !config.enabled {
        return;
    }
    let secret = state.secret();
    let Some(bus) = state.bus() else {
        return;
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            let task_state = state.clone();
            let task_bus = bus.clone();
            handle.spawn(async move {
                run_sync_for_config(&task_state, &task_bus, &secret, &config, "scheduled").await;
            });
        }
        Err(_) => {
            let now = Utc::now();
            let run_id = Uuid::new_v4();
            state.runs.insert(
                run_id,
                HolidaySyncRun {
                    id: run_id,
                    tenant_id: config.tenant_id.clone(),
                    company_id: config.company_id.clone(),
                    config_id: config.id,
                    provider_id: config.provider_id.clone(),
                    country: config.countries.first().cloned().unwrap_or_default(),
                    year: config.years.first().copied().unwrap_or(0),
                    status: "failed".to_string(),
                    trigger: "scheduled".to_string(),
                    holidays_found: 0,
                    holidays_stored: 0,
                    error: Some("No async runtime available to run the scheduled sync".to_string()),
                    started_at: now,
                    finished_at: now,
                    fetched_at: now,
                },
            );
        }
    }
}

/// Scheduler listener for `holidays.sync_all`: payload
/// {"tenant_id"}. Syncs every enabled config of that tenant, each
/// in its own spawned task.
fn on_sync_all_requested(state: Arc<HolidaysState>, ctx: &mut HookContext) {
    let Some(tenant_id) = ctx
        .get("tenant_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    // The tenant id must be a valid UUID (the tenancy schema name
    // derives from it): an unparseable id gets no answer, fail closed.
    if schema_name_for_str(&tenant_id).is_err() {
        return;
    }
    let configs: Vec<HolidaySyncConfig> = state
        .configs
        .iter()
        .filter(|entry| entry.tenant_id == tenant_id && entry.enabled)
        .map(|entry| entry.clone())
        .collect();
    if configs.is_empty() {
        return;
    }
    let secret = state.secret();
    let Some(bus) = state.bus() else {
        return;
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            for config in configs {
                let task_state = state.clone();
                let task_bus = bus.clone();
                let task_secret = secret.clone();
                handle.spawn(async move {
                    run_sync_for_config(&task_state, &task_bus, &task_secret, &config, "sync_all").await;
                });
            }
        }
        Err(_) => {
            let now = Utc::now();
            for config in configs {
                let run_id = Uuid::new_v4();
                state.runs.insert(
                    run_id,
                    HolidaySyncRun {
                        id: run_id,
                        tenant_id: config.tenant_id.clone(),
                        company_id: config.company_id.clone(),
                        config_id: config.id,
                        provider_id: config.provider_id.clone(),
                        country: config.countries.first().cloned().unwrap_or_default(),
                        year: config.years.first().copied().unwrap_or(0),
                        status: "failed".to_string(),
                        trigger: "sync_all".to_string(),
                        holidays_found: 0,
                        holidays_stored: 0,
                        error: Some("No async runtime available to run the scheduled sync".to_string()),
                        started_at: now,
                        finished_at: now,
                        fetched_at: now,
                    },
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers: providers (platform reference data).
// ---------------------------------------------------------------------------

async fn list_providers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .providers
            .iter()
            .map(|entry| entry.public_view())
            .collect();
        if let Some(search) = params.search.as_deref().filter(|text| !text.trim().is_empty()) {
            let needle = search.trim().to_ascii_lowercase();
            items.retain(|item| {
                item.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.to_ascii_lowercase().contains(&needle))
                    || item
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|text| text.to_ascii_lowercase().contains(&needle))
            });
        }
        if let Some(enabled) = params.filters.get("enabled") {
            let wanted = enabled.trim() == "true";
            items.retain(|item| {
                item.get("enabled").and_then(Value::as_bool).unwrap_or(false) == wanted
            });
        }
        if let Some(kind) = params.filters.get("kind") {
            items.retain(|item| item.get("kind").and_then(Value::as_str) == Some(kind.as_str()));
        }
        items.sort_by(|a, b| {
            a.get("id")
                .and_then(Value::as_str)
                .cmp(&b.get("id").and_then(Value::as_str))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn create_provider(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    body: web::Json<CreateProviderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let body = body.into_inner();
        let id = validate_provider_id(&body.id)?;
        if state.providers.contains_key(&id) {
            return Err(UnifiedError::conflict(format!(
                "Provider \"{id}\" already exists"
            )));
        }
        let Some(kind) = ProviderKind::from_str(body.kind.trim()) else {
            return Err(UnifiedError::bad_request(format!(
                "Unknown provider kind \"{}\" (nager_date, calendarific, abstract_api, google_ical, generic_ical)",
                body.kind
            )));
        };
        let name = body.name.trim().to_string();
        if name.is_empty() || name.len() > 100 {
            return Err(UnifiedError::bad_request(
                "Provider name must be 1-100 characters",
            ));
        }
        let base_url = body.base_url.trim().to_string();
        if kind == ProviderKind::GenericIcal {
            if !base_url.is_empty() {
                return Err(UnifiedError::bad_request(
                    "A generic_ical provider has no base URL: the feed URL lives on the sync config",
                ));
            }
        } else if base_url.is_empty() || base_url.len() > 500 {
            return Err(UnifiedError::bad_request(
                "Provider base URL must be 1-500 characters",
            ));
        }
        let mut allowed_hosts: Vec<String> = body
            .allowed_hosts
            .iter()
            .map(|host| host.trim().to_ascii_lowercase())
            .filter(|host| !host.is_empty())
            .collect();
        allowed_hosts.sort();
        allowed_hosts.dedup();
        if kind != ProviderKind::GenericIcal && allowed_hosts.is_empty() {
            return Err(UnifiedError::bad_request(
                "Provider allowed_hosts must list at least one host",
            ));
        }
        if kind != ProviderKind::GenericIcal {
            let probe = format!("{}/", base_url.trim_end_matches('/'));
            validate_provider_url(&probe, &allowed_hosts).map_err(|_| {
                UnifiedError::bad_request(
                    "Provider base URL host is not on allowed_hosts, is not https, or resolves to a private address",
                )
            })?;
        }
        let api_key_required = body.api_key_required.unwrap_or_else(|| kind.key_required());
        if api_key_required && !kind.key_required() {
            return Err(UnifiedError::bad_request(
                "Only calendarific and abstract_api providers take an API key",
            ));
        }
        let min_interval_secs = body.min_interval_secs.unwrap_or(1);
        if min_interval_secs > 3600 {
            return Err(UnifiedError::bad_request(
                "min_interval_secs must be between 0 and 3600",
            ));
        }
        let max_requests_per_minute = body.max_requests_per_minute.unwrap_or(60);
        if !(1..=1000).contains(&max_requests_per_minute) {
            return Err(UnifiedError::bad_request(
                "max_requests_per_minute must be between 1 and 1000",
            ));
        }
        let mut calendar_ids: HashMap<String, String> = HashMap::new();
        if let Some(ids) = body.calendar_ids {
            if kind != ProviderKind::GoogleIcal {
                return Err(UnifiedError::bad_request(
                    "calendar_ids only apply to the google_ical provider",
                ));
            }
            for (raw_code, calendar_id) in ids {
                let code = normalize_country_code(&raw_code)?;
                let calendar_id = calendar_id.trim().to_string();
                if calendar_id.is_empty() || calendar_id.len() > 200 {
                    return Err(UnifiedError::bad_request(
                        "Google calendar id must be 1-200 characters",
                    ));
                }
                calendar_ids.insert(code, calendar_id);
            }
        }
        let now = Utc::now();
        let mut provider = HolidayProvider {
            id: id.clone(),
            name,
            kind,
            base_url,
            allowed_hosts,
            api_key_required,
            api_key_encrypted: None,
            key_fingerprint: None,
            min_interval_secs,
            max_requests_per_minute,
            calendar_ids,
            enabled: body.enabled.unwrap_or(true),
            builtin: false,
            created_at: now,
            updated_at: now,
        };
        if let Some(api_key) = body.api_key.as_deref().filter(|key| !key.is_empty()) {
            if !kind.key_required() {
                return Err(UnifiedError::bad_request(
                    "This provider kind does not take an API key",
                ));
            }
            let secret = encryption_secret(&ctx);
            provider.api_key_encrypted = Some(encrypt_api_key(&secret, api_key));
            provider.key_fingerprint = Some(key_fingerprint(api_key));
        } else if api_key_required {
            return Err(UnifiedError::bad_request(
                "This provider needs an API key: pass api_key once (write-only, stored encrypted)",
            ));
        }
        let view = provider.public_view();
        state.providers.insert(id.clone(), provider);
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_PROVIDER_CREATED,
                json!({ "provider_id": id, "kind": kind.as_str() }),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

async fn get_provider(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let id = path.into_inner();
        let Some(provider) = state.providers.get(&id) else {
            return Err(not_found_for_tenant("Provider"));
        };
        Ok(provider.public_view())
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn update_provider(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProviderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let id = path.into_inner();
        let body = body.into_inner();
        let Some(mut provider) = state.providers.get(&id).map(|entry| entry.clone()) else {
            return Err(not_found_for_tenant("Provider"));
        };
        if let Some(name) = body.name {
            let name = name.trim().to_string();
            if name.is_empty() || name.len() > 100 {
                return Err(UnifiedError::bad_request(
                    "Provider name must be 1-100 characters",
                ));
            }
            provider.name = name;
        }
        if let Some(hosts) = body.allowed_hosts {
            let mut allowed_hosts: Vec<String> = hosts
                .iter()
                .map(|host| host.trim().to_ascii_lowercase())
                .filter(|host| !host.is_empty())
                .collect();
            allowed_hosts.sort();
            allowed_hosts.dedup();
            if provider.kind != ProviderKind::GenericIcal && allowed_hosts.is_empty() {
                return Err(UnifiedError::bad_request(
                    "Provider allowed_hosts must list at least one host",
                ));
            }
            if provider.kind != ProviderKind::GenericIcal && !provider.base_url.is_empty() {
                let probe = format!("{}/", provider.base_url.trim_end_matches('/'));
                validate_provider_url(&probe, &allowed_hosts).map_err(|_| {
                    UnifiedError::bad_request(
                        "Provider base URL host is not on allowed_hosts, is not https, or resolves to a private address",
                    )
                })?;
            }
            provider.allowed_hosts = allowed_hosts;
        }
        if let Some(min_interval_secs) = body.min_interval_secs {
            if min_interval_secs > 3600 {
                return Err(UnifiedError::bad_request(
                    "min_interval_secs must be between 0 and 3600",
                ));
            }
            provider.min_interval_secs = min_interval_secs;
        }
        if let Some(max_requests_per_minute) = body.max_requests_per_minute {
            if !(1..=1000).contains(&max_requests_per_minute) {
                return Err(UnifiedError::bad_request(
                    "max_requests_per_minute must be between 1 and 1000",
                ));
            }
            provider.max_requests_per_minute = max_requests_per_minute;
        }
        if let Some(ids) = body.calendar_ids {
            if provider.kind != ProviderKind::GoogleIcal {
                return Err(UnifiedError::bad_request(
                    "calendar_ids only apply to the google_ical provider",
                ));
            }
            let mut calendar_ids: HashMap<String, String> = HashMap::new();
            for (raw_code, calendar_id) in ids {
                let code = normalize_country_code(&raw_code)?;
                let calendar_id = calendar_id.trim().to_string();
                if calendar_id.is_empty() || calendar_id.len() > 200 {
                    return Err(UnifiedError::bad_request(
                        "Google calendar id must be 1-200 characters",
                    ));
                }
                calendar_ids.insert(code, calendar_id);
            }
            provider.calendar_ids = calendar_ids;
        }
        if let Some(api_key) = body.api_key {
            if !provider.kind.key_required() {
                return Err(UnifiedError::bad_request(
                    "This provider kind does not take an API key",
                ));
            }
            if api_key.is_empty() {
                provider.api_key_encrypted = None;
                provider.key_fingerprint = None;
            } else {
                let secret = encryption_secret(&ctx);
                provider.api_key_encrypted = Some(encrypt_api_key(&secret, &api_key));
                provider.key_fingerprint = Some(key_fingerprint(&api_key));
            }
        }
        if let Some(enabled) = body.enabled {
            provider.enabled = enabled;
        }
        provider.updated_at = Utc::now();
        let view = provider.public_view();
        state.providers.insert(id.clone(), provider);
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_PROVIDER_UPDATED,
                json!({ "provider_id": id }),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn delete_provider(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let id = path.into_inner();
        let Some(provider) = state.providers.get(&id).map(|entry| entry.clone()) else {
            return Err(not_found_for_tenant("Provider"));
        };
        if provider.builtin {
            return Err(UnifiedError::conflict(
                "Built-in providers cannot be deleted; disable them instead",
            ));
        }
        let referenced = state.configs.iter().any(|entry| {
            entry.provider_id == id || entry.fallback_provider_ids.contains(&id)
        });
        if referenced {
            return Err(UnifiedError::conflict(
                "Provider is referenced by a sync config; remove it from the config first",
            ));
        }
        state.providers.remove(&id);
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_PROVIDER_DELETED,
                json!({ "provider_id": id }),
            );
        }
        Ok(json!({ "deleted": true, "provider_id": id }))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn test_provider(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TestProviderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_SYNC)?;
        let id = path.into_inner();
        let body = body.into_inner();
        let Some(provider) = state.providers.get(&id).map(|entry| entry.clone()) else {
            return Err(not_found_for_tenant("Provider"));
        };
        if !provider.enabled {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{id}\" is disabled"
            )));
        }
        let country = match body.country {
            Some(raw) => normalize_country_code(&raw)?,
            None => "ID".to_string(),
        };
        let year = body.year.unwrap_or_else(|| Utc::now().year());
        if !(MIN_SYNC_YEAR..=MAX_SYNC_YEAR).contains(&year) {
            return Err(UnifiedError::bad_request(format!(
                "Year must be between {MIN_SYNC_YEAR} and {MAX_SYNC_YEAR}"
            )));
        }
        // A throwaway config carries the test feed URL for a
        // generic_ical provider; it is never stored.
        let probe_config = HolidaySyncConfig {
            id: Uuid::new_v4(),
            tenant_id: String::new(),
            company_id: String::new(),
            provider_id: provider.id.clone(),
            fallback_provider_ids: Vec::new(),
            countries: vec![country.clone()],
            years: vec![year],
            schedule: DEFAULT_SCHEDULE.to_string(),
            feed_url: body.feed_url,
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let secret = encryption_secret(&ctx);
        let started = Utc::now();
        let fetched = fetch_holidays_for_provider(
            &state,
            &provider,
            &probe_config,
            &country,
            year,
            &secret,
        )
        .await?;
        let finished = Utc::now();
        Ok::<Value, UnifiedError>(json!({
            "provider_id": provider.id,
            "kind": provider.kind.as_str(),
            "country": country,
            "year": year,
            "reachable": true,
            "holidays_found": fetched.len(),
            "sample": fetched.iter().take(3).map(|item| json!({
                "date": item.date.to_string(),
                "name": item.name,
            })).collect::<Vec<Value>>(),
            "started_at": started.to_rfc3339(),
            "finished_at": finished.to_rfc3339(),
        }))
    })()
    .await;
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: countries (platform reference data).
// ---------------------------------------------------------------------------

async fn list_countries(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .countries
            .iter()
            .map(|entry| {
                json!({
                    "code": entry.code,
                    "name": entry.name,
                    "enabled": entry.enabled,
                    "created_at": entry.created_at.to_rfc3339(),
                    "updated_at": entry.updated_at.to_rfc3339(),
                })
            })
            .collect();
        if let Some(search) = params.search.as_deref().filter(|text| !text.trim().is_empty()) {
            let needle = search.trim().to_ascii_lowercase();
            items.retain(|item| {
                item.get("code")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.to_ascii_lowercase().contains(&needle))
                    || item
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|text| text.to_ascii_lowercase().contains(&needle))
            });
        }
        if let Some(enabled) = params.filters.get("enabled") {
            let wanted = enabled.trim() == "true";
            items.retain(|item| {
                item.get("enabled").and_then(Value::as_bool).unwrap_or(false) == wanted
            });
        }
        items.sort_by(|a, b| {
            a.get("code")
                .and_then(Value::as_str)
                .cmp(&b.get("code").and_then(Value::as_str))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn create_country(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    body: web::Json<CreateCountryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let body = body.into_inner();
        let code = normalize_country_code(&body.code)?;
        if state.countries.contains_key(&code) {
            return Err(UnifiedError::conflict(format!(
                "Country \"{code}\" already exists"
            )));
        }
        let name = body.name.trim().to_string();
        if name.is_empty() || name.len() > 100 {
            return Err(UnifiedError::bad_request(
                "Country name must be 1-100 characters",
            ));
        }
        let now = Utc::now();
        let country = HolidayCountry {
            code: code.clone(),
            name: name.clone(),
            enabled: body.enabled.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.countries.insert(code.clone(), country);
        Ok(json!({
            "code": code,
            "name": name,
            "enabled": body.enabled.unwrap_or(true),
            "created_at": now.to_rfc3339(),
            "updated_at": now.to_rfc3339(),
        }))
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

async fn update_country(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCountryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let code = normalize_country_code(&path.into_inner())?;
        let body = body.into_inner();
        let Some(mut country) = state.countries.get(&code).map(|entry| entry.clone()) else {
            return Err(not_found_for_tenant("Country"));
        };
        if let Some(name) = body.name {
            let name = name.trim().to_string();
            if name.is_empty() || name.len() > 100 {
                return Err(UnifiedError::bad_request(
                    "Country name must be 1-100 characters",
                ));
            }
            country.name = name;
        }
        if let Some(enabled) = body.enabled {
            country.enabled = enabled;
        }
        country.updated_at = Utc::now();
        let view = json!({
            "code": country.code,
            "name": country.name,
            "enabled": country.enabled,
            "created_at": country.created_at.to_rfc3339(),
            "updated_at": country.updated_at.to_rfc3339(),
        });
        state.countries.insert(code, country);
        Ok(view)
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn delete_country(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let code = normalize_country_code(&path.into_inner())?;
        if !state.countries.contains_key(&code) {
            return Err(not_found_for_tenant("Country"));
        }
        let referenced = state
            .configs
            .iter()
            .any(|entry| entry.countries.contains(&code));
        if referenced {
            return Err(UnifiedError::conflict(
                "Country is selected by a sync config; remove it from the config first",
            ));
        }
        state.countries.remove(&code);
        Ok(json!({ "deleted": true, "code": code }))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: sync configs (tenant business data).
// ---------------------------------------------------------------------------

fn load_config_for_tenant(
    state: &HolidaysState,
    caller: &Caller,
    config_id: Uuid,
) -> UnifiedResult<HolidaySyncConfig> {
    let Some(config) = state.configs.get(&config_id).map(|entry| entry.clone()) else {
        return Err(not_found_for_tenant("Sync config"));
    };
    if config.tenant_id != caller.tenant_id {
        return Err(not_found_for_tenant("Sync config"));
    }
    Ok(config)
}

async fn list_configs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .configs
            .iter()
            .filter(|entry| entry.tenant_id == caller.tenant_id)
            .map(|entry| entry.public_view())
            .collect();
        if let Some(provider_id) = params.filters.get("provider_id") {
            items.retain(|item| {
                item.get("provider_id").and_then(Value::as_str) == Some(provider_id.as_str())
            });
        }
        if let Some(country) = params.filters.get("country") {
            let wanted = country.trim().to_ascii_uppercase();
            items.retain(|item| {
                item.get("countries")
                    .and_then(Value::as_array)
                    .is_some_and(|countries| {
                        countries
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|code| code == wanted)
                    })
            });
        }
        if let Some(enabled) = params.filters.get("enabled") {
            let wanted = enabled.trim() == "true";
            items.retain(|item| {
                item.get("enabled").and_then(Value::as_bool).unwrap_or(false) == wanted
            });
        }
        items.sort_by(|a, b| {
            a.get("created_at")
                .and_then(Value::as_str)
                .cmp(&b.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn create_config(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    body: web::Json<CreateConfigRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let body = body.into_inner();
        let provider_id = body
            .provider_id
            .as_deref()
            .unwrap_or(DEFAULT_PROVIDER_ID)
            .trim()
            .to_string();
        let provider = load_enabled_provider(&state, &provider_id)?;
        let fallback_provider_ids = validate_fallback_chain(
            &state,
            &provider_id,
            body.fallback_provider_ids.as_deref().unwrap_or(&[]),
        )?;
        let countries = validate_config_countries(&state, &body.countries)?;
        let years = match body.years {
            Some(years) => validate_years(&years)?,
            None => default_years(),
        };
        let schedule = validate_schedule(body.schedule.as_deref().unwrap_or(DEFAULT_SCHEDULE))?;
        let feed_url = match body.feed_url {
            Some(raw) => {
                if provider.kind != ProviderKind::GenericIcal {
                    return Err(UnifiedError::bad_request(
                        "feed_url only applies to the generic_ical provider",
                    ));
                }
                let url = raw.trim().to_string();
                if url.is_empty() {
                    return Err(UnifiedError::bad_request(
                        "feed_url must not be empty for the generic_ical provider",
                    ));
                }
                Some(url)
            }
            None => None,
        };
        if provider.kind == ProviderKind::GenericIcal && feed_url.is_none() {
            return Err(UnifiedError::bad_request(
                "The generic_ical provider needs a feed_url on the sync config",
            ));
        }
        if let Some(url) = feed_url.as_deref() {
            let parsed = url::Url::parse(url)
                .map_err(|_| UnifiedError::bad_request("feed_url is invalid"))?;
            let Some(host) = parsed.host_str() else {
                return Err(UnifiedError::bad_request("feed_url must have a host"));
            };
            validate_provider_url(url, &[host.to_string()])?;
        }
        let now = Utc::now();
        let config = HolidaySyncConfig {
            id: Uuid::new_v4(),
            tenant_id: caller.tenant_id.clone(),
            company_id: caller.company_id.clone(),
            provider_id: provider.id,
            fallback_provider_ids,
            countries,
            years,
            schedule,
            feed_url,
            enabled: body.enabled.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        let view = config.public_view();
        state.configs.insert(config.id, config.clone());
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_CONFIG_CREATED,
                json!({
                    "config_id": config.id,
                    "tenant_id": config.tenant_id,
                    "provider_id": config.provider_id,
                }),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

async fn get_config(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let config_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid config id"))?;
        let config = load_config_for_tenant(&state, &caller, config_id)?;
        Ok(config.public_view())
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn update_config(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateConfigRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let config_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid config id"))?;
        let mut config = load_config_for_tenant(&state, &caller, config_id)?;
        let body = body.into_inner();
        if let Some(provider_id) = body.provider_id {
            let provider_id = provider_id.trim().to_string();
            let provider = load_enabled_provider(&state, &provider_id)?;
            config.provider_id = provider.id;
            config.fallback_provider_ids.retain(|id| *id != config.provider_id);
            if provider.kind != ProviderKind::GenericIcal {
                config.feed_url = None;
            }
        }
        if let Some(fallback_ids) = body.fallback_provider_ids {
            config.fallback_provider_ids =
                validate_fallback_chain(&state, &config.provider_id, &fallback_ids)?;
        }
        if let Some(countries) = body.countries {
            config.countries = validate_config_countries(&state, &countries)?;
        }
        if let Some(years) = body.years {
            config.years = validate_years(&years)?;
        }
        if let Some(schedule) = body.schedule {
            config.schedule = validate_schedule(&schedule)?;
        }
        if let Some(feed_url) = body.feed_url {
            let provider = load_enabled_provider(&state, &config.provider_id)?;
            if feed_url.trim().is_empty() {
                config.feed_url = None;
            } else {
                if provider.kind != ProviderKind::GenericIcal {
                    return Err(UnifiedError::bad_request(
                        "feed_url only applies to the generic_ical provider",
                    ));
                }
                let url = feed_url.trim().to_string();
                let parsed = url::Url::parse(&url)
                    .map_err(|_| UnifiedError::bad_request("feed_url is invalid"))?;
                let Some(host) = parsed.host_str() else {
                    return Err(UnifiedError::bad_request("feed_url must have a host"));
                };
                validate_provider_url(&url, &[host.to_string()])?;
                config.feed_url = Some(url);
            }
        }
        let provider = load_enabled_provider(&state, &config.provider_id)?;
        if provider.kind == ProviderKind::GenericIcal && config.feed_url.is_none() {
            return Err(UnifiedError::bad_request(
                "The generic_ical provider needs a feed_url on the sync config",
            ));
        }
        if let Some(enabled) = body.enabled {
            config.enabled = enabled;
        }
        config.updated_at = Utc::now();
        let view = config.public_view();
        state.configs.insert(config.id, config.clone());
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_CONFIG_UPDATED,
                json!({
                    "config_id": config.id,
                    "tenant_id": config.tenant_id,
                    "provider_id": config.provider_id,
                }),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn delete_config(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let config_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid config id"))?;
        let config = load_config_for_tenant(&state, &caller, config_id)?;
        state.configs.remove(&config.id);
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_CONFIG_DELETED,
                json!({
                    "config_id": config.id,
                    "tenant_id": config.tenant_id,
                }),
            );
        }
        Ok(json!({ "deleted": true, "config_id": config.id }))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: sync triggers and runs (tenant business data).
// ---------------------------------------------------------------------------

async fn sync_config(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_SYNC)?;
        let config_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid config id"))?;
        let config = load_config_for_tenant(&state, &caller, config_id)?;
        if !config.enabled {
            return Err(UnifiedError::bad_request(
                "Sync config is disabled; enable it before syncing",
            ));
        }
        let secret = encryption_secret(&ctx);
        let bus = ctx.hooks.clone();
        let scheduler_status = enqueue_scheduler_job(
            &bus,
            SCHEDULER_JOB_KIND_SYNC,
            json!({
                "config_id": config.id.to_string(),
                "tenant_id": config.tenant_id,
            }),
        );
        let mut summary = run_sync_for_config(&state, &bus, &secret, &config, "manual").await;
        summary.scheduler_status = scheduler_status;
        Ok::<Value, UnifiedError>(json!({
            "config_id": summary.config_id,
            "tenant_id": summary.tenant_id,
            "trigger": summary.trigger,
            "attempted": summary.attempted,
            "succeeded": summary.succeeded,
            "failed": summary.failed,
            "holidays_found": summary.holidays_found,
            "holidays_stored": summary.holidays_stored,
            "scheduler_status": summary.scheduler_status,
            "runs": summary.runs,
        }))
    })()
    .await;
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn sync_all_configs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_SYNC)?;
        let configs: Vec<HolidaySyncConfig> = state
            .configs
            .iter()
            .filter(|entry| entry.tenant_id == caller.tenant_id && entry.enabled)
            .map(|entry| entry.clone())
            .collect();
        let secret = encryption_secret(&ctx);
        let bus = ctx.hooks.clone();
        let scheduler_status = enqueue_scheduler_job(
            &bus,
            SCHEDULER_JOB_KIND_SYNC_ALL,
            json!({ "tenant_id": caller.tenant_id }),
        );
        let mut summaries: Vec<Value> = Vec::new();
        let mut attempted: usize = 0;
        let mut succeeded: usize = 0;
        let mut failed: usize = 0;
        let mut holidays_found: i64 = 0;
        let mut holidays_stored: i64 = 0;
        for config in configs {
            let mut summary = run_sync_for_config(&state, &bus, &secret, &config, "sync_all").await;
            attempted += summary.attempted;
            succeeded += summary.succeeded;
            failed += summary.failed;
            holidays_found += summary.holidays_found;
            holidays_stored += summary.holidays_stored;
            summary.scheduler_status = scheduler_status.clone();
            summaries.push(json!({
                "config_id": summary.config_id,
                "trigger": summary.trigger,
                "attempted": summary.attempted,
                "succeeded": summary.succeeded,
                "failed": summary.failed,
                "holidays_found": summary.holidays_found,
                "holidays_stored": summary.holidays_stored,
                "scheduler_status": summary.scheduler_status,
                "runs": summary.runs,
            }));
        }
        Ok::<Value, UnifiedError>(json!({
            "tenant_id": caller.tenant_id,
            "trigger": "sync_all",
            "configs_synced": summaries.len(),
            "attempted": attempted,
            "succeeded": succeeded,
            "failed": failed,
            "holidays_found": holidays_found,
            "holidays_stored": holidays_stored,
            "scheduler_status": scheduler_status,
            "summaries": summaries,
        }))
    })()
    .await;
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn list_sync_runs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .runs
            .iter()
            .filter(|entry| entry.tenant_id == caller.tenant_id)
            .map(|entry| entry.public_view())
            .collect();
        if let Some(config_id) = params.filters.get("config_id") {
            items.retain(|item| {
                item.get("config_id").and_then(Value::as_str) == Some(config_id.as_str())
            });
        }
        if let Some(provider_id) = params.filters.get("provider_id") {
            items.retain(|item| {
                item.get("provider_id").and_then(Value::as_str) == Some(provider_id.as_str())
            });
        }
        if let Some(country) = params.filters.get("country") {
            let wanted = country.trim().to_ascii_uppercase();
            items.retain(|item| {
                item.get("country").and_then(Value::as_str) == Some(wanted.as_str())
            });
        }
        if let Some(year) = params.filters.get("year") {
            items.retain(|item| {
                item.get("year").and_then(Value::as_i64)
                    == year.trim().parse::<i64>().ok()
            });
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|item| item.get("status").and_then(Value::as_str) == Some(status.as_str()));
        }
        items.sort_by(|a, b| {
            b.get("started_at")
                .and_then(Value::as_str)
                .cmp(&a.get("started_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn get_sync_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let run_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid sync run id"))?;
        let Some(run) = state.runs.get(&run_id).map(|entry| entry.clone()) else {
            return Err(not_found_for_tenant("Sync run"));
        };
        if run.tenant_id != caller.tenant_id {
            return Err(not_found_for_tenant("Sync run"));
        }
        Ok(run.public_view())
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: holidays (shared reference data, tenant-scoped reads).
// ---------------------------------------------------------------------------

async fn list_holidays(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let params = query.into_inner();
        // A tenant only ever sees the countries its configs select.
        let selected = state.tenant_countries(&caller.tenant_id);
        let mut items: Vec<Value> = state
            .holidays
            .iter()
            .filter(|entry| selected.contains(&entry.country))
            .map(|entry| entry.public_view())
            .collect();
        if let Some(country) = params.filters.get("country") {
            let wanted = normalize_country_code(country)?;
            if !selected.contains(&wanted) {
                return Err(not_found_for_tenant("Country"));
            }
            items.retain(|item| item.get("country").and_then(Value::as_str) == Some(wanted.as_str()));
        }
        if let Some(year) = params.filters.get("year") {
            let wanted = year
                .trim()
                .parse::<i32>()
                .map_err(|_| UnifiedError::bad_request("Year filter must be a number"))?;
            items.retain(|item| {
                item.get("date")
                    .and_then(Value::as_str)
                    .and_then(|date| date.get(..4))
                    .and_then(|text| text.parse::<i32>().ok())
                    == Some(wanted)
            });
        }
        if let Some(source) = params.filters.get("source") {
            items.retain(|item| item.get("source").and_then(Value::as_str) == Some(source.as_str()));
        }
        if let Some(provider_id) = params.filters.get("provider_id") {
            items.retain(|item| {
                item.get("provider_id").and_then(Value::as_str) == Some(provider_id.as_str())
            });
        }
        if let Some(search) = params.search.as_deref().filter(|text| !text.trim().is_empty()) {
            let needle = search.trim().to_ascii_lowercase();
            items.retain(|item| {
                item.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.to_ascii_lowercase().contains(&needle))
            });
        }
        items.sort_by(|a, b| {
            a.get("date")
                .and_then(Value::as_str)
                .cmp(&b.get("date").and_then(Value::as_str))
                .then_with(|| {
                    a.get("name")
                        .and_then(Value::as_str)
                        .cmp(&b.get("name").and_then(Value::as_str))
                })
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn get_is_holiday(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    query: web::Query<IsHolidayQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let query = query.into_inner();
        let date = parse_date(&query.date)?;
        let country = normalize_country_code(&query.country)?;
        Ok(answer_is_holiday(&state, &caller.tenant_id, &country, date))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn get_non_working_days(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    query: web::Query<NonWorkingDaysQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let query = query.into_inner();
        let start = parse_date(&query.start)?;
        let end = parse_date(&query.end)?;
        let country = normalize_country_code(&query.country)?;
        let (start, end) = if end < start { (end, start) } else { (start, end) };
        let span = end.signed_duration_since(start).num_days();
        if span > MAX_NON_WORKING_RANGE_DAYS {
            return Err(UnifiedError::bad_request(format!(
                "Date range must span at most {MAX_NON_WORKING_RANGE_DAYS} days"
            )));
        }
        let selected = state.tenant_countries(&caller.tenant_id);
        if !selected.contains(&country) {
            return Ok(json!({
                "start": start.to_string(),
                "end": end.to_string(),
                "country": country,
                "in_scope": false,
                "dates": [],
            }));
        }
        let mut dates: Vec<Value> = Vec::new();
        let mut day = start;
        while day <= end {
            if let Some(override_entry) = state.tenant_override(&caller.tenant_id, &country, day) {
                if override_entry.override_kind != "working_day" {
                    dates.push(json!({
                        "date": day.to_string(),
                        "name": override_entry.name,
                        "source": "manual",
                        "provider": "manual",
                    }));
                }
            } else {
                let mut day_holidays: Vec<Holiday> = state
                    .holidays
                    .iter()
                    .filter(|entry| entry.country == country && entry.date == day)
                    .map(|entry| entry.clone())
                    .collect();
                day_holidays.sort_by(|a, b| a.name.cmp(&b.name));
                for holiday in day_holidays {
                    dates.push(json!({
                        "date": day.to_string(),
                        "name": holiday.name,
                        "source": "synced",
                        "provider": holiday.provider_id,
                    }));
                }
            }
            day += chrono::Duration::days(1);
        }
        Ok(json!({
            "start": start.to_string(),
            "end": end.to_string(),
            "country": country,
            "in_scope": true,
            "dates": dates,
        }))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: manual overrides (tenant business data).
// ---------------------------------------------------------------------------

fn load_override_for_tenant(
    state: &HolidaysState,
    caller: &Caller,
    override_id: Uuid,
) -> UnifiedResult<HolidayOverride> {
    let Some(override_entry) = state.overrides.get(&override_id).map(|entry| entry.clone()) else {
        return Err(not_found_for_tenant("Override"));
    };
    if override_entry.tenant_id != caller.tenant_id {
        return Err(not_found_for_tenant("Override"));
    }
    Ok(override_entry)
}

async fn list_overrides(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .overrides
            .iter()
            .filter(|entry| entry.tenant_id == caller.tenant_id)
            .map(|entry| entry.public_view())
            .collect();
        if let Some(country) = params.filters.get("country") {
            let wanted = normalize_country_code(country)?;
            items.retain(|item| item.get("country").and_then(Value::as_str) == Some(wanted.as_str()));
        }
        if let Some(kind) = params.filters.get("override_kind") {
            items.retain(|item| {
                item.get("override_kind").and_then(Value::as_str) == Some(kind.as_str())
            });
        }
        if let Some(year) = params.filters.get("year") {
            let wanted = year
                .trim()
                .parse::<i32>()
                .map_err(|_| UnifiedError::bad_request("Year filter must be a number"))?;
            items.retain(|item| {
                item.get("date")
                    .and_then(Value::as_str)
                    .and_then(|date| date.get(..4))
                    .and_then(|text| text.parse::<i32>().ok())
                    == Some(wanted)
            });
        }
        items.sort_by(|a, b| {
            a.get("date")
                .and_then(Value::as_str)
                .cmp(&b.get("date").and_then(Value::as_str))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn create_override(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    body: web::Json<CreateOverrideRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let body = body.into_inner();
        let country = normalize_country_code(&body.country)?;
        let date = parse_date(&body.date)?;
        let name = body.name.trim().to_string();
        if name.is_empty() || name.len() > 200 {
            return Err(UnifiedError::bad_request(
                "Override name must be 1-200 characters",
            ));
        }
        let override_kind = body
            .override_kind
            .as_deref()
            .unwrap_or("day_off")
            .trim()
            .to_string();
        if !matches!(override_kind.as_str(), "day_off" | "working_day") {
            return Err(UnifiedError::bad_request(
                "override_kind must be \"day_off\" or \"working_day\"",
            ));
        }
        if state
            .tenant_override(&caller.tenant_id, &country, date)
            .is_some()
        {
            return Err(UnifiedError::conflict(
                "This tenant already has an override for that country and date",
            ));
        }
        let note = body.note.map(|text| text.trim().to_string()).filter(|text| !text.is_empty());
        if let Some(note) = note.as_deref() {
            if note.len() > 500 {
                return Err(UnifiedError::bad_request(
                    "Override note must be at most 500 characters",
                ));
            }
        }
        let now = Utc::now();
        let override_entry = HolidayOverride {
            id: Uuid::new_v4(),
            tenant_id: caller.tenant_id.clone(),
            company_id: caller.company_id.clone(),
            country,
            date,
            name,
            override_kind,
            note,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        let view = override_entry.public_view();
        state.overrides.insert(override_entry.id, override_entry.clone());
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_OVERRIDE_CREATED,
                json!({
                    "override_id": override_entry.id,
                    "tenant_id": override_entry.tenant_id,
                    "country": override_entry.country,
                    "date": override_entry.date.to_string(),
                    "override_kind": override_entry.override_kind,
                }),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

async fn get_override(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let override_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid override id"))?;
        let override_entry = load_override_for_tenant(&state, &caller, override_id)?;
        Ok(override_entry.public_view())
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn update_override(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateOverrideRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let override_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid override id"))?;
        let mut override_entry = load_override_for_tenant(&state, &caller, override_id)?;
        let body = body.into_inner();
        if let Some(name) = body.name {
            let name = name.trim().to_string();
            if name.is_empty() || name.len() > 200 {
                return Err(UnifiedError::bad_request(
                    "Override name must be 1-200 characters",
                ));
            }
            override_entry.name = name;
        }
        if let Some(kind) = body.override_kind {
            let kind = kind.trim().to_string();
            if !matches!(kind.as_str(), "day_off" | "working_day") {
                return Err(UnifiedError::bad_request(
                    "override_kind must be \"day_off\" or \"working_day\"",
                ));
            }
            override_entry.override_kind = kind;
        }
        if let Some(note) = body.note {
            let note = note.trim().to_string();
            if note.len() > 500 {
                return Err(UnifiedError::bad_request(
                    "Override note must be at most 500 characters",
                ));
            }
            override_entry.note = if note.is_empty() { None } else { Some(note) };
        }
        override_entry.updated_at = Utc::now();
        let view = override_entry.public_view();
        state.overrides.insert(override_entry.id, override_entry.clone());
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_OVERRIDE_UPDATED,
                json!({
                    "override_id": override_entry.id,
                    "tenant_id": override_entry.tenant_id,
                }),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn delete_override(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let override_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid override id"))?;
        let override_entry = load_override_for_tenant(&state, &caller, override_id)?;
        state.overrides.remove(&override_entry.id);
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_OVERRIDE_DELETED,
                json!({
                    "override_id": override_entry.id,
                    "tenant_id": override_entry.tenant_id,
                }),
            );
        }
        Ok(json!({ "deleted": true, "override_id": override_entry.id }))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module declaration.
// ---------------------------------------------------------------------------

pub struct HolidaysModule {
    state: HolidaysState,
}

impl HolidaysModule {
    pub fn new() -> Self {
        Self {
            state: HolidaysState::new(),
        }
    }
}

impl Default for HolidaysModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for HolidaysModule {
    fn id(&self) -> &'static str {
        "holidays"
    }

    fn name(&self) -> &'static str {
        "Holidays"
    }

    fn description(&self) -> &'static str {
        "Global public-holiday synchronization: a data-driven provider registry (Nager.Date default, Calendarific, Abstract API, Google Calendar public iCal, generic iCal), per-tenant sync configs with fallback chains, cached holidays per country and year, manual overrides that win over synced days, and hook answers (holidays.is_holiday, holidays.non_working_days) that feed the calendar and HR, academic, appointments, and projects modules with non-working days. Tenant-scoped throughout; provider keys are write-only and encrypted."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "scheduler"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "scheduler"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some(ProductCategory::Productivity.as_str())
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        *self
            .state
            .encryption_secret
            .write()
            .expect("encryption secret lock poisoned") = Some(encryption_secret(ctx));
        self.state.seed_defaults();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = Arc::new(self.state.clone());
        let state_for_hook = state.clone();
        bus.register(
            EVENT_IS_HOLIDAY,
            "holidays",
            Arc::new(move |ctx: &mut HookContext| {
                on_is_holiday_requested(state_for_hook.clone(), ctx)
            }),
        );
        let state_for_hook = state.clone();
        bus.register(
            EVENT_NON_WORKING_DAYS,
            "holidays",
            Arc::new(move |ctx: &mut HookContext| {
                on_non_working_days_requested(state_for_hook.clone(), ctx)
            }),
        );
        let state_for_hook = state.clone();
        bus.register(
            EVENT_SYNC_REQUESTED,
            "holidays",
            Arc::new(move |ctx: &mut HookContext| {
                on_sync_requested(state_for_hook.clone(), ctx)
            }),
        );
        let state_for_hook = state.clone();
        bus.register(
            EVENT_SYNC_ALL_REQUESTED,
            "holidays",
            Arc::new(move |ctx: &mut HookContext| {
                on_sync_all_requested(state_for_hook.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/holidays")
                .app_data(state)
                .route("/providers", web::get().to(list_providers))
                .route("/providers", web::post().to(create_provider))
                .route("/providers/{id}", web::get().to(get_provider))
                .route("/providers/{id}", web::patch().to(update_provider))
                .route("/providers/{id}", web::delete().to(delete_provider))
                .route("/providers/{id}/test", web::post().to(test_provider))
                .route("/countries", web::get().to(list_countries))
                .route("/countries", web::post().to(create_country))
                .route("/countries/{code}", web::patch().to(update_country))
                .route("/countries/{code}", web::delete().to(delete_country))
                .route("/configs", web::get().to(list_configs))
                .route("/configs", web::post().to(create_config))
                .route("/configs/{id}", web::get().to(get_config))
                .route("/configs/{id}", web::patch().to(update_config))
                .route("/configs/{id}", web::delete().to(delete_config))
                .route("/configs/{id}/sync", web::post().to(sync_config))
                .route("/sync-all", web::post().to(sync_all_configs))
                .route("/sync-runs", web::get().to(list_sync_runs))
                .route("/sync-runs/{id}", web::get().to(get_sync_run))
                .route("", web::get().to(list_holidays))
                .route("/is-holiday", web::get().to(get_is_holiday))
                .route("/non-working-days", web::get().to(get_non_working_days))
                .route("/overrides", web::get().to(list_overrides))
                .route("/overrides", web::post().to(create_override))
                .route("/overrides/{id}", web::get().to(get_override))
                .route("/overrides/{id}", web::patch().to(update_override))
                .route("/overrides/{id}", web::delete().to(delete_override)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/holidays/providers", owner: "holidays", description: "List providers in the registry (paginated; filters: enabled, kind, search)." },
            RouteInfo { method: "POST", path: "/api/v1/holidays/providers", owner: "holidays", description: "Create a provider definition; api_key is write-only and stored encrypted." },
            RouteInfo { method: "GET", path: "/api/v1/holidays/providers/{id}", owner: "holidays", description: "Get one provider (masked view: key_configured and fingerprint only)." },
            RouteInfo { method: "PATCH", path: "/api/v1/holidays/providers/{id}", owner: "holidays", description: "Update a provider; api_key rotates the stored key, empty string clears it." },
            RouteInfo { method: "DELETE", path: "/api/v1/holidays/providers/{id}", owner: "holidays", description: "Delete a non-built-in provider not referenced by any sync config." },
            RouteInfo { method: "POST", path: "/api/v1/holidays/providers/{id}/test", owner: "holidays", description: "Test one provider fetch for a country and year (no storage)." },
            RouteInfo { method: "GET", path: "/api/v1/holidays/countries", owner: "holidays", description: "List ISO 3166-1 alpha-2 countries (paginated; filters: enabled, search)." },
            RouteInfo { method: "POST", path: "/api/v1/holidays/countries", owner: "holidays", description: "Add a country to the reference list." },
            RouteInfo { method: "PATCH", path: "/api/v1/holidays/countries/{code}", owner: "holidays", description: "Update a country (name, enabled)." },
            RouteInfo { method: "DELETE", path: "/api/v1/holidays/countries/{code}", owner: "holidays", description: "Delete a country not selected by any sync config." },
            RouteInfo { method: "GET", path: "/api/v1/holidays/configs", owner: "holidays", description: "List the tenant's sync configs (paginated; filters: provider_id, country, enabled)." },
            RouteInfo { method: "POST", path: "/api/v1/holidays/configs", owner: "holidays", description: "Create a tenant sync config (provider, fallback chain, countries, years, schedule)." },
            RouteInfo { method: "GET", path: "/api/v1/holidays/configs/{id}", owner: "holidays", description: "Get one sync config of the tenant." },
            RouteInfo { method: "PATCH", path: "/api/v1/holidays/configs/{id}", owner: "holidays", description: "Update a sync config of the tenant." },
            RouteInfo { method: "DELETE", path: "/api/v1/holidays/configs/{id}", owner: "holidays", description: "Delete a sync config of the tenant." },
            RouteInfo { method: "POST", path: "/api/v1/holidays/configs/{id}/sync", owner: "holidays", description: "Run a sync for one config now (primary provider, then the fallback chain)." },
            RouteInfo { method: "POST", path: "/api/v1/holidays/sync-all", owner: "holidays", description: "Run a sync for every enabled config of the tenant." },
            RouteInfo { method: "GET", path: "/api/v1/holidays/sync-runs", owner: "holidays", description: "List the tenant's sync runs (paginated; filters: config_id, provider_id, country, year, status)." },
            RouteInfo { method: "GET", path: "/api/v1/holidays/sync-runs/{id}", owner: "holidays", description: "Get one sync run of the tenant." },
            RouteInfo { method: "GET", path: "/api/v1/holidays", owner: "holidays", description: "List synced holidays visible to the tenant (paginated; filters: country, year, source, provider_id, search)." },
            RouteInfo { method: "GET", path: "/api/v1/holidays/is-holiday", owner: "holidays", description: "Is a date a holiday for the tenant (query: date, country)?" },
            RouteInfo { method: "GET", path: "/api/v1/holidays/non-working-days", owner: "holidays", description: "Non-working days in a range for the tenant (query: start, end, country; max 366 days)." },
            RouteInfo { method: "GET", path: "/api/v1/holidays/overrides", owner: "holidays", description: "List the tenant's manual overrides (paginated; filters: country, override_kind, year)." },
            RouteInfo { method: "POST", path: "/api/v1/holidays/overrides", owner: "holidays", description: "Create a manual override (day_off or working_day) for the tenant." },
            RouteInfo { method: "GET", path: "/api/v1/holidays/overrides/{id}", owner: "holidays", description: "Get one manual override of the tenant." },
            RouteInfo { method: "PATCH", path: "/api/v1/holidays/overrides/{id}", owner: "holidays", description: "Update a manual override of the tenant." },
            RouteInfo { method: "DELETE", path: "/api/v1/holidays/overrides/{id}", owner: "holidays", description: "Delete a manual override of the tenant." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(HolidaysModule::new()));
}

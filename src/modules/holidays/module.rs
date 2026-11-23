//! Module registration: struct, Module impl and register().

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, ProductCategory, RouteInfo, RouteVersionSupport};
use unified_core::tenancy::{schema_name_for_str, TenantContext as TenancyContext};
use unified_core::pagination::{PageResult, PaginationParams};
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

use super::*;
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
//     {"tenant_id", "audience": "role:admin", "title", "body", "kind",
//     "category", "module", "priority", "link"} — the audience is
//     what lets the notifications module resolve recipients; without
//     it the alert is never delivered), so a failed provider chain is
//     logged and alerted, never silently empty. Previously synced holidays
//     stay cached when a sync fails.
//
// Tenant scoping (standing rule — read before changing anything
// below). Sync configs, sync runs, and manual overrides are TENANT
// business data: every record carries `tenant_id` (uuidv7) and
// `company_id`, every lookup is filtered by the resolved tenant,
// and cross-tenant reads answer 404, never 403. The tenant context
// is resolved ONLY through `unified_core::tenancy::TenantContext`
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
// verified 2026-11-22 in crates/unified-core/src/tenancy.rs`: `schema_name()`,
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
// characters of the SHA-256). There is no fallback secret: when
// `app.secret_key` is unset, storing a provider key is refused and
// any sync needing a stored key fails closed.
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
        // No secret configured at boot means None, not a fallback:
        // storing a provider key is refused and syncs needing a
        // stored key fail closed (see `encryption_secret`).
        *self
            .state
            .encryption_secret
            .write()
            .expect("encryption secret lock poisoned") = encryption_secret(ctx).ok();
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
            RouteInfo { method: "GET", path: "/api/v1/holidays/providers", owner: "holidays", description: "List providers in the registry (paginated; filters: enabled, kind, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/holidays/providers", owner: "holidays", description: "Create a provider definition; api_key is write-only and stored encrypted.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/holidays/providers/{id}", owner: "holidays", description: "Get one provider (masked view: key_configured and fingerprint only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/holidays/providers/{id}", owner: "holidays", description: "Update a provider; api_key rotates the stored key, empty string clears it.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/holidays/providers/{id}", owner: "holidays", description: "Delete a non-built-in provider not referenced by any sync config.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/holidays/providers/{id}/test", owner: "holidays", description: "Test one provider fetch for a country and year (no storage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/holidays/countries", owner: "holidays", description: "List ISO 3166-1 alpha-2 countries (paginated; filters: enabled, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/holidays/countries", owner: "holidays", description: "Add a country to the reference list.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/holidays/countries/{code}", owner: "holidays", description: "Update a country (name, enabled).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/holidays/countries/{code}", owner: "holidays", description: "Delete a country not selected by any sync config.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/holidays/configs", owner: "holidays", description: "List the tenant's sync configs (paginated; filters: provider_id, country, enabled).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/holidays/configs", owner: "holidays", description: "Create a tenant sync config (provider, fallback chain, countries, years, schedule).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/holidays/configs/{id}", owner: "holidays", description: "Get one sync config of the tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/holidays/configs/{id}", owner: "holidays", description: "Update a sync config of the tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/holidays/configs/{id}", owner: "holidays", description: "Delete a sync config of the tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/holidays/configs/{id}/sync", owner: "holidays", description: "Run a sync for one config now (primary provider, then the fallback chain).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/holidays/sync-all", owner: "holidays", description: "Run a sync for every enabled config of the tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/holidays/sync-runs", owner: "holidays", description: "List the tenant's sync runs (paginated; filters: config_id, provider_id, country, year, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/holidays/sync-runs/{id}", owner: "holidays", description: "Get one sync run of the tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/holidays", owner: "holidays", description: "List synced holidays visible to the tenant (paginated; filters: country, year, source, provider_id, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/holidays/is-holiday", owner: "holidays", description: "Is a date a holiday for the tenant (query: date, country)?", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/holidays/non-working-days", owner: "holidays", description: "Non-working days in a range for the tenant (query: start, end, country; max 366 days).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/holidays/overrides", owner: "holidays", description: "List the tenant's manual overrides (paginated; filters: country, override_kind, year).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/holidays/overrides", owner: "holidays", description: "Create a manual override (day_off or working_day) for the tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/holidays/overrides/{id}", owner: "holidays", description: "Get one manual override of the tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/holidays/overrides/{id}", owner: "holidays", description: "Update a manual override of the tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/holidays/overrides/{id}", owner: "holidays", description: "Delete a manual override of the tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(HolidaysModule::new()));
}

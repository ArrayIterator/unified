//! Public data types, request/response shapes and their impls.

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
pub(crate) struct FetchedHoliday {
    pub(crate) date: NaiveDate,
    pub(crate) name: String,
    pub(crate) local_name: Option<String>,
    pub(crate) global: bool,
    pub(crate) types: Vec<String>,
    pub(crate) counties: Vec<String>,
}

/// Per-provider rate-limit state (in-memory only).
#[derive(Debug, Default)]
pub(crate) struct RateState {
    pub(crate) last_request: Option<Instant>,
    /// Request start instants inside the rolling one-minute window.
    pub(crate) window: VecDeque<Instant>,
}

/// Summary of one config sync, returned by the sync endpoints.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SyncSummary {
    pub(crate) config_id: Uuid,
    pub(crate) tenant_id: String,
    pub(crate) trigger: String,
    pub(crate) runs: Vec<Value>,
    pub(crate) attempted: usize,
    pub(crate) succeeded: usize,
    pub(crate) failed: usize,
    pub(crate) holidays_found: i64,
    pub(crate) holidays_stored: i64,
    pub(crate) scheduler_status: String,
}

// ---------------------------------------------------------------------------
// Request DTOs.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct CreateProviderRequest {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) base_url: String,
    pub(crate) allowed_hosts: Vec<String>,
    pub(crate) api_key_required: Option<bool>,
    /// Write-only: accepted once, encrypted, never returned.
    pub(crate) api_key: Option<String>,
    pub(crate) min_interval_secs: Option<u64>,
    pub(crate) max_requests_per_minute: Option<u32>,
    pub(crate) calendar_ids: Option<HashMap<String, String>>,
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateProviderRequest {
    pub(crate) name: Option<String>,
    pub(crate) allowed_hosts: Option<Vec<String>>,
    /// Write-only rotation: present + non-empty replaces the stored
    /// key; absent leaves it unchanged; empty string clears it.
    pub(crate) api_key: Option<String>,
    pub(crate) min_interval_secs: Option<u64>,
    pub(crate) max_requests_per_minute: Option<u32>,
    pub(crate) calendar_ids: Option<HashMap<String, String>>,
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TestProviderRequest {
    pub(crate) country: Option<String>,
    pub(crate) year: Option<i32>,
    /// Feed URL for a `generic_ical` test (validated with the SSRF
    /// rules; never stored by this endpoint).
    pub(crate) feed_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateCountryRequest {
    pub(crate) code: String,
    pub(crate) name: String,
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateCountryRequest {
    pub(crate) name: Option<String>,
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateConfigRequest {
    pub(crate) provider_id: Option<String>,
    pub(crate) fallback_provider_ids: Option<Vec<String>>,
    pub(crate) countries: Vec<String>,
    pub(crate) years: Option<Vec<i32>>,
    pub(crate) schedule: Option<String>,
    pub(crate) feed_url: Option<String>,
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateConfigRequest {
    pub(crate) provider_id: Option<String>,
    pub(crate) fallback_provider_ids: Option<Vec<String>>,
    pub(crate) countries: Option<Vec<String>>,
    pub(crate) years: Option<Vec<i32>>,
    pub(crate) schedule: Option<String>,
    /// Present + non-empty replaces the feed URL; empty string
    /// clears it; absent leaves it unchanged.
    pub(crate) feed_url: Option<String>,
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateOverrideRequest {
    pub(crate) country: String,
    pub(crate) date: String,
    pub(crate) name: String,
    pub(crate) override_kind: Option<String>,
    pub(crate) note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateOverrideRequest {
    pub(crate) name: Option<String>,
    pub(crate) override_kind: Option<String>,
    pub(crate) note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IsHolidayQuery {
    pub(crate) date: String,
    pub(crate) country: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NonWorkingDaysQuery {
    pub(crate) start: String,
    pub(crate) end: String,
    pub(crate) country: String,
}

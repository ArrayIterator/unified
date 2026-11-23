//! In-memory module state.

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
    pub(crate) rate_state: Arc<DashMap<String, Arc<Mutex<RateState>>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
    /// Encryption secret captured at `init` (a clone of
    /// `app.secret_key`) so scheduled syncs running in spawned
    /// tasks can decrypt provider keys without an `AppContext` at
    /// hand. `None` when no secret was configured at boot: there is
    /// no fallback secret, and any sync needing a stored key fails
    /// closed.
    pub(crate) encryption_secret: Arc<RwLock<Option<String>>>,
    /// Per-tenant locks serializing manual-override creation, so
    /// the (tenant, country, date) uniqueness check and the insert
    /// are atomic (see `insert_override_atomic`).
    override_locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
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
            override_locks: Arc::new(DashMap::new()),
        }
    }
}

impl HolidaysState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    /// The encryption secret captured at `init`, or `None` when no
    /// real secret was configured. Never a hard-coded fallback:
    /// callers that need a key fail closed on `None`.
    pub(crate) fn secret(&self) -> Option<String> {
        self.encryption_secret
            .read()
            .expect("encryption secret lock poisoned")
            .clone()
    }

    /// Insert a manual override, refusing when this tenant already
    /// has one for the same (country, date). The check and the
    /// insert run under a per-tenant lock, so two concurrent
    /// creates for the same date cannot both pass the check and
    /// insert duplicates. Returns false when an override already
    /// exists.
    pub(crate) fn insert_override_atomic(&self, entry: HolidayOverride) -> bool {
        let lock = self
            .override_locks
            .entry(entry.tenant_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .value()
            .clone();
        let _guard = lock.lock().expect("override lock poisoned");
        if self
            .tenant_override(&entry.tenant_id, &entry.country, entry.date)
            .is_some()
        {
            return false;
        }
        self.overrides.insert(entry.id, entry);
        true
    }

    /// Seed the provider registry and the country list on first
    /// init. Idempotent: only inserts when the key is absent, so a
    /// restart never overwrites an admin's edits.
    pub(crate) fn seed_defaults(&self) {
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
    pub(crate) fn tenant_countries(&self, tenant_id: &str) -> HashSet<String> {
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
    pub(crate) fn tenant_override(&self, tenant_id: &str, country: &str, date: NaiveDate) -> Option<HolidayOverride> {
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
    pub(crate) fn synced_on(&self, country: &str, date: NaiveDate) -> Option<Holiday> {
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
pub(crate) fn seed_providers(now: DateTime<Utc>) -> Vec<HolidayProvider> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_override(tenant_id: &str, date: NaiveDate) -> HolidayOverride {
        let now = Utc::now();
        HolidayOverride {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.to_string(),
            company_id: String::new(),
            country: "ID".to_string(),
            date,
            name: "Test Day".to_string(),
            override_kind: "day_off".to_string(),
            note: None,
            created_by: "tester".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn secret_is_none_without_a_configured_secret() {
        // No hard-coded fallback: a fresh state has no secret until
        // init captures app.secret_key.
        assert_eq!(HolidaysState::default().secret(), None);
    }

    #[test]
    fn insert_override_atomic_rejects_a_duplicate_date() {
        let state = HolidaysState::default();
        let date = NaiveDate::from_ymd_opt(2026, 12, 25).unwrap();
        assert!(state.insert_override_atomic(test_override("tenant-1", date)));
        assert!(!state.insert_override_atomic(test_override("tenant-1", date)));
        // Another tenant, or another date, is still fine.
        assert!(state.insert_override_atomic(test_override("tenant-2", date)));
        assert!(state.insert_override_atomic(test_override(
            "tenant-1",
            NaiveDate::from_ymd_opt(2026, 12, 26).unwrap()
        )));
        assert_eq!(state.overrides.len(), 3);
    }

    #[test]
    fn insert_override_atomic_is_safe_under_concurrency() {
        let state = HolidaysState::default();
        let date = NaiveDate::from_ymd_opt(2026, 12, 25).unwrap();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let state = state.clone();
            handles.push(std::thread::spawn(move || {
                state.insert_override_atomic(test_override("tenant-1", date))
            }));
        }
        let mut inserted = 0;
        for handle in handles {
            if handle.join().expect("override insert thread panicked") {
                inserted += 1;
            }
        }
        assert_eq!(inserted, 1);
        assert_eq!(state.overrides.len(), 1);
    }
}

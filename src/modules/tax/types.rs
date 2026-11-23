// Tax module — tax types and shared state.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One tax rule for a jurisdiction. `rate_percent` is a percentage
/// (11.0 means 11%). `threshold_amount` is the minimum base amount
/// the rule applies to, or `None` for no threshold. `inclusive`
/// means prices already include the tax; `compound` means an
/// exclusive rule is computed on the base plus earlier taxes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxRule {
    pub id: String,
    pub company_id: Uuid,
    pub country_code: String,
    #[serde(default)]
    pub region_code: Option<String>,
    pub name: String,
    pub tax_type: String,
    pub rate_percent: f64,
    /// Minimum base amount the rule applies to, in integer minor
    /// units; JSON carries major units. `None` means no threshold.
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub threshold_amount: Option<i64>,
    pub currency: String,
    pub inclusive: bool,
    pub compound: bool,
    pub effective_from: NaiveDate,
    #[serde(default)]
    pub effective_to: Option<NaiveDate>,
    pub sort_order: i32,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaxRule {
    pub country_code: String,
    pub region_code: Option<String>,
    pub name: String,
    pub tax_type: String,
    pub rate_percent: f64,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub threshold_amount: Option<i64>,
    pub currency: Option<String>,
    pub inclusive: Option<bool>,
    pub compound: Option<bool>,
    pub effective_from: String,
    pub effective_to: Option<String>,
    pub sort_order: Option<i32>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTaxRule {
    pub country_code: Option<String>,
    pub region_code: Option<String>,
    pub name: Option<String>,
    pub tax_type: Option<String>,
    pub rate_percent: Option<f64>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub threshold_amount: Option<i64>,
    pub currency: Option<String>,
    pub inclusive: Option<bool>,
    pub compound: Option<bool>,
    pub effective_from: Option<String>,
    pub effective_to: Option<String>,
    pub sort_order: Option<i32>,
    pub enabled: Option<bool>,
}

/// Per-company tax settings. One row per company.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxSettings {
    pub id: String,
    pub company_id: Uuid,
    pub fiscal_year_start_month: u32,
    pub fiscal_year_start_day: u32,
    pub filing_frequency: String,
    pub default_currency: String,
    #[serde(default)]
    pub default_country_code: Option<String>,
    #[serde(default)]
    pub default_region_code: Option<String>,
    pub reminder_days_before: u32,
    pub filing_due_days_after_period: u32,
    #[serde(default)]
    pub notify_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Default for TaxSettings {
    fn default() -> Self {
        Self {
            id: String::new(),
            company_id: Uuid::nil(),
            fiscal_year_start_month: 1,
            fiscal_year_start_day: 1,
            filing_frequency: "quarterly".to_string(),
            default_currency: "USD".to_string(),
            default_country_code: None,
            default_region_code: None,
            reminder_days_before: 7,
            filing_due_days_after_period: DEFAULT_DUE_DAYS_AFTER_PERIOD,
            notify_user_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpsertTaxSettings {
    pub fiscal_year_start_month: Option<u32>,
    pub fiscal_year_start_day: Option<u32>,
    pub filing_frequency: Option<String>,
    pub default_currency: Option<String>,
    pub default_country_code: Option<String>,
    pub default_region_code: Option<String>,
    pub reminder_days_before: Option<u32>,
    pub filing_due_days_after_period: Option<u32>,
    pub notify_user_id: Option<String>,
}

/// One line of a tax computation: the tax one rule contributes.
/// Money is integer minor units; JSON carries major units.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxLine {
    pub rule_id: String,
    pub rule_name: String,
    pub tax_type: String,
    pub rate_percent: f64,
    #[serde(with = "unified_core::money::serde_major")]
    pub base: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
    pub inclusive: bool,
    /// True only when the compound flag actually changed the base:
    /// compound applies to exclusive rules only.
    pub compound_applied: bool,
}

/// The result of the pure tax computation. Money is integer minor
/// units; JSON carries major units.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxBreakdown {
    /// The amount tax was computed over (net for exclusive rules,
    /// gross for inclusive rules).
    #[serde(with = "unified_core::money::serde_major")]
    pub base: i64,
    pub lines: Vec<TaxLine>,
    #[serde(with = "unified_core::money::serde_major")]
    pub tax_total: i64,
    /// Net + tax for exclusive rules; the gross amount itself for
    /// inclusive rules (the tax is already inside it).
    #[serde(with = "unified_core::money::serde_major")]
    pub grand_total: i64,
    pub currency: String,
}

/// One stored tax computation for an invoice, an order, or a manual
/// compute call. The source document is never mutated; this record
/// is the tax module's own view of what was computed for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxComputation {
    pub id: String,
    pub company_id: Uuid,
    /// `invoice`, `order`, or `manual`.
    pub source_type: String,
    pub source_id: String,
    #[serde(default)]
    pub country_code: Option<String>,
    #[serde(default)]
    pub region_code: Option<String>,
    pub currency: String,
    /// Base amount in integer minor units; JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub base_amount: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub tax_total: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub grand_total: i64,
    pub lines: Vec<TaxLine>,
    pub computed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComputeRequest {
    /// Amount to compute tax over, in integer minor units; JSON
    /// carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
    pub currency: Option<String>,
    pub country_code: Option<String>,
    pub region_code: Option<String>,
    /// Date the tax is computed for (`YYYY-MM-DD`); defaults to today.
    pub date: Option<String>,
    /// Optional source to attach the computation to.
    pub source_type: Option<String>,
    pub source_id: Option<String>,
}

/// One filing period for one jurisdiction of one company.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxFiling {
    pub id: String,
    pub company_id: Uuid,
    pub country_code: String,
    #[serde(default)]
    pub region_code: Option<String>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub frequency: String,
    pub due_date: NaiveDate,
    /// `upcoming`, `due`, `submitted`, or `overdue`.
    pub status: String,
    /// Tax total for the period in integer minor units; JSON
    /// carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub tax_total: i64,
    #[serde(default)]
    pub submitted_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub submitted_via: Option<String>,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub last_reminder_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaxFiling {
    pub country_code: String,
    pub region_code: Option<String>,
    pub period_start: String,
    pub period_end: String,
    pub frequency: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubmitTaxFiling {
    /// Connector to file through; when absent the filing is recorded
    /// as a manual submission.
    pub connector_id: Option<String>,
    /// Reference for a manual submission (for example a receipt
    /// number from the tax authority's portal).
    pub reference: Option<String>,
}

/// One tax reporting / filing provider connection. The credential
/// ciphertext fields are never serialized.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxConnector {
    pub id: String,
    pub company_id: Uuid,
    pub name: String,
    /// `generic_http` or `example`.
    pub kind: String,
    pub endpoint_url: String,
    pub allowed_hosts: Vec<String>,
    pub auth_scheme: String,
    #[serde(skip_serializing)]
    pub credential_encrypted: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_fingerprint: Option<String>,
    #[serde(skip_serializing)]
    pub signing_secret_encrypted: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_secret_fingerprint: Option<String>,
    pub timeout_ms: u64,
    pub max_retries: u32,
    pub enabled: bool,
    #[serde(default)]
    pub request_mapping: Value,
    #[serde(default)]
    pub response_mapping: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The public read view of a connector: ciphertext is never exposed,
/// only whether a credential is configured and its fingerprint.
#[derive(Debug, Clone, Serialize)]
pub struct TaxConnectorView {
    pub id: String,
    pub company_id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint_url: String,
    pub allowed_hosts: Vec<String>,
    pub auth_scheme: String,
    pub credential_configured: bool,
    pub credential_fingerprint: Option<String>,
    pub signing_configured: bool,
    pub signing_secret_fingerprint: Option<String>,
    pub timeout_ms: u64,
    pub max_retries: u32,
    pub enabled: bool,
    pub request_mapping: Value,
    pub response_mapping: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&TaxConnector> for TaxConnectorView {
    fn from(connector: &TaxConnector) -> Self {
        Self {
            id: connector.id.clone(),
            company_id: connector.company_id,
            name: connector.name.clone(),
            kind: connector.kind.clone(),
            endpoint_url: connector.endpoint_url.clone(),
            allowed_hosts: connector.allowed_hosts.clone(),
            auth_scheme: connector.auth_scheme.clone(),
            credential_configured: connector.credential_encrypted.is_some(),
            credential_fingerprint: connector.credential_fingerprint.clone(),
            signing_configured: connector.signing_secret_encrypted.is_some(),
            signing_secret_fingerprint: connector.signing_secret_fingerprint.clone(),
            timeout_ms: connector.timeout_ms,
            max_retries: connector.max_retries,
            enabled: connector.enabled,
            request_mapping: connector.request_mapping.clone(),
            response_mapping: connector.response_mapping.clone(),
            created_at: connector.created_at,
            updated_at: connector.updated_at,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaxConnector {
    pub name: String,
    pub kind: String,
    pub endpoint_url: String,
    pub allowed_hosts: Vec<String>,
    pub auth_scheme: Option<String>,
    /// Write-only: accepted once, stored encrypted, never returned.
    pub credential: Option<String>,
    /// Write-only signing secret for the `hmac` scheme.
    pub signing_secret: Option<String>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub enabled: Option<bool>,
    pub request_mapping: Option<Value>,
    pub response_mapping: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTaxConnector {
    pub name: Option<String>,
    pub endpoint_url: Option<String>,
    pub allowed_hosts: Option<Vec<String>>,
    pub auth_scheme: Option<String>,
    /// Write-only: when present it replaces the stored credential.
    pub credential: Option<String>,
    /// Write-only: when present it replaces the stored signing secret.
    pub signing_secret: Option<String>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub enabled: Option<bool>,
    pub request_mapping: Option<Value>,
    pub response_mapping: Option<Value>,
}

/// One entry of this module's own audit trail (`tax_audit` table).
/// `details` never carries a credential value, only fingerprints
/// and flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxAuditEntry {
    pub id: String,
    pub company_id: Uuid,
    pub actor: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub details: Value,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct TaxState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) rules: DashMap<String, TaxRule>,
    pub(crate) settings: DashMap<Uuid, TaxSettings>,
    pub(crate) computations: DashMap<String, TaxComputation>,
    pub(crate) filings: DashMap<String, TaxFiling>,
    pub(crate) connectors: DashMap<String, TaxConnector>,
    pub(crate) audit: DashMap<String, TaxAuditEntry>,
    /// One async mutex per filing id, serialising concurrent
    /// submissions of the same filing (see `filing_submit_lock`).
    pub(crate) filing_submits: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
}

impl TaxState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            rules: DashMap::new(),
            settings: DashMap::new(),
            computations: DashMap::new(),
            filings: DashMap::new(),
            connectors: DashMap::new(),
            audit: DashMap::new(),
            filing_submits: DashMap::new(),
        }
    }

    /// The mutex serialising submissions of one filing. The same
    /// filing id always returns the same mutex, so holding it
    /// across the status check, the connector call, and the status
    /// update makes a submit atomic against concurrent submits of
    /// that filing. Different filings get different mutexes and
    /// submit in parallel.
    pub(crate) fn filing_submit_lock(&self, filing_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.filing_submits
            .entry(filing_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .value()
            .clone()
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Record a change in this module's own audit trail and emit the
    /// matching audit-shaped `tax.*` event for the audit module.
    /// `details` must never contain a credential value.
    pub(crate) fn record_audit(
        &self,
        company_id: Uuid,
        actor: &str,
        action: &str,
        resource_type: &str,
        resource_id: &str,
        event: &str,
        details: Value,
    ) {
        let entry = TaxAuditEntry {
            id: new_id(),
            company_id,
            actor: actor.to_string(),
            action: action.to_string(),
            resource_type: resource_type.to_string(),
            resource_id: resource_id.to_string(),
            details: details.clone(),
            created_at: now(),
        };
        self.audit.insert(entry.id.clone(), entry);
        let mut payload = match details {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        payload.insert("id".to_string(), Value::String(resource_id.to_string()));
        payload.insert("actor_id".to_string(), Value::String(actor.to_string()));
        payload.insert(
            "resource".to_string(),
            Value::String(format!("{resource_type}:{resource_id}")),
        );
        payload.insert(
            "company_id".to_string(),
            Value::String(company_id.to_string()),
        );
        self.emit(event, Value::Object(payload));
    }

    /// Rules for one company and jurisdiction that apply on `on`:
    /// enabled, effective, and above their threshold. Ordered by
    /// `sort_order`, then creation time, which is the order compound
    /// taxes stack in.
    ///
    /// Region semantics: a national rule (`region_code: None`)
    /// applies everywhere, in every region and when no region is
    /// given. A region-specific rule applies only when the caller
    /// names that region — a caller with no region never matches
    /// a region-specific rule, and a caller naming another region
    /// never matches it either.
    pub(crate) fn applicable_rules(
        &self,
        company_id: Uuid,
        country_code: Option<&str>,
        region_code: Option<&str>,
        amount: i64,
        on: NaiveDate,
    ) -> Vec<TaxRule> {
        let mut rules: Vec<TaxRule> = self
            .rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                rule.company_id == company_id
                    && rule.enabled
                    && rule.effective_from <= on
                    && rule.effective_to.is_none_or(|until| on <= until)
                    && rule
                        .threshold_amount
                        .is_none_or(|threshold| amount >= threshold)
                    && country_code
                        .is_none_or(|country| rule.country_code.eq_ignore_ascii_case(country))
                    && rule.region_code.as_deref().is_none_or(|rule_region| {
                        region_code.is_some_and(|region| rule_region.eq_ignore_ascii_case(region))
                    })
            })
            .collect();
        rules.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(a.created_at.cmp(&b.created_at))
        });
        rules
    }

    pub(crate) fn settings_for(&self, company_id: Uuid) -> TaxSettings {
        self.settings
            .get(&company_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| TaxSettings {
                id: new_id(),
                company_id,
                ..TaxSettings::default()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, region_code: Option<&str>) -> TaxRule {
        TaxRule {
            id: id.to_string(),
            company_id: Uuid::nil(),
            country_code: "ID".to_string(),
            region_code: region_code.map(str::to_string),
            name: "VAT".to_string(),
            tax_type: "vat".to_string(),
            rate_percent: 11.0,
            threshold_amount: None,
            currency: "IDR".to_string(),
            inclusive: false,
            compound: false,
            effective_from: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            effective_to: None,
            sort_order: 0,
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn on() -> NaiveDate {
        NaiveDate::from_ymd_opt(2025, 6, 1).unwrap()
    }

    fn ids(rules: &[TaxRule]) -> Vec<&str> {
        rules.iter().map(|rule| rule.id.as_str()).collect()
    }

    #[test]
    fn applicable_rules_national_rules_apply_everywhere_region_rules_only_on_match() {
        // Regression (T3): a caller with no region used to match
        // region-specific rules too, and the semantics were
        // ambiguous. National rules apply everywhere; region rules
        // apply only when the region matches.
        let state = TaxState::new();
        state
            .rules
            .insert("national".to_string(), rule("national", None));
        state.rules.insert("jk".to_string(), rule("jk", Some("JK")));
        state.rules.insert("jb".to_string(), rule("jb", Some("JB")));

        let no_region = state.applicable_rules(Uuid::nil(), Some("ID"), None, 10000, on());
        assert_eq!(ids(&no_region), vec!["national"]);
        let jakarta = state.applicable_rules(Uuid::nil(), Some("ID"), Some("JK"), 10000, on());
        assert_eq!(ids(&jakarta), vec!["national", "jk"]);
        let bandung = state.applicable_rules(Uuid::nil(), Some("ID"), Some("JB"), 10000, on());
        assert_eq!(ids(&bandung), vec!["national", "jb"]);
        // Region match is case-insensitive, like the country match.
        let lower = state.applicable_rules(Uuid::nil(), Some("ID"), Some("jk"), 10000, on());
        assert!(lower.iter().any(|rule| rule.id == "jk"));
    }

    #[test]
    fn filing_submit_lock_is_shared_per_filing_and_distinct_across_filings() {
        // Regression (T5): concurrent submits of one filing could
        // both pass the "already submitted" check. The per-filing
        // mutex is what serialises them, so the same filing id
        // must always resolve to the same mutex.
        let state = TaxState::new();
        let first = state.filing_submit_lock("filing-1");
        let again = state.filing_submit_lock("filing-1");
        assert!(Arc::ptr_eq(&first, &again));
        let other = state.filing_submit_lock("filing-2");
        assert!(!Arc::ptr_eq(&first, &other));
    }
}

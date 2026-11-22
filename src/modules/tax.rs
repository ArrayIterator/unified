// Tax Management module.
//
// Tax rules per jurisdiction (country / region code, rate, thresholds,
// effective dates, inclusive vs exclusive, compound flag), per-company
// settings (rates, thresholds, fiscal year start, filing frequency),
// pure tax computation, tax reports, filing reminders, and a generic
// HTTP connector framework for tax reporting / filing providers.
//
// Company scoping: every record belongs to one company (`company_id`).
// Callers identify themselves with the `X-User-Id` / `X-Role` headers
// and the company they act for with `X-Company-Id` (falling back to
// the `user.company_requested` hook, the same shape the AI module
// uses). A caller whose company differs from the target company's
// records is refused with 403. Capability checks run on every route
// through the `role.check` hook and fail closed: `tax:view` for reads,
// `tax:manage` for rule / settings / filing writes, `tax:file` for
// filing submission, `tax:manage_connectors` for connector writes.
//
// Interconnection happens only through the hook bus — this module
// never calls another module directly.
//
// Events emitted:
//   - `tax.rule_created`, `tax.rule_updated`, `tax.rule_deleted`
//   - `tax.computed` — after a tax computation, with the source id
//     and the line-by-line breakdown. The source document (invoice
//     or order) is never mutated.
//   - `tax.settings_updated`
//   - `tax.filing_created`, `tax.filing_due`, `tax.filing_submitted`
//   - `tax.connector_created`, `tax.connector_updated`,
//     `tax.connector_deleted`, `tax.connector_tested`
//   - Audit-shaped events: every mutation also emits its `tax.*`
//     event with `actor_id`, `resource`, and `id` in the payload, in
//     the same shape the AI module's `emit_audit` uses. The audit
//     module only records events listed in its `AUDITED_EVENTS`, so
//     the integrator must add the `tax.*` names there (see
//     INTEGRATION_NOTES_BATCH3_B3.md). Every change is also recorded
//     in this module's own `tax_audit` table, which is always
//     available regardless of the audit module's list.
//
// Events consumed:
//   - `invoice.created` — the real event the billing module emits
//     when an invoice is created. The brief also names
//     `billing.invoice_created`; that name does not exist in the
//     billing module, so this module subscribes to both spellings
//     and de-duplicates by source id, so a computation is stored
//     exactly once no matter which spelling arrives.
//   - `order.created` — the real event the store / billing flow
//     emits. `billing.order_created` is subscribed as an alias in
//     the same way.
//   - `tax.filing.check_requested` — the filing-reminder check. The
//     scheduler module only dispatches its own fixed job kinds, so
//     there is no scheduler kind to register for this check; the
//     check runs through this event and through
//     `POST /api/v1/tax/filings/check-due`.
//
// Credentials are write-only, following the AI module's documented
// pattern: a connector credential (API key or signing secret) is
// accepted once, encrypted with a key derived from
// `app.secret_key`, and stored only as ciphertext. The encryption is
// a versioned (`v1.`) HMAC keystream with an HMAC tag — not an AEAD
// cipher — because no AEAD crate is in `Cargo.toml` yet; when one is
// added, replace `encrypt_tax_credential` / `decrypt_tax_credential`
// and bump the version prefix so old values can be re-encrypted on
// next write. No response, log line, hook payload, or audit record
// ever contains a credential or a fragment of one; read views expose
// only `credential_configured` and a short fingerprint prefix.
//
// Connector SSRF protection: an endpoint URL must use HTTPS, must
// carry no userinfo, and its host must be listed in the connector's
// `allowed_hosts`. The host is resolved and every resolved address
// is rejected when it is private, loopback, link-local, multicast,
// unspecified, broadcast, benchmarking, or reserved. The check runs
// at connector create / update time and again immediately before
// every outbound call.
//
// Money: amounts are `f64` rounded to 4 decimal places in Rust
// (matching the billing module's `f64` convention) and
// `NUMERIC(19,4)` in SQL. Rates are percentages (11.0 means 11%).
//
// Reports are exportable as JSON and as CSV. CSV is produced
// directly by the export endpoint; the import/export module's
// `export.requested` event is not used because this module's export
// is a synchronous read of its own in-memory store.
//
// The in-memory DashMap store is authoritative; the migration
// `2026-11-22-000038_tax` is the persistence target (see TODO.md
// conventions in the other modules).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Capabilities checked through the `role.check` hook.
pub const PERM_VIEW: &str = "tax:view";
pub const PERM_MANAGE: &str = "tax:manage";
pub const PERM_FILE: &str = "tax:file";
pub const PERM_MANAGE_CONNECTORS: &str = "tax:manage_connectors";

/// Tax types a rule may carry.
const TAX_TYPES: &[&str] = &[
    "vat",
    "gst",
    "sales",
    "use",
    "withholding",
    "excise",
    "other",
];

/// Filing frequencies a company may configure.
const FILING_FREQUENCIES: &[&str] = &["monthly", "quarterly", "yearly"];

/// Connector kinds. `example` is a clearly-marked example provider
/// mapping: it never performs a network call and its endpoint and
/// field names are obviously fake.
const CONNECTOR_KINDS: &[&str] = &["generic_http", "example"];

/// Authentication schemes a generic HTTP connector may use.
const AUTH_SCHEMES: &[&str] = &["none", "bearer", "api_key", "hmac"];

/// Development fallback when `app.secret_key` is not configured.
/// Production config validation requires a real secret key.
const DEV_ENCRYPTION_SECRET: &str = "unified-tax-development-secret";

/// Maximum bytes of a provider response body returned by the test
/// endpoint (a preview only; the full body is never stored).
const RESPONSE_PREVIEW_LIMIT: usize = 2048;

/// Default days after a period ends before its filing is due.
const DEFAULT_DUE_DAYS_AFTER_PERIOD: u32 = 20;

// ---------------------------------------------------------------------------
// Credential encryption (write-only storage)
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

/// Encrypt a connector credential. Output format:
/// `v1.<base64url(nonce || ciphertext || tag)>`. The tag covers
/// nonce + ciphertext.
fn encrypt_tax_credential(secret: &str, plaintext: &str) -> String {
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

/// Decrypt a value produced by `encrypt_tax_credential`. The error
/// message never includes any part of the stored value.
fn decrypt_tax_credential(secret: &str, stored: &str) -> UnifiedResult<String> {
    let encoded = stored
        .strip_prefix("v1.")
        .ok_or_else(|| UnifiedError::internal("Stored tax credential has an unknown format"))?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| UnifiedError::internal("Stored tax credential cannot be decoded"))?;
    if blob.len() < 16 + 32 {
        return Err(UnifiedError::internal(
            "Stored tax credential cannot be decrypted",
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
            "Stored tax credential failed authentication",
        ));
    }
    let stream = keystream(&key, nonce, ciphertext.len());
    let plaintext: Vec<u8> = ciphertext
        .iter()
        .zip(stream.iter())
        .map(|(cipher, mask)| cipher ^ mask)
        .collect();
    String::from_utf8(plaintext)
        .map_err(|_| UnifiedError::internal("Stored tax credential cannot be decrypted"))
}

/// Short fingerprint prefix of a credential, for the masked read
/// view. Only the first 12 hex characters of the SHA-256 are
/// exposed, never the credential itself.
fn credential_fingerprint(plaintext: &str) -> String {
    sha256_hex_of(plaintext.as_bytes()).chars().take(12).collect()
}

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
    #[serde(default)]
    pub threshold_amount: Option<f64>,
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
    pub threshold_amount: Option<f64>,
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
    pub threshold_amount: Option<f64>,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxLine {
    pub rule_id: String,
    pub rule_name: String,
    pub tax_type: String,
    pub rate_percent: f64,
    pub base: f64,
    pub amount: f64,
    pub inclusive: bool,
    /// True only when the compound flag actually changed the base:
    /// compound applies to exclusive rules only.
    pub compound_applied: bool,
}

/// The result of the pure tax computation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxBreakdown {
    /// The amount tax was computed over (net for exclusive rules,
    /// gross for inclusive rules).
    pub base: f64,
    pub lines: Vec<TaxLine>,
    pub tax_total: f64,
    /// Net + tax for exclusive rules; the gross amount itself for
    /// inclusive rules (the tax is already inside it).
    pub grand_total: f64,
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
    pub base_amount: f64,
    pub tax_total: f64,
    pub grand_total: f64,
    pub lines: Vec<TaxLine>,
    pub computed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComputeRequest {
    pub amount: f64,
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
    pub tax_total: f64,
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
    hooks: RwLock<Arc<HookBus>>,
    rules: DashMap<String, TaxRule>,
    settings: DashMap<Uuid, TaxSettings>,
    computations: DashMap<String, TaxComputation>,
    filings: DashMap<String, TaxFiling>,
    connectors: DashMap<String, TaxConnector>,
    audit: DashMap<String, TaxAuditEntry>,
}

impl TaxState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            rules: DashMap::new(),
            settings: DashMap::new(),
            computations: DashMap::new(),
            filings: DashMap::new(),
            connectors: DashMap::new(),
            audit: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Record a change in this module's own audit trail and emit the
    /// matching audit-shaped `tax.*` event for the audit module.
    /// `details` must never contain a credential value.
    fn record_audit(
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
    fn applicable_rules(
        &self,
        company_id: Uuid,
        country_code: Option<&str>,
        region_code: Option<&str>,
        amount: f64,
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
                    && country_code.is_none_or(|country| {
                        rule.country_code.eq_ignore_ascii_case(country)
                    })
                    && region_code.is_none_or(|region| {
                        rule.region_code
                            .as_deref()
                            .is_none_or(|rule_region| rule_region.eq_ignore_ascii_case(region))
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

    fn settings_for(&self, company_id: Uuid) -> TaxSettings {
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

// ---------------------------------------------------------------------------
// Pure computation
// ---------------------------------------------------------------------------

/// Round money to 4 decimal places (`NUMERIC(19,4)`).
fn round_tax(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Pure tax computation over (`amount`, rules, `on`).
///
/// Exclusive rules add tax on top of the amount; a compound
/// exclusive rule is computed on the amount plus earlier exclusive
/// taxes. Inclusive rules extract the tax already inside the amount:
/// `tax = gross - gross / (1 + rate)`. A compound flag on an
/// inclusive rule has no effect — the line reports
/// `compound_applied: false` — because extracting twice from the
/// same gross amount would double-count the tax.
pub fn compute_tax_breakdown(amount: f64, rules: &[TaxRule], on: NaiveDate) -> TaxBreakdown {
    let amount = round_tax(amount);
    let mut lines: Vec<TaxLine> = Vec::new();
    let mut exclusive_tax_total = 0.0;
    let mut inclusive_tax_total = 0.0;
    for rule in rules {
        if rule.effective_from > on || rule.effective_to.is_some_and(|until| on > until) {
            continue;
        }
        let rate = rule.rate_percent / 100.0;
        if rule.inclusive {
            let tax = round_tax(amount - amount / (1.0 + rate));
            inclusive_tax_total += tax;
            lines.push(TaxLine {
                rule_id: rule.id.clone(),
                rule_name: rule.name.clone(),
                tax_type: rule.tax_type.clone(),
                rate_percent: rule.rate_percent,
                base: amount,
                amount: tax,
                inclusive: true,
                compound_applied: false,
            });
        } else {
            let compound_applied = rule.compound;
            let base = if compound_applied {
                round_tax(amount + exclusive_tax_total)
            } else {
                amount
            };
            let tax = round_tax(base * rate);
            exclusive_tax_total += tax;
            lines.push(TaxLine {
                rule_id: rule.id.clone(),
                rule_name: rule.name.clone(),
                tax_type: rule.tax_type.clone(),
                rate_percent: rule.rate_percent,
                base,
                amount: tax,
                inclusive: false,
                compound_applied,
            });
        }
    }
    let tax_total = round_tax(exclusive_tax_total + inclusive_tax_total);
    let has_inclusive = lines.iter().any(|line| line.inclusive);
    let grand_total = if has_inclusive && exclusive_tax_total == 0.0 {
        amount
    } else if has_inclusive {
        round_tax(amount + exclusive_tax_total)
    } else {
        round_tax(amount + tax_total)
    };
    TaxBreakdown {
        base: amount,
        lines,
        tax_total,
        grand_total,
        currency: String::new(),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn today() -> NaiveDate {
    Utc::now().date_naive()
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

/// The caller, from the headers every module in this codebase uses:
/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// or id the capability check runs against.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
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
    Ok(Caller { actor, role })
}

/// The company the caller acts for: `X-Company-Id` first, then the
/// `user.company_requested` hook answered by the users module.
fn resolve_caller_company(ctx: &AppContext, req: &HttpRequest, caller: &Caller) -> Option<Uuid> {
    if let Some(raw) = req
        .headers()
        .get("x-company-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if let Ok(id) = Uuid::parse_str(raw) {
            return Some(id);
        }
    }
    let hook = ctx
        .hooks
        .emit("user.company_requested", json!({ "user_id": caller.actor }));
    hook.get("company_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
}

/// Company scoping for writes: a caller who belongs to a company may
/// only manage records of that company. A caller with no resolvable
/// company is a platform admin and may manage any company.
fn require_company_scope(caller_company: Option<Uuid>, target_company: Uuid) -> UnifiedResult<()> {
    match caller_company {
        Some(company) if company != target_company => Err(UnifiedError::forbidden(
            "A company admin may only manage records of their own company",
        )),
        _ => Ok(()),
    }
}

/// True when the company module confirms the company exists, through
/// the `company.exists` hook. Fail-closed when the hook is answered;
/// when no handler is registered the company module is absent and
/// the check is skipped.
fn company_exists(ctx: &AppContext, company_id: Uuid) -> bool {
    if !ctx.hooks.has("company.exists") {
        return true;
    }
    let answer = ctx.hooks.emit(
        "company.exists",
        json!({ "company_id": company_id.to_string() }),
    );
    answer
        .results
        .first()
        .and_then(|result| result.get("exists"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn require_company(ctx: &AppContext, company_id: Uuid) -> UnifiedResult<()> {
    if company_exists(ctx, company_id) {
        Ok(())
    } else {
        Err(UnifiedError::not_found(format!(
            "Company {company_id} does not exist"
        )))
    }
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

/// Resolve the acting company for a request: the caller must name a
/// company (header or hook), and it must exist.
fn acting_company(
    ctx: &AppContext,
    req: &HttpRequest,
    caller: &Caller,
) -> UnifiedResult<Uuid> {
    let company_id = resolve_caller_company(ctx, req, caller).ok_or_else(|| {
        UnifiedError::bad_request(
            "Missing company: send X-Company-Id or belong to a company resolvable through user.company_requested",
        )
    })?;
    require_company(ctx, company_id)?;
    Ok(company_id)
}

fn parse_date(value: &str, field: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be a date (YYYY-MM-DD)")))
}

fn valid_currency(code: &str) -> bool {
    code.len() == 3
        && code
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
}

fn valid_country_code(code: &str) -> bool {
    code.len() == 2 && code.chars().all(|ch| ch.is_ascii_alphabetic())
}

fn valid_region_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 16
        && code
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn escape_csv(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

// ---------------------------------------------------------------------------
// Connector endpoint validation (SSRF protection) and signing
// ---------------------------------------------------------------------------

/// True when the address must never be a connector target: private,
/// loopback, link-local, multicast, unspecified, broadcast,
/// benchmarking, or reserved ranges.
fn address_is_forbidden(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_benchmarking()
                || ip.is_reserved()
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip.is_unique_local()
        }
    }
}

/// Validate a connector endpoint URL: HTTPS only, no userinfo, host
/// present and listed in `allowed_hosts` (exact match, case
/// insensitive), and every address the host resolves to must be a
/// public address. Hostnames `localhost` and `*.local` are rejected
/// outright, and so is any literal IP that is not public.
fn validate_endpoint_url(endpoint: &str, allowed_hosts: &[String]) -> UnifiedResult<Url> {
    let url = Url::parse(endpoint.trim())
        .map_err(|_| UnifiedError::bad_request("endpoint_url must be a valid URL"))?;
    if url.scheme() != "https" {
        return Err(UnifiedError::bad_request(
            "endpoint_url must use https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "endpoint_url must not contain userinfo",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| UnifiedError::bad_request("endpoint_url must have a host"))?;
    let host_lower = host.to_lowercase();
    if host_lower == "localhost" || host_lower.ends_with(".local") {
        return Err(UnifiedError::bad_request(
            "endpoint_url host must not be localhost or a .local name",
        ));
    }
    if !allowed_hosts
        .iter()
        .any(|allowed| allowed.trim().eq_ignore_ascii_case(&host_lower))
    {
        return Err(UnifiedError::bad_request(
            "endpoint_url host is not in the connector allowed_hosts list",
        ));
    }
    if let Ok(addr) = host.parse::<IpAddr>() {
        if address_is_forbidden(addr) {
            return Err(UnifiedError::bad_request(
                "endpoint_url host must resolve to a public address",
            ));
        }
        return Ok(url);
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let resolved = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|_| {
            UnifiedError::bad_request("endpoint_url host could not be resolved")
        })?;
    let mut any = false;
    for socket in resolved {
        any = true;
        if address_is_forbidden(socket.ip()) {
            return Err(UnifiedError::bad_request(
                "endpoint_url host resolves to a non-public address",
            ));
        }
    }
    if !any {
        return Err(UnifiedError::bad_request(
            "endpoint_url host could not be resolved",
        ));
    }
    Ok(url)
}

/// Sign a request body for the `hmac` scheme: HMAC-SHA256 over
/// `timestamp + "." + body`, hex-encoded. The timestamp is Unix
/// seconds, so a replayed request expires with the provider's own
/// window.
fn sign_body(secret: &str, timestamp: i64, body: &str) -> String {
    let message = format!("{timestamp}.{body}");
    let tag = hmac_sha256(secret.as_bytes(), message.as_bytes());
    tag.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Read a dotted path (`a.b.c`) out of a JSON value.
fn read_dotted_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

/// Apply a response mapping (`{"output_field": "source.path"}`) to a
/// provider response, returning the renamed fields that resolved.
fn apply_response_mapping(mapping: &Value, response: &Value) -> Value {
    let Some(map) = mapping.as_object() else {
        return json!({});
    };
    let mut out = serde_json::Map::new();
    for (output_field, source) in map {
        if let Some(path) = source.as_str() {
            if let Some(value) = read_dotted_path(response, path) {
                out.insert(output_field.clone(), value.clone());
            }
        }
    }
    Value::Object(out)
}

/// Apply a request mapping (`{"provider_field": "local_field"}`) to a
/// local payload, renaming the fields that are present.
fn apply_request_mapping(mapping: &Value, payload: &Value) -> Value {
    let Some(map) = mapping.as_object() else {
        return payload.clone();
    };
    let mut out = match payload.as_object() {
        Some(object) => object.clone(),
        None => serde_json::Map::new(),
    };
    for (provider_field, local_field) in map {
        if let Some(local) = local_field.as_str() {
            if let Some(value) = payload.get(local) {
                out.insert(provider_field.clone(), value.clone());
                out.remove(local);
            }
        }
    }
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// Filing periods
// ---------------------------------------------------------------------------

fn last_day_of_month(year: i32, month: u32) -> NaiveDate {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .map(|first| first.pred_opt().unwrap_or(first))
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(year, month, 28).unwrap_or(today()))
}

/// The period containing `on` for a filing frequency, using the
/// company's fiscal year start for yearly filings.
fn period_for(on: NaiveDate, frequency: &str, settings: &TaxSettings) -> (NaiveDate, NaiveDate) {
    match frequency {
        "monthly" => {
            let start = NaiveDate::from_ymd_opt(on.year(), on.month(), 1).unwrap_or(on);
            (start, last_day_of_month(on.year(), on.month()))
        }
        "yearly" => {
            let start_month = settings.fiscal_year_start_month;
            let start_day = settings.fiscal_year_start_day.min(28);
            let (start_year, end_year) = if (on.month(), on.day()) < (start_month, start_day) {
                (on.year() - 1, on.year())
            } else {
                (on.year(), on.year() + 1)
            };
            let start = NaiveDate::from_ymd_opt(start_year, start_month, start_day).unwrap_or(on);
            let end = NaiveDate::from_ymd_opt(end_year, start_month, start_day)
                .and_then(|date| date.pred_opt())
                .unwrap_or(on);
            (start, end)
        }
        _ => {
            // Quarterly: calendar quarters.
            let quarter_start_month = ((on.month() - 1) / 3) * 3 + 1;
            let start =
                NaiveDate::from_ymd_opt(on.year(), quarter_start_month, 1).unwrap_or(on);
            let end_month = quarter_start_month + 2;
            (start, last_day_of_month(on.year(), end_month))
        }
    }
}

/// Shift a period one step back for its frequency.
fn previous_period(
    start: NaiveDate,
    _end: NaiveDate,
    frequency: &str,
    settings: &TaxSettings,
) -> (NaiveDate, NaiveDate) {
    let before = start.pred_opt().unwrap_or(start);
    period_for(before, frequency, settings)
}

/// Tax total of stored computations for one company, jurisdiction,
/// and period (inclusive of both ends).
fn period_tax_total(
    state: &TaxState,
    company_id: Uuid,
    country_code: &str,
    region_code: Option<&str>,
    start: NaiveDate,
    end: NaiveDate,
) -> f64 {
    let mut total = 0.0;
    for entry in state.computations.iter() {
        let computation = entry.value();
        if computation.company_id != company_id {
            continue;
        }
        if computation.country_code.as_deref() != Some(country_code) {
            continue;
        }
        if region_code.is_some() && computation.region_code.as_deref() != region_code {
            continue;
        }
        let day = computation.computed_at.date_naive();
        if day >= start && day <= end {
            total += computation.tax_total;
        }
    }
    round_tax(total)
}

// ---------------------------------------------------------------------------
// Shared compute + store
// ---------------------------------------------------------------------------

/// Compute tax for a source document and store the computation.
/// Re-computing the same (`source_type`, `source_id`) replaces the
/// stored row instead of duplicating it, so a billing event that
/// arrives twice (real name and alias spelling) stores exactly one
/// computation. The source document itself is never mutated.
fn compute_and_store(
    state: &TaxState,
    company_id: Uuid,
    source_type: &str,
    source_id: &str,
    amount: f64,
    currency: &str,
    country_code: Option<&str>,
    region_code: Option<&str>,
    on: NaiveDate,
) -> TaxComputation {
    let rules = state.applicable_rules(company_id, country_code, region_code, amount, on);
    let mut breakdown = compute_tax_breakdown(amount, &rules, on);
    breakdown.currency = currency.to_string();
    let existing_id = state
        .computations
        .iter()
        .find(|entry| {
            entry.value().source_type == source_type && entry.value().source_id == source_id
        })
        .map(|entry| entry.key().clone());
    let id = existing_id.unwrap_or_else(new_id);
    let computation = TaxComputation {
        id: id.clone(),
        company_id,
        source_type: source_type.to_string(),
        source_id: source_id.to_string(),
        country_code: country_code.map(str::to_string),
        region_code: region_code.map(str::to_string),
        currency: currency.to_string(),
        base_amount: breakdown.base,
        tax_total: breakdown.tax_total,
        grand_total: breakdown.grand_total,
        lines: breakdown.lines.clone(),
        computed_at: now(),
        created_at: now(),
        updated_at: now(),
    };
    state.computations.insert(id, computation.clone());
    state.emit(
        "tax.computed",
        json!({
            "computation_id": computation.id,
            "company_id": company_id.to_string(),
            "source_type": source_type,
            "source_id": source_id,
            "country_code": country_code,
            "region_code": region_code,
            "currency": currency,
            "base_amount": computation.base_amount,
            "tax_total": computation.tax_total,
            "grand_total": computation.grand_total,
            "lines": computation.lines,
        }),
    );
    computation
}

// ---------------------------------------------------------------------------
// Handlers: rules
// ---------------------------------------------------------------------------

async fn list_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let auth: UnifiedResult<Uuid> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        acting_company(&ctx, &req, &caller)
    })();
    let company_id = match auth {
        Ok(company_id) => company_id,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let mut items: Vec<TaxRule> = state
        .rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| {
            rule.company_id == company_id
                && matches_search(
                    &format!("{} {} {} {}", rule.name, rule.country_code, rule.tax_type, rule.currency),
                    &params,
                )
                && params
                    .filters
                    .get("country_code")
                    .is_none_or(|country| rule.country_code.eq_ignore_ascii_case(country))
                && params
                    .filters
                    .get("region_code")
                    .is_none_or(|region| {
                        rule.region_code
                            .as_deref()
                            .is_none_or(|rule_region| rule_region.eq_ignore_ascii_case(region))
                    })
                && params
                    .filters
                    .get("tax_type")
                    .is_none_or(|kind| rule.tax_type == *kind)
                && params
                    .filters
                    .get("enabled")
                    .is_none_or(|flag| rule.enabled == (flag == "true"))
        })
        .collect();
    items.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then(a.country_code.cmp(&b.country_code))
            .then(a.created_at.cmp(&b.created_at))
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    body: web::Json<CreateTaxRule>,
) -> HttpResponse {
    let result: UnifiedResult<TaxRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        if !valid_country_code(input.country_code.trim()) {
            return Err(UnifiedError::bad_request(
                "country_code must be a 2-letter ISO code",
            ));
        }
        let region_code = input
            .region_code
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(region) = &region_code {
            if !valid_region_code(region) {
                return Err(UnifiedError::bad_request(
                    "region_code must be at most 16 letters, digits, hyphens, or underscores",
                ));
            }
        }
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("name must not be empty"));
        }
        if !TAX_TYPES.contains(&input.tax_type.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "tax_type must be one of: {}",
                TAX_TYPES.join(", ")
            )));
        }
        if !(0.0..=100.0).contains(&input.rate_percent) || !input.rate_percent.is_finite() {
            return Err(UnifiedError::bad_request(
                "rate_percent must be a finite number between 0 and 100",
            ));
        }
        if let Some(threshold) = input.threshold_amount {
            if threshold < 0.0 || !threshold.is_finite() {
                return Err(UnifiedError::bad_request(
                    "threshold_amount must be zero or greater",
                ));
            }
        }
        let currency = input
            .currency
            .unwrap_or_else(|| "USD".to_string())
            .trim()
            .to_uppercase();
        if !valid_currency(&currency) {
            return Err(UnifiedError::bad_request(
                "currency must be a 3-letter code",
            ));
        }
        let effective_from = parse_date(&input.effective_from, "effective_from")?;
        let effective_to = input
            .effective_to
            .map(|value| parse_date(&value, "effective_to"))
            .transpose()?;
        if let Some(until) = effective_to {
            if until < effective_from {
                return Err(UnifiedError::bad_request(
                    "effective_to must not be before effective_from",
                ));
            }
        }
        let rule = TaxRule {
            id: new_id(),
            company_id,
            country_code: input.country_code.trim().to_uppercase(),
            region_code,
            name: input.name.trim().to_string(),
            tax_type: input.tax_type,
            rate_percent: round_tax(input.rate_percent),
            threshold_amount: input.threshold_amount.map(round_tax),
            currency,
            inclusive: input.inclusive.unwrap_or(false),
            compound: input.compound.unwrap_or(false),
            effective_from,
            effective_to,
            sort_order: input.sort_order.unwrap_or(0),
            enabled: input.enabled.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.rules.insert(rule.id.clone(), rule.clone());
        state.record_audit(
            company_id,
            &caller.actor,
            "rule_created",
            "tax_rule",
            &rule.id,
            "tax.rule_created",
            json!({
                "country_code": rule.country_code,
                "tax_type": rule.tax_type,
                "rate_percent": rule.rate_percent,
            }),
        );
        Ok(rule)
    })();
    respond_created(result)
}

async fn get_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<TaxRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let rule = state
            .rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax rule not found"))?;
        require_company_scope(resolve_caller_company(&ctx, &req, &caller), rule.company_id)?;
        if rule.company_id != company_id {
            return Err(UnifiedError::not_found("Tax rule not found"));
        }
        Ok(rule)
    })();
    json::respond(result, false)
}

async fn update_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTaxRule>,
) -> HttpResponse {
    let result: UnifiedResult<TaxRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        if let Some(country) = &input.country_code {
            if !valid_country_code(country.trim()) {
                return Err(UnifiedError::bad_request(
                    "country_code must be a 2-letter ISO code",
                ));
            }
        }
        if let Some(region) = &input.region_code {
            if !region.trim().is_empty() && !valid_region_code(region.trim()) {
                return Err(UnifiedError::bad_request(
                    "region_code must be at most 16 letters, digits, hyphens, or underscores",
                ));
            }
        }
        if let Some(tax_type) = &input.tax_type {
            if !TAX_TYPES.contains(&tax_type.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "tax_type must be one of: {}",
                    TAX_TYPES.join(", ")
                )));
            }
        }
        if let Some(rate) = input.rate_percent {
            if !(0.0..=100.0).contains(&rate) || !rate.is_finite() {
                return Err(UnifiedError::bad_request(
                    "rate_percent must be a finite number between 0 and 100",
                ));
            }
        }
        if let Some(threshold) = input.threshold_amount {
            if threshold < 0.0 || !threshold.is_finite() {
                return Err(UnifiedError::bad_request(
                    "threshold_amount must be zero or greater",
                ));
            }
        }
        if let Some(currency) = &input.currency {
            if !valid_currency(currency.trim().to_uppercase().as_str()) {
                return Err(UnifiedError::bad_request(
                    "currency must be a 3-letter code",
                ));
            }
        }
        let effective_from = input
            .effective_from
            .as_deref()
            .map(|value| parse_date(value, "effective_from"))
            .transpose()?;
        let effective_to = input
            .effective_to
            .as_deref()
            .map(|value| parse_date(value, "effective_to"))
            .transpose()?;
        // Validate the effective range against the stored rule before
        // any mutation is applied: a failed validation must leave the
        // stored rule untouched.
        let current = state
            .rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax rule not found"))?;
        if current.company_id != company_id {
            return Err(UnifiedError::not_found("Tax rule not found"));
        }
        let prospective_from = effective_from.unwrap_or(current.effective_from);
        let prospective_to = effective_to.or(current.effective_to);
        if let Some(until) = prospective_to {
            if until < prospective_from {
                return Err(UnifiedError::bad_request(
                    "effective_to must not be before effective_from",
                ));
            }
        }
        let mut rule = state
            .rules
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Tax rule not found"))?;
        if rule.company_id != company_id {
            return Err(UnifiedError::not_found("Tax rule not found"));
        }
        if let Some(country) = input.country_code {
            rule.country_code = country.trim().to_uppercase();
        }
        if let Some(region) = input.region_code {
            rule.region_code = if region.trim().is_empty() {
                None
            } else {
                Some(region.trim().to_string())
            };
        }
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("name must not be empty"));
            }
            rule.name = name.trim().to_string();
        }
        if let Some(tax_type) = input.tax_type {
            rule.tax_type = tax_type;
        }
        if let Some(rate) = input.rate_percent {
            rule.rate_percent = round_tax(rate);
        }
        if let Some(threshold) = input.threshold_amount {
            rule.threshold_amount = Some(round_tax(threshold));
        }
        if let Some(currency) = input.currency {
            rule.currency = currency.trim().to_uppercase();
        }
        if let Some(inclusive) = input.inclusive {
            rule.inclusive = inclusive;
        }
        if let Some(compound) = input.compound {
            rule.compound = compound;
        }
        if let Some(from) = effective_from {
            rule.effective_from = from;
        }
        if let Some(until) = effective_to {
            rule.effective_to = Some(until);
        }
        if let Some(sort_order) = input.sort_order {
            rule.sort_order = sort_order;
        }
        if let Some(enabled) = input.enabled {
            rule.enabled = enabled;
        }
        rule.updated_at = now();
        let updated = rule.clone();
        drop(rule);
        state.record_audit(
            company_id,
            &caller.actor,
            "rule_updated",
            "tax_rule",
            &updated.id,
            "tax.rule_updated",
            json!({
                "country_code": updated.country_code,
                "tax_type": updated.tax_type,
                "rate_percent": updated.rate_percent,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let rule = state
            .rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax rule not found"))?;
        if rule.company_id != company_id {
            return Err(UnifiedError::not_found("Tax rule not found"));
        }
        state.rules.remove(path.as_str());
        state.record_audit(
            company_id,
            &caller.actor,
            "rule_deleted",
            "tax_rule",
            &rule.id,
            "tax.rule_deleted",
            json!({ "country_code": rule.country_code, "name": rule.name }),
        );
        Ok(json!({ "deleted": true, "id": rule.id }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Handlers: compute and computations
// ---------------------------------------------------------------------------

async fn compute_tax(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    body: web::Json<ComputeRequest>,
) -> HttpResponse {
    let result: UnifiedResult<TaxComputation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        if !input.amount.is_finite() || input.amount < 0.0 {
            return Err(UnifiedError::bad_request(
                "amount must be a finite number zero or greater",
            ));
        }
        let currency = input
            .currency
            .unwrap_or_else(|| state.settings_for(company_id).default_currency)
            .trim()
            .to_uppercase();
        if !valid_currency(&currency) {
            return Err(UnifiedError::bad_request(
                "currency must be a 3-letter code",
            ));
        }
        let country_code = input
            .country_code
            .map(|value| value.trim().to_uppercase())
            .filter(|value| !value.is_empty());
        if let Some(country) = &country_code {
            if !valid_country_code(country) {
                return Err(UnifiedError::bad_request(
                    "country_code must be a 2-letter ISO code",
                ));
            }
        }
        let region_code = input
            .region_code
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(region) = &region_code {
            if !valid_region_code(region) {
                return Err(UnifiedError::bad_request(
                    "region_code must be at most 16 letters, digits, hyphens, or underscores",
                ));
            }
        }
        let on = input
            .date
            .as_deref()
            .map(|value| parse_date(value, "date"))
            .transpose()?
            .unwrap_or_else(today);
        let source_type = input.source_type.unwrap_or_else(|| "manual".to_string());
        if !matches!(source_type.as_str(), "manual" | "invoice" | "order") {
            return Err(UnifiedError::bad_request(
                "source_type must be one of: manual, invoice, order",
            ));
        }
        let source_id = input.source_id.unwrap_or_else(new_id);
        Ok(compute_and_store(
            &state,
            company_id,
            &source_type,
            &source_id,
            input.amount,
            &currency,
            country_code.as_deref(),
            region_code.as_deref(),
            on,
        ))
    })();
    json::respond(result, false)
}

async fn list_computations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let auth: UnifiedResult<Uuid> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        acting_company(&ctx, &req, &caller)
    })();
    let company_id = match auth {
        Ok(company_id) => company_id,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let mut items: Vec<TaxComputation> = state
        .computations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|computation| {
            computation.company_id == company_id
                && params
                    .filters
                    .get("source_type")
                    .is_none_or(|kind| computation.source_type == *kind)
                && params
                    .filters
                    .get("source_id")
                    .is_none_or(|id| computation.source_id == *id)
                && params
                    .filters
                    .get("country_code")
                    .is_none_or(|country| {
                        computation
                            .country_code
                            .as_deref()
                            .is_none_or(|code| code.eq_ignore_ascii_case(country))
                    })
                && params
                    .filters
                    .get("currency")
                    .is_none_or(|currency| computation.currency.eq_ignore_ascii_case(currency))
        })
        .collect();
    items.sort_by(|a, b| b.computed_at.cmp(&a.computed_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn get_computation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<TaxComputation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let computation = state
            .computations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax computation not found"))?;
        if computation.company_id != company_id {
            return Err(UnifiedError::not_found("Tax computation not found"));
        }
        Ok(computation)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Handlers: settings
// ---------------------------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<TaxSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        Ok(state.settings_for(company_id))
    })();
    json::respond(result, false)
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    body: web::Json<UpsertTaxSettings>,
) -> HttpResponse {
    let result: UnifiedResult<TaxSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        let mut settings = state.settings_for(company_id);
        if let Some(month) = input.fiscal_year_start_month {
            if !(1..=12).contains(&month) {
                return Err(UnifiedError::bad_request(
                    "fiscal_year_start_month must be between 1 and 12",
                ));
            }
            settings.fiscal_year_start_month = month;
        }
        if let Some(day) = input.fiscal_year_start_day {
            if !(1..=31).contains(&day) {
                return Err(UnifiedError::bad_request(
                    "fiscal_year_start_day must be between 1 and 31",
                ));
            }
            settings.fiscal_year_start_day = day;
        }
        if let Some(frequency) = input.filing_frequency {
            if !FILING_FREQUENCIES.contains(&frequency.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "filing_frequency must be one of: {}",
                    FILING_FREQUENCIES.join(", ")
                )));
            }
            settings.filing_frequency = frequency;
        }
        if let Some(currency) = input.default_currency {
            let currency = currency.trim().to_uppercase();
            if !valid_currency(&currency) {
                return Err(UnifiedError::bad_request(
                    "default_currency must be a 3-letter code",
                ));
            }
            settings.default_currency = currency;
        }
        if let Some(country) = input.default_country_code {
            let country = country.trim().to_uppercase();
            if !country.is_empty() && !valid_country_code(&country) {
                return Err(UnifiedError::bad_request(
                    "default_country_code must be a 2-letter ISO code",
                ));
            }
            settings.default_country_code = if country.is_empty() { None } else { Some(country) };
        }
        if let Some(region) = input.default_region_code {
            let region = region.trim().to_string();
            if !region.is_empty() && !valid_region_code(&region) {
                return Err(UnifiedError::bad_request(
                    "default_region_code must be at most 16 letters, digits, hyphens, or underscores",
                ));
            }
            settings.default_region_code = if region.is_empty() { None } else { Some(region) };
        }
        if let Some(days) = input.reminder_days_before {
            if days > 90 {
                return Err(UnifiedError::bad_request(
                    "reminder_days_before must be between 0 and 90",
                ));
            }
            settings.reminder_days_before = days;
        }
        if let Some(days) = input.filing_due_days_after_period {
            if days > 365 {
                return Err(UnifiedError::bad_request(
                    "filing_due_days_after_period must be between 0 and 365",
                ));
            }
            settings.filing_due_days_after_period = days;
        }
        if let Some(user_id) = input.notify_user_id {
            settings.notify_user_id = if user_id.trim().is_empty() {
                None
            } else {
                Some(user_id.trim().to_string())
            };
        }
        settings.updated_at = now();
        state.settings.insert(company_id, settings.clone());
        state.record_audit(
            company_id,
            &caller.actor,
            "settings_updated",
            "tax_settings",
            &settings.id,
            "tax.settings_updated",
            json!({
                "filing_frequency": settings.filing_frequency,
                "default_currency": settings.default_currency,
            }),
        );
        Ok(settings)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Handlers: reports
// ---------------------------------------------------------------------------

/// Parse the `period_start` / `period_end` query filters. Both are
/// optional; when absent the range is open-ended.
fn report_range(params: &PaginationParams) -> UnifiedResult<(Option<NaiveDate>, Option<NaiveDate>)> {
    let start = params
        .filters
        .get("period_start")
        .map(|value| parse_date(value, "period_start"))
        .transpose()?;
    let end = params
        .filters
        .get("period_end")
        .map(|value| parse_date(value, "period_end"))
        .transpose()?;
    if let (Some(start), Some(end)) = (start, end) {
        if end < start {
            return Err(UnifiedError::bad_request(
                "period_end must not be before period_start",
            ));
        }
    }
    Ok((start, end))
}

fn in_report_range(
    day: NaiveDate,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> bool {
    start.is_none_or(|from| day >= from) && end.is_none_or(|until| day <= until)
}

fn report_computations(
    state: &TaxState,
    company_id: Uuid,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> Vec<TaxComputation> {
    state
        .computations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|computation| {
            computation.company_id == company_id
                && in_report_range(computation.computed_at.date_naive(), start, end)
        })
        .collect()
}

fn summary_value(
    state: &TaxState,
    company_id: Uuid,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> Value {
    let computations = report_computations(state, company_id, start, end);
    let mut base_total = 0.0;
    let mut tax_total = 0.0;
    let mut grand_total = 0.0;
    let mut by_jurisdiction: Vec<Value> = Vec::new();
    let mut by_period: Vec<Value> = Vec::new();
    for computation in &computations {
        base_total += computation.base_amount;
        tax_total += computation.tax_total;
        grand_total += computation.grand_total;
        let jurisdiction = format!(
            "{}-{}",
            computation.country_code.clone().unwrap_or_default(),
            computation.region_code.clone().unwrap_or_default()
        );
        let position = by_jurisdiction
            .iter()
            .position(|entry| entry.get("jurisdiction").and_then(|v| v.as_str()) == Some(&jurisdiction));
        let row = match position {
            Some(index) => &mut by_jurisdiction[index],
            None => {
                by_jurisdiction.push(json!({
                    "jurisdiction": jurisdiction,
                    "country_code": computation.country_code,
                    "region_code": computation.region_code,
                    "currency": computation.currency,
                    "computation_count": 0,
                    "base_total": 0.0,
                    "tax_total": 0.0,
                    "grand_total": 0.0,
                }));
                by_jurisdiction.last_mut().expect("row just pushed")
            }
        };
        let object = row.as_object_mut().expect("summary row is an object");
        let count = object
            .get("computation_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        object.insert("computation_count".to_string(), json!(count + 1));
        for (field, amount) in [
            ("base_total", computation.base_amount),
            ("tax_total", computation.tax_total),
            ("grand_total", computation.grand_total),
        ] {
            let current = object.get(field).and_then(|v| v.as_f64()).unwrap_or(0.0);
            object.insert(field.to_string(), json!(round_tax(current + amount)));
        }
        let period = computation.computed_at.format("%Y-%m").to_string();
        let position = by_period
            .iter()
            .position(|entry| entry.get("period").and_then(|v| v.as_str()) == Some(&period));
        let row = match position {
            Some(index) => &mut by_period[index],
            None => {
                by_period.push(json!({
                    "period": period,
                    "computation_count": 0,
                    "base_total": 0.0,
                    "tax_total": 0.0,
                    "grand_total": 0.0,
                }));
                by_period.last_mut().expect("row just pushed")
            }
        };
        let object = row.as_object_mut().expect("summary row is an object");
        let count = object
            .get("computation_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        object.insert("computation_count".to_string(), json!(count + 1));
        for (field, amount) in [
            ("base_total", computation.base_amount),
            ("tax_total", computation.tax_total),
            ("grand_total", computation.grand_total),
        ] {
            let current = object.get(field).and_then(|v| v.as_f64()).unwrap_or(0.0);
            object.insert(field.to_string(), json!(round_tax(current + amount)));
        }
    }
    by_jurisdiction.sort_by(|a, b| {
        a.get("jurisdiction")
            .and_then(|v| v.as_str())
            .cmp(&b.get("jurisdiction").and_then(|v| v.as_str()))
    });
    by_period.sort_by(|a, b| {
        a.get("period")
            .and_then(|v| v.as_str())
            .cmp(&b.get("period").and_then(|v| v.as_str()))
    });
    json!({
        "company_id": company_id.to_string(),
        "period_start": start,
        "period_end": end,
        "computation_count": computations.len(),
        "base_total": round_tax(base_total),
        "tax_total": round_tax(tax_total),
        "grand_total": round_tax(grand_total),
        "by_jurisdiction": by_jurisdiction,
        "by_period": by_period,
    })
}

async fn report_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let (start, end) = report_range(&query.into_inner())?;
        Ok(summary_value(&state, company_id, start, end))
    })();
    json::respond(result, false)
}

async fn report_jurisdictions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let (start, end) = report_range(&query.into_inner())?;
        let summary = summary_value(&state, company_id, start, end);
        Ok(summary
            .get("by_jurisdiction")
            .cloned()
            .unwrap_or(Value::Null))
    })();
    json::respond(result, false)
}

async fn report_periods(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let (start, end) = report_range(&query.into_inner())?;
        let summary = summary_value(&state, company_id, start, end);
        Ok(summary.get("by_period").cloned().unwrap_or(Value::Null))
    })();
    json::respond(result, false)
}

async fn report_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<(Uuid, Option<NaiveDate>, Option<NaiveDate>, String)> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let params = query.into_inner();
        let format = params
            .filters
            .get("format")
            .cloned()
            .unwrap_or_else(|| "json".to_string());
        if !matches!(format.as_str(), "json" | "csv") {
            return Err(UnifiedError::bad_request(
                "format must be one of: json, csv",
            ));
        }
        let (start, end) = report_range(&params)?;
        Ok((company_id, start, end, format))
    })();
    let (company_id, start, end, format) = match result {
        Ok(values) => values,
        Err(error) => return json::error_response(&error, false),
    };
    let summary = summary_value(&state, company_id, start, end);
    if format == "csv" {
        let mut csv = String::from(
            "period,country_code,region_code,currency,computation_count,base_total,tax_total,grand_total\n",
        );
        if let Some(rows) = summary.get("by_jurisdiction").and_then(|v| v.as_array()) {
            for row in rows {
                let period = summary
                    .get("period_start")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                csv.push_str(&format!(
                    "{},{},{},{},{},{},{},{}\n",
                    escape_csv(period),
                    escape_csv(row.get("country_code").and_then(|v| v.as_str()).unwrap_or("")),
                    escape_csv(row.get("region_code").and_then(|v| v.as_str()).unwrap_or("")),
                    escape_csv(row.get("currency").and_then(|v| v.as_str()).unwrap_or("")),
                    row.get("computation_count").and_then(|v| v.as_u64()).unwrap_or(0),
                    row.get("base_total").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    row.get("tax_total").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    row.get("grand_total").and_then(|v| v.as_f64()).unwrap_or(0.0),
                ));
            }
        }
        return HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header(("Content-Disposition", "attachment; filename=\"tax-report.csv\""))
            .body(csv);
    }
    json::success_response(summary, 200)
}

// ---------------------------------------------------------------------------
// Handlers: filings
// ---------------------------------------------------------------------------

/// Run the filing-reminder check for one company: refresh the
/// current and previous period filings for every jurisdiction that
/// has tax rules, update their statuses, and emit `tax.filing_due`
/// plus `notification.requested` for filings inside the reminder
/// window (or already overdue) that have not been reminded about
/// since their status last changed.
fn run_filing_check(state: &TaxState, company_id: Uuid) -> Vec<TaxFiling> {
    let settings = state.settings_for(company_id);
    let today = today();
    let mut jurisdictions: HashSet<(String, Option<String>)> = HashSet::new();
    for entry in state.rules.iter() {
        let rule = entry.value();
        if rule.company_id == company_id && rule.enabled {
            jurisdictions.insert((rule.country_code.clone(), rule.region_code.clone()));
        }
    }
    if jurisdictions.is_empty() {
        if let Some(country) = settings.default_country_code.clone() {
            jurisdictions.insert((country, settings.default_region_code.clone()));
        }
    }
    let mut changed: Vec<TaxFiling> = Vec::new();
    for (country_code, region_code) in jurisdictions {
        let (current_start, current_end) =
            period_for(today, &settings.filing_frequency, &settings);
        let (previous_start, previous_end) =
            previous_period(current_start, current_end, &settings.filing_frequency, &settings);
        for (period_start, period_end) in [(current_start, current_end), (previous_start, previous_end)] {
            let due_date = period_end
                + chrono::Duration::days(i64::from(settings.filing_due_days_after_period));
            let tax_total = period_tax_total(
                state,
                company_id,
                &country_code,
                region_code.as_deref(),
                period_start,
                period_end,
            );
            let existing = state
                .filings
                .iter()
                .find(|entry| {
                    let filing = entry.value();
                    filing.company_id == company_id
                        && filing.country_code == country_code
                        && filing.region_code == region_code
                        && filing.period_start == period_start
                        && filing.period_end == period_end
                })
                .map(|entry| entry.key().clone());
            let status = if today > due_date {
                "overdue"
            } else if today >= due_date - chrono::Duration::days(i64::from(settings.reminder_days_before)) {
                "due"
            } else {
                "upcoming"
            };
            let mut filing = match existing {
                Some(id) => state
                    .filings
                    .get(&id)
                    .map(|entry| entry.value().clone())
                    .unwrap_or_else(|| TaxFiling {
                        id,
                        company_id,
                        country_code: country_code.clone(),
                        region_code: region_code.clone(),
                        period_start,
                        period_end,
                        frequency: settings.filing_frequency.clone(),
                        due_date,
                        status: status.to_string(),
                        tax_total,
                        submitted_at: None,
                        submitted_via: None,
                        reference: None,
                        last_reminder_at: None,
                        created_at: now(),
                        updated_at: now(),
                    }),
                None => TaxFiling {
                    id: new_id(),
                    company_id,
                    country_code: country_code.clone(),
                    region_code: region_code.clone(),
                    period_start,
                    period_end,
                    frequency: settings.filing_frequency.clone(),
                    due_date,
                    status: status.to_string(),
                    tax_total,
                    submitted_at: None,
                    submitted_via: None,
                    reference: None,
                    last_reminder_at: None,
                    created_at: now(),
                    updated_at: now(),
                },
            };
            if filing.status == "submitted" {
                filing.tax_total = tax_total;
                filing.updated_at = now();
                state.filings.insert(filing.id.clone(), filing.clone());
                continue;
            }
            let was_due = filing.status == "due" || filing.status == "overdue";
            filing.status = status.to_string();
            filing.tax_total = tax_total;
            filing.updated_at = now();
            let needs_reminder = (status == "due" || status == "overdue")
                && (!was_due
                    || filing.last_reminder_at.is_none_or(|at| {
                        at.date_naive() < today
                    }));
            if needs_reminder {
                filing.last_reminder_at = Some(now());
                state.filings.insert(filing.id.clone(), filing.clone());
                state.emit(
                    "tax.filing_due",
                    json!({
                        "filing_id": filing.id,
                        "company_id": company_id.to_string(),
                        "country_code": filing.country_code,
                        "region_code": filing.region_code,
                        "period_start": filing.period_start,
                        "period_end": filing.period_end,
                        "due_date": filing.due_date,
                        "status": filing.status,
                        "tax_total": filing.tax_total,
                    }),
                );
                let mut notification = json!({
                    "company_id": company_id.to_string(),
                    "title": format!(
                        "Tax filing {} for {}",
                        filing.status, filing.country_code
                    ),
                    "body": format!(
                        "Tax filing for {} ({} to {}) is {} with tax total {}. Due {}.",
                        filing.country_code,
                        filing.period_start,
                        filing.period_end,
                        filing.status,
                        filing.tax_total,
                        filing.due_date
                    ),
                    "kind": "tax",
                });
                if let Some(user_id) = &settings.notify_user_id {
                    if let Value::Object(map) = &mut notification {
                        map.insert("user_id".to_string(), Value::String(user_id.clone()));
                    }
                }
                state.emit("notification.requested", notification);
                changed.push(filing);
            } else {
                state.filings.insert(filing.id.clone(), filing.clone());
                changed.push(filing);
            }
        }
    }
    changed
}

async fn list_filings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let auth: UnifiedResult<Uuid> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        acting_company(&ctx, &req, &caller)
    })();
    let company_id = match auth {
        Ok(company_id) => company_id,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let mut items: Vec<TaxFiling> = state
        .filings
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|filing| {
            filing.company_id == company_id
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| filing.status == *status)
                && params
                    .filters
                    .get("country_code")
                    .is_none_or(|country| filing.country_code.eq_ignore_ascii_case(country))
                && params
                    .filters
                    .get("frequency")
                    .is_none_or(|frequency| filing.frequency == *frequency)
        })
        .collect();
    items.sort_by(|a, b| a.due_date.cmp(&b.due_date));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_filing(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    body: web::Json<CreateTaxFiling>,
) -> HttpResponse {
    let result: UnifiedResult<TaxFiling> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        if !valid_country_code(input.country_code.trim()) {
            return Err(UnifiedError::bad_request(
                "country_code must be a 2-letter ISO code",
            ));
        }
        let region_code = input
            .region_code
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(region) = &region_code {
            if !valid_region_code(region) {
                return Err(UnifiedError::bad_request(
                    "region_code must be at most 16 letters, digits, hyphens, or underscores",
                ));
            }
        }
        let period_start = parse_date(&input.period_start, "period_start")?;
        let period_end = parse_date(&input.period_end, "period_end")?;
        if period_end < period_start {
            return Err(UnifiedError::bad_request(
                "period_end must not be before period_start",
            ));
        }
        let frequency = input
            .frequency
            .unwrap_or_else(|| state.settings_for(company_id).filing_frequency);
        if !FILING_FREQUENCIES.contains(&frequency.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "frequency must be one of: {}",
                FILING_FREQUENCIES.join(", ")
            )));
        }
        if state.filings.iter().any(|entry| {
            let filing = entry.value();
            filing.company_id == company_id
                && filing.country_code.eq_ignore_ascii_case(input.country_code.trim())
                && filing.region_code == region_code
                && filing.period_start == period_start
                && filing.period_end == period_end
        }) {
            return Err(UnifiedError::conflict(
                "A filing already exists for this company, jurisdiction, and period",
            ));
        }
        let settings = state.settings_for(company_id);
        let due_date =
            period_end + chrono::Duration::days(i64::from(settings.filing_due_days_after_period));
        let tax_total = period_tax_total(
            &state,
            company_id,
            &input.country_code.trim().to_uppercase(),
            region_code.as_deref(),
            period_start,
            period_end,
        );
        let today = today();
        let status = if today > due_date {
            "overdue"
        } else if today >= due_date - chrono::Duration::days(i64::from(settings.reminder_days_before)) {
            "due"
        } else {
            "upcoming"
        };
        let filing = TaxFiling {
            id: new_id(),
            company_id,
            country_code: input.country_code.trim().to_uppercase(),
            region_code,
            period_start,
            period_end,
            frequency,
            due_date,
            status: status.to_string(),
            tax_total,
            submitted_at: None,
            submitted_via: None,
            reference: None,
            last_reminder_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.filings.insert(filing.id.clone(), filing.clone());
        state.record_audit(
            company_id,
            &caller.actor,
            "filing_created",
            "tax_filing",
            &filing.id,
            "tax.filing_created",
            json!({
                "country_code": filing.country_code,
                "period_start": filing.period_start,
                "period_end": filing.period_end,
                "due_date": filing.due_date,
            }),
        );
        Ok(filing)
    })();
    respond_created(result)
}

async fn get_filing(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<TaxFiling> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let filing = state
            .filings
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax filing not found"))?;
        if filing.company_id != company_id {
            return Err(UnifiedError::not_found("Tax filing not found"));
        }
        Ok(filing)
    })();
    json::respond(result, false)
}

async fn check_due_filings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let changed = run_filing_check(&state, company_id);
        Ok(json!({
            "company_id": company_id.to_string(),
            "checked_filings": changed.len(),
            "filings": changed,
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Handlers: filing submission through a connector
// ---------------------------------------------------------------------------

/// File one filing through a connector. The `example` kind is a
/// clearly-marked example provider mapping: it performs no network
/// call and returns a simulated confirmation with obviously fake
/// field names. A `generic_http` connector performs a real signed
/// POST to its validated endpoint with timeout and retry policy.
async fn submit_through_connector(
    ctx: &Arc<AppContext>,
    connector: &TaxConnector,
    filing: &TaxFiling,
) -> UnifiedResult<(String, String)> {
    if connector.kind == "example" {
        let reference = format!("EXAMPLE-{}-{}", filing.country_code, filing.id);
        return Ok((
            format!("example provider (simulated): {}", connector.name),
            reference,
        ));
    }
    let url = validate_endpoint_url(&connector.endpoint_url, &connector.allowed_hosts)?;
    let secret = encryption_secret(ctx);
    let credential = match &connector.credential_encrypted {
        Some(stored) => Some(decrypt_tax_credential(&secret, stored)?),
        None => None,
    };
    let signing_secret = match &connector.signing_secret_encrypted {
        Some(stored) => Some(decrypt_tax_credential(&secret, stored)?),
        None => None,
    };
    let payload = apply_request_mapping(
        &connector.request_mapping,
        &json!({
            "filing_id": filing.id,
            "company_id": filing.company_id.to_string(),
            "country_code": filing.country_code,
            "region_code": filing.region_code,
            "period_start": filing.period_start,
            "period_end": filing.period_end,
            "due_date": filing.due_date,
            "tax_total": filing.tax_total,
        }),
    );
    let body = serde_json::to_string(&payload)
        .map_err(|error| UnifiedError::internal(format!("Cannot serialize filing payload: {error}")))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(connector.timeout_ms))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))?;
    let mut last_error = String::new();
    let attempts = connector.max_retries as usize + 1;
    for attempt in 0..attempts {
        let timestamp = Utc::now().timestamp();
        let mut request = client
            .post(url.clone())
            .header("Content-Type", "application/json")
            .header("X-Tax-Filing-Id", filing.id.clone());
        match connector.auth_scheme.as_str() {
            "bearer" => {
                if let Some(key) = &credential {
                    request = request.bearer_auth(key);
                }
            }
            "api_key" => {
                if let Some(key) = &credential {
                    request = request.header("X-Api-Key", key);
                }
            }
            "hmac" => {
                if let Some(signing) = &signing_secret {
                    let signature = sign_body(signing, timestamp, &body);
                    request = request
                        .header("X-Tax-Signature", format!("sha256={signature}"))
                        .header("X-Tax-Timestamp", timestamp.to_string());
                }
            }
            _ => {}
        }
        match request.body(body.clone()).send().await {
            Ok(response) => {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                if !status.is_success() {
                    last_error = format!(
                        "Provider returned HTTP {}: {}",
                        status.as_u16(),
                        text.chars().take(200).collect::<String>()
                    );
                    continue;
                }
                let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                let mapped = apply_response_mapping(&connector.response_mapping, &parsed);
                let reference = mapped
                    .get("reference")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        parsed
                            .get("reference")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| format!("{}-{}", connector.name, filing.id));
                return Ok((connector.name.clone(), reference));
            }
            Err(error) => {
                last_error = format!("Provider request failed: {error}");
                if attempt + 1 < attempts {
                    tokio::time::sleep(Duration::from_millis(200 * (1 << attempt))).await;
                }
            }
        }
    }
    Err(UnifiedError::internal(format!(
        "Filing submission failed after {attempts} attempt(s): {last_error}"
    )))
}

async fn submit_filing(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SubmitTaxFiling>,
) -> HttpResponse {
    let result: UnifiedResult<(TaxFiling, SubmitTaxFiling)> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FILE)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        let filing_id = path.as_str().to_string();
        let filing = state
            .filings
            .get(&filing_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax filing not found"))?;
        if filing.company_id != company_id {
            return Err(UnifiedError::not_found("Tax filing not found"));
        }
        if filing.status == "submitted" {
            return Err(UnifiedError::conflict(
                "Filing is already submitted",
            ));
        }
        Ok((filing, input))
    })();
    let (mut filing, input) = match result {
        Ok(values) => values,
        Err(error) => return json::error_response(&error, false),
    };
    let (submitted_via, reference) = if let Some(connector_id) = input.connector_id {
        let connector = match state.connectors.get(connector_id.trim()) {
            Some(entry) => entry.value().clone(),
            None => {
                return json::error_response(
                    &UnifiedError::not_found("Tax connector not found"),
                    false,
                );
            }
        };
        if connector.company_id != filing.company_id {
            return json::error_response(
                &UnifiedError::not_found("Tax connector not found"),
                false,
            );
        }
        if !connector.enabled {
            return json::error_response(
                &UnifiedError::conflict("Tax connector is disabled"),
                false,
            );
        }
        match submit_through_connector(&ctx, &connector, &filing).await {
            Ok(outcome) => outcome,
            Err(error) => return json::error_response(&error, false),
        }
    } else {
        (
            "manual".to_string(),
            input
                .reference
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| format!("MANUAL-{}", filing.id)),
        )
    };
    filing.status = "submitted".to_string();
    filing.submitted_at = Some(now());
    filing.submitted_via = Some(submitted_via.clone());
    filing.reference = Some(reference.clone());
    filing.updated_at = now();
    state.filings.insert(filing.id.clone(), filing.clone());
    let caller_actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    state.record_audit(
        filing.company_id,
        &caller_actor,
        "filing_submitted",
        "tax_filing",
        &filing.id,
        "tax.filing_submitted",
        json!({
            "country_code": filing.country_code,
            "period_start": filing.period_start,
            "period_end": filing.period_end,
            "submitted_via": submitted_via,
            "reference": reference,
        }),
    );
    json::respond(Ok::<TaxFiling, UnifiedError>(filing), false)
}

// ---------------------------------------------------------------------------
// Handlers: connectors
// ---------------------------------------------------------------------------

async fn list_connectors(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let auth: UnifiedResult<Uuid> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        acting_company(&ctx, &req, &caller)
    })();
    let company_id = match auth {
        Ok(company_id) => company_id,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let mut items: Vec<TaxConnectorView> = state
        .connectors
        .iter()
        .map(|entry| TaxConnectorView::from(entry.value()))
        .filter(|connector| {
            connector.company_id == company_id
                && matches_search(
                    &format!("{} {} {}", connector.name, connector.kind, connector.endpoint_url),
                    &params,
                )
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| connector.kind == *kind)
                && params
                    .filters
                    .get("enabled")
                    .is_none_or(|flag| connector.enabled == (flag == "true"))
        })
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_connector(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    body: web::Json<CreateTaxConnector>,
) -> HttpResponse {
    let result: UnifiedResult<TaxConnectorView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_CONNECTORS)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("name must not be empty"));
        }
        if !CONNECTOR_KINDS.contains(&input.kind.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "kind must be one of: {}",
                CONNECTOR_KINDS.join(", ")
            )));
        }
        let auth_scheme = input.auth_scheme.unwrap_or_else(|| "none".to_string());
        if !AUTH_SCHEMES.contains(&auth_scheme.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "auth_scheme must be one of: {}",
                AUTH_SCHEMES.join(", ")
            )));
        }
        let allowed_hosts: Vec<String> = input
            .allowed_hosts
            .iter()
            .map(|host| host.trim().to_lowercase())
            .filter(|host| !host.is_empty())
            .collect();
        if allowed_hosts.is_empty() {
            return Err(UnifiedError::bad_request(
                "allowed_hosts must list at least one host",
            ));
        }
        if input.kind == "example" {
            // The example provider mapping is clearly marked and uses
            // an obviously fake endpoint; it never performs a network
            // call, so its host is not DNS-checked.
            let url = Url::parse(input.endpoint_url.trim()).map_err(|_| {
                UnifiedError::bad_request("endpoint_url must be a valid URL")
            })?;
            if url.scheme() != "https" {
                return Err(UnifiedError::bad_request(
                    "endpoint_url must use https",
                ));
            }
        } else {
            validate_endpoint_url(&input.endpoint_url, &allowed_hosts)?;
        }
        if state.connectors.iter().any(|entry| {
            entry.value().company_id == company_id
                && entry.value().name.eq_ignore_ascii_case(input.name.trim())
        }) {
            return Err(UnifiedError::conflict(format!(
                "A tax connector named \"{}\" already exists for this company",
                input.name.trim()
            )));
        }
        let timeout_ms = input.timeout_ms.unwrap_or(10_000);
        if !(100..=120_000).contains(&timeout_ms) {
            return Err(UnifiedError::bad_request(
                "timeout_ms must be between 100 and 120000",
            ));
        }
        let max_retries = input.max_retries.unwrap_or(2);
        if max_retries > 5 {
            return Err(UnifiedError::bad_request(
                "max_retries must be between 0 and 5",
            ));
        }
        if auth_scheme != "none" && input.credential.as_deref().is_none_or(|v| v.trim().is_empty()) {
            return Err(UnifiedError::bad_request(
                "credential is required when auth_scheme is not none",
            ));
        }
        if auth_scheme == "hmac"
            && input.signing_secret.as_deref().is_none_or(|v| v.trim().is_empty())
        {
            return Err(UnifiedError::bad_request(
                "signing_secret is required when auth_scheme is hmac",
            ));
        }
        let secret = encryption_secret(&ctx);
        let (credential_encrypted, credential_fingerprint) = match input.credential {
            Some(plain) if !plain.trim().is_empty() => (
                Some(encrypt_tax_credential(&secret, plain.trim())),
                Some(credential_fingerprint(plain.trim())),
            ),
            _ => (None, None),
        };
        let (signing_secret_encrypted, signing_secret_fingerprint) = match input.signing_secret {
            Some(plain) if !plain.trim().is_empty() => (
                Some(encrypt_tax_credential(&secret, plain.trim())),
                Some(credential_fingerprint(plain.trim())),
            ),
            _ => (None, None),
        };
        let connector = TaxConnector {
            id: new_id(),
            company_id,
            name: input.name.trim().to_string(),
            kind: input.kind,
            endpoint_url: input.endpoint_url.trim().to_string(),
            allowed_hosts,
            auth_scheme,
            credential_encrypted,
            credential_fingerprint,
            signing_secret_encrypted,
            signing_secret_fingerprint,
            timeout_ms,
            max_retries,
            enabled: input.enabled.unwrap_or(true),
            request_mapping: input.request_mapping.unwrap_or_else(|| json!({})),
            response_mapping: input.response_mapping.unwrap_or_else(|| json!({})),
            created_at: now(),
            updated_at: now(),
        };
        state
            .connectors
            .insert(connector.id.clone(), connector.clone());
        state.record_audit(
            company_id,
            &caller.actor,
            "connector_created",
            "tax_connector",
            &connector.id,
            "tax.connector_created",
            json!({
                "name": connector.name,
                "kind": connector.kind,
                "credential_configured": connector.credential_encrypted.is_some(),
                "credential_fingerprint": connector.credential_fingerprint,
            }),
        );
        Ok(TaxConnectorView::from(&connector))
    })();
    respond_created(result)
}

async fn get_connector(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<TaxConnectorView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let connector = state
            .connectors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax connector not found"))?;
        if connector.company_id != company_id {
            return Err(UnifiedError::not_found("Tax connector not found"));
        }
        Ok(TaxConnectorView::from(&connector))
    })();
    json::respond(result, false)
}

async fn update_connector(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTaxConnector>,
) -> HttpResponse {
    let result: UnifiedResult<TaxConnectorView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_CONNECTORS)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        // Read the current connector first, with no write guard held,
        // so every validation — including the name-uniqueness scan
        // over the same map — runs before any mutation is applied.
        // A failed validation must leave the stored connector
        // untouched.
        let current = state
            .connectors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax connector not found"))?;
        if current.company_id != company_id {
            return Err(UnifiedError::not_found("Tax connector not found"));
        }
        if let Some(auth_scheme) = &input.auth_scheme {
            if !AUTH_SCHEMES.contains(&auth_scheme.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "auth_scheme must be one of: {}",
                    AUTH_SCHEMES.join(", ")
                )));
            }
        }
        let new_allowed_hosts = input.allowed_hosts.as_ref().map(|hosts| {
            hosts
                .iter()
                .map(|host| host.trim().to_lowercase())
                .filter(|host| !host.is_empty())
                .collect::<Vec<String>>()
        });
        if let Some(hosts) = &new_allowed_hosts {
            if hosts.is_empty() {
                return Err(UnifiedError::bad_request(
                    "allowed_hosts must list at least one host",
                ));
            }
        }
        let new_endpoint = input
            .endpoint_url
            .as_ref()
            .map(|value| value.trim().to_string());
        let effective_hosts: Vec<String> = new_allowed_hosts
            .clone()
            .unwrap_or_else(|| current.allowed_hosts.clone());
        if let Some(endpoint) = &new_endpoint {
            if current.kind == "example" {
                let url = Url::parse(endpoint).map_err(|_| {
                    UnifiedError::bad_request("endpoint_url must be a valid URL")
                })?;
                if url.scheme() != "https" {
                    return Err(UnifiedError::bad_request(
                        "endpoint_url must use https",
                    ));
                }
            } else {
                validate_endpoint_url(endpoint, &effective_hosts)?;
            }
        } else if let Some(hosts) = &new_allowed_hosts {
            if current.kind != "example" {
                validate_endpoint_url(&current.endpoint_url, hosts)?;
            }
        }
        if let Some(name) = &input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("name must not be empty"));
            }
            if state.connectors.iter().any(|entry| {
                entry.key() != path.as_str()
                    && entry.value().company_id == company_id
                    && entry.value().name.eq_ignore_ascii_case(name.trim())
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A tax connector named \"{}\" already exists for this company",
                    name.trim()
                )));
            }
        }
        if let Some(timeout_ms) = input.timeout_ms {
            if !(100..=120_000).contains(&timeout_ms) {
                return Err(UnifiedError::bad_request(
                    "timeout_ms must be between 100 and 120000",
                ));
            }
        }
        if let Some(max_retries) = input.max_retries {
            if max_retries > 5 {
                return Err(UnifiedError::bad_request(
                    "max_retries must be between 0 and 5",
                ));
            }
        }
        // Effective credential state after the update, checked before
        // any mutation: a scheme that needs a credential must still
        // have one, and `hmac` must still have a signing secret.
        let effective_scheme = input
            .auth_scheme
            .clone()
            .unwrap_or_else(|| current.auth_scheme.clone());
        let credential_will_exist = match &input.credential {
            Some(plain) => !plain.trim().is_empty(),
            None => current.credential_encrypted.is_some(),
        };
        let signing_will_exist = match &input.signing_secret {
            Some(plain) => !plain.trim().is_empty(),
            None => current.signing_secret_encrypted.is_some(),
        };
        if effective_scheme != "none" && !credential_will_exist {
            return Err(UnifiedError::bad_request(
                "credential is required when auth_scheme is not none",
            ));
        }
        if effective_scheme == "hmac" && !signing_will_exist {
            return Err(UnifiedError::bad_request(
                "signing_secret is required when auth_scheme is hmac",
            ));
        }
        let secret = encryption_secret(&ctx);
        let mut connector = state
            .connectors
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Tax connector not found"))?;
        if let Some(name) = input.name {
            connector.name = name.trim().to_string();
        }
        if let Some(endpoint) = new_endpoint {
            connector.endpoint_url = endpoint;
        }
        if let Some(hosts) = new_allowed_hosts {
            connector.allowed_hosts = hosts;
        }
        if let Some(auth_scheme) = input.auth_scheme {
            connector.auth_scheme = auth_scheme;
        }
        if let Some(plain) = input.credential {
            if plain.trim().is_empty() {
                connector.credential_encrypted = None;
                connector.credential_fingerprint = None;
            } else {
                connector.credential_encrypted =
                    Some(encrypt_tax_credential(&secret, plain.trim()));
                connector.credential_fingerprint = Some(credential_fingerprint(plain.trim()));
            }
        }
        if let Some(plain) = input.signing_secret {
            if plain.trim().is_empty() {
                connector.signing_secret_encrypted = None;
                connector.signing_secret_fingerprint = None;
            } else {
                connector.signing_secret_encrypted =
                    Some(encrypt_tax_credential(&secret, plain.trim()));
                connector.signing_secret_fingerprint = Some(credential_fingerprint(plain.trim()));
            }
        }
        if let Some(timeout_ms) = input.timeout_ms {
            connector.timeout_ms = timeout_ms;
        }
        if let Some(max_retries) = input.max_retries {
            connector.max_retries = max_retries;
        }
        if let Some(enabled) = input.enabled {
            connector.enabled = enabled;
        }
        if let Some(mapping) = input.request_mapping {
            connector.request_mapping = mapping;
        }
        if let Some(mapping) = input.response_mapping {
            connector.response_mapping = mapping;
        }
        connector.updated_at = now();
        let view = TaxConnectorView::from(&*connector);
        let connector_id = connector.id.clone();
        let connector_name = connector.name.clone();
        let connector_kind = connector.kind.clone();
        let credential_configured = connector.credential_encrypted.is_some();
        let fingerprint = connector.credential_fingerprint.clone();
        drop(connector);
        state.record_audit(
            company_id,
            &caller.actor,
            "connector_updated",
            "tax_connector",
            &connector_id,
            "tax.connector_updated",
            json!({
                "name": connector_name,
                "kind": connector_kind,
                "credential_configured": credential_configured,
                "credential_fingerprint": fingerprint,
            }),
        );
        Ok(view)
    })();
    json::respond(result, false)
}

async fn delete_connector(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_CONNECTORS)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let connector = state
            .connectors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax connector not found"))?;
        if connector.company_id != company_id {
            return Err(UnifiedError::not_found("Tax connector not found"));
        }
        state.connectors.remove(path.as_str());
        state.record_audit(
            company_id,
            &caller.actor,
            "connector_deleted",
            "tax_connector",
            &connector.id,
            "tax.connector_deleted",
            json!({ "name": connector.name, "kind": connector.kind }),
        );
        Ok(json!({ "deleted": true, "id": connector.id }))
    })();
    json::respond(result, false)
}

async fn test_connector(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<Value>,
) -> HttpResponse {
    let found: UnifiedResult<(Caller, Uuid, TaxConnector, Value)> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_CONNECTORS)?;
        let company_id = acting_company(&ctx, &req, &caller)?;
        let connector = state
            .connectors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax connector not found"))?;
        if connector.company_id != company_id {
            return Err(UnifiedError::not_found("Tax connector not found"));
        }
        Ok((caller, company_id, connector, body.into_inner()))
    })();
    let (caller, company_id, connector, payload) = match found {
        Ok(values) => values,
        Err(error) => return json::error_response(&error, false),
    };
    if connector.kind == "example" {
        // The example provider mapping never performs a network call:
        // the test returns a simulated confirmation with obviously
        // fake field names.
        let result = json!({
            "connector_id": connector.id,
            "kind": "example",
            "simulated": true,
            "http_status": 200,
            "example_reference_number": format!("EXAMPLE-TEST-{}", connector.id),
            "example_confirmation_code": "EXAMPLE-OK",
            "received_payload": payload,
        });
        state.record_audit(
            company_id,
            &caller.actor,
            "connector_tested",
            "tax_connector",
            &connector.id,
            "tax.connector_tested",
            json!({ "name": connector.name, "kind": "example", "simulated": true }),
        );
        return json::success_response(result, 200);
    }
    let validation = validate_endpoint_url(&connector.endpoint_url, &connector.allowed_hosts);
    let url = match validation {
        Ok(url) => url,
        Err(error) => return json::error_response(&error, false),
    };
    let secret = encryption_secret(&ctx);
    let credential = match &connector.credential_encrypted {
        Some(stored) => match decrypt_tax_credential(&secret, stored) {
            Ok(value) => Some(value),
            Err(error) => return json::error_response(&error, false),
        },
        None => None,
    };
    let signing_secret = match &connector.signing_secret_encrypted {
        Some(stored) => match decrypt_tax_credential(&secret, stored) {
            Ok(value) => Some(value),
            Err(error) => return json::error_response(&error, false),
        },
        None => None,
    };
    let mut test_payload = json!({ "test": true });
    if let Value::Object(map) = &payload {
        if let Value::Object(test_map) = &mut test_payload {
            for (key, value) in map {
                test_map.insert(key.clone(), value.clone());
            }
        }
    }
    let test_payload = apply_request_mapping(&connector.request_mapping, &test_payload);
    let body_text = match serde_json::to_string(&test_payload) {
        Ok(text) => text,
        Err(error) => {
            return json::error_response(
                &UnifiedError::internal(format!("Cannot serialize test payload: {error}")),
                false,
            )
        }
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(connector.timeout_ms))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return json::error_response(
                &UnifiedError::internal(format!("Cannot build HTTP client: {error}")),
                false,
            )
        }
    };
    let mut last_error = String::new();
    let attempts = connector.max_retries as usize + 1;
    let mut outcome: Option<Value> = None;
    for attempt in 0..attempts {
        let timestamp = Utc::now().timestamp();
        let mut request = client
            .post(url.clone())
            .header("Content-Type", "application/json")
            .header("X-Tax-Test", "true");
        match connector.auth_scheme.as_str() {
            "bearer" => {
                if let Some(key) = &credential {
                    request = request.bearer_auth(key);
                }
            }
            "api_key" => {
                if let Some(key) = &credential {
                    request = request.header("X-Api-Key", key);
                }
            }
            "hmac" => {
                if let Some(signing) = &signing_secret {
                    let signature = sign_body(signing, timestamp, &body_text);
                    request = request
                        .header("X-Tax-Signature", format!("sha256={signature}"))
                        .header("X-Tax-Timestamp", timestamp.to_string());
                }
            }
            _ => {}
        }
        match request.body(body_text.clone()).send().await {
            Ok(response) => {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                let preview: String = text.chars().take(RESPONSE_PREVIEW_LIMIT).collect();
                let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                let mapped = apply_response_mapping(&connector.response_mapping, &parsed);
                outcome = Some(json!({
                    "connector_id": connector.id,
                    "kind": connector.kind,
                    "simulated": false,
                    "http_status": status.as_u16(),
                    "success": status.is_success(),
                    "response_preview": preview,
                    "mapped": mapped,
                }));
                break;
            }
            Err(error) => {
                last_error = format!("Provider request failed: {error}");
                if attempt + 1 < attempts {
                    tokio::time::sleep(Duration::from_millis(200 * (1 << attempt))).await;
                }
            }
        }
    }
    let result = match outcome {
        Some(value) => value,
        None => {
            return json::error_response(
                &UnifiedError::internal(format!(
                    "Connector test failed after {attempts} attempt(s): {last_error}"
                )),
                false,
            )
        }
    };
    state.record_audit(
        company_id,
        &caller.actor,
        "connector_tested",
        "tax_connector",
        &connector.id,
        "tax.connector_tested",
        json!({
            "name": connector.name,
            "kind": connector.kind,
            "simulated": false,
            "http_status": result.get("http_status"),
        }),
    );
    json::success_response(result, 200)
}

// ---------------------------------------------------------------------------
// Handlers: audit trail
// ---------------------------------------------------------------------------

async fn list_audit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let auth: UnifiedResult<Uuid> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        acting_company(&ctx, &req, &caller)
    })();
    let company_id = match auth {
        Ok(company_id) => company_id,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let mut items: Vec<TaxAuditEntry> = state
        .audit
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            entry.company_id == company_id
                && params
                    .filters
                    .get("action")
                    .is_none_or(|action| entry.action == *action)
                && params
                    .filters
                    .get("resource_type")
                    .is_none_or(|kind| entry.resource_type == *kind)
                && params
                    .filters
                    .get("resource_id")
                    .is_none_or(|id| entry.resource_id == *id)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// Handle a billing source event (`invoice.created` / `order.created`
/// and their `billing.*` alias spellings): compute the tax for the
/// source document and store the computation. The source document is
/// never mutated. The event is ignored when no company can be
/// resolved for it, because tax records are company-scoped.
fn on_source_created(state: Arc<TaxState>, ctx: &mut HookContext, source_type: &str) {
    let data = ctx.data.clone();
    let Some(source_id) = data.get("id").and_then(|v| v.as_str()) else {
        return;
    };
    let source_id = source_id.to_string();
    if state.computations.iter().any(|entry| {
        entry.value().source_type == source_type && entry.value().source_id == source_id
    }) {
        return;
    }
    let company_id = data
        .get("company_id")
        .and_then(|v| v.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .or_else(|| {
            let user_id = data.get("user_id").and_then(|v| v.as_str())?;
            let answer = state
                .bus()
                .emit("user.company_requested", json!({ "user_id": user_id }));
            answer
                .get("company_id")
                .and_then(|v| v.as_str())
                .and_then(|raw| Uuid::parse_str(raw).ok())
        });
    let Some(company_id) = company_id else {
        return;
    };
    let amount = data
        .get("subtotal")
        .and_then(|v| v.as_f64())
        .or_else(|| data.get("total").and_then(|v| v.as_f64()))
        .or_else(|| {
            data.get("items").and_then(|v| v.as_array()).map(|items| {
                items
                    .iter()
                    .map(|item| {
                        let price = item
                            .get("unit_price")
                            .and_then(|v| v.as_f64())
                            .or_else(|| item.get("amount").and_then(|v| v.as_f64()))
                            .unwrap_or(0.0);
                        let quantity = item
                            .get("quantity")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(1) as f64;
                        price * quantity
                    })
                    .sum::<f64>()
            })
        })
        .unwrap_or(0.0);
    let currency = data
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("USD")
        .to_string();
    let country_code = data
        .get("country_code")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("country").and_then(|v| v.as_str()))
        .or_else(|| data.get("billing_country").and_then(|v| v.as_str()))
        .map(str::to_string)
        .or_else(|| {
            state
                .settings_for(company_id)
                .default_country_code
                .clone()
        });
    let region_code = data
        .get("region_code")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("region").and_then(|v| v.as_str()))
        .or_else(|| data.get("billing_region").and_then(|v| v.as_str()))
        .map(str::to_string)
        .or_else(|| state.settings_for(company_id).default_region_code.clone());
    let on = data
        .get("issue_date")
        .and_then(|v| v.as_str())
        .and_then(|raw| raw.get(..10))
        .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok())
        .unwrap_or_else(today);
    // An inclusive invoice already carries its tax inside the total:
    // compute over the total as the gross amount. An exclusive
    // invoice computes over the net subtotal.
    let tax_inclusive = data
        .get("tax_inclusive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let base = if tax_inclusive {
        data.get("total").and_then(|v| v.as_f64()).unwrap_or(amount)
    } else {
        amount
    };
    compute_and_store(
        &state,
        company_id,
        source_type,
        &source_id,
        base,
        &currency,
        country_code.as_deref(),
        region_code.as_deref(),
        on,
    );
}

/// Run the filing-reminder check for every company that has tax
/// settings stored, answering `tax.filing.check_requested`.
fn on_filing_check_requested(state: Arc<TaxState>, ctx: &mut HookContext) {
    let company_ids: Vec<Uuid> = if let Some(company_id) = ctx
        .get("company_id")
        .and_then(|v| v.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
    {
        vec![company_id]
    } else {
        state.settings.iter().map(|entry| *entry.key()).collect()
    };
    let mut checked = 0usize;
    for company_id in company_ids {
        checked += run_filing_check(&state, company_id).len();
    }
    ctx.add_result(json!({ "checked_filings": checked }));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct TaxModule {
    state: Arc<TaxState>,
}

impl TaxModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(TaxState::new()),
        }
    }
}

impl Default for TaxModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for TaxModule {
    fn id(&self) -> &'static str {
        "tax"
    }

    fn name(&self) -> &'static str {
        "Tax Management"
    }

    fn description(&self) -> &'static str {
        "Tax rules per jurisdiction, per-company tax settings, tax computation for invoices and orders, tax reports, filing reminders, and a generic HTTP connector framework for tax reporting and filing providers."
    }

    fn requires(&self) -> &[&'static str] {
        &["billing", "settings", "notifications", "scheduler", "audit"]
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Real billing event names, plus the `billing.*` alias
        // spellings named in the module brief (the billing module
        // emits `invoice.created` and consumes `order.created`).
        for event in ["invoice.created", "billing.invoice_created"] {
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                on_source_created(state.clone(), ctx, "invoice")
            });
            bus.register(event, "tax", handler);
        }
        for event in ["order.created", "billing.order_created"] {
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                on_source_created(state.clone(), ctx, "order")
            });
            bus.register(event, "tax", handler);
        }
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_filing_check_requested(state.clone(), ctx));
        bus.register("tax.filing.check_requested", "tax", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/tax")
                // Literal segments first: they must win over `/{id}`.
                .route("/rules", web::get().to(list_rules))
                .route("/rules", web::post().to(create_rule))
                .route("/rules/{id}", web::get().to(get_rule))
                .route("/rules/{id}", web::put().to(update_rule))
                .route("/rules/{id}", web::delete().to(delete_rule))
                .route("/compute", web::post().to(compute_tax))
                .route("/computations", web::get().to(list_computations))
                .route("/computations/{id}", web::get().to(get_computation))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/reports/summary", web::get().to(report_summary))
                .route("/reports/jurisdictions", web::get().to(report_jurisdictions))
                .route("/reports/periods", web::get().to(report_periods))
                .route("/reports/export", web::get().to(report_export))
                .route("/filings/check-due", web::post().to(check_due_filings))
                .route("/filings", web::get().to(list_filings))
                .route("/filings", web::post().to(create_filing))
                .route("/filings/{id}", web::get().to(get_filing))
                .route("/filings/{id}/submit", web::post().to(submit_filing))
                .route("/connectors", web::get().to(list_connectors))
                .route("/connectors", web::post().to(create_connector))
                .route("/connectors/{id}", web::get().to(get_connector))
                .route("/connectors/{id}", web::put().to(update_connector))
                .route("/connectors/{id}", web::delete().to(delete_connector))
                .route("/connectors/{id}/test", web::post().to(test_connector))
                .route("/audit", web::get().to(list_audit)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/tax/rules", owner: "tax", description: "List tax rules for the acting company (paginated; filter by country_code, region_code, tax_type, enabled)." },
            RouteInfo { method: "POST", path: "/api/v1/tax/rules", owner: "tax", description: "Create a tax rule for the acting company." },
            RouteInfo { method: "GET", path: "/api/v1/tax/rules/{id}", owner: "tax", description: "Get one tax rule." },
            RouteInfo { method: "PUT", path: "/api/v1/tax/rules/{id}", owner: "tax", description: "Update a tax rule." },
            RouteInfo { method: "DELETE", path: "/api/v1/tax/rules/{id}", owner: "tax", description: "Delete a tax rule." },
            RouteInfo { method: "POST", path: "/api/v1/tax/compute", owner: "tax", description: "Compute tax for an amount and jurisdiction; returns the line-by-line breakdown and stores the computation." },
            RouteInfo { method: "GET", path: "/api/v1/tax/computations", owner: "tax", description: "List stored tax computations (paginated; filter by source_type, source_id, country_code, currency)." },
            RouteInfo { method: "GET", path: "/api/v1/tax/computations/{id}", owner: "tax", description: "Get one stored tax computation." },
            RouteInfo { method: "GET", path: "/api/v1/tax/settings", owner: "tax", description: "Get the acting company's tax settings." },
            RouteInfo { method: "PUT", path: "/api/v1/tax/settings", owner: "tax", description: "Create or update the acting company's tax settings." },
            RouteInfo { method: "GET", path: "/api/v1/tax/reports/summary", owner: "tax", description: "Tax summary for a period: totals plus per-jurisdiction and per-period breakdowns." },
            RouteInfo { method: "GET", path: "/api/v1/tax/reports/jurisdictions", owner: "tax", description: "Tax totals grouped by jurisdiction for a period." },
            RouteInfo { method: "GET", path: "/api/v1/tax/reports/periods", owner: "tax", description: "Tax totals grouped by month for a period." },
            RouteInfo { method: "GET", path: "/api/v1/tax/reports/export", owner: "tax", description: "Export the tax report as JSON (default) or CSV (?format=csv)." },
            RouteInfo { method: "POST", path: "/api/v1/tax/filings/check-due", owner: "tax", description: "Run the filing-reminder check for the acting company: refresh filings and emit reminders for due and overdue filings." },
            RouteInfo { method: "GET", path: "/api/v1/tax/filings", owner: "tax", description: "List tax filings for the acting company (paginated; filter by status, country_code, frequency)." },
            RouteInfo { method: "POST", path: "/api/v1/tax/filings", owner: "tax", description: "Create a tax filing period for the acting company." },
            RouteInfo { method: "GET", path: "/api/v1/tax/filings/{id}", owner: "tax", description: "Get one tax filing." },
            RouteInfo { method: "POST", path: "/api/v1/tax/filings/{id}/submit", owner: "tax", description: "Submit a tax filing, manually or through a connector." },
            RouteInfo { method: "GET", path: "/api/v1/tax/connectors", owner: "tax", description: "List tax provider connectors for the acting company (credentials are never returned)." },
            RouteInfo { method: "POST", path: "/api/v1/tax/connectors", owner: "tax", description: "Create a tax provider connector (credential is write-only, stored encrypted)." },
            RouteInfo { method: "GET", path: "/api/v1/tax/connectors/{id}", owner: "tax", description: "Get one tax provider connector (credential fingerprint only)." },
            RouteInfo { method: "PUT", path: "/api/v1/tax/connectors/{id}", owner: "tax", description: "Update a tax provider connector (a supplied credential replaces the stored one)." },
            RouteInfo { method: "DELETE", path: "/api/v1/tax/connectors/{id}", owner: "tax", description: "Delete a tax provider connector." },
            RouteInfo { method: "POST", path: "/api/v1/tax/connectors/{id}/test", owner: "tax", description: "Test a tax provider connector (example kind returns a simulated result and performs no network call)." },
            RouteInfo { method: "GET", path: "/api/v1/tax/audit", owner: "tax", description: "List this module's own tax audit trail (paginated; filter by action, resource_type, resource_id)." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TaxModule::new()));
}

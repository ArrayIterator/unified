// Privacy module.
//
// PII redaction helpers, data-minimization field declarations,
// one-way anonymization, private-by-default visibility, consent
// records, right-to-erasure requests, and versioned privacy
// notices. The in-memory DashMap store is authoritative; the
// migration 2026-11-22-000065_privacy tables are persistence
// targets only (see the migration header). Interconnection with
// the Compliance, Vault, and Files modules happens only through
// the hook bus: this module emits events, it never calls into
// those modules directly.
//
// Security notes:
//   - Every endpoint requires the `X-User-Id` and `X-Role`
//     headers and a capability check (`privacy:read`,
//     `privacy:write`, or `privacy:admin`), failing closed.
//   - Anonymization tokens are HMAC-SHA-256 values keyed by a
//     server-side key that is write-only: it is read from the
//     `UNIFIED_PRIVACY_ANONYMIZATION_KEY` environment variable
//     at startup (or generated in memory when unset) and is
//     never stored in a record, logged, or returned. The
//     mapping is not reversible. Tokens are stable across
//     restarts only when the environment key is configured.
//   - No original personal data is kept in anonymization,
//     erasure, or audit records: only derived tokens, scopes,
//     and actor ids.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

pub const PERM_PRIVACY_READ: &str = "privacy:read";
pub const PERM_PRIVACY_WRITE: &str = "privacy:write";
pub const PERM_PRIVACY_ADMIN: &str = "privacy:admin";

/// Event emitted when a user is anonymized. Payload:
/// `{"user_id", "token", "scope_modules", "actor_id", "reason",
/// "already_anonymized"}`. The token is the derived token, never
/// an original value.
pub const EVENT_USER_ANONYMIZED: &str = "user.anonymized";
/// Privacy action events, emitted for the audit log to consume
/// (the integrator adds them to the audit module's subscription
/// list): `privacy.anonymized`, `privacy.consent.changed`,
/// `privacy.erasure.requested`, `privacy.erasure.fulfilled`,
/// `privacy.visibility.changed`, `privacy.fields.changed`,
/// `privacy.notice.published`.
pub const EVENT_PRIVACY_ANONYMIZED: &str = "privacy.anonymized";
pub const EVENT_PRIVACY_CONSENT_CHANGED: &str = "privacy.consent.changed";
pub const EVENT_PRIVACY_ERASURE_REQUESTED: &str = "privacy.erasure.requested";
pub const EVENT_PRIVACY_ERASURE_FULFILLED: &str = "privacy.erasure.fulfilled";
pub const EVENT_PRIVACY_VISIBILITY_CHANGED: &str = "privacy.visibility.changed";
pub const EVENT_PRIVACY_FIELDS_CHANGED: &str = "privacy.fields.changed";
pub const EVENT_PRIVACY_NOTICE_PUBLISHED: &str = "privacy.notice.published";
/// Compliance events this module emits for the Compliance module
/// to consume (this module never edits compliance code).
pub const EVENT_COMPLIANCE_CONSENT_GRANTED: &str = "compliance.consent.granted";
pub const EVENT_COMPLIANCE_CONSENT_WITHDRAWN: &str = "compliance.consent.withdrawn";
pub const EVENT_COMPLIANCE_ERASURE_REQUESTED: &str = "compliance.erasure.requested";
pub const EVENT_COMPLIANCE_ERASURE_FULFILLED: &str = "compliance.erasure.fulfilled";

/// Generic fallback notice returned for a module that has no
/// published notice of its own. Always marked `generic: true`.
pub const GENERIC_NOTICE_TEXT: &str = "We collect only the personal data needed to provide this feature, keep it private by default, and never share it without your consent. You may request access, correction, or erasure of your personal data at any time.";

/// Built-in consent types. Admins may define more through
/// `POST /api/v1/privacy/consent-types`.
pub const BUILTIN_CONSENT_TYPES: &[&str] =
    &["marketing", "analytics", "data-processing", "ai-training"];

/// Keys that look like personal data. A payload key matches when
/// its lowercased form contains one of these substrings.
const PII_KEY_MARKERS: &[&str] = &[
    "email", "mail", "phone", "tel", "mobile", "cell", "address", "street", "name",
    "birth", "dob", "national", "passport", "ssn", "tax_id", "taxid", "bank", "iban",
    "card_number", "salary", "gender", "photo", "latitude", "longitude", "ip_address",
    "device_id", "biometric", "fingerprint", "face", "account_number", "emergency_contact",
];

fn email_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").expect("email regex")
    })
}

fn phone_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\+?\d[\d\s().\-]{7,17}\d").expect("phone regex")
    })
}

fn long_digits_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b\d{10,}\b").expect("long digits regex"))
}

// ---------------------------------------------------------------------------
// PII redaction helpers (public, for other modules to reuse)
// ---------------------------------------------------------------------------

/// Redact an email address, keeping the first character of the
/// local part and the full domain: `jane@example.com` becomes
/// `j***@example.com`. Values without `@` become `***`.
pub fn redact_email(value: &str) -> String {
    let trimmed = value.trim();
    match trimmed.split_once('@') {
        Some((local, domain)) if !domain.is_empty() => {
            let first = local.chars().next().unwrap_or('*');
            format!("{first}***@{domain}")
        }
        _ => "***".to_string(),
    }
}

/// Redact a phone number, keeping only the last two digits and a
/// leading `+` when present: `+62 812-3456-7890` becomes `+***90`.
pub fn redact_phone(value: &str) -> String {
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 4 {
        return "***".to_string();
    }
    let tail = &digits[digits.len() - 2..];
    if value.trim_start().starts_with('+') {
        format!("+***{tail}")
    } else {
        format!("***{tail}")
    }
}

/// Redact an ID number, keeping the first two and last two
/// characters: `1234567890123456` becomes `12************56`.
pub fn redact_id_number(value: &str) -> String {
    let trimmed = value.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 4 {
        return "***".to_string();
    }
    let first: String = chars.iter().take(2).collect();
    let last: String = chars.iter().skip(chars.len() - 2).collect();
    format!("{first}{}{last}", "*".repeat(chars.len() - 4))
}

/// Redact a personal name, keeping only the first character:
/// `Jane Doe` becomes `J***`.
pub fn redact_name(value: &str) -> String {
    match value.trim().chars().next() {
        Some(first) => format!("{first}***"),
        None => "***".to_string(),
    }
}

/// True when the match looks like a date or time rather than a
/// phone number, so `redact_text` leaves timestamps alone.
fn looks_like_date_or_time(matched: &str) -> bool {
    if matched.contains(':') {
        return true;
    }
    let bytes = matched.as_bytes();
    // `YYYY-MM-DD` or `YYYY/MM/DD` at the start of the match.
    bytes.len() >= 10
        && bytes[0..4].iter().all(|b| b.is_ascii_digit())
        && (bytes[4] == b'-' || bytes[4] == b'/')
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && (bytes[7] == b'-' || bytes[7] == b'/')
}

/// Redact emails, phone-like numbers, and long ID-like digit runs
/// inside free text, keeping enough structure to stay useful.
/// Timestamps and dates are left untouched.
pub fn redact_text(input: &str) -> String {
    let step1 = email_re().replace_all(input, |caps: &Captures| {
        redact_email(caps.get(0).map(|m| m.as_str()).unwrap_or(""))
    });
    let step2 = long_digits_re().replace_all(&step1, |caps: &Captures| {
        redact_id_number(caps.get(0).map(|m| m.as_str()).unwrap_or(""))
    });
    let step3 = phone_re().replace_all(&step2, |caps: &Captures| {
        let matched = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        if looks_like_date_or_time(matched) {
            return matched.to_string();
        }
        let digits = matched.chars().filter(|c| c.is_ascii_digit()).count();
        if (7..=15).contains(&digits) {
            redact_phone(matched)
        } else {
            matched.to_string()
        }
    });
    step3.into_owned()
}

/// True when a JSON object key looks like personal data.
pub fn is_pii_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    PII_KEY_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Redact a JSON value in place, walking objects and arrays.
/// String values under keys that look like personal data are
/// fully redacted (email keys with `redact_email`, phone keys
/// with `redact_phone`, name keys with `redact_name` only when
/// `redact_names` is true, other PII keys by value shape);
/// every other string value is passed through `redact_text`,
/// which only masks detected email / phone / ID patterns.
pub fn redact_value(value: &Value, redact_names: bool) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, item) in map {
                out.insert(key.clone(), redact_value_for_key(key, item, redact_names));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_value(item, redact_names))
                .collect(),
        ),
        Value::String(text) => Value::String(redact_text(text)),
        other => other.clone(),
    }
}

fn redact_value_for_key(key: &str, value: &Value, redact_names: bool) -> Value {
    if !is_pii_key(key) {
        return redact_value(value, redact_names);
    }
    let lower = key.to_lowercase();
    match value {
        Value::String(text) => {
            let redacted = if lower.contains("email") || lower.contains("mail") {
                redact_email(text)
            } else if lower.contains("phone")
                || lower.contains("tel")
                || lower.contains("mobile")
                || lower.contains("cell")
            {
                redact_phone(text)
            } else if lower.contains("name") {
                if redact_names {
                    redact_name(text)
                } else {
                    redact_text(text)
                }
            } else if text.contains('@') {
                redact_email(text)
            } else if text.chars().filter(|c| c.is_ascii_digit()).count() >= 10
                && text
                    .chars()
                    .all(|c| c.is_ascii_digit() || c.is_whitespace() || c == '-' || c == '+')
            {
                redact_id_number(text)
            } else {
                redact_text(text)
            };
            Value::String(redacted)
        }
        Value::Object(_) | Value::Array(_) => redact_value(value, redact_names),
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// Visibility
// ---------------------------------------------------------------------------

/// Visibility of a record or collection. Ordered narrowest to
/// widest; new records default to the narrowest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Private,
    Company,
    Public,
}

impl Visibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Visibility::Private => "private",
            Visibility::Company => "company",
            Visibility::Public => "public",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "private" => Some(Visibility::Private),
            "company" => Some(Visibility::Company),
            "public" => Some(Visibility::Public),
            _ => None,
        }
    }
}

/// The default visibility for new records: always private.
/// Callers widen it only through an explicitly configured
/// per-module default (see `PrivacyState::effective_visibility`).
pub fn default_visibility() -> Visibility {
    Visibility::Private
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDeclaration {
    pub id: String,
    pub module_id: String,
    pub field_name: String,
    pub purpose: String,
    pub required: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnonymizationRecord {
    pub id: String,
    pub user_id: String,
    /// Derived HMAC-SHA-256 hex token. Not reversible, and no
    /// original value is stored anywhere in this record.
    pub token: String,
    /// Per-field derived tokens, keyed by field name.
    pub field_tokens: HashMap<String, String>,
    pub scope_modules: Vec<String>,
    pub reason: String,
    pub actor: String,
    /// How many times anonymization was applied to this user.
    /// Re-applying yields the same tokens and only bumps this.
    pub times_applied: u64,
    pub anonymized_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisibilityDefault {
    pub module_id: String,
    pub visibility: Visibility,
    /// True only when an admin explicitly widened the default
    /// beyond private. Private defaults are never "explicit".
    pub explicit: bool,
    pub configured_by: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentTypeDef {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentRecord {
    pub id: String,
    pub user_id: String,
    pub consent_type: String,
    pub granted: bool,
    pub granted_at: Option<DateTime<Utc>>,
    pub withdrawn_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub source: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ConsentRecord {
    /// Current status, derived from the stored flags and expiry:
    /// `withdrawn` wins over expiry, expiry wins over granted.
    pub fn status(&self, now: DateTime<Utc>) -> &'static str {
        if !self.granted || self.withdrawn_at.is_some() {
            return "withdrawn";
        }
        if let Some(expires_at) = self.expires_at {
            if now > expires_at {
                return "expired";
            }
        }
        "granted"
    }

    pub fn is_currently_granted(&self, now: DateTime<Utc>) -> bool {
        self.status(now) == "granted"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentHistoryEntry {
    pub id: String,
    pub consent_id: String,
    pub user_id: String,
    pub consent_type: String,
    pub action: String,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErasureRequest {
    pub id: String,
    pub user_id: String,
    pub modules: Vec<String>,
    pub reason: String,
    pub status: String,
    pub requested_by: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
    pub fulfilled_at: Option<DateTime<Utc>>,
    pub anonymization_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyNotice {
    pub id: String,
    pub module_id: String,
    pub text: String,
    pub version: u64,
    pub effective_from: DateTime<Utc>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyAuditEntry {
    pub id: String,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub details: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentNotice {
    pub module_id: String,
    pub notice_id: Option<String>,
    pub text: String,
    pub version: u64,
    pub effective_from: Option<DateTime<Utc>>,
    /// True when no module notice exists and the generic
    /// fallback text is returned instead.
    pub generic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectiveVisibility {
    pub module_id: String,
    pub visibility: Visibility,
    /// `configured` when an admin set a per-module default,
    /// `default` when the private fallback applies.
    pub source: String,
    pub explicit: bool,
}

// ---------------------------------------------------------------------------
// Request inputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct RedactInput {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub value: Option<Value>,
    #[serde(default)]
    pub redact_names: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFieldDeclarationInput {
    pub module_id: String,
    pub field_name: String,
    pub purpose: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFieldDeclarationInput {
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValidatePayloadInput {
    pub module_id: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnonymizeInput {
    pub user_id: String,
    #[serde(default)]
    pub scope_modules: Vec<String>,
    #[serde(default)]
    pub reason: String,
    /// Personal-data field names to derive per-field tokens for.
    #[serde(default)]
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetVisibilityInput {
    pub module_id: String,
    pub visibility: String,
    /// Must be true to widen the default beyond private.
    #[serde(default)]
    pub explicit: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateConsentTypeInput {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GrantConsentInput {
    pub user_id: String,
    pub consent_type: String,
    pub granted: bool,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateErasureInput {
    pub user_id: String,
    #[serde(default)]
    pub modules: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RejectErasureInput {
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateNoticeInput {
    pub module_id: String,
    pub text: String,
    pub version: u64,
    pub effective_from: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CurrentConsentQuery {
    pub user_id: String,
    pub consent_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CurrentNoticeQuery {
    pub module_id: String,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct PrivacyState {
    hooks: RwLock<Arc<HookBus>>,
    declarations: DashMap<String, FieldDeclaration>,
    anonymizations: DashMap<String, AnonymizationRecord>,
    visibility_defaults: DashMap<String, VisibilityDefault>,
    consent_types: DashMap<String, ConsentTypeDef>,
    consents: DashMap<String, ConsentRecord>,
    consent_history: DashMap<String, ConsentHistoryEntry>,
    erasure_requests: DashMap<String, ErasureRequest>,
    notices: DashMap<String, PrivacyNotice>,
    audit_entries: DashMap<String, PrivacyAuditEntry>,
    /// Write-only anonymization key. Never serialized, logged,
    /// or returned in any response.
    anonymization_key: [u8; 32],
}

fn load_anonymization_key() -> [u8; 32] {
    match std::env::var("UNIFIED_PRIVACY_ANONYMIZATION_KEY") {
        Ok(configured) if configured.trim().len() >= 16 => {
            let mut hasher = Sha256::new();
            hasher.update(configured.trim().as_bytes());
            hasher.finalize().into()
        }
        _ => {
            // No key configured: generate one in memory. Tokens
            // stay stable for the life of this process only; set
            // the environment variable for stable tokens across
            // restarts.
            let seed = Uuid::new_v4();
            let mut hasher = Sha256::new();
            hasher.update(seed.as_bytes());
            hasher.update(Utc::now().to_rfc3339().as_bytes());
            hasher.finalize().into()
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

impl PrivacyState {
    fn new() -> Self {
        let now = Utc::now();
        let consent_types: DashMap<String, ConsentTypeDef> = DashMap::new();
        for slug in BUILTIN_CONSENT_TYPES {
            consent_types.insert(
                slug.to_string(),
                ConsentTypeDef {
                    slug: slug.to_string(),
                    name: slug.to_string(),
                    description: format!("Built-in consent type: {slug}"),
                    created_at: now,
                },
            );
        }
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            declarations: DashMap::new(),
            anonymizations: DashMap::new(),
            visibility_defaults: DashMap::new(),
            consent_types,
            consents: DashMap::new(),
            consent_history: DashMap::new(),
            erasure_requests: DashMap::new(),
            notices: DashMap::new(),
            audit_entries: DashMap::new(),
            anonymization_key: load_anonymization_key(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Record a privacy action in the local append-only audit
    /// trail and emit it on the hook bus so the audit module can
    /// consume it. No privacy change is ever silent.
    fn audit(&self, event: &str, actor: &str, action: &str, resource: &str, details: Value) {
        let entry = PrivacyAuditEntry {
            id: new_id(),
            actor: actor.to_string(),
            action: action.to_string(),
            resource: resource.to_string(),
            details: details.clone(),
            created_at: Utc::now(),
        };
        self.audit_entries.insert(entry.id.clone(), entry);
        let mut payload = details;
        if let Value::Object(map) = &mut payload {
            map.insert("actor_id".to_string(), json!(actor));
            map.insert("action".to_string(), json!(action));
            map.insert("resource".to_string(), json!(resource));
        }
        self.emit(event, payload);
    }

    fn hmac_hex(&self, message: &str) -> UnifiedResult<String> {
        let mut mac = HmacSha256::new_from_slice(&self.anonymization_key)
            .map_err(|_| UnifiedError::internal("Anonymization key is not usable"))?;
        mac.update(message.as_bytes());
        Ok(hex_encode(&mac.finalize().into_bytes()))
    }

    /// Derived token for a user. Deterministic: the same user
    /// always yields the same token under the same key.
    pub fn anonymization_token(&self, user_id: &str) -> UnifiedResult<String> {
        self.hmac_hex(&format!("privacy-anonymize-v1:{user_id}"))
    }

    fn field_token(&self, user_id: &str, field: &str) -> UnifiedResult<String> {
        self.hmac_hex(&format!("privacy-anonymize-v1:{user_id}:{field}"))
    }

    /// One-way anonymization of a user. Idempotent: anonymizing
    /// the same user again returns the same tokens and only bumps
    /// `times_applied` and the timestamps. Never stores or
    /// returns an original value.
    fn anonymize_internal(
        &self,
        user_id: &str,
        scope_modules: &[String],
        fields: &[String],
        reason: &str,
        actor: &str,
    ) -> UnifiedResult<AnonymizationRecord> {
        let token = self.anonymization_token(user_id)?;
        let mut field_tokens: HashMap<String, String> = HashMap::new();
        for field in fields {
            field_tokens.insert(field.clone(), self.field_token(user_id, field)?);
        }
        let now = Utc::now();
        if let Some(mut existing) = self.anonymizations.get_mut(user_id) {
            existing.times_applied += 1;
            existing.updated_at = now;
            for (field, field_token) in field_tokens {
                existing.field_tokens.insert(field, field_token);
            }
            if !scope_modules.is_empty() {
                for module_id in scope_modules {
                    if !existing.scope_modules.contains(module_id) {
                        existing.scope_modules.push(module_id.to_string());
                    }
                }
            }
            let record = existing.clone();
            self.audit(
                EVENT_PRIVACY_ANONYMIZED,
                actor,
                "user.anonymized",
                &format!("user:{user_id}"),
                json!({
                    "user_id": user_id,
                    "token": record.token,
                    "scope_modules": record.scope_modules,
                    "reason": reason,
                    "already_anonymized": true,
                    "times_applied": record.times_applied,
                }),
            );
            self.emit(
                EVENT_USER_ANONYMIZED,
                json!({
                    "user_id": user_id,
                    "token": record.token,
                    "scope_modules": record.scope_modules,
                    "actor_id": actor,
                    "reason": reason,
                    "already_anonymized": true,
                }),
            );
            return Ok(record);
        }
        let record = AnonymizationRecord {
            id: new_id(),
            user_id: user_id.to_string(),
            token,
            field_tokens,
            scope_modules: scope_modules.to_vec(),
            reason: reason.to_string(),
            actor: actor.to_string(),
            times_applied: 1,
            anonymized_at: now,
            created_at: now,
            updated_at: now,
        };
        self.anonymizations
            .insert(user_id.to_string(), record.clone());
        self.audit(
            EVENT_PRIVACY_ANONYMIZED,
            actor,
            "user.anonymized",
            &format!("user:{user_id}"),
            json!({
                "user_id": user_id,
                "token": record.token,
                "scope_modules": record.scope_modules,
                "reason": reason,
                "already_anonymized": false,
                "times_applied": 1,
            }),
        );
        self.emit(
            EVENT_USER_ANONYMIZED,
            json!({
                "user_id": user_id,
                "token": record.token,
                "scope_modules": record.scope_modules,
                "actor_id": actor,
                "reason": reason,
                "already_anonymized": false,
            }),
        );
        Ok(record)
    }

    /// Declared personal-data field names for a module. A module
    /// with no declaration allows no personal data fields.
    fn declared_fields(&self, module_id: &str) -> Vec<String> {
        let mut fields: Vec<String> = self
            .declarations
            .iter()
            .filter(|entry| entry.value().module_id == module_id)
            .map(|entry| entry.value().field_name.clone())
            .collect();
        fields.sort();
        fields
    }

    /// Effective visibility default for a module: the configured
    /// default when one exists, otherwise private.
    pub fn effective_visibility(&self, module_id: &str) -> EffectiveVisibility {
        match self.visibility_defaults.get(module_id) {
            Some(entry) => EffectiveVisibility {
                module_id: module_id.to_string(),
                visibility: entry.visibility,
                source: "configured".to_string(),
                explicit: entry.explicit,
            },
            None => EffectiveVisibility {
                module_id: module_id.to_string(),
                visibility: default_visibility(),
                source: "default".to_string(),
                explicit: false,
            },
        }
    }

    /// Current notice for a module: the highest version whose
    /// effective date has passed, or the generic fallback.
    pub fn current_notice(&self, module_id: &str) -> CurrentNotice {
        let now = Utc::now();
        let mut best: Option<PrivacyNotice> = None;
        for entry in self.notices.iter() {
            let notice = entry.value();
            if notice.module_id != module_id || notice.effective_from > now {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|current| notice.version > current.version)
            {
                best = Some(notice.clone());
            }
        }
        match best {
            Some(notice) => CurrentNotice {
                module_id: module_id.to_string(),
                notice_id: Some(notice.id),
                text: notice.text,
                version: notice.version,
                effective_from: Some(notice.effective_from),
                generic: false,
            },
            None => CurrentNotice {
                module_id: module_id.to_string(),
                notice_id: None,
                text: GENERIC_NOTICE_TEXT.to_string(),
                version: 0,
                effective_from: None,
                generic: true,
            },
        }
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

/// Caller identity: `X-User-Id` (required) plus `X-Role`
/// (required for capability checks), exactly like the ai and
/// events modules.
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

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not
/// allowed (same fail-closed shape as the ai module).
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

fn valid_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn valid_field_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn validate_module_list(modules: &[String]) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for module_id in modules {
        let cleaned = module_id.trim().to_string();
        if !valid_slug(&cleaned) {
            return Err(UnifiedError::bad_request(format!(
                "Module id \"{module_id}\" must be a lowercase slug"
            )));
        }
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    Ok(out)
}

fn sort_desc_by_created<T>(items: &mut Vec<T>, created_at: fn(&T) -> DateTime<Utc>) {
    items.sort_by(|a, b| created_at(b).cmp(&created_at(a)));
}

// ---------------------------------------------------------------------------
// Handlers: redaction, field declarations, payload validation
// ---------------------------------------------------------------------------

async fn redact(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<RedactInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let input = body.into_inner();
        let redacted = match (&input.text, &input.value) {
            (Some(text), _) => json!({ "text": redact_text(text) }),
            (None, Some(value)) => {
                json!({ "value": redact_value(value, input.redact_names) })
            }
            (None, None) => {
                return Err(UnifiedError::bad_request(
                    "Provide \"text\" or \"value\" to redact",
                ));
            }
        };
        state.audit(
            "privacy.redaction.used",
            &caller.actor,
            "privacy.redact",
            "privacy:redact",
            json!({ "redact_names": input.redact_names }),
        );
        Ok(redacted)
    })();
    json::respond(result, debug)
}

async fn list_field_declarations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<FieldDeclaration>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let mut items: Vec<FieldDeclaration> = state
            .declarations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(
                    &format!("{} {} {}", record.module_id, record.field_name, record.purpose),
                    &params,
                ) && params
                    .filters
                    .get("module_id")
                    .is_none_or(|module_id| record.module_id == *module_id)
                    && params
                        .filters
                        .get("field_name")
                        .is_none_or(|field| record.field_name == *field)
            })
            .collect();
        sort_desc_by_created(&mut items, |record| record.created_at);
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_field_declaration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<CreateFieldDeclarationInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FieldDeclaration> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_WRITE)?;
        let input = body.into_inner();
        if !valid_slug(&input.module_id) {
            return Err(UnifiedError::bad_request(
                "module_id must be a lowercase slug",
            ));
        }
        if !valid_field_name(&input.field_name) {
            return Err(UnifiedError::bad_request(
                "field_name must be lowercase letters, digits, or '_'",
            ));
        }
        let purpose = require_text(&input.purpose, "purpose", 500)?;
        if state.declarations.iter().any(|entry| {
            entry.value().module_id == input.module_id
                && entry.value().field_name == input.field_name
        }) {
            return Err(UnifiedError::conflict(format!(
                "Field \"{}\" is already declared for module \"{}\"",
                input.field_name, input.module_id
            )));
        }
        let record = FieldDeclaration {
            id: new_id(),
            module_id: input.module_id,
            field_name: input.field_name,
            purpose,
            required: input.required,
            created_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .declarations
            .insert(record.id.clone(), record.clone());
        state.audit(
            EVENT_PRIVACY_FIELDS_CHANGED,
            &caller.actor,
            "privacy.field_declaration.created",
            &format!("module:{}", record.module_id),
            json!({
                "module_id": record.module_id,
                "field_name": record.field_name,
                "declaration_id": record.id,
            }),
        );
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_field_declaration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FieldDeclaration> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        state
            .declarations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Field declaration not found"))
    })();
    json::respond(result, debug)
}

async fn update_field_declaration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFieldDeclarationInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FieldDeclaration> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_WRITE)?;
        let input = body.into_inner();
        let mut record = state
            .declarations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Field declaration not found"))?;
        if let Some(purpose) = &input.purpose {
            record.purpose = require_text(purpose, "purpose", 500)?;
        }
        if let Some(required) = input.required {
            record.required = required;
        }
        record.updated_at = now();
        state
            .declarations
            .insert(record.id.clone(), record.clone());
        state.audit(
            EVENT_PRIVACY_FIELDS_CHANGED,
            &caller.actor,
            "privacy.field_declaration.updated",
            &format!("module:{}", record.module_id),
            json!({
                "module_id": record.module_id,
                "field_name": record.field_name,
                "declaration_id": record.id,
            }),
        );
        Ok(record)
    })();
    json::respond(result, debug)
}

async fn delete_field_declaration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_WRITE)?;
        let record = state
            .declarations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Field declaration not found"))?;
        state.declarations.remove(path.as_str());
        state.audit(
            EVENT_PRIVACY_FIELDS_CHANGED,
            &caller.actor,
            "privacy.field_declaration.deleted",
            &format!("module:{}", record.module_id),
            json!({
                "module_id": record.module_id,
                "field_name": record.field_name,
                "declaration_id": record.id,
            }),
        );
        Ok(json!({ "deleted": true, "id": record.id }))
    })();
    json::respond(result, debug)
}

/// Validate an incoming payload against the declared personal-data
/// fields of a module. Undeclared personal-data keys are rejected
/// with a 422 listing them; a module with no declaration allows no
/// personal-data fields at all. Non-personal keys always pass.
async fn validate_payload(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<ValidatePayloadInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_WRITE)?;
        let input = body.into_inner();
        if !valid_slug(&input.module_id) {
            return Err(UnifiedError::bad_request(
                "module_id must be a lowercase slug",
            ));
        }
        let object = match input.payload.as_object() {
            Some(object) => object,
            None => {
                return Err(UnifiedError::bad_request(
                    "payload must be a JSON object",
                ));
            }
        };
        let declared = state.declared_fields(&input.module_id);
        let mut rejected: Vec<String> = Vec::new();
        for key in object.keys() {
            if is_pii_key(key) && !declared.iter().any(|field| field == key) {
                rejected.push(key.clone());
            }
        }
        rejected.sort();
        if !rejected.is_empty() {
            return Err(UnifiedError::unprocessable(format!(
                "Payload for module \"{}\" contains undeclared personal-data fields: {}",
                input.module_id,
                rejected.join(", ")
            ))
            .with_metadata("rejected_fields", json!(rejected))
            .with_metadata("module_id", json!(input.module_id))
            .with_metadata("declared_fields", json!(declared)));
        }
        Ok(json!({
            "module_id": input.module_id,
            "allowed": true,
            "checked_fields": object.keys().collect::<Vec<_>>(),
            "declared_fields": declared,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: anonymization and visibility
// ---------------------------------------------------------------------------

async fn anonymize_user(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<AnonymizeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AnonymizationRecord> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_ADMIN)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 256)?;
        let scope_modules = validate_module_list(&input.scope_modules)?;
        let mut fields: Vec<String> = Vec::new();
        for field in &input.fields {
            if !valid_field_name(field) {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{field}\" must be lowercase letters, digits, or '_'"
                )));
            }
            if !fields.contains(field) {
                fields.push(field.clone());
            }
        }
        let reason = if input.reason.trim().is_empty() {
            "admin anonymization".to_string()
        } else {
            require_text(&input.reason, "reason", 500)?
        };
        state.anonymize_internal(&user_id, &scope_modules, &fields, &reason, &caller.actor)
    })();
    json::respond(result, debug)
}

async fn list_anonymizations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AnonymizationRecord>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let mut items: Vec<AnonymizationRecord> = state
            .anonymizations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(&format!("{} {}", record.user_id, record.token), &params)
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|user_id| record.user_id == *user_id)
            })
            .collect();
        sort_desc_by_created(&mut items, |record| record.created_at);
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn get_anonymization(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AnonymizationRecord> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        state
            .anonymizations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Anonymization record not found"))
    })();
    json::respond(result, debug)
}

async fn list_visibility_defaults(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<VisibilityDefault>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let mut items: Vec<VisibilityDefault> = state
            .visibility_defaults
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(&record.module_id, &params)
                    && params
                        .filters
                        .get("module_id")
                        .is_none_or(|module_id| record.module_id == *module_id)
                    && params
                        .filters
                        .get("visibility")
                        .is_none_or(|visibility| record.visibility.as_str() == visibility)
            })
            .collect();
        items.sort_by(|a, b| a.module_id.cmp(&b.module_id));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn set_visibility_default(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<SetVisibilityInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VisibilityDefault> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_ADMIN)?;
        let input = body.into_inner();
        if !valid_slug(&input.module_id) {
            return Err(UnifiedError::bad_request(
                "module_id must be a lowercase slug",
            ));
        }
        let visibility = Visibility::parse(&input.visibility).ok_or_else(|| {
            UnifiedError::bad_request(
                "visibility must be one of: private, company, public",
            )
        })?;
        // Widening beyond private is only allowed when the admin
        // explicitly confirms it; a missing or false `explicit`
        // flag keeps the default at private.
        if visibility != Visibility::Private && !input.explicit {
            return Err(UnifiedError::unprocessable(
                "Widening a visibility default beyond private requires explicit=true",
            ));
        }
        let record = VisibilityDefault {
            module_id: input.module_id.clone(),
            visibility,
            explicit: input.explicit && visibility != Visibility::Private,
            configured_by: caller.actor.clone(),
            updated_at: now(),
        };
        state
            .visibility_defaults
            .insert(input.module_id.clone(), record.clone());
        state.audit(
            EVENT_PRIVACY_VISIBILITY_CHANGED,
            &caller.actor,
            "privacy.visibility.changed",
            &format!("module:{}", record.module_id),
            json!({
                "module_id": record.module_id,
                "visibility": record.visibility.as_str(),
                "explicit": record.explicit,
            }),
        );
        Ok(record)
    })();
    json::respond(result, debug)
}

async fn get_effective_visibility(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EffectiveVisibility> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let module_id = path.into_inner();
        if !valid_slug(&module_id) {
            return Err(UnifiedError::bad_request(
                "module_id must be a lowercase slug",
            ));
        }
        Ok(state.effective_visibility(&module_id))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: consent types and consent records
// ---------------------------------------------------------------------------

async fn list_consent_types(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ConsentTypeDef>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let mut items: Vec<ConsentTypeDef> = state
            .consent_types
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| matches_search(&format!("{} {}", record.slug, record.name), &params))
            .collect();
        items.sort_by(|a, b| a.slug.cmp(&b.slug));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_consent_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<CreateConsentTypeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ConsentTypeDef> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_ADMIN)?;
        let input = body.into_inner();
        if !valid_slug(&input.slug) {
            return Err(UnifiedError::bad_request(
                "slug must be a lowercase slug",
            ));
        }
        let name = require_text(&input.name, "name", 128)?;
        let description = input.description.trim().to_string();
        if description.len() > 500 {
            return Err(UnifiedError::bad_request(
                "description must be at most 500 characters",
            ));
        }
        if state.consent_types.contains_key(&input.slug) {
            return Err(UnifiedError::conflict(format!(
                "Consent type \"{}\" already exists",
                input.slug
            )));
        }
        let record = ConsentTypeDef {
            slug: input.slug.clone(),
            name,
            description,
            created_at: now(),
        };
        state
            .consent_types
            .insert(input.slug.clone(), record.clone());
        state.audit(
            EVENT_PRIVACY_CONSENT_CHANGED,
            &caller.actor,
            "privacy.consent_type.created",
            &format!("consent-type:{}", record.slug),
            json!({ "consent_type": record.slug }),
        );
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

fn record_consent_change(
    state: &PrivacyState,
    record: &ConsentRecord,
    action: &str,
    actor: &str,
) {
    let entry = ConsentHistoryEntry {
        id: new_id(),
        consent_id: record.id.clone(),
        user_id: record.user_id.clone(),
        consent_type: record.consent_type.clone(),
        action: action.to_string(),
        actor: actor.to_string(),
        created_at: now(),
    };
    state
        .consent_history
        .insert(entry.id.clone(), entry);
    let event = if action == "granted" {
        EVENT_COMPLIANCE_CONSENT_GRANTED
    } else {
        EVENT_COMPLIANCE_CONSENT_WITHDRAWN
    };
    state.emit(
        event,
        json!({
            "user_id": record.user_id,
            "consent_type": record.consent_type,
            "consent_id": record.id,
            "actor_id": actor,
            "expires_at": record.expires_at,
        }),
    );
    state.audit(
        EVENT_PRIVACY_CONSENT_CHANGED,
        actor,
        format!("privacy.consent.{action}").as_str(),
        &format!("user:{}", record.user_id),
        json!({
            "user_id": record.user_id,
            "consent_type": record.consent_type,
            "consent_id": record.id,
            "granted": record.granted,
            "expires_at": record.expires_at,
        }),
    );
}

async fn grant_consent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<GrantConsentInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(ConsentRecord, bool)> = (|| {
        let caller = caller(&req)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 256)?;
        if !valid_slug(&input.consent_type) {
            return Err(UnifiedError::bad_request(
                "consent_type must be a lowercase slug",
            ));
        }
        if !state.consent_types.contains_key(&input.consent_type) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown consent type \"{}\"",
                input.consent_type
            )));
        }
        // Self-service: the subject user may grant or withdraw
        // their own consent without a write capability; anyone
        // else needs privacy:write.
        if caller.actor != user_id {
            require_permission(&ctx, &caller.role, PERM_PRIVACY_WRITE)?;
        }
        if let Some(expires_at) = input.expires_at {
            if expires_at <= now() {
                return Err(UnifiedError::bad_request(
                    "expires_at must be in the future",
                ));
            }
        }
        let source = input
            .source
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("api")
            .to_string();
        if source.len() > 128 {
            return Err(UnifiedError::bad_request(
                "source must be at most 128 characters",
            ));
        }
        let existing = state
            .consents
            .iter()
            .find(|entry| {
                entry.value().user_id == user_id
                    && entry.value().consent_type == input.consent_type
            })
            .map(|entry| entry.value().clone());
        let action = if input.granted { "granted" } else { "withdrawn" };
        match existing {
            Some(mut record) => {
                record.granted = input.granted;
                if input.granted {
                    record.granted_at = Some(now());
                    record.withdrawn_at = None;
                } else {
                    record.withdrawn_at = Some(now());
                }
                record.expires_at = input.expires_at;
                record.source = source;
                record.updated_at = now();
                state.consents.insert(record.id.clone(), record.clone());
                record_consent_change(&state, &record, action, &caller.actor);
                Ok((record, false))
            }
            None => {
                let record = ConsentRecord {
                    id: new_id(),
                    user_id,
                    consent_type: input.consent_type,
                    granted: input.granted,
                    granted_at: if input.granted { Some(now()) } else { None },
                    withdrawn_at: if input.granted { None } else { Some(now()) },
                    expires_at: input.expires_at,
                    source,
                    created_by: caller.actor.clone(),
                    created_at: now(),
                    updated_at: now(),
                };
                state.consents.insert(record.id.clone(), record.clone());
                record_consent_change(&state, &record, action, &caller.actor);
                Ok((record, true))
            }
        }
    })();
    match result {
        Ok((record, is_new)) => json::success_response(record, if is_new { 201 } else { 200 }),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_consents(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ConsentRecord>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let current = now();
        let mut items: Vec<ConsentRecord> = state
            .consents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(&format!("{} {}", record.user_id, record.consent_type), &params)
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|user_id| record.user_id == *user_id)
                    && params
                        .filters
                        .get("consent_type")
                        .is_none_or(|consent_type| record.consent_type == *consent_type)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| record.status(current) == status)
            })
            .collect();
        sort_desc_by_created(&mut items, |record| record.created_at);
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn get_current_consent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<CurrentConsentQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let input = query.into_inner();
        let record = state
            .consents
            .iter()
            .find(|entry| {
                entry.value().user_id == input.user_id
                    && entry.value().consent_type == input.consent_type
            })
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No consent record for user \"{}\" and type \"{}\"",
                    input.user_id, input.consent_type
                ))
            })?;
        let current = now();
        Ok(json!({
            "user_id": record.user_id,
            "consent_type": record.consent_type,
            "consent_id": record.id,
            "status": record.status(current),
            "granted": record.is_currently_granted(current),
            "granted_at": record.granted_at,
            "withdrawn_at": record.withdrawn_at,
            "expires_at": record.expires_at,
        }))
    })();
    json::respond(result, debug)
}

async fn get_consent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ConsentRecord> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        state
            .consents
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Consent record not found"))
    })();
    json::respond(result, debug)
}

async fn withdraw_consent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ConsentRecord> = (|| {
        let caller = caller(&req)?;
        let mut record = state
            .consents
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Consent record not found"))?;
        // Self-service: the subject user may withdraw their own
        // consent; anyone else needs privacy:write.
        if caller.actor != record.user_id {
            require_permission(&ctx, &caller.role, PERM_PRIVACY_WRITE)?;
        }
        record.granted = false;
        record.withdrawn_at = Some(now());
        record.updated_at = now();
        state.consents.insert(record.id.clone(), record.clone());
        record_consent_change(&state, &record, "withdrawn", &caller.actor);
        Ok(record)
    })();
    json::respond(result, debug)
}

async fn list_consent_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ConsentHistoryEntry>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let consent_id = path.into_inner();
        if !state.consents.contains_key(&consent_id) {
            return Err(UnifiedError::not_found("Consent record not found"));
        }
        let params = query.into_inner();
        let mut items: Vec<ConsentHistoryEntry> = state
            .consent_history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                entry.consent_id == consent_id
                    && params
                        .filters
                        .get("action")
                        .is_none_or(|action| entry.action == *action)
            })
            .collect();
        sort_desc_by_created(&mut items, |entry| entry.created_at);
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: right-to-erasure requests
// ---------------------------------------------------------------------------

async fn create_erasure_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<CreateErasureInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ErasureRequest> = (|| {
        let caller = caller(&req)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 256)?;
        // Self-service: the subject user may request their own
        // erasure; anyone else needs privacy:write.
        if caller.actor != user_id {
            require_permission(&ctx, &caller.role, PERM_PRIVACY_WRITE)?;
        }
        let modules = validate_module_list(&input.modules)?;
        let reason = require_text(&input.reason, "reason", 1000)?;
        let record = ErasureRequest {
            id: new_id(),
            user_id: user_id.clone(),
            modules,
            reason,
            status: "pending".to_string(),
            requested_by: caller.actor.clone(),
            reviewed_by: None,
            reviewed_at: None,
            rejection_reason: None,
            fulfilled_at: None,
            anonymization_id: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .erasure_requests
            .insert(record.id.clone(), record.clone());
        state.emit(
            EVENT_COMPLIANCE_ERASURE_REQUESTED,
            json!({
                "user_id": record.user_id,
                "erasure_request_id": record.id,
                "modules": record.modules,
                "actor_id": caller.actor,
                "reason": record.reason,
            }),
        );
        state.audit(
            EVENT_PRIVACY_ERASURE_REQUESTED,
            &caller.actor,
            "privacy.erasure.requested",
            &format!("user:{user_id}"),
            json!({
                "user_id": record.user_id,
                "erasure_request_id": record.id,
                "modules": record.modules,
            }),
        );
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_erasure_requests(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ErasureRequest>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let mut items: Vec<ErasureRequest> = state
            .erasure_requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(&format!("{} {}", record.user_id, record.status), &params)
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|user_id| record.user_id == *user_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| record.status == *status)
            })
            .collect();
        sort_desc_by_created(&mut items, |record| record.created_at);
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn get_erasure_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ErasureRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        state
            .erasure_requests
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Erasure request not found"))
    })();
    json::respond(result, debug)
}

async fn approve_erasure_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ErasureRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_ADMIN)?;
        let mut record = state
            .erasure_requests
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Erasure request not found"))?;
        if record.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending erasure request can be approved (current status: {})",
                record.status
            )));
        }
        record.status = "approved".to_string();
        record.reviewed_by = Some(caller.actor.clone());
        record.reviewed_at = Some(now());
        record.updated_at = now();
        state
            .erasure_requests
            .insert(record.id.clone(), record.clone());
        state.audit(
            "privacy.erasure.approved",
            &caller.actor,
            "privacy.erasure.approved",
            &format!("user:{}", record.user_id),
            json!({
                "user_id": record.user_id,
                "erasure_request_id": record.id,
                "modules": record.modules,
            }),
        );
        Ok(record)
    })();
    json::respond(result, debug)
}

async fn fulfill_erasure_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ErasureRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_ADMIN)?;
        let mut record = state
            .erasure_requests
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Erasure request not found"))?;
        if record.status != "approved" {
            return Err(UnifiedError::conflict(format!(
                "Only an approved erasure request can be fulfilled (current status: {})",
                record.status
            )));
        }
        // Fulfillment drives anonymization for the requested user
        // across the requested modules. The anonymization itself
        // is idempotent and emits `user.anonymized`.
        let anonymization = state.anonymize_internal(
            &record.user_id,
            &record.modules,
            &[],
            &format!("erasure request {}", record.id),
            &caller.actor,
        )?;
        record.status = "fulfilled".to_string();
        record.fulfilled_at = Some(now());
        record.anonymization_id = Some(anonymization.id.clone());
        record.updated_at = now();
        state
            .erasure_requests
            .insert(record.id.clone(), record.clone());
        state.emit(
            EVENT_COMPLIANCE_ERASURE_FULFILLED,
            json!({
                "user_id": record.user_id,
                "erasure_request_id": record.id,
                "modules": record.modules,
                "anonymization_id": anonymization.id,
                "token": anonymization.token,
                "actor_id": caller.actor,
            }),
        );
        state.audit(
            EVENT_PRIVACY_ERASURE_FULFILLED,
            &caller.actor,
            "privacy.erasure.fulfilled",
            &format!("user:{}", record.user_id),
            json!({
                "user_id": record.user_id,
                "erasure_request_id": record.id,
                "modules": record.modules,
                "anonymization_id": anonymization.id,
            }),
        );
        Ok(record)
    })();
    json::respond(result, debug)
}

async fn reject_erasure_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RejectErasureInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ErasureRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_ADMIN)?;
        let input = body.into_inner();
        let reason = require_text(&input.reason, "reason", 1000)?;
        let mut record = state
            .erasure_requests
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Erasure request not found"))?;
        if record.status != "pending" && record.status != "approved" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending or approved erasure request can be rejected (current status: {})",
                record.status
            )));
        }
        record.status = "rejected".to_string();
        record.reviewed_by = Some(caller.actor.clone());
        record.reviewed_at = Some(now());
        record.rejection_reason = Some(reason);
        record.updated_at = now();
        state
            .erasure_requests
            .insert(record.id.clone(), record.clone());
        state.audit(
            "privacy.erasure.rejected",
            &caller.actor,
            "privacy.erasure.rejected",
            &format!("user:{}", record.user_id),
            json!({
                "user_id": record.user_id,
                "erasure_request_id": record.id,
                "rejection_reason": record.rejection_reason,
            }),
        );
        Ok(record)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: privacy notices and audit trail
// ---------------------------------------------------------------------------

async fn list_notices(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PrivacyNotice>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let mut items: Vec<PrivacyNotice> = state
            .notices
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(&format!("{} {}", record.module_id, record.text), &params)
                    && params
                        .filters
                        .get("module_id")
                        .is_none_or(|module_id| record.module_id == *module_id)
            })
            .collect();
        items.sort_by(|a, b| {
            b.version
                .cmp(&a.version)
                .then(b.effective_from.cmp(&a.effective_from))
        });
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_notice(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<CreateNoticeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PrivacyNotice> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_ADMIN)?;
        let input = body.into_inner();
        if !valid_slug(&input.module_id) {
            return Err(UnifiedError::bad_request(
                "module_id must be a lowercase slug",
            ));
        }
        let text = require_text(&input.text, "text", 20000)?;
        if input.version < 1 {
            return Err(UnifiedError::bad_request(
                "version must be at least 1",
            ));
        }
        if state.notices.iter().any(|entry| {
            entry.value().module_id == input.module_id
                && entry.value().version == input.version
        }) {
            return Err(UnifiedError::conflict(format!(
                "Notice version {} already exists for module \"{}\"",
                input.version, input.module_id
            )));
        }
        let record = PrivacyNotice {
            id: new_id(),
            module_id: input.module_id.clone(),
            text,
            version: input.version,
            effective_from: input.effective_from,
            created_by: caller.actor.clone(),
            created_at: now(),
        };
        state.notices.insert(record.id.clone(), record.clone());
        state.audit(
            EVENT_PRIVACY_NOTICE_PUBLISHED,
            &caller.actor,
            "privacy.notice.published",
            &format!("module:{}", record.module_id),
            json!({
                "module_id": record.module_id,
                "notice_id": record.id,
                "version": record.version,
                "effective_from": record.effective_from,
            }),
        );
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_current_notice(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<CurrentNoticeQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CurrentNotice> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let input = query.into_inner();
        if !valid_slug(&input.module_id) {
            return Err(UnifiedError::bad_request(
                "module_id must be a lowercase slug",
            ));
        }
        Ok(state.current_notice(&input.module_id))
    })();
    json::respond(result, debug)
}

async fn get_notice(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PrivacyNotice> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        state
            .notices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Privacy notice not found"))
    })();
    json::respond(result, debug)
}

async fn list_audit_entries(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PrivacyAuditEntry>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let mut items: Vec<PrivacyAuditEntry> = state
            .audit_entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                matches_search(
                    &format!("{} {} {}", entry.action, entry.actor, entry.resource),
                    &params,
                ) && params
                    .filters
                    .get("actor")
                    .is_none_or(|actor| entry.actor == *actor)
                    && params
                        .filters
                        .get("action")
                        .is_none_or(|action| entry.action == *action)
            })
            .collect();
        sort_desc_by_created(&mut items, |entry| entry.created_at);
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct PrivacyModule {
    state: Arc<PrivacyState>,
}

impl PrivacyModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(PrivacyState::new()),
        }
    }
}

impl Default for PrivacyModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PrivacyModule {
    fn id(&self) -> &'static str {
        "privacy"
    }

    fn name(&self) -> &'static str {
        "Privacy"
    }

    fn description(&self) -> &'static str {
        "Privacy: PII redaction, data minimization, anonymization, private-by-default visibility, consent, erasure, and privacy notices."
    }

    fn requires(&self) -> &[&'static str] {
        &["roles", "audit"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/privacy/redact", owner: "privacy", description: "Redact PII in text or a JSON value" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/field-declarations", owner: "privacy", description: "List declared personal-data fields" },
            RouteInfo { method: "POST", path: "/api/v1/privacy/field-declarations", owner: "privacy", description: "Declare a personal-data field for a module" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/field-declarations/{id}", owner: "privacy", description: "Get a field declaration" },
            RouteInfo { method: "PUT", path: "/api/v1/privacy/field-declarations/{id}", owner: "privacy", description: "Update a field declaration" },
            RouteInfo { method: "DELETE", path: "/api/v1/privacy/field-declarations/{id}", owner: "privacy", description: "Delete a field declaration" },
            RouteInfo { method: "POST", path: "/api/v1/privacy/validate-payload", owner: "privacy", description: "Validate a payload against declared fields" },
            RouteInfo { method: "POST", path: "/api/v1/privacy/anonymize", owner: "privacy", description: "Anonymize a user (one-way, idempotent)" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/anonymizations", owner: "privacy", description: "List anonymization records" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/anonymizations/{user_id}", owner: "privacy", description: "Get an anonymization record by user" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/visibility/defaults", owner: "privacy", description: "List per-module visibility defaults" },
            RouteInfo { method: "POST", path: "/api/v1/privacy/visibility/defaults", owner: "privacy", description: "Set a per-module visibility default" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/visibility/{module_id}", owner: "privacy", description: "Get the effective visibility default for a module" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/consent-types", owner: "privacy", description: "List consent types" },
            RouteInfo { method: "POST", path: "/api/v1/privacy/consent-types", owner: "privacy", description: "Define a consent type" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/consents", owner: "privacy", description: "List consent records" },
            RouteInfo { method: "POST", path: "/api/v1/privacy/consents", owner: "privacy", description: "Grant or withdraw consent" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/consents/current", owner: "privacy", description: "Get current consent for a user and type" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/consents/{id}", owner: "privacy", description: "Get a consent record" },
            RouteInfo { method: "POST", path: "/api/v1/privacy/consents/{id}/withdraw", owner: "privacy", description: "Withdraw a consent record" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/consents/{id}/history", owner: "privacy", description: "List consent change history" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/erasure-requests", owner: "privacy", description: "List erasure requests" },
            RouteInfo { method: "POST", path: "/api/v1/privacy/erasure-requests", owner: "privacy", description: "Create an erasure request" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/erasure-requests/{id}", owner: "privacy", description: "Get an erasure request" },
            RouteInfo { method: "POST", path: "/api/v1/privacy/erasure-requests/{id}/approve", owner: "privacy", description: "Approve an erasure request" },
            RouteInfo { method: "POST", path: "/api/v1/privacy/erasure-requests/{id}/fulfill", owner: "privacy", description: "Fulfill an erasure request via anonymization" },
            RouteInfo { method: "POST", path: "/api/v1/privacy/erasure-requests/{id}/reject", owner: "privacy", description: "Reject an erasure request with a reason" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/notices", owner: "privacy", description: "List privacy notices" },
            RouteInfo { method: "POST", path: "/api/v1/privacy/notices", owner: "privacy", description: "Publish a versioned privacy notice" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/notices/current", owner: "privacy", description: "Get the current notice for a module" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/notices/{id}", owner: "privacy", description: "Get a privacy notice" },
            RouteInfo { method: "GET", path: "/api/v1/privacy/audit", owner: "privacy", description: "List privacy audit entries" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/privacy")
                .route("/redact", web::post().to(redact))
                .route("/field-declarations", web::get().to(list_field_declarations))
                .route("/field-declarations", web::post().to(create_field_declaration))
                .route("/field-declarations/{id}", web::get().to(get_field_declaration))
                .route("/field-declarations/{id}", web::put().to(update_field_declaration))
                .route("/field-declarations/{id}", web::delete().to(delete_field_declaration))
                .route("/validate-payload", web::post().to(validate_payload))
                .route("/anonymize", web::post().to(anonymize_user))
                .route("/anonymizations", web::get().to(list_anonymizations))
                .route("/anonymizations/{user_id}", web::get().to(get_anonymization))
                .route("/visibility/defaults", web::get().to(list_visibility_defaults))
                .route("/visibility/defaults", web::post().to(set_visibility_default))
                .route("/visibility/{module_id}", web::get().to(get_effective_visibility))
                .route("/consent-types", web::get().to(list_consent_types))
                .route("/consent-types", web::post().to(create_consent_type))
                .route("/consents", web::get().to(list_consents))
                .route("/consents", web::post().to(grant_consent))
                .route("/consents/current", web::get().to(get_current_consent))
                .route("/consents/{id}", web::get().to(get_consent))
                .route("/consents/{id}/withdraw", web::post().to(withdraw_consent))
                .route("/consents/{id}/history", web::get().to(list_consent_history))
                .route("/erasure-requests", web::get().to(list_erasure_requests))
                .route("/erasure-requests", web::post().to(create_erasure_request))
                .route("/erasure-requests/{id}", web::get().to(get_erasure_request))
                .route("/erasure-requests/{id}/approve", web::post().to(approve_erasure_request))
                .route("/erasure-requests/{id}/fulfill", web::post().to(fulfill_erasure_request))
                .route("/erasure-requests/{id}/reject", web::post().to(reject_erasure_request))
                .route("/notices", web::get().to(list_notices))
                .route("/notices", web::post().to(create_notice))
                .route("/notices/current", web::get().to(get_current_notice))
                .route("/notices/{id}", web::get().to(get_notice))
                .route("/audit", web::get().to(list_audit_entries)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PrivacyModule::new()));
}

// ---------------------------------------------------------------------------
// Unit tests: redaction helpers (pure functions)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redact_email_keeps_first_char_and_domain() {
        assert_eq!(redact_email("jane@example.com"), "j***@example.com");
        assert_eq!(redact_email("a@example.com"), "a***@example.com");
        assert_eq!(redact_email("not-an-email"), "***");
        assert_eq!(redact_email("  jane@example.com  "), "j***@example.com");
    }

    #[test]
    fn redact_phone_keeps_last_two_digits() {
        assert_eq!(redact_phone("+62 812-3456-7890"), "+***90");
        assert_eq!(redact_phone("081234567890"), "***90");
        assert_eq!(redact_phone("123"), "***");
    }

    #[test]
    fn redact_id_number_keeps_ends() {
        assert_eq!(redact_id_number("1234567890123456"), "12************56");
        assert_eq!(redact_id_number("1234"), "***");
        assert_eq!(redact_id_number("123456"), "12**56");
    }

    #[test]
    fn redact_text_masks_email_phone_and_id_but_not_dates() {
        let input = "Reach jane@example.com or +62 812-3456-7890, ID 1234567890123456, on 2026-11-22 11:42:23.";
        let out = redact_text(input);
        assert!(out.contains("j***@example.com"), "{out}");
        assert!(out.contains("+***90"), "{out}");
        assert!(out.contains("12************56"), "{out}");
        assert!(out.contains("2026-11-22"), "{out}");
        assert!(!out.contains("jane@example.com"), "{out}");
        assert!(!out.contains("1234567890123456"), "{out}");
    }

    #[test]
    fn redact_text_leaves_clean_text_alone() {
        assert_eq!(redact_text("Hello world, no PII here."), "Hello world, no PII here.");
    }

    #[test]
    fn redact_value_redacts_pii_keys_and_nested_text() {
        let value = json!({
            "email": "jane@example.com",
            "phone": "+62 812-3456-7890",
            "name": "Jane Doe",
            "note": "Call jane@example.com please",
            "tags": ["plain", "jane@example.com"],
            "age": 41
        });
        let redacted = redact_value(&value, true);
        assert_eq!(redacted["email"], json!("j***@example.com"));
        assert_eq!(redacted["phone"], json!("+***90"));
        assert_eq!(redacted["name"], json!("J***"));
        assert_eq!(redacted["note"], json!("Call j***@example.com please"));
        assert_eq!(redacted["tags"][1], json!("j***@example.com"));
        assert_eq!(redacted["age"], json!(41));
    }

    #[test]
    fn redact_value_keeps_names_when_not_flagged() {
        let value = json!({ "name": "Jane Doe", "email": "jane@example.com" });
        let redacted = redact_value(&value, false);
        assert_eq!(redacted["name"], json!("Jane Doe"));
        assert_eq!(redacted["email"], json!("j***@example.com"));
    }

    #[test]
    fn is_pii_key_matches_markers() {
        assert!(is_pii_key("email_address"));
        assert!(is_pii_key("phone_number"));
        assert!(is_pii_key("home_address"));
        assert!(!is_pii_key("title"));
        assert!(!is_pii_key("status"));
    }

    #[test]
    fn visibility_ordering_and_default() {
        assert_eq!(default_visibility(), Visibility::Private);
        assert!(Visibility::Private < Visibility::Company);
        assert!(Visibility::Company < Visibility::Public);
        assert_eq!(Visibility::parse("public"), Some(Visibility::Public));
        assert_eq!(Visibility::parse("bogus"), None);
    }

    #[test]
    fn anonymization_is_deterministic_and_idempotent() {
        let state = PrivacyState::new();
        let first = state
            .anonymize_internal("user-1", &["crm".to_string()], &["email".to_string()], "test", "admin")
            .expect("anonymize");
        let second = state
            .anonymize_internal("user-1", &["crm".to_string()], &["email".to_string()], "test", "admin")
            .expect("anonymize again");
        assert_eq!(first.token, second.token);
        assert_eq!(first.field_tokens, second.field_tokens);
        assert_eq!(second.times_applied, 2);
        assert_ne!(first.token, "user-1");
        assert_eq!(first.token.len(), 64);
    }

    #[test]
    fn consent_status_honours_withdrawal_and_expiry() {
        let now = Utc::now();
        let granted = ConsentRecord {
            id: new_id(),
            user_id: "u1".to_string(),
            consent_type: "marketing".to_string(),
            granted: true,
            granted_at: Some(now),
            withdrawn_at: None,
            expires_at: None,
            source: "api".to_string(),
            created_by: "u1".to_string(),
            created_at: now,
            updated_at: now,
        };
        assert_eq!(granted.status(now), "granted");
        let mut expired = granted.clone();
        expired.expires_at = Some(now - chrono::Duration::seconds(1));
        assert_eq!(expired.status(now), "expired");
        let mut withdrawn = granted.clone();
        withdrawn.granted = false;
        withdrawn.withdrawn_at = Some(now);
        assert_eq!(withdrawn.status(now), "withdrawn");
    }

    #[test]
    fn current_notice_falls_back_to_generic() {
        let state = PrivacyState::new();
        let notice = state.current_notice("no-such-module");
        assert!(notice.generic);
        assert_eq!(notice.version, 0);
        assert_eq!(notice.text, GENERIC_NOTICE_TEXT);
    }

    #[test]
    fn effective_visibility_defaults_to_private() {
        let state = PrivacyState::new();
        let effective = state.effective_visibility("no-such-module");
        assert_eq!(effective.visibility, Visibility::Private);
        assert_eq!(effective.source, "default");
    }
}

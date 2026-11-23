// Privacy state and anonymization engine.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use argon2::{Algorithm, Argon2, Params, Version};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::Mac;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct PrivacyState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) declarations: DashMap<String, FieldDeclaration>,
    pub(crate) anonymizations: DashMap<String, AnonymizationRecord>,
    pub(crate) visibility_defaults: DashMap<String, VisibilityDefault>,
    pub(crate) consent_types: DashMap<String, ConsentTypeDef>,
    pub(crate) consents: DashMap<String, ConsentRecord>,
    pub(crate) consent_history: DashMap<String, ConsentHistoryEntry>,
    pub(crate) erasure_requests: DashMap<String, ErasureRequest>,
    pub(crate) notices: DashMap<String, PrivacyNotice>,
    pub(crate) audit_entries: DashMap<String, PrivacyAuditEntry>,
    /// Write-only anonymization key. Never serialized, logged,
    /// or returned in any response.
    pub(crate) anonymization_key: [u8; 32],
    /// Serializes consent find-or-create / withdraw transitions
    /// (see `apply_consent_grant` and `withdraw_consent_record`).
    /// The consent map stays keyed by record id so GET-by-id URLs
    /// are stable; uniqueness of (user_id, consent_type) is
    /// enforced by holding this lock across find + insert instead.
    /// The lock is never held across hook emission or audit
    /// writes: only the map transition runs under it.
    pub(crate) consent_write_lock: Mutex<()>,
    /// One mutex per erasure request, guarding the fulfill
    /// transition (see `fulfill_erasure_request`). Entries are
    /// retained for the life of the process: the map has the same
    /// cardinality as `erasure_requests`, and removing an entry
    /// after fulfill would let a late contender create a fresh
    /// guard and race the in-flight fulfill.
    pub(crate) erasure_fulfill_locks: DashMap<String, Arc<Mutex<()>>>,
}

/// Fixed domain-separation salt for the anonymization-key KDF.
///
/// The salt is a public, fixed application constant, not a secret:
/// it only separates this derivation from every other Argon2id use
/// of the same input. It MUST stay fixed: a random salt would derive
/// a different key on every restart and change all tokens. The
/// configured key itself is a high-entropy server secret, so a fixed
/// salt is the correct shape here (key derivation, not password
/// storage).
pub(crate) const ANONYMIZATION_KEY_SALT: &[u8] = b"unified-privacy-anonymization-v1";

/// Argon2id cost parameters for the anonymization-key KDF (OWASP
/// recommended values, the same shape as password hashing). These
/// MUST NOT change: the derived key feeds every anonymization
/// token, so changing them would silently change all tokens. Any
/// rotation needs a token migration, not a parameter tweak.
pub(crate) const ANONYMIZATION_KDF_M_COST: u32 = 19_456;
pub(crate) const ANONYMIZATION_KDF_T_COST: u32 = 2;
pub(crate) const ANONYMIZATION_KDF_P_COST: u32 = 1;

/// Derive the 32-byte anonymization key from a configured secret
/// with Argon2id, the strongest KDF among the repo's existing
/// dependencies (`argon2` is already used for password hashing, so
/// no new dependency is needed). Deterministic: the same secret
/// and salt always yield the same key, so tokens stay stable
/// across restarts.
pub(crate) fn derive_anonymization_key(
    configured_key: &str,
    salt: &[u8],
) -> UnifiedResult<[u8; 32]> {
    let params = Params::new(
        ANONYMIZATION_KDF_M_COST,
        ANONYMIZATION_KDF_T_COST,
        ANONYMIZATION_KDF_P_COST,
        Some(32),
    )
    .map_err(|error| {
        UnifiedError::internal(format!("Invalid anonymization KDF parameters: {error}"))
    })?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon2
        .hash_password_into(configured_key.as_bytes(), salt, &mut key)
        .map_err(|error| {
            UnifiedError::internal(format!("Anonymization key derivation failed: {error}"))
        })?;
    Ok(key)
}

/// Validate a configured key value and derive the anonymization
/// key from it. Rejects a missing key and keys shorter than 16
/// characters (after trimming). The secret itself is never logged.
pub(crate) fn try_derive_anonymization_key(configured: Option<&str>) -> UnifiedResult<[u8; 32]> {
    match configured.map(str::trim).filter(|value| !value.is_empty()) {
        Some(key) if key.len() >= 16 => derive_anonymization_key(key, ANONYMIZATION_KEY_SALT),
        Some(_) => Err(UnifiedError::internal(
            "UNIFIED_PRIVACY_ANONYMIZATION_KEY must be at least 16 characters",
        )),
        None => Err(UnifiedError::internal(
            "UNIFIED_PRIVACY_ANONYMIZATION_KEY is not set: anonymization tokens cannot be stable across restarts without it",
        )),
    }
}

/// Strict loader for production startup: read the key from the
/// environment and derive it, failing when it is unset or too
/// short. Production startup MUST use this (via
/// `PrivacyState::try_new`) and refuse to boot on error, instead
/// of silently falling back to an ephemeral key.
pub(crate) fn try_load_anonymization_key() -> UnifiedResult<[u8; 32]> {
    try_derive_anonymization_key(
        std::env::var("UNIFIED_PRIVACY_ANONYMIZATION_KEY")
            .ok()
            .as_deref(),
    )
}

pub(crate) fn load_anonymization_key() -> [u8; 32] {
    match try_load_anonymization_key() {
        Ok(key) => key,
        Err(error) => {
            // LOUD fallback, dev/test only: no usable key is
            // configured, so an ephemeral key is generated in
            // memory. Tokens stay stable for the life of this
            // process only and CHANGE on every restart. This must
            // never be relied on in production: production startup
            // must fail fast through `try_load_anonymization_key`
            // (see `PrivacyState::try_new` and the module notes)
            // instead of booting with unstable tokens.
            log::error!(
                "UNIFIED_PRIVACY_ANONYMIZATION_KEY is not configured ({error}); generating an ephemeral in-memory anonymization key. Anonymization tokens WILL CHANGE on restart. Set the environment variable to at least 16 characters for stable tokens."
            );
            let seed = Uuid::new_v4();
            let mut hasher = Sha256::new();
            hasher.update(seed.as_bytes());
            hasher.update(Utc::now().to_rfc3339().as_bytes());
            hasher.finalize().into()
        }
    }
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

impl PrivacyState {
    fn new_with_key(anonymization_key: [u8; 32]) -> Self {
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
            anonymization_key,
            consent_write_lock: Mutex::new(()),
            erasure_fulfill_locks: DashMap::new(),
        }
    }

    pub(crate) fn new() -> Self {
        Self::new_with_key(load_anonymization_key())
    }

    /// Fallible constructor for production startup: fails when no
    /// usable anonymization key is configured, so the process
    /// refuses to boot instead of issuing per-process tokens.
    /// `register_all` is currently infallible, so this constructor
    /// is not wired into it; instead `PrivacyModule::init`
    /// enforces the same rule in production mode (failing startup
    /// when the key is missing or too short), while development
    /// keeps the loud fallback in `load_anonymization_key`.
    pub(crate) fn try_new() -> UnifiedResult<Self> {
        Ok(Self::new_with_key(try_load_anonymization_key()?))
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Record a privacy action in the local append-only audit
    /// trail and emit it on the hook bus so the audit module can
    /// consume it. No privacy change is ever silent.
    pub(crate) fn audit(&self, event: &str, actor: &str, action: &str, resource: &str, details: Value) {
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

    pub(crate) fn hmac_hex(&self, message: &str) -> UnifiedResult<String> {
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

    pub(crate) fn field_token(&self, user_id: &str, field: &str) -> UnifiedResult<String> {
        self.hmac_hex(&format!("privacy-anonymize-v1:{user_id}:{field}"))
    }

    /// One-way anonymization of a user. Idempotent: anonymizing
    /// the same user again returns the same tokens and only bumps
    /// `times_applied` and the timestamps. Never stores or
    /// returns an original value.
    pub(crate) fn anonymize_internal(
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

    /// Grant (or re-grant) consent for one (user_id, consent_type)
    /// pair, creating the record when none exists. Inputs are
    /// already validated by the HTTP handler.
    ///
    /// The find-or-create runs under `consent_write_lock`, so two
    /// concurrent grants for the same pair can never both miss and
    /// insert a duplicate: exactly one record exists per pair and
    /// `get_current_consent` can never return an arbitrary one of
    /// several duplicates. History/audit recording happens after
    /// the lock is released; each grant call still appends exactly
    /// one history entry. Returns the record and whether it was
    /// newly created.
    pub(crate) fn apply_consent_grant(
        &self,
        user_id: &str,
        consent_type: &str,
        granted: bool,
        expires_at: Option<DateTime<Utc>>,
        source: &str,
        actor: &str,
    ) -> (ConsentRecord, bool) {
        let (record, is_new) = {
            let _guard = self
                .consent_write_lock
                .lock()
                .expect("consent write lock poisoned");
            let existing = self
                .consents
                .iter()
                .find(|entry| {
                    entry.value().user_id == user_id && entry.value().consent_type == consent_type
                })
                .map(|entry| entry.value().clone());
            match existing {
                Some(mut record) => {
                    record.granted = granted;
                    if granted {
                        record.granted_at = Some(Utc::now());
                        record.withdrawn_at = None;
                    } else {
                        record.withdrawn_at = Some(Utc::now());
                    }
                    record.expires_at = expires_at;
                    record.source = source.to_string();
                    record.updated_at = Utc::now();
                    self.consents.insert(record.id.clone(), record.clone());
                    (record, false)
                }
                None => {
                    let record = ConsentRecord {
                        id: new_id(),
                        user_id: user_id.to_string(),
                        consent_type: consent_type.to_string(),
                        granted,
                        granted_at: if granted { Some(Utc::now()) } else { None },
                        withdrawn_at: if granted { None } else { Some(Utc::now()) },
                        expires_at,
                        source: source.to_string(),
                        created_by: actor.to_string(),
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    };
                    self.consents.insert(record.id.clone(), record.clone());
                    (record, true)
                }
            }
        };
        let action = if granted { "granted" } else { "withdrawn" };
        record_consent_change(self, &record, action, actor);
        (record, is_new)
    }

    /// Withdraw a consent record by id.
    ///
    /// Idempotent no-op: withdrawing an already-withdrawn record
    /// returns the record unchanged (HTTP 200) and appends no
    /// history entry and no audit event, so a retried withdraw
    /// cannot pollute the history. Withdrawing a record that is
    /// granted (including an expired-but-granted one) performs the
    /// withdraw and records exactly one history entry. The check
    /// and the transition run under `consent_write_lock`, so
    /// concurrent withdraws cannot both append a history entry.
    pub(crate) fn withdraw_consent_record(
        &self,
        consent_id: &str,
        actor: &str,
    ) -> UnifiedResult<ConsentRecord> {
        let record = {
            let _guard = self
                .consent_write_lock
                .lock()
                .expect("consent write lock poisoned");
            let mut record = self
                .consents
                .get(consent_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found("Consent record not found"))?;
            if record.status(Utc::now()) == "withdrawn" {
                return Ok(record);
            }
            record.granted = false;
            record.withdrawn_at = Some(Utc::now());
            record.updated_at = Utc::now();
            self.consents.insert(record.id.clone(), record.clone());
            record
        };
        record_consent_change(self, &record, "withdrawn", actor);
        Ok(record)
    }

    /// Fulfill an approved erasure request.
    ///
    /// The whole load-check-anonymize-mark-fulfilled transition
    /// runs under a per-request guard
    /// (`erasure_fulfill_locks[request_id]`), so two concurrent
    /// fulfills for the same request cannot both pass the approved
    /// check: the second blocks on the guard, then sees status
    /// `fulfilled` and gets a 409 conflict. Anonymization itself
    /// is idempotent, so even a double run could not erase twice;
    /// the guard exists to keep the audit trail and history free
    /// of duplicate fulfill noise. Different requests fulfill in
    /// parallel: only the same request id contends. Hook handlers
    /// must never call this for a request whose fulfill is already
    /// in flight.
    pub(crate) fn fulfill_erasure_request(
        &self,
        request_id: &str,
        actor: &str,
    ) -> UnifiedResult<ErasureRequest> {
        let guard = self
            .erasure_fulfill_locks
            .entry(request_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _held = guard.lock().expect("erasure fulfill lock poisoned");
        let mut record = self
            .erasure_requests
            .get(request_id)
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
        let anonymization = self.anonymize_internal(
            &record.user_id,
            &record.modules,
            &[],
            &format!("erasure request {}", record.id),
            actor,
        )?;
        record.status = "fulfilled".to_string();
        record.fulfilled_at = Some(Utc::now());
        record.anonymization_id = Some(anonymization.id.clone());
        record.updated_at = Utc::now();
        self.erasure_requests
            .insert(record.id.clone(), record.clone());
        self.emit(
            EVENT_COMPLIANCE_ERASURE_FULFILLED,
            json!({
                "user_id": record.user_id,
                "erasure_request_id": record.id,
                "modules": record.modules,
                "anonymization_id": anonymization.id,
                "token": anonymization.token,
                "actor_id": actor,
            }),
        );
        self.audit(
            EVENT_PRIVACY_ERASURE_FULFILLED,
            actor,
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
    }

    /// Declared personal-data field names for a module. A module
    /// with no declaration allows no personal data fields.
    pub(crate) fn declared_fields(&self, module_id: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encode_produces_lowercase_hex() {
        assert_eq!(hex_encode(&[0xab, 0x01, 0xff]), "ab01ff");
        assert_eq!(hex_encode(&[0]), "00");
        assert_eq!(hex_encode(&[255, 254]), "fffe");
    }

    #[test]
    fn hex_encode_empty_is_empty_and_deterministic() {
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_encode(&[0xde, 0xad]), hex_encode(&[0xde, 0xad]));
    }

    #[test]
    fn derive_anonymization_key_is_deterministic() {
        let first =
            derive_anonymization_key("correct horse battery staple", ANONYMIZATION_KEY_SALT)
                .expect("derive key");
        let second =
            derive_anonymization_key("correct horse battery staple", ANONYMIZATION_KEY_SALT)
                .expect("derive key again");
        assert_eq!(first, second);
        assert_eq!(first.len(), 32);
    }

    #[test]
    fn derive_anonymization_key_is_salt_sensitive() {
        let base = derive_anonymization_key("correct horse battery staple", ANONYMIZATION_KEY_SALT)
            .expect("derive key");
        let other = derive_anonymization_key(
            "correct horse battery staple",
            b"unified-privacy-anonymization-v2",
        )
        .expect("derive key with other salt");
        assert_ne!(base, other);
    }

    #[test]
    fn derive_anonymization_key_differs_across_inputs_and_from_legacy_sha256() {
        let key = derive_anonymization_key("correct horse battery staple", ANONYMIZATION_KEY_SALT)
            .expect("derive key");
        let other =
            derive_anonymization_key("a different configured secret", ANONYMIZATION_KEY_SALT)
                .expect("derive other key");
        assert_ne!(key, other);
        // The old scheme was raw SHA-256 of the configured value;
        // the KDF output must not equal it.
        let mut hasher = Sha256::new();
        hasher.update(b"correct horse battery staple");
        let legacy: [u8; 32] = hasher.finalize().into();
        assert_ne!(key, legacy);
    }

    #[test]
    fn try_derive_anonymization_key_rejects_missing_short_and_blank_keys() {
        assert!(try_derive_anonymization_key(None).is_err());
        assert!(try_derive_anonymization_key(Some("")).is_err());
        assert!(try_derive_anonymization_key(Some("   ")).is_err());
        assert!(try_derive_anonymization_key(Some("too-short")).is_err());
        assert!(try_derive_anonymization_key(Some("exactly-sixteen!")).is_ok());
    }
    #[test]
    fn hex_encode_matches_reference_for_every_byte_value() {
        // Exhaustive oracle: the lookup-table encoding must agree
        // with plain per-byte formatting for all 256 byte values.
        // (`format!` is the reference here, in test code only.)
        for byte in 0..=u8::MAX {
            assert_eq!(hex_encode(&[byte]), format!("{byte:02x}"));
        }
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

}

// Public link sharing and auditor sandbox - shared machinery.
//
// This module holds the security-critical machinery shared by the
// public-link endpoints and the auditor accounts, with no HTTP
// handlers of its own (handlers live in `the server modules::auditors`).
// Keeping the machinery here means every module that serves a shared
// resource validates links and auditor scopes through exactly the
// same code path instead of re-implementing token handling.
//
// Security invariants (read before changing anything here):
//   - A public link exposes exactly ONE resource: one resource kind,
//     one resource id, one view (`ShareScope`). A link NEVER grants a
//     module, a list, or a search. There is no wildcard scope.
//   - A link is read-only by construction: the scope carries no
//     action, and no write operation may accept a link token as
//     authorization. Any handler that needs a write must require a
//     normal authenticated caller with a capability.
//   - The raw token is never stored. Only its SHA-256 hex digest is
//     stored (`token_hash`). The raw token is shown exactly once, at
//     creation time, in the creation response.
//   - Token comparison is constant-time over the hex digests.
//   - A link has a required expiry, at most 30 days from creation.
//   - Every access attempt - allowed AND denied - is recorded in the
//     access log by the caller (see `PublicLinkAccessAttempt`); this
//     module builds the record, the auditors module stores it.
//   - IP addresses and user agents are never stored raw in the access
//     log: only their SHA-256 hex digests (`ip_hash`,
//     `user_agent_hash`).
//   - Auditor scope evaluation is a SECURITY BOUNDARY. `AuditorScope`
//     answers allow/deny for one (module, resource kind, resource
//     id, company id) tuple, and modules MUST filter at the data
//     layer: a query for a module's records must add the scope
//     predicate to the query itself (Diesel `.filter(...)` on the
//     record id / company id sets from the scope) so records outside
//     the scope are never loaded. Filtering after fetch is NOT
//     sufficient: a list endpoint that loads then filters still
//     leaks counts, and a detail endpoint that loads then checks
//     still runs the record's side effects. The exact predicate each
//     module must add is documented per module in the auditors
//     module report (integration notes), because this core module
//     cannot know other modules' tables.
//   - Auditor accounts are read-only always: no mutation, no settings
//     access, no API-key creation, no role assignment, no password
//     change to a permanent account. Exports are denied unless the
//     scope entry for that exact resource grants `allow_export`.
//
// Hook contract - `sharing.allow_check` (see `check_allow_public_link`):
//   - Emitted with payload `{"kind", "id", "view", "actor",
//     "allowed": false}` when a public link is created for a resource.
//   - The module that OWNS the resource kind answers it: a handler
//     sets `"allowed": true` only when the target record's own
//     `allow_public_link` flag is true. No handler answering means
//     denied (fail closed). Handlers must never set `"allowed": true`
//     unconditionally, and must check that the actor may share that
//     record (ownership or a manage capability on the owning module).
//   - Because the check runs through the hook bus, modules adopt it
//     without this core module depending on them, and without the
//     auditors module editing their files.
//
// Rate limiting: failed link validations feed the existing hardening
// engine (`HardeningState::record_login_failure`) with the account
// key `public_link:<link id>` (or `public_link:unknown` when no link
// row matched), so repeated guessing of tokens for one link - or
// from one IP - trips the same progressive lockout as failed logins.
// This module does NOT duplicate the rate-limit machinery in
// `crate::hardening`; see `record_failed_link_validation`.
//
// Events emitted on the hook bus (names in `sharing_events`):
//   - `sharing.link.created`, `sharing.link.revoked`
//   - `sharing.link.accessed`, `sharing.link.access_denied`
//   - `sharing.public_access` (a public link was used to read a
//     resource; the owning module emits it when it serves content)
//   - `auditors.created`, `auditors.revoked`, `auditors.expired`,
//     `auditors.action`, `auditors.session.started`,
//     `auditors.session.revoked`, `auditors.expire_requested`
// Events this machinery answers (via the auditors module):
//   - `sharing.allow_check` - see the contract above.
//   - `auditors.scope.check` - payload `{"user_id", "module",
//     "resource_kind", "resource_id", "company_id", "action",
//     "allowed": false}`; the auditors module answers `allowed`
//     from the auditor's stored scope. `action` is `"read"` or
//     `"export"`; anything else is denied.

use crate::error::{UnifiedError, UnifiedResult};
use crate::hardening::HardeningState;
use crate::hooks::HookBus;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::RwLock;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Well-known sharing / auditor events emitted on the hook bus.
pub mod sharing_events {
    /// Emitted when a public link is created. Payload:
    /// `{"link_id", "scope_kind", "scope_id", "scope_view",
    /// "created_by", "expires_at"}`.
    pub const LINK_CREATED: &str = "sharing.link.created";
    /// Emitted when a public link is revoked. Payload:
    /// `{"link_id", "revoked_by", "reason"}`.
    pub const LINK_REVOKED: &str = "sharing.link.revoked";
    /// Emitted when a public link access is allowed. Payload:
    /// `{"link_id", "scope_kind", "scope_id", "scope_view"}`.
    /// No IP or user agent is carried in the payload; the access log
    /// row holds only their hashes.
    pub const LINK_ACCESSED: &str = "sharing.link.accessed";
    /// Emitted when a public link access is denied. Payload:
    /// `{"link_id", "reason"}` where `link_id` may be null when no
    /// link row matched the presented token.
    pub const LINK_ACCESS_DENIED: &str = "sharing.link.access_denied";
    /// Emitted by the owning module when it serves resource content
    /// for a validated public link. Payload: `{"link_id",
    /// "scope_kind", "scope_id", "scope_view"}`.
    pub const PUBLIC_ACCESS: &str = "sharing.public_access";
    /// Hook a resource-owning module answers to allow or deny public
    /// link creation for one of its records. Payload:
    /// `{"kind", "id", "view", "actor", "allowed": false}`.
    /// Default (no handler): denied.
    pub const ALLOW_CHECK: &str = "sharing.allow_check";
    /// Emitted when an auditor account is created. Payload:
    /// `{"auditor_id", "user_id", "auditor_of", "expires_at"}`.
    pub const AUDITOR_CREATED: &str = "auditors.created";
    /// Emitted when an auditor account is revoked. Payload:
    /// `{"auditor_id", "revoked_by", "reason"}`.
    pub const AUDITOR_REVOKED: &str = "auditors.revoked";
    /// Emitted when an auditor account expires (auto-expire sweep).
    /// Payload: `{"auditor_id", "expired_at"}`.
    pub const AUDITOR_EXPIRED: &str = "auditors.expired";
    /// Emitted for every auditor action, allowed or denied. Payload:
    /// `{"auditor_id", "actor", "resource", "action", "allowed"}`.
    /// The audit module subscribes to this event (see the integrator
    /// snippet: add it to `AUDITED_EVENTS` in `audit.rs`).
    pub const AUDITOR_ACTION: &str = "auditors.action";
    /// Emitted when an auditor session starts. Payload:
    /// `{"auditor_id", "session_id", "user_id"}`.
    pub const AUDITOR_SESSION_STARTED: &str = "auditors.session.started";
    /// Emitted when an auditor session is revoked. Payload:
    /// `{"auditor_id", "session_id", "revoked_by"}`.
    pub const AUDITOR_SESSION_REVOKED: &str = "auditors.session.revoked";
    /// Emitted by the scheduler when an `auditors.expire` job fires,
    /// following the scheduler naming rule (`backup.run` ->
    /// `backup.run_requested`). Payload: `{"auditor_id"}` when one
    /// account is targeted, or `{}` for a full sweep.
    pub const AUDITOR_EXPIRE_REQUESTED: &str = "auditors.expire_requested";
    /// Hook the auditors module answers to check one auditor's scope.
    /// Payload: `{"user_id", "module", "resource_kind",
    /// "resource_id", "company_id", "action", "allowed": false}`.
    /// `action` is `"read"` or `"export"`; anything else is denied.
    pub const AUDITOR_SCOPE_CHECK: &str = "auditors.scope.check";
}

/// Maximum lifetime of a public link, in days. Creation with a
/// longer lifetime is rejected.
pub const MAX_LINK_TTL_DAYS: i64 = 30;
/// Maximum lifetime of an auditor account, in hours (30 days).
pub const MAX_AUDITOR_TTL_HOURS: i64 = 720;
/// Minimum lifetime of an auditor account, in hours.
pub const MIN_AUDITOR_TTL_HOURS: i64 = 1;
/// Random bytes in a link token (32 bytes -> 43 base64url chars).
pub const LINK_TOKEN_BYTES: usize = 32;
/// Random bytes in an auditor one-time password (shown once).
pub const AUDITOR_PASSWORD_BYTES: usize = 24;

/// Configuration section for sharing and auditor accounts. The
/// integrator adds `pub sharing: SharingSettings` to `AppConfig`
/// (see the integrator snippet in the auditors module report).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SharingSettings {
    /// Master switch for public link creation. When false, link
    /// creation is rejected with 403; already-created links keep
    /// validating (revoke them to stop access).
    pub public_links_enabled: bool,
    /// Master switch for auditor account creation. When false,
    /// creation is rejected with 403; existing auditors keep
    /// working until they expire or are revoked.
    pub auditors_enabled: bool,
    /// Default link lifetime in hours when the caller does not ask
    /// for a specific one (creation always requires an expiry, so
    /// this is the value used when `expires_in_hours` is omitted).
    pub default_link_ttl_hours: i64,
    /// Maximum link lifetime in hours. Must not exceed
    /// `MAX_LINK_TTL_DAYS * 24`.
    pub max_link_ttl_hours: i64,
    /// Default auditor lifetime in hours when the caller omits it
    /// (creation requires an expiry, so this is only a fallback
    /// for callers that pass no value).
    pub default_auditor_ttl_hours: i64,
    /// Maximum auditor lifetime in hours. Must not exceed
    /// `MAX_AUDITOR_TTL_HOURS`.
    pub max_auditor_ttl_hours: i64,
    /// Failed public-link validations per link (account key) that
    /// trip the hardening lockout. Mirrors the brute-force
    /// threshold; the hardening engine owns the actual value, this
    /// is documentation for operators.
    pub failed_validation_lockout_note: String,
}

impl Default for SharingSettings {
    fn default() -> Self {
        Self {
            public_links_enabled: true,
            auditors_enabled: true,
            default_link_ttl_hours: 72,
            max_link_ttl_hours: MAX_LINK_TTL_DAYS * 24,
            default_auditor_ttl_hours: 168,
            max_auditor_ttl_hours: MAX_AUDITOR_TTL_HOURS,
            failed_validation_lockout_note: "Failed link validations feed HardeningState::record_login_failure with account key public_link:<link id>; the brute-force thresholds in SecuritySettings apply.".to_string(),
        }
    }
}

impl SharingSettings {
    /// Validate the section. Returns a list of problems; an empty
    /// list means the section is valid.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.default_link_ttl_hours < 1 {
            problems.push("sharing.default_link_ttl_hours must be at least 1".to_string());
        }
        if self.max_link_ttl_hours < 1 || self.max_link_ttl_hours > MAX_LINK_TTL_DAYS * 24 {
            problems.push(format!(
                "sharing.max_link_ttl_hours must be between 1 and {}",
                MAX_LINK_TTL_DAYS * 24
            ));
        }
        if self.default_link_ttl_hours > self.max_link_ttl_hours {
            problems.push(
                "sharing.default_link_ttl_hours must not exceed sharing.max_link_ttl_hours"
                    .to_string(),
            );
        }
        if self.default_auditor_ttl_hours < MIN_AUDITOR_TTL_HOURS {
            problems.push(format!(
                "sharing.default_auditor_ttl_hours must be at least {MIN_AUDITOR_TTL_HOURS}"
            ));
        }
        if self.max_auditor_ttl_hours < MIN_AUDITOR_TTL_HOURS
            || self.max_auditor_ttl_hours > MAX_AUDITOR_TTL_HOURS
        {
            problems.push(format!(
                "sharing.max_auditor_ttl_hours must be between {MIN_AUDITOR_TTL_HOURS} and {MAX_AUDITOR_TTL_HOURS}"
            ));
        }
        if self.default_auditor_ttl_hours > self.max_auditor_ttl_hours {
            problems.push(
                "sharing.default_auditor_ttl_hours must not exceed sharing.max_auditor_ttl_hours"
                    .to_string(),
            );
        }
        problems
    }
}

// Share scope

/// Exactly what a public link exposes: one resource kind, one
/// resource id, one view. A link NEVER grants a module, a list, or
/// a search. Serialised as `"kind:uuid:view"`, for example
/// `"project:3f8a...:board"` or `"invoice:3f8a...:detail"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareScope {
    /// Resource kind, for example `"project"`, `"invoice"`,
    /// `"event"`. Lowercase letters, digits, `_` and `-` only.
    pub kind: String,
    /// The single resource id the link exposes.
    pub id: Uuid,
    /// The single view of that resource, for example `"board"`,
    /// `"detail"`, `"summary"`. Lowercase letters, digits, `_`
    /// and `-` only.
    pub view: String,
}

impl ShareScope {
    /// Build a scope, validating the kind and view shapes.
    pub fn new(kind: &str, id: Uuid, view: &str) -> UnifiedResult<Self> {
        let kind = valid_scope_segment(kind, "kind")?;
        let view = valid_scope_segment(view, "view")?;
        Ok(Self { kind, id, view })
    }

    /// Parse the `"kind:uuid:view"` form.
    pub fn parse(raw: &str) -> UnifiedResult<Self> {
        let mut parts = raw.splitn(3, ':');
        let kind = parts
            .next()
            .ok_or_else(|| UnifiedError::bad_request("scope must look like \"kind:uuid:view\""))?;
        let id = parts
            .next()
            .ok_or_else(|| UnifiedError::bad_request("scope must look like \"kind:uuid:view\""))?;
        let view = parts
            .next()
            .ok_or_else(|| UnifiedError::bad_request("scope must look like \"kind:uuid:view\""))?;
        let id = Uuid::parse_str(id)
            .map_err(|_| UnifiedError::bad_request(format!("scope id is not a valid UUID: {id}")))?;
        Self::new(kind, id, view)
    }

    /// Serialise to the `"kind:uuid:view"` form.
    pub fn to_scope_string(&self) -> String {
        format!("{}:{}:{}", self.kind, self.id, self.view)
    }

    /// True when `other` names exactly this same resource and view.
    /// A link grants nothing beyond an exact match: a different view
    /// of the same resource is a different scope and is denied.
    pub fn matches(&self, other: &ShareScope) -> bool {
        self.kind == other.kind && self.id == other.id && self.view == other.view
    }
}

/// Validate one scope segment (kind or view): 1..=64 chars,
/// lowercase letters, digits, `_` and `-` only.
fn valid_scope_segment(value: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = value.trim().to_lowercase();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err(UnifiedError::bad_request(format!(
            "scope {field} must be between 1 and 64 characters"
        )));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(UnifiedError::bad_request(format!(
            "scope {field} must contain only lowercase letters, digits, '_' or '-'"
        )));
    }
    Ok(trimmed)
}

// Shareable registry (the `allow_public_link` flag, hook-based)

/// One resource kind that may be shared through a public link, as
/// registered by its owning module. The registry is only a catalog
/// for validation and discovery: whether a SPECIFIC record may be
/// shared is decided per record by the `sharing.allow_check` hook
/// (see `check_allow_public_link`), never by this registry.
#[derive(Debug, Clone, Serialize)]
pub struct ShareableKind {
    /// Resource kind, for example `"project"`.
    pub kind: String,
    /// Views the owning module serves for this kind, for example
    /// `["board", "summary"]`. A link may only name a listed view.
    pub views: Vec<String>,
    /// Short description of what the kind is.
    pub description: String,
}

/// Registry of shareable resource kinds. Modules register their
/// kinds at startup (from `init` or `register_hooks`); the auditors
/// module rejects link creation for an unregistered kind or view
/// before it ever asks the owning module, so a typo can never mint
/// a link for a kind nothing serves.
#[derive(Debug, Default)]
pub struct ShareRegistry {
    kinds: RwLock<HashMap<String, ShareableKind>>,
}

impl ShareRegistry {
    pub fn new() -> Self {
        Self {
            kinds: RwLock::new(HashMap::new()),
        }
    }

    /// Register (or replace) one shareable kind. View names are
    /// validated with the same segment rule as `ShareScope`.
    pub fn register(&self, kind: &str, views: &[&str], description: &str) -> UnifiedResult<()> {
        let kind = valid_scope_segment(kind, "kind")?;
        let mut clean_views: Vec<String> = Vec::new();
        for view in views {
            clean_views.push(valid_scope_segment(view, "view")?);
        }
        if clean_views.is_empty() {
            return Err(UnifiedError::bad_request(
                "a shareable kind must declare at least one view",
            ));
        }
        self.kinds.write().expect("share registry lock poisoned").insert(
            kind.clone(),
            ShareableKind {
                kind,
                views: clean_views,
                description: description.to_string(),
            },
        );
        Ok(())
    }

    /// True when the kind is registered.
    pub fn is_known_kind(&self, kind: &str) -> bool {
        self.kinds
            .read()
            .expect("share registry lock poisoned")
            .contains_key(kind)
    }

    /// True when the kind is registered and the view is one of its
    /// declared views.
    pub fn is_known_view(&self, kind: &str, view: &str) -> bool {
        self.kinds
            .read()
            .expect("share registry lock poisoned")
            .get(kind)
            .is_some_and(|entry| entry.views.iter().any(|v| v == view))
    }

    /// All registered kinds, sorted by kind name.
    pub fn kinds(&self) -> Vec<ShareableKind> {
        let mut out: Vec<ShareableKind> = self
            .kinds
            .read()
            .expect("share registry lock poisoned")
            .values()
            .cloned()
            .collect();
        out.sort_by(|a, b| a.kind.cmp(&b.kind));
        out
    }
}

/// Ask the owning module whether the record named by `scope` may be
/// shared as a public link, through the `sharing.allow_check` hook.
///
/// Contract (implement exactly this in the owning module):
///   - Register a handler for `sharing.allow_check` (event name
///     `sharing_events::ALLOW_CHECK`).
///   - Read `"kind"`, `"id"` (UUID string) and `"view"` from the
///     payload; load the record; if it does not exist, leave
///     `"allowed"` as false.
///   - Set `"allowed": true` ONLY when the record carries
///     `allow_public_link: true` AND the `"actor"` in the payload
///     may share that record (the record owner, or a caller holding
///     a manage capability on the owning module). A handler must
///     never set `"allowed": true` unconditionally and must never
///     answer for a kind it does not own.
///   - The payload starts with `"allowed": false`, so a module that
///     has not adopted the hook yet denies creation (fail closed).
///   - The check is enforced server-side by the auditors module:
///     a `false` answer rejects link creation with 403. Hiding the
///     share button in the UI is not a substitute.
pub fn check_allow_public_link(bus: &HookBus, scope: &ShareScope, actor: &str) -> bool {
    let context = bus.emit(
        sharing_events::ALLOW_CHECK,
        serde_json::json!({
            "kind": scope.kind,
            "id": scope.id.to_string(),
            "view": scope.view,
            "actor": actor,
            "allowed": false,
        }),
    );
    context
        .get("allowed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

// Link tokens

/// SHA-256 hex digest of a string. Used for token hashes and for
/// hashing IPs / user agents in the access log (never store raw).
pub fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = hasher.finalize();
        let mut out = String::with_capacity(digest.len() * 2);
        for &byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

/// SHA-256 hex digest of raw bytes.
pub fn sha256_hex_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = hasher.finalize();
        let mut out = String::with_capacity(digest.len() * 2);
        for &byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

/// Constant-time equality for two byte slices. Length mismatch is
/// not equal; the loop itself runs over the shorter length only
/// after the length check, matching the codebase convention in the
/// auth and storage modules (equal-length digests only).
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for index in 0..left.len() {
        diff |= left[index] ^ right[index];
    }
    diff == 0
}

/// Generate a fresh link token: returns `(raw_token, token_hash)`.
/// The raw token is 32 cryptographically random bytes, base64url
/// encoded (43 characters, no padding). Store ONLY the hash; show
/// the raw token exactly once, in the creation response.
pub fn generate_link_token() -> (String, String) {
    let mut bytes = [0u8; LINK_TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let raw = URL_SAFE_NO_PAD.encode(bytes);
    let hash = sha256_hex(&raw);
    (raw, hash)
}

/// Hash a presented token for lookup / comparison.
pub fn hash_link_token(presented: &str) -> String {
    sha256_hex(presented.trim())
}

/// Generate a random one-time password for a new auditor account.
/// Shown exactly once, in the creation response; only its Argon2
/// hash (computed by the auditors module) is stored.
pub fn generate_auditor_password() -> String {
    let mut bytes = [0u8; AUDITOR_PASSWORD_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

// HMAC-signed link URL payload

/// Sign a link URL payload with HMAC-SHA256 under `secret`.
/// Returns `base64url(payload).hex(hmac)` where the payload is
/// `"link_id|kind:uuid:view|expires_unix"`. The secret comes from
/// configuration (`app.secret_key`); a per-process fallback secret
/// must never be used here, because a signed URL must keep
/// verifying after a restart. Callers that have no configured
/// secret must refuse to mint signed URLs instead of inventing one.
pub fn sign_link_url(secret: &[u8], link_id: Uuid, scope: &ShareScope, expires_at: DateTime<Utc>) -> String {
    let payload = format!(
        "{}|{}|{}",
        link_id,
        scope.to_scope_string(),
        expires_at.timestamp()
    );
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(payload.as_bytes());
    let tag: String = {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let tag = mac.finalize().into_bytes();
        let mut out = String::with_capacity(tag.len() * 2);
        for &byte in tag.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    };
    format!("{}.{}", URL_SAFE_NO_PAD.encode(payload.as_bytes()), tag)
}

/// Verify a signed link URL payload produced by `sign_link_url`.
/// Returns `(link_id, scope, expires_at)` on success. A bad
/// signature, a malformed payload, or an expired payload is an
/// error; the caller maps it to `LinkDenial::BadToken` /
/// `LinkDenial::Expired` and logs the attempt.
pub fn verify_signed_link_url(
    secret: &[u8],
    signed: &str,
    now: DateTime<Utc>,
) -> UnifiedResult<(Uuid, ShareScope, DateTime<Utc>)> {
    let (encoded, tag) = signed
        .rsplit_once('.')
        .ok_or_else(|| UnifiedError::bad_request("signed link payload is malformed"))?;
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| UnifiedError::bad_request("signed link payload is malformed"))?;
    let payload = String::from_utf8(payload_bytes)
        .map_err(|_| UnifiedError::bad_request("signed link payload is malformed"))?;
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(payload.as_bytes());
    let expected: String = {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let tag = mac.finalize().into_bytes();
        let mut out = String::with_capacity(tag.len() * 2);
        for &byte in tag.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    };
    if !constant_time_eq(expected.as_bytes(), tag.as_bytes()) {
        return Err(UnifiedError::unauthorized("signed link signature does not match"));
    }
    let mut parts = payload.splitn(3, '|');
    let link_id = parts
        .next()
        .ok_or_else(|| UnifiedError::bad_request("signed link payload is malformed"))?;
    let scope_raw = parts
        .next()
        .ok_or_else(|| UnifiedError::bad_request("signed link payload is malformed"))?;
    let expires_raw = parts
        .next()
        .ok_or_else(|| UnifiedError::bad_request("signed link payload is malformed"))?;
    let link_id = Uuid::parse_str(link_id)
        .map_err(|_| UnifiedError::bad_request("signed link payload is malformed"))?;
    let scope = ShareScope::parse(scope_raw)?;
    let expires_unix: i64 = expires_raw
        .parse()
        .map_err(|_| UnifiedError::bad_request("signed link payload is malformed"))?;
    let expires_at = DateTime::from_timestamp(expires_unix, 0)
        .ok_or_else(|| UnifiedError::bad_request("signed link payload is malformed"))?;
    if now > expires_at {
        return Err(UnifiedError::unauthorized("signed link has expired"));
    }
    Ok((link_id, scope, expires_at))
}

// Public link record + validation

/// One public link, as stored (in memory in the auditors module;
/// the durable row is `public_links` in migration 000067). The raw
/// token is NEVER stored: `token_hash` is the SHA-256 hex of it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicLink {
    pub id: Uuid,
    /// SHA-256 hex of the raw token. Unique in the database.
    pub token_hash: String,
    pub scope: ShareScope,
    /// Opaque actor id that created the link (`X-User-Id` value).
    pub created_by: String,
    pub expires_at: DateTime<Utc>,
    /// Set when revoked; a revoked link never validates again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl PublicLink {
    /// True when the link is past its expiry at `now`.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now > self.expires_at
    }

    /// True when the link was revoked.
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
}

/// Why a public link validation failed. The reason string is what
/// the access log stores; the client-visible message is always the
/// same generic "link is not valid" text, so an attacker cannot
/// distinguish "no such link" from "revoked" from "wrong scope".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkDenial {
    /// No link row matched the presented token hash.
    NotFound,
    /// The presented token does not match the stored hash.
    BadToken,
    /// The link was revoked.
    Revoked,
    /// The link is past its expiry.
    Expired,
    /// The link is valid but for a different resource or view.
    ScopeMismatch,
}

impl LinkDenial {
    /// Stable reason string for the access log and hook payloads.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::BadToken => "bad_token",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::ScopeMismatch => "scope_mismatch",
        }
    }
}

/// Validate a presented token against a stored link for the
/// requested scope. `link` is the row found by token-hash lookup
/// (`None` when no row matched). The caller is responsible for the
/// hash lookup and for writing the access attempt (see
/// `PublicLinkAccessAttempt`); this function only decides.
///
/// Order matters: hash comparison first (constant-time), then
/// revoked, then expired, then scope. A revoked or expired link
/// still costs the attacker a failed validation (the caller feeds
/// it to the hardening engine), and the denial reason is recorded.
pub fn validate_public_link(
    link: Option<&PublicLink>,
    presented_token: &str,
    requested: &ShareScope,
    now: DateTime<Utc>,
) -> Result<ShareScope, LinkDenial> {
    let Some(link) = link else {
        return Err(LinkDenial::NotFound);
    };
    let presented_hash = hash_link_token(presented_token);
    if !constant_time_eq(
        presented_hash.as_bytes(),
        link.token_hash.as_bytes(),
    ) {
        return Err(LinkDenial::BadToken);
    }
    if link.is_revoked() {
        return Err(LinkDenial::Revoked);
    }
    if link.is_expired(now) {
        return Err(LinkDenial::Expired);
    }
    if !link.scope.matches(requested) {
        return Err(LinkDenial::ScopeMismatch);
    }
    Ok(link.scope.clone())
}

/// One public-link access attempt, for the access log. Every
/// attempt is recorded - allowed AND denied - with only hashes of
/// the IP and user agent, never the raw values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicLinkAccessAttempt {
    pub id: Uuid,
    /// The link the attempt was for, when a row matched; `None`
    /// when the token matched no link at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<Uuid>,
    pub accessed_at: DateTime<Utc>,
    /// SHA-256 hex of the client IP (or `None` when unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_hash: Option<String>,
    /// SHA-256 hex of the user agent (or `None` when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent_hash: Option<String>,
    pub allowed: bool,
    /// Reason string from `LinkDenial::as_str`, or `"ok"`.
    pub reason: String,
}

/// Build an access-log record for one validation outcome. Hashes
/// the IP and user agent; the caller stores the record.
pub fn access_attempt(
    link_id: Option<Uuid>,
    ip: Option<&str>,
    user_agent: Option<&str>,
    outcome: &Result<ShareScope, LinkDenial>,
    now: DateTime<Utc>,
) -> PublicLinkAccessAttempt {
    let (allowed, reason) = match outcome {
        Ok(_) => (true, "ok".to_string()),
        Err(denial) => (false, denial.as_str().to_string()),
    };
    PublicLinkAccessAttempt {
        id: Uuid::now_v7(),
        link_id,
        accessed_at: now,
        ip_hash: ip
            .filter(|value| !value.is_empty())
            .map(sha256_hex),
        user_agent_hash: user_agent
            .filter(|value| !value.is_empty())
            .map(sha256_hex),
        allowed,
        reason,
    }
}

/// Feed a failed public-link validation into the existing hardening
/// engine, so token guessing trips the same progressive lockout as
/// failed logins. The account key is `public_link:<link id>` (or
/// `public_link:unknown` when no link row matched), so guessing
/// against one link locks that link's key while guessing across
/// many links still locks the source IP. This function does NOT
/// implement its own rate limiter; the thresholds, windows, and
/// lockout durations all come from `SecuritySettings`.
pub fn record_failed_link_validation(
    hardening: &HardeningState,
    ip: Option<&str>,
    link_id: Option<Uuid>,
) {
    let account = match link_id {
        Some(id) => format!("public_link:{id}"),
        None => "public_link:unknown".to_string(),
    };
    hardening.record_login_failure(ip, Some(&account));
}

/// A successful validation resets the hardening counters for the IP
/// and the link key, exactly like a successful login does.
pub fn record_successful_link_validation(
    hardening: &HardeningState,
    ip: Option<&str>,
    link_id: Uuid,
) {
    let account = format!("public_link:{link_id}");
    hardening.record_login_success(ip, Some(&account));
}

// Auditor scope (the security boundary)

/// One scope entry: what an auditor may see. The three shapes are:
///   - `resource_id` set: exactly that one record.
///   - `resource_id` empty and `company_id` set: every record of
///     this kind inside that company.
///   - both empty: every record of this kind in this module.
/// An auditor with no entries sees nothing. There is no entry that
/// means "everything": a scope must name modules explicitly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditorScopeEntry {
    /// Owning module id, for example `"projects"`.
    pub module: String,
    /// Resource kind inside the module, for example `"project"`.
    pub resource_kind: String,
    /// One record, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<Uuid>,
    /// One company, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    /// True only when this entry also permits exporting the
    /// records it covers. Default false; exports are denied
    /// unless an entry explicitly grants them.
    #[serde(default)]
    pub allow_export: bool,
}

/// The data an auditor may see: a set of scope entries plus the
/// rule that anything not covered is invisible. Scope evaluation
/// happens here, in one place, so every module answers the same
/// question the same way through the `auditors.scope.check` hook.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditorScope {
    #[serde(default)]
    pub entries: Vec<AuditorScopeEntry>,
}

impl AuditorScope {
    /// True when the scope covers reading this exact record.
    /// `resource_id` and `company_id` are the record's own ids as
    /// known by the owning module; pass `None` when the module does
    /// not carry that dimension (a record without a company never
    /// matches a company-scoped entry).
    pub fn allows(
        &self,
        module: &str,
        resource_kind: &str,
        resource_id: Option<Uuid>,
        company_id: Option<Uuid>,
    ) -> bool {
        self.entries.iter().any(|entry| {
            if entry.module != module || entry.resource_kind != resource_kind {
                return false;
            }
            if let Some(wanted) = entry.resource_id {
                return Some(wanted) == resource_id;
            }
            if let Some(wanted_company) = entry.company_id {
                return Some(wanted_company) == company_id;
            }
            true
        })
    }

    /// True when the scope covers reading ANY record of this kind
    /// in this module (used for list endpoints: a list is only
    /// served when at least one entry covers the kind, and the
    /// query itself must then be restricted to the covered ids -
    /// see the data-layer note at the top of this file).
    pub fn allows_kind(&self, module: &str, resource_kind: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.module == module && entry.resource_kind == resource_kind)
    }

    /// True when the scope covers exporting this exact record.
    /// Export requires an entry that both covers the record AND
    /// sets `allow_export: true`.
    pub fn allows_export(
        &self,
        module: &str,
        resource_kind: &str,
        resource_id: Option<Uuid>,
        company_id: Option<Uuid>,
    ) -> bool {
        self.entries.iter().any(|entry| {
            if !entry.allow_export || entry.module != module || entry.resource_kind != resource_kind {
                return false;
            }
            if let Some(wanted) = entry.resource_id {
                return Some(wanted) == resource_id;
            }
            if let Some(wanted_company) = entry.company_id {
                return Some(wanted_company) == company_id;
            }
            true
        })
    }

    /// The set of record ids this scope covers for one
    /// (module, kind), when the scope names records individually.
    /// Returns `None` when the scope covers the kind wholesale
    /// (a module-wide or company-wide entry): the caller must then
    /// filter by company id instead of by record id. This is the
    /// helper a list endpoint uses to build its data-layer filter:
    /// `Some(ids)` -> `record_id IN (ids)`; `None` plus a company
    /// set -> `company_id IN (companies)`; neither -> deny the list.
    pub fn covered_record_ids(&self, module: &str, resource_kind: &str) -> Option<Vec<Uuid>> {
        let mut ids: Vec<Uuid> = Vec::new();
        let mut wholesale = false;
        for entry in &self.entries {
            if entry.module != module || entry.resource_kind != resource_kind {
                continue;
            }
            match entry.resource_id {
                Some(id) => ids.push(id),
                None => wholesale = true,
            }
        }
        if wholesale {
            None
        } else if ids.is_empty() {
            Some(Vec::new())
        } else {
            ids.sort();
            ids.dedup();
            Some(ids)
        }
    }

    /// The set of company ids this scope covers for one
    /// (module, kind) through company-wide entries. Empty when the
    /// scope names records individually or covers the kind
    /// wholesale without a company bound.
    pub fn covered_company_ids(&self, module: &str, resource_kind: &str) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self
            .entries
            .iter()
            .filter(|entry| {
                entry.module == module
                    && entry.resource_kind == resource_kind
                    && entry.resource_id.is_none()
            })
            .filter_map(|entry| entry.company_id)
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

/// Actions an auditor account may never perform, whatever its
/// scope says. The auditors module rejects these before any scope
/// check runs, and the list is enforced in
/// `assert_auditor_action_allowed` below.
pub const AUDITOR_FORBIDDEN_ACTIONS: &[&str] = &[
    "create",
    "update",
    "delete",
    "settings.write",
    "settings.read",
    "apikey.create",
    "role.assign",
    "role.create",
    "password.change",
    "user.create",
    "user.delete",
];

/// Decide whether an auditor may perform `action` (`"read"` or
/// `"export"`) on one record. Mutations and settings access are
/// rejected here without consulting the scope: auditors are
/// read-only always. Export additionally requires the scope entry
/// to grant it. Returns `Ok(())` when allowed, otherwise a
/// 403 `UnifiedError` whose message names the denial without
/// revealing whether the record exists.
pub fn assert_auditor_action_allowed(
    scope: &AuditorScope,
    module: &str,
    resource_kind: &str,
    resource_id: Option<Uuid>,
    company_id: Option<Uuid>,
    action: &str,
) -> UnifiedResult<()> {
    if AUDITOR_FORBIDDEN_ACTIONS.contains(&action) {
        return Err(UnifiedError::forbidden(format!(
            "Auditor accounts may not perform \"{action}\""
        )));
    }
    let allowed = match action {
        "read" => scope.allows(module, resource_kind, resource_id, company_id),
        "export" => scope.allows_export(module, resource_kind, resource_id, company_id),
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Auditor scope does not cover this resource",
        ))
    }
}

/// One auditor identity, as the rest of the application sees it.
/// `is_auditor` is always true for these rows; `auditor_of` is the
/// admin actor id that created the account; `expires_at` is the
/// hard expiry after which the account is disabled and its sessions
/// revoked. An auditor identity must NEVER be convertible into a
/// permanent account: there is no endpoint that clears
/// `is_auditor`, extends expiry past the configured maximum, or
/// assigns a role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditorIdentity {
    pub auditor_id: Uuid,
    /// Opaque user id string (the codebase convention: actor ids
    /// are strings, not necessarily the users table UUID).
    pub user_id: String,
    pub is_auditor: bool,
    pub auditor_of: String,
    pub expires_at: DateTime<Utc>,
}

impl AuditorIdentity {
    /// True when the account is still within its lifetime at `now`.
    /// Expiry is checked on every use, not only by the scheduler
    /// sweep, so a missed sweep can never leave an expired auditor
    /// able to read.
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        self.is_auditor && now <= self.expires_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_known_vectors() {
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_hex_bytes_matches_known_vectors() {
        assert_eq!(
            sha256_hex_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}

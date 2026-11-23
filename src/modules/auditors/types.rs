// Auditor types and request bodies.

use unified_core::sharing::{AuditorIdentity, AuditorScopeEntry, PublicLink, ShareScope};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_LABEL_LEN: usize = 200;
pub(crate) const MAX_EMAIL_LEN: usize = 254;
pub(crate) const MAX_REASON_LEN: usize = 500;
pub(crate) const MAX_SCOPE_ENTRIES: usize = 100;
pub(crate) const MAX_MODULE_LEN: usize = 64;
pub(crate) const MAX_KIND_LEN: usize = 64;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_AUDITORS_VIEW: &str = "auditors.view";
pub(crate) const PERM_AUDITORS_CREATE: &str = "auditors.create";
pub(crate) const PERM_AUDITORS_MANAGE: &str = "auditors.manage";
pub(crate) const PERM_AUDITORS_REVOKE: &str = "auditors.revoke";
pub(crate) const PERM_SHARING_VIEW: &str = "sharing.view";
pub(crate) const PERM_SHARING_CREATE: &str = "sharing.create";
pub(crate) const PERM_SHARING_REVOKE: &str = "sharing.revoke";

/// Hook event names this module listens to (literals repeated here
/// so the module stays decoupled from the auth / scheduler modules,
/// exactly like the events module repeats the payments constants).
pub(crate) const EXPIRE_REQUESTED: &str = "auditors.expire_requested";
pub(crate) const SCOPE_CHECK: &str = "auditors.scope.check";
pub(crate) const SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Lifecycle of an auditor account: `active` -> `revoked` (admin
/// action, immediate) or `expired` (expiry time passed; set by the
/// sweep, or derived at read time when the sweep has not run yet).
/// The stored status never moves back to `active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditorStatus {
    Active,
    Revoked,
    Expired,
}

impl AuditorStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }
}

/// One auditor account. `user_id` is the opaque auditor user id
/// (a UUID string minted here; the users module owns real user
/// rows and no row is created there). `password_hash` is the Argon2id
/// PHC hash of the one-time password shown once at creation, and is
/// never serialised. `is_auditor` is always true and `auditor_of`
/// is the admin actor id that created the account, so the account
/// is distinguishable in audit records and sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditorAccount {
    pub id: Uuid,
    pub user_id: String,
    pub label: String,
    pub email: String,
    pub is_auditor: bool,
    pub auditor_of: String,
    pub status: AuditorStatus,
    pub expires_at: DateTime<Utc>,
    #[serde(skip_serializing)]
    pub password_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoke_reason: Option<String>,
    /// Scheduler outcome for the expiry job queued at creation:
    /// `queued` or `declined` (see the scheduler note in the module
    /// header). Never pretends a job was queued when it was not.
    pub scheduler_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_job_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AuditorAccount {
    /// Effective status at read time: an active account whose
    /// expiry has passed reads as `expired` even when the sweep
    /// has not run yet, so expiry is enforced on every use.
    pub fn effective_status(&self, now: DateTime<Utc>) -> AuditorStatus {
        match self.status {
            AuditorStatus::Active if now > self.expires_at => AuditorStatus::Expired,
            other => other,
        }
    }

    /// True when the account may still read at `now`.
    pub fn is_usable(&self, now: DateTime<Utc>) -> bool {
        self.effective_status(now) == AuditorStatus::Active
    }

    /// The auditor identity view of this account.
    pub fn identity(&self) -> AuditorIdentity {
        AuditorIdentity {
            auditor_id: self.id,
            user_id: self.user_id.clone(),
            is_auditor: self.is_auditor,
            auditor_of: self.auditor_of.clone(),
            expires_at: self.expires_at,
        }
    }
}

/// One auditor account with its scope entries, as returned by the
/// detail endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct AuditorAccountView {
    #[serde(flatten)]
    pub account: AuditorAccount,
    pub scopes: Vec<AuditorScopeEntry>,
}

/// One auditor session: a recorded login of an auditor user id.
/// `revoked_at` set means the session was killed (by account
/// revoke, expiry, or a single-session revoke). Sessions are
/// records, not issued tokens: the auth flow owns the actual
/// session token, this module only tracks liveness for the banner
/// and for one-click revoke.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditorSession {
    pub id: Uuid,
    pub auditor_id: Uuid,
    pub user_id: String,
    pub started_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_by: Option<String>,
}

impl AuditorSession {
    /// True when the session is live at `now` for an account that
    /// is still usable. A session outlives nothing: account revoke
    /// or expiry kills it (checked here, not only by the sweep).
    pub fn is_live(&self, account_usable: bool) -> bool {
        self.revoked_at.is_none() && account_usable
    }
}

/// One entry in the auditor action log: every auditor action,
/// allowed AND denied, with actor, auditor, resource, action, and
/// outcome. The log is append-only inside this module; there is no
/// update or delete endpoint for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditorActionLogEntry {
    pub id: Uuid,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auditor_id: Option<Uuid>,
    pub resource: String,
    pub action: String,
    pub allowed: bool,
    pub created_at: DateTime<Utc>,
}

/// One public-link access log row, as stored (the durable row is
/// `public_link_access_log` in migration 000067). Only hashes of
/// the IP and user agent are stored, never the raw values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicLinkAccessLogEntry {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<Uuid>,
    pub accessed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent_hash: Option<String>,
    pub allowed: bool,
    pub reason: String,
}

/// One public link as returned by the list / detail endpoints.
/// The raw token is NEVER included: it exists only in the creation
/// response, once.
#[derive(Debug, Clone, Serialize)]
pub struct PublicLinkView {
    pub id: Uuid,
    pub scope: ShareScope,
    pub created_by: String,
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// True when the link is past its expiry at read time.
    pub expired: bool,
}

impl From<&PublicLink> for PublicLinkView {
    fn from(link: &PublicLink) -> Self {
        Self {
            id: link.id,
            scope: link.scope.clone(),
            created_by: link.created_by.clone(),
            expires_at: link.expires_at,
            revoked_at: link.revoked_at,
            created_at: link.created_at,
            expired: link.is_expired(Utc::now()),
        }
    }
}

/// Creation response for a public link: the ONLY place the raw
/// token appears. Clients must store it now; it cannot be
/// retrieved again.
#[derive(Debug, Clone, Serialize)]
pub struct PublicLinkCreated {
    #[serde(flatten)]
    pub link: PublicLinkView,
    /// The raw token, shown exactly once. Never stored server-side.
    pub token: String,
    /// The public URL that serves this link (token in the path,
    /// scope in the query). The URL alone is not the credential
    /// without the token, and the token alone is not enough without
    /// the matching scope query.
    pub url: String,
    /// HMAC-signed payload for the link (`link_id|scope|expiry`,
    /// signed under `app.secret_key`), when a secret is configured.
    /// Owning modules can verify it with
    /// `core::sharing::verify_signed_link_url` instead of trusting
    /// a caller-supplied scope string. `None` when no secret is
    /// configured — the link still works through the token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_payload: Option<String>,
}

/// Creation response for an auditor account: the ONLY place the
/// one-time password appears.
#[derive(Debug, Clone, Serialize)]
pub struct AuditorCreated {
    #[serde(flatten)]
    pub account: AuditorAccountView,
    /// The one-time password, shown exactly once. Only its Argon2id
    /// hash is stored. The auditor must change nothing about it:
    /// there is no password-change endpoint for auditors, and the
    /// password never converts the account into a permanent one.
    pub one_time_password: String,
}

/// Active auditor sessions for the UI banner: who is watching,
/// since when, and what they may see.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveAuditorSessionView {
    pub session_id: Uuid,
    pub auditor_id: Uuid,
    pub user_id: String,
    pub label: String,
    pub auditor_of: String,
    pub started_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub scopes: Vec<AuditorScopeEntry>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AuditorScopeInput {
    pub module: String,
    pub resource_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    #[serde(default)]
    pub allow_export: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateAuditorRequest {
    pub label: String,
    pub email: String,
    /// Required lifetime in hours (1..=max_auditor_ttl_hours).
    pub expires_in_hours: i64,
    /// At least one scope entry; an auditor with no entries sees
    /// nothing. Each entry names one module + resource kind, plus
    /// an optional single record, an optional single company, and
    /// an optional export grant.
    pub scopes: Vec<AuditorScopeInput>,
}

#[derive(Debug, Deserialize)]
pub struct ExtendAuditorRequest {
    /// Additional hours to add to the current expiry.
    /// The resulting expiry must not exceed now + max TTL.
    pub additional_hours: i64,
}

#[derive(Debug, Deserialize)]
pub struct RevokeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateLinkRequest {
    pub scope_kind: String,
    pub scope_id: Uuid,
    pub scope_view: String,
    /// Required lifetime in hours (1..=max_link_ttl_hours, and
    /// never more than 30 days).
    pub expires_in_hours: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PublicLinkQuery {
    pub scope_kind: Option<String>,
    pub scope_id: Option<String>,
    pub scope_view: Option<String>,
}

// Auditors & Public Link Sharing module — temporary sandbox
// auditor accounts with scoped read-only access, and public links
// that expose exactly one resource in one view.
//
// Routes live under `/api/v1/auditors` and `/api/v1/sharing`:
//
//   Auditors:
//     POST / — create a temp auditor account (expiry required).
//     GET  / — list auditor accounts (paginated, filter by status,
//          auditor_of, company_id, search on label/email).
//     GET  /{id} — one auditor account with its scope.
//     POST /{id}/revoke — revoke immediately (kills live sessions).
//     POST /{id}/extend — extend the expiry (capped at the maximum).
//     GET  /{id}/sessions — sessions of one auditor.
//     POST /{id}/sessions/{session_id}/revoke — kill one session.
//     GET  /{id}/activity — auditor action log (paginated, filter
//          by action, allowed).
//     GET  /sessions/active — active auditor sessions (banner data
//          for the UI: who is watching, since when, what scope).
//     GET  /me/scope — the calling auditor's own scope (self only).
//     POST /expire-sweep — run the expiry sweep now (admin).
//   Public links:
//     POST /api/v1/sharing/links — create a link (the target's
//          `allow_public_link` flag is enforced server-side through
//          the `sharing.allow_check` hook; a false answer is a 403).
//     GET  /api/v1/sharing/links — list links (paginated, filter by
//          scope_kind, created_by, revoked, expired).
//     GET  /api/v1/sharing/links/{id} — one link (never the token).
//     POST /api/v1/sharing/links/{id}/revoke — revoke a link.
//     GET  /api/v1/sharing/links/{id}/access-log — access log of one
//          link (paginated, filter by allowed).
//     GET  /api/v1/sharing/access-log — access log of all links
//          (paginated, filter by allowed, link_id, reason).
//     GET  /api/v1/sharing/kinds — registered shareable kinds.
//   Public (no auth; see the public-access note below):
//     GET  /api/v1/sharing/public/{token}?scope_kind=&scope_id=
//          &scope_view= — validate a link token and return its scope
//          descriptor. Serves NO resource content: the owning module
//          serves content after calling
//          `crate::core::sharing::validate_public_link` itself.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory auditor accounts, scopes, sessions, action log, public
// links, and access log; Argon2id hashing of the auditor one-time
// password (shown exactly once at creation); SHA-256 token hashes
// (raw tokens are never stored); constant-time token comparison;
// HMAC-signed link payloads; server-side enforcement of the
// `allow_public_link` flag through the `sharing.allow_check` hook
// (fail closed when no module answers); failed link validations
// feeding the hardening engine's progressive lockout; auto-expire
// of auditor accounts (checked on every use, plus a scheduler job
// kind `auditors.expire`); one-click revoke that kills live
// sessions immediately; scope evaluation through
// `crate::core::sharing::AuditorScope` (allow/deny for one
// (module, kind, record, company) tuple, export only when the
// scope entry grants it). What is NOT built here: the actual user
// row in the users module (the auditor account references a user
// by opaque id string, following the codebase convention; the
// users module stays the source of truth for identity, and no
// user row is created or modified here); login / session issuing
// for auditors (the auth flow owns sessions — this module only
// records a session row when `auth.login` fires for an auditor
// user id, and revokes sessions by marking them revoked); the
// resource content behind a public link (owning modules serve it
// after validating the link through `core::sharing`); and the
// durable read path (see the persistence note below).
//
// Persistence note: the in-memory DashMap store is authoritative,
// exactly like the events module. Durable rows for these types are
// defined by migration `2026-11-22-000067_sharing_auditor`; until
// that migration is executed, auditor accounts and links do not
// survive a restart. The migration has never been executed against
// a live PostgreSQL database.
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`, and denied outright when a rule exists
// with `allowed: false`). The public route below relies on that
// gate: an administrator must create the matching `ApiAccessRule`
// row for `GET /api/v1/sharing/public/{token}`. The route is
// registered in `route_info()` with `route_version_support()`
// defaulting to `All`, so the API Manager's per-version registry
// picks it up. Do NOT treat the absence of a capability check on
// the public route as an open door: without the rule the API
// Manager denies the call, and without a valid token the handler
// itself denies it.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai and events modules. Capability
// checks use the `role.check` hook and fail closed (no answer
// means not allowed). The public route takes no identity headers:
// the link token IS the credential, and it grants read-only
// access to exactly one scoped resource.
//
// Auditor escalation guards: an auditor account can never gain a
// role (no endpoint assigns one), can never change a password to a
// permanent account (the one-time password is set once at creation
// and there is no password-change endpoint for auditors), and can
// never create an API key (API-key creation requires a capability
// auditors never hold, and the scope check rejects the
// `apikey.create` action before any scope lookup). Auditor callers
// are recognised by `X-User-Id` matching an active auditor account;
// when a request carries an auditor identity, the module's own
// handlers reject every mutation with 403 — auditors are
// read-only always, including on this module's own routes (an
// auditor may read its own scope and nothing else here).
//
// Events emitted on the hook bus:
//   - `sharing.link.created`, `sharing.link.revoked`,
//     `sharing.link.accessed`, `sharing.link.access_denied`
//   - `auditors.created`, `auditors.revoked`, `auditors.expired`,
//     `auditors.action`, `auditors.session.started`,
//     `auditors.session.revoked`
//   - `scheduler.enqueue` with `{"kind": "auditors.expire",
//     "payload": {"auditor_id", "run_at"}}` when an auditor is
//     created (the sweep job for its expiry).
//   - `notification.requested` when an auditor account is created
//     or revoked, addressed to the creating admin (`auditor_of`).
// Events this module listens to:
//   - `auditors.expire_requested` — the event the scheduler emits
//     when an `auditors.expire` job fires, following its naming
//     rule (`backup.run` -> `backup.run_requested`); expires the
//     named account (when due) or sweeps all due accounts.
//   - `auditors.scope.check` — answers `allowed` from the stored
//     auditor scope for one (module, kind, record, company,
//     action) tuple; `action` is `"read"` or `"export"`, anything
//     else is denied.
//   - `auth.login` — when the logging-in user id belongs to an
//     active auditor account, records a session row so the banner
//     endpoint can show it. The payload shape is read defensively
//     (`user_id`, then `actor`, then `id`); an unrecognised payload
//     is ignored, never an error.
//
// Scheduler note: `auditors.expire` is NOT in the scheduler's
// `KNOWN_KINDS` list yet, so today the scheduler answers
// `{"queued": false, "error": "Unknown job kind: ..."}` and the
// auditor row records `scheduler_status: "declined"` instead of
// pretending it was scheduled. The account is stored either way
// and stays usable; expiry is ALSO checked on every use (see
// `AuditorIdentity::is_active`), so a missed sweep can never
// leave an expired auditor able to read. The follow-up is to add
// `"auditors.expire"` to `KNOWN_KINDS` in `scheduler.rs` and arm
// the dispatch so the job emits `auditors.expire_requested` with
// the auditor payload (see the integrator snippet in the module
// report).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::sharing::{
    self, access_attempt, assert_auditor_action_allowed, check_allow_public_link,
    generate_auditor_password, generate_link_token, hash_link_token, record_failed_link_validation,
    record_successful_link_validation, sharing_events, validate_public_link, AuditorIdentity,
    AuditorScope, AuditorScopeEntry, PublicLink, ShareRegistry, ShareScope, SharingSettings,
    MIN_AUDITOR_TTL_HOURS,
};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_LABEL_LEN: usize = 200;
const MAX_EMAIL_LEN: usize = 254;
const MAX_REASON_LEN: usize = 500;
const MAX_SCOPE_ENTRIES: usize = 100;
const MAX_MODULE_LEN: usize = 64;
const MAX_KIND_LEN: usize = 64;

/// Capability names checked through the `role.check` hook.
const PERM_AUDITORS_VIEW: &str = "auditors.view";
const PERM_AUDITORS_CREATE: &str = "auditors.create";
const PERM_AUDITORS_MANAGE: &str = "auditors.manage";
const PERM_AUDITORS_REVOKE: &str = "auditors.revoke";
const PERM_SHARING_VIEW: &str = "sharing.view";
const PERM_SHARING_CREATE: &str = "sharing.create";
const PERM_SHARING_REVOKE: &str = "sharing.revoke";

/// Hook event names this module listens to (literals repeated here
/// so the module stays decoupled from the auth / scheduler modules,
/// exactly like the events module repeats the payments constants).
const EXPIRE_REQUESTED: &str = "auditors.expire_requested";
const SCOPE_CHECK: &str = "auditors.scope.check";
const SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";

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
    fn as_str(&self) -> &'static str {
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

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for auditors and public links. The hook bus is
/// attached in `init` (the bus lives in `AppContext`); handlers
/// tolerate a missing bus by skipping event emission. Durable rows
/// for these types are defined by migration
/// `2026-11-22-000067_sharing_auditor`; until that migration is
/// executed, auditors and links do not survive a restart.
#[derive(Clone)]
pub struct AuditorsState {
    pub auditors: Arc<DashMap<Uuid, AuditorAccount>>,
    pub scopes: Arc<DashMap<Uuid, Vec<AuditorScopeEntry>>>,
    pub sessions: Arc<DashMap<Uuid, AuditorSession>>,
    pub links: Arc<DashMap<Uuid, PublicLink>>,
    pub links_by_hash: Arc<DashMap<String, Uuid>>,
    pub access_log: Arc<DashMap<Uuid, PublicLinkAccessLogEntry>>,
    pub action_log: Arc<DashMap<Uuid, AuditorActionLogEntry>>,
    pub registry: Arc<ShareRegistry>,
    pub settings: Arc<RwLock<SharingSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for AuditorsState {
    fn default() -> Self {
        Self {
            auditors: Arc::new(DashMap::new()),
            scopes: Arc::new(DashMap::new()),
            sessions: Arc::new(DashMap::new()),
            links: Arc::new(DashMap::new()),
            links_by_hash: Arc::new(DashMap::new()),
            access_log: Arc::new(DashMap::new()),
            action_log: Arc::new(DashMap::new()),
            registry: Arc::new(ShareRegistry::new()),
            settings: Arc::new(RwLock::new(SharingSettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl AuditorsState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn settings(&self) -> SharingSettings {
        self.settings
            .read()
            .expect("sharing settings lock poisoned")
            .clone()
    }

    /// Register the shareable kinds this platform serves. Until a
    /// kind is registered, link creation for it fails closed by
    /// design — the registry rejects the kind before the owning
    /// module is ever asked. Registration is data only (kind name,
    /// views, description); the owning module still enforces
    /// `allow_public_link` and actor permission through its own
    /// `sharing.allow_check` answer, and no answer still means
    /// denied.
    ///
    /// Kinds registered here:
    ///   - `network_tools_run` / `["summary"]` — a network-tools
    ///     run result. The network_tools module answers
    ///     `sharing.allow_check` for it (owner-only), so links for
    ///     it work end to end once registered.
    ///   - `project` / `["board", "summary"]` — a project board or
    ///     summary (batch 5 contract). NOTE: the projects module
    ///     does NOT answer `sharing.allow_check` yet, so links for
    ///     this kind still fail closed at the check until the
    ///     projects module adds the answerer.
    /// Called from `AuditorsModule::init`, which is the one place
    /// that holds the registry handle; there is no hook for
    /// registering kinds.
    fn register_builtin_shareable_kinds(&self) -> UnifiedResult<()> {
        self.registry.register(
            "network_tools_run",
            &["summary"],
            "Network-tools run result, shared as a read-only summary (owner-only).",
        )?;
        self.registry.register(
            "project",
            &["board", "summary"],
            "Project board or summary, shared read-only (owning module must allow it).",
        )?;
        Ok(())
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape (see the events module's `notify`):
    /// `kind`, `user_id`, `title`, `body`, `module: "auditors"`,
    /// `link`, `priority`, `channels`.
    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "auditors",
                "category": "auditors",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    /// Append one entry to the auditor action log and emit
    /// `auditors.action` for the audit module. Every auditor action
    /// — allowed and denied — goes through here.
    fn log_auditor_action(
        &self,
        actor: &str,
        auditor_id: Option<Uuid>,
        resource: &str,
        action: &str,
        allowed: bool,
    ) {
        let entry = AuditorActionLogEntry {
            id: Uuid::now_v7(),
            actor: actor.to_string(),
            auditor_id,
            resource: resource.to_string(),
            action: action.to_string(),
            allowed,
            created_at: Utc::now(),
        };
        self.action_log.insert(entry.id, entry);
        self.emit(
            sharing_events::AUDITOR_ACTION,
            json!({
                "auditor_id": auditor_id.map(|id| id.to_string()),
                "actor": actor,
                "resource": resource,
                "action": action,
                "allowed": allowed,
            }),
        );
    }

    /// Scope of one auditor account, as an `AuditorScope` value
    /// (empty when the account has no entries: sees nothing).
    fn scope_of(&self, auditor_id: Uuid) -> AuditorScope {
        AuditorScope {
            entries: self
                .scopes
                .get(&auditor_id)
                .map(|entry| entry.value().clone())
                .unwrap_or_default(),
        }
    }

    /// Find the auditor account for a user id, when the caller IS
    /// an auditor. Returns the account regardless of status; the
    /// caller checks usability (expiry / revoke) itself.
    fn auditor_for_user(&self, user_id: &str) -> Option<AuditorAccount> {
        self.auditors
            .iter()
            .map(|entry| entry.value().clone())
            .find(|account| account.user_id == user_id)
    }

    /// Kill every live session of one auditor (account revoke or
    /// expiry). Returns how many sessions were killed.
    fn kill_sessions(&self, auditor_id: Uuid, revoked_by: &str) -> usize {
        let now = Utc::now();
        let mut killed = 0usize;
        for mut entry in self.sessions.iter_mut() {
            if entry.value().auditor_id == auditor_id && entry.value().revoked_at.is_none() {
                entry.value_mut().revoked_at = Some(now);
                entry.value_mut().revoked_by = Some(revoked_by.to_string());
                killed += 1;
            }
        }
        if killed > 0 {
            self.emit(
                sharing_events::AUDITOR_SESSION_REVOKED,
                json!({
                    "auditor_id": auditor_id.to_string(),
                    "revoked_by": revoked_by,
                    "sessions_killed": killed,
                }),
            );
        }
        killed
    }
}

/// Expire every due auditor account: active accounts whose expiry
/// has passed become `expired` and their live sessions are killed.
/// Returns the ids that were expired by this call. This is the
/// function the `auditors.expire_requested` hook listener and the
/// manual sweep endpoint both call; expiry is also enforced on
/// every read through `effective_status`, so this sweep is a
/// bookkeeping step, never the only enforcement.
pub fn expire_due_auditors(state: &AuditorsState, now: DateTime<Utc>) -> Vec<Uuid> {
    let mut expired: Vec<Uuid> = Vec::new();
    let due: Vec<Uuid> = state
        .auditors
        .iter()
        .filter(|entry| {
            entry.value().status == AuditorStatus::Active && now > entry.value().expires_at
        })
        .map(|entry| *entry.key())
        .collect();
    for id in due {
        if let Some(mut account) = state.auditors.get_mut(&id) {
            account.status = AuditorStatus::Expired;
            account.updated_at = now;
        }
        state.kill_sessions(id, "system:expiry");
        state.log_auditor_action(
            "system:expiry",
            Some(id),
            &format!("auditors:{id}"),
            "auditors.expired",
            true,
        );
        state.emit(
            sharing_events::AUDITOR_EXPIRED,
            json!({ "auditor_id": id.to_string(), "expired_at": now.to_rfc3339() }),
        );
        expired.push(id);
    }
    expired.sort();
    expired
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the ai and events modules.
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
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai and events modules).
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

/// Reject the call when the caller is an auditor account: auditors
/// are read-only always, including on this module's own routes.
/// The only auditor-allowed read in this module is
/// `GET /api/v1/auditors/me/scope`, which does not call this guard.
fn reject_auditor_caller(state: &AuditorsState, actor: &str) -> UnifiedResult<()> {
    if let Some(account) = state.auditor_for_user(actor) {
        if account.is_usable(Utc::now()) {
            return Err(UnifiedError::forbidden(
                "Auditor accounts are read-only and may not perform this action",
            ));
        }
    }
    Ok(())
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
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

fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
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
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

/// Client IP for the access log: the real remote address when the
/// proxy headers are trusted by the server, else the peer address.
/// Only its SHA-256 hash is ever stored (see `access_attempt`).
fn client_ip(req: &HttpRequest) -> Option<String> {
    req.connection_info()
        .realip_remote_addr()
        .map(str::to_string)
        .or_else(|| req.peer_addr().map(|addr| addr.ip().to_string()))
}

fn user_agent(req: &HttpRequest) -> Option<String> {
    req.headers()
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn load_auditor(state: &AuditorsState, raw_id: &str) -> UnifiedResult<AuditorAccount> {
    let id = parse_uuid(raw_id, "auditor")?;
    state
        .auditors
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Auditor not found: {id}")))
}

fn load_link(state: &AuditorsState, raw_id: &str) -> UnifiedResult<PublicLink> {
    let id = parse_uuid(raw_id, "public link")?;
    state
        .links
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Public link not found: {id}")))
}

fn validate_scope_input(input: &AuditorScopeInput) -> UnifiedResult<AuditorScopeEntry> {
    let module = require_text(&input.module, "scopes[].module", MAX_MODULE_LEN)?.to_lowercase();
    if !module
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(UnifiedError::bad_request(
            "scopes[].module must contain only lowercase letters, digits, '_' or '-'",
        ));
    }
    let resource_kind =
        require_text(&input.resource_kind, "scopes[].resource_kind", MAX_KIND_LEN)?.to_lowercase();
    if !resource_kind
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(UnifiedError::bad_request(
            "scopes[].resource_kind must contain only lowercase letters, digits, '_' or '-'",
        ));
    }
    Ok(AuditorScopeEntry {
        module,
        resource_kind,
        resource_id: input.resource_id,
        company_id: input.company_id,
        allow_export: input.allow_export,
    })
}

/// Queue the expiry job for one auditor through `scheduler.enqueue`
/// with kind `auditors.expire`. Returns `(scheduler_status,
/// scheduler_job_id)`, honestly reporting `declined` when the
/// scheduler does not know the kind yet (see the scheduler note in
/// the module header). The account is stored either way.
fn schedule_expiry(state: &AuditorsState, auditor: &AuditorAccount) -> (String, Option<String>) {
    let Some(bus) = state.bus() else {
        return ("declined".to_string(), None);
    };
    let answer = bus.emit(
        SCHEDULER_ENQUEUE,
        json!({
            "kind": "auditors.expire",
            "payload": {
                "auditor_id": auditor.id.to_string(),
                "run_at": auditor.expires_at.to_rfc3339(),
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => (
            "queued".to_string(),
            result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        ),
        _ => ("declined".to_string(), None),
    }
}

// ---------------------------------------------------------------------------
// Hook listeners
// ---------------------------------------------------------------------------

/// Answer `auditors.scope.check` from the stored scope. Payload:
/// `{"user_id", "module", "resource_kind", "resource_id",
/// "company_id", "action", "allowed": false}`. `action` is `"read"`
/// or `"export"`; anything else is denied. An unknown user id, an
/// expired account, or a revoked account is denied. The answer is
/// written back into the payload's `"allowed"` key.
fn on_scope_check(state: AuditorsState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let module = ctx
        .get("module")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let resource_kind = ctx
        .get("resource_kind")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let resource_id = ctx
        .get("resource_id")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let company_id = ctx
        .get("company_id")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let action = ctx
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let allowed = match state.auditor_for_user(&user_id) {
        Some(account) if account.is_usable(Utc::now()) => {
            let scope = state.scope_of(account.id);
            assert_auditor_action_allowed(
                &scope,
                &module,
                &resource_kind,
                resource_id,
                company_id,
                &action,
            )
            .is_ok()
        }
        _ => false,
    };
    ctx.set("allowed", Value::Bool(allowed));
}

/// Handle `auditors.expire_requested` from the scheduler: expire
/// the named account when due, or sweep all due accounts when no
/// id is given. The payload id is never trusted blindly: the
/// account is expired only when its stored expiry has actually
/// passed.
fn on_expire_requested(state: AuditorsState, ctx: &mut HookContext) {
    let now = Utc::now();
    match ctx
        .get("auditor_id")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())
    {
        Some(id) => {
            let due = state
                .auditors
                .get(&id)
                .is_some_and(|account| account.status == AuditorStatus::Active && now > account.expires_at);
            if due {
                let expired = expire_due_auditors(&state, now);
                ctx.add_result(json!({ "expired": expired.iter().map(|id| id.to_string()).collect::<Vec<_>>() }));
            } else {
                ctx.add_result(json!({ "expired": Vec::<String>::new(), "reason": "auditor not due for expiry" }));
            }
        }
        None => {
            let expired = expire_due_auditors(&state, now);
            ctx.add_result(json!({ "expired": expired.iter().map(|id| id.to_string()).collect::<Vec<_>>() }));
        }
    }
}

/// Handle `auth.login`: when the logging-in user id belongs to an
/// active auditor account, record a session row so the banner
/// endpoint can show it. The payload shape is read defensively;
/// an unrecognised payload is ignored, never an error.
fn on_auth_login(state: AuditorsState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .or_else(|| ctx.get("actor"))
        .or_else(|| ctx.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if user_id.is_empty() {
        return;
    }
    let Some(account) = state.auditor_for_user(&user_id) else {
        return;
    };
    if !account.is_usable(Utc::now()) {
        return;
    }
    let now = Utc::now();
    let session = AuditorSession {
        id: Uuid::now_v7(),
        auditor_id: account.id,
        user_id: account.user_id.clone(),
        started_at: now,
        last_seen_at: now,
        revoked_at: None,
        revoked_by: None,
    };
    state.sessions.insert(session.id, session.clone());
    state.emit(
        sharing_events::AUDITOR_SESSION_STARTED,
        json!({
            "auditor_id": account.id.to_string(),
            "session_id": session.id.to_string(),
            "user_id": account.user_id,
        }),
    );
}

// ---------------------------------------------------------------------------
// Auditor handlers
// ---------------------------------------------------------------------------

async fn create_auditor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    body: web::Json<CreateAuditorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AuditorCreated> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_CREATE)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let settings = state.settings();
        if !settings.auditors_enabled {
            return Err(UnifiedError::forbidden(
                "Auditor accounts are disabled by configuration",
            ));
        }
        let input = body.into_inner();
        let label = require_text(&input.label, "label", MAX_LABEL_LEN)?;
        if !valid_email(&input.email) {
            return Err(UnifiedError::bad_request("email must be a valid email address"));
        }
        if input.expires_in_hours < MIN_AUDITOR_TTL_HOURS
            || input.expires_in_hours > settings.max_auditor_ttl_hours
        {
            return Err(UnifiedError::bad_request(format!(
                "expires_in_hours must be between {} and {}",
                MIN_AUDITOR_TTL_HOURS, settings.max_auditor_ttl_hours
            )));
        }
        if input.scopes.is_empty() {
            return Err(UnifiedError::bad_request(
                "scopes must contain at least one entry; an auditor with no scope sees nothing",
            ));
        }
        if input.scopes.len() > MAX_SCOPE_ENTRIES {
            return Err(UnifiedError::bad_request(format!(
                "scopes must contain at most {MAX_SCOPE_ENTRIES} entries"
            )));
        }
        let mut scopes: Vec<AuditorScopeEntry> = Vec::new();
        for entry in &input.scopes {
            scopes.push(validate_scope_input(entry)?);
        }
        scopes.sort_by(|a, b| {
            a.module
                .cmp(&b.module)
                .then(a.resource_kind.cmp(&b.resource_kind))
                .then(a.resource_id.cmp(&b.resource_id))
        });
        scopes.dedup_by(|a, b| {
            a.module == b.module
                && a.resource_kind == b.resource_kind
                && a.resource_id == b.resource_id
                && a.company_id == b.company_id
                && a.allow_export == b.allow_export
        });
        let now = Utc::now();
        let expires_at = now + chrono::Duration::hours(input.expires_in_hours);
        // One-time password: generated here, hashed with Argon2id,
        // raw value returned exactly once in this response.
        let one_time_password = generate_auditor_password();
        let password_hash = crate::auth::password::hash(&one_time_password)
            .map_err(|error| UnifiedError::internal(format!("Password hashing failed: {error}")))?;
        let mut account = AuditorAccount {
            id: Uuid::now_v7(),
            user_id: Uuid::now_v7().to_string(),
            label,
            email: input.email.trim().to_string(),
            is_auditor: true,
            auditor_of: caller.actor.clone(),
            status: AuditorStatus::Active,
            expires_at,
            password_hash,
            revoked_at: None,
            revoked_by: None,
            revoke_reason: None,
            scheduler_status: "declined".to_string(),
            scheduler_job_id: None,
            created_at: now,
            updated_at: now,
        };
        let (scheduler_status, scheduler_job_id) = schedule_expiry(&state, &account);
        account.scheduler_status = scheduler_status;
        account.scheduler_job_id = scheduler_job_id;
        state.auditors.insert(account.id, account.clone());
        state.scopes.insert(account.id, scopes.clone());
        state.log_auditor_action(
            &caller.actor,
            Some(account.id),
            &format!("auditors:{}", account.id),
            "auditors.created",
            true,
        );
        state.emit(
            sharing_events::AUDITOR_CREATED,
            json!({
                "auditor_id": account.id.to_string(),
                "user_id": account.user_id,
                "auditor_of": account.auditor_of,
                "expires_at": account.expires_at.to_rfc3339(),
            }),
        );
        state.notify(
            "auditors.created",
            &caller.actor,
            "Auditor account created",
            &format!(
                "Auditor \"{}\" was created by {} and expires at {}.",
                account.label,
                caller.actor,
                account.expires_at.to_rfc3339()
            ),
            Some(format!("/api/v1/auditors/{}", account.id)),
        );
        Ok(AuditorCreated {
            account: AuditorAccountView {
                account,
                scopes,
            },
            one_time_password,
        })
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn list_auditors(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AuditorAccount>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let params = query.into_inner();
        let now = Utc::now();
        let status_filter = params.filters.get("status").cloned();
        let auditor_of_filter = params.filters.get("auditor_of").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let mut items: Vec<AuditorAccount> = state
            .auditors
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|account| {
                if let Some(wanted) = &status_filter {
                    if account.effective_status(now).as_str() != wanted.as_str() {
                        return false;
                    }
                }
                if let Some(wanted) = &auditor_of_filter {
                    if &account.auditor_of != wanted {
                        return false;
                    }
                }
                if let Some(wanted) = &company_filter {
                    let has_company = state
                        .scopes
                        .get(&account.id)
                        .map(|entries| {
                            entries.iter().any(|entry| {
                                entry
                                    .company_id
                                    .is_some_and(|id| id.to_string() == *wanted)
                            })
                        })
                        .unwrap_or(false);
                    if !has_company {
                        return false;
                    }
                }
                if let Some(search) = &params.search {
                    let needle = search.to_lowercase();
                    if !account.label.to_lowercase().contains(&needle)
                        && !account.email.to_lowercase().contains(&needle)
                        && !account.user_id.to_lowercase().contains(&needle)
                    {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_auditor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AuditorAccountView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let account = load_auditor(&state, &path.into_inner())?;
        let scopes = state.scope_of(account.id).entries;
        Ok(AuditorAccountView { account, scopes })
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn revoke_auditor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RevokeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AuditorAccountView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_REVOKE)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let input = body.into_inner();
        let reason = match &input.reason {
            Some(value) => Some(require_text(value, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let id = parse_uuid(&path.into_inner(), "auditor")?;
        let now = Utc::now();
        let mut account = state
            .auditors
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found(format!("Auditor not found: {id}")))?;
        if account.status == AuditorStatus::Revoked {
            return Err(UnifiedError::conflict("Auditor account is already revoked"));
        }
        account.status = AuditorStatus::Revoked;
        account.revoked_at = Some(now);
        account.revoked_by = Some(caller.actor.clone());
        account.revoke_reason = reason.clone();
        account.updated_at = now;
        let account = account.clone();
        drop(account);
        // One-click revoke kills live sessions immediately.
        let killed = state.kill_sessions(id, &caller.actor);
        state.log_auditor_action(
            &caller.actor,
            Some(id),
            &format!("auditors:{id}"),
            "auditors.revoked",
            true,
        );
        state.emit(
            sharing_events::AUDITOR_REVOKED,
            json!({
                "auditor_id": id.to_string(),
                "revoked_by": caller.actor,
                "reason": reason,
                "sessions_killed": killed,
            }),
        );
        state.notify(
            "auditors.revoked",
            &caller.actor,
            "Auditor account revoked",
            &format!("Auditor account {id} was revoked by {}. {killed} live session(s) killed.", caller.actor),
            Some(format!("/api/v1/auditors/{id}")),
        );
        let account = load_auditor(&state, &id.to_string())?;
        let scopes = state.scope_of(id).entries;
        Ok(AuditorAccountView { account, scopes })
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn extend_auditor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ExtendAuditorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AuditorAccountView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_MANAGE)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let input = body.into_inner();
        if input.additional_hours < 1 {
            return Err(UnifiedError::bad_request("additional_hours must be at least 1"));
        }
        let settings = state.settings();
        let id = parse_uuid(&path.into_inner(), "auditor")?;
        let now = Utc::now();
        let max_expiry = now + chrono::Duration::hours(settings.max_auditor_ttl_hours);
        let mut account = state
            .auditors
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found(format!("Auditor not found: {id}")))?;
        if account.status == AuditorStatus::Revoked {
            return Err(UnifiedError::conflict("A revoked auditor account cannot be extended"));
        }
        let new_expiry = account.expires_at + chrono::Duration::hours(input.additional_hours);
        if new_expiry > max_expiry {
            return Err(UnifiedError::bad_request(format!(
                "Extension would exceed the maximum auditor lifetime (now + {} hours)",
                settings.max_auditor_ttl_hours
            )));
        }
        account.expires_at = new_expiry;
        account.updated_at = now;
        let account = account.clone();
        drop(account);
        state.log_auditor_action(
            &caller.actor,
            Some(id),
            &format!("auditors:{id}"),
            "auditors.extended",
            true,
        );
        // Re-queue the expiry job for the new expiry time.
        let account = load_auditor(&state, &id.to_string())?;
        let (scheduler_status, scheduler_job_id) = schedule_expiry(&state, &account);
        if let Some(mut stored) = state.auditors.get_mut(&id) {
            stored.scheduler_status = scheduler_status;
            stored.scheduler_job_id = scheduler_job_id;
        }
        let account = load_auditor(&state, &id.to_string())?;
        let scopes = state.scope_of(id).entries;
        Ok(AuditorAccountView { account, scopes })
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_auditor_sessions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AuditorSession>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let params = query.into_inner();
        let account = load_auditor(&state, &path.into_inner())?;
        let live_filter = params.filters.get("live").cloned();
        let mut items: Vec<AuditorSession> = state
            .sessions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|session| session.auditor_id == account.id)
            .filter(|session| match live_filter.as_deref() {
                Some("true") => session.is_live(account.is_usable(Utc::now())),
                Some("false") => !session.is_live(account.is_usable(Utc::now())),
                _ => true,
            })
            .collect();
        items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn revoke_auditor_session(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AuditorSession> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_REVOKE)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let (auditor_raw, session_raw) = path.into_inner();
        let auditor_id = parse_uuid(&auditor_raw, "auditor")?;
        let session_id = parse_uuid(&session_raw, "session")?;
        let account = load_auditor(&state, &auditor_raw)?;
        let _ = auditor_id;
        let now = Utc::now();
        let mut session = state
            .sessions
            .get_mut(&session_id)
            .filter(|entry| entry.auditor_id == account.id)
            .ok_or_else(|| UnifiedError::not_found(format!("Session not found: {session_id}")))?;
        if session.revoked_at.is_some() {
            return Err(UnifiedError::conflict("Session is already revoked"));
        }
        session.revoked_at = Some(now);
        session.revoked_by = Some(caller.actor.clone());
        let session = session.clone();
        drop(session);
        state.log_auditor_action(
            &caller.actor,
            Some(account.id),
            &format!("auditors:{}/sessions:{session_id}", account.id),
            "auditors.session.revoked",
            true,
        );
        state.emit(
            sharing_events::AUDITOR_SESSION_REVOKED,
            json!({
                "auditor_id": account.id.to_string(),
                "session_id": session_id.to_string(),
                "revoked_by": caller.actor,
            }),
        );
        Ok(state
            .sessions
            .get(&session_id)
            .map(|entry| entry.value().clone())
            .expect("session stored above"))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_auditor_activity(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AuditorActionLogEntry>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let params = query.into_inner();
        let account = load_auditor(&state, &path.into_inner())?;
        let action_filter = params.filters.get("action").cloned();
        let allowed_filter = params.filters.get("allowed").cloned();
        let mut items: Vec<AuditorActionLogEntry> = state
            .action_log
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.auditor_id == Some(account.id))
            .filter(|entry| match action_filter.as_deref() {
                Some(wanted) => entry.action == wanted,
                None => true,
            })
            .filter(|entry| match allowed_filter.as_deref() {
                Some("true") => entry.allowed,
                Some("false") => !entry.allowed,
                _ => true,
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_active_auditor_sessions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ActiveAuditorSessionView>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let params = query.into_inner();
        let now = Utc::now();
        let mut items: Vec<ActiveAuditorSessionView> = Vec::new();
        for session in state.sessions.iter().map(|entry| entry.value().clone()) {
            let Some(account) = state.auditors.get(&session.auditor_id).map(|entry| entry.value().clone()) else {
                continue;
            };
            if !session.is_live(account.is_usable(now)) {
                continue;
            }
            items.push(ActiveAuditorSessionView {
                session_id: session.id,
                auditor_id: account.id,
                user_id: account.user_id.clone(),
                label: account.label.clone(),
                auditor_of: account.auditor_of.clone(),
                started_at: session.started_at,
                last_seen_at: session.last_seen_at,
                expires_at: account.expires_at,
                scopes: state.scope_of(account.id).entries,
            });
        }
        items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_my_scope(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        // Self-only read for an auditor caller: no role capability
        // is checked here, because auditors hold no roles. The
        // caller must be an active auditor account; anyone else
        // gets a 403, and an expired or revoked auditor gets a 403
        // too (never a scope).
        let actor = req
            .headers()
            .get("x-user-id")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
        let account = state
            .auditor_for_user(&actor)
            .ok_or_else(|| UnifiedError::forbidden("Caller is not an auditor account"))?;
        if !account.is_usable(Utc::now()) {
            state.log_auditor_action(
                &actor,
                Some(account.id),
                &format!("auditors:{}/scope", account.id),
                "auditors.scope.read",
                false,
            );
            return Err(UnifiedError::forbidden(
                "Auditor account is expired or revoked",
            ));
        }
        state.log_auditor_action(
            &actor,
            Some(account.id),
            &format!("auditors:{}/scope", account.id),
            "auditors.scope.read",
            true,
        );
        Ok(json!({
            "auditor_id": account.id,
            "user_id": account.user_id,
            "label": account.label,
            "auditor_of": account.auditor_of,
            "expires_at": account.expires_at,
            "is_auditor": true,
            "read_only": true,
            "scopes": state.scope_of(account.id).entries,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn expire_sweep(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_MANAGE)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let expired = expire_due_auditors(&state, Utc::now());
        Ok(json!({
            "expired": expired.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
            "expired_count": expired.len(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Public link handlers
// ---------------------------------------------------------------------------

async fn create_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    body: web::Json<CreateLinkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PublicLinkCreated> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SHARING_CREATE)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let settings = state.settings();
        if !settings.public_links_enabled {
            return Err(UnifiedError::forbidden(
                "Public links are disabled by configuration",
            ));
        }
        let input = body.into_inner();
        if input.expires_in_hours < 1 || input.expires_in_hours > settings.max_link_ttl_hours {
            return Err(UnifiedError::bad_request(format!(
                "expires_in_hours must be between 1 and {}",
                settings.max_link_ttl_hours
            )));
        }
        if let Some(label) = &input.label {
            require_text(label, "label", MAX_LABEL_LEN)?;
        }
        // The kind and view must be registered by the owning module;
        // a typo or an unadopted kind can never mint a link.
        if !state.registry.is_known_kind(&input.scope_kind) {
            return Err(UnifiedError::bad_request(format!(
                "scope_kind \"{}\" is not a registered shareable kind",
                input.scope_kind
            )));
        }
        if !state.registry.is_known_view(&input.scope_kind, &input.scope_view) {
            return Err(UnifiedError::bad_request(format!(
                "scope_view \"{}\" is not a registered view of kind \"{}\"",
                input.scope_view, input.scope_kind
            )));
        }
        let scope = ShareScope::new(&input.scope_kind, input.scope_id, &input.scope_view)?;
        // SERVER-SIDE enforcement of the target's allow flag: the
        // owning module answers `sharing.allow_check` from the
        // record's own `allow_public_link` flag. A false answer is
        // a 403 — never just hidden in the UI.
        if !check_allow_public_link(&ctx.hooks, &scope, &caller.actor) {
            state.log_auditor_action(
                &caller.actor,
                None,
                &format!("sharing:{}", scope.to_scope_string()),
                "sharing.link.create_denied",
                false,
            );
            return Err(UnifiedError::forbidden(
                "This record does not allow public links (allow_public_link is false or unset)",
            )
            .with_metadata("scope", serde_json::json!(scope.to_scope_string())));
        }
        let now = Utc::now();
        let expires_at = now + chrono::Duration::hours(input.expires_in_hours);
        let (token, token_hash) = generate_link_token();
        let link = PublicLink {
            id: Uuid::now_v7(),
            token_hash: token_hash.clone(),
            scope: scope.clone(),
            created_by: caller.actor.clone(),
            expires_at,
            revoked_at: None,
            created_at: now,
        };
        state.links_by_hash.insert(token_hash, link.id);
        state.links.insert(link.id, link.clone());
        state.log_auditor_action(
            &caller.actor,
            None,
            &format!("sharing:{}", scope.to_scope_string()),
            "sharing.link.created",
            true,
        );
        state.emit(
            sharing_events::LINK_CREATED,
            json!({
                "link_id": link.id.to_string(),
                "scope_kind": scope.kind,
                "scope_id": scope.id.to_string(),
                "scope_view": scope.view,
                "created_by": caller.actor,
                "expires_at": expires_at.to_rfc3339(),
            }),
        );
        // HMAC-signed payload for the link, when a secret is
        // configured. A missing secret means no signed payload —
        // the link still works through the token.
        let secret_key = ctx
            .config
            .read()
            .expect("config lock poisoned")
            .app
            .secret_key
            .clone();
        let signed_payload = if secret_key.len() >= 32 {
            Some(sharing::sign_link_url(
                secret_key.as_bytes(),
                link.id,
                &scope,
                expires_at,
            ))
        } else {
            None
        };
        let url = format!(
            "/api/v1/sharing/public/{}?scope_kind={}&scope_id={}&scope_view={}",
            token, scope.kind, scope.id, scope.view
        );
        Ok(PublicLinkCreated {
            link: PublicLinkView::from(&link),
            token,
            url,
            signed_payload,
        })
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn list_links(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PublicLinkView>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SHARING_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let params = query.into_inner();
        let now = Utc::now();
        let kind_filter = params.filters.get("scope_kind").cloned();
        let created_by_filter = params.filters.get("created_by").cloned();
        let revoked_filter = params.filters.get("revoked").cloned();
        let expired_filter = params.filters.get("expired").cloned();
        let mut items: Vec<PublicLinkView> = state
            .links
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|link| match kind_filter.as_deref() {
                Some(wanted) => link.scope.kind == wanted,
                None => true,
            })
            .filter(|link| match created_by_filter.as_deref() {
                Some(wanted) => link.created_by == wanted,
                None => true,
            })
            .filter(|link| match revoked_filter.as_deref() {
                Some("true") => link.is_revoked(),
                Some("false") => !link.is_revoked(),
                _ => true,
            })
            .filter(|link| match expired_filter.as_deref() {
                Some("true") => link.is_expired(now),
                Some("false") => !link.is_expired(now),
                _ => true,
            })
            .map(|link| PublicLinkView::from(&link))
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PublicLinkView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SHARING_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let link = load_link(&state, &path.into_inner())?;
        Ok(PublicLinkView::from(&link))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn revoke_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RevokeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PublicLinkView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SHARING_REVOKE)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let input = body.into_inner();
        let reason = match &input.reason {
            Some(value) => Some(require_text(value, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let id = parse_uuid(&path.into_inner(), "public link")?;
        let now = Utc::now();
        let mut link = state
            .links
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found(format!("Public link not found: {id}")))?;
        if link.is_revoked() {
            return Err(UnifiedError::conflict("Public link is already revoked"));
        }
        link.revoked_at = Some(now);
        let link = link.clone();
        drop(link);
        state.log_auditor_action(
            &caller.actor,
            None,
            &format!("sharing:{}", id),
            "sharing.link.revoked",
            true,
        );
        state.emit(
            sharing_events::LINK_REVOKED,
            json!({
                "link_id": id.to_string(),
                "revoked_by": caller.actor,
                "reason": reason,
            }),
        );
        let link = load_link(&state, &id.to_string())?;
        Ok(PublicLinkView::from(&link))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_link_access_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PublicLinkAccessLogEntry>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SHARING_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let params = query.into_inner();
        let link = load_link(&state, &path.into_inner())?;
        let allowed_filter = params.filters.get("allowed").cloned();
        let reason_filter = params.filters.get("reason").cloned();
        let mut items: Vec<PublicLinkAccessLogEntry> = state
            .access_log
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.link_id == Some(link.id))
            .filter(|entry| match allowed_filter.as_deref() {
                Some("true") => entry.allowed,
                Some("false") => !entry.allowed,
                _ => true,
            })
            .filter(|entry| match reason_filter.as_deref() {
                Some(wanted) => entry.reason == wanted,
                None => true,
            })
            .collect();
        items.sort_by(|a, b| b.accessed_at.cmp(&a.accessed_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_access_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PublicLinkAccessLogEntry>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SHARING_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let params = query.into_inner();
        let allowed_filter = params.filters.get("allowed").cloned();
        let link_filter = params.filters.get("link_id").cloned();
        let reason_filter = params.filters.get("reason").cloned();
        let mut items: Vec<PublicLinkAccessLogEntry> = state
            .access_log
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| match allowed_filter.as_deref() {
                Some("true") => entry.allowed,
                Some("false") => !entry.allowed,
                _ => true,
            })
            .filter(|entry| match link_filter.as_deref() {
                Some(wanted) => entry
                    .link_id
                    .is_some_and(|id| id.to_string() == wanted),
                None => true,
            })
            .filter(|entry| match reason_filter.as_deref() {
                Some(wanted) => entry.reason == wanted,
                None => true,
            })
            .collect();
        items.sort_by(|a, b| b.accessed_at.cmp(&a.accessed_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_shareable_kinds(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<sharing::ShareableKind>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SHARING_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        Ok(state.registry.kinds())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// PUBLIC route: validate a link token and return its scope
/// descriptor. No identity headers, no capability check — the link
/// token IS the credential, and it grants read-only access to
/// exactly one scoped resource. Serves NO resource content: the
/// owning module serves content after validating the link through
/// `core::sharing::validate_public_link` itself. Gated by the API
/// Manager `ApiAccessRule` for this path (default deny — see the
/// module header). Every attempt is written to the access log and
/// a failed attempt feeds the hardening lockout.
async fn get_public_by_token(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PublicLinkQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let now = Utc::now();
    let ip = client_ip(&req);
    let agent = user_agent(&req);
    let result: UnifiedResult<Value> = (|| {
        let presented = path.into_inner();
        if presented.trim().is_empty() {
            return Err(UnifiedError::unauthorized("Link token is required"));
        }
        let query = query.into_inner();
        let scope_kind = query.scope_kind.ok_or_else(|| {
            UnifiedError::bad_request("scope_kind query parameter is required")
        })?;
        let scope_id_raw = query.scope_id.ok_or_else(|| {
            UnifiedError::bad_request("scope_id query parameter is required")
        })?;
        let scope_view = query.scope_view.ok_or_else(|| {
            UnifiedError::bad_request("scope_view query parameter is required")
        })?;
        let scope_id = parse_uuid(&scope_id_raw, "scope")?;
        let requested = ShareScope::new(&scope_kind, scope_id, &scope_view)?;
        // Hash lookup first (the raw token is never stored), then
        // the shared validator decides. The client-visible error is
        // always the same generic text, whatever the denial reason.
        let token_hash = hash_link_token(&presented);
        let link_id = state.links_by_hash.get(&token_hash).map(|entry| *entry.value());
        let link = link_id.and_then(|id| state.links.get(&id).map(|entry| entry.value().clone()));
        let outcome = validate_public_link(link.as_ref(), &presented, &requested, now);
        // Every attempt is logged — allowed AND denied — with only
        // hashes of the IP and user agent.
        let attempt = access_attempt(link_id, ip.as_deref(), agent.as_deref(), &outcome, now);
        let attempt_entry = PublicLinkAccessLogEntry {
            id: attempt.id,
            link_id: attempt.link_id,
            accessed_at: attempt.accessed_at,
            ip_hash: attempt.ip_hash,
            user_agent_hash: attempt.user_agent_hash,
            allowed: attempt.allowed,
            reason: attempt.reason.clone(),
        };
        state.access_log.insert(attempt_entry.id, attempt_entry);
        match &outcome {
            Ok(scope) => {
                let link = link.expect("validated link exists");
                record_successful_link_validation(
                    &crate::core::hardening::shared_state(),
                    ip.as_deref(),
                    link.id,
                );
                state.emit(
                    sharing_events::LINK_ACCESSED,
                    json!({
                        "link_id": link.id.to_string(),
                        "scope_kind": scope.kind,
                        "scope_id": scope.id.to_string(),
                        "scope_view": scope.view,
                    }),
                );
                Ok(json!({
                    "link_id": link.id,
                    "scope": scope,
                    "scope_string": scope.to_scope_string(),
                    "expires_at": link.expires_at,
                    "created_by": link.created_by,
                    "read_only": true,
                }))
            }
            Err(denial) => {
                record_failed_link_validation(
                    &crate::core::hardening::shared_state(),
                    ip.as_deref(),
                    link_id,
                );
                state.emit(
                    sharing_events::LINK_ACCESS_DENIED,
                    json!({
                        "link_id": link_id.map(|id| id.to_string()),
                        "reason": denial.as_str(),
                    }),
                );
                let _ = denial;
                Err(UnifiedError::unauthorized(
                    "This public link is not valid for the requested resource",
                ))
            }
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct AuditorsModule {
    state: AuditorsState,
}

impl AuditorsModule {
    pub fn new() -> Self {
        Self {
            state: AuditorsState::default(),
        }
    }
}

impl Default for AuditorsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AuditorsModule {
    fn id(&self) -> &'static str {
        "auditors"
    }

    fn name(&self) -> &'static str {
        "Auditors & Public Link Sharing"
    }

    fn description(&self) -> &'static str {
        "Temporary sandbox auditor accounts with scoped read-only access (time-limited, auto-expiring, one-click revoke, full action log) and public links that expose exactly one resource in one view (token hashed, HMAC-signed, server-side allow_public_link enforcement, full access log)."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "audit", "scheduler", "security"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "audit", "scheduler", "security"]
    }

    /// Auditors & sharing is core security infrastructure, not a
    /// sellable product: it is not declared available as a product.
    /// (The integrator may flip this if the product team decides
    /// auditor seats are sold separately; the category would then
    /// be "infrastructure".)
    fn available_as_product(&self) -> bool {
        false
    }

    fn product_category(&self) -> Option<&'static str> {
        None
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        // Register the shareable kinds before any link can be
        // minted: an unregistered kind fails closed by design.
        // A failure here fails module init, never a half-registered
        // registry.
        self.state.register_builtin_shareable_kinds()?;
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = Arc::new(self.state.clone());
        bus.register(
            EXPIRE_REQUESTED,
            "auditors",
            Arc::new(move |ctx: &mut HookContext| {
                on_expire_requested(Arc::as_ref(&state).clone(), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            SCOPE_CHECK,
            "auditors",
            Arc::new(move |ctx: &mut HookContext| {
                on_scope_check(Arc::as_ref(&state).clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::AUTH_LOGIN,
            "auditors",
            Arc::new(move |ctx: &mut HookContext| {
                on_auth_login(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/auditors")
                .app_data(state.clone())
                .route("", web::post().to(create_auditor))
                .route("", web::get().to(list_auditors))
                .route("/sessions/active", web::get().to(list_active_auditor_sessions))
                .route("/me/scope", web::get().to(get_my_scope))
                .route("/expire-sweep", web::post().to(expire_sweep))
                .route("/{id}", web::get().to(get_auditor))
                .route("/{id}/revoke", web::post().to(revoke_auditor))
                .route("/{id}/extend", web::post().to(extend_auditor))
                .route("/{id}/sessions", web::get().to(list_auditor_sessions))
                .route(
                    "/{id}/sessions/{session_id}/revoke",
                    web::post().to(revoke_auditor_session),
                )
                .route("/{id}/activity", web::get().to(list_auditor_activity)),
        );
        cfg.service(
            web::scope("/api/v1/sharing")
                .app_data(state)
                .route("/links", web::post().to(create_link))
                .route("/links", web::get().to(list_links))
                .route("/kinds", web::get().to(list_shareable_kinds))
                .route("/access-log", web::get().to(list_access_log))
                .route("/public/{token}", web::get().to(get_public_by_token))
                .route("/links/{id}", web::get().to(get_link))
                .route("/links/{id}/revoke", web::post().to(revoke_link))
                .route("/links/{id}/access-log", web::get().to(list_link_access_log)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/auditors", owner: "auditors", description: "Create a temp auditor account (expiry in hours required; one-time password shown once; expiry job queued as auditors.expire)." },
            RouteInfo { method: "GET", path: "/api/v1/auditors", owner: "auditors", description: "List auditor accounts (paginated, filter by status, auditor_of, company_id, search on label/email)." },
            RouteInfo { method: "GET", path: "/api/v1/auditors/{id}", owner: "auditors", description: "Get one auditor account with its scope." },
            RouteInfo { method: "POST", path: "/api/v1/auditors/{id}/revoke", owner: "auditors", description: "Revoke an auditor account immediately (kills live sessions)." },
            RouteInfo { method: "POST", path: "/api/v1/auditors/{id}/extend", owner: "auditors", description: "Extend an auditor account expiry (capped at now + max auditor lifetime)." },
            RouteInfo { method: "GET", path: "/api/v1/auditors/{id}/sessions", owner: "auditors", description: "List sessions of one auditor (paginated, filter by live)." },
            RouteInfo { method: "POST", path: "/api/v1/auditors/{id}/sessions/{session_id}/revoke", owner: "auditors", description: "Kill one auditor session." },
            RouteInfo { method: "GET", path: "/api/v1/auditors/{id}/activity", owner: "auditors", description: "Auditor action log (paginated, filter by action, allowed)." },
            RouteInfo { method: "GET", path: "/api/v1/auditors/sessions/active", owner: "auditors", description: "Active auditor sessions for the UI banner (who is watching, since when, what scope)." },
            RouteInfo { method: "GET", path: "/api/v1/auditors/me/scope", owner: "auditors", description: "The calling auditor's own scope (self only; no role capability — the caller must be an active auditor account)." },
            RouteInfo { method: "POST", path: "/api/v1/auditors/expire-sweep", owner: "auditors", description: "Run the auditor expiry sweep now (admin; same sweep the auditors.expire job runs)." },
            RouteInfo { method: "POST", path: "/api/v1/sharing/links", owner: "auditors", description: "Create a public link (target allow_public_link enforced server-side via sharing.allow_check; 403 when false)." },
            RouteInfo { method: "GET", path: "/api/v1/sharing/links", owner: "auditors", description: "List public links (paginated, filter by scope_kind, created_by, revoked, expired)." },
            RouteInfo { method: "GET", path: "/api/v1/sharing/links/{id}", owner: "auditors", description: "Get one public link (never the raw token)." },
            RouteInfo { method: "POST", path: "/api/v1/sharing/links/{id}/revoke", owner: "auditors", description: "Revoke a public link." },
            RouteInfo { method: "GET", path: "/api/v1/sharing/links/{id}/access-log", owner: "auditors", description: "Access log of one public link (paginated, filter by allowed, reason)." },
            RouteInfo { method: "GET", path: "/api/v1/sharing/access-log", owner: "auditors", description: "Access log of all public links (paginated, filter by allowed, link_id, reason)." },
            RouteInfo { method: "GET", path: "/api/v1/sharing/kinds", owner: "auditors", description: "List registered shareable resource kinds and their views." },
            RouteInfo { method: "GET", path: "/api/v1/sharing/public/{token}", owner: "auditors", description: "PUBLIC: validate a link token and return its scope descriptor (no auth; gated by the API Manager ApiAccessRule; serves no resource content)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AuditorsModule::new()));
}

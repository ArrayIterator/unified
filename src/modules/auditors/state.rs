// Auditor state and expiry sweep.

use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use unified_core::sharing::{
    sharing_events,
    AuditorScope,
    AuditorScopeEntry,
    PublicLink,
    ShareRegistry,
    SharingSettings,
};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// Shared state

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
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn settings(&self) -> SharingSettings {
        self.settings
            .read()
            .expect("sharing settings lock poisoned")
            .clone()
    }

    /// Register the shareable kinds this platform serves. Until a
    /// kind is registered, link creation for it fails closed by
    /// design - the registry rejects the kind before the owning
    /// module is ever asked. Registration is data only (kind name,
    /// views, description); the owning module still enforces
    /// `allow_public_link` and actor permission through its own
    /// `sharing.allow_check` answer, and no answer still means
    /// denied.
    ///
    /// Kinds registered here:
    ///   - `network_tools_run` / `["summary"]` - a network-tools
    ///     run result. The network_tools module answers
    ///     `sharing.allow_check` for it (owner-only), so links for
    ///     it work end to end once registered.
    ///   - `project` / `["board", "summary"]` - a project board or
    ///     summary (batch 5 contract). NOTE: the projects module
    ///     does NOT answer `sharing.allow_check` yet, so links for
    ///     this kind still fail closed at the check until the
    ///     projects module adds the answerer.
    /// Called from `AuditorsModule::init`, which is the one place
    /// that holds the registry handle; there is no hook for
    /// registering kinds.
    pub(crate) fn register_builtin_shareable_kinds(&self) -> UnifiedResult<()> {
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
    pub(crate) fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
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
    /// - allowed and denied - goes through here.
    pub(crate) fn log_auditor_action(
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
    pub(crate) fn scope_of(&self, auditor_id: Uuid) -> AuditorScope {
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
    pub(crate) fn auditor_for_user(&self, user_id: &str) -> Option<AuditorAccount> {
        self.auditors
            .iter()
            .map(|entry| entry.value().clone())
            .find(|account| account.user_id == user_id)
    }

    /// Kill every live session of one auditor (account revoke or
    /// expiry). Returns how many sessions were killed.
    pub(crate) fn kill_sessions(&self, auditor_id: Uuid, revoked_by: &str) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn account(
        id: uuid::Uuid,
        user_id: &str,
        status: AuditorStatus,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> AuditorAccount {
        AuditorAccount {
            id,
            user_id: user_id.to_string(),
            label: "Test auditor".to_string(),
            email: "auditor@example.test".to_string(),
            is_auditor: true,
            auditor_of: "company-1".to_string(),
            status,
            expires_at,
            password_hash: "hash".to_string(),
            revoked_at: None,
            revoked_by: None,
            revoke_reason: None,
            scheduler_status: "declined".to_string(),
            scheduler_job_id: None,
            created_at: expires_at,
            updated_at: expires_at,
        }
    }

    #[test]
    fn expire_due_auditors_expires_only_due_active_accounts() {
        let state = AuditorsState::default();
        let now = chrono::Utc::now();
        let due = account(
            uuid::Uuid::now_v7(),
            "due",
            AuditorStatus::Active,
            now - chrono::Duration::hours(1),
        );
        let not_yet = account(
            uuid::Uuid::now_v7(),
            "not-yet",
            AuditorStatus::Active,
            now + chrono::Duration::hours(1),
        );
        let already_revoked = account(
            uuid::Uuid::now_v7(),
            "revoked",
            AuditorStatus::Revoked,
            now - chrono::Duration::hours(1),
        );
        state.auditors.insert(due.id, due.clone());
        state.auditors.insert(not_yet.id, not_yet.clone());
        state.auditors.insert(already_revoked.id, already_revoked.clone());
        let expired = expire_due_auditors(&state, now);
        assert_eq!(expired, vec![due.id]);
        assert_eq!(
            state.auditors.get(&due.id).expect("due kept").status,
            AuditorStatus::Expired
        );
        assert_eq!(
            state.auditors.get(&not_yet.id).expect("not-yet kept").status,
            AuditorStatus::Active
        );
        assert_eq!(
            state.auditors.get(&already_revoked.id).expect("revoked kept").status,
            AuditorStatus::Revoked
        );
        // A second sweep finds nothing due.
        assert!(expire_due_auditors(&state, now).is_empty());
    }

    #[test]
    fn auditor_for_user_finds_by_user_id() {
        let state = AuditorsState::default();
        let now = chrono::Utc::now();
        let first = account(
            uuid::Uuid::now_v7(),
            "user-a",
            AuditorStatus::Active,
            now + chrono::Duration::hours(1),
        );
        let second = account(
            uuid::Uuid::now_v7(),
            "user-b",
            AuditorStatus::Active,
            now + chrono::Duration::hours(1),
        );
        state.auditors.insert(first.id, first.clone());
        state.auditors.insert(second.id, second.clone());
        assert_eq!(
            state.auditor_for_user("user-b").expect("found").id,
            second.id
        );
        assert!(state.auditor_for_user("nobody").is_none());
    }
}

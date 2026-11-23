// Tenant Destroy module — destroy request, audit, and tenant reference types.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::{
    drop_tenant_schema, is_valid_schema_name, schema_exists, schema_name,
    EVENT_TENANT_DESTROY_REQUESTED, EVENT_TENANT_DESTROYED, EVENT_TENANT_SUSPENDED,
};
use unified_core::db::DbPool;
use crate::modules::import_export::import_export_events;
use crate::modules::tenants::{
    begin_deprovision, finish_deprovision_as_deleted, set_export_reference, TenantsState,
    STATUS_DEPROVISIONING,
};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;
use super::*;

// ---------------------------------------------------------------------------
// Records.
// ---------------------------------------------------------------------------

/// One audit entry on a destroy request: who did what, and when.
/// The trail is append-only; entries are never edited or removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestroyAuditEntry {
    pub at: DateTime<Utc>,
    pub actor: String,
    pub action: String,
    pub detail: String,
}

/// One external-cleanup item: an area outside the tenant schema that
/// must be cleaned up by its owning module and confirmed before the
/// destroy request reaches `destroyed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalCleanupItem {
    pub kind: String,
    pub status: String,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub confirmed_by: Option<String>,
    pub note: Option<String>,
}

/// One destroy request: the full state of a tenant destroy flow.
/// The in-memory store is authoritative until the migration lands;
/// the table shape for the integrator is in the builder report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestroyRequest {
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// Tenant slug snapshot taken from the tenants module at request
    /// time. The typed confirmation is matched against this snapshot
    /// (and the live slug is re-checked at confirmation time).
    pub tenant_slug: String,
    pub status: String,
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
    pub reason: Option<String>,
    /// Proof of the mandatory export: the import/export job id.
    pub export_job_id: Option<Uuid>,
    pub export_requested_at: Option<DateTime<Utc>>,
    pub export_completed_at: Option<DateTime<Utc>>,
    pub export_download_url: Option<String>,
    /// How the export completion was proven: `completed_event` or
    /// `export_lookup`.
    pub export_verified_via: Option<String>,
    /// True when the `export.requested` event was emitted and at
    /// least one handler answered with an export job id. False means
    /// the export must be created through the import/export endpoint
    /// and attached manually (see the OPEN notes).
    pub export_request_claimed: bool,
    pub confirmation_attempts: u32,
    pub failed_confirmation_attempts: u32,
    pub confirmation_locked_until: Option<DateTime<Utc>>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub confirmed_by: Option<String>,
    pub second_approval_required: bool,
    pub second_approval_reason: Option<String>,
    pub second_approver: Option<String>,
    pub second_approved_at: Option<DateTime<Utc>>,
    pub grace_period_days: i64,
    pub grace_expires_at: Option<DateTime<Utc>>,
    pub suspended_at: Option<DateTime<Utc>>,
    pub destroyed_at: Option<DateTime<Utc>>,
    /// Schema name resolved at execution time, kept only as an audit
    /// snapshot. The drop itself always re-resolves the name from
    /// the tenant id through `schema_name()` at execution time.
    pub schema_name_snapshot: Option<String>,
    /// Whether the subdomain revoke request was emitted.
    pub subdomains_revoke_requested: bool,
    pub external_cleanup: Vec<ExternalCleanupItem>,
    pub audit_trail: Vec<DestroyAuditEntry>,
    pub cancel_reason: Option<String>,
    pub cancelled_by: Option<String>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl DestroyRequest {
    /// True when the request can no longer change state.
    pub fn is_terminal(&self) -> bool {
        TERMINAL_STATUSES.contains(&self.status.as_str())
    }

    /// True when the request can still be cancelled by an admin.
    pub fn is_cancellable(&self) -> bool {
        CANCELLABLE_STATUSES.contains(&self.status.as_str())
    }

    /// True when the grace period has expired (only meaningful in
    /// `suspended_grace`).
    pub fn grace_expired(&self) -> bool {
        match self.grace_expires_at {
            Some(expires_at) => Utc::now() >= expires_at,
            None => false,
        }
    }

    /// Seconds of grace remaining, if any (never negative).
    pub fn grace_remaining_seconds(&self) -> Option<i64> {
        self.grace_expires_at.map(|expires_at| {
            (expires_at - Utc::now()).num_seconds().max(0)
        })
    }

    /// True when every external-cleanup item is confirmed.
    pub fn external_cleanup_complete(&self) -> bool {
        !self.external_cleanup.is_empty()
            && self
                .external_cleanup
                .iter()
                .all(|item| item.status == "confirmed")
    }
}

/// Destroyed tenants have NO record of their own in this module.
/// The tenants row itself is the guard record: after the final
/// destroy it carries status `deleted` plus `destroyed_at` and the
/// export reference (moved there by the tenants module's
/// `finish_deprovision_as_deleted`). Keeping a second
/// destroyed-tenants store here would let the two drift, so the
/// only destroy-state queries in this file are `tenant.lookup`
/// (the tenants row) and `is_destroyed_tenant()` below.

/// The tenant record as answered by the tenants module through the
/// `tenant.lookup` hook query. This module never reads the tenants
/// tables directly; this shape is the contract B2 must answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantRef {
    pub id: Uuid,
    pub slug: String,
    pub status: String,
    pub created_at: Option<DateTime<Utc>>,
    pub data_size_bytes: Option<u64>,
    pub destroyed_at: Option<DateTime<Utc>>,
    pub export_reference: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with(status: &str) -> DestroyRequest {
        DestroyRequest {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            tenant_slug: "acme".to_string(),
            status: status.to_string(),
            requested_by: "admin".to_string(),
            requested_at: Utc::now(),
            reason: None,
            export_job_id: None,
            export_requested_at: None,
            export_completed_at: None,
            export_download_url: None,
            export_verified_via: None,
            export_request_claimed: false,
            confirmation_attempts: 0,
            failed_confirmation_attempts: 0,
            confirmation_locked_until: None,
            confirmed_at: None,
            confirmed_by: None,
            second_approval_required: false,
            second_approval_reason: None,
            second_approver: None,
            second_approved_at: None,
            grace_period_days: 30,
            grace_expires_at: None,
            suspended_at: None,
            destroyed_at: None,
            schema_name_snapshot: None,
            subdomains_revoke_requested: false,
            external_cleanup: Vec::new(),
            audit_trail: Vec::new(),
            cancel_reason: None,
            cancelled_by: None,
            cancelled_at: None,
            last_error: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn cleanup_item(kind: &str, status: &str) -> ExternalCleanupItem {
        ExternalCleanupItem {
            kind: kind.to_string(),
            status: status.to_string(),
            confirmed_at: None,
            confirmed_by: None,
            note: None,
        }
    }

    #[test]
    fn terminal_statuses_are_destroyed_and_cancelled() {
        assert!(request_with("destroyed").is_terminal());
        assert!(request_with("cancelled").is_terminal());
        assert!(!request_with("requested").is_terminal());
        assert!(!request_with("suspended_grace").is_terminal());
    }

    #[test]
    fn cancellable_statuses_exclude_terminal_and_cleanup_states() {
        assert!(request_with("requested").is_cancellable());
        assert!(request_with("export_pending").is_cancellable());
        assert!(request_with("suspended_grace").is_cancellable());
        assert!(!request_with("destroyed").is_cancellable());
        assert!(!request_with("cancelled").is_cancellable());
        assert!(!request_with("destroyed_pending_external_cleanup").is_cancellable());
    }

    #[test]
    fn grace_expiry_depends_on_the_expiry_timestamp() {
        let mut pending = request_with("suspended_grace");
        assert!(!pending.grace_expired());
        pending.grace_expires_at = Some(Utc::now() - chrono::Duration::hours(1));
        assert!(pending.grace_expired());
        pending.grace_expires_at = Some(Utc::now() + chrono::Duration::hours(1));
        assert!(!pending.grace_expired());
        assert!(pending.grace_remaining_seconds().unwrap() > 0);
    }

    #[test]
    fn external_cleanup_is_complete_only_when_every_item_confirms() {
        let mut request = request_with("destroyed_pending_external_cleanup");
        assert!(!request.external_cleanup_complete());
        request.external_cleanup = vec![
            cleanup_item("storage", "confirmed"),
            cleanup_item("backups", "confirmed"),
        ];
        assert!(request.external_cleanup_complete());
        request.external_cleanup.push(cleanup_item("search_index", "pending"));
        assert!(!request.external_cleanup_complete());
    }
}

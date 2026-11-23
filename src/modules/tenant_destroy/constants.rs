// Tenant Destroy module — permission, event, status, and cleanup constants.
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

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_REASON_LEN: usize = 2000;

pub(crate) const MAX_SLUG_LEN: usize = 100;

pub(crate) const MAX_NOTE_LEN: usize = 2000;

/// Capability names checked through the `role.check` hook.
pub const PERM_TENANTS_DESTROY: &str = "tenants:destroy";

pub const PERM_TENANTS_DESTROY_READ: &str = "tenants:destroy_read";

/// Scheduler kind offered to `scheduler.enqueue`, and the event the
/// scheduler emits when it dispatches the run.
pub const DESTROY_DUE_KIND: &str = "tenant.destroy_due";

pub const SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";

/// Event names this module emits (beyond the tenancy and
/// import/export constants imported above).
pub const EVENT_EXPORT_READY: &str = "tenant.destroy.export_ready";

pub const EVENT_CONFIRMATION_FAILED: &str = "tenant.destroy.confirmation_failed";

pub const EVENT_SECOND_APPROVAL_REQUIRED: &str = "tenant.destroy.second_approval_required";

pub const EVENT_TENANT_RESUMED: &str = "tenant.resumed";

pub const EVENT_DESTROY_CANCELLED: &str = "tenant.destroy.cancelled";

pub const EVENT_DESTROY_FAILED: &str = "tenant.destroy.failed";

pub const EVENT_SUBDOMAIN_RELEASE_REQUESTED: &str = "saas_domains.subdomain.release_requested";

/// Superseded: the control-plane tenant row is now moved to
/// `deleted` directly through the tenants module's
/// `finish_deprovision_as_deleted` helper (single source of truth),
/// so this module no longer asks for a soft-delete by event. The
/// constant is kept so existing emitters/listeners keep compiling;
/// no listener for it exists in the tenants module.
pub const EVENT_TENANT_SOFT_DELETE_REQUESTED: &str = "tenant.soft_delete_requested";

pub const EVENT_STORAGE_CLEANUP_REQUESTED: &str = "tenant.storage_cleanup_requested";

pub const EVENT_BACKUP_CLEANUP_REQUESTED: &str = "tenant.backup_cleanup_requested";

pub const EVENT_SEARCH_INDEX_CLEANUP_REQUESTED: &str = "tenant.search_index_cleanup_requested";

pub const EVENT_EXTERNAL_CLEANUP_COMPLETED: &str = "tenant.destroy.external_cleanup_completed";

/// Event names this module listens to.
pub const EVENT_STORAGE_CLEANUP_COMPLETED: &str = "tenant.storage_cleanup.completed";

pub const EVENT_BACKUP_CLEANUP_COMPLETED: &str = "tenant.backup_cleanup.completed";

pub const EVENT_SEARCH_INDEX_CLEANUP_COMPLETED: &str = "tenant.search_index_cleanup.completed";

pub const EVENT_DESTROYED_CHECK: &str = "tenant.destroyed.check";

/// Hook queries this module sends.
pub const QUERY_TENANT_LOOKUP: &str = "tenant.lookup";

pub const QUERY_EXPORT_LOOKUP: &str = "export.lookup";

/// Destroy-request lifecycle statuses. Closed vocabulary: the state
/// machine in the header is the only legal sequence, enforced by
/// every transition function in this file.
pub const STATUS_REQUESTED: &str = "requested";

pub const STATUS_EXPORT_PENDING: &str = "export_pending";

pub const STATUS_EXPORT_READY: &str = "export_ready";

pub const STATUS_AWAITING_CONFIRMATION: &str = "awaiting_confirmation";

pub const STATUS_AWAITING_SECOND_APPROVAL: &str = "awaiting_second_approval";

pub const STATUS_SUSPENDED_GRACE: &str = "suspended_grace";

pub const STATUS_DESTROYED_PENDING_EXTERNAL_CLEANUP: &str = "destroyed_pending_external_cleanup";

pub const STATUS_DESTROYED: &str = "destroyed";

pub const STATUS_CANCELLED: &str = "cancelled";

/// Statuses from which a destroy request can still be cancelled.
/// `destroyed_pending_external_cleanup` is deliberately absent: the
/// schema is already gone, so there is nothing left to resume.
pub(crate) const CANCELLABLE_STATUSES: &[&str] = &[
    STATUS_REQUESTED,
    STATUS_EXPORT_PENDING,
    STATUS_EXPORT_READY,
    STATUS_AWAITING_CONFIRMATION,
    STATUS_AWAITING_SECOND_APPROVAL,
    STATUS_SUSPENDED_GRACE,
];

/// Terminal statuses: no further transition is possible.
pub(crate) const TERMINAL_STATUSES: &[&str] = &[STATUS_DESTROYED, STATUS_CANCELLED];

/// External-cleanup kinds. Closed vocabulary from the brief: the
/// three areas that live outside the tenant schema. Each item is
/// tracked separately and must be confirmed before the request
/// reaches `destroyed`.
pub const CLEANUP_STORAGE: &str = "storage";

pub const CLEANUP_BACKUPS: &str = "backups";

pub const CLEANUP_SEARCH_INDEX: &str = "search_index";

pub const EXTERNAL_CLEANUP_KINDS: &[&str] = &[CLEANUP_STORAGE, CLEANUP_BACKUPS, CLEANUP_SEARCH_INDEX];

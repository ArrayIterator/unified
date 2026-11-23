// Tenant Destroy module — request bodies.
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
// Request bodies.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct RequestDestroyBody {
    pub(crate) tenant_id: Uuid,
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AttachExportProofBody {
    pub(crate) export_job_id: Uuid,
    pub(crate) download_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TypedConfirmationBody {
    pub(crate) typed_slug: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SecondApprovalBody {
    pub(crate) note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CancelDestroyBody {
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ConfirmExternalCleanupBody {
    pub(crate) item: String,
    pub(crate) note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateSettingsBody {
    pub(crate) grace_period_days: Option<i64>,
    pub(crate) second_approval_min_tenant_age_days: Option<Option<i64>>,
    pub(crate) second_approval_min_data_size_bytes: Option<Option<u64>>,
    pub(crate) max_failed_confirmations: Option<u32>,
    pub(crate) confirmation_lockout_minutes: Option<i64>,
    pub(crate) export_entity: Option<String>,
    pub(crate) export_pending_stale_days: Option<i64>,
}

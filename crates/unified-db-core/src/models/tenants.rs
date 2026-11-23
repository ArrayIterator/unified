// Diesel models for the tenants domain of the unified-db-core bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Tenants (batch 17; models derived from the `tenants.rs` header
// snippets — no model structs were given in the notes).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
// NOTE: the path must stay fully qualified to the TABLE module.
// `crate::schema::tenants` is the schema file-module (schema/tenants.rs),
// not the table: the `pub use tenants::*;` re-export in schema.rs
// cannot surface the inner `tenants` table over the file-module of
// the same name, so the table's columns live at
// `crate::schema::tenants::tenants`. Pointing `table_name` at the
// file-module (or the bare name `tenants`) makes every field fail
// with E0425 "cannot find type ... in module `tenants`".
#[diesel(table_name = crate::schema::tenants::tenants)]
pub struct Tenant {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub status: String,
    pub owner_user_id: Uuid,
    pub package_id: Option<Uuid>,
    pub preset_id: Option<Uuid>,
    pub plan_id: Option<String>,
    pub subdomain: Option<String>,
    pub subdomain_pending: bool,
    pub schema_name: String,
    pub suspended_at: Option<DateTime<Utc>>,
    pub suspended_reason: Option<String>,
    pub destroyed_at: Option<DateTime<Utc>>,
    pub export_reference: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = crate::schema::tenants::tenants)]
pub struct NewTenant {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub owner_user_id: Uuid,
    pub package_id: Option<Uuid>,
    pub preset_id: Option<Uuid>,
    pub plan_id: Option<String>,
    pub subdomain: Option<String>,
    pub schema_name: String,
    pub suspended_at: Option<DateTime<Utc>>,
    pub suspended_reason: Option<String>,
    pub destroyed_at: Option<DateTime<Utc>>,
    pub export_reference: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tenant_provisioning_steps)]
pub struct TenantProvisioningStep {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub step: String,
    pub status: String,
    pub attempt: i32,
    pub error: Option<String>,
    pub detail: Value,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tenant_provisioning_steps)]
pub struct NewTenantProvisioningStep {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub step: String,
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tenant_module_grants)]
pub struct TenantModuleGrant {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub module_id: String,
    pub granted_via: String,
    pub granted_via_id: Option<Uuid>,
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub granted_by: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tenant_module_grants)]
pub struct NewTenantModuleGrant {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub module_id: String,
    pub granted_via: String,
    pub granted_via_id: Option<Uuid>,
    pub expires_at: Option<DateTime<Utc>>,
    pub granted_by: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Tenant destroy requests (no migration yet; derived from the
// `DestroyRequest` struct in src/modules/tenant_destroy.rs).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tenant_destroy_requests)]
pub struct TenantDestroyRequest {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub tenant_slug: String,
    pub status: String,
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
    pub reason: Option<String>,
    pub export_job_id: Option<Uuid>,
    pub export_requested_at: Option<DateTime<Utc>>,
    pub export_completed_at: Option<DateTime<Utc>>,
    pub export_download_url: Option<String>,
    pub export_verified_via: Option<String>,
    pub export_request_claimed: bool,
    pub confirmation_attempts: i32,
    pub failed_confirmation_attempts: i32,
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
    pub schema_name_snapshot: Option<String>,
    pub subdomains_revoke_requested: bool,
    pub external_cleanup: Value,
    pub audit_trail: Value,
    pub cancel_reason: Option<String>,
    pub cancelled_by: Option<String>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tenant_destroy_requests)]
pub struct NewTenantDestroyRequest {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub tenant_slug: String,
    pub status: String,
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
    pub reason: Option<String>,
    pub export_job_id: Option<Uuid>,
    pub export_requested_at: Option<DateTime<Utc>>,
    pub export_completed_at: Option<DateTime<Utc>>,
    pub export_download_url: Option<String>,
    pub export_verified_via: Option<String>,
    pub export_request_claimed: bool,
    pub confirmation_attempts: i32,
    pub failed_confirmation_attempts: i32,
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
    pub schema_name_snapshot: Option<String>,
    pub subdomains_revoke_requested: bool,
    pub external_cleanup: Value,
    pub audit_trail: Value,
    pub cancel_reason: Option<String>,
    pub cancelled_by: Option<String>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

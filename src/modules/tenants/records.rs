//! tenants - records.

use unified_core::tenancy::{schema_name};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value};
use uuid::Uuid;
use super::{constants::*, vocabulary::*, events::*, settings::*, state::*, auth::*, helpers::*, provisioning::*, destroy::*, handlers::*, module::*, tenancy::*};



#[derive(Debug, Clone, Serialize, Deserialize)]
// Records.
/// One tenant: the control-plane record for a provisioned schema.
/// The tenant id is a uuidv7 and doubles as the company id the
/// rest of the platform keys tenant-scoped records by. The schema
/// name is derived from it and never typed by hand.
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


/// One recorded provisioning step: the idempotency record that
/// lets a half-finished provision resume instead of duplicating
/// work. One row per (tenant, step).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisioningStep {
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


/// One module grant: the durable record that a tenant was granted
/// a module, and through what. Revocation is a soft delete
/// (`revoked_at`); the row is kept as history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleGrant {
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


/// A module the grant step skipped, with the reason. Skips are
/// recorded, never silent: a non-entitled module is never
/// provisioned, and the caller can see exactly why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedModule {
    pub module_id: String,
    pub reason: String,
}


/// Provisioning status as returned by `GET /{id}/provision`: the
/// tenant, every recorded step, and the first step that still
/// needs work (`None` when provisioning finished).
#[derive(Debug, Clone, Serialize)]
pub struct ProvisionStatus {
    pub tenant: Tenant,
    pub steps: Vec<ProvisioningStep>,
    pub next_step: Option<String>,
    pub completed: bool,
}


/// The tenant context as resolved for one tenant: the values every
/// other module must carry to resolve tenant data (tenant id,
/// schema, entitlement scope). Served by `GET /{id}/context` so
/// operators and the integrator can verify the resolution path.
#[derive(Debug, Clone, Serialize)]
pub struct TenantContextView {
    pub tenant_id: Uuid,
    pub schema: String,
    pub status: String,
    pub suspended: bool,
    pub subdomain: Option<String>,
    pub subdomain_pending: bool,
    pub modules: Vec<String>,
}


/// Create-tenant request. The tenant id is never accepted from
/// the client: it is generated server-side. `module_ids` is an
/// explicit module list; `package_id` resolves its list through
/// the packages event; `preset_id` is recorded on the tenant and
/// its module list is resolved by the presets apply flow (see the
/// grant step - a preset id alone resolves no modules today).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTenantRequest {
    pub slug: String,
    pub display_name: String,
    pub owner_user_id: Uuid,
    pub package_id: Option<Uuid>,
    pub preset_id: Option<Uuid>,
    pub plan_id: Option<String>,
    pub module_ids: Option<Vec<String>>,
}


/// Suspend request: a reason is required so the audit trail can
/// explain every suspension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuspendTenantRequest {
    pub reason: String,
}


/// Grant-modules request: explicit module ids to grant on top of
/// the tenant's current grants. Every id is entitlement-checked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantModulesRequest {
    pub module_ids: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}


/// Revoke-grant request: a reason is required for the audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeGrantRequest {
    pub reason: String,
}

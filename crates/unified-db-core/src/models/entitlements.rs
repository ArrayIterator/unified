// Diesel models for the entitlements domain of the unified-db-core bounded context.
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
// Entitlements (batch 15 B1 notes §4). `EntitlementRow` /
// `NewEntitlementRow` are verbatim from the notes (the given
// `NewEntitlementRow` deliberately carries the DB-defaulted
// `status` / `granted_at`); the other five pairs follow the file
// convention (DB-defaulted columns omitted from `New*`). The notes
// name only a Queryable struct for `entitlement_settings` and
// `entitlement_module_activations`; `New*` structs are added per
// the file convention.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
// NOTE: the path must stay fully qualified to the TABLE module.
// `crate::schema::entitlements` is the schema file-module (schema/entitlements.rs),
// not the table: the `pub use entitlements::*;` re-export in schema.rs
// cannot surface the inner `entitlements` table over the file-module of
// the same name, so the table's columns live at
// `crate::schema::entitlements::entitlements`. Pointing `table_name` at the
// file-module (or the bare name `entitlements`) makes every field fail
// with E0425 "cannot find type ... in module `entitlements`".
#[diesel(table_name = crate::schema::entitlements::entitlements)]
pub struct EntitlementRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub source_ref: Option<String>,
    pub module_id: String,
    pub status: String,
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub granted_by: Option<String>,
    pub reason: Option<String>,
    pub grace_started_at: Option<DateTime<Utc>>,
    pub grace_ends_at: Option<DateTime<Utc>>,
    pub lapsed_at: Option<DateTime<Utc>>,
    pub lapse_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = crate::schema::entitlements::entitlements)]
pub struct NewEntitlementRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub source_ref: Option<String>,
    pub module_id: String,
    pub status: String,
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub granted_by: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = entitlement_history)]
pub struct EntitlementHistoryRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub entitlement_id: Option<Uuid>,
    pub company_id: String,
    pub module_id: String,
    pub action: String,
    pub actor_id: Option<String>,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = entitlement_history)]
pub struct NewEntitlementHistoryRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub entitlement_id: Option<Uuid>,
    pub company_id: String,
    pub module_id: String,
    pub action: String,
    pub actor_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = entitlement_denied_attempts)]
pub struct EntitlementDeniedAttemptRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub module_id: String,
    pub actor_id: Option<String>,
    pub reason: String,
    pub error_code: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = entitlement_denied_attempts)]
pub struct NewEntitlementDeniedAttemptRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub module_id: String,
    pub actor_id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = entitlement_package_changes)]
pub struct EntitlementPackageChangeRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub new_module_ids: Value,
    pub newly_entitled: Value,
    pub kept: Value,
    pub kept_via_other_source: Value,
    pub requires_confirmation: Value,
    pub disabled_module_ids: Value,
    pub status: String,
    pub requested_by: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = entitlement_package_changes)]
pub struct NewEntitlementPackageChangeRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub requested_by: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = entitlement_settings)]
pub struct EntitlementSettingsRow {
    pub id: String,
    pub grace_period_days: i64,
    pub read_only_grace_enabled: bool,
    pub default_deny: bool,
    pub updated_by: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = entitlement_settings)]
pub struct NewEntitlementSettingsRow {
    pub id: String,
    pub updated_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = entitlement_module_activations)]
pub struct EntitlementModuleActivationRow {
    pub tenant_id: String,
    pub company_id: String,
    pub module_id: String,
    pub enabled: bool,
    pub enabled_at: DateTime<Utc>,
    pub enabled_by: Option<String>,
    pub source_kind: Option<String>,
    pub source_id: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = entitlement_module_activations)]
pub struct NewEntitlementModuleActivationRow {
    pub tenant_id: String,
    pub company_id: String,
    pub module_id: String,
    pub enabled_by: Option<String>,
    pub source_kind: Option<String>,
    pub source_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = reference_id_settings)]
pub struct ReferenceIdSetting {
    pub tenant_id: Uuid,
    pub default_prefix: String,
    pub product_prefixes: Value,
    pub max_total_length: i32,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = reference_id_settings)]
pub struct NewReferenceIdSetting {
    pub tenant_id: Uuid,
}

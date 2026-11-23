// Diesel models for the saas_packages domain of the unified-db-commerce bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// SaaS packages (migration 000102_saas_packages).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saas_packages)]
pub struct SaasPackage {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub status: String,
    pub current_version: i32,
    pub product_id: Option<String>,
    pub preset_settings: Value,
    pub wizard_steps: Value,
    pub created_by: String,
    pub vendor_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saas_packages)]
pub struct NewSaasPackage {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub status: String,
    pub product_id: Option<String>,
    pub created_by: String,
    pub vendor_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saas_package_modules)]
pub struct SaasPackageModule {
    pub package_id: String,
    pub version: i32,
    pub module_id: String,
    pub required: bool,
    pub sort_order: i32,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saas_package_modules)]
pub struct NewSaasPackageModule {
    pub package_id: String,
    pub version: i32,
    pub module_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saas_package_purchases)]
pub struct SaasPackagePurchase {
    pub id: String,
    pub package_id: String,
    pub package_version: i32,
    pub buyer_id: String,
    pub company_id: String,
    pub status: String,
    pub subdomain: Option<String>,
    pub subdomain_pending: bool,
    pub order_id: Option<String>,
    pub subscription_id: Option<String>,
    pub product_id: Option<String>,
    pub enabled_modules: Value,
    pub grace_ends_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub provision_attempts: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saas_package_purchases)]
pub struct NewSaasPackagePurchase {
    pub id: String,
    pub package_id: String,
    pub package_version: i32,
    pub buyer_id: String,
    pub company_id: String,
    pub status: String,
    pub subdomain: Option<String>,
    pub order_id: Option<String>,
    pub subscription_id: Option<String>,
    pub product_id: Option<String>,
    pub grace_ends_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saas_package_provision_steps)]
pub struct SaasPackageProvisionStep {
    pub id: String,
    pub purchase_id: String,
    pub step: String,
    pub status: String,
    pub outcome: Option<String>,
    pub detail: Option<String>,
    pub attempt: i32,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saas_package_provision_steps)]
pub struct NewSaasPackageProvisionStep {
    pub id: String,
    pub purchase_id: String,
    pub step: String,
    pub status: String,
    pub outcome: Option<String>,
    pub detail: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

//! SaaS Domains - reservation, alias and product-subdomain records.
//!
//! Part of the `saas_domains` module split; see `src/modules/saas_domains.rs`.

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Records.
// ---------------------------------------------------------------------------

/// Reservation statuses: `reserved`, `active`, `released`, `expired`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainReservation {
    pub id: Uuid,
    pub domain: String,
    pub company_id: Uuid,
    pub reserved_by: String,
    pub reserved_until: DateTime<Utc>,
    pub status: String,
    pub site_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Alias statuses: `pending_verification`, `verified`, `suspended`,
/// `released`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainAlias {
    pub id: Uuid,
    pub domain: String,
    pub company_id: Uuid,
    pub target_type: String,
    pub target_value: String,
    pub status: String,
    pub site_id: Option<Uuid>,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Product-subdomain statuses: `pending_verification`, `active`,
/// `suspended`, `released`. A claim always starts
/// `pending_verification`; verification only records `verified_at`
/// (it never activates on its own); activation moves a verified
/// record to `active`. `released` starts the grace period
/// (`grace_until`); the label is free for reclaim only after the
/// grace period ends (computed from `grace_until`, never by
/// deleting the record).
pub const PRODUCT_SUBDOMAIN_STATUSES: [&str; 4] =
    ["pending_verification", "active", "suspended", "released"];

/// One purchased product's own subdomain. `product_id` is the
/// product identifier from the products module, or the sellable
/// module id itself (for example `academic` for the academic system
/// and `exams` for online exams); `package_id` names the purchased
/// the claim is package-scoped and is null for a standalone product
/// claim. `custom_domain` holds a verified whitelabel domain that
/// replaces this subdomain for the same product scope; it is only
/// ever set from the `whitelabel.domain.verified` event, never from
/// an API request. `site_id` is set when the product surface is
/// provisioned (the SaaS module owns site rows).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductSubdomain {
    pub id: Uuid,
    pub company_id: Uuid,
    pub product_id: String,
    pub package_id: Option<String>,
    pub label: String,
    pub host: String,
    pub custom_domain: Option<String>,
    pub site_id: Option<Uuid>,
    pub status: String,
    pub claimed_by: String,
    pub claimed_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    pub grace_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The product scope a request arrived under: the tenant, the
/// product (or package) the host is scoped to, and the record that
/// granted the scope. Handlers of product modules call
/// `require_product_scope` with their own module id to enforce it.
#[derive(Debug, Clone, Serialize)]
pub struct ProductScope {
    pub company_id: Uuid,
    pub product_id: String,
    pub package_id: Option<String>,
    pub product_subdomain_id: Uuid,
    pub host: String,
}

/// Subdomain allocation statuses: `active`, `pending_approval`,
/// `suspended`, `released`, `rejected`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubdomainAllocation {
    pub id: Uuid,
    pub label: String,
    pub hostname: String,
    pub company_id: Uuid,
    pub site_id: Option<Uuid>,
    pub status: String,
    pub approved_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

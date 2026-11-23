//! SaaS Domains - request body types.
//!
//! Part of the `saas_domains` module split; see `src/modules/saas_domains.rs`.

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct CreateReservationRequest {
    pub(crate) domain: String,
    pub(crate) company_id: Uuid,
    #[serde(default)]
    pub(crate) reserved_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub(crate) site_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProvisionReservationRequest {
    pub(crate) company_id: Uuid,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) theme_id: Option<String>,
    #[serde(default)]
    pub(crate) site_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateAliasRequest {
    pub(crate) domain: String,
    pub(crate) company_id: Uuid,
    pub(crate) target_type: String,
    pub(crate) target_value: String,
    #[serde(default)]
    pub(crate) site_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateAliasRequest {
    #[serde(default)]
    pub(crate) target_type: Option<String>,
    #[serde(default)]
    pub(crate) target_value: Option<String>,
    #[serde(default)]
    pub(crate) site_id: Option<Option<Uuid>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateSubdomainRequest {
    pub(crate) label: String,
    pub(crate) company_id: Uuid,
    #[serde(default)]
    pub(crate) site_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResolveQuery {
    pub(crate) host: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CheckProductSubdomainRequest {
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) product_id: Option<String>,
    #[serde(default)]
    pub(crate) company_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClaimProductSubdomainRequest {
    pub(crate) label: String,
    pub(crate) company_id: Uuid,
    pub(crate) product_id: String,
    #[serde(default)]
    pub(crate) package_id: Option<String>,
    #[serde(default)]
    pub(crate) site_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateSettingsRequest {
    #[serde(default)]
    pub(crate) wildcard_base: Option<String>,
    #[serde(default)]
    pub(crate) main_host: Option<String>,
    #[serde(default)]
    pub(crate) reserved_labels: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) label_pattern: Option<String>,
    #[serde(default)]
    pub(crate) max_subdomains_per_tenant: Option<u32>,
    #[serde(default)]
    pub(crate) require_subdomain_approval: Option<bool>,
    #[serde(default)]
    pub(crate) default_reservation_ttl_hours: Option<u32>,
    #[serde(default)]
    pub(crate) alias_target_types: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) max_product_subdomains_per_tenant: Option<u32>,
    #[serde(default)]
    pub(crate) max_product_subdomains_per_product: Option<u32>,
    #[serde(default)]
    pub(crate) default_product_subdomain_grace_days: Option<u32>,
    #[serde(default)]
    pub(crate) require_product_subdomain_approval: Option<bool>,
    #[serde(default)]
    pub(crate) product_subdomain_suggestion_suffixes: Option<Vec<String>>,
}

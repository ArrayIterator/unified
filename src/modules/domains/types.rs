// Domains module - shared types and request/response shapes.
//
// Split from `src/modules/domains.rs`; the module root re-exports the public API of this module.

use diesel::prelude::*;
use serde::Deserialize;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct CreateDomainRequest {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) company_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) site_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateDomainRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) is_active: Option<bool>,
    #[serde(default)]
    pub(crate) ssl_status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VerifyRequest {
    #[serde(default)]
    pub(crate) selector: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateSubdomainRequest {
    /// Subdomain label, for example `"app"` for `app.example.com`.
    pub(crate) name: String,
}

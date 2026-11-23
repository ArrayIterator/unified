// Plan request bodies.

use chrono::{DateTime, Utc};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Plan request bodies.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct CreatePlanRequest {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    pub(crate) quota_bytes: i64,
    #[serde(default)]
    pub(crate) max_files: Option<i64>,
    pub(crate) included_provider_kinds: Vec<String>,
    #[serde(default)]
    pub(crate) product_id: Option<String>,
    #[serde(default)]
    pub(crate) billing_cycle: Option<String>,
    #[serde(default)]
    pub(crate) is_public: Option<bool>,
    #[serde(default)]
    pub(crate) sort_order: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdatePlanRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) quota_bytes: Option<i64>,
    #[serde(default)]
    pub(crate) max_files: Option<i64>,
    #[serde(default)]
    pub(crate) included_provider_kinds: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) product_id: Option<Option<String>>,
    #[serde(default)]
    pub(crate) billing_cycle: Option<Option<String>>,
    #[serde(default)]
    pub(crate) is_public: Option<bool>,
    #[serde(default)]
    pub(crate) sort_order: Option<i64>,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AssignPlanRequest {
    /// Company the plan is assigned to (optional for personal plans).
    #[serde(default)]
    pub(crate) company_id: Option<String>,
    /// User the plan is assigned to (required: every assignment has
    /// an owning user, even a company-scoped one).
    pub(crate) user_id: String,
    #[serde(default)]
    pub(crate) expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CancelAssignmentRequest {
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReportMeteringRequest {
    #[serde(default)]
    pub(crate) company_id: Option<String>,
    pub(crate) user_id: String,
    /// `YYYY-MM`. Defaults to the current month.
    #[serde(default)]
    pub(crate) period: Option<String>,
}

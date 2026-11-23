// Types and request bodies.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Configurable settings (admin-editable; defaults are defaults only)
// ---------------------------------------------------------------------------

/// All behaviour-shaping lists and defaults of this module. An
/// administrator edits these through `PUT /settings`; every
/// validation and default in this file reads the live value, so
/// changing a list here changes what the module accepts without
/// any code change. The values below are CONFIGURABLE DEFAULTS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpensesSettings {
    /// Allowed expense kinds (for example standard / mileage /
    /// per_diem). Validated against this list on every write.
    pub expense_kinds: Vec<String>,
    /// Currency codes the module accepts, as ISO codes.
    pub supported_currencies: Vec<String>,
    /// Default currency for new reports and policies.
    pub default_currency: String,
    /// Default mileage rate in minor units per distance unit,
    /// used when the report's policy sets no rate.
    pub default_mileage_rate_minor: i64,
    /// Distance unit the default mileage rate is priced in
    /// (for example km or mi). Policies may set their own unit.
    pub default_mileage_unit: String,
    /// Default per-diem rate in minor units per day, used when
    /// the report's policy sets no rate.
    pub default_per_diem_rate_minor: i64,
    /// Warning level for a report total in minor units: a report
    /// over this total is flagged in the violations analytics.
    /// This is a flag only — it never blocks submission.
    pub report_total_warning_minor: i64,
}

impl Default for ExpensesSettings {
    fn default() -> Self {
        Self {
            expense_kinds: vec![
                "standard".to_string(),
                "mileage".to_string(),
                "per_diem".to_string(),
            ],
            supported_currencies: vec![
                "USD".to_string(),
                "EUR".to_string(),
                "GBP".to_string(),
            ],
            default_currency: "USD".to_string(),
            default_mileage_rate_minor: 50,
            default_mileage_unit: "km".to_string(),
            default_per_diem_rate_minor: 5000,
            report_total_warning_minor: 100_000,
        }
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One expense category. Admin-editable: the receipt requirement
/// and both limits are data, so a category can be tightened or
/// loosened without any code change. `receipt_required_above_minor`
/// is the threshold at or above which a receipt is required when
/// `receipt_required` is true; `None` means every expense in the
/// category needs a receipt. Limits of `None` mean no limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseCategory {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    pub receipt_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_required_above_minor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_expense_limit_minor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_day_limit_minor: Option<i64>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One expense policy: the per-company rules for reports. The
/// mileage and per-diem rates are policy data; the approval chain
/// is an opaque reference to a chain owned by the approvals flow
/// (this module never interprets it). `max_report_total_minor`
/// flags a report as over-limit; it never silently approves or
/// rejects anything — the flag is shown to approvers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpensePolicy {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    pub currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mileage_rate_minor: Option<i64>,
    pub mileage_unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_diem_rate_minor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_report_total_minor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_chain_id: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One expense report. `total_minor` is maintained server-side
/// from the report's expense lines and is never accepted from
/// the client. `violations` is the report-level flag list
/// (currently the over-total flag); line-level flags live on
/// each expense. History is append-only: a rejected report is
/// never rewritten, it is withdrawn to draft and resubmitted as
/// a new submission with a fresh approval request id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseReport {
    pub id: Uuid,
    pub employee_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_id: Option<Uuid>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub status: String,
    pub currency: String,
    pub total_minor: i64,
    pub violations: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_request_id: Option<String>,
    /// The approvals module's `request_id` for this report's
    /// current submission (the generic `approval.requested`
    /// correlation id). Stored from the synchronous answer to the
    /// generic request on submit; `None` when the approvals module
    /// did not accept the request (no active definition, or not
    /// registered). Cleared on withdraw, like `approval_request_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generic_approval_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reimbursement_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One expense line inside a report. `amount_minor` is the line
/// amount in the report currency. For `mileage` lines the amount
/// is computed server-side as distance times the policy rate and
/// the rate used is recorded in `rate_minor_applied`; for
/// `per_diem` lines the amount is days times the policy rate.
/// `project_id` and `client_id` are opaque ids only — the
/// projects and CRM modules own the records they point at, and
/// no project or client data is duplicated here. `violations`
/// holds the policy flags for this line, recomputed on every
/// write and on submit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Expense {
    pub id: Uuid,
    pub report_id: Uuid,
    pub kind: String,
    pub category_id: Uuid,
    pub expense_date: NaiveDate,
    pub amount_minor: i64,
    pub currency: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_minor_applied: Option<i64>,
    pub violations: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One receipt reference. The bytes live in the documents /
/// storage modules: `storage_target` plus `storage_key` is the
/// reference this module stores, exactly like other modules
/// store media references. Nothing here ever holds file bytes.
/// `verified` is false until the storage side confirms the
/// object exists: attaching a receipt records an unverified
/// claim, and only a verified receipt clears the
/// `receipt_missing` violation (see `compute_violations` and
/// the `storage.object.stored` listener). A receipt is verified
/// when the storage manager emits `storage.object.stored` for
/// its key, and un-verified again when the object is deleted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseReceipt {
    pub id: Uuid,
    pub expense_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub uploaded_by: String,
    #[serde(default)]
    pub verified: bool,
    pub created_at: DateTime<Utc>,
}

/// One reimbursement: the pay-out for an approved report. The
/// billing flow owns the money movement; this row tracks the
/// request and its outcome. `payable_reference` is the opaque
/// reference handed to billing so the two sides can be matched.
/// A failed reimbursement is retried, never rewritten: `attempts`
/// counts the requests and `failure_reason` records the last one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reimbursement {
    pub id: Uuid,
    pub report_id: Uuid,
    pub employee_id: String,
    pub amount_minor: i64,
    pub currency: String,
    pub status: String,
    pub payable_reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paid_at: Option<DateTime<Utc>>,
    pub attempts: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One refund or adjustment against a report. Adjustments are
/// separate entries that adjust the net spend in analytics; they
/// never mutate the report total or any expense line, so the
/// submitted history stays intact. What an adjustment DOES change
/// is the payable: when the report has a reimbursement that is
/// still pending, the adjustment reduces that reimbursement's
/// amount (floored at zero); when the reimbursement is already
/// paid there is no payable left to reduce, so the adjustment
/// stays record-only and is netted in spend analytics only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseAdjustment {
    pub id: Uuid,
    pub report_id: Uuid,
    pub kind: String,
    pub amount_minor: i64,
    pub currency: String,
    pub reason: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// One stored exchange rate. Rates are stored data with a source
/// label (for example a manual entry or a named provider feed);
/// this module never fetches a rate live. `rate` is quoted as
/// target minor units per one base minor unit, so a conversion is
/// `round(amount_minor * rate)`; rates for a pair are stored in
/// that quoted direction and the inverse pair is derived as
/// `1 / rate` when only the reverse direction is stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrencyRate {
    pub id: Uuid,
    pub base_currency: String,
    pub target_currency: String,
    pub rate: f64,
    pub source: String,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateCategoryRequest {
    pub name: String,
    pub code: String,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub receipt_required: Option<bool>,
    #[serde(default)]
    pub receipt_required_above_minor: Option<i64>,
    #[serde(default)]
    pub per_expense_limit_minor: Option<i64>,
    #[serde(default)]
    pub per_day_limit_minor: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCategoryRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub receipt_required: Option<bool>,
    #[serde(default)]
    pub receipt_required_above_minor: Option<i64>,
    #[serde(default)]
    pub per_expense_limit_minor: Option<i64>,
    #[serde(default)]
    pub per_day_limit_minor: Option<i64>,
    #[serde(default)]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePolicyRequest {
    pub name: String,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub mileage_rate_minor: Option<i64>,
    #[serde(default)]
    pub mileage_unit: Option<String>,
    #[serde(default)]
    pub per_diem_rate_minor: Option<i64>,
    #[serde(default)]
    pub max_report_total_minor: Option<i64>,
    #[serde(default)]
    pub approval_chain_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePolicyRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub mileage_rate_minor: Option<i64>,
    #[serde(default)]
    pub mileage_unit: Option<String>,
    #[serde(default)]
    pub per_diem_rate_minor: Option<i64>,
    #[serde(default)]
    pub max_report_total_minor: Option<i64>,
    #[serde(default)]
    pub approval_chain_id: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateReportRequest {
    pub title: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub policy_id: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateReportRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub policy_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateExpenseRequest {
    pub kind: String,
    pub category_id: String,
    /// Date as `YYYY-MM-DD`.
    pub expense_date: String,
    /// Amount in minor units. Ignored for `mileage` and `per_diem`
    /// lines: those amounts are computed server-side from the
    /// policy rate, so a client-supplied amount can never override
    /// the rate.
    #[serde(default)]
    pub amount_minor: Option<i64>,
    #[serde(default)]
    pub currency: Option<String>,
    pub description: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub distance: Option<f64>,
    #[serde(default)]
    pub days: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateExpenseRequest {
    #[serde(default)]
    pub category_id: Option<String>,
    #[serde(default)]
    pub expense_date: Option<String>,
    #[serde(default)]
    pub amount_minor: Option<i64>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub distance: Option<f64>,
    #[serde(default)]
    pub days: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct AttachReceiptRequest {
    pub storage_target: String,
    pub storage_key: String,
    pub file_name: String,
    #[serde(default)]
    pub content_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAdjustmentRequest {
    pub kind: String,
    pub amount_minor: i64,
    #[serde(default)]
    pub currency: Option<String>,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateCurrencyRateRequest {
    pub base_currency: String,
    pub target_currency: String,
    pub rate: f64,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ConvertRequest {
    pub amount_minor: i64,
    pub from_currency: String,
    pub to_currency: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default)]
    pub expense_kinds: Option<Vec<String>>,
    #[serde(default)]
    pub supported_currencies: Option<Vec<String>>,
    #[serde(default)]
    pub default_currency: Option<String>,
    #[serde(default)]
    pub default_mileage_rate_minor: Option<i64>,
    #[serde(default)]
    pub default_mileage_unit: Option<String>,
    #[serde(default)]
    pub default_per_diem_rate_minor: Option<i64>,
    #[serde(default)]
    pub report_total_warning_minor: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SpendQuery {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub group_by: Option<String>,
}

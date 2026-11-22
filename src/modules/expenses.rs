// Expenses module — employee expense reports with categories,
// policies, mileage and per-diem entries, receipt references,
// approval through the approvals flow (events only), and
// reimbursement through the billing flow (events only).
//
// Routes live under `/api/v1/expenses`:
//
//   Categories (admin-editable):
//     GET/POST /categories — list / create a category.
//     GET/PATCH/DELETE /categories/{id}.
//   Policies (per-company rules):
//     GET/POST /policies — list / create a policy.
//     GET/PATCH/DELETE /policies/{id}.
//   Reports:
//     GET/POST /reports — list / create a report (draft).
//     GET/PATCH/DELETE /reports/{id}.
//     POST /reports/{id}/submit — draft -> submitted (requests approval).
//     POST /reports/{id}/withdraw — submitted -> draft.
//     GET  /reports/{id}/export?format=csv|json — the report as CSV or JSON.
//   Expenses (lines inside a report):
//     GET/POST /reports/{id}/expenses — list / add a line.
//     GET/PATCH/DELETE /expenses/{id}.
//     POST /expenses/{id}/receipts — attach a receipt reference.
//     DELETE /expenses/{id}/receipts/{receipt_id} — remove a reference.
//   Reimbursements:
//     GET  /reimbursements — list (own only unless manager).
//     GET  /reimbursements/{id}.
//     POST /reimbursements/{id}/retry — re-request a failed reimbursement.
//   Adjustments and refunds (separate entries, never history edits):
//     GET/POST /reports/{id}/adjustments.
//   Currency rates (stored rates only — never guessed):
//     GET/POST /currency-rates — list / store a rate.
//     GET/DELETE /currency-rates/{id}.
//     POST /currency-rates/convert — convert with a stored rate.
//   Analytics:
//     GET  /analytics/spend — spend per category / employee / period.
//     GET  /analytics/pending-approvals — reports waiting on a decision.
//     GET  /analytics/reimbursement-backlog — pending / failed pay-outs.
//     GET  /analytics/violations — flagged expenses and over-limit reports.
//   Settings:
//     GET/PUT /settings — the configurable defaults (manager).
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory category / policy / report / expense / receipt /
// reimbursement / adjustment / currency-rate management; a report
// status machine (draft -> submitted -> approved / rejected ->
// reimbursed) with checked transitions; mileage amounts computed
// server-side from distance times the policy rate and per-diem
// amounts computed from days times the policy rate; policy
// violation flags (over per-expense limit, over per-day limit,
// receipt missing) that are recorded on the expense and surfaced
// to approvers, never silently auto-approved; receipt references
// only (storage references handed to the documents / storage
// modules — no receipt bytes live here); currency conversion
// from stored rates only, with the calculator module consulted
// through its `calculator.rate_requested` event when this module
// holds no stored rate for the pair. What is NOT built here:
// durable storage (the DashMap stores are authoritative;
// migration `2026-11-22-000083_expenses` is the persistence
// target); an approval engine of any kind — approval decisions
// arrive only as `expenses.approval.decided` events from the
// approvals flow, and this module never decides an approval
// itself; any payment — reimbursement pay-outs are requested
// from the billing flow with `expenses.reimbursement.requested`
// and only ever confirmed through its paid / failed events; any
// project or client data — project and client links are opaque
// ids only, resolved and owned by the projects / CRM modules.
//
// CONFIG-DRIVEN — READ THIS FIRST. Nothing behavioural is
// hard-coded: the allowed expense kinds, the supported
// currencies, the default currency, the default mileage and
// per-diem rates, and the maximum report total warning level
// live in `ExpensesSettings`, editable by an administrator
// through `PUT /settings`. Categories carry their own receipt
// requirement, receipt threshold, per-expense limit, and
// per-day limit; policies carry their own mileage rate,
// per-diem rate, report total limit, and approval chain
// reference. The values in `ExpensesSettings::default` are
// CONFIGURABLE DEFAULTS only — an admin change applies to
// every validation and default that reads them, and no code
// path consults a baked-in list instead. The one deliberate
// exception is the report status vocabulary itself
// (`draft`, `submitted`, `approved`, `rejected`,
// `reimbursed`): it is the state machine of this module, and
// the allowed transitions are the transition table in
// `allowed_report_transitions`, not a settings list.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not
// allowed). Employees see only their own reports, expenses, and
// reimbursements; managers (the manage capability) see every
// company row they are allowed to see through the same check.
//
// Approval contract (the approvals flow owns the decision):
//   1. `POST /reports/{id}/submit` moves a draft report to
//      `submitted`, stores a fresh `approval_request_id`, and
//      emits `expenses.approval.requested` with
//      `{approval_request_id, report_id, employee_id,
//      company_id, total_minor, currency, approval_chain_id,
//      violations}`. It does NOT approve anything. The same
//      submit ALSO emits the generic `approval.requested`
//      (module `expenses`, record_type `expense_report`,
//      record_id = the report id, requester_id = the employee,
//      summary, amount_minor, currency, company_id) so the
//      generic approvals module can run its workflow; the
//      approvals `request_id` from its synchronous answer is
//      stored on the report as `generic_approval_request_id`
//      and is the correlation id for the generic decision.
//      When the approvals module has no active definition for
//      `expenses` / `expense_report` (or is not registered) the
//      submit still succeeds and the report stays `submitted`:
//      a missing definition never auto-approves or auto-rejects.
//   2. The approvals flow runs its chain and emits
//      `approval.decided` with `{request_id, decision,
//      decided_by, module, record_type, record_id,
//      requester_id}`. This module listens to that event,
//      correlates it by the stored `generic_approval_request_id`
//      (falling back to the (`module`, `record_type`,
//      `record_id`) triple for requests created before the
//      bridge, or by an admin through the approvals API), and
//      moves the report to `approved` or `rejected`.
//      `changes_requested` leaves the report `submitted` — the
//      requester withdraws it to draft to revise it. The
//      outcome is then re-emitted as `expenses.approval.decided`
//      with `{approval_request_id, report_id, decision,
//      decided_by, reason}` for any listener still on the
//      expenses-specific name. A decision for an unknown request
//      id, a mismatched report, or a report that is no longer
//      `submitted` is ignored as a safe no-op (the request may
//      have been withdrawn). The expenses-specific
//      `expenses.approval.decided` event is still accepted
//      directly with `{approval_request_id, report_id, decision,
//      decided_by, reason}` where `decision` is `approved`
//      or `rejected`, for emitters that speak the old name.
//   3. On approval this module creates a reimbursement row in
//      `pending` and emits `expenses.reimbursement.requested`
//      with `{reimbursement_id, report_id, employee_id,
//      amount_minor, currency, payable_reference}` for the
//      billing flow. It does NOT move any money.
//   4. The billing flow confirms the pay-out by emitting
//      `expenses.reimbursement.paid` with
//      `{reimbursement_id, paid_at}` (or by emitting the
//      generic `payment.captured` carrying the same
//      `reimbursement_id`), and this module moves the
//      reimbursement to `paid` and the report to
//      `reimbursed`. A failure is confirmed with
//      `expenses.reimbursement.failed` (or `payment.failed`
//      carrying the `reimbursement_id`) with
//      `{reimbursement_id, reason}`; the report stays
//      `approved` and the reimbursement can be retried.
//   5. `POST /reports/{id}/withdraw` moves a `submitted`
//      report back to `draft` and emits
//      `expenses.approval.cancelled` with
//      `{approval_request_id, report_id}` so a pending chain
//      can stand down. There is no second approval engine in
//      this module: no endpoint approves or rejects a report.
//
// Currency contract: conversion uses a stored rate only. This
// module first consults its own stored rates, then emits
// `calculator.rate_requested` with `{base_currency,
// target_currency}` and uses the first rate the calculator
// module answers with (its stored rate, if any). When neither
// side holds a rate the conversion fails with 422 — a rate is
// never guessed, interpolated, or fetched live from any
// provider by this module.
//
// Events emitted on the hook bus:
//   - `expenses.category.created`, `expenses.category.updated`,
//     `expenses.category.deleted`
//   - `expenses.policy.created`, `expenses.policy.updated`,
//     `expenses.policy.deleted`
//   - `expenses.report.created`, `expenses.report.updated`,
//     `expenses.report.deleted`, `expenses.report.submitted`,
//     `expenses.report.withdrawn`, `expenses.report.approved`,
//     `expenses.report.rejected`, `expenses.report.reimbursed`
//   - `expenses.approval.requested`, `expenses.approval.cancelled`
//   - `expenses.expense.created`, `expenses.expense.updated`,
//     `expenses.expense.deleted`
//   - `expenses.receipt.attached`, `expenses.receipt.removed`
//   - `expenses.reimbursement.requested`,
//     `expenses.reimbursement.retried`
//   - `expenses.adjustment.created`
//   - `expenses.currency_rate.created`,
//     `expenses.currency_rate.deleted`
//   - `expenses.settings.updated`
//   - `approval.requested` (generic approvals contract; see the
//     contract above) — emitted on submit alongside
//     `expenses.approval.requested`.
//   - `calculator.rate_requested` (rate lookup; answered by the
//     calculator module when it holds a stored rate)
//   - `notification.requested` (approval decision and
//     reimbursement outcome, addressed to the employee)
// Events this module listens to:
//   - `approval.decided` — generic approval decision from the
//     approvals flow (see the contract above); correlated by the
//     stored approvals `request_id`, then re-emitted as
//     `expenses.approval.decided`.
//   - `expenses.approval.decided` — approval decision from the
//     approvals flow (see the contract above).
//   - `expenses.reimbursement.paid` — reimbursement confirmed
//     paid by the billing flow.
//   - `expenses.reimbursement.failed` — reimbursement failed in
//     the billing flow.
//   - `payment.captured` — treated as a reimbursement payment
//     only when the payload carries a `reimbursement_id` this
//     module owns; anything else is ignored.
//   - `payment.failed` — treated as a reimbursement failure
//     only when the payload carries a `reimbursement_id` this
//     module owns; anything else is ignored.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::tenancy::TenantContext as TenancyContext;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_CODE_LEN: usize = 64;
const MAX_SLUG_LEN: usize = 200;
const MAX_REASON_LEN: usize = 1000;
const MAX_REF_LEN: usize = 512;
const MAX_CURRENCY_LEN: usize = 3;
/// Maximum amount in minor units accepted for one expense or one
/// report total. This is an abuse guard on the input shape, not a
/// business limit: business limits live in category and policy
/// data and are checked separately as violations.
const MAX_AMOUNT_MINOR: i64 = 1_000_000_000_000;
/// Maximum distance (in the policy mileage unit) accepted for one
/// mileage entry, and maximum days accepted for one per-diem entry.
const MAX_MILEAGE_DISTANCE: f64 = 100_000.0;
const MAX_PER_DIEM_DAYS: i64 = 365;

/// Capability names checked through the `role.check` hook.
const PERM_EXPENSES_VIEW: &str = "expenses.view";
const PERM_EXPENSES_SUBMIT: &str = "expenses.submit";
const PERM_EXPENSES_MANAGE: &str = "expenses.manage";

/// Report lifecycle statuses. The strings are the stored status
/// values; the transition table in `allowed_report_transitions`
/// is the only place that decides which move is legal.
const REPORT_DRAFT: &str = "draft";
const REPORT_SUBMITTED: &str = "submitted";
const REPORT_APPROVED: &str = "approved";
const REPORT_REJECTED: &str = "rejected";
const REPORT_REIMBURSED: &str = "reimbursed";

/// Reimbursement statuses.
const REIMB_PENDING: &str = "pending";
const REIMB_PAID: &str = "paid";
const REIMB_FAILED: &str = "failed";

/// Adjustment kinds.
const ADJUST_REFUND: &str = "refund";
const ADJUST_ADJUSTMENT: &str = "adjustment";

/// Violation flag names recorded on an expense.
const VIOLATION_OVER_PER_EXPENSE_LIMIT: &str = "over_per_expense_limit";
const VIOLATION_OVER_PER_DAY_LIMIT: &str = "over_per_day_limit";
const VIOLATION_RECEIPT_MISSING: &str = "receipt_missing";

/// Event names this module listens to.
const APPROVAL_DECIDED: &str = "expenses.approval.decided";
/// Generic approvals contract (see the module header): the event
/// this module emits on submit and the decision event it listens
/// to. `APPROVAL_MODULE` / `APPROVAL_RECORD_TYPE` are the pair the
/// approvals module keys its definition on.
const GENERIC_APPROVAL_REQUESTED: &str = "approval.requested";
const GENERIC_APPROVAL_DECIDED: &str = "approval.decided";
const APPROVAL_MODULE: &str = "expenses";
const APPROVAL_RECORD_TYPE: &str = "expense_report";
const REIMBURSEMENT_PAID: &str = "expenses.reimbursement.paid";
const REIMBURSEMENT_FAILED: &str = "expenses.reimbursement.failed";
const PAYMENT_CAPTURED: &str = "payment.captured";
const PAYMENT_FAILED: &str = "payment.failed";
const CALCULATOR_RATE_REQUESTED: &str = "calculator.rate_requested";

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
/// submitted history stays intact.
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

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ExpensesState {
    pub categories: Arc<DashMap<Uuid, ExpenseCategory>>,
    pub policies: Arc<DashMap<Uuid, ExpensePolicy>>,
    pub reports: Arc<DashMap<Uuid, ExpenseReport>>,
    pub expenses: Arc<DashMap<Uuid, Expense>>,
    pub receipts: Arc<DashMap<Uuid, ExpenseReceipt>>,
    pub reimbursements: Arc<DashMap<Uuid, Reimbursement>>,
    pub adjustments: Arc<DashMap<Uuid, ExpenseAdjustment>>,
    pub currency_rates: Arc<DashMap<Uuid, CurrencyRate>>,
    pub settings: Arc<RwLock<ExpensesSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ExpensesState {
    fn default() -> Self {
        Self {
            categories: Arc::new(DashMap::new()),
            policies: Arc::new(DashMap::new()),
            reports: Arc::new(DashMap::new()),
            expenses: Arc::new(DashMap::new()),
            receipts: Arc::new(DashMap::new()),
            reimbursements: Arc::new(DashMap::new()),
            adjustments: Arc::new(DashMap::new()),
            currency_rates: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(ExpensesSettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ExpensesState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn settings(&self) -> ExpensesSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "expenses",
                "category": "expenses",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the events module.
struct Caller {
    actor: String,
    role: String,
}

/// Resolve the tenant context for a request. THE single
/// convergence point for tenant-scoped data access in this file
/// (see `crate::core::tenancy`): this function returns the tenancy
/// `TenantContext` (tenant id as Uuid + schema name derived only
/// by `tenancy::schema_name()`), and the Diesel persistence wiring
/// — when it lands — takes the Uuid from here into
/// `tenancy::with_tenant_connection`. Fail closed:
/// `from_http_request` answers 401 when the request did not
/// resolve to a tenant and 403 when the resolved tenant id is not
/// a valid UUID; no resolved tenant means the request is refused
/// here, never served from `public`.
///
/// OPEN (not closed by this guard): the in-memory DashMap stores
/// in this module carry no `tenant_id` on their records yet, so
/// there is no per-record tenant filtering to apply once the
/// context is resolved — that needs a store-shape change (tenant
/// id on every record + a migration column) and is integrator
/// work. What this guard does close is the context requirement
/// itself: every tenant-data endpoint resolves the tenant first
/// through `caller()`, and a request with no tenant context is
/// rejected before any business data is read.
fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    // Tenant first: no resolved tenant context means fail closed,
    // never fall back to the public schema. The resolved context
    // is bound for the Diesel persistence layer (see
    // `resolve_tenant`); the in-memory stores below carry no
    // tenant id yet, so per-record filtering is still open.
    let _tenant = resolve_tenant(req)?;
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the events module's capability check).
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn is_manager(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_EXPENSES_MANAGE)
}

/// Employees may act on their own rows; managers may act on any row.
fn require_row_access(
    ctx: &AppContext,
    caller: &Caller,
    owner_id: &str,
) -> UnifiedResult<()> {
    if is_manager(ctx, &caller.role) || caller.actor == owner_id {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Only the owner or a manager may access this row",
        ))
    }
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

/// Validate a value against a settings list. The list is the live
/// admin-configured one — never a constant in this file.
fn require_in_list(value: &str, field: &str, allowed: &[String]) -> UnifiedResult<String> {
    let checked = require_text(value, field, MAX_SLUG_LEN)?;
    if !allowed.iter().any(|item| item == &checked) {
        return Err(UnifiedError::bad_request(format!(
            "{field} \"{checked}\" is not allowed; allowed values: {}",
            allowed.join(", ")
        )));
    }
    Ok(checked)
}

fn require_currency(value: &str) -> UnifiedResult<String> {
    let checked = require_text(value, "currency", MAX_CURRENCY_LEN)?;
    if checked.len() != 3 || !checked.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code",
        ));
    }
    Ok(checked.to_ascii_uppercase())
}

fn require_amount(value: i64, field: &str) -> UnifiedResult<i64> {
    if value < 0 || value > MAX_AMOUNT_MINOR {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between 0 and {MAX_AMOUNT_MINOR}"
        )));
    }
    Ok(value)
}

fn optional_amount(value: Option<i64>, field: &str) -> UnifiedResult<Option<i64>> {
    match value {
        Some(raw) => Ok(Some(require_amount(raw, field)?)),
        None => Ok(None),
    }
}

fn parse_date(raw: &str, field: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be a date as YYYY-MM-DD")))
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Allowed report status transitions. Anything not listed is
/// rejected with a 409 carrying the allowed list, so a report can
/// never jump between states outside this table. Note that
/// `submitted -> approved` and `submitted -> rejected` are only
/// ever performed by the approval-decision event listener, and
/// `approved -> reimbursed` only by the reimbursement-paid event
/// listener — no HTTP handler performs those moves.
fn allowed_report_transitions(from: &str) -> &'static [&'static str] {
    match from {
        REPORT_DRAFT => &[REPORT_SUBMITTED],
        REPORT_SUBMITTED => &[REPORT_DRAFT, REPORT_APPROVED, REPORT_REJECTED],
        REPORT_APPROVED => &[REPORT_REIMBURSED],
        REPORT_REJECTED => &[REPORT_DRAFT],
        REPORT_REIMBURSED => &[],
        _ => &[],
    }
}

fn transition_report(report: &mut ExpenseReport, to: &str) -> UnifiedResult<()> {
    if !allowed_report_transitions(&report.status).contains(&to) {
        return Err(UnifiedError::conflict(format!(
            "Cannot move report from \"{}\" to \"{}\"",
            report.status, to
        ))
        .with_metadata(
            "allowed_transitions",
            json!(allowed_report_transitions(&report.status)),
        ));
    }
    report.status = to.to_string();
    report.updated_at = Utc::now();
    Ok(())
}

// --- Loaders ---------------------------------------------------------------

fn load_category(state: &ExpensesState, raw_id: &str) -> UnifiedResult<ExpenseCategory> {
    let id = parse_uuid(raw_id, "category")?;
    state
        .categories
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Category not found: {id}")))
}

fn load_policy(state: &ExpensesState, raw_id: &str) -> UnifiedResult<ExpensePolicy> {
    let id = parse_uuid(raw_id, "policy")?;
    state
        .policies
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Policy not found: {id}")))
}

fn load_report(state: &ExpensesState, raw_id: &str) -> UnifiedResult<ExpenseReport> {
    let id = parse_uuid(raw_id, "report")?;
    state
        .reports
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Report not found: {id}")))
}

fn load_expense(state: &ExpensesState, raw_id: &str) -> UnifiedResult<Expense> {
    let id = parse_uuid(raw_id, "expense")?;
    state
        .expenses
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Expense not found: {id}")))
}

fn load_reimbursement(state: &ExpensesState, raw_id: &str) -> UnifiedResult<Reimbursement> {
    let id = parse_uuid(raw_id, "reimbursement")?;
    state
        .reimbursements
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Reimbursement not found: {id}")))
}

fn load_currency_rate(state: &ExpensesState, raw_id: &str) -> UnifiedResult<CurrencyRate> {
    let id = parse_uuid(raw_id, "currency rate")?;
    state
        .currency_rates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Currency rate not found: {id}")))
}

fn report_expenses(state: &ExpensesState, report_id: Uuid) -> Vec<Expense> {
    let mut items: Vec<Expense> = state
        .expenses
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|row| row.report_id == report_id)
        .collect();
    items.sort_by(|a, b| (a.expense_date, a.created_at).cmp(&(b.expense_date, b.created_at)));
    items
}

fn expense_receipts(state: &ExpensesState, expense_id: Uuid) -> Vec<ExpenseReceipt> {
    let mut items: Vec<ExpenseReceipt> = state
        .receipts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|row| row.expense_id == expense_id)
        .collect();
    items.sort_by_key(|row| row.created_at);
    items
}

/// Recompute and store a report's total from its lines. Totals are
/// always derived server-side; a client can never set one.
fn refresh_report_total(state: &ExpensesState, report_id: Uuid) {
    let total: i64 = report_expenses(state, report_id)
        .iter()
        .map(|row| row.amount_minor)
        .sum();
    if let Some(mut report) = state.reports.get_mut(&report_id) {
        report.total_minor = total;
        report.updated_at = Utc::now();
    }
}

/// Refresh the report-level violation flags: a report whose total
/// exceeds its policy limit (or the settings warning level when
/// the report has no policy) is flagged `over_report_total`.
/// The flag is advisory — it never blocks submission.
fn refresh_report_violations(state: &ExpensesState, report_id: Uuid) {
    let settings = state.settings();
    let Some(mut report) = state.reports.get_mut(&report_id) else {
        return;
    };
    let limit = report
        .policy_id
        .and_then(|policy_id| state.policies.get(&policy_id))
        .and_then(|policy| policy.max_report_total_minor)
        .unwrap_or(settings.report_total_warning_minor);
    let mut violations: Vec<String> = Vec::new();
    if report.total_minor > limit {
        violations.push("over_report_total".to_string());
    }
    report.violations = violations;
    report.updated_at = Utc::now();
}

// --- Policy rates and violation checks -------------------------------------

/// Mileage rate for a report: the policy rate when the report has
/// a policy with one, otherwise the settings default. The rate is
/// policy data, never a constant in a handler.
fn mileage_rate_for(state: &ExpensesState, report: &ExpenseReport) -> i64 {
    let settings = state.settings();
    report
        .policy_id
        .and_then(|policy_id| state.policies.get(&policy_id))
        .and_then(|policy| policy.mileage_rate_minor)
        .unwrap_or(settings.default_mileage_rate_minor)
}

/// Per-diem rate for a report: the policy rate when the report has
/// a policy with one, otherwise the settings default.
fn per_diem_rate_for(state: &ExpensesState, report: &ExpenseReport) -> i64 {
    let settings = state.settings();
    report
        .policy_id
        .and_then(|policy_id| state.policies.get(&policy_id))
        .and_then(|policy| policy.per_diem_rate_minor)
        .unwrap_or(settings.default_per_diem_rate_minor)
}

/// Approval chain reference for a report: the policy chain when
/// the report has a policy with one, otherwise `None` (the
/// approvals flow then applies its own default chain).
fn approval_chain_for(state: &ExpensesState, report: &ExpenseReport) -> Option<String> {
    report
        .policy_id
        .and_then(|policy_id| state.policies.get(&policy_id))
        .and_then(|policy| policy.approval_chain_id.clone())
}

/// Compute the violation flags for one expense line. Over-limit
/// lines are flagged, never silently dropped or auto-approved;
/// a missing receipt is flagged when the category requires one
/// at this amount. The per-day check sums the employee's other
/// lines in the same category on the same date, across reports.
fn compute_violations(state: &ExpensesState, report: &ExpenseReport, expense: &Expense) -> Vec<String> {
    let mut violations: Vec<String> = Vec::new();
    if let Some(category) = state.categories.get(&expense.category_id) {
        if let Some(limit) = category.per_expense_limit_minor {
            if expense.amount_minor > limit {
                violations.push(VIOLATION_OVER_PER_EXPENSE_LIMIT.to_string());
            }
        }
        if let Some(limit) = category.per_day_limit_minor {
            let day_total: i64 = state
                .expenses
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|row| {
                    row.id != expense.id
                        && row.category_id == expense.category_id
                        && row.expense_date == expense.expense_date
                        && state
                            .reports
                            .get(&row.report_id)
                            .map(|owner| owner.employee_id == report.employee_id)
                            .unwrap_or(false)
                })
                .map(|row| row.amount_minor)
                .sum::<i64>()
                + expense.amount_minor;
            if day_total > limit {
                violations.push(VIOLATION_OVER_PER_DAY_LIMIT.to_string());
            }
        }
        let threshold = category
            .receipt_required_above_minor
            .unwrap_or(0);
        if category.receipt_required && expense.amount_minor >= threshold {
            let has_receipt = state
                .receipts
                .iter()
                .any(|entry| entry.value().expense_id == expense.id);
            if !has_receipt {
                violations.push(VIOLATION_RECEIPT_MISSING.to_string());
            }
        }
    }
    violations
}

/// Recompute violations for every line of a report and store them.
fn refresh_expense_violations(state: &ExpensesState, report: &ExpenseReport) {
    for line in report_expenses(state, report.id) {
        let violations = compute_violations(state, report, &line);
        if let Some(mut stored) = state.expenses.get_mut(&line.id) {
            stored.violations = violations;
            stored.updated_at = Utc::now();
        }
    }
}

// --- Currency conversion (stored rates only) -------------------------------

/// Find a stored rate for a pair in this module's own rates: the
/// direct pair first, then the inverse pair derived as `1 / rate`.
/// Returns `None` when this module stores nothing for the pair.
fn stored_rate(state: &ExpensesState, from: &str, to: &str) -> Option<f64> {
    let mut direct: Option<f64> = None;
    let mut inverse: Option<f64> = None;
    for entry in state.currency_rates.iter() {
        let row = entry.value();
        if row.base_currency == from && row.target_currency == to {
            direct = Some(row.rate);
        }
        if row.base_currency == to && row.target_currency == from && row.rate != 0.0 {
            inverse = Some(1.0 / row.rate);
        }
    }
    direct.or(inverse)
}

/// Ask the calculator module for a stored rate through its
/// `calculator.rate_requested` event. The calculator answers with
/// a result row when it holds a stored rate; no answer means no
/// stored rate, and the caller must treat the rate as unavailable.
fn calculator_rate(bus: &HookBus, from: &str, to: &str) -> Option<f64> {
    let context = bus.emit(
        CALCULATOR_RATE_REQUESTED,
        json!({ "base_currency": from, "target_currency": to }),
    );
    context
        .results
        .iter()
        .find_map(|result| result.get("rate").and_then(|value| value.as_f64()))
        .filter(|rate| rate.is_finite() && *rate > 0.0)
}

/// Resolve a conversion rate from stored data only: this module's
/// own stored rates first, then the calculator module's stored
/// rates. Returns `None` when no stored rate exists anywhere —
/// the caller then fails the conversion instead of guessing.
fn resolve_rate(state: &ExpensesState, bus: &HookBus, from: &str, to: &str) -> Option<f64> {
    if from == to {
        return Some(1.0);
    }
    stored_rate(state, from, to).or_else(|| calculator_rate(bus, from, to))
}

/// Convert an amount in minor units using a stored rate only.
/// Fails with 422 when no stored rate exists for the pair.
fn convert_amount(
    state: &ExpensesState,
    bus: &HookBus,
    amount_minor: i64,
    from: &str,
    to: &str,
) -> UnifiedResult<i64> {
    if from == to {
        return Ok(amount_minor);
    }
    let rate = resolve_rate(state, bus, from, to).ok_or_else(|| {
        UnifiedError::unprocessable(format!(
            "No stored exchange rate for {from} to {to}; conversion refused rather than guessing a rate"
        ))
    })?;
    let converted = (amount_minor as f64 * rate).round();
    if !converted.is_finite() || converted < 0.0 || converted > MAX_AMOUNT_MINOR as f64 {
        return Err(UnifiedError::unprocessable(
            "Converted amount is out of range",
        ));
    }
    Ok(converted as i64)
}

// --- Hook listeners ----------------------------------------------------------

/// Handle an approval decision from the approvals flow. Only a
/// `submitted` report whose stored request id matches the event
/// may move; every other shape is a safe no-op (withdrawn,
/// already decided, or unknown request). Approval creates the
/// reimbursement row and requests the pay-out from billing.
fn on_approval_decided(state: Arc<ExpensesState>, ctx: &mut HookContext) {
    let Some(request_id) = opt_string(&ctx.data, "approval_request_id") else {
        return;
    };
    let Some(report_id_raw) = opt_string(&ctx.data, "report_id") else {
        return;
    };
    let Ok(report_id) = Uuid::parse_str(&report_id_raw) else {
        return;
    };
    let Some(decision) = opt_string(&ctx.data, "decision") else {
        return;
    };
    if decision != REPORT_APPROVED && decision != REPORT_REJECTED {
        return;
    }
    let Some(mut report) = state.reports.get_mut(&report_id) else {
        return;
    };
    if report.status != REPORT_SUBMITTED {
        return;
    }
    if report.approval_request_id.as_deref() != Some(request_id.as_str()) {
        return;
    }
    let decided_by = opt_string(&ctx.data, "decided_by");
    let reason = opt_string(&ctx.data, "reason");
    let now = Utc::now();
    if transition_report(&mut report, &decision).is_err() {
        return;
    }
    report.decided_at = Some(now);
    report.decided_by = decided_by.clone();
    report.decision_reason = reason.clone();
    report.updated_at = now;
    let report_snapshot = report.clone();
    drop(report);
    if decision == REPORT_REJECTED {
        state.emit(
            "expenses.report.rejected",
            json!({
                "report_id": report_id.to_string(),
                "employee_id": report_snapshot.employee_id,
                "decided_by": decided_by,
                "reason": reason,
            }),
        );
        state.notify(
            "expense_report_rejected",
            &report_snapshot.employee_id,
            "Expense report rejected",
            &format!(
                "Your expense report \"{}\" was rejected{}",
                report_snapshot.title,
                reason
                    .as_ref()
                    .map(|value| format!(": {value}"))
                    .unwrap_or_default()
            ),
            None,
        );
        return;
    }
    state.emit(
        "expenses.report.approved",
        json!({
            "report_id": report_id.to_string(),
            "employee_id": report_snapshot.employee_id,
            "total_minor": report_snapshot.total_minor,
            "currency": report_snapshot.currency,
            "decided_by": decided_by,
        }),
    );
    let reimbursement = Reimbursement {
        id: Uuid::now_v7(),
        report_id,
        employee_id: report_snapshot.employee_id.clone(),
        amount_minor: report_snapshot.total_minor,
        currency: report_snapshot.currency.clone(),
        status: REIMB_PENDING.to_string(),
        payable_reference: format!("expenses-reimbursement-{}", Uuid::now_v7()),
        failure_reason: None,
        paid_at: None,
        attempts: 1,
        created_at: now,
        updated_at: now,
    };
    state
        .reimbursements
        .insert(reimbursement.id, reimbursement.clone());
    if let Some(mut stored) = state.reports.get_mut(&report_id) {
        stored.reimbursement_id = Some(reimbursement.id);
        stored.updated_at = Utc::now();
    }
    state.emit(
        "expenses.reimbursement.requested",
        json!({
            "reimbursement_id": reimbursement.id.to_string(),
            "report_id": report_id.to_string(),
            "employee_id": reimbursement.employee_id,
            "amount_minor": reimbursement.amount_minor,
            "currency": reimbursement.currency,
            "payable_reference": reimbursement.payable_reference,
        }),
    );
    state.notify(
        "expense_report_approved",
        &report_snapshot.employee_id,
        "Expense report approved",
        &format!(
            "Your expense report \"{}\" was approved; reimbursement of {} {} has been requested",
            report_snapshot.title, report_snapshot.total_minor, report_snapshot.currency
        ),
        None,
    );
}

/// Emit the generic `approval.requested` for a submitted report and
/// return the approvals `request_id` from the synchronous answer,
/// when the approvals module accepts the request. Returns `None`
/// when nobody answers or the request is declined (no active
/// definition for `expenses` / `expense_report`): the report stays
/// `submitted` and no correlation id is stored.
fn request_generic_approval(state: &ExpensesState, report: &ExpenseReport) -> Option<String> {
    let Some(bus) = state.bus() else {
        return None;
    };
    let summary = format!(
        "Expense report \"{}\" — {} {}",
        report.title, report.total_minor, report.currency
    );
    let context = bus.emit(
        GENERIC_APPROVAL_REQUESTED,
        json!({
            "module": APPROVAL_MODULE,
            "record_type": APPROVAL_RECORD_TYPE,
            "record_id": report.id.to_string(),
            "requester_id": report.employee_id,
            "summary": summary,
            "amount_minor": report.total_minor,
            "currency": report.currency,
            "company_id": report.company_id,
        }),
    );
    let answer = context
        .get("result")
        .cloned()
        .or_else(|| context.results.first().cloned())?;
    if answer.get("accepted").and_then(|value| value.as_bool()) != Some(true) {
        return None;
    }
    answer
        .get("request_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Handle a generic `approval.decided` from the approvals flow.
/// Correlates the decision to a submitted report — by the stored
/// approvals `request_id` when the report carries one, otherwise
/// by the (`module`, `record_type`, `record_id`) triple (requests
/// created before this bridge landed, or by an admin through the
/// approvals API) — then re-emits it as `expenses.approval.decided`
/// so the existing listener applies the outcome (status move,
/// reimbursement, notifications) exactly once. A decision for an
/// unknown request, a mismatched report, or a report that is no
/// longer `submitted` is ignored as a safe no-op. `approved` and
/// `rejected` advance the report; `changes_requested` is re-emitted
/// for listeners but leaves the report `submitted` — the requester
/// withdraws it to draft to revise it.
fn on_generic_approval_decided(state: Arc<ExpensesState>, ctx: &mut HookContext) {
    if opt_string(&ctx.data, "module").as_deref() != Some(APPROVAL_MODULE) {
        return;
    }
    if opt_string(&ctx.data, "record_type").as_deref() != Some(APPROVAL_RECORD_TYPE) {
        return;
    }
    let Some(record_id_raw) = opt_string(&ctx.data, "record_id") else {
        return;
    };
    let Ok(report_id) = Uuid::parse_str(&record_id_raw) else {
        return;
    };
    let Some(decision) = opt_string(&ctx.data, "decision") else {
        return;
    };
    if decision != REPORT_APPROVED
        && decision != REPORT_REJECTED
        && decision != "changes_requested"
    {
        return;
    }
    let Some(report) = state.reports.get(&report_id).map(|entry| entry.value().clone()) else {
        return;
    };
    if report.status != REPORT_SUBMITTED {
        return;
    }
    // When the report carries the approvals request id from submit,
    // the decision must carry the same id — a stale decision for an
    // earlier round (withdrawn and resubmitted) must not apply to
    // the current round. Without a stored id, the record triple
    // above is the correlation.
    if let Some(stored) = &report.generic_approval_request_id {
        if opt_string(&ctx.data, "request_id").as_deref() != Some(stored.as_str()) {
            return;
        }
    }
    // A submitted report always carries its expenses-side request
    // id (set on submit); without it the existing listener could
    // not correlate the re-emitted event, so there is nothing to
    // forward.
    let Some(approval_request_id) = report.approval_request_id.clone() else {
        return;
    };
    let decided_by = opt_string(&ctx.data, "decided_by");
    let reason = opt_string(&ctx.data, "reason");
    state.emit(
        APPROVAL_DECIDED,
        json!({
            "approval_request_id": approval_request_id,
            "report_id": report.id.to_string(),
            "decision": decision,
            "decided_by": decided_by,
            "reason": reason,
        }),
    );
}

/// Shared outcome handler for reimbursement paid / failed events.
/// A paid event moves the reimbursement to `paid` and the report
/// to `reimbursed`; a failed event records the reason and leaves
/// the report `approved` so the reimbursement can be retried.
fn apply_reimbursement_outcome(state: &ExpensesState, ctx: &HookContext, paid: bool) {
    let Some(reimbursement_id_raw) = opt_string(&ctx.data, "reimbursement_id") else {
        return;
    };
    let Ok(reimbursement_id) = Uuid::parse_str(&reimbursement_id_raw) else {
        return;
    };
    let Some(mut reimbursement) = state.reimbursements.get_mut(&reimbursement_id) else {
        return;
    };
    if reimbursement.status == REIMB_PAID {
        return;
    }
    let now = Utc::now();
    if paid {
        reimbursement.status = REIMB_PAID.to_string();
        reimbursement.paid_at = Some(now);
        reimbursement.failure_reason = None;
        reimbursement.updated_at = now;
        let snapshot = reimbursement.clone();
        drop(reimbursement);
        if let Some(mut report) = state.reports.get_mut(&snapshot.report_id) {
            if report.status == REPORT_APPROVED {
                let _ = transition_report(&mut report, REPORT_REIMBURSED);
                report.updated_at = now;
            }
        }
        state.emit(
            "expenses.report.reimbursed",
            json!({
                "report_id": snapshot.report_id.to_string(),
                "reimbursement_id": snapshot.id.to_string(),
                "employee_id": snapshot.employee_id,
                "amount_minor": snapshot.amount_minor,
                "currency": snapshot.currency,
            }),
        );
        state.notify(
            "expense_reimbursed",
            &snapshot.employee_id,
            "Expense reimbursement paid",
            &format!(
                "Your reimbursement of {} {} has been paid",
                snapshot.amount_minor, snapshot.currency
            ),
            None,
        );
    } else {
        reimbursement.status = REIMB_FAILED.to_string();
        reimbursement.failure_reason = opt_string(&ctx.data, "reason");
        reimbursement.updated_at = now;
    }
}

fn on_reimbursement_paid(state: Arc<ExpensesState>, ctx: &mut HookContext) {
    apply_reimbursement_outcome(&state, ctx, true);
}

fn on_reimbursement_failed(state: Arc<ExpensesState>, ctx: &mut HookContext) {
    apply_reimbursement_outcome(&state, ctx, false);
}

/// Generic payment events carry many kinds of payments; only a
/// payload that names a reimbursement id this module owns is a
/// reimbursement outcome. Everything else is ignored.
fn on_payment_event(state: Arc<ExpensesState>, ctx: &mut HookContext, paid: bool) {
    let Some(reimbursement_id_raw) = opt_string(&ctx.data, "reimbursement_id") else {
        return;
    };
    if Uuid::parse_str(&reimbursement_id_raw).is_err() {
        return;
    }
    apply_reimbursement_outcome(&state, ctx, paid);
}

/// Escape one CSV field: quote when it contains a comma, a quote,
/// or a newline, doubling embedded quotes.
fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

// --- Handlers: categories ---------------------------------------------------

async fn list_categories(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let active_filter = params.filters.get("active").cloned();
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut items: Vec<ExpenseCategory> = state
            .categories
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if let Some(company) = &company_filter {
                    if row.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if let Some(active) = &active_filter {
                    let wanted = active == "true";
                    if row.active != wanted {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    let haystack = format!("{} {}", row.name, row.code).to_lowercase();
                    if !haystack.contains(needle) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    body: web::Json<CreateCategoryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseCategory> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let code = require_text(&input.code, "code", MAX_CODE_LEN)?;
        let company_id = optional_text(&input.company_id, "company_id", MAX_SLUG_LEN)?;
        let receipt_required_above_minor =
            optional_amount(input.receipt_required_above_minor, "receipt_required_above_minor")?;
        let per_expense_limit_minor =
            optional_amount(input.per_expense_limit_minor, "per_expense_limit_minor")?;
        let per_day_limit_minor =
            optional_amount(input.per_day_limit_minor, "per_day_limit_minor")?;
        let now = Utc::now();
        let category = ExpenseCategory {
            id: Uuid::now_v7(),
            name,
            code,
            company_id,
            receipt_required: input.receipt_required.unwrap_or(false),
            receipt_required_above_minor,
            per_expense_limit_minor,
            per_day_limit_minor,
            active: true,
            created_at: now,
            updated_at: now,
        };
        state.categories.insert(category.id, category.clone());
        state.emit(
            "expenses.category.created",
            json!({ "category_id": category.id.to_string(), "name": category.name }),
        );
        Ok(category)
    })();
    match result {
        Ok(category) => created(category),
        Err(error) => err(error, debug),
    }
}

async fn get_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseCategory> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        load_category(&state, &path.into_inner())
    })();
    match result {
        Ok(category) => ok(category),
        Err(error) => err(error, debug),
    }
}

async fn update_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCategoryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseCategory> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let input = body.into_inner();
        let mut category = load_category(&state, &path.into_inner())?;
        if let Some(raw) = &input.name {
            category.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.code {
            category.code = require_text(raw, "code", MAX_CODE_LEN)?;
        }
        if let Some(value) = input.receipt_required {
            category.receipt_required = value;
        }
        if input.receipt_required_above_minor.is_some() {
            category.receipt_required_above_minor =
                optional_amount(input.receipt_required_above_minor, "receipt_required_above_minor")?;
        }
        if input.per_expense_limit_minor.is_some() {
            category.per_expense_limit_minor =
                optional_amount(input.per_expense_limit_minor, "per_expense_limit_minor")?;
        }
        if input.per_day_limit_minor.is_some() {
            category.per_day_limit_minor =
                optional_amount(input.per_day_limit_minor, "per_day_limit_minor")?;
        }
        if let Some(value) = input.active {
            category.active = value;
        }
        category.updated_at = Utc::now();
        state.categories.insert(category.id, category.clone());
        state.emit(
            "expenses.category.updated",
            json!({ "category_id": category.id.to_string(), "name": category.name }),
        );
        Ok(category)
    })();
    match result {
        Ok(category) => ok(category),
        Err(error) => err(error, debug),
    }
}

async fn delete_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let category = load_category(&state, &path.into_inner())?;
        let in_use = state
            .expenses
            .iter()
            .any(|entry| entry.value().category_id == category.id);
        if in_use {
            return Err(UnifiedError::conflict(
                "Category is used by expenses and cannot be deleted; deactivate it instead",
            ));
        }
        state.categories.remove(&category.id);
        state.emit(
            "expenses.category.deleted",
            json!({ "category_id": category.id.to_string() }),
        );
        Ok(json!({ "deleted": true, "category_id": category.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// --- Handlers: policies -----------------------------------------------------

async fn list_policies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let active_filter = params.filters.get("active").cloned();
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut items: Vec<ExpensePolicy> = state
            .policies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if let Some(company) = &company_filter {
                    if row.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if let Some(active) = &active_filter {
                    let wanted = active == "true";
                    if row.active != wanted {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    if !row.name.to_lowercase().contains(needle) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    body: web::Json<CreatePolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpensePolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let company_id = optional_text(&input.company_id, "company_id", MAX_SLUG_LEN)?;
        let currency = match &input.currency {
            Some(raw) => require_in_list(raw, "currency", &settings.supported_currencies)
                .and_then(|value| require_currency(&value))?,
            None => settings.default_currency.clone(),
        };
        let mileage_rate_minor = optional_amount(input.mileage_rate_minor, "mileage_rate_minor")?;
        let mileage_unit = match &input.mileage_unit {
            Some(raw) => require_text(raw, "mileage_unit", 16)?,
            None => settings.default_mileage_unit.clone(),
        };
        let per_diem_rate_minor =
            optional_amount(input.per_diem_rate_minor, "per_diem_rate_minor")?;
        let max_report_total_minor =
            optional_amount(input.max_report_total_minor, "max_report_total_minor")?;
        let approval_chain_id =
            optional_text(&input.approval_chain_id, "approval_chain_id", MAX_SLUG_LEN)?;
        let now = Utc::now();
        let policy = ExpensePolicy {
            id: Uuid::now_v7(),
            name,
            company_id,
            currency,
            mileage_rate_minor,
            mileage_unit,
            per_diem_rate_minor,
            max_report_total_minor,
            approval_chain_id,
            active: true,
            created_at: now,
            updated_at: now,
        };
        state.policies.insert(policy.id, policy.clone());
        state.emit(
            "expenses.policy.created",
            json!({ "policy_id": policy.id.to_string(), "name": policy.name }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => created(policy),
        Err(error) => err(error, debug),
    }
}

async fn get_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpensePolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        load_policy(&state, &path.into_inner())
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

async fn update_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpensePolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        let mut policy = load_policy(&state, &path.into_inner())?;
        if let Some(raw) = &input.name {
            policy.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.currency {
            policy.currency =
                require_in_list(raw, "currency", &settings.supported_currencies)
                    .and_then(|value| require_currency(&value))?;
        }
        if input.mileage_rate_minor.is_some() {
            policy.mileage_rate_minor =
                optional_amount(input.mileage_rate_minor, "mileage_rate_minor")?;
        }
        if let Some(raw) = &input.mileage_unit {
            policy.mileage_unit = require_text(raw, "mileage_unit", 16)?;
        }
        if input.per_diem_rate_minor.is_some() {
            policy.per_diem_rate_minor =
                optional_amount(input.per_diem_rate_minor, "per_diem_rate_minor")?;
        }
        if input.max_report_total_minor.is_some() {
            policy.max_report_total_minor =
                optional_amount(input.max_report_total_minor, "max_report_total_minor")?;
        }
        if input.approval_chain_id.is_some() {
            policy.approval_chain_id =
                optional_text(&input.approval_chain_id, "approval_chain_id", MAX_SLUG_LEN)?;
        }
        if let Some(value) = input.active {
            policy.active = value;
        }
        policy.updated_at = Utc::now();
        state.policies.insert(policy.id, policy.clone());
        state.emit(
            "expenses.policy.updated",
            json!({ "policy_id": policy.id.to_string(), "name": policy.name }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

async fn delete_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let policy = load_policy(&state, &path.into_inner())?;
        let in_use = state
            .reports
            .iter()
            .any(|entry| entry.value().policy_id == Some(policy.id));
        if in_use {
            return Err(UnifiedError::conflict(
                "Policy is used by reports and cannot be deleted; deactivate it instead",
            ));
        }
        state.policies.remove(&policy.id);
        state.emit(
            "expenses.policy.deleted",
            json!({ "policy_id": policy.id.to_string() }),
        );
        Ok(json!({ "deleted": true, "policy_id": policy.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// --- Handlers: reports ------------------------------------------------------

async fn list_reports(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let employee_filter = params.filters.get("employee_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let policy_filter = params.filters.get("policy_id").cloned();
        let search = params.search.clone().map(|value| value.to_lowercase());
        let manager = is_manager(&ctx, &caller.role);
        let mut items: Vec<ExpenseReport> = state
            .reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if !manager && row.employee_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                if let Some(employee) = &employee_filter {
                    if &row.employee_id != employee {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if row.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if let Some(policy) = &policy_filter {
                    if row.policy_id.map(|id| id.to_string()) != Some(policy.clone()) {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    let haystack = format!("{} {}", row.title, row.notes.clone().unwrap_or_default())
                        .to_lowercase();
                    if !haystack.contains(needle) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    body: web::Json<CreateReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let input = body.into_inner();
        let settings = state.settings();
        let title = require_text(&input.title, "title", MAX_NAME_LEN)?;
        let notes = optional_text(&input.notes, "notes", MAX_DESCRIPTION_LEN)?;
        let company_id = optional_text(&input.company_id, "company_id", MAX_SLUG_LEN)?;
        let policy_id = match &input.policy_id {
            Some(raw) => {
                let policy = load_policy(&state, raw)?;
                if !policy.active {
                    return Err(UnifiedError::bad_request(
                        "Policy is not active",
                    ));
                }
                Some(policy.id)
            }
            None => None,
        };
        let currency = match &input.currency {
            Some(raw) => require_in_list(raw, "currency", &settings.supported_currencies)
                .and_then(|value| require_currency(&value))?,
            None => {
                if let Some(policy_id) = policy_id {
                    state
                        .policies
                        .get(&policy_id)
                        .map(|policy| policy.currency.clone())
                        .unwrap_or_else(|| settings.default_currency.clone())
                } else {
                    settings.default_currency.clone()
                }
            }
        };
        let now = Utc::now();
        let report = ExpenseReport {
            id: Uuid::now_v7(),
            employee_id: caller.actor.clone(),
            company_id,
            policy_id,
            title,
            notes,
            status: REPORT_DRAFT.to_string(),
            currency,
            total_minor: 0,
            violations: Vec::new(),
            approval_request_id: None,
            generic_approval_request_id: None,
            submitted_at: None,
            decided_at: None,
            decided_by: None,
            decision_reason: None,
            reimbursement_id: None,
            created_at: now,
            updated_at: now,
        };
        state.reports.insert(report.id, report.clone());
        state.emit(
            "expenses.report.created",
            json!({
                "report_id": report.id.to_string(),
                "employee_id": report.employee_id,
                "status": report.status,
            }),
        );
        Ok(report)
    })();
    match result {
        Ok(report) => created(report),
        Err(error) => err(error, debug),
    }
}

async fn get_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let report = load_report(&state, &path.into_inner())?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        let lines = report_expenses(&state, report.id);
        Ok(json!({ "report": report, "expenses": lines }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let input = body.into_inner();
        let mut report = load_report(&state, &path.into_inner())?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        if report.status != REPORT_DRAFT {
            return Err(UnifiedError::conflict(format!(
                "Only a draft report can be edited; this report is \"{}\"",
                report.status
            ))
            .with_metadata("allowed_transitions", json!(allowed_report_transitions(&report.status))));
        }
        if let Some(raw) = &input.title {
            report.title = require_text(raw, "title", MAX_NAME_LEN)?;
        }
        if input.notes.is_some() {
            report.notes = optional_text(&input.notes, "notes", MAX_DESCRIPTION_LEN)?;
        }
        if input.policy_id.is_some() {
            match &input.policy_id {
                Some(raw) => {
                    let policy = load_policy(&state, raw)?;
                    if !policy.active {
                        return Err(UnifiedError::bad_request("Policy is not active"));
                    }
                    report.policy_id = Some(policy.id);
                }
                None => report.policy_id = None,
            }
        }
        report.updated_at = Utc::now();
        state.reports.insert(report.id, report.clone());
        refresh_report_violations(&state, report.id);
        let report = load_report(&state, &report.id.to_string())?;
        state.emit(
            "expenses.report.updated",
            json!({ "report_id": report.id.to_string(), "status": report.status }),
        );
        Ok(report)
    })();
    match result {
        Ok(report) => ok(report),
        Err(error) => err(error, debug),
    }
}

async fn delete_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let report = load_report(&state, &path.into_inner())?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        if report.status != REPORT_DRAFT {
            return Err(UnifiedError::conflict(format!(
                "Only a draft report can be deleted; this report is \"{}\"",
                report.status
            ))
            .with_metadata("allowed_transitions", json!(allowed_report_transitions(&report.status))));
        }
        for line in report_expenses(&state, report.id) {
            state
                .receipts
                .iter()
                .filter(|entry| entry.value().expense_id == line.id)
                .map(|entry| *entry.key())
                .collect::<Vec<Uuid>>()
                .into_iter()
                .for_each(|receipt_id| {
                    state.receipts.remove(&receipt_id);
                });
            state.expenses.remove(&line.id);
        }
        state
            .adjustments
            .iter()
            .filter(|entry| entry.value().report_id == report.id)
            .map(|entry| *entry.key())
            .collect::<Vec<Uuid>>()
            .into_iter()
            .for_each(|adjustment_id| {
                state.adjustments.remove(&adjustment_id);
            });
        state.reports.remove(&report.id);
        state.emit(
            "expenses.report.deleted",
            json!({ "report_id": report.id.to_string() }),
        );
        Ok(json!({ "deleted": true, "report_id": report.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn submit_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let mut report = load_report(&state, &path.into_inner())?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        if report.status != REPORT_DRAFT {
            return Err(UnifiedError::conflict(format!(
                "Only a draft report can be submitted; this report is \"{}\"",
                report.status
            ))
            .with_metadata("allowed_transitions", json!(allowed_report_transitions(&report.status))));
        }
        let lines = report_expenses(&state, report.id);
        if lines.is_empty() {
            return Err(UnifiedError::bad_request(
                "A report with no expenses cannot be submitted",
            ));
        }
        refresh_expense_violations(&state, &report);
        refresh_report_total(&state, report.id);
        let lines = report_expenses(&state, report.id);
        let missing_receipts: Vec<String> = lines
            .iter()
            .filter(|line| line.violations.contains(&VIOLATION_RECEIPT_MISSING.to_string()))
            .map(|line| line.id.to_string())
            .collect();
        if !missing_receipts.is_empty() {
            return Err(UnifiedError::conflict(
                "Report cannot be submitted: some expenses are missing a required receipt",
            )
            .with_metadata("expenses_missing_receipt", json!(missing_receipts)));
        }
        let mut report = load_report(&state, &report.id.to_string())?;
        transition_report(&mut report, REPORT_SUBMITTED)?;
        let request_id = Uuid::now_v7().to_string();
        report.approval_request_id = Some(request_id.clone());
        report.submitted_at = Some(Utc::now());
        report.decided_at = None;
        report.decided_by = None;
        report.decision_reason = None;
        report.updated_at = Utc::now();
        state.reports.insert(report.id, report.clone());
        refresh_report_violations(&state, report.id);
        let mut report = load_report(&state, &report.id.to_string())?;
        let all_violations: Vec<String> = lines
            .iter()
            .flat_map(|line| line.violations.clone())
            .collect::<std::collections::HashSet<String>>()
            .into_iter()
            .collect();
        state.emit(
            "expenses.approval.requested",
            json!({
                "approval_request_id": request_id,
                "report_id": report.id.to_string(),
                "employee_id": report.employee_id,
                "company_id": report.company_id,
                "total_minor": report.total_minor,
                "currency": report.currency,
                "approval_chain_id": approval_chain_for(&state, &report),
                "violations": all_violations,
            }),
        );
        // Bridge to the generic approvals flow (see the module
        // header): the same submission as `approval.requested`.
        // The approvals `request_id` from the synchronous answer
        // is stored on the report as the correlation id for the
        // generic `approval.decided` answer. When the approvals
        // module declines (no active definition) or is not
        // registered, the submit still succeeds and the report
        // stays `submitted` — a missing definition never advances
        // the report on its own.
        if let Some(generic_id) = request_generic_approval(&state, &report) {
            report.generic_approval_request_id = Some(generic_id);
            report.updated_at = Utc::now();
            state.reports.insert(report.id, report.clone());
        }
        state.emit(
            "expenses.report.submitted",
            json!({
                "report_id": report.id.to_string(),
                "employee_id": report.employee_id,
                "approval_request_id": request_id,
                "total_minor": report.total_minor,
                "currency": report.currency,
            }),
        );
        Ok(report)
    })();
    match result {
        Ok(report) => ok(report),
        Err(error) => err(error, debug),
    }
}

async fn withdraw_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let mut report = load_report(&state, &path.into_inner())?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        if report.status != REPORT_SUBMITTED && report.status != REPORT_REJECTED {
            return Err(UnifiedError::conflict(format!(
                "Only a submitted or rejected report can be withdrawn; this report is \"{}\"",
                report.status
            ))
            .with_metadata("allowed_transitions", json!(allowed_report_transitions(&report.status))));
        }
        let request_id = report.approval_request_id.clone();
        transition_report(&mut report, REPORT_DRAFT)?;
        report.approval_request_id = None;
        report.generic_approval_request_id = None;
        report.submitted_at = None;
        report.updated_at = Utc::now();
        state.reports.insert(report.id, report.clone());
        if let Some(request_id) = request_id {
            state.emit(
                "expenses.approval.cancelled",
                json!({
                    "approval_request_id": request_id,
                    "report_id": report.id.to_string(),
                }),
            );
        }
        state.emit(
            "expenses.report.withdrawn",
            json!({
                "report_id": report.id.to_string(),
                "employee_id": report.employee_id,
            }),
        );
        Ok(report)
    })();
    match result {
        Ok(report) => ok(report),
        Err(error) => err(error, debug),
    }
}

async fn export_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let report = load_report(&state, &path.into_inner())?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        Ok(report)
    })();
    let report = match result {
        Ok(report) => report,
        Err(error) => return err(error, debug),
    };
    let lines = report_expenses(&state, report.id);
    let format = query
        .format
        .clone()
        .unwrap_or_else(|| "json".to_string())
        .to_lowercase();
    if format == "csv" {
        let mut out = String::from("expense_id,kind,category_id,expense_date,amount_minor,currency,description,project_id,client_id,violations\n");
        for line in &lines {
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{}\n",
                line.id,
                csv_field(&line.kind),
                line.category_id,
                line.expense_date,
                line.amount_minor,
                csv_field(&line.currency),
                csv_field(&line.description),
                csv_field(line.project_id.as_deref().unwrap_or("")),
                csv_field(line.client_id.as_deref().unwrap_or("")),
                csv_field(&line.violations.join(";")),
            ));
        }
        return HttpResponse::Ok()
            .content_type("text/csv")
            .body(out);
    }
    ok(json!({ "report": report, "expenses": lines }))
}

// --- Handlers: expenses ------------------------------------------------------

async fn list_report_expenses(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let report = load_report(&state, &path.into_inner())?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let category_filter = params.filters.get("category_id").cloned();
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut items: Vec<Expense> = report_expenses(&state, report.id)
            .into_iter()
            .filter(|row| {
                if let Some(kind) = &kind_filter {
                    if &row.kind != kind {
                        return false;
                    }
                }
                if let Some(category) = &category_filter {
                    if row.category_id.to_string() != *category {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    if !row.description.to_lowercase().contains(needle) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| (a.expense_date, a.created_at).cmp(&(b.expense_date, b.created_at)));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_expense(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateExpenseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Expense> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let report = load_report(&state, &path.into_inner())?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        if report.status != REPORT_DRAFT {
            return Err(UnifiedError::conflict(format!(
                "Expenses can only be added to a draft report; this report is \"{}\"",
                report.status
            ))
            .with_metadata("allowed_transitions", json!(allowed_report_transitions(&report.status))));
        }
        let input = body.into_inner();
        let settings = state.settings();
        let kind = require_in_list(&input.kind, "kind", &settings.expense_kinds)?;
        let category = load_category(&state, &input.category_id)?;
        if !category.active {
            return Err(UnifiedError::bad_request("Category is not active"));
        }
        let expense_date = parse_date(&input.expense_date, "expense_date")?;
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let project_id = optional_text(&input.project_id, "project_id", MAX_SLUG_LEN)?;
        let client_id = optional_text(&input.client_id, "client_id", MAX_SLUG_LEN)?;
        let currency = match &input.currency {
            Some(raw) => {
                let code = require_in_list(raw, "currency", &settings.supported_currencies)
                    .and_then(|value| require_currency(&value))?;
                if code != report.currency {
                    // The line is stored in the report currency;
                    // convert with a stored rate only.
                    convert_amount(&state, &ctx.hooks, 0, &code, &report.currency)?;
                }
                code
            }
            None => report.currency.clone(),
        };
        // Mileage and per-diem amounts are computed server-side
        // from policy rates; a client-supplied amount is ignored
        // for those kinds so the rate can never be overridden.
        let (amount_minor, distance, days, rate_minor_applied) = match kind.as_str() {
            "mileage" => {
                let distance = input.distance.ok_or_else(|| {
                    UnifiedError::bad_request("Mileage expenses require a distance")
                })?;
                if !(distance > 0.0) || distance > MAX_MILEAGE_DISTANCE || !distance.is_finite() {
                    return Err(UnifiedError::bad_request(format!(
                        "distance must be between 0 and {MAX_MILEAGE_DISTANCE}"
                    )));
                }
                let rate = mileage_rate_for(&state, &report);
                let amount = (distance * rate as f64).round() as i64;
                let amount = require_amount(amount, "amount_minor")?;
                (amount, Some(distance), None, Some(rate))
            }
            "per_diem" => {
                let days = input.days.ok_or_else(|| {
                    UnifiedError::bad_request("Per-diem expenses require days")
                })?;
                if days < 1 || days > MAX_PER_DIEM_DAYS {
                    return Err(UnifiedError::bad_request(format!(
                        "days must be between 1 and {MAX_PER_DIEM_DAYS}"
                    )));
                }
                let rate = per_diem_rate_for(&state, &report);
                let amount = require_amount(days * rate, "amount_minor")?;
                (amount, None, Some(days), Some(rate))
            }
            _ => {
                let amount = require_amount(
                    input.amount_minor.ok_or_else(|| {
                        UnifiedError::bad_request("Standard expenses require amount_minor")
                    })?,
                    "amount_minor",
                )?;
                (amount, None, None, None)
            }
        };
        // Convert a foreign-currency standard line into the report
        // currency with a stored rate only; mileage and per-diem
        // lines are already priced in the report currency.
        let amount_minor = if currency != report.currency && rate_minor_applied.is_none() {
            convert_amount(&state, &ctx.hooks, amount_minor, &currency, &report.currency)?
        } else {
            amount_minor
        };
        let now = Utc::now();
        let mut expense = Expense {
            id: Uuid::now_v7(),
            report_id: report.id,
            kind,
            category_id: category.id,
            expense_date,
            amount_minor,
            currency: report.currency.clone(),
            description,
            project_id,
            client_id,
            distance,
            days,
            rate_minor_applied,
            violations: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        expense.violations = compute_violations(&state, &report, &expense);
        state.expenses.insert(expense.id, expense.clone());
        refresh_report_total(&state, report.id);
        refresh_report_violations(&state, report.id);
        state.emit(
            "expenses.expense.created",
            json!({
                "expense_id": expense.id.to_string(),
                "report_id": report.id.to_string(),
                "kind": expense.kind,
                "amount_minor": expense.amount_minor,
                "currency": expense.currency,
                "violations": expense.violations,
            }),
        );
        Ok(expense)
    })();
    match result {
        Ok(expense) => created(expense),
        Err(error) => err(error, debug),
    }
}

async fn get_expense(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let expense = load_expense(&state, &path.into_inner())?;
        let report = load_report(&state, &expense.report_id.to_string())?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        let receipts = expense_receipts(&state, expense.id);
        Ok(json!({ "expense": expense, "receipts": receipts }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_expense(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateExpenseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Expense> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let input = body.into_inner();
        let mut expense = load_expense(&state, &path.into_inner())?;
        let report = load_report(&state, &expense.report_id.to_string())?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        if report.status != REPORT_DRAFT {
            return Err(UnifiedError::conflict(format!(
                "Expenses can only be edited on a draft report; this report is \"{}\"",
                report.status
            ))
            .with_metadata("allowed_transitions", json!(allowed_report_transitions(&report.status))));
        }
        if let Some(raw) = &input.category_id {
            let category = load_category(&state, raw)?;
            if !category.active {
                return Err(UnifiedError::bad_request("Category is not active"));
            }
            expense.category_id = category.id;
        }
        if let Some(raw) = &input.expense_date {
            expense.expense_date = parse_date(raw, "expense_date")?;
        }
        if let Some(raw) = &input.description {
            expense.description = require_text(raw, "description", MAX_DESCRIPTION_LEN)?;
        }
        if input.project_id.is_some() {
            expense.project_id = optional_text(&input.project_id, "project_id", MAX_SLUG_LEN)?;
        }
        if input.client_id.is_some() {
            expense.client_id = optional_text(&input.client_id, "client_id", MAX_SLUG_LEN)?;
        }
        // Recompute computed kinds from the policy rate whenever
        // the distance or days change; a standard line keeps its
        // stored amount unless a new amount is supplied.
        match expense.kind.as_str() {
            "mileage" => {
                if let Some(distance) = input.distance {
                    if !(distance > 0.0) || distance > MAX_MILEAGE_DISTANCE || !distance.is_finite() {
                        return Err(UnifiedError::bad_request(format!(
                            "distance must be between 0 and {MAX_MILEAGE_DISTANCE}"
                        )));
                    }
                    let rate = mileage_rate_for(&state, &report);
                    expense.distance = Some(distance);
                    expense.rate_minor_applied = Some(rate);
                    expense.amount_minor =
                        require_amount((distance * rate as f64).round() as i64, "amount_minor")?;
                }
            }
            "per_diem" => {
                if let Some(days) = input.days {
                    if days < 1 || days > MAX_PER_DIEM_DAYS {
                        return Err(UnifiedError::bad_request(format!(
                            "days must be between 1 and {MAX_PER_DIEM_DAYS}"
                        )));
                    }
                    let rate = per_diem_rate_for(&state, &report);
                    expense.days = Some(days);
                    expense.rate_minor_applied = Some(rate);
                    expense.amount_minor = require_amount(days * rate, "amount_minor")?;
                }
            }
            _ => {
                if let Some(amount) = input.amount_minor {
                    expense.amount_minor = require_amount(amount, "amount_minor")?;
                }
            }
        }
        expense.violations = compute_violations(&state, &report, &expense);
        expense.updated_at = Utc::now();
        state.expenses.insert(expense.id, expense.clone());
        refresh_report_total(&state, report.id);
        refresh_report_violations(&state, report.id);
        state.emit(
            "expenses.expense.updated",
            json!({
                "expense_id": expense.id.to_string(),
                "report_id": report.id.to_string(),
                "amount_minor": expense.amount_minor,
                "violations": expense.violations,
            }),
        );
        Ok(expense)
    })();
    match result {
        Ok(expense) => ok(expense),
        Err(error) => err(error, debug),
    }
}

async fn delete_expense(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let expense = load_expense(&state, &path.into_inner())?;
        let report = load_report(&state, &expense.report_id.to_string())?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        if report.status != REPORT_DRAFT {
            return Err(UnifiedError::conflict(format!(
                "Expenses can only be deleted from a draft report; this report is \"{}\"",
                report.status
            ))
            .with_metadata("allowed_transitions", json!(allowed_report_transitions(&report.status))));
        }
        state
            .receipts
            .iter()
            .filter(|entry| entry.value().expense_id == expense.id)
            .map(|entry| *entry.key())
            .collect::<Vec<Uuid>>()
            .into_iter()
            .for_each(|receipt_id| {
                state.receipts.remove(&receipt_id);
            });
        state.expenses.remove(&expense.id);
        refresh_report_total(&state, report.id);
        refresh_report_violations(&state, report.id);
        state.emit(
            "expenses.expense.deleted",
            json!({
                "expense_id": expense.id.to_string(),
                "report_id": report.id.to_string(),
            }),
        );
        Ok(json!({ "deleted": true, "expense_id": expense.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// --- Handlers: receipts ------------------------------------------------------

async fn attach_receipt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AttachReceiptRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseReceipt> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let expense = load_expense(&state, &path.into_inner())?;
        let report = load_report(&state, &expense.report_id.to_string())?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        if report.status != REPORT_DRAFT {
            return Err(UnifiedError::conflict(format!(
                "Receipts can only be attached to a draft report; this report is \"{}\"",
                report.status
            ))
            .with_metadata("allowed_transitions", json!(allowed_report_transitions(&report.status))));
        }
        let input = body.into_inner();
        let storage_target = require_text(&input.storage_target, "storage_target", MAX_SLUG_LEN)?;
        let storage_key = require_text(&input.storage_key, "storage_key", MAX_REF_LEN)?;
        let file_name = require_text(&input.file_name, "file_name", MAX_NAME_LEN)?;
        let content_type = optional_text(&input.content_type, "content_type", MAX_SLUG_LEN)?;
        let now = Utc::now();
        let receipt = ExpenseReceipt {
            id: Uuid::now_v7(),
            expense_id: expense.id,
            storage_target,
            storage_key,
            file_name,
            content_type,
            uploaded_by: caller.actor.clone(),
            created_at: now,
        };
        state.receipts.insert(receipt.id, receipt.clone());
        // Attaching a receipt can clear the receipt-missing flag.
        let mut expense = expense;
        expense.violations = compute_violations(&state, &report, &expense);
        expense.updated_at = Utc::now();
        state.expenses.insert(expense.id, expense);
        state.emit(
            "expenses.receipt.attached",
            json!({
                "receipt_id": receipt.id.to_string(),
                "expense_id": receipt.expense_id.to_string(),
                "report_id": report.id.to_string(),
                "storage_target": receipt.storage_target,
                "storage_key": receipt.storage_key,
            }),
        );
        Ok(receipt)
    })();
    match result {
        Ok(receipt) => created(receipt),
        Err(error) => err(error, debug),
    }
}

async fn remove_receipt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let (expense_id_raw, receipt_id_raw) = path.into_inner();
        let expense = load_expense(&state, &expense_id_raw)?;
        let report = load_report(&state, &expense.report_id.to_string())?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        if report.status != REPORT_DRAFT {
            return Err(UnifiedError::conflict(format!(
                "Receipts can only be removed from a draft report; this report is \"{}\"",
                report.status
            ))
            .with_metadata("allowed_transitions", json!(allowed_report_transitions(&report.status))));
        }
        let receipt_id = parse_uuid(&receipt_id_raw, "receipt")?;
        let receipt = state
            .receipts
            .get(&receipt_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Receipt not found: {receipt_id}")))?;
        if receipt.expense_id != expense.id {
            return Err(UnifiedError::not_found(format!(
                "Receipt not found: {receipt_id}"
            )));
        }
        state.receipts.remove(&receipt_id);
        // Removing a receipt can restore the receipt-missing flag.
        let mut expense = expense;
        expense.violations = compute_violations(&state, &report, &expense);
        expense.updated_at = Utc::now();
        state.expenses.insert(expense.id, expense);
        state.emit(
            "expenses.receipt.removed",
            json!({
                "receipt_id": receipt.id.to_string(),
                "expense_id": receipt.expense_id.to_string(),
                "report_id": report.id.to_string(),
            }),
        );
        Ok(json!({ "deleted": true, "receipt_id": receipt.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// --- Handlers: reimbursements -----------------------------------------------

async fn list_reimbursements(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let employee_filter = params.filters.get("employee_id").cloned();
        let report_filter = params.filters.get("report_id").cloned();
        let manager = is_manager(&ctx, &caller.role);
        let mut items: Vec<Reimbursement> = state
            .reimbursements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if !manager && row.employee_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                if let Some(employee) = &employee_filter {
                    if &row.employee_id != employee {
                        return false;
                    }
                }
                if let Some(report) = &report_filter {
                    if row.report_id.to_string() != *report {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_reimbursement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Reimbursement> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let reimbursement = load_reimbursement(&state, &path.into_inner())?;
        require_row_access(&ctx, &caller, &reimbursement.employee_id)?;
        Ok(reimbursement)
    })();
    match result {
        Ok(reimbursement) => ok(reimbursement),
        Err(error) => err(error, debug),
    }
}

async fn retry_reimbursement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Reimbursement> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let mut reimbursement = load_reimbursement(&state, &path.into_inner())?;
        if reimbursement.status != REIMB_FAILED {
            return Err(UnifiedError::conflict(format!(
                "Only a failed reimbursement can be retried; this one is \"{}\"",
                reimbursement.status
            )));
        }
        reimbursement.status = REIMB_PENDING.to_string();
        reimbursement.failure_reason = None;
        reimbursement.attempts += 1;
        reimbursement.updated_at = Utc::now();
        state
            .reimbursements
            .insert(reimbursement.id, reimbursement.clone());
        state.emit(
            "expenses.reimbursement.requested",
            json!({
                "reimbursement_id": reimbursement.id.to_string(),
                "report_id": reimbursement.report_id.to_string(),
                "employee_id": reimbursement.employee_id,
                "amount_minor": reimbursement.amount_minor,
                "currency": reimbursement.currency,
                "payable_reference": reimbursement.payable_reference,
                "attempt": reimbursement.attempts,
            }),
        );
        state.emit(
            "expenses.reimbursement.retried",
            json!({
                "reimbursement_id": reimbursement.id.to_string(),
                "report_id": reimbursement.report_id.to_string(),
                "attempt": reimbursement.attempts,
            }),
        );
        Ok(reimbursement)
    })();
    match result {
        Ok(reimbursement) => ok(reimbursement),
        Err(error) => err(error, debug),
    }
}

// --- Handlers: adjustments ---------------------------------------------------

async fn list_adjustments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let report = load_report(&state, &path.into_inner())?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let mut items: Vec<ExpenseAdjustment> = state
            .adjustments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.report_id != report.id {
                    return false;
                }
                if let Some(kind) = &kind_filter {
                    if &row.kind != kind {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_adjustment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateAdjustmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseAdjustment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let report = load_report(&state, &path.into_inner())?;
        if report.status != REPORT_APPROVED && report.status != REPORT_REIMBURSED {
            return Err(UnifiedError::conflict(format!(
                "Adjustments can only be recorded against an approved or reimbursed report; this report is \"{}\"",
                report.status
            )));
        }
        let input = body.into_inner();
        if input.kind != ADJUST_REFUND && input.kind != ADJUST_ADJUSTMENT {
            return Err(UnifiedError::bad_request(format!(
                "kind must be \"{ADJUST_REFUND}\" or \"{ADJUST_ADJUSTMENT}\""
            )));
        }
        let amount = require_amount(input.amount_minor, "amount_minor")?;
        let currency = match &input.currency {
            Some(raw) => require_currency(raw)?,
            None => report.currency.clone(),
        };
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        let adjustment = ExpenseAdjustment {
            id: Uuid::now_v7(),
            report_id: report.id,
            kind: input.kind.clone(),
            amount_minor: amount,
            currency,
            reason,
            created_by: caller.actor.clone(),
            created_at: Utc::now(),
        };
        state
            .adjustments
            .insert(adjustment.id, adjustment.clone());
        state.emit(
            "expenses.adjustment.created",
            json!({
                "adjustment_id": adjustment.id.to_string(),
                "report_id": report.id.to_string(),
                "kind": adjustment.kind,
                "amount_minor": adjustment.amount_minor,
                "currency": adjustment.currency,
            }),
        );
        Ok(adjustment)
    })();
    match result {
        Ok(adjustment) => created(adjustment),
        Err(error) => err(error, debug),
    }
}

// --- Handlers: currency rates -------------------------------------------------

async fn list_currency_rates(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let params = query.into_inner();
        let base_filter = params.filters.get("base_currency").cloned();
        let target_filter = params.filters.get("target_currency").cloned();
        let mut items: Vec<CurrencyRate> = state
            .currency_rates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if let Some(base) = &base_filter {
                    if row.base_currency != *base {
                        return false;
                    }
                }
                if let Some(target) = &target_filter {
                    if row.target_currency != *target {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_currency_rate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    body: web::Json<CreateCurrencyRateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CurrencyRate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let input = body.into_inner();
        let base = require_currency(&input.base_currency)?;
        let target = require_currency(&input.target_currency)?;
        if base == target {
            return Err(UnifiedError::bad_request(
                "base_currency and target_currency must differ",
            ));
        }
        if !input.rate.is_finite() || input.rate <= 0.0 || input.rate > 1_000_000.0 {
            return Err(UnifiedError::bad_request(
                "rate must be a finite number greater than 0",
            ));
        }
        let source = match &input.source {
            Some(raw) => require_text(raw, "source", MAX_SLUG_LEN)?,
            None => "manual".to_string(),
        };
        let rate = CurrencyRate {
            id: Uuid::now_v7(),
            base_currency: base,
            target_currency: target,
            rate: input.rate,
            source,
            created_at: Utc::now(),
        };
        state.currency_rates.insert(rate.id, rate.clone());
        state.emit(
            "expenses.currency_rate.created",
            json!({
                "rate_id": rate.id.to_string(),
                "base_currency": rate.base_currency,
                "target_currency": rate.target_currency,
                "rate": rate.rate,
            }),
        );
        Ok(rate)
    })();
    match result {
        Ok(rate) => created(rate),
        Err(error) => err(error, debug),
    }
}

async fn get_currency_rate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CurrencyRate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        load_currency_rate(&state, &path.into_inner())
    })();
    match result {
        Ok(rate) => ok(rate),
        Err(error) => err(error, debug),
    }
}

async fn delete_currency_rate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let rate = load_currency_rate(&state, &path.into_inner())?;
        state.currency_rates.remove(&rate.id);
        state.emit(
            "expenses.currency_rate.deleted",
            json!({ "rate_id": rate.id.to_string() }),
        );
        Ok(json!({ "deleted": true, "rate_id": rate.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn convert_currency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    body: web::Json<ConvertRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let input = body.into_inner();
        let amount = require_amount(input.amount_minor, "amount_minor")?;
        let from = require_currency(&input.from_currency)?;
        let to = require_currency(&input.to_currency)?;
        let rate = resolve_rate(&state, &ctx.hooks, &from, &to).ok_or_else(|| {
            UnifiedError::unprocessable(format!(
                "No stored exchange rate for {from} to {to}; conversion refused rather than guessing a rate"
            ))
        })?;
        let converted = convert_amount(&state, &ctx.hooks, amount, &from, &to)?;
        Ok(json!({
            "amount_minor": amount,
            "from_currency": from,
            "to_currency": to,
            "converted_minor": converted,
            "rate": rate,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// --- Handlers: analytics ------------------------------------------------------

async fn spend_analytics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    query: web::Query<SpendQuery>,
    page: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let params = page.into_inner();
        let filters = query.into_inner();
        let from = match &filters.from {
            Some(raw) => Some(parse_date(raw, "from")?),
            None => None,
        };
        let to = match &filters.to {
            Some(raw) => Some(parse_date(raw, "to")?),
            None => None,
        };
        let group_by = filters
            .group_by
            .clone()
            .unwrap_or_else(|| "category".to_string());
        if group_by != "category" && group_by != "employee" && group_by != "period" {
            return Err(UnifiedError::bad_request(
                "group_by must be category, employee, or period",
            ));
        }
        let manager = is_manager(&ctx, &caller.role);
        let mut totals: HashMap<String, i64> = HashMap::new();
        let mut counts: HashMap<String, i64> = HashMap::new();
        for entry in state.expenses.iter() {
            let line = entry.value();
            let Some(report) = state.reports.get(&line.report_id) else {
                continue;
            };
            if !manager && report.employee_id != caller.actor {
                continue;
            }
            if report.status == REPORT_DRAFT {
                continue;
            }
            if let Some(from) = from {
                if line.expense_date < from {
                    continue;
                }
            }
            if let Some(to) = to {
                if line.expense_date > to {
                    continue;
                }
            }
            let key = match group_by.as_str() {
                "employee" => report.employee_id.clone(),
                "period" => line.expense_date.format("%Y-%m").to_string(),
                _ => line.category_id.to_string(),
            };
            *totals.entry(key.clone()).or_insert(0) += line.amount_minor;
            *counts.entry(key).or_insert(0) += 1;
        }
        // Refunds and adjustments reduce net spend; they are
        // separate entries and never mutate the lines above.
        let mut refund_totals: HashMap<String, i64> = HashMap::new();
        for entry in state.adjustments.iter() {
            let adjustment = entry.value();
            if adjustment.kind != ADJUST_REFUND {
                continue;
            }
            let Some(report) = state.reports.get(&adjustment.report_id) else {
                continue;
            };
            if !manager && report.employee_id != caller.actor {
                continue;
            }
            let key = match group_by.as_str() {
                "employee" => report.employee_id.clone(),
                _ => continue,
            };
            *refund_totals.entry(key).or_insert(0) += adjustment.amount_minor;
        }
        let mut rows: Vec<Value> = totals
            .iter()
            .map(|(key, total)| {
                let refunds = refund_totals.get(key).copied().unwrap_or(0);
                json!({
                    "group": key,
                    "group_by": group_by,
                    "total_minor": total,
                    "expense_count": counts.get(key).copied().unwrap_or(0),
                    "refund_minor": refunds,
                    "net_minor": total - refunds,
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            b.get("total_minor")
                .and_then(|value| value.as_i64())
                .cmp(
                    &a.get("total_minor")
                        .and_then(|value| value.as_i64()),
                )
        });
        Ok(paginate(rows, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn pending_approvals_analytics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let params = query.into_inner();
        let manager = is_manager(&ctx, &caller.role);
        let mut items: Vec<ExpenseReport> = state
            .reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.status == REPORT_SUBMITTED && (manager || row.employee_id == caller.actor)
            })
            .collect();
        items.sort_by(|a, b| a.submitted_at.cmp(&b.submitted_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn reimbursement_backlog_analytics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let params = query.into_inner();
        let manager = is_manager(&ctx, &caller.role);
        let mut items: Vec<Reimbursement> = state
            .reimbursements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                (row.status == REIMB_PENDING || row.status == REIMB_FAILED)
                    && (manager || row.employee_id == caller.actor)
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn violations_analytics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let params = query.into_inner();
        let manager = is_manager(&ctx, &caller.role);
        let mut items: Vec<Value> = Vec::new();
        for entry in state.expenses.iter() {
            let line = entry.value();
            if line.violations.is_empty() {
                continue;
            }
            let Some(report) = state.reports.get(&line.report_id) else {
                continue;
            };
            if !manager && report.employee_id != caller.actor {
                continue;
            }
            items.push(json!({
                "type": "expense",
                "expense_id": line.id.to_string(),
                "report_id": line.report_id.to_string(),
                "employee_id": report.employee_id,
                "amount_minor": line.amount_minor,
                "currency": line.currency,
                "violations": line.violations,
            }));
        }
        for entry in state.reports.iter() {
            let report = entry.value();
            if report.violations.is_empty() {
                continue;
            }
            if !manager && report.employee_id != caller.actor {
                continue;
            }
            items.push(json!({
                "type": "report",
                "report_id": report.id.to_string(),
                "employee_id": report.employee_id,
                "total_minor": report.total_minor,
                "currency": report.currency,
                "status": report.status,
                "violations": report.violations,
            }));
        }
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// --- Handlers: settings ------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpensesSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        Ok(state.settings())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpensesSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(kinds) = &input.expense_kinds {
            if kinds.is_empty() {
                return Err(UnifiedError::bad_request(
                    "expense_kinds must not be empty",
                ));
            }
            let mut cleaned: Vec<String> = Vec::new();
            for kind in kinds {
                cleaned.push(require_text(kind, "expense kind", MAX_SLUG_LEN)?);
            }
            settings.expense_kinds = cleaned;
        }
        if let Some(currencies) = &input.supported_currencies {
            if currencies.is_empty() {
                return Err(UnifiedError::bad_request(
                    "supported_currencies must not be empty",
                ));
            }
            let mut cleaned: Vec<String> = Vec::new();
            for currency in currencies {
                cleaned.push(require_currency(currency)?);
            }
            settings.supported_currencies = cleaned;
        }
        if let Some(raw) = &input.default_currency {
            let currency = require_currency(raw)?;
            if !settings.supported_currencies.contains(&currency) {
                return Err(UnifiedError::bad_request(
                    "default_currency must be one of supported_currencies",
                ));
            }
            settings.default_currency = currency;
        }
        if let Some(value) = input.default_mileage_rate_minor {
            settings.default_mileage_rate_minor = require_amount(value, "default_mileage_rate_minor")?;
        }
        if let Some(raw) = &input.default_mileage_unit {
            settings.default_mileage_unit = require_text(raw, "default_mileage_unit", 16)?;
        }
        if let Some(value) = input.default_per_diem_rate_minor {
            settings.default_per_diem_rate_minor =
                require_amount(value, "default_per_diem_rate_minor")?;
        }
        if let Some(value) = input.report_total_warning_minor {
            settings.report_total_warning_minor =
                require_amount(value, "report_total_warning_minor")?;
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        state.emit(
            "expenses.settings.updated",
            json!({
                "default_currency": settings.default_currency,
                "expense_kinds": settings.expense_kinds,
                "supported_currencies": settings.supported_currencies,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ExpensesModule {
    state: Arc<ExpensesState>,
}

impl ExpensesModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(ExpensesState::default()),
        }
    }
}

impl Default for ExpensesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ExpensesModule {
    fn id(&self) -> &'static str {
        "expenses"
    }

    fn name(&self) -> &'static str {
        "Expenses"
    }

    fn description(&self) -> &'static str {
        "Employee expenses: admin-editable categories with receipt and limit rules, per-company policies with mileage and per-diem rates, expense reports with a checked draft to reimbursed lifecycle, receipt references through the storage modules, approval through the approvals flow (events only, no second approval engine here), reimbursement pay-outs requested from billing (events only), stored-rate currency conversion with calculator fallback, and spend / approvals / backlog / violations analytics. All lists, rates, limits, and defaults are admin-configurable data, never hard-coded."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["billing", "storage", "roles"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["billing", "storage", "roles"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            APPROVAL_DECIDED,
            "expenses",
            Arc::new(move |ctx: &mut HookContext| {
                on_approval_decided(state.clone(), ctx)
            }),
        );
        // Generic approvals decision (see the module header): the
        // bridge half of the submit-time `approval.requested`.
        // Correlates by the stored approvals request id and
        // re-emits as `expenses.approval.decided` for the listener
        // above, which applies the outcome exactly once.
        let state = self.state.clone();
        bus.register(
            GENERIC_APPROVAL_DECIDED,
            "expenses",
            Arc::new(move |ctx: &mut HookContext| {
                on_generic_approval_decided(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            REIMBURSEMENT_PAID,
            "expenses",
            Arc::new(move |ctx: &mut HookContext| {
                on_reimbursement_paid(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            REIMBURSEMENT_FAILED,
            "expenses",
            Arc::new(move |ctx: &mut HookContext| {
                on_reimbursement_failed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            PAYMENT_CAPTURED,
            "expenses",
            Arc::new(move |ctx: &mut HookContext| {
                on_payment_event(state.clone(), ctx, true)
            }),
        );
        let state = self.state.clone();
        bus.register(
            PAYMENT_FAILED,
            "expenses",
            Arc::new(move |ctx: &mut HookContext| {
                on_payment_event(state.clone(), ctx, false)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/expenses")
                .app_data(state)
                .route("/categories", web::get().to(list_categories))
                .route("/categories", web::post().to(create_category))
                .route("/categories/{id}", web::get().to(get_category))
                .route("/categories/{id}", web::patch().to(update_category))
                .route("/categories/{id}", web::delete().to(delete_category))
                .route("/policies", web::get().to(list_policies))
                .route("/policies", web::post().to(create_policy))
                .route("/policies/{id}", web::get().to(get_policy))
                .route("/policies/{id}", web::patch().to(update_policy))
                .route("/policies/{id}", web::delete().to(delete_policy))
                .route("/reports", web::get().to(list_reports))
                .route("/reports", web::post().to(create_report))
                .route("/reports/{id}", web::get().to(get_report))
                .route("/reports/{id}", web::patch().to(update_report))
                .route("/reports/{id}", web::delete().to(delete_report))
                .route("/reports/{id}/submit", web::post().to(submit_report))
                .route("/reports/{id}/withdraw", web::post().to(withdraw_report))
                .route("/reports/{id}/export", web::get().to(export_report))
                .route("/reports/{id}/expenses", web::get().to(list_report_expenses))
                .route("/reports/{id}/expenses", web::post().to(create_expense))
                .route("/expenses/{id}", web::get().to(get_expense))
                .route("/expenses/{id}", web::patch().to(update_expense))
                .route("/expenses/{id}", web::delete().to(delete_expense))
                .route("/expenses/{id}/receipts", web::post().to(attach_receipt))
                .route(
                    "/expenses/{id}/receipts/{receipt_id}",
                    web::delete().to(remove_receipt),
                )
                .route("/reimbursements", web::get().to(list_reimbursements))
                .route("/reimbursements/{id}", web::get().to(get_reimbursement))
                .route(
                    "/reimbursements/{id}/retry",
                    web::post().to(retry_reimbursement),
                )
                .route("/reports/{id}/adjustments", web::get().to(list_adjustments))
                .route(
                    "/reports/{id}/adjustments",
                    web::post().to(create_adjustment),
                )
                .route("/currency-rates", web::get().to(list_currency_rates))
                .route("/currency-rates", web::post().to(create_currency_rate))
                .route("/currency-rates/{id}", web::get().to(get_currency_rate))
                .route(
                    "/currency-rates/{id}",
                    web::delete().to(delete_currency_rate),
                )
                .route("/currency-rates/convert", web::post().to(convert_currency))
                .route("/analytics/spend", web::get().to(spend_analytics))
                .route(
                    "/analytics/pending-approvals",
                    web::get().to(pending_approvals_analytics),
                )
                .route(
                    "/analytics/reimbursement-backlog",
                    web::get().to(reimbursement_backlog_analytics),
                )
                .route("/analytics/violations", web::get().to(violations_analytics))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/expenses/categories", owner: "expenses", description: "List expense categories (paginated, filter by company_id, active, search)." },
            RouteInfo { method: "POST", path: "/api/v1/expenses/categories", owner: "expenses", description: "Create an expense category with receipt and limit rules (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/categories/{id}", owner: "expenses", description: "Get one expense category." },
            RouteInfo { method: "PATCH", path: "/api/v1/expenses/categories/{id}", owner: "expenses", description: "Update an expense category (manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/expenses/categories/{id}", owner: "expenses", description: "Delete a category with no expenses (manager; deactivate instead when in use)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/policies", owner: "expenses", description: "List expense policies (paginated, filter by company_id, active, search)." },
            RouteInfo { method: "POST", path: "/api/v1/expenses/policies", owner: "expenses", description: "Create a per-company expense policy with mileage / per-diem rates and an approval chain reference (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/policies/{id}", owner: "expenses", description: "Get one expense policy." },
            RouteInfo { method: "PATCH", path: "/api/v1/expenses/policies/{id}", owner: "expenses", description: "Update an expense policy (manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/expenses/policies/{id}", owner: "expenses", description: "Delete a policy with no reports (manager; deactivate instead when in use)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/reports", owner: "expenses", description: "List expense reports (own only unless manager; filter by status, employee_id, company_id, policy_id)." },
            RouteInfo { method: "POST", path: "/api/v1/expenses/reports", owner: "expenses", description: "Create a draft expense report." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/reports/{id}", owner: "expenses", description: "Get one report with its expense lines (own or manager)." },
            RouteInfo { method: "PATCH", path: "/api/v1/expenses/reports/{id}", owner: "expenses", description: "Update a draft report (title, notes, policy)." },
            RouteInfo { method: "DELETE", path: "/api/v1/expenses/reports/{id}", owner: "expenses", description: "Delete a draft report with its lines, receipts, and adjustments." },
            RouteInfo { method: "POST", path: "/api/v1/expenses/reports/{id}/submit", owner: "expenses", description: "Submit a draft report: checks receipt rules, moves to submitted, and emits expenses.approval.requested plus the generic approval.requested for the approvals flow." },
            RouteInfo { method: "POST", path: "/api/v1/expenses/reports/{id}/withdraw", owner: "expenses", description: "Withdraw a submitted or rejected report back to draft (cancels the pending approval request)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/reports/{id}/export", owner: "expenses", description: "Export a report as JSON or CSV (?format=json|csv; CSV is a plain-text body, not the JSON envelope)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/reports/{id}/expenses", owner: "expenses", description: "List the expense lines of a report (paginated, filter by kind, category_id)." },
            RouteInfo { method: "POST", path: "/api/v1/expenses/reports/{id}/expenses", owner: "expenses", description: "Add an expense line to a draft report (mileage and per-diem amounts are computed server-side from policy rates)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/expenses/{id}", owner: "expenses", description: "Get one expense line with its receipt references (own or manager)." },
            RouteInfo { method: "PATCH", path: "/api/v1/expenses/expenses/{id}", owner: "expenses", description: "Update an expense line on a draft report (mileage / per-diem amounts recompute from the policy rate)." },
            RouteInfo { method: "DELETE", path: "/api/v1/expenses/expenses/{id}", owner: "expenses", description: "Delete an expense line from a draft report (with its receipt references)." },
            RouteInfo { method: "POST", path: "/api/v1/expenses/expenses/{id}/receipts", owner: "expenses", description: "Attach a receipt reference (storage target + key; no bytes live here) to a draft-report expense." },
            RouteInfo { method: "DELETE", path: "/api/v1/expenses/expenses/{id}/receipts/{receipt_id}", owner: "expenses", description: "Remove a receipt reference from a draft-report expense." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/reimbursements", owner: "expenses", description: "List reimbursements (own only unless manager; filter by status, employee_id, report_id)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/reimbursements/{id}", owner: "expenses", description: "Get one reimbursement (own or manager)." },
            RouteInfo { method: "POST", path: "/api/v1/expenses/reimbursements/{id}/retry", owner: "expenses", description: "Re-request a failed reimbursement from billing (manager; failed only)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/reports/{id}/adjustments", owner: "expenses", description: "List refunds and adjustments of a report (paginated, filter by kind)." },
            RouteInfo { method: "POST", path: "/api/v1/expenses/reports/{id}/adjustments", owner: "expenses", description: "Record a refund or adjustment against an approved or reimbursed report (manager; never mutates history)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/currency-rates", owner: "expenses", description: "List stored exchange rates (paginated, filter by base_currency, target_currency)." },
            RouteInfo { method: "POST", path: "/api/v1/expenses/currency-rates", owner: "expenses", description: "Store an exchange rate (manager; rates are stored data, never fetched live)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/currency-rates/{id}", owner: "expenses", description: "Get one stored exchange rate." },
            RouteInfo { method: "DELETE", path: "/api/v1/expenses/currency-rates/{id}", owner: "expenses", description: "Delete a stored exchange rate (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/expenses/currency-rates/convert", owner: "expenses", description: "Convert an amount with a stored rate (own rates first, then the calculator module's stored rate; 422 when no stored rate exists)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/analytics/spend", owner: "expenses", description: "Spend analytics grouped by category, employee, or period (?group_by=category|employee|period, ?from=YYYY-MM-DD, ?to=YYYY-MM-DD; refunds reduce net spend)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/analytics/pending-approvals", owner: "expenses", description: "Reports waiting on an approval decision (own only unless manager)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/analytics/reimbursement-backlog", owner: "expenses", description: "Pending and failed reimbursements (own only unless manager)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/analytics/violations", owner: "expenses", description: "Flagged expenses and over-limit reports (own only unless manager)." },
            RouteInfo { method: "GET", path: "/api/v1/expenses/settings", owner: "expenses", description: "Get the configurable expenses settings (kinds, currencies, default rates)." },
            RouteInfo { method: "PUT", path: "/api/v1/expenses/settings", owner: "expenses", description: "Update the configurable expenses settings (manager)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ExpensesModule::new()));
}
